"""End-to-end A2DP test of BlueCaster against a virtual sink - no Bluetooth hardware.

    python tools/emu/setup_env.py          (once)
    python tools/emu/run_test.py [--codecs sbc aac ...] [--duration 5]

For each codec: starts emu_sink.py (Bumble virtual link + A2DP sink), runs
bluecaster-core.exe --cli with a test tone over H4/TCP, then checks both HCI captures
with bluecaster_decode:
  - BlueCaster streams the requested codec and bluecaster_decode finds no problem,
    on the sent side and on the received side
  - the sink received every media packet / frame BlueCaster sent
  - decoded audio (all codecs but LDAC, LHDC V5 and aptX Adaptive, which
    have no open-source decoder) is identical on both sides and is the test
    tone: 1 kHz left, 1.5 kHz right, amplitude 0.5
The cases of NEGOTIATION run the same way with another sink or command line
(Auto choosing SBC XQ, or SBC on a sink without dual channel or one that
refuses it; Opus and Opus 05 at 48 kHz
from a 44.1 kHz request, Auto choosing Opus over AAC and LHDC V5 over Opus,
but never Opus 05; SBC with another frame setup from --sbc-* options;
LDAC from a sink listing 18 SEPs). The cases of EXPECT_ERROR must stop with
an error instead, naming the codec that caused it (not offered, in use, refused).

Outputs (captures, logs, reports, WAV) go to tools/emu/out/<codec>/.
Exit code 0 when every codec passed. Uses only the Python standard library;
the sink runs in the tools/emu/.venv environment.

    python tools/emu/run_test.py --le-audio [case ...]

runs the experimental LE Audio cases instead (le_audio_tests/: stereo,
two-ase, earbuds, mono-16k and the profiles' own): emu_le_audio_sink.py with
the case's layout, bluecaster-core.exe --cli --le-audio with the test tone, and
bluecaster_decode --le-audio on both sides:
  - one LC3 stream of the expected configuration on both sides, consecutive
    ISO SDU sequence numbers, no decoder problems
  - the sinks received every SDU BlueCaster sent; decoded audio identical on both
    sides and the test tone (1 kHz left, 1.5 kHz right; mono: both) in every
    100 ms of every stream but a short gap at its start and end
  - no SDU reached the emulated controller too late for a real one (its log
    says "late SDU"), no underrun in BlueCaster's counters, audio (not counting
    inserted silence) for most of the duration
  - the checks of le_audio_tests/<profile>.py (core.py: each CIS's channel
    allocation within the PACS locations of the sink it goes to; mic.py: the
    headset microphone of the mic-* cases, run with --headset-mic
    --mic-output <out>/le-<case>/mic.wav)
Outputs go to tools/emu/out/le-<case>/.

    python tools/emu/run_test.py --auracast [case ...]

runs the Auracast cases instead (le_audio_tests/auracast.py: stereo, mono,
encrypted, split and the matrix layouts): emu_le_audio_sink.py --layout
auracast with one broadcast sink per broadcast, bluecaster-core.exe --cli
--auracast with the test tone. Checked from both logs (the BIS audio is
not decoded):
  - each broadcast's name, Broadcast_ID, announced quality and BASE
    (subgroups, LC3 configuration, BIS locations, labels), one presentation
    delay for all
  - the BIG asked of the controller for each (BIS, max SDU, RTN)
  - every BIS delivered SDUs of its length for the whole duration, without
    a sequence gap and none too late; left and right differ, and BIS with
    the same audio and configuration carry the same SDU
Outputs go to tools/emu/out/auracast-<case>/.

    python tools/emu/run_test.py --scan [case ...]

runs the device scan cases instead (eir, name-request): emu_sink.py answers
the inquiry (with its name in the extended inquiry response, or without it),
bluecaster-core.exe --cli --scan lists it:
  - listed once (it answers several times per inquiry), with its name, an
    RSSI and as an audio device
  - listed within 3 s of the inquiry's start (the sink answers after 1 s),
    not at the scan's end; its name from the inquiry result within 3 s, or
    from a name request after the first inquiry
Outputs go to tools/emu/out/scan-<case>/.

    python tools/emu/run_test.py --voice [cvsd msbc lc3swb cvsd-roundtrip cvsd-auto lc3swb-msbc lc3swb-cvsd msbc-gain
                                          msbc-agent cvsd-agent-killed]

runs the HFP audio cases instead (headset mode: bluecaster-core --headset-mic
--mic-output <out>/mic.wav): emu_sink.py --hfp <codec> adds a hands-free
unit (cvsd: without codec negotiation, msbc / lc3swb: offering those codecs)
and carries SCO between the virtual controllers (hfp_audio.py); BlueCaster
streams SBC with the test tone, and once the HFP service level connection is
up suspends A2DP, opens the audio connection and sends the test tone (mixed
to mono) over it; the hands-free unit sends a 1.5 kHz tone back (CVSD as PCM,
mSBC / LC3-SWB as frames encoded by BlueCaster's codecs, voice_frames.py):
  - the audio connection is established with the expected codec (BlueCaster's
    log; the sink's log for the negotiated codec)
  - the battery level the hands-free unit sends behind AT+XAPL, before the
    OK to it, is answered OK and shown (70 %)
  - SCO packets in both directions in both HCI captures, at about the air
    rate (CVSD 16-bit: 267/s, transparent: 133/s, each way)
  - the sink received SCO packets: cvsd, with the test tone's 1 kHz in the
    PCM; msbc / lc3swb, each packet an H2 frame in sequence (no decoder
    here: voice-link/tests/emu_frames.rs checks the encoder's frames)
  - mic.wav, the microphone as BlueCaster decoded it, holds the 1.5 kHz tone
cvsd-roundtrip: headset mode on 3 s after streaming starts and off at 6 s
(--headset-mic-toggle 3,6); also A2DP media after the voice link closed and
the test tone in the last second the sink decoded.
cvsd-auto: --headset-mic auto; a WAV output counts as recorded from, so the
checks are cvsd's, plus Auto's "an application records" in the log.
lc3swb-msbc / lc3swb-cvsd: the hands-free unit offers LC3-SWB, BlueCaster is
given --voice-codec msbc / cvsd (the call codec chosen with Auto-select off):
the link must come up with that codec instead.
msbc-gain: msbc with --mic-gain 6: the microphone's processing runs, so
mic.wav holds the tone 6 dB louder.
msbc-agent: msbc with --session-agent: the microphone is decoded in the core
and written to mic.wav by the session agent (render channel).
cvsd-agent-killed: cvsd with --session-agent and the agent killed 1.5 s
after it opened the microphone's output: the core opens the output and the
uplink capture again through the next agent; the next agent's mic.wav
(written anew) holds the tone, the sink the test tone.
Outputs go to tools/emu/out/voice-<case>/.

    python tools/emu/run_test.py --le-pairing [link-key link-key-ct2 no-link-key replaces-classic drops-authenticated]

runs the dual-mode pairing cases instead (issue #86): one emu_sink.py
(--dual-mode --le-audio-adv --check-link-key) for several bluecaster-core runs
with the same settings folder, LE ones (--le-audio: pairs over LE, then ends
with an error, as the sink has no LE Audio services) and classic ones (A2DP
SBC). The sink pairs over LE with LinkKey in its key distribution or not
(--le-link-key on / off; link-key-ct2: h7) and authenticates a classic link
only with the link key it has for BlueCaster:
  - link-key: after the LE pairing BlueCaster's link_keys.txt holds the key
    the sink derived (type 7), and the classic connection authenticates with
    it, without pairing
  - no-link-key: no link key after the LE pairing; the classic connection
    pairs
  - replaces-classic: an older (unauthenticated) link key, then an LE
    pairing with LinkKey: the derived key replaces it and authenticates the
    next classic connection
  - drops-authenticated: a classic pairing (authenticated), then an LE
    pairing with LinkKey (Just Works): the older key is dropped, not
    replaced, and the next classic connection pairs
Outputs go to tools/emu/out/lepair-<case>/.

    python tools/emu/run_test.py --hid [hid-connect hid-incoming ... hid-le-audio-untouched]

runs the keyboard and mouse cases instead (experimental HID host, issue
#413): emu_hid.py is a classic HID keyboard, mouse and consumer keys that
sends a fixed script of input reports once its channels are open;
bluecaster-core --cli --hid-test prints the HID host's counts once a second
(dry run: over --hci-tcp nothing reaches Windows):
  - hid-connect: BlueCaster opens the channels (--hid-connect); every report
    arrives and is decoded into the events the script implies, none injected
    although --hid-inject is given
  - hid-incoming: the device opens them itself and is on the list; the same
  - hid-refused: the device is not on the list: its control channel is
    refused, nothing is counted but the refusal
  - hid-release: the device drops the link with a key still held: one event
    more (the release), and nothing connected at the end
  - hid-off: a core without --hid-* (the scan mode): the HID PSMs are not
    there, the device's control channel is refused as an unknown PSM
  - hid-kept-descriptor: the device connects by itself and types at once,
    twice on one configuration: the first run may drop the first report
    (the descriptor is read after the channels open), the second decodes
    every report with the descriptor kept from the first
  - hid-leds: as hid-incoming, with the test lights --hid-leds 5: the
    device gets them as output report 1 (Num Lock and Scroll Lock); every
    other case checks that no lights are sent without --hid-leds
  - hid-incoming-helper, hid-release-helper: as hid-incoming and
    hid-release, with the input through a real input helper process that
    records what it would pass to Windows (--hid-helper-record, debug
    builds only: run these with a debug bluecaster-core); every event is
    recorded, none dropped, the held key released after the drop
  - hid-passkey: a keyboard that can only type and asks for MITM: BlueCaster
    (adding it with --hid-connect) answers as a display, prints
    "hid passkey shown <digits>" (over --hci-tcp only), the harness hands
    the digits to the device, which types them; pairing succeeds and the
    script's input arrives. Every other case checks that no number is shown
    (hid-connect: a device without input or output pairs with Just Works)
and the LE ones: emu_hid_le.py is the same keyboard, mouse and consumer
keys over HID over GATT (BlueCaster's controller there keeps a filter accept
list, which the background connection uses):
  - hid-le-connect: BlueCaster connects (background connection), pairs
    (Just Works, one SMP Pairing Request, one encryption), subscribes to the
    three input reports; every report decoded, none undecoded
  - hid-le-reconnect: the device drops the link after its script and
    advertises again: BlueCaster connects again by itself, encrypts with
    the stored bond (no second Pairing Request, two encryptions) and the
    script's input arrives again; the key held at the drop is released
  - hid-le-release: the same drop, the device stays away: one release
    event, nothing connected at the end
  - hid-le-not-listed: another LE device is on the list: the emulated one
    is never connected to and nothing is subscribed to
  - hid-le-scan: --hid-scan lists the device as an LE keyboard or mouse;
    the device scan of LE Audio (--scan --le-audio, keyboards and mice off)
    does not list it
  - hid-le-audio-untouched: keyboards and mice on, LE Audio off: only the
    HID module runs, the device reads BlueCaster's GATT services and finds
    none of LE Audio, no LE Audio advertiser is listed
  - hid-le-leds: a background connection with --hid-leds 5: the lights
    are written to the Output report (Report Reference type 2)
  - hid-le-privacy: the device advertises with a resolvable private
    address, is listed with it, pairs and tells its identity and IRK; it
    drops the link and comes back with another private address, which the
    accept list (BlueCaster's controller here does not resolve) cannot
    match: BlueCaster's passive scan resolves it with the IRK and connects,
    without pairing again, and the script's input arrives again
  - hid-le-early-report: as hid-le-reconnect, and the device sends a mouse
    move as soon as the second link is encrypted (before BlueCaster reads
    its services): counted, decoded with the kept report handles
  - hid-le-release-helper: hid-le-release through a recording input
    helper (debug builds only)
  - hid-le-passkey: the keyboard asks for MITM: BlueCaster shows a number,
    the device types it, pairing and input as hid-le-connect (which, without
    MITM, still pairs with Just Works and shows no number)
and the check of a device being added (--hid-confirm, a short countdown
instead of the app's 30 s):
  - hid-confirm, hid-le-confirm: the device sends its script within the
    countdown: "hid check confirmed", and it stays connected
  - hid-confirm-silent, hid-le-confirm-silent: the device connects and sends
    nothing: "hid check removed no_input" after the countdown, and
    BlueCaster closes its link
Outputs go to tools/emu/out/hid-<case>/.
"""

import argparse
import glob
import json
import math
import os
import queue
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import wave

import le_audio_tests
import le_audio_tests.auracast

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
VENV_PYTHON = os.path.join(HERE, '.venv', 'Scripts', 'python.exe')
if not os.path.exists(VENV_PYTHON) and sys.prefix != sys.base_prefix:
    # No .venv here (e.g. a git worktree): run the sink with the virtual
    # environment this script runs in (one with bumble installed)
    VENV_PYTHON = sys.executable
SINK_ADDRESS = '00:A2:D0:00:00:02'
BASE_PORT = 9101  # A2DP cases; LE Audio cases use BASE_PORT + 100

CODECS = ['sbc', 'sbcxq', 'aac', 'aptx', 'aptxhd', 'aptxll', 'ldac', 'lhdcv5', 'faststream', 'opus', 'opus05']
CONFIG_PATTERN = {  # bluecaster_decode's "Configuration :" line
    'sbc': r'SBC \d+ Hz, joint stereo, 16 blocks, 8 subbands, loudness, bitpool 2-53$',
    # SBC XQ: dual channel up to bitpool 47 (44.1 kHz) / 43 (48 kHz)
    'sbcxq': r'SBC (44100 Hz, dual channel, .*bitpool 2-47|48000 Hz, dual channel, .*bitpool 2-43)$',
    # Sinks with a low max bitpool: the configuration (and every frame,
    # bluecaster_decode checks) stays within it
    'sbc-bp35': r'SBC \d+ Hz, joint stereo, 16 blocks, 8 subbands, loudness, bitpool 2-35$',
    'sbcxq-bp45': r'SBC 44100 Hz, dual channel, 16 blocks, 8 subbands, loudness, bitpool 2-45$',
    'sbcxq-bp39': r'SBC \d+ Hz, dual channel, 16 blocks, 8 subbands, loudness, bitpool 2-39$',
    # SBC XQ not held to the sink's list (issue #86): up to SBC's 53
    'sbcxq-bp53': r'SBC \d+ Hz, dual channel, 16 blocks, 8 subbands, loudness, bitpool 2-53$',
    'sbc-bp28': r'SBC \d+ Hz, joint stereo, 16 blocks, 8 subbands, loudness, bitpool 2-28$',
    # A profile's SBC frame setup (--sbc-*), in the configuration and every frame
    'sbc-8b-4sb-snr': r'SBC \d+ Hz, joint stereo, 8 blocks, 4 subbands, SNR, bitpool 2-53$',
    'sbc-stereo-12b': r'SBC 44100 Hz, stereo, 12 blocks, 8 subbands, loudness, bitpool 2-53$',
    'aac': r'AAC ', 'aptx': r'aptX \d', 'aptxhd': r'aptX HD \d',
    'aptxll': r'aptX LL \d', 'ldac': r'LDAC \d', 'lhdcv5': r'LHDC V5 \d',
    'ldac-96k': r'LDAC 96000',
    'ldac-48k': r'LDAC 48000',
    'faststream': r'FastStream \d+ Hz, direction 0x01 \(music\)',   # no voice backchannel
    'opus': r'Opus 48000 Hz, stereo, 20 ms',
    # PipeWire's choice: 10 ms, one coupled stream, 312 x 1024 bps, no return channel
    'opus05': r'Opus 05 48000 Hz, stereo, 10 ms frames, location 0x00000003, max 312 x 1024 bps, no return channel$',
    # The Px7 S2e's configuration from a host source: its latencies with LL0
    # raised to 80 ms and HQ's 0 to 200 ms; an R2 sink gets the source's own
    'aptxad': r'aptX Adaptive 48000 Hz, stereo, R1, source type 2, target latency LL0 80 ms, LL1 106 ms, '
              r'HQ 200 ms, TWS 350 ms$',
    'aptxad-r2': r'aptX Adaptive 48000 Hz, stereo, R2, source type 2, target latency LL0 80 ms, LL1 100 ms, '
                 r'HQ 200 ms, TWS 200 ms$',
    'aptxad-r2-96k': r'aptX Adaptive 96000 Hz, stereo, R2, source type 2, target latency LL0 80 ms, LL1 100 ms, '
                     r'HQ 200 ms, TWS 200 ms$',
}
# Frame headers are validated, audio is not decoded (aptX Adaptive: its RTP
# header, packet sizes and frame types)
NO_DECODER = {'ldac', 'ldac-96k', 'ldac-48k', 'lhdcv5', 'aptxad', 'aptxad-r2', 'aptxad-r2-96k'}
# Cases run with the developer override on
APTXAD_SINK = ['--codecs', 'sbc', 'aac', 'aptx', 'aptxad']
# The name they give tells the vendor (issue #237); LOG_CHECKS read the sink's log
HUAWEI_SINK = ['--codecs', 'sbc', 'aac', '--log-level', 'INFO', '--name', 'HUAWEI FreeBuds Pro 4']
HUAWEI_NAME = ['--device-name', 'HUAWEI FreeBuds Pro 4']
OPPO_SINK = ['--codecs', 'sbc', 'aac', 'lhdcv5', '--log-level', 'INFO', '--name', 'OPPO Enco Free4']
# A device that gives another name, whatever it is saved as
OTHER_NAME = ['--name', 'Other Buds']
OPPO_NAME = ['--device-name', 'OPPO Enco Free4']
OPPO_TWICE = OPPO_NAME + ['--sessions', '2']
CTKD_SINK = ['--codecs', 'sbc', '--ctkd', '--log-level', 'INFO']
CTKD_ON = {'debug_mode': True, 'le_audio_experimental': True}
# Further cases: name -> (BlueCaster -c, emu_sink.py arguments, the codec expected,
# more BlueCaster arguments)
NEGOTIATION = {
    # The playback EQ on (--eq, EQ_POINTS): a cut leaves the level alone, a
    # boost is taken off again by the preamp
    'sbc-eq': ('sbc', [], 'sbc', ['--eq', '1500:-6:2']),
    'aac-eq': ('aac', [], 'aac', ['--eq', '1000:6:4']),
    'aptxhd-eq': ('aptxhd', [], 'aptxhd', ['--eq', '1000:6:4']),  # 24-bit
    'sbcxq-44k': ('sbcxq', [], 'sbcxq', ['--rate', '44100']),            # bitpool 47
    # A codec chosen on its card that the sink does not list (the profile's
    # codec_forced, issue #86): LDAC to a sink that lists SBC only
    'force-ldac': ('ldac', ['--codecs', 'sbc'], 'ldac', ['--force-codec']),
    'auto-sbcxq': ('auto', ['--codecs', 'sbc'], 'sbcxq', []),            # Auto takes SBC XQ over SBC
    'auto-sbc-no-xq': ('auto', ['--codecs', 'sbc', '--sbc-no-dual'], 'sbc', []),  # no dual channel: SBC
    # Dual channel listed but refused: SBC XQ falls back to SBC
    'auto-sbcxq-refused': ('auto', ['--codecs', 'sbc', '--sbc-reject-dual'], 'sbc', []),
    # Max bitpool 35: SBC capped there
    'sbc-bp35': ('sbc', ['--codecs', 'sbc', '--sbc-max-bitpool', '35'], 'sbc-bp35', []),
    # ... unless its card shows more than the sink lists (the profile's
    # unlisted_options, issue #86): BlueCaster's own range
    'sbc-bp35-unlisted': ('sbc', ['--codecs', 'sbc', '--sbc-max-bitpool', '35'], 'sbc', ['--unlisted-options']),
    # Stops the codec causes (issue #86, EXPECT_ERROR): a chosen codec the
    # sink does not list, one it holds for another source, a configuration
    # it refuses (SBC XQ's dual channel, chosen, so no SBC instead)
    'ldac-not-offered': ('ldac', ['--codecs', 'sbc'], None, []),
    'ldac-in-use': ('ldac', ['--in-use', 'ldac'], None, []),
    'sbcxq-chosen-refused': ('sbcxq', ['--codecs', 'sbc', '--sbc-reject-dual'], None, []),
    # Max bitpool 39 with dual channel (Moto buds+, issue #86): Auto takes
    # SBC XQ capped at 39, as PipeWire; below 29 it does not (plain SBC)
    'auto-sbcxq-bp39': ('auto', ['--codecs', 'sbc', '--sbc-max-bitpool', '39'], 'sbcxq-bp39', []),
    'auto-sbc-bp28': ('auto', ['--codecs', 'sbc', '--sbc-max-bitpool', '28'], 'sbc-bp28', []),
    'sbcxq-bp45': ('sbcxq', ['--sbc-max-bitpool', '45'], 'sbcxq-bp45', ['--rate', '44100']),
    # Its card's "Show only options it supports" off: bitpool 53 (issue #86)
    'sbcxq-unlisted-53': ('sbcxq', [], 'sbcxq-bp53', ['--unlisted-options', '--sbc-bitpool', '53']),
    # SBC frame setups other than the default (a profile's SBC settings)
    'sbc-8b-4sb-snr': ('sbc', [], 'sbc-8b-4sb-snr', ['--sbc-blocks', '8', '--sbc-subbands', '4', '--sbc-alloc', 'snr']),
    'sbc-stereo-12b': ('sbc', [], 'sbc-stereo-12b',
                       ['--rate', '44100', '--sbc-mode', 'stereo', '--sbc-blocks', '12', '--sbc-bitpool', '40']),
    # SBC / SBC XQ with ABR (the adaptive bitpool): the emulated link keeps
    # up, so the bitpool stays at the ceiling; every frame within the range
    'sbc-abr': ('sbc', [], 'sbc', ['-a']),
    'sbcxq-abr': ('sbcxq', [], 'sbcxq', ['-a', '--rate', '44100']),
    # Developer mode's encode at send time (DEV_SETTINGS), with ABR and without
    'sbcxq-abr-hold': ('sbcxq', [], 'sbcxq', ['-a', '--rate', '44100']),
    'ldac-hold': ('ldac', [], 'ldac', []),
    'faststream-44k': ('faststream', [], 'faststream', ['--rate', '44100']),
    # Auto never takes FastStream, even over SBC
    'auto-not-faststream': ('auto', ['--codecs', 'sbc', 'faststream', '--sbc-no-dual'], 'sbc', []),
    # Opus: 48 kHz whatever the capture rate asked for; Auto takes it over
    # AAC and SBC, and LHDC V5 over it (and over LDAC: issue #86's order)
    'opus-from-44k': ('opus', [], 'opus', ['--rate', '44100']),
    'auto-opus': ('auto', ['--codecs', 'sbc', 'aac', 'opus'], 'opus', []),
    'auto-lhdcv5-over-opus': ('auto', ['--codecs', 'sbc', 'aac', 'opus', 'lhdcv5'], 'lhdcv5', []),
    'auto-lhdcv5-over-ldac': ('auto', ['--codecs', 'sbc', 'aac', 'ldac', 'lhdcv5'], 'lhdcv5', []),
    # A codec the sink does not offer, with a profile's fallback: the best
    # one it offers, not the next one below it (issue #86 round 8: Opus
    # asked of an EarFun gave aptX, not aptX Adaptive); aptX HD missing:
    # LDAC above it, not AAC below
    'fallback-lhdcv5-to-ldac': ('lhdcv5', ['--codecs', 'sbc', 'aac', 'ldac'], 'ldac', ['--fallback']),
    # LHDC V5 low latency asked for (issue #543): the sink lists it (P9 0x40)
    'lhdcv5-ll': ('lhdcv5', [], 'lhdcv5', ['--lhdcv5-ll']),
    'fallback-aptxhd-to-ldac': ('aptxhd', ['--codecs', 'sbc', 'aac', 'ldac'], 'ldac', ['--fallback']),
    'fallback-opus-to-aptxad': ('opus', APTXAD_SINK, 'aptxad', ['--fallback']),
    # Opus 05: 48 kHz from a 44.1 kHz request; Auto never takes it (AAC here)
    'opus05-from-44k': ('opus05', [], 'opus05', ['--rate', '44100']),
    'auto-not-opus05': ('auto', ['--codecs', 'sbc', 'aac', 'opus05'], 'aac', []),
    # aptX Adaptive (offered by the sink only when named, with SBC, AAC and
    # aptX as aptX Adaptive headsets list them: emu_sink.py): HQ, low latency
    # (SSRC 0xA1, 348-byte packets), an R2 sink in HQ and LL (frame types 3 /
    # 4), ABR, 48 kHz from a 44.1 kHz request; 96 kHz when asked for, in HQ
    # to an R2 sink that lists it (emu_sink.py's R2 caps do), else 48 kHz (an
    # R1 sink, low latency); Auto takes it over aptX and AAC (issue #86)
    'aptxad': ('aptxad', APTXAD_SINK, 'aptxad', []),
    # aptX LL to a sink with the extended capabilities, which also offers
    # aptX Adaptive (a FiiO BTR13): a 17-byte configuration (issue #520)
    'aptxll-ext': ('aptxll', ['--codecs', 'sbc', 'aac', 'aptx', 'aptxll', 'aptxad', '--aptxll-ext'], 'aptxll', []),
    'aptxad-ll': ('aptxad', APTXAD_SINK, 'aptxad', ['-q', 'll']),
    'aptxad-r2': ('aptxad', APTXAD_SINK + ['--aptxad-r2'], 'aptxad-r2', []),
    'aptxad-r2-ll': ('aptxad', APTXAD_SINK + ['--aptxad-r2'], 'aptxad-r2', ['-q', 'll']),
    'aptxad-abr': ('aptxad', APTXAD_SINK, 'aptxad', ['-a']),
    'aptxad-from-44k': ('aptxad', APTXAD_SINK, 'aptxad', ['--rate', '44100']),
    'aptxad-r2-96k': ('aptxad', APTXAD_SINK + ['--aptxad-r2'], 'aptxad-r2-96k', ['--rate', '96000']),
    'aptxad-r2-96k-abr': ('aptxad', APTXAD_SINK + ['--aptxad-r2'], 'aptxad-r2-96k', ['--rate', '96000', '-a']),
    # Real sinks' rate bytes (issue #86): an EarFun Air Pro 4+ lists 0xD1
    # (96 kHz by Android's 0x80 bit, no 0x20), a Bose QC Ultra 0x51 (44.1
    # and 48 kHz only)
    'aptxad-r2-96k-earfun': ('aptxad', APTXAD_SINK + ['--aptxad-r2', '--aptxad-rates', '0xD1'], 'aptxad-r2-96k',
                             ['--rate', '96000']),
    'aptxad-r2-96k-bose': ('aptxad', APTXAD_SINK + ['--aptxad-r2', '--aptxad-rates', '0x51'], 'aptxad-r2',
                           ['--rate', '96000']),
    'aptxad-r1-from-96k': ('aptxad', APTXAD_SINK, 'aptxad', ['--rate', '96000']),
    'aptxad-r2-ll-from-96k': ('aptxad', APTXAD_SINK + ['--aptxad-r2'], 'aptxad-r2', ['--rate', '96000', '-q', 'll']),
    'auto-aptxad': ('auto', APTXAD_SINK, 'aptxad', []),
    # The rate on Auto: 48 kHz where the sink lists it, not its highest
    # (issue #86: 96 kHz only when chosen), to an R2 aptX Adaptive sink
    # that takes 96 kHz and an LDAC one; LDAC 96 kHz when chosen
    'auto-aptxad-r2-48k': ('auto', APTXAD_SINK + ['--aptxad-r2'], 'aptxad-r2', []),
    'auto-ldac-48k': ('auto', ['--codecs', 'sbc', 'aac', 'ldac'], 'ldac-48k', []),
    'ldac-chosen-96k': ('ldac', ['--codecs', 'sbc', 'aac', 'ldac'], 'ldac-96k', ['--rate', '96000']),
    # 18 free sink SEPs, LDAC the 14th: BTstack 1.8.1 overflowed its 10-entry
    # SEP discovery array (crash); none may be dropped either
    'many-seps-ldac': ('ldac', ['--extra-seps', '8'], 'ldac', []),
    # Huawei earbuds that list LDAC once told "sound quality first" on their
    # control channel (issue #30): LDAC chosen connects again and gets it,
    # with the mode off or on already, or when they end the link at the
    # write; earbuds without the mode, or another name, are left alone (a
    # write would give them LDAC here)
    'huawei-ldac': ('ldac', HUAWEI_SINK + ['--huawei', 'off'], 'ldac', HUAWEI_NAME),
    'huawei-ldac-mode-on': ('ldac', HUAWEI_SINK + ['--huawei', 'on'], 'ldac', HUAWEI_NAME),
    'huawei-ldac-drop': ('ldac', HUAWEI_SINK + ['--huawei', 'off', '--huawei-drop'], 'ldac', HUAWEI_NAME),
    'huawei-no-mode': ('ldac', HUAWEI_SINK + ['--huawei', 'off', '--huawei-silent'], 'aac',
                       HUAWEI_NAME + ['--fallback']),
    'huawei-other-name': ('ldac', HUAWEI_SINK + ['--huawei', 'off'] + OTHER_NAME, 'aac', HUAWEI_NAME + ['--fallback']),
    # The write taken and still no LDAC: AAC, the mode back off when it was off
    'huawei-no-ldac': ('ldac', HUAWEI_SINK + ['--huawei', 'off', '--huawei-no-ldac'], 'aac',
                       HUAWEI_NAME + ['--fallback']),
    'huawei-no-ldac-drop': ('ldac', HUAWEI_SINK + ['--huawei', 'off', '--huawei-no-ldac', '--huawei-drop'], 'aac',
                            HUAWEI_NAME + ['--fallback']),
    'huawei-no-ldac-mode-on': ('ldac', HUAWEI_SINK + ['--huawei', 'on', '--huawei-no-ldac'], 'aac',
                               HUAWEI_NAME + ['--fallback']),
    # The serial port on a channel of its own, which SDP does not list
    'huawei-channel-1': ('ldac', HUAWEI_SINK + ['--huawei', 'off', '--huawei-channel', '1'], 'ldac', HUAWEI_NAME),
    'huawei-channel-16': ('ldac', HUAWEI_SINK + ['--huawei', 'off', '--huawei-channel', '16'], 'ldac', HUAWEI_NAME),
    # Enco Free4 earbuds that cap LHDC V5 until asked for Hi-Res (issue #237):
    # one new connection for the write gives the first stream 1000 kbps, the
    # second goes by what they list, with no write; with the mode off or on
    # already, by whichever write they advertise, or ending the link for it
    'oppo-hi-res': ('lhdcv5', OPPO_SINK + ['--oppo', 'off'], 'lhdcv5', OPPO_TWICE),
    'oppo-hi-res-on': ('lhdcv5', OPPO_SINK + ['--oppo', 'on'], 'lhdcv5', OPPO_TWICE),
    'oppo-codec-write': ('lhdcv5', OPPO_SINK + ['--oppo', 'off', '--oppo-codec-write'], 'lhdcv5', OPPO_TWICE),
    'oppo-drop': ('lhdcv5', OPPO_SINK + ['--oppo', 'off', '--oppo-drop'], 'lhdcv5', OPPO_NAME),
    # Asked once: three streams, one read of the earbuds
    'oppo-once': ('lhdcv5', OPPO_SINK + ['--oppo', 'off'], 'lhdcv5', OPPO_NAME + ['--sessions', '3']),
    # Nothing gained, seen after the new connection: Hi-Res back off when it
    # was off, as it was when it was on
    'oppo-no-change': ('lhdcv5', OPPO_SINK + ['--oppo', 'off', '--oppo-no-change'], 'lhdcv5', OPPO_TWICE),
    'oppo-no-change-on': ('lhdcv5', OPPO_SINK + ['--oppo', 'on', '--oppo-no-change'], 'lhdcv5', OPPO_TWICE),
    # Another model (an Enco X4) asked the same way
    'oppo-other-product': ('lhdcv5', OPPO_SINK + ['--oppo', 'off', '--oppo-product', '06EC10'], 'lhdcv5',
                           OPPO_TWICE),
    # Left alone: no feature write listed, spatial audio on, another name
    'oppo-no-write': ('lhdcv5', OPPO_SINK + ['--oppo', 'off', '--oppo-no-write'], 'lhdcv5', OPPO_NAME),
    'oppo-spatial': ('lhdcv5', OPPO_SINK + ['--oppo', 'off', '--oppo-spatial'], 'lhdcv5', OPPO_NAME),
    'oppo-other-name': ('lhdcv5', OPPO_SINK + ['--oppo', 'off'] + OTHER_NAME, 'lhdcv5', OPPO_NAME),
    # Auto picks LHDC V5: asked the same way
    'oppo-auto': ('auto', OPPO_SINK + ['--oppo', 'off'], 'lhdcv5', OPPO_NAME),
    # Saved under another name (issue #237): the name they give tells them
    'oppo-renamed': ('lhdcv5', OPPO_SINK + ['--oppo', 'off'], 'lhdcv5', ['--device-name', 'test', '--sessions', '2']),
    'huawei-renamed': ('ldac', HUAWEI_SINK + ['--huawei', 'off'], 'ldac', ['--device-name', 'test']),
    # Developer mode's vendor settings (DEV_SETTINGS): OPPO's protocol for
    # earbuds of any name, for none, and Hi-Res asked on every connection
    'oppo-always': ('lhdcv5', OPPO_SINK + ['--oppo', 'off'] + OTHER_NAME, 'lhdcv5', []),
    'oppo-never': ('lhdcv5', OPPO_SINK + ['--oppo', 'off'], 'lhdcv5', OPPO_NAME),
    # (earbuds that keep their limit: each stream asks, gains nothing, undoes it)
    'oppo-every-connection': ('lhdcv5', OPPO_SINK + ['--oppo', 'off', '--oppo-no-change'], 'lhdcv5', OPPO_TWICE),
    # LE keys derived from the link key (issue #86; DEV_SETTINGS): asked
    # after pairing, and the sink's LTK, IRK and
    # identity address kept (CTKD_CASES); the next connection, from a sink
    # that lists SMP over BR/EDR only from then on; none from a sink without
    # it, one that refuses or one that never answers; with developer mode
    # off or LE Audio off too (the keys do not depend on LE Audio: SM
    # alone, nothing of LE Audio set up); the stream as always
    'ctkd': ('sbc', CTKD_SINK, 'sbc', ['--sessions', '2']),
    'ctkd-next-connection': ('sbc', CTKD_SINK + ['--ctkd-from', '2', '--ctkd-public-identity'], 'sbc',
                             ['--sessions', '3']),
    'ctkd-unlisted': ('sbc', CTKD_SINK + ['--ctkd-from', '99'], 'sbc', []),
    'ctkd-refused': ('sbc', CTKD_SINK + ['--ctkd-refuse'], 'sbc', []),
    'ctkd-silent': ('sbc', CTKD_SINK + ['--ctkd-silent'], 'sbc', []),
    'ctkd-developer-off': ('sbc', CTKD_SINK, 'sbc', []),
    'ctkd-le-off': ('sbc', CTKD_SINK, 'sbc', []),
    # The capture in the session agent (--session-agent, or its first name
    # --capture-agent: the test tone made there, its PCM through shared
    # memory, requests over the agent pipe): the decoded tone shows it
    # arrives intact; a 44.1 kHz source reaches the encoder as it is; an
    # agent killed mid-stream (AGENT_KILL) ends the capture as a lost device
    # does, and the core starts a new one. AGENT_FIRST: an agent started by
    # hand before the core serves it (the core starts none), and goes back
    # to waiting when the core exits; or is killed mid-stream and the core
    # starts its own
    'sbc-agent': ('sbc', [], 'sbc', ['--session-agent']),
    'aac-agent': ('aac', [], 'aac', ['--capture-agent']),
    'sbcxq-44k-agent': ('sbcxq', [], 'sbcxq', ['--rate', '44100', '--session-agent']),
    'sbc-agent-killed': ('sbc', [], 'sbc', ['--session-agent']),
    'sbc-agent-first': ('sbc', [], 'sbc', ['--session-agent']),
    'sbc-agent-replaced': ('sbc', [], 'sbc', ['--session-agent']),
}
# --capture-agent cases whose agent is killed: when, as a share of --duration
# after the agent started (after the middle of the stream, which
# tone_check reads)
AGENT_KILL = {'sbc-agent-killed': 0.7, 'sbc-agent-replaced': 0.7}
AGENT_STARTED = re.compile(r'session agent: started \(process (\d+)\)')
# The agent cases with an agent started by hand before the core (its
# output: agent.log); it is the one killed in AGENT_KILL
AGENT_FIRST = {'sbc-agent-first', 'sbc-agent-replaced'}
AGENT_ON = [('bluecaster.log', 'audio capture: through a capture agent', 1),
            ('bluecaster.log', 'session agent: started (process', 1),
            ('bluecaster.log', 'session agent: connected (process', 1),
            ('bluecaster.log', 'capture agent: capturing', 1),
            # The agent's own lines, in the core's log
            ('bluecaster.log', '[agent] capture agent: channel opened (1 open)', True),
            ('bluecaster.log', 'capture agent: the agent exited', False)]
# Cases that must stop with an error before streaming: what the CLI's log
# says of it (its "Codec stop" line carries the structured cause)
EXPECT_ERROR = {
    'ldac-not-offered': r'Codec stop: ldac Unsupported',
    'ldac-in-use': r'Codec stop: ldac InUse',
    'sbcxq-chosen-refused': r'Codec stop: sbcxq Refused',
}
# The settings.json a case starts with (developer mode)
DEV_SETTINGS = {
    'sbcxq-abr-hold': {'debug_mode': True, 'dev_overrides': {'encode_at_send': True}},
    'ldac-hold': {'debug_mode': True, 'dev_overrides': {'encode_at_send': True}},
    'oppo-always': {'debug_mode': True, 'dev_vendors': {'oppo': {'mode': 'always'}}},
    'oppo-never': {'debug_mode': True, 'dev_vendors': {'oppo': {'mode': 'never'}}},
    'oppo-every-connection': {'debug_mode': True, 'dev_vendors': {'oppo': {'options': ['every_connection']}}},
    'ctkd': CTKD_ON,
    'ctkd-next-connection': CTKD_ON,
    'ctkd-unlisted': CTKD_ON,
    'ctkd-refused': CTKD_ON,
    'ctkd-silent': CTKD_ON,
    'ctkd-developer-off': {'debug_mode': False, 'le_audio_experimental': True},
    'ctkd-le-off': {'debug_mode': True, 'le_audio_experimental': False},
}
QUALITY_ON, QUALITY_OFF = 'Huawei control: 2BA2 010101', 'Huawei control: 2BA2 010100'
HI_RES_ON, HI_RES_OFF, HI_RES_CODEC = 'OPPO control: 0403 1801', 'OPPO control: 0403 1800', 'OPPO control: 041A 080100'
LL_ASKED = 'LHDC V5: asking for low latency'
ENCODED_1000, ENCODED_400 = 'encoder ready: LHDC V5 1000 kbps', 'encoder ready: LHDC V5 400 kbps'
CTKD_GIVEN, CTKD_ASKING = f'CTKD: {SINK_ADDRESS} gave its LE keys', f'CTKD: asking {SINK_ADDRESS} for its LE keys'
PAIRED = f'new link key from pairing with {SINK_ADDRESS}'
# What these cases' logs must, and must not, have: name -> (log, text, whether
# it is there, or how many times)
LOG_CHECKS = {
    # Issue #543: low latency only when asked for; then 10 ms encoding, the
    # 100 ms send queue limit and the queue guard (ABR off)
    'lhdcv5': [('bluecaster.log', LL_ASKED, False), ('bluecaster.log', 'LHDC V5: encode interval 20 ms', True)],
    'lhdcv5-ll': [('bluecaster.log', LL_ASKED, True), ('bluecaster.log', 'LHDC V5: encode interval 10 ms', True),
                  ('bluecaster.log', 'packets dropped after 100 ms (LHDC V5 low latency)', True),
                  ('bluecaster.log', 'LHDC V5: queue guard on', True)],
    'sbcxq-abr-hold': [('bluecaster.log', 'encode at send time', True)],
    'ldac-hold': [('bluecaster.log', 'encode at send time', True)],
    'huawei-ldac': [('sink.log', QUALITY_ON, True), ('sink.log', QUALITY_OFF, False),
                    ('bluecaster.log', 'Emulated phone', False)],
    'huawei-no-mode': [('sink.log', 'Huawei control: 2BA2', False)],
    'huawei-other-name': [('sink.log', 'Huawei control', False)],
    'huawei-no-ldac': [('sink.log', QUALITY_ON, True), ('sink.log', QUALITY_OFF, True)],
    'huawei-no-ldac-drop': [('sink.log', QUALITY_ON, True), ('sink.log', QUALITY_OFF, True)],
    'huawei-no-ldac-mode-on': [('sink.log', QUALITY_ON, True), ('sink.log', QUALITY_OFF, False)],
    'oppo-hi-res': [('sink.log', HI_RES_ON, 1), ('bluecaster.log', ENCODED_400, False), ('bluecaster.log', ENCODED_1000, 2),
                    ('sink.log', 'AVDTP connection', 3)],
    'oppo-hi-res-on': [('sink.log', HI_RES_ON, 1), ('bluecaster.log', ENCODED_400, False), ('bluecaster.log', ENCODED_1000, 2)],
    'oppo-codec-write': [('sink.log', HI_RES_CODEC, 1), ('sink.log', HI_RES_ON, False),
                         ('bluecaster.log', ENCODED_400, False), ('bluecaster.log', ENCODED_1000, 2)],
    'oppo-drop': [('sink.log', HI_RES_ON, True), ('bluecaster.log', ENCODED_1000, True), ('bluecaster.log', ENCODED_400, False)],
    'oppo-once': [('sink.log', 'OPPO control: 0100', 1), ('bluecaster.log', ENCODED_1000, 3)],
    'oppo-no-change': [('sink.log', HI_RES_ON, 1), ('sink.log', HI_RES_OFF, 1), ('bluecaster.log', ENCODED_400, 2),
                       ('sink.log', 'AVDTP connection', 3)],
    'oppo-no-change-on': [('sink.log', HI_RES_ON, 1), ('sink.log', HI_RES_OFF, False),
                          ('bluecaster.log', ENCODED_400, 2)],
    'oppo-other-product': [('sink.log', HI_RES_ON, 1), ('bluecaster.log', ENCODED_400, False),
                           ('bluecaster.log', ENCODED_1000, 2)],
    'oppo-no-write': [('sink.log', 'OPPO control: 0403', False), ('bluecaster.log', ENCODED_400, True)],
    'oppo-spatial': [('sink.log', 'OPPO control: 0403', False), ('bluecaster.log', ENCODED_400, True)],
    'oppo-other-name': [('sink.log', 'OPPO control', False), ('bluecaster.log', ENCODED_400, True)],
    'oppo-auto': [('sink.log', HI_RES_ON, 1), ('bluecaster.log', ENCODED_1000, 1), ('bluecaster.log', ENCODED_400, False)],
    'oppo-renamed': [('sink.log', HI_RES_ON, 1), ('bluecaster.log', ENCODED_400, False), ('bluecaster.log', ENCODED_1000, 2),
                     ('bluecaster.log', 'name=“OPPO Enco Free4”', 1)],
    'huawei-renamed': [('sink.log', QUALITY_ON, True), ('bluecaster.log', 'name=“HUAWEI FreeBuds Pro 4”', True)],
    'oppo-always': [('sink.log', HI_RES_ON, 1), ('bluecaster.log', ENCODED_1000, 1), ('bluecaster.log', 'remote name of', False)],
    'oppo-never': [('sink.log', 'OPPO control', False), ('bluecaster.log', ENCODED_400, True)],
    'oppo-every-connection': [('sink.log', HI_RES_ON, 2), ('sink.log', HI_RES_OFF, 2), ('bluecaster.log', ENCODED_400, 2),
                              ('sink.log', 'AVDTP connection', 4)],
    # Paired once: the link key stays valid after the keys (a later
    # connection authenticates with it), and they are asked for only once
    'sbc-agent': AGENT_ON,
    'aac-agent': AGENT_ON,
    'sbcxq-44k-agent': AGENT_ON + [('bluecaster.log', 'capture: 44100 Hz, 2 channel(s), F32', True)],
    'sbc-agent-killed': [('bluecaster.log', 'capture agent: the agent exited, capture lost', 1),
                         ('bluecaster.log', 'audio capture ended (Some(AgentLost))', 1),
                         ('bluecaster.log', 'audio capture: device opened again, capturing', 1),
                         ('bluecaster.log', 'session agent: started (process', 2),
                         ('bluecaster.log', 'session agent: connected (process', 2),
                         ('bluecaster.log', 'capture agent: capturing', 2)],
    'sbc-agent-first': [('bluecaster.log', 'session agent: started (process', False),
                        ('bluecaster.log', 'session agent: connected (process', 1),
                        ('bluecaster.log', 'capture agent: capturing', 1),
                        ('bluecaster.log', '[agent] capture agent: channel opened (1 open)', True),
                        ('bluecaster.log', 'capture agent: the agent exited', False),
                        ('agent.log', 'session agent: waiting for a core', 2),
                        ('agent.log', 'session agent: the core went away, waiting for the next one', 1)],
    'sbc-agent-replaced': [('bluecaster.log', 'session agent: started (process', 1),
                           ('bluecaster.log', 'session agent: connected (process', 2),
                           ('bluecaster.log', 'capture agent: the agent exited, capture lost', 1),
                           ('bluecaster.log', 'audio capture ended (Some(AgentLost))', 1),
                           ('bluecaster.log', 'audio capture: device opened again, capturing', 1),
                           ('bluecaster.log', 'capture agent: capturing', 2)],
    'ctkd': [('bluecaster.log', CTKD_GIVEN, 1), ('sink.log', 'CTKD: keys LTK', 1), ('bluecaster.log', PAIRED, 1),
             ('bluecaster.log', CTKD_ASKING, False)],
    'ctkd-next-connection': [('sink.log', 'SMP over BR/EDR not listed (connection 1)', 1),
                             ('sink.log', 'SMP over BR/EDR listed (connection 2)', 1),
                             ('bluecaster.log', CTKD_ASKING, 1), ('bluecaster.log', CTKD_GIVEN, 1),
                             ('sink.log', 'CTKD: keys LTK', 1), ('bluecaster.log', PAIRED, 1)],
    'ctkd-unlisted': [('sink.log', 'SMP over BR/EDR not listed', True), ('bluecaster.log', CTKD_GIVEN, False),
                      ('sink.log', 'CTKD:', False),
                      ('bluecaster.log', f'CTKD: {SINK_ADDRESS} does not take SMP over BR/EDR', True)],
    'ctkd-refused': [('sink.log', 'CTKD: refusing', 1), ('bluecaster.log', CTKD_GIVEN, False),
                     ('sink.log', 'CTKD: keys LTK', False),
                     ('bluecaster.log', f'CTKD: no LE keys from {SINK_ADDRESS} (Refused', 1)],
    'ctkd-silent': [('sink.log', 'CTKD: ignoring SMP PDU', True), ('bluecaster.log', CTKD_GIVEN, False),
                    # NoAnswer after SM's 30 s (--duration 35), else the link ends
                    # first and the next connection asks again (NotAsked)
                    ('bluecaster.log', f'CTKD: no LE keys from {SINK_ADDRESS} (', 1)],
    'ctkd-developer-off': [('bluecaster.log', CTKD_GIVEN, 1), ('sink.log', 'CTKD: keys LTK', 1),
                           ('bluecaster.log', 'LE keys: initializing', True),
                           ('bluecaster.log', 'LE Audio: initializing', False)],
    'ctkd-le-off': [('bluecaster.log', CTKD_GIVEN, 1), ('sink.log', 'CTKD: keys LTK', 1),
                    ('bluecaster.log', 'LE keys: initializing', True),
                    ('bluecaster.log', 'LE Audio: initializing', False)],
}
# Cases whose LE keys BlueCaster keeps must equal the sink's (ctkd_check)
CTKD_CASES = {'ctkd', 'ctkd-next-connection', 'ctkd-developer-off', 'ctkd-le-off'}
TONES = ((1000.0, 0.5), (1500.0, 0.5))  # (frequency, amplitude) per channel, as --test-tone
# The EQ points (Hz, dB, Q; peaking) of the NEGOTIATION cases that turn it on
EQ_POINTS = {'sbc-eq': [(1500.0, -6.0, 2.0)], 'aac-eq': [(1000.0, 6.0, 4.0)], 'aptxhd-eq': [(1000.0, 6.0, 4.0)]}


def peaking_db(points, freq, rate):
    """The response (dB) of peaking points at freq, as BlueCaster's EQ
    filters them (RBJ cookbook)"""
    total = 0.0
    for f0, gain, q in points:
        w0 = 2 * math.pi * f0 / rate
        alpha, a = math.sin(w0) / (2 * q), 10 ** (gain / 40)
        b = (1 + alpha * a, -2 * math.cos(w0), 1 - alpha * a)
        d = (1 + alpha / a, -2 * math.cos(w0), 1 - alpha / a)
        z = complex(math.cos(2 * math.pi * freq / rate), -math.sin(2 * math.pi * freq / rate))
        h = (b[0] + b[1] * z + b[2] * z * z) / (d[0] + d[1] * z + d[2] * z * z)
        total += 20 * math.log10(abs(h))
    return total


def eq_tones(points, rate):
    """The test tone after the EQ: each channel's tone shaped by the points
    and lowered by the preamp (the highest boost)"""
    top = min(20000.0, rate * 0.49)
    grid = [20.0 * (top / 20.0) ** (i / 191) for i in range(192)] + [p[0] for p in points]
    preamp = max([0.0] + [peaking_db(points, f, rate) for f in grid])
    return tuple(((f, a * 10 ** ((peaking_db(points, f, rate) - preamp) / 20)),) for f, a in TONES)
LATE_SDU_FAILURE = 'reached the controller after their SDU interval began'  # LE Audio cases run once more


def parse_report(text):
    """First stream of an bluecaster_decode report."""
    def find(pattern, cast=str):
        m = re.search(pattern, text, re.M)
        return cast(m.group(1)) if m else None
    return {
        'config': find(r'Configuration : (.*)$'),
        'packets': find(r'Media packets : (\d+)', int),
        'frames': find(r'Frames\s+: (\d+)', int),
        'audio_s': find(r'Audio\s+: ([\d.]+) s', float),
        'wav': find(r'WAV\s+: (.*)$'),
        'streams': len(re.findall(r'^=== Stream ', text, re.M)),
        'result_ok': re.search(r'^RESULT: OK', text, re.M) is not None,
        'problems': re.findall(r'^    - (.*)$', text, re.M),
    }


def read_wav(path):
    with wave.open(path) as w:
        ch, width, rate, n = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes()
        raw = w.readframes(n)
    full = float(1 << (8 * width - 1))
    samples = [[] for _ in range(ch)]
    if width == 2:
        values = struct.unpack(f'<{n * ch}h', raw)
        for c in range(ch):
            samples[c] = [v / full for v in values[c::ch]]
        return rate, samples
    for i in range(n * ch):
        b = raw[i * width:(i + 1) * width]
        if width == 3:
            b += b'\xff' if b[2] & 0x80 else b'\x00'
        samples[i % ch].append(struct.unpack('<i', b)[0] / full)
    return rate, samples


def tone_block(xs, rate, expected):
    """(ok, notes) for one block of samples: each expected (frequency,
    amplitude) there within 0.05, and > 95% of the energy in them"""
    count = len(xs)
    mean_sq = sum(x * x for x in xs) / count
    energy, ok, notes = 0.0, True, []
    for freq, amp in expected:
        k = 2 * math.cos(2 * math.pi * freq / rate)
        s1 = s2 = 0.0
        for x in xs:
            s1, s2 = x + k * s1 - s2, s1
        tone_amp = 2 * math.sqrt(max(s1 * s1 + s2 * s2 - k * s1 * s2, 0.0)) / count
        energy += tone_amp * tone_amp / 2
        ok &= abs(tone_amp - amp) < 0.05
        notes.append(f'{freq:.0f} Hz amp {tone_amp:.3f}')
    share = energy / mean_sq if mean_sq else 0.0
    ok &= share > 0.95
    notes[-1] += f' ({100 * share:.1f}%)'
    return ok, notes


def tone_check(path, tones=None, eq=None):
    """Returns (ok, description) for the test tone in the middle of the WAV.
    tones: per channel the (frequency, amplitude) pairs expected there
    (default: TONES, one per channel; eq: through these EQ points)."""
    rate, channels = read_wav(path)
    tones = tones or (eq_tones(eq, rate) if eq else tuple((t,) for t in TONES))
    if len(channels) != 2 or len(channels[0]) < rate:
        return False, f'expected >= 1 s of stereo audio, got {len(channels)} ch'
    start, count = len(channels[0]) // 2 - rate // 4, rate // 2
    notes, ok = [], True
    for xs, expected in zip(channels, tones):
        block_ok, block_notes = tone_block(xs[start:start + count], rate, expected)
        ok &= block_ok
        notes += block_notes
    return ok, ', '.join(notes)


# LE Audio: the tone is checked in blocks of TONE_BLOCK_S over the whole
# stream. Only the start and the end may lack it: BlueCaster sends encoded
# silence until its jitter buffer holds a few SDU sets (5 x 10 ms) and LC3
# needs a frame or two to settle; at the end BlueCaster stops the test tone
# 100 ms before the stream (its buffer runs empty: silence). Up to
# MAX_START_GAP_S / MAX_END_GAP_S are excluded there. Any other block without
# the tone (an underrun's silence, a lost or doubled SDU) fails.
TONE_BLOCK_S = 0.1    # whole periods of 1 kHz and 1.5 kHz
MAX_START_GAP_S = 0.3
MAX_END_GAP_S = 0.3
# BlueCaster's underrun counter: the one when the test tone stops before the
# stream (above) is not counted, i.e. underruns in the last END_UNDERRUN_S of
# the last stream
END_UNDERRUN_S = 0.3


def tone_check_stream(path, tones, max_end_gap=MAX_END_GAP_S):
    """(ok, description) for the test tone over the whole WAV of a stream,
    except gaps of up to MAX_START_GAP_S at the start and `max_end_gap` at
    the end"""
    rate, channels = read_wav(path)
    n = int(rate * TONE_BLOCK_S)
    if len(channels) != 2 or len(channels[0]) < rate:
        return False, f'expected >= 1 s of stereo audio, got {len(channels)} ch, {len(channels[0]) / rate:.2f} s'
    blocks = len(channels[0]) // n
    good, bad, notes, first_notes = [], [], None, None
    for b in range(blocks):
        ok, block_notes = True, []
        for xs, expected in zip(channels, tones):
            block_ok, ch_notes = tone_block(xs[b * n:(b + 1) * n], rate, expected)
            ok &= block_ok
            block_notes += ch_notes
        if ok:
            good.append(b)
            if notes is None and b >= blocks // 2:
                notes = ', '.join(block_notes)   # a typical block, for the summary
            first_notes = first_notes or ', '.join(block_notes)
        else:
            bad.append((b, ', '.join(block_notes)))
    if not good:
        return False, f'no {TONE_BLOCK_S * 1000:.0f} ms block with the tone in {blocks * TONE_BLOCK_S:.1f} s'
    start_gap, end_gap = good[0] * TONE_BLOCK_S, (blocks - 1 - good[-1]) * TONE_BLOCK_S
    bad = [x for x in bad if good[0] < x[0] < good[-1]]
    desc = (f'{notes or first_notes}; {blocks * TONE_BLOCK_S:.1f} s checked, gaps {start_gap:.1f} s at the start, '
            f'{end_gap:.1f} s at the end')
    problems = []
    if start_gap > MAX_START_GAP_S + 1e-9:
        problems.append(f'tone only from {start_gap:.1f} s (at most {MAX_START_GAP_S} s expected)')
    if end_gap > max_end_gap + 1e-9:
        problems.append(f'no tone in the last {end_gap:.1f} s (at most {max_end_gap} s expected)')
    if bad:
        b, block_notes = bad[0]
        problems.append(f'{len(bad)} block(s) without the tone after it started, first at '
                        f'{b * TONE_BLOCK_S:.1f} s: {block_notes}')
    return not problems, '; '.join(problems + [desc])


def free_port(port):
    """`port`, or the next one nobody listens on (another program or test run
    may hold it)."""
    for p in range(port, port + 50):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            try:
                s.bind(('127.0.0.1', p))
            except OSError:
                continue
        if p != port:
            print(f'    port {port} is in use, the sink takes {p}')
        return p
    raise RuntimeError(f'no free TCP port from {port} to {port + 49}')


def start_sink(port, capture, log_path, script='emu_sink.py', extra=()):
    """Starts a virtual sink; its READY line is in proc.ready_line."""
    log = open(log_path, 'w', encoding='utf-8')
    # The sink writes UTF-8 whatever the system locale (a Japanese Windows
    # would write cp932, and a non-ASCII log line, e.g. MCP's simulated
    # track title, would arrive with U+FFFD)
    env = dict(os.environ, PYTHONIOENCODING='utf-8', PYTHONUTF8='1')
    proc = subprocess.Popen(
        [VENV_PYTHON, '-u', os.path.join(HERE, script), '--port', str(port), '--capture', capture]
        + list(extra),
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding='utf-8', errors='replace', env=env)
    lines = queue.Queue()

    def pump():
        for line in proc.stdout:
            log.write(line)
            log.flush()
            lines.put(line)
        log.close()
    threading.Thread(target=pump, daemon=True).start()
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            line = lines.get(timeout=0.5)
            if line.startswith('READY'):
                proc.ready_line = line.strip()
                return proc
        except queue.Empty:
            if proc.poll() is not None:
                break
    proc.kill()
    raise RuntimeError(f'virtual sink did not start, see {log_path}')


def read_secret(path):
    """A file of the pairing folder: encrypted for the Windows user
    (win-sys dpapi.rs: "BCDPAPI1" + a DPAPI blob), or plain text as older
    builds wrote it"""
    import ctypes
    from ctypes import wintypes

    class Blob(ctypes.Structure):
        _fields_ = [('cbData', wintypes.DWORD), ('pbData', ctypes.POINTER(ctypes.c_char))]

    try:
        with open(path, 'rb') as f:
            data = f.read()
    except FileNotFoundError:
        return b''
    magic = b'BCDPAPI1'
    if not data.startswith(magic):
        return data
    blob = data[len(magic):]
    entropy = b'BlueCaster pairing keys'
    src = Blob(len(blob), ctypes.cast(ctypes.create_string_buffer(blob, len(blob)), ctypes.POINTER(ctypes.c_char)))
    ent = Blob(len(entropy), ctypes.cast(ctypes.create_string_buffer(entropy, len(entropy)),
                                         ctypes.POINTER(ctypes.c_char)))
    out = Blob()
    if not ctypes.windll.crypt32.CryptUnprotectData(ctypes.byref(src), None, ctypes.byref(ent), None, None, 1,
                                                    ctypes.byref(out)):
        raise OSError(f'{path}: cannot be decrypted')
    try:
        return ctypes.string_at(out.pbData, out.cbData)
    finally:
        ctypes.windll.kernel32.LocalFree(out.pbData)


def read_tlv(path):
    """BlueCaster's LE TLV file (the format btstack_tlv_windows wrote):
    tag -> value, the last write of each tag (an empty one deletes it)"""
    data = read_secret(path)
    if not data.startswith(b'BTstack'):
        return {}
    tags, i = {}, 8
    while i + 8 <= len(data):
        tag, size = struct.unpack_from('>II', data, i)
        value = data[i + 8:i + 8 + size]
        i += 8 + size
        if size:
            tags[tag] = value
        else:
            tags.pop(tag, None)
    return tags


def ctkd_check(out):
    """The LE keys BlueCaster keeps of the sink (its bond file: le_device_db's
    'BTD' entry beside the 'AWK' note of the classic address) are the ones the
    sink has: LTK and IRK (BTstack keeps them most significant octet first,
    the sink in wire order) and its identity address"""
    m = re.search(r'CTKD: keys LTK ([0-9a-f]{32}) IRK ([0-9a-f]{32}) identity ([0-9A-F:]{17})',
                  read_text(os.path.join(out, 'sink.log')))
    if not m:
        return ['sink.log: no CTKD keys']
    ltk, irk, identity = m.groups()
    # No link key of the identity address (BTstack would store one, see
    # btstack-sys build.rs patched_sm)
    keys = [line.split()[0] for line
            in read_secret(os.path.join(out, 'config', 'pairing', 'link_keys.txt')).decode().splitlines()
            if line.strip() and not line.startswith('#')]
    if keys != [SINK_ADDRESS.replace(':', '-')]:
        return [f'link_keys.txt holds {keys}']
    path = os.path.join(out, 'config', 'pairing', 'le_audio_bonds.tlv')
    if not os.path.exists(path):
        return [f'no {path}']
    tags = read_tlv(path)
    classic = bytes.fromhex(SINK_ADDRESS.replace(':', ''))
    for tag, value in tags.items():
        if tag >> 8 != int.from_bytes(b'AWK', 'big') or value[:6] != classic:
            continue
        entry = tags.get(int.from_bytes(b'BTD', 'big') << 8 | (tag & 0xFF))
        if entry is None or len(entry) < 46:
            return [f'no LE bond {tag & 0xFF} for the note of {SINK_ADDRESS}']
        failures = []
        if entry[8:14].hex(':').upper() != identity or value[7:13] != entry[8:14]:
            failures.append(f'identity {entry[8:14].hex(":").upper()} kept, the sink gave {identity}')
        if entry[14:30][::-1].hex() != irk:
            failures.append('the IRK kept is not the sink\'s')
        if entry[30:46][::-1].hex() != ltk:
            failures.append('the LTK kept is not the sink\'s')
        return failures
    return [f'no LE keys noted for {SINK_ADDRESS} in {path}']


def start_agent_killer(log_path, delay, pid=None):
    """Kills the session agent `delay` seconds after it started capturing
    (AGENT_KILL): the one the core's log names, or `pid` (an agent this
    script started); the thread's .killed tells whether it did"""
    def run():
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            text = read_text(log_path)
            started = AGENT_STARTED.search(text)
            found = pid if pid and 'capture agent: capturing' in text else started and int(started.group(1))
            if found:
                time.sleep(delay)
                try:
                    os.kill(found, signal.SIGTERM)  # TerminateProcess on Windows
                    thread.killed = True
                except OSError:
                    pass
                return
            time.sleep(0.05)
    thread = threading.Thread(target=run, daemon=True)
    thread.killed = False
    thread.start()
    return thread


def run_codec(args, codec, port):
    out = os.path.join(args.out, codec)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    src_capture = os.path.join(out, 'bluecaster.pklg')
    sink_capture = os.path.join(out, 'sink.pklg')
    failures = []

    cli_codec, sink_args, expected, bluecaster_args = NEGOTIATION.get(codec, (codec, [], codec, []))
    port = free_port(port)
    sink = start_sink(port, sink_capture, os.path.join(out, 'sink.log'), extra=sink_args)
    hand_agent = None
    try:
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'))  # isolated link keys
        if any(a in bluecaster_args for a in ('--session-agent', '--capture-agent')):
            env['BLUECASTER_INSTANCE'] = f'emu-{codec}-{port}'  # its own agent pipe
        if codec in AGENT_FIRST:
            agent_log = open(os.path.join(out, 'agent.log'), 'w', encoding='utf-8', errors='replace')
            hand_agent = subprocess.Popen([args.bluecaster, '--agent'], stdout=agent_log, stderr=subprocess.STDOUT,
                                          stdin=subprocess.DEVNULL, env=env)
            time.sleep(1)
        if codec in DEV_SETTINGS:
            os.makedirs(env['BLUECASTER_CONFIG_DIR'])
            with open(os.path.join(env['BLUECASTER_CONFIG_DIR'], 'settings.json'), 'w', encoding='utf-8') as f:
                json.dump(DEV_SETTINGS[codec], f)
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8', errors='replace') as log:
            killer = (start_agent_killer(os.path.join(out, 'bluecaster.log'), AGENT_KILL[codec] * args.duration,
                                         hand_agent and hand_agent.pid)
                      if codec in AGENT_KILL else None)
            try:
                rc = subprocess.run(
                    [args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}', '-d', SINK_ADDRESS,
                     '-c', cli_codec, '--test-tone', '--duration', str(args.duration),
                     '--hci-capture', src_capture] + bluecaster_args
                    + (['--max-packet', str(args.max_packet)] if args.max_packet else []),
                    stdout=log, stderr=subprocess.STDOUT, env=env, timeout=args.duration + 90).returncode
            except subprocess.TimeoutExpired:
                rc = 'timeout'
        if rc != 0 and codec not in EXPECT_ERROR:
            failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
        if killer and not killer.killed:
            failures.append('the capture agent was not killed (not found in bluecaster.log)')
        if hand_agent:
            # Not killed: it outlives the core and waits for the next one
            time.sleep(1)
            if codec not in AGENT_KILL and hand_agent.poll() is not None:
                failures.append(f'the agent started first exited with the core ({hand_agent.returncode})')
    finally:
        if hand_agent:
            hand_agent.kill()
            hand_agent.wait(10)
            agent_log.close()
        sink.terminate()
        sink.wait(10)
    if codec in EXPECT_ERROR:
        log_text = read_text(os.path.join(out, 'bluecaster.log'))
        if rc == 0 or rc == 'timeout':
            failures.append(f'BlueCaster exited with {rc}, expected an error exit')
        if not re.search(EXPECT_ERROR[codec], log_text):
            failures.append(f'no line matching "{EXPECT_ERROR[codec]}" in bluecaster.log')
        if '[Streaming]' in log_text:
            failures.append('BlueCaster streamed although the codec should have stopped it')
        return failures, f'exit {rc}'
    for name, text, present in LOG_CHECKS.get(codec, []):
        found = read_text(os.path.join(out, name)).count(text)
        if isinstance(present, bool) and (found > 0) != present:
            failures.append(f'{name}: "{text}" {"missing" if present else "found"}')
        elif not isinstance(present, bool) and found != present:
            failures.append(f'{name}: "{text}" {found} times, not {present}')
    if codec in CTKD_CASES:
        failures.extend(ctkd_check(out))

    reports = {}
    for side, capture, extra in (('sent', src_capture, []), ('received', sink_capture, ['--received'])):
        if not os.path.exists(capture):
            failures.append(f'{side}: no capture')
            continue
        text = subprocess.run([args.decoder, capture, '-o', os.path.join(out, side)] + extra,
                              capture_output=True, text=True, encoding='utf-8', errors='replace').stdout
        with open(os.path.join(out, f'{side}_report.txt'), 'w', encoding='utf-8') as f:
            f.write(text)
        r = reports[side] = parse_report(text)
        # One stream per --sessions
        streams = int(bluecaster_args[bluecaster_args.index('--sessions') + 1]) if '--sessions' in bluecaster_args else 1
        if r['streams'] != streams:
            failures.append(f'{side}: {r["streams"]} streams (expected {streams})')
        if not re.match(CONFIG_PATTERN[expected], r['config'] or ''):
            failures.append(f'{side}: configuration "{r["config"]}"')
        if not r['result_ok']:
            failures.extend(f'{side}: {p}' for p in r['problems'] or ['bluecaster_decode reported problems'])

    sent, received = reports.get('sent'), reports.get('received')
    detail = ''
    if sent and received:
        if (sent['packets'], sent['frames']) != (received['packets'], received['frames']):
            failures.append(f'sent {sent["packets"]} packets / {sent["frames"]} frames, '
                            f'received {received["packets"]} / {received["frames"]}')
        if (sent['audio_s'] or 0) < 0.9 * args.duration:
            failures.append(f'only {sent["audio_s"]} s of audio for --duration {args.duration}')
        detail = f'{sent["packets"]} packets, {sent["frames"]} frames, {sent["audio_s"]} s'
        if expected not in NO_DECODER:
            if not (sent['wav'] and received['wav']):
                failures.append('no decoded WAV')
            else:
                with open(sent['wav'], 'rb') as a, open(received['wav'], 'rb') as b:
                    if a.read() != b.read():
                        failures.append('decoded audio differs between sent and received side')
                ok, notes = tone_check(received['wav'], eq=EQ_POINTS.get(codec))
                detail += f' | {notes}'
                if not ok:
                    failures.append(f'test tone not found: {notes}')
    return failures, detail


def parse_le_report(text):
    """bluecaster_decode --le-audio report: as parse_report (configuration and WAV
    of the first stream), packets / frames / audio summed over all streams,
    plus the CIS lines"""
    r = parse_report(text)
    r['cis'] = re.findall(r'^  CIS 0x[0-9A-F]+ +: (.*)$', text, re.M)
    r['wavs'] = re.findall(r'WAV\s+: (.*)$', text, re.M)   # one per stream
    r['text'] = text
    r['packets'] = sum(int(x) for x in re.findall(r'Media packets : (\d+)', text))
    r['frames'] = sum(int(x) for x in re.findall(r'Frames\s+: (\d+)', text))
    r['audio_s'] = round(sum(float(x) for x in re.findall(r'Audio\s+: ([\d.]+) s', text)), 3)
    return r


def run_le_case(args, case, port):
    out = os.path.join(args.out, 'le-' + case.name)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    src_capture = os.path.join(out, 'bluecaster.pklg')
    sink_capture = os.path.join(out, 'sink.pklg')
    failures = []

    port = free_port(port)
    sink = start_sink(port, sink_capture, os.path.join(out, 'sink.log'), 'emu_le_audio_sink.py',
                      ['--layout', case.layout] + case.sink_args)
    addresses = sink.ready_line.split('sinks=')[1].split(',')
    try:
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'))  # isolated bonds
        if any(a in case.bluecaster_args for a in ('--session-agent', '--capture-agent')):
            env['BLUECASTER_INSTANCE'] = f'emu-le-{case.name}-{port}'  # its own agent pipe
        cmd = [args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}', '--le-audio', '-d', addresses[0]]
        for a in ([] if case.first_address_only else addresses[1:]):
            cmd += ['--le-device', a]
        duration = case.duration or args.duration
        cmd += ['--test-tone', '--duration', str(duration), '--hci-capture', src_capture] + case.bluecaster_args
        cmd += le_audio_tests.mic.bluecaster_args(case, out)   # --headset-mic --mic-output <out>/mic.wav
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8', errors='replace') as log:
            try:
                rc = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, env=env,
                                    timeout=duration + 120).returncode
            except subprocess.TimeoutExpired:
                rc = 'timeout'
        if case.expect_error is None and rc != 0:
            failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
    finally:
        sink.terminate()
        sink.wait(10)

    if case.expect_error is not None:
        # BlueCaster must refuse with a clear error, not stream
        log_text = read_text(os.path.join(out, 'bluecaster.log'))
        if rc == 0 or rc == 'timeout':
            failures.append(f'BlueCaster exited with {rc}, expected an error exit')
        if not re.search(case.expect_error, log_text):
            failures.append(f'no error matching "{case.expect_error}" in bluecaster.log')
        if 'Streaming LE Audio:' in log_text:
            failures.append('BlueCaster streamed although it should have refused')
        return failures, f'exit {rc}'

    sink_captures =[sink_capture] + [os.path.join(out, f'sink-{i + 1}.pklg') for i in range(1, len(addresses))]
    reports = {}
    for side, captures, extra in (('sent', [src_capture], []), ('received', sink_captures, ['--received'])):
        missing = [c for c in captures if not os.path.exists(c)]
        if missing:
            failures.append(f'{side}: no capture {missing}')
            continue
        text = subprocess.run([args.decoder, '--le-audio'] + captures + ['-o', os.path.join(out, side)] + extra,
                              capture_output=True, text=True, encoding='utf-8', errors='replace').stdout
        with open(os.path.join(out, f'{side}_report.txt'), 'w', encoding='utf-8') as f:
            f.write(text)
        r = reports[side] = parse_le_report(text)
        if r['streams'] != case.streams:
            failures.append(f'{side}: {r["streams"]} streams (expected {case.streams})')
        if not re.match(case.config, r['config'] or ''):
            failures.append(f'{side}: configuration "{r["config"]}"')
        if not r['result_ok']:
            failures.extend(f'{side}: {p}' for p in r['problems'] or ['bluecaster_decode reported problems'])

    sent, received = reports.get('sent'), reports.get('received')
    bluecaster_log = read_text(os.path.join(out, 'bluecaster.log'))
    detail = ''
    if sent and received:
        missing = (sent['packets'] or 0) - (received['packets'] or 0)
        if (not case.lossy and (sent['packets'], sent['frames']) != (received['packets'], received['frames'])) \
                or (case.lossy and not 0 <= missing <= 8 * case.streams):
            failures.append(f'sent {sent["packets"]} SDUs / {sent["frames"]} frames, '
                            f'received {received["packets"]} / {received["frames"]}')
        detail = f'{sent["config"]}: {sent["packets"]} SDUs, {sent["frames"]} LC3 frames, {sent["audio_s"]} s'
        if len(sent['wavs']) != len(received['wavs']) or not received['wavs']:
            failures.append(f'decoded WAVs: {len(sent["wavs"])} sent, {len(received["wavs"])} received')
        else:
            if not case.lossy:
                for a_path, b_path in zip(sent['wavs'], received['wavs']):
                    with open(a_path, 'rb') as a, open(b_path, 'rb') as b:
                        if a.read() != b.read():
                            failures.append('decoded audio differs between sent and received side')
            if case.tones:
                # Every stream (the one set up again after a link loss too)
                for index, path in enumerate(received['wavs']):
                    last = index == len(received['wavs']) - 1
                    ok, notes = tone_check_stream(path, case.tones,
                                                  MAX_END_GAP_S if last else MAX_END_GAP_S + case.play_on_s)
                    label = f'stream {index + 1}: ' if len(received['wavs']) > 1 else ''
                    detail += f' | {label}{notes}'
                    if not ok:
                        failures.append(f'{label}test tone: {notes}')

    # SDUs that reached the emulated controller too late for a real one
    # (le_audio/controller.py logs them, its own stalls not counted)
    late = re.findall(r'late SDU: CIS (0x[0-9A-F]+) seq \d+ reached the controller ([\d.]+) ms .*?(\d+) late SDU',
                      read_text(os.path.join(out, 'sink.log')))
    if late:
        per_cis = {}
        for h, ms, count in late:
            n, worst = per_cis.get(h, (0, 0.0))
            per_cis[h] = (max(n, int(count)), max(worst, float(ms)))
        for h, (n, worst) in sorted(per_cis.items()):
            failures.append(f'CIS {h}: {n} SDU(s) {LATE_SDU_FAILURE} '
                            f'(worst {worst} ms; the queue of a real controller would have run empty)')

    # BlueCaster's own counters, per stream set-up ("stopped sending", the
    # counters start anew with each): no underrun (silence inserted while
    # streaming), and the audio sent (not counting the silence while its
    # buffer fills) must cover the duration
    frame_s = (7.5 if sent and re.search(r' 7\.5 ms ', sent['config'] or '') else 10) / 1000
    stops = le_stream_stats(bluecaster_log, frame_s)
    if not stops:
        failures.append('no "LE Audio: stopped sending" statistics in bluecaster.log')
    else:
        sets, silence, underruns, at_end = (sum(s[i] for s in stops) for i in range(4))
        detail += f' | {silence} silence sets, {underruns} underruns ({at_end} as the tone stopped)'
        if underruns - at_end > case.max_underruns:
            failures.append(f'{underruns - at_end} underrun(s) (BlueCaster sent silence while streaming; '
                            f'at most {case.max_underruns} allowed)')
        if sent:
            audio_s = (sets - silence) * frame_s
            min_audio = case.min_audio if case.min_audio is not None else 0.6 if case.lossy else 0.9
            if audio_s < min_audio * duration:
                failures.append(f'only {audio_s:.2f} s of audio (without silence) for --duration {duration}')

    ctx = dict(out=out, bluecaster_log=bluecaster_log, sink_log=read_text(os.path.join(out, 'sink.log')),
               sent=sent, received=received, sink_addresses=addresses)
    failures.extend(le_audio_tests.check(case, ctx))
    if ctx.get('mic_detail'):
        detail += f" | {ctx['mic_detail']}"
    return failures, detail


def le_stream_stats(log, frame_s):
    """Per stream set-up of BlueCaster's log ("sending ISO SDUs" ... "stopped
    sending: N SDU sets ..., S silence, U underruns"): (N, S, U, E), E = the
    underruns of the last stream within END_UNDERRUN_S of its end (as its
    "no audio for SDU set K" warnings say; the test tone stops first)"""
    streams, warned = [], []
    for line in log.splitlines():
        m = re.search(r'LE Audio: no audio for SDU set (\d+)', line)
        if m:
            warned.append(int(m.group(1)))
        m = re.search(r'LE Audio: stopped sending: (\d+) SDU sets .*?(\d+) silence, (\d+) underruns', line)
        if m:
            streams.append([int(m.group(1)), int(m.group(2)), int(m.group(3)), 0, warned])
            warned = []
    if streams:
        last = streams[-1]
        last[3] = sum(1 for k in last[4] if k >= last[0] - END_UNDERRUN_S / frame_s)
    return [tuple(s[:4]) for s in streams]


def read_text(path):
    try:
        with open(path, encoding='utf-8', errors='replace') as f:
            return f.read()
    except OSError:
        return ''


def main_le(args):
    all_cases = le_audio_tests.cases()
    names = [c.name for c in all_cases]
    wanted = args.le_audio or names
    unknown = [n for n in wanted if n not in names]
    if unknown:
        sys.exit(f'Unknown LE Audio case(s) {unknown}; known: {names}')
    results = []
    for index, case in enumerate(c for c in all_cases if c.name in wanted):
        print(f'--- LE Audio {case.name} ...', flush=True)
        failures, detail = run_le_case(args, case, args.base_port + 100 + index)
        if failures and all(LATE_SDU_FAILURE in f for f in failures):
            # Late SDUs only: a shared machine (a CI runner) can hold up
            # BlueCaster's sender thread a few ms now and then. Run the case
            # once more (the first run kept as le-<case>-try1); late SDUs
            # again fail it
            print(f'    late SDUs only, once more  {detail}')
            for failure in failures:
                print(f'      - {failure}')
            out = os.path.join(args.out, 'le-' + case.name)
            shutil.rmtree(out + '-try1', ignore_errors=True)
            os.rename(out, out + '-try1')
            failures, detail = run_le_case(args, case, args.base_port + 100 + index)
        results.append((case.name, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')
    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} LE Audio cases passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


def run_auracast_case(args, case, port):
    out = os.path.join(args.out, 'auracast-' + case.name)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    failures = []
    port = free_port(port)
    follow = [b.suffix for b in case.broadcasts if b.suffix]
    sink = start_sink(port, os.path.join(out, 'sink.pklg'), os.path.join(out, 'sink.log'), 'emu_le_audio_sink.py',
                      ['--layout', 'auracast', '--log-level', 'INFO']
                      + (['--follow'] + follow if follow else [])
                      + (['--broadcast-code', case.code] if case.code else []))
    duration = max(args.duration, 5)   # the SDU compared across the BIS is 3 s in
    try:
        # Its own settings folder: the mixer's defaults, and LE Audio on in
        # developer mode (a broadcast goes through the service, which asks)
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'))
        os.makedirs(env['BLUECASTER_CONFIG_DIR'])
        with open(os.path.join(env['BLUECASTER_CONFIG_DIR'], 'settings.json'), 'w', encoding='utf-8') as f:
            json.dump({'debug_mode': True, 'le_audio_experimental': True}, f)
        for knob in ('BLUECASTER_AURACAST_RTN', 'BLUECASTER_AURACAST_LATENCY_MS'):
            env.pop(knob, None)
        cmd = [args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}', '--auracast', le_audio_tests.auracast.NAME,
               '--test-tone', '--duration', str(duration),
               '--hci-capture', os.path.join(out, 'bluecaster.pklg')] + case.bluecaster_args
        if case.code:
            cmd += ['--auracast-code', case.code]
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8', errors='replace') as log:
            try:
                rc = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, env=env,
                                    timeout=duration + 120).returncode
            except subprocess.TimeoutExpired:
                rc = 'timeout'
        if rc != 0:
            failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
        time.sleep(0.5)   # the sinks log the end of their BIG
    finally:
        sink.terminate()
        sink.wait(10)
    more, detail = le_audio_tests.auracast.check(case, read_text(os.path.join(out, 'sink.log')),
                                                 read_text(os.path.join(out, 'bluecaster.log')), duration)
    return failures + more, detail


def main_auracast(args):
    names = [c.name for c in le_audio_tests.auracast.CASES]
    wanted = args.auracast or names
    unknown = [n for n in wanted if n not in names]
    if unknown:
        sys.exit(f'Unknown Auracast case(s) {unknown}; known: {names}')
    results = []
    for index, case in enumerate(c for c in le_audio_tests.auracast.CASES if c.name in wanted):
        print(f'--- Auracast {case.name} ...', flush=True)
        failures, detail = run_auracast_case(args, case, args.base_port + 200 + index)
        results.append((case.name, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')
    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} Auracast cases passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


# Device scan cases (--scan): name -> (emu_sink.py arguments, where the name
# comes from, how soon it must be known after the inquiry started). The sink
# first answers 1 s into an inquiry (emu_sink.py INQUIRY_FIRST_ANSWER).
SCAN_CASES = {
    'eir': (['--dual-mode'], 'inquiry result', 3.0),                      # in its extended inquiry response
    'name-request': (['--dual-mode', '--no-eir-name'], 'remote name', 15.0),  # asked after the first inquiry (10.24 s)
    # Earbuds advertising LE Audio too (issue #86), LE Audio on: both entries,
    # the LE one found by the LE scan beside the inquiry, not after it
    'le-audio': (['--dual-mode', '--le-audio-adv'], 'inquiry result', 3.0),
}
# Scan cases with LE Audio on: BlueCaster's arguments
SCAN_LE_CASES = {'le-audio': ['--le-audio', '--mcp-dry-run']}
SCAN_LE_FOUND_MAX_S = 3.0
SCAN_SECONDS = 10
SCAN_LISTED_MAX_S = 3.0
SINK_NAME = 'BlueCaster Test Sink'
# Link cases: sink arguments, BlueCaster arguments, (log, pattern, found or not).
# Disconnect after the 10 s in which BTstack ends an idle link by itself; the
# headphones turned back on open A2DP themselves, or leave it to the PC.
LINK_CASES = {
    'disconnect': ([], ['--duration', '12', '--sessions', '2'], [
        ('bluecaster.log', r'Disconnecting \(session 1.*dropping the ACL link.*HCI disconnection.*Connecting again', True),
    ]),
    'device-link': (['--turn-off-after', '4'], ['--duration', '16', '--wait-off'], [
        ('bluecaster.log', r'the headphones connected again.*reconnected on attempt 1', True),
        ('sink.log', r'turned on', True),
    ]),
    'device-a2dp': (['--turn-off-after', '4', '--opens-a2dp'], ['--duration', '16', '--wait-off'], [
        ('bluecaster.log', r'connected A2DP itself.*reconnected on attempt 1', True),
        ('bluecaster.log', r'closing a leftover AVDTP', False),
        ('sink.log', r'A2DP opened', True),
        ('sink.log', r'A2DP closed.*turned off|turned on.*A2DP closed.*A2DP opened', False),
    ]),
    # ...and set up the stream itself, a vendor codec (LHDC V5 headphones)
    'device-stream': (['--turn-off-after', '4', '--opens-a2dp', '--opens-stream'],
                      ['-c', 'ldac', '--duration', '16', '--wait-off'], [
        ('bluecaster.log', r'opened the stream itself, starting LDAC on it.*reconnected on attempt 1', True),
        ('bluecaster.log', r'closing a leftover AVDTP', False),
        ('sink.log', r'stream opened \(ldac\)', True),
        ('sink.log', r'A2DP closed.*stream opened', False),
    ]),
    # The link details: the remote chip and the readings the virtual
    # controller answers; the failed contact counter it refuses is left
    # out without holding up the other commands or the stream
    'details': ([], ['--duration', '12'], [
        ('bluecaster.log', r'remote controller of .*company 0x0094 \(Airoha\), LMP version 12', True),
        ('bluecaster.log', r'remote features of ', True),
        ('bluecaster.log', r'supervision timeout Some\(32000\) slots', True),
        ('bluecaster.log', r'does not support command 0x1401', True),
        ('bluecaster.log', r'Bluetooth stack stalled', False),
    ]),
    # A controller that never answers Read Remote Version: the command
    # counts as unsupported after its timeout and the readings behind it
    # (supervision timeout, then RSSI and link quality) go on
    'details-unanswered': (['--silent-remote-version'], ['--duration', '12'], [
        ('bluecaster.log', r'no answer to command 0x041D in \d+ ms', True),
        ('bluecaster.log', r'no answer to command 0x041D.*supervision timeout Some\(32000\) slots', True),
        ('bluecaster.log', r'no answer to command 0x041D.*radio: link quality 230', True),
        ('bluecaster.log', r'remote controller of ', False),
        ('bluecaster.log', r'Bluetooth stack stalled', False),
    ]),
    # A setting changed just after the link came up: the next connection
    # starts while that link goes down
    'restart-on-link': (['--disconnect-delay', '0.5'], ['--duration', '3', '--restart-on-link'], [
        ('bluecaster.log', r'Starting again.*HCI disconnection.*state Streaming', True),
        ('bluecaster.log', r'Bluetooth stack stalled', False),
    ]),
    # Headphones with Delay Reporting: their playout delay reaches the log
    'delay-report': (['--delay-report', '187.5'], ['--duration', '3'], [
        ('bluecaster.log', r'supports delay reporting.*device delay report: 187\.5 ms', True),
        ('sink.log', r'delay report sent \(187\.5 ms\)', True),
    ]),
    # Headphone buttons: each pass-through answer carries the command's state
    # flag, so a release is not answered as a press (issue #420); each press
    # and release reaches the IPC clients (--ipc, buttons_client)
    'avrcp-keys': (['--avrcp-keys', '3'], ['--duration', '8', '--ipc'], [
        ('sink.log', r'avrcp keys: 6 of 6 answers carry the state flag sent', True),
        ('bluecaster.log', r'AVRCP operation 0x49 released', True),
        ('bluecaster.log', r'Headphone button: FastForward released', True),
    ]),
    # PLAY, a button with a media key, through the session agent: each press
    # reaches the agent as a key, a dry run over --hci-tcp (nothing pressed
    # on this PC), and the core sends no key itself
    'avrcp-keys-agent': (['--avrcp-keys', '2', '--avrcp-key', 'PLAY'], ['--duration', '8', '--session-agent'], [
        ('sink.log', r'avrcp keys: 4 of 4 answers carry the state flag sent', True),
        ('bluecaster.log', r'session agent: connected \(process', True),
        ('bluecaster.log', r'AVRCP button PlayPause -> media key.*AVRCP button PlayPause -> media key', True),
        ('bluecaster.log', r'\[agent\] media key PlayPause: dry run, not sent'
                           r'.*\[agent\] media key PlayPause: dry run, not sent', True),
        ('bluecaster.log', r'(?<!\[agent\] )media key PlayPause: dry run', False),
        ('bluecaster.log', r'no session agent connected', False),
    ]),
    # A headset that answers OPEN with a stray REJECT of DISCOVER under the
    # OPEN's label first: BTstack waits on for the OPEN accept
    'stray-reject': (['--stray-reject'], ['--duration', '3'], [
        ('sink.log', r'stray reject: REJECT DISCOVER', True),
        ('bluecaster.log', r'ignoring a response to signal 1 while waiting for 6', True),
        ('bluecaster.log', r'streaming started', True),
        ('bluecaster.log', r'stream configuration timed out', False),
    ]),
    # The IPC's RFCOMM passthrough (--ipc, rfcomm_client): an echo service
    # opened by its UUID while streaming, the hands-free service refused,
    # a channel closed by the client and one by the stream's end. One log
    # line per open, with the client and the service and no address.
    'rfcomm': (['--echo', '--codecs', 'sbc'], ['--duration', '12', '--ipc'], [
        ('sink.log', r'echo: channel open.*echo: 16 bytes.*echo: channel closed', True),
        ('bluecaster.log', r'streaming started.*'
                           r'IPC: emu-rfcomm/1 #\d+ rfcomm.open of 5D6A7B8C-0E1F-4A2B-9C3D-4E5F60718293: opened #', True),
        ('bluecaster.log', r'IPC: emu-rfcomm/1 #\d+ rfcomm.open of 0000111E-0000-1000-8000-00805F9B34FB: '
                           r'refused in \d+ ms: rfcomm.service_reserved', True),
        ('bluecaster.log', r'rfcomm.open of [^\n]*' + SINK_ADDRESS, False),
    ]),
}
# Dual-mode pairing cases (issue #86): sink options, BlueCaster runs ('le' or
# 'classic'), whether the last LE pairing leaves a link key, and patterns
# counted in bluecaster-<n>-<run>.log (n from 1) / sink.log
LE_PAIRING_SINK = ['--codecs', 'sbc', '--dual-mode', '--le-audio-adv', '--check-link-key', '--log-level', 'INFO']
LE_STORED = 'LE pairing: BR/EDR link key derived and stored'
CLASSIC_PAIRED = f'new link key from pairing with {SINK_ADDRESS}'
SINK_AUTHENTICATED = 'link key: authenticated with the key the sink has'
LE_PAIRING_CASES = {
    'link-key': (['--le-link-key', 'on'], ['le', 'classic'], True, [
        ('bluecaster-1-le.log', LE_STORED, 1),
        ('bluecaster-1-le.log', 'h6 key', True),
        ('bluecaster-2-classic.log', CLASSIC_PAIRED, 0),
        ('bluecaster-2-classic.log', 'streaming started', True),
        ('sink.log', SINK_AUTHENTICATED, 1),
        ('sink.log', 'authentication failed', 0),
    ]),
    'link-key-ct2': (['--le-link-key', 'on', '--le-ct2'], ['le', 'classic'], True, [
        ('bluecaster-1-le.log', LE_STORED, 1),
        ('bluecaster-1-le.log', 'h7 key', True),
        ('bluecaster-2-classic.log', CLASSIC_PAIRED, 0),
        ('bluecaster-2-classic.log', 'streaming started', True),
        ('sink.log', SINK_AUTHENTICATED, 1),
        ('sink.log', 'authentication failed', 0),
    ]),
    'no-link-key': (['--le-link-key', 'off'], ['le', 'classic'], False, [
        ('bluecaster-1-le.log', 'LE pairing: BR/EDR link key not derived (the device does not distribute one)', 1),
        ('bluecaster-1-le.log', 'link keys: stored', 0),
        ('bluecaster-2-classic.log', CLASSIC_PAIRED, 1),
        ('bluecaster-2-classic.log', 'streaming started', True),
        ('sink.log', 'LE pairing: link key -', 1),
        ('sink.log', 'authentication failed', 0),
    ]),
    # An older unauthenticated key ('seed': written as Windows' keys are
    # taken over, type 4) is replaced by the derived one
    'replaces-classic': (['--le-link-key', 'on'], ['seed', 'le', 'classic'], True, [
        ('bluecaster-2-le.log', 'LE pairing: BR/EDR link key derived and stored in place of the older one', 1),
        ('bluecaster-3-classic.log', CLASSIC_PAIRED, 0),
        ('bluecaster-3-classic.log', 'streaming started', True),
        ('sink.log', SINK_AUTHENTICATED, 1),
        ('sink.log', 'authentication failed', 0),
    ]),
    # An authenticated key (the emulated classic pairing always makes one)
    # is not replaced by an unauthenticated derived one (Just Works over
    # LE) but dropped: the classic side pairs again. Without SMP over BR/EDR
    # the classic pairing gives no LE keys, so the LE run pairs anew.
    'drops-authenticated': (['--le-link-key', 'on', '--no-smp-br-edr'], ['classic', 'le', 'classic'], None, [
        ('bluecaster-1-classic.log', CLASSIC_PAIRED, 1),
        ('bluecaster-2-le.log', 'the older authenticated one dropped, the classic side pairs again', 1),
        ('bluecaster-3-classic.log', CLASSIC_PAIRED, 1),
        ('bluecaster-3-classic.log', 'streaming started', True),
        ('sink.log', 'authentication failed', 0),
    ]),
}
# A link key taken over from Windows (pairing.rs WINDOWS_KEY_TYPE)
SEEDED_KEY = ('00112233445566778899aabbccddeeff', 4)


def link_keys_of(config):
    """BlueCaster's link_keys.txt as {address: (key hex, type)}"""
    keys = {}
    for line in read_secret(os.path.join(config, 'pairing', 'link_keys.txt')).decode().splitlines():
        parts = line.split()
        if len(parts) == 3 and not line.startswith('#'):
            keys[parts[0].replace('-', ':')] = (parts[1].lower(), int(parts[2]))
    return keys


def run_le_pairing_case(args, case, port):
    """One sink, several bluecaster-core runs: the link key an LE pairing
    leaves is the one the sink derived, and a classic link authenticates
    with it."""
    sink_args, runs, derived, checks = LE_PAIRING_CASES[case]
    out = os.path.join(args.out, 'lepair-' + case)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    config = os.path.join(out, 'config')
    failures, keys_after = [], []
    port = free_port(port)
    sink = start_sink(port, os.path.join(out, 'sink.pklg'), os.path.join(out, 'sink.log'),
                      extra=LE_PAIRING_SINK + sink_args)
    try:
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=config, BLUECASTER_LOG='debug')
        for n, kind in enumerate(runs, 1):
            if kind == 'seed':
                # Plain text, as older builds wrote it (read and encrypted)
                os.makedirs(os.path.join(config, 'pairing'), exist_ok=True)
                with open(os.path.join(config, 'pairing', 'link_keys.txt'), 'w', encoding='utf-8') as f:
                    f.write(f'{SINK_ADDRESS.replace(":", "-")} {SEEDED_KEY[0]} {SEEDED_KEY[1]}\n')
                keys_after.append(link_keys_of(config))
                continue
            cmd = [args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}', '-d', SINK_ADDRESS, '--test-tone',
                   '--duration', '3', '--hci-capture', os.path.join(out, f'bluecaster-{n}-{kind}.pklg')]
            cmd += ['--le-audio'] if kind == 'le' else ['-c', 'sbc']
            with open(os.path.join(out, f'bluecaster-{n}-{kind}.log'), 'w', encoding='utf-8', errors='replace') as log:
                try:
                    rc = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, env=env, timeout=120).returncode
                except subprocess.TimeoutExpired:
                    rc = 'timeout'
            # An LE run ends with an error (no LE Audio services), after pairing
            if kind == 'classic' and rc != 0:
                failures.append(f'run {n} ({kind}): BlueCaster exited with {rc}')
            if kind == 'le' and rc == 'timeout':
                failures.append(f'run {n} ({kind}): timeout')
            keys_after.append(link_keys_of(config))
    finally:
        sink.terminate()
        sink.wait(10)
    for name, text, present in checks:
        found = read_text(os.path.join(out, name)).count(text)
        if isinstance(present, bool) and (found > 0) != present:
            failures.append(f'{name}: "{text}" {"missing" if present else "found"}')
        elif not isinstance(present, bool) and found != present:
            failures.append(f'{name}: "{text}" {found} times, not {present}')
    # The key after the (last) LE pairing: the sink's derived one; none (the
    # one from before, unchanged); derived None: dropped
    le_run = max(i for i, k in enumerate(runs) if k == 'le')
    sink_keys = re.findall(r'LE pairing: link key ([0-9a-f]{32}|-)', read_text(os.path.join(out, 'sink.log')))
    kept = keys_after[le_run].get(SINK_ADDRESS)
    before = keys_after[le_run - 1].get(SINK_ADDRESS) if le_run > 0 else None
    if not sink_keys:
        failures.append('sink.log: no LE pairing')
    elif derived is None:
        if before is None or kept is not None:
            failures.append(f'the older key {before} not dropped: {kept}')
    elif derived:
        if kept != (sink_keys[-1], 7):
            failures.append(f'after the LE pairing BlueCaster keeps {kept}, the sink derived {sink_keys[-1]} (type 7)')
        if before is not None and kept is not None and before[0] == kept[0]:
            failures.append('the classic key was not replaced')
    elif sink_keys[-1] != '-' or kept != before:
        failures.append(f'a link key without LinkKey: sink {sink_keys[-1]}, BlueCaster {kept} (before {before})')
    others = {a for keys in keys_after for a in keys} - {SINK_ADDRESS}
    if others:
        failures.append(f'link keys of {len(others)} other address(es)')
    return failures, f'link key after the LE pairing: {"stored" if kept else "none"}'


def main_le_pairing(args):
    wanted = args.le_pairing or list(LE_PAIRING_CASES)
    unknown = [n for n in wanted if n not in LE_PAIRING_CASES]
    if unknown:
        sys.exit(f'Unknown LE pairing case(s) {unknown}; known: {list(LE_PAIRING_CASES)}')
    results = []
    for index, case in enumerate(wanted):
        print(f'--- le-pairing {case} ...', flush=True)
        failures, detail = run_le_pairing_case(args, case, args.base_port + 500 + index)
        results.append((case, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')
    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} LE pairing cases passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


LOG_TIME =re.compile(r'(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d{3})')


def log_time(log, pattern):
    """Seconds since the epoch of the first log line matching pattern, or None."""
    for line in log.splitlines():
        if re.search(pattern, line) and (m := LOG_TIME.search(line)):
            return time.mktime(time.strptime(m.group(1)[:19], '%Y-%m-%d %H:%M:%S')) + float(m.group(1)[19:])
    return None


def run_scan_case(args, case, port):
    """bluecaster-core --cli --scan against the sink: listed once, with its name
    and RSSI, soon after the inquiry started."""
    sink_args, name_from, name_max_s = SCAN_CASES[case]
    out = os.path.join(args.out, 'scan-' + case)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    failures = []
    port = free_port(port)
    sink = start_sink(port, os.path.join(out, 'sink.pklg'), os.path.join(out, 'sink.log'), extra=sink_args)
    try:
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'), BLUECASTER_LOG='debug')
        started = time.time()
        try:
            r = subprocess.run(
                [args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}', '--scan', str(SCAN_SECONDS),
                 '--hci-capture', os.path.join(out, 'bluecaster.pklg')] + SCAN_LE_CASES.get(case, []),
                capture_output=True, text=True, encoding='utf-8', errors='replace', env=env,
                timeout=SCAN_SECONDS + 60)
            rc, text = r.returncode, r.stdout + r.stderr
        except subprocess.TimeoutExpired as e:
            partial = e.stdout or ''
            rc, text = 'timeout', partial.decode('utf-8', 'replace') if isinstance(partial, bytes) else partial
        took = time.time() - started
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8') as f:
            f.write(text)
    finally:
        sink.terminate()
        sink.wait(10)
    if rc != 0:
        failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
    lines = [line for line in text.splitlines() if line.startswith(SINK_ADDRESS)]
    le_lines = [line for line in lines if '(LE Audio)' in line]
    lines = [line for line in lines if line not in le_lines]
    if case in SCAN_LE_CASES:
        if len(le_lines) != 1 or SINK_NAME not in le_lines[0]:
            failures.append(f'the LE Audio entry listed {len(le_lines)} times, or without its name: {le_lines}')
    elif le_lines:
        failures.append(f'an LE Audio entry with LE Audio off: {le_lines}')
    if len(lines) != 1:
        failures.append(f'the sink listed {len(lines)} times (expected once)')
    elif SINK_NAME not in lines[0]:
        failures.append(f'listed without its name: {lines[0].strip()}')
    elif not re.search(r'RSSI -\d+', lines[0]) or '(audio)' not in lines[0]:
        failures.append(f'no RSSI or not an audio device: {lines[0].strip()}')
    t0 = log_time(text, r'starting inquiry')
    listed = log_time(text, r'scan: \d+ device\(s\) listed')
    if name_from == 'inquiry result':
        named = log_time(text, rf'inquiry result: {SINK_ADDRESS} .*name=“{SINK_NAME}”')
    else:
        named = log_time(text, rf'remote name of {SINK_ADDRESS}: status 0x00 name=“{SINK_NAME}”')
    since = lambda t: None if t is None or t0 is None else round(t - t0, 2)
    if since(listed) is None or since(listed) > SCAN_LISTED_MAX_S:
        failures.append(f'listed {since(listed)} s after the inquiry started (at most {SCAN_LISTED_MAX_S} s)')
    if since(named) is None or since(named) > name_max_s:
        failures.append(f'name from the {name_from} {since(named)} s after the inquiry started (at most {name_max_s} s)')
    detail = f'listed after {since(listed)} s, named after {since(named)} s'
    if case in SCAN_LE_CASES:
        le_found = log_time(text, rf'LE Audio: found {SINK_NAME} \({SINK_ADDRESS}')
        if since(le_found) is None or since(le_found) > SCAN_LE_FOUND_MAX_S:
            failures.append(f'LE Audio entry found {since(le_found)} s after the inquiry started '
                            f'(at most {SCAN_LE_FOUND_MAX_S} s)')
        detail += f', LE Audio found after {since(le_found)} s'
    return failures, f'{detail}, scan took {took:.1f} s'


def main_scan(args):
    wanted = args.scan or list(SCAN_CASES)
    unknown = [n for n in wanted if n not in SCAN_CASES]
    if unknown:
        sys.exit(f'Unknown scan case(s) {unknown}; known: {list(SCAN_CASES)}')
    results = []
    for index, case in enumerate(wanted):
        print(f'--- scan {case} ...', flush=True)
        failures, detail = run_scan_case(args, case, args.base_port + 200 + index)
        results.append((case, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')
    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} scan cases passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


ECHO_UUID = '5D6A7B8C-0E1F-4A2B-9C3D-4E5F60718293'   # emu_sink.py EchoService.UUID


class IpcClient:
    """A client of the core's named pipe (docs/integration.md): frames of a
    u32 length and JSON. Reads only what has arrived (PeekNamedPipe), so a
    write never waits behind a blocking read on the same handle."""

    def __init__(self, instance, name, log):
        import ctypes
        import msvcrt
        sid = subprocess.run(['whoami', '/user', '/fo', 'csv', '/nh'], capture_output=True,
                             text=True).stdout.strip().split(',')[1].strip('"')
        path = rf'\\.\pipe\bluecaster-{sid}-{instance}'
        deadline = time.time() + 30
        while True:
            try:
                self.pipe = open(path, 'r+b', buffering=0)
                break
            except OSError:
                if time.time() > deadline:
                    raise RuntimeError('the IPC pipe never appeared') from None
                time.sleep(0.2)
        self.handle = msvcrt.get_osfhandle(self.pipe.fileno())
        self.peek = ctypes.windll.kernel32.PeekNamedPipe
        self.peek.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p,
                              ctypes.POINTER(ctypes.c_ulong), ctypes.c_void_p]
        self.available = ctypes.c_ulong()
        self.log, self.next_id, self.events = log, 0, []
        self.send({'type': 'hello', 'protocol': 1, 'client': name})
        self.welcome = self.recv(5)
        self.snapshot = self.recv(5)

    def close(self):
        self.pipe.close()

    def send(self, msg):
        data = json.dumps(msg).encode()
        self.pipe.write(struct.pack('<I', len(data)) + data)

    def _read(self, n):
        buf = b''
        while len(buf) < n:
            chunk = self.pipe.read(n - len(buf))
            if not chunk:
                raise EOFError('the core closed the pipe')
            buf += chunk
        return buf

    def recv(self, timeout):
        """The next message, or None after timeout seconds"""
        import ctypes
        deadline = time.time() + timeout
        while True:
            if not self.peek(self.handle, None, 0, None, ctypes.byref(self.available), None):
                raise EOFError('the core closed the pipe')
            if self.available.value >= 4:
                msg = json.loads(self._read(struct.unpack('<I', self._read(4))[0]))
                if msg.get('type') == 'event' and msg['event']['kind'] in ('rfcomm_data', 'rfcomm_closed'):
                    self.log.write(f'{time.strftime("%H:%M:%S")} {json.dumps(msg["event"])}\n')
                return msg
            if time.time() > deadline:
                return None
            time.sleep(0.02)

    def request(self, method, params=None, timeout=30):
        """(result, error) of a request; the events meanwhile are kept"""
        self.next_id += 1
        msg = {'type': 'request', 'id': self.next_id, 'method': method}
        if params is not None:
            msg['params'] = params
        self.send(msg)
        deadline = time.time() + timeout
        while time.time() < deadline:
            m = self.recv(deadline - time.time())
            if m is None:
                break
            if m.get('type') == 'response' and m.get('id') == self.next_id:
                self.log.write(f'{time.strftime("%H:%M:%S")} {method} {json.dumps(params)} -> '
                               f'{json.dumps(m.get("result"))} {json.dumps(m.get("error"))}\n')
                return m.get('result'), m.get('error')
            if m.get('type') == 'event':
                self.events.append(m['event'])
        raise TimeoutError(f'{method}: no answer in {timeout} s')

    def event(self, kind, timeout, match=lambda value: True):
        """The first event of this kind (also one kept), or None"""
        for i, e in enumerate(self.events):
            if e['kind'] == kind and match(e['value']):
                return self.events.pop(i)['value']
        deadline = time.time() + timeout
        while time.time() < deadline:
            m = self.recv(deadline - time.time())
            if m is None:
                break
            if m.get('type') == 'event':
                if m['event']['kind'] == kind and match(m['event']['value']):
                    return m['event']['value']
                self.events.append(m['event'])
        return None


def rfcomm_client(instance, log):
    """The 'rfcomm' link case's client: opens the sink's echo service once
    A2DP is up, gets back what it sends, is refused the hands-free service,
    closes the channel, then opens another one that must close when the
    stream stops"""
    import base64
    failures = []
    c = IpcClient(instance, 'emu-rfcomm/1', log)
    try:
        if 'rfcomm' not in c.welcome.get('features', []):
            return [f'IPC: no "rfcomm" in the features: {c.welcome}']
        # Opens fail with rfcomm.not_connected until the A2DP connection is
        # up; the stream starts right after it (the log check makes sure)
        time.sleep(1)
        deadline = time.time() + 40
        while True:
            opened, error = c.request('rfcomm.open', {'address': SINK_ADDRESS, 'uuid': ECHO_UUID})
            if not error or error['code'] != 'rfcomm.not_connected' or time.time() > deadline:
                break
            time.sleep(0.5)
        if error:
            return [f'rfcomm.open of the echo service: {error}']
        channel = opened['channel_id']
        sent = b'BlueCaster echo\n'
        _, error = c.request('rfcomm.send', {'channel_id': channel, 'data': base64.b64encode(sent).decode()})
        if error:
            failures.append(f'rfcomm.send: {error}')
        received = b''
        while len(received) < len(sent):
            data = c.event('rfcomm_data', 10, lambda v: v['channel_id'] == channel)
            if data is None:
                break
            received += base64.b64decode(data['data'])
        if received != sent:
            failures.append(f'echo: sent {sent!r}, received {received!r}')
        for uuid in ('111E', '0000111f-0000-1000-8000-00805f9b34fb'):
            opened, error = c.request('rfcomm.open', {'address': SINK_ADDRESS, 'uuid': uuid})
            if not error or error['code'] != 'rfcomm.service_reserved':
                failures.append(f'rfcomm.open of {uuid}: {opened} {error}, expected rfcomm.service_reserved')
        _, error = c.request('rfcomm.close', {'channel_id': channel})
        if error:
            failures.append(f'rfcomm.close: {error}')
        _, error = c.request('rfcomm.send', {'channel_id': channel, 'data': 'AA=='})
        if not error or error['code'] != 'rfcomm.no_channel':
            failures.append(f'rfcomm.send after the close: {error}, expected rfcomm.no_channel')
        # Another channel: it closes with the stream at --duration's end
        opened, error = c.request('rfcomm.open', {'address': SINK_ADDRESS, 'uuid': ECHO_UUID})
        if error:
            return failures + [f'second rfcomm.open: {error}']
        closed = c.event('rfcomm_closed', 30, lambda v: v['channel_id'] == opened['channel_id'])
        if closed is None:
            failures.append('no rfcomm_closed when the stream stopped')
        elif closed['reason'] not in ('disconnected', 'stopped'):
            failures.append(f'rfcomm_closed with reason {closed["reason"]} when the stream stopped')
        else:
            log.write(f'closed with the stream ({closed["reason"]})\n')
    except EOFError as e:
        failures.append(f'IPC: {e}')
    finally:
        c.close()
    return failures


def buttons_client(instance, log):
    """The 'avrcp-keys' link case's client: every press and release of the
    sink's FAST FORWARD arrives as a media_button event, in order; over the
    emulator's TCP transport no adapter is in use"""
    c = IpcClient(instance, 'emu-buttons/1', log)
    try:
        failures = []
        if 'media_button' not in c.welcome.get('features', []):
            failures.append(f'IPC: no "media_button" in the features: {c.welcome}')
        adapter = c.snapshot['state']['adapter']
        if 'active' not in adapter or adapter['active'] is not None:
            failures.append(f'IPC: adapter.active should be null over TCP: {adapter.get("active", "missing")}')
        got = []
        while len(got) < 6:
            value = c.event('media_button', 30)
            if value is None:
                break
            log.write(f'{time.strftime("%H:%M:%S")} media_button {json.dumps(value)}\n')
            got.append((value['button'], value['pressed'], value['le_audio']))
        wanted = [('fast_forward', pressed, False) for _ in range(3) for pressed in (True, False)]
        if got != wanted:
            failures.append(f'media_button events {got}, expected {wanted}')
        return failures
    except EOFError as e:
        return [f'IPC: {e}']
    finally:
        c.close()


# Link cases with an IPC client beside the run: case -> function(instance,
# log) -> failures, on a thread of its own from the start of bluecaster-core
LINK_CLIENTS = {'rfcomm': rfcomm_client, 'avrcp-keys': buttons_client}


def run_link_case(args, case, port):
    """bluecaster-core --cli with the sink ending or opening links: each pattern
    of the case is found, or not, in bluecaster.log / sink.log; with an IPC
    client (LINK_CLIENTS) beside it, its checks too."""
    sink_args, bluecaster_args, checks = LINK_CASES[case]
    out = os.path.join(args.out, 'link-' + case)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    failures = []
    port = free_port(port)
    sink = start_sink(port, os.path.join(out, 'sink.pklg'), os.path.join(out, 'sink.log'), extra=sink_args)
    client = LINK_CLIENTS.get(case)
    client_failures = []
    try:
        # A test instance of its own (--ipc serves its pipe): never the
        # installed core's, nor another run's
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'), BLUECASTER_LOG='debug',
                   BLUECASTER_INSTANCE=f'emu-{case}-{port}')
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8', errors='replace') as log:
            proc = subprocess.Popen(
                [args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}', '-d', SINK_ADDRESS]
                + ([] if '-c' in bluecaster_args else ['-c', 'sbc'])
                + ['--test-tone', '--hci-capture', os.path.join(out, 'bluecaster.pklg')] + bluecaster_args,
                stdout=log, stderr=subprocess.STDOUT, env=env)
            thread = None
            if client:
                def run_client():
                    with open(os.path.join(out, 'client.log'), 'w', encoding='utf-8') as client_log:
                        try:
                            client_failures.extend(client(env['BLUECASTER_INSTANCE'], client_log))
                        except Exception as e:  # noqa: BLE001 - reported as the case's failure
                            client_failures.append(f'IPC client: {e!r}')
                thread = threading.Thread(target=run_client, daemon=True)
                thread.start()
            try:
                rc = proc.wait(timeout=120)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
                rc = 'timeout'
            if thread:
                thread.join(30)
                if thread.is_alive():
                    client_failures.append('IPC client: still running after BlueCaster exited')
    finally:
        sink.terminate()
        sink.wait(10)
    if rc != 0:
        failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
    failures += client_failures
    for name, pattern, present in checks:
        if bool(re.search(pattern, read_text(os.path.join(out, name)), re.S)) != present:
            failures.append(f'{name}: "{pattern}" {"missing" if present else "found"}')
    return failures, ''


# HFP audio cases: the codec BlueCaster's log names, SCO packets per second
# each way (60-byte packets: CVSD with 16-bit samples 16 kB/s, transparent
# 8 kB/s)
VOICE_CASES = {
    'cvsd': ('CVSD', 16000 / 60),
    'msbc': ('mSBC', 8000 / 60),
    'lc3swb': ('LC3-SWB', 8000 / 60),
    'cvsd-roundtrip': ('CVSD', 16000 / 60),
    'cvsd-auto': ('CVSD', 16000 / 60),
    'lc3swb-msbc': ('mSBC', 8000 / 60),
    'lc3swb-cvsd': ('CVSD', 16000 / 60),
    'msbc-gain': ('mSBC', 8000 / 60),
    'msbc-agent': ('mSBC', 8000 / 60),
    'cvsd-agent-killed': ('CVSD', 16000 / 60),
}
# Cases with the session agent (--session-agent): the microphone decoded in
# the core, written to mic.wav by the agent. -killed: the agent killed this
# many seconds after it opened the microphone's output; the core opens it
# again through the next agent, whose mic.wav (written anew) holds the tone
VOICE_AGENT = {'msbc-agent', 'cvsd-agent-killed'}
VOICE_AGENT_KILL = {'cvsd-agent-killed': 1.5}
VOICE_AGENT_LINES = ('voice render: the microphone output runs in the session agent (',
                     '[agent] render: channel opened (1 open)')
VOICE_AGENT_KILLED_LINES = ('voice render: the session agent was lost, opening the microphone output again',
                            'voice render: the microphone output runs again in the session agent (1 reopen(s)',
                            'voice: the uplink capture was lost with the session agent, capturing again')
# Cases with the microphone's gain (--mic-gain, dB): mic.wav's tone that much louder
VOICE_MIC_GAIN = {'msbc-gain': 6}
# Cases that choose the call codec (--voice-codec): the hands-free unit
# offers more, the link takes the one chosen (hfp_audio.py's name for it)
VOICE_CHOSEN = {'lc3swb-msbc': ('msbc', 'msbc'), 'lc3swb-cvsd': ('cvsd', 'cvsd')}
# The hands-free unit's tone (hfp_audio.py TX_TONE_HZ, voice_frames.py) as
# BlueCaster's mic.wav must hold it: frequency and amplitude
MIC_TONE = (1500.0, 8000 / 32768)
# cvsd-roundtrip: headset mode on / off at these seconds of the stream
ROUNDTRIP_TOGGLE = (3, 6)


def pklg_records(path):
    """(time in s, type, length) of each record of a PacketLogger capture"""
    records = []
    try:
        with open(path, 'rb') as f:
            data = f.read()
    except OSError:
        return records
    i = 0
    while i + 13 <= len(data):
        length, secs, usecs = struct.unpack_from('>III', data, i)
        records.append((secs + usecs / 1e6, data[i + 12], length - 9))
        i += 4 + length
    return records


def mic_tone_check(path, gain_db=0):
    """(ok, description) for the hands-free unit's tone in the middle half
    second of BlueCaster's mic.wav (gain_db: the microphone's gain)"""
    try:
        rate, channels = read_wav(path)
    except (OSError, EOFError, wave.Error) as e:
        return False, f'no mic.wav ({e})'
    xs = channels[0] if channels else []
    if len(channels) != 1 or len(xs) < rate:
        return False, f'expected >= 1 s of mono audio, got {len(channels)} ch, {len(xs) / max(rate, 1):.2f} s'
    start = len(xs) // 2 - rate // 4
    freq, amp = MIC_TONE
    ok, notes = tone_block(xs[start:start + rate // 2], rate, ((freq, amp * 10 ** (gain_db / 20)),))
    return ok, f'{rate} Hz {len(xs) / rate:.1f} s, {", ".join(notes)}'


def pklg_counts(path):
    """Records of a PacketLogger capture by type"""
    counts = {}
    try:
        with open(path, 'rb') as f:
            data = f.read()
    except OSError:
        return counts
    i = 0
    while i + 13 <= len(data):
        length, = struct.unpack_from('>I', data, i)
        counts[data[i + 12]] = counts.get(data[i + 12], 0) + 1
        i += 4 + length
    return counts


def start_voice_agent_killer(log_path, delay):
    """Kills the session agent the core's log names `delay` seconds after
    it opened the microphone's output (VOICE_AGENT_KILL); .killed tells
    whether it did"""
    def run():
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            text = read_text(log_path)
            started = AGENT_STARTED.findall(text)
            if started and '[agent] render: channel opened' in text:
                time.sleep(delay)
                try:
                    os.kill(int(started[-1]), signal.SIGTERM)  # TerminateProcess on Windows
                    thread.killed = True
                except OSError:
                    pass
                return
            time.sleep(0.05)
    thread = threading.Thread(target=run, daemon=True)
    thread.killed = False
    thread.start()
    return thread


def run_voice_case(args, case, port):
    """bluecaster-core --headset-mic against emu_sink.py --hfp <codec>"""
    codec, rate = VOICE_CASES[case]
    hf_case = case.split('-')[0]
    roundtrip = case.endswith('-roundtrip')
    out = os.path.join(args.out, 'voice-' + case)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    failures = []
    duration = max(args.duration, 10 if roundtrip or case in VOICE_AGENT_KILL else 6)
    port = free_port(port)
    src_capture, sink_capture = os.path.join(out, 'bluecaster.pklg'), os.path.join(out, 'sink.pklg')
    mic_wav = os.path.join(out, 'mic.wav')
    auto = case.endswith('-auto')
    headset = (['--headset-mic-toggle', ','.join(map(str, ROUNDTRIP_TOGGLE))] if roundtrip
               else ['--headset-mic', 'auto'] if auto else ['--headset-mic'])
    # The codec the link negotiates (hfp_audio.py's name), the one chosen
    chosen, negotiated = VOICE_CHOSEN.get(case, (None, hf_case))
    if chosen:
        headset += ['--voice-codec', chosen]
    gain = VOICE_MIC_GAIN.get(case, 0)
    if gain:
        headset += ['--mic-gain', str(gain)]
    agent = case in VOICE_AGENT
    if agent:
        headset += ['--session-agent']
    sink = start_sink(port, sink_capture, os.path.join(out, 'sink.log'), extra=['--hfp', hf_case])
    try:
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'), BLUECASTER_LOG='debug')
        if agent:
            env['BLUECASTER_INSTANCE'] = f'emu-voice-{case}-{port}'  # its own agent pipe
        killer = (start_voice_agent_killer(os.path.join(out, 'bluecaster.log'), VOICE_AGENT_KILL[case])
                  if case in VOICE_AGENT_KILL else None)
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8', errors='replace') as log:
            try:
                rc = subprocess.run(
                    [args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}', '-d', SINK_ADDRESS, '-c', 'sbc',
                     '--test-tone', '--duration', str(duration), '--hci-capture', src_capture]
                    + headset + ['--mic-output', mic_wav],
                    stdout=log, stderr=subprocess.STDOUT, env=env, timeout=duration + 90).returncode
            except subprocess.TimeoutExpired:
                rc = 'timeout'
    finally:
        sink.terminate()
        sink.wait(10)
    if rc != 0:
        failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
    bc_log, sink_log = read_text(os.path.join(out, 'bluecaster.log')), read_text(os.path.join(out, 'sink.log'))
    m = re.search(r'HFP: audio connection established, codec (\S+)', bc_log)
    if not m:
        failures.append('bluecaster.log: no audio connection')
    elif m.group(1) != codec:
        failures.append(f'audio connection with {m.group(1)}, not {codec}')
    if hf_case != 'cvsd' and f'HFP: codec negotiated {negotiated}' not in sink_log:
        failures.append(f'sink.log: codec {negotiated} not negotiated')
    if not re.search(r'^Voice: \S+ at \d+ Hz', bc_log, re.M):
        failures.append('bluecaster.log: the voice link never carried audio ("Voice: ..." missing)')
    if re.search(r'Traceback|HFP-HF protocol failed', sink_log):
        failures.append('sink.log: a Python error')
    # The battery level, sent before the AG's OK to AT+XAPL (hfp_audio.py)
    m = re.search(r'HFP: battery sent with AT\+XAPL: (.*)', sink_log)
    if not m or m.group(1).strip() != 'OK OK':
        failures.append(f'sink.log: AT+XAPL and AT+IPHONEACCEV answered {m.group(1).strip() if m else "never"}, not OK OK')
    if 'HFP: battery level 6 of 0-9 = 70%' not in bc_log:
        failures.append('bluecaster.log: no battery level 70 %')
    # Both captures: SCO out (0x08) and in (0x09), at about the air rate for
    # the time the link was up (at least a second of it)
    detail = []
    for side, capture in (('bluecaster', src_capture), ('sink', sink_capture)):
        c = pklg_counts(capture)
        sent, received = c.get(0x08, 0), c.get(0x09, 0)
        detail.append(f'{side} SCO out {sent} in {received}')
        if sent < rate or received < rate:
            failures.append(f'{side}.pklg: SCO out {sent}, in {received} (less than a second of audio)')
    ok, notes = mic_tone_check(mic_wav, gain)
    detail.append(f'mic.wav {notes}')
    if not ok:
        failures.append(f'mic.wav: the sink\'s 1.5 kHz tone not found ({notes})')
    rx = re.findall(r'HFP rx: (\d+) packets \d+ bytes codec (\S+) status (.*?) tone (.*?) \| tx: (\d+)', sink_log)
    if not rx:
        failures.append('sink.log: no HFP rx summary')
    else:
        packets, sink_codec, status, tone, tx = rx[-1]
        detail.append(f'sink rx {packets} tx {tx} status {status} tone {tone}')
        if int(packets) == 0:
            failures.append('the sink received no SCO packets')
        if sink_codec != negotiated:
            failures.append(f'sink codec {sink_codec}, not {negotiated}')
        if not tone.startswith('ok'):
            failures.append(f'sink: what BlueCaster sent is not right ({tone})')
    if 'dropped an incoming HCI packet' in bc_log:
        failures.append('bluecaster.log: incoming HCI packets dropped')
    if gain and f'microphone processing: gain {gain} dB' not in bc_log:
        failures.append(f"bluecaster.log: the microphone's gain of {gain} dB never applied")
    if auto and 'voice: auto: an application records' not in bc_log:
        failures.append('bluecaster.log: Auto never saw the WAV output as recorded from')
    if roundtrip:
        failures += roundtrip_checks(args, out, sink_capture, bc_log, detail)
    if agent:
        lines = VOICE_AGENT_LINES + (VOICE_AGENT_KILLED_LINES if killer else ())
        failures += [f'bluecaster.log: "{line}" missing' for line in lines if line not in bc_log]
        if killer and not killer.killed:
            failures.append('the session agent was not killed')
    elif 'voice render:' in bc_log:
        failures.append('bluecaster.log: the microphone went through a session agent without --session-agent')
    return failures, ' | '.join(detail)


def roundtrip_checks(args, out, sink_capture, bc_log, detail):
    """cvsd-roundtrip: A2DP back after the voice link, with the test tone"""
    failures = []
    if 'voice: A2DP resumed' not in bc_log:
        failures.append('bluecaster.log: A2DP did not resume ("voice: A2DP resumed" missing)')
    # The sink's capture: media packets (incoming ACL, longer than any
    # signalling) after its last SCO packet
    records = pklg_records(sink_capture)
    sco = [t for t, kind, _ in records if kind in (0x08, 0x09)]
    if not sco:
        return failures + ['sink.pklg: no SCO']
    media = [t for t, kind, length in records if kind == 0x03 and length > 300]
    before = [t for t in media if t < sco[0]]
    after = [t for t in media if t > sco[-1] + 0.2]
    span = after[-1] - after[0] if len(after) > 1 else 0
    detail.append(f'A2DP {len(before)} media packets before the voice link, {len(after)} in {span:.1f} s after')
    if not before:
        failures.append('sink.pklg: no A2DP media before the voice link')
    if span < 2.0:
        failures.append(f'sink.pklg: A2DP media only {span:.1f} s after the voice link closed')
    # The test tone in the last second the sink decoded
    text = subprocess.run([args.decoder, sink_capture, '-o', os.path.join(out, 'received'), '--received'],
                          capture_output=True, text=True, encoding='utf-8', errors='replace').stdout
    with open(os.path.join(out, 'received_report.txt'), 'w', encoding='utf-8') as f:
        f.write(text)
    wavs = re.findall(r'WAV\s+: (.*)$', text, re.M)
    if not wavs:
        return failures + ['received: no decoded WAV']
    rate, channels = read_wav(wavs[-1].strip())
    end = len(channels[0]) - int(rate * MAX_END_GAP_S) if channels else 0
    if len(channels) != 2 or end < rate:
        return failures + [f'received: {len(channels)} channel(s), {end / max(rate, 1):.1f} s']
    ok, notes = True, []
    for xs, tone in zip(channels, TONES):
        block_ok, block_notes = tone_block(xs[end - rate // 2:end], rate, (tone,))
        ok &= block_ok
        notes += block_notes
    detail.append(f'last A2DP second {", ".join(notes)}')
    if not ok:
        failures.append(f'the test tone is not in the last second the sink decoded: {", ".join(notes)}')
    return failures


def main_voice(args):
    wanted = args.voice or list(VOICE_CASES)
    unknown = [n for n in wanted if n not in VOICE_CASES]
    if unknown:
        sys.exit(f'Unknown voice case(s) {unknown}; known: {list(VOICE_CASES)}')
    results = []
    for index, case in enumerate(wanted):
        print(f'--- voice {case} ...', flush=True)
        failures, detail = run_voice_case(args, case, args.base_port + 400 + index)
        results.append((case, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')
    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} voice cases passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


def main_link(args):
    wanted = args.link or list(LINK_CASES)
    unknown = [n for n in wanted if n not in LINK_CASES]
    if unknown:
        sys.exit(f'Unknown link case(s) {unknown}; known: {list(LINK_CASES)}')
    results = []
    for index, case in enumerate(wanted):
        print(f'--- link {case} ...', flush=True)
        failures, detail = run_link_case(args, case, args.base_port + 300 + index)
        results.append((case, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')
    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} link cases passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


HID_ADDRESS = '00:A2:D0:00:00:03'
# An address on the allow-list that is not the emulated device's
HID_OTHER_ADDRESS = '00:A2:D0:00:00:09'
HID_SECONDS = 14
# emu_hid.py's built-in script: 12 reports, and what BlueCaster's decoder
# (hid-input decode.rs) makes of them: one event per key or button going
# down or up (Shift + B down and up: 2 each), one per report that moves the
# pointer and one per wheel report. The last key stays held: a dropped link
# releases it, one event more.
HID_SCRIPT_REPORTS = 12
HID_SCRIPT_EVENTS = 14
HID_PSM_CONTROL, HID_PSM_INTERRUPT = 0x0011, 0x0013
# --hid-leds of the light cases: Num Lock and Scroll Lock
HID_LEDS = 5
# In a case's device options: the file run_cli writes the passkey to
PASSKEY_FILE = '<passkey-file>'
# The input through a recording input helper (a test switch of debug builds)
HELPER_RECORD = '--hid-helper-record'
HELPER_LINE = re.compile(r'^hid helper: (.*)$', re.M)
# BlueCaster's line with the number to type (printed over --hci-tcp only)
PASSKEY_LINE = re.compile(r'^hid passkey shown (\d{6})$', re.M)


def run_cli(cmd, env, timeout, passkey_file=None):
    """bluecaster-core with its output read as it comes: (exit code, output).
    With passkey_file, the number of a "hid passkey shown" line is written
    there for the emulated keyboard to type."""
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding='utf-8',
                            errors='replace', env=env)
    lines = []

    def read():
        for line in proc.stdout:
            lines.append(line)
            m = PASSKEY_LINE.match(line.rstrip('\r\n'))
            if m and passkey_file:
                with open(passkey_file, 'w', encoding='ascii') as f:
                    f.write(m.group(1))

    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    try:
        rc = proc.wait(timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(10)
        rc = 'timeout'
    reader.join(10)
    return rc, ''.join(lines)


def with_passkey_file(device_args, out):
    """The device options with PASSKEY_FILE made a path in `out` (None: the
    case types no passkey)"""
    if PASSKEY_FILE not in device_args:
        return device_args, None
    path = os.path.join(out, 'passkey.txt')
    if os.path.exists(path):
        os.remove(path)
    return [path if a == PASSKEY_FILE else a for a in device_args], path


def check_passkey(case, text, emu_text, expect_passkey):
    """A passkey case: one number shown, typed by the device, then cleared.
    Any other case: no number shown at all (Just Works as before)."""
    failures = []
    shown = PASSKEY_LINE.findall(text)
    if expect_passkey:
        if len(shown) != 1:
            failures.append(f'{len(shown)} "hid passkey shown" line(s), expected 1')
        if 'hid: typing the passkey and Enter' not in emu_text:
            failures.append('the device did not type the passkey (see hid.log)')
        if 'hid passkey cleared' not in text:
            failures.append('the number was not cleared after pairing')
        # The number never in a log line, only in the line for the test
        for d in shown:
            if any(d in line for line in text.splitlines()
                   if not PASSKEY_LINE.match(line) and re.search(r'passkey|number|\bpin\b', line, re.I)):
                failures.append(f'{case}: the passkey appears in another output line')
                break
    elif shown or 'hid: asked to type a passkey' in emu_text:
        failures.append('a number was shown or asked for, expected Just Works')
    return failures


# case: (emu_hid.py options, bluecaster-core options; None: the scan mode)
HID_CASES = {
    # --hid-inject: over --hci-tcp the input must still only be counted
    'hid-connect': ([], ['--hid-device', HID_ADDRESS, '--hid-connect', HID_ADDRESS, '--hid-inject']),
    'hid-incoming': (['--initiate'], ['--hid-device', HID_ADDRESS]),
    'hid-refused': (['--initiate'], ['--hid-device', HID_OTHER_ADDRESS]),
    'hid-release': (['--initiate', '--disconnect-after', '1'], ['--hid-device', HID_ADDRESS]),
    'hid-off': (['--initiate'], None),
    # Two runs on one configuration: the device connects by itself and
    # types at once; the second run decodes from the first report with the
    # descriptor kept from the first
    'hid-kept-descriptor': (['--initiate', '--start-delay', '0'], ['--hid-device', HID_ADDRESS]),
    # The keyboard lights from --hid-leds (Num Lock + Scroll Lock) reach the
    # device as an output report once it is connected
    'hid-leds': (['--initiate'], ['--hid-device', HID_ADDRESS, '--hid-leds', str(HID_LEDS)]),
    # A keyboard that asks for a passkey (keyboard only, MITM): BlueCaster
    # shows a number, the device types it (PASSKEY_FILE: see run_cli)
    # Through a real input helper process that records what it would pass
    # to Windows instead (--hid-helper-record, debug builds only)
    'hid-incoming-helper': (['--initiate'], ['--hid-device', HID_ADDRESS, HELPER_RECORD]),
    'hid-release-helper': (['--initiate', '--disconnect-after', '1'], ['--hid-device', HID_ADDRESS, HELPER_RECORD]),
    'hid-passkey': (['--passkey-file', PASSKEY_FILE],
                    ['--hid-device', HID_ADDRESS, '--hid-connect', HID_ADDRESS]),
}


def check_helper(cli_args, text, final, expect, held):
    """With --hid-helper-record: one helper ran and recorded every event
    (each one of the script maps to a Windows input), Windows "took" them
    all, nothing was dropped, and `held` keys were still held at the end.
    Without it: no helper at all."""
    m = HELPER_LINE.search(text)
    if HELPER_RECORD not in cli_args:
        return [f'a helper line without {HELPER_RECORD}: {m.group(0)}'] if m else []
    if 'is a test switch of debug builds' in text:
        return [f'{HELPER_RECORD} needs a debug build of bluecaster-core (--bluecaster)']
    if m is None:
        return ['no "hid helper:" line (see bluecaster.log)']
    h = parse_hid_line(m.group(1))
    failures = []
    events = final.get('events', 0)
    if h.get('started') != 1 or h.get('connected') != 1:
        failures.append(f'helper started={h.get("started")} connected={h.get("connected")}, expected 1 and 1')
    if h.get('recorded') != events:
        failures.append(f'the helper recorded {h.get("recorded")} inputs, expected {events} (one per event)')
    if h.get('held') != held:
        failures.append(f'{h.get("held")} key(s) held in the record at the end, expected {held}')
    if final.get('dropped') != 0:
        failures.append(f'dropped={final.get("dropped")}, expected 0')
    expect['injected'] = events
    return failures


def check_hid_leds(cli_args, emu_text, final):
    """The keyboard lights: with --hid-leds the device gets them as output
    report 1 (one byte, the boot keyboard's bits); without it (dry run, no
    test lights) nothing is sent"""
    outputs = [o.strip() for o in re.findall(r'hid: output report ([0-9a-f ]+)', emu_text)]
    if '--hid-leds' in cli_args:
        expected = f'01 {HID_LEDS:02x}'
        if expected not in outputs:
            return [f'the device got output reports {outputs}, expected "{expected}" (see hid.log)']
        if final.get('leds', 0) < 1:
            return [f'leds={final.get("leds")}, expected >= 1']
        return []
    if outputs or final.get('leds'):
        return [f'keyboard lights sent without --hid-leds ({outputs}, leds={final.get("leds")})']
    return []


def l2cap_signals(path):
    """BR/EDR L2CAP signaling of a PacketLogger capture: (sent, code, data)
    for connection requests (code 2), their responses (3) and disconnection
    requests (6); sent is True for what the capture's host sent. Plus the number of HID input reports
    (DATA | input, 0xA1) the host received on dynamic channels."""
    signals, reports = [], 0
    try:
        with open(path, 'rb') as f:
            data = f.read()
    except OSError:
        return signals, reports
    i = 0
    while i + 13 <= len(data):
        length, = struct.unpack_from('>I', data, i)
        kind, payload = data[i + 12], data[i + 13:i + 4 + length]
        i += 4 + length
        if kind not in (0x02, 0x03) or len(payload) < 8:
            continue
        flags = struct.unpack_from('<H', payload, 0)[0] >> 12 & 0x3
        if flags == 0x1:
            continue  # a continuation fragment (these PDUs are all small)
        cid = struct.unpack_from('<H', payload, 6)[0]
        body = payload[8:]
        if cid == 0x0001:
            j = 0
            while j + 4 <= len(body):
                code, _, size = body[j], body[j + 1], struct.unpack_from('<H', body, j + 2)[0]
                if code in (0x02, 0x03, 0x06):
                    signals.append((kind == 0x02, code, body[j + 4:j + 4 + size]))
                j += 4 + size
        elif cid >= 0x0040 and kind == 0x03 and body[:1] == b'\xa1':
            reports += 1
    return signals, reports


def hid_channels(signals):
    """{psm: (initiated by the capture's host, final result)} of the HID
    channels: the last response to each request (pending ones come first);
    'closed' when the answering side ended a pending one with a
    disconnection request instead (BTstack's way of declining a HID device)"""
    channels = {}
    for sent, code, d in signals:
        if code == 0x02 and len(d) >= 4:
            psm, scid = struct.unpack_from('<HH', d)
            if psm in (HID_PSM_CONTROL, HID_PSM_INTERRUPT):
                channels[(not sent, scid)] = [psm, sent, None]
        elif code == 0x03 and len(d) >= 6:
            dcid, scid, result = struct.unpack_from('<HHH', d)
            # The response comes from the side that did not ask. BTstack puts
            # the asker's CID first when it refuses an unknown PSM.
            key = (sent, scid) if (sent, scid) in channels else (sent, dcid)
            if key in channels:
                channels[key][2] = result
        elif code == 0x06 and len(d) >= 4:
            dcid, _ = struct.unpack_from('<HH', d)
            if (sent, dcid) in channels and channels[(sent, dcid)][2] in (None, 1):
                channels[(sent, dcid)][2] = 'closed'
    return {psm: (sent, result) for psm, sent, result in channels.values()}


def run_hid_kept_descriptor(args, case, port):
    """hid-kept-descriptor: the first run may drop the first reports
    (BTstack reads the descriptor only after the channels are open) and
    keeps the descriptor; the second decodes every report"""
    out = os.path.join(args.out, case)
    shutil.rmtree(out, ignore_errors=True)
    config = os.path.join(out, 'config')
    first, detail1 = run_hid_case(args, case, port, os.path.join(out, 'first'), config)
    # The first run's own failures are expected (undecoded reports): only
    # that it ran matters
    first = [f for f in first if not f.startswith(('undecoded=', 'events='))]
    kept = glob.glob(os.path.join(config, '**', 'hid_descriptors', '*.bin'), recursive=True)
    if not kept:
        first.append('no report descriptor kept after the first run')
    second, detail2 = run_hid_case(args, case, port + 1, os.path.join(out, 'second'), config)
    if 'report descriptor known' not in read_text(os.path.join(out, 'second', 'bluecaster.log')):
        second.append('the kept descriptor was not in use when the channels opened')
    return [f'first run: {f}' for f in first] + [f'second run: {f}' for f in second],         f'first: {detail1}; second: {detail2}'


def parse_hid_line(line):
    return {k: int(v) for k, v in re.findall(r'(\w+)=(\d+)', line)}


def run_hid_case(args, case, port, out=None, config=None):
    """bluecaster-core --cli --hid-test against emu_hid.py"""
    device_args, cli_args = HID_CASES[case]
    if case == 'hid-kept-descriptor' and out is None:
        return run_hid_kept_descriptor(args, case, port)
    out = out or os.path.join(args.out, case)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    failures = []
    port = free_port(port)
    emu_log = os.path.join(out, 'hid.log')
    device_args, passkey_file = with_passkey_file(device_args, out)
    emu = start_sink(port, os.path.join(out, 'hid.pklg'), emu_log, script='emu_hid.py',
                     extra=['--address', HID_ADDRESS] + device_args)
    seconds = HID_SECONDS if cli_args is not None else SCAN_SECONDS
    mode = ['--hid-test', str(HID_SECONDS)] + cli_args if cli_args is not None else ['--scan', str(SCAN_SECONDS)]
    try:
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=config or os.path.join(out, 'config'), BLUECASTER_LOG='debug')
        rc, text = run_cli([args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}',
                            '--hci-capture', os.path.join(out, 'bluecaster.pklg')] + mode,
                           env, seconds + 60, passkey_file)
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8') as f:
            f.write(text)
    finally:
        emu.terminate()
        emu.wait(10)
    emu_text = read_text(emu_log)
    if rc != 0:
        failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
    signals, captured_reports = l2cap_signals(os.path.join(out, 'bluecaster.pklg'))
    channels = hid_channels(signals)
    sent = re.search(r'hid: sent (\d+) of (\d+) reports', emu_text)
    sent = int(sent.group(1)) if sent else 0

    if cli_args is None:
        # Off: nothing answers on the HID PSMs, the device's control
        # channel is refused as an unknown PSM (0x0002)
        control = channels.get(HID_PSM_CONTROL)
        if control is None or control[0]:
            failures.append(f'no HID control channel request from the device in the capture ({channels})')
        elif control[1] != 0x0002:
            failures.append(f'control channel answered {control[1]}, expected 2 (PSM not supported)')
        if HID_PSM_INTERRUPT in channels:
            failures.append('an interrupt channel was asked for')
        if not re.search(r'hid: control channel (refused|failed)', emu_text):
            failures.append('the device did not report its control channel refused or failed')
        if re.search(r'^hid', text, re.M):
            failures.append('HID lines from a core without --hid-*')
        return failures, f'control channel {channels.get(HID_PSM_CONTROL)}, {captured_reports} reports captured'

    lines = [parse_hid_line(line) for line in text.splitlines() if line.startswith('hid: t=')]
    final = next((parse_hid_line(line) for line in text.splitlines() if line.startswith('hid final:')), None)
    if HELPER_RECORD in cli_args and 'is a test switch of debug builds' in text:
        return [f'{HELPER_RECORD} needs a debug build of bluecaster-core (--bluecaster)'], 'release build'
    if final is None:
        return failures + ['no "hid final:" line (see bluecaster.log)'], 'no counts'
    detail = ' '.join(f'{k}={v}' for k, v in final.items())
    peak = max((c.get('connected', 0) for c in lines), default=0)
    expect = {'injected': 0, 'blocked': 0, 'undecoded': 0}
    if case == 'hid-refused':
        expect.update(connected=0, reports=0, events=0)
        if final.get('refused', 0) < 1:
            failures.append(f'refused={final.get("refused")}, expected >= 1')
        if peak:
            failures.append(f'connected={peak} at some point, expected 0 throughout')
        control = channels.get(HID_PSM_CONTROL)
        if control is None or control[0] or control[1] in (None, 0, 1):
            failures.append(f'the control channel was not refused or closed in the capture ({control})')
        if HID_PSM_INTERRUPT in channels and channels[HID_PSM_INTERRUPT][1] == 0:
            failures.append('an interrupt channel was accepted')
        if 'hid: control channel refused' not in emu_text:
            failures.append('the device did not report its control channel refused')
    else:
        release = case in ('hid-release', 'hid-release-helper')
        expect.update(reports=HID_SCRIPT_REPORTS, events=HID_SCRIPT_EVENTS + release, refused=0,
                      connected=0 if release else 1)
        if sent != HID_SCRIPT_REPORTS:
            failures.append(f'the device sent {sent} reports, expected {HID_SCRIPT_REPORTS} (see hid.log)')
        if captured_reports != HID_SCRIPT_REPORTS:
            failures.append(f'{captured_reports} input reports in the capture, expected {HID_SCRIPT_REPORTS}')
        if peak != 1:
            failures.append(f'connected={peak} at most, expected 1')
        by_bluecaster = case in ('hid-connect', 'hid-passkey')
        for psm in (HID_PSM_CONTROL, HID_PSM_INTERRUPT):
            ch = channels.get(psm)
            if ch is None or ch[0] != by_bluecaster or ch[1] != 0:
                failures.append(f'PSM 0x{psm:04X}: {ch}, expected opened by '
                                f'{"BlueCaster" if by_bluecaster else "the device"} with result 0')
        if release:
            if 'hid: dropping the link' not in emu_text:
                failures.append('the device did not drop the link')
            # The key still held at the drop goes up: one event after the reports
            before = [c for c in lines if c.get('reports') == HID_SCRIPT_REPORTS and c.get('connected') == 1]
            if not any(c.get('events') == HID_SCRIPT_EVENTS for c in before):
                failures.append(f'never events={HID_SCRIPT_EVENTS} while connected after the last report')
    if '--hid-inject' in cli_args and '--hid-inject is not used with --hci-tcp' not in text:
        failures.append('--hid-inject over --hci-tcp: no word that the input is only counted')
    if case != 'hid-refused':
        failures += check_hid_leds(cli_args, emu_text, final)
        # The script's last key stays held until the link drops
        failures += check_helper(cli_args, text, final, expect, 0 if release else 1)
    failures += check_passkey(case, text, emu_text, passkey_file is not None)
    for key, value in expect.items():
        if final.get(key) != value:
            failures.append(f'{key}={final.get(key)}, expected {value}')
    # Counts only: neither the device's address nor anything typed in the
    # HID host's own lines
    for line in text.splitlines():
        if re.search(r'\bhid\b|keyboards and mice', line, re.I) and HID_ADDRESS in line:
            failures.append(f'an address in a HID line: {line.strip()[:120]}')
            break
    return failures, detail


HID_LE_ADDRESS = '00:A2:D0:00:00:05'
HID_LE_OTHER_ADDRESS = '00:A2:D0:00:00:0A'
HID_LE_SECONDS = 16
# case: (emu_hid_le.py options, bluecaster-core options)
HID_LE_CASES = {
    # --hid-inject: over --hci-tcp the input must still only be counted
    'hid-le-connect': ([], ['--hid-le-device', HID_LE_ADDRESS, '--hid-connect', HID_LE_ADDRESS, '--hid-inject']),
    'hid-le-reconnect': (['--drop-after', '1'], ['--hid-le-device', HID_LE_ADDRESS]),
    'hid-le-release': (['--drop-after', '1', '--stay-away'], ['--hid-le-device', HID_LE_ADDRESS]),
    'hid-le-not-listed': ([], ['--hid-le-device', HID_LE_OTHER_ADDRESS]),
    'hid-le-scan': ([], None),
    'hid-le-audio-untouched': (['--discover-host'], ['--hid-le-device', HID_LE_ADDRESS, '--hid-connect', HID_LE_ADDRESS]),
    # The keyboard lights from --hid-leds written to the Output report
    'hid-le-leds': ([], ['--hid-le-device', HID_LE_ADDRESS, '--hid-leds', str(HID_LEDS)]),
    # Listed with its first private address ({rpa}: the device's READY
    # line), it pairs and tells its identity; it drops the link and comes
    # back with another private address, which only its IRK resolves
    'hid-le-privacy': (['--privacy', '--drop-after', '1'], ['--hid-le-device', '{rpa}/random']),
    # A bonded device sends input at once when the link is encrypted again
    'hid-le-early-report': (['--drop-after', '1', '--report-on-encrypt'], ['--hid-le-device', HID_LE_ADDRESS]),
    # A keyboard that asks for MITM: BlueCaster shows a number, the device
    # types it (PASSKEY_FILE: see run_cli)
    'hid-le-release-helper': (['--drop-after', '1', '--stay-away'], ['--hid-le-device', HID_LE_ADDRESS, HELPER_RECORD]),
    'hid-le-passkey': (['--passkey-file', PASSKEY_FILE],
                       ['--hid-le-device', HID_LE_ADDRESS, '--hid-connect', HID_LE_ADDRESS]),
}
# The cases with a second link (the device drops the first one and comes back)
HID_LE_TWO_LINKS = ('hid-le-reconnect', 'hid-le-privacy', 'hid-le-early-report')
# The services of LE Audio the PC's GATT server has with it on (PACS, ASCS,
# CAS, CSIS, VCS, MCS / GMCS, TBS / GTBS, TMAS, GMAS, BASS, PBA)
LE_AUDIO_SERVICES = ['1843', '1844', '1845', '1846', '1848', '1849', '184B', '184C', '184D', '184E', '184F',
                     '1850', '1853', '1855', '1856', '1858']


def le_capture(path):
    """What BlueCaster's PacketLogger capture shows of LE: SMP Pairing
    Requests it sent, LE Enable Encryption commands, ATT Write Requests of a
    CCC value enabling notifications, successful LE (Enhanced) Connection
    Complete events"""
    counts = dict(pairing_requests=0, encryptions=0, subscriptions=0, connections=0)
    try:
        with open(path, 'rb') as f:
            data = f.read()
    except OSError:
        return counts
    i = 0
    while i + 13 <= len(data):
        length, = struct.unpack_from('>I', data, i)
        kind, payload = data[i + 12], data[i + 13:i + 4 + length]
        i += 4 + length
        if kind == 0x00 and len(payload) >= 2 and struct.unpack_from('<H', payload, 0)[0] == 0x2019:
            counts['encryptions'] += 1
        elif kind == 0x01 and len(payload) >= 4 and payload[0] == 0x3E and payload[2] in (0x01, 0x0A):
            counts['connections'] += payload[3] == 0
        elif kind == 0x02 and len(payload) >= 9:
            cid = struct.unpack_from('<H', payload, 6)[0]
            body = payload[8:]
            if cid == 0x0006 and body[:1] == b'\x01':
                counts['pairing_requests'] += 1
            elif cid == 0x0004 and len(body) == 5 and body[0] == 0x12 and body[3:5] == b'\x01\x00':
                counts['subscriptions'] += 1
    return counts


def run_core(args, out, port, mode, emu_args, seconds):
    """emu_hid_le.py and one bluecaster-core run against it: (exit code,
    BlueCaster's output, the device's log)"""
    os.makedirs(out, exist_ok=True)
    port = free_port(port)
    emu_log = os.path.join(out, 'hid.log')
    emu_args, passkey_file = with_passkey_file(emu_args, out)
    emu = start_sink(port, os.path.join(out, 'hid.pklg'), emu_log, script='emu_hid_le.py',
                     extra=['--address', HID_LE_ADDRESS] + emu_args)
    # {rpa}: the first private address the device advertises with
    rpa = re.search(r'\brpa=(\S+)', emu.ready_line)
    mode = [m.replace('{rpa}', rpa.group(1) if rpa else '') for m in mode]
    try:
        env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'), BLUECASTER_LOG='debug')
        rc, text = run_cli([args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}',
                            '--hci-capture', os.path.join(out, 'bluecaster.pklg')] + mode,
                           env, seconds + 60, passkey_file)
        with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8') as f:
            f.write(text)
    finally:
        emu.terminate()
        emu.wait(10)
    return rc, text, read_text(emu_log)


def run_hid_le_case(args, case, port):
    """bluecaster-core --cli --hid-test (or the scan modes) against emu_hid_le.py"""
    device_args, cli_args = HID_LE_CASES[case]
    out = os.path.join(args.out, case)
    shutil.rmtree(out, ignore_errors=True)
    failures = []
    if cli_args is None:
        # Keyboards and mice on: listed as one; off (LE Audio's scan): not
        rc, text, _ = run_core(args, os.path.join(out, 'on'), port, ['--hid-scan', '6'], device_args, 6)
        on = [line for line in text.splitlines() if line.startswith(HID_LE_ADDRESS)]
        if rc != 0:
            failures.append(f'--hid-scan exited with {rc} (see on/bluecaster.log)')
        if not any('(LE keyboard or mouse, public)' in line for line in on):
            failures.append(f'--hid-scan did not list the device as an LE keyboard or mouse ({on})')
        rc, text, _ = run_core(args, os.path.join(out, 'off'), port + 1, ['--scan', '6', '--le-audio'], device_args, 6)
        off = [line for line in text.splitlines() if line.startswith(HID_LE_ADDRESS)]
        if rc != 0:
            failures.append(f'--scan --le-audio exited with {rc} (see off/bluecaster.log)')
        if off:
            failures.append(f'listed with keyboards and mice off: {off}')
        if 'LE Audio: scanning beside the classic scan' not in text:
            failures.append('the LE Audio scan did not run (no LE scan to leave the device out of)')
        return failures, f'on: {len(on)} entry, off: {len(off)} entries'

    rc, text, emu_text = run_core(args, out, port, ['--hid-test', str(HID_LE_SECONDS)] + cli_args, device_args,
                                  HID_LE_SECONDS)
    if rc != 0:
        failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
    cap = le_capture(os.path.join(out, 'bluecaster.pklg'))
    lines = [parse_hid_line(line) for line in text.splitlines() if line.startswith('hid: t=')]
    final = next((parse_hid_line(line) for line in text.splitlines() if line.startswith('hid final:')), None)
    if HELPER_RECORD in cli_args and 'is a test switch of debug builds' in text:
        return [f'{HELPER_RECORD} needs a debug build of bluecaster-core (--bluecaster)'], 'release build'
    if final is None:
        return failures + ['no "hid final:" line (see bluecaster.log)'], 'no counts'
    detail = ' '.join(f'{k}={v}' for k, v in final.items()) + ' ' + ' '.join(f'{k}={v}' for k, v in cap.items())
    peak = max((c.get('connected', 0) for c in lines), default=0)
    sent = [int(n) for n in re.findall(r'hid: sent (\d+) of \d+ reports', emu_text)]
    links = emu_text.count('hid: link up')
    expect = {'injected': 0, 'blocked': 0, 'undecoded': 0, 'refused': 0}
    if case == 'hid-le-not-listed':
        expect.update(connected=0, reports=0, events=0)
        if peak:
            failures.append(f'connected={peak} at some point, expected 0 throughout')
        if links or cap['connections']:
            failures.append(f'the device was connected to ({links} link(s), {cap["connections"]} in the capture)')
        if cap['subscriptions']:
            failures.append(f'{cap["subscriptions"]} subscription(s) in the capture, expected 0')
    else:
        rounds = 2 if case in HID_LE_TWO_LINKS else 1
        released = case in HID_LE_TWO_LINKS or case in ('hid-le-release', 'hid-le-release-helper')
        # The mouse move sent at once on the second encryption
        early = 1 if case == 'hid-le-early-report' else 0
        expect.update(reports=HID_SCRIPT_REPORTS * rounds + early,
                      events=HID_SCRIPT_EVENTS * rounds + released + early,
                      connected=0 if case in ('hid-le-release', 'hid-le-release-helper') else 1)
        if sum(sent) != HID_SCRIPT_REPORTS * rounds:
            failures.append(f'the device sent {sent} reports, expected {HID_SCRIPT_REPORTS} x {rounds} (see hid.log)')
        if peak != 1:
            failures.append(f'connected={peak} at most, expected 1')
        if links != rounds or cap['connections'] != rounds:
            failures.append(f'{links} link(s) at the device, {cap["connections"]} in the capture, expected {rounds}')
        # One pairing; a reconnection encrypts with the stored bond
        if cap['pairing_requests'] != 1 or emu_text.count('hid: paired') != 1:
            failures.append(f'{cap["pairing_requests"]} Pairing Request(s), {emu_text.count("hid: paired")} pairing(s) '
                            'at the device, expected 1')
        if cap['encryptions'] != rounds:
            failures.append(f'{cap["encryptions"]} encryption(s), expected {rounds}')
        if cap['subscriptions'] != 3 * rounds:
            failures.append(f'{cap["subscriptions"]} subscription(s), expected {3 * rounds}')
        if released:
            if 'hid: dropping the link' not in emu_text:
                failures.append('the device did not drop the link')
            before = [c for c in lines if c.get('reports') == HID_SCRIPT_REPORTS and c.get('connected') == 1]
            if not any(c.get('events') == HID_SCRIPT_EVENTS for c in before):
                failures.append(f'never events={HID_SCRIPT_EVENTS} while connected after the first script')
        if case in ('hid-le-reconnect', 'hid-le-early-report') and 'connected by itself' not in text:
            failures.append('no background connection in bluecaster.log')
        if case == 'hid-le-early-report':
            if 'hid: sent one report at once on encryption' not in emu_text:
                failures.append('the device did not send its report at once on the second encryption (see hid.log)')
            if 'kept input report(s) are listened to' not in text:
                failures.append('the kept report handles were not listened to on the second link')
        if case == 'hid-le-privacy':
            rpas = re.findall(r'hid: advertising with private address \d+ \((\S+)\)', emu_text)
            if len(rpas) < 2 or len(set(rpas)) != len(rpas):
                failures.append(f'the device did not come back with another private address ({len(rpas)} seen)')
            if 'found by its private address connected' not in text:
                failures.append('the second link was not made from the private address the PC resolved')
            if 'listed with its identity address from now on' not in text:
                failures.append('the device was not listed with its identity address after pairing')
            for line in text.splitlines():
                if re.search(r'\bhid\b|keyboards? (and|or) m|LE: ', line, re.I) and any(
                        r.upper() in line.upper() for r in rpas):
                    failures.append(f'a private address in a HID line: {line.strip()[:120]}')
                    break
    if case == 'hid-le-audio-untouched':
        services = re.search(r'hid: host services (.*)', emu_text)
        if services is None:
            failures.append("the device did not read BlueCaster's GATT services (see hid.log)")
        else:
            found = [u for u in LE_AUDIO_SERVICES if u in services.group(1).upper()]
            if found:
                failures.append(f'LE Audio services in BlueCaster\'s GATT server: {found}')
            if '1800' not in services.group(1):
                failures.append(f'GAP service not in what the device read: {services.group(1)[:200]}')
        if 'LE: modules HOGP' not in text:
            failures.append('not "LE: modules HOGP" alone in bluecaster.log')
        for word in ('LE Audio: initializing', 'LE Audio: found', 'LE Audio: ready'):
            if word in text:
                failures.append(f'"{word}" in bluecaster.log with LE Audio off')
    failures += check_passkey(case, text, emu_text, PASSKEY_FILE in device_args)
    if '--hid-inject' in cli_args and '--hid-inject is not used with --hci-tcp' not in text:
        failures.append('--hid-inject over --hci-tcp: no word that the input is only counted')
    if case != 'hid-le-not-listed':
        failures += check_hid_leds(cli_args, emu_text, final)
        failures += check_helper(cli_args, text, final, expect, 0)
    for key, value in expect.items():
        if final.get(key) != value:
            failures.append(f'{key}={final.get(key)}, expected {value}')
    # Counts only: no address in the lines about keyboards and mice
    for line in text.splitlines():
        if re.search(r'\bhid\b|keyboards? (and|or) m', line, re.I) and HID_LE_ADDRESS in line.upper():
            failures.append(f'an address in a HID line: {line.strip()[:120]}')
            break
    return failures, detail


# The check of a device being added: case -> (LE, sends its script).
# BlueCaster connects it as the app adds one, with a countdown of
# HID_CONFIRM_SECONDS from when it is ready.
HID_CONFIRM_CASES = {
    'hid-confirm': (False, True),
    'hid-confirm-silent': (False, False),
    'hid-le-confirm': (True, True),
    'hid-le-confirm-silent': (True, False),
}
HID_CONFIRM_SECONDS = 4


def run_hid_confirm_case(args, case, port):
    """bluecaster-core --cli --hid-test --hid-connect --hid-confirm against
    emu_hid.py or emu_hid_le.py, the device with or without its script"""
    le, types = HID_CONFIRM_CASES[case]
    out = os.path.join(args.out, case)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    failures = []
    device_args = []
    if not types:
        empty = os.path.join(out, 'no-reports.txt')
        with open(empty, 'w', encoding='ascii') as f:
            f.write('# no input at all\n')
        device_args = ['--reports', empty]
    address = HID_LE_ADDRESS if le else HID_ADDRESS
    listed = ['--hid-le-device' if le else '--hid-device', address]
    mode = ['--hid-test', str(HID_LE_SECONDS if le else HID_SECONDS)] + listed + [
        '--hid-connect', address, '--hid-confirm', str(HID_CONFIRM_SECONDS)]
    if le:
        rc, text, emu_text = run_core(args, out, port, mode, device_args, HID_LE_SECONDS)
    else:
        port = free_port(port)
        emu_log = os.path.join(out, 'hid.log')
        emu = start_sink(port, os.path.join(out, 'hid.pklg'), emu_log, script='emu_hid.py',
                         extra=['--address', HID_ADDRESS] + device_args)
        try:
            env = dict(os.environ, BLUECASTER_CONFIG_DIR=os.path.join(out, 'config'), BLUECASTER_LOG='debug')
            rc, text = run_cli([args.bluecaster, '--cli', '--hci-tcp', f'127.0.0.1:{port}',
                                '--hci-capture', os.path.join(out, 'bluecaster.pklg')] + mode,
                               env, HID_SECONDS + 60)
            with open(os.path.join(out, 'bluecaster.log'), 'w', encoding='utf-8') as f:
                f.write(text)
        finally:
            emu.terminate()
            emu.wait(10)
        emu_text = read_text(emu_log)
    if rc != 0:
        failures.append(f'BlueCaster exited with {rc} (see bluecaster.log)')
    lines = text.splitlines()
    final = next((parse_hid_line(line) for line in lines if line.startswith('hid final:')), None)
    if final is None:
        return failures + ['no "hid final:" line (see bluecaster.log)'], 'no counts'
    checks = [line.strip() for line in lines if line.startswith('hid check ')]
    detail = f'{" / ".join(checks) or "no check lines"}; ' + ' '.join(f'{k}={v}' for k, v in final.items())
    if 'hid check waiting' not in checks:
        failures.append('no "hid check waiting": the countdown never started')
    if types:
        if checks[-1:] != ['hid check confirmed']:
            failures.append(f'expected "hid check confirmed" last, got {checks}')
        expect = dict(connected=1, reports=HID_SCRIPT_REPORTS, events=HID_SCRIPT_EVENTS)
        # Still connected until the test ends (the link goes down at exit)
        after = lines[next((i for i, line in enumerate(lines) if line == 'hid check confirmed'), len(lines)):]
        if any(parse_hid_line(line).get('connected') != 1 for line in after if line.startswith('hid: t=')):
            failures.append('a confirmed device was disconnected during the test')
    else:
        if checks[-1:] != ['hid check removed no_input']:
            failures.append(f'expected "hid check removed no_input" last, got {checks}')
        expect = dict(connected=0, reports=0, events=0)
        if 'hid: link down' not in emu_text:
            failures.append('the device was not disconnected after the countdown (see hid.log)')
        if 'the new device is removed again (no_input)' not in text:
            failures.append('no log line of the removal')
        # Not before the countdown ran out: the removal comes after a count
        # line of at least HID_CONFIRM_SECONDS
        removed_at = next((i for i, line in enumerate(lines) if line.startswith('hid check removed')), None)
        seconds = [int(m.group(1)) for line in lines[:removed_at or 0] if (m := re.match(r'hid: t=(\d+)', line))]
        if removed_at is not None and max(seconds, default=0) < HID_CONFIRM_SECONDS:
            failures.append(f'removed after {max(seconds, default=0)} s, before the {HID_CONFIRM_SECONDS} s countdown')
    for key, value in expect.items():
        if final.get(key) != value:
            failures.append(f'{key}={final.get(key)}, expected {value}')
    for line in lines:
        if re.search(r'\bhid\b|keyboards? (and|or) m', line, re.I) and address in line.upper():
            failures.append(f'an address in a HID line: {line.strip()[:120]}')
            break
    return failures, detail


def main_hid(args):
    known = list(HID_CASES) + list(HID_LE_CASES) + list(HID_CONFIRM_CASES)
    wanted = args.hid or known
    unknown = [n for n in wanted if n not in known]
    if unknown:
        sys.exit(f'Unknown HID case(s) {unknown}; known: {known}')
    results = []
    for index, case in enumerate(wanted):
        print(f'--- {case} ...', flush=True)
        run = (run_hid_confirm_case if case in HID_CONFIRM_CASES
               else run_hid_le_case if case in HID_LE_CASES else run_hid_case)
        failures, detail = run(args, case, args.base_port + 600 + 2 * index)
        # The recording helper exists in debug builds only: with the whole
        # suite on a release build these cases are skipped, asked for by
        # name they fail
        if detail == 'release build' and not args.hid:
            print('    SKIP  needs a debug build of bluecaster-core')
            continue
        results.append((case, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')
    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} HID cases passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


def main():
    parser = argparse.ArgumentParser(description='End-to-end A2DP test of BlueCaster against a virtual sink')
    parser.add_argument('--codecs', nargs='+', choices=CODECS + list(NEGOTIATION),
                        default=CODECS + list(NEGOTIATION),
                        help='codecs and negotiation cases (%(default)s)')
    parser.add_argument('--le-audio', nargs='*', metavar='CASE',
                        help='run the LE Audio cases instead (default: all of them)')
    parser.add_argument('--auracast', nargs='*', metavar='CASE',
                        help='run the Auracast cases instead (default: all of them)')
    parser.add_argument('--scan', nargs='*', metavar='CASE',
                        help=f'run the device scan cases instead ({", ".join(SCAN_CASES)}; default: all of them)')
    parser.add_argument('--link', nargs='*', metavar='CASE',
                        help=f'run the link cases instead ({", ".join(LINK_CASES)}; default: all of them)')
    parser.add_argument('--voice', nargs='*', metavar='CASE',
                        help=f'run the HFP audio cases instead ({", ".join(VOICE_CASES)}; default: all of them)')
    parser.add_argument('--le-pairing', nargs='*', metavar='CASE',
                        help=f'run the dual-mode pairing cases instead ({", ".join(LE_PAIRING_CASES)}; '
                             'default: all of them)')
    parser.add_argument('--hid', nargs='*', metavar='CASE',
                        help='run the keyboard and mouse cases instead '
                             f'({", ".join(list(HID_CASES) + list(HID_LE_CASES) + list(HID_CONFIRM_CASES))}; '
                             'default: all of them)')
    parser.add_argument('--duration', type=int, default=5, help='seconds of streaming per codec')
    parser.add_argument('--max-packet', type=int, help='passed to BlueCaster --max-packet (default: its own)')
    parser.add_argument('--build-dir', default=os.path.join(REPO, 'build'),
                        help="the decoder tool's CMake builds (cmake -S tools/a2dp_decode -B build/a2dp_decode)")
    parser.add_argument('--config', default='Release', help='build configuration (Release / Debug)')
    parser.add_argument('--bluecaster', help='BlueCaster core executable to test (default: target/release/bluecaster-core.exe)')
    parser.add_argument('--decoder', help='bluecaster_decode executable (default: <build-dir>/a2dp_decode/<config>)')
    parser.add_argument('--out', default=os.path.join(HERE, 'out'))
    parser.add_argument('--base-port', type=int, default=BASE_PORT,
                        help='first TCP port (LE Audio cases use +100); change it to run several tests at once')
    args = parser.parse_args()
    args.bluecaster = args.bluecaster or os.path.join(REPO, 'target', args.config.lower(), 'bluecaster-core.exe')
    args.decoder = args.decoder or os.path.join(args.build_dir, 'a2dp_decode', args.config, 'bluecaster_decode.exe')
    for path, hint in ((VENV_PYTHON, 'run: python tools/emu/setup_env.py'),
                       (args.bluecaster, 'build BlueCaster'), (args.decoder, 'build bluecaster_decode')):
        voice_decodes = args.voice is not None and any(c.endswith('-roundtrip') for c in args.voice or VOICE_CASES)
        if path == args.decoder and (args.scan is not None or args.link is not None or args.le_pairing is not None
                                     or args.hid is not None or args.auracast is not None
                                     or (args.voice is not None and not voice_decodes)):
            continue  # a scan or HID case streams nothing, a link or voice case decodes nothing (but a round trip)
        if not os.path.exists(path):
            sys.exit(f'Missing {path} ({hint})')
    if args.le_audio is not None:
        main_le(args)
    if args.auracast is not None:
        main_auracast(args)
    if args.scan is not None:
        main_scan(args)
    if args.link is not None:
        main_link(args)
    if args.voice is not None:
        main_voice(args)
    if args.le_pairing is not None:
        main_le_pairing(args)
    if args.hid is not None:
        main_hid(args)

    results = []
    for index, codec in enumerate(args.codecs):
        print(f'--- {codec} ...', flush=True)
        failures, detail = run_codec(args, codec, args.base_port + index)
        results.append((codec, failures))
        print(f'    {"PASS" if not failures else "FAIL"}  {detail}')
        for failure in failures:
            print(f'      - {failure}')

    passed = sum(1 for _, f in results if not f)
    print(f'\n{passed}/{len(results)} codecs passed. Details: {args.out}')
    sys.exit(0 if passed == len(results) else 1)


if __name__ == '__main__':
    main()
