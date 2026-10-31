//! qcy-cache.json in the settings folder (%APPDATA%\A2DPWB, next to
//! settings.json but not in it): where the last read found each QCY
//! earbuds, so the first read after a restart connects straight to them
//! (no scan) with the GATT handles of the last link (no discovery).
//!
//! Keyed by the earbuds' classic (BR/EDR) address. Holds only what any
//! scanner sees over the air: the LE address and its type, the GATT
//! handles, the name, the firmware string and when it was saved. No link
//! key, no IRK, nothing secret. A file that does not parse is ignored (the
//! read scans and rewrites it).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bt_host::{GattChar, GattService, LeAddr};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "qcy-cache.json";
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedChar {
    pub uuid16: u16,
    /// 32 hex digits, BTstack order
    pub uuid128: String,
    pub start_handle: u16,
    pub value_handle: u16,
    pub end_handle: u16,
    pub properties: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedService {
    pub uuid16: u16,
    pub uuid128: String,
    pub start_handle: u16,
    pub end_handle: u16,
    pub primary: bool,
    pub included_by: u16,
    pub chars: Vec<CachedChar>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    /// "XX:XX:XX:XX:XX:XX"
    pub le_address: String,
    /// "public" or "random"
    pub address_type: String,
    pub handles: Vec<CachedService>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    /// Unix ms
    pub saved_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheFile {
    pub version: u32,
    /// Classic (BR/EDR) address of the earbuds -> where LE found them
    pub devices: BTreeMap<String, Entry>,
}

impl Default for CacheFile {
    fn default() -> Self {
        CacheFile { version: VERSION, devices: BTreeMap::new() }
    }
}

pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The file, or an empty one when it is missing, unreadable or of another
/// version
pub fn load(dir: &Path) -> CacheFile {
    let Ok(text) = std::fs::read_to_string(path_in(dir)) else { return CacheFile::default() };
    match serde_json::from_str::<CacheFile>(&text) {
        Ok(f) if f.version == VERSION => f,
        _ => {
            tracing::info!("QCY: {FILE} ignored (unreadable or another version)");
            CacheFile::default()
        }
    }
}

/// Writes atomically (temporary file + rename)
pub fn save(dir: &Path, f: &CacheFile) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(f).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path_in(dir))
}

/// Stores `entry` for `classic`, replacing the old one
pub fn remember(dir: &Path, classic: &str, entry: Entry) {
    let mut f = load(dir);
    f.devices.insert(classic.to_uppercase(), entry);
    if let Err(e) = save(dir, &f) {
        tracing::warn!("QCY: could not write {FILE}: {e}");
    }
}

/// Forgets `classic` (its address or handles failed)
pub fn invalidate(dir: &Path, classic: &str) {
    let mut f = load(dir);
    if f.devices.remove(&classic.to_uppercase()).is_some()
        && let Err(e) = save(dir, &f)
    {
        tracing::warn!("QCY: could not write {FILE}: {e}");
    }
}

/// The most recently saved device that converts back cleanly
pub fn latest(f: &CacheFile) -> Option<(String, LeAddr, Vec<GattService>, &Entry)> {
    f.devices
        .iter()
        .filter_map(|(classic, e)| Some((classic.clone(), to_addr(e)?, to_services(&e.handles)?, e)))
        .filter(|(_, _, s, _)| !s.is_empty())
        .max_by_key(|(_, _, _, e)| e.saved_at)
}

pub fn entry(name: &str, addr: LeAddr, services: &[GattService], firmware: Option<String>, saved_at: u64) -> Entry {
    Entry {
        name: name.to_string(),
        le_address: bt_host::addr_str(&addr.addr),
        address_type: if addr.addr_type == LeAddr::RANDOM { "random" } else { "public" }.into(),
        handles: services.iter().map(from_service).collect(),
        firmware,
        saved_at,
    }
}

fn to_addr(e: &Entry) -> Option<LeAddr> {
    let addr = bt_host::parse_addr(&e.le_address)?;
    let addr_type = match e.address_type.as_str() {
        "public" => LeAddr::PUBLIC,
        "random" => LeAddr::RANDOM,
        _ => return None,
    };
    Some(LeAddr { addr, addr_type })
}

fn hex(b: &[u8; 16]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn from_service(s: &GattService) -> CachedService {
    CachedService {
        uuid16: s.uuid16,
        uuid128: hex(&s.uuid128),
        start_handle: s.start_handle,
        end_handle: s.end_handle,
        primary: s.primary,
        included_by: s.included_by,
        chars: s
            .chars
            .iter()
            .map(|c| CachedChar {
                uuid16: c.uuid16,
                uuid128: hex(&c.uuid128),
                start_handle: c.start_handle,
                value_handle: c.value_handle,
                end_handle: c.end_handle,
                properties: c.properties,
            })
            .collect(),
    }
}

/// None when anything is malformed (then the whole entry is unusable)
fn to_services(v: &[CachedService]) -> Option<Vec<GattService>> {
    v.iter()
        .map(|s| {
            let chars = s
                .chars
                .iter()
                .map(|c| {
                    (c.value_handle != 0
                        && c.start_handle <= c.value_handle
                        && c.value_handle <= c.end_handle.max(c.value_handle))
                    .then_some(())?;
                    Some(GattChar {
                        uuid16: c.uuid16,
                        uuid128: unhex(&c.uuid128)?,
                        start_handle: c.start_handle,
                        value_handle: c.value_handle,
                        end_handle: c.end_handle,
                        properties: c.properties,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            (s.start_handle != 0 && s.start_handle <= s.end_handle).then_some(())?;
            Some(GattService {
                uuid16: s.uuid16,
                uuid128: unhex(&s.uuid128)?,
                start_handle: s.start_handle,
                end_handle: s.end_handle,
                primary: s.primary,
                included_by: s.included_by,
                chars,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("a2dpwb-qcy-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn services() -> Vec<GattService> {
        let mut u = [0u8; 16];
        u[0] = 0xAB;
        vec![GattService {
            uuid16: 0,
            uuid128: u,
            start_handle: 0x10,
            end_handle: 0x20,
            primary: true,
            included_by: 0,
            chars: vec![GattChar {
                uuid16: 0x1001,
                uuid128: u,
                start_handle: 0x11,
                value_handle: 0x12,
                end_handle: 0x13,
                properties: 0x14,
            }],
        }]
    }

    fn addr() -> LeAddr {
        LeAddr { addr: bt_host::parse_addr("C4:33:96:11:0C:83").unwrap(), addr_type: LeAddr::RANDOM }
    }

    #[test]
    fn save_then_load_round_trips() {
        let d = dir("roundtrip");
        remember(&d, "c4:33:96:11:0c:82", entry("Melon Buds N70", addr(), &services(), Some("3.0.13".into()), 7));
        let f = load(&d);
        let (classic, a, s, e) = latest(&f).unwrap();
        assert_eq!(classic, "C4:33:96:11:0C:82");
        assert_eq!(a, addr());
        assert_eq!(s, services());
        assert_eq!(e.le_address, "C4:33:96:11:0C:83");
        assert_eq!(e.address_type, "random");
        assert_eq!(e.firmware.as_deref(), Some("3.0.13"));
        let text = std::fs::read_to_string(path_in(&d)).unwrap().to_lowercase();
        assert!(!text.contains("key") && !text.contains("irk"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn invalidate_forgets_only_that_device() {
        let d = dir("invalidate");
        remember(&d, "C4:33:96:11:0C:82", entry("a", addr(), &services(), None, 1));
        remember(&d, "00:11:22:33:44:55", entry("b", addr(), &services(), None, 2));
        invalidate(&d, "c4:33:96:11:0c:82");
        let f = load(&d);
        assert_eq!(f.devices.len(), 1);
        assert!(f.devices.contains_key("00:11:22:33:44:55"));
        invalidate(&d, "00:11:22:33:44:55");
        assert!(latest(&load(&d)).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_or_broken_files_are_empty() {
        let d = dir("broken");
        assert!(load(&d).devices.is_empty());
        std::fs::write(path_in(&d), "{ not json").unwrap();
        assert!(load(&d).devices.is_empty());
        std::fs::write(path_in(&d), r#"{"version":99,"devices":{}}"#).unwrap();
        assert!(load(&d).devices.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_entries_are_skipped_and_latest_wins() {
        let mut f = CacheFile::default();
        let mut bad = entry("bad", addr(), &services(), None, 99);
        bad.address_type = "weird".into();
        f.devices.insert("AA:AA:AA:AA:AA:AA".into(), bad);
        let mut bad_handle = entry("bad2", addr(), &services(), None, 98);
        bad_handle.handles[0].chars[0].uuid128 = "zz".into();
        f.devices.insert("BB:BB:BB:BB:BB:BB".into(), bad_handle);
        f.devices.insert("CC:CC:CC:CC:CC:CC".into(), entry("old", addr(), &services(), None, 1));
        f.devices.insert("DD:DD:DD:DD:DD:DD".into(), entry("new", addr(), &services(), None, 5));
        assert_eq!(latest(&f).unwrap().3.name, "new");
        let mut empty = entry("empty", addr(), &[], None, 100);
        empty.handles.clear();
        f.devices.insert("EE:EE:EE:EE:EE:EE".into(), empty);
        assert_eq!(latest(&f).unwrap().3.name, "new");
    }
}
