---
title: Adding Chip Support
layout: default
nav_order: 4.5
---

# Adding Chip Support

BlueCaster drives the Bluetooth adapter directly (WinUSB), so it must do what the vendor's Windows driver does to get the adapter working; on many chips, that means loading firmware at every start. Realtek, Intel, Broadcom and CSR adapters work today, as do the MediaTek MT7925, MT7927, MT7922, MT7921, MT7902 and MT7663 and the Qualcomm QCA61x4, QCA9377, WCN6855, WCN785x and AR30xx (the MT7927, MT7922, MT7902, MT7663 and the Qualcomm chips are not yet tested on hardware). The other MediaTek chips (older ones) and Qualcomm chips (QCA6290) do not work yet, because nobody has written their loader. For another MediaTek chip, start from the MT79xx loader in `app/src/crates/bt-host/src/mediatek.rs`.

This page explains how to add a loader. You need an adapter with that chip; the maintainers do not have one. Even if you do not write code, you can help: steps 1 and 2 produce the report a contributor needs.

{: .note }
The [developer note](chip-bringup-dev) covers the design behind this and what is still open.

## Table of contents
{: .no_toc .text-delta }

1. TOC
{:toc}

---

## What you need

- The adapter, plugged into a Windows 10 / 11 PC
- BlueCaster 2.0.2 or later (the steps below only need the app)
- To write the loader: a build setup as in [Building](building), and [USBPcap](https://desowin.org/usbpcap/) with [Wireshark](https://www.wireshark.org/) to record what Windows' own driver does

## Step 1: Give the adapter to BlueCaster

1. **Settings > Developer mode**: on
2. **Adapters**: on the adapter's card, **Replace with WinUSB driver** (Windows asks for administrator permission). For a chip BlueCaster cannot use yet, this button is only enabled in developer mode
3. If the card says **Replug or restart Windows**, do that

While the adapter uses WinUSB, Windows' own Bluetooth cannot use it. On a laptop whose only Bluetooth is that chip, keyboards, mice and headsets connected through Windows stop working until you restore the driver with **Restore original driver** on the same card.

## Step 2: Read what the chip says (chip probe)

In the **Firmware** section of the adapter's card, click **Read chip info**. The Bluetooth stack stops for a moment, the probe runs, and Explorer opens the report, `probe-<date>_<time>.json` in `logs\probe\` in the settings folder (`%APPDATA%\BlueCaster`). The newest 10 are kept.

The probe only reads. It sends no HCI Reset and no vendor command:

| Request | What it shows |
|:--|:--|
| Read Local Version Information | HCI / LMP version, the manufacturer (company ID), LMP subversion: often the ROM or firmware version |
| Read BD_ADDR | The adapter's address (zero or a placeholder on some chips before their firmware runs) |
| Read Local Supported Commands / Features, Read Buffer Size | What the running code offers: a ROM often offers much less than the firmware |
| Read Local Name | Some ROMs name themselves |
| LE Read Local Supported Features, LE Read Buffer Size | The same for LE |
| USB device descriptor, manufacturer and product strings | Standard USB requests on the default pipe |

It also records the USB configuration Windows reports (interfaces, endpoints) and any event the chip sent unprompted. A request without an answer is recorded too: a chip that answers nothing until its firmware is loaded is a finding in itself.

From the command line, with the adapter's VID:PID or instance ID (the adapter card shows them):

```bat
"%LOCALAPPDATA%\BlueCaster\bluecaster-core.exe" --cli -u 0e8d:7961 --probe
```

Quit BlueCaster from the tray first, since only one of them can use the adapter at a time.

**Sending a report**: after **Read chip info**, the adapter's card shows what was read and a **Report on GitHub** button. The button opens a new issue with the version, the adapter, that summary, your Windows build and the report itself already filled in. Add where the adapter came from (for a laptop's built-in adapter, the laptop model).

If the report is too long for the issue, the **Probe report** field stays empty and Explorer shows the file instead. Drag the file into that field; GitHub won't create the issue without it. The reports are in `%APPDATA%\BlueCaster\logs\probe`, and the newest one has the latest date in its name. The report contains the adapter's Bluetooth address and name; it does not contain anything about your headphones. A **diagnostic bundle** (Diagnostics screen) with HCI captures included also contains the probe reports, under `probe/`.

## Step 3: Record what Windows' driver does

A probe shows the chip's state, not how to change it. The vendor's Windows driver knows how: record it once, and the loader can repeat the same steps.

1. **Restore original driver** on the adapter's card, so Windows' driver takes it over again
2. Install USBPcap and Wireshark; start Wireshark as administrator and capture the USBPcap interface the adapter is on. `USBPcapCMD.exe` (in USBPcap's folder) lists the devices on each root hub; a laptop's built-in adapter sits on an internal hub, often listed only as "USB Composite Device" or by its VID:PID. When unsure, capture every USBPcap interface at once
3. Make the driver load the firmware: unplug and replug the adapter; for a built-in one, **Disable device** and **Enable device** in Device Manager. Firmware that survives a warm restart may load only after a full shutdown
4. Stop the capture once the adapter shows in Windows' Bluetooth settings. Find the adapter's device descriptor (its VID:PID) among the first packets, filter on its address (`usb.bus_id == <bus> && usb.device_address == <address>`) and save the capture

What to look for, in order:

- **Control transfers** before any HCI command: `bmRequestType` 0x40 / 0xC0 (vendor, OUT / IN) with their `bRequest`, `wValue`, `wIndex`. These often read a chip ID or reset part of the chip
- **HCI commands** (class requests, `bmRequestType` 0x20) with opcodes 0xFC00 to 0xFFFF: vendor commands. A long run of the same opcode with large parameters is the firmware being sent; the replies show how each part is acknowledged
- **Events** that are not Command Complete / Status: vendor events (code 0xFF, or a vendor's own code such as 0xE4), for example "firmware started", or the acknowledgment of each firmware part in place of a Command Complete
- **Bulk OUT** data before the first HCI Reset: some chips take the firmware on the ACL pipe
- Where the file comes from: compare the data sent with the files in the driver's folder (Device Manager > driver details, or `C:\Windows\System32\DriverStore\FileRepository\`) and with the chip's files in [linux-firmware](https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/tree/) (`mediatek/`, `qca/`), which may be redistributed and which BlueCaster users can download

Note any delay the driver waits between steps (see the capture's timestamps): a chip that is still resetting does not answer.

{: .warning }
Work from captures, probe reports and vendor documentation. Do not copy or closely follow GPL code, such as the Linux kernel's `btmtk` or `btqca` drivers: BlueCaster is licensed under Apache-2.0 or MIT, and a loader derived from them could not be included.

## Try it without a build: chip scripts {#chip-scripts}

Before writing a loader, try the captured sequence on the adapter with a **chip script**: the sequence as text, one request per line, run in place of the Bluetooth stack. Someone without the adapter can write a script and send it to someone who has one; the result comes back as a report, with no new build.

**Running a script**: in developer mode, open **Developer > Chip**, choose the adapter (on WinUSB), type the script or open a file, and click **Run**. Each line's answer appears below, and **Show Report** opens `script-<date>_<time>.json` in `logs\probe\` (which includes the script). From the command line: `bluecaster-core --cli -u <adapter> --script <file>`.

```text
# one request per line; numbers are hexadecimal (0x optional) except wait, timeout= and chunk=
hci 1001                           # an HCI command: opcode, then parameter bytes
hci fc05 ff
bulk fc09 00 01 02                 # the same on the bulk OUT pipe
usb in vendor device 63 0 0 4      # control IN: type, recipient, request, value, index, length
usb out vendor device 64 0 0 01    # control OUT: ..., then the data bytes
wait 100                           # milliseconds
expect e4 02 01 timeout=5000       # an event: its code and its first parameter bytes
bootloader off                     # bootloader mode (on from the start)
upload mediatek/patch.bin fc6f 01 01 chunk=240 ack=event:e4:02,01
upload-bulk patch.bin fc09 01      # the same on the bulk OUT pipe
```

- `hci` and `bulk` wait for the command's Command Complete or Command Status (2 s) and record it, whatever its status
- `upload` sends a file of the firmware folder in commands of that opcode, each with the header bytes and up to `chunk` bytes of the file (default 240), and waits for each acknowledgment: `ack=complete` (default: a Command Complete or Command Status of the opcode with status 0) or `ack=event:<code>[:<bytes>]`. An error status stops it
- The script stops at the first line that fails (no answer, an error, a missing file) and says which
- Events that answer no line are kept in the report too

**Safety**: only requests known to be read-only run without asking: the standard HCI reads (informational and status parameters, Read Local Name, the LE reads), standard USB descriptor requests, `wait`, `expect` and `bootloader`. A script with any other line runs only when you allow it (**Run them** under the list of those lines; `--allow-writes` on the command line). Vendor commands that can write memory a chip keeps across power cycles never run, whatever you allow (`denied_opcodes` in `app/src/crates/bt-host/src/chips.rs`: CSR's BCCMD so far). Bootloader mode is on from the start, so closing the adapter afterwards sends no HCI Reset. Firmware loaded into a chip's RAM is gone after a replug.

## Step 4: Write the loader

From here on, paths are relative to the repository root. Three crates matter:

| Crate (package) | Folder | What is there |
|:--|:--|:--|
| `bt-host` | `app/src/crates/bt-host` | The Bluetooth host: loaders, the probe, chip detection |
| `bluecaster-core` (library) | `app/src/crates/core` | Settings and adapters: which loader runs |
| `bluecaster-core-exe` | `app/src/core` | The service executable (`bluecaster-core.exe`): the window's requests, adapter notes |

The last two both have "core" in their name; `cargo -p` takes the package names above.

A loader is a state machine with no I/O of its own: it receives the controller's events, the results of its USB requests and its timer, and returns **actions** for the app to carry out. This makes the loader testable without the chip and keeps USB and threading details out of it.

1. Copy `app/src/crates/bt-host/src/bringup/sample.rs`, a complete loader for a made-up chip with its tests, to `app/src/crates/bt-host/src/<vendor>.rs` (for example `mediatek.rs`). Add `pub mod <vendor>;` to `app/src/crates/bt-host/src/lib.rs`, in alphabetical order with the other modules
2. Keep the names: the type is `Loader` (used as `<vendor>::Loader`), `vendor()` returns the vendor's name. Keep the tests in the file, as the sample does
3. Replace everything the sample marks "made up" with what steps 2 and 3 showed: the version check, the USB request, the file name, the download command and how it is acknowledged, how the firmware is started
4. Rewrite the module comment at the top: what the chip needs, the steps in order, and where each fact comes from (your capture of Windows' driver, vendor documentation, probe reports). Do not cite other drivers' source code as the source of a step

### The interface

Everything is in `app/src/crates/bt-host/src/bringup.rs`.

| `ChipLoader` method | Called |
|:--|:--|
| `vendor()` | For log lines ("MediaTek") |
| `start()` | Once, on the open adapter, before HCI init |
| `on_event(event)` | For every HCI event: code, length, parameters |
| `on_control(result)` | With the result of the last `UsbControl`: the bytes read, or why it failed |
| `on_timeout()` | When the timer of the last `Timer` fires |
| `missing_file_hint(file)` | For the log when the loader ended with `MissingFile`: what to put where ("x.bin from linux-firmware (mediatek/)") |
| `step()`, `progress()` | After each batch of actions: the step's name ("download") and how far along it is (commands sent out of the total), for the Adapters screen while the adapter starts |

After `Done` the app drops the loader: a late answer never reaches it.

| `Action` | Does |
|:--|:--|
| `Command(bytes)` | Sends an HCI command (opcode, length, parameters) on the control pipe; logged in the HCI capture |
| `BulkCommand(bytes)` | Sends a command, the same bytes as for `Command`, on the bulk OUT pipe, where some bootloaders take the firmware |
| `UsbControl(request)` | A control transfer on the default pipe (vendor registers); runs immediately, and `on_control` gets the result before the next action. Logged in the HCI capture as a note |
| `Bootloader(on)` | For a chip whose loader answers on the bulk IN pipe, or must not get an HCI Reset (some hang on one); see the patterns. A ROM chip that answers HCI normally does not need it |
| `Timer(ms)` | Starts or restarts the loader's single timer |
| `Done(Ok(outcome))` | Finished: HCI init follows. `AlreadyRunning`, `Loaded { firmware, detail }`, `Rom` (`Probed` is the probe's own) |
| `Done(Err(error))` | The adapter cannot be used: `MissingFile(name)`, `BadFile { file, why }`, `Command { opcode, status }`, `Timeout(step)`, `Rejected(code)`, `BadVersion`, `UnsupportedVariant(v)`, `Unsupported(why)`, `Host(why)` |

Helpers, so a loader does not parse bytes by hand:

- `hci_command(opcode, params)` builds a command
- `parse_event(event)` gives `HciEvent::Complete { opcode, status, ret }` (return parameters after the status), `Status { opcode, status }`, `Vendor(params)` (event 0xFF) or `Other { code, params }` (any other code, including a vendor's own, such as 0xE4)

**Firmware files**: the loader gets a `FileReader` and asks it for a file **name without a folder** (`"BT_RAM_CODE_MT7961_1_2_hdr.bin"`). The app looks in the firmware folder (`%APPDATA%\BlueCaster\firmware`) and in the vendor's subfolder there (`firmware\mediatek\`), as linux-firmware lays them out; step 6 sets that subfolder.

### Patterns

The sample shows each of these; drop what your chip does not need.

- **One command, one acknowledgment**: send, start the timer, and in the waiting state accept only the acknowledgment of that opcode: a Command Complete, or a Command Status with status 0 for a command whose result comes later. Anything else is ignored, so a late answer does not advance the loader
- **Acknowledged by a vendor event**: some chips answer each firmware part with their own event instead of a Command Complete. Wait for that event alone (its code and the parameters that identify it), ignore a Command Complete or a status-0 Command Status of that command in that state, but still end on an error status in either. The sample treats a Command Complete and a Command Status as the same acknowledgment, so for this its `on_event` needs a match on both the state and the event
- **Several files** (a patch, then a configuration): load and check all of them before sending any, send them in order, and name the last one in `Loaded { firmware, .. }` (the log lines name the others)
- **A deliberate pause** (the driver waits 100 ms before the next step): add a state for it and send `Timer(100)`; in that state, `on_timeout` sends the next step instead of failing. In the tests, `SimRun::timeouts` counts these pauses too
- **A command answered twice** (an acknowledgment, then a vendor event such as "firmware started"): two states, the second with its own timer. The sample's `Launching` and `Starting` do this. When the event carries a result code, a bad one ends the loader with `Rejected(code)`
- **An error status** in a Command Complete or Command Status ends the loader with `Command { opcode, status }`
- **Bootloader mode**: `Bootloader(true)` before the first command, even before the version check. On success (`Loaded` and also `AlreadyRunning`) send `Bootloader(false)` just before `Done`, so HCI init works normally; after a failure leave it on, as the chip is still in its bootloader. The sample's `finish()` does this
- **Already running**: when the version or a register shows the operational firmware, finish with `AlreadyRunning` at once. The adapter keeps its firmware until it loses power, and a second download may fail
- **Check the file first**: parse and check the whole file before sending any of it (`BadFile` with the reason), and name the exact file that is missing
- **Only your chip**: when the loader covers some of a vendor's chips, check that the adapter is one of them before sending anything (its own chip ID register, or the USB IDs with `ControlRequest::device_descriptor()`), and end with `Unsupported` otherwise
- **Log** what you learn from the chip at `info` (chip ID, ROM version, the file chosen): these lines are what reports show
- Never write to the chip's flash or OTP memory, and read files only through the `FileReader`

## Step 5: Test it against a simulated controller

The tests use `app/src/crates/bt-host/src/bringup/sim.rs`. You write a small simulated chip (`impl SimController`) that answers commands and control transfers the way the real one did in your capture, and `sim::run(&mut loader, &mut chip)` drives the loader exactly as the app does, including the timer. The result (`SimRun`) has the outcome, every command sent with its pipe (`commands[i].bulk`), every control transfer, bootloader mode at the end (`bootloader`), and how often the timer fired. `sim::command_complete`, `command_status`, `vendor_event`, `event` (any code), `opcode` and `params` build and read the bytes; answer bulk commands in `bulk_command`.

Cover at least, as the sample does:

- the full download, checking the commands sent (count, order, sizes, pipe)
- bootloader mode at the end of every case, if the loader uses it
- firmware already running
- a missing file, and a broken file (nothing of it sent)
- a rejected command (error status), a silent controller (timeout), and each wait that can time out
- a failed USB transfer, if the loader uses one

Bytes copied from your capture make the best test data. Run the tests with:

```bat
cargo test -p bt-host
```

## Step 6: Hook it into the app

These are all the files to change, for a vendor already listed as unsupported (MediaTek, Qualcomm) or a new one. For a new vendor, the compiler points to the `match`es that need its variant; for everything else, including all changes for an already listed vendor, only this list helps.

**One chip or the whole vendor?** A loader usually covers only some of a vendor's chips (the MT7921 but not the MT7925). Then only those chips may lose the "unsupported" status: list their VID:PIDs in `LOADER_CHIPS` in `app/src/crates/bt-host/src/chips.rs`, and leave the vendor in `listed_chips_only()`. `chips::chip_supported(vendor, vid, pid)` and `chip_support_tag()` then answer per adapter, and the adapter's name tag, its note on the Adapters screen, whether that screen offers WinUSB, and the "not implemented" warning all follow them. Take a vendor out of `listed_chips_only()` only for a loader that covers all its chips (as Intel's does).

{: .note }
Keep each file's line endings: some source files use CRLF, others LF. Some editors and `sed -i` in Git Bash rewrite them silently, and `cargo fmt` does not restore them. `git diff --stat` reveals this: a file you changed in a few places shows every line as changed. If so, convert the file back (for example with VS Code's line ending switch in the status bar) before committing.

**Detection** (`app/src/crates/bt-host/src/chips.rs`):

1. `ChipVendor`: a new vendor gets a variant (with a doc line) and an arm in `name()`. For any vendor, update the enum's doc comment (which vendors have loaders) and the variant's doc line ("not supported")
2. Some chips: their VID:PIDs in `LOADER_CHIPS`, and a test in the loader's module that its own chip check (chip ID, version) accepts exactly what those entries are. The whole vendor: take it out of `listed_chips_only()`
3. `vendor_for_usb()`: the vendor's USB VIDs. A VID other makers use too must be matched as `VID if pid == PID`, never as a whole: laptop modules under the module maker's VID (AzureWave 0x13D3, Foxconn 0x0489), and shared VIDs such as pid.codes 0x1209. Every VID listed here also makes `is_bluetooth_vendor_vid` treat that maker's devices as Bluetooth adapters. A VID:PID in BTstack's Realtek table or in `OEM_TABLE` (both in this file's `resolve_realtek_pid`) is treated as Realtek before `vendor_for_usb` is consulted: check that yours is in neither
4. `company_name()`: the Bluetooth SIG company ID from Read Local Version, if it is not there yet
5. Tests: `app/src/crates/bt-host/tests/chips.rs` (the VID:PIDs and the tag)

**Opening the adapter**: Bluetooth-class devices (class E0/01/01) open without changes. A chip that reports a vendor class (FF) in its ROM also needs its VID:PIDs in `BUILTIN_VENDOR_CLASS` in `app/src/crates/core/src/adapter.rs`; the probe report's `usb.interfaces` line shows the class.

**Starting the loader**, as Intel does (search for `intel_loader`):

6. `app/src/crates/bt-host/src/lib.rs`: a `HostConfig` field like `intel_loader` (with its default in `HostConfig::new`, the destructuring in `BtHost::launch`, and the debug line), and the branch in `BtHost::launch` that installs the transport and calls the app's start function instead of `setup_hci_and_power_on`, only when not on H4 (`&& !tcp`, as Intel's does: the emulator's controller is not a real chip). Only one loader runs per start: when two vendors' flags are set (both adapters on WinUSB), the first branch wins, so test with only your adapter on WinUSB
7. `app/src/crates/bt-host/src/app.rs`: a `start_<vendor>_loader` like `start_intel_loader` (with `use crate::<vendor>;`), giving the loader `read_firmware_file(&dir, "<vendor>", name)` (the subfolder name as in linux-firmware) and calling `start_loader`
8. `app/src/crates/core/src/adapter.rs`, `configure()`: set the field when an adapter of the vendor with `chips::chip_supported(a.vendor, a.device.vid, a.device.pid)` is on WinUSB and not in use (as `config.intel_loader`, with Intel's "in use by another program" log for the others), and add it to the `adapter:` debug line there. The "firmware loading for this chip is not implemented" warning below it already follows `chip_supported`
9. `app/src/crates/core/src/service.rs`, `host_config()`: add the field to the debug line

**What the window shows**:

10. `app/src/core/src/state.rs`, `vendor_note()`: an arm with a new key `firmware.vendor_<vendor>`, taken out of the `MediaTek | Qualcomm` arm, and its test. Before that arm, the function returns `firmware.vendor_unsupported` for chips that `chip_supported` rejects, so the vendor's other chips keep that note. The Adapters screen offers WinUSB to every adapter whose note is not `firmware.vendor_unsupported`
11. `app/lang/*.json` (all 10 languages, same line in each): the new key, like the Intel one ("Intel adapter. …"). Where it names a button or folder, use that language's own words from the same file (the firmware folder's button is `firmware.open_folder`). Whole vendor: take its name out of `firmware.vendor_unsupported`. Some chips: leave it there, as the vendor's other chips still show that text
12. `app/src/ui/src/lib/links.ts`: the vendor's firmware page (in `LINKS`, `firmwareUrl()` and `vendorFirmwareUrl()`, like Intel's linux-firmware folder), so the Adapters screen shows where to get the files; tests in `app/src/ui/src/lib/chips.test.ts`. The Adapters screen shows the page only for adapters whose note is not `firmware.vendor_unsupported`, so the vendor's other chips do not get it

**Docs**:

13. The chip table in `docs/setup.md` and `docs/*/setup.md` (all languages): a row like Intel's, with the chips it covers, the firmware files and where they go (the firmware folder: `%APPDATA%\BlueCaster\firmware`, or its `<vendor>` subfolder; older rows still say "config folder"). Name the window's buttons as each language's `app/lang/*.json` has them (`ui.nav.adapter` for the Adapters screen, `firmware.open_folder` for the button that opens that folder). Whole vendor: take it out of the "Not supported" row. Some chips: keep the vendor there and name the chips still not supported. Add the vendor to the sentence at the top of the Firmware section ("Intel and Broadcom adapters may need firmware files of their own")
14. [Verified Hardware](hardware) (`docs/hardware.md`) once someone has used it, and the opening of this page (`docs/chip-support.md`, all languages), which names the vendors without a loader (some chips: "the other <vendor> chips")

## Step 7: Try it on the adapter

Build in the repository root (`cargo build --release -p bluecaster-core-exe`), quit the installed app (from its tray icon), and run the core from the command line with an HCI capture:

```bat
target\release\bluecaster-core.exe --cli -u 0e8d:7961 --scan 5 --hci-capture loader.pklg
```

The log shows each step; `loader.pklg` opens in Wireshark with every command, event and control transfer in order. Unplug the adapter (or shut down) and try again: the download must work on a cold chip and do nothing when the firmware is already running.

{: .note }
With several adapters on WinUSB, BlueCaster prefers a Realtek one and applies its chip setup, even with `-u`. Set **Adapters > Bluetooth Chip** to **Not Realtek**, or test with only your adapter on WinUSB.

Then stream to headphones for a while with the window, and check that **Restart Bluetooth** and a replug both bring the adapter back. While the adapter starts, the **Adapters** screen shows the loader's state under the Bluetooth status ("loading 42%", or a failure); in developer mode it stays visible after the adapter is ready, with your `step()` names and the outcome.

## Step 8: Open the pull request

Before opening it, run these checks in order (see [Building](building) for the setup). Start with the UI, in `app\src\ui`: this checks your change from step 6 (item 12), and the Rust workspace includes the window's crate, which needs the built UI:

```bat
pnpm install
pnpm build
pnpm exec tsc --noEmit
pnpm exec vitest run
```

Then in the repository root:

```bat
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python tools/check_lang.py
```

Without Node.js, run the tests and clippy on the three crates above instead (`-p bt-host -p bluecaster-core -p bluecaster-core-exe`), leave the UI checks to the pull request's CI, and say so in the pull request. `pwsh ./build.ps1 -Test` runs everything.

Clippy also checks names: a `State` variant called `PatchState` fails `enum_variant_names`; call it `Patch`. `check_lang.py` may report "long hints left" that were there before your change; only errors and missing keys are yours to fix.

Other places mention MediaTek or Qualcomm as unsupported in passing (the UI's mock data and tests, comments in `Adapter.tsx` and `probe.rs`, the "Not Realtek" choice): leave them as they are.

In the pull request:

- Base it on the current `develop-v*` branch
- Say which adapters you tested (product, VID:PID, chip), cold start and warm, and attach a probe report and the HCI capture of a download
- Say where the facts in the loader come from
