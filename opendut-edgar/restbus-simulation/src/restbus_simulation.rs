use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{self, Duration, Instant};
use tracing::{debug, error, info, warn};

use crate::arxml_structs::*;
use crate::arxml_utils::extract_init_values;

// ---- SocketCAN low-level helpers ----

/// Linux SocketCAN address family / protocol constants.
const AF_CAN: i32 = 29;
const PF_CAN: i32 = AF_CAN;
const CAN_RAW: i32 = 1;
const CAN_MTU: usize = 16; // sizeof(struct can_frame)
const CAN_EFF_FLAG: u32 = 0x8000_0000;

/// Mirror of Linux `struct sockaddr_can` (16 bytes).
#[repr(C)]
struct SockAddrCan {
    can_family: u16,
    _pad_align: u16,
    ifindex: i32,
    // rx_id / tx_id union – unused for raw CAN
    _pad: [u8; 8],
}

/// Mirror of Linux `struct can_frame` (16 bytes).
#[repr(C)]
#[derive(Clone)]
struct CanFrame {
    can_id: u32,
    len: u8,
    _pad: u8,
    _res0: u8,
    _res1: u8,
    data: [u8; 8],
}

impl CanFrame {
    fn new(can_id: u32, data: &[u8], extended: bool) -> Self {
        let mut frame = CanFrame {
            can_id: if extended { can_id | CAN_EFF_FLAG } else { can_id },
            len: data.len().min(8) as u8,
            _pad: 0,
            _res0: 0,
            _res1: 0,
            data: [0u8; 8],
        };
        let copy_len = data.len().min(8);
        frame.data[..copy_len].copy_from_slice(&data[..copy_len]);
        frame
    }

    fn as_bytes(&self) -> &[u8] {
        // Safety: CanFrame is repr(C) with known layout matching the kernel struct.
        unsafe { std::slice::from_raw_parts(self as *const CanFrame as *const u8, CAN_MTU) }
    }
}

/// A raw SocketCAN socket bound to a specific interface.
struct CanSocket {
    fd: i32,
}

impl CanSocket {
    /// Open a raw CAN socket and bind it to the given interface name (e.g. "vcan0").
    fn open(interface_name: &str) -> io::Result<Self> {
        // Resolve interface index
        let ifindex = Self::if_nametoindex(interface_name)?;

        // Create raw CAN socket
        let fd = unsafe { libc::socket(PF_CAN, libc::SOCK_RAW, CAN_RAW) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // Bind to the CAN interface
        let addr = SockAddrCan {
            can_family: AF_CAN as u16,
            _pad_align: 0,
            ifindex: ifindex as i32,
            _pad: [0u8; 8],
        };
        let ret = unsafe {
            libc::bind(
                fd,
                &addr as *const SockAddrCan as *const libc::sockaddr,
                std::mem::size_of::<SockAddrCan>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }

        Ok(CanSocket { fd })
    }

    fn if_nametoindex(name: &str) -> io::Result<u32> {
        let c_name =
            std::ffi::CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let idx = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
        if idx == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("interface '{}' not found", name),
            ));
        }
        Ok(idx)
    }

    fn send_frame(&self, frame: &CanFrame) -> io::Result<()> {
        let bytes = frame.as_bytes();
        let written = unsafe { libc::write(self.fd, bytes.as_ptr() as *const libc::c_void, CAN_MTU) };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        if (written as usize) != CAN_MTU {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("incomplete CAN frame write: {} of {} bytes", written, CAN_MTU),
            ));
        }
        Ok(())
    }
}

impl Drop for CanSocket {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

// Safety: The file descriptor is thread-safe when only used for writing.
unsafe impl Send for CanSocket {}
unsafe impl Sync for CanSocket {}

// ---- Frame preparation ----

/// A prepared CAN frame together with its cyclic timing parameters.
#[derive(Clone)]
struct ScheduledFrame {
    frame: CanFrame,
    /// Cyclic period in seconds (must be > 0 for the frame to be scheduled).
    period_secs: f64,
    /// Initial offset in seconds before the first transmission.
    offset_secs: f64,
    /// Human-readable label for logging.
    label: String,
}

/// Build the list of [`ScheduledFrame`]s from a parsed [`CanCluster`].
///
/// For each `CanFrameTriggering` we look at the first PDU mapping that is an
/// `ISignalIPDU` (which carries cyclic timing information). The initial data
/// bytes are computed from signal init-values via [`extract_init_values`].
fn build_scheduled_frames(cluster: &CanCluster) -> Vec<ScheduledFrame> {
    let mut frames: Vec<ScheduledFrame> = Vec::new();

    for (can_id, triggering) in &cluster.can_frame_triggerings {
        let extended = triggering.addressing_mode == "Extended";

        // Gather timing + signals from the first ISignalIPDU mapping.
        let mut period_secs: f64 = 0.0;
        let mut offset_secs: f64 = 0.0;
        let mut data_bytes: Option<Vec<u8>> = None;

        for pdu_mapping in &triggering.pdu_mappings {
            match &pdu_mapping.pdu {
                PDU::ISignalIPDU(ipdu) => {
                    period_secs = ipdu.cyclic_timing_period_value;
                    offset_secs = ipdu.cyclic_timing_offset_value;

                    data_bytes = Some(extract_init_values(
                        ipdu.unused_bit_pattern,
                        &ipdu.ungrouped_signals,
                        &ipdu.grouped_signals,
                        triggering.frame_length,
                        &pdu_mapping.byte_order,
                    ));
                    break; // use the first ISignalIPDU
                }
                PDU::NMPDU(nmpdu) => {
                    // NM PDUs have no cyclic timing on their own; build the
                    // data but leave period_secs at 0 so they won't be
                    // scheduled for now.
                    data_bytes = Some(extract_init_values(
                        nmpdu.unused_bit_pattern,
                        &nmpdu.ungrouped_signals,
                        &nmpdu.grouped_signals,
                        triggering.frame_length,
                        &pdu_mapping.byte_order,
                    ));
                }
            }
        }

        // If we have no data at all (e.g. unsupported PDU types only), skip.
        let data = match data_bytes {
            Some(d) => d,
            None => {
                warn!(
                    can_id = *can_id,
                    frame = %triggering.frame_name,
                    "No supported PDU mapping found, skipping frame"
                );
                continue;
            }
        };

        let can_id_u32 = match u32::try_from(*can_id) {
            Ok(id) => id,
            Err(_) => {
                warn!(
                    can_id = *can_id,
                    frame = %triggering.frame_name,
                    "CAN ID out of u32 range, skipping frame"
                );
                continue;
            }
        };
        let frame = CanFrame::new(can_id_u32, &data, extended);

        let label = format!(
            "{}(0x{:X})",
            triggering.frame_name, can_id
        );

        frames.push(ScheduledFrame {
            frame,
            period_secs,
            offset_secs,
            label,
        });
    }

    frames
}

// ---- Simulation engine ----

/// Configuration for the restbus simulation.
pub struct RestbusSimulationConfig {
    /// Name of the SocketCAN interface to send frames on (e.g. "vcan0").
    pub interface_name: String,
}

impl Default for RestbusSimulationConfig {
    fn default() -> Self {
        Self {
            interface_name: "vcan0".to_string(),
        }
    }
}

/// Handle returned by [`RestbusSimulation::start`] that can be used to stop
/// the simulation.
pub struct SimulationHandle {
    stop_tx: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl SimulationHandle {
    /// Signal all frame-sender tasks to stop and await their completion.
    pub async fn stop(self) {
        info!("Stopping restbus simulation");
        // Signal stop
        let _ = self.stop_tx.send(true);
        // Wait for all tasks
        for task in self.tasks {
            let _ = task.await;
        }
        info!("Restbus simulation stopped");
    }
}

/// The restbus simulation engine.
///
/// Takes one or more parsed [`CanCluster`] definitions and replays their
/// frames on a local SocketCAN interface, respecting cyclic timing periods
/// and initial signal values.
pub struct RestbusSimulation;

impl RestbusSimulation {
    /// Start the restbus simulation for the given clusters.
    ///
    /// A separate tokio task is spawned for each frame that has a non-zero
    /// cyclic timing period. The returned [`SimulationHandle`] can be used to
    /// gracefully stop all tasks.
    pub async fn start(
        clusters: &HashMap<String, CanCluster>,
        config: RestbusSimulationConfig,
    ) -> io::Result<SimulationHandle> {
        let socket = Arc::new(CanSocket::open(&config.interface_name)?);
        info!(
            interface = %config.interface_name,
            cluster_count = clusters.len(),
            "Starting restbus simulation"
        );

        let (stop_tx, _) = watch::channel(false);
        let mut tasks: Vec<JoinHandle<()>> = Vec::new();

        for (cluster_name, cluster) in clusters {
            let scheduled_frames = build_scheduled_frames(cluster);
            let cyclic_count = scheduled_frames
                .iter()
                .filter(|sf| sf.period_secs > 0.0)
                .count();

            info!(
                cluster = %cluster_name,
                total_frames = scheduled_frames.len(),
                cyclic_frames = cyclic_count,
                "Prepared frames for cluster"
            );

            for sf in scheduled_frames {
                if sf.period_secs <= 0.0 {
                    debug!(
                        frame = %sf.label,
                        "Skipping non-cyclic frame (period = 0)"
                    );
                    continue;
                }

                let socket = Arc::clone(&socket);
                let mut stop_rx = stop_tx.subscribe();

                let task = tokio::spawn(async move {
                    let period = Duration::from_secs_f64(sf.period_secs);
                    let offset = Duration::from_secs_f64(sf.offset_secs);

                    debug!(
                        frame = %sf.label,
                        period_ms = period.as_millis(),
                        offset_ms = offset.as_millis(),
                        "Scheduling cyclic frame"
                    );

                    // Apply initial offset
                    if !offset.is_zero() {
                        tokio::select! {
                            _ = time::sleep(offset) => {}
                            _ = stop_rx.changed() => { return; }
                        }
                    }

                    let mut interval = time::interval_at(Instant::now() + period, period);
                    // Send the first frame immediately after offset
                    if let Err(e) = socket.send_frame(&sf.frame) {
                        error!(frame = %sf.label, error = %e, "Failed to send CAN frame");
                    }

                    loop {
                        tokio::select! {
                            _ = interval.tick() => {
                                if let Err(e) = socket.send_frame(&sf.frame) {
                                    error!(
                                        frame = %sf.label,
                                        error = %e,
                                        "Failed to send CAN frame"
                                    );
                                }
                            }
                            _ = stop_rx.changed() => {
                                debug!(frame = %sf.label, "Frame sender stopping");
                                break;
                            }
                        }
                    }
                });

                tasks.push(task);
            }
        }

        info!(task_count = tasks.len(), "Restbus simulation running");

        Ok(SimulationHandle { stop_tx, tasks })
    }
}

// ---- Tests ----

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a minimal ISignalIPDU with a given cyclic period.
    fn make_isignal_ipdu(period: f64, offset: f64, signals: Vec<ISignal>) -> ISignalIPDU {
        ISignalIPDU {
            cyclic_timing_period_value: period,
            cyclic_timing_period_tolerance: None,
            cyclic_timing_offset_value: offset,
            cyclic_timing_offset_tolerance: None,
            number_of_repetitions: 0,
            repetition_period_value: 0.0,
            repetition_period_tolerance: None,
            unused_bit_pattern: false,
            ungrouped_signals: signals,
            grouped_signals: Vec::new(),
        }
    }

    /// Helper: create a CanFrameTriggering with a single ISignalIPDU mapping.
    fn make_triggering(
        name: &str,
        can_id: i64,
        frame_length: i64,
        ipdu: ISignalIPDU,
    ) -> CanFrameTriggering {
        CanFrameTriggering {
            frame_triggering_name: format!("{}_Triggering", name),
            frame_name: name.to_string(),
            can_id,
            addressing_mode: "Standard".to_string(),
            frame_rx_behavior: String::new(),
            frame_tx_behavior: String::new(),
            rx_range_lower: 0,
            rx_range_upper: 0,
            sender_ecus: vec!["ECU1".to_string()],
            receiver_ecus: vec!["ECU2".to_string()],
            frame_length,
            pdu_mappings: vec![PDUMapping {
                name: format!("{}_PDU", name),
                byte_order: false, // little-endian
                start_position: 0,
                length: frame_length,
                dynamic_length: String::new(),
                category: String::new(),
                contained_header_id_short: String::new(),
                contained_header_id_long: String::new(),
                pdu: PDU::ISignalIPDU(ipdu),
            }],
        }
    }

    #[test]
    fn test_can_frame_standard_id() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let frame = CanFrame::new(0x123, &data, false);

        assert_eq!(frame.can_id, 0x123);
        assert_eq!(frame.len, 4);
        assert_eq!(&frame.data[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn test_can_frame_extended_id() {
        let data = [0x01, 0x02];
        let frame = CanFrame::new(0x1234_5678, &data, true);

        assert_eq!(frame.can_id, 0x1234_5678 | CAN_EFF_FLAG);
        assert_eq!(frame.len, 2);
    }

    #[test]
    fn test_can_frame_truncates_to_8_bytes() {
        let data = [0u8; 12];
        let frame = CanFrame::new(0x100, &data, false);

        assert_eq!(frame.len, 8);
    }

    #[test]
    fn test_can_frame_as_bytes_length() {
        let frame = CanFrame::new(0x100, &[1, 2, 3], false);
        assert_eq!(frame.as_bytes().len(), CAN_MTU);
    }

    #[test]
    fn test_build_scheduled_frames_with_cyclic_ipdu() {
        let signal = ISignal {
            name: "TestSignal".to_string(),
            byte_order: false,
            start_pos: 0,
            length: 8,
            init_values: InitValues::Single(0xAB),
        };

        let ipdu = make_isignal_ipdu(0.01, 0.0, vec![signal]);
        let triggering = make_triggering("TestFrame", 0x100, 1, ipdu);

        let mut can_frame_triggerings = HashMap::new();
        can_frame_triggerings.insert(0x100_i64, triggering);

        let cluster = CanCluster {
            name: "TestCluster".to_string(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings,
        };

        let mut clusters = HashMap::new();
        clusters.insert("TestCluster".to_string(), cluster);

        let frames = build_scheduled_frames(clusters.get("TestCluster").unwrap());

        assert_eq!(frames.len(), 1);
        assert!((frames[0].period_secs - 0.01).abs() < f64::EPSILON);
        assert!((frames[0].offset_secs - 0.0).abs() < f64::EPSILON);
        assert_eq!(frames[0].frame.can_id, 0x100);
    }

    #[test]
    fn test_build_scheduled_frames_skips_zero_period() {
        let ipdu = make_isignal_ipdu(0.0, 0.0, Vec::new());
        let triggering = make_triggering("NoTimingFrame", 0x200, 8, ipdu);

        let mut can_frame_triggerings = HashMap::new();
        can_frame_triggerings.insert(0x200_i64, triggering);

        let cluster = CanCluster {
            name: "TestCluster".to_string(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings,
        };

        let frames = build_scheduled_frames(&cluster);

        // Frame is built but has zero period
        assert_eq!(frames.len(), 1);
        assert!((frames[0].period_secs).abs() < f64::EPSILON);
    }

    #[test]
    fn test_build_scheduled_frames_with_offset() {
        let signal = ISignal {
            name: "Sig1".to_string(),
            byte_order: false,
            start_pos: 0,
            length: 16,
            init_values: InitValues::Single(0x1234),
        };

        let ipdu = make_isignal_ipdu(0.1, 0.05, vec![signal]);
        let triggering = make_triggering("OffsetFrame", 0x300, 2, ipdu);

        let mut can_frame_triggerings = HashMap::new();
        can_frame_triggerings.insert(0x300_i64, triggering);

        let cluster = CanCluster {
            name: "OffsetCluster".to_string(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings,
        };

        let frames = build_scheduled_frames(&cluster);

        assert_eq!(frames.len(), 1);
        assert!((frames[0].period_secs - 0.1).abs() < f64::EPSILON);
        assert!((frames[0].offset_secs - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn test_build_scheduled_frames_nm_pdu_no_timing() {
        let signal = ISignal {
            name: "NmSig".to_string(),
            byte_order: false,
            start_pos: 0,
            length: 8,
            init_values: InitValues::Single(0x01),
        };

        let nm_pdu = NMPDU {
            unused_bit_pattern: false,
            ungrouped_signals: vec![signal],
            grouped_signals: Vec::new(),
        };

        let triggering = CanFrameTriggering {
            frame_triggering_name: "NmTriggering".to_string(),
            frame_name: "NmFrame".to_string(),
            can_id: 0x400,
            addressing_mode: "Standard".to_string(),
            frame_rx_behavior: String::new(),
            frame_tx_behavior: String::new(),
            rx_range_lower: 0,
            rx_range_upper: 0,
            sender_ecus: Vec::new(),
            receiver_ecus: Vec::new(),
            frame_length: 1,
            pdu_mappings: vec![PDUMapping {
                name: "NmPduMapping".to_string(),
                byte_order: false,
                start_position: 0,
                length: 1,
                dynamic_length: String::new(),
                category: String::new(),
                contained_header_id_short: String::new(),
                contained_header_id_long: String::new(),
                pdu: PDU::NMPDU(nm_pdu),
            }],
        };

        let mut can_frame_triggerings = HashMap::new();
        can_frame_triggerings.insert(0x400_i64, triggering);

        let cluster = CanCluster {
            name: "NmCluster".to_string(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings,
        };

        let frames = build_scheduled_frames(&cluster);

        // NM PDU has no timing, so period should be 0
        assert_eq!(frames.len(), 1);
        assert!((frames[0].period_secs).abs() < f64::EPSILON);
    }

    #[test]
    fn test_build_scheduled_frames_extended_addressing() {
        let ipdu = make_isignal_ipdu(0.02, 0.0, Vec::new());

        let triggering = CanFrameTriggering {
            frame_triggering_name: "ExtTriggering".to_string(),
            frame_name: "ExtFrame".to_string(),
            can_id: 0x1800_0001,
            addressing_mode: "Extended".to_string(),
            frame_rx_behavior: String::new(),
            frame_tx_behavior: String::new(),
            rx_range_lower: 0,
            rx_range_upper: 0,
            sender_ecus: Vec::new(),
            receiver_ecus: Vec::new(),
            frame_length: 8,
            pdu_mappings: vec![PDUMapping {
                name: "ExtPdu".to_string(),
                byte_order: false,
                start_position: 0,
                length: 8,
                dynamic_length: String::new(),
                category: String::new(),
                contained_header_id_short: String::new(),
                contained_header_id_long: String::new(),
                pdu: PDU::ISignalIPDU(ipdu),
            }],
        };

        let mut can_frame_triggerings = HashMap::new();
        can_frame_triggerings.insert(0x1800_0001_i64, triggering);

        let cluster = CanCluster {
            name: "ExtCluster".to_string(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings,
        };

        let frames = build_scheduled_frames(&cluster);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame.can_id, 0x1800_0001 | CAN_EFF_FLAG);
    }

    #[test]
    fn test_build_scheduled_frames_multiple_signals() {
        let sig1 = ISignal {
            name: "Sig1".to_string(),
            byte_order: false,
            start_pos: 0,
            length: 8,
            init_values: InitValues::Single(0xFF),
        };
        let sig2 = ISignal {
            name: "Sig2".to_string(),
            byte_order: false,
            start_pos: 8,
            length: 8,
            init_values: InitValues::Single(0x00),
        };

        let ipdu = make_isignal_ipdu(0.05, 0.0, vec![sig1, sig2]);
        let triggering = make_triggering("MultiSigFrame", 0x500, 2, ipdu);

        let mut can_frame_triggerings = HashMap::new();
        can_frame_triggerings.insert(0x500_i64, triggering);

        let cluster = CanCluster {
            name: "MultiSigCluster".to_string(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings,
        };

        let frames = build_scheduled_frames(&cluster);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame.len, 2);
    }

    #[test]
    fn test_build_empty_cluster() {
        let cluster = CanCluster {
            name: "EmptyCluster".to_string(),
            baudrate: 500_000,
            canfd_baudrate: 0,
            can_frame_triggerings: HashMap::new(),
        };

        let frames = build_scheduled_frames(&cluster);
        assert!(frames.is_empty());
    }
}
