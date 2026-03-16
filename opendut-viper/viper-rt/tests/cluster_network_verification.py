# VIPER_VERSION = 1.0
from viper import unittest, parameters, metadata

METADATA = metadata.Metadata(
    display_name="Cluster Network Verification",
    description="Verifies Ethernet bridge reachability and CAN bus frame forwarding across a two-peer openDuT cluster."
)

# ---------------------------------------------------------------------------
# Parameters
# ---------------------------------------------------------------------------

BRIDGE_NAME = parameters.TextParameter(
    "bridge_name",
    default="br-opendut",
    display_name="Bridge Name",
    description="Name of the Ethernet bridge interface on the EDGAR peer."
)

PEER_A_IP = parameters.TextParameter(
    "peer_a_ip",
    display_name="Peer A IP",
    description="IP address of the first peer in the cluster."
)

PEER_B_IP = parameters.TextParameter(
    "peer_b_ip",
    display_name="Peer B IP",
    description="IP address of the second peer in the cluster."
)

ETH_INTERFACES = parameters.TextParameter(
    "eth_interfaces",
    default="eth0",
    display_name="Ethernet Interfaces",
    description="Comma-separated list of Ethernet interface names to verify on the bridge."
)

CAN_INTERFACES = parameters.TextParameter(
    "can_interfaces",
    default="can0",
    display_name="CAN Interfaces",
    description="Comma-separated list of CAN bus interface names to verify for frame forwarding."
)

PING_COUNT = parameters.NumberParameter(
    "ping_count",
    default=3,
    min=1,
    max=100,
    display_name="Ping Count",
    description="Number of ICMP echo requests to send per interface."
)

PING_TIMEOUT = parameters.NumberParameter(
    "ping_timeout",
    default=5,
    min=1,
    max=60,
    display_name="Ping Timeout",
    description="Timeout in seconds for each ping probe."
)

CAN_FRAME_ID = parameters.TextParameter(
    "can_frame_id",
    default="123",
    display_name="CAN Frame ID",
    description="CAN arbitration ID (hex) used for the forwarding test."
)

CAN_FRAME_DATA = parameters.TextParameter(
    "can_frame_data",
    default="DEADBEEF",
    display_name="CAN Frame Data",
    description="CAN frame payload (hex) used for the forwarding test."
)

CAN_TIMEOUT = parameters.NumberParameter(
    "can_timeout",
    default=10,
    min=1,
    max=60,
    display_name="CAN Timeout",
    description="Timeout in seconds to wait for a CAN frame on the receiver side."
)

CONTAINER_IMAGE = parameters.TextParameter(
    "container_image",
    default="docker.io/library/alpine:latest",
    display_name="Container Image",
    description="Container image used for network probe containers."
)

CAN_UTILS_IMAGE = parameters.TextParameter(
    "can_utils_image",
    default="docker.io/library/alpine:latest",
    display_name="CAN Utils Image",
    description="Container image with can-utils installed, used for CAN bus tests."
)

VERBOSE = parameters.BooleanParameter(
    "verbose",
    default=False,
    display_name="Verbose Output",
    description="Enable verbose logging of container output in the report."
)


# ---------------------------------------------------------------------------
# Ethernet Bridge Reachability
# ---------------------------------------------------------------------------

class EthernetBridgeReachabilityTestCase(unittest.TestCase):
    """Verify that each Ethernet interface on the bridge is reachable
    from the remote peer via ICMP ping.  One sub-test runs per interface
    and the result is recorded as a report property."""

    @classmethod
    def setUpClass(cls):
        cls.interfaces = []

    def setUp(self):
        self.bridge = self.parameters.get(BRIDGE_NAME)
        self.peer_a = self.parameters.get(PEER_A_IP)
        self.peer_b = self.parameters.get(PEER_B_IP)
        self.count = self.parameters.get(PING_COUNT)
        self.timeout = self.parameters.get(PING_TIMEOUT)
        self.image = self.parameters.get(CONTAINER_IMAGE)
        self.verbose_mode = self.parameters.get(VERBOSE)

        raw = self.parameters.get(ETH_INTERFACES)
        self.interfaces = [iface.strip() for iface in raw.split(",") if iface.strip()]

    def tearDown(self):
        pass

    # -- per-interface ping tests ------------------------------------------

    def test_bridge_peer_a_to_peer_b(self):
        """Ping Peer B from Peer A over every configured Ethernet interface."""
        target_ip = self.parameters.get(PEER_B_IP)
        for iface in self.interfaces:
            self._ping_interface(iface, target_ip, direction="a_to_b")

    def test_bridge_peer_b_to_peer_a(self):
        """Ping Peer A from Peer B over every configured Ethernet interface."""
        target_ip = self.parameters.get(PEER_A_IP)
        for iface in self.interfaces:
            self._ping_interface(iface, target_ip, direction="b_to_a")

    # -- helper ------------------------------------------------------------

    def _ping_interface(self, iface, target_ip, direction):
        container_name = f"ping-{direction}-{iface}"
        count = self.parameters.get(PING_COUNT)
        timeout = self.parameters.get(PING_TIMEOUT)
        image = self.parameters.get(CONTAINER_IMAGE)

        self.container.remove(container_name)

        cid = self.container.create(
            image,
            ["-c", str(count), "-W", str(timeout), "-I", iface, target_ip],
            entrypoint=["ping"],
            name=container_name,
            network="host"
        )

        self.container.start(cid)
        exit_code = self.container.wait(cid)

        log_lines = self.container.log(cid)
        if self.parameters.get(VERBOSE):
            for line in log_lines:
                print(f"[{container_name}] {line}")

        result = "PASS" if exit_code == 0 else "FAIL"
        self.report.property(f"eth_{direction}_{iface}_result", result)
        self.report.property(f"eth_{direction}_{iface}_exit_code", exit_code)

        self.container.remove(container_name)

        self.assertEquals(
            0, exit_code,
            f"Ping {direction} over {iface} to {target_ip} failed (exit_code={exit_code})"
        )


# ---------------------------------------------------------------------------
# CAN Bus Frame Forwarding
# ---------------------------------------------------------------------------

class CanBusForwardingTestCase(unittest.TestCase):
    """Verify that CAN frames sent on one peer are received on the
    other peer for each configured CAN interface.  Uses can-utils
    inside containers to send (cansend) and receive (candump)."""

    @classmethod
    def setUpClass(cls):
        cls.interfaces = []

    def setUp(self):
        self.peer_a = self.parameters.get(PEER_A_IP)
        self.peer_b = self.parameters.get(PEER_B_IP)
        self.frame_id = self.parameters.get(CAN_FRAME_ID)
        self.frame_data = self.parameters.get(CAN_FRAME_DATA)
        self.can_timeout = self.parameters.get(CAN_TIMEOUT)
        self.image = self.parameters.get(CAN_UTILS_IMAGE)
        self.verbose_mode = self.parameters.get(VERBOSE)

        raw = self.parameters.get(CAN_INTERFACES)
        self.interfaces = [iface.strip() for iface in raw.split(",") if iface.strip()]

    def tearDown(self):
        pass

    # -- per-interface CAN forwarding tests --------------------------------

    def test_can_forward_a_to_b(self):
        """Send a CAN frame on Peer A and verify reception on Peer B."""
        for iface in self.interfaces:
            self._can_forward_test(iface, direction="a_to_b")

    def test_can_forward_b_to_a(self):
        """Send a CAN frame on Peer B and verify reception on Peer A."""
        for iface in self.interfaces:
            self._can_forward_test(iface, direction="b_to_a")

    # -- helper ------------------------------------------------------------

    def _can_forward_test(self, iface, direction):
        frame_id = self.parameters.get(CAN_FRAME_ID)
        frame_data = self.parameters.get(CAN_FRAME_DATA)
        can_timeout = self.parameters.get(CAN_TIMEOUT)
        image = self.parameters.get(CAN_UTILS_IMAGE)

        recv_name = f"candump-{direction}-{iface}"
        send_name = f"cansend-{direction}-{iface}"

        # Clean up any previous containers
        self.container.remove(recv_name)
        self.container.remove(send_name)

        # Start receiver first (candump with a timeout)
        recv_cid = self.container.create(
            image,
            ["-c",
             f"timeout {can_timeout} candump {iface},0x{frame_id}:0x7FF -n 1"],
            entrypoint=["sh"],
            name=recv_name,
            network="host"
        )
        self.container.start(recv_cid)

        # Send the CAN frame
        send_cid = self.container.create(
            image,
            ["-c", f"cansend {iface} {frame_id}#{frame_data}"],
            entrypoint=["sh"],
            name=send_name,
            network="host"
        )
        self.container.start(send_cid)
        send_exit = self.container.wait(send_cid)

        # Wait for the receiver
        recv_exit = self.container.wait(recv_cid)

        # Collect logs
        send_log = self.container.log(send_name)
        recv_log = self.container.log(recv_name)

        if self.parameters.get(VERBOSE):
            for line in send_log:
                print(f"[{send_name}] {line}")
            for line in recv_log:
                print(f"[{recv_name}] {line}")

        send_result = "PASS" if send_exit == 0 else "FAIL"
        recv_result = "PASS" if recv_exit == 0 else "FAIL"

        self.report.property(f"can_{direction}_{iface}_send_result", send_result)
        self.report.property(f"can_{direction}_{iface}_send_exit_code", send_exit)
        self.report.property(f"can_{direction}_{iface}_recv_result", recv_result)
        self.report.property(f"can_{direction}_{iface}_recv_exit_code", recv_exit)

        # Clean up
        self.container.remove(send_name)
        self.container.remove(recv_name)

        self.assertEquals(
            0, send_exit,
            f"cansend {direction} on {iface} failed (exit_code={send_exit})"
        )
        self.assertEquals(
            0, recv_exit,
            f"candump {direction} on {iface} failed to receive frame (exit_code={recv_exit})"
        )
