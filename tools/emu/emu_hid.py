"""Virtual Bluetooth link with a HID keyboard and mouse, for testing BlueCaster's HID host (issue #413).

Runs two Bumble virtual controllers on one in-process link, as emu_sink.py:
  - controller "BlueCaster" is served as H4 over TCP; BlueCaster connects to it with
    `bluecaster-core.exe --cli --hci-tcp 127.0.0.1:<port> --hid-test <s> --hid-device <address>`
  - controller "HID" is driven by a Bumble host acting as a classic HID device:
    one report descriptor for a keyboard (report ID 1), a mouse (report ID 2) and
    consumer keys (report ID 3), served in its SDP HID record

Once both HID channels are open (BlueCaster opened them, or the device did with
--initiate) the device sends a script of input reports on the interrupt channel.
The built-in script ends with a key held down; --disconnect-after drops the
link while it is still held.

Prints "READY" once BlueCaster can connect, then one line per step ("hid: ..."),
for run_test.py --hid. The device's HCI traffic is written to a PacketLogger
(.pklg) capture. Stops on SIGTERM / Ctrl+C.
"""

import argparse
import asyncio
import logging

import bumble.logging
from bumble import hci
from bumble import hid as bumble_hid
from bumble.core import (
    BT_BR_EDR_TRANSPORT,
    BT_HIDP_PROTOCOL_ID,
    BT_HUMAN_INTERFACE_DEVICE_SERVICE,
    BT_L2CAP_PROTOCOL_ID,
)
from bumble.device import Device
from bumble.host import Host
from bumble.link import LocalLink
from bumble.pairing import PairingConfig, PairingDelegate
from bumble.sdp import (
    SDP_ADDITIONAL_PROTOCOL_DESCRIPTOR_LIST_ATTRIBUTE_ID,
    SDP_BLUETOOTH_PROFILE_DESCRIPTOR_LIST_ATTRIBUTE_ID,
    SDP_BROWSE_GROUP_LIST_ATTRIBUTE_ID,
    SDP_LANGUAGE_BASE_ATTRIBUTE_ID_LIST_ATTRIBUTE_ID,
    SDP_PROTOCOL_DESCRIPTOR_LIST_ATTRIBUTE_ID,
    SDP_PUBLIC_BROWSE_ROOT,
    SDP_SERVICE_CLASS_ID_LIST_ATTRIBUTE_ID,
    SDP_SERVICE_RECORD_HANDLE_ATTRIBUTE_ID,
    DataElement,
    ServiceAttribute,
)
from bumble.transport import open_transport
from bumble.transport.common import AsyncPipeSink

from emu_sink import BLUECASTER_ADDRESS, DUAL_MODE_FEATURES, EmuController, PacketLoggerSnooper

HID_ADDRESS = '00:A2:D0:00:00:03'
HID_RECORD_HANDLE = 0x00010002

log = logging.getLogger('emu_hid')

# Keyboard (ID 1: modifiers, reserved byte, 6-key array; output: the 5
# boot keyboard lights and 3 bits of padding), mouse (ID 2: 5 buttons, 16-bit
# relative X / Y, wheel) and consumer keys (ID 3: volume up, volume down,
# mute, play/pause as 1-bit fields)
REPORT_DESCRIPTOR = bytes([
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x05, 0x07, 0x19, 0x00, 0x2A, 0xFF, 0x00, 0x81, 0x00,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x05, 0x91, 0x02,
    0x95, 0x01, 0x75, 0x03, 0x91, 0x01,
    0xC0,
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x03, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x06,
    0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06,
    0xC0, 0xC0,
    0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x04, 0x09, 0xE9, 0x09, 0xEA, 0x09, 0xE2, 0x09, 0xCD, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x04, 0x81, 0x01,
    0xC0,
])


def keyboard(modifiers=0, *keys):
    return bytes([1, modifiers, 0] + list(keys) + [0] * (6 - len(keys)))


def mouse(buttons=0, dx=0, dy=0, wheel=0):
    return bytes([2, buttons]) + dx.to_bytes(2, 'little', signed=True) + dy.to_bytes(2, 'little', signed=True) \
        + wheel.to_bytes(1, 'little', signed=True)


def consumer(bits=0):
    return bytes([3, bits])


# The built-in script. run_test.py's HID_SCRIPT_EVENTS is what BlueCaster's
# decoder makes of it: one event per key or button that goes down or up, one
# per report that moves, one per wheel step report.
BUILT_IN_SCRIPT = [
    keyboard(0, 0x04),         # A down: 1
    keyboard(),                # A up: 1
    keyboard(0x02, 0x05),      # Left Shift and B down: 2
    keyboard(),                # both up: 2
    mouse(dx=10, dy=-5),       # move: 1
    mouse(dx=-3, dy=7),        # move: 1
    mouse(buttons=1),          # primary button down: 1
    mouse(),                   # up: 1
    mouse(wheel=1),            # wheel: 1
    consumer(0x01),            # volume up down: 1
    consumer(),                # up: 1
    keyboard(0, 0x06),         # C down, left held: 1
]


def read_script(path):
    """Reports of a script file: one per line in hex, report ID first; '#'
    starts a comment"""
    reports = []
    with open(path, encoding='utf-8') as f:
        for line in f:
            line = line.split('#', 1)[0].strip()
            if line:
                reports.append(bytes.fromhex(line))
    return reports


def hid_sdp_records(handle):
    """The HID service record: control PSM, interrupt PSM and the report
    descriptor, as BTstack's HID host reads them"""
    l2cap_psm = lambda psm: DataElement.sequence([
        DataElement.sequence([DataElement.uuid(BT_L2CAP_PROTOCOL_ID), DataElement.unsigned_integer_16(psm)]),
        DataElement.sequence([DataElement.uuid(BT_HIDP_PROTOCOL_ID)]),
    ])
    u8 = lambda v: DataElement.unsigned_integer_8(v)
    u16 = lambda v: DataElement.unsigned_integer_16(v)
    return [
        ServiceAttribute(SDP_SERVICE_RECORD_HANDLE_ATTRIBUTE_ID, DataElement.unsigned_integer_32(handle)),
        ServiceAttribute(SDP_SERVICE_CLASS_ID_LIST_ATTRIBUTE_ID,
                         DataElement.sequence([DataElement.uuid(BT_HUMAN_INTERFACE_DEVICE_SERVICE)])),
        ServiceAttribute(SDP_PROTOCOL_DESCRIPTOR_LIST_ATTRIBUTE_ID, l2cap_psm(bumble_hid.HID_CONTROL_PSM)),
        ServiceAttribute(SDP_BROWSE_GROUP_LIST_ATTRIBUTE_ID, DataElement.sequence([DataElement.uuid(SDP_PUBLIC_BROWSE_ROOT)])),
        ServiceAttribute(SDP_LANGUAGE_BASE_ATTRIBUTE_ID_LIST_ATTRIBUTE_ID,
                         DataElement.sequence([u16(0x656E), u16(0x006A), u16(0x0100)])),
        ServiceAttribute(SDP_BLUETOOTH_PROFILE_DESCRIPTOR_LIST_ATTRIBUTE_ID, DataElement.sequence([
            DataElement.sequence([DataElement.uuid(BT_HUMAN_INTERFACE_DEVICE_SERVICE), u16(0x0101)])])),
        ServiceAttribute(SDP_ADDITIONAL_PROTOCOL_DESCRIPTOR_LIST_ATTRIBUTE_ID,
                         DataElement.sequence([l2cap_psm(bumble_hid.HID_INTERRUPT_PSM)])),
        ServiceAttribute(0x0100, DataElement.text_string(b'BlueCaster Test HID')),
        ServiceAttribute(0x0201, u16(0x0111)),          # HID parser version
        ServiceAttribute(0x0202, u8(0xC0)),             # subclass: combo keyboard / pointing
        ServiceAttribute(0x0203, u8(0x00)),             # country code
        ServiceAttribute(0x0204, DataElement.boolean(True)),   # virtual cable
        ServiceAttribute(0x0205, DataElement.boolean(True)),   # reconnect initiate
        ServiceAttribute(0x0206, DataElement.sequence([DataElement.sequence([
            u8(0x22), DataElement.text_string(REPORT_DESCRIPTOR)])])),
        ServiceAttribute(0x0207, DataElement.sequence([DataElement.sequence([u16(0x0409), u16(0x0100)])])),
        ServiceAttribute(0x020B, u16(0x0100)),          # HID profile version
        ServiceAttribute(0x020C, u16(0x0C80)),          # supervision timeout
        ServiceAttribute(0x020D, DataElement.boolean(True)),   # normally connectable
        ServiceAttribute(0x020E, DataElement.boolean(True)),   # boot device
    ]


class TypingDelegate(PairingDelegate):
    """A keyboard without a display that asks for a passkey: the user types
    the number the PC shows. Here run_test.py writes the number BlueCaster
    printed ("hid passkey shown <digits>") to a file, and the device types
    what it finds there."""

    def __init__(self, passkey_file, wait=20.0):
        super().__init__(io_capability=PairingDelegate.IoCapability.KEYBOARD_INPUT_ONLY)
        self.passkey_file = passkey_file
        self.wait = wait

    async def get_number(self):
        print('hid: asked to type a passkey', flush=True)
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self.wait
        while loop.time() < deadline:
            try:
                with open(self.passkey_file, encoding='ascii') as f:
                    digits = f.read().strip()
                if len(digits) == 6 and digits.isdigit():
                    print('hid: typing the passkey and Enter', flush=True)
                    return int(digits)
            except OSError:
                pass
            await asyncio.sleep(0.05)
        print('hid: no passkey to type', flush=True)
        return None


def typing_pairing_config(passkey_file):
    """Pairing as a keyboard that types a passkey, with MITM asked for"""
    return lambda connection: PairingConfig(sc=True, mitm=True, bonding=True,
                                            delegate=TypingDelegate(passkey_file))


class HidDevice(bumble_hid.Device):
    """Bumble's HID device, telling when its interrupt channel opens"""

    on_interrupt_open = None

    def on_l2cap_channel_open(self, l2cap_channel):
        super().on_l2cap_channel_open(l2cap_channel)
        channel = 'control' if l2cap_channel.psm == bumble_hid.HID_CONTROL_PSM else 'interrupt'
        print(f'hid: {channel} channel open', flush=True)
        if channel == 'interrupt' and self.on_interrupt_open:
            self.on_interrupt_open()


async def main(args):
    link = LocalLink()
    transport = await open_transport(f'tcp-server:127.0.0.1:{args.port}')
    bluecaster_controller = EmuController(
        'BlueCaster', host_source=transport.source, host_sink=transport.sink, link=link,
        public_address=BLUECASTER_ADDRESS)
    hid_controller = EmuController('HID', link=link, public_address=args.address)
    # BR/EDR controllers: with Bumble's default (LE only) BTstack skips its
    # classic setup and never turns page scan on
    bluecaster_controller.lmp_features = hid_controller.lmp_features = DUAL_MODE_FEATURES
    host = Host(hid_controller, AsyncPipeSink(hid_controller))
    if args.capture:
        host.snooper = PacketLoggerSnooper(args.capture)
    device = Device(name='BlueCaster Test HID', address=args.address, host=host)
    device.classic_enabled = True
    device.class_of_device = 0x0025C0  # Peripheral, combo keyboard / pointing device
    device.sdp_service_records = {HID_RECORD_HANDLE: hid_sdp_records(HID_RECORD_HANDLE)}
    if args.passkey_file:
        device.pairing_config_factory = typing_pairing_config(args.passkey_file)
    hid = HidDevice(device)
    # Report protocol, as BTstack asks for after a device opened the channels
    ok = bumble_hid.Device.GetSetReturn.SUCCESS
    hid.register_set_protocol_cb(lambda mode: bumble_hid.Device.GetSetStatus(status=ok))
    hid.register_get_protocol_cb(lambda: bumble_hid.Device.GetSetStatus(data=bytes([1]), status=ok))
    hid.register_get_report_cb(
        lambda report_id, report_type, size: bumble_hid.Device.GetSetStatus(
            status=bumble_hid.Device.GetSetReturn.REPORT_ID_NOT_FOUND))
    hid.register_set_report_cb(lambda *_: bumble_hid.Device.GetSetStatus(status=ok))

    def on_interrupt_data(pdu):
        # DATA | Output (0xA2): the keyboard lights, report ID first
        if pdu and pdu[0] == 0xA2:
            print(f'hid: output report {pdu[1:].hex(" ")}', flush=True)
    hid.on(hid.EVENT_INTERRUPT_DATA, on_interrupt_data)
    await device.power_on()
    await device.set_discoverable(True)
    await device.set_connectable(True)

    script = read_script(args.reports) if args.reports else BUILT_IN_SCRIPT
    state = {'sent': False}

    async def send_script():
        # BTstack reads the report descriptor over SDP after the channels of
        # a device that connected itself are open: input before that is lost
        await asyncio.sleep(args.start_delay)
        sent = 0
        for report in script:
            if hid.l2cap_intr_channel is None:
                break
            hid.send_data(report)
            sent += 1
            await asyncio.sleep(args.report_gap)
        print(f'hid: sent {sent} of {len(script)} reports', flush=True)
        if args.disconnect_after is not None and hid.connection is not None:
            await asyncio.sleep(args.disconnect_after)
            print('hid: dropping the link', flush=True)
            await hid.connection.disconnect(hci.HCI_REMOTE_DEVICE_TERMINATED_CONNECTION_DUE_TO_POWER_OFF_ERROR)

    def on_interrupt_open():
        if state['sent']:
            return
        state['sent'] = True
        task = asyncio.ensure_future(send_script())
        task.add_done_callback(lambda t: t.exception() and print(f'hid: script failed: {t.exception()!r}', flush=True))
    hid.on_interrupt_open = on_interrupt_open

    def on_connection(connection):
        print('hid: link up', flush=True)
        connection.on('disconnection', lambda reason: print(f'hid: link down (0x{reason:02X})', flush=True))
    device.on('connection', on_connection)

    async def initiate():
        # BlueCaster's stack is up once it enables page scan
        loop = asyncio.get_running_loop()
        deadline = loop.time() + args.wait_bluecaster
        while not bluecaster_controller.classic_scan_enable & 0x02 and loop.time() < deadline:
            await asyncio.sleep(0.1)
        if not bluecaster_controller.classic_scan_enable & 0x02:
            print('hid: BlueCaster is not connectable, paging it anyway', flush=True)
        await asyncio.sleep(args.initiate_delay)
        await device.connect(BLUECASTER_ADDRESS, transport=BT_BR_EDR_TRANSPORT)
        for name, open_channel in (('control', hid.connect_control_channel),
                                   ('interrupt', hid.connect_interrupt_channel)):
            try:
                await open_channel()
            except Exception as e:  # refused by BlueCaster: the result the test looks for
                print(f'hid: {name} channel refused ({type(e).__name__}: {e})', flush=True)
                return
            except asyncio.CancelledError:
                # No answer Bumble could match before the link went down (BTstack
                # swaps the CIDs when it refuses an unknown PSM)
                print(f'hid: {name} channel failed (the link went down first)', flush=True)
                return
            print(f'hid: {name} channel open', flush=True)
        on_interrupt_open()

    if args.initiate:
        task = asyncio.ensure_future(initiate())
        task.add_done_callback(lambda t: t.exception() and print(f'hid: paging failed: {t.exception()!r}', flush=True))
    print(f'READY 127.0.0.1:{args.port} hid={args.address}', flush=True)
    await asyncio.get_running_loop().create_future()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--port', type=int, default=9001, help='TCP port for BlueCaster (H4)')
    parser.add_argument('--capture', help='PacketLogger capture of the device HCI traffic')
    parser.add_argument('--log-level', default='WARNING')
    parser.add_argument('--address', default=HID_ADDRESS, help='the device address (default %(default)s)')
    parser.add_argument('--initiate', action='store_true',
                        help='page BlueCaster and open the HID channels, as a mouse that wakes up')
    parser.add_argument('--initiate-delay', type=float, default=1.0, metavar='S',
                        help='with --initiate, seconds after BlueCaster turned page scan on')
    parser.add_argument('--wait-bluecaster', type=float, default=30.0, metavar='S',
                        help='with --initiate, how long to wait for BlueCaster at most')
    parser.add_argument('--reports', metavar='FILE',
                        help='input reports to send, one per line in hex with the report ID first '
                             '(default: a built-in script that ends with a key held)')
    parser.add_argument('--start-delay', type=float, default=2.0, metavar='S',
                        help='seconds between the interrupt channel opening and the first report')
    parser.add_argument('--report-gap', type=float, default=0.1, metavar='S', help='seconds between reports')
    parser.add_argument('--disconnect-after', type=float, metavar='S',
                        help='drop the link this many seconds after the last report')
    parser.add_argument('--passkey-file', metavar='FILE',
                        help='pair as a keyboard that types a passkey (MITM): the six digits are read from '
                             'this file once BlueCaster shows them (default: Just Works, no input or output)')
    bumble.logging.setup_basic_logging(parser.parse_known_args()[0].log_level)
    asyncio.run(main(parser.parse_args()))
