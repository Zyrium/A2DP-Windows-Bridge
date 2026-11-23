---
title: Keyboards and mice (developer note)
layout: default
nav_exclude: true
english_only: true
---

# Keyboards and mice (developer note)

Status: prototype for issue #413. Developer mode only, off by default.
Verified on the emulator only. No real keyboard, mouse or adapter has been
tested.

## What the feature does

BlueCaster takes over the whole Bluetooth adapter, so Windows loses the
keyboards and mice connected through that adapter. With this feature on,
BlueCaster connects those keyboards and mice and passes their input to
Windows with `SendInput`. The feature supports both classic Bluetooth (HID)
and Bluetooth LE (HID over GATT) devices.

## Why the feature is dangerous

Input works only while `bluecaster-core.exe` is running and the Bluetooth
stack is up. If the PC's only keyboard and mouse are connected this way, the
user has no input when:

- BlueCaster closes, crashes or updates.
- BlueCaster has not started yet: on the sign-in screen and the lock screen.
- A UAC prompt or a window running as administrator has focus. The core
  runs without elevation, so Windows blocks the core's input there.

BlueCaster normally restarts the stack before each new audio stream,
because later streams on the same stack stuttered (issue #84). While a
keyboard or mouse is connected, BlueCaster keeps the stack running instead,
so the stutter may return. The stack still restarts after sleep, when
adapter settings change, when LE Audio is turned on or off, and when headset
mode needs voice. Each restart briefly disconnects keyboards and mice, which
then reconnect automatically.

## Turning the feature on

The `hid_host` setting takes effect only while developer mode is on. Only
the `hid.enable` request can turn the setting on:

1. The core issues a six-digit code (`hid.code`, valid for two minutes,
   usable once).
2. The dialog shows the risks, two check boxes and the code. The user types
   the code. The button stays disabled for ten seconds.
3. The core checks developer mode, both check boxes and the code again
   (`Service::hid_confirm`).

`settings.set` can turn the feature off but never on. Turning developer mode
off also saves the feature as off. While the feature is on, every screen
shows a warning banner, and BlueCaster asks before turning the feature off.

## Parts

| Part | Where |
| --- | --- |
| Report descriptor parser, decoder, key map, `SendInput` | `app/src/crates/hid-input`, `win-sys/src/input_inject.rs` |
| Shared engine: allow-list, counters, release of held keys | `bt-host/src/hid.rs` |
| Classic HID host (BTstack `hid_host`) | `bt-host/src/hid.rs`, `btstack-sys/csrc/shim.c` |
| LE host (HID over GATT) as an LE module | `bt-host/src/le/modules/hogp.rs` |
| Pairing with a number shown | `bt-host/src/hid/pairing.rs`, `btstack-sys/csrc/shim.c` |
| Keyboard lights | `hid-input/src/leds.rs`, `bt-host/src/hid.rs`, `hogp.rs` |
| Guard against held keys | `win-sys/src/hid_guard.rs`, `crates/core/src/hid.rs` |
| Where input goes (this process, the input helper, or counted only) | `hid-input/src/sink.rs`, `crates/input-helper`, `win-sys/src/input_desktop.rs` |
| Setting, device list, confirmation, keeping the stack up | `crates/core/src/hid.rs` |
| IPC (`hid.*`), settings rules | `crates/ipc/src/protocol.rs`, `core/src/state.rs` |
| UI | `ui/src/components/HidHost.tsx`, `HidEnableDialog.tsx` |

The code follows these rules:

- Only devices on the device list may connect. BlueCaster refuses any other
  device.
- However a connection ends, the code releases any keys and buttons still
  held.
- Logs contain counts only: no addresses, key codes, typed text or pairing
  numbers.
- Only the device being added pairs with a number shown, for at most 90
  seconds or until its pairing ends. Every other device pairs as before.
- With the feature off, none of its code runs: no HID channels, no LE for
  keyboards and mice, no extra scan entries.

With the feature on and LE Audio off, the stack starts LE for keyboards and
mice only. No LE Audio service, scan entry or UI state appears.

## Checks that undo a change

Two checks keep a change from leaving the user without a working keyboard
or mouse. They work like Windows' "Keep these display settings?" prompt
(`crates/core/src/hid/check.rs`).

### What counts as delivered

Input counts as delivered only when it came from a report of that very
device and the sink passed it on for real. Each sink says what that means
in `InputSink::delivered` (`hid-input/src/sink.rs`):

| Sink | Delivered when |
| --- | --- |
| `LocalSink` (this process) | `SendInput` inserted every input of the batch |
| `HelperSink` (input helper) | The helper's ack covers the batch, and the ack reports nothing refused |
| `DryRunSink` (emulator, `--hci-tcp`) | The batch was counted |

The engine passes each batch with the address of the device it came from.
Key-ups of a release carry no address, so a release never counts. Inputs of
two devices are never merged in the helper's queue.

Each delivery also records a hint: whether Windows registered input since
then (`GetLastInputInfo`). The process that injects reads the hint, so the
core reads it for `LocalSink` and the helper for `HelperSink`. The helper
reports the hint as a `Seen` frame. Record mode and dry run never read it.
The hint never decides anything. Microsoft documents `GetLastInputInfo` as
per session, but does not say whether injected input updates it. The
documentation also says nothing about desktops or about callers running as
SYSTEM. The log line of a confirmation shows the hint.

### Adding a device

After `hid.add`, a device that was not on the list is unconfirmed. Adding a
device again that is already on the list starts no check.

- The countdown of 30 seconds starts when the device is ready for input
  (`HidEvent::Ready`). Over classic Bluetooth the device is ready once its
  channels are open and its report descriptor is known, or turned out to be
  unusable. Over LE the device is ready once every input report is
  subscribed to. Pairing with a number comes first.
- The UI asks for a key press, a mouse move, or either, from the report
  descriptor (`Decoder::kind`).
- Delivered input from that device confirms it. Input from another device
  does not. A confirmed device is never removed by this check later.
- The device is removed when the countdown ends without input, when the
  device went away and did not come back in time, or when the device never
  became ready within 120 seconds. The removal is the one `hid.remove` does:
  the device is disconnected, taken off the list and its pairing is
  forgotten. The UI then shows one sentence and **Try again**.
- The core keeps the state, so the check goes on with the window closed.
  `devices/hid_unconfirmed.txt` lists the unconfirmed devices. At its next
  start the core removes every device in that file.
- No request confirms a device. `hid.check_cancel` (remove now) and
  `hid.check_dismiss` (hide the notice) are open requests, because they
  only remove.

### Turning the feature on

Right after `hid.enable` turns the feature on, the core runs a self-test
(`InputSink::self_test`). The sink injects one relative pointer move of
zero in each direction. The pointer stays where it is, and no window gets a
key or a click. `LocalSink` passes the move with `SendInput`. `HelperSink`
passes it through the queue, the pipe and the helper, which proves that the
helper started and is attached to the input desktop. `DryRunSink` reports
"not tested" and passes.

If the test fails, the core turns the feature off again, as `hid.disable`
does, and `hid.enable` fails with the reason: the helper did not start,
Windows refused the input, or the test timed out. If no stack with
keyboards and mice is up within 10 seconds (a stream keeps the old stack),
the test runs when such a stack starts and only warns then.

Later self-tests run once per core start with the feature already on, and
for every input helper after the first one (a new session, a helper that
died). A failure then never turns anything off. The banner shows a warning
instead, because turning the feature off cannot help someone whose input
already fails, and a false alarm would cut a keyboard that works. No client
request can start a self-test, other than `hid.enable`.

Tests never inject: `LocalSink` takes a fake `LocalInjector` in unit tests,
and the helper tests use the record mode.

## Pairing with a number

Some keyboards ask for a number when they are added. BlueCaster shows six
digits, and the user types the digits on the keyboard and presses Enter.
Older keyboards use a PIN instead, and BlueCaster provides the PIN the same
way. The PIN path is untested. The CLI prints the number only over
`--hci-tcp`, so use the app to add a real keyboard that asks for a number.

## LE devices that change their address

A paired LE device that uses a private address reconnects automatically.
BlueCaster scans in the background and recognises the device by the pairing
keys. With LE Audio off, the adapter also resolves the address where
possible. After pairing, the device list stores the device's identity
address.

## Keyboard lights

BlueCaster sends the Num Lock, Caps Lock and Scroll Lock states to every
connected keyboard: once after the keyboard connects, and again whenever a
state changes in Windows. BlueCaster checks every 150 ms.

## Keys held when BlueCaster dies

A hidden guard (`bluecaster-core.exe --hid-guard`) releases the keys and
buttons still held if the core exits or crashes. The core starts the guard
while passing input to Windows and records the held keys and buttons in
shared memory. When the feature is turned off, the guard exits without
releasing anything.

## Files

- `devices/hid_devices.txt`: `address|name`, with a third field `le:public`
  or `le:random` for LE devices.
- `devices/hid_descriptors/`: each device's report descriptor and, for LE
  devices, its report handles, so that input can be decoded as soon as the
  device reconnects.

## Testing

```
pwsh ./build.ps1 -Decoder
python tools/emu/run_test.py --hid
```

The emulator test never types into the PC: over `--hci-tcp`, BlueCaster
only decodes and counts input. The 18 cases cover connections in both
directions, a device that is not on the device list, a link lost with a key
held, the saved descriptor, LE pairing, LE reconnection without pairing
again, an LE device whose address changes, input sent right after an LE
reconnection, pairing with a number (classic and LE), keyboard lights
(classic and LE), the LE scan, and the feature turned off.

Four more cases test the check of a device being added, with a countdown of
4 seconds (`--hid-confirm 4`). In `hid-confirm` and `hid-le-confirm` the
device types, and BlueCaster keeps the device. In `hid-confirm-silent` and
`hid-le-confirm-silent` the device sends nothing, and BlueCaster removes the
device and closes its link after the countdown.

`cargo test -p bluecaster-core-exe --test hid_guard` tests the guard without
passing any input to Windows.

To test by hand, run
`bluecaster-core --cli --hid-test 60 --hid-device <address>`. The command
prints the counts once a second. Add `--hid-inject` to pass the input to
Windows.

## Known limitations

- Address resolution by the adapter is untested. The emulator does not
  resolve addresses, so the tests covered only the background scan. With LE
  Audio on, BlueCaster uses only the background scan.
- Reading the lock keys from Windows, passing real input to Windows and a
  real release by the guard are untested, because the emulator test never
  touches the PC's input.
- Outside the service, input does not reach the sign-in screen, the lock
  screen, UAC prompts or administrator windows. The input helper in the
  service (stage 4) has not been tested on these screens yet.
- Games with anti-cheat may ignore injected input.
- BlueCaster does not pass input from gamepads or absolute pointing devices
  to Windows.
- If an LE device has changed its report handles since the last connection,
  BlueCaster may drop input that arrives before the device's services are
  read again.

## Hardware checklist (not done yet)

- [ ] Classic mouse and keyboard: add, type, reconnect after power off.
- [ ] LE mouse and keyboard: add, type, reconnect after sleep.
- [ ] Audio stream running while a mouse moves: dropouts, pointer lag.
- [ ] Codec switch and headphone reconnect: how long the mouse stays disconnected.
- [ ] Sleep and resume.
- [ ] BlueCaster killed with a key or button held: the guard releases the
      key or button.
- [ ] A keyboard that asks for a number (classic and LE): the right
      number pairs the keyboard, a wrong number fails cleanly.
- [ ] An LE device with a changing address: reconnects after power off.
- [ ] Lock key lights on classic and LE keyboards, also after a reconnect.
- [ ] Several streams in a row with a mouse connected: does the stutter of
      issue #84 return?
- [ ] An adapter without Bluetooth LE.
- [ ] Add a keyboard and do nothing: BlueCaster removes it after 30 s and
      shows **Try again**.
- [ ] Add a keyboard and type: the keyboard stays.
- [ ] The same two steps with a mouse.
- [ ] Close the window during the countdown: the device is still removed.
      Stop the core during the countdown: the device is removed at the next
      start.
- [ ] Turn the feature on with the service: the self-test passes. The
      service's log shows whether Windows registered the input.
- [ ] Turn the feature on with an administrator window in front (without
      the service): the self-test fails, and the feature turns off again.

## Moving to a Windows service (in progress)

Goal: keyboards and mice also work on the sign-in screen, the lock screen,
UAC prompts and administrator windows. No process run by the signed-in user
can tamper with the process that produces input. To get there, the whole
core moves into a service. An agent in the user's session does only the work
that needs that session.

Stage 1 is done. With `--capture-agent`, a second process captures the audio
and passes it to the core through shared memory (`crates/pcm-link`,
`audio-capture/src/remote.rs`, `win-sys/src/shm.rs`). This second process is
the agent (`bluecaster-core.exe --agent`).

The foundation of stage 2 is done too: `crates/session-host`. With
`--session-agent`, the core serves the agent pipe
`\\.\pipe\bluecaster-agent-<user SID><instance>`. `--capture-agent` still
works. The core starts one long-lived agent (`--agent --child`) whenever no
agent is connected. An agent started by hand (`--agent`) waits for a core
and serves that core instead. When that core exits, the agent waits for
another core.

Requests carry ids in both directions. The agent sends events, including
the agent's log lines. PCM goes through channels in both directions
(`host/capture.rs`, `host/render.rs`).

When the agent dies, waiting requests fail with "agent lost". Running
captures end with `EndReason::AgentLost` and are reopened. The next agent
serves new requests.

The rest of the core calls the facade (`session_host::host()`, trait
`SessionHost`) instead of calling Windows. To add a group of operations,
see the doc comment in `session-host/src/lib.rs`. Stage 3 checks the
connecting agent in one place: `authorize` (hub.rs). Stage 4 starts the
agent in the user's session in one place: `launch_agent` (hub.rs).

Stage 2 is done. Every group of user-session work goes through the facade:

| Group | What runs in the agent |
| --- | --- |
| `devices`, `default_device`, `endpoints` | Device lists and formats, switching and restoring the default output and input, device change notifications, the app list |
| `volume`, `activity` | Output volume and mute, the Volume Mixer, watching an app's process, microphone-in-use checks |
| `capture`, `render` | Audio capture, and the headset microphone output |
| `media` | The Windows media session and media keys |
| `shell`, `autostart`, `elevation` | The tray, notifications, opening the UI, the Run entry, the driver helper and the VB-CABLE setup |

`bluecaster-core.exe --headless` runs the core without a tray or window,
with the agent turned on. The service will host this mode. While a headless
core is connected, the agent shows the tray.

The agent holds some changes on the core's behalf: a switched default
device and a muted output. If the core dies, the agent restores the default
device and unmutes the output. If the agent dies, the core reapplies both
changes through the next agent. Nothing on the BTstack thread or an audio
thread waits for the agent.

Run the tests with `cargo test -p session-host` and
`cargo test -p bluecaster-core-exe --test session_agent`. The second command
runs the real agent program. Tests that would change the PC (default
devices, volume, mute, the Run entry, elevation, the tray) use fakes.
Emulator cases that use the agent: `sbc-agent`, `aac-agent`,
`sbcxq-44k-agent`, `sbc-agent-killed`, `sbc-agent-first`,
`sbc-agent-replaced`, `avrcp-keys-agent`, `msbc-agent` and
`cvsd-agent-killed`. For LE Audio: `stereo-agent`, `mic-stereo-agent`,
`mcp-read-agent` and `mcp-control-agent`.

Not verified: the real tray in the agent, real elevation through the agent,
real default device, volume and mute changes through the agent, and the
headset microphone played into a real output. The core and the agent still
run as the same user. The installed app does not use the agent yet.

### Service data (stage 3)

`--headless --service-data <folder>` runs the core in service mode. The
core keeps all its data in that folder instead of the user's folder. The
service will pass `%ProgramData%\BlueCaster`. Only SYSTEM, the
Administrators and the service's own account can open the folder
(`win-sys/src/acl.rs`). In service mode, the core encrypts the pairing keys
for its own account (`dpapi::Scope::Service`). The window keeps its log in
the user's folder. `crates/core/src/service_data.rs` lists where each file
lives.

At its first start, the core moves the user's data into the folder once,
through the agent (group `migration`). The agent reads a fixed list of
files and decrypts the keys. The core checks each file and encrypts the
keys again. The user's files stay where they are. The move never takes the
keyboard and mouse list, and it turns the feature off. Driver records,
Windows' replaced keys and firmware stay behind too. Run
`cargo test -p bluecaster-core-exe --test service_data` to test the move
with a real agent.

### Pipes and request rules (stage 3)

In service mode, the core serves the UI on `\\.\pipe\bluecaster-service`.
Interactive users can connect, but they cannot create another instance of
the pipe. The core reads each client's identity from Windows
(`win-sys/src/pipe_peer.rs`): process, session, user, elevation and program
path. Before trusting the pipe, the UI checks the core's identity the same
way. The agent must run in the console session and must be the program
file next to the core's.

Every request has a class (`ipc/src/access.rs`, `core/src/rules.rs`).
Adding a request without a class breaks the build.

| Class | Requests |
| --- | --- |
| Open | Streaming, scanning, profiles, ordinary settings, the Volume Mixer, turning this feature off, the code for turning it on (`hid.code`), Exit (`app.close`) |
| Needs an administrator | Turning this feature on, adding or removing a keyboard or mouse, developer mode and the settings behind it, raw adapter commands, driver changes, VB-CABLE setup, reading pairing keys, captures and bundles |
| Refused | Quitting the core, and any file path from the client |

When a request needs an administrator, the UI sends `app.run_as_admin`. The
core issues a single-use ticket for that exact request. The agent starts
`bluecaster-core.exe --request` elevated, so Windows shows its own prompt.
The core accepts the ticket only from an elevated copy of BlueCaster.

### Logs and test switches (stage 3)

The agent and the core both redact every forwarded log line
(`session-host/src/redact.rs`). Placeholders replace quoted names, paths,
file names, IDs and addresses. Log statements that can run in the agent
must put every name in quotes. The test switches (`--test-shell`,
`BLUECASTER_TEST_FAKE_VOLUME`, the guard's `--report`) work only in debug
builds. A release build given a test switch exits with code 2.

Not verified in stage 3: a real elevated request and the Windows prompt, a
core running as SYSTEM or as a service account, and the real
`%ProgramData%` folder. The tests simulate an elevated client, which only
the test build allows.

### The service and the user's session (stage 4)

`bluecaster-core.exe --service` is the service's entry point. Windows'
service control manager starts it. The service runs the headless core in
service mode, with the data folder `%ProgramData%\BlueCaster`
(`core/src/service_host.rs`, `win-sys/src/service.rs`). It reports start
and stop progress, stops the Bluetooth stack on stop and shutdown, and
passes session changes to the agent's hub. Sleep and resume use the
core's own power notifications, which work in a service too.

`--install-service` and `--uninstall-service` create and remove the
service and need an administrator (stage 5 replaces them with
`--service-setup`). The service starts automatically,
without a delay, so that keyboards and mice work on the sign-in screen.
Windows restarts it after a crash. It runs as LocalSystem and keeps only
four privileges. `service_host.rs` explains why, and what a split into a
low-privilege stack host and a small launcher would take.

The agent starts in the active console session once a user signs in
there (`session-host/src/sessions.rs`), with that user's token and a clean
environment (`win-sys/src/session_launch.rs`). Before anyone signs in
there is no agent, and the core runs without one. When another user takes
the console (fast user switching), a running stream stops and the old
user's agent ends. The stream's audio belongs to the old user.

In service mode the agent connects to `\\.\pipe\bluecaster-service-agent`,
and checks that the pipe's server is the core's own program. The core
accepts only the process it started, in the session it started it in.
PCM channels go to the agent as handles duplicated into its process,
because a service and the user's session share no `Local\` names.

The data move runs when the first agent connects. The user's files
replace the defaults the service wrote before anyone signed in
(`migration_pending.json`), and the core reads the moved data again
without a restart.

Run the tests with `cargo test -p bluecaster-core-exe --test
service_session`. A test file stands for the console's session
(`BLUECASTER_TEST_SESSIONS`, debug builds only). Never tested: the real
service, the agent started with another token, and the install commands.

### What the user gets in service mode (stage 4)

The signed-in user cannot open the service's folder, so in service mode:

- **Files the user asked for.** Diagnostic bundles, chip probe reports
  and chip script reports reach the user through the agent (group
  `handover`, `core/src/handover.rs`). The core sends only the file's kind
  and content. The agent picks the folder and file name
  (`logs\bundles\bluecaster-diag-<time>.zip` and
  `logs\probe\probe-<time>.json` in the user's BlueCaster folder), It checks
  the size and content, then returns the path. The core never reads
  from the user's folder. All three requests need an administrator, so
  an administrator approved every file the user receives. The agent writes to the console user's profile, not to the
  profile of the administrator who approved the prompt.
- **HCI captures** stay in the service's folder, because stopping a
  capture needs no administrator. A capture reaches the user inside a
  bundle. The UI shows only the capture's file name.
- **Folders.** The snapshot sets `service_mode` and leaves `config_dir`
  and `log_dir` empty. The UI hides the buttons that open the settings,
  firmware and log folders. The firmware download still works.
- **Logs.** The service's log contains no app names, device IDs or paths
  (`bluecaster_core::diag::log_name`). The Diagnostics screen still shows
  the log lines, and bundles include the log files.
- **Verbose logging.** The UI cannot read the core's `verbose.json` and
  `settings.json`. While the UI is connected to the service, its log
  level follows the snapshot's `verbose` and developer mode's
  `dev_log.always_verbose` (`ui/src-tauri/src/logging.rs`).
- **Exit.** `app.quit` is refused. The tray's Exit, the Quit button and
  closing the window without "Minimize to System Tray" send `app.close`.
  Streaming stops, the UI closes (event `ui_close`) and the tray icon
  disappears.
  The core keeps running, so keyboards and mice keep working. The tray
  icon returns when BlueCaster's window reconnects or a new agent connects.
- **Updates.** In-app updates are off. The UI shows the release page instead.
- **Turning on keyboards and mice** asks for an administrator once.
  `hid.code` is open, because the code alone turns nothing on. `hid.enable` needs
  an administrator, so the prompt appears only after the user ticks both
  check boxes, types the code and waits ten seconds. The core still checks
  developer mode, both check boxes and the code.
- **Is the pipe's server the service?** The UI and the elevated helper
  ask the service control manager (read-only, `win_sys::service_query`)
  for the service's program and process. The pipe server must be that process,
  run that program and run in session 0.

### The input helper (stage 4)

A service cannot pass input to the user's desktop or read the lock keys.
So a headless core hands keyboard and mouse input to a helper,
`bluecaster-core.exe --input-helper <pipe>`, in the console session (crate
`input-helper`). Under the service, the helper runs as SYSTEM, which can
switch to the sign-in screen, the lock screen and UAC prompts. When you
start the core by hand, the helper is a plain child process of the core. The resident app and
the command line still pass input themselves, with the guard.

- **The pipe.** For each helper, the core creates a pipe with a random
  name that only SYSTEM and the core's user may open. The core then
  starts the helper. The helper accepts the pipe only when the pipe server
  is its own parent process, and keeps a handle to that parent open. The
  core accepts only the client process it started. Frames are small and binary: key, button,
  move and wheel records with checked fields and bounded counts, and no
  paths or text. Any malformed frame stops the helper.
- **The queue.** The BTstack thread only adds input to a queue. When the
  queue is full, the core drops the oldest pointer moves (counted as
  `dropped`). Key and button transitions are never dropped singly. If the
  queue fills with transitions alone, the core discards all of them and
  the helper releases every key and button instead.
- **Held keys.** The helper releases every held key and button when the
  core closes the pipe or exits, when asked to stop, on a malformed frame,
  and on the new desktop after a desktop switch. The core tracks what the
  helper may be holding. If a helper is killed before it can release
  anything, the next helper releases those keys and buttons first.
- **Lock keys.** The helper reads the lock keys on its desktop every
  150 ms and reports changes. The core no longer reads them.
- **Another user at the console.** The helper releases all keys and
  stops, and a new helper starts in the new session. When no one is at the
  console, the core drops input.
- **Ctrl+Alt+Del.** `SendInput` cannot produce it. The helper reports it,
  and the service calls `SendSAS` only if the policy "Disable or enable
  software Secure Attention Sequence" (`SoftwareSASGeneration` under
  `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`) allows
  services (1 or 3). BlueCaster never changes the policy. Windows leaves
  it off because allowing it lets every service on the PC open the secure
  screen.

Run the tests with `cargo test -p input-helper` and `cargo test -p
bluecaster-core-exe --test input_helper`. In debug builds, the helper's
`--record` option writes the input to a pipe that the test reads, instead
of calling `SendInput`. The emulator cases
`hid-incoming-helper`, `hid-release-helper` and `hid-le-release-helper` run
the CLI's `--hid-helper-record` and need a debug build. Not tested yet: the
helper's real `SendInput`, desktop switches on a real secure desktop, the
helper running as SYSTEM, and `SendSAS`.

### The service as an add-on (stage 5)

BlueCaster still installs per user, without administrator rights. Nothing
changes for people who don't use keyboards and mice. Only those who do add
the service. With one administrator prompt, the app copies the programs the
service needs to a folder under Program Files and registers the service
from there. Only an administrator can change that folder, which is what
gives the program checks of stages 3 and 4 their value.

- **The layout.** The service folder is `<Program Files>\BlueCaster
  Service` (`win-sys/src/service_files.rs`). The path comes from the
  known-folder API, never from an environment variable. The folder holds
  its own copies of `bluecaster-core.exe`, `bluecaster-driver.exe` and
  `licenses\` (`SERVICE_PROGRAMS` and `SERVICE_DIRS` in
  `core/src/service_setup.rs`). `bluecaster-core.exe` runs as the service,
  the agent, the input helper and the elevated helper. The folder's DACL
  gives SYSTEM and Administrators full access, and users read and execute
  access only. The window, `bluecaster.exe`, stays in the user's own
  installation and is not copied.
- **The set-up tool.** `bluecaster-core.exe --service-setup
  <install|update|remove> [--remove-data]` (`core/src/service_setup.rs`)
  runs elevated from the per-user folder. The window starts the tool
  (`ui/src-tauri/src/service.rs`), so Windows shows its own prompt. For
  install and update, the tool copies the files from its own folder into
  the service folder, then registers or updates the service and starts it.
  Each copy is read back and compared before it replaces the old file, and
  a failed step undoes what the run did. For remove, the tool stops the
  service, deletes it and deletes the service folder. `--remove-data` also
  deletes `%ProgramData%\BlueCaster`. The tool refuses to install or update
  from the service folder itself. Stage 4's `--install-service` and
  `--uninstall-service` now run install and remove the same way. Exit
  codes: 0 done, 1 failed (everything undone), 2 bad arguments, 5 not
  elevated, 20 a file to copy is missing, 21 a program is not signed by
  the tool's publisher, 22 the service folder or something in it is a link,
  23 a copy failed, 24 the service could not be registered, stopped,
  started or deleted, 25 run from the service folder, 1060 nothing to
  update.
- **Signatures.** When the tool itself is signed (a release), every file it
  copies must carry a valid signature from the same certificate, which
  means the same SHA-256 certificate thumbprint (`win-sys/src/authenticode.rs`).
  The tool checks each file through a handle that denies writes, and
  copies the bytes it read through that handle. Revocation is not checked.
  An unsigned tool (a developer build) copies without checking and says so
  in its output. A tool with a broken signature copies nothing. The
  licence files are compared, not signed. The agent applies the same check
  to the window it starts. `installer/build-release.ps1` signs all three
  programs with one certificate (`installer/README.md`).
- **The version rule.** The app and the service must be the same version.
  The service's version is the file version of its core
  (`ipc::service_state::service_state`). In the handshake, the service
  refuses a window of another version (`core/src/server.rs`):
  `ipc.service_needs_update` when the service is older, and
  `ipc.app_older_than_service` when the app is older. The window then
  shows both versions. One administrator prompt updates the service
  (`--service-setup update`). The service does not check the version of
  other pipe clients. Setup updates only the per-user installation and
  never elevates, so after every app update the service needs an update
  too.
- **What the trust checks compare.** For the real service, every check
  requires the service folder's core, never "the core beside me"
  (`ipc::service_state::expected_core`). The window and the elevated
  helper check that the pipe's server is the process the service control
  manager runs, in session 0, from that program
  (`ipc::service_state::check_service_pipe`). The service starts an agent
  only when the service itself runs from the service folder
  (`core/src/agent.rs`, `agent_program`). An elevated caller counts as
  BlueCaster's only when its program is the service folder's core
  (`core/src/rules.rs`, `elevated_programs`). An elevated window from the
  user's folder does not count, because software running as the user could
  replace that file. A test instance (`BLUECASTER_INSTANCE`) keeps the old
  check against the core beside it.
- **Per-user core or service.** While the service is not added, the window
  starts and uses the per-user core as before. Once the service is added,
  the window uses only the service pipe and never starts a core of its own
  beside the service (`ui/src-tauri/src/connection.rs`). If the service is
  stopped or has another version, the window shows a status instead. While
  the tool adds or removes the service, the window waits for the new core
  instead of closing when the old core sends Bye.
- **Finding the window.** The agent finds the user's `bluecaster.exe`
  through the folder that Setup's uninstall entry names
  (`ipc::service_state::UNINSTALL_KEY`, `InstallLocation`,
  `session-host/src/host/shell.rs`).
- **Setup.** Setup never elevates and never touches the service. `--quit`
  reaches only the user's own core. Restart Manager closes only programs
  that use the per-user folder's files, which here means the window. The
  uninstaller removes the service first, with one administrator prompt. If
  the user declines the prompt or the removal fails, the uninstall stops
  before removing anything (`installer/BlueCaster.iss`, `RemoveService`).
- **Other pipe clients.** The service pipe, its access rules and the new
  errors are documented in [Integration (IPC)](integration#the-service-pipe).

Still to do:

1. Run the test plan below on a test PC. None of the service code has run
   as a Windows service yet, and none of the set-up tool's privileged steps
   have run.
2. Test the input helper as SYSTEM on real hardware: the sign-in screen,
   the lock screen, a UAC prompt and fast user switching.
3. Sign the releases. This needs a certificate that Windows trusts. Until
   then, releases carry no Authenticode signature, and the set-up tool
   copies their files without checking them.
4. Driver records. In service mode the core keeps the adapters' driver
   records in the service's folder, and the uninstaller cannot read that
   folder. The uninstaller then shows **Restore the original Bluetooth
   driver** only in its no-record form, which is off by default.
   `--remove-data` deletes the records. Decide whether `--service-setup
   remove` restores those adapters first.
5. Decide what Windows users on the same PC share. Today, the core moves
   only the first user's data, and every user sees the same devices.
6. Decide whether to split the service into a low-privilege process for the
   Bluetooth stack and a small LocalSystem launcher. Today, one LocalSystem
   service does both, so the stack handles radio data as SYSTEM.
7. Decide whether headphones may reconnect after Exit while the tray icon
   is hidden.

### Testing the service by hand

None of these steps have been run yet. Use a test PC or a virtual machine
with a spare adapter, and keep a USB keyboard and mouse connected the whole
time. On a virtual machine, take a snapshot first. Restoring the snapshot
is the quickest undo for every step.

Use one build for every program. A signed set-up tool copies only files
signed with the tool's own certificate. An unsigned tool (a developer
build) copies without checking.

These commands show the service's state without changing it:

```
sc qc BlueCaster
sc queryex BlueCaster
sc qfailure BlueCaster
icacls "%ProgramFiles%\BlueCaster Service"
```

The service's log is in `%ProgramData%\BlueCaster\logs`, which only an
elevated prompt can read. Setup and the uninstaller write their logs to
`%TEMP%\Setup Log <date> #<n>.txt`.

To remove the service by hand at any point, run
`bluecaster-core.exe --service-setup remove` from an elevated prompt in
the per-user folder (`%LOCALAPPDATA%\BlueCaster`). The copy in the
service folder removes the service too, but cannot delete its own
folder. If the tool fails, run `sc stop BlueCaster` and `sc delete BlueCaster`, then
delete the service folder. To remove the service's data as well, add
`--remove-data` or delete `%ProgramData%\BlueCaster`. The user's own
folder is not affected.

### Test plan

This is the shortest sequence that covers everything not yet run. Each
step says what to look at and how to undo it. Step 9 needs two builds with
different versions: install the older one in step 1, and run the newer
one's setup in step 9. The version comes from `Cargo.toml` and
`app/src/ui/package.json`, so two local builds with different numbers work
too.

1. **Install the per-user app** (the older build). Look at: nothing asks
   for administrator rights, `sc query BlueCaster` reports that the
   service does not exist, and there is no `BlueCaster Service` folder.
   Undo: uninstall.
2. **Add the service.** Turn on developer mode. On **Developer**, in
   **Bluetooth keyboards and mice**, click **Add** in the row **Also on the
   sign-in screen and in administrator windows**, then confirm. Look at:
   exactly one UAC prompt, for `bluecaster-core.exe` from the per-user
   folder. Afterwards the row shows **Added**. `sc qc BlueCaster` shows
   `AUTO_START`, `LocalSystem` and the program in the service folder. The
   folder holds `bluecaster-core.exe` and `bluecaster-driver.exe` and no
   `bluecaster.exe`. `icacls` shows full access only for SYSTEM and
   Administrators. In each file's **Properties > Digital Signatures**, the
   signer is the same as on the per-user files (signed builds only). The
   window reconnects, and the agent's tray icon appears. Headphones still
   play. Undo: step 11.
3. **Turn on keyboards and mice and add one.** Turn the feature on (code,
   both check boxes, the wait, then one UAC prompt). Add a Bluetooth
   keyboard and a mouse. Type in Notepad. Look at: the text appears in
   Notepad, and the mouse moves the pointer. Undo: turn the feature off.
4. **Administrator window and UAC prompt.** Open an elevated Command
   Prompt and type in it. Open any program that shows a UAC prompt, and
   answer the prompt with only the Bluetooth keyboard and mouse. Look at:
   input works in both. Undo: close them.
5. **Lock.** Press Win+L with the USB keyboard, then unlock with the
   Bluetooth keyboard only. Look at: the lock screen takes input, and no
   key stays held after unlocking. Undo: none needed.
6. **Sign out and sign in.** Sign out. On the sign-in screen, use only the
   Bluetooth keyboard and mouse. Look at: input works before anyone signs
   in. After signing in, the tray icon appears and the window connects.
   The service's log shows the agent starting in the new session. Undo:
   none needed.
7. **Restart to the sign-in screen.** Restart Windows. Sign in with the
   Bluetooth keyboard only. Look at: `sc queryex BlueCaster` shows
   `RUNNING` with a process ID, input works on the sign-in screen, and
   headphones reconnect after sign-in. Undo: none needed.
8. **Fast user switching.** Create a second local user. Start playback.
   Switch to the second user (Win+L, then **Switch user**) and sign in
   with the Bluetooth keyboard. Look at: the first user's stream stops,
   input works for the second user, and the second user's window connects
   to the service (or shows its own state if BlueCaster is not installed
   for that user). Switch back. Look at: no key stays held. Undo: delete
   the second user.
9. **Update with a version mismatch.** As the first user, run the newer
   setup. Look at: Setup asks for no administrator rights. Setup closes
   the window but leaves the service running: `sc queryex BlueCaster`
   shows the same process ID as before. Input keeps working during and
   after Setup. When the window starts again, it says that the service
   needs an update and shows both versions. Nothing plays yet. Click
   **Update** and choose **No** in the UAC prompt. Look at: nothing
   changes, and the window still asks for the update. Click **Update**
   again and choose **Yes**. Look at: one UAC prompt, then the window
   connects and playback works. The service folder's `bluecaster-core.exe`
   has the new version (**Properties > Details**). Undo: restore the
   snapshot, or remove the service and install the older build again.
10. **Held key and a killed helper.** Hold down a key on the Bluetooth
    keyboard, and end `bluecaster-core.exe --input-helper` in Task Manager
    run as administrator. Look at: the key is released when the next
    helper starts. Then run `sc stop BlueCaster`. Look at: the service
    stops within a few seconds, and no key stays held. Undo:
    `sc start BlueCaster`.
11. **Remove from the app.** Click **Remove**, leave **Also delete the
    service's settings and pairings** off, and confirm. Look at: one UAC
    prompt. `sc query BlueCaster` reports that the service does not exist,
    the service folder is gone, `%ProgramData%\BlueCaster` remains, and the
    window starts and uses the per-user core again. Undo: step 2.
12. **Uninstall with the service present.** Add the service again (step 2).
    Uninstall BlueCaster in **Settings > Apps**. Look at: the uninstall page
    lists **Remove the BlueCaster service**, checked, above the other
    choices. With **Delete user data** checked, the data line also names
    `%ProgramData%\BlueCaster`. First click **Uninstall** and choose **No**
    in the UAC prompt. Look at: a message says that BlueCaster was not
    uninstalled, and the app, its files and the service all remain. Then
    uninstall again and choose **Yes**. Look at: one UAC prompt for the
    service, the window closes, the service and its folder are gone, and
    `%ProgramData%\BlueCaster` is gone too if **Delete user data** was
    checked. **Restore the original Bluetooth driver** works as it does
    without the service, except for adapters assigned while the service
    ran (see "Still to do", item 4). Undo: install again.

Ctrl+Alt+Del from a Bluetooth keyboard works only if a Windows policy lets
services send it, and BlueCaster never changes that policy. The policy's
value name and numbers in `win-sys/src/input_desktop.rs` and in "The input
helper" are unverified. Check them against Microsoft's documentation before
relying on them.
