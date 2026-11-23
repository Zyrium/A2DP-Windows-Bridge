"""Bumble virtual controller with what LE Audio needs on the in-process link.

Bumble's Controller sets up CIGs and CISes but does not carry HCI ISO data
between the two controllers of a LocalLink. LeEmuController adds that:

  - an HCI ISO data packet (complete SDU) from the host of the central is
    delivered to the host of the peripheral on the matching CIS (same CIG /
    CIS ID), with a time stamp and the sender's sequence number, at the next
    free SDU interval of that CIS (like the air schedule: one SDU per
    interval), and Number Of Completed Packets goes back to the sender then.
    So BlueCaster is paced by the "controller" as with a real one. The other
    way (a source ASE: the headset microphone) the same, with the CIG's
    P->C SDU interval and latency; a late SDU of that direction comes from
    the emulated headset's own sender and is logged as "late microphone
    SDU" (not a BlueCaster problem, run_test.py does not count it).
  - LE CIS Established reports the CIG parameters (ISO interval, max PDU,
    PHY 2M) instead of zeros.
  - pending ISO deliveries are flushed before a CIS disconnects or its CIG
    is removed, so the sink receives every SDU BlueCaster sent.
  - every command gets its Command Complete / Command Status, as from a real
    controller: Set AFH Host Channel Classification succeeds (BlueCaster sends
    it depending on the PC's Wi-Fi), others Bumble does not implement fail
    with Unknown HCI Command. Bumble alone answered nothing to commands it
    has no class for, and BTstack then waited for ever.
  - with --no-iso-buffers (le_audio/__init__.py) the "BlueCaster" controller
    reports no ISO buffers in LE Read Buffer Size v2 (LE without ISO).
  - with --refuse-cis N the central's controller answers the Nth CIS of an
    LE Create CIS with LE CIS Established, status 0x1E, while the others
    come up; with --cis-up-after they come up that much later, on the
    central's side only (the sink has released its ASEs by then and would
    not accept them).
  - with --reencrypt-fail the central's controller fails its first
    re-encryption with the stored key once: the sink ends the link (drop)
    or has no key (key-missing).
  - with --iso-buffers N the "BlueCaster" controller reports N ISO buffers
    (a Realtek adapter has 20).
  - with --completion-delay-ms MS Number Of Completed Packets for an SDU of
    BlueCaster's goes back MS after its delivery (a controller that holds an
    unacknowledged PDU until its flush point), and LE CIS Established
    reports the flush timeout that matches.
  - with --withhold-completions N the Nth CIS of LE Create CIS (0: the
    first) never gets Number Of Completed Packets while it is up (a CIS the
    controller stopped serving); its SDUs are still delivered, and the
    buffers come back just before it disconnects.

PacketLoggerSnooper also writes ISO data (PacketLogger types 0x0c / 0x0d),
which bluecaster_decode --le-audio reads.
"""

import asyncio
import logging
import math
import os
import struct
import sys
import time

from bumble import core, hci, ll
from bumble.link import LocalLink
from bumble.snoop import Snooper

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from emu_sink import EmuController  # noqa: E402  (classic HCI additions, ACL buffers)

log = logging.getLogger('emu_le_audio')


def _precise_event_loop_timing():
    """On Windows, asyncio's clock is time.monotonic() = GetTickCount64(),
    which moves in 15.6 ms steps, and a timer fires when it is due within
    that resolution: SDU deliveries (and their Number Of Completed Packets)
    came up to ~16 ms early or late, in bursts. Use the performance counter
    for the loop's clock and 1 ms system timer resolution for its waits, so
    the emulated controller keeps its SDU intervals to about a millisecond.
    Called on import, before emu_le_audio_sink.py starts its event loop."""
    if sys.platform != 'win32':
        return
    import ctypes
    asyncio.BaseEventLoop.time = lambda self: time.perf_counter()
    original_init = asyncio.BaseEventLoop.__init__

    def init(self, *args, **kwargs):
        original_init(self, *args, **kwargs)
        self._clock_resolution = 1e-4
    asyncio.BaseEventLoop.__init__ = init
    try:
        ctypes.windll.winmm.timeBeginPeriod(1)
    except OSError:
        pass


_precise_event_loop_timing()

# CIG parameters set by the central, by (cig_id, cis_id): the peripheral's
# controller reports them in its LE CIS Established too (one central here)
CIS_PARAMS = {}
LATE_INTERVALS = 3
LATE_US = 30000      # the same tolerance in time for shorter SDU intervals
# An SDU that reaches this controller more than HOST_LATE_MS after the start
# of its SDU interval would have missed it on a real controller (whose queue
# ran empty): logged as "late SDU" (run_test.py
# fails the case). The time this emulation's own event loop was stalled in
# between is not counted (see LeEmuController._heartbeat).
HOST_LATE_MS = 2.0
HEARTBEAT_S = 0.002
STALL_S = 0.003      # heartbeat later than this: the event loop stalled (timer jitter is ~1.5 ms)
LATE_LOG_MAX = 20    # late SDU lines per CIS (then every 100th)
# Names of the controllers that report no ISO buffers (--no-iso-buffers)
NO_ISO_BUFFERS = set()
# --refuse-cis / --cis-up-after: (index of the refused CIS in LE Create CIS,
# seconds until the others are up or None for the usual way), or None
REFUSE_CIS = None
# --reencrypt-fail: what the "BlueCaster" controller makes of the first
# re-encryption with a stored key (its second LE Enable Encryption, the first
# being the pairing's): 'drop' the sink ends the link instead (0x13),
# 'key-missing' the sink has no key (Encryption Change 0x06, encryption off,
# as a controller reports LL_REJECT_EXT_IND with PIN or Key Missing); None:
# every encryption succeeds
REENCRYPT_FAIL = None
# --iso-buffers: Total_Num_ISO_Data_Packets of BlueCaster's controller, or None
ISO_BUFFERS = None
# --completion-delay-ms: seconds between an SDU's delivery and its Number Of
# Completed Packets on BlueCaster's controller
COMPLETION_DELAY_S = 0.0
# --withhold-completions: index in LE Create CIS of the CIS without
# completions, or None
WITHHOLD_CIS = None


class LeEmuController(EmuController):
    """EmuController plus HCI ISO data over the LocalLink (see module doc)."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.iso_next_slot = {}    # CIS handle -> number of the next free SDU interval
        self.iso_pending = {}      # CIS handle -> [(TimerHandle, deliver)]
        self.iso_t0 = None
        self.iso_count = 0
        self.iso_late = {}         # CIS handle -> late SDUs
        self.iso_completion_stall = {}  # CIS handle -> stall that held up its last completion (s)
        self.stalls = []           # (from, to) loop times this event loop did not run (recent ones)
        self.heartbeat = None
        self.heartbeat_due = 0.0
        self.encryptions = 0       # LE Enable Encryption commands (--reencrypt-fail)
        self.iso_delayed = {}      # CIS handle -> [TimerHandle] of delayed completions
        self.withheld = {}         # CIS handle -> completions withheld (--withhold-completions)

    def _heartbeat(self, due):
        """Every HEARTBEAT_S while ISO data flows: when this timer fires more
        than STALL_S late, the event loop did not run in between (a stall of
        this emulation; it delays reading BlueCaster's SDUs as much)."""
        loop = asyncio.get_running_loop()
        now = loop.time()
        if now - due > STALL_S:
            self.stalls.append((due, now))
            if len(self.stalls) > 1000:
                del self.stalls[:500]
        self.heartbeat_due = max(due + HEARTBEAT_S, now)
        self.heartbeat = loop.call_at(self.heartbeat_due, self._heartbeat, self.heartbeat_due)

    def stalled(self, start, end):
        """Seconds of [start, end] this event loop was stalled. A stall still
        going on (the heartbeat overdue: the SDU was read in the same loop
        iteration that ended it, before the heartbeat could fire) counts too."""
        stalls = list(self.stalls)
        if end - self.heartbeat_due > STALL_S:
            stalls.append((self.heartbeat_due, end))
        return sum(max(0.0, min(end, b) - max(start, a)) for a, b in stalls if b > start)

    # ---- host -> controller ----

    def on_hci_packet(self, packet):
        if isinstance(packet, hci.HCI_IsoDataPacket):
            self.on_hci_iso_data_packet(packet)
            return
        super().on_hci_packet(packet)

    def on_hci_command_packet(self, command):
        # Bumble answers synchronous commands (Command Complete) itself, also
        # those it does not implement. For the others it relies on the
        # handler to send Command Status, and sends nothing for a command it
        # has no handler (or no class) for: answer those here.
        if isinstance(command, hci.HCI_SyncCommand):
            return super().on_hci_command_packet(command)
        handler = getattr(self, f'on_{command.name.lower()}', None)
        result = handler(command) if handler is not None else None
        if handler is None:
            log.warning('unsupported command %s: Unknown HCI Command', command.name)
            result = hci.HCI_StatusReturnParameters(status=hci.HCI_ErrorCode.UNKNOWN_HCI_COMMAND_ERROR)
        if result is None:
            return None   # the handler answered
        if isinstance(command, hci.HCI_AsyncCommand):
            self._send_hci_command_status(result.status, command.op_code)
        else:
            # A command Bumble has no class for: its answer is unknown here,
            # Command Complete (with the status) is what most commands get
            self.send_hci_packet(hci.HCI_Command_Complete_Event(
                num_hci_command_packets=1, command_opcode=command.op_code, return_parameters=result))
        return None

    def on_hci_le_enable_encryption_command(self, command):
        if self.name != 'BlueCaster' or REENCRYPT_FAIL is None:
            return super().on_hci_le_enable_encryption_command(command)
        self.encryptions += 1
        connection = self.find_le_connection_by_handle(command.connection_handle)
        if self.encryptions != 2 or connection is None:
            return super().on_hci_le_enable_encryption_command(command)
        self._send_hci_command_status(hci.HCI_COMMAND_STATUS_PENDING, command.op_code)
        if REENCRYPT_FAIL == 'drop':
            def fail():
                if self.le_connections.get(connection.peer_address) is not connection:
                    return
                log.info('%s: the sink ends the link during re-encryption (--reencrypt-fail drop)', self.name)
                reason = hci.HCI_ErrorCode.REMOTE_USER_TERMINATED_CONNECTION_ERROR
                connection.send_ll_control_pdu(ll.TerminateInd(reason))
                self.on_le_disconnected(connection, reason)
        else:
            def fail():
                log.info('%s: the sink has no key for the re-encryption (--reencrypt-fail key-missing)', self.name)
                self.send_hci_packet(hci.HCI_Encryption_Change_Event(
                    status=hci.HCI_ErrorCode.PIN_OR_KEY_MISSING_ERROR, connection_handle=connection.handle,
                    encryption_enabled=0))
        asyncio.get_running_loop().call_later(0.05, fail)
        return None

    def on_hci_set_afh_host_channel_classification_command(self, _command):
        # Vol 4 Part E 7.3.46: Command Complete with the status only. The
        # channel map does not matter on the virtual link.
        return hci.HCI_StatusReturnParameters(status=hci.HCI_ErrorCode.SUCCESS)

    def on_hci_le_read_buffer_size_v2_command(self, command):
        result = super().on_hci_le_read_buffer_size_v2_command(command)
        if self.name in NO_ISO_BUFFERS:
            result.iso_data_packet_length = 0
            result.total_num_iso_data_packets = 0
        elif ISO_BUFFERS is not None and self.name == 'BlueCaster':
            result.total_num_iso_data_packets = ISO_BUFFERS
        return result

    def on_hci_le_set_cig_parameters_command(self, command):
        for i, cis_id in enumerate(command.cis_id):
            CIS_PARAMS[(command.cig_id, cis_id)] = dict(
                sdu_interval=command.sdu_interval_c_to_p,
                latency_ms=command.max_transport_latency_c_to_p,
                max_sdu_c_to_p=command.max_sdu_c_to_p[i],
                max_sdu_p_to_c=command.max_sdu_p_to_c[i],
                rtn=command.rtn_c_to_p[i],
                sdu_interval_p_to_c=command.sdu_interval_p_to_c,
                latency_p_to_c_ms=command.max_transport_latency_p_to_c,
                rtn_p_to_c=command.rtn_p_to_c[i],
            )
        return super().on_hci_le_set_cig_parameters_command(command)

    def on_hci_le_create_cis_command(self, command):
        handles = list(command.cis_connection_handle)
        if WITHHOLD_CIS is not None and WITHHOLD_CIS < len(handles):
            self.withheld[handles[WITHHOLD_CIS]] = 0
            log.info('withholding Number Of Completed Packets of CIS 0x%04x (--withhold-completions)',
                     handles[WITHHOLD_CIS])
        if REFUSE_CIS is None or REFUSE_CIS[0] >= len(handles):
            return super().on_hci_le_create_cis_command(command)
        refused, up_after = REFUSE_CIS
        loop = asyncio.get_running_loop()
        for i, (cis_handle, acl_handle) in enumerate(zip(handles, command.acl_connection_handle)):
            if i == refused:
                continue
            cis_link = self.central_cis_links[cis_handle]
            cis_link.acl_connection = self.find_connection_by_handle(acl_handle)
            if up_after is None:
                cis_link.acl_connection.send_ll_control_pdu(ll.CisReq(cig_id=cis_link.cig_id, cis_id=cis_link.cis_id))
            else:
                loop.call_later(up_after, self.on_le_cis_established, cis_link.cig_id, cis_link.cis_id)
        self._send_hci_command_status(hci.HCI_COMMAND_STATUS_PENDING, command.op_code)
        log.info('refusing CIS 0x%04x with 0x1E (--refuse-cis)', handles[refused])
        self.send_hci_packet(
            hci.HCI_LE_CIS_Established_Event(
                status=hci.HCI_ErrorCode.INVALID_LMP_OR_LL_PARAMETERS_ERROR,
                connection_handle=handles[refused],
                cig_sync_delay=0,
                cis_sync_delay=0,
                transport_latency_c_to_p=0,
                transport_latency_p_to_c=0,
                phy_c_to_p=0,
                phy_p_to_c=0,
                nse=0,
                bn_c_to_p=0,
                bn_p_to_c=0,
                ft_c_to_p=0,
                ft_p_to_c=0,
                max_pdu_c_to_p=0,
                max_pdu_p_to_c=0,
                iso_interval=0,
            )
        )
        return None

    def on_le_cis_established(self, cig_id, cis_id):
        cis_link = next(
            (link for link in list(self.central_cis_links.values()) + list(self.peripheral_cis_links.values())
             if link.cis_id == cis_id and link.cig_id == cig_id), None)
        if cis_link is None:
            return
        p = CIS_PARAMS.get((cig_id, cis_id), dict(sdu_interval=10000, latency_ms=20, max_sdu_c_to_p=0,
                                                  max_sdu_p_to_c=0, rtn=2))
        self.send_hci_packet(
            hci.HCI_LE_CIS_Established_Event(
                status=hci.HCI_ErrorCode.SUCCESS,
                connection_handle=cis_link.handle,
                cig_sync_delay=p['sdu_interval'] // 2,
                cis_sync_delay=p['sdu_interval'] // 2,
                transport_latency_c_to_p=p['latency_ms'] * 1000,
                transport_latency_p_to_c=p.get('latency_p_to_c_ms', p['latency_ms']) * 1000,
                phy_c_to_p=2,
                phy_p_to_c=2,
                nse=min(p['rtn'] + 1, 31),
                bn_c_to_p=1 if p['max_sdu_c_to_p'] else 0,
                bn_p_to_c=1 if p['max_sdu_p_to_c'] else 0,
                # An SDU completes up to FT ISO intervals after its first event
                ft_c_to_p=max(1, math.ceil(COMPLETION_DELAY_S * 1e6 / (max(p['sdu_interval'] // 1250, 4) * 1250))),
                ft_p_to_c=1,
                max_pdu_c_to_p=p['max_sdu_c_to_p'],
                max_pdu_p_to_c=p['max_sdu_p_to_c'],
                iso_interval=max(p['sdu_interval'] // 1250, 4),
            )
        )

    def _peer_cis(self, link):
        """(peer controller, peer CIS link) of a CIS link, or (None, None)"""
        if not self.link or link.acl_connection is None:
            return None, None
        peer = self.link.find_le_controller(link.acl_connection.peer_address)
        if peer is None:
            return None, None
        links = list(peer.peripheral_cis_links.values()) + list(peer.central_cis_links.values())
        peer_link = next((x for x in links if x.cig_id == link.cig_id and x.cis_id == link.cis_id), None)
        return peer, peer_link

    def iso_completed(self, handle):
        if handle in self.withheld:
            self.withheld[handle] += 1
            return
        if COMPLETION_DELAY_S > 0 and handle in self.central_cis_links:
            loop = asyncio.get_running_loop()
            timers = self.iso_delayed.setdefault(handle, [])
            entry = []

            def fire():
                if entry and entry[0] in timers:
                    timers.remove(entry[0])
                self.send_completed(handle, 1)
            entry.append(loop.call_later(COMPLETION_DELAY_S, fire))
            timers.append(entry[0])
            return
        self.send_completed(handle, 1)

    def send_completed(self, handle, count):
        self.send_hci_packet(hci.HCI_Number_Of_Completed_Packets_Event(
            connection_handles=[handle], num_completed_packets=[count]))

    def return_withheld(self, handle):
        """Completions held back (delayed or withheld) go back at once, before
        the CIS disconnects, as a controller frees its buffers then."""
        timers = self.iso_delayed.pop(handle, [])
        for timer in timers:
            timer.cancel()
        count = len(timers) + self.withheld.pop(handle, 0)
        if count:
            log.info('CIS 0x%04x: %d completion(s) held back returned', handle, count)
            self.send_completed(handle, count)

    def on_hci_iso_data_packet(self, packet):
        handle = packet.connection_handle
        link = self.central_cis_links.get(handle) or self.peripheral_cis_links.get(handle)
        if link is None:
            log.warning('ISO data for unknown handle 0x%04X', handle)
            self.iso_completed(handle)
            return
        params = CIS_PARAMS.get((link.cig_id, link.cis_id), {})

        def receivers():
            peer, peer_link = self._peer_cis(link)
            return [(peer, peer_link.handle)] if peer is not None and peer_link is not None else []
        if handle in self.peripheral_cis_links:
            # P->C: the headset's microphone (a source ASE)
            interval = params.get('sdu_interval_p_to_c') or params.get('sdu_interval', 10000)
            latency = params.get('latency_p_to_c_ms', params.get('latency_ms', 0))
            self.schedule_iso_sdu(packet, 'CIS', interval, latency, receivers, late_label='late microphone SDU')
            return
        self.schedule_iso_sdu(packet, 'CIS', params.get('sdu_interval', 10000), params.get('latency_ms', 0),
                              receivers)

    def schedule_iso_sdu(self, packet, kind, interval_us, latency_ms, receivers, late_label='late SDU'):
        """Carries one SDU of a CIS or BIS (kind) at its SDU interval: to the
        (controller, handle) pairs receivers() returns when it is due, then
        Number Of Completed Packets to the sender."""
        handle = packet.connection_handle
        loop = asyncio.get_running_loop()
        if packet.pb_flag != 0b10:
            log.warning('ISO data with PB flag %s (only complete SDUs are carried)', bin(packet.pb_flag))
        interval = interval_us / 1e6
        now = loop.time()
        if self.iso_t0 is None:
            self.iso_t0 = now
        if self.heartbeat is None:
            self.heartbeat_due = now + HEARTBEAT_S
            self.heartbeat = loop.call_at(self.heartbeat_due, self._heartbeat, self.heartbeat_due)
        # SDU interval n of the CIS or BIS: the first SDU takes the next interval,
        # then each the next free one. The host's lateness is how long after
        # the start of that interval the SDU arrived, less the time this
        # event loop was stalled (such stalls happen, 60-90 ms): meanwhile,
        # as it delays reading the SDU as much, and while the last completion
        # on the CIS was held up, as the host can only send when its queue
        # has room. More than HOST_LATE_MS is logged as a late SDU (BlueCaster
        # sends 2 intervals ahead, so it has that long). Only when the host is
        # late by more than
        # LATE_INTERVALS does the SDU move to a later interval (a real
        # controller would have flushed it): that is in time, not intervals,
        # at least LATE_US (4 intervals of 7.5 ms, 3 of 10 ms), and as a real
        # controller only flushes an SDU after the CIG's Max Transport
        # Latency, that much too.
        late_us = max(LATE_US, latency_ms * 1000)
        late = max(LATE_INTERVALS, math.ceil(late_us / interval_us))
        n_now = int(math.ceil((now - self.iso_t0) / interval - 1e-6))
        expected = self.iso_next_slot.get(handle)
        if expected is None:
            n = n_now
        else:
            n = expected
            held_up = self.iso_completion_stall.get(handle, 0.0)
            due = self.iso_t0 + expected * interval + held_up
            if (now - due) * 1000 > HOST_LATE_MS:
                stalled = self.stalled(due, now)
                host_late = now - due - stalled
                stalled += held_up
                if host_late * 1e6 > late_us:
                    n = max(expected, n_now - late)
                if host_late * 1000 > HOST_LATE_MS:
                    count = self.iso_late[handle] = self.iso_late.get(handle, 0) + 1
                    if count <= LATE_LOG_MAX or count % 100 == 0:
                        log.warning('%s: %s 0x%04X seq %d reached the controller %.1f ms after its SDU '
                                    'interval began (not counting %.1f ms this emulation stalled); '
                                    '%d late SDU(s) on this %s so far', late_label, kind, handle,
                                    packet.packet_sequence_number or 0, host_late * 1000, stalled * 1000, count, kind)
        self.iso_next_slot[handle] = n + 1
        slot = self.iso_t0 + n * interval
        time_stamp = (n * interval_us) & 0xFFFFFFFF
        seq = packet.packet_sequence_number or 0
        data = packet.iso_sdu_fragment
        entry = []

        def deliver():
            pending = self.iso_pending.get(handle, [])
            if entry and entry[0] in pending:
                pending.remove(entry[0])
            fired = loop.time()
            self.iso_completion_stall[handle] = self.stalled(slot, fired) if fired - slot > STALL_S else 0.0
            for peer, peer_handle in receivers():
                peer.send_hci_packet(hci.HCI_IsoDataPacket(
                    connection_handle=peer_handle,
                    pb_flag=0b10,
                    data_total_length=8 + len(data),
                    time_stamp=time_stamp,
                    packet_sequence_number=seq,
                    iso_sdu_length=len(data),
                    packet_status_flag=0,
                    iso_sdu_fragment=data,
                ))
            self.iso_completed(handle)
            self.iso_count += 1

        timer = loop.call_at(slot, deliver)
        entry.append((timer, deliver))
        self.iso_pending.setdefault(handle, []).append(entry[0])

    def flush_iso(self, handles):
        for handle in handles:
            for timer, deliver in list(self.iso_pending.get(handle, [])):
                timer.cancel()
                deliver()
            self.iso_pending.pop(handle, None)
            self.iso_next_slot.pop(handle, None)
            self.return_withheld(handle)

    def on_le_disconnected(self, connection, reason):
        # The CISes of an ACL go down with it, each with its own Disconnection
        # Complete before the ACL's (Bumble keeps them, so BTstack would wait
        # for them for ever)
        for links, central in ((self.central_cis_links, True), (self.peripheral_cis_links, False)):
            for handle, link in list(links.items()):
                if link.acl_connection is not connection:
                    continue
                self.flush_iso([handle])
                if central:
                    link.acl_connection = None   # removed with LE Remove CIG
                else:
                    links.pop(handle)
                self.send_hci_packet(hci.HCI_Disconnection_Complete_Event(
                    status=hci.HCI_ErrorCode.SUCCESS, connection_handle=handle, reason=reason))
        super().on_le_disconnected(connection, reason)

    def on_hci_disconnect_command(self, command):
        handle = command.connection_handle
        self.flush_iso([handle])
        link = self.central_cis_links.get(handle) or self.peripheral_cis_links.get(handle)
        if link is not None and (link.acl_connection is None or
                                 link.acl_connection.peer_address not in self.le_connections):
            # A CIS whose ACL is gone (already reported down)
            self._send_hci_command_status(hci.HCI_ErrorCode.UNKNOWN_CONNECTION_IDENTIFIER_ERROR,
                                          command.op_code)
            return None
        return super().on_hci_disconnect_command(command)

    def on_hci_le_remove_cig_command(self, command):
        self.flush_iso([h for h, link in self.central_cis_links.items() if link.cig_id == command.cig_id])
        return super().on_hci_le_remove_cig_command(command)


class LeLocalLink(LocalLink):
    """LocalLink that sends LE ACL data from the address the sender's
    connection uses. Bumble's LocalLink takes the sender controller's random
    address, which does not match a connection made with a public address
    (BTstack connects with its public address)."""

    def send_acl_data(self, sender_controller, destination_address, transport, data):
        if transport == core.PhysicalTransport.LE:
            connection = sender_controller.le_connections.get(destination_address)
            destination = self.find_le_controller(destination_address)
            if connection is not None and destination is not None:
                source = connection.self_address
                asyncio.get_running_loop().call_soon(
                    lambda: destination.on_link_acl_data(source, transport, data))
                return
        super().send_acl_data(sender_controller, destination_address, transport, data)


class PacketLoggerSnooper(Snooper):
    """Writes HCI packets in PacketLogger format (as BlueCaster's own capture),
    ISO data included."""

    def __init__(self, path):
        self.file = open(path, 'wb', buffering=0)  # unbuffered: survives being killed

    def snoop(self, hci_packet, direction):
        h4_type, payload = hci_packet[0], hci_packet[1:]
        to_controller = direction == Snooper.Direction.HOST_TO_CONTROLLER
        if h4_type == hci.HCI_COMMAND_PACKET:
            pl_type = 0x00
        elif h4_type == hci.HCI_EVENT_PACKET:
            pl_type = 0x01
        elif h4_type == hci.HCI_ACL_DATA_PACKET:
            pl_type = 0x02 if to_controller else 0x03
        elif h4_type == hci.HCI_ISO_DATA_PACKET:
            pl_type = 0x0C if to_controller else 0x0D
        else:
            return
        now = time.time()
        sec, usec = int(now), int((now - int(now)) * 1e6)
        self.file.write(struct.pack('>IIIB', 9 + len(payload), sec, usec, pl_type) + payload)
