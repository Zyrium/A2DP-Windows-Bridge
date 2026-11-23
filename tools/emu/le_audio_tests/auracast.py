"""Auracast cases of run_test.py --auracast: what BlueCaster broadcasts
(--auracast, --auracast-layout), as the broadcast sinks of
emu_le_audio_sink.py see it (le_audio/broadcast_sink.py; one sink per
broadcast, each following its name suffix).

Checked from the logs (bluecaster_decode does not read BIS yet, so the audio
is not decoded): each broadcast's name and Broadcast_ID, its Public
Broadcast Announcement quality, its BASE (subgroups, LC3 configuration, BIS
and their locations, Program_Info labels), the BIG the controller was asked
for (RTN, max SDU), and that every BIS delivered SDUs of the right length
for the whole duration without a sequence gap. The SDU with sequence number
300 of a left and a right BIS must differ (the test tone is 1 kHz left,
1.5 kHz right), and BIS that carry the same side at the same configuration
must be identical (a stereo source's front on the rear outputs too)."""

import dataclasses
import re

NAME = 'BlueCaster test'
FL, FR = 0x1, 0x2


@dataclasses.dataclass
class Broadcast:
    suffix: str               # name suffix its sink follows (None: the name as it is)
    hz: int
    octets: int
    bis: tuple                # per BIS: (allocation, 'L' / 'R' / 'M': the side of the tone it carries)
    labels: tuple = ()        # a subgroup per BIS, each with this Program_Info; (): one subgroup


@dataclasses.dataclass
class AuracastCase:
    name: str                 # also the output folder: <out>/auracast-<name>
    broadcasts: list
    rtn: int                  # of every BIG (LE Create BIG)
    bluecaster_args: list = dataclasses.field(default_factory=list)
    code: str = None          # Broadcast_Code, given to BlueCaster and the sinks
    log: tuple = ()           # texts bluecaster.log must have


def one(suffix, side, hz=48000, octets=120):
    return Broadcast(suffix, hz, octets, ((0, side),))


def stereo(suffix, hz=48000, octets=120):
    return Broadcast(suffix, hz, octets, ((FL, 'L'), (FR, 'R')))


def layout(name):
    return ['--auracast-layout', name]


CASES = [
    # One broadcast, Front Left / Right, the 48 kHz preset with its RTN
    AuracastCase('stereo', [stereo(None)], rtn=4),
    # One mono BIS of the downmix at 24 kHz
    AuracastCase('mono-24k', [one(None, 'M', 24000, 60)], rtn=4,
                 bluecaster_args=['--auracast-mono', '--le-preset', '24_2_2']),
    # Encrypted: the sink syncs with the same Broadcast_Code
    AuracastCase('encrypted', [stereo(None)], rtn=4, code='secret'),
    # Left and right as two broadcasts of one mono BIS each
    AuracastCase('split', [one(' L', 'L'), one(' R', 'R')], rtn=4, bluecaster_args=['--auracast-split']),
    # Matrix layouts (more than two BIS: RTN 2). The test tone is a stereo
    # source, so the mixer plays the front on the rear outputs too.
    # Front and rear as two stereo broadcasts
    AuracastCase('matrix-pairs', [stereo(' Front'), stereo(' Rear')], rtn=2, bluecaster_args=layout('pairs')),
    # Each speaker a mono broadcast of its own: four BIGs
    AuracastCase('matrix-quad', [one(' FL', 'L'), one(' FR', 'R'), one(' RL', 'L'), one(' RR', 'R')], rtn=2,
                 bluecaster_args=layout('quad')),
    # A stereo front and two mono rears: three BIGs
    AuracastCase('matrix-front-rear', [stereo(' Front'), one(' RL', 'L'), one(' RR', 'R')], rtn=2,
                 bluecaster_args=layout('front-rear')),
    # The rear as one broadcast of two labelled subgroups
    AuracastCase('matrix-rear-subgroups',
                 [stereo(' Front'),
                  Broadcast(' Rear', 48000, 120, ((0, 'L'), (0, 'R')), labels=('Rear left', 'Rear right'))],
                 rtn=2, bluecaster_args=layout('rear-subgroups')),
    # The rear only, each output a broadcast (two BIS: the preset's RTN)
    AuracastCase('matrix-rear-only', [one(' RL', 'L'), one(' RR', 'R')], rtn=4,
                 bluecaster_args=layout('matrix-off-separate')),
    # The rear at 24 kHz beside a 48 kHz front: its own BIG, BASE and SDU
    # length, the encoder decimating its channels
    AuracastCase('matrix-mixed-rates', [stereo(' Front'), stereo(' Rear', 24000, 60)], rtn=2,
                 bluecaster_args=layout('pairs') + ['--auracast-rear-preset', '24_2_2'],
                 log=('decimated from 48000 Hz',)),
]

CREATE_BIG = re.compile(r'BlueCaster: LE Create BIG: BIG (\d+) on advertising set \d+, (\d+) BIS, '
                        r'SDU interval (\d+) us, max SDU (\d+), max transport latency \d+ ms, RTN (\d+), '
                        r'PHY \S+, \w+, (\w+)')
SYNC_LOST_BIS = re.compile(r'BIS (\d+): (\d+) SDUs of ([\d/]*) octets( \(\d+ sequence gaps\))?')


def sink_tag(b):
    return 'Auracast sink' + (f' {b.suffix.strip()}' if b.suffix else '')


def check_broadcast(case, b, sink_log, duration):
    """Failures of one broadcast, and (Broadcast_ID, presentation delay,
    {BIS index: digest}) as far as its sink logged them"""
    tag = sink_tag(b)
    text = '\n'.join(line.split(f'{tag}: ', 1)[1] for line in sink_log.splitlines() if f'{tag}: ' in line)
    fail = []
    found = [None, None, {}]

    def say(what):
        fail.append(f'{tag}: {what}')

    m = re.search(r'broadcast from \S+ SID \d+: Broadcast ID 0x([0-9A-F]{6}), Broadcast Name "(.*)"', text)
    if not m:
        say('found no broadcast')
        return fail, found
    found[0] = int(m.group(1), 16)
    if m.group(2) != NAME + (b.suffix or ''):
        say(f'Broadcast Name "{m.group(2)}", expected "{NAME + (b.suffix or "")}"')
    m = re.search(r'Public Broadcast Announcement: features 0x([0-9A-F]{2})', text)
    # PBP features: bit 0 encrypted, bit 1 standard quality, bit 2 high quality
    expected = (0x04 if b.hz == 48000 else 0x02) | (0x01 if case.code else 0)
    if not m:
        say('no Public Broadcast Announcement')
    elif int(m.group(1), 16) != expected:
        say(f'Public Broadcast Announcement features 0x{m.group(1)}, expected 0x{expected:02X}')

    m = re.search(r'BASE: presentation delay (\d+) us, (\d+) subgroup', text)
    if not m:
        say('no BASE')
        return fail, found
    found[1] = int(m.group(1))
    subgroups = re.findall(r'BASE subgroup (\d+): (\S+), (\d+) Hz, (\d+) us, (\d+) octets, .*?metadata (.*), (\d+) BIS',
                           text)
    want = len(b.labels) or 1
    if int(m.group(2)) != want or len(subgroups) != want:
        say(f'BASE with {m.group(2)} subgroup(s), expected {want}')
    for k, (_, codec, hz, frame_us, octets, metadata, n) in enumerate(subgroups):
        if (codec, int(hz), int(frame_us), int(octets)) != ('LC3', b.hz, 10000, b.octets):
            say(f'BASE subgroup {k + 1}: {codec} {hz} Hz {frame_us} us {octets} octets, '
                f'expected LC3 {b.hz} Hz 10000 us {b.octets} octets')
        if int(n) != (1 if b.labels else len(b.bis)):
            say(f'BASE subgroup {k + 1}: {n} BIS')
        if b.labels and k < len(b.labels) and b.labels[k] not in metadata:
            say(f'BASE subgroup {k + 1}: metadata "{metadata}" without the label "{b.labels[k]}"')
    allocations = re.findall(r'BASE BIS (\d+): allocation (\S+)', text)
    expected = [(str(i + 1), f'0x{a:x}' if a else 'none') for i, (a, _) in enumerate(b.bis)]
    if allocations != expected:
        say(f'BASE BIS allocations {allocations}, expected {expected}')

    m = re.search(r'BIGInfo: (\d+) BIS, .*?max SDU (\d+), .*?, (encrypted|open)', text)
    if not m:
        say('no BIGInfo')
    elif (int(m.group(1)), int(m.group(2)), m.group(3)) != (len(b.bis), b.octets, 'encrypted' if case.code else 'open'):
        say(f'BIGInfo: {m.group(1)} BIS, max SDU {m.group(2)}, {m.group(3)}')
    if 'BIG sync established' not in text:
        say('no BIG sync')
        return fail, found
    m = re.search(r'BIG sync lost \(reason 0x[0-9A-F]{2}\): (.*)', text)
    if not m:
        say('the BIG did not end (no "BIG sync lost")')
        return fail, found
    counts = SYNC_LOST_BIS.findall(m.group(1))
    if len(counts) != len(b.bis):
        say(f'SDUs on {len(counts)} BIS, expected {len(b.bis)}')
    for index, n, lengths, gaps in counts:
        if lengths != str(b.octets):
            say(f'BIS {index}: SDUs of {lengths or "no"} octets, expected {b.octets}')
        if gaps:
            say(f'BIS {index}:{gaps}')
        # 100 SDUs a second; the sink takes a moment to find and sync
        if int(n) < 80 * duration:
            say(f'BIS {index}: only {n} SDUs in {duration} s')
    if len({n for _, n, _, _ in counts}) > 1:
        say(f'its BIS did not get the same number of SDUs: {m.group(1)}')
    found[2] = {int(i): d for i, d in re.findall(r'BIS (\d+): SDU \d+: ([0-9a-f]+)', text)}
    if len(found[2]) != len(b.bis):
        say(f'SDU digests of {len(found[2])} BIS, expected {len(b.bis)}')
    return fail, found


def check(case, sink_log, bluecaster_log, duration):
    """Failure texts and a one-line summary"""
    fail = []
    ids, delays, digests = [], set(), {}
    for b in case.broadcasts:
        f, (broadcast_id, delay, sdus) = check_broadcast(case, b, sink_log, duration)
        fail += f
        if broadcast_id is not None:
            ids.append(broadcast_id)
        if delay is not None:
            delays.add(delay)
        for index, digest in sdus.items():
            if index <= len(b.bis):
                # Per configuration and side: the digests seen
                digests.setdefault((b.hz, b.octets, b.bis[index - 1][1]), set()).add(digest)
    # Broadcast_IDs count up from the first
    if ids and ids != list(range(ids[0], ids[0] + len(ids))):
        fail.append(f'Broadcast_IDs {[f"0x{i:06X}" for i in ids]} do not count up')
    if len(delays) > 1:
        fail.append(f'the broadcasts announce different presentation delays: {sorted(delays)}')
    for (hz, octets, side), seen in sorted(digests.items()):
        if len(seen) > 1:
            fail.append(f'{hz} Hz, side {side}: the BIS carry different audio in the same SDU')
        other = digests.get((hz, octets, 'R')) if side == 'L' else None
        if other and seen & other:
            fail.append(f'{hz} Hz: a left and a right BIS carry the same audio')

    # In BIG handle order: the order of the layout's broadcasts
    bigs = sorted(CREATE_BIG.findall(sink_log), key=lambda big: int(big[0]))
    if len(bigs) != len(case.broadcasts):
        fail.append(f'{len(bigs)} LE Create BIG, expected {len(case.broadcasts)}')
    for (handle, num_bis, interval, max_sdu, rtn, crypt), b in zip(bigs, case.broadcasts):
        got = (int(num_bis), int(interval), int(max_sdu), int(rtn), crypt)
        want = (len(b.bis), 10000, b.octets, case.rtn, 'encrypted' if case.code else 'open')
        if got != want:
            fail.append(f'BIG {handle}: (BIS, SDU interval, max SDU, RTN, encryption) {got}, expected {want}')
    for problem in re.findall(r'problem: (.*)', sink_log):
        fail.append(f'sink: {problem}')
    if 'late SDU' in sink_log:
        fail.append('SDUs reached the emulated controller too late for a real one (sink.log: "late SDU")')
    for text in ('Auracast: stopped',) + tuple(case.log):
        if text not in bluecaster_log:
            fail.append(f'bluecaster.log: no "{text}"')
    m = re.search(r'LE Audio: stopped sending: (\d+) SDU sets .*?(\d+) silence, (\d+) underruns', bluecaster_log)
    detail = f'{len(case.broadcasts)} broadcast(s), {sum(len(b.bis) for b in case.broadcasts)} BIS'
    if not m:
        fail.append('no "LE Audio: stopped sending" statistics in bluecaster.log')
    else:
        detail += f', {m.group(1)} SDU sets ({m.group(2)} silence, {m.group(3)} underruns)'
    return fail, detail
