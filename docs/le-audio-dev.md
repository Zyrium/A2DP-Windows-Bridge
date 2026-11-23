---
title: LE Audio (developer note)
layout: default
nav_exclude: true
english_only: true
---

# LE Audio (experimental) - developer note

BlueCaster can stream to LE Audio devices as a **BAP unicast client** (audio
source): LC3 over Connected Isochronous Streams (CIS), on the same BTstack
session and WinUSB adapter as A2DP. It is **experimental**: it is used only
for a device's LE Audio entry (or `--le-audio` in CLI mode), and since
issue #86 only in developer mode (`Settings::le_audio`: the setting and
`debug_mode`; the Adapters switch is shown only there). `le_audio_experimental`
in `settings.json` (on by default, no switch in the window) turns LE Audio
off altogether. With the setting off, only SM and the bonds file are set up,
to hold the LE keys of classic devices (see "LE Audio off"). LE Audio has only
been verified against the Bumble emulator (`tools/emu`), not yet with real
headphones or a real 5.2 controller.

The code was ported from v1 (C++, branch `feat/le-audio`, commit `b819893`)
on 2026-10-18, keeping v1's behaviour, constants and log lines; the emulator's
checks parse those lines, so do not reword them. The v1 sources remain in git
for reference (`git show b819893:app/src/le_audio/<file>`).

## Where the code is

| Path | What |
|------|------|
| `app/src/crates/le-audio/` | Pure logic, no BTstack, `#![forbid(unsafe_code)]`: `defs` (UUIDs, LTV types, contexts, locations, ASE states), `ltv`, `pacs_ascs` (PAC records, ASE values, Control Point PDUs), `presets` (BAP / GMAP configurations), `plan` (`plan_stream`: preset, ASEs, CIS, channels; the source direction of a conversational stream), `csip_crypto` (AES-128, AES-CMAC, s1 / k1 / sef / sdf / sih, RSI), `vcp_scale`, `ccp_model` (call model, simulation), `mcp_model` (media state, `SimulatedPlayer`), `db_hash` (Database Hash), `sdu_queue` (jitter buffer, send guard, counters and their log lines) |
| `app/src/crates/lc3-sys/` | liblc3 (`extern/btstack/3rd-party/lc3-google`, Apache-2.0) built with `cc`; `Lc3Encoder` |
| `app/src/crates/codec/src/lc3.rs` | LC3 behind the `codec` crate's `lc3` feature (not a `CodecKind`) |
| `app/src/crates/btstack-sys/` | `btstack_config.h` LE block (`ENABLE_BLE`, LE central / peripheral, LE Secure Connections with micro-ecc, extended advertising, `ENABLE_LE_ISOCHRONOUS_STREAMS`, `MAX_NR_CIS 4`, `HCI_ISO_PAYLOAD_SIZE 400`, `NVM_NUM_DEVICE_DB_ENTRIES 64` LE bonds, `NVN_NUM_GATT_SERVER_CCC` for all 23 CCCs of each); `build.rs` adds `src/ble` and micro-ecc; `src/le.rs` (Stack's LE API: scan, connect, SM, LE device DB, TLV through a `TlvStore` (bt-host's `le_bond_file::TlvFile`, encrypted at rest), GATT client, att_db_util, att_server, CIG / CIS, ISO send), `src/le_bonds.rs` (what the TLV keeps beside a bond, and a full bond list: the oldest bond not in use goes, with a warning), `src/att.rs` (ATT read / write callbacks, notification queue), `src/events.rs` (event readers), `src/transport.rs` (LE hidden, below), `src/timer_resolution.rs` (`HighTimerResolution`) |
| `app/src/crates/hci-transport/src/usb.rs` | HCI ISO data on the ACL bulk endpoints (OUT: the bulk ACL slot; IN: passed up as ACL, `hci.c` reclassifies by CIS handle), as in v1 and Linux btusb |
| `app/src/crates/bt-host/src/le/` | The LE core: `mod.rs` (public types), `api.rs` (`BtHost::le_*`, worker side), `core.rs` (`LeCore`, `LeCtx`, scan / connect / SM / discovery, module dispatch), `gatt_client.rs` (per-connection queue), `gatt_server.rs` (the PC's GATT server), `bap.rs` (PACS / ASCS client, `Unicast` state machine), `iso_send.rs` (`IsoSender`), `iso_recv.rs` (ISO data of source ASEs: parser, `IsoRx`, `LeRxHandle`), `module.rs` (hook traits) |
| `app/src/crates/bt-host/src/le/modules/` | The profiles: `csip.rs`, `vcp.rs`, `mcp.rs`, `ccp.rs`, `tmap_gmap.rs`, `bas.rs`; `mod.rs` registers them (v1's order: CSIP, VCP, MCP, CCP, TMAP / GMAP, then BAS) |
| `app/src/crates/pipeline/src/le.rs` | The LE encode thread: capture -> stereo downmix -> linear resampling to the LC3 rate -> LC3 per channel -> SDU set per frame into the `LeSduSink` |
| `app/src/crates/pipeline/src/le_rx.rs` | The headset microphone: the "le-decode" thread (jitter buffer by sequence number, LC3 decoding with PLC, mono mix) and its output trait `PcmOut` |
| `app/src/crates/core/src/le_stream.rs` | The session of an LE Audio profile (v1 `a2dp_service_le_audio.cpp`): connect, start, VolumeSync, re-setup after a loss (3 times), the `error.le_audio_*` messages; the headset microphone (`LeMicPath`) into the output that `voice.rs` resolves |
| `app/src/crates/session-host/src/host/media/smtc.rs` | MCP's Windows backend (System Media Transport Controls, media keys); `host/media.rs` runs it in the session agent with `--session-agent` |
| `app/src/crates/core/src/service.rs` | `HostConfig.le_audio` from the setting or the CLI, the deferred toggle, `scan_devices` (classic + LE entries; also the CLI's `--scan` with `--le-audio`), `le_stream()` |
| `app/src/crates/core/src/devices.rs` | `le_audio_devices.txt` (`load_le` / `save_le` / `remove_le`) |
| `app/src/core/src/cli.rs` | `--le-audio`, `--le-device`, `--le-preset`, `--le-low-latency`, `--le-scan`, `--le-sim-volume`, `--mcp-dry-run`, `--ccp-sim`, `--ccp-console` (v1 `le_audio_cli.cpp`'s flow and lines); `--headset-mic`, `--mic-output` (the microphone) |
| `app/src/core/src/state.rs`, `resident.rs` | The UI's state: LE settings, LE devices, `le_scanning`, `StreamInfo.le`, `LinkSample.iso`, `settings.set`'s `le_audio_deferred`; diagnostics section `[LE Audio (experimental)]` (`crates/core/src/diag.rs`) |
| `app/src/ui/src/` | The Codecs screen's LC3 and LC3 LL cards (LC3 LL = the profile's `le_low_latency`), DevicePicker's LE tags, Diagnostics' ISO counters; `?mock=le` mocks LE |
| `tools/a2dp_decode/le_audio_decode.cpp` | `bluecaster_decode --le-audio` |
| `tools/emu/emu_le_audio_sink.py`, `tools/emu/le_audio/` | Bumble LE Audio sink(s); `le_audio/<profile>.py` = the sink side of each profile; `le_audio/mic.py` + `mic_frames.py` = the headset microphone (`--mic`) |
| `tools/emu/le_audio_tests/` | `run_test.py --le-audio` cases and checks, one file per profile (`core.py`, `privacy.py`, `csip.py`, `vcp.py`, `mcp.py`, `ccp.py`, `tmap_gmap.py`, `bas.py`, `mic.py`, `gatt_cache.py`) |

Files in the config folder (the formats are the same as in v1; the paths are
listed in [Settings and Files](architecture#settings-and-files)):
`pairing\le_audio_bonds.tlv` (LE bonds and the GATT server's persistent CCC
values), `pairing\le_audio_csip.txt` (coordinated sets),
`pairing\le_gatt_cache.txt` (each bonded device's GATT services with its
Database Hash, `le/gatt_cache.rs`; also what the Services tag lists),
`devices\le_audio_devices.txt` (LE Audio entries of the device list).

## Threads - the one rule that matters

BTstack is single-threaded. Everything that touches BTstack, an `LeDevice`,
the GATT client or the GATT server runs on the **BTstack thread**, through an
`LeCtx`:

- `LeModule` hooks (all of them), GATT client callbacks (`GattValue`,
  `GattDone`, `GattNotify`, `GattDiscovered`), timers (`LeCtx::run_after`)
  and posted closures get `&mut LeCtx`. They must not block. A callback
  reaches its module with `ctx.module_mut::<XModule>()` (this returns None for
  the module whose hook is running, because the core takes a module out of its
  registry while its hook runs).
- The worker side (`BtHost::le_*`, `LeGroupHooks::prepare_group` /
  `adjust_stream_request`) blocks with timeouts and reaches the BTstack
  thread with `BtHost::le_run(timeout, f)` (run and wait for the result) or
  `BtHost::le_call(timeout, f(ctx, done))` (`f` calls `done(ok)` later, e.g.
  from a GATT callback). A user stop ends these waits (the host's cancel),
  except the teardown ones (`le_stop_stream`, `le_disconnect(_all)`).
- From any thread: `LePoster::post(f)` (the module gets its poster in
  `LeModuleSetup`); `LeGroupHooks::remote_volume` / `request_volume` keep
  their state in atomics or a mutex and post the GATT work.
- Never call `le_run` / `le_call` or the blocking `BtHost::le_*` from the
  BTstack thread: they wait for that thread and would time out.
- Closures for `le_run` must capture only owned data, because they may still
  run after a timeout.
- `LeCore::run` "settles" after every entry: it starts queued GATT operations,
  runs finished callbacks (never inside the call that queued them), passes
  GATT server notifications to `att_server_send` and arms the LE timer.

## Lifecycle

1. `HostConfig.le_audio = Some(LeConfig { bond_dir, media, ccp_sim,
   ccp_console })` (the service sets it from the setting, or from `--le-audio`;
   None = no LE Audio, in which case `HostConfig.le_keys` sets up only SM and
   the bonds file). LE is unhidden before `hci_init`.
2. Stack init (BTstack thread, after the classic endpoints): LE bond TLV,
   `le_device_db_tlv`, SM (Secure Connections, bonding, Just Works), GATT
   client, HCI / SM / ISO handlers, the modules (`modules::create_all`, once
   per BtHost), **`on_stack_init`**, the GATT server (GAP, GATT with Service
   Changed and Database Hash, **`add_gatt_services`**), `att_server_init`.
3. Once HCI is working: `le_ready()`; ISO buffers (LE Read Buffer Size v2) in
   `le_iso_buffers()`. If either is 0, the adapter has no ISO support and
   `le_start_stream` refuses ("adapter does not support LE Audio (no ISO
   buffers)"): BTstack's ISO send would loop forever.
4. `le_connect(addr, name, timeout)`: `gap_connect` -> **`on_connected`** ->
   SM pairing / re-encryption (if the device lost the bond, the bond is
   deleted and pairing is repeated once) -> **`on_encrypted`** -> GATT discovery into
   `LeDevice::services` (skipped when the device's Database Hash matches the
   one cached with its services in `le_gatt_cache.txt`, or when the device has
   no usable Database Hash but has Service Changed; a Service Changed
   indication or a failed setup drops the cached list) -> PACS / ASCS reads
   and notifications ->
   **`on_gatt_ready`** -> the device id. On a timeout or cancel, the connect
   is cancelled; a link that still comes up afterwards is disconnected.
5. `le_start_stream(devices, request, silence)`: **`prepare_group`** (worker)
   -> **`adjust_stream_request`** (worker) -> `plan_stream` -> `Unicast`:
   Config Codec -> LE Set CIG Parameters -> Config QoS -> Enable (contexts +
   CCID list; waits for an available context) -> LE Create CIS (all CIS at
   once) -> sink ASEs go Streaming (source ASEs: Receiver Start Ready, see
   "Headset microphone") -> `IsoSender` starts ->
   **`on_stream_state(status, true)`**. Returns the `LeSduSink` that the
   encode thread pushes to, and `rx` (the microphone's SDUs) when the plan has a
   source direction.
6. `le_stop_stream()`: Disable -> HCI_Disconnect of the CIS -> Release ->
   remove the CIG -> **`on_stream_state(status, false)`**. Devices stay
   connected (`le_disconnect_all()` drops them).
7. A link or CIS lost while streaming: `le_stream_lost()` becomes true once;
   the session stops and sets up the stream again (3 tries).
   **`on_disconnected(dev, reason)`** is called for every LE link that goes down.
8. Stack deinit (run loop stopped): **`on_stack_deinit`**, no BTstack calls.
   Device ids restart from 0 with the next BtHost.

## Module hooks (`le/module.rs`)

A module file has `pub fn create(setup: &mut LeModuleSetup) -> LeModuleEntry`
returning its `LeModule` and, if it has worker / volume hooks, an
`Arc<dyn LeGroupHooks>` sharing state with it.

| Hook | Thread | When / what for |
|------|--------|-----------------|
| `name()` | - | log name |
| `on_stack_init(ctx)` | BTstack | session start, before HCI power on |
| `add_gatt_services(builder)` | BTstack | the PC-side services of the profile (only here) |
| `on_stack_deinit()` | BTstack (loop stopped) | drop all state |
| `on_scan_result(ctx, result, ad)` | BTstack | every advertising report while a scan / find runs; may set `le_audio`, `set_id`, `note` |
| `on_connected` / `on_encrypted` / `on_gatt_ready(ctx, id)` | BTstack | link up / encrypted / services and PACS / ASCS read (queue your reads and subscriptions here) |
| `on_disconnected(ctx, id, reason)` | BTstack | link down; the device still has its last data |
| `on_stream_state(ctx, status, streaming)` | BTstack | audio flows (true) / stopped or lost (false) |
| `add_ccids(contexts, ccids)` | BTstack | CCIDs for the Enable metadata (MCP: `CCID_GMCS` for every context but pure Conversational; CCP: `CCID_GTBS` for Conversational) |
| `on_att_read(ctx, read)` / `on_att_write(ctx, write)` | BTstack | reads / writes of this module's characteristics (whole value; the server serves offsets and assembles prepared writes) |
| `prepare_group(host, ids, cancelled)` | **worker** | CSIP: append the other set members (may scan / connect) |
| `adjust_stream_request(host, ids, request)` | **worker** | GMAP: low-latency presets first, the Game context |
| `remote_volume()` | any | headset volume 0-127, None unknown; the first `Some` wins |
| `request_volume(level)` | any | set the headset volume; true if handled |

`LeDevice` (`le/core.rs`): `id`, `addr` (the identity once known),
`peer_addr` (the address of the current link, maybe an RPA), `name`, `acl`,
`encrypted`, `ready`, `services` (`find_service`, `find_char`,
`find_chars`; secondary / included services have `primary == false` and
`included_by`), PACS (`sink_pacs`, `sink_locations`, available / supported
contexts, `source_pacs`, `source_locations`), ASCS (`ases`, `ase_cp_handle`). In
callbacks, check that the device is still on the same link
(`LeCtx::device_on(id, con)`), as the BAP client does.

### Addresses and privacy

Devices are keyed by identity: `addr` becomes the identity on SM's identity
events (pairing, or a reconnect with a resolvable private address); a device
keeps its id across reconnects with new RPAs. While scanning, BlueCaster looks
up every RPA (`sm_address_resolution_lookup`); `LeScanResult::identity` holds
the result. `le_find(addr)` matches the address, an advertiser whose RPA
resolves to it, or one resolving to the same identity as a saved RPA;
`le_connect` uses the RPA the device was last seen with (within 5 minutes).
Resolution is done in software only (no controller resolving list), so it
works with any 5.2 controller. Not covered: RPAs without a bond, RSIs that change with the
RPA. Emulator case `privacy-rpa`.

## GATT client: always through the queue

BTstack's `gatt_client` allows one query per connection. Never call it
directly: use `ctx.gatt()` (`le/gatt_client.rs`):

```rust
fn on_gatt_ready(&mut self, ctx: &mut LeCtx<'_>, id: LeDeviceId) {
    let Some(dev) = ctx.device(id) else { return };
    let Some(state) = dev.find_char(UUID_SERVICE_VCS, UUID_VOLUME_STATE).cloned() else { return };
    let con = dev.acl;
    ctx.gatt().listen(con, state.value_handle, Box::new(move |ctx, value| { /* notifications */ }));
    ctx.gatt().read(con, state.value_handle, Box::new(move |ctx, status, value| {
        if let Some(m) = ctx.module_mut::<VcpModule>() { /* status 0: value */ }
    }));
    ctx.gatt().subscribe(con, &state, false, None);
}
```

- `read` (long reads included), `write` (with response; a long write when
  the value does not fit the MTU), `subscribe` (writes the CCC),
  `discover_all`, `listen`.
- Operations run in order per connection; callbacks get status 0 or an ATT
  error / BTstack status; when the link goes down, everything pending
  completes with `ATT_ERROR_HCI_DISCONNECT_RECEIVED` (0x1F) and listeners are
  dropped. A busy `gatt_client` (BTstack's own MTU exchange) is retried.

## GATT server: services of the PC

Only in `add_gatt_services(builder)` (the database is fixed once att_server
runs): `begin_service(AttUuid, primary)`, `add_characteristic(uuid,
properties, read_security, write_security)` -> value handle (CCC at + 1 with
NOTIFY / INDICATE), `end_service()`. Reads and writes come to the module's
`on_att_read` / `on_att_write` synchronously (btstack-sys's ATT callbacks
reach the app through a thread-local handler; a request that arrives while the
app is busy is answered "pending" and BTstack repeats it).

- `ctx.gatt_server().notify(con, handle, value)` / `indicate(...)` send only
  to clients that enabled them (`client_config(con, handle)`), queue until ATT
  can send, and are truncated to ATT_MTU - 3 by btstack-sys (modules that must
  truncate at a UTF-8 boundary or per the spec do so themselves: MCP, CCP).
- CCC values of **bonded** clients persist in `le_audio_bonds.tlv` (BTstack's
  `BTC` tags) and are replayed when the link is encrypted again (emulator
  case `mcp-resume`).
- The GATT service has Service Changed and a **Database Hash** (0x2B2A,
  AES-CMAC over the DB, `le_audio::db_hash`; `mcp-read` checks it). When the
  stored hash (`BTDB`) differs, the stored CCC values are dropped and bonded
  clients with Service Changed indications enabled receive one (0x0001-0xFFFF)
  on their next encrypted link (`AWS` tag until then). No Robust Caching.
- Prepared writes are executed for all handles of the connection (BTstack
  passes handle 0 to EXECUTE; v1 dropped them).

## Stream configuration

`PresetTable::new()` (`le_audio::presets`) holds BAP Table 5.2 by name
(`16_1_1` ... `48_6_2`), GMAP's `_gr` presets and, for a gaming headset's
microphone, GMAP's `_gs` presets (`32_1_gs` ... `48_2_gs`, presentation delay
60 ms; written from memory, see the TODO). `StreamRequest::preset_order`
(default `48_6_2, 48_4_2, 48_2_2, 32_2_2, 24_2_2, 16_2_2`) is tried in order;
the first preset that every device's sink PAC records allow and whose SDU fits
in one HCI ISO data packet (SDU + 4 <= ISO_Data_Packet_Length) wins.

Channels: a device with Front Left + Right gets one 2-channel ASE when its PAC
allows 2 channels and the SDU fits, otherwise two 1-channel ASEs, otherwise
one mono ASE; a device with one location gets one ASE for that location; no
location = mono. Left / right audio go to FL / FR only when the group has both;
otherwise every channel carries the downmix. One CIG (id 1, or another id
while BTstack still holds id 1), CIS in plan order, at most `MAX_CIS` (4). The
presentation delay is clamped to every ASE's range, and the max transport
latency is capped by the servers' preference; framed ISO is used when an ASE
cannot do unframed.

Developer > Debug > LE Audio stream (developer mode, read when the next
stream is set up; issue #86) overrides some of these values, to try how a
controller behaves with other values without a new build:

- max transport latency C->P / P->C and RTN C->P / P->C
  (`bt_host::CigOverrides`, `bap.rs` `apply_overrides`; Set CIG Parameters
  and Config QoS get the same values, no server preference caps them,
  clamped to 5-4000 ms and RTN 0-15), interleaved packing;
- the microphone preset (`StreamRequest::source_preset_exact`: no
  high-reliability twin, no GMAP order);
- the retry without the microphone on the same link instead of a new one
  (`le_stream.rs`).

The log lists them ("LE Audio: Developer > Debug CIG: ...", "LE Audio:
Developer > Debug: ..."), and the diagnostics' settings.json includes them. The
ISO interval, NSE, BN and FT are the controller's choice: LE Set CIG
Parameters Test would set them, but BTstack's CIG handling (`gap_cig_create`)
sends only Set CIG Parameters, and extern/btstack is not patched.

Streaming Audio Contexts (Enable): the requested contexts that are available
on every device, otherwise Unspecified if available; they are evaluated again
right before Enable, which waits for an Available Audio Contexts notification
when nothing is available ("no audio context available for Enable"; with a
microphone also for an available source context). The CCID
list comes from the modules' `add_ccids` (log: "Enable metadata: contexts
0x0004, CCID list 1").

## Headset microphone (source ASEs)

`LeRequest::headset_mic` (`--headset-mic` on an `--le-audio` run) sets
`StreamRequest::conversational`. `plan_stream` keeps the sink layout and
preset (C->P) and adds a source direction (P->C) to the CIS plan
(`CisPlan::source_ase: Option<SourceDir>`, `StreamPlan::source`):

- every device with a source ASE gets one mono source direction (its first
  source ASE, with the Source Audio Location if there is exactly one,
  otherwise mono)
  on its first sink CIS; a set member with only source ASEs gets its own CIS
  that carries P->C only (it gets no SDUs from the encode thread and is not
  sent on)
- the source preset: the first of `source_preset_order` (default `32_2_1,
  32_1_1, 24_2_1, 24_1_1, 16_2_1, 16_1_1`; those with the sink preset's frame
  duration first; with a high-reliability sink preset, the high-reliability
  preset of the same rate, frame and octets instead of a low-latency one)
  that every such device's Source PAC records allow;
  GMAP low latency with UGTs that all have the Source feature puts the `_gs`
  presets first; `source_rate` (the rate chosen, `--mic-rate`) takes the first
  of those at that rate, or the first at any rate if there is none;
  `SourcePlan::rates` lists the rates that fit, Auto's first
- source contexts: Conversational (GMAP: Game + Conversational), chosen
  from the Available Source Contexts; the sink ASEs also ask for
  Conversational alongside Media where available (CCP's `add_ccids` then adds
  GTBS's CCID)
- no source ASE, no Source PAC match or no CIS left: the plan has no source
  direction and `source_note` gives the reason; playback continues ("LE Audio:
  streaming without the microphone: ...")

BAP: Config Codec / QoS / Enable carry both kinds of ASE; LE Set CIG
Parameters gets `max_sdu_p_to_c`, `rtn_p_to_c`, the P->C SDU interval and
latency (BTstack then sets up the controller -> host data path); once
all CIS are up the client (the receiver) writes Receiver Start Ready for the
source ASEs and starts receiving; `IsoSender` gets only the CIS with a sink
ASE. Stopping: Disable puts a source ASE in Disabling, the client writes
Receiver Stop Ready, the ASE goes to QoS Configured, then Release as before.

Receiving (`le/iso_recv.rs`): the ISO handler gets HCI ISO data as
HCI_ISO_DATA_PACKET (routed on the packet type first: a data packet's first
byte is the handle's low byte); `IsoRx` parses each packet (time stamp if flagged,
sequence number, SDU length, packet status) and pushes `RxSdu`s (fixed size)
into an `rtrb` ring; `LeStreamStarted::rx` (`LeRxHandle`) is the consumer.
`pipeline::le_rx` ("le-decode" thread, polled every 2 ms): per source CIS a
jitter buffer of 16 sequence numbers that releases an SDU once two newer ones
have arrived (out-of-order SDUs are put back in order; liblc3's PLC conceals a
missing or invalid one; a jump past the buffer resets it), one LC3 decoder per
channel, all channels of all streams mixed to
mono, frame by frame into a `PcmOut`. No allocation per frame. At the
end: "LE Audio: microphone: N frames out (D decoded, C concealed), S SDUs,
L late, R resyncs, E errors".

The output (core `le_stream.rs`, `LeMicPath`): the `PcmOut` pushes into the
queue of voice-link's `Renderer` (the render endpoint the request names,
opened at the source rate, 40 ms prefill; its drift control absorbs the
headset's clock difference, since the decode thread has no clock) or `WavSink`
(`--mic-output
x.wav`; a stream set up again writes `x-2.wav`, ...). The output is
resolved and checked as for a classic device (`crates/core/src/voice.rs`:
`resolve_mic_output`, `mic_conflict`) before the stream asks for the
conversational configuration: with no output device, or one that the stream
captures (feedback loop), the stream plays without the microphone, with
`status.headset_mic_output_failed` / `error.headset_mic_loop` as the
status. While the microphone runs, the session reports `VoiceStatus { codec: "lc3",
link: "le", rate, mic_device_id }` (`StreamStatus::voice`), and None when it
stops. A profile's request gets `LeRequest::headset_mic` / `mic_output`
from the same decision as a classic one (developer mode, the profile's
switch, an output chosen); changing them restarts the LE stream.

## Audio path

`le_stream.rs` requests the plan's LC3 rate from the capture; `pipeline::le`
downmixes any PCM to stereo, resamples linearly if needed, encodes LC3 per
channel and pushes one SDU set per frame (10 or 7.5 ms) into the jitter
buffer (`le_audio::sdu_queue::SduQueue`: prefill queue + 3 sets, one more per
underrun up to queue + 7, 16 at most, dropping the oldest beyond that). No
allocation per frame: sets are recycled through the queue's pool.

The BTstack thread sends unicast audio paced by the SDU interval
(`le/iso_send.rs`, run loop timer `TIMER_ISO`), 2 intervals ahead of real
time. Each interval it takes the next set and gives every CIS its SDU with
that interval's sequence number: complete SDUs, **no time stamp**, and the
number counts SDU intervals, so a dropped SDU leaves a gap. Each CIS then
sends on its own while it has fewer SDUs in the controller than its depth,
counted from our sends and Number Of Completed Packets (both HCI events
reach `IsoSender::on_hci_event` through `LeCore::on_hci`).

- BTstack's CIS can-send-now is not used for sending (only to learn that the
  CIG is active): `hci_iso_notify_can_send_now` grants the CIS of a CIG in
  lockstep and stops at the first CIS whose queue is full ("wait for next
  round"), so one CIS whose completions came late or never stopped the
  others. `bluecaster_iso_send` (shim.c) sends without it; BTstack itself
  does not check controller space for ISO.
- Depth per CIS: its share of the controller's ISO buffers (at most 16),
  narrowed to what the flush timeout of LE CIS Established needs (FT x ISO
  interval in SDU intervals + 5, at least 4). A controller holds a PDU the
  peer did not acknowledge until its flush point, so with FT 10 an SDU can
  stay ~100 ms. MediaTek controllers keep 12 or Developer > Debug "ISO
  packets queued per CIS".
- A CIS without room keeps its SDUs waiting and sends them once completions
  make room. An SDU is dropped ("late") only when its interval plus a wait
  budget has passed: the CIG's max transport latency C->P, 40 to 200 ms
  (100 ms if unknown). Completions held up for a few intervals, as a
  controller does with a PDU the peer did not acknowledge or the emulator
  during a 50-100 ms pause, cost no audio; a CIS that never completes keeps
  at most budget / interval + 3 SDUs waiting. A late run loop timer makes
  the missed intervals at once, skipping only those already past the
  budget. BTstack's single HCI packet buffer is often taken (the
  previous ISO packet, a GATT write): the send is refused with 0x57
  (`BTSTACK_ACL_BUFFERS_FULL`) and the same SDU, same sequence number, goes
  again on `HCI_EVENT_TRANSPORT_PACKET_SENT`.
- Watchdog: a CIS without a completion for 2 s while SDUs wait in the
  controller is stalled ("CIS 0x....: no SDU completed for 2 s"). All CIS
  stalled -> the stream is reported lost and set up again; some -> the
  others play on, `LeStreamStats::cis_stalled` and `status.le_no_audio`. A
  CIS stalled from the start has the session set the stream up again once.
- Logs: every LE CIS Established ("CIS 0x.... parameters: NSE, BN, FT, max
  PDU, ISO interval, transport latency, PHY"), every 10 s "LE stream health"
  (per CIS: SDUs/s, late, completion delay p50 / p90, queue peak), and at the
  end each CIS's counters before "LE Audio: stopped sending: ...". The
  counters are published about once a second, so the live bit rate is what
  was really sent.

Underrun -> encoded silence ("LE Audio: no audio for SDU set N ...").
While sending, the system timer resolution is 1 ms
(`btstack_sys::HighTimerResolution`, like v1's `timeBeginPeriod(1)`);
otherwise BTstack's Windows run loop and its clock move in 15.6 ms steps.
A broadcast (BIG) is still sent in rounds on `HCI_EVENT_BIS_CAN_SEND_NOW`,
with a send guard keeping the rounds at most queue + 2 intervals ahead. A
BIS send refused with 0x57 (the packet buffer taken, typically for the
second BIS of the first round) waits in the round and goes again with the
same SDU and sequence number on `HCI_EVENT_TRANSPORT_PACKET_SENT`. A stop
while a round is only partly sent lets that round finish first
(`IsoSender::finish_round`, 2 intervals at most, no further round), so every
BIS ends on the same SDU; LE Terminate BIG follows once the round is out or
the time has run out.

The UI's ISO counters (`LinkSample.iso`) come from `BtHost::le_stream_stats()`
(sets, silence, underruns, dropped, prefill).

## Emulator (`tools/emu`)

`emu_le_audio_sink.py --port P --capture sink.pklg --layout L` runs BlueCaster's
controller (H4 over TCP) and one or two sinks on a Bumble `LocalLink`:

| Layout | Sinks |
|--------|-------|
| `stereo` | 1 sink `00:A2:D0:00:00:12`, FL+FR, 2 sink ASEs, 1 or 2 channels per ASE |
| `two-ase` | 1 sink, FL+FR, 2 sink ASEs of 1 channel |
| `earbuds` | 2 sinks (`...:12` FL, `...:13` FR), 1 ASE each |
| `mono` | 1 sink, no location, 1 ASE |

It prints `READY 127.0.0.1:P sinks=<addr>[,<addr>]`. Options:
`--ascs-release-waits-for-cis`, `--drop-link-after S`, `--privacy`,
`--no-iso-buffers`, `--refuse-cis N` / `--cis-up-after S` (the controller
refuses the Nth CIS of LE Create CIS with 0x1E; the others come up, with
`--cis-up-after` only S seconds later), `--iso-buffers N` (ISO buffers of
BlueCaster's controller), `--completion-delay-ms MS` (Number Of Completed
Packets MS after delivery, with the matching FT in LE CIS Established),
`--withhold-completions N` (the Nth CIS never completes while up),
`--reject-enable` (every Enable is rejected with 0x0E after the CIG is set),
`--mic [SINKS]` / `--mic-rates` (a
microphone: Source PAC 16 / 32 kHz 10 ms, a source ASE, a 1 kHz tone sent
from pre-encoded LC3 frames in `le_audio/mic_frames.py` once BlueCaster has
written Receiver Start Ready), and each profile's own options (`--csip-set`,
`--vcp`, `--mcp-client`,
`--ccp`, `--tmap-role`, `--gmap-role`, ...; see each `le_audio/<profile>.py`).

Emulator facts to know:
- Legacy advertising, 31 bytes: the name gets whatever space flags + ASCS +
  CAS service data (and a CSIP RSI) leave.
- `LeEmuController` (`le_audio/controller.py`) carries ISO data between the
  controllers, one SDU per interval per CIS, both ways (P->C with the CIG's
  P->C interval; a late SDU from the emulated headset's own sender is logged
  as "late microphone SDU" and does not fail a case). An SDU that arrives more than
  2 ms after its interval began is logged as `late SDU` and fails the case (a
  real controller's queue would have run empty); stalls in
  the emulator's own event loop are not counted. The emulator runs asyncio on
  the performance counter with 1 ms timers.
- Every HCI command gets an answer (unknown ones: Unknown HCI Command).
- Bumble's ASCS goes to Streaming right after the CIS is up; pairing is Just
  Works with an in-memory key store; the controller does not check LTKs, so
  the "device lost its bond" path is not covered.
- When an LE link drops, the CISes are reported down first, as real
  controllers do.
- The sink writes UTF-8 (`run_test.py` sets `PYTHONIOENCODING` / `PYTHONUTF8`
  for it; on Japanese Windows it would otherwise write cp932).

## Running the LE cases

Once: `python tools/emu/setup_env.py` (Bumble venv in `tools/emu/.venv`).
Then:

```
pwsh ./build.ps1 -Decoder                       # bluecaster_decode -> build/a2dp_decode/Release
cargo build --release -p bluecaster-core-exe        # target/release/bluecaster-core.exe
tools\emu\.venv\Scripts\python.exe tools\emu\run_test.py --le-audio
tools\emu\.venv\Scripts\python.exe tools\emu\run_test.py --le-audio stereo earbuds --base-port 10200 --out $env:TEMP\le
tools\emu\.venv\Scripts\python.exe tools\emu\run_test.py          # the A2DP codecs: must keep passing
```

`--le-audio` without names runs all 51 cases; CI runs them after the A2DP set
(`.github/workflows/ci.yml`, emulator job). Each case gets its own config
folder (`BLUECASTER_CONFIG_DIR=<out>/le-<case>/config`), so nothing touches
`%APPDATA%\BlueCaster`. Parallel runs need distinct `--base-port`s (each case
uses its own port counting from there). For each case, `<out>/le-<case>/`
contains
`bluecaster.log`, `sink.log`, the captures and `sent_report.txt` /
`received_report.txt` from `bluecaster_decode --le-audio`.

| File | Cases |
|------|-------|
| `core.py` | `stereo`, `two-ase`, `earbuds`, `mono-16k`, `link-loss`, `release-cis`, `contexts-later`, `afh`, `no-iso-buffers`, `cis-refused`, `cis-refused-late`, `enable-rejected`, `slow-completions`, `cis-stalled` |
| `privacy.py` | `privacy-rpa` |
| `csip.py` | `csip-set`, `csip-encrypted-sirk`, `csip-missing-member`, `csip-both-named`, `csip-link-loss`, `csip-lock-denied`, `csip-no-lock` |
| `vcp.py` | `vcp-set`, `vcp-remote`, `vcp-change-counter`, `vcp-earbuds`, `vcp-press-during-write`, `vcp-press-before-write` |
| `mcp.py` | `mcp-read`, `mcp-control`, `mcp-unsupported`, `mcp-ccid`, `mcp-long-read`, `mcp-resume` |
| `ccp.py` | `ccp-read`, `ccp-incoming-accept`, `ccp-terminate`, `ccp-invalid` |
| `tmap_gmap.py` | `tmap-role`, `gmap-low-latency`, `gmap-earbuds`, `gmap-off`, `gmap-not-ugt`, `gmap-media-context` |
| `bas.py` | `bas-level`, `bas-earbuds` |
| `mic.py` | `mic-stereo`, `mic-earbuds`, `mic-mono-16k` (run with `--headset-mic --mic-output <out>/le-<case>/mic.wav`: the planned microphone preset, Receiver Start / Stop Ready, the headset's SDUs in BlueCaster's capture, mic.wav's frame count and the 1 kHz tone in every 100 ms) |

Each case checks: BlueCaster exits 0; one stream of the expected configuration
on both sides; no decoder problems (consecutive SDU sequence numbers,
lengths, LC3 decoding); the sinks received every SDU BlueCaster sent; decoded
audio identical on both sides and the test tone (1 kHz left, 1.5 kHz right;
mono: both) in every 100 ms of every stream except a short gap at its start
and end; no `late SDU`; no underrun in BlueCaster's "stopped sending" counters
(one when the tone stops is allowed) and most of the duration sent as audio; each
CIS's channel allocation within the PACS locations of its sink; the
teardown steps in the log; then the checks in every
`le_audio_tests/<profile>.py`. `Case` fields: `le_audio_tests/case.py`.

A case runs `bluecaster-core.exe --cli --hci-tcp 127.0.0.1:<port> --le-audio -d
<first sink> --le-device <other sinks> --test-tone --duration N
--hci-capture ...` plus its `bluecaster_args` (`--hci-tcp` implies
`--mcp-dry-run`: MCP never controls the PC's real players).

Unit tests: `cargo test -p le-audio -p lc3-sys -p bt-host -p pipeline -p
bluecaster-core` (BAP phases, GATT queue and server, ISO rounds, each module
against a scripted peer, the CLI's parsing); btstack-sys's
`tests/le_emulator.rs` is ignored by default and runs against a sink specified in
`BLUECASTER_EMU_LE=host:port`.

## The profiles

**CSIP (Set Coordinator, `csip.rs`).** Reads SIRK (plaintext, or encrypted:
decrypted with sdf and the bond's LTK), size, rank and lock of a CSIS;
devices with equal SIRKs form a set. Resolves RSIs in scan results (cache of
256). `prepare_group` waits up to 6 s for the reads, adds connected members,
scans up to 10 s for the missing ones, connects them one at a time and
orders the set by rank ("CSIP: streaming to coordinated set N, k of n
members: ..."; "only k of n" otherwise). Locks the members in rank order
before the stream (Lock Denied: back-off, 5 attempts, then without the lock)
and releases them in reverse order once the stream is running or has stopped
(after 50 s at the latest). Sets of bonded devices persist in `le_audio_csip.txt`. The
crypto self-check (CSIS appendix A) runs at stack init.

**VCP (Volume Controller, `vcp.rs`).** Volume State read and notifications,
Set Absolute Volume / Unmute with the Change Counter (one retry after 0x80),
one group volume for all connected VCS devices (later members are aligned to
it), button presses during BlueCaster's own write are applied afterwards, 0-127 <->
0-255 with `vcp_scale`. `remote_volume` / `request_volume` feed VolumeSync,
so the profile's volume sync applies as for A2DP. `--le-sim-volume` runs
VolumeSync against a simulated Windows volume in the CLI.

**MCP (GMCS server, `mcp.rs`, Windows side in `session-host/src/host/media/smtc.rs`).**
GMCS with v1's 11 characteristics, CCID 1. The control point goes to the
current SMTC session, otherwise to media keys; with "Control playback with the
audio device's buttons" off, it answers Command Cannot Be Completed.
`--mcp-dry-run` (always with `--hci-tcp`): a
simulated player; commands are only logged ("MCP: dry run: PLAY ...").
With `--session-agent` the player (Windows' or the simulated one) runs in
the session agent: the core answers reads from a copy that the agent's
events update, and sends commands without blocking the BTstack thread. Long
reads of changing text return 0x80; notifications are truncated to ATT_MTU - 3
at a UTF-8 boundary.

**CCP (GTBS server, `ccp.rs`).** A spec-correct GTBS (16 characteristics,
CCID 2) around `ccp_model::CallModel` with the simulation backend: BlueCaster has
no telephony. `--ccp-sim "<t>:<cmd>;..."` runs commands after the first client
subscribes to Call State, `--ccp-console` reads them from stdin.
The notification order follows Zephyr's `tbs.c`. Call indices restart at 1 with each
BTstack session (v1 kept counting).

**TMAP / GMAP (`tmap_gmap.rs`).** The PC serves TMAS (CG + UMS: the
conversational configuration with source ASEs alongside CCP's GTBS) and GMAS
(UGG, features 0x07: Multiplex, 96 kbps Source, Multisink) and reads the headset's roles ("LE Audio: <addr>: TMAP: UMR,
GMAP: UGT (UGT features 0x3C: ...)"; also in the diagnostics). With the
profile's low-latency setting (`le_low_latency` in `profiles.json`, on by
default; the "LC3 LL" card on the Codecs screen; `--le-low-latency` in the
CLI) and a group of UGTs with the Sink feature,
`adjust_stream_request` puts `48_3_gr, 48_4_gr, 48_1_gr, 48_2_gr, 32_1_gr,
32_2_gr` first with the Game context (Media when Game is not available).

**BAS (Battery Service client, `bas.rs`).** Reads each
device's Battery Level and follows its notifications; with a set of earbuds
the lowest level is reported (`HostEvent::Battery`, "BAS: device N: battery
X% (reported Y%)"). Emulator option `--bas L[,L2]`, `--bas-change S:L`.

## Broadcast (Auracast source, developer mode)

BlueCaster can also broadcast instead of streaming to a device: any LE Audio
receiver nearby (earbuds, a speaker, a phone acting as an assistant) can
pick it up. It is started from the Developer screen's Auracast tab (name,
password, high / standard quality, stereo / mono, test tone or system
audio; `stream.start_broadcast`, refused with `developer.off` unless
developer mode is on) or with `--cli --auracast <name> [--auracast-code
<code>] [--auracast-mono | --auracast-split] [--le-preset 24_2_2]`, which
goes through the
same service path (`state::broadcast_request`, `le_stream::run_broadcast`).
It needs LE Audio on (`le_audio_experimental`, else error.le_audio_disabled)
and an adapter with ISO buffers and the Isochronous Broadcaster feature.
Home does not show a running broadcast; starting a device's stream replaces
it.

- `le_audio::broadcast` (pure): the presets (BAP 1.0.1 Table 6.4, high
  reliability, written from memory: RTN 4, 60 ms for 16 / 24 / 32 kHz, 65
  ms for 48 kHz, 10 ms frames, 40 ms presentation delay; "high" = 48_4_2,
  "standard" = 24_2_2), the extended advertising data (Broadcast Audio
  Announcement 0x1852 with a random 24-bit Broadcast_ID, Public Broadcast
  Announcement 0x1856 with the encrypted / standard / high quality bits,
  Broadcast Name 0x30), the BASE (0x1851, periodic advertising: one
  subgroup, LC3, Media; BIS 1 Front Left and BIS 2 Front Right, or one mono
  BIS without a location) and a `StreamPlan` with one entry per BIS, so the
  encode thread and the ISO sender are the same as for unicast.
- btstack-sys: `ENABLE_LE_PERIODIC_ADVERTISING`, `MAX_NR_BIS 2`; the shim
  holds the advertising set, its data and the BIG in static storage
  (`bluecaster_broadcast_advertise`, `bluecaster_big_create`, `bluecaster_big_terminate`,
  `bluecaster_broadcast_advertise_stop`).
- bt-host `le/broadcast.rs`: advertising + LE Create BIG ->
  GAP_SUBEVENT_BIG_CREATED -> `IsoSender::start_big`; stop: the round going
  on finishes, LE Terminate
  BIG -> GAP_SUBEVENT_BIG_TERMINATED -> advertising removed ("Auracast:
  stopped"). A BIG the controller ends counts as a lost stream
  (error.auracast). `BtHost::le_start_broadcast` / `le_stop_broadcast`.
- core `le_stream::run_broadcast`: no device; capture as for unicast. An
  adapter that stops working under the broadcast (reset, unplugged, sleep:
  `BtHost::stalled`, a stale stack) ends no BIG the stack hears of, so the
  session checks for it: with reconnecting on, the state goes to
  Reconnecting and the broadcast is set up again on a new stack (up to 5
  attempts, 3 s apart, same name, Broadcast_ID and source); otherwise, or
  when the adapter does not come back, error.auracast.
- Layouts (`le_audio::broadcast::BroadcastLayout`; `--auracast-layout`,
  the Auracast tab's channel choice, `stream.start_broadcast`'s `layout`):
  stereo, mono, split and `matrix_<front>_<rear>` (older names for some of
  those: quad, front_rear, pairs, rear_subgroups, subgroups). Below: split
  first, then the matrix layouts built on it.
- Split (`--auracast-split`, the Auracast tab's "Left / right separately",
  layout `split`): left and right as two broadcasts of
  their own, "<name> L" and "<name> R" (Broadcast_ID + 1 for R), each an
  advertising set (SID 1 / 2) and a BIG (handle 0 / 1) of one mono BIS
  without a location, for a pair of speakers that each tune to one
  broadcast (as FlooGoo's "true wireless stereo" firmware does).
  `le_audio::broadcast::groups` lists the broadcasts, `group_plan` /
  `group_adv_data` give each its BASE and announcements; the shim holds
  `BLUECASTER_BROADCASTS` (4) slots, a slot's index being its BIG handle;
  bt-host creates every BIG and starts sending once all are up
  (`IsoSender::start_bigs`: one SDU set per interval across the BIGs, each
  BIG asked for BIS can-send-now, the round ends when every BIS got its
  SDU). A BIG that fails or ends takes the others with it. Every slot after
  the first advertises from a static random address of its own (derived
  from the public one): Android lists broadcasters by address and showed
  two sets from one address as one broadcast whose name kept changing.
  Tried on 2026-11-20 on an ASUS BT600 (Realtek, 0B05:1D70) with two JBL
  GO 4, one on L and one on R, at 24_2_2 and 48_4_2: a good stereo image,
  no audible offset between the two, now and then a short stutter, on the
  whole it works fairly well. Open: the cause of the stutters, and how far
  apart in time the BIGs' anchors are on other controllers (each receiver
  renders relative to its own BIG).
  Split and mono exclude each other: `--auracast-mono` with
  `--auracast-split` is an argument error, and `stream.start_broadcast` with
  both `mono` and `split` true is refused as a bad request
  (`stream.bad_channels`).
- Quad (layout `quad`, `matrix_separate_separate`): the four speakers of a
  multichannel source, each a mono broadcast of its own, "<name> FL" /
  "FR" / "RL" / "RR" (BIG 0-3, SID 1-4, all four shim slots). Front +
  rear (`front_rear`): the front as one stereo broadcast "<name> Front"
  (two BIS, Front Left / Right) and the rear as two mono ones. The plan
  marks the rear BIS `ChannelSource::RearLeft` / `RearRight`. The encoder
  then takes the source's speakers instead of the stereo downmix:
  `audio_capture::Capture::channel_mask` (the mix format's dwChannelMask)
  goes through `LePipelineConfig::channel_mask`, and the interleaved
  channels are mapped by their mask bits to four lanes, front left / right
  and rear left / right. A matrix layout gets the lanes from the matrix
  mixer (below); `pipeline::le::speaker_matrix` is the fixed mapping of a
  plan with rear channels and no mixer (the centre at -3 dB and the LFE at
  -6 dB in both fronts, back and side in the rear, -3 dB each when a source
  has both). No mask, or one that does not fit the channel count: the usual
  mask of the count. A source without rear speakers (stereo) plays its
  front on the rear too. The EQ runs on all four lanes. More than two BIS:
  RTN 2 instead of the presets' 4 (`RTN_MANY_BIS`; four BIS at RTN 4 and
  their advertising hardly fit a 10 ms interval). The rear may run at
  another preset than the front (see "Matrix mixer"). Source: an output
  device set to 5.1 / 7.1 (e.g. VB-Audio Hi-Fi Cable, its speaker setup in
  the Windows sound settings) as the default output, captured as the
  system audio (the tab's source), or chosen as the virtual / output device.
  Tried on 2026-11-20 on the ASUS BT600: quad and front_rear do not start.
  Two BIGs come up, the third LE Create BIG gets 0x0D (Connection Rejected,
  Limited Resources) from the controller, at 24 and 48 kHz alike. Pairs
  (layout `pairs`, "<name> Front" / "<name> Rear", two stereo broadcasts:
  four BIS in two BIGs) tells whether the limit is two BIGs or two BIS.
  It started at 24 and 48 kHz: the limit is two BIGs.
- Rear subgroups (layout `rear_subgroups`):
  the front in stereo, the rear as one broadcast "<name> Rear" whose BASE
  has two subgroups of one mono BIS each, labelled by Program_Info (0x03)
  "Rear left" / "Rear right", as multi-language broadcasts offer their
  languages (`base_with`, `group_base_ad`). Three streams in two BIGs;
  whether a receiver lets one pick the subgroup is what it tries. On
  2026-11-20 (BT600, a phone and JBL GO 4): it starts, Front joins as
  stereo, but the receiver shows "Rear left" or "Rear right" seemingly at
  random, so a subgroup cannot be picked reliably. Layout `subgroups`
  puts the front in subgroups too (mono BIS, "Front left" / "Front right").
  It runs, but the receiver always takes the left (first) subgroup. Left
  as it is for now: other receivers may let one pick.
- Adapter (`Settings::auracast_adapter_id`, the Auracast tab's **Adapter**,
  shown once two adapters have WinUSB): the adapter broadcasts go on.
  Adapters differ here: some cannot broadcast (on the Intel AX211 the
  broadcast setup timed out), and some only broadcast, with no classic
  Bluetooth, but run more BIGs than the others (an nRF5340 DK: four; the
  ASUS BT600: two). So the adapter for everything else and the one for
  Auracast are chosen apart. Empty, **Use the current adapter**: the
  adapter everything else uses. A process holds one BTstack, so the two
  never run at once: `run_broadcast` holds `Inner::broadcast_wanted` while
  it runs, `adapter::adapter_for` then names the Auracast adapter, and
  `ensure_host` restarts the stack when the running one was started for
  another adapter (`Inner::host_adapter`): on it for a broadcast, back on
  `Settings::adapter_id` for the next scan or connection. When a broadcast
  on its own adapter ends, BlueCaster shuts that stack down, which frees
  the Auracast adapter. A broadcast does not start while the Auracast
  adapter is missing (`Inner::auracast_adapter_missing`): the first other
  adapter found might not support broadcasting. A broadcast set up again
  after its adapter stopped working waits until that adapter is back
  (`Inner::wanted_adapter_present`) and opens the same adapter, even when
  none is chosen (`Inner::broadcast_adapter`): a stack started in the
  meantime would open the first other adapter found. Developer mode off:
  not used. The
  list names each adapter with its MAC address, as two can share a name
  ("Bluetooth Controller").

BTstack 1.8.1 pitfalls found along the way:
- `HCI_EVENT_BIS_CAN_SEND_NOW` is emitted only 4 bytes long, so the BIS handle
  it should carry is cut off. The sender maps the BIS by its index.
- LE Create BIG takes the Broadcast_Code in BTstack's `K` format (reversed
  on the wire). A code is the UTF-8 string with its first character in the
  first HCI octet, zero padded (as Android and Bumble send it), so
  `Stack::big_create` passes it to BTstack reversed.

Emulator: `emu_le_audio_sink.py --layout auracast [--broadcast-code CODE]`
(`le_audio/broadcast.py`: BIG source and synchronized receiver in the
emulated controllers; `le_audio/broadcast_sink.py`: a Bumble host as the
broadcast sink, parsing the announcements and the BASE with Bumble's own
code). Checked by hand on 2026-11-05: open (all SDUs on both BIS, no late
SDU), encrypted with the right code (synced) and a wrong one (MIC failure,
no audio), mono 24_2_2. `run_test.py --auracast` runs the cases in
`le_audio_tests/auracast.py`: stereo, mono, encrypted, split, the matrix
layouts, and a rear at its own rate (`--auracast-rear-preset`). Each
broadcast gets its own sink (`--layout auracast --follow <suffix> ...`).
The cases check BlueCaster's log and the sinks' log: names, Broadcast_IDs,
BASE, the requested BIGs, and the SDUs of every BIS (count, length,
sequence, left differs from right). Not done yet: `bluecaster_decode`
support for BIS (the decoder change in `tools/a2dp_decode` is unfinished),
so the cases do not decode the audio.
`--layout auracast-split` starts two sinks, one following "<name> L" and
one "<name> R" (for `--auracast-split`). Checked by hand on 2026-11-21: both
BIGs up, each sink got every SDU of its BIS; a refused LE Create BIG for
either broadcast ends the other and removes both advertising sets. After a
stop both sinks have received the same number of SDUs; before the stop
let the last round finish, R ended one SDU short in 3 of 3 runs.
Real hardware: not tried.

### Matrix mixer

A broadcast's outputs (FL FR RL RR) come from the source's speakers (FL FR
FC LFE BL BR SL SR, by the capture's dwChannelMask) through a 4×8 matrix
(`pipeline::mix`), then the EQ, then a per-output delay (0–500 ms, linear
interpolation, so 0.01 ms steps work) and polarity. Automatic: fronts as
they are, centre (-3 dB) and LFE (-6 dB) into both fronts, back/side into
the rear (or folded into the fronts at -3 dB when the layout has no rear),
with an optional row normalisation; or every gain by hand. The settings and
named presets live in `auracast_mix.json`; the encode thread reads a
versioned `MixHandle`, so changes from the Auracast tab (`auracast.mix_set`)
are heard from the next block while broadcasting. Only the matrix layout
(`matrix_<front>_<rear>`, each pair stereo / separate / subgroups / off;
`BroadcastLayout::Matrix`) uses it: stereo, mono and split, and unicast
streams, keep the fixed downmix. The BT600 runs two BIGs at most, which the
Auracast tab warns about (stereo and subgroups take one BIG, separate two).

The rear may run at another rate than the front (`rear_quality`,
`BroadcastConfig::rear_preset`, same 10 ms frames): its BIS carry their own
`CisPlan::preset`, each BIG and BASE is made from its group's
(`group_plan`), and all broadcasts share one presentation delay. The encode
thread works at the top rate (`StreamPlan::top_sampling_hz`); a 24 kHz
channel next to 48 kHz ones goes through a 63-tap windowed-sinc decimator,
and the 48 kHz channels through the same 31-sample (0.65 ms) delay, so front
and rear stay in step.

The source switches while broadcasting (`auracast.set_source`, tone or
system audio): `Service::change_source` hands it to the broadcast loop,
which stops the capture and starts the new one into the same queue; the
BIGs stay (silence goes out for the moment in between).

## LE Audio off

Users with the setting off get no LE Audio, but LE itself stays up to hold the
LE keys of classic devices (issue #86):

- **LE keys.** The service always sets `HostConfig.le_keys`, so LE is not
  hidden and BTstack runs its normal HCI init for a dual-mode controller, as
  with LE Audio on. `Stack::le_init` then sets up the bonds file and SM; the LE
  core, its modules and the GATT server do not exist. A classic pairing also
  yields the device's LE keys (`btstack-sys/src/ctkd.rs`), just as Windows and
  Linux get them from headphones without LE Audio. Forgetting the device
  deletes them (`BtHost::forget_link_key`). Emulator cases
  `ctkd-developer-off` and `ctkd-le-off`.
- **LE hidden.** A BtHost with neither `le_keys` nor `le_audio` (tests,
  tools) keeps LE hidden, the default for every run:
  `btstack-sys/src/transport.rs` clears LE Supported (Controller) in Read
  Local Supported Features and the LE command bits BTstack looks up in Read
  Local Supported Commands, so BTstack skips all LE setup; the transport
  answers LE commands that BTstack still sends (LE Set Scan Parameters, whose
  flag cannot be cleared from outside) with Unknown HCI Command, so they never
  reach the controller.
- **Toggling the setting** takes effect when BTstack starts: an idle stack is
  stopped immediately; while streaming or scanning, the change waits for the
  next scan or connection (`Service::le_audio_deferred()`; the UI shows v1's
  note from `settings.set`'s `le_audio_deferred`).
- **The device scan** (`Service::scan_devices`) runs the LE scan alongside the
  classic inquiry from the start (scan window 30 ms of every 60 ms), so LE
  Audio entries appear as they are found, even when the scan is stopped once
  the classic entry appears, and in the first-run setup's long scan; a scan
  that nobody stops scans LE for at least 5 s (`status.le_scanning` while only the
  LE part runs). Chained extended advertising data is joined before it is
  parsed. The log has one `LE scan: ...` summary line per scan, and an INFO
  line for each advertiser without an LE Audio service that has the address or
  name of a classic device from the scan or `devices.txt` (earbuds whose LE
  Audio is off still advertise over LE for their app). Emulator scan case
  `le-audio` (`run_test.py --scan le-audio`).
- **Devices and profiles.** A dual-mode device has two entries (classic and
  LE Audio, same address) in the core; an LE scan never marks a classic
  entry. The UI shows them as one (issue #86, `lib/profile.ts`
  `listedDevices`): with the setting on, the LE Audio entry represents both,
  Add / Rename / Forget act on both and Add makes both profiles; a
  scan that finds the other entry of a saved device saves that entry too
  (`service.rs` `save_twins`). Turning the setting on or off while a device
  streams restarts the stack and starts the device's profile of the other
  kind (`state.rs` `le_switched_profile`, an LE profile made from the
  classic one when missing). CTKD runs whenever SM is up, not only in
  developer mode, so a classic pairing always yields the LE keys.
- **Link key from an LE pairing.** Conversely, an LE pairing (the LE Audio
  entry) yields the link key (issue #86): SM offers LinkKey and
  CT2, and derives the BR/EDR link key (h6, or h7 with CT2 on both sides)
  only when the device's Pairing Response carries LinkKey in both key
  distributions, so the device derives the same key. `ctkd.rs`
  (`bluecaster_rs_ctkd_from_le`) keeps it in `link_keys.txt` under a public
  identity address only (type 7 or 8), replacing an older key, since the
  device replaced it too. An authenticated older key is dropped instead of being
  replaced by an unauthenticated one (BLURtooth). Developer mode's Pairing
  keys then shows the key for the LE Audio entry; forgetting the LE Audio entry
  deletes the key unless the classic entry is still saved. The log has one `LE
  pairing: BR/EDR link key ...` line per bonding, without an address.
  Emulator cases `run_test.py --le-pairing` (one dual-mode sink, LE then
  classic runs; the sink authenticates only with the key it derived).
- **Conversational contexts.** With the microphone, the sink ASEs name the
  source contexts alone (Conversational), not Media | Conversational: EarFun
  earbuds answered that Enable with Rejected Metadata. A conversational
  start that fails is retried once without the microphone. LE Audio
  entries are saved in `le_audio_devices.txt` so that `devices.txt` stays
  readable by older versions; `addr|name|le` lines of earlier prototypes are
  still read and moved. With the setting off the UI does not list LE Audio
  entries, and an LE Audio profile fails with `error.le_audio_disabled`.
  `profiles.json` has `le_audio` only for LE Audio profiles.

## Pitfalls

- Touch BTstack only from the BTstack thread and never block it; do blocking
  waits only on the worker.
- All CIS of a CIG are created at once and a CIG cannot get new CIS later:
  to change the CIS set, stop the stream and start it again. BTstack never
  removes a CIG whose CIS were never established (`gap_cig_remove`
  "succeeds" without sending LE Remove CIG); btstack-sys has 4 CIG slots,
  freed when LE Remove CIG is sent, after a failed CIG_CREATED or at power
  off.
- `gap_cig_remove` inside `GAP_SUBEVENT_CIS_CREATED` is lost: `hci.c` sets the
  CIG's state right after emitting the event. `bap.rs` defers the removal
  until the event has been handled (`UnicastTimer::Remove`) and drops a CIS
  that comes up while the stream is stopping.
- Only one LE connection can be set up at a time: connect set members one
  after another.
- LE ACL buffers can be small (27 bytes in the emulator): GATT writes are
  fragmented; BTstack negotiates the ATT MTU before the first query.
- Connect with the address type the advertisement showed.
- The device / CIS handles in captures differ between BlueCaster's controller
  and the sink's; `bluecaster_decode --le-audio` maps them by CIG / CIS id.

## Known limitations

- The headset microphone (source ASEs) is verified only against the
  emulator, which writes it to a WAV file (rendering to a real cable is not
  exercised there); one mono source direction per device; the
  source preset is fixed by the plan (no renegotiation when the headset
  changes its available contexts); the mix of two earbuds' microphones
  aligns them by arrival, not by time stamp.
- Broadcast works only as a source (developer mode, see above); no BASS, no CAP
  procedures beyond what BAP needs.
- SDUs without time stamps; no LE Read ISO TX Sync; a simple jitter buffer
  (the prefill only grows).
- LE Audio does not support application capture (it does support system
  loopback, virtual devices and the test tone).
- Not verified with real LE Audio headphones or a real controller's CIS
  scheduling; extended scanning only through BTstack's fallback to legacy
  scanning in the emulator. The aarch64 build of the LE code has not been
  checked.

## TODO (before LE Audio can leave "experimental")

Collected from the v1 profile branches (2026-10-16) and the v2 port
(2026-10-18). So far, everything has been verified only against the Bumble
emulator.

Verification
- [ ] Real hardware: a Bluetooth 5.2+ adapter with ISO channels in WinUSB
  mode (the HCI ISO path over the bulk endpoints has never run on real
  hardware), an LE Audio headset and a pair of LE Audio earbuds.
- [ ] The UI with real devices: the LE Audio settings, LE devices and set
  members in the scan list, streaming an LE Audio profile; classic A2DP
  while the LE setting is on; toggling the setting while streaming; a
  dual-mode headset's two entries.
- [ ] MCP commands to real Windows players (SMTC and the media-key fallback);
  only reading SMTC was tried.
- [ ] Values written from memory because the specs could not be fetched:
  TBS (Bearer Technology 0x04, Signal Strength 255, ATT error 0x80 for a long
  read that changed, ATT 0x0D for a wrong length; the notification order
  follows Zephyr's `tbs.c`), MCS (ATT error 0x80 for a long read that
  changed), CSIS error codes (0x80-0x84) and GMAP (Table 3.16 preset values,
  UGG / UGT feature bits, Table 3.18 `_gs` presets: 60 ms presentation delay).
- [ ] Service Changed after a database change and the Database Hash were
  only exercised for the hash itself (`mcp-read`); a client bonded with an
  older database has no emulator case.
- [ ] Untested paths: a device that lost its bond, extended advertising
  scanning, rotating RSIs, CSIS included by CAS, RPAs on real devices
  (emulated: `privacy-rpa`).
- [ ] Emulator: late SDUs are measured against each SDU interval with the
  emulator's own stalls excluded; watch whether `late SDU` still fails
  occasionally on a loaded machine (with the 1 ms timer resolution, it did not
  appear on an idle one).
- [ ] The aarch64 build (`cargo check --target aarch64-pc-windows-msvc`).

Missing features
- [ ] Source ASEs (microphone): real headsets; the render to a real
  virtual cable over hours (the drift control against a headset's clock);
  GMAP `_gs` values checked against the spec; CCP calls with the
  microphone (the conversational stream does not follow CCP's call state
  yet).
- [ ] Broadcast: emulator cases in `run_test.py`, BIS in `bluecaster_decode`,
  real hardware; TMAP BMS / GMAP BGS roles.
- [ ] CSIP: SIRK / size / rank change notifications, OOB-only SIRK; after a
  link loss the CLI reconnects only the address it was given; the 10 s member
  search time is fixed.
- [ ] VCP: "remember volume" per device, Mute and relative opcodes, mute
  mirrored to Windows, Volume Flags.
- [ ] MCP: fast forward / rewind and seeking state, artist in the track
  title.
- [ ] CCP: a real call source (softphone integration via
  `ccp_model::Backend`), Join, TBS instances besides GTBS, signal strength
  reports.
- [ ] TMAP / GMAP roles in the UI's device list (the diagnostics show them);
  TMAP low-latency 48_x_1 presets are only used with `--le-preset`.
- [ ] Core: SDU time stamps / LE Read ISO TX Sync, a better jitter buffer,
  application capture mode.
