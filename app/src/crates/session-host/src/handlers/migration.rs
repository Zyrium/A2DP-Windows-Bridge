//! Group `migration` in the agent: the files of the fixed list read from
//! the signed-in user's folder ([`set_user_folder`]), the secret ones
//! decrypted for the user. Nothing outside the list is read, links are
//! not followed, and nothing in the folder is changed: the user's files
//! stay as they are (the installed app, or a return to it, still has them).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::Value;

use crate::host::migration::{Entry, Found, Item, Request, to_hex};
use crate::wire::{ErrorCode, WireError, decode, reply};
use crate::{AgentContext, Handler};

static USER_FOLDER: OnceLock<PathBuf> = OnceLock::new();

/// The user's folder (`%APPDATA%\BlueCaster` or the test's), once, before
/// [`crate::run_agent`]; without it the agent has nothing to move
pub fn set_user_folder(dir: PathBuf) -> bool {
    USER_FOLDER.set(dir).is_ok()
}

/// The user's folder of [`set_user_folder`] (the handover group writes
/// below it)
pub(crate) fn user_folder() -> Option<PathBuf> {
    USER_FOLDER.get().cloned()
}

pub(crate) fn new(_ctx: AgentContext) -> Box<dyn Handler> {
    Box::new(Migration { folder: user_folder() })
}

struct Migration {
    folder: Option<PathBuf>,
}

/// One item of the folder `dir`
pub(crate) fn read_item(dir: &Path, item: Item) -> Found {
    let path = item.relative_path().split('/').fold(dir.to_path_buf(), |p, part| p.join(part));
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Found::Missing,
        Err(_) => return Found::Unreadable,
    };
    // A plain file only: no link, folder or device
    if !meta.file_type().is_file() {
        return Found::Unreadable;
    }
    // An encrypted file is a little larger than its content
    if meta.len() > (item.max_bytes() as u64) * 2 + 4096 {
        return Found::TooLarge;
    }
    let data = if item.secret() {
        match win_sys::dpapi::read_user_secret(&path) {
            Ok(Some(d)) => d,
            Ok(None) => return Found::Unreadable,
            Err(_) => return Found::Unreadable,
        }
    } else {
        match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => return Found::Unreadable,
        }
    };
    if data.len() > item.max_bytes() {
        return Found::TooLarge;
    }
    Found::Data { hex: to_hex(&data) }
}

/// Every item of the folder `dir`
pub(crate) fn collect(dir: &Path) -> Vec<Entry> {
    Item::ALL.iter().map(|&item| Entry { item, found: read_item(dir, item) }).collect()
}

impl Handler for Migration {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        let Request::Collect = decode(body)?;
        let Some(dir) = &self.folder else {
            tracing::warn!("migration: this agent knows no user folder");
            return Err(WireError::new(ErrorCode::Failed, "no user folder"));
        };
        let entries = collect(dir);
        let sent = entries.iter().filter(|e| matches!(e.found, Found::Data { .. })).count();
        tracing::info!("migration: {sent} of {} files sent to the core", entries.len());
        reply(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::migration::from_hex;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bluecaster-migration-agent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("pairing")).unwrap();
        std::fs::create_dir_all(d.join("devices")).unwrap();
        d
    }

    fn data(found: &Found) -> Vec<u8> {
        match found {
            Found::Data { hex } => from_hex(hex).unwrap(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reads_the_list_and_decrypts_the_secrets() {
        let d = temp("read");
        std::fs::write(d.join("settings.json"), "{}").unwrap();
        win_sys::dpapi::write_secret(&d.join("pairing").join("link_keys.txt"), b"keys").unwrap();
        // Not on the list: never read
        std::fs::write(d.join("devices").join("hid_devices.txt"), "x").unwrap();
        let entries = collect(&d);
        assert_eq!(entries.len(), Item::ALL.len());
        let of = |i: Item| &entries.iter().find(|e| e.item == i).unwrap().found;
        assert_eq!(data(of(Item::Settings)), b"{}");
        assert_eq!(data(of(Item::LinkKeys)), b"keys");
        assert_eq!(*of(Item::Profiles), Found::Missing);
        // The user's files are left as they were
        assert!(win_sys::dpapi::is_encrypted(&std::fs::read(d.join("pairing").join("link_keys.txt")).unwrap()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn large_undecryptable_and_odd_files_are_not_sent() {
        let d = temp("odd");
        std::fs::write(d.join("profiles.json"), vec![b' '; Item::Profiles.max_bytes() + 1]).unwrap();
        let mut foreign = win_sys::dpapi::MAGIC.to_vec();
        foreign.extend([1, 2, 3]);
        std::fs::write(d.join("pairing").join("link_keys.txt"), &foreign).unwrap();
        std::fs::create_dir_all(d.join("settings.json")).unwrap();
        assert_eq!(read_item(&d, Item::Profiles), Found::TooLarge);
        assert_eq!(read_item(&d, Item::LinkKeys), Found::Unreadable);
        assert_eq!(read_item(&d, Item::Settings), Found::Unreadable);
        // The undecryptable file is not renamed or rewritten
        assert_eq!(std::fs::read(d.join("pairing").join("link_keys.txt")).unwrap(), foreign);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn without_a_folder_the_request_fails() {
        let mut h = Migration { folder: None };
        assert_eq!(h.request(serde_json::json!({"op": "collect"})).unwrap_err().code, ErrorCode::Failed);
        assert_eq!(
            h.request(serde_json::json!({"op": "read", "path": "C:/x"})).unwrap_err().code,
            ErrorCode::BadRequest
        );
        let d = temp("handler");
        std::fs::write(d.join("settings.json"), "{}").unwrap();
        let mut h = Migration { folder: Some(d.clone()) };
        let got: Vec<Entry> = serde_json::from_value(h.request(serde_json::json!({"op": "collect"})).unwrap()).unwrap();
        assert_eq!(got.len(), Item::ALL.len());
        let _ = std::fs::remove_dir_all(&d);
    }
}
