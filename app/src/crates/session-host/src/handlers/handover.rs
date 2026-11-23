//! Group `handover` in the agent: a file of the core written into the
//! signed-in user's folder (the folder of
//! [`crate::handlers::migration::set_user_folder`]), below its kind's
//! folder, under a name of the fixed pattern. While it arrives the file is
//! a hidden `.part` file next to its final place; it gets its name only
//! when it is complete, of its announced size and of its kind. A file not
//! finished (refused, aborted, the connection lost) is removed.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::host::handover::{Begun, CHUNK, Finished, Kind, MAX_OPEN, Request};
use crate::host::migration::from_hex;
use crate::wire::{ErrorCode, WireError, decode, reply};
use crate::{AgentContext, Handler};

pub(crate) fn new(_ctx: AgentContext) -> Box<dyn Handler> {
    Box::new(Handover::new(crate::handlers::migration::user_folder()))
}

struct Partial {
    kind: Kind,
    dir: PathBuf,
    part: PathBuf,
    file: Option<File>,
    size: u64,
    written: u64,
}

impl Partial {
    fn discard(mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.part);
    }
}

pub(crate) struct Handover {
    folder: Option<PathBuf>,
    open: HashMap<u64, Partial>,
    next: u64,
}

fn failed(why: &str) -> WireError {
    tracing::info!("handover: refused ({why})");
    WireError::new(ErrorCode::Failed, why)
}

/// Local time, "YYYYMMDD_HHMMSS" (as the core's file names)
fn stamp() -> String {
    let t = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!("{:04}{:02}{:02}_{:02}{:02}{:02}", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute(), t.second())
}

/// Gives the complete file `part` its name in `dir`: the first name of the
/// pattern not taken (a hard link never replaces a file), then the part
/// file goes
fn place(part: &Path, dir: &Path, kind: Kind, stamp: &str) -> std::io::Result<PathBuf> {
    for n in 1..=99 {
        let target = dir.join(kind.file_name(stamp, n));
        match std::fs::hard_link(part, &target) {
            Ok(()) => {
                let _ = std::fs::remove_file(part);
                return Ok(target);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // A file system without hard links: a rename, once the name
            // is free
            Err(_) if !target.exists() => {
                std::fs::rename(part, &target)?;
                return Ok(target);
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "every name is taken"))
}

impl Handover {
    pub(crate) fn new(folder: Option<PathBuf>) -> Handover {
        Handover { folder, open: HashMap::new(), next: 1 }
    }

    fn begin(&mut self, kind: Kind, size: u64) -> Result<Begun, WireError> {
        let Some(folder) = &self.folder else { return Err(failed("no user folder")) };
        if size > kind.max_bytes() {
            return Err(failed("too large"));
        }
        if self.open.len() >= MAX_OPEN {
            return Err(WireError::new(ErrorCode::Busy, "too many files at once"));
        }
        let dir = kind.folder().split('/').fold(folder.clone(), |p, part| p.join(part));
        std::fs::create_dir_all(&dir).map_err(|_| failed("the folder cannot be made"))?;
        let id = self.next;
        self.next += 1;
        let part = dir.join(format!(".handover-{}-{id}.part", std::process::id()));
        let file =
            File::options().write(true).create_new(true).open(&part).map_err(|_| failed("the file cannot be made"))?;
        self.open.insert(id, Partial { kind, dir, part, file: Some(file), size, written: 0 });
        Ok(Begun { id })
    }

    fn write(&mut self, id: u64, hex: &str) -> Result<(), WireError> {
        let Some(p) = self.open.get_mut(&id) else { return Err(failed("no such file")) };
        let bytes = from_hex(hex).filter(|b| b.len() <= CHUNK);
        let fits = bytes.as_ref().is_some_and(|b| p.written + b.len() as u64 <= p.size);
        let written = match (&bytes, fits, p.file.as_mut()) {
            (Some(b), true, Some(f)) => f.write_all(b).is_ok(),
            _ => false,
        };
        if !written {
            let why = if bytes.is_none() {
                "not a piece of a file"
            } else if !fits {
                "larger than announced"
            } else {
                "the file cannot be written"
            };
            if let Some(p) = self.open.remove(&id) {
                p.discard();
            }
            return Err(failed(why));
        }
        p.written += bytes.map_or(0, |b| b.len() as u64);
        Ok(())
    }

    fn finish(&mut self, id: u64) -> Result<Finished, WireError> {
        let Some(mut p) = self.open.remove(&id) else { return Err(failed("no such file")) };
        let flushed = p.file.take().is_some_and(|mut f| f.flush().is_ok());
        if !flushed || p.written != p.size {
            p.discard();
            return Err(failed("incomplete"));
        }
        let ok = std::fs::read(&p.part).is_ok_and(|data| p.kind.content_ok(&data));
        if !ok {
            p.discard();
            return Err(failed("not of its kind"));
        }
        match place(&p.part, &p.dir, p.kind, &stamp()) {
            Ok(path) => {
                tracing::info!("handover: a file of {} bytes given to the user", p.size);
                Ok(Finished { path: path.display().to_string() })
            }
            Err(_) => {
                p.discard();
                Err(failed("no name for the file"))
            }
        }
    }
}

impl Handler for Handover {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::Begin { kind, size } => reply(self.begin(kind, size)?),
            Request::Write { id, hex } => reply(self.write(id, &hex)?),
            Request::Finish { id } => reply(self.finish(id)?),
            Request::Abort { id } => {
                if let Some(p) = self.open.remove(&id) {
                    p.discard();
                }
                reply(())
            }
        }
    }
}

impl Drop for Handover {
    /// The connection ended: files not finished go
    fn drop(&mut self) {
        for (_, p) in self.open.drain() {
            p.discard();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::migration::to_hex;
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bluecaster-handover-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn zip(body: usize) -> Vec<u8> {
        let mut z = b"PK\x03\x04".to_vec();
        z.extend(std::iter::repeat_n(7u8, body));
        z.extend(b"PK\x05\x06");
        z.extend([0u8; 18]);
        z
    }

    fn send(h: &mut Handover, kind: &str, data: &[u8]) -> Result<String, WireError> {
        let begun = h.request(json!({"op": "begin", "kind": kind, "size": data.len()}))?;
        let id = begun["id"].as_u64().unwrap();
        for c in data.chunks(CHUNK) {
            h.request(json!({"op": "write", "id": id, "hex": to_hex(c)}))?;
        }
        let done = h.request(json!({"op": "finish", "id": id}))?;
        Ok(done["path"].as_str().unwrap().to_string())
    }

    /// Only the hidden part files of unfinished hand-overs may be left
    fn files(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn a_file_lands_in_its_folder_under_the_fixed_name() {
        let d = temp("lands");
        let mut h = Handover::new(Some(d.clone()));
        // Larger than one piece
        let data = zip(CHUNK + 10);
        let path = PathBuf::from(send(&mut h, "bundle", &data).unwrap());
        assert_eq!(path.parent().unwrap(), d.join("logs").join("bundles"));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("bluecaster-diag-") && name.ends_with(".zip"), "{name}");
        assert_eq!(std::fs::read(&path).unwrap(), data);
        // A second one in the same second: its own name, the first kept
        let again = PathBuf::from(send(&mut h, "bundle", &data).unwrap());
        assert_ne!(again, path);
        assert!(path.is_file());
        let report = br#"{"summary":"8 of 8"}"#;
        let probe = PathBuf::from(send(&mut h, "probe_report", report).unwrap());
        assert_eq!(probe.parent().unwrap(), d.join("logs").join("probe"));
        assert!(probe.file_name().unwrap().to_string_lossy().starts_with("probe-"));
        // No part files left behind
        assert!(files(&d.join("logs").join("bundles")).iter().all(|n| !n.ends_with(".part")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn oversized_wrong_kind_and_short_files_are_refused() {
        let d = temp("refused");
        let mut h = Handover::new(Some(d.clone()));
        let too_big = Kind::ProbeReport.max_bytes() + 1;
        let e = h.request(json!({"op": "begin", "kind": "probe_report", "size": too_big})).unwrap_err();
        assert_eq!(e.code, ErrorCode::Failed);
        // Not a zip
        assert!(send(&mut h, "bundle", b"MZ\x90\x00 a program, not a bundle").is_err());
        // Not JSON
        assert!(send(&mut h, "script_report", b"PK\x03\x04").is_err());
        // More than announced
        let b = h.request(json!({"op": "begin", "kind": "probe_report", "size": 2})).unwrap();
        let id = b["id"].as_u64().unwrap();
        assert!(h.request(json!({"op": "write", "id": id, "hex": "7b7d7b"})).is_err());
        // Less than announced
        let b = h.request(json!({"op": "begin", "kind": "probe_report", "size": 9})).unwrap();
        let id = b["id"].as_u64().unwrap();
        h.request(json!({"op": "write", "id": id, "hex": "7b7d"})).unwrap();
        assert!(h.request(json!({"op": "finish", "id": id})).is_err());
        // Not hex
        let b = h.request(json!({"op": "begin", "kind": "probe_report", "size": 2})).unwrap();
        let id = b["id"].as_u64().unwrap();
        assert!(h.request(json!({"op": "write", "id": id, "hex": "zz"})).is_err());
        // Anything else is not a request of the group
        let bad = h.request(json!({"op": "save", "path": "C:/Windows/x.exe"})).unwrap_err();
        assert_eq!(bad.code, ErrorCode::BadRequest);
        // Nothing was given a name, and nothing is left behind
        assert!(files(&d.join("logs").join("bundles")).is_empty());
        assert!(files(&d.join("logs").join("probe")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unfinished_files_go_with_the_connection() {
        let d = temp("dropped");
        let mut h = Handover::new(Some(d.clone()));
        for _ in 0..MAX_OPEN {
            h.request(json!({"op": "begin", "kind": "bundle", "size": 10})).unwrap();
        }
        let busy = h.request(json!({"op": "begin", "kind": "bundle", "size": 10})).unwrap_err();
        assert_eq!(busy.code, ErrorCode::Busy);
        assert_eq!(files(&d.join("logs").join("bundles")).len(), MAX_OPEN);
        drop(h);
        assert!(files(&d.join("logs").join("bundles")).is_empty());
        // Without a user folder nothing is taken
        let mut none = Handover::new(None);
        assert!(none.request(json!({"op": "begin", "kind": "bundle", "size": 10})).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
