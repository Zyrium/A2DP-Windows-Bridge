"""LE Audio pieces of the virtual sink (tools/emu/emu_le_audio_sink.py).

The sink side of each LE Audio profile is a module here, in the order of
MODULES. A module may define any of:

    add_arguments(parser)       its command-line options (prefix --<name>-)
    configure_sink(sink, args)  before power on, for every emulated sink
                                (sink.SinkContext): add GATT services with
                                sink.device.add_service(), AD structures to
                                sink.adv_structures, keep state in sink.state
    async start(sink, args)     after the sink is powered on and advertising

bap.py is the unicast server (PACS, ASCS, CAS) every sink has (mic.py its
source ASE with --mic); csip, vcp,
mcp, ccp and tmap_gmap are stubs for the profile work packages, each owned by
one of them. emu_le_audio_sink.py calls the modules; add checks for
run_test.py in your module's own files, not in bap.py.
"""

from . import bap, bas, ccp, controller, csip, gatt_cache, mcp, mic, tmap_gmap, vcp

MODULES = [bap, mic, csip, vcp, mcp, ccp, tmap_gmap, bas, gatt_cache]


def add_arguments(parser):
    # The emulated controllers themselves (controller.py)
    parser.add_argument('--no-iso-buffers', action='store_true',
                        help="BlueCaster's controller has LE but no ISO buffers (LE Read Buffer Size v2: 0)")
    parser.add_argument('--refuse-cis', type=int, metavar='N',
                        help="BlueCaster's controller refuses the Nth CIS of LE Create CIS (0: the first) "
                             'with 0x1E; the others come up')
    parser.add_argument('--cis-up-after', type=float, metavar='SECONDS',
                        help='with --refuse-cis: the other CIS come up this much later, '
                             "on BlueCaster's side only")
    parser.add_argument('--reencrypt-fail', choices=['drop', 'key-missing'],
                        help="the first re-encryption with BlueCaster's stored key fails once: the sink ends the "
                             'link (drop) or has no key (key-missing, 0x06)')
    parser.add_argument('--iso-buffers', type=int, metavar='N',
                        help="BlueCaster's controller reports N ISO buffers (LE Read Buffer Size v2)")
    parser.add_argument('--completion-delay-ms', type=float, metavar='MS',
                        help="BlueCaster's controller sends Number Of Completed Packets MS after delivering "
                             'an SDU (and reports the matching flush timeout)')
    parser.add_argument('--withhold-completions', type=int, metavar='N',
                        help="BlueCaster's controller never completes the SDUs of the Nth CIS of LE Create CIS "
                             '(0: the first) while it is up')
    for module in MODULES:
        if hasattr(module, 'add_arguments'):
            module.add_arguments(parser)


def configure_sink(sink, args):
    if args.no_iso_buffers:
        controller.NO_ISO_BUFFERS.add('BlueCaster')   # the controller served over H4 (emu_le_audio_sink.py)
    if args.refuse_cis is not None:
        controller.REFUSE_CIS = (args.refuse_cis, args.cis_up_after)
    controller.REENCRYPT_FAIL = args.reencrypt_fail
    if args.iso_buffers is not None:
        controller.ISO_BUFFERS = args.iso_buffers
    if args.completion_delay_ms:
        controller.COMPLETION_DELAY_S = args.completion_delay_ms / 1000
    if args.withhold_completions is not None:
        controller.WITHHOLD_CIS = args.withhold_completions
    for module in MODULES:
        if hasattr(module, 'configure_sink'):
            module.configure_sink(sink, args)


async def start(sink, args):
    for module in MODULES:
        if hasattr(module, 'start'):
            await module.start(sink, args)
