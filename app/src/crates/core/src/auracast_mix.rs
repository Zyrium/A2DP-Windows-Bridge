//! The Auracast matrix mixer (developer mode): which source speakers make
//! each broadcast output (FL FR RL RR), with a delay and polarity per
//! output. Kept in auracast_mix.json with the user's named presets; one
//! [`MixHandle`] is shared with the encode thread of the broadcast running,
//! so a change is heard from the next block of audio, without restarting.

use std::path::Path;
use std::sync::{LazyLock, Mutex};

use ipc::{AuracastMix, AuracastMixPreset, AuracastMixState};
use pipeline::mix::{INPUTS, MixHandle, MixSettings, auto_matrix, effective_matrix, present_columns};
use serde::{Deserialize, Serialize};

const OUTPUTS: usize = 4;

/// auracast_mix.json
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct MixFile {
    current: Option<AuracastMix>,
    presets: Vec<AuracastMixPreset>,
}

/// The source and outputs of the broadcast running with the mixer
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Live {
    pub channels: usize,
    pub mask: u32,
    pub rear: bool,
}

static HANDLE: LazyLock<MixHandle> = LazyLock::new(|| {
    let h = MixHandle::default();
    // Tests keep to their own folders: the defaults
    if cfg!(test) {
        return h;
    }
    let file = load(&crate::paths::config_dir());
    if let Some(m) = &file.current {
        h.set(settings(m));
    }
    h
});
static LIVE: Mutex<Option<Live>> = Mutex::new(None);

/// The mixer the broadcast's encode thread reads
pub fn handle() -> MixHandle {
    HANDLE.clone()
}

/// The broadcast running with the mixer (None: it ended)
pub fn set_live(live: Option<Live>) {
    *LIVE.lock().unwrap_or_else(|e| e.into_inner()) = live;
}

fn live() -> Option<Live> {
    *LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn file_path(dir: &Path) -> std::path::PathBuf {
    crate::paths::auracast_mix_file(dir)
}

/// A file that cannot be read is put aside as auracast_mix.json.bad, so the
/// next save does not write over the presets in it
fn load(dir: &Path) -> MixFile {
    let path = file_path(dir);
    match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_else(|e| {
            let aside = path.with_extension("json.bad");
            match std::fs::rename(&path, &aside) {
                Ok(()) => tracing::warn!("Auracast mixer: settings not read ({e}), kept as auracast_mix.json.bad"),
                Err(io) => tracing::warn!("Auracast mixer: settings not read ({e}) and not put aside ({io})"),
            }
            MixFile::default()
        }),
        Err(_) => MixFile::default(),
    }
}

fn save(dir: &Path, file: &MixFile) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    let tmp = file_path(dir).with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file_path(dir))
}

/// What the encode thread takes, from the UI's form (missing entries 0)
pub fn settings(m: &AuracastMix) -> MixSettings {
    let mut matrix = [[0.0f32; INPUTS]; OUTPUTS];
    for (row, r) in matrix.iter_mut().zip(&m.matrix) {
        for (g, x) in row.iter_mut().zip(r) {
            *g = *x;
        }
    }
    let mut delay_ms = [0.0f32; OUTPUTS];
    for (d, x) in delay_ms.iter_mut().zip(&m.delay_ms) {
        *d = *x;
    }
    let mut invert = [false; OUTPUTS];
    for (i, x) in invert.iter_mut().zip(&m.invert) {
        *i = *x;
    }
    MixSettings {
        auto: m.auto,
        center_db: m.center_db,
        lfe_db: m.lfe_db,
        surround_db: m.surround_db,
        normalize: m.normalize,
        matrix,
        delay_ms,
        invert,
    }
    .held()
}

/// As the UI gets it
pub fn to_ipc(s: &MixSettings) -> AuracastMix {
    AuracastMix {
        auto: s.auto,
        center_db: s.center_db,
        lfe_db: s.lfe_db,
        surround_db: s.surround_db,
        normalize: s.normalize,
        matrix: rows(&s.matrix),
        delay_ms: s.delay_ms.to_vec(),
        invert: s.invert.to_vec(),
    }
}

fn rows(m: &pipeline::mix::Matrix) -> Vec<Vec<f32>> {
    m.iter().map(|r| r.to_vec()).collect()
}

/// The program's own presets (names are ids the UI translates)
pub fn builtin_presets() -> Vec<AuracastMixPreset> {
    let all = [true; INPUTS];
    let standard = MixSettings::default();
    let manual = |matrix| MixSettings { auto: false, matrix, ..standard };
    let rear = auto_matrix(&all, false, true, &standard);
    let fold = auto_matrix(&all, false, false, &standard);
    let zero = [0.0f32; INPUTS];
    let list = [
        ("standard", standard),
        ("normalized", MixSettings { normalize: true, ..standard }),
        ("front_everywhere", manual([fold[0], fold[1], fold[0], fold[1]])),
        ("front_only", manual([fold[0], fold[1], zero, zero])),
        ("rear_only", manual([zero, zero, rear[2], rear[3]])),
        ("swapped", manual([rear[1], rear[0], rear[3], rear[2]])),
    ];
    list.into_iter()
        .map(|(name, s)| AuracastMixPreset { name: name.into(), mix: to_ipc(&s.held()), builtin: true })
        .collect()
}

/// The state for the broadcast running, else for a source of `channels`
/// (default 8, the usual speakers) with or without rear outputs
pub fn state(dir: &Path, channels: Option<u32>, rear: Option<bool>) -> AuracastMixState {
    let file = load(dir);
    let s = HANDLE.get();
    let live = live();
    let (channels, mask, rear) = match live {
        Some(l) => (l.channels, l.mask, l.rear),
        None => (channels.map_or(8, |c| c.clamp(1, 8) as usize), 0, rear.unwrap_or(true)),
    };
    let mut presets = builtin_presets();
    presets.extend(file.presets.into_iter().map(|p| AuracastMixPreset { builtin: false, ..p }));
    AuracastMixState {
        mix: to_ipc(&s),
        presets,
        live: live.is_some(),
        channels: channels as u32,
        present: present_columns(channels, mask).to_vec(),
        rear,
        effective: rows(&effective_matrix(channels, mask, rear, &s)),
    }
}

/// New settings: heard at once while broadcasting, and saved
pub fn set(dir: &Path, m: &AuracastMix) -> std::io::Result<()> {
    let s = settings(m);
    HANDLE.set(s);
    let mut file = load(dir);
    file.current = Some(to_ipc(&s));
    save(dir, &file)
}

/// Saves `m` as the preset `name` (replacing one of that name)
pub fn save_preset(dir: &Path, name: &str, m: &AuracastMix) -> std::io::Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(std::io::Error::other("no name"));
    }
    // The built-in ones go by these names: one saved under such a name
    // could not be chosen or deleted
    if builtin_presets().iter().any(|p| p.name == name) {
        return Err(std::io::Error::other("the name of a built-in preset"));
    }
    let mut file = load(dir);
    let mix = to_ipc(&settings(m));
    match file.presets.iter_mut().find(|p| p.name == name) {
        Some(p) => p.mix = mix,
        None => file.presets.push(AuracastMixPreset { name: name.into(), mix, builtin: false }),
    }
    save(dir, &file)
}

pub fn delete_preset(dir: &Path, name: &str) -> std::io::Result<()> {
    let mut file = load(dir);
    file.presets.retain(|p| p.name != name);
    save(dir, &file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_round_trips_and_is_held_to_the_ranges() {
        let s = MixSettings {
            delay_ms: [1.25, 0.0, 600.0, 0.0],
            invert: [false, true, false, false],
            ..Default::default()
        };
        let m = to_ipc(&s);
        assert_eq!(m.matrix.len(), 4);
        assert!(m.matrix.iter().all(|r| r.len() == 8));
        let back = settings(&m);
        assert_eq!(back.delay_ms, [1.25, 0.0, 500.0, 0.0]);
        assert_eq!(back.invert, s.invert);
        // Short rows: the rest at 0
        let short = AuracastMix { matrix: vec![vec![1.0]], ..Default::default() };
        assert_eq!(settings(&short).matrix[0][..2], [1.0, 0.0]);
    }

    #[test]
    fn presets_are_saved_replaced_and_deleted() {
        let dir = std::env::temp_dir().join(format!("bc-mix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = to_ipc(&MixSettings::default());
        save_preset(&dir, " Room ", &m).unwrap();
        let mut m2 = m.clone();
        m2.delay_ms[0] = 3.0;
        save_preset(&dir, "Room", &m2).unwrap();
        let st = state(&dir, Some(6), Some(false));
        let user: Vec<_> = st.presets.iter().filter(|p| !p.builtin).collect();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].mix.delay_ms[0], 3.0);
        assert_eq!(st.present, [true, true, true, true, true, true, false, false]);
        assert!(!st.rear);
        delete_preset(&dir, "Room").unwrap();
        assert!(state(&dir, None, None).presets.iter().all(|p| p.builtin));
        // Not under a built-in preset's name
        assert!(save_preset(&dir, "standard", &m).is_err());
        assert!(state(&dir, None, None).presets.iter().all(|p| p.builtin));
        // A file that cannot be read is kept aside, not written over
        save_preset(&dir, "Room", &m).unwrap();
        let path = crate::paths::auracast_mix_file(&dir);
        let saved = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &saved[..saved.len() / 2]).unwrap();
        save_preset(&dir, "Other", &m).unwrap();
        let aside = std::fs::read_to_string(path.with_extension("json.bad")).unwrap();
        assert_eq!(aside, saved[..saved.len() / 2]);
        let names: Vec<_> =
            state(&dir, None, None).presets.into_iter().filter(|p| !p.builtin).map(|p| p.name).collect();
        assert_eq!(names, ["Other"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn builtin_presets_route_as_named() {
        let p = builtin_presets();
        let get = |n: &str| settings(&p.iter().find(|p| p.name == n).unwrap().mix);
        let front = get("front_only");
        assert!(!front.auto && front.matrix[2].iter().all(|g| *g == 0.0));
        let rear = get("rear_only");
        assert!(rear.matrix[0].iter().all(|g| *g == 0.0) && rear.matrix[2][4] > 0.0);
        let swapped = get("swapped");
        assert_eq!((swapped.matrix[0][1], swapped.matrix[1][0]), (1.0, 1.0));
    }
}
