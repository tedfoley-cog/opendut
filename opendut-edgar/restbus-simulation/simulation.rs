use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::arxml_structs::{CanCluster, CanFrameTriggering, PDU};
use crate::arxml_utils::extract_init_values;

/// Maximum data length for a standard CAN frame.
const CAN_MAX_DLEN: usize = 8;

/// Maximum data length for a CAN FD frame.
const CANFD_MAX_DLEN: usize = 64;

/// CAN frame flag indicating an extended (29-bit) identifier.
const CAN_EFF_FLAG: u32 = 0x8000_0000;

// SocketCAN constants
const AF_CAN: libc::c_int = 29;
const PF_CAN: libc::c_int = AF_CAN;
const CAN_RAW: libc::c_int = 1;
const CANFD_MTU: usize = 72;

/// Enable CAN FD frames on the socket.
const CAN_RAW_FD_FRAMES: libc::c_int = 5;
const SOL_CAN_RAW: libc::c_int = 101;

/// Raw CAN frame as expected by the Linux SocketCAN interface.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawCanFrame {
    can_id: u32,
    len: u8,
    flags: u8,
    _res0: u8,
    _res1: u8,
    data: [u8; CANFD_MAX_DLEN],
}

/// Bind address for a CAN socket.
#[repr(C)]
struct SockaddrCan {
    can_family: libc::sa_family_t,
    can_ifindex: libc::c_int,
    // transport protocol-specific address information (unused for CAN_RAW)
    _can_addr: [u8; 8],
}

/// A CAN frame scheduled for cyclic transmission.
#[derive(Debug, Clone)]
pub struct ScheduledFrame {
    /// CAN identifier (11-bit standard or 29-bit extended).
    pub can_id: u32,
    /// Frame data bytes built from initial signal values.
    pub data: Vec<u8>,
    /// Cyclic transmission period.
    pub period: Duration,
    /// Initial offset before the first transmission.
    pub offset: Duration,
    /// Human-readable frame name for logging.
    pub frame_name: String,
    /// Whether the CAN identifier uses extended (29-bit) addressing.
    pub extended_id: bool,
}

/// Errors that can occur during simulation.
#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    #[error("failed to open CAN socket on interface '{interface}': {source}")]
    SocketOpen {
        interface: String,
        source: std::io::Error,
    },
    #[error("failed to send CAN frame: {0}")]
    FrameSend(std::io::Error),
    #[error("no scheduled frames to simulate")]
    NoFrames,
}

/// Thin wrapper around a Linux SocketCAN raw socket.
///
/// Supports both standard CAN and CAN FD frames via the kernel's
/// `CAN_RAW_FD_FRAMES` socket option.
struct CanSocket {
    fd: libc::c_int,
}

// SAFETY: The file descriptor is used only through `write_frame` which takes
// `&self` and performs a single atomic `write()` syscall. The kernel guarantees
// that concurrent `write()` calls on the same CAN socket are safe.
unsafe impl Send for CanSocket {}
unsafe impl Sync for CanSocket {}

impl CanSocket {
    /// Open a CAN_RAW socket bound to the named interface (e.g. `"vcan0"`).
    fn open(interface: &str) -> Result<Self, SimulationError> {
        // SAFETY: all FFI calls operate on kernel-managed resources; error
        // codes are checked immediately after each call.
        unsafe {
            let fd = libc::socket(PF_CAN, libc::SOCK_RAW, CAN_RAW);
            if fd < 0 {
                return Err(SimulationError::SocketOpen {
                    interface: interface.to_owned(),
                    source: std::io::Error::last_os_error(),
                });
            }

            // Look up the interface index via ioctl(SIOCGIFINDEX).
            let mut ifr: libc::ifreq = std::mem::zeroed();
            let name_bytes = interface.as_bytes();
            let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
            std::ptr::copy_nonoverlapping(
                name_bytes.as_ptr(),
                ifr.ifr_name.as_mut_ptr().cast::<u8>(),
                copy_len,
            );

            if libc::ioctl(fd, libc::SIOCGIFINDEX as libc::c_ulong, &mut ifr) < 0 {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(SimulationError::SocketOpen {
                    interface: interface.to_owned(),
                    source: err,
                });
            }

            // Enable CAN FD support so we can send frames with >8 bytes.
            let enable: libc::c_int = 1;
            libc::setsockopt(
                fd,
                SOL_CAN_RAW,
                CAN_RAW_FD_FRAMES,
                &enable as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );

            // Bind to the CAN interface.
            let addr = SockaddrCan {
                can_family: AF_CAN as libc::sa_family_t,
                can_ifindex: ifr.ifr_ifru.ifru_ifindex,
                _can_addr: [0u8; 8],
            };

            if libc::bind(
                fd,
                &addr as *const SockaddrCan as *const libc::sockaddr,
                std::mem::size_of::<SockaddrCan>() as libc::socklen_t,
            ) < 0
            {
                let err = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(SimulationError::SocketOpen {
                    interface: interface.to_owned(),
                    source: err,
                });
            }

            Ok(CanSocket { fd })
        }
    }

    /// Write a single CAN frame to the socket.
    ///
    /// Frames with data length <= 8 are sent as classic CAN; longer frames
    /// are sent as CAN FD.
    fn write_frame(&self, id: u32, data: &[u8]) -> Result<(), SimulationError> {
        let mut frame: RawCanFrame = unsafe { std::mem::zeroed() };
        frame.can_id = id;
        let copy_len = data.len().min(CANFD_MAX_DLEN);
        frame.len = copy_len as u8;
        frame.data[..copy_len].copy_from_slice(&data[..copy_len]);

        // Determine the wire size: classic CAN (16 bytes) or CAN FD (72 bytes).
        let write_len = if copy_len > CAN_MAX_DLEN {
            CANFD_MTU
        } else {
            // Classic CAN frame: 8 bytes header + 8 bytes data = 16
            16
        };

        let written = unsafe {
            libc::write(
                self.fd,
                &frame as *const RawCanFrame as *const libc::c_void,
                write_len,
            )
        };

        if written < 0 {
            Err(SimulationError::FrameSend(std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }
}

impl Drop for CanSocket {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// The restbus simulation engine.
///
/// Takes parsed [`CanCluster`] definitions and replays their CAN frames on a
/// SocketCAN interface at the cyclic timing rates defined in the ARXML source.
pub struct RestbusSimulation {
    interface_name: String,
    scheduled_frames: Vec<ScheduledFrame>,
}

impl RestbusSimulation {
    /// Build a new simulation from one or more parsed CAN clusters.
    ///
    /// Iterates over every [`CanFrameTriggering`] in each cluster and, for
    /// those that carry an `ISignalIPDU` with a non-zero cyclic timing period,
    /// constructs a [`ScheduledFrame`] containing the packed initial signal
    /// values.
    pub fn new(interface_name: &str, clusters: &HashMap<String, CanCluster>) -> Self {
        let mut scheduled_frames = Vec::new();

        for cluster in clusters.values() {
            for triggering in cluster.can_frame_triggerings.values() {
                match Self::build_scheduled_frame(triggering) {
                    Ok(Some(frame)) => {
                        tracing::info!(
                            can_id = frame.can_id,
                            period_ms = frame.period.as_millis() as u64,
                            name = %frame.frame_name,
                            "scheduled frame"
                        );
                        scheduled_frames.push(frame);
                    }
                    Ok(None) => {
                        tracing::debug!(
                            name = %triggering.frame_name,
                            "skipping frame without cyclic timing"
                        );
                    }
                    Err(msg) => {
                        tracing::warn!(
                            name = %triggering.frame_name,
                            error = %msg,
                            "failed to build scheduled frame"
                        );
                    }
                }
            }
        }

        tracing::info!(
            count = scheduled_frames.len(),
            interface = %interface_name,
            "restbus simulation ready"
        );

        RestbusSimulation {
            interface_name: interface_name.to_owned(),
            scheduled_frames,
        }
    }

    /// Return a reference to the list of frames that will be transmitted.
    pub fn scheduled_frames(&self) -> &[ScheduledFrame] {
        &self.scheduled_frames
    }

    /// Return the target CAN interface name.
    pub fn interface_name(&self) -> &str {
        &self.interface_name
    }

    /// Build a [`ScheduledFrame`] from a single [`CanFrameTriggering`].
    ///
    /// Returns `Ok(None)` if the triggering has no cyclic timing (i.e. it is
    /// event-driven only) or has no PDU mappings.
    fn build_scheduled_frame(
        triggering: &CanFrameTriggering,
    ) -> Result<Option<ScheduledFrame>, String> {
        if triggering.pdu_mappings.is_empty() {
            return Ok(None);
        }

        let frame_length = triggering.frame_length as usize;
        if frame_length == 0 || frame_length > CANFD_MAX_DLEN {
            return Err(format!(
                "invalid frame length {} for '{}'",
                frame_length, triggering.frame_name
            ));
        }

        // Start with all-ones (0xFF) which is the typical unused-bit default
        // for automotive CAN buses.
        let mut frame_data = vec![0xFFu8; frame_length];
        let mut period = Duration::ZERO;
        let mut offset = Duration::ZERO;

        for pdu_mapping in &triggering.pdu_mappings {
            // AUTOSAR defines PDU start position in bits within the frame.
            let pdu_byte_offset = (pdu_mapping.start_position / 8) as usize;

            match &pdu_mapping.pdu {
                PDU::ISignalIPDU(ipdu) => {
                    // Extract the cyclic timing period; if zero the frame is
                    // event-triggered and we skip it.
                    let timing_secs = ipdu.cyclic_timing_period_value;
                    if timing_secs > 0.0 {
                        period = Duration::from_secs_f64(timing_secs);
                        offset = Duration::from_secs_f64(ipdu.cyclic_timing_offset_value);
                    }

                    let pdu_data = extract_init_values(
                        ipdu.unused_bit_pattern,
                        &ipdu.ungrouped_signals,
                        &ipdu.grouped_signals,
                        pdu_mapping.length,
                        &pdu_mapping.byte_order,
                    );

                    copy_pdu_into_frame(
                        &mut frame_data,
                        &pdu_data,
                        pdu_byte_offset,
                    );
                }
                PDU::NMPDU(nmpdu) => {
                    let pdu_data = extract_init_values(
                        nmpdu.unused_bit_pattern,
                        &nmpdu.ungrouped_signals,
                        &nmpdu.grouped_signals,
                        pdu_mapping.length,
                        &pdu_mapping.byte_order,
                    );

                    copy_pdu_into_frame(
                        &mut frame_data,
                        &pdu_data,
                        pdu_byte_offset,
                    );
                }
            }
        }

        if period.is_zero() {
            return Ok(None);
        }

        // Determine if the CAN ID uses extended (29-bit) addressing.
        let extended_id = triggering.addressing_mode == "Extended";
        let mut can_id = triggering.can_id as u32;
        if extended_id {
            can_id |= CAN_EFF_FLAG;
        }

        Ok(Some(ScheduledFrame {
            can_id,
            data: frame_data,
            period,
            offset,
            frame_name: triggering.frame_name.clone(),
            extended_id,
        }))
    }

    /// Run the simulation until the supplied [`CancellationToken`] is
    /// cancelled.
    ///
    /// Opens a SocketCAN socket on the configured interface and spawns one
    /// async task per scheduled frame. Each task sleeps for the frame's
    /// initial offset, then enters a cyclic loop transmitting the frame at
    /// the configured period.
    pub async fn run(&self, cancel: CancellationToken) -> Result<(), SimulationError> {
        if self.scheduled_frames.is_empty() {
            return Err(SimulationError::NoFrames);
        }

        let socket = CanSocket::open(&self.interface_name)?;
        let socket = Arc::new(Mutex::new(socket));

        tracing::info!(
            interface = %self.interface_name,
            frames = self.scheduled_frames.len(),
            "starting restbus simulation"
        );

        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        for frame in &self.scheduled_frames {
            let socket = Arc::clone(&socket);
            let cancel = cancel.clone();
            let frame = frame.clone();

            let handle = tokio::spawn(async move {
                // Respect the initial timing offset before beginning cyclic
                // transmission.
                if !frame.offset.is_zero() {
                    tokio::select! {
                        () = tokio::time::sleep(frame.offset) => {}
                        () = cancel.cancelled() => return,
                    }
                }

                let mut interval = tokio::time::interval(frame.period);
                // The first tick completes immediately; consume it so that the
                // first real transmission happens after one full period.
                interval.tick().await;

                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let sock = socket.lock().await;
                            if let Err(e) = sock.write_frame(frame.can_id, &frame.data) {
                                tracing::error!(
                                    frame = %frame.frame_name,
                                    can_id = frame.can_id,
                                    error = %e,
                                    "CAN frame send failed"
                                );
                            } else {
                                tracing::trace!(
                                    frame = %frame.frame_name,
                                    can_id = frame.can_id,
                                    "CAN frame sent"
                                );
                            }
                        }
                        () = cancel.cancelled() => {
                            tracing::debug!(frame = %frame.frame_name, "task cancelled");
                            break;
                        }
                    }
                }
            });

            handles.push(handle);
        }

        // Block until all per-frame tasks finish (they exit on cancellation).
        for handle in handles {
            let _ = handle.await;
        }

        tracing::info!("restbus simulation stopped");
        Ok(())
    }
}

/// Copy PDU data into a frame buffer at the given byte offset.
fn copy_pdu_into_frame(frame_data: &mut [u8], pdu_data: &[u8], byte_offset: usize) {
    let frame_len = frame_data.len();
    if byte_offset >= frame_len {
        return;
    }
    let available = frame_len - byte_offset;
    let copy_len = pdu_data.len().min(available);
    frame_data[byte_offset..byte_offset + copy_len].copy_from_slice(&pdu_data[..copy_len]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arxml_structs::*;

    /// Helper: create a minimal ISignalIPDU with a cyclic timing period.
    fn make_isignal_ipdu(
        period_secs: f64,
        offset_secs: f64,
        unused_bit_pattern: bool,
        signals: Vec<ISignal>,
    ) -> ISignalIPDU {
        ISignalIPDU {
            cyclic_timing_period_value: period_secs,
            cyclic_timing_period_tolerance: None,
            cyclic_timing_offset_value: offset_secs,
            cyclic_timing_offset_tolerance: None,
            number_of_repetitions: 0,
            repetition_period_value: 0.0,
            repetition_period_tolerance: None,
            unused_bit_pattern,
            ungrouped_signals: signals,
            grouped_signals: Vec::new(),
        }
    }

    /// Helper: create a CanFrameTriggering with a single PDU mapping.
    fn make_triggering(
        can_id: i64,
        frame_length: i64,
        pdu: PDU,
        pdu_length: i64,
    ) -> CanFrameTriggering {
        CanFrameTriggering {
            frame_triggering_name: format!("FT_{:#X}", can_id),
            frame_name: format!("Frame_{:#X}", can_id),
            can_id,
            addressing_mode: "Standard".to_owned(),
            frame_rx_behavior: String::new(),
            frame_tx_behavior: String::new(),
            rx_range_lower: 0,
            rx_range_upper: 0,
            sender_ecus: vec!["ECU_A".to_owned()],
            receiver_ecus: vec!["ECU_B".to_owned()],
            frame_length,
            pdu_mappings: vec![PDUMapping {
                name: "PDU_0".to_owned(),
                byte_order: false, // little-endian
                start_position: 0,
                length: pdu_length,
                dynamic_length: String::new(),
                category: String::new(),
                contained_header_id_short: String::new(),
                contained_header_id_long: String::new(),
                pdu,
            }],
        }
    }

    #[test]
    fn build_scheduled_frame_with_cyclic_timing() {
        let ipdu = make_isignal_ipdu(0.010, 0.0, false, Vec::new());
        let triggering = make_triggering(0x123, 8, PDU::ISignalIPDU(ipdu), 8);

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        let frame = result.unwrap().expect("should produce a scheduled frame");

        assert_eq!(frame.can_id, 0x123);
        assert_eq!(frame.data.len(), 8);
        assert_eq!(frame.period, Duration::from_millis(10));
        assert_eq!(frame.offset, Duration::ZERO);
        assert!(!frame.extended_id);
    }

    #[test]
    fn build_scheduled_frame_skips_zero_period() {
        let ipdu = make_isignal_ipdu(0.0, 0.0, false, Vec::new());
        let triggering = make_triggering(0x200, 8, PDU::ISignalIPDU(ipdu), 8);

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        assert!(result.unwrap().is_none(), "zero period should produce None");
    }

    #[test]
    fn build_scheduled_frame_respects_offset() {
        let ipdu = make_isignal_ipdu(0.020, 0.005, false, Vec::new());
        let triggering = make_triggering(0x300, 8, PDU::ISignalIPDU(ipdu), 8);

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        let frame = result.unwrap().unwrap();

        assert_eq!(frame.period, Duration::from_millis(20));
        assert_eq!(frame.offset, Duration::from_millis(5));
    }

    #[test]
    fn build_scheduled_frame_extended_id() {
        let ipdu = make_isignal_ipdu(0.100, 0.0, false, Vec::new());
        let mut triggering = make_triggering(0x18FEF100, 8, PDU::ISignalIPDU(ipdu), 8);
        triggering.addressing_mode = "Extended".to_owned();

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        let frame = result.unwrap().unwrap();

        assert!(frame.extended_id);
        assert_eq!(frame.can_id, 0x18FEF100 | CAN_EFF_FLAG);
    }

    #[test]
    fn build_scheduled_frame_packs_signal_init_values() {
        // Create a signal at bit position 0 with length 8 and init value 0xAB.
        let signal = ISignal {
            name: "TestSignal".to_owned(),
            byte_order: false, // little-endian
            start_pos: 0,
            length: 8,
            init_values: InitValues::Single(0xAB),
        };

        let ipdu = make_isignal_ipdu(0.050, 0.0, false, vec![signal]);
        let triggering = make_triggering(0x400, 8, PDU::ISignalIPDU(ipdu), 8);

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        let frame = result.unwrap().unwrap();

        // The first byte should contain 0xAB from the signal init value
        // (little-endian PDU mapping, little-endian signal).
        assert_eq!(frame.data[0], 0xAB);
    }

    #[test]
    fn build_scheduled_frame_rejects_invalid_length() {
        let ipdu = make_isignal_ipdu(0.010, 0.0, false, Vec::new());
        let triggering = make_triggering(0x500, 0, PDU::ISignalIPDU(ipdu), 0);

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        assert!(result.is_err(), "zero length should be rejected");
    }

    #[test]
    fn build_scheduled_frame_empty_pdu_mappings() {
        let triggering = CanFrameTriggering {
            frame_triggering_name: "FT_Empty".to_owned(),
            frame_name: "Frame_Empty".to_owned(),
            can_id: 0x600,
            addressing_mode: "Standard".to_owned(),
            frame_rx_behavior: String::new(),
            frame_tx_behavior: String::new(),
            rx_range_lower: 0,
            rx_range_upper: 0,
            sender_ecus: Vec::new(),
            receiver_ecus: Vec::new(),
            frame_length: 8,
            pdu_mappings: Vec::new(),
        };

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn copy_pdu_into_frame_basic() {
        let mut frame = vec![0xFFu8; 8];
        let pdu = vec![0x11, 0x22, 0x33];
        copy_pdu_into_frame(&mut frame, &pdu, 2);
        assert_eq!(frame, vec![0xFF, 0xFF, 0x11, 0x22, 0x33, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn copy_pdu_into_frame_clamps_at_boundary() {
        let mut frame = vec![0xFFu8; 4];
        let pdu = vec![0xAA, 0xBB, 0xCC, 0xDD];
        copy_pdu_into_frame(&mut frame, &pdu, 2);
        // Only 2 bytes fit at offset 2 in a 4-byte frame.
        assert_eq!(frame, vec![0xFF, 0xFF, 0xAA, 0xBB]);
    }

    #[test]
    fn copy_pdu_into_frame_offset_past_end() {
        let mut frame = vec![0xFFu8; 4];
        let pdu = vec![0x11];
        copy_pdu_into_frame(&mut frame, &pdu, 10);
        // Nothing should change.
        assert_eq!(frame, vec![0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn simulation_new_collects_cyclic_frames() {
        let ipdu_cyclic = make_isignal_ipdu(0.010, 0.0, false, Vec::new());
        let ipdu_event = make_isignal_ipdu(0.0, 0.0, false, Vec::new());

        let t1 = make_triggering(0x100, 8, PDU::ISignalIPDU(ipdu_cyclic), 8);
        let t2 = make_triggering(0x200, 8, PDU::ISignalIPDU(ipdu_event), 8);

        let mut frame_triggerings = HashMap::new();
        frame_triggerings.insert(t1.can_id, t1);
        frame_triggerings.insert(t2.can_id, t2);

        let cluster = CanCluster {
            name: "TestCluster".to_owned(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings: frame_triggerings,
        };

        let mut clusters = HashMap::new();
        clusters.insert(cluster.name.clone(), cluster);

        let sim = RestbusSimulation::new("vcan0", &clusters);

        // Only the frame with a non-zero cyclic period should be scheduled.
        assert_eq!(sim.scheduled_frames().len(), 1);
        assert_eq!(sim.scheduled_frames()[0].can_id, 0x100);
    }

    #[test]
    fn simulation_new_handles_multiple_clusters() {
        let ipdu1 = make_isignal_ipdu(0.020, 0.0, false, Vec::new());
        let ipdu2 = make_isignal_ipdu(0.050, 0.0, false, Vec::new());

        let t1 = make_triggering(0x100, 8, PDU::ISignalIPDU(ipdu1), 8);
        let t2 = make_triggering(0x200, 8, PDU::ISignalIPDU(ipdu2), 8);

        let cluster1 = CanCluster {
            name: "Cluster1".to_owned(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings: {
                let mut m = HashMap::new();
                m.insert(t1.can_id, t1);
                m
            },
        };
        let cluster2 = CanCluster {
            name: "Cluster2".to_owned(),
            baudrate: 250_000,
            canfd_baudrate: 0,
            can_frame_triggerings: {
                let mut m = HashMap::new();
                m.insert(t2.can_id, t2);
                m
            },
        };

        let mut clusters = HashMap::new();
        clusters.insert(cluster1.name.clone(), cluster1);
        clusters.insert(cluster2.name.clone(), cluster2);

        let sim = RestbusSimulation::new("vcan0", &clusters);
        assert_eq!(sim.scheduled_frames().len(), 2);
    }

    #[test]
    fn build_frame_unused_bit_pattern_fills_defaults() {
        // unused_bit_pattern = true means undefined bits are 1 (0xFF bytes).
        // With no signals, the entire PDU should be 0xFF.
        let ipdu = make_isignal_ipdu(0.010, 0.0, true, Vec::new());
        let triggering = make_triggering(0x700, 8, PDU::ISignalIPDU(ipdu), 8);

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        let frame = result.unwrap().unwrap();

        assert!(frame.data.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn build_frame_nmpdu() {
        let nmpdu = NMPDU {
            unused_bit_pattern: false,
            ungrouped_signals: Vec::new(),
            grouped_signals: Vec::new(),
        };
        // NMPDUs don't carry cyclic timing, so the frame should not be
        // scheduled (period remains zero).
        let triggering = make_triggering(0x800, 8, PDU::NMPDU(nmpdu), 8);

        let result = RestbusSimulation::build_scheduled_frame(&triggering);
        assert!(result.unwrap().is_none());
    }
}
