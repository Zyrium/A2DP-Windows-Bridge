//! MediaTek MT7663 ROM patch download over USB
//!
//! The MT7663 is the generation before the MT79xx, with its own steps.
//! [`super::Loader`] finds it by its chip ID register (0x80000008 reads
//! 0x7663) and hands over to [`Mt7663`]:
//!
//! 1. read the ROM version (register 0x80000004): its low byte + 1 names the
//!    patch, `mt7663_patch_e2_hdr.bin` for version 1. The file is read and
//!    checked before anything is sent to the chip
//! 2. wait 3 s, then ask whether the chip has the patch (WMT opcode 0x17,
//!    flag 1). Answer 0 or 1: it has, go to step 6. Another answer, or none:
//!    download
//! 3. switch the chip's Wi-Fi part on (WMT function control, flag 3, data
//!    1); its answer is only logged
//! 4. send the patch without its 30-byte header on the bulk OUT pipe, in
//!    parts of up to 2039 bytes. Each part is a WMT patch download command
//!    framed like an ACL packet (handle 0xFC6F, 16-bit length): flag 1 for
//!    the first, 2 for a middle and 3 for the last part. The chip
//!    acknowledges each with a WMT event, status 0
//! 5. WMT reset (opcode 7, flag 4), answered with status 0
//! 6. switch Bluetooth on (WMT function control, flag 0, data 1): status 0
//!    is on, 2 "not yet" (asked again after 100 ms, at most 3 times)
//!
//! The WMT commands and events are those of the MT79xx (see the parent
//! module): commands on the control pipe, their events read with a vendor
//! control transfer. All of this was read from MediaTek's Windows driver for
//! the MT7663 (mtkbtfilterx.sys 1.2.7.71 to 1.1034.0.320); none of it has
//! been tried on the chip.

use std::time::Instant;

use super::{
    MAX_POLLS, OP_WMT, POLL_INTERVAL_MS, WMT_FUNC_CTRL, WMT_PATCH_DOWNLOAD, WMT_TIMEOUT_MS, parse_wmt_event,
    read_register, read_wmt_event, wmt_command,
};
use crate::bringup::{Action, FileReader, HciEvent, LoadError, Outcome, parse_event};

/// The value of the chip ID register 0x80000008
pub(super) const CHIP_ID: u32 = 0x7663;
/// Its low byte + 1 is the patch file's number
const REGISTER_ROM_VERSION: u32 = 0x8000_0004;

/// WMT opcodes of the MT7663
const WMT_PATCH_QUERY: u8 = 0x17;
const WMT_RESET: u8 = 0x07;

/// The patch file: a 30-byte header (build time, "7663", hardware and
/// software version), then what is sent
const HEADER_SIZE: usize = 0x1E;
const PLATFORM: std::ops::Range<usize> = 0x10..0x14;

/// Patch bytes per part
pub(super) const MAX_PART: usize = 2039;

/// Before the first WMT command
const COLD_WAIT_MS: u32 = 3000;
/// "Not yet" answers to Bluetooth on, asked again after this
const BT_ON_RETRY_MS: u32 = 100;
const BT_ON_RETRIES: u8 = 3;

/// The patch file for a ROM version: "mt7663_patch_e2_hdr.bin" for 1
pub fn firmware_name(rom_version: u32) -> String {
    format!("mt7663_patch_e{:x}_hdr.bin", (rom_version & 0xFF) + 1)
}

/// What is sent of a patch file. Err: why the file is not one
pub(super) fn patch_body(file: &[u8]) -> Result<&[u8], &'static str> {
    if file.len() <= HEADER_SIZE {
        return Err("too short for an MT7663 patch");
    }
    if &file[PLATFORM] != b"7663" {
        return Err("not an MT7663 patch");
    }
    Ok(&file[HEADER_SIZE..])
}

/// The bulk OUT packets that carry the patch: 0xFC6F, length (u16), then a
/// WMT patch download command (direction 1, opcode 1, length of flag + data
/// as u16, flag, data). The flags as MediaTek's driver sets them: the first
/// part 1 (3 if it is shorter than a full part), a full middle part 2, the
/// last 3
pub(super) fn parts(body: &[u8]) -> Vec<Vec<u8>> {
    let mut parts = Vec::new();
    let mut sent = 0;
    while sent < body.len() {
        let n = (body.len() - sent).min(MAX_PART);
        let flag = match (sent == 0, n == MAX_PART) {
            (true, true) => 1,
            (false, true) if body.len() - sent > MAX_PART => 2,
            _ => 3,
        };
        let mut p = vec![0x6F, 0xFC];
        p.extend_from_slice(&((n + 5) as u16).to_le_bytes());
        p.extend_from_slice(&[0x01, WMT_PATCH_DOWNLOAD]);
        p.extend_from_slice(&((n + 1) as u16).to_le_bytes());
        p.push(flag);
        p.extend_from_slice(&body[sent..sent + n]);
        parts.push(p);
        sent += n;
    }
    parts
}

/// What a WMT command is for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wmt {
    /// Does the chip have the patch?
    Query,
    WifiOn,
    /// A part of the patch
    Part,
    Reset,
    BluetoothOn,
}

enum State {
    /// Reading the ROM version
    RomVersion,
    /// The wait before the first WMT command
    ColdWait,
    /// A WMT command sent: its event is read with control transfers
    Waiting {
        what: Wmt,
        polls: u32,
        since: Instant,
    },
    /// Before asking for Bluetooth on again
    BluetoothOnRetry,
    Done,
}

/// The MT7663's download, from the ROM version read to Bluetooth on
pub(super) struct Mt7663 {
    state: State,
    file: String,
    parts: Vec<Vec<u8>>,
    /// The next part to send
    next: usize,
    /// Parts acknowledged
    done_parts: usize,
    bt_on_retries: u8,
    downloaded: bool,
}

impl Mt7663 {
    /// The chip ID read: the ROM version comes next
    pub(super) fn start() -> (Mt7663, Vec<Action>) {
        tracing::info!("mediatek: chip MT7663");
        let m = Mt7663 {
            state: State::RomVersion,
            file: String::new(),
            parts: Vec::new(),
            next: 0,
            done_parts: 0,
            bt_on_retries: 0,
            downloaded: false,
        };
        (m, vec![Action::UsbControl(read_register(REGISTER_ROM_VERSION))])
    }

    fn finish(&mut self, result: Result<Outcome, LoadError>) -> Vec<Action> {
        tracing::debug!("mediatek: MT7663 finished: {result:?}");
        self.state = State::Done;
        vec![Action::Done(result)]
    }

    /// Sends a WMT command (on the control pipe, or a patch part on the bulk
    /// pipe) and starts reading its event
    fn send(&mut self, what: Wmt, command: Action) -> Vec<Action> {
        self.state = State::Waiting { what, polls: 0, since: Instant::now() };
        vec![command, Action::UsbControl(read_wmt_event())]
    }

    fn query(&mut self) -> Vec<Action> {
        self.send(Wmt::Query, Action::Command(wmt_command(WMT_PATCH_QUERY, 1, &[])))
    }

    fn wifi_on(&mut self) -> Vec<Action> {
        self.send(Wmt::WifiOn, Action::Command(wmt_command(WMT_FUNC_CTRL, 3, &[0x01])))
    }

    /// The next part, or the reset after the last
    fn next_part(&mut self) -> Vec<Action> {
        match self.parts.get(self.next).cloned() {
            Some(p) => {
                self.next += 1;
                self.send(Wmt::Part, Action::BulkCommand(p))
            }
            None => {
                tracing::info!("mediatek: MT7663 patch sent in {} parts", self.parts.len());
                self.send(Wmt::Reset, Action::Command(wmt_command(WMT_RESET, 4, &[])))
            }
        }
    }

    fn bluetooth_on(&mut self) -> Vec<Action> {
        self.send(Wmt::BluetoothOn, Action::Command(wmt_command(WMT_FUNC_CTRL, 0, &[0x01])))
    }

    /// The ROM version read: the file, checked before anything is sent
    fn on_rom_version(&mut self, version: u32, read_file: &mut FileReader) -> Vec<Action> {
        let file = firmware_name(version);
        tracing::info!("mediatek: MT7663 ROM version 0x{version:08X}");
        let Some(data) = read_file(&file) else { return self.finish(Err(LoadError::MissingFile(file))) };
        let body = match patch_body(&data) {
            Ok(body) => body,
            Err(why) => return self.finish(Err(LoadError::BadFile { file, why })),
        };
        self.parts = parts(body);
        tracing::info!("mediatek: {file}: {} bytes, {} parts", data.len(), self.parts.len());
        self.file = file;
        self.state = State::ColdWait;
        vec![Action::Timer(COLD_WAIT_MS)]
    }

    fn on_wmt(&mut self, what: Wmt, op: u8, flag: u8) -> Vec<Action> {
        let expected = match what {
            Wmt::Query => WMT_PATCH_QUERY,
            Wmt::WifiOn | Wmt::BluetoothOn => WMT_FUNC_CTRL,
            Wmt::Part => WMT_PATCH_DOWNLOAD,
            Wmt::Reset => WMT_RESET,
        };
        if op != expected {
            tracing::warn!("mediatek: WMT event for opcode 0x{op:02X}, waiting for 0x{expected:02X}");
            return match what {
                // MediaTek's driver goes on as if unanswered
                Wmt::Query | Wmt::WifiOn => self.unanswered(what),
                _ => self.finish(Err(LoadError::Unsupported("the chip answered another WMT command"))),
            };
        }
        match what {
            Wmt::Query if flag <= 1 => {
                tracing::info!("mediatek: the MT7663 has its patch already ({flag})");
                self.bluetooth_on()
            }
            Wmt::Query => {
                tracing::info!("mediatek: the MT7663 needs its patch ({flag})");
                self.wifi_on()
            }
            Wmt::WifiOn => {
                if flag != 0 {
                    tracing::warn!("mediatek: MT7663 Wi-Fi on answered with {flag}");
                }
                self.downloaded = true;
                self.next_part()
            }
            Wmt::Part if flag == 0 => {
                self.done_parts += 1;
                self.next_part()
            }
            Wmt::Reset if flag == 0 => self.bluetooth_on(),
            Wmt::Part | Wmt::Reset => {
                tracing::warn!("mediatek: MT7663 {what:?} answered with {flag}");
                self.finish(Err(LoadError::Rejected(flag)))
            }
            Wmt::BluetoothOn if flag == 0 => {
                tracing::info!("mediatek: MT7663 Bluetooth on");
                let detail = (!self.downloaded).then_some("the chip had the firmware already");
                let firmware = std::mem::take(&mut self.file);
                self.finish(Ok(Outcome::Loaded { firmware, detail }))
            }
            Wmt::BluetoothOn if flag == 2 && self.bt_on_retries < BT_ON_RETRIES => {
                self.bt_on_retries += 1;
                self.state = State::BluetoothOnRetry;
                vec![Action::Timer(BT_ON_RETRY_MS)]
            }
            Wmt::BluetoothOn if flag == 2 => self.finish(Err(LoadError::Timeout("Bluetooth on"))),
            Wmt::BluetoothOn => {
                tracing::warn!("mediatek: MT7663 Bluetooth on answered with {flag}");
                self.finish(Err(LoadError::Rejected(flag)))
            }
        }
    }

    /// No answer in time: MediaTek's driver downloads after an unanswered
    /// query and ignores Wi-Fi on; the other steps fail
    fn unanswered(&mut self, what: Wmt) -> Vec<Action> {
        match what {
            Wmt::Query => {
                tracing::info!("mediatek: MT7663 patch query unanswered, downloading");
                self.wifi_on()
            }
            Wmt::WifiOn => {
                tracing::info!("mediatek: MT7663 Wi-Fi on unanswered");
                self.downloaded = true;
                self.next_part()
            }
            Wmt::Part => self.finish(Err(LoadError::Timeout("download"))),
            Wmt::Reset => self.finish(Err(LoadError::Timeout("WMT reset"))),
            Wmt::BluetoothOn => self.finish(Err(LoadError::Timeout("Bluetooth on"))),
        }
    }

    /// A WMT event read returned: the event, or nothing yet
    fn on_poll(&mut self, data: Vec<u8>) -> Vec<Action> {
        let State::Waiting { what, polls, since } = self.state else { return Vec::new() };
        if data.first().is_none_or(|&b| b == 0) {
            if polls >= MAX_POLLS || since.elapsed().as_millis() > WMT_TIMEOUT_MS {
                return self.unanswered(what);
            }
            self.state = State::Waiting { what, polls: polls + 1, since };
            return vec![Action::Timer(POLL_INTERVAL_MS)];
        }
        match parse_wmt_event(&data) {
            Some((op, flag, _)) => self.on_wmt(what, op, flag),
            None => {
                tracing::warn!("mediatek: unexpected answer to the WMT event read: {data:02X?}");
                self.finish(Err(LoadError::Unsupported("an answer that is not a WMT event")))
            }
        }
    }

    pub(super) fn on_event(&mut self, event: &[u8]) -> Vec<Action> {
        let State::Waiting { what, .. } = self.state else { return Vec::new() };
        match parse_event(event) {
            HciEvent::Complete { opcode: OP_WMT, status, .. } | HciEvent::Status { opcode: OP_WMT, status }
                if status != 0 =>
            {
                self.finish(Err(LoadError::Command { opcode: OP_WMT, status }))
            }
            // A WMT event on the interrupt pipe after all
            _ => match parse_wmt_event(event) {
                Some((op, flag, _)) => self.on_wmt(what, op, flag),
                None => Vec::new(),
            },
        }
    }

    pub(super) fn on_timeout(&mut self) -> Vec<Action> {
        match self.state {
            State::ColdWait => self.query(),
            State::Waiting { .. } => vec![Action::UsbControl(read_wmt_event())],
            State::BluetoothOnRetry => self.bluetooth_on(),
            State::RomVersion | State::Done => Vec::new(),
        }
    }

    pub(super) fn on_control(&mut self, result: Result<Vec<u8>, String>, read_file: &mut FileReader) -> Vec<Action> {
        match (&self.state, result) {
            (State::RomVersion, Ok(d)) if d.len() == 4 => {
                self.on_rom_version(u32::from_le_bytes([d[0], d[1], d[2], d[3]]), read_file)
            }
            (State::RomVersion, other) => {
                tracing::warn!("mediatek: reading the MT7663 ROM version failed: {other:02X?}");
                self.finish(Err(LoadError::Host("the chip's ROM version could not be read")))
            }
            (State::Waiting { .. }, Ok(d)) => self.on_poll(d),
            (State::Waiting { .. }, Err(e)) => {
                tracing::warn!("mediatek: USB control transfer failed: {e}");
                self.finish(Err(LoadError::Host("a USB control transfer to the chip failed")))
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn step(&self) -> &'static str {
        match self.state {
            State::RomVersion => "chip ID",
            State::ColdWait | State::Waiting { what: Wmt::Query | Wmt::WifiOn | Wmt::Part, .. } => "download",
            State::Waiting { .. } | State::BluetoothOnRetry => "start",
            State::Done => "",
        }
    }

    pub(super) fn progress(&self) -> Option<(usize, usize)> {
        match self.state {
            State::Waiting { what: Wmt::Part, .. } => Some((self.done_parts, self.parts.len())),
            _ => None,
        }
    }
}

/// A patch file of MediaTek's MT7663 driver: its INF copies the file to
/// System32\drivers and keeps it in its driver store package (not in a
/// container as the MT79xx driver does). Only names of MT7663 patches.
pub(super) fn from_windows_driver(name: &str, windows: &std::path::Path) -> Option<Vec<u8>> {
    let lower = name.to_ascii_lowercase();
    if !lower.starts_with("mt7663_patch_") {
        return None;
    }
    let names = [lower];
    let found = crate::driver_files::find(
        windows,
        crate::driver_files::Names::Exact(&names),
        &|n: &str| n.starts_with("mtkbt"),
        &[],
    );
    found.iter().find_map(|path| {
        let data = std::fs::read(path).ok()?;
        patch_body(&data).ok()?;
        tracing::info!("mediatek: {name} from MediaTek's driver, {} bytes", data.len());
        Some(data)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::{EVENT_WMT, Loader, OP_WMT};
    use super::*;
    use crate::bringup::sim::{self, SimController, command_complete, command_status, event};
    use crate::bringup::{ControlData, ControlKind, ControlRecipient, ControlRequest};

    const NAME: &str = "mt7663_patch_e2_hdr.bin";

    /// A patch file: the header as MediaTek's files have it, `size` bytes
    /// after it
    fn patch(size: usize) -> Vec<u8> {
        let mut f = b"2304271616b235a\n7663e2ccn10\x8a\xa5\x0f".to_vec();
        assert_eq!(f.len(), HEADER_SIZE);
        f.extend((0..size).map(|n| (n * 7 + n / 251) as u8));
        f
    }

    fn files(size: usize) -> HashMap<String, Vec<u8>> {
        HashMap::from([(NAME.to_owned(), patch(size))])
    }

    /// An MT7663 as MediaTek's driver expects it to answer: its ROM refuses
    /// HCI, its chip ID register 0x80000008 reads 0x7663, and each WMT
    /// command is answered with the event the driver compares
    struct Chip {
        firmware_runs: bool,
        /// Never answers Read Local Version
        version_silent: bool,
        /// Register 0x70010200, which the MT79xx path reads first
        id_79xx: Result<u32, String>,
        chip_id: u32,
        rom_version: Result<u32, String>,
        /// The answer to the patch query (its status byte); None: silent
        query: Option<u8>,
        /// Wi-Fi on's status; None: silent
        wifi_on: Option<u8>,
        /// Each part's status; None: silent
        part: Option<u8>,
        reset: Option<u8>,
        /// Bluetooth on's statuses in order (the last repeats); empty: silent
        bt_on: Vec<u8>,
        /// The status of a Command Complete for each WMT command
        command_status: Option<u8>,
        /// The event read fails from the n-th read on
        fail_read: Option<usize>,
        /// Empty reads before each event
        empty_reads: u32,
        pending: Vec<Vec<u8>>,
        empty_left: u32,
        reads: usize,
        /// What it received, in order: WMT (opcode, flag, data) of each
        /// control pipe command; the patch parts (flag, data)
        wmt: Vec<(u8, u8, Vec<u8>)>,
        parts: Vec<(u8, Vec<u8>)>,
        bluetooth_on: bool,
    }

    impl Chip {
        fn new() -> Chip {
            Chip {
                firmware_runs: false,
                version_silent: false,
                id_79xx: Ok(0),
                chip_id: CHIP_ID,
                rom_version: Ok(0x0000_8A01),
                query: Some(2),
                wifi_on: Some(0),
                part: Some(0),
                reset: Some(0),
                bt_on: vec![0],
                command_status: None,
                fail_read: None,
                empty_reads: 2,
                pending: Vec::new(),
                empty_left: 0,
                reads: 0,
                wmt: Vec::new(),
                parts: Vec::new(),
                bluetooth_on: false,
            }
        }

        /// The event MediaTek's driver expects: E4 05 02 <op> 01 00 <status>
        fn answer(&mut self, op: u8, status: Option<u8>) {
            if let Some(s) = status {
                self.pending.push(event(EVENT_WMT, &[0x02, op, 0x01, 0x00, s]));
                self.empty_left = self.empty_reads;
            }
        }
    }

    impl SimController for Chip {
        fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
            let opcode = sim::opcode(c);
            if opcode == 0x1001 {
                if self.version_silent {
                    return Vec::new();
                }
                return vec![command_complete(opcode, if self.firmware_runs { 0 } else { 0x0C }, &[])];
            }
            assert_eq!(opcode, OP_WMT, "only WMT commands before HCI init");
            let p = sim::params(c);
            let (op, len, flag, data) = (p[1], u16::from_le_bytes([p[2], p[3]]) as usize, p[4], &p[5..]);
            assert_eq!((p[0], len), (0x01, data.len() + 1), "WMT header");
            self.wmt.push((op, flag, data.to_vec()));
            match (op, flag, data) {
                (WMT_PATCH_QUERY, 1, []) => self.answer(op, self.query),
                (WMT_FUNC_CTRL, 3, [1]) => {
                    assert!(self.parts.is_empty(), "Wi-Fi on before the patch");
                    self.answer(op, self.wifi_on)
                }
                (WMT_RESET, 4, []) => {
                    assert!(!self.parts.is_empty(), "the reset after the patch");
                    self.answer(op, self.reset)
                }
                (WMT_FUNC_CTRL, 0, [1]) => {
                    let status =
                        if self.bt_on.len() > 1 { Some(self.bt_on.remove(0)) } else { self.bt_on.first().copied() };
                    self.bluetooth_on = status == Some(0);
                    self.answer(op, status)
                }
                other => panic!("unexpected WMT command {other:?}"),
            }
            self.command_status.map(|s| vec![command_complete(OP_WMT, s, &[])]).unwrap_or_default()
        }

        fn bulk_command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
            // 6F FC, length (u16), 01 01, length (u16), flag, data
            assert_eq!(c[..2], [0x6F, 0xFC]);
            let len = u16::from_le_bytes([c[2], c[3]]) as usize;
            assert_eq!(len, c.len() - 4, "ACL length");
            assert_eq!(c[4..6], [0x01, WMT_PATCH_DOWNLOAD]);
            assert_eq!(u16::from_le_bytes([c[6], c[7]]) as usize, len - 4, "WMT length");
            assert!(c.len() - 9 <= MAX_PART);
            assert!(self.wmt.iter().any(|w| w.0 == WMT_FUNC_CTRL && w.1 == 3), "Wi-Fi on first");
            self.parts.push((c[8], c[9..].to_vec()));
            self.answer(WMT_PATCH_DOWNLOAD, self.part);
            Vec::new()
        }

        fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
            assert_eq!((r.kind, r.recipient), (ControlKind::Vendor, ControlRecipient::Device));
            let word = |v: &Result<u32, String>| v.clone().map(|v| v.to_le_bytes().to_vec());
            match (r.request, r.value, r.index, &r.data) {
                (0x63, 0x7001, 0x0200, ControlData::In(4)) => word(&self.id_79xx),
                (0x63, 0x8000, 0x0008, ControlData::In(4)) => Ok(self.chip_id.to_le_bytes().to_vec()),
                (0x63, 0x8000, 0x0004, ControlData::In(4)) => word(&self.rom_version),
                (0x01, 0x30, 0, ControlData::In(_)) => {
                    self.reads += 1;
                    if self.fail_read.is_some_and(|n| self.reads >= n) {
                        return Err("stall".into());
                    }
                    if self.empty_left > 0 {
                        self.empty_left -= 1;
                        return Ok(Vec::new());
                    }
                    Ok(if self.pending.is_empty() { Vec::new() } else { self.pending.remove(0) })
                }
                other => panic!("unexpected control transfer {other:?}"),
            }
        }
    }

    fn run(chip: &mut Chip, files: HashMap<String, Vec<u8>>) -> sim::SimRun {
        let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
        sim::run(&mut loader, chip)
    }

    fn wmt_ops(chip: &Chip) -> Vec<(u8, u8)> {
        chip.wmt.iter().map(|w| (w.0, w.1)).collect()
    }

    #[test]
    fn names_the_patch_by_rom_version() {
        assert_eq!(firmware_name(0x0000_8A01), NAME);
        assert_eq!(firmware_name(0), "mt7663_patch_e1_hdr.bin");
        assert_eq!(firmware_name(0x0F), "mt7663_patch_e10_hdr.bin");
    }

    #[test]
    fn splits_the_patch_as_mediateks_driver_does() {
        let flags =
            |n: usize| -> Vec<(u8, usize)> { parts(&vec![0u8; n]).iter().map(|p| (p[8], p.len() - 9)).collect() };
        assert_eq!(flags(10), [(3, 10)]);
        assert_eq!(flags(MAX_PART + 1), [(1, MAX_PART), (3, 1)]);
        assert_eq!(flags(2 * MAX_PART), [(1, MAX_PART), (3, MAX_PART)]);
        assert_eq!(flags(3 * MAX_PART + 5), [(1, MAX_PART), (2, MAX_PART), (2, MAX_PART), (3, 5)]);
        // One full part: the driver sends it with flag 1 and nothing after
        assert_eq!(flags(MAX_PART), [(1, MAX_PART)]);
        // The real file's size: 136 full parts and the rest
        let real = flags(278_558 - HEADER_SIZE);
        assert_eq!((real.len(), real[0], real[135], real[136]), (137, (1, MAX_PART), (2, MAX_PART), (3, 1224)));
        let p = &parts(&[0xAB; 3])[0];
        assert_eq!(p[..], [0x6F, 0xFC, 8, 0, 0x01, 0x01, 4, 0, 3, 0xAB, 0xAB, 0xAB]);
    }

    #[test]
    fn downloads_the_patch_then_switches_bluetooth_on() {
        let mut chip = Chip::new();
        let size = 2 * MAX_PART + 100;
        let r = run(&mut chip, files(size));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
        // On the control pipe: Read Local Version, the query, Wi-Fi on, the
        // reset, Bluetooth on; the patch on the bulk pipe
        assert_eq!(wmt_ops(&chip), [(WMT_PATCH_QUERY, 1), (WMT_FUNC_CTRL, 3), (WMT_RESET, 4), (WMT_FUNC_CTRL, 0)]);
        assert_eq!(r.commands.iter().filter(|c| c.bulk).count(), 3);
        let body: Vec<u8> = chip.parts.iter().flat_map(|(_, d)| d.clone()).collect();
        assert_eq!(body, patch(size)[HEADER_SIZE..]);
        let flags: Vec<u8> = chip.parts.iter().map(|p| p.0).collect();
        assert_eq!(flags, [1, 2, 3]);
        assert!(chip.bluetooth_on);
        assert!(!r.bootloader);
        // The wait before the first WMT command
        assert_eq!(r.timers.iter().filter(|&&t| t == COLD_WAIT_MS).count(), 1);
        // Chip ID registers, the ROM version, then only event reads
        let registers: Vec<(u16, u16)> =
            r.controls.iter().filter(|c| c.request == 0x63).map(|c| (c.value, c.index)).collect();
        assert_eq!(registers, [(0x7001, 0x0200), (0x8000, 0x0008), (0x8000, 0x0004)]);
    }

    #[test]
    fn a_chip_with_its_patch_goes_straight_to_bluetooth_on() {
        for status in [0, 1] {
            let mut chip = Chip { query: Some(status), ..Chip::new() };
            let r = run(&mut chip, files(100));
            assert_eq!(
                r.result,
                Ok(Outcome::Loaded { firmware: NAME.into(), detail: Some("the chip had the firmware already") })
            );
            assert_eq!(wmt_ops(&chip), [(WMT_PATCH_QUERY, 1), (WMT_FUNC_CTRL, 0)]);
            assert!(chip.parts.is_empty() && chip.bluetooth_on);
        }
    }

    #[test]
    fn nothing_to_do_when_bluetooth_runs() {
        let r = run(&mut Chip { firmware_runs: true, ..Chip::new() }, HashMap::new());
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert!(r.controls.is_empty());
    }

    #[test]
    fn names_the_missing_file_and_sends_nothing() {
        let mut chip = Chip::new();
        let r = run(&mut chip, HashMap::new());
        assert_eq!(r.result, Err(LoadError::MissingFile(NAME.into())));
        assert!(chip.wmt.is_empty() && chip.parts.is_empty());
        // Another ROM version names another file; e2 is not taken instead
        let mut chip = Chip { rom_version: Ok(0x0000_8A02), ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert_eq!(r.result, Err(LoadError::MissingFile("mt7663_patch_e3_hdr.bin".into())));
    }

    #[test]
    fn checks_the_file_before_sending_any_of_it() {
        let mut other = patch(100);
        other[0x10..0x14].copy_from_slice(b"7961");
        for bad in [patch(0), other, b"<!doctype html><html></html>".to_vec()] {
            let mut chip = Chip::new();
            let r = run(&mut chip, HashMap::from([(NAME.to_owned(), bad)]));
            assert!(matches!(r.result, Err(LoadError::BadFile { .. })), "{:?}", r.result);
            assert!(chip.wmt.is_empty() && chip.parts.is_empty(), "nothing sent");
        }
    }

    #[test]
    fn an_unreadable_rom_version_ends_it_before_sending() {
        let mut chip = Chip { rom_version: Err("stall".into()), ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert!(matches!(r.result, Err(LoadError::Host(_))), "{:?}", r.result);
        assert!(chip.wmt.is_empty());
    }

    #[test]
    fn a_rejected_part_or_reset_ends_it() {
        let mut chip = Chip { part: Some(3), ..Chip::new() };
        let r = run(&mut chip, files(3 * MAX_PART));
        assert_eq!(r.result, Err(LoadError::Rejected(3)));
        assert_eq!(chip.parts.len(), 1, "no part after the rejected one");
        assert!(!chip.bluetooth_on);
        let mut chip = Chip { reset: Some(1), ..Chip::new() };
        assert_eq!(run(&mut chip, files(10)).result, Err(LoadError::Rejected(1)));
        assert!(!chip.bluetooth_on);
    }

    #[test]
    fn an_error_status_ends_it() {
        let mut chip = Chip { command_status: Some(0x0C), ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert_eq!(r.result, Err(LoadError::Command { opcode: OP_WMT, status: 0x0C }));
        assert!(chip.parts.is_empty());
        // A Command Status with status 0 is no answer
        struct StatusChip(Chip);
        impl SimController for StatusChip {
            fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                let mut e = self.0.command(c);
                if sim::opcode(c) == OP_WMT {
                    e.push(command_status(OP_WMT, 0));
                }
                e
            }
            fn bulk_command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                self.0.bulk_command(c)
            }
            fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
                self.0.control(r)
            }
        }
        let mut loader = Loader::new(Box::new(|_| Some(patch(10))));
        let r = sim::run(&mut loader, &mut StatusChip(Chip::new()));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
    }

    #[test]
    fn bluetooth_on_not_yet_is_asked_again() {
        let mut chip = Chip { bt_on: vec![2, 2, 0], ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert!(r.result.is_ok(), "{:?}", r.result);
        assert_eq!(wmt_ops(&chip).iter().filter(|w| **w == (WMT_FUNC_CTRL, 0)).count(), 3);
        assert_eq!(r.timers.iter().filter(|&&t| t == BT_ON_RETRY_MS).count(), 2);
        // Three times again at most
        let mut chip = Chip { bt_on: vec![2], ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert_eq!(r.result, Err(LoadError::Timeout("Bluetooth on")));
        assert_eq!(wmt_ops(&chip).iter().filter(|w| **w == (WMT_FUNC_CTRL, 0)).count(), 4);
        let mut chip = Chip { bt_on: vec![5], ..Chip::new() };
        assert_eq!(run(&mut chip, files(10)).result, Err(LoadError::Rejected(5)));
    }

    /// MediaTek's driver downloads after an unanswered query and ignores
    /// Wi-Fi on's answer
    #[test]
    fn an_unanswered_query_or_wifi_on_does_not_stop_the_download() {
        let mut chip = Chip { query: None, wifi_on: None, ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
        assert_eq!(chip.parts.len(), 1);
        let mut chip = Chip { wifi_on: Some(1), ..Chip::new() };
        assert!(run(&mut chip, files(10)).result.is_ok());
    }

    /// The driver's plain patch file, in System32\drivers or its driver
    /// store package; only MT7663 patch names, only real patches
    #[test]
    fn reads_the_patch_from_mediateks_driver() {
        let windows = std::env::temp_dir().join(format!("bc-mt7663-driver-{}", std::process::id()));
        let drivers = windows.join("System32").join("drivers");
        let package =
            windows.join("System32").join("DriverStore").join("FileRepository").join("mtkbtfilter.inf_amd64_1");
        for d in [&drivers, &package] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(package.join(NAME), patch(5)).unwrap();
        std::fs::write(drivers.join("mt7663_patch_e3_hdr.bin"), b"not a patch").unwrap();
        std::fs::write(drivers.join("BT_RAM_CODE_MT7961_1_2_hdr.bin"), patch(5)).unwrap();
        let found = from_windows_driver(NAME, &windows);
        let broken = from_windows_driver("mt7663_patch_e3_hdr.bin", &windows);
        let other = from_windows_driver("BT_RAM_CODE_MT7961_1_2_hdr.bin", &windows);
        let _ = std::fs::remove_dir_all(&windows);
        assert_eq!(found, Some(patch(5)));
        assert_eq!(broken, None);
        assert_eq!(other, None, "the MT79xx files stay with the container");
    }

    /// MediaTek's driver neither sends Read Local Version nor reads
    /// 0x70010200 on an MT7663: neither may stop it
    #[test]
    fn steps_its_driver_skips_cannot_stop_it() {
        let mut chip = Chip { version_silent: true, ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
        assert!(chip.bluetooth_on);
        for first in [Err("stall".to_owned()), Ok(0xDEAD_BEEF)] {
            let mut chip = Chip { id_79xx: first, ..Chip::new() };
            let r = run(&mut chip, files(10));
            assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
            assert!(chip.bluetooth_on);
        }
        let mut chip = Chip { version_silent: true, id_79xx: Err("stall".into()), ..Chip::new() };
        assert!(run(&mut chip, files(10)).result.is_ok());
    }

    #[test]
    fn a_silent_chip_times_out_at_the_patch() {
        let mut chip = Chip { query: None, wifi_on: None, part: None, ..Chip::new() };
        let r = run(&mut chip, files(3 * MAX_PART));
        assert_eq!(r.result, Err(LoadError::Timeout("download")));
        assert_eq!(chip.parts.len(), 1, "the second part waits for the first's answer");
    }

    #[test]
    fn each_later_wait_can_time_out() {
        let mut chip = Chip { reset: None, ..Chip::new() };
        assert_eq!(run(&mut chip, files(10)).result, Err(LoadError::Timeout("WMT reset")));
        let mut chip = Chip { bt_on: Vec::new(), ..Chip::new() };
        assert_eq!(run(&mut chip, files(10)).result, Err(LoadError::Timeout("Bluetooth on")));
    }

    #[test]
    fn a_failed_event_read_ends_it() {
        let mut chip = Chip { fail_read: Some(1), ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert!(matches!(r.result, Err(LoadError::Host(_))), "{:?}", r.result);
        assert!(chip.parts.is_empty());
    }

    /// Only 0x7663 in 0x80000008 takes this path: an MT7668, which an old
    /// MediaTek driver loaded the same way, is refused
    #[test]
    fn only_the_mt7663_is_taken() {
        let mut chip = Chip { chip_id: 0x7668, ..Chip::new() };
        let r = run(&mut chip, files(10));
        assert!(matches!(r.result, Err(LoadError::Unsupported(_))), "{:?}", r.result);
        assert!(chip.wmt.is_empty());
    }

    #[test]
    fn tells_its_step_and_progress() {
        let files = files(3 * MAX_PART);
        let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
        use crate::bringup::ChipLoader;
        loader.start();
        loader.on_event(&command_complete(0x1001, 0x0C, &[]));
        loader.on_control(Ok(0u32.to_le_bytes().to_vec()));
        loader.on_control(Ok(CHIP_ID.to_le_bytes().to_vec()));
        assert_eq!(loader.step(), "chip ID");
        loader.on_control(Ok(1u32.to_le_bytes().to_vec()));
        assert_eq!((loader.step(), loader.progress()), ("download", None));
        loader.on_timeout();
        loader.on_control(Ok(event(EVENT_WMT, &[0x02, WMT_PATCH_QUERY, 1, 0, 2])));
        loader.on_control(Ok(event(EVENT_WMT, &[0x02, WMT_FUNC_CTRL, 1, 0, 0])));
        assert_eq!((loader.step(), loader.progress()), ("download", Some((0, 3))));
        loader.on_control(Ok(event(EVENT_WMT, &[0x02, WMT_PATCH_DOWNLOAD, 1, 0, 0])));
        assert_eq!(loader.progress(), Some((1, 3)));
        assert!(loader.missing_file_hint(NAME).contains("MT7663"));
    }

    /// The USB IDs of MediaTek's MT7663 drivers are listed for the loader
    #[test]
    fn its_usb_ids_are_listed() {
        use crate::chips::{ChipVendor, LOADER_CHIPS, vendor_for_usb};
        for (vid, pid) in [(0x0E8D, 0x7663), (0x13D3, 0x3550), (0x13D3, 0x3560), (0x04CA, 0x3801)] {
            assert!(LOADER_CHIPS.contains(&(ChipVendor::MediaTek, vid, pid)), "{vid:04X}:{pid:04X}");
            assert_eq!(vendor_for_usb(vid, pid, None), ChipVendor::MediaTek);
        }
    }

    /// Against MediaTek's real patch files: `MT7663_PATCH=<file>[;<file>...]
    /// cargo test -p bt-host --lib mt7663_real -- --ignored`
    #[test]
    #[ignore = "needs MediaTek's mt7663_patch_e2_hdr.bin (MT7663_PATCH)"]
    fn mt7663_real_patch_files_load() {
        let paths = std::env::var("MT7663_PATCH").expect("MT7663_PATCH");
        for path in paths.split(';') {
            let file = std::fs::read(path).unwrap();
            let body = patch_body(&file).unwrap().to_vec();
            let mut chip = Chip::new();
            let r = run(&mut chip, HashMap::from([(NAME.to_owned(), file)]));
            assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }), "{path}");
            let sent: Vec<u8> = chip.parts.iter().flat_map(|(_, d)| d.clone()).collect();
            assert_eq!(sent, body, "{path}");
            eprintln!("{path}: {} bytes in {} parts", sent.len(), chip.parts.len());
        }
    }
}
