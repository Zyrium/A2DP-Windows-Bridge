"""BAP unicast cases: the stream set-ups BlueCaster plans from the sink's PACS /
ASCS (app/src/crates/le-audio/src/presets.rs), one per emu_le_audio_sink.py layout."""

import re

from .case import MONO_TONES, Case

# bluecaster.log of the cis-refused cases: the CIG removed, then the error
CIS_REFUSED = r'(?s)CIG 1 removed.*cannot stream: CIS 0x[0-9a-f]+ could not be established \(0x1e\)'
# bluecaster.log of the enable-rejected case: the CIG removed, then the error
ENABLE_REJECTED = r'(?s)rejected \(response 0x0e.*CIG 1 removed.*cannot stream: .*rejected \(response 0x0e'

CASES = [
    # One headset, one 2-channel ASE: 1 CIS, LC3 48_6_2 (the best media preset)
    Case('stereo', 'stereo', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 1 CIS'),
    # The same with the capture in the session agent (--capture-agent, the
    # first name of --session-agent): the tone checked over the whole stream
    # shows the PCM arrives intact
    Case('stereo-agent', 'stereo', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 1 CIS',
         bluecaster_args=['--capture-agent']),
    # One headset, two 1-channel ASEs: 2 CIS on one link
    Case('two-ase', 'two-ase', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 2 CIS'),
    # Two earbuds (Front Left / Front Right), one ASE each: 2 CIS on two links
    Case('earbuds', 'earbuds', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 2 CIS'),
    # Mono sink with a forced low-latency preset (--le-preset): downmix, 16 kHz
    Case('mono-16k', 'mono', r'LC3 16000 Hz 10 ms 40 octets, 1 channel\(s\) over 1 CIS',
         tones=MONO_TONES, bluecaster_args=['--le-preset', '16_2_1']),
    # The sink drops the LE link 3 s after BlueCaster connected: BlueCaster tears the
    # stream down (CIS, CIG) and sets it up again on a new link (bond reused)
    Case('link-loss', 'stereo', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 1 CIS',
         sink_args=['--drop-link-after', '3'], streams=2, lossy=True, duration=14),
    # The sink keeps released ASEs in Releasing until the client drops the
    # CIS: BlueCaster disconnects the CIS after Disable, so stop does not wait
    # out the Release timeout
    Case('release-cis', 'two-ase', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 2 CIS',
         sink_args=['--ascs-release-waits-for-cis'], tags=('release-cis',)),
    # No sink context available until 3 s after the connection: Enable waits
    # for the Available Audio Contexts notification instead of failing
    Case('contexts-later', 'stereo', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 1 CIS',
         sink_args=['--available-contexts-after', '3'], tags=('contexts-later',)),
    # AFH host channel classification forced (--afh 6,11; by default BlueCaster
    # sends it depending on the PC's Wi-Fi): the controller must answer it
    # and streaming go on as usual
    Case('afh', 'stereo', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 1 CIS',
         bluecaster_args=['--afh', '6,11']),
    # A controller with LE but no ISO buffers (LE Read Buffer Size v2: 0):
    # BlueCaster must refuse LE Audio with an error that names ISO
    Case('no-iso-buffers', 'stereo', r'.*', sink_args=['--no-iso-buffers'], expect_error=r'ISO'),
    # The controller refuses the second of two CIS (0x1E) while the first
    # comes up (issue #86: BTstack crashed on the refused CIS's handle):
    # BlueCaster must drop the first CIS, remove the CIG and report the CIS
    Case('cis-refused', 'earbuds', r'.*', sink_args=['--refuse-cis', '1'], expect_error=CIS_REFUSED),
    # The same with the first CIS up only after the tear-down reached the
    # CIG removal: asked of BTstack inside that CIS's event, the removal was
    # lost (no LE Remove CIG)
    Case('cis-refused-late', 'earbuds', r'.*', sink_args=['--refuse-cis', '1', '--cis-up-after', '0.5'],
         expect_error=CIS_REFUSED),
    # The sink rejects Enable (0x0E) after Set CIG Parameters, so no CIS was
    # ever created: the CIG must still be removed (BTstack waited for those
    # CIS forever, and the CIGs left behind made Set CIG Parameters fail)
    Case('enable-rejected', 'two-ase', r'.*', sink_args=['--reject-enable'], expect_error=ENABLE_REJECTED),
    # The controller (20 ISO buffers, as a Realtek adapter) completes each SDU
    # 40 ms after its interval, as one holds a PDU the earbud did not
    # acknowledge until its flush point: BlueCaster must queue enough per CIS
    # to send every SDU in time (with 2 per CIS it sent about a third)
    Case('slow-completions', 'earbuds', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 2 CIS',
         sink_args=['--iso-buffers', '20', '--completion-delay-ms', '40'], tags=('slow-completions',)),
    # The controller never completes the SDUs of the second CIS: the first
    # must play on (the sender ran both in lockstep and stopped), the second
    # be reported stalled, and the stream not be set up again
    Case('cis-stalled', 'earbuds', r'LC3 48000 Hz 10 ms 155 octets, 2 channel\(s\) over 2 CIS',
         sink_args=['--withhold-completions', '1'], tones=None, tags=('cis-stalled',)),
]

# bluecaster.log at the end of a stream: each CIS's own counters
CIS_SENT = re.compile(r'LE Audio: CIS 0x([0-9A-F]+): (\d+) SDUs sent, (\d+) completed, (\d+) late')

# The emulator's PACS Sink Audio Locations per sink, in capture order
# (le_audio/bap.py logs them)
SINK_LOCATIONS = re.compile(r'LE Audio sink, locations 0x([0-9a-fA-F]+)')
# bluecaster_decode --le-audio: a CIS of a stream, its Audio_Channel_Allocation
# (Config Codec) and the capture (sink) it is in
CIS_LINE = re.compile(r'^  CIS 0x([0-9A-F]+) +: ASE (\d+) on ACL 0x[0-9A-F]+, location\(s\) (\S+), '
                      r'.*, capture (\d+)$', re.M)


def check_locations(ctx):
    """Each CIS's Audio_Channel_Allocation is within the PACS Sink Audio
    Locations of the sink it goes to (a sink with locations gets some of
    them, one without gets none): e.g. the left earbud Front Left, the right
    one Front Right. That the audio of each allocation is the right channel
    (1 kHz left, 1.5 kHz right) is the tone check's part: the decoder places
    every CIS's channels by their allocation."""
    received = ctx['received']
    if not received:
        return []
    pacs = [int(x, 16) for x in SINK_LOCATIONS.findall(ctx['sink_log'])]
    cis = CIS_LINE.findall(received['text'])
    if not cis:
        return ['no CIS lines (with their capture) in the received report']
    failures = []
    for handle, ase, locs, capture in cis:
        allocation = 0 if locs == 'mono' else sum(int(x, 16) for x in locs.split(','))
        index = int(capture) - 1
        if index >= len(pacs):
            failures.append(f'sink {index + 1}: no PACS locations in sink.log')
            continue
        where = f'sink {index + 1} ({ctx["sink_addresses"][index]}, PACS locations 0x{pacs[index]:x})'
        if pacs[index] == 0 and allocation:
            failures.append(f'{where}: CIS 0x{handle} ASE {ase} got allocation 0x{allocation:x}, expected none')
        elif pacs[index] and (not allocation or allocation & ~pacs[index]):
            failures.append(f'{where}: CIS 0x{handle} ASE {ase} got allocation '
                            f'{"none" if not allocation else f"0x{allocation:x}"}, not within its locations')
    return failures


def check(case, ctx):
    failures = []
    log = ctx['bluecaster_log']
    failures += check_locations(ctx)
    # Bumble's complaints about a command the emulated controller did not
    # answer (BTstack then waits for ever)
    for complaint in ('Async command handlers should return None', 'Sync command handlers should return'):
        if complaint in ctx['sink_log']:
            failures.append(f'emulated controller: "{complaint}" (a command was not answered)')
    if case.name == 'afh' and 'AFH host classification: avoid Wi-Fi' not in log:
        failures.append('BlueCaster did not send the AFH host channel classification (--afh 6,11)')
    if 'Streaming LE Audio:' not in log:
        failures.append('BlueCaster did not report "Streaming LE Audio"')
    # The stream must be torn down in order: Disable / Release / Remove CIG
    for step in ('unicast: Disable', 'unicast: Release', 'CIG 1 removed'):
        if step not in log:
            failures.append(f'teardown step missing from the log: {step}')
    # (the underrun / silence counters are checked by run_test.py)
    m = re.search(r'LE Audio: (\d+) SDU sets sent .*?(\d+) underruns', log)
    if not m:
        failures.append('no LE Audio statistics line')
    if case.name == 'link-loss' and 'LE Audio stream restored' not in log:
        failures.append('the stream was not set up again after the link loss')
    for timeout in ('Disable timed out', 'Release timed out', 'CIG removal timed out'):
        if timeout in log and not case.lossy:
            failures.append(f'teardown: {timeout}')
    if 'release-cis' in case.tags and 'disconnecting CIS' not in log:
        failures.append('BlueCaster did not disconnect the CIS before Release')
    if 'contexts-later' in case.tags and 'no audio context available for Enable' not in log:
        failures.append('BlueCaster did not wait for an available audio context')
    failures += check_cis_counters(case, ctx)
    return failures


def check_cis_counters(case, ctx):
    """Every CIS got its SDUs into the controller in time (none dropped as
    late), except where the case stalls one on purpose (cis-stalled: the
    other CIS plays the whole time, the stalled one is reported and the
    stream is not lost)"""
    log = ctx['bluecaster_log']
    counters = [(h, int(sent), int(late)) for h, sent, _, late in CIS_SENT.findall(log)]
    if not counters:
        return ['no per-CIS counters ("LE Audio: CIS 0x...: N SDUs sent") in bluecaster.log']
    failures = []
    if 'cis-stalled' in case.tags:
        sent = sorted(n for _, n, _ in counters)
        if sent[-1] < 100 * 4:
            failures.append(f'the CIS that completes sent only {sent[-1]} SDUs')
        if sent[0] > 20:
            failures.append(f'the CIS without completions sent {sent[0]} SDUs (expected its queue only)')
        if not re.search(r'CIS 0x[0-9A-F]+: no SDU completed for \d+ s', log):
            failures.append('the CIS without completions was not reported stalled')
        if 'ISO sending stalled' in log or 'stream lost' in log:
            failures.append('the stream was given up although one CIS played')
        return failures
    if not case.lossy:
        for handle, _, late in counters:
            if late > 2:
                failures.append(f'CIS 0x{handle}: {late} SDUs dropped as late')
    if 'slow-completions' in case.tags and not re.search(r'parameters: NSE \d+, BN 1 / \d+, FT 4 / ', log):
        failures.append('LE CIS Established parameters (FT 4) not logged')
    return failures
