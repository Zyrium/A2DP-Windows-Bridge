"""Virtual Bluetooth link with an LE keyboard and mouse (HID over GATT), for testing BlueCaster's HID host (issue #413).

Runs two Bumble virtual controllers on one in-process link, as emu_hid.py:
  - controller "BlueCaster" is served as H4 over TCP; BlueCaster connects to it with
    `bluecaster-core.exe --cli --hci-tcp 127.0.0.1:<port> --hid-test <s> --hid-le-device <address>`.
    It is dual mode, and on top of Bumble's controller it keeps a filter
    accept list: LE Create Connection with the accept list connects to the
    first listed device that advertises (BlueCaster's background connection),
    and LE Create Connection Cancel ends a pending attempt with LE Connection
    Complete 0x02, as a real controller does
  - controller "HIDLE" is driven by a Bumble host acting as an LE HID device:
    a GATT server with the HID service (HID Information, a Report Map of the
    keyboard / mouse / consumer layout emu_hid.py uses, longer than one ATT
    packet, three Input Reports with Report Reference descriptors, Protocol
    Mode, HID Control Point), bondable with Just Works, advertising the HID
    service UUID and the keyboard appearance

Once all three input reports are subscribed to on a link, the device sends
the script of emu_hid.py (the same reports, without their report ID: the
characteristic tells it). --drop-after drops the first link that many
seconds after the script; the device advertises again unless --stay-away.
With --discover-host the device reads BlueCaster's GATT services once it is
encrypted and prints their UUIDs.

With --privacy the device advertises with a resolvable private address made
from its IRK, a new one each time it advertises (as a device does after a
reconnection), and distributes its IRK and public identity address when it
pairs; the READY line names the first one ("rpa=..."). BlueCaster's
controller here resolves nothing (its accept list compares addresses as
they are), as a controller without address resolution. With
--report-on-encrypt the device sends one input report (a mouse move) as soon
as a later link is encrypted, before BlueCaster reads its services: a bonded
device keeps its notifications on.

Prints "READY" once BlueCaster can connect, then one line per step ("hid: ..."),
for run_test.py --hid. The device's HCI traffic is written to a PacketLogger
(.pklg) capture. Stops on SIGTERM / Ctrl+C.
"""

import argparse
import asyncio
import logging

import bumble.logging
from bumble import hci
from bumble.att import Attribute
from bumble.core import AdvertisingData
from bumble.device import Device, Peer
from bumble.gatt import (
    GATT_HID_CONTROL_POINT_CHARACTERISTIC,
    GATT_HID_INFORMATION_CHARACTERISTIC,
    GATT_HUMAN_INTERFACE_DEVICE_SERVICE,
    GATT_PROTOCOL_MODE_CHARACTERISTIC,
    GATT_REPORT_CHARACTERISTIC,
    GATT_REPORT_MAP_CHARACTERISTIC,
    GATT_REPORT_REFERENCE_DESCRIPTOR,
    Characteristic,
    CharacteristicValue,
    Descriptor,
    Service,
)
from bumble.host import Host
from bumble.keys import MemoryKeyStore
from bumble.pairing import PairingConfig, PairingDelegate
from bumble.transport import open_transport
from bumble.transport.common import AsyncPipeSink

from emu_hid import BUILT_IN_SCRIPT, REPORT_DESCRIPTOR, TypingDelegate, mouse, read_script
from emu_sink import BLUECASTER_ADDRESS, DUAL_MODE_FEATURES
from le_audio.controller import LeEmuController, LeLocalLink, PacketLoggerSnooper

HID_LE_ADDRESS = '00:A2:D0:00:00:05'
APPEARANCE_KEYBOARD = 0x03C1
NAME = 'BC LE Keyboard'
REPORT_IDS = (1, 2, 3)

log = logging.getLogger('emu_hid_le')


class AcceptListController(LeEmuController):
    """BlueCaster's controller, with a filter accept list (see module doc)."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.accept_list = set()

    def on_hci_le_clear_filter_accept_list_command(self, _command):
        self.accept_list.clear()
        return hci.HCI_StatusReturnParameters(status=hci.HCI_ErrorCode.SUCCESS)

    def on_hci_le_add_device_to_filter_accept_list_command(self, command):
        self.accept_list.add(bytes(command.address.address_bytes))
        print(f'hid: accept list has {len(self.accept_list)} device(s)', flush=True)
        return hci.HCI_StatusReturnParameters(status=hci.HCI_ErrorCode.SUCCESS)

    def on_hci_le_remove_device_from_filter_accept_list_command(self, command):
        self.accept_list.discard(bytes(command.address.address_bytes))
        return hci.HCI_StatusReturnParameters(status=hci.HCI_ErrorCode.SUCCESS)

    def on_hci_le_create_connection_cancel_command(self, _command):
        pending = self.pending_le_connection
        self.pending_le_connection = None
        if pending is not None:
            # After the Command Complete: the attempt ended (0x02)
            asyncio.get_running_loop().call_soon(self._cancelled)
        return hci.HCI_StatusReturnParameters(status=hci.HCI_ErrorCode.SUCCESS)

    def _cancelled(self):
        self.send_hci_packet(hci.HCI_LE_Connection_Complete_Event(
            status=hci.HCI_ErrorCode.UNKNOWN_CONNECTION_IDENTIFIER_ERROR,
            connection_handle=0, role=hci.Role.CENTRAL, peer_address_type=0, peer_address=hci.Address.ANY,
            connection_interval=0, peripheral_latency=0, supervision_timeout=0, central_clock_accuracy=0))

    def on_advertising_pdu(self, pdu):
        pending = self.pending_le_connection
        if (pending is not None and pending.initiator_filter_policy == 1
                and bytes(pdu.advertiser_address.address_bytes) in self.accept_list):
            # Bumble connects to the command's peer address: the advertiser's
            pending.peer_address = pdu.advertiser_address
            print('hid: BlueCaster connects to the device from its accept list', flush=True)
        super().on_advertising_pdu(pdu)


def on_output_report(_connection, value):
    """BlueCaster wrote the keyboard lights (Output report ID 1)"""
    print(f'hid: output report 01 {bytes(value).hex(" ")}', flush=True)


def hid_service(reports):
    """The HID service; `reports` gets report ID -> Input Report characteristic.
    Report ID 1 also has an Output report: the keyboard lights."""
    readable = Attribute.READABLE | Attribute.READ_REQUIRES_ENCRYPTION
    writeable = Attribute.WRITEABLE | Attribute.WRITE_REQUIRES_ENCRYPTION
    lengths = {1: 8, 2: 6, 3: 1}
    characteristics = [
        Characteristic(GATT_PROTOCOL_MODE_CHARACTERISTIC,
                       Characteristic.READ | Characteristic.WRITE_WITHOUT_RESPONSE, readable | writeable, bytes([1])),
        # bcdHID 1.11, no country, normally connectable
        Characteristic(GATT_HID_INFORMATION_CHARACTERISTIC, Characteristic.READ, readable, bytes([0x11, 0x01, 0, 0x02])),
        Characteristic(GATT_REPORT_MAP_CHARACTERISTIC, Characteristic.READ, readable, REPORT_DESCRIPTOR),
        Characteristic(GATT_HID_CONTROL_POINT_CHARACTERISTIC, Characteristic.WRITE_WITHOUT_RESPONSE, writeable,
                       bytes([0])),
    ]
    for report_id in REPORT_IDS:
        c = Characteristic(GATT_REPORT_CHARACTERISTIC, Characteristic.READ | Characteristic.NOTIFY, readable,
                           bytes(lengths[report_id]),
                           descriptors=[Descriptor(GATT_REPORT_REFERENCE_DESCRIPTOR, readable, bytes([report_id, 1]))])
        reports[report_id] = c
        characteristics.append(c)
    characteristics.append(Characteristic(
        GATT_REPORT_CHARACTERISTIC,
        Characteristic.READ | Characteristic.WRITE | Characteristic.WRITE_WITHOUT_RESPONSE, readable | writeable,
        CharacteristicValue(read=lambda _connection: bytes(1), write=on_output_report),
        descriptors=[Descriptor(GATT_REPORT_REFERENCE_DESCRIPTOR, readable, bytes([1, 2]))]))
    return Service(GATT_HUMAN_INTERFACE_DEVICE_SERVICE, characteristics)


def advertising_data():
    return bytes(AdvertisingData([
        (AdvertisingData.FLAGS, bytes([0x06])),
        (AdvertisingData.APPEARANCE, APPEARANCE_KEYBOARD.to_bytes(2, 'little')),
        (AdvertisingData.INCOMPLETE_LIST_OF_16_BIT_SERVICE_CLASS_UUIDS, (0x1812).to_bytes(2, 'little')),
        (AdvertisingData.COMPLETE_LOCAL_NAME, NAME.encode()),
    ]))


async def main(args):
    link = LeLocalLink()
    transport = await open_transport(f'tcp-server:127.0.0.1:{args.port}')
    bluecaster_controller = AcceptListController(
        'BlueCaster', host_source=transport.source, host_sink=transport.sink, link=link,
        public_address=BLUECASTER_ADDRESS)
    bluecaster_controller.lmp_features = DUAL_MODE_FEATURES
    device_controller = LeEmuController('HIDLE', link=link, public_address=args.address)
    host = Host(device_controller, AsyncPipeSink(device_controller))
    if args.capture:
        host.snooper = PacketLoggerSnooper(args.capture)
    device = Device(name=NAME, address=args.address, host=host)
    device.le_enabled = True
    device.classic_enabled = False
    device.keystore = MemoryKeyStore()
    if args.privacy:
        # Bumble's default IRK is all zero, which hosts take as no IRK
        device.irk = bytes(range(0x11, 0x21))
    # A keyboard can type a passkey, but nothing asks for one: Just Works
    # with the PC (no input, no output), bonded. With --passkey-file it
    # asks for MITM and types the number BlueCaster shows.
    device.pairing_config_factory = lambda connection: PairingConfig(
        sc=True, mitm=bool(args.passkey_file), bonding=True, identity_address_type=hci.Address.PUBLIC_DEVICE_ADDRESS,
        delegate=TypingDelegate(args.passkey_file) if args.passkey_file
        else PairingDelegate(io_capability=PairingDelegate.IoCapability.KEYBOARD_INPUT_ONLY))
    reports = {}
    device.add_service(hid_service(reports))
    await device.power_on()

    script = read_script(args.reports) if args.reports else BUILT_IN_SCRIPT
    state = {'links': 0, 'dropped': False, 'subscribed': {}, 'rpas': []}

    async def advertise():
        if args.privacy:
            # A new resolvable private address of the device's IRK each time
            rpa = hci.Address.generate_private_address(device.irk)
            device.random_address = rpa
            await device.send_sync_command(hci.HCI_LE_Set_Random_Address_Command(random_address=rpa))
            state['rpas'].append(str(rpa))
            print(f'hid: advertising with private address {len(state["rpas"])} ({rpa})', flush=True)
            await device.start_advertising(own_address_type=hci.OwnAddressType.RANDOM,
                                           advertising_data=advertising_data(), auto_restart=False)
            return
        await device.start_advertising(own_address_type=hci.OwnAddressType.PUBLIC,
                                       advertising_data=advertising_data(), auto_restart=False)

    async def send_script(connection):
        await asyncio.sleep(args.start_delay)
        sent = 0
        for report in script:
            if getattr(connection, 'bc_down', False):
                break
            c = reports.get(report[0])
            if c is None:
                continue
            try:
                await device.notify_subscriber(connection, c, report[1:])
            except Exception as e:  # the link went down meanwhile
                print(f'hid: notifying failed ({type(e).__name__})', flush=True)
                break
            sent += 1
            await asyncio.sleep(args.report_gap)
        print(f'hid: sent {sent} of {len(script)} reports', flush=True)
        if args.drop_after is not None and not state['dropped']:
            state['dropped'] = True
            await asyncio.sleep(args.drop_after)
            print('hid: dropping the link', flush=True)
            await connection.disconnect(hci.HCI_REMOTE_USER_TERMINATED_CONNECTION_ERROR)

    def on_subscription(report_id):
        def handler(connection, notify, _indicate):
            subs = state['subscribed'].setdefault(connection.handle, set())
            if notify:
                subs.add(report_id)
            if notify and subs == set(REPORT_IDS):
                print('hid: notifications on for every input report', flush=True)
                task = asyncio.ensure_future(send_script(connection))
                task.add_done_callback(
                    lambda t: t.exception() and print(f'hid: script failed: {t.exception()!r}', flush=True))
        return handler

    for report_id, c in reports.items():
        c.on('subscription', on_subscription(report_id))

    async def discover_host(connection):
        try:
            peer = Peer(connection)
            services = await peer.discover_services()
            uuids = sorted(str(s.uuid) for s in services)
            print(f'hid: host services {",".join(uuids)}', flush=True)
        except Exception as e:
            print(f'hid: host services not read ({type(e).__name__}: {e})', flush=True)

    def on_connection(connection):
        state['links'] += 1
        print(f'hid: link up ({state["links"]})', flush=True)

        def on_disconnection(reason):
            connection.bc_down = True
            print(f'hid: link down (0x{reason:02X})', flush=True)
            state['subscribed'].pop(connection.handle, None)
            if args.stay_away and state['dropped']:
                print('hid: staying away', flush=True)
                return
            asyncio.ensure_future(advertise())

        connection.on('disconnection', on_disconnection)
        connection.on('pairing', lambda keys: print('hid: paired', flush=True))
        connection.on('pairing_failure', lambda reason: print(f'hid: pairing failed ({reason})', flush=True))

        def on_encryption_change():
            print(f'hid: encryption {"on" if connection.is_encrypted else "off"}', flush=True)
            if args.discover_host and connection.is_encrypted:
                asyncio.ensure_future(discover_host(connection))
            if args.report_on_encrypt and connection.is_encrypted and state['links'] > 1:
                # Bonded: notifications stay on (forced: Bumble forgets the
                # subscriptions of a link that went down)
                async def early():
                    await device.notify_subscriber(connection, reports[2], mouse(dx=1, dy=1)[1:], force=True)
                    print('hid: sent one report at once on encryption', flush=True)
                asyncio.ensure_future(early())
        connection.on('connection_encryption_change', on_encryption_change)

    device.on('connection', on_connection)
    await advertise()
    rpa = f' rpa={state["rpas"][0]}' if state['rpas'] else ''
    print(f'READY 127.0.0.1:{args.port} hid={args.address}{rpa}', flush=True)
    await asyncio.get_running_loop().create_future()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--port', type=int, default=9001, help='TCP port for BlueCaster (H4)')
    parser.add_argument('--capture', help='PacketLogger capture of the device HCI traffic')
    parser.add_argument('--log-level', default='WARNING')
    parser.add_argument('--address', default=HID_LE_ADDRESS, help='the device public address (default %(default)s)')
    parser.add_argument('--reports', metavar='FILE',
                        help='input reports to send, one per line in hex with the report ID first '
                             '(default: emu_hid.py\'s built-in script that ends with a key held)')
    parser.add_argument('--start-delay', type=float, default=1.0, metavar='S',
                        help='seconds between the last subscription and the first report')
    parser.add_argument('--report-gap', type=float, default=0.1, metavar='S', help='seconds between reports')
    parser.add_argument('--drop-after', type=float, metavar='S',
                        help='drop the first link this many seconds after the last report')
    parser.add_argument('--stay-away', action='store_true',
                        help='with --drop-after, do not advertise again after the drop')
    parser.add_argument('--discover-host', action='store_true',
                        help="read BlueCaster's GATT services once encrypted and print them")
    parser.add_argument('--passkey-file', metavar='FILE',
                        help='ask for MITM and type the passkey BlueCaster shows, read from this file')
    parser.add_argument('--privacy', action='store_true',
                        help='advertise with a new resolvable private address each time, distribute the IRK')
    parser.add_argument('--report-on-encrypt', action='store_true',
                        help='send one input report as soon as a later link is encrypted')
    bumble.logging.setup_basic_logging(parser.parse_known_args()[0].log_level)
    asyncio.run(main(parser.parse_args()))
