//! Coordinated sets (CSIP) of the saved LE Audio devices. True-wireless
//! earbuds are two LE devices, one per ear, each with an address of its
//! own; connecting either one streams to both. They are one saved device:
//! every member's address stands for the same entry of
//! le_audio_devices.txt.
//!
//! le_audio_sets.txt (devices folder) keeps the sets learned once a
//! connection read the set's key: one set per line, its members' identity
//! addresses separated by spaces. Only addresses are kept here; the key
//! (SIRK) stays with the bonds, encrypted (bt_host's le_audio_csip.txt).

use std::collections::BTreeMap;
use std::path::Path;

use crate::devices::SavedDevice;
use crate::paths;
use crate::profiles::Profile;

/// The sets known, from the file, the running stack and the bonds: each a
/// list of member addresses (upper case); sets that share an address are
/// one
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownSets {
    groups: Vec<Vec<String>>,
    /// The stack's set index (`LeScanResult::set_id`) and one member
    ids: Vec<(u32, String)>,
}

impl KnownSets {
    /// From the lines of le_audio_sets.txt
    pub fn of(groups: Vec<Vec<String>>) -> KnownSets {
        let mut k = KnownSets::default();
        for g in groups {
            k.add(g, None);
        }
        k
    }

    /// Adds a set's members; `set_id`: the running stack's index of it
    pub fn add(&mut self, members: impl IntoIterator<Item = String>, set_id: Option<u32>) {
        let mut merged: Vec<String> = Vec::new();
        for a in members.into_iter().map(|a| a.to_uppercase()) {
            if !merged.contains(&a) {
                merged.push(a);
            }
        }
        let Some(first) = merged.first().cloned() else { return };
        // A set seen before (it shares a member) grows; earlier members
        // first
        let mut kept: Vec<Vec<String>> = Vec::new();
        let mut joined: Vec<String> = Vec::new();
        for g in std::mem::take(&mut self.groups) {
            if g.iter().any(|a| merged.contains(a)) {
                for a in g {
                    if !joined.contains(&a) {
                        joined.push(a);
                    }
                }
            } else {
                kept.push(g);
            }
        }
        for a in merged {
            if !joined.contains(&a) {
                joined.push(a);
            }
        }
        kept.push(joined);
        self.groups = kept;
        if let Some(id) = set_id {
            self.ids.retain(|(i, _)| *i != id);
            self.ids.push((id, first));
        }
    }

    pub fn groups(&self) -> &[Vec<String>] {
        &self.groups
    }

    /// The members of `address`'s set (itself included)
    pub fn group_of(&self, address: &str) -> Option<&[String]> {
        self.groups.iter().find(|g| g.iter().any(|a| a.eq_ignore_ascii_case(address))).map(Vec::as_slice)
    }

    /// The members of the set the running stack calls `set_id`
    pub fn group_of_set(&self, set_id: u32) -> Option<&[String]> {
        let (_, member) = self.ids.iter().find(|(i, _)| *i == set_id)?;
        self.group_of(member)
    }

    /// The saved entry that stands for `address`: its own, else the first
    /// saved member of its set (in the order the devices were saved)
    pub fn saved_entry<'a>(&self, address: &str, saved_le: &'a [SavedDevice]) -> Option<&'a SavedDevice> {
        if let Some(d) = saved_le.iter().find(|d| d.address.eq_ignore_ascii_case(address)) {
            return Some(d);
        }
        let group = self.group_of(address)?;
        saved_le.iter().find(|d| group.iter().any(|a| a.eq_ignore_ascii_case(&d.address)))
    }
}

/// The sets of le_audio_sets.txt
pub fn load(dir: &Path) -> Vec<Vec<String>> {
    let Ok(text) = std::fs::read_to_string(paths::le_sets_file(dir)) else { return Vec::new() };
    text.lines()
        .map(|l| {
            l.split_whitespace()
                .filter(|a| bt_host::parse_addr(a).is_some())
                .map(str::to_uppercase)
                .collect::<Vec<String>>()
        })
        .filter(|g| g.len() > 1)
        .collect()
}

fn write(dir: &Path, groups: &[Vec<String>]) -> std::io::Result<()> {
    let text: String = groups.iter().filter(|g| g.len() > 1).map(|g| format!("{}\n", g.join(" "))).collect();
    let path = paths::le_sets_file(dir);
    if text.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(paths::devices_dir(dir))?;
    std::fs::write(path, text)
}

/// Keeps a set learned from a connection (one member alone is no set);
/// true when the file changed
pub fn remember(dir: &Path, members: &[String]) -> std::io::Result<bool> {
    if members.len() < 2 {
        return Ok(false);
    }
    let before = load(dir);
    let mut known = KnownSets::of(before.clone());
    known.add(members.iter().cloned(), None);
    if known.groups() == before.as_slice() {
        return Ok(false);
    }
    write(dir, known.groups())?;
    Ok(true)
}

/// Forgets `address`'s set; its members (none when it is in no set)
pub fn forget(dir: &Path, address: &str) -> Vec<String> {
    let mut groups = load(dir);
    let Some(i) = groups.iter().position(|g| g.iter().any(|a| a.eq_ignore_ascii_case(address))) else {
        return Vec::new();
    };
    let members = groups.remove(i);
    if let Err(e) = write(dir, &groups) {
        tracing::warn!("devices: cannot write the LE Audio sets: {e}");
    }
    members
}

/// Two saved entries of one set: `drop` goes, `keep` stays and takes its
/// profiles
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    pub keep: String,
    pub drop: String,
}

/// The saved LE Audio entries that are members of one set, as merges into
/// one entry per set: the entry with the user's settings (`has_settings`:
/// a profile or a saved volume) stays, else the one saved first
pub fn merges(saved_le: &[SavedDevice], sets: &KnownSets, has_settings: impl Fn(&str) -> bool) -> Vec<Merge> {
    let mut out = Vec::new();
    for group in sets.groups() {
        let members: Vec<&SavedDevice> =
            saved_le.iter().filter(|d| group.iter().any(|a| a.eq_ignore_ascii_case(&d.address))).collect();
        if members.len() < 2 {
            continue;
        }
        let keep = members.iter().find(|d| has_settings(&d.address)).unwrap_or(&members[0]);
        for d in members.iter().filter(|d| d.address != keep.address) {
            out.push(Merge { keep: keep.address.clone(), drop: d.address.clone() });
        }
    }
    out
}

/// The LE Audio profiles of `merges`' dropped entries move to the entries
/// kept (under the kept entry's name when it has one); true if any did
pub fn move_profiles(profiles: &mut [Profile], merges: &[Merge], saved_le: &[SavedDevice]) -> bool {
    let mut moved = false;
    for p in profiles.iter_mut().filter(|p| p.le_audio) {
        let Some(m) = merges.iter().find(|m| m.drop.eq_ignore_ascii_case(&p.device_address)) else { continue };
        p.device_address = m.keep.clone();
        if let Some(d) = saved_le.iter().find(|d| d.address == m.keep).filter(|d| !d.name.is_empty()) {
            p.device_name = d.name.clone();
        }
        moved = true;
    }
    moved
}

/// The saved volumes of `merges`' dropped entries: moved to the entry kept
/// when it has none, else dropped; true if any changed
pub fn move_volumes(volumes: &mut BTreeMap<String, i32>, merges: &[Merge]) -> bool {
    let mut changed = false;
    for m in merges {
        let Some(v) = volumes.remove(&m.drop.to_uppercase()) else { continue };
        volumes.entry(m.keep.to_uppercase()).or_insert(v);
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bluecaster-sets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(paths::devices_dir(&dir)).unwrap();
        dir
    }

    fn saved(a: &str, name: &str) -> SavedDevice {
        SavedDevice { address: a.into(), name: name.into(), le_audio: true }
    }

    const L: &str = "C0:00:00:00:00:01";
    const R: &str = "C0:00:00:00:00:02";
    const OTHER: &str = "C0:00:00:00:00:03";

    #[test]
    fn sets_sharing_a_member_are_one() {
        let mut k = KnownSets::default();
        k.add([L.to_lowercase()], Some(4));
        k.add([R.into(), OTHER.into()], None);
        assert_eq!(k.groups().len(), 2);
        k.add([L.into(), R.into()], None);
        assert_eq!(k.groups(), &[vec![L.to_string(), R.into(), OTHER.into()]]);
        assert_eq!(k.group_of(&R.to_lowercase()).map(<[String]>::len), Some(3));
        assert_eq!(k.group_of_set(4).map(<[String]>::len), Some(3));
        assert_eq!(k.group_of_set(5), None);
        assert_eq!(k.group_of("C0:00:00:00:00:09"), None);
    }

    #[test]
    fn every_member_stands_for_the_saved_entry() {
        let k = KnownSets::of(vec![vec![L.into(), R.into()]]);
        let list = [saved("AA:BB:CC:DD:EE:FF", "Speaker"), saved(R, "Buds")];
        assert_eq!(k.saved_entry(L, &list).map(|d| d.address.as_str()), Some(R));
        assert_eq!(k.saved_entry(R, &list).map(|d| d.address.as_str()), Some(R));
        assert_eq!(k.saved_entry(OTHER, &list), None);
        // Nothing of the set saved
        assert_eq!(k.saved_entry(L, &list[..1]), None);
    }

    #[test]
    fn learned_sets_survive_a_restart_and_go_when_forgotten() {
        let dir = temp_dir("file");
        assert!(load(&dir).is_empty());
        // One member alone is no set
        assert!(!remember(&dir, &[L.into()]).unwrap());
        assert!(!paths::le_sets_file(&dir).exists());
        assert!(remember(&dir, &[L.to_lowercase(), R.into()]).unwrap());
        // Learned again: nothing written
        assert!(!remember(&dir, &[R.into(), L.into()]).unwrap());
        assert_eq!(load(&dir), vec![vec![L.to_string(), R.into()]]);
        assert_eq!(std::fs::read_to_string(paths::le_sets_file(&dir)).unwrap(), format!("{L} {R}\n"));
        assert_eq!(forget(&dir, &R.to_lowercase()), vec![L.to_string(), R.into()]);
        assert!(forget(&dir, L).is_empty());
        assert!(!paths::le_sets_file(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicates_merge_into_the_entry_with_settings() {
        let k = KnownSets::of(vec![vec![L.into(), R.into()]]);
        let list = [saved(L, "Buds"), saved("AA:BB:CC:DD:EE:FF", "Speaker"), saved(R, "Buds")];
        // Nothing set up: the one saved first stays
        assert_eq!(merges(&list, &k, |_| false), [Merge { keep: L.into(), drop: R.into() }]);
        // The one with a profile stays
        assert_eq!(merges(&list, &k, |a| a == R), [Merge { keep: R.into(), drop: L.into() }]);
        // Both: the one saved first
        assert_eq!(merges(&list, &k, |_| true), [Merge { keep: L.into(), drop: R.into() }]);
        // One member saved: nothing to merge
        assert!(merges(&list[..2], &k, |_| false).is_empty());
    }

    #[test]
    fn merged_entries_keep_their_profiles_and_volume() {
        let list = [saved(L, "Buds (L)"), saved(R, "Buds")];
        let merge = [Merge { keep: R.into(), drop: L.into() }];
        let profile = |addr: &str, le: bool| Profile {
            name: format!("p {addr}"),
            device_address: addr.into(),
            device_name: "x".into(),
            le_audio: le,
            ..Default::default()
        };
        let mut profiles = vec![profile(L, true), profile(L, false), profile(OTHER, true)];
        assert!(move_profiles(&mut profiles, &merge, &list));
        assert_eq!((profiles[0].device_address.as_str(), profiles[0].device_name.as_str()), (R, "Buds"));
        // A classic profile of the same address is another device
        assert_eq!(profiles[1].device_address, L);
        assert_eq!(profiles[2].device_address, OTHER);
        assert!(!move_profiles(&mut profiles, &merge, &list));

        let mut volumes = BTreeMap::from([(L.to_string(), 40)]);
        assert!(move_volumes(&mut volumes, &merge));
        assert_eq!(volumes, BTreeMap::from([(R.to_string(), 40)]));
        // The kept entry's own volume wins
        let mut volumes = BTreeMap::from([(L.to_string(), 40), (R.to_string(), 70)]);
        assert!(move_volumes(&mut volumes, &merge));
        assert_eq!(volumes, BTreeMap::from([(R.to_string(), 70)]));
    }
}
