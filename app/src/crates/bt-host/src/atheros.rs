//! Qualcomm Atheros AR30xx USB patch and system configuration download
//!
//! The Qualcomm Atheros Bluetooth 3.0 / 4.0 USB chips (AR3011, AR3012 and
//! the Bluetooth part of the Wi-Fi combos the same driver serves) start in
//! their ROM. Before they are used, the host sends a patch for that ROM and
//! the system configuration ("ramps") of the board. All of it goes over USB,
//! not HCI:
//!
//! - vendor requests on the default pipe, to the endpoint recipient with
//!   index 0 (bmRequestType 0xC2 / 0x42): 0x05 reads a state byte (bit 7:
//!   the patch is in, bit 6: the system configuration is in; low 6 bits
//!   0x0B or 0x0C: the driver loads nothing), 0x09 reads 20 bytes of
//!   version (ROM, build and RAM version as u32, the reference clock at 12,
//!   the clock class at 16, 0x23 at 17 on a "TF" chip), 0x0B resets the
//!   chip (warm reset, no data)
//! - a download is one file: its 20-byte header in vendor request 0x01
//!   (OUT), 100 ms, then the rest on the bulk OUT pipe (endpoint 0x02):
//!   three transfers of 320 bytes, then the rest at once, 10 ms after each.
//!   100 ms later the state byte tells whether the chip took it. The bulk
//!   parts go out with [`Action::BulkCommand`], which sends its bytes as they
//!   are and does not wait for them, so the state is read until it shows the
//!   file.
//!
//! The order: state, 10 ms, version, 10 ms, the patch, the system
//! configuration. The patch goes only to a ROM the driver knows, and only
//! when it is newer than the RAM version the chip reports (the file's last
//! 8 bytes: its ROM and RAM version). A chip that has the patch but not the
//! system configuration is reset first when the file is newer (400 ms
//! wait); if the reset cleared the patch, the patch is sent again.
//!
//! Files: `AthrBT_0x<ROM>.dfu` and `ramps_0x<ROM>_<clock>[_0x<class>].dfu`
//! ("TF_" after the first underscore on a TF chip). The header: load
//! address, entry, the length of the rest, a check word, the kind (4: a
//! patch, 2: a system configuration), u32 each.
//!
//! Read from Qualcomm Atheros' Windows driver (btfilter.sys 10.0.3.12 to
//! 10.0.3.22 for 64-bit Windows 10, and the INF of 10.0.1.14), and checked
//! against every .dfu file those packages ship; written without vendor or
//! GPL source. Nothing of it was tried on a chip yet.
//!
//! Not done: the driver resets the USB port and reads once more when the
//! state or version read fails (WinUSB cannot reset the port); OEM file
//! suffixes and the system configuration upgrade the driver takes from the
//! registry (none of its INFs sets them); the full firmware download
//! (AtherosBT.bin) an old 32-bit driver did for chips in state 0x0C.
//!
//! Firmware: the firmware folder first (also its `qca\` subfolder), by the
//! Windows driver's names, then the files Qualcomm Atheros' Windows driver
//! installed (System32\drivers or the driver store, see
//! [`from_windows_driver`]).

use std::path::PathBuf;
use std::time::Instant;

use crate::bringup::{
    Action, ChipLoader, ControlData, ControlKind, ControlRecipient, ControlRequest, FileReader, LoadError, Outcome,
};

/// Vendor requests: a download's header, the warm reset (OUT); the state,
/// the version (IN)
pub const REQUEST_DOWNLOAD: u8 = 0x01;
pub const REQUEST_STATE: u8 = 0x05;
pub const REQUEST_VERSION: u8 = 0x09;
pub const REQUEST_RESET: u8 = 0x0B;
pub const VERSION_LEN: u16 = 20;

/// State bits: the patch, the system configuration is in
pub const STATE_PATCHED: u8 = 0x80;
pub const STATE_SYSCFG: u8 = 0x40;
/// States (low 6 bits) in which the driver loads nothing
const STATE_REFUSED: [u8; 2] = [0x0B, 0x0C];

/// A file's header, sent in the control request
pub const HEADER: usize = 20;
const KIND_PATCH: u32 = 4;
const KIND_SYSCFG: u32 = 2;
/// A patch ends with its ROM and RAM version
const TRAILER: usize = 8;

/// The first bulk transfers of a download are this short, then the rest
/// at once, as the driver sends them
pub const FIRST_CHUNK: usize = 320;
const FIRST_CHUNKS: usize = 3;
const REST_CHUNK: usize = 0x7FFFC;
/// After each bulk transfer
const CHUNK_GAP_MS: u32 = 10;
/// After the state read, and after the version read
const SHORT_WAIT_MS: u32 = 10;
/// After a header; after a file (then the state read that checks it)
pub const SETTLE_MS: u32 = 100;
/// After the warm reset
pub const RESET_WAIT_MS: u32 = 400;
/// State reads before a file counts as not taken. The bulk writes are not
/// waited for, so this also covers the transfer
const MAX_POLLS: u32 = 30;

/// What the driver does for a ROM, by its RAM version: `load` the patch
/// and system configuration at all, `reset` a chip that has the patch but
/// not the system configuration. None: a ROM the driver does not load
fn rom_rules(rom: u32, ram: u32) -> Option<(bool, bool)> {
    match rom {
        0x0102_0001 => Some((true, false)),
        0x0102_0200 => Some((true, ram >= 0x52)),
        0x1102_0000 => {
            let load = !(0x160..=0x17E).contains(&ram) && !(0x2870..=0x288E).contains(&ram);
            Some((load, load))
        }
        0x0102_0201 | 0x1102_0100 | 0x3101_0000 | 0x3101_0100 | 0x4101_0000 | 0x4102_0000 | 0x4102_0100 => {
            Some((true, true))
        }
        _ => None,
    }
}

/// The version the chip reports (request 0x09)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Version {
    pub rom: u32,
    pub build: u32,
    /// The patch's RAM version when a patch is in
    pub ram: u32,
    pub ref_clock: u8,
    pub clock_class: u8,
    /// A "TF" chip (byte 17 is 0x23), which has files of its own
    pub tf: bool,
}

impl Version {
    pub fn parse(d: &[u8]) -> Option<Version> {
        let d = d.get(..VERSION_LEN as usize)?;
        let word = |at: usize| u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
        Some(Version {
            rom: word(0),
            build: word(4),
            ram: word(8),
            ref_clock: d[12],
            clock_class: d[16],
            tf: d[17] == 0x23,
        })
    }

    /// The reference clock (MHz) in the system configuration's name
    fn clock(&self) -> &'static str {
        match self.ref_clock {
            0 => "_26",
            1 => "_40",
            2 => "_19",
            3 => "_24",
            4 => "_16",
            5 => "_37",
            6 => "_38",
            7 => "_48",
            8 => "_52",
            _ => "_0",
        }
    }
}

/// The patch file of a chip, as the driver names it
pub fn patch_name(v: &Version) -> String {
    let tf = if v.tf { "TF_" } else { "" };
    format!("AthrBT_{tf}0x{:08x}.dfu", v.rom)
}

/// The system configuration file of a chip: its clock, and its clock class
/// when that is 1 to 0xFE
pub fn syscfg_name(v: &Version) -> String {
    let tf = if v.tf { "TF_" } else { "" };
    let class = if (1..=0xFE).contains(&v.clock_class) { format!("_0x{:02x}", v.clock_class) } else { String::new() };
    format!("ramps_{tf}0x{:08x}{}{class}.dfu", v.rom, v.clock())
}

fn word(file: &[u8], at: usize) -> Option<u32> {
    file.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A file's header: its kind, and that the rest is as long as it says.
/// Err: why it is not a file of this kind
fn check_file(file: &[u8], kind: u32) -> Result<(), &'static str> {
    let (Some(len), Some(k)) = (word(file, 8), word(file, 16)) else {
        return Err("too short for an Atheros firmware file");
    };
    if k != kind {
        return Err(if kind == KIND_PATCH { "not a patch file" } else { "not a system configuration file" });
    }
    if len as usize + HEADER != file.len() || len == 0 {
        return Err("its length is not the one in its header");
    }
    if kind == KIND_PATCH && len < TRAILER as u32 {
        return Err("too short for a patch");
    }
    Ok(())
}

/// The ROM and RAM version a patch is for (its last 8 bytes)
fn patch_version(file: &[u8]) -> Option<(u32, u32)> {
    let at = file.len().checked_sub(TRAILER)?;
    Some((word(file, at)?, word(file, at + 4)?))
}

fn vendor(request: u8, data: ControlData) -> ControlRequest {
    ControlRequest {
        kind: ControlKind::Vendor,
        recipient: ControlRecipient::Endpoint,
        request,
        value: 0,
        index: 0,
        data,
    }
}

pub fn read_state() -> ControlRequest {
    vendor(REQUEST_STATE, ControlData::In(1))
}

pub fn read_version() -> ControlRequest {
    vendor(REQUEST_VERSION, ControlData::In(VERSION_LEN))
}

fn send_header(header: &[u8]) -> ControlRequest {
    vendor(REQUEST_DOWNLOAD, ControlData::Out(header.to_vec()))
}

fn warm_reset() -> ControlRequest {
    vendor(REQUEST_RESET, ControlData::Out(Vec::new()))
}

/// One file to send, and the state bit that shows the chip took it
#[derive(Debug, Clone)]
struct Download {
    file: String,
    data: Vec<u8>,
    done_bit: u8,
    what: &'static str,
}

impl Download {
    /// The bulk transfers after the header
    fn chunks(&self) -> Vec<Vec<u8>> {
        let body = &self.data[HEADER..];
        let first = body.len().min(FIRST_CHUNK * FIRST_CHUNKS);
        let mut chunks: Vec<Vec<u8>> = body[..first].chunks(FIRST_CHUNK).map(<[u8]>::to_vec).collect();
        chunks.extend(body[first..].chunks(REST_CHUNK).map(<[u8]>::to_vec));
        chunks
    }
}

enum Step {
    Idle,
    /// The state byte asked for
    ReadState,
    /// Waiting before the version read, then reading it
    VersionWait,
    Version,
    /// Waiting before the first request that changes the chip
    Settle,
    /// The warm reset sent, its wait, the state read after it
    Reset,
    ResetWait,
    ResetRead,
    /// A download's header sent in the control request, the wait after it
    Header,
    HeaderWait,
    /// Its bulk transfers: the next one when the timer fires
    Body {
        next: usize,
    },
    /// Its end: a state read when the timer fires, `polls` of them so far
    Check {
        polls: u32,
    },
    Done,
}

pub struct Loader {
    read_file: FileReader,
    state: Step,
    started: Instant,
    chip_state: u8,
    version: Version,
    patch: Option<Download>,
    syscfg: Option<Download>,
    downloads: Vec<Download>,
    /// The download running (index in `downloads`), its bulk transfers
    current: usize,
    chunks: Vec<Vec<u8>>,
    /// Bytes sent of all downloads
    sent: usize,
    total: usize,
    /// The chip has the patch, the file is newer: warm reset first
    reset_first: bool,
    /// Why no patch is sent, for the log line at the end
    no_patch: Option<&'static str>,
}

impl Loader {
    pub fn new(read_file: FileReader) -> Loader {
        Loader {
            read_file,
            state: Step::Idle,
            started: Instant::now(),
            chip_state: 0,
            version: Version::default(),
            patch: None,
            syscfg: None,
            downloads: Vec::new(),
            current: 0,
            chunks: Vec::new(),
            sent: 0,
            total: 0,
            reset_first: false,
            no_patch: None,
        }
    }

    fn finish(&mut self, result: Result<Outcome, LoadError>) -> Vec<Action> {
        tracing::debug!("atheros loader finished after {} ms: {result:?}", self.started.elapsed().as_millis());
        self.state = Step::Done;
        vec![Action::Done(result)]
    }

    /// A file by name, checked
    fn file(&mut self, name: String, kind: u32, what: &'static str, done_bit: u8) -> Result<Download, LoadError> {
        let Some(data) = (self.read_file)(&name) else { return Err(LoadError::MissingFile(name)) };
        check_file(&data, kind).map_err(|why| LoadError::BadFile { file: name.clone(), why })?;
        tracing::info!("atheros: {name}: {} bytes", data.len());
        Ok(Download { file: name, data, done_bit, what })
    }

    /// State and version known: picks and checks every file before sending
    /// anything
    fn plan(&mut self) -> Vec<Action> {
        let v = self.version;
        let (patched, syscfg) = (self.chip_state & STATE_PATCHED != 0, self.chip_state & STATE_SYSCFG != 0);
        let Some((load, reset)) = rom_rules(v.rom, v.ram) else {
            if patched && syscfg {
                // Nothing would be sent anyway
                return self.finish(Ok(Outcome::AlreadyRunning));
            }
            tracing::warn!("atheros: ROM version 0x{:08X} is not one this loader knows", v.rom);
            return self.finish(Err(LoadError::Unsupported("an Atheros chip this loader does not know")));
        };
        if !load {
            tracing::info!("atheros: RAM version 0x{:X} of this ROM needs no download", v.ram);
            return self.finish(Ok(Outcome::Rom));
        }
        if patched && syscfg {
            return self.finish(Ok(Outcome::AlreadyRunning));
        }
        let patch = match self.file(patch_name(&v), KIND_PATCH, "patch", STATE_PATCHED) {
            Ok(d) => d,
            Err(e) => return self.finish(Err(e)),
        };
        let Some((for_rom, for_ram)) = patch_version(&patch.data) else {
            return self.finish(Err(LoadError::BadFile { file: patch.file, why: "too short for a patch" }));
        };
        tracing::info!("atheros: {} is for ROM 0x{for_rom:08X}, RAM version 0x{for_ram:X}", patch.file);
        if for_rom != v.rom {
            return self.finish(Err(LoadError::BadFile { file: patch.file, why: "made for another ROM version" }));
        }
        if !syscfg {
            match self.file(syscfg_name(&v), KIND_SYSCFG, "system configuration", STATE_SYSCFG) {
                Ok(d) => self.syscfg = Some(d),
                Err(e) => return self.finish(Err(e)),
            }
        }
        let newer = for_ram > v.ram;
        if !newer {
            tracing::info!("atheros: the chip's RAM version 0x{:X} is as new as the patch's: not sent", v.ram);
            self.no_patch = Some("the chip's patch is as new as the file's");
        } else if patched {
            self.no_patch = Some("its patch was in the chip already");
        }
        // A chip with the patch in: reset first, if the driver does for its
        // ROM, else only the system configuration
        self.reset_first = patched && newer && reset;
        self.patch = (newer && (!patched || self.reset_first)).then_some(patch);
        if !self.reset_first {
            self.queue(true, true);
            if self.downloads.is_empty() {
                return self.finish(Ok(Outcome::AlreadyRunning));
            }
        }
        self.state = Step::Settle;
        vec![Action::Timer(SHORT_WAIT_MS)]
    }

    /// The downloads to run, in order
    fn queue(&mut self, patch: bool, syscfg: bool) {
        let mut downloads = Vec::new();
        if patch && let Some(d) = self.patch.take() {
            downloads.push(d);
        }
        if syscfg && let Some(d) = self.syscfg.take() {
            downloads.push(d);
        }
        self.total = downloads.iter().map(|d| d.data.len()).sum();
        self.downloads = downloads;
    }

    fn start_download(&mut self, index: usize) -> Vec<Action> {
        self.current = index;
        let d = &self.downloads[index];
        tracing::info!("atheros: sending the {} {}, {} bytes", d.what, d.file, d.data.len());
        self.chunks = d.chunks();
        self.state = Step::Header;
        vec![Action::UsbControl(send_header(&d.data[..HEADER]))]
    }

    /// The next bulk transfer, or the check after the last
    fn body(&mut self, next: usize) -> Vec<Action> {
        match self.chunks.get(next).cloned() {
            Some(chunk) => {
                self.sent += chunk.len();
                self.state = Step::Body { next: next + 1 };
                vec![Action::BulkCommand(chunk), Action::Timer(CHUNK_GAP_MS)]
            }
            None => {
                self.state = Step::Check { polls: 0 };
                vec![Action::Timer(SETTLE_MS)]
            }
        }
    }

    fn on_state(&mut self, state: u8) -> Vec<Action> {
        match std::mem::replace(&mut self.state, Step::Idle) {
            Step::ReadState => {
                tracing::info!("atheros: state 0x{state:02X}");
                if STATE_REFUSED.contains(&(state & 0x3F)) {
                    return self.finish(Err(LoadError::Unsupported(
                        "the chip is in a state Qualcomm Atheros' driver does not load it in",
                    )));
                }
                self.chip_state = state;
                self.state = Step::VersionWait;
                vec![Action::Timer(SHORT_WAIT_MS)]
            }
            Step::ResetRead => {
                let patched = state & STATE_PATCHED != 0;
                tracing::info!("atheros: state 0x{state:02X} after the reset");
                self.no_patch = patched.then_some("its patch was in the chip already");
                self.queue(!patched, state & STATE_SYSCFG == 0);
                if self.downloads.is_empty() {
                    return self.finish(Ok(Outcome::AlreadyRunning));
                }
                self.start_download(0)
            }
            Step::Check { polls } => {
                let d = &self.downloads[self.current];
                if state & d.done_bit != 0 {
                    tracing::info!("atheros: the chip took the {} (state 0x{state:02X})", d.what);
                    let next = self.current + 1;
                    if next < self.downloads.len() {
                        return self.start_download(next);
                    }
                    let file = d.file.clone();
                    let patch_sent = self.downloads.iter().any(|d| d.done_bit == STATE_PATCHED);
                    let detail = if patch_sent { None } else { self.no_patch };
                    return self.finish(Ok(Outcome::Loaded { firmware: file, detail }));
                }
                if polls + 1 >= MAX_POLLS {
                    tracing::warn!("atheros: state 0x{state:02X} after the {}", d.what);
                    return self.finish(Err(LoadError::Timeout(d.what)));
                }
                self.state = Step::Check { polls: polls + 1 };
                vec![Action::Timer(SETTLE_MS)]
            }
            state => {
                self.state = state;
                Vec::new()
            }
        }
    }
}

impl ChipLoader for Loader {
    fn vendor(&self) -> &'static str {
        "Qualcomm Atheros"
    }

    fn start(&mut self) -> Vec<Action> {
        self.started = Instant::now();
        self.state = Step::ReadState;
        vec![Action::UsbControl(read_state())]
    }

    /// The chip runs its ROM, which answers HCI: nothing to wait for there
    fn on_event(&mut self, _event: &[u8]) -> Vec<Action> {
        Vec::new()
    }

    fn on_timeout(&mut self) -> Vec<Action> {
        match std::mem::replace(&mut self.state, Step::Idle) {
            Step::VersionWait => {
                self.state = Step::Version;
                vec![Action::UsbControl(read_version())]
            }
            Step::Settle => {
                if self.reset_first {
                    tracing::info!("atheros: the patch is in but not the system configuration: warm reset");
                    self.state = Step::Reset;
                    return vec![Action::UsbControl(warm_reset())];
                }
                self.start_download(0)
            }
            Step::ResetWait => {
                self.state = Step::ResetRead;
                vec![Action::UsbControl(read_state())]
            }
            Step::HeaderWait => self.body(0),
            Step::Body { next } => self.body(next),
            state @ Step::Check { .. } => {
                self.state = state;
                vec![Action::UsbControl(read_state())]
            }
            state => {
                self.state = state;
                Vec::new()
            }
        }
    }

    fn on_control(&mut self, result: Result<Vec<u8>, String>) -> Vec<Action> {
        match (&self.state, result) {
            (Step::ReadState | Step::ResetRead | Step::Check { .. }, Ok(d)) if d.len() == 1 => self.on_state(d[0]),
            (Step::Version, Ok(d)) => match Version::parse(&d) {
                Some(v) => {
                    tracing::info!(
                        "atheros: ROM 0x{:08X}, build 0x{:X}, RAM 0x{:X}, clock {}, class 0x{:02X}{}",
                        v.rom,
                        v.build,
                        v.ram,
                        v.ref_clock,
                        v.clock_class,
                        if v.tf { ", TF" } else { "" }
                    );
                    self.version = v;
                    self.plan()
                }
                None => self.finish(Err(LoadError::BadVersion)),
            },
            (Step::Reset, Ok(_)) => {
                self.state = Step::ResetWait;
                vec![Action::Timer(RESET_WAIT_MS)]
            }
            (Step::Header, Ok(_)) => {
                self.sent += HEADER;
                self.state = Step::HeaderWait;
                vec![Action::Timer(SETTLE_MS)]
            }
            (Step::ReadState | Step::ResetRead | Step::Check { .. }, Ok(d)) => {
                tracing::warn!("atheros: a state of {} bytes", d.len());
                self.finish(Err(LoadError::Unsupported("a state answer of another size")))
            }
            (
                Step::Idle
                | Step::Done
                | Step::VersionWait
                | Step::Settle
                | Step::ResetWait
                | Step::HeaderWait
                | Step::Body { .. },
                _,
            ) => Vec::new(),
            (_, Err(e)) => {
                tracing::warn!("atheros: USB control transfer failed: {e}");
                self.finish(Err(LoadError::Host("a USB control transfer to the chip failed")))
            }
        }
    }

    fn missing_file_hint(&self, file: &str) -> String {
        format!("{file}, from Qualcomm Atheros' Windows Bluetooth driver (read from it when installed)")
    }

    fn step(&self) -> &'static str {
        match self.state {
            Step::ReadState | Step::VersionWait | Step::Version | Step::Settle => "version",
            Step::Reset | Step::ResetWait | Step::ResetRead => "reset",
            Step::Header | Step::HeaderWait | Step::Body { .. } | Step::Check { .. } => "download",
            Step::Idle | Step::Done => "",
        }
    }

    fn progress(&self) -> Option<(usize, usize)> {
        match self.state {
            Step::Header | Step::HeaderWait | Step::Body { .. } | Step::Check { .. } => Some((self.sent, self.total)),
            _ => None,
        }
    }
}

// ---- Qualcomm Atheros' Windows driver ----

/// A firmware file from Qualcomm Atheros' Windows driver, if it is
/// installed. Newest first (crate::driver_files): its packages in the
/// driver store (btfilter.inf, qca_btusb.inf), and the copies its INF puts
/// in System32\drivers (also the Windows folder and System32). All stay
/// after the adapter moved to WinUSB.
pub fn from_windows_driver(name: &str) -> Option<Vec<u8>> {
    if !name.to_ascii_lowercase().ends_with(".dfu") || name.contains(['\\', '/']) {
        return None;
    }
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    let names = [name.to_string()];
    let package = |n: &str| n.starts_with("btfilter.inf") || n.starts_with("qca_btusb.inf");
    crate::driver_files::find(&windows, crate::driver_files::Names::Exact(&names), &package, &[]).iter().find_map(
        |path| {
            let data = std::fs::read(path).ok().filter(|d| !d.is_empty())?;
            tracing::info!(
                "atheros: {name} from Qualcomm Atheros' driver ({}), {} bytes",
                crate::driver_files::file_name(path),
                data.len()
            );
            Some(data)
        },
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::bringup::sim::{self, SimController};

    const AR3012: Version =
        Version { rom: 0x3101_0000, build: 0x0A, ram: 0x40, ref_clock: 1, clock_class: 0, tf: false };

    /// A file: the 20-byte header (load address, entry, length, check word,
    /// kind), then `body`
    fn file(kind: u32, body: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        for w in [0x0059_4800, 0x0099_5500, body.len() as u32, 0x1234_5678, kind] {
            f.extend_from_slice(&w.to_le_bytes());
        }
        f.extend_from_slice(body);
        f
    }

    /// A patch for `rom` with this RAM version and `size` bytes of code
    fn patch(rom: u32, ram: u32, size: usize) -> Vec<u8> {
        let mut body: Vec<u8> = (0..size).map(|n| (n * 3) as u8).collect();
        body.extend_from_slice(&rom.to_le_bytes());
        body.extend_from_slice(&ram.to_le_bytes());
        file(KIND_PATCH, &body)
    }

    fn syscfg(size: usize) -> Vec<u8> {
        file(KIND_SYSCFG, &(0..size).map(|n| (n * 7) as u8).collect::<Vec<u8>>())
    }

    fn version_bytes(v: &Version) -> Vec<u8> {
        let mut d = Vec::new();
        for w in [v.rom, v.build, v.ram] {
            d.extend_from_slice(&w.to_le_bytes());
        }
        d.extend_from_slice(&[v.ref_clock, 0, 0, 0, v.clock_class, if v.tf { 0x23 } else { 0 }, 0, 0]);
        d
    }

    /// The chip: its state and version; the files it took, as received.
    /// It knows a file is complete by the length in its header and takes it
    /// by its kind.
    struct Chip {
        state: u8,
        version: Version,
        receiving: Vec<u8>,
        received: Vec<Vec<u8>>,
        /// Takes the files but never reports them
        ignores: bool,
        fail_header: bool,
        /// The warm reset clears the patch (and the system configuration)
        reset_clears: bool,
        resets: usize,
        /// Bulk transfer sizes, in order
        transfers: Vec<usize>,
        /// State reads in a row that still miss the bit (the transfer is
        /// on its way)
        slow_reads: u32,
        slow_left: u32,
        /// Answer the version with this instead
        version_answer: Option<Vec<u8>>,
    }

    impl Chip {
        fn new(version: Version) -> Chip {
            Chip {
                state: 0,
                version,
                receiving: Vec::new(),
                received: Vec::new(),
                ignores: false,
                fail_header: false,
                reset_clears: true,
                resets: 0,
                transfers: Vec::new(),
                slow_reads: 0,
                slow_left: 0,
                version_answer: None,
            }
        }

        fn take(&mut self) {
            let Some(len) = word(&self.receiving, 8) else { return };
            if self.receiving.len() == HEADER + len as usize {
                let f = std::mem::take(&mut self.receiving);
                if !self.ignores {
                    self.state |= if word(&f, 16) == Some(KIND_PATCH) { STATE_PATCHED } else { STATE_SYSCFG };
                }
                self.received.push(f);
                self.slow_left = self.slow_reads;
            }
        }
    }

    impl SimController for Chip {
        fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
            panic!("no HCI command from this loader: {c:02X?}");
        }

        fn bulk_command(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
            assert!(!self.receiving.is_empty(), "bulk data before a header");
            self.transfers.push(data.len());
            self.receiving.extend_from_slice(data);
            self.take();
            Vec::new()
        }

        fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
            assert_eq!(
                (r.kind, r.recipient, r.value, r.index),
                (ControlKind::Vendor, ControlRecipient::Endpoint, 0, 0)
            );
            match (r.request, &r.data) {
                (REQUEST_STATE, ControlData::In(1)) => {
                    if self.slow_left > 0 {
                        self.slow_left -= 1;
                        return Ok(vec![self.state & !(STATE_PATCHED | STATE_SYSCFG)]);
                    }
                    Ok(vec![self.state])
                }
                (REQUEST_VERSION, ControlData::In(VERSION_LEN)) => {
                    Ok(self.version_answer.clone().unwrap_or_else(|| version_bytes(&self.version)))
                }
                (REQUEST_DOWNLOAD, ControlData::Out(header)) => {
                    if self.fail_header {
                        return Err("stall".into());
                    }
                    assert!(self.receiving.is_empty(), "a header while a file is still coming");
                    assert_eq!(header.len(), HEADER);
                    self.receiving = header.clone();
                    Ok(Vec::new())
                }
                (REQUEST_RESET, ControlData::Out(d)) if d.is_empty() => {
                    self.resets += 1;
                    if self.reset_clears {
                        self.state &= !(STATE_PATCHED | STATE_SYSCFG);
                    }
                    Ok(Vec::new())
                }
                other => panic!("unexpected control transfer {other:?}"),
            }
        }
    }

    fn run(chip: &mut Chip, files: HashMap<String, Vec<u8>>) -> sim::SimRun {
        let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
        sim::run(&mut loader, chip)
    }

    fn ar3012_files() -> HashMap<String, Vec<u8>> {
        HashMap::from([
            ("AthrBT_0x31010000.dfu".to_owned(), patch(0x3101_0000, 0x5C, 46_000)),
            ("ramps_0x31010000_40.dfu".to_owned(), syscfg(1906)),
        ])
    }

    /// The control requests sent, by request number
    fn requests(r: &sim::SimRun) -> Vec<u8> {
        r.controls.iter().map(|c| c.request).collect()
    }

    fn headers(r: &sim::SimRun) -> usize {
        r.controls.iter().filter(|c| c.request == REQUEST_DOWNLOAD).count()
    }

    #[test]
    fn loads_the_patch_then_the_system_configuration() {
        let mut chip = Chip::new(AR3012);
        let r = run(&mut chip, ar3012_files());
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: "ramps_0x31010000_40.dfu".into(), detail: None }));
        let files = ar3012_files();
        assert_eq!(chip.received, [files["AthrBT_0x31010000.dfu"].clone(), files["ramps_0x31010000_40.dfu"].clone()]);
        assert_eq!(
            requests(&r),
            [REQUEST_STATE, REQUEST_VERSION, REQUEST_DOWNLOAD, REQUEST_STATE, REQUEST_DOWNLOAD, REQUEST_STATE]
        );
        // Three short transfers, then the rest at once, for each file
        let patch_body = 46_000 + 8;
        assert_eq!(chip.transfers, [320, 320, 320, patch_body - 960, 320, 320, 320, 1906 - 960]);
        assert!(r.commands.iter().all(|c| c.bulk), "the bodies go on the bulk pipe, no HCI command");
        // 10 ms after the state and the version, 100 ms after each header,
        // 10 ms after each transfer, 100 ms before the state read
        let per_file = [SETTLE_MS, 10, 10, 10, 10, SETTLE_MS];
        let expected: Vec<u32> = [10, 10].into_iter().chain(per_file).chain(per_file).collect();
        assert_eq!(r.timers, expected);
        assert_eq!(chip.state, STATE_PATCHED | STATE_SYSCFG);
        assert!(!r.bootloader);
    }

    #[test]
    fn names_the_files_as_the_driver() {
        assert_eq!(patch_name(&AR3012), "AthrBT_0x31010000.dfu");
        assert_eq!(syscfg_name(&AR3012), "ramps_0x31010000_40.dfu");
        let v = Version { rom: 0x0102_0200, ref_clock: 0, clock_class: 0x01, ..AR3012 };
        assert_eq!(syscfg_name(&v), "ramps_0x01020200_26_0x01.dfu");
        let v = Version { clock_class: 0xF1, ..AR3012 };
        assert_eq!(syscfg_name(&v), "ramps_0x31010000_40_0xf1.dfu");
        // No class suffix for 0 and 0xFF; an unknown clock is "_0"
        assert_eq!(syscfg_name(&Version { clock_class: 0xFF, ..AR3012 }), "ramps_0x31010000_40.dfu");
        assert_eq!(syscfg_name(&Version { ref_clock: 9, ..AR3012 }), "ramps_0x31010000_0.dfu");
        let clocks: Vec<String> =
            (0..9).map(|c| syscfg_name(&Version { ref_clock: c, ..AR3012 })[16..].to_string()).collect();
        assert_eq!(
            clocks,
            ["_26.dfu", "_40.dfu", "_19.dfu", "_24.dfu", "_16.dfu", "_37.dfu", "_38.dfu", "_48.dfu", "_52.dfu"]
        );
        let tf = Version { tf: true, ..AR3012 };
        assert_eq!(
            (patch_name(&tf), syscfg_name(&tf)),
            ("AthrBT_TF_0x31010000.dfu".into(), "ramps_TF_0x31010000_40.dfu".into())
        );
    }

    #[test]
    fn nothing_is_sent_when_the_firmware_is_in() {
        let mut chip = Chip { state: STATE_PATCHED | STATE_SYSCFG, ..Chip::new(AR3012) };
        let r = run(&mut chip, HashMap::new());
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert_eq!(requests(&r), [REQUEST_STATE, REQUEST_VERSION]);
        assert!(r.commands.is_empty());
    }

    #[test]
    fn a_chip_with_the_patch_but_not_the_configuration_is_reset_first() {
        let mut chip = Chip { state: STATE_PATCHED, ..Chip::new(AR3012) };
        let r = run(&mut chip, ar3012_files());
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: "ramps_0x31010000_40.dfu".into(), detail: None }));
        assert_eq!(chip.resets, 1);
        assert_eq!(requests(&r)[..4], [REQUEST_STATE, REQUEST_VERSION, REQUEST_RESET, REQUEST_STATE]);
        assert_eq!(headers(&r), 2, "the patch again, then the configuration");
        assert_eq!(r.timers[..3], [10, 10, RESET_WAIT_MS]);
    }

    #[test]
    fn a_reset_that_keeps_the_patch_sends_the_configuration_only() {
        let mut chip = Chip { state: STATE_PATCHED, reset_clears: false, ..Chip::new(AR3012) };
        let r = run(&mut chip, ar3012_files());
        assert_eq!(
            r.result,
            Ok(Outcome::Loaded {
                firmware: "ramps_0x31010000_40.dfu".into(),
                detail: Some("its patch was in the chip already")
            })
        );
        assert_eq!((chip.resets, headers(&r)), (1, 1));
    }

    #[test]
    fn no_reset_when_the_chips_patch_is_as_new() {
        // The chip reports the file's RAM version: the configuration only
        let mut chip = Chip { state: STATE_PATCHED, ..Chip::new(Version { ram: 0x5C, ..AR3012 }) };
        let r = run(&mut chip, ar3012_files());
        assert!(r.result.is_ok(), "{:?}", r.result);
        assert_eq!((chip.resets, headers(&r)), (0, 1));
        // Nor on a ROM the driver does not reset (0x01020200 before RAM 0x52)
        let v = Version { rom: 0x0102_0200, ram: 0x40, ..AR3012 };
        let files = HashMap::from([
            ("AthrBT_0x01020200.dfu".to_owned(), patch(0x0102_0200, 0x92, 3000)),
            ("ramps_0x01020200_40.dfu".to_owned(), syscfg(1184)),
        ]);
        let mut chip = Chip { state: STATE_PATCHED, ..Chip::new(v) };
        let r = run(&mut chip, files.clone());
        assert!(r.result.is_ok(), "{:?}", r.result);
        assert_eq!((chip.resets, headers(&r)), (0, 1));
        let mut chip = Chip { state: STATE_PATCHED, ..Chip::new(Version { ram: 0x52, ..v }) };
        assert!(run(&mut chip, files).result.is_ok());
        assert_eq!(chip.resets, 1);
    }

    #[test]
    fn a_patch_no_newer_than_the_chips_is_not_sent() {
        let mut chip = Chip::new(Version { ram: 0x60, ..AR3012 });
        let r = run(&mut chip, ar3012_files());
        assert_eq!(
            r.result,
            Ok(Outcome::Loaded {
                firmware: "ramps_0x31010000_40.dfu".into(),
                detail: Some("the chip's patch is as new as the file's")
            })
        );
        assert_eq!(headers(&r), 1, "the configuration only");
        // And with the configuration in, nothing at all
        let mut chip = Chip { state: STATE_SYSCFG, ..Chip::new(Version { ram: 0x60, ..AR3012 }) };
        let r = run(&mut chip, ar3012_files());
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert_eq!(headers(&r), 0);
    }

    #[test]
    fn a_rom_version_the_driver_leaves_alone_gets_nothing() {
        for ram in [0x160, 0x17E, 0x2870, 0x288E] {
            let mut chip = Chip::new(Version { rom: 0x1102_0000, ram, ..AR3012 });
            let r = run(&mut chip, HashMap::new());
            assert_eq!(r.result, Ok(Outcome::Rom), "RAM 0x{ram:X}");
            assert_eq!(requests(&r), [REQUEST_STATE, REQUEST_VERSION]);
        }
        assert_eq!(rom_rules(0x1102_0000, 0x15F), Some((true, true)));
        assert_eq!(rom_rules(0x0102_0001, 0), Some((true, false)));
    }

    #[test]
    fn an_unknown_rom_is_left_alone() {
        for rom in [0x0000_0302, 0x0013_0201, 0x3101_0200, 0x000D_0100] {
            let r = run(&mut Chip::new(Version { rom, ..AR3012 }), ar3012_files());
            assert!(matches!(r.result, Err(LoadError::Unsupported(_))), "{rom:08X}: {:?}", r.result);
            assert_eq!(requests(&r), [REQUEST_STATE, REQUEST_VERSION], "nothing sent to it");
        }
        let mut chip = Chip { state: 0xC0, ..Chip::new(Version { rom: 0x302, ..AR3012 }) };
        assert_eq!(run(&mut chip, HashMap::new()).result, Ok(Outcome::AlreadyRunning));
    }

    #[test]
    fn a_state_the_driver_refuses_is_refused() {
        for state in [0x0B, 0x0C, 0x8C] {
            let r = run(&mut Chip { state, ..Chip::new(AR3012) }, ar3012_files());
            assert!(matches!(r.result, Err(LoadError::Unsupported(_))), "{:?}", r.result);
            assert_eq!(requests(&r), [REQUEST_STATE], "only the state read");
        }
    }

    #[test]
    fn checks_every_file_before_sending_any() {
        // A missing configuration: not even the patch goes
        let mut files = ar3012_files();
        files.remove("ramps_0x31010000_40.dfu");
        let r = run(&mut Chip::new(AR3012), files);
        assert_eq!(r.result, Err(LoadError::MissingFile("ramps_0x31010000_40.dfu".into())));
        assert_eq!(headers(&r), 0);
        let r = run(&mut Chip::new(AR3012), HashMap::new());
        assert_eq!(r.result, Err(LoadError::MissingFile("AthrBT_0x31010000.dfu".into())));
        // A patch for another ROM, cut short, of another kind, too short
        let mut cut = patch(0x3101_0000, 0x5C, 100);
        cut.pop();
        for bad in [patch(0x3101_0100, 0x5C, 100), cut, syscfg(100), file(KIND_PATCH, &[1, 2, 3]), vec![0; 12]] {
            let mut files = ar3012_files();
            files.insert("AthrBT_0x31010000.dfu".into(), bad);
            let r = run(&mut Chip::new(AR3012), files);
            assert!(matches!(r.result, Err(LoadError::BadFile { .. })), "{:?}", r.result);
            assert_eq!(headers(&r), 0);
            assert!(r.commands.is_empty());
        }
        // A configuration that is a patch
        let mut files = ar3012_files();
        files.insert("ramps_0x31010000_40.dfu".into(), patch(0x3101_0000, 1, 10));
        let r = run(&mut Chip::new(AR3012), files);
        assert!(matches!(r.result, Err(LoadError::BadFile { .. })), "{:?}", r.result);
        assert_eq!(headers(&r), 0);
    }

    #[test]
    fn waits_for_the_chip_to_report_the_file() {
        let mut chip = Chip { slow_reads: 4, ..Chip::new(AR3012) };
        let r = run(&mut chip, ar3012_files());
        assert!(r.result.is_ok(), "{:?}", r.result);
        let reads = r.controls.iter().filter(|c| c.request == REQUEST_STATE).count();
        assert_eq!(reads, 1 + 5 + 5);
    }

    #[test]
    fn a_file_the_chip_does_not_report_fails() {
        let r = run(&mut Chip { ignores: true, ..Chip::new(AR3012) }, ar3012_files());
        assert_eq!(r.result, Err(LoadError::Timeout("patch")));
        assert_eq!(r.timeouts, r.timers.len());
        // The configuration alone
        let mut chip = Chip { ignores: true, ..Chip::new(Version { ram: 0x60, ..AR3012 }) };
        assert_eq!(run(&mut chip, ar3012_files()).result, Err(LoadError::Timeout("system configuration")));
    }

    #[test]
    fn a_failed_transfer_ends_it() {
        let r = run(&mut Chip { fail_header: true, ..Chip::new(AR3012) }, ar3012_files());
        assert!(matches!(r.result, Err(LoadError::Host(_))), "{:?}", r.result);
        // Over H4 (the emulator) the first request fails: a silent chip
        struct H4;
        impl SimController for H4 {
            fn command(&mut self, _: &[u8]) -> Vec<Vec<u8>> {
                Vec::new()
            }
        }
        let mut loader = Loader::new(Box::new(|_| None));
        let r = sim::run(&mut loader, &mut H4);
        assert!(matches!(r.result, Err(LoadError::Host(_))), "{:?}", r.result);
        assert_eq!(r.controls.len(), 1);
    }

    #[test]
    fn an_unreadable_version_ends_it() {
        let mut chip = Chip { version_answer: Some(version_bytes(&AR3012)[..19].to_vec()), ..Chip::new(AR3012) };
        let r = run(&mut chip, ar3012_files());
        assert_eq!(r.result, Err(LoadError::BadVersion));
        assert_eq!(headers(&r), 0);
    }

    #[test]
    fn reads_the_version() {
        let mut d = version_bytes(&AR3012);
        d[17] = 0x23;
        d[16] = 0x12;
        let v = Version::parse(&d).unwrap();
        assert!(v.tf);
        assert_eq!((v.rom, v.build, v.ram, v.ref_clock, v.clock_class), (0x3101_0000, 0x0A, 0x40, 1, 0x12));
        assert_eq!(Version::parse(&d[..19]), None);
    }

    #[test]
    fn tells_its_step_and_progress() {
        let files = ar3012_files();
        let total = files.values().map(Vec::len).sum::<usize>();
        let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
        loader.start();
        assert_eq!(loader.step(), "version");
        loader.on_control(Ok(vec![0]));
        loader.on_timeout();
        loader.on_control(Ok(version_bytes(&AR3012)));
        loader.on_timeout();
        assert_eq!((loader.step(), loader.progress()), ("download", Some((0, total))));
        loader.on_control(Ok(Vec::new()));
        loader.on_timeout();
        assert_eq!(loader.progress(), Some((HEADER + 320, total)));
    }

    #[test]
    fn every_listed_usb_id_is_taken_as_qualcomm() {
        use crate::chips::{ChipVendor, LOADER_CHIPS, QUALCOMM_AR30XX, chip_supported, vendor_for_usb};
        assert_eq!(QUALCOMM_AR30XX.len(), 130);
        for id in [(0x0CF3, 0x3002), (0x0CF3, 0x3004), (0x0CF3, 0x311D), (0x0489, 0xE036), (0x13D3, 0x3375)] {
            assert!(QUALCOMM_AR30XX.contains(&id), "{id:04X?}");
        }
        for &(vid, pid) in QUALCOMM_AR30XX {
            assert_eq!(vendor_for_usb(vid, pid, None), ChipVendor::Qualcomm, "{vid:04X}:{pid:04X}");
            assert!(chip_supported(ChipVendor::Qualcomm, vid, pid), "{vid:04X}:{pid:04X}");
            // Not also a chip of the ROME / WCN6855 loader, or another vendor's
            assert!(!LOADER_CHIPS.iter().any(|&(_, v, p)| (v, p) == (vid, pid)), "{vid:04X}:{pid:04X}");
        }
        let mut sorted = QUALCOMM_AR30XX.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), QUALCOMM_AR30XX.len());
        // Qualcomm's other chips stay unsupported (the QCA6290)
        assert!(!chip_supported(ChipVendor::Qualcomm, 0x0CF3, 0xE400));
        // The AR3011 before its firmware runs (no Windows driver loads it)
        assert!(!chip_supported(ChipVendor::Qualcomm, 0x0CF3, 0x3000));
    }

    #[test]
    fn reads_the_header_of_real_files() {
        // The header and last 8 bytes of AthrBT_0x31010000.dfu and the
        // header of ramps_0x31010000_40.dfu (10.0.3.22)
        let patch_header = [
            0x00, 0x48, 0x59, 0x00, 0x00, 0x55, 0x99, 0x00, 0xD0, 0xB6, 0x00, 0x00, 0xA2, 0x52, 0x79, 0xDA, 0x04, 0x00,
            0x00, 0x00,
        ];
        let patch_end = [0x00, 0x00, 0x01, 0x31, 0x5C, 0x00, 0x00, 0x00];
        let mut file = patch_header.to_vec();
        file.resize(46_820 - 8, 0);
        file.extend_from_slice(&patch_end);
        assert_eq!(check_file(&file, KIND_PATCH), Ok(()));
        assert_eq!(patch_version(&file), Some((0x3101_0000, 0x5C)));
        file.pop();
        assert!(check_file(&file, KIND_PATCH).is_err());
        let ramps_header = [
            0x00, 0x38, 0x59, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x72, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00,
        ];
        let mut file = ramps_header.to_vec();
        file.resize(1926, 0);
        assert_eq!(check_file(&file, KIND_SYSCFG), Ok(()));
        assert!(check_file(&file, KIND_PATCH).is_err());
    }

    /// Against Qualcomm Atheros' real driver files: `ATHEROS_DRIVER_DIR=<a
    /// folder with the .dfu files> cargo test -p bt-host --lib
    /// atheros::tests::real -- --ignored`
    #[test]
    #[ignore = "needs Qualcomm Atheros' driver files (ATHEROS_DRIVER_DIR)"]
    fn real_driver_files_load() {
        let dir = PathBuf::from(std::env::var("ATHEROS_DRIVER_DIR").expect("ATHEROS_DRIVER_DIR"));
        let read = |name: &str| std::fs::read(dir.join(name)).ok();
        let mut loaded = 0;
        for rom in [0x0102_0200u32, 0x0102_0201, 0x1102_0000, 0x1102_0100, 0x3101_0000, 0x3101_0100, 0x4102_0000] {
            for (ref_clock, clock_class) in [(0, 0), (1, 0), (1, 1), (1, 0xF1)] {
                let v = Version { rom, build: 0, ram: 0, ref_clock, clock_class, tf: false };
                let (p, s) = (patch_name(&v), syscfg_name(&v));
                let (Some(pd), Some(sd)) = (read(&p), read(&s)) else { continue };
                let mut chip = Chip::new(v);
                let r = run(&mut chip, HashMap::from([(p.clone(), pd.clone()), (s.clone(), sd.clone())]));
                assert_eq!(r.result, Ok(Outcome::Loaded { firmware: s.clone(), detail: None }), "{p}");
                assert_eq!(chip.received, [pd, sd]);
                eprintln!("ROM 0x{rom:08X}: {p} and {s} loaded");
                loaded += 1;
            }
        }
        assert!(loaded > 0, "no Atheros files in ATHEROS_DRIVER_DIR");
    }
}
