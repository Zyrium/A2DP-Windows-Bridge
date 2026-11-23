//! The service's data folder (service mode, crate::paths::set_service_data):
//! what lives there, and the one-time move of the signed-in user's data
//! into it.
//!
//! # Where each file lives
//!
//! In service mode the core keeps everything it reads or writes in the
//! service's folder, one for the PC ([`CLASSIFICATION`]): what is needed
//! before anyone signs in, and what is security-relevant (pairing keys, the
//! keyboard and mouse list, adapter and chip settings, firmware, the
//! settings that start the stack and the keyboard and mouse feature, the
//! core's logs and crash reports). The window (bluecaster-ui) never runs in
//! service mode: its log and crash reports stay in the user's folder.
//! Profiles go to the service's folder too: the core reads them before
//! anyone signs in (the tray's list, reconnecting) and they name only
//! devices and codecs. A PC with several users would need per-user
//! profiles and window preferences (settings.json's language, theme and
//! tray options) keyed by the user, which the prototype leaves out.
//!
//! # The move
//!
//! [`migrate`] runs once, when the service's folder has no record of it
//! ([`MARKER`]), as soon as the first agent connects (a service starts
//! before anyone signs in; what it wrote by then is replaced, see
//! [`note_pending`]): the session agent reads the files of a fixed list
//! (session_host::migration) in the user's folder and decrypts the keys;
//! the core treats each file as untrusted (a user-level process could send
//! anything), checks its size and format, rebuilds what it can, encrypts
//! the secrets for its own account (win_sys::dpapi's service scope) and
//! writes only files the service's folder does not have yet. The user's
//! files are left as they are.
//!
//! Never moved by it: the keyboard and mouse list and their descriptors,
//! and the feature's on state (a forged list would let a user-level
//! process add a keyboard that types into the sign-in screen); they need
//! the elevated confirmation instead. Neither are the inputs of the
//! elevated helpers (driver records, Windows' replaced keys) nor firmware
//! (code the adapter runs). Headphones' pairing keys are moved: a forged
//! key can at worst let a device connect as headphones.

use std::path::{Path, PathBuf};

use session_host::migration::{Entry, Found, Item, from_hex};

/// The record that the move ran, in the service's folder
pub const MARKER: &str = "migration.json";

/// Whose folder a file belongs to in service mode
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// The service's folder, for the whole PC
    Machine,
    /// The signed-in user's folder (the window's own files)
    User,
}

/// What the one-time move does with a file
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    /// Taken from the user's folder (checked, secrets encrypted again)
    Yes,
    /// Only through the elevated confirmation, never by the move
    NeedsConsent,
    /// Not taken: rebuilt, downloaded again or kept with the user
    No,
}

/// One file or folder of the layout, `/`-separated below the root; a
/// trailing `/` is a folder and everything in it
#[derive(Clone, Copy, Debug)]
pub struct DataFile {
    pub path: &'static str,
    pub place: Place,
    pub moved: Moved,
}

const fn f(path: &'static str, place: Place, moved: Moved) -> DataFile {
    DataFile { path, place, moved }
}

/// Every file of crate::paths' layout, and where it lives in service mode
pub const CLASSIFICATION: &[DataFile] = &[
    // Starts the stack and the keyboard and mouse feature; also holds the
    // window's preferences (one file for now). Moved with the feature off.
    f("settings.json", Place::Machine, Moved::Yes),
    f("profiles.json", Place::Machine, Moved::Yes),
    f("devices/devices.txt", Place::Machine, Moved::Yes),
    f("devices/le_audio_devices.txt", Place::Machine, Moved::Yes),
    f("devices/le_audio_sets.txt", Place::Machine, Moved::Yes),
    f("devices/device_codecs.json", Place::Machine, Moved::Yes),
    f("devices/device_services.json", Place::Machine, Moved::Yes),
    f("devices/hid_devices.txt", Place::Machine, Moved::NeedsConsent),
    f("devices/hid_descriptors/", Place::Machine, Moved::NeedsConsent),
    f("pairing/link_keys.txt", Place::Machine, Moved::Yes),
    f("pairing/le_audio_bonds.tlv", Place::Machine, Moved::Yes),
    f("pairing/le_audio_csip.txt", Place::Machine, Moved::Yes),
    f("pairing/windows_le_keys.txt", Place::Machine, Moved::Yes),
    // A cache, read again at the next connection
    f("pairing/le_gatt_cache.txt", Place::Machine, Moved::No),
    // There only while the elevated helper runs
    f("pairing/keys_for_windows.json", Place::Machine, Moved::No),
    // Written back to Windows by the elevated helper: its input
    f("pairing/replaced_windows_keys.json", Place::Machine, Moved::No),
    // A cache of what the adapters can do
    f("adapters/adapter_caps.json", Place::Machine, Moved::No),
    // The driver helper's and the uninstaller's records: the installer's job
    f("adapters/driver/", Place::Machine, Moved::No),
    // Code the adapter runs: downloaded again
    f("firmware/", Place::Machine, Moved::No),
    // The window's log and crash reports (before the core's, which they
    // would otherwise match)
    f("logs/<day>/ui.log", Place::User, Moved::No),
    f("logs/crash/bluecaster-ui-", Place::User, Moved::No),
    // The core's logs, captures, reports: history stays with the user
    f("logs/", Place::Machine, Moved::No),
    f("hci_", Place::Machine, Moved::No),
    f("backup/", Place::Machine, Moved::No),
    f(MARKER, Place::Machine, Moved::No),
    f(PENDING, Place::Machine, Moved::No),
];

/// The entry of `path` (`/`-separated below the root; a day folder's name
/// as `<day>`)
pub fn classify(path: &str) -> Option<&'static DataFile> {
    let path = normalize_day(path);
    CLASSIFICATION.iter().find(|e| {
        if e.path.ends_with('/') || e.path.ends_with('-') || e.path.ends_with('_') {
            path.starts_with(e.path)
        } else {
            path == e.path
        }
    })
}

/// "logs/2026-10-27/ui.log" -> "logs/<day>/ui.log"
fn normalize_day(path: &str) -> String {
    path.split('/').map(|p| if crate::paths::is_day_name(p) { "<day>" } else { p }).collect::<Vec<_>>().join("/")
}

/// What one run of [`migrate`] did (counts only: for the log)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// It ran before: nothing asked, nothing written
    pub already_done: bool,
    /// Files written into the service's folder
    pub taken: usize,
    /// The service's folder had the file already: kept
    pub kept: usize,
    /// The user had none (or the agent could not read it)
    pub missing: usize,
    /// Too large, malformed, or sent more than once
    pub refused: usize,
    /// Of `taken`: files the service had written itself before the move
    /// (its defaults, before anyone signed in), replaced by the user's
    pub replaced: usize,
}

/// The record, until the move ran, of the files the service's folder had
/// before the core first ran in it ([`note_pending`])
pub const PENDING: &str = "migration_pending.json";

/// Service mode, before the core writes anything: the first time the core
/// runs in `root` with the move still to do, it records which of the
/// move's files the folder has already (left there by an administrator, or
/// an earlier build). Every other file of the list that exists when the
/// move runs was written by the service itself in the meantime (its
/// defaults, while nobody was signed in: the move now runs when the first
/// agent connects, not at the start), and the user's file replaces it.
/// Nothing is recorded once the move ran, or when the record exists.
pub fn note_pending(root: &Path) -> std::io::Result<()> {
    if root.join(MARKER).exists() || root.join(PENDING).exists() {
        return Ok(());
    }
    let there: Vec<&str> = Item::ALL.iter().filter(|i| item_path(root, **i).exists()).map(|i| i.name()).collect();
    let record = serde_json::json!({ "version": 1, "existing": there });
    write(&root.join(PENDING), false, record.to_string().as_bytes())
}

/// The files [`note_pending`] found; None without its record (a folder
/// from before stage 4: every file there is kept)
fn existing_before(root: &Path) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join(PENDING)).ok()?).ok()?;
    Some(v.get("existing")?.as_array()?.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
}

/// The move into `root` (the service's folder), once: `collect` asks the
/// agent for the user's files (session_host::migration::collect). Err: no
/// answer, or a file could not be written; the move is not recorded then
/// and runs again with the next agent (files written already are kept). A
/// file the service wrote itself before the move ([`note_pending`]) is
/// replaced by the user's.
pub fn migrate(root: &Path, collect: impl FnOnce() -> Result<Vec<Entry>, String>) -> Result<Outcome, String> {
    if root.join(MARKER).exists() {
        return Ok(Outcome { already_done: true, ..Outcome::default() });
    }
    let before = existing_before(root);
    let replaceable = |item: Item| before.as_ref().is_some_and(|b| !b.iter().any(|n| n == item.name()));
    let entries = collect()?;
    let mut out = Outcome::default();
    let mut seen = Vec::new();
    for e in entries {
        // One answer per item: a second one is not trusted either
        if seen.contains(&e.item) {
            out.refused += 1;
            continue;
        }
        seen.push(e.item);
        let data = match e.found {
            Found::Data { hex } if hex.len() <= e.item.max_bytes() * 2 => match from_hex(&hex) {
                Some(d) => d,
                None => {
                    out.refused += 1;
                    continue;
                }
            },
            Found::Data { .. } | Found::TooLarge => {
                out.refused += 1;
                continue;
            }
            Found::Missing | Found::Unreadable => {
                out.missing += 1;
                continue;
            }
        };
        let path = item_path(root, e.item);
        let replacing = path.exists();
        if replacing && !replaceable(e.item) {
            out.kept += 1;
            continue;
        }
        let Some(checked) = check(e.item, &data) else {
            tracing::warn!("migration: the {} file was refused (not in its format)", e.item.name());
            out.refused += 1;
            continue;
        };
        write(&path, e.item.secret(), &checked)
            .map_err(|err| format!("cannot write the {} file: {}", e.item.name(), err.kind()))?;
        out.taken += 1;
        out.replaced += usize::from(replacing);
    }
    out.missing += Item::ALL.iter().filter(|i| !seen.contains(i)).count();
    let record = serde_json::json!({
        "version": 1,
        "taken": out.taken,
        "kept": out.kept,
        "refused": out.refused,
    });
    write(&root.join(MARKER), false, record.to_string().as_bytes())
        .map_err(|err| format!("cannot record the move: {}", err.kind()))?;
    let _ = std::fs::remove_file(root.join(PENDING));
    Ok(out)
}

/// The item's file below `root`
pub fn item_path(root: &Path, item: Item) -> PathBuf {
    item.relative_path().split('/').fold(root.to_path_buf(), |p, part| p.join(part))
}

fn write(path: &Path, secret: bool, data: &[u8]) -> std::io::Result<()> {
    if secret {
        return win_sys::dpapi::write_secret(path, data);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The file to write for an item's content from the agent, or None when
/// it is not in the item's format
pub fn check(item: Item, data: &[u8]) -> Option<Vec<u8>> {
    if data.len() > item.max_bytes() {
        return None;
    }
    match item {
        Item::Settings => settings(data),
        Item::Profiles => profiles(data),
        Item::DeviceCodecs | Item::DeviceServices => {
            let v: serde_json::Value = serde_json::from_slice(data).ok()?;
            v.is_object().then(|| data.to_vec())
        }
        Item::Devices | Item::LeDevices | Item::LeSets | Item::LeCsip | Item::WindowsLeKeys => {
            text(data).map(|t| t.as_bytes().to_vec())
        }
        Item::LinkKeys => link_keys(data),
        Item::LeBonds => tlv(data).then(|| data.to_vec()),
    }
}

/// Plain text: UTF-8, lines of a sane length, no control characters
fn text(data: &[u8]) -> Option<&str> {
    const MAX_LINES: usize = 2000;
    const MAX_LINE: usize = 1024;
    let t = std::str::from_utf8(data).ok()?;
    let ok = t.lines().count() <= MAX_LINES
        && t.lines().all(|l| l.len() <= MAX_LINE)
        && t.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'));
    ok.then_some(t)
}

/// settings.json with the keyboard and mouse feature off and the developer
/// experiments that reach the adapter or the network reset: those need the
/// elevated confirmation in the service. Must read as the settings.
fn settings(data: &[u8]) -> Option<Vec<u8>> {
    let mut v: serde_json::Value = serde_json::from_slice(data).ok()?;
    serde_json::from_value::<crate::settings::Settings>(v.clone()).ok()?;
    let o = v.as_object_mut()?;
    o.insert("hid_host".into(), serde_json::Value::Bool(false));
    // Raw adapter commands and register writes, the LE keys handed to the
    // elevated helper
    o.remove("dev_debug");
    // Audio sent to a host on the network
    o.insert("remote_sink".into(), serde_json::Value::String(String::new()));
    serde_json::to_vec_pretty(&v).ok()
}

/// profiles.json: {"profiles": [objects]}
fn profiles(data: &[u8]) -> Option<Vec<u8>> {
    const MAX_PROFILES: usize = 1000;
    let v: serde_json::Value = serde_json::from_slice(data).ok()?;
    let list = v.get("profiles")?.as_array()?;
    (list.len() <= MAX_PROFILES && list.iter().all(serde_json::Value::is_object)).then(|| data.to_vec())
}

/// link_keys.txt rebuilt from its valid lines only ("XX-XX-XX-XX-XX-XX
/// <32 hex digits> <type>", bt_host::link_keys' format); None when a line
/// is neither that, a comment nor empty
fn link_keys(data: &[u8]) -> Option<Vec<u8>> {
    const MAX_KEYS: usize = 32;
    let t = text(data)?;
    let mut out = String::from("# BTstack link key database \u{2014} do not edit while app is running\n");
    let mut keys = 0;
    for line in t.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (addr, key, ty) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || !is_addr(addr) || key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        // HCI link key types 0..=8
        let ty: u8 = ty.parse().ok().filter(|t| *t <= 8)?;
        keys += 1;
        if keys > MAX_KEYS {
            return None;
        }
        out.push_str(&format!("{} {} {ty}\n", addr.to_ascii_uppercase(), key.to_ascii_lowercase()));
    }
    Some(out.into_bytes())
}

fn is_addr(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 17
        && b.iter().enumerate().all(|(i, c)| if i % 3 == 2 { *c == b'-' || *c == b':' } else { c.is_ascii_hexdigit() })
}

/// le_audio_bonds.tlv: "BTstack\0", then entries of a big-endian tag and
/// length and the value, to the last byte (bt_host::le_bond_file)
fn tlv(data: &[u8]) -> bool {
    const MAX_VALUE: usize = 2048;
    if data.len() < 8 || &data[..8] != b"BTstack\0" {
        return false;
    }
    let mut at = 8;
    while at < data.len() {
        if at + 8 > data.len() {
            return false;
        }
        let len = u32::from_be_bytes([data[at + 4], data[at + 5], data[at + 6], data[at + 7]]) as usize;
        at += 8;
        if len > MAX_VALUE || at + len > data.len() {
            return false;
        }
        at += len;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;
    use session_host::migration::to_hex;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> TempDir {
            let d = std::env::temp_dir().join(format!("bluecaster-service-data-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rel(root: &Path, p: &Path) -> String {
        p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn every_file_of_the_layout_has_a_place() {
        let r = Path::new(r"C:\root");
        let day = "2026-11-22";
        let files = [
            paths::settings_file(r),
            paths::profiles_file(r),
            paths::capture_file(r, "20261122_101010"),
            paths::devices_file(r),
            paths::le_devices_file(r),
            paths::le_sets_file(r),
            paths::device_codecs_file(r),
            paths::device_services_file(r),
            crate::hid::devices_file(r),
            crate::hid::descriptors_dir(r).join("x.bin"),
            paths::link_keys_file(r),
            paths::le_bonds_file(r),
            paths::pairing_dir(r).join("le_audio_csip.txt"),
            paths::pairing_dir(r).join(bt_host::le_gatt_cache::FILE_NAME),
            paths::windows_le_keys_file(r),
            paths::keys_for_windows_file(r),
            paths::replaced_windows_keys_file(r),
            paths::adapter_caps_file(r),
            paths::driver_record_file(r, "USB\\VID_1"),
            r.join("firmware").join("rtl8761bu_fw.bin"),
            paths::core_log_file(r, day),
            paths::driver_log_file(r, day),
            paths::verbose_file(r),
            paths::recorder_dir(r).join("hci-1.pklg"),
            paths::crash_dir(r).join("bluecaster-core-1.txt"),
            paths::probe_dir(r).join("p.json"),
            paths::bundles_dir(r).join("b.zip"),
            paths::backup_v1_dir(r).join("settings.json"),
            paths::profiles_backup_file(r),
        ];
        for p in &files {
            let e = classify(&rel(r, p)).unwrap_or_else(|| panic!("{} has no place", rel(r, p)));
            assert_eq!(e.place, Place::Machine, "{}", rel(r, p));
        }
        // The window's own files stay with the user
        assert_eq!(classify(&rel(r, &paths::ui_log_file(r, day))).unwrap().place, Place::User);
        assert_eq!(classify("logs/crash/bluecaster-ui-20261122.txt").unwrap().place, Place::User);
        assert_eq!(classify("logs/crash/bluecaster-core-20261122.txt").unwrap().place, Place::Machine);
        // The keyboard and mouse files need the elevated confirmation
        for p in [crate::hid::devices_file(r), crate::hid::descriptors_dir(r).join("x.bin")] {
            assert_eq!(classify(&rel(r, &p)).unwrap().moved, Moved::NeedsConsent);
        }
        assert!(classify("other.txt").is_none());
    }

    #[test]
    fn the_move_takes_exactly_the_agents_list() {
        let moved: Vec<&str> = CLASSIFICATION.iter().filter(|e| e.moved == Moved::Yes).map(|e| e.path).collect();
        let list: Vec<&str> = Item::ALL.iter().map(|i| i.relative_path()).collect();
        assert_eq!(moved, list);
        // The same places as crate::paths
        let r = Path::new(r"C:\root");
        assert_eq!(item_path(r, Item::Settings), paths::settings_file(r));
        assert_eq!(item_path(r, Item::Profiles), paths::profiles_file(r));
        assert_eq!(item_path(r, Item::Devices), paths::devices_file(r));
        assert_eq!(item_path(r, Item::LeDevices), paths::le_devices_file(r));
        assert_eq!(item_path(r, Item::LeSets), paths::le_sets_file(r));
        assert_eq!(item_path(r, Item::DeviceCodecs), paths::device_codecs_file(r));
        assert_eq!(item_path(r, Item::DeviceServices), paths::device_services_file(r));
        assert_eq!(item_path(r, Item::LinkKeys), paths::link_keys_file(r));
        assert_eq!(item_path(r, Item::LeBonds), paths::le_bonds_file(r));
        assert_eq!(item_path(r, Item::LeCsip), paths::pairing_dir(r).join("le_audio_csip.txt"));
        assert_eq!(item_path(r, Item::WindowsLeKeys), paths::windows_le_keys_file(r));
        // Every secret file is one bt_host encrypts
        for i in Item::ALL {
            assert_eq!(i.secret(), i.relative_path().starts_with("pairing/"), "{i:?}");
        }
    }

    fn data(item: Item, bytes: &[u8]) -> Entry {
        Entry { item, found: Found::Data { hex: to_hex(bytes) } }
    }

    const KEY_LINE: &str = "00-A2-D0-00-00-02 00112233445566778899aabbccddeeff 4\n";

    fn sample() -> Vec<Entry> {
        vec![
            data(Item::Settings, br#"{"language":"de","hid_host":true,"debug_mode":true,"remote_sink":"10.0.0.1","dev_debug":{"mtk_command":"FD98 08 01"}}"#),
            data(Item::Profiles, br#"{"profiles":[{"name":"p1"}]}"#),
            data(Item::Devices, b"00:A2:D0:00:00:02|Headphones\n"),
            data(Item::LinkKeys, KEY_LINE.as_bytes()),
            data(Item::LeBonds, b"BTstack\0\0BTD\0\0\0\x02ab"),
            Entry { item: Item::LeCsip, found: Found::Missing },
        ]
    }

    #[test]
    fn the_move_checks_encrypts_and_records() {
        let t = TempDir::new("move");
        let root = &t.0;
        let out = migrate(root, || Ok(sample())).unwrap();
        assert_eq!(out.taken, 5);
        assert_eq!(out.refused, 0);
        assert_eq!(out.missing, Item::ALL.len() - 5);
        // The feature is off and the experiments gone; the rest kept
        let s: serde_json::Value = serde_json::from_slice(&std::fs::read(paths::settings_file(root)).unwrap()).unwrap();
        assert_eq!(s["hid_host"], false);
        assert_eq!(s["debug_mode"], true);
        assert_eq!(s["language"], "de");
        assert_eq!(s["remote_sink"], "");
        assert!(s.get("dev_debug").is_none());
        // Secrets encrypted for this process's scope, readable by the stack
        let on_disk = std::fs::read(paths::link_keys_file(root)).unwrap();
        assert!(win_sys::dpapi::is_encrypted(&on_disk));
        let keys = bt_host::link_keys::FileLinkKeyStore::open(&paths::link_keys_file(root));
        assert_eq!(keys.entries().len(), 1);
        assert!(win_sys::dpapi::is_encrypted(&std::fs::read(paths::le_bonds_file(root)).unwrap()));
        assert!(root.join(MARKER).is_file());
        // Never twice: the agent is not even asked
        let again = migrate(root, || panic!("asked again")).unwrap();
        assert!(again.already_done);
    }

    #[test]
    fn existing_service_data_is_not_overwritten() {
        let t = TempDir::new("keep");
        let root = &t.0;
        std::fs::write(paths::settings_file(root), r#"{"language":"fr"}"#).unwrap();
        let out = migrate(root, || Ok(sample())).unwrap();
        assert_eq!((out.taken, out.kept), (4, 1));
        assert_eq!(std::fs::read_to_string(paths::settings_file(root)).unwrap(), r#"{"language":"fr"}"#);
    }

    /// The service ran before anyone signed in and wrote its defaults: the
    /// user's files replace them; a file that was there before the service
    /// first ran is kept
    #[test]
    fn defaults_the_service_wrote_itself_are_replaced() {
        let t = TempDir::new("defaults");
        let root = &t.0;
        std::fs::create_dir_all(root.join("devices")).unwrap();
        std::fs::write(paths::devices_file(root), "00:A2:D0:00:00:07|Admin's\n").unwrap();
        note_pending(root).unwrap();
        assert!(root.join(PENDING).is_file());
        // The core's defaults, written after the record
        std::fs::write(paths::settings_file(root), r#"{"language":"en"}"#).unwrap();
        // A second start does not record again (the defaults stay replaceable)
        note_pending(root).unwrap();
        let out = migrate(root, || Ok(sample())).unwrap();
        assert_eq!((out.taken, out.kept, out.replaced), (4, 1, 1));
        let s: serde_json::Value = serde_json::from_slice(&std::fs::read(paths::settings_file(root)).unwrap()).unwrap();
        assert_eq!(s["language"], "de", "the user's settings replaced the defaults");
        assert!(std::fs::read_to_string(paths::devices_file(root)).unwrap().contains("Admin's"));
        assert!(!root.join(PENDING).exists(), "the record goes with the move");
        // Once moved, nothing is recorded again
        note_pending(root).unwrap();
        assert!(!root.join(PENDING).exists());
    }

    #[test]
    fn no_answer_records_nothing_and_runs_again() {
        let t = TempDir::new("noagent");
        let root = &t.0;
        assert!(migrate(root, || Err("no agent".into())).is_err());
        assert!(!root.join(MARKER).exists());
        assert_eq!(migrate(root, || Ok(sample())).unwrap().taken, 5);
    }

    #[test]
    fn malformed_oversized_and_repeated_entries_are_refused() {
        let t = TempDir::new("bad");
        let root = &t.0;
        let big = vec![b'a'; Item::Devices.max_bytes() + 1];
        let entries = vec![
            // Not the settings (a type that does not fit)
            data(Item::Settings, br#"{"language": 5}"#),
            // Not JSON
            data(Item::Profiles, b"\xff\xfe"),
            // Larger than the item may be
            data(Item::Devices, &big),
            Entry { item: Item::LeDevices, found: Found::TooLarge },
            // Not hex
            Entry { item: Item::LeSets, found: Found::Data { hex: "zz".into() } },
            // A key line that is not one
            data(Item::LinkKeys, b"00-A2-D0-00-00-02 short 4\n"),
            // A TLV cut short
            data(Item::LeBonds, b"BTstack\0\0BTD\0\0\0\x09ab"),
            // Control characters
            data(Item::LeCsip, b"a\x1b[2Jb\n"),
            // A good one, then the same item again with other content
            data(Item::WindowsLeKeys, b"ok\n"),
            data(Item::WindowsLeKeys, b"second\n"),
        ];
        let out = migrate(root, || Ok(entries)).unwrap();
        assert_eq!(out.taken, 1);
        assert_eq!(out.refused, 9);
        for item in Item::ALL.iter().filter(|i| **i != Item::WindowsLeKeys) {
            assert!(!item_path(root, *item).exists(), "{item:?}");
        }
        assert_eq!(win_sys::dpapi::read_secret(&paths::windows_le_keys_file(root)).unwrap(), b"ok\n");
        assert!(root.join(MARKER).is_file());
    }

    #[test]
    fn the_checks_one_by_one() {
        // Link keys rebuilt in the stack's format, comments dropped
        let keys = format!("# header\n\n{}", KEY_LINE.to_lowercase());
        assert_eq!(
            String::from_utf8(check(Item::LinkKeys, keys.as_bytes()).unwrap()).unwrap().lines().nth(1).unwrap(),
            KEY_LINE.trim()
        );
        assert!(check(Item::LinkKeys, b"00-A2-D0-00-00-02 00112233445566778899aabbccddeeff 9\n").is_none());
        assert!(check(Item::LinkKeys, b"00-A2-D0-00-00-02 00112233445566778899aabbccddeeff 4 extra\n").is_none());
        let many: String =
            (0..33).map(|i| format!("00-A2-D0-00-00-{i:02X} 00112233445566778899aabbccddeeff 4\n")).collect();
        assert!(check(Item::LinkKeys, many.as_bytes()).is_none());
        // TLV: empty is fine, a header only too, trailing bytes not
        assert!(check(Item::LeBonds, b"BTstack\0").is_some());
        assert!(check(Item::LeBonds, b"BTstack\0\x01").is_none());
        assert!(check(Item::LeBonds, b"Other\0\0\0").is_none());
        // JSON objects only
        assert!(check(Item::DeviceCodecs, b"[]").is_none());
        assert!(check(Item::DeviceCodecs, b"{}").is_some());
        assert!(check(Item::Profiles, br#"{"profiles":[1]}"#).is_none());
        assert!(check(Item::Settings, b"[]").is_none());
        // Long lines
        let long = vec![b'a'; 1025];
        assert!(check(Item::Devices, &long).is_none());
    }
}
