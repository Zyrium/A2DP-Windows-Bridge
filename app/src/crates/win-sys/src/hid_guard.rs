//! Keys and mouse buttons left held in Windows when the core dies
//!
//! While keyboards and mice pass their input to Windows (HID host,
//! experimental), a key or button the core pressed stays pressed in
//! Windows if the core exits before it lets go: a crash, or the process
//! killed. The key then repeats until the user presses it again, and a
//! held mouse button drags everything the pointer touches.
//!
//! So the core starts a guard: the same executable in a hidden mode
//! ([`run`]), handed three handles it inherits:
//!
//! - the core's process, to wait on;
//! - a small unnamed shared memory section ([`HeldSection`]) in which the
//!   core keeps what it holds pressed right now ([`HeldSet`]), updated on
//!   every key or button change it injects;
//! - an event the core sets when the guard is no longer needed (the
//!   feature turned off, [`Guard`] dropped).
//!
//! When the core's process ends, the guard reads the section and sends a
//! key-up or button-up for whatever is still held (with
//! [`crate::input_inject::INJECTED_TAG`], as the core's own input), then
//! exits. After a clean stop the core has released everything itself, the
//! set is empty and the guard sends nothing.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{
    DUPLICATE_HANDLE_OPTIONS, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateEventW, GetCurrentProcess, INFINITE, PROCESS_SYNCHRONIZE, SetEvent, WaitForMultipleObjects,
    WaitForSingleObject,
};
use windows::core::PCWSTR;

use crate::input_inject::{Inject, MouseButton};

/// The guard's mode on the command line: `--hid-guard <process> <section>
/// <stop> [--report <file>]`, the three handles as decimal numbers
pub const GUARD_ARG: &str = "--hid-guard";
/// Test switch: the guard writes what it would release to this file, one
/// line each, instead of sending it to Windows. Debug builds only; a
/// release guard refuses it (exit code 2).
pub const REPORT_ARG: &str = "--report";

/// Most keys and buttons kept; more held at once are not released by the
/// guard (a keyboard holds at most a few dozen)
pub const CAPACITY: usize = 254;
/// First word of the section ("BCHG")
const MAGIC: u32 = 0x4243_4847;
/// The section: magic, count, then the entries
const WORDS: usize = 2 + CAPACITY;

// Entry kinds, in bits 24..31 of a word
const KIND_SCAN: u32 = 1;
const KIND_VK: u32 = 2;
const KIND_BUTTON: u32 = 3;
// Bit 16: the scan code's E0 prefix
const EXTENDED: u32 = 1 << 16;

// Scan codes of Ctrl, Shift, Alt and the Windows keys: released last
fn is_modifier(scan: u16, extended: bool) -> bool {
    matches!((scan, extended), (0x1D | 0x38, _) | (0x2A | 0x36, false) | (0x5B | 0x5C, true))
}

fn button_code(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 3,
        MouseButton::X1 => 4,
        MouseButton::X2 => 5,
    }
}

/// An injected event as a set entry and whether it goes up; None for
/// moves and wheel turns, which hold nothing
fn entry(inject: Inject) -> Option<(u32, bool)> {
    match inject {
        Inject::Key { scan, extended, up } => {
            let ext = if extended { EXTENDED } else { 0 };
            Some((KIND_SCAN << 24 | ext | u32::from(scan), up))
        }
        Inject::VirtualKey { vk, up } => Some((KIND_VK << 24 | u32::from(vk), up)),
        Inject::Button { button, up } => Some((KIND_BUTTON << 24 | button_code(button), up)),
        Inject::Move { .. } | Inject::Wheel(_) | Inject::HWheel(_) => None,
    }
}

/// The key-up or button-up of a set entry; None for a word no version of
/// this code writes
fn release_of(word: u32) -> Option<Inject> {
    let code = (word & 0xFFFF) as u16;
    match word >> 24 {
        KIND_SCAN if word & 0x00FE_0000 == 0 => {
            Some(Inject::Key { scan: code, extended: word & EXTENDED != 0, up: true })
        }
        KIND_VK if word & 0x00FF_0000 == 0 => Some(Inject::VirtualKey { vk: code, up: true }),
        KIND_BUTTON if word & 0x00FF_0000 == 0 => {
            let button = match code {
                1 => MouseButton::Left,
                2 => MouseButton::Right,
                3 => MouseButton::Middle,
                4 => MouseButton::X1,
                5 => MouseButton::X2,
                _ => return None,
            };
            Some(Inject::Button { button, up: true })
        }
        _ => None,
    }
}

/// The keys and buttons held down in Windows by injected input. One entry
/// per key, whatever device pressed it: Windows treats a second key-down
/// as a repeat, and one key-up releases the key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldSet {
    words: BTreeSet<u32>,
}

impl HeldSet {
    /// Follows events that Windows took, in order; true when the set changed
    pub fn apply(&mut self, injected: &[Inject]) -> bool {
        let mut changed = false;
        for &i in injected {
            let Some((word, up)) = entry(i) else { continue };
            changed |= if up { self.words.remove(&word) } else { self.words.insert(word) };
        }
        changed
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    /// The events that let go of everything held: keys first, then Ctrl,
    /// Shift, Alt and the Windows keys, then mouse buttons
    pub fn releases(&self) -> Vec<Inject> {
        let mut out: Vec<Inject> = self.words.iter().filter_map(|&w| release_of(w)).collect();
        out.sort_by_key(|i| match *i {
            Inject::Key { scan, extended, .. } if is_modifier(scan, extended) => 1,
            Inject::Key { .. } | Inject::VirtualKey { .. } => 0,
            _ => 2,
        });
        out
    }

    /// The section's words: magic, count, entries (at most [`CAPACITY`])
    fn encode(&self) -> Vec<u32> {
        let entries: Vec<u32> = self.words.iter().copied().take(CAPACITY).collect();
        let mut out = vec![MAGIC, entries.len() as u32];
        out.extend(entries);
        out
    }

    /// From the section's words; empty when they are not a set
    fn decode(words: &[u32]) -> HeldSet {
        let (Some(&MAGIC), Some(&count)) = (words.first(), words.get(1)) else { return HeldSet::default() };
        let count = (count as usize).min(CAPACITY);
        HeldSet { words: words.iter().skip(2).take(count).copied().collect() }
    }
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn owned(handle: HANDLE) -> OwnedHandle {
    // SAFETY: every caller passes a handle it just created or duplicated
    // and owns, so OwnedHandle closes it exactly once.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}

/// A duplicate of `source` in this process, with `access` (None: the same
/// access) and inheritable or not
fn duplicate(source: HANDLE, access: Option<u32>, inherit: bool) -> std::io::Result<OwnedHandle> {
    let mut out = HANDLE::default();
    let (rights, options) = match access {
        Some(a) => (a, DUPLICATE_HANDLE_OPTIONS(0)),
        None => (0, DUPLICATE_SAME_ACCESS),
    };
    // SAFETY: `source` is a live handle of this process (or its pseudo
    // handle); `out` is written on success only.
    unsafe {
        let me = GetCurrentProcess();
        DuplicateHandle(me, source, me, &mut out, rights, inherit, options).map_err(|_| last_error())?;
    }
    Ok(owned(out))
}

/// The shared memory of a [`HeldSet`]: the core writes it, the guard reads
/// it once the core is gone. Unnamed: only processes that are handed the
/// handle can open it.
pub struct HeldSection {
    mapping: OwnedHandle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the view is only reached through atomics (`slots`), and is
// unmapped only on drop, when no reference to it is left.
unsafe impl Send for HeldSection {}
// SAFETY: as above.
unsafe impl Sync for HeldSection {}

impl std::fmt::Debug for HeldSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HeldSection")
    }
}

impl HeldSection {
    /// A new section, empty, readable and writable by this process
    pub fn create() -> std::io::Result<HeldSection> {
        let size = (WORDS * 4) as u32;
        // SAFETY: a mapping of the paging file without a name or security
        // attributes; the handle is owned below.
        let mapping =
            unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, size, PCWSTR::null()) }
                .map_err(|_| last_error())?;
        let section = Self::map(owned(mapping), FILE_MAP_READ | FILE_MAP_WRITE)?;
        section.store(&HeldSet::default());
        Ok(section)
    }

    fn map(mapping: OwnedHandle, access: FILE_MAP) -> std::io::Result<HeldSection> {
        // SAFETY: `mapping` is a live file mapping handle of at least
        // WORDS * 4 bytes (created so, or handed over by the core).
        let view = unsafe { MapViewOfFile(HANDLE(mapping.as_raw_handle()), access, 0, 0, WORDS * 4) };
        if view.Value.is_null() {
            return Err(last_error());
        }
        Ok(HeldSection { mapping, view })
    }

    fn slots(&self) -> &[AtomicU32] {
        // SAFETY: the view is WORDS * 4 bytes, page aligned and mapped for
        // as long as `self` lives; AtomicU32 has the layout of u32 and every
        // access to the section goes through these atomics.
        unsafe { std::slice::from_raw_parts(self.view.Value as *const AtomicU32, WORDS) }
    }

    /// Writes the set: the entries first, then the count (a process that
    /// dies in between leaves the old count over new entries, which at
    /// worst releases a key that is no longer held)
    pub fn store(&self, set: &HeldSet) {
        let words = set.encode();
        let slots = self.slots();
        for (slot, &w) in slots.iter().zip(&words).skip(2) {
            slot.store(w, Ordering::Relaxed);
        }
        slots[0].store(MAGIC, Ordering::Relaxed);
        slots[1].store(words[1], Ordering::Release);
    }

    pub fn load(&self) -> HeldSet {
        let slots = self.slots();
        let count = slots[1].load(Ordering::Acquire);
        let mut words = vec![slots[0].load(Ordering::Relaxed), count];
        words.extend(slots[2..].iter().map(|s| s.load(Ordering::Relaxed)));
        HeldSet::decode(&words)
    }
}

impl Drop for HeldSection {
    fn drop(&mut self) {
        // SAFETY: the view was mapped by `map` and nothing refers to it any
        // more; the mapping handle closes after this (OwnedHandle).
        let _ = unsafe { UnmapViewOfFile(self.view) };
    }
}

/// The running guard process. Dropping it tells the guard to exit without
/// releasing anything.
pub struct Guard {
    child: std::process::Child,
    stop: OwnedHandle,
}

impl Guard {
    /// Starts `exe` as the guard of `watched` (None: this process), reading
    /// `section`. `report`: the test switch ([`REPORT_ARG`]).
    pub fn spawn(
        exe: &Path,
        watched: Option<&std::process::Child>,
        section: &HeldSection,
        report: Option<&Path>,
    ) -> std::io::Result<Guard> {
        // SAFETY: a manual-reset event, unnamed, not signalled; owned below.
        let stop = owned(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(|_| last_error())?);
        let process = match watched {
            Some(child) => HANDLE(child.as_raw_handle()),
            // SAFETY: the pseudo handle of this process; DuplicateHandle
            // turns it into a real one.
            None => unsafe { GetCurrentProcess() },
        };
        // Inheritable duplicates with only what the guard needs, closed here
        // once the guard has its own
        let child_process = duplicate(process, Some(PROCESS_SYNCHRONIZE.0), true)?;
        let child_section = duplicate(HANDLE(section.mapping.as_raw_handle()), Some(FILE_MAP_READ.0), true)?;
        let child_stop = duplicate(HANDLE(stop.as_raw_handle()), Some(PROCESS_SYNCHRONIZE.0), true)?;
        let number = |h: &OwnedHandle| (h.as_raw_handle() as usize).to_string();
        let mut cmd = std::process::Command::new(exe);
        cmd.arg(GUARD_ARG)
            .arg(number(&child_process))
            .arg(number(&child_section))
            .arg(number(&child_stop))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0);
        if let Some(file) = report {
            cmd.arg(REPORT_ARG).arg(file);
        }
        let child = cmd.spawn()?;
        tracing::debug!("hid guard: started");
        Ok(Guard { child, stop })
    }

    /// Whether the guard process still runs
    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: `stop` is the live event the guard waits on.
        let _ = unsafe { SetEvent(HANDLE(self.stop.as_raw_handle())) };
        // A moment for it to go, so that a guard started next does not
        // overlap it for long; it never releases anything after a stop
        // SAFETY: the child's process handle lives as long as `self.child`.
        let _ = unsafe { WaitForSingleObject(HANDLE(self.child.as_raw_handle()), 1000) };
        let _ = self.child.try_wait();
    }
}

/// Parses the guard's arguments (those after [`GUARD_ARG`])
fn parse(args: &[String]) -> Option<([usize; 3], Option<PathBuf>)> {
    let mut it = args.iter();
    let mut handles = [0usize; 3];
    for h in &mut handles {
        *h = it.next()?.parse().ok().filter(|&v| v != 0)?;
    }
    let report = match it.next().map(String::as_str) {
        None => None,
        // Debug builds only: a release guard always releases (and never
        // writes a file named on its command line)
        Some(REPORT_ARG) if cfg!(debug_assertions) => Some(PathBuf::from(it.next()?)),
        Some(_) => return None,
    };
    it.next().is_none().then_some((handles, report))
}

/// One line of the test report
fn report_line(i: &Inject) -> String {
    match *i {
        Inject::Key { scan, extended, .. } => format!("key {scan:#04x}{}", if extended { " extended" } else { "" }),
        Inject::VirtualKey { vk, .. } => format!("vk {vk:#04x}"),
        Inject::Button { button, .. } => format!("button {button:?}"),
        _ => String::new(),
    }
}

/// The guard process: `args` are those after [`GUARD_ARG`]. Waits for the
/// watched process to end or for the stop event, then releases what the
/// section holds (on the first only). Its exit code: 0 done, 2 bad
/// arguments or handles.
pub fn run(args: &[String]) -> i32 {
    let Some(([process, section, stop], report)) = parse(args) else { return 2 };
    let raw = |v: usize| HANDLE(v as *mut c_void);
    // The handles were inherited: this process owns them now
    let process = owned(raw(process));
    let stop = owned(raw(stop));
    let Ok(section) = HeldSection::map(owned(raw(section)), FILE_MAP_READ) else { return 2 };
    let waits = [HANDLE(process.as_raw_handle()), HANDLE(stop.as_raw_handle())];
    // SAFETY: both handles are live and have SYNCHRONIZE access.
    let woke = unsafe { WaitForMultipleObjects(&waits, false, INFINITE) };
    if woke != WAIT_OBJECT_0 {
        // Stopped (or the wait failed): nothing to release
        return 0;
    }
    let releases = section.load().releases();
    match report {
        Some(file) => {
            let text: String = releases.iter().map(|i| report_line(i) + "\n").collect();
            if std::fs::write(file, text).is_err() {
                return 2;
            }
        }
        None => {
            let _ = crate::input_inject::send(&releases);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scan: u16, extended: bool, up: bool) -> Inject {
        Inject::Key { scan, extended, up }
    }

    #[test]
    fn set_follows_downs_and_ups() {
        let mut s = HeldSet::default();
        assert!(s.apply(&[key(0x1E, false, false), key(0x1D, true, false), Inject::Move { dx: 1, dy: 1 }]));
        // A second down (another keyboard, or a repeat) changes nothing
        assert!(!s.apply(&[key(0x1E, false, false), Inject::Wheel(120)]));
        assert_eq!(s.len(), 2);
        // Right Ctrl (extended) is not Left Ctrl
        assert!(!s.apply(&[key(0x1D, false, true)]));
        assert!(s.apply(&[key(0x1D, true, true), key(0x1E, false, true)]));
        assert!(s.is_empty());
    }

    #[test]
    fn releases_keys_then_modifiers_then_buttons() {
        let mut s = HeldSet::default();
        s.apply(&[
            Inject::Button { button: MouseButton::X2, up: false },
            key(0x2A, false, false),
            key(0x5B, true, false),
            key(0x48, true, false),
            Inject::VirtualKey { vk: 0xAE, up: false },
            key(0x1E, false, false),
        ]);
        assert_eq!(
            s.releases(),
            vec![
                key(0x1E, false, true),
                key(0x48, true, true),
                Inject::VirtualKey { vk: 0xAE, up: true },
                key(0x2A, false, true),
                key(0x5B, true, true),
                Inject::Button { button: MouseButton::X2, up: true },
            ]
        );
        assert!(HeldSet::default().releases().is_empty());
    }

    #[test]
    fn encode_decode_round_trip() {
        let mut s = HeldSet::default();
        s.apply(&[
            key(0x38, true, false),
            Inject::VirtualKey { vk: 0x13, up: false },
            Inject::Button { button: MouseButton::Left, up: false },
        ]);
        let words = s.encode();
        assert_eq!(words[..2], [MAGIC, 3]);
        assert_eq!(HeldSet::decode(&words), s);
        // Not a set: nothing
        assert!(HeldSet::decode(&[0, 3, 1, 2, 3]).is_empty());
        assert!(HeldSet::decode(&[MAGIC]).is_empty());
        // A count larger than the words is cut to the words
        assert_eq!(HeldSet::decode(&[MAGIC, 99, words[2]]).len(), 1);
    }

    #[test]
    fn unknown_words_release_nothing() {
        let s = HeldSet { words: [0x0900_0001, KIND_BUTTON << 24 | 9, KIND_SCAN << 24 | 0x20_0000 | 0x1E].into() };
        assert!(s.releases().is_empty());
    }

    #[test]
    fn capacity_is_kept() {
        let mut s = HeldSet::default();
        let many: Vec<Inject> = (1..=300).map(|n| key(n, false, false)).collect();
        s.apply(&many);
        let words = s.encode();
        assert_eq!(words.len(), WORDS);
        assert_eq!(HeldSet::decode(&words).len(), CAPACITY);
    }

    #[test]
    fn section_store_and_load() {
        let section = HeldSection::create().unwrap();
        assert!(section.load().is_empty());
        let mut s = HeldSet::default();
        s.apply(&[key(0x10, false, false), Inject::Button { button: MouseButton::Right, up: false }]);
        section.store(&s);
        assert_eq!(section.load(), s);
        // Fewer entries than before: the old ones beyond the count are not read
        s.apply(&[key(0x10, false, true)]);
        section.store(&s);
        assert_eq!(section.load(), s);
    }

    #[test]
    fn arguments() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&a(&["4", "8", "12"])), Some(([4, 8, 12], None)));
        let report = parse(&a(&["4", "8", "12", "--report", "x.txt"]));
        if cfg!(debug_assertions) {
            assert_eq!(report, Some(([4, 8, 12], Some(PathBuf::from("x.txt")))));
        } else {
            assert_eq!(report, None);
        }
        assert_eq!(parse(&a(&["4", "8"])), None);
        assert_eq!(parse(&a(&["4", "0", "12"])), None);
        assert_eq!(parse(&a(&["4", "8", "12", "--report"])), None);
        assert_eq!(parse(&a(&["4", "8", "12", "extra"])), None);
        assert_eq!(run(&a(&["nonsense"])), 2);
    }
}
