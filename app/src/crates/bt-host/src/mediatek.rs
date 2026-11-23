//! MediaTek MT79xx firmware download over USB (issue #86)
//!
//! A MediaTek combo chip (MT7925, the Bluetooth half of a Wi-Fi card) starts
//! with its ROM: it answers standard HCI commands with "Command Disallowed"
//! (0x0C) until its RAM code is downloaded and Bluetooth is switched on. Both
//! go through MediaTek's WMT ("wireless management task") commands:
//!
//! - a WMT command is the vendor HCI command 0xFC6F: direction 1, WMT
//!   opcode, length (data + 1, little-endian), a flag, the data
//! - its answer is a WMT event (event code 0xE4: direction 2, the same
//!   opcode, length, flag, data). Over USB the chip does not send it on the
//!   interrupt pipe: the host reads it with a vendor control transfer (IN,
//!   request 1, value 0x30), which comes back empty until the event is there
//!
//! The firmware file (`BT_RAM_CODE_MT7925_1_1_hdr.bin`, the same name in
//! MediaTek's Windows driver and in linux-firmware) has a 32-byte header, a
//! 64-byte global descriptor (the section count at offset 12) and one 64-byte
//! map per section: type, offset and size in the file, then 52 bytes the chip
//! is told before the section is sent. For each section the loader asks
//! whether the chip has it (a download command with flag 0 and those 52
//! bytes): "done" skips it, "in progress" asks again, else the section goes
//! in chunks of up to 250 bytes (flag 1 first, 2 middle, 3 last). Then
//! Bluetooth is switched on (WMT function control, 1) and HCI init can run.
//!
//! The layout and the steps were read from MediaTek's Windows driver package
//! (mtkbtfilterx.sys and the firmware it ships, 26.40.3.69 and 1.1147.0.616)
//! and the chip probe in #86; written without vendor or GPL source.
//!
//! The MT7927 (chip ID 0x6639, experimental, not tested on hardware) goes
//! the MT7925's way in that driver, with three differences: its file is
//! `BT_RAM_CODE_MT6639_2_1_hdr.bin`, which has sections the driver does not
//! send at start (it picks them by type, [`START_SECTION_TYPES`]), and one
//! more WMT command comes before Bluetooth on ([`MT6639_SETUP`]). The
//! driver also starts its interface 2 at alternate setting 0, where the
//! MT7925 gets 1: hci_transport's iso_intr, whose LE Audio settings were
//! measured on an MT7925, picks the setting from the descriptors and does
//! not know the chip, so they are unconfirmed on the MT7927.
//!
//! The MT7663, the generation before, has a chip ID register of its own
//! (0x80000008 reads 0x7663 on it) and other steps: [`mt7663`].
//!
//! Firmware: the folder first (as for the other loaders, also in its
//! `mediatek\` subfolder), then the container MediaTek's Windows driver
//! installed (`mtkbt_v2.dat`, see [`from_windows_driver`]): a PC with a
//! MediaTek card has it even after the adapter moved to WinUSB. The MT7663
//! driver installs its patch as a plain file instead.

mod mt7663;

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::bringup::{
    Action, ChipLoader, ControlData, ControlKind, ControlRecipient, ControlRequest, FileReader, HciEvent, LoadError,
    Outcome, hci_command, parse_event,
};

const OP_READ_LOCAL_VERSION: u16 = 0x1001;
/// The WMT command
pub const OP_WMT: u16 = 0xFC6F;
/// The WMT event's code
pub const EVENT_WMT: u8 = 0xE4;

/// WMT opcodes
pub const WMT_PATCH_DOWNLOAD: u8 = 0x01;
pub const WMT_FUNC_CTRL: u8 = 0x06;

/// Download flags: the section query, then the first, a middle and the last
/// chunk
const FLAG_QUERY: u8 = 0;
const FLAG_FIRST: u8 = 1;
const FLAG_MIDDLE: u8 = 2;
const FLAG_LAST: u8 = 3;

/// The answer to a section query, in the event's flag
const SECTION_IN_PROGRESS: u8 = 1;
const SECTION_DONE: u8 = 2;

/// Firmware bytes per WMT command (the HCI parameters hold 255, the WMT
/// header takes 5)
pub const CHUNK: usize = 250;

/// Vendor control requests: read a chip register (IN, 4 bytes; value = the
/// register's upper half, index = the lower), read the pending WMT event
const REQUEST_READ_REGISTER: u8 = 0x63;
const REQUEST_WMT_EVENT: u8 = 0x01;
const WMT_EVENT_VALUE: u16 = 0x30;
/// The longest WMT event the chip sends
const WMT_EVENT_MAX: u16 = 64;

/// Chip ID registers: the first answers on older chips, the second on the
/// MT79xx (0 from the first)
const REGISTER_CHIP_ID: u32 = 0x7001_0200;
const REGISTER_CHIP_ID_79XX: u32 = 0x8000_0008;
/// The ROM version: its low byte + 1 is the firmware file's last number
const REGISTER_FW_VERSION: u32 = 0x8002_1004;
/// The MT7961's flavor: bit 7 set takes the "1a" file
/// (BT_RAM_CODE_MT7961_1a_2_hdr.bin)
const REGISTER_FLAVOR: u32 = 0x7001_0020;
const FLAVOR_1A: u32 = 0x80;
const CHIP_FLAVORED: u16 = 0x7961;

/// Chips this loader takes (the chip ID's low half): the MT79xx of
/// MediaTek's Windows driver, whose files share one layout. The MT7921
/// reports 0x7961, the MT7927 0x6639. USB IDs in chips::LOADER_CHIPS; the
/// MT7925 (#86) and an MT7921 have been seen, the others are untested.
pub const CHIPS: &[u16] = &[0x7925, 0x7922, 0x7961, 0x7902, CHIP_MT6639];

/// The MT7927's chip ID
const CHIP_MT6639: u16 = 0x6639;

/// The chips whose sections MediaTek's driver picks by type at start
/// ([`START_SECTION_TYPES`]); the others get every section in file order
const TYPED_SECTION_CHIPS: &[u16] = &[0x7925, CHIP_MT6639];
/// The section types (a section map's word 7) the driver sends at start, in
/// this order, the first section of each. The MT6639's file has more
/// (type 0x20000) that the driver sends only later, on a separate path; the
/// MT7925's lists one without data. For the MT7925 file this is its file
/// order.
const START_SECTION_TYPES: &[u32] = &[0, 2, 3, 0x80, 0x90, 0x1_0000];

/// The MT6639's command between the last section and Bluetooth on, as
/// MediaTek's driver sends it (WMT opcode, flag, data), and the answer it
/// expects (flag, data). Its meaning is not known.
const MT6639_SETUP: (u8, u8, &[u8]) = (0x55, 0x02, &[0x02, 0x01, 0x03, 0x03, 0x03, 0x01]);
const MT6639_SETUP_ANSWER: (u8, &[u8]) = (0x00, &[0x02]);

/// The firmware file's layout
const HEADER_SIZE: usize = 32;
const GLOBAL_SIZE: usize = 64;
const SECTION_MAP_SIZE: usize = 64;
/// The part of a section map the chip is told before the section: after
/// type, offset and size
const SECTION_SPEC: std::ops::Range<usize> = 12..64;
/// More is a broken file
const MAX_SECTIONS: usize = 64;

pub const COMMAND_TIMEOUT_MS: u32 = 2000;
/// A WMT event read that came back empty is repeated after this
const POLL_INTERVAL_MS: u32 = 1;
/// Empty reads before a WMT command counts as unanswered: the timer is
/// coarse, so the clock decides too ([`WMT_TIMEOUT_MS`])
const MAX_POLLS: u32 = 3000;
const WMT_TIMEOUT_MS: u128 = 3000;
/// Read Local Version sends: the first event after a restart can get lost
const VERSION_TRIES: u8 = 2;
/// "In progress" answers to a section query before giving up, one every
/// [`PROGRESS_WAIT_MS`]
const MAX_PROGRESS: u32 = 20;
const PROGRESS_WAIT_MS: u32 = 100;
/// Between the last section and switching Bluetooth on
const SETTLE_MS: u32 = 100;

/// One section to send: the 52 bytes of its map, its data
#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    spec: Vec<u8>,
    data: Vec<u8>,
}

/// A firmware file as `chip` takes it: the sections with data, in the order
/// to send them. Err: why the file is not one
fn parse_firmware(file: &[u8], chip: u16) -> Result<Vec<Section>, &'static str> {
    let count_at = HEADER_SIZE + 12;
    let count = file.get(count_at..count_at + 4).ok_or("too short for a MediaTek firmware header")?;
    let count = u32::from_le_bytes([count[0], count[1], count[2], count[3]]) as usize;
    if count == 0 || count > MAX_SECTIONS {
        return Err("no plausible section count");
    }
    // (type, section) of every section with data
    let mut found = Vec::new();
    for i in 0..count {
        let at = HEADER_SIZE + GLOBAL_SIZE + i * SECTION_MAP_SIZE;
        let map = file.get(at..at + SECTION_MAP_SIZE).ok_or("section maps cut short")?;
        let word = |n: usize| u32::from_le_bytes([map[n * 4], map[n * 4 + 1], map[n * 4 + 2], map[n * 4 + 3]]);
        // Offset in the file, then the size to download (the spec's second
        // word); sections without one only describe the image
        let (offset, size) = (word(1) as usize, word(4) as usize);
        if size == 0 {
            continue;
        }
        let data =
            offset.checked_add(size).and_then(|end| file.get(offset..end)).ok_or("a section is outside the file")?;
        found.push((word(7), Section { spec: map[SECTION_SPEC].to_vec(), data: data.to_vec() }));
    }
    let sections: Vec<Section> = if TYPED_SECTION_CHIPS.contains(&chip) {
        START_SECTION_TYPES
            .iter()
            .filter_map(|t| found.iter().position(|(kind, _)| kind == t))
            .map(|i| found[i].1.clone())
            .collect()
    } else {
        found.into_iter().map(|(_, s)| s).collect()
    };
    if sections.is_empty() {
        return Err("no section to download");
    }
    Ok(sections)
}

/// A WMT command: direction 1, opcode, length of flag + data, flag, data
pub fn wmt_command(op: u8, flag: u8, data: &[u8]) -> Vec<u8> {
    let len = (data.len() + 1) as u16;
    let mut params = vec![0x01, op];
    params.extend_from_slice(&len.to_le_bytes());
    params.push(flag);
    params.extend_from_slice(data);
    hci_command(OP_WMT, &params)
}

/// Whether this command is the loader's "Bluetooth on" (WMT function
/// control, 1); Developer > Debug steps go in before it
/// (crate::mtk_debug_steps)
pub fn is_func_on(command: &[u8]) -> bool {
    command == wmt_command(WMT_FUNC_CTRL, 0, &[0x01]).as_slice()
}

/// A WMT event as read: (opcode, flag, data after the flag); None if the
/// bytes are not one. The code is 0xE4, or 0xFF where a host rewrote it as a
/// plain vendor event.
fn parse_wmt_event(event: &[u8]) -> Option<(u8, u8, &[u8])> {
    match event {
        [code, len, rest @ ..] if (*code == EVENT_WMT || *code == 0xFF) && rest.len() >= *len as usize => {
            match &rest[..*len as usize] {
                [0x02, op, _, _, flag, data @ ..] => Some((*op, *flag, data)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn read_register(register: u32) -> ControlRequest {
    ControlRequest {
        kind: ControlKind::Vendor,
        recipient: ControlRecipient::Device,
        request: REQUEST_READ_REGISTER,
        value: (register >> 16) as u16,
        index: register as u16,
        data: ControlData::In(4),
    }
}

fn read_wmt_event() -> ControlRequest {
    ControlRequest {
        kind: ControlKind::Vendor,
        recipient: ControlRecipient::Device,
        request: REQUEST_WMT_EVENT,
        value: WMT_EVENT_VALUE,
        index: 0,
        data: ControlData::In(WMT_EVENT_MAX),
    }
}

/// The firmware file of a chip: "BT_RAM_CODE_MT7925_1_1_hdr.bin", the
/// MT7961's "1a" flavor "BT_RAM_CODE_MT7961_1a_2_hdr.bin", the MT6639's
/// "BT_RAM_CODE_MT6639_2_1_hdr.bin"
pub fn firmware_name(chip: u16, fw_version: u32, flavor_a: bool) -> String {
    let flavor = match (chip, flavor_a) {
        (CHIP_MT6639, _) => "2",
        (_, true) => "1a",
        (_, false) => "1",
    };
    format!("BT_RAM_CODE_MT{chip:04X}_{flavor}_{:x}_hdr.bin", (fw_version & 0xFF) + 1)
}

/// What a WMT command is for: what its answer moves on to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wmt {
    /// Does the chip have section `.0`?
    Query(usize),
    /// A chunk of the section
    Chunk,
    /// The MT6639's [`MT6639_SETUP`], before Bluetooth on
    Setup,
    /// Bluetooth on
    FuncOn,
}

enum State {
    Idle,
    /// Read Local Version sent: answered when the firmware runs. Sent again
    /// once: the first event after a restart can get lost
    Version {
        tries: u8,
    },
    /// Chip ID and ROM version: the register reads asked for
    ChipId,
    ChipId79xx,
    FwVersion {
        chip: u16,
    },
    Flavor {
        chip: u16,
        fw_version: u32,
    },
    /// A WMT command sent: its event is read with control transfers
    Waiting {
        what: Wmt,
        polls: u32,
        since: Instant,
    },
    /// Waiting before the next step (a section in progress, the settle
    /// time): the timer moves on
    Pause {
        then: Wmt,
    },
    Done,
}

pub struct Loader {
    read_file: FileReader,
    state: State,
    started: Instant,
    /// The chip ID, once read
    chip: u16,
    file: String,
    sections: Vec<Section>,
    /// The section being sent, its chunks and the next one
    current: usize,
    chunks: Vec<Vec<u8>>,
    next_chunk: usize,
    /// "In progress" answers to the current section's query
    progress_waits: u32,
    /// Chunks acknowledged of all sections' chunks (sections the chip had
    /// count as done)
    done_chunks: usize,
    total_chunks: usize,
    /// Sections sent; 0 at the end: the chip had the firmware already
    sent_sections: usize,
    /// An MT7663: its own download runs from the chip ID on
    mt7663: Option<mt7663::Mt7663>,
}

impl Loader {
    pub fn new(read_file: FileReader) -> Loader {
        Loader {
            read_file,
            state: State::Idle,
            started: Instant::now(),
            chip: 0,
            file: String::new(),
            sections: Vec::new(),
            current: 0,
            chunks: Vec::new(),
            next_chunk: 0,
            progress_waits: 0,
            done_chunks: 0,
            total_chunks: 0,
            sent_sections: 0,
            mt7663: None,
        }
    }

    fn read_version(&mut self, tries: u8) -> Vec<Action> {
        self.state = State::Version { tries };
        vec![Action::Command(hci_command(OP_READ_LOCAL_VERSION, &[])), Action::Timer(COMMAND_TIMEOUT_MS)]
    }

    fn finish(&mut self, result: Result<Outcome, LoadError>) -> Vec<Action> {
        tracing::debug!("mediatek loader finished after {} ms: {result:?}", self.started.elapsed().as_millis());
        self.state = State::Done;
        vec![Action::Done(result)]
    }

    /// Sends a WMT command and starts reading its event
    fn send(&mut self, what: Wmt, command: Vec<u8>) -> Vec<Action> {
        self.state = State::Waiting { what, polls: 0, since: Instant::now() };
        vec![Action::Command(command), Action::UsbControl(read_wmt_event())]
    }

    fn query(&mut self, section: usize) -> Vec<Action> {
        let mut data = vec![0u8];
        data.extend_from_slice(&self.sections[section].spec);
        self.send(Wmt::Query(section), wmt_command(WMT_PATCH_DOWNLOAD, FLAG_QUERY, &data))
    }

    /// The query of section `index`, or Bluetooth on after the last one
    fn section(&mut self, index: usize) -> Vec<Action> {
        if index < self.sections.len() {
            self.progress_waits = 0;
            return self.query(index);
        }
        tracing::info!(
            "mediatek: {} of {} sections sent, switching Bluetooth on",
            self.sent_sections,
            self.sections.len()
        );
        self.state = State::Pause { then: Wmt::FuncOn };
        vec![Action::Timer(SETTLE_MS)]
    }

    fn chunk_count(section: &Section) -> usize {
        section.data.len().div_ceil(CHUNK)
    }

    fn start_section(&mut self, index: usize) -> Vec<Action> {
        let data = &self.sections[index].data;
        let n = Self::chunk_count(&self.sections[index]);
        self.chunks = data
            .chunks(CHUNK)
            .enumerate()
            .map(|(i, part)| {
                let flag = match i {
                    0 => FLAG_FIRST,
                    i if i + 1 == n => FLAG_LAST,
                    _ => FLAG_MIDDLE,
                };
                wmt_command(WMT_PATCH_DOWNLOAD, flag, part)
            })
            .collect();
        tracing::info!("mediatek: sending section {index}, {} bytes in {n} commands", data.len());
        self.sent_sections += 1;
        self.next_chunk = 1;
        let first = self.chunks[0].clone();
        self.current = index;
        self.send(Wmt::Chunk, first)
    }

    /// After the settle time: the MT6639's setup command, else Bluetooth on
    fn before_func_on(&mut self) -> Vec<Action> {
        if self.chip == CHIP_MT6639 {
            let (op, flag, data) = MT6639_SETUP;
            return self.send(Wmt::Setup, wmt_command(op, flag, data));
        }
        self.func_on()
    }

    fn func_on(&mut self) -> Vec<Action> {
        self.send(Wmt::FuncOn, wmt_command(WMT_FUNC_CTRL, 0, &[0x01]))
    }

    fn on_wmt(&mut self, what: Wmt, op: u8, flag: u8, data: &[u8]) -> Vec<Action> {
        let expected = match what {
            Wmt::FuncOn => WMT_FUNC_CTRL,
            Wmt::Setup => MT6639_SETUP.0,
            Wmt::Query(_) | Wmt::Chunk => WMT_PATCH_DOWNLOAD,
        };
        if op != expected {
            tracing::warn!("mediatek: WMT event for opcode 0x{op:02X}, waiting for 0x{expected:02X}");
            return self.finish(Err(LoadError::Unsupported("the chip answered another WMT command")));
        }
        match what {
            Wmt::Query(index) => match flag {
                SECTION_DONE => {
                    tracing::info!("mediatek: the chip has section {index} already");
                    self.done_chunks += Self::chunk_count(&self.sections[index]);
                    self.section(index + 1)
                }
                SECTION_IN_PROGRESS if self.progress_waits < MAX_PROGRESS => {
                    self.progress_waits += 1;
                    self.state = State::Pause { then: Wmt::Query(index) };
                    vec![Action::Timer(PROGRESS_WAIT_MS)]
                }
                SECTION_IN_PROGRESS => self.finish(Err(LoadError::Timeout("section still in progress"))),
                _ => self.start_section(index),
            },
            Wmt::Chunk => {
                self.done_chunks += 1;
                match self.chunks.get(self.next_chunk).cloned() {
                    Some(c) => {
                        self.next_chunk += 1;
                        self.send(Wmt::Chunk, c)
                    }
                    None => {
                        let next = self.current + 1;
                        self.section(next)
                    }
                }
            }
            Wmt::Setup => {
                // MediaTek's driver stops on another answer; going on shows
                // in the log whether the chip starts all the same
                if (flag, data) == MT6639_SETUP_ANSWER {
                    tracing::info!("mediatek: MT6639 setup command answered");
                } else {
                    tracing::warn!("mediatek: MT6639 setup command answered with flag {flag}, data {data:02X?}");
                }
                self.func_on()
            }
            Wmt::FuncOn => {
                let status = data.get(..2).map(|s| u16::from_be_bytes([s[0], s[1]]));
                tracing::info!("mediatek: Bluetooth on, status {status:04X?}");
                let file = std::mem::take(&mut self.file);
                let detail = (self.sent_sections == 0).then_some("the chip had the firmware already");
                self.finish(Ok(Outcome::Loaded { firmware: file, detail }))
            }
        }
    }

    /// The ROM version (and flavor) known: the file, else the chip's other
    /// file numbers MediaTek's driver ships ("_1_1", the MT7961's "_1_2")
    fn on_chip(&mut self, chip: u16, fw_version: u32, flavor_a: bool) -> Vec<Action> {
        let wanted = firmware_name(chip, fw_version, flavor_a);
        let mut names = vec![wanted.clone()];
        for v in [0, 1] {
            let n = firmware_name(chip, v, flavor_a);
            if !names.contains(&n) {
                names.push(n);
            }
        }
        let found = names.into_iter().find_map(|n| (self.read_file)(&n).map(|d| (n, d)));
        let Some((file, data)) = found else { return self.finish(Err(LoadError::MissingFile(wanted))) };
        if file != wanted {
            tracing::info!("mediatek: {wanted} not found, using {file}");
        }
        self.chip = chip;
        match parse_firmware(&data, chip) {
            Ok(sections) => {
                self.total_chunks = sections.iter().map(Self::chunk_count).sum();
                tracing::info!(
                    "mediatek: chip MT{chip:04X}, {file}: {} bytes, {} sections to check",
                    data.len(),
                    sections.len()
                );
                self.sections = sections;
                self.file = file;
                self.section(0)
            }
            Err(why) => self.finish(Err(LoadError::BadFile { file, why })),
        }
    }

    /// The MT7961 also tells its flavor; the others pick their file now
    fn after_version(&mut self, chip: u16, fw_version: u32) -> Vec<Action> {
        if chip == CHIP_FLAVORED {
            self.state = State::Flavor { chip, fw_version };
            return vec![Action::UsbControl(read_register(REGISTER_FLAVOR))];
        }
        self.on_chip(chip, fw_version, false)
    }

    fn on_register(&mut self, value: u32) -> Vec<Action> {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::ChipId | State::ChipId79xx if CHIPS.contains(&(value as u16)) => {
                let chip = value as u16;
                self.state = State::FwVersion { chip };
                vec![Action::UsbControl(read_register(REGISTER_FW_VERSION))]
            }
            State::ChipId => {
                tracing::debug!("mediatek: chip ID register 0x{REGISTER_CHIP_ID:08X} = 0x{value:08X}");
                self.state = State::ChipId79xx;
                vec![Action::UsbControl(read_register(REGISTER_CHIP_ID_79XX))]
            }
            State::ChipId79xx if value == mt7663::CHIP_ID => {
                let (m, actions) = mt7663::Mt7663::start();
                self.mt7663 = Some(m);
                actions
            }
            State::ChipId79xx => {
                tracing::warn!("mediatek: chip ID 0x{value:08X} is not one this loader knows");
                self.finish(Err(LoadError::Unsupported("a MediaTek chip this loader does not know")))
            }
            State::FwVersion { chip } => {
                tracing::info!("mediatek: ROM version 0x{value:08X}");
                self.after_version(chip, value)
            }
            State::Flavor { chip, fw_version } => {
                tracing::info!("mediatek: flavor register 0x{value:08X}");
                self.on_chip(chip, fw_version, value & FLAVOR_1A != 0)
            }
            state => {
                self.state = state;
                Vec::new()
            }
        }
    }

    /// A WMT event read returned: the event, or nothing yet
    fn on_poll(&mut self, data: Vec<u8>) -> Vec<Action> {
        let State::Waiting { what, polls, since } = self.state else { return Vec::new() };
        if data.first().is_none_or(|&b| b == 0) {
            if polls >= MAX_POLLS || since.elapsed().as_millis() > WMT_TIMEOUT_MS {
                return self.finish(Err(LoadError::Timeout(match what {
                    Wmt::Query(_) => "section query",
                    Wmt::Chunk => "download",
                    Wmt::Setup => "MT6639 setup",
                    Wmt::FuncOn => "Bluetooth on",
                })));
            }
            self.state = State::Waiting { what, polls: polls + 1, since };
            return vec![Action::Timer(POLL_INTERVAL_MS)];
        }
        match parse_wmt_event(&data) {
            Some((op, flag, rest)) => {
                let rest = rest.to_vec();
                self.on_wmt(what, op, flag, &rest)
            }
            None => {
                tracing::warn!("mediatek: unexpected answer to the WMT event read: {data:02X?}");
                self.finish(Err(LoadError::Unsupported("an answer that is not a WMT event")))
            }
        }
    }
}

impl ChipLoader for Loader {
    fn vendor(&self) -> &'static str {
        "MediaTek"
    }

    fn start(&mut self) -> Vec<Action> {
        self.started = Instant::now();
        self.read_version(1)
    }

    fn on_event(&mut self, event: &[u8]) -> Vec<Action> {
        if let Some(m) = &mut self.mt7663 {
            return m.on_event(event);
        }
        match (&self.state, parse_event(event)) {
            // Answered: the firmware runs and Bluetooth is on
            (State::Version { .. }, HciEvent::Complete { opcode: OP_READ_LOCAL_VERSION, status: 0, .. }) => {
                self.finish(Ok(Outcome::AlreadyRunning))
            }
            // The ROM refuses HCI: download
            (State::Version { .. }, HciEvent::Complete { opcode: OP_READ_LOCAL_VERSION, status, .. })
            | (State::Version { .. }, HciEvent::Status { opcode: OP_READ_LOCAL_VERSION, status }) => {
                tracing::info!("mediatek: Read Local Version refused (0x{status:02X}): the ROM runs, downloading");
                self.state = State::ChipId;
                vec![Action::UsbControl(read_register(REGISTER_CHIP_ID))]
            }
            // A WMT command refused outright
            (State::Waiting { .. }, HciEvent::Complete { opcode: OP_WMT, status, .. })
            | (State::Waiting { .. }, HciEvent::Status { opcode: OP_WMT, status })
                if status != 0 =>
            {
                self.finish(Err(LoadError::Command { opcode: OP_WMT, status }))
            }
            // A WMT event on the interrupt pipe after all
            (State::Waiting { what, .. }, _) => match parse_wmt_event(event) {
                Some((op, flag, rest)) => {
                    let what = *what;
                    self.on_wmt(what, op, flag, rest)
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn on_timeout(&mut self) -> Vec<Action> {
        if let Some(m) = &mut self.mt7663 {
            return m.on_timeout();
        }
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Version { tries } if tries < VERSION_TRIES => {
                tracing::info!("mediatek: no answer to Read Local Version, sending it again");
                self.read_version(tries + 1)
            }
            State::Version { .. } => {
                // WinUSB on a chip the OEM driver had (MT7921): no answer at
                // all. Not proof that the firmware runs (HCI Reset stays
                // unanswered too): the ROM may wait for its download, so read
                // the chip ID over endpoint 0 and take the normal path
                tracing::info!("mediatek: no answer to Read Local Version, reading the chip ID to download");
                self.state = State::ChipId;
                vec![Action::UsbControl(read_register(REGISTER_CHIP_ID))]
            }
            // The next read of the pending event
            state @ State::Waiting { .. } => {
                self.state = state;
                vec![Action::UsbControl(read_wmt_event())]
            }
            State::Pause { then: Wmt::Query(index) } => self.query(index),
            State::Pause { then: Wmt::FuncOn } => self.before_func_on(),
            state => {
                self.state = state;
                Vec::new()
            }
        }
    }

    fn on_control(&mut self, result: Result<Vec<u8>, String>) -> Vec<Action> {
        if let Some(m) = &mut self.mt7663 {
            return m.on_control(result, &mut self.read_file);
        }
        // The first chip ID register unreadable: the second may still name
        // the chip (MediaTek's driver never reads the first on an MT7663)
        if matches!(self.state, State::ChipId) && !matches!(&result, Ok(d) if d.len() == 4) {
            tracing::info!(
                "mediatek: chip ID register 0x{REGISTER_CHIP_ID:08X} unreadable ({result:02X?}), reading 0x{REGISTER_CHIP_ID_79XX:08X}"
            );
            self.state = State::ChipId79xx;
            return vec![Action::UsbControl(read_register(REGISTER_CHIP_ID_79XX))];
        }
        let reading_register =
            matches!(self.state, State::ChipId | State::ChipId79xx | State::FwVersion { .. } | State::Flavor { .. });
        // The ROM version and the flavor only pick the file: without them,
        // the chip's first file
        let unread = |state: &State| match *state {
            State::FwVersion { chip } => Some((chip, 0, true)),
            State::Flavor { chip, fw_version } => Some((chip, fw_version, false)),
            _ => None,
        };
        match result {
            Ok(d) if reading_register && d.len() == 4 => self.on_register(u32::from_le_bytes([d[0], d[1], d[2], d[3]])),
            Ok(_) if reading_register => match unread(&self.state) {
                Some((chip, v, true)) => self.after_version(chip, v),
                Some((chip, v, false)) => self.on_chip(chip, v, false),
                None => self.finish(Err(LoadError::Unsupported("chip ID register of another size"))),
            },
            Ok(d) => self.on_poll(d),
            Err(e) => {
                if let Some((chip, v, version)) = unread(&self.state) {
                    tracing::warn!(
                        "mediatek: reading the {} failed: {e}",
                        if version { "ROM version" } else { "flavor" }
                    );
                    return if version { self.after_version(chip, v) } else { self.on_chip(chip, v, false) };
                }
                if matches!(self.state, State::Idle | State::Done | State::Version { .. } | State::Pause { .. }) {
                    return Vec::new();
                }
                tracing::warn!("mediatek: USB control transfer failed: {e}");
                self.finish(Err(LoadError::Host("a USB control transfer to the chip failed")))
            }
        }
    }

    fn missing_file_hint(&self, file: &str) -> String {
        if self.mt7663.is_some() {
            return format!("{file}, from MediaTek's MT7663 Windows Bluetooth driver (read from it when installed)");
        }
        format!(
            "{file}, from MediaTek's Windows Bluetooth driver (read from it when installed) or linux-firmware (mediatek/)"
        )
    }

    fn step(&self) -> &'static str {
        if let Some(m) = &self.mt7663 {
            return m.step();
        }
        match self.state {
            State::Version { .. } => "version",
            State::ChipId | State::ChipId79xx | State::FwVersion { .. } | State::Flavor { .. } => "chip ID",
            State::Waiting { what: Wmt::FuncOn | Wmt::Setup, .. } | State::Pause { then: Wmt::FuncOn } => "start",
            State::Waiting { .. } | State::Pause { .. } => "download",
            State::Idle | State::Done => "",
        }
    }

    fn progress(&self) -> Option<(usize, usize)> {
        if let Some(m) = &self.mt7663 {
            return m.progress();
        }
        match self.state {
            State::Waiting { what: Wmt::Query(_) | Wmt::Chunk, .. } | State::Pause { then: Wmt::Query(_) } => {
                Some((self.done_chunks, self.total_chunks))
            }
            _ => None,
        }
    }
}

// ---- MediaTek's Windows driver ----

/// A file in the container MediaTek's Windows Bluetooth driver installs
/// (`mtkbt_v2.dat`): "MTK-", the entry count (u16) at 4, then from 0x10 one
/// 0x4C-byte entry per file: its name (0x30 bytes), a time stamp (0x10), its
/// offset and size (u32 each), 4 reserved bytes
pub fn driver_container_file(container: &[u8], name: &str) -> Option<Vec<u8>> {
    const FIRST: usize = 0x10;
    const ENTRY: usize = 0x4C;
    if !container.starts_with(b"MTK-") {
        return None;
    }
    let count = u16::from_le_bytes([*container.get(4)?, *container.get(5)?]) as usize;
    (0..count).find_map(|i| {
        let entry = container.get(FIRST + i * ENTRY..FIRST + (i + 1) * ENTRY)?;
        let stored = entry[..0x30].split(|&b| b == 0).next()?;
        if !stored.eq_ignore_ascii_case(name.as_bytes()) {
            return None;
        }
        let word = |at: usize| u32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]]) as usize;
        let (offset, size) = (word(0x40), word(0x44));
        container.get(offset..offset.checked_add(size)?).map(<[u8]>::to_vec)
    })
}

/// The containers (mtkbt*.dat) of MediaTek's Windows driver on this PC,
/// newest first (crate::driver_files): its packages in the driver store
/// (mtkbtfilter.inf, mtkbtfilterx.inf; its INF keeps the container there),
/// and copies in the Windows folder, System32 and System32\drivers. All stay
/// after the adapter moved to WinUSB.
fn driver_containers(windows: &Path) -> Vec<PathBuf> {
    let container = |n: &str| n.starts_with("mtkbt") && n.ends_with(".dat");
    crate::driver_files::find(
        windows,
        crate::driver_files::Names::Matching(&container),
        &|n: &str| n.starts_with("mtkbt"),
        &[],
    )
}

/// A firmware file from MediaTek's Windows driver, if it is installed: from
/// its container, or the MT7663 driver's plain patch file
pub fn from_windows_driver(name: &str) -> Option<Vec<u8>> {
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    driver_containers(&windows)
        .iter()
        .find_map(|path| {
            let data = std::fs::read(path).ok()?;
            let file = driver_container_file(&data, name)?;
            tracing::info!(
                "mediatek: {name} from MediaTek's driver ({}), {} bytes",
                crate::driver_files::file_name(path),
                file.len()
            );
            Some(file)
        })
        .or_else(|| mt7663::from_windows_driver(name, &windows))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::bringup::sim::{self, SimController, command_complete, event};

    /// A firmware file: the header, one map per (size, done-already) section
    fn firmware(sizes: &[usize]) -> Vec<u8> {
        let mut f = vec![0u8; HEADER_SIZE + GLOBAL_SIZE];
        f[..14].copy_from_slice(b"20260821201251");
        f[HEADER_SIZE + 12..HEADER_SIZE + 16].copy_from_slice(&(sizes.len() as u32).to_le_bytes());
        let mut offset = HEADER_SIZE + GLOBAL_SIZE + sizes.len() * SECTION_MAP_SIZE;
        let mut body = Vec::new();
        for (i, &size) in sizes.iter().enumerate() {
            let mut map = vec![0u8; SECTION_MAP_SIZE];
            map[0..4].copy_from_slice(&0x0003_0002u32.to_le_bytes());
            map[4..8].copy_from_slice(&(offset as u32).to_le_bytes());
            map[8..12].copy_from_slice(&(size as u32).to_le_bytes());
            // dlAddr, dlSize, then a marker to find the spec by
            map[12..16].copy_from_slice(&(0x0090_0000u32 + i as u32).to_le_bytes());
            map[16..20].copy_from_slice(&(size as u32).to_le_bytes());
            // Types in the order the MT7925 is sent them
            map[28..32].copy_from_slice(&START_SECTION_TYPES[(i + 1) % START_SECTION_TYPES.len()].to_le_bytes());
            map[63] = 0xA0 + i as u8;
            f.extend_from_slice(&map);
            body.extend((0..size).map(|n| (n + i) as u8));
            offset += size;
        }
        f.extend_from_slice(&body);
        f
    }

    /// The chip, as the probe in #86 showed its ROM: HCI refused with 0x0C
    struct Chip {
        firmware_runs: bool,
        chip_id: u32,
        id_79xx: u32,
        fw_version: Result<u32, String>,
        flavor: Result<u32, String>,
        /// Sections it has already; answered "in progress" this often first
        has: Vec<usize>,
        busy: u32,
        /// Empty reads before each event
        empty_reads: u32,
        /// Never answers a WMT command
        silent: bool,
        /// Read Local Version answers lost on the way
        lost_versions: u32,
        /// WMT events not read yet
        pending: Vec<Vec<u8>>,
        empty_left: u32,
        /// The sections' specs as queried
        specs: Vec<Vec<u8>>,
        /// Sent chunks per section, the current one
        received: Vec<Vec<u8>>,
        current: Option<usize>,
        bluetooth_on: bool,
        /// The MT6639's setup command came
        setup: bool,
    }

    impl Chip {
        fn new() -> Chip {
            Chip {
                firmware_runs: false,
                chip_id: 0,
                id_79xx: 0x0000_7925,
                fw_version: Ok(0x0000_8a00),
                flavor: Ok(0),
                has: Vec::new(),
                busy: 0,
                empty_reads: 2,
                silent: false,
                lost_versions: 0,
                pending: Vec::new(),
                empty_left: 0,
                specs: Vec::new(),
                received: Vec::new(),
                current: None,
                bluetooth_on: false,
                setup: false,
            }
        }

        fn answer(&mut self, op: u8, flag: u8, data: &[u8]) {
            let mut params = vec![0x02, op];
            params.extend_from_slice(&((data.len() + 1) as u16).to_le_bytes());
            params.push(flag);
            params.extend_from_slice(data);
            self.pending.push(event(EVENT_WMT, &params));
            self.empty_left = self.empty_reads;
        }
    }

    impl SimController for Chip {
        fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
            let opcode = sim::opcode(c);
            if opcode == OP_READ_LOCAL_VERSION {
                if self.lost_versions > 0 {
                    self.lost_versions -= 1;
                    return Vec::new();
                }
                let status = if self.firmware_runs { 0 } else { 0x0C };
                return vec![command_complete(opcode, status, &[])];
            }
            assert_eq!(opcode, OP_WMT, "only WMT commands before HCI init");
            if self.silent {
                return Vec::new();
            }
            let p = sim::params(c);
            let (op, len, flag, data) = (p[1], u16::from_le_bytes([p[2], p[3]]) as usize, p[4], &p[5..]);
            assert_eq!((p[0], len), (0x01, data.len() + 1), "WMT header");
            match (op, flag) {
                (WMT_PATCH_DOWNLOAD, FLAG_QUERY) => {
                    assert_eq!((data.len(), data[0]), (53, 0), "0 then the section's 52 bytes");
                    // Sections are asked in order: the index of its first query
                    let spec = data[1..].to_vec();
                    let section = match self.specs.iter().position(|s| *s == spec) {
                        Some(i) => i,
                        None => {
                            self.specs.push(spec);
                            self.specs.len() - 1
                        }
                    };
                    if self.has.contains(&section) {
                        self.answer(op, SECTION_DONE, &[]);
                    } else if self.busy > 0 {
                        self.busy -= 1;
                        self.answer(op, SECTION_IN_PROGRESS, &[]);
                    } else {
                        self.current = Some(section);
                        self.answer(op, 0, &[]);
                    }
                }
                (WMT_PATCH_DOWNLOAD, flag) => {
                    let section = self.current.expect("a chunk before its section's query");
                    while self.received.len() <= section {
                        self.received.push(Vec::new());
                    }
                    let first = self.received[section].is_empty();
                    assert_eq!(flag == FLAG_FIRST, first, "flag 1 only on the first chunk");
                    assert!(data.len() <= CHUNK);
                    self.received[section].extend_from_slice(data);
                    self.answer(op, 0, &[]);
                }
                (op, flag) if (op, flag) == (MT6639_SETUP.0, MT6639_SETUP.1) => {
                    assert_eq!(self.id_79xx as u16, CHIP_MT6639, "only the MT6639 gets it");
                    assert_eq!(data, MT6639_SETUP.2);
                    assert!(!self.received.is_empty() || !self.has.is_empty(), "after the download");
                    self.setup = true;
                    self.answer(op, MT6639_SETUP_ANSWER.0, MT6639_SETUP_ANSWER.1);
                }
                (WMT_FUNC_CTRL, 0) => {
                    assert_eq!(self.setup, self.id_79xx as u16 == CHIP_MT6639, "the MT6639's setup first");
                    assert_eq!(data, [0x01]);
                    self.bluetooth_on = true;
                    self.answer(op, 0, &[0x04, 0x04]);
                }
                other => panic!("unexpected WMT command {other:?}"),
            }
            Vec::new()
        }

        fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
            assert_eq!((r.kind, r.recipient), (ControlKind::Vendor, ControlRecipient::Device));
            match (r.request, r.value, r.index, &r.data) {
                (REQUEST_READ_REGISTER, 0x7001, 0x0200, ControlData::In(4)) => Ok(self.chip_id.to_le_bytes().to_vec()),
                (REQUEST_READ_REGISTER, 0x8000, 0x0008, ControlData::In(4)) => Ok(self.id_79xx.to_le_bytes().to_vec()),
                (REQUEST_READ_REGISTER, 0x8002, 0x1004, ControlData::In(4)) => {
                    self.fw_version.clone().map(|v| v.to_le_bytes().to_vec())
                }
                (REQUEST_READ_REGISTER, 0x7001, 0x0020, ControlData::In(4)) => {
                    assert_eq!(self.id_79xx as u16, CHIP_FLAVORED, "only the MT7961 is asked its flavor");
                    self.flavor.clone().map(|v| v.to_le_bytes().to_vec())
                }
                (REQUEST_WMT_EVENT, WMT_EVENT_VALUE, 0, ControlData::In(WMT_EVENT_MAX)) => {
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

    const NAME: &str = "BT_RAM_CODE_MT7925_1_1_hdr.bin";

    fn run(chip: &mut Chip, files: HashMap<String, Vec<u8>>) -> sim::SimRun {
        let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
        sim::run(&mut loader, chip)
    }

    fn files(sizes: &[usize]) -> HashMap<String, Vec<u8>> {
        HashMap::from([(NAME.to_owned(), firmware(sizes))])
    }

    #[test]
    fn names_the_file_by_chip_and_rom_version() {
        assert_eq!(firmware_name(0x7925, 0x0000_8a00, false), NAME);
        assert_eq!(firmware_name(0x7961, 0x0000_0001, false), "BT_RAM_CODE_MT7961_1_2_hdr.bin");
        assert_eq!(firmware_name(0x7961, 0x0000_0001, true), "BT_RAM_CODE_MT7961_1a_2_hdr.bin");
    }

    /// An MT7961 (MT7921) of each flavor gets its own file
    #[test]
    fn the_mt7961_takes_the_file_of_its_flavor() {
        let both = |_: ()| {
            HashMap::from([
                ("BT_RAM_CODE_MT7961_1_2_hdr.bin".to_owned(), firmware(&[10])),
                ("BT_RAM_CODE_MT7961_1a_2_hdr.bin".to_owned(), firmware(&[20])),
            ])
        };
        let mt7961 = |flavor| Chip { id_79xx: 0x7961, fw_version: Ok(1), flavor, ..Chip::new() };
        for (flavor, file) in [
            (Ok(0x0000_0080), "BT_RAM_CODE_MT7961_1a_2_hdr.bin"),
            (Ok(0), "BT_RAM_CODE_MT7961_1_2_hdr.bin"),
            (Err("stall".to_owned()), "BT_RAM_CODE_MT7961_1_2_hdr.bin"),
        ] {
            let r = run(&mut mt7961(flavor), both(()));
            assert_eq!(r.result, Ok(Outcome::Loaded { firmware: file.into(), detail: None }));
        }
        // The MT7925 is never asked
        assert!(run(&mut Chip::new(), files(&[10])).controls.iter().all(|c| c.index != 0x0020));
    }

    /// The MT6639 section of MediaTek's driver 26.40.3.69
    const MT7927_IDS: [(u16, u16); 5] =
        [(0x0E8D, 0x6639), (0x0E8D, 0x7927), (0x0489, 0xE110), (0x0E8D, 0x0738), (0x13D3, 0x3589)];

    #[test]
    fn every_chip_has_mediatek_usb_ids_listed() {
        use crate::chips::{ChipVendor, LOADER_CHIPS, vendor_for_usb};
        let ids: Vec<(u16, u16)> = LOADER_CHIPS
            .iter()
            .filter(|(v, _, _)| *v == ChipVendor::MediaTek)
            .map(|&(_, vid, pid)| (vid, pid))
            .collect();
        for (vid, pid) in [(0x0E8D, 0x7925), (0x0E8D, 0x7922), (0x0E8D, 0x7961), (0x0E8D, 0x7902), (0x0E8D, 0x0608)]
            .into_iter()
            .chain(MT7927_IDS)
        {
            assert!(ids.contains(&(vid, pid)), "{vid:04X}:{pid:04X}");
        }
        // Every listed ID is taken as MediaTek
        for (vid, pid) in ids {
            assert_eq!(vendor_for_usb(vid, pid, None), ChipVendor::MediaTek, "{vid:04X}:{pid:04X}");
        }
    }

    #[test]
    fn builds_wmt_commands() {
        assert_eq!(wmt_command(WMT_FUNC_CTRL, 0, &[1]), [0x6F, 0xFC, 6, 0x01, 0x06, 2, 0, 0, 1]);
    }

    #[test]
    fn downloads_every_section_then_switches_bluetooth_on() {
        let mut chip = Chip::new();
        let r = run(&mut chip, files(&[600, 250, 1]));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
        let fw = firmware(&[600, 250, 1]);
        let body = &fw[HEADER_SIZE + GLOBAL_SIZE + 3 * SECTION_MAP_SIZE..];
        assert_eq!(chip.received, [body[..600].to_vec(), body[600..850].to_vec(), body[850..].to_vec()]);
        assert!(chip.bluetooth_on);
        // Read Local Version, 3 queries, 3 + 1 + 1 chunks, Bluetooth on
        assert_eq!(r.opcodes().len(), 10);
        assert_eq!(r.commands[0].opcode(), OP_READ_LOCAL_VERSION);
        assert!(r.timers.contains(&SETTLE_MS));
        assert!(!r.bootloader);
    }

    /// The first event after a restart got lost (#86): asked again once
    #[test]
    fn asks_the_version_again_when_its_answer_is_lost() {
        let r = run(&mut Chip { firmware_runs: true, lost_versions: 1, ..Chip::new() }, HashMap::new());
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert_eq!(r.opcodes(), [OP_READ_LOCAL_VERSION, OP_READ_LOCAL_VERSION]);
        let r = run(&mut Chip { lost_versions: 1, ..Chip::new() }, files(&[10]));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
    }

    /// Read Local Version never answered (MT7921 under WinUSB): the chip ID,
    /// then the normal download
    #[test]
    fn a_chip_silent_to_the_version_is_downloaded_anyway() {
        let mut chip = Chip { lost_versions: u32::MAX, ..Chip::new() };
        let r = run(&mut chip, files(&[10]));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
        assert_eq!(r.opcodes()[..2], [OP_READ_LOCAL_VERSION, OP_READ_LOCAL_VERSION]);
        assert!(r.opcodes()[2..].iter().all(|&o| o == OP_WMT));
        assert!(chip.bluetooth_on);
    }

    #[test]
    fn the_last_chunk_has_flag_3() {
        let r = run(&mut Chip::new(), files(&[600]));
        let flags: Vec<u8> = r.commands.iter().filter(|c| c.opcode() == OP_WMT).map(|c| c.params()[4]).collect();
        assert_eq!(flags, [FLAG_QUERY, FLAG_FIRST, FLAG_MIDDLE, FLAG_LAST, 0]);
    }

    #[test]
    fn nothing_to_do_when_bluetooth_runs() {
        let r = run(&mut Chip { firmware_runs: true, ..Chip::new() }, HashMap::new());
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert!(r.controls.is_empty());
    }

    #[test]
    fn sections_the_chip_has_are_skipped() {
        let mut chip = Chip { has: vec![0, 1], ..Chip::new() };
        let r = run(&mut chip, files(&[600, 300]));
        assert_eq!(
            r.result,
            Ok(Outcome::Loaded { firmware: NAME.into(), detail: Some("the chip had the firmware already") })
        );
        assert!(chip.received.is_empty());
        assert!(chip.bluetooth_on, "Bluetooth is switched on all the same");
    }

    #[test]
    fn a_section_in_progress_is_asked_again() {
        let mut chip = Chip { busy: 3, ..Chip::new() };
        let r = run(&mut chip, files(&[10]));
        assert!(r.result.is_ok(), "{:?}", r.result);
        let queries = r
            .commands
            .iter()
            .filter(|c| c.opcode() == OP_WMT && c.params()[1] == WMT_PATCH_DOWNLOAD && c.params()[4] == FLAG_QUERY)
            .count();
        assert_eq!(queries, 4, "asked again after each of the 3 answers");
    }

    #[test]
    fn takes_the_older_chip_id_register_when_it_answers() {
        let r = run(&mut Chip { chip_id: 0x7925, id_79xx: 0, ..Chip::new() }, files(&[10]));
        assert!(r.result.is_ok(), "{:?}", r.result);
        assert_eq!(r.controls.iter().filter(|c| c.request == REQUEST_READ_REGISTER).count(), 2);
    }

    #[test]
    fn an_unknown_chip_is_left_alone() {
        let r = run(&mut Chip { id_79xx: 0x7668, ..Chip::new() }, files(&[10]));
        assert!(matches!(r.result, Err(LoadError::Unsupported(_))), "{:?}", r.result);
        assert!(r.opcodes().iter().all(|&o| o != OP_WMT), "nothing sent to it");
    }

    #[test]
    fn falls_back_to_the_first_file_when_the_rom_names_another() {
        let r = run(&mut Chip { fw_version: Ok(0x0000_0001), ..Chip::new() }, files(&[10]));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
        let r = run(&mut Chip { fw_version: Err("stall".into()), ..Chip::new() }, files(&[10]));
        assert!(r.result.is_ok(), "{:?}", r.result);
    }

    #[test]
    fn names_the_missing_file() {
        let r = run(&mut Chip::new(), HashMap::new());
        assert_eq!(r.result, Err(LoadError::MissingFile(NAME.into())));
    }

    #[test]
    fn checks_the_file_before_sending_any_of_it() {
        let mut bad = firmware(&[100]);
        bad.truncate(bad.len() - 1);
        let r = run(&mut Chip::new(), HashMap::from([(NAME.to_owned(), bad)]));
        assert!(matches!(r.result, Err(LoadError::BadFile { .. })), "{:?}", r.result);
        assert!(r.opcodes().iter().all(|&o| o != OP_WMT));
        let r = run(&mut Chip::new(), HashMap::from([(NAME.to_owned(), b"<!doctype html>".to_vec())]));
        assert!(matches!(r.result, Err(LoadError::BadFile { .. })), "{:?}", r.result);
    }

    #[test]
    fn a_silent_chip_times_out() {
        let r = run(&mut Chip { silent: true, ..Chip::new() }, files(&[10]));
        assert_eq!(r.result, Err(LoadError::Timeout("section query")));
    }

    #[test]
    fn over_h4_the_first_transfer_ends_it() {
        struct H4;
        impl SimController for H4 {
            fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                vec![command_complete(sim::opcode(c), 0x0C, &[])]
            }
        }
        let mut loader = Loader::new(Box::new(|_| None));
        let r = sim::run(&mut loader, &mut H4);
        assert!(matches!(r.result, Err(LoadError::Host(_))), "{:?}", r.result);
    }

    /// A file with these maps (offset, size, download size, type) and
    /// section data that tells its offset
    fn file_with_maps(maps: &[(u32, u32, u32, u32)]) -> Vec<u8> {
        let len = maps.iter().map(|&(offset, size, _, _)| (offset + size) as usize).max().unwrap();
        let mut f: Vec<u8> = (0..len).map(|n| (n * 7 + n / 251) as u8).collect();
        f[..HEADER_SIZE + GLOBAL_SIZE + maps.len() * SECTION_MAP_SIZE].fill(0);
        f[HEADER_SIZE + 12] = maps.len() as u8;
        for (i, (offset, size, dl, kind)) in maps.iter().enumerate() {
            let at = HEADER_SIZE + GLOBAL_SIZE + i * SECTION_MAP_SIZE;
            f[at + 4..at + 8].copy_from_slice(&offset.to_le_bytes());
            f[at + 8..at + 12].copy_from_slice(&size.to_le_bytes());
            f[at + 16..at + 20].copy_from_slice(&dl.to_le_bytes());
            f[at + 28..at + 32].copy_from_slice(&kind.to_le_bytes());
        }
        f
    }

    /// The maps of BT_RAM_CODE_MT7925_1_1_hdr.bin in driver 26.40.3.69
    const MT7925_MAPS: [(u32, u32, u32, u32); 5] = [
        (0x1A0, 0x12DC0, 0x12DC0, 2),
        (0x12F60, 0x54840, 0x54840, 0x80),
        (0x677A0, 0xA240, 0xA240, 0x1_0000),
        (0x719E0, 0, 0, 0x2_0000),
        (0x719E0, 0x3A, 0, 0),
    ];

    /// The maps of BT_RAM_CODE_MT6639_2_1_hdr.bin in driver 26.40.3.69
    const MT6639_MAPS: [(u32, u32, u32, u32); 9] = [
        (0x2A0, 0xF1C0, 0xF1C0, 3),
        (0xF460, 0xD900, 0xD900, 2),
        (0x1CD60, 0x201C0, 0x201C0, 0x90),
        (0x3CF20, 0x2B680, 0x2B680, 0x80),
        (0x685A0, 0xEF80, 0xEF80, 0x1_0000),
        (0x77520, 0xD700, 0xD700, 0x2_0000),
        (0x84C20, 0x7280, 0x7280, 0x2_0000),
        (0x8BEA0, 0, 0, 0x2_0000),
        (0x8BEA0, 0x35, 0, 0),
    ];

    #[test]
    fn reads_the_section_layout_of_the_real_file() {
        let f = file_with_maps(&MT7925_MAPS);
        let sizes = |chip| -> Vec<usize> { parse_firmware(&f, chip).unwrap().iter().map(|s| s.data.len()).collect() };
        assert_eq!(sizes(0x7925), [0x12DC0, 0x54840, 0xA240], "the two without a download size are skipped");
        assert_eq!(sizes(0x7922), sizes(0x7925), "file order gives the same");
        assert!(parse_firmware(&f, 0x7925).unwrap().iter().all(|s| s.spec.len() == 52));
    }

    /// The MT6639's sections go in the driver's order of types, and those
    /// of type 0x20000 stay out
    #[test]
    fn the_mt6639_gets_its_sections_by_type() {
        let f = file_with_maps(&MT6639_MAPS);
        let sections = parse_firmware(&f, CHIP_MT6639).unwrap();
        let expected: Vec<&[u8]> = [1, 0, 3, 2, 4]
            .iter()
            .map(|&i| {
                let (offset, size, _, _) = MT6639_MAPS[i];
                &f[offset as usize..(offset + size) as usize]
            })
            .collect();
        let data: Vec<&[u8]> = sections.iter().map(|s| s.data.as_slice()).collect();
        assert_eq!(data, expected);
    }

    #[test]
    fn loads_an_mt6639_with_its_file_and_setup_command() {
        let name = "BT_RAM_CODE_MT6639_2_1_hdr.bin";
        assert_eq!(firmware_name(CHIP_MT6639, 0, false), name);
        assert_eq!(firmware_name(CHIP_MT6639, 0x0000_8a00, false), name);
        let file = file_with_maps(&MT6639_MAPS);
        let mut chip = Chip { id_79xx: CHIP_MT6639 as u32, fw_version: Ok(0x0000_8a00), ..Chip::new() };
        let r = run(&mut chip, HashMap::from([(name.to_owned(), file.clone())]));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: name.into(), detail: None }));
        let sent: Vec<Vec<u8>> = parse_firmware(&file, CHIP_MT6639).unwrap().into_iter().map(|s| s.data).collect();
        assert_eq!(chip.received, sent);
        assert!(chip.setup && chip.bluetooth_on);
        // The setup command as MediaTek's driver sends it, right before
        // Bluetooth on
        let wmt: Vec<&[u8]> = r.commands.iter().filter(|c| c.opcode() == OP_WMT).map(|c| c.params()).collect();
        let n = wmt.len();
        assert_eq!(wmt[n - 2], [0x01, 0x55, 0x07, 0x00, 0x02, 0x02, 0x01, 0x03, 0x03, 0x03, 0x01]);
        assert_eq!(wmt[n - 1], [0x01, 0x06, 0x02, 0x00, 0x00, 0x01]);
        // Another chip never gets it
        let mut mt7925 = Chip::new();
        run(&mut mt7925, files(&[10]));
        assert!(!mt7925.setup && mt7925.bluetooth_on);
    }

    #[test]
    fn finds_a_file_in_the_drivers_container() {
        let mut c = b"MTK-".to_vec();
        c.extend_from_slice(&2u16.to_le_bytes());
        c.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let names = ["BT_RAM_CODE_MT6639_2_1_hdr.bin", NAME];
        let data_at = 0x10 + 2 * 0x4C;
        for (i, n) in names.iter().enumerate() {
            let mut e = vec![0u8; 0x4C];
            e[..n.len()].copy_from_slice(n.as_bytes());
            e[0x30..0x3E].copy_from_slice(b"20260821201251");
            e[0x40..0x44].copy_from_slice(&((data_at + i * 3) as u32).to_le_bytes());
            e[0x44..0x48].copy_from_slice(&3u32.to_le_bytes());
            c.extend_from_slice(&e);
        }
        c.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(driver_container_file(&c, NAME), Some(vec![4, 5, 6]));
        assert_eq!(driver_container_file(&c, "bt_ram_code_mt7925_1_1_hdr.bin"), Some(vec![4, 5, 6]));
        assert_eq!(driver_container_file(&c, "BT_RAM_CODE_MT7922_1_1_hdr.bin"), None);
        assert_eq!(driver_container_file(b"MZ\x90\x00", NAME), None);
        c.truncate(c.len() - 1);
        assert_eq!(driver_container_file(&c, NAME), None, "cut short");
    }

    /// Against MediaTek's real driver: `MTKBT_DAT=<path to mtkbt_v2.dat>
    /// cargo test -p bt-host --lib real_driver -- --ignored`
    #[test]
    #[ignore = "needs MediaTek's mtkbt_v2.dat (MTKBT_DAT)"]
    fn real_driver_container_has_loadable_files() {
        let path = std::env::var("MTKBT_DAT").expect("MTKBT_DAT");
        let container = std::fs::read(path).unwrap();
        // Chip, ROM version, flavor of each file the driver may have
        let cases = [
            (0x7925u16, 0u32, 0u32),
            (0x7922, 0, 0),
            (0x7902, 0, 0),
            (0x7961, 1, 0),
            (0x7961, 1, FLAVOR_1A),
            (CHIP_MT6639, 0, 0),
        ];
        let mut loaded = 0;
        for (chip_id, version, flavor) in cases {
            let name = firmware_name(chip_id, version, flavor != 0);
            let Some(file) = driver_container_file(&container, &name) else { continue };
            let sections = parse_firmware(&file, chip_id).unwrap();
            let mut chip = Chip { id_79xx: chip_id as u32, fw_version: Ok(version), flavor: Ok(flavor), ..Chip::new() };
            let r = run(&mut chip, HashMap::from([(name.clone(), file)]));
            assert_eq!(r.result, Ok(Outcome::Loaded { firmware: name.clone(), detail: None }), "{name}");
            let sent: Vec<Vec<u8>> = sections.into_iter().map(|s| s.data).collect();
            assert_eq!(chip.received, sent, "{name}");
            eprintln!("{name}: {} sections loaded", chip.received.len());
            loaded += 1;
        }
        assert!(loaded > 0, "no MT79xx file in the container");
    }

    /// What an MT79xx run sends, as one fingerprint: commands (pipe and
    /// bytes) and control transfers, in order
    fn sequence(r: &sim::SimRun) -> (usize, usize, u64) {
        let mut text = String::new();
        for c in &r.commands {
            text += &format!("{} {:02x?}\n", c.bulk, c.bytes);
        }
        for c in &r.controls {
            text += &format!("{c:?}\n");
        }
        let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
        (r.commands.len(), r.controls.len(), hash)
    }

    /// The MT79xx sequences are those of the loader before the MT7663 came
    /// (fingerprints taken at commit 7f5ad7d5 with this same function)
    #[test]
    fn the_mt79xx_sequence_is_unchanged() {
        let a = run(&mut Chip::new(), files(&[600, 250, 1]));
        let b = run(&mut Chip { chip_id: 0x7925, id_79xx: 0, ..Chip::new() }, files(&[10]));
        let both = HashMap::from([
            ("BT_RAM_CODE_MT7961_1_2_hdr.bin".to_owned(), firmware(&[10])),
            ("BT_RAM_CODE_MT7961_1a_2_hdr.bin".to_owned(), firmware(&[20])),
        ]);
        let c = run(&mut Chip { id_79xx: 0x7961, fw_version: Ok(1), flavor: Ok(0x80), ..Chip::new() }, both);
        let d = run(&mut Chip { has: vec![0], busy: 2, lost_versions: 1, ..Chip::new() }, files(&[600, 300]));
        assert_eq!(sequence(&a), (10, 30, 1196921624536549366));
        assert_eq!(sequence(&b), (4, 11, 2799098120537372090));
        assert_eq!(sequence(&c), (4, 13, 14035482706166086166));
        assert_eq!(sequence(&d), (9, 24, 14989985001770669069));
    }

    /// Read Local Version refused (either event, any error status) or never
    /// answered: the chip ID is read and the download goes on
    #[test]
    fn any_version_failure_goes_on_to_the_chip_id() {
        struct Refusing(Chip, Vec<u8>);
        impl SimController for Refusing {
            fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                if sim::opcode(c) == OP_READ_LOCAL_VERSION {
                    return vec![self.1.clone()];
                }
                self.0.command(c)
            }
            fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
                self.0.control(r)
            }
        }
        for answer in [
            command_complete(OP_READ_LOCAL_VERSION, 0x01, &[]),
            command_complete(OP_READ_LOCAL_VERSION, 0x0C, &[]),
            sim::command_status(OP_READ_LOCAL_VERSION, 0x0C),
        ] {
            let mut chip = Refusing(Chip::new(), answer);
            let files = files(&[10]);
            let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
            let r = sim::run(&mut loader, &mut chip);
            assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }));
            assert!(chip.0.bluetooth_on);
        }
        let r = run(&mut Chip { lost_versions: u32::MAX, ..Chip::new() }, files(&[10]));
        assert!(r.result.is_ok(), "{:?}", r.result);
    }

    /// The first chip ID register failing or of another size: the second
    /// is read; neither naming a known chip: refused before any WMT command
    #[test]
    fn an_unreadable_first_chip_id_register_is_not_the_end() {
        struct Stalling(Chip, Result<Vec<u8>, String>);
        impl SimController for Stalling {
            fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                self.0.command(c)
            }
            fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
                if (r.request, r.value, r.index) == (REQUEST_READ_REGISTER, 0x7001, 0x0200) {
                    return self.1.clone();
                }
                self.0.control(r)
            }
        }
        for first in [Err("stall".to_owned()), Ok(Vec::new()), Ok(vec![1, 2])] {
            let files = files(&[10]);
            let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
            let mut chip = Stalling(Chip::new(), first.clone());
            let r = sim::run(&mut loader, &mut chip);
            assert_eq!(r.result, Ok(Outcome::Loaded { firmware: NAME.into(), detail: None }), "{first:?}");
            let mut loader = Loader::new(Box::new(|_| None));
            let mut chip = Stalling(Chip { id_79xx: 0x1234, ..Chip::new() }, first.clone());
            let r = sim::run(&mut loader, &mut chip);
            assert!(matches!(r.result, Err(LoadError::Unsupported(_))), "{:?}", r.result);
            assert!(r.opcodes().iter().all(|&o| o != OP_WMT), "nothing sent to it");
        }
    }

    #[test]
    fn tells_its_step_and_progress() {
        let files = files(&[600]);
        let mut loader = Loader::new(Box::new(move |name| files.get(name).cloned()));
        loader.start();
        assert_eq!(loader.step(), "version");
        loader.on_event(&command_complete(OP_READ_LOCAL_VERSION, 0x0C, &[]));
        assert_eq!(loader.step(), "chip ID");
        loader.on_control(Ok(0u32.to_le_bytes().to_vec()));
        loader.on_control(Ok(0x7925u32.to_le_bytes().to_vec()));
        loader.on_control(Ok(0u32.to_le_bytes().to_vec()));
        assert_eq!((loader.step(), loader.progress()), ("download", Some((0, 3))));
    }
}
