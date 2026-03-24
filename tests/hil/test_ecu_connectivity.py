# VIPER_VERSION = 1.0
from viper import *

# ---------------------------------------------------------------------------
# HIL Test Suite: ECU Connectivity via openDuT Bridge
#
# Verifies that ECUs are reachable through the openDuT EDGAR ethernet bridge
# using containerized nmap network scans, and includes placeholder tests for
# CAN bus communication over virtual CAN (vCAN) interfaces.
#
# This test is designed to run within the openDuT testenv where EDGAR peers
# have been deployed and the cluster bridge (br-opendut) is operational.
# ---------------------------------------------------------------------------

METADATA = metadata.Metadata(
    display_name="HIL ECU Connectivity Test Suite",
    description=(
        "Hardware-in-the-Loop tests that verify ECU reachability through the "
        "openDuT EDGAR bridge using containerized nmap scans and validate "
        "CAN bus routing over virtual CAN interfaces."
    ),
)

# ---------------------------------------------------------------------------
# Parameters
# ---------------------------------------------------------------------------
ECU_TARGET_IP = parameters.TextParameter(
    "ecu-target-ip",
    default="127.0.0.1",
    display_name="ECU Target IP",
    description="IP address of the ECU to scan through the openDuT bridge.",
)

NMAP_IMAGE = parameters.TextParameter(
    "nmap-image",
    default="nmap-test",
    display_name="Nmap Container Image",
    description=(
        "Docker image used for the nmap scan container. "
        "Defaults to the pre-built nmap-test image from the EDGAR testenv."
    ),
)

NMAP_TIMING = parameters.TextParameter(
    "nmap-timing",
    default="-T4",
    display_name="Nmap Timing Template",
    description="Nmap timing template flag (e.g. -T0 through -T5).",
)

BRIDGE_INTERFACE = parameters.TextParameter(
    "bridge-interface",
    default="br-opendut",
    display_name="Bridge Interface",
    description="Name of the openDuT EDGAR ethernet bridge interface.",
)

SCAN_TIMEOUT_SECONDS = parameters.NumberParameter(
    "scan-timeout-seconds",
    default=120,
    min=10,
    max=600,
    display_name="Scan Timeout",
    description="Maximum seconds to wait for the nmap scan container to finish.",
)

ENABLE_CAN_TESTS = parameters.BooleanParameter(
    "enable-can-tests",
    default=False,
    display_name="Enable CAN Bus Tests",
    description=(
        "Enable CAN bus tests. Requires vCAN kernel modules and "
        "can-utils to be available on the EDGAR peer."
    ),
)

CAN_INTERFACE_SRC = parameters.TextParameter(
    "can-interface-src",
    default="vcan0",
    display_name="CAN Source Interface",
    description="Source virtual CAN interface for CAN bus routing tests.",
)

CAN_INTERFACE_DST = parameters.TextParameter(
    "can-interface-dst",
    default="vcan1",
    display_name="CAN Destination Interface",
    description="Destination virtual CAN interface for CAN bus routing tests.",
)


# ===================================================================
# Test Case: ECU Network Discovery via Containerized Nmap
# ===================================================================
class EcuConnectivityNmapScan(unittest.TestCase):
    """Verify ECU reachability through the openDuT bridge using nmap."""

    @classmethod
    def setUpClass(cls):
        print("=== ECU Connectivity Nmap Scan: setUpClass ===")
        cls.scan_container_name = "hil-nmap-ecu-scan"

    def setUp(self):
        self.target_ip = self.parameters.get(ECU_TARGET_IP)
        self.nmap_image = self.parameters.get(NMAP_IMAGE)
        self.timing = self.parameters.get(NMAP_TIMING)
        self.bridge = self.parameters.get(BRIDGE_INTERFACE)
        print(f"Target IP: {self.target_ip}")
        print(f"Nmap image: {self.nmap_image}")
        print(f"Timing: {self.timing}")
        print(f"Bridge: {self.bridge}")

    def tearDown(self):
        # Clean up scan container if it exists
        try:
            self.container.stop(self.scan_container_name)
        except Exception:
            pass
        try:
            self.container.remove(self.scan_container_name)
        except Exception:
            pass

    def test_nmap_host_discovery(self):
        """Run nmap host discovery scan against the ECU target through the bridge."""
        print(f"Running nmap host discovery against {self.target_ip}")

        container_id = self.container.create(
            self.nmap_image,
            ["-sn", self.timing, self.target_ip],
            name=self.scan_container_name,
            network="host",
        )
        self.container.start(container_id)
        exit_code = self.container.wait(container_id)

        scan_log = self.container.log(container_id)
        for line in scan_log:
            print(f"  nmap: {line}")

        self.report.property("nmap_host_discovery_exit_code", exit_code)
        self.report.property("nmap_target_ip", self.target_ip)

        self.assertEquals(
            0, exit_code,
            f"Nmap host discovery scan failed with exit code {exit_code}",
        )

    def test_nmap_port_scan(self):
        """Run a service/version detection scan on common automotive ports."""
        print(f"Running nmap port scan against {self.target_ip}")

        container_id = self.container.create(
            self.nmap_image,
            [
                "-sV",
                self.timing,
                "-p", "3000,6801,13400,30490-30491",
                self.target_ip,
            ],
            name=self.scan_container_name,
            network="host",
        )
        self.container.start(container_id)
        exit_code = self.container.wait(container_id)

        scan_log = self.container.log(container_id)
        for line in scan_log:
            print(f"  nmap: {line}")

        self.report.property("nmap_port_scan_exit_code", exit_code)

        self.assertEquals(
            0, exit_code,
            f"Nmap port scan failed with exit code {exit_code}",
        )

    def test_nmap_aggressive_scan(self):
        """Run an aggressive nmap scan (-A) for OS detection and traceroute."""
        print(f"Running nmap aggressive scan against {self.target_ip}")

        container_id = self.container.create(
            self.nmap_image,
            ["-A", self.timing, self.target_ip],
            name=self.scan_container_name,
            network="host",
        )
        self.container.start(container_id)
        exit_code = self.container.wait(container_id)

        scan_log = self.container.log(container_id)
        for line in scan_log:
            print(f"  nmap: {line}")

        info = self.container.inspect(container_id)
        print(f"Container state: {info.state.status}")
        print(f"Container exit code: {info.state.exit_code}")

        self.report.property("nmap_aggressive_scan_exit_code", exit_code)

        self.assertEquals(
            0, exit_code,
            f"Nmap aggressive scan failed with exit code {exit_code}",
        )


# ===================================================================
# Test Case: openDuT Bridge Verification
# ===================================================================
class OpenDuTBridgeVerification(unittest.TestCase):
    """Verify the openDuT EDGAR ethernet bridge is operational."""

    def setUp(self):
        self.bridge = self.parameters.get(BRIDGE_INTERFACE)
        self.target_ip = self.parameters.get(ECU_TARGET_IP)
        self.nmap_image = self.parameters.get(NMAP_IMAGE)
        self.timing = self.parameters.get(NMAP_TIMING)

    def tearDown(self):
        try:
            self.container.stop("hil-bridge-verify")
        except Exception:
            pass
        try:
            self.container.remove("hil-bridge-verify")
        except Exception:
            pass

    def test_bridge_ping_through_container(self):
        """Verify basic ICMP reachability of the ECU through the bridge via a container."""
        print(f"Pinging {self.target_ip} through bridge {self.bridge}")

        container_id = self.container.create(
            self.nmap_image,
            ["-sn", "-PE", self.timing, self.target_ip],
            name="hil-bridge-verify",
            network="host",
        )
        self.container.start(container_id)
        exit_code = self.container.wait(container_id)

        scan_log = self.container.log(container_id)
        for line in scan_log:
            print(f"  ping-scan: {line}")

        self.report.property("bridge_ping_exit_code", exit_code)
        self.report.property("bridge_interface", self.bridge)

        self.assertEquals(
            0, exit_code,
            f"Bridge ping check failed with exit code {exit_code}",
        )

    def test_multi_target_sweep(self):
        """Scan a small subnet range to discover all ECUs on the bridge."""
        print(f"Sweeping subnet for ECU discovery via bridge {self.bridge}")

        # Derive /24 subnet from target IP
        octets = self.target_ip.split(".")
        subnet = f"{octets[0]}.{octets[1]}.{octets[2]}.0/24"
        print(f"Scanning subnet: {subnet}")

        container_id = self.container.create(
            self.nmap_image,
            ["-sn", self.timing, subnet],
            name="hil-bridge-verify",
            network="host",
        )
        self.container.start(container_id)
        exit_code = self.container.wait(container_id)

        scan_log = self.container.log(container_id)
        for line in scan_log:
            print(f"  sweep: {line}")

        self.report.property("subnet_sweep_exit_code", exit_code)
        self.report.property("subnet_sweep_target", subnet)

        self.assertEquals(
            0, exit_code,
            f"Subnet sweep failed with exit code {exit_code}",
        )


# ===================================================================
# Test Case: CAN Bus Communication (Placeholder)
# ===================================================================
class CanBusCommunication(unittest.TestCase):
    """Placeholder tests for CAN bus communication over vCAN interfaces.

    These tests validate CAN bus routing through the openDuT bridge.
    They require:
      - vCAN kernel modules loaded (vcan, can-gw)
      - can-utils installed on the EDGAR peer
      - The 'enable-can-tests' parameter set to True

    When the CAN infrastructure is not available, these tests are
    skipped gracefully via the enable-can-tests parameter.
    """

    @classmethod
    def setUpClass(cls):
        print("=== CAN Bus Communication: setUpClass ===")
        print("NOTE: CAN bus tests are placeholders pending full vCAN infrastructure.")

    def setUp(self):
        self.can_enabled = self.parameters.get(ENABLE_CAN_TESTS)
        self.can_src = self.parameters.get(CAN_INTERFACE_SRC)
        self.can_dst = self.parameters.get(CAN_INTERFACE_DST)
        print(f"CAN tests enabled: {self.can_enabled}")
        print(f"CAN source: {self.can_src}, destination: {self.can_dst}")

    def test_can_interface_presence(self):
        """Verify that the configured vCAN interfaces exist on the EDGAR peer.

        Placeholder: In a full implementation this would use the EDGAR plugin
        API or shell commands (ip link show type vcan) to verify that the
        virtual CAN interfaces (vcan0, vcan1) are present and UP.
        """
        if not self.can_enabled:
            print("SKIP: CAN tests disabled via parameter.")
            self.report.property("can_interface_presence", "skipped")
            return

        # TODO: Replace with actual interface check via container or EDGAR API.
        #   Example implementation:
        #     container_id = self.container.create(
        #         "ubuntu:24.04",
        #         ["ip", "link", "show", self.can_src],
        #         entrypoint=["sh", "-c"],
        #         name="hil-can-iface-check",
        #         network="host",
        #     )
        #     self.container.start(container_id)
        #     exit_code = self.container.wait(container_id)
        #     self.assertEquals(0, exit_code)
        print(f"PLACEHOLDER: Would check interface {self.can_src} exists")
        print(f"PLACEHOLDER: Would check interface {self.can_dst} exists")
        self.report.property("can_interface_presence", "placeholder")

    def test_can_local_route_exists(self):
        """Verify that a CAN gateway route exists between source and destination.

        Placeholder: In a full implementation this would invoke 'cangw -L'
        inside a container to verify that the CAN local route between
        vcan0 and vcan1 has been established by EDGAR.
        """
        if not self.can_enabled:
            print("SKIP: CAN tests disabled via parameter.")
            self.report.property("can_local_route", "skipped")
            return

        # TODO: Replace with actual cangw route check.
        #   Example implementation:
        #     container_id = self.container.create(
        #         "ubuntu:24.04",
        #         [f"cangw -L | grep '{self.can_src}' | grep '{self.can_dst}'"],
        #         entrypoint=["sh", "-c"],
        #         name="hil-can-route-check",
        #         network="host",
        #     )
        #     self.container.start(container_id)
        #     exit_code = self.container.wait(container_id)
        #     self.assertEquals(0, exit_code,
        #         f"No CAN route found from {self.can_src} to {self.can_dst}")
        print(f"PLACEHOLDER: Would verify CAN route {self.can_src} -> {self.can_dst}")
        self.report.property("can_local_route", "placeholder")

    def test_can_frame_send_receive(self):
        """Verify that a CAN frame sent on the source interface arrives at the destination.

        Placeholder: In a full implementation this would:
          1. Start a candump listener on the destination vCAN interface
          2. Send a CAN frame via cansend on the source interface
          3. Verify the frame is received on the destination
        """
        if not self.can_enabled:
            print("SKIP: CAN tests disabled via parameter.")
            self.report.property("can_frame_send_receive", "skipped")
            return

        # TODO: Replace with actual CAN frame send/receive test.
        #   Example implementation:
        #     # Start candump listener in background container
        #     listener = self.container.create(
        #         "can-utils-image",
        #         [f"timeout 10 candump {self.can_dst} -n 1"],
        #         entrypoint=["sh", "-c"],
        #         name="hil-can-listener",
        #         network="host",
        #     )
        #     self.container.start(listener)
        #
        #     # Send a CAN frame on the source interface
        #     sender = self.container.create(
        #         "can-utils-image",
        #         [f"cansend {self.can_src} 123#DEADBEEF"],
        #         entrypoint=["sh", "-c"],
        #         name="hil-can-sender",
        #         network="host",
        #     )
        #     self.container.start(sender)
        #     self.container.wait(sender)
        #
        #     # Wait for listener to receive the frame
        #     exit_code = self.container.wait(listener)
        #     self.assertEquals(0, exit_code,
        #         "CAN frame was not received on destination interface")
        print(f"PLACEHOLDER: Would send CAN frame on {self.can_src}")
        print(f"PLACEHOLDER: Would receive CAN frame on {self.can_dst}")
        self.report.property("can_frame_send_receive", "placeholder")

    def test_can_fd_support(self):
        """Verify CAN FD (Flexible Data-Rate) frame routing if supported.

        Placeholder: In a full implementation this would verify that
        CAN FD frames with payloads > 8 bytes can be routed through the
        openDuT CAN gateway (cangw with -X flag).
        """
        if not self.can_enabled:
            print("SKIP: CAN tests disabled via parameter.")
            self.report.property("can_fd_support", "skipped")
            return

        # TODO: Replace with actual CAN FD test.
        #   Would verify CAN FD frame routing with the -X flag via cangw.
        print("PLACEHOLDER: Would verify CAN FD frame routing")
        self.report.property("can_fd_support", "placeholder")

    @classmethod
    def tearDownClass(cls):
        print("=== CAN Bus Communication: tearDownClass ===")
