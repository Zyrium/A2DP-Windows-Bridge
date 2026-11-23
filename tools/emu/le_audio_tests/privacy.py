"""LE privacy and bond cases and checks of run_test.py --le-audio.

The sink uses resolvable private addresses (emu_le_audio_sink.py --privacy):
BlueCaster is given the RPA the sink advertised first (as a scan list would have
saved it), pairs (the sink distributes its IRK and its public identity
address), streams, and after the sink dropped the link and took a new RPA it
must find the sink again through the IRK, reconnect to the new RPA and keep
the same device id.

The reencrypt cases fail the first re-encryption with the stored key once
(--reencrypt-fail): when the sink ends the link meanwhile, the bond (and its
IRK) must stay, so the next attempt finds the sink's next RPA and encrypts
without pairing; when the sink has no key (PIN or Key Missing), BlueCaster
pairs again on that link.
"""

import re

from .case import Case

CONFIG = r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 1 CIS'

CASES = [
    Case('privacy-rpa', 'stereo', CONFIG,
         sink_args=['--privacy', '--drop-link-after', '3'], tags=('privacy',), streams=2, lossy=True,
         duration=14),
    Case('reencrypt-drop', 'stereo', CONFIG,
         sink_args=['--privacy', '--drop-link-after', '3', '--reencrypt-fail', 'drop'],
         tags=('privacy', 'reencrypt-drop'), streams=2, lossy=True, duration=24, min_audio=0.4),
    Case('reencrypt-key-missing', 'stereo', CONFIG,
         sink_args=['--privacy', '--drop-link-after', '3', '--reencrypt-fail', 'key-missing'],
         tags=('privacy', 'reencrypt-key-missing'), streams=2, lossy=True, duration=14),
]

IDENTITY = '00:A2:D0:00:00:12'


def check(case, ctx):
    if 'privacy' not in case.tags:
        return []
    failures = []
    log, sink_log = ctx['bluecaster_log'], ctx['sink_log']
    rpas = re.findall(r'new RPA ([0-9A-F:]{17})', sink_log)
    if not rpas:
        failures.append('the sink did not change its RPA')
    first = ctx['sink_addresses'][0].upper()
    if first == IDENTITY:
        failures.append('the sink did not advertise an RPA')
    if f'has the identity address {IDENTITY}' not in log:
        failures.append('BlueCaster did not learn the identity address from pairing')
    if not re.search(rf'resolves to {IDENTITY}', log):
        failures.append('BlueCaster did not resolve an RPA to the identity address')
    if rpas and not re.search(rf'advertises as {re.escape(rpas[0])} now \(identity {IDENTITY}\)', log, re.I):
        failures.append(f'find() did not match the new RPA {rpas[0]} to the saved one')
    connects = re.findall(r'connecting with ([0-9A-F:]{17})', log)
    if rpas and rpas[0] not in connects:
        failures.append(f'BlueCaster did not connect to the new RPA {rpas[0]} (connected with {connects})')
    ids = re.findall(r'LE Audio: \S+ ready \(device (\d+)\)', log)
    if len(ids) < 2:
        failures.append(f'expected 2 connections, the log has {len(ids)}')
    elif len(set(ids)) != 1:
        failures.append(f'the device id changed on reconnect: {ids}')
    if 'LE Audio stream restored' not in log:
        failures.append('the stream was not set up again after the reconnect')
    pairings = log.count('paired and bonded')
    if 'reencrypt-drop' in case.tags:
        if 'the sink ends the link during re-encryption' not in sink_log:
            failures.append('the sink did not end the link during the re-encryption')
        if not re.search(r'encryption with the stored key did not complete \(0x13\): bond kept', log):
            failures.append('BlueCaster did not keep the bond when the link went down during re-encryption')
        if pairings != 1:
            failures.append(f'expected 1 pairing (the bond kept), the log has {pairings}')
        # The RPA the sink took after the failed attempt resolves with the
        # kept IRK and is connected
        if len(rpas) < 2:
            failures.append(f'the sink took {len(rpas)} new RPA(s), expected 2')
        elif rpas[1] not in connects:
            failures.append(f'BlueCaster did not connect to the RPA after the failed attempt {rpas[1]} '
                            f'(connected with {connects})')
    if 'reencrypt-key-missing' in case.tags:
        if 'the sink has no key for the re-encryption' not in sink_log:
            failures.append('the sink did not reject the re-encryption')
        if 'the device has no key for the stored bond (0x06): pairing again' not in log:
            failures.append('BlueCaster did not pair again when the sink had no key')
        if pairings != 2:
            failures.append(f'expected 2 pairings, the log has {pairings}')
    return failures
