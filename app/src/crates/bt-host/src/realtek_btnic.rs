//! Realtek chips whose firmware only Realtek's Windows driver has: the
//! RTL8852BD (0BDA:B853, issue #374), the RTL8922A and the RTL8852BT, all
//! untested on hardware
//!
//! BTstack's Realtek download knows none of them: the RTL8852BD's ROM
//! (ROM version 3) is not in rtl8852bu_fw.bin, and BTstack's table has no
//! USB ID of the other two. Realtek's Windows driver (RtkBtFilter2, 18.4021
//! to 246.4050) carries their firmware in two formats of its own and
//! downloads it in a sequence of its own. Everything below comes from that
//! driver and its files, and BTstack's Realtek code; no other driver's
//! source was used.
//!
//! Which chip a USB ID is: the driver's table of hardware IDs, each with
//! the chip's firmware file (see [`CHIPS`]). The driver also holds the
//! identity of each chip's ROM: its LMP subversion and HCI revision.
//!
//! `rtl8852bd_mp_chip_new.dat`, a container that starts with "BTNIC003":
//! - "BTNIC003", an 8-byte version, a u32 (0), the section count (u32 at
//!   0x14); the sections follow from 0x18
//! - a section: opcode (u32: 1 = patch, 2 = signed patch), length (u32), a
//!   u32 (0), then `length` bytes: a 0x408-byte description and the payload
//! - the description: project ID (u16 at 0, 0x14 for the RTL8852B family),
//!   the ROM version + 1 (byte 2), the key ID (byte 3), 0 (byte 4); two
//!   register writes (address and value, u32 each, at 0x3DF / 0x3E7 and
//!   0x3EF / 0x3F7; address 0 or 0xFFFFFFFF: none); the order of the
//!   section (byte 0x3FF); the payload length (u32 at 0x400)
//!
//! `rtl8922a_mp_chip_new.dat` and `rtl8852bt_mp_chip_new.dat`, patches that
//! start with "RTBTCore" (the format BTstack's Realtek code reads too):
//! - "RTBTCore", an 8-byte version, the section count (u32 at 0x10); the
//!   sections follow from 0x14: opcode (u32), length (u32), then `length`
//!   bytes
//! - the driver reads sections 1 to 3 only: a part count (u16) and 2 bytes,
//!   then the parts: the ROM version + 1 (byte 0), priority (byte 1), key
//!   ID (byte 2), a byte, the payload length (u32), the payload
//! - the driver takes, of section 1, the parts of the chip's ROM version (and
//!   key ID, when it read one); of section 2, those of the ROM version; of
//!   section 3, those of the ROM version and key ID when the key ID is not
//!   0. It moves lower priorities forward ([`core_select`]), takes at most
//!   32, and joins their payloads
//! - the file ends with records read backwards from before its last 4 bytes
//!   (0x51 0x04 0xFD 0x77), one of them the project ID (BTstack's format):
//!   checked here, so another chip's file is refused
//!
//! The sequence, after Read Local Version found the ROM code:
//! 1. vendor command 0xFC66 (no parameters), then 200 ms
//! 2. 0xFC6D: the ROM version (first byte after the status)
//! 3. 0xFC61 {0x21, 0xB000ADA4}: the key ID (first byte after the status);
//!    of these chips, the driver reads it for the RTL8922A only. This
//!    loader also reads it for the RTL8852BD, as it did before
//! 4. BTNIC003: the sections of this project, ROM version and key ID, by
//!    their order; for each: its register write as 0xFC62 {0x21, address,
//!    value}, then the payload in 0xFC20 commands of an index byte and up to
//!    252 bytes. The index starts at 1 for each section, goes from 0x7F
//!    back to 1, and the controller answers each with the index it got;
//!    0xFC20 {0x80} ends the download, answered with 0x80
//! 5. RTBTCore: the joined payloads in 0xFC20 commands of an index byte and
//!    up to 252 bytes; the index starts at 0, goes from 0x7F back to 1, and
//!    the last command's has 0x80 set; the controller answers each with the
//!    index it got
//!
//! The description itself is never sent, nor any config file. What the
//! driver does that this file does not: signed BTNIC003 sections (opcode 2)
//! and their second register write (0x3EF), which the RTL8852BD's file has
//! none of (such a file is refused before anything is sent); for the
//! RTL8922A, a configuration file (`rtl8922a_cfg.dat`, in no driver package
//! seen) appended to the image and a recovery command (0xFCF1) under
//! conditions this loader cannot see.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::bringup::{Action, ChipLoader, FileReader, HciEvent, LoadError, Outcome, hci_command, parse_event};
use crate::driver_files::{self, file_name};

/// How a chip's firmware file is read and sent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A BTNIC003 container: the sections of this project ID
    Btnic { project: u16 },
    /// An RTBTCore patch of this project ID; `read_key`: the driver reads
    /// the chip's key ID and picks the parts of section 1 by it too
    Core { project: u8, read_key: bool },
}

/// A chip this loader serves
#[derive(Debug, PartialEq, Eq)]
pub struct DriverChip {
    pub name: &'static str,
    /// The file of Realtek's Windows driver
    pub file: &'static str,
    /// The LMP subversion while the ROM code runs
    pub rom_subversion: u16,
    /// The HCI revision of its ROM, when checked: the chips of one ROM
    /// subversion differ by it
    pub rom_revision: Option<u16>,
    pub format: Format,
}

const RTL8852BD: DriverChip = DriverChip {
    name: "RTL8852BD",
    file: "rtl8852bd_mp_chip_new.dat",
    rom_subversion: 0x8852,
    rom_revision: None,
    format: Format::Btnic { project: 0x14 },
};
const RTL8922A: DriverChip = DriverChip {
    name: "RTL8922A",
    file: "rtl8922a_mp_chip_new.dat",
    rom_subversion: 0x8922,
    rom_revision: Some(0x000A),
    format: Format::Core { project: 44, read_key: true },
};
const RTL8852BT: DriverChip = DriverChip {
    name: "RTL8852BT",
    file: "rtl8852bt_mp_chip_new.dat",
    rom_subversion: 0x8852,
    rom_revision: Some(0x0087),
    format: Format::Core { project: 47, read_key: false },
};

/// The chips this loader serves: USB VID, PID, chip. The IDs of Realtek's
/// Windows driver (RtkBtFilter2 18.4021 to 246.4050) for these chips; none
/// is in BTstack's Realtek table or chips::OEM_TABLE.
pub const CHIPS: &[(u16, u16, &DriverChip)] = &[
    (0x0BDA, 0xB853, &RTL8852BD),
    (0x0489, 0xE130, &RTL8922A),
    (0x0BDA, 0x8922, &RTL8922A),
    (0x0BDA, 0xA100, &RTL8922A),
    (0x0BDA, 0xA890, &RTL8922A),
    (0x0BDA, 0xA891, &RTL8922A),
    (0x0BDA, 0xA892, &RTL8922A),
    (0x0BDA, 0xB85F, &RTL8922A),
    (0x0BDA, 0xB892, &RTL8922A),
    (0x0BDA, 0xD922, &RTL8922A),
    (0x0BDA, 0xD923, &RTL8922A),
    (0x13D3, 0x3616, &RTL8922A),
    (0x13D3, 0x3617, &RTL8922A),
    (0x2001, 0x3331, &RTL8922A),
    (0x0489, 0xE12F, &RTL8852BT),
    (0x0BDA, 0x8520, &RTL8852BT),
    (0x0BDA, 0xB522, &RTL8852BT),
    (0x0BDA, 0xB524, &RTL8852BT),
    (0x0BDA, 0xB526, &RTL8852BT),
    (0x0BDA, 0xB86A, &RTL8852BT),
    (0x13D3, 0x3618, &RTL8852BT),
    (0x13D3, 0x3619, &RTL8852BT),
];

/// The chip of a USB VID:PID, if this loader serves it: its name and file
pub fn chip(vid: u16, pid: u16) -> Option<(&'static str, &'static str)> {
    CHIPS.iter().find(|c| (c.0, c.1) == (vid, pid)).map(|c| (c.2.name, c.2.file))
}

fn driver_chip(name: &str) -> Option<&'static DriverChip> {
    CHIPS.iter().map(|c| c.2).find(|c| c.name == name)
}

const OP_READ_LOCAL_VERSION: u16 = 0x1001;
const OP_PREPARE: u16 = 0xFC66;
const OP_READ_ROM_VERSION: u16 = 0xFC6D;
const OP_READ_REGISTER: u16 = 0xFC61;
const OP_WRITE_REGISTER: u16 = 0xFC62;
const OP_DOWNLOAD: u16 = 0xFC20;

/// The register 0xFC61 reads the key ID from, and the access type the
/// driver gives with it and with 0xFC62
const KEY_ID_REGISTER: u32 = 0xB000_ADA4;
const REGISTER_ACCESS: u8 = 0x21;
/// The index of the end-of-download command (BTNIC003), the flag of the
/// last command's index (RTBTCore)
const LAST_INDEX: u8 = 0x80;
const CHUNK: usize = 252;

const MAGIC: &[u8; 8] = b"BTNIC003";
const SECTIONS_AT: usize = 0x18;
const SECTION_HEADER: usize = 12;
const DESCRIPTION: usize = 0x408;

const CORE_MAGIC: &[u8; 8] = b"RTBTCore";
const CORE_SECTIONS_AT: usize = 0x14;
/// The parts the driver takes at most
const CORE_MAX_PARTS: usize = 32;

pub const COMMAND_TIMEOUT_MS: u32 = 2000;
/// After 0xFC66, as the driver waits
pub const PREPARE_WAIT_MS: u32 = 200;
/// The answer to the download's last command
pub const LAST_TIMEOUT_MS: u32 = 1500;

/// A section of a BTNIC003 file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub opcode: u32,
    pub project: u16,
    /// The ROM version + 1
    pub eco: u8,
    pub key_id: u8,
    pub reserved: u8,
    pub order: u8,
    /// The register write before the payload (address, value)
    pub write: Option<(u32, u32)>,
    /// The second one (not supported)
    pub write2: Option<(u32, u32)>,
    /// Where the payload is in the file
    pub payload: std::ops::Range<usize>,
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn write_at(desc: &[u8], at: usize) -> Option<(u32, u32)> {
    let address = u32_at(desc, at)?;
    let value = u32_at(desc, at + 8)?;
    (address != 0 && address != u32::MAX).then_some((address, value))
}

/// The sections of a BTNIC003 file; an error if it is none or cut short
pub fn sections(data: &[u8]) -> Result<Vec<Section>, &'static str> {
    if !data.starts_with(MAGIC) {
        return Err("not a BTNIC003 file");
    }
    let count = u32_at(data, 0x14).ok_or("cut short")? as usize;
    let mut at = SECTIONS_AT;
    let mut found = Vec::new();
    for _ in 0..count {
        let opcode = u32_at(data, at).ok_or("cut short")?;
        let len = u32_at(data, at + 4).ok_or("cut short")? as usize;
        let body = at + SECTION_HEADER;
        let end = body.checked_add(len).filter(|&e| e <= data.len()).ok_or("cut short")?;
        if len < DESCRIPTION {
            return Err("a section is too short");
        }
        let desc = &data[body..body + DESCRIPTION];
        let payload_len = u32_at(desc, 0x400).ok_or("cut short")? as usize;
        if payload_len > len - DESCRIPTION {
            return Err("a section's payload is longer than the section");
        }
        found.push(Section {
            opcode,
            project: u16::from_le_bytes([desc[0], desc[1]]),
            eco: desc[2],
            key_id: desc[3],
            reserved: desc[4],
            order: desc[0x3FF],
            write: write_at(desc, 0x3DF),
            write2: write_at(desc, 0x3EF),
            payload: body + DESCRIPTION..body + DESCRIPTION + payload_len,
        });
        at = end;
    }
    Ok(found)
}

/// The sections for a chip of this project with this ROM version and key
/// ID, in download order; Err if there are none or they need what this
/// loader cannot do
pub fn select(sections: &[Section], project: u16, rom_version: u8, key_id: u8) -> Result<Vec<&Section>, &'static str> {
    let mut chosen: Vec<&Section> = sections
        .iter()
        .filter(|s| {
            s.project == project
                && s.eco == rom_version.wrapping_add(1)
                && s.key_id == key_id
                && s.reserved == 0
                && matches!(s.opcode, 1 | 2)
        })
        .collect();
    if chosen.is_empty() {
        return Err("no patch for this chip's ROM version and key ID");
    }
    if chosen.iter().any(|s| s.opcode != 1) {
        return Err("signed sections, which BlueCaster cannot send yet");
    }
    if chosen.iter().any(|s| s.write2.is_some()) {
        return Err("a second register write, which BlueCaster cannot send yet");
    }
    chosen.sort_by_key(|s| s.order);
    Ok(chosen)
}

/// A part of an RTBTCore patch
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The section's opcode (1 to 3)
    pub section: u32,
    /// The ROM version + 1
    pub eco: u8,
    pub priority: u8,
    pub key_id: u8,
    /// Where the payload is in the file
    pub payload: std::ops::Range<usize>,
}

/// The parts of an RTBTCore patch's sections 1 to 3 (the others are
/// skipped, as the driver does); an error if it is cut short
pub fn core_parts(data: &[u8]) -> Result<Vec<Part>, &'static str> {
    if !data.starts_with(CORE_MAGIC) {
        return Err("not an RTBTCore file");
    }
    let count = u32_at(data, 0x10).ok_or("cut short")? as usize;
    let mut at = CORE_SECTIONS_AT;
    let mut parts = Vec::new();
    for _ in 0..count {
        let opcode = u32_at(data, at).ok_or("cut short")?;
        let len = u32_at(data, at + 4).ok_or("cut short")? as usize;
        let body = at + 8;
        let end = body.checked_add(len).filter(|&e| e <= data.len()).ok_or("cut short")?;
        if (1..=3).contains(&opcode) {
            let n = data.get(body..body + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or("cut short")?;
            let mut p = body + 4;
            for _ in 0..n {
                let head = data.get(p..p + 8).filter(|_| p + 8 <= end).ok_or("a part is cut short")?;
                let plen = u32_at(head, 4).ok_or("cut short")? as usize;
                let payload = p + 8..p.checked_add(8 + plen).filter(|&e| e <= end).ok_or("a part is cut short")?;
                parts.push(Part { section: opcode, eco: head[0], priority: head[1], key_id: head[2], payload });
                p += 8 + plen;
            }
        }
        at = end;
    }
    Ok(parts)
}

/// The parts the driver sends to a chip with this ROM version and key ID
/// (`key_read`: it read the key ID), in its order; Err if there are none or
/// more than it takes
pub fn core_select(parts: &[Part], rom_version: u8, key_id: u8, key_read: bool) -> Result<Vec<&Part>, &'static str> {
    let eco = rom_version.wrapping_add(1);
    let mut chosen: Vec<&Part> = parts
        .iter()
        .filter(|p| {
            p.eco == eco
                && match p.section {
                    1 => !key_read || p.key_id == key_id,
                    2 => true,
                    _ => key_id != 0 && p.key_id == key_id,
                }
        })
        .collect();
    if chosen.is_empty() {
        return Err("no patch for this chip's ROM version and key ID");
    }
    if chosen.len() > CORE_MAX_PARTS {
        return Err("more parts than the controller takes");
    }
    // The driver's order: for each place, the last later part of a lower
    // priority than the one there swaps with it (not a full sort)
    for i in 0..chosen.len().saturating_sub(1) {
        if let Some(j) = (i + 1..chosen.len()).rev().find(|&j| chosen[i].priority > chosen[j].priority) {
            chosen.swap(i, j);
        }
    }
    Ok(chosen)
}

/// What one download command expects back
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expect {
    /// A successful Command Complete of 0xFC62
    Written,
    /// The index echoed by 0xFC20
    Index(u8),
}

/// A download's commands, each with its answer, and what it holds (for
/// the log)
type Download = (Vec<(Vec<u8>, Expect)>, String);

/// The commands of a BTNIC003 download, built from the file before any is
/// sent
fn download_commands(data: &[u8], chosen: &[&Section]) -> Vec<(Vec<u8>, Expect)> {
    let mut commands = Vec::new();
    for s in chosen {
        if let Some((address, value)) = s.write {
            let mut p = vec![REGISTER_ACCESS];
            p.extend_from_slice(&address.to_le_bytes());
            p.extend_from_slice(&value.to_le_bytes());
            commands.push((hci_command(OP_WRITE_REGISTER, &p), Expect::Written));
        }
        let mut index = 1u8;
        for part in data[s.payload.clone()].chunks(CHUNK) {
            let mut p = vec![index];
            p.extend_from_slice(part);
            commands.push((hci_command(OP_DOWNLOAD, &p), Expect::Index(index)));
            index = if index >= 0x7F { 1 } else { index + 1 };
        }
    }
    commands.push((hci_command(OP_DOWNLOAD, &[LAST_INDEX]), Expect::Index(LAST_INDEX)));
    commands
}

/// The commands of an RTBTCore download: the joined payloads, the index
/// from 0, the last one's with 0x80 set
fn core_commands(data: &[u8], chosen: &[&Part]) -> Vec<(Vec<u8>, Expect)> {
    let image: Vec<u8> = chosen.iter().flat_map(|p| data[p.payload.clone()].iter().copied()).collect();
    let chunks: Vec<&[u8]> = image.chunks(CHUNK).collect();
    let mut commands = Vec::new();
    let mut index = 0u8;
    for (i, part) in chunks.iter().enumerate() {
        let sent = if i + 1 == chunks.len() { index | LAST_INDEX } else { index };
        let mut p = vec![sent];
        p.extend_from_slice(part);
        commands.push((hci_command(OP_DOWNLOAD, &p), Expect::Index(sent)));
        index = if index >= 0x7F { 1 } else { index + 1 };
    }
    commands
}

/// The project ID of a patch with BTstack's extension records (at its end,
/// read backwards from before the last 4 bytes: {data, length, opcode}
/// until opcode 0, the project ID, or the 0xFF that ends them). None
/// without them.
fn patch_project(data: &[u8]) -> Option<u8> {
    if !data.ends_with(&EPATCH_EXTENSION) {
        return None;
    }
    let mut i = data.len().checked_sub(5)?;
    loop {
        let opcode = data[i];
        let len = *data.get(i.checked_sub(1)?)?;
        if opcode == 0xFF {
            return None;
        }
        if opcode == 0x00 && len == 1 {
            return data.get(i.checked_sub(2)?).copied();
        }
        i = i.checked_sub(len as usize + 2)?;
    }
}

enum State {
    Idle,
    Version,
    Prepare,
    PrepareWait,
    RomVersion,
    KeyId { rom_version: u8 },
    Downloading { file: String, commands: Vec<(Vec<u8>, Expect)>, next: usize },
    Done,
}

pub struct Loader {
    read_file: FileReader,
    chip: &'static str,
    file: &'static str,
    state: State,
    started: Instant,
}

impl Loader {
    /// The loader of a chip in [`CHIPS`] (its name and file)
    pub fn new(read_file: FileReader, chip: &'static str, file: &'static str) -> Loader {
        Loader { read_file, chip, file, state: State::Idle, started: Instant::now() }
    }

    fn finish(&mut self, result: Result<Outcome, LoadError>) -> Vec<Action> {
        tracing::debug!(
            "realtek {}: loader finished after {} ms: {result:?}",
            self.chip,
            self.started.elapsed().as_millis()
        );
        self.state = State::Done;
        vec![Action::Done(result)]
    }

    fn send(&mut self, state: State, opcode: u16, params: &[u8]) -> Vec<Action> {
        self.state = state;
        vec![Action::Command(hci_command(opcode, params)), Action::Timer(COMMAND_TIMEOUT_MS)]
    }

    fn known(&self) -> Option<&'static DriverChip> {
        driver_chip(self.chip).filter(|c| c.file == self.file)
    }

    fn on_version(&mut self, ret: &[u8]) -> Vec<Action> {
        let Some(chip) = self.known() else { return self.finish(Err(LoadError::Unsupported("unknown Realtek chip"))) };
        let Some(subversion) = ret.get(6..8).map(|v| u16::from_le_bytes([v[0], v[1]])) else {
            return self.finish(Err(LoadError::BadVersion));
        };
        let revision = ret.get(1..3).map_or(0, |v| u16::from_le_bytes([v[0], v[1]]));
        tracing::info!(
            "realtek {}: HCI version {}, revision 0x{revision:04X}, LMP subversion 0x{subversion:04X}",
            self.chip,
            ret[0]
        );
        if subversion != chip.rom_subversion {
            return self.finish(Ok(Outcome::AlreadyRunning));
        }
        if chip.rom_revision.is_some_and(|r| r != revision) {
            // Another chip of the same ROM subversion: its firmware is not
            // this file
            return self.finish(Err(LoadError::UnsupportedVariant(revision as u8)));
        }
        self.send(State::Prepare, OP_PREPARE, &[])
    }

    fn after_prepare(&mut self) -> Vec<Action> {
        self.state = State::PrepareWait;
        vec![Action::Timer(PREPARE_WAIT_MS)]
    }

    fn on_rom_version(&mut self, rom_version: u8) -> Vec<Action> {
        let read_key = match self.known().map(|c| c.format) {
            Some(Format::Core { read_key, .. }) => read_key,
            _ => true,
        };
        if !read_key {
            return self.on_key_id(rom_version, 0, false);
        }
        self.send(
            State::KeyId { rom_version },
            OP_READ_REGISTER,
            &[&[REGISTER_ACCESS][..], &KEY_ID_REGISTER.to_le_bytes()].concat(),
        )
    }

    fn on_key_id(&mut self, rom_version: u8, key_id: u8, key_read: bool) -> Vec<Action> {
        let Some(chip) = self.known() else { return self.finish(Err(LoadError::Unsupported("unknown Realtek chip"))) };
        tracing::info!("realtek {}: ROM version {rom_version}, key ID {key_id} (read {key_read})", self.chip);
        let file = self.file.to_owned();
        let Some(data) = (self.read_file)(&file) else { return self.finish(Err(LoadError::MissingFile(file))) };
        let built = match chip.format {
            Format::Btnic { project } => btnic_download(&data, project, rom_version, key_id),
            Format::Core { project, .. } => core_download(&data, project, rom_version, key_id, key_read),
        };
        let (commands, summary) = match built {
            Ok(b) => b,
            Err(why) => return self.finish(Err(LoadError::BadFile { file, why })),
        };
        tracing::info!(
            "realtek {}: {file} ({} bytes, version {:02X?}), {summary} in {} commands",
            self.chip,
            data.len(),
            &data[8..16],
            commands.len()
        );
        self.state = State::Downloading { file, commands, next: 0 };
        self.send_next()
    }

    fn send_next(&mut self) -> Vec<Action> {
        let State::Downloading { commands, next, .. } = &self.state else { return Vec::new() };
        let Some((command, _)) = commands.get(*next) else { return Vec::new() };
        let timeout = if *next + 1 == commands.len() { LAST_TIMEOUT_MS } else { COMMAND_TIMEOUT_MS };
        vec![Action::Command(command.clone()), Action::Timer(timeout)]
    }

    fn on_download_answer(&mut self, opcode: u16, ret: &[u8]) -> Vec<Action> {
        let State::Downloading { file, commands, next } = &mut self.state else { return Vec::new() };
        let expect = commands[*next].1.clone();
        let ok = match expect {
            Expect::Written => opcode == OP_WRITE_REGISTER,
            Expect::Index(i) => opcode == OP_DOWNLOAD && ret.first() == Some(&i),
        };
        if !ok {
            if opcode != OP_DOWNLOAD {
                return Vec::new();
            }
            // The index the controller got is not the one sent
            let got = ret.first().copied().unwrap_or(0);
            tracing::warn!("realtek: download command {} answered with index 0x{got:02X}, sent {expect:?}", *next);
            return self.finish(Err(LoadError::Rejected(got)));
        }
        *next += 1;
        if *next == commands.len() {
            let firmware = std::mem::take(file);
            return self.finish(Ok(Outcome::Loaded { firmware, detail: None }));
        }
        self.send_next()
    }

    fn on_complete(&mut self, opcode: u16, status: u8, ret: &[u8]) -> Vec<Action> {
        match (&self.state, opcode) {
            // Whatever it answers, as the driver goes on
            (State::Prepare, OP_PREPARE) => {
                tracing::debug!("realtek {}: 0xFC66 status 0x{status:02X}", self.chip);
                self.after_prepare()
            }
            (_, _) if status != 0 && !matches!(self.state, State::Idle | State::Done) => {
                self.finish(Err(LoadError::Command { opcode, status }))
            }
            (State::Version, OP_READ_LOCAL_VERSION) => self.on_version(ret),
            (State::RomVersion, OP_READ_ROM_VERSION) => match ret.first() {
                Some(&rom_version) => self.on_rom_version(rom_version),
                None => self.finish(Err(LoadError::BadVersion)),
            },
            (&State::KeyId { rom_version }, OP_READ_REGISTER) => match ret.first() {
                Some(&key_id) => self.on_key_id(rom_version, key_id, true),
                None => self.finish(Err(LoadError::BadVersion)),
            },
            (State::Downloading { .. }, OP_WRITE_REGISTER | OP_DOWNLOAD) => self.on_download_answer(opcode, ret),
            // A late answer: ignored
            _ => Vec::new(),
        }
    }
}

/// The BTNIC003 download of a file for this project, ROM version and key ID
fn btnic_download(data: &[u8], project: u16, rom_version: u8, key_id: u8) -> Result<Download, &'static str> {
    let all = sections(data)?;
    let chosen = select(&all, project, rom_version, key_id).inspect_err(|_| {
        tracing::info!("realtek: sections {all:?}");
    })?;
    let bytes: usize = chosen.iter().map(|s| s.payload.len()).sum();
    Ok((download_commands(data, &chosen), format!("{} of {} sections, {bytes} bytes", chosen.len(), all.len())))
}

/// The RTBTCore download of a file for this project, ROM version and key ID
fn core_download(
    data: &[u8],
    project: u8,
    rom_version: u8,
    key_id: u8,
    key_read: bool,
) -> Result<Download, &'static str> {
    let all = core_parts(data)?;
    match patch_project(data) {
        Some(p) if p == project => {}
        Some(_) => return Err("another chip's firmware (project ID)"),
        None => return Err("no project ID"),
    }
    let chosen = core_select(&all, rom_version, key_id, key_read).inspect_err(|_| {
        tracing::info!("realtek: parts {all:?}");
    })?;
    let bytes: usize = chosen.iter().map(|p| p.payload.len()).sum();
    Ok((core_commands(data, &chosen), format!("{} of {} parts, {bytes} bytes", chosen.len(), all.len())))
}

impl ChipLoader for Loader {
    fn vendor(&self) -> &'static str {
        "Realtek"
    }

    fn start(&mut self) -> Vec<Action> {
        if self.known().is_none() {
            // Nothing sent: the app names only chips of CHIPS
            return self.finish(Err(LoadError::Unsupported("unknown Realtek chip")));
        }
        tracing::info!("realtek {}: checking its firmware, {}", self.chip, self.file);
        self.send(State::Version, OP_READ_LOCAL_VERSION, &[])
    }

    fn on_event(&mut self, event: &[u8]) -> Vec<Action> {
        match parse_event(event) {
            HciEvent::Complete { opcode, status, ret } => self.on_complete(opcode, status, ret),
            HciEvent::Status { opcode, status } if status != 0 => match self.state {
                State::Prepare if opcode == OP_PREPARE => self.after_prepare(),
                State::Idle | State::Done => Vec::new(),
                _ => self.finish(Err(LoadError::Command { opcode, status })),
            },
            _ => Vec::new(),
        }
    }

    fn on_timeout(&mut self) -> Vec<Action> {
        match self.state {
            // Not every ROM may answer it: go on as after an answer
            State::Prepare => self.after_prepare(),
            State::PrepareWait => self.send(State::RomVersion, OP_READ_ROM_VERSION, &[]),
            State::Version => self.finish(Err(LoadError::Timeout("Read Local Version"))),
            State::RomVersion => self.finish(Err(LoadError::Timeout("read ROM version"))),
            State::KeyId { .. } => self.finish(Err(LoadError::Timeout("read key ID"))),
            State::Downloading { .. } => self.finish(Err(LoadError::Timeout("firmware download"))),
            State::Idle | State::Done => Vec::new(),
        }
    }

    fn missing_file_hint(&self, file: &str) -> String {
        format!("{file} (from Realtek's Windows Bluetooth driver)")
    }

    fn step(&self) -> &'static str {
        match self.state {
            State::Downloading { .. } => "download",
            _ => "",
        }
    }

    fn progress(&self) -> Option<(usize, usize)> {
        match &self.state {
            State::Downloading { commands, next, .. } => Some((*next, commands.len())),
            _ => None,
        }
    }
}

// ---- Realtek's Windows driver ----

/// The names a driver file has on this PC: the package's own ("x.dat"), and
/// the copy the driver's install puts in the Windows folder, without the
/// extension ("x") or, in older packages, as "x.dll"
fn installed_names(name: &str) -> Vec<String> {
    match name.strip_suffix(".dat") {
        Some(stem) => vec![name.to_string(), stem.to_string(), format!("{stem}.dll")],
        None => vec![name.to_string()],
    }
}

/// Where Realtek's Windows driver keeps the file on this PC, newest first
/// (crate::driver_files): any package of the driver store (OEM packages
/// name their INF differently), the Windows folder, System32 and its drivers
/// folder (where the driver's install copies it, also after the package left
/// the driver store: issue #86), and the folder the driver reads updates from
fn driver_files(windows: &Path, name: &str) -> Vec<PathBuf> {
    let program_data =
        std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let names = installed_names(name);
    driver_files::find(
        windows,
        driver_files::Names::Exact(&names),
        &driver_files::any_package,
        &[program_data.join("Realtek").join("Patch")],
    )
}

/// A firmware file from Realtek's Windows driver, if it is installed: a
/// BTNIC003 or RTBTCore file of that name
pub fn from_windows_driver(name: &str) -> Option<Vec<u8>> {
    if name.contains(['\\', '/']) {
        return None;
    }
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    driver_files(&windows, name).iter().find_map(|path| {
        let data = std::fs::read(path).ok().filter(|d| d.starts_with(MAGIC) || d.starts_with(CORE_MAGIC))?;
        tracing::info!("realtek: {name} from Realtek's driver ({}), {} bytes", file_name(path), data.len());
        Some(data)
    })
}

/// The RTL8761B's firmware in Realtek's Windows driver (RtkBtfilter): the
/// same "Realtech" patch format as rtl8761bu_fw.bin, under its own name, and
/// no config file beside it (BTstack's download runs without one)
const RTL8761B_DRIVER_FILE: &str = "rtl8761b_mp_chip_bt40_fw_asic_rom_patch_new.dat";
/// The project ID the RTL8761B's patch carries
const RTL8761B_PROJECT: u8 = 14;
const EPATCH_MAGIC: &[u8; 8] = b"Realtech";
/// The last 4 bytes of a patch file
const EPATCH_EXTENSION: [u8; 4] = [0x51, 0x04, 0xFD, 0x77];

/// The driver's file that stands in for `fw_stem` of the firmware folder,
/// and the project ID its patch must carry
fn driver_firmware_name(fw_stem: &str) -> Option<(&'static str, u8)> {
    match fw_stem {
        "rtl8761bu_fw" => Some((RTL8761B_DRIVER_FILE, RTL8761B_PROJECT)),
        _ => None,
    }
}

/// The project ID of a "Realtech" patch ([`patch_project`]); None for
/// another format
fn epatch_project(data: &[u8]) -> Option<u8> {
    if !data.starts_with(EPATCH_MAGIC) {
        return None;
    }
    patch_project(data)
}

/// The project ID of a patch in either format BTstack's download reads:
/// "Realtech" or "RTBTCore" (btstack-sys build.rs patched_realtek)
fn download_patch_project(data: &[u8]) -> Option<u8> {
    if !data.starts_with(EPATCH_MAGIC) && !data.starts_with(CORE_MAGIC) {
        return None;
    }
    patch_project(data)
}

/// The version a "Realtech" patch stores after its signature
fn epatch_version(data: &[u8]) -> Option<u32> {
    let v = data.get(8..12)?;
    Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}

/// Realtek's Windows driver's firmware for a chip BTstack's Realtek download
/// loads (issue #86): of the copies on this PC that are a patch for this
/// chip, the one with the highest version its patch stores, then the newest
/// file; None for another chip or none there
pub fn driver_firmware(fw_stem: &str) -> Option<PathBuf> {
    let (name, project) = driver_firmware_name(fw_stem)?;
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    pick_patch(name, &driver_files(&windows, name), project)
}

/// The firmware of the adapter's own driver package,
/// the driver-store package whose INF lists this USB VID:PID, when it holds
/// the chip's patch (either format). Adapters on the same chip get board
/// tuned firmware from their vendor: TP-Link's UB600 (37AD:0600) driver has
/// a v2 patch (Bluetooth 6.0) where the generic Realtek one (UB500,
/// 2357:0604) has v1 (5.4), and each ran clearly worse on the other's
/// (measured on Realme Buds Air 8 Pro, LHDC V5 1000 kbps). None when no
/// package names the adapter: the caller falls back to [`driver_firmware`].
pub fn own_driver_firmware(fw_stem: &str, vid: u16, pid: u16) -> Option<PathBuf> {
    let (name, project) = driver_firmware_name(fw_stem)?;
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    let found = own_package_patch(&windows, name, project, vid, pid);
    match &found {
        Some(p) => tracing::info!(
            "realtek: {} of the adapter's own driver package ({}), for {vid:04X}:{pid:04X}",
            file_name(p),
            p.parent().map(file_name).unwrap_or_default()
        ),
        None => tracing::debug!("realtek: no driver package names {vid:04X}:{pid:04X} with this chip's firmware"),
    }
    found
}

/// The patch for `project` under one of `name`'s installed names in the
/// driver-store packages of `windows` whose INF lists `vid`:`pid`, the
/// newest file if several
fn own_package_patch(windows: &Path, name: &str, project: u8, vid: u16, pid: u16) -> Option<PathBuf> {
    let id = format!("VID_{vid:04X}&PID_{pid:04X}");
    let store = windows.join("System32").join("DriverStore").join("FileRepository");
    let names = installed_names(name);
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(store).ok()?.flatten() {
        let dir = entry.path();
        // The package's INF is read only when it holds one of the names
        let held: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).filter(|p| p.is_file()).collect();
        if held.is_empty() || !inf_lists(&dir, &id) {
            continue;
        }
        for path in held {
            let Ok(data) = std::fs::read(&path) else { continue };
            if download_patch_project(&data) == Some(project) {
                found.push(path);
            } else {
                tracing::debug!(
                    "realtek: {} of the adapter's package skipped: not this chip's firmware",
                    file_name(&path)
                );
            }
        }
    }
    let modified = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    found.into_iter().max_by_key(modified)
}

/// Whether an INF of the package folder `dir` lists the hardware ID part
/// `id` ("VID_37AD&PID_0600"); INFs come in ANSI or UTF-16
fn inf_lists(dir: &Path, id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| {
        let path = e.path();
        path.extension().is_some_and(|x| x.eq_ignore_ascii_case("inf"))
            && std::fs::read(&path).is_ok_and(|bytes| inf_text(&bytes).to_ascii_uppercase().contains(id))
    })
}

fn inf_text(bytes: &[u8]) -> String {
    match bytes.strip_prefix(&[0xFF, 0xFE]) {
        Some(utf16) => {
            let units: Vec<u16> = utf16.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            String::from_utf16_lossy(&units)
        }
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Of `found` (newest first), the patch for `project` with the highest
/// stored version, the newest file of equal ones
fn pick_patch(name: &str, found: &[PathBuf], project: u8) -> Option<PathBuf> {
    // driver_files lists the newest first: max_by_key keeps the last of
    // equal versions, so the list is walked oldest first
    let valid: Vec<(u32, &PathBuf)> = found
        .iter()
        .rev()
        .filter_map(|p| {
            let data = std::fs::read(p).ok()?;
            if epatch_project(&data) != Some(project) {
                tracing::debug!("realtek: {} of Realtek's driver skipped: not this chip's firmware", file_name(p));
                return None;
            }
            Some((epatch_version(&data)?, p))
        })
        .collect();
    let pick = valid.iter().max_by_key(|(v, _)| *v).map(|(v, p)| (*v, (*p).clone()));
    match &pick {
        Some((v, p)) => tracing::debug!(
            "realtek: {} of Realtek's driver, version 0x{v:08X} ({} usable of {} copies)",
            file_name(p),
            valid.len(),
            found.len()
        ),
        None => {
            tracing::debug!("realtek: no usable {name} of Realtek's driver on this PC ({} copies found)", found.len())
        }
    }
    pick.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_firmware_only_for_the_rtl8761b() {
        assert_eq!(driver_firmware_name("rtl8761bu_fw"), Some((RTL8761B_DRIVER_FILE, RTL8761B_PROJECT)));
        assert_eq!(driver_firmware_name("rtl8852bu_fw"), None);
        assert_eq!(driver_firmware_name(""), None);
    }

    #[test]
    fn the_installed_copies_of_a_driver_file() {
        assert_eq!(installed_names("a_new.dat"), ["a_new.dat", "a_new", "a_new.dll"]);
        assert_eq!(installed_names("a_new.bin"), ["a_new.bin"]);
    }

    /// A "Realtech" patch whose extension records end with this project ID
    fn epatch(project: u8) -> Vec<u8> {
        let mut f = EPATCH_MAGIC.to_vec();
        f.extend_from_slice(&[0x11; 40]);
        // The project ID, then (read first) a record of another opcode
        f.extend_from_slice(&[0xFF, project, 0x01, 0x00, 0xAA, 0xBB, 0x02, 0x05]);
        f.extend_from_slice(&EPATCH_EXTENSION);
        f
    }

    #[test]
    fn the_project_id_of_a_patch() {
        assert_eq!(epatch_project(&epatch(RTL8761B_PROJECT)), Some(RTL8761B_PROJECT));
        assert_eq!(epatch_project(&epatch(20)), Some(20));
        // No extension records, another format, too short
        let mut bare = EPATCH_MAGIC.to_vec();
        bare.extend_from_slice(&[0xFF, 0x00, 0x00]);
        bare.extend_from_slice(&EPATCH_EXTENSION);
        assert_eq!(epatch_project(&bare), None);
        assert_eq!(epatch_project(&container(&[])), None);
        assert_eq!(epatch_project(b"Realtech"), None);
        assert_eq!(epatch_project(&[]), None);
        // An RTBTCore patch has them too, but is not a "Realtech" one
        let core = core_file(44, &[]);
        assert_eq!(patch_project(&core), Some(44));
        assert_eq!(epatch_project(&core), None);
    }

    /// A "RTBTCore" (v2) patch with the same extension records
    fn epatch_v2(project: u8) -> Vec<u8> {
        let mut f = epatch(project);
        f[..8].copy_from_slice(CORE_MAGIC);
        f
    }

    #[test]
    fn both_patch_formats_carry_the_project() {
        assert_eq!(download_patch_project(&epatch_v2(RTL8761B_PROJECT)), Some(RTL8761B_PROJECT));
        assert_eq!(download_patch_project(&epatch(RTL8761B_PROJECT)), Some(RTL8761B_PROJECT));
        assert_eq!(epatch_project(&epatch_v2(RTL8761B_PROJECT)), None, "the generic pick stays v1");
        assert_eq!(download_patch_project(&container(&[])), None);
        assert_eq!(download_patch_project(b"RTBTCore"), None);
    }

    /// The package whose INF names the adapter gives its firmware, in
    /// either format and INF encoding; a package naming it with another
    /// chip's patch, or none naming it, gives nothing
    #[test]
    fn the_adapters_own_package_wins() {
        let windows = std::env::temp_dir().join(format!("bc-rtk-own-{}", std::process::id()));
        let store = windows.join("System32").join("DriverStore").join("FileRepository");
        let stem = RTL8761B_DRIVER_FILE.strip_suffix(".dat").unwrap();
        let package = |dir: &str, inf: &[u8], file: &str, data: Vec<u8>| {
            let d = store.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("x.inf"), inf).unwrap();
            std::fs::write(d.join(file), data).unwrap();
            d.join(file)
        };
        let tplink = package(
            "rtkbtfilterd.inf_amd64_1",
            b"[RsBthFilterD]\r\n%RealTKD.DeviceDesc% = RsBthFilterD, USB\\VID_37AD&PID_0600 ;TPLink BT6\r\n",
            stem,
            epatch_v2(RTL8761B_PROJECT),
        );
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("%Dev% = RtkFilter, usb\\vid_2357&pid_0604\r\n".encode_utf16().flat_map(u16::to_le_bytes));
        let generic = package("rtkfilter.inf_amd64_2", &utf16, RTL8761B_DRIVER_FILE, epatch(RTL8761B_PROJECT));
        package("other.inf_amd64_3", b"USB\\VID_0BDA&PID_8771", stem, epatch(20));
        let own = |vid, pid| own_package_patch(&windows, RTL8761B_DRIVER_FILE, RTL8761B_PROJECT, vid, pid);
        let (ub600, ub500, other, none) = (own(0x37AD, 0x0600), own(0x2357, 0x0604), own(0x0BDA, 0x8771), own(1, 2));
        let _ = std::fs::remove_dir_all(&windows);
        assert_eq!(ub600, Some(tplink));
        assert_eq!(ub500, Some(generic));
        assert_eq!(other, None, "its package holds another chip's patch");
        assert_eq!(none, None);
    }

    /// Issue #86: a PC whose driver store no longer holds Realtek's package
    /// still has the copies its install made in the Windows folder, without
    /// the extension or as .dll
    #[test]
    fn the_copies_in_the_windows_folder_are_found() {
        let windows = std::env::temp_dir().join(format!("bc-rtk-driver-{}", std::process::id()));
        let drivers = windows.join("System32").join("drivers");
        std::fs::create_dir_all(&drivers).unwrap();
        let stem = RTL8761B_DRIVER_FILE.strip_suffix(".dat").unwrap();
        std::fs::write(windows.join(stem), epatch(RTL8761B_PROJECT)).unwrap();
        std::fs::write(drivers.join(format!("{stem}.dll")), epatch(RTL8761B_PROJECT)).unwrap();
        let mut found: Vec<String> =
            driver_files(&windows, RTL8761B_DRIVER_FILE).iter().map(|p| file_name(p)).collect();
        found.sort();
        let _ = std::fs::remove_dir_all(&windows);
        assert_eq!(found, [stem.to_string(), format!("{stem}.dll")]);
    }

    #[test]
    fn the_highest_patch_version_wins_then_the_newest_file() {
        let dir = std::env::temp_dir().join(format!("bc-rtk-pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |n: &str, version: u32, project: u8| {
            let mut d = epatch(project);
            d[8..12].copy_from_slice(&version.to_le_bytes());
            std::fs::write(dir.join(n), d).unwrap();
            dir.join(n)
        };
        let (old, new, other, twin) =
            (file("old", 1, 14), file("new", 2, 14), file("other", 9, 20), file("twin", 2, 14));
        // Newest first, as driver_files lists them
        let pick = |list: &[PathBuf]| pick_patch("x", list, 14).map(|p| file_name(&p));
        assert_eq!(pick(&[old.clone(), new.clone(), other.clone()]).as_deref(), Some("new"));
        assert_eq!(pick(&[twin.clone(), new.clone()]).as_deref(), Some("twin"));
        assert_eq!(pick(&[new.clone(), twin.clone()]).as_deref(), Some("new"));
        assert_eq!(pick(&[other]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The installed Realtek driver of this PC, if it has the RTL8761B's
    /// firmware (nothing to check otherwise): what the lookup finds is a
    /// patch for the chip. `cargo test -- --ignored --nocapture` shows it.
    #[test]
    #[ignore = "reads this PC's driver store"]
    fn the_rtl8761b_firmware_of_this_pc() {
        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        for p in driver_files(&windows, RTL8761B_DRIVER_FILE) {
            let project = std::fs::read(&p).ok().and_then(|d| epatch_project(&d));
            println!("{} -> project {project:?}", p.display());
        }
        if let Some(p) = driver_firmware("rtl8761bu_fw") {
            println!("picked {}", p.display());
            assert_eq!(epatch_project(&std::fs::read(p).unwrap()), Some(RTL8761B_PROJECT));
        }
    }

    /// What this PC's driver store gives TP-Link's UB600 and UB500 as their
    /// own firmware. `cargo test -- --ignored --nocapture` shows it.
    #[test]
    #[ignore = "reads this PC's driver store"]
    fn the_own_firmware_of_this_pcs_adapters() {
        for (vid, pid) in [(0x37AD, 0x0600), (0x2357, 0x0604)] {
            let picked = own_driver_firmware("rtl8761bu_fw", vid, pid);
            let size = picked.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
            println!("{vid:04X}:{pid:04X} -> {picked:?} ({size:?} bytes)");
            if let Some(p) = picked {
                assert_eq!(download_patch_project(&std::fs::read(p).unwrap()), Some(RTL8761B_PROJECT));
            }
        }
    }
    use crate::bringup::sim::{self, SimController, command_complete, command_status};

    /// A test section: opcode, eco, key ID, order, register write, payload
    type TestSection = (u32, u8, u8, u8, Option<(u32, u32)>, Vec<u8>);

    /// A BTNIC003 file of these sections
    fn container(sections: &[TestSection]) -> Vec<u8> {
        let mut f = MAGIC.to_vec();
        f.extend_from_slice(&[0x2E, 0x28, 0x35, 0x01, 0xCF, 0x34, 0x03, 0x00]);
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&(sections.len() as u32).to_le_bytes());
        for (opcode, eco, key, order, write, payload) in sections {
            let mut desc = vec![0u8; DESCRIPTION];
            desc[0..2].copy_from_slice(&0x14u16.to_le_bytes());
            desc[2] = *eco;
            desc[3] = *key;
            if let Some((a, v)) = write {
                desc[0x3DF..0x3E3].copy_from_slice(&a.to_le_bytes());
                desc[0x3E7..0x3EB].copy_from_slice(&v.to_le_bytes());
            }
            desc[0x3FF] = *order;
            desc[0x400..0x404].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            f.extend_from_slice(&opcode.to_le_bytes());
            f.extend_from_slice(&((DESCRIPTION + payload.len()) as u32).to_le_bytes());
            f.extend_from_slice(&0u32.to_le_bytes());
            f.extend_from_slice(&desc);
            f.extend_from_slice(payload);
        }
        f
    }

    fn sample_file() -> Vec<u8> {
        container(&[
            (1, 4, 0, 1, Some((0x8012_00CC, 0x8010_E990)), vec![0xB1; 10]),
            (1, 4, 0, 0, Some((0x8012_00CC, 0x8010_F720)), vec![0xA1; 300]),
            // Another ROM version: never sent
            (1, 2, 0, 0, None, vec![0xEE; 20]),
        ])
    }

    /// A Realtek chip in its ROM, as the driver checks it: Read Local
    /// Version (HCI revision, LMP subversion), the ROM version, the key ID
    /// register; each download command answered with its index
    struct Chip {
        subversion: u16,
        revision: u16,
        rom_version: u8,
        key_id: u8,
        /// Answer the download's last command with this index (None: the
        /// index sent)
        last_answer: Option<u8>,
        /// Silent to this opcode
        silent: Option<u16>,
        /// Answer this opcode with this error status
        fails: Option<(u16, u8)>,
    }

    impl Chip {
        fn rtl8852bd() -> Chip {
            Chip {
                subversion: 0x8852,
                revision: 0,
                rom_version: 3,
                key_id: 0,
                last_answer: None,
                silent: None,
                fails: None,
            }
        }
        fn rtl8922a() -> Chip {
            Chip { subversion: 0x8922, revision: 0x000A, rom_version: 1, key_id: 1, ..Chip::rtl8852bd() }
        }
        fn rtl8852bt() -> Chip {
            Chip { subversion: 0x8852, revision: 0x0087, rom_version: 0, key_id: 0, ..Chip::rtl8852bd() }
        }
    }

    impl SimController for Chip {
        fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
            let opcode = sim::opcode(c);
            let p = sim::params(c);
            if self.silent == Some(opcode) {
                return Vec::new();
            }
            if let Some((op, status)) = self.fails.filter(|f| f.0 == opcode) {
                return vec![command_complete(op, status, &[])];
            }
            match opcode {
                OP_READ_LOCAL_VERSION => {
                    let mut ret = vec![0x0B];
                    ret.extend_from_slice(&self.revision.to_le_bytes());
                    ret.extend_from_slice(&[0x0B, 0x5D, 0x00]);
                    ret.extend_from_slice(&self.subversion.to_le_bytes());
                    vec![command_complete(opcode, 0, &ret)]
                }
                OP_PREPARE => vec![command_complete(opcode, 0, &[])],
                OP_READ_ROM_VERSION => vec![command_complete(opcode, 0, &[self.rom_version])],
                OP_READ_REGISTER if p == [0x21, 0xA4, 0xAD, 0x00, 0xB0] => {
                    vec![command_complete(opcode, 0, &[self.key_id, 0, 0, 0])]
                }
                OP_WRITE_REGISTER => vec![command_complete(opcode, 0, &[])],
                OP_DOWNLOAD if p[0] & LAST_INDEX != 0 => {
                    vec![command_complete(opcode, 0, &[self.last_answer.unwrap_or(p[0])])]
                }
                OP_DOWNLOAD => vec![command_complete(opcode, 0, &[p[0]])],
                _ => Vec::new(),
            }
        }
    }

    fn run_chip(chip: &mut Chip, name: &'static str, file: Option<Vec<u8>>) -> sim::SimRun {
        let c = driver_chip(name).unwrap();
        let mut loader = Loader::new(Box::new(move |_| file.clone()), c.name, c.file);
        sim::run(&mut loader, chip)
    }

    fn run(chip: &mut Chip, file: Option<Vec<u8>>) -> sim::SimRun {
        run_chip(chip, "RTL8852BD", file)
    }

    #[test]
    fn the_sections_are_read_from_the_container() {
        let f = sample_file();
        let s = sections(&f).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].eco, s[0].order, s[0].write), (4, 1, Some((0x8012_00CC, 0x8010_E990))));
        assert_eq!(&f[s[1].payload.clone()], &[0xA1; 300][..]);
        assert_eq!(s[2].write, None);
        assert!(sections(&f[..f.len() - 1]).is_err());
        assert!(sections(b"RTBTCore").is_err());
    }

    #[test]
    fn only_the_rom_versions_sections_are_chosen_in_their_order() {
        let f = sample_file();
        let all = sections(&f).unwrap();
        let chosen = select(&all, 0x14, 3, 0).unwrap();
        assert_eq!(chosen.iter().map(|s| s.order).collect::<Vec<_>>(), [0, 1]);
        assert!(select(&all, 0x14, 3, 1).is_err(), "another key ID");
        assert!(select(&all, 0x14, 5, 0).is_err(), "another ROM version");
        assert!(select(&all, 0x19, 3, 0).is_err(), "another project");
    }

    #[test]
    fn the_download_follows_the_drivers_sequence() {
        let r = run(&mut Chip::rtl8852bd(), Some(sample_file()));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: "rtl8852bd_mp_chip_new.dat".into(), detail: None }));
        assert_eq!(
            r.opcodes(),
            [
                OP_READ_LOCAL_VERSION,
                OP_PREPARE,
                OP_READ_ROM_VERSION,
                OP_READ_REGISTER,
                OP_WRITE_REGISTER,
                OP_DOWNLOAD,
                OP_DOWNLOAD,
                OP_WRITE_REGISTER,
                OP_DOWNLOAD,
                OP_DOWNLOAD
            ]
        );
        assert_eq!(r.timers[..3], [COMMAND_TIMEOUT_MS, COMMAND_TIMEOUT_MS, PREPARE_WAIT_MS]);
        assert_eq!(r.commands[3].params(), [0x21, 0xA4, 0xAD, 0x00, 0xB0]);
        // The order-0 section first, its register write, then its payload
        // from index 1: 252 bytes, then 48
        assert_eq!(r.commands[4].params(), [0x21, 0xCC, 0x00, 0x12, 0x80, 0x20, 0xF7, 0x10, 0x80]);
        assert_eq!(r.commands[5].params()[0], 1);
        assert_eq!(r.commands[5].params().len(), 253);
        assert_eq!(r.commands[6].params()[0], 2);
        assert_eq!(&r.commands[6].params()[1..], &[0xA1; 48][..]);
        // The next section starts at index 1 again
        assert_eq!(r.commands[7].params(), [0x21, 0xCC, 0x00, 0x12, 0x80, 0x90, 0xE9, 0x10, 0x80]);
        assert_eq!(r.commands[8].params(), [&[1u8][..], &[0xB1; 10]].concat());
        assert_eq!(r.commands[9].params(), [LAST_INDEX]);
        assert_eq!(*r.timers.last().unwrap(), LAST_TIMEOUT_MS);
        assert!(!r.bootloader);
    }

    #[test]
    fn the_index_goes_from_0x7f_back_to_1() {
        let f = container(&[(1, 4, 0, 0, None, vec![0x55; CHUNK * 130])]);
        let all = sections(&f).unwrap();
        let commands = download_commands(&f, &select(&all, 0x14, 3, 0).unwrap());
        let indexes: Vec<u8> = commands.iter().map(|(c, _)| c[3]).collect();
        assert_eq!(indexes[126], 0x7F);
        assert_eq!(indexes[127], 1);
        assert_eq!(*indexes.last().unwrap(), LAST_INDEX);
    }

    #[test]
    fn running_firmware_is_left_alone() {
        let mut chip = Chip { subversion: 0x950E, ..Chip::rtl8852bd() };
        let r = run(&mut chip, Some(sample_file()));
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert_eq!(r.opcodes(), [OP_READ_LOCAL_VERSION]);
    }

    #[test]
    fn nothing_is_sent_without_a_patch_for_the_rom() {
        let mut chip = Chip { rom_version: 5, ..Chip::rtl8852bd() };
        let r = run(&mut chip, Some(sample_file()));
        assert!(matches!(r.result, Err(LoadError::BadFile { .. })), "{:?}", r.result);
        assert!(!r.opcodes().contains(&OP_DOWNLOAD));
        let r = run(&mut Chip::rtl8852bd(), None);
        assert_eq!(r.result, Err(LoadError::MissingFile("rtl8852bd_mp_chip_new.dat".into())));
    }

    #[test]
    fn a_wrong_end_answer_fails_the_download() {
        let mut chip = Chip { last_answer: Some(0x05), ..Chip::rtl8852bd() };
        let r = run(&mut chip, Some(sample_file()));
        assert_eq!(r.result, Err(LoadError::Rejected(0x05)));
    }

    #[test]
    fn a_silent_prepare_command_is_passed() {
        let mut chip = Chip { silent: Some(OP_PREPARE), ..Chip::rtl8852bd() };
        let r = run(&mut chip, Some(sample_file()));
        assert!(matches!(r.result, Ok(Outcome::Loaded { .. })), "{:?}", r.result);
    }

    #[test]
    fn the_rtl8852bd_is_a_realtek_adapter_outside_btstacks_table() {
        use crate::chips::{self, ChipVendor};
        // No BTstack Realtek init for it: that would look for rtl8852bu_fw
        assert_eq!(chips::resolve_realtek_pid(0x0BDA, 0xB853, crate::realtek_usb_firmware()), None);
        assert_eq!(chips::vendor_for_usb(0x0BDA, 0xB853, None), ChipVendor::Realtek);
        assert_eq!(chips::display_name(0x0BDA, 0xB853, None, ChipVendor::Realtek), "RTL8852BD (0x0BDA:0xB853)");
        assert_eq!(chips::chip_support_tag(ChipVendor::Realtek, 0x0BDA, 0xB853), None);
        assert_eq!(chip(0x0BDA, 0x4853), None);
    }

    /// Every chip of the table: in neither BTstack's Realtek table nor the
    /// OEM table (they would take it to BTstack's download), not another
    /// vendor's loader chip, a Realtek adapter with its chip's name, and its
    /// file and ROM as the chip says
    #[test]
    fn the_driver_chips_are_realtek_adapters_btstack_does_not_load() {
        use crate::chips::{self, ChipVendor};
        let btstack = crate::realtek_usb_firmware();
        for &(vid, pid, c) in CHIPS {
            assert_eq!(chips::resolve_realtek_pid(vid, pid, btstack), None, "{vid:04X}:{pid:04X}");
            assert!(!chips::LOADER_CHIPS.iter().any(|&(_, v, p)| (v, p) == (vid, pid)), "{vid:04X}:{pid:04X}");
            assert_eq!(chips::vendor_for_usb(vid, pid, None), ChipVendor::Realtek, "{vid:04X}:{pid:04X}");
            assert_eq!(
                chips::display_name(vid, pid, None, ChipVendor::Realtek),
                format!("{} (0x{vid:04X}:0x{pid:04X})", c.name)
            );
            assert_eq!(chip(vid, pid), Some((c.name, c.file)));
            assert_eq!(CHIPS.iter().filter(|o| (o.0, o.1) == (vid, pid)).count(), 1, "{vid:04X}:{pid:04X} twice");
        }
        assert_eq!(chip(0x0BDA, 0xB892), Some(("RTL8922A", "rtl8922a_mp_chip_new.dat")));
        assert_eq!(chip(0x0BDA, 0xB86A), Some(("RTL8852BT", "rtl8852bt_mp_chip_new.dat")));
        // The RTL8922D (another ROM revision, signed sections) is not served
        assert_eq!(chip(0x0BDA, 0x88F1), None);
    }

    #[test]
    fn an_unknown_chip_gets_nothing_sent() {
        let mut loader = Loader::new(Box::new(|_| Some(sample_file())), "RTL8922D", "rtl8922d_mp_chip_new.dat");
        let r = sim::run(&mut loader, &mut Chip::rtl8852bd());
        assert!(matches!(r.result, Err(LoadError::Unsupported(_))), "{:?}", r.result);
        assert!(r.commands.is_empty());
    }

    // ---- RTBTCore (RTL8922A, RTL8852BT) ----

    /// A test part: section opcode, eco, priority, key ID, payload
    type TestPart = (u32, u8, u8, u8, Vec<u8>);

    /// An RTBTCore patch of this project ID with these parts, each section
    /// opcode in a section of its own, and a section the driver skips
    fn core_file(project: u8, parts: &[TestPart]) -> Vec<u8> {
        let mut sections: Vec<(u32, Vec<u8>)> = Vec::new();
        for &(op, eco, pri, key, ref payload) in parts {
            if !sections.iter().any(|s| s.0 == op) {
                sections.push((op, vec![0, 0, 0, 0]));
            }
            let s = sections.iter_mut().find(|s| s.0 == op).unwrap();
            let n = u16::from_le_bytes([s.1[0], s.1[1]]) + 1;
            s.1[0..2].copy_from_slice(&n.to_le_bytes());
            s.1.extend_from_slice(&[eco, pri, key, 0]);
            s.1.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            s.1.extend_from_slice(payload);
        }
        sections.push((8, Vec::new()));
        let mut f = CORE_MAGIC.to_vec();
        f.extend_from_slice(&[0x5F, 0x27, 0x35, 0x01, 0x51, 0xA9, 0x02, 0x00]);
        f.extend_from_slice(&(sections.len() as u32).to_le_bytes());
        for (op, body) in &sections {
            f.extend_from_slice(&op.to_le_bytes());
            f.extend_from_slice(&(body.len() as u32).to_le_bytes());
            f.extend_from_slice(body);
        }
        f.extend_from_slice(&[0xFF, 0x00, project, 0x01, 0x00]);
        f.extend_from_slice(&EPATCH_EXTENSION);
        f
    }

    /// An RTL8922A file: ROM version 1 (eco 2) for key IDs 0 to 2, a part
    /// of another ROM version
    fn rtl8922a_file() -> Vec<u8> {
        core_file(
            44,
            &[
                (1, 2, 15, 0, vec![0xA0; 300]),
                (1, 2, 15, 1, vec![0xA1; 300]),
                (1, 2, 15, 2, vec![0xA2; 300]),
                (1, 1, 15, 1, vec![0xEE; 40]),
            ],
        )
    }

    /// An RTL8852BT file: ROM version 0 (eco 1)
    fn rtl8852bt_file() -> Vec<u8> {
        core_file(47, &[(1, 1, 15, 0, vec![0xB0; 100]), (1, 2, 15, 0, vec![0xEE; 40])])
    }

    #[test]
    fn the_parts_of_an_rtbtcore_patch() {
        let f = rtl8922a_file();
        let parts = core_parts(&f).unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!((parts[1].section, parts[1].eco, parts[1].priority, parts[1].key_id), (1, 2, 15, 1));
        assert_eq!(&f[parts[1].payload.clone()], &[0xA1; 300][..]);
        assert_eq!(patch_project(&f), Some(44));
        // Cut short anywhere in the sections, or another format
        assert!(core_parts(&f[..f.len() - 1000]).is_err());
        assert!(core_parts(&f[..30]).is_err());
        assert!(core_parts(&sample_file()).is_err());
    }

    #[test]
    fn the_parts_are_chosen_as_the_driver_does() {
        let f = core_file(
            44,
            &[
                (1, 2, 15, 0, vec![1]),
                (1, 2, 15, 1, vec![2]),
                (2, 2, 3, 0, vec![3]),
                (3, 2, 9, 1, vec![4]),
                (3, 2, 9, 2, vec![5]),
                (2, 1, 1, 0, vec![6]),
            ],
        );
        let parts = core_parts(&f).unwrap();
        let payloads = |c: Vec<&Part>| c.iter().map(|p| f[p.payload.start]).collect::<Vec<u8>>();
        // Key ID 1 read: section 1's part of that key, section 2's of the
        // ROM version, section 3's of the key; in the driver's order, which
        // moves a lower priority forward but is no full sort (the real files'
        // parts all have priority 15)
        assert_eq!(payloads(core_select(&parts, 1, 1, true).unwrap()), [4, 3, 2]);
        // No key ID read: every part of section 1, none of section 3
        assert_eq!(payloads(core_select(&parts, 1, 0, false).unwrap()), [3, 2, 1]);
        assert!(core_select(&parts, 5, 0, false).is_err());
    }

    #[test]
    fn the_rtl8922a_download_follows_the_drivers_sequence() {
        let r = run_chip(&mut Chip::rtl8922a(), "RTL8922A", Some(rtl8922a_file()));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: "rtl8922a_mp_chip_new.dat".into(), detail: None }));
        assert_eq!(
            r.opcodes(),
            [OP_READ_LOCAL_VERSION, OP_PREPARE, OP_READ_ROM_VERSION, OP_READ_REGISTER, OP_DOWNLOAD, OP_DOWNLOAD]
        );
        assert_eq!(r.commands[3].params(), [0x21, 0xA4, 0xAD, 0x00, 0xB0]);
        // Key ID 1's part only, 300 bytes: index 0 with 252, then the last
        // (0x80 | 1) with 48
        assert_eq!(r.commands[4].params(), [&[0u8][..], &[0xA1; 252]].concat());
        assert_eq!(r.commands[5].params(), [&[0x81u8][..], &[0xA1; 48]].concat());
        assert_eq!(r.timers[..3], [COMMAND_TIMEOUT_MS, COMMAND_TIMEOUT_MS, PREPARE_WAIT_MS]);
        assert_eq!(*r.timers.last().unwrap(), LAST_TIMEOUT_MS);
        assert!(r.commands.iter().all(|c| !c.bulk));
    }

    #[test]
    fn the_rtl8852bt_download_reads_no_key_id() {
        let r = run_chip(&mut Chip::rtl8852bt(), "RTL8852BT", Some(rtl8852bt_file()));
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: "rtl8852bt_mp_chip_new.dat".into(), detail: None }));
        assert_eq!(r.opcodes(), [OP_READ_LOCAL_VERSION, OP_PREPARE, OP_READ_ROM_VERSION, OP_DOWNLOAD]);
        // One command, the last: index 0x80
        assert_eq!(r.commands[3].params(), [&[0x80u8][..], &[0xB0; 100]].concat());
    }

    #[test]
    fn the_rtbtcore_index_starts_at_0_and_goes_from_0x7f_back_to_1() {
        let f = core_file(47, &[(1, 1, 15, 0, vec![0x55; CHUNK * 130 + 5])]);
        let parts = core_parts(&f).unwrap();
        let commands = core_commands(&f, &core_select(&parts, 0, 0, false).unwrap());
        let indexes: Vec<u8> = commands.iter().map(|(c, _)| c[3]).collect();
        assert_eq!(indexes.len(), 131);
        assert_eq!((indexes[0], indexes[127], indexes[128]), (0, 0x7F, 1));
        assert_eq!(indexes[130], 0x80 | 3);
        assert_eq!(commands[130].0.len(), 3 + 1 + 5);
    }

    #[test]
    fn a_core_chip_running_its_firmware_is_left_alone() {
        let mut chip = Chip { subversion: 0x1234, ..Chip::rtl8922a() };
        let r = run_chip(&mut chip, "RTL8922A", Some(rtl8922a_file()));
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert_eq!(r.opcodes(), [OP_READ_LOCAL_VERSION]);
    }

    /// Another chip of the ROM subversion (the RTL8852B, C, BD answer 0x8852
    /// too): refused after Read Local Version alone
    #[test]
    fn another_rom_revision_is_refused_before_anything_is_sent() {
        let mut chip = Chip { revision: 0x000B, ..Chip::rtl8852bt() };
        let r = run_chip(&mut chip, "RTL8852BT", Some(rtl8852bt_file()));
        assert_eq!(r.result, Err(LoadError::UnsupportedVariant(0x0B)));
        assert_eq!(r.opcodes(), [OP_READ_LOCAL_VERSION]);
        let mut chip = Chip { revision: 0x000D, ..Chip::rtl8922a() };
        let r = run_chip(&mut chip, "RTL8922A", Some(rtl8922a_file()));
        assert_eq!(r.result, Err(LoadError::UnsupportedVariant(0x0D)));
    }

    #[test]
    fn a_missing_or_broken_core_file_sends_nothing_of_it() {
        let downloads = |r: &sim::SimRun| r.opcodes().iter().filter(|&&o| o == OP_DOWNLOAD).count();
        let r = run_chip(&mut Chip::rtl8922a(), "RTL8922A", None);
        assert_eq!(r.result, Err(LoadError::MissingFile("rtl8922a_mp_chip_new.dat".into())));
        // Cut short, another chip's project, no part for this ROM or key
        let mut cut = rtl8922a_file();
        cut.truncate(cut.len() - 400);
        let other = core_file(47, &[(1, 2, 15, 1, vec![0xA1; 300])]);
        let no_key = core_file(44, &[(1, 2, 15, 0, vec![0xA0; 300])]);
        for file in [cut, other, no_key, sample_file()] {
            let r = run_chip(&mut Chip::rtl8922a(), "RTL8922A", Some(file));
            assert!(matches!(r.result, Err(LoadError::BadFile { .. })), "{:?}", r.result);
            assert_eq!(downloads(&r), 0);
        }
    }

    #[test]
    fn a_rejected_command_ends_the_core_download() {
        for (op, status) in [(OP_READ_ROM_VERSION, 0x01), (OP_READ_REGISTER, 0x0C), (OP_DOWNLOAD, 0x12)] {
            let mut chip = Chip { fails: Some((op, status)), ..Chip::rtl8922a() };
            let r = run_chip(&mut chip, "RTL8922A", Some(rtl8922a_file()));
            assert_eq!(r.result, Err(LoadError::Command { opcode: op, status }));
        }
        // A Command Status with an error, in place of the answer
        struct Refusing(Chip);
        impl SimController for Refusing {
            fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                if sim::opcode(c) == OP_DOWNLOAD { vec![command_status(OP_DOWNLOAD, 0x0D)] } else { self.0.command(c) }
            }
        }
        let c = driver_chip("RTL8852BT").unwrap();
        let mut loader = Loader::new(Box::new(|_| Some(rtl8852bt_file())), c.name, c.file);
        let r = sim::run(&mut loader, &mut Refusing(Chip::rtl8852bt()));
        assert_eq!(r.result, Err(LoadError::Command { opcode: OP_DOWNLOAD, status: 0x0D }));
        // A wrong index echoed for the last command
        let mut chip = Chip { last_answer: Some(0x80), ..Chip::rtl8922a() };
        let r = run_chip(&mut chip, "RTL8922A", Some(rtl8922a_file()));
        assert_eq!(r.result, Err(LoadError::Rejected(0x80)));
    }

    #[test]
    fn each_wait_of_the_core_download_can_time_out() {
        for (op, step) in [
            (OP_READ_LOCAL_VERSION, "Read Local Version"),
            (OP_READ_ROM_VERSION, "read ROM version"),
            (OP_READ_REGISTER, "read key ID"),
            (OP_DOWNLOAD, "firmware download"),
        ] {
            let mut chip = Chip { silent: Some(op), ..Chip::rtl8922a() };
            let r = run_chip(&mut chip, "RTL8922A", Some(rtl8922a_file()));
            assert_eq!(r.result, Err(LoadError::Timeout(step)));
        }
        // The prepare command's silence is passed, as for the RTL8852BD
        let mut chip = Chip { silent: Some(OP_PREPARE), ..Chip::rtl8852bt() };
        let r = run_chip(&mut chip, "RTL8852BT", Some(rtl8852bt_file()));
        assert!(matches!(r.result, Ok(Outcome::Loaded { .. })), "{:?}", r.result);
        assert_eq!(r.timeouts, 2, "the prepare command's, then the 200 ms pause");
    }

    /// Against Realtek's real file: `RTL8852BD_DAT=<path to
    /// rtl8852bd_mp_chip_new.dat> cargo test -p bt-host real_btnic -- --ignored`
    #[test]
    #[ignore = "needs Realtek's rtl8852bd_mp_chip_new.dat (RTL8852BD_DAT)"]
    fn real_btnic_file() {
        let data = std::fs::read(std::env::var("RTL8852BD_DAT").unwrap()).unwrap();
        let all = sections(&data).unwrap();
        for s in &all {
            println!("{:?} payload {} bytes", s, s.payload.len());
        }
        let chosen = select(&all, 0x14, 3, 0).unwrap();
        let commands = download_commands(&data, &chosen);
        println!("ROM version 3, key 0: {} sections, {} commands", chosen.len(), commands.len());
        let r = run(&mut Chip::rtl8852bd(), Some(data));
        assert!(matches!(r.result, Ok(Outcome::Loaded { .. })), "{:?}", r.result);
    }

    /// Against Realtek's real RTBTCore files: `RTK_CORE_DIR=<folder with
    /// rtl8922a_mp_chip_new.dat and/or rtl8852bt_mp_chip_new.dat> cargo test
    /// -p bt-host real_core -- --ignored --nocapture`
    #[test]
    #[ignore = "needs Realtek's RTBTCore files (RTK_CORE_DIR)"]
    fn real_core_files() {
        let dir = PathBuf::from(std::env::var("RTK_CORE_DIR").unwrap());
        for (name, mut chip, keys) in
            [("RTL8922A", Chip::rtl8922a(), &[0u8, 1, 2][..]), ("RTL8852BT", Chip::rtl8852bt(), &[0u8][..])]
        {
            let c = driver_chip(name).unwrap();
            let Ok(data) = std::fs::read(dir.join(c.file)) else { continue };
            let parts = core_parts(&data).unwrap();
            println!("{name}: {} parts {:?}", parts.len(), parts);
            for &key in keys {
                chip.key_id = key;
                let r = run_chip(&mut chip, name, Some(data.clone()));
                println!("  key {key}: {:?}, {} commands", r.result, r.commands.len());
                assert!(matches!(r.result, Ok(Outcome::Loaded { .. })), "{:?}", r.result);
            }
        }
    }
}
