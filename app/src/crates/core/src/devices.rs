//! Saved devices, in v1's files: devices.txt for the classic (A2DP) ones
//! and le_audio_devices.txt for the experimental LE Audio ones, one
//! "XX:XX:XX:XX:XX:XX|name" per line. Older versions read devices.txt only,
//! and would show an LE Audio entry there as one more classic device, so LE
//! Audio devices have a file of their own (none while there are none). A
//! dual-mode device can have a classic and an LE Audio entry.
//!
//! An early LE Audio prototype wrote "addr|name|le" lines into devices.txt:
//! they are read as LE Audio entries and moved to le_audio_devices.txt the
//! next time the files are written (as v1 did).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use crate::paths;
use crate::stream::KnownCodecs;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SavedDevice {
    pub address: String,
    pub name: String,
    /// An LE Audio (experimental) entry
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub le_audio: bool,
}

/// The suffix of the prototype's LE Audio lines in devices.txt
const LEGACY_LE_SUFFIX: &str = "|le";

/// One "addr|name" line (v1 parse_saved_device_line); the address in upper
/// case
fn parse_line(line: &str) -> Option<(String, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (addr, name) = line.split_once('|')?;
    bt_host::parse_addr(addr)?;
    Some((addr.to_uppercase(), name))
}

fn read_lines(file: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(file) else {
        tracing::trace!("devices: no {}", file.display());
        return Vec::new();
    };
    let lines: Vec<(String, String)> = text.lines().filter_map(parse_line).map(|(a, n)| (a, n.to_string())).collect();
    tracing::trace!("devices: {}: {} device(s) of {} line(s)", file.display(), lines.len(), text.lines().count());
    lines
}

/// Both files: the classic devices, then the LE Audio ones (le_audio set;
/// the prototype's "|le" lines of devices.txt first, an address listed in
/// both places once)
pub fn load_all(dir: &Path) -> Vec<SavedDevice> {
    let mut classic = Vec::new();
    let mut le: Vec<SavedDevice> = Vec::new();
    for (address, name) in read_lines(&paths::devices_file(dir)) {
        match name.strip_suffix(LEGACY_LE_SUFFIX) {
            Some(name) => le.push(SavedDevice { address, name: name.to_string(), le_audio: true }),
            None => classic.push(SavedDevice { address, name, le_audio: false }),
        }
    }
    for (address, name) in read_lines(&paths::le_devices_file(dir)) {
        if !le.iter().any(|d| d.address == address) {
            le.push(SavedDevice { address, name, le_audio: true });
        }
    }
    classic.extend(le);
    classic
}

/// Writes both files (v1 write_saved_devices): devices.txt with the classic
/// devices only, le_audio_devices.txt with the LE Audio ones, deleted when
/// there are none
fn write_all(dir: &Path, devices: &[SavedDevice]) -> std::io::Result<()> {
    let lines = |le: bool| -> String {
        devices.iter().filter(|d| d.le_audio == le).map(|d| format!("{}|{}\n", d.address, d.name)).collect()
    };
    std::fs::create_dir_all(paths::devices_dir(dir))?;
    std::fs::write(paths::devices_file(dir), lines(false))?;
    let le = lines(true);
    let le_path = paths::le_devices_file(dir);
    if le.is_empty() {
        match std::fs::remove_file(&le_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::write(le_path, le)
    }
}

/// The classic (A2DP) devices
pub fn load(dir: &Path) -> Vec<SavedDevice> {
    load_all(dir).into_iter().filter(|d| !d.le_audio).collect()
}

/// The LE Audio (experimental) devices, incl. the prototype's "|le" lines
pub fn load_le(dir: &Path) -> Vec<SavedDevice> {
    load_all(dir).into_iter().filter(|d| d.le_audio).collect()
}

fn save_kind(dir: &Path, address: &str, name: &str, le_audio: bool) -> std::io::Result<()> {
    let mut devices = load_all(dir);
    let address = address.to_uppercase();
    // Another earbud of a saved pair: the same device
    let set_member = le_audio
        && !devices.iter().any(|d| d.address == address && d.le_audio)
        && saved_set_member(dir, &devices, &address);
    match devices.iter_mut().find(|d| d.address == address && d.le_audio == le_audio) {
        Some(d) => {
            if !name.is_empty() {
                d.name = name.to_string();
            }
        }
        None if set_member => {
            tracing::debug!("devices: not adding a member of a saved LE Audio set");
            return Ok(());
        }
        None => {
            tracing::debug!("devices: adding {address} \"{name}\" (LE Audio {le_audio})");
            devices.push(SavedDevice { address, name: name.to_string(), le_audio });
        }
    }
    let r = write_all(dir, &devices);
    tracing::debug!("devices: saved {} device(s) to {}: {r:?}", devices.len(), dir.display());
    r
}

/// `address` is a member of a set learned (crate::le_sets) whose other
/// member is saved as an LE Audio device
fn saved_set_member(dir: &Path, devices: &[SavedDevice], address: &str) -> bool {
    let sets = crate::le_sets::KnownSets::of(crate::le_sets::load(dir));
    let saved_le: Vec<SavedDevice> = devices.iter().filter(|d| d.le_audio).cloned().collect();
    sets.saved_entry(address, &saved_le).is_some()
}

fn remove_kind(dir: &Path, address: &str, le_audio: bool) -> std::io::Result<()> {
    let mut devices = load_all(dir);
    let before = devices.len();
    devices.retain(|d| !(d.le_audio == le_audio && d.address.eq_ignore_ascii_case(address)));
    let r = write_all(dir, &devices);
    tracing::debug!("devices: removing {address} (LE Audio {le_audio}): {} removed, {r:?}", before - devices.len());
    r
}

/// Adds or renames a classic device (an empty name keeps the known one)
pub fn save(dir: &Path, address: &str, name: &str) -> std::io::Result<()> {
    save_kind(dir, address, name, false)
}

/// Forgets a classic device (its LE Audio entry stays)
pub fn remove(dir: &Path, address: &str) -> std::io::Result<()> {
    remove_kind(dir, address, false)
}

/// Adds or renames an LE Audio device (an empty name keeps the known one)
pub fn save_le(dir: &Path, address: &str, name: &str) -> std::io::Result<()> {
    save_kind(dir, address, name, true)
}

/// Forgets an LE Audio device (its classic entry stays)
pub fn remove_le(dir: &Path, address: &str) -> std::io::Result<()> {
    remove_kind(dir, address, true)
}

/// The LE Audio devices whose LE pairing gave a classic link key (cross-
/// transport key derivation, stored under the public address), without a
/// classic entry yet: their classic side saved under the same name, so
/// with LE Audio off they are listed and connect over classic without
/// pairing again. Returns how many were saved.
pub fn save_classic_sides(dir: &Path, keys: &[bt_host::link_keys::LinkKeyEntry]) -> usize {
    let all = load_all(dir);
    let mut n = 0;
    for d in all.iter().filter(|d| d.le_audio) {
        let classic_saved = all.iter().any(|c| !c.le_audio && c.address.eq_ignore_ascii_case(&d.address));
        let keyed = bt_host::parse_addr(&d.address).is_some_and(|a| keys.iter().any(|k| k.addr == a));
        if classic_saved || !keyed {
            continue;
        }
        match save(dir, &d.address, &d.name) {
            Ok(()) => n += 1,
            Err(e) => tracing::warn!("devices: saving a classic side: {e}"),
        }
    }
    if n > 0 {
        tracing::info!("devices: {n} LE Audio device(s) also saved as classic (link key from the LE pairing)");
    }
    n
}

/// What each classic device offered when it last connected, by upper-case
/// address: kept so the Codecs screen knows it after a restart (issue #86)
pub fn load_codecs(dir: &Path) -> BTreeMap<String, KnownCodecs> {
    std::fs::read_to_string(paths::device_codecs_file(dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_codecs(dir: &Path, codecs: &BTreeMap<String, KnownCodecs>) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(codecs).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(paths::devices_dir(dir))?;
    std::fs::write(paths::device_codecs_file(dir), text)
}

/// The services each classic device listed in SDP at its last connection,
/// by upper-case address (device_services.json): what its Services tag
/// shows while it is not connected (issue #86)
type KeptServices = BTreeMap<String, Vec<ipc::DeviceService>>;

/// One reader or writer of device_services.json at a time (the dialog's
/// requests and the query after each connection run on threads of their
/// own)
static SERVICES_FILE: Mutex<()> = Mutex::new(());

fn load_services_file(dir: &Path) -> KeptServices {
    std::fs::read_to_string(paths::device_services_file(dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Temp file + rename: a quit or crash while writing never leaves a cut
/// file (one that does not parse would lose every device's list)
fn write_services_file(dir: &Path, kept: &KeptServices) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(kept).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(paths::devices_dir(dir))?;
    let path = paths::device_services_file(dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// What a device's kept list becomes with a new answer; None: it stays as
/// it is. An empty answer never replaces a kept list (a device that listed
/// nothing this time still has its services).
fn merged_services(kept: Option<&[ipc::DeviceService]>, new: &[ipc::DeviceService]) -> Option<Vec<ipc::DeviceService>> {
    match kept {
        Some(k) if k == new => None,
        Some(k) if new.is_empty() && !k.is_empty() => None,
        _ => Some(new.to_vec()),
    }
}

/// The services a classic device listed at its last connection
pub fn last_services(dir: &Path, address: &str) -> Option<Vec<ipc::DeviceService>> {
    let _one = SERVICES_FILE.lock().unwrap_or_else(|e| e.into_inner());
    load_services_file(dir).remove(&address.to_uppercase())
}

/// Keeps what a classic device listed (see [`merged_services`]); true when
/// the file changed
pub fn keep_services(dir: &Path, address: &str, services: &[ipc::DeviceService]) -> bool {
    let _one = SERVICES_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut kept = load_services_file(dir);
    let key = address.to_uppercase();
    let Some(list) = merged_services(kept.get(&key).map(Vec::as_slice), services) else { return false };
    kept.insert(key, list);
    match write_services_file(dir, &kept) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("cannot keep the devices' services: {e}");
            false
        }
    }
}

/// A forgotten classic device's services go with it
pub fn forget_services(dir: &Path, address: &str) {
    let _one = SERVICES_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut kept = load_services_file(dir);
    if kept.remove(&address.to_uppercase()).is_some()
        && let Err(e) = write_services_file(dir, &kept)
    {
        tracing::warn!("cannot keep the devices' services: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bluecaster-dev-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(paths::devices_dir(&dir)).unwrap();
        dir
    }

    #[test]
    fn an_le_device_with_a_classic_key_gets_its_classic_side() {
        let dir = temp_dir("classic-side");
        save_le(&dir, "00:A2:D0:00:00:70", "WH-CH530").unwrap();
        save_le(&dir, "00:A2:D0:00:00:71", "No key").unwrap();
        save_le(&dir, "00:A2:D0:00:00:72", "Both").unwrap();
        save(&dir, "00:A2:D0:00:00:72", "Both").unwrap();
        let key = |a: &str| bt_host::link_keys::LinkKeyEntry {
            addr: bt_host::parse_addr(a).unwrap(),
            key: [0; 16],
            key_type: 7,
        };
        let keys = [key("00:A2:D0:00:00:70"), key("00:A2:D0:00:00:72")];
        assert_eq!(save_classic_sides(&dir, &keys), 1);
        let classic: Vec<_> = load(&dir).into_iter().map(|d| (d.address, d.name)).collect();
        assert!(classic.contains(&("00:A2:D0:00:00:70".into(), "WH-CH530".into())), "{classic:?}");
        assert!(!classic.iter().any(|(a, _)| a == "00:A2:D0:00:00:71"));
        // Done once
        assert_eq!(save_classic_sides(&dir, &keys), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_load_remove() {
        let dir = temp_dir("classic");
        std::fs::write(paths::devices_file(&dir), "00:11:22:33:44:55|Old\nbad line\n").unwrap();
        save(&dir, "aa:bb:cc:dd:ee:ff", "Phones").unwrap();
        save(&dir, "00:11:22:33:44:55", "").unwrap();
        let d = load(&dir);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].name, "Old");
        assert_eq!(d[1].address, "AA:BB:CC:DD:EE:FF");
        assert!(!d[1].le_audio);
        remove(&dir, "00:11:22:33:44:55").unwrap();
        assert_eq!(load(&dir).len(), 1);
        // No LE Audio device: no file of them
        assert!(!paths::le_devices_file(&dir).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codecs_survive_a_restart() {
        let dir = temp_dir("codecs");
        assert!(load_codecs(&dir).is_empty());
        let mut map = BTreeMap::new();
        map.insert(
            "AA:BB:CC:DD:EE:FF".to_string(),
            KnownCodecs { keys: vec!["sbc".into(), "aac".into()], ..Default::default() },
        );
        save_codecs(&dir, &map).unwrap();
        assert_eq!(load_codecs(&dir), map);
        // A damaged file is as none
        std::fs::write(paths::device_codecs_file(&dir), "{ not json").unwrap();
        assert!(load_codecs(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn service(uuid: &str, name: Option<&str>) -> ipc::DeviceService {
        ipc::DeviceService { uuids: vec![uuid.into()], name: name.map(Into::into) }
    }

    #[test]
    fn an_empty_answer_never_replaces_kept_services() {
        let good = vec![service("0000110b-0000-1000-8000-00805f9b34fb", Some("Audio Sink"))];
        let other = vec![service("0000111e-0000-1000-8000-00805f9b34fb", None)];
        assert_eq!(merged_services(None, &good), Some(good.clone()));
        assert_eq!(merged_services(Some(&good), &good), None);
        assert_eq!(merged_services(Some(&good), &[]), None);
        assert_eq!(merged_services(Some(&good), &other), Some(other.clone()));
        // Nothing kept yet: an empty answer is what is known
        assert_eq!(merged_services(None, &[]), Some(vec![]));
        assert_eq!(merged_services(Some(&[]), &good), Some(good));
    }

    #[test]
    fn services_survive_a_restart_and_go_with_the_device() {
        let dir = temp_dir("services");
        let a = vec![service("0000110b-0000-1000-8000-00805f9b34fb", Some("Audio Sink"))];
        let b = vec![service("0000111e-0000-1000-8000-00805f9b34fb", None)];
        assert!(keep_services(&dir, "aa:bb:cc:dd:ee:0f", &a));
        assert!(keep_services(&dir, "AA:BB:CC:DD:EE:10", &b));
        // The same answer again, or an empty one: nothing written
        assert!(!keep_services(&dir, "AA:BB:CC:DD:EE:0F", &a));
        assert!(!keep_services(&dir, "AA:BB:CC:DD:EE:0F", &[]));
        assert_eq!(last_services(&dir, "AA:BB:CC:DD:EE:0F"), Some(a.clone()));
        // No temp file left behind
        assert!(!paths::device_services_file(&dir).with_extension("json.tmp").exists());
        forget_services(&dir, "aa:bb:cc:dd:ee:0f");
        assert_eq!(last_services(&dir, "AA:BB:CC:DD:EE:0F"), None);
        assert_eq!(last_services(&dir, "aa:bb:cc:dd:ee:10"), Some(b));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn le_audio_devices_have_their_own_file() {
        let dir = temp_dir("le");
        std::fs::write(paths::le_devices_file(&dir), "00:A2:D0:00:00:12|Buds\r\nnot an address|x\n").unwrap();
        save(&dir, "00:a2:d0:00:00:12", "Buds classic").unwrap();
        save_le(&dir, "00:a2:d0:00:00:13", "Buds R").unwrap();
        save_le(&dir, "00:A2:D0:00:00:12", "").unwrap();

        // The dual-mode device has two entries
        let classic = load(&dir);
        let le = load_le(&dir);
        assert_eq!(classic.len(), 1);
        assert_eq!((classic[0].name.as_str(), classic[0].le_audio), ("Buds classic", false));
        let names: Vec<_> = le.iter().map(|d| (d.address.as_str(), d.name.as_str(), d.le_audio)).collect();
        assert_eq!(names, [("00:A2:D0:00:00:12", "Buds", true), ("00:A2:D0:00:00:13", "Buds R", true)]);
        assert_eq!(std::fs::read_to_string(paths::devices_file(&dir)).unwrap(), "00:A2:D0:00:00:12|Buds classic\n");
        assert_eq!(
            std::fs::read_to_string(paths::le_devices_file(&dir)).unwrap(),
            "00:A2:D0:00:00:12|Buds\n00:A2:D0:00:00:13|Buds R\n"
        );

        // Forgetting one kind keeps the other
        remove_le(&dir, "00:a2:d0:00:00:12").unwrap();
        assert_eq!(load(&dir).len(), 1);
        assert_eq!(load_le(&dir).len(), 1);
        remove(&dir, "00:A2:D0:00:00:12").unwrap();
        remove_le(&dir, "00:A2:D0:00:00:13").unwrap();
        assert!(load(&dir).is_empty() && load_le(&dir).is_empty());
        assert!(!paths::le_devices_file(&dir).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_other_earbud_of_a_saved_set_is_not_saved_again() {
        let dir = temp_dir("set");
        save_le(&dir, "C0:00:00:00:00:01", "Buds").unwrap();
        // Not known as one set yet: two entries
        save_le(&dir, "C0:00:00:00:00:03", "Other").unwrap();
        crate::le_sets::remember(&dir, &["C0:00:00:00:00:01".into(), "C0:00:00:00:00:02".into()]).unwrap();
        save_le(&dir, "c0:00:00:00:00:02", "Buds").unwrap();
        let le: Vec<_> = load_le(&dir).into_iter().map(|d| d.address).collect();
        assert_eq!(le, ["C0:00:00:00:00:01", "C0:00:00:00:00:03"]);
        // A classic entry of that address is another device
        save(&dir, "C0:00:00:00:00:02", "Buds").unwrap();
        assert_eq!(load(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prototype_le_lines_move_to_the_le_file() {
        let dir = temp_dir("legacy");
        std::fs::write(paths::devices_file(&dir), "00:11:22:33:44:55|Phones\n00:A2:D0:00:00:12|Buds|le\n").unwrap();
        // Also listed in the LE file: one entry, the prototype's line first
        std::fs::write(paths::le_devices_file(&dir), "00:A2:D0:00:00:12|Buds (new)\n").unwrap();
        assert_eq!(load(&dir).len(), 1);
        let le = load_le(&dir);
        assert_eq!(le.len(), 1);
        assert_eq!((le[0].name.as_str(), le[0].le_audio), ("Buds", true));

        // The next write moves it out of devices.txt, which older versions
        // read as a classic device
        save(&dir, "00:11:22:33:44:55", "").unwrap();
        assert_eq!(std::fs::read_to_string(paths::devices_file(&dir)).unwrap(), "00:11:22:33:44:55|Phones\n");
        assert_eq!(std::fs::read_to_string(paths::le_devices_file(&dir)).unwrap(), "00:A2:D0:00:00:12|Buds\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
