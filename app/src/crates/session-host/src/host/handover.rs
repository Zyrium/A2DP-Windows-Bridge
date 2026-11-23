//! Group `handover`: a finished file of the core handed to the signed-in
//! user. In service mode the core's data folder is protected, so a report
//! or a diagnostic bundle made there is out of the user's reach. The core
//! sends the file's content through the agent; the agent writes it into
//! the user's own folder and returns the new file's path, which the UI
//! shows and opens.
//!
//! What crosses, and what does not:
//!
//! - The core sends a [`Kind`] and the bytes, never a name or a folder.
//!   The agent picks the place from the kind ([`Kind::folder`], below the
//!   user's BlueCaster folder) and the name from a fixed pattern
//!   (`<prefix>-<stamp>.<extension>`).
//! - The agent refuses a file larger than its kind's limit
//!   ([`Kind::max_bytes`]) and content that is not of its kind (a zip, a
//!   JSON object): [`Kind::content_ok`].
//! - The reply is the path only. The core never reads anything back from
//!   the user's folder.
//!
//! Which files the core hands over is the core's decision (bundles and
//! chip reports only, each the result of a request an administrator
//! approved); this group only carries them. The content is sent in pieces
//! ([`CHUNK`]) so that each frame stays below `ipc::MAX_FRAME`.

use serde::{Deserialize, Serialize};

use crate::{HostError, LocalHost, RemoteHost};

pub const GROUP: &str = "handover";

/// The bytes of one [`Request::Write`] (twice that as hex in the frame)
pub const CHUNK: usize = 1 << 20;

/// Files the agent writes at once (begun and not finished)
pub const MAX_OPEN: usize = 4;

/// What the core hands over
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A diagnostic bundle (diag.bundle), a .zip
    Bundle,
    /// A chip probe's report (adapter.probe), JSON
    ProbeReport,
    /// A chip script's report (adapter.run_script), JSON
    ScriptReport,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Bundle, Kind::ProbeReport, Kind::ScriptReport];

    /// The most the agent takes. A bundle stays well below this
    /// (bluecaster-core's bundle.rs keeps it near GitHub's 25 MB).
    pub fn max_bytes(self) -> u64 {
        match self {
            Kind::Bundle => 256 << 20,
            Kind::ProbeReport | Kind::ScriptReport => 8 << 20,
        }
    }

    /// The folder below the user's BlueCaster folder, `/`-separated: where
    /// the window's own copy of BlueCaster keeps the same files
    /// (bluecaster_core::paths: logs\bundles, logs\probe)
    pub fn folder(self) -> &'static str {
        match self {
            Kind::Bundle => "logs/bundles",
            Kind::ProbeReport | Kind::ScriptReport => "logs/probe",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Bundle => "bluecaster-diag",
            Kind::ProbeReport => "probe",
            Kind::ScriptReport => "script",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Kind::Bundle => "zip",
            Kind::ProbeReport | Kind::ScriptReport => "json",
        }
    }

    /// The file's name for `stamp` ("YYYYMMDD_HHMMSS") and the `n`th try
    /// (1: none taken yet)
    pub fn file_name(self, stamp: &str, n: u32) -> String {
        match n {
            0 | 1 => format!("{}-{stamp}.{}", self.prefix(), self.extension()),
            n => format!("{}-{stamp}-{n}.{}", self.prefix(), self.extension()),
        }
    }

    /// Whether `data` (the whole file) is of this kind: a zip archive
    /// (local header first, the end record last), or one JSON object
    pub fn content_ok(self, data: &[u8]) -> bool {
        match self {
            Kind::Bundle => zip_like(data),
            Kind::ProbeReport | Kind::ScriptReport => {
                serde_json::from_slice::<serde_json::Value>(data).is_ok_and(|v| v.is_object())
            }
        }
    }
}

/// A zip archive: a local file header at the start and the end of central
/// directory record in the last 64 KiB (it may carry a comment)
fn zip_like(data: &[u8]) -> bool {
    const LOCAL: &[u8] = b"PK\x03\x04";
    const END: &[u8] = b"PK\x05\x06";
    if data.len() < 22 || !data.starts_with(LOCAL) {
        return false;
    }
    let tail = &data[data.len().saturating_sub(22 + 0xFFFF)..];
    tail.windows(4).rposition(|w| w == END).is_some_and(|i| tail.len() - i >= 22)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// A new file of `kind` and `size` bytes -> [`Begun`]
    Begin { kind: Kind, size: u64 },
    /// The next piece of file `id`, in hex (at most [`CHUNK`] bytes) -> null
    Write { id: u64, hex: String },
    /// File `id` is complete: checked and given its name -> [`Finished`]
    Finish { id: u64 },
    /// File `id` is not wanted any more: removed -> null
    Abort { id: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Begun {
    pub id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finished {
    /// The file in the user's folder
    pub path: String,
}

/// The facade's method. Blocks until the agent has the whole file: not for
/// the BTstack thread or an audio thread.
pub trait HandoverHost {
    /// Hands `data` to the signed-in user as a file of `kind` -> its path
    /// in the user's folder
    fn hand_over(&self, kind: Kind, data: &[u8]) -> Result<String, HostError>;
}

impl HandoverHost for LocalHost {
    /// In-process the user can open the core's folder: nothing to hand over
    fn hand_over(&self, _kind: Kind, _data: &[u8]) -> Result<String, HostError> {
        Err(HostError::NoAgent)
    }
}

impl HandoverHost for RemoteHost {
    fn hand_over(&self, kind: Kind, data: &[u8]) -> Result<String, HostError> {
        let Begun { id } = self.call(GROUP, &Request::Begin { kind, size: data.len() as u64 })?;
        let sent = data.chunks(CHUNK).try_for_each(|c| {
            let hex = crate::host::migration::to_hex(c);
            self.call::<serde_json::Value>(GROUP, &Request::Write { id, hex }).map(drop)
        });
        let finished = sent.and_then(|()| self.call::<Finished>(GROUP, &Request::Finish { id }));
        if finished.is_err() {
            let _ = self.call::<serde_json::Value>(GROUP, &Request::Abort { id });
        }
        finished.map(|f| f.path)
    }
}

/// [`HandoverHost::hand_over`] of this process's host
pub fn hand_over(kind: Kind, data: &[u8]) -> Result<String, HostError> {
    crate::host().hand_over(kind, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_places_are_fixed() {
        assert_eq!(Kind::Bundle.file_name("20261122_101500", 1), "bluecaster-diag-20261122_101500.zip");
        assert_eq!(Kind::ProbeReport.file_name("20261122_101500", 3), "probe-20261122_101500-3.json");
        for k in Kind::ALL {
            let f = k.folder();
            assert!(!f.contains("..") && !f.contains(':') && !f.contains('\\') && !f.starts_with('/'), "{f}");
        }
        // A CHUNK of hex and the frame's JSON fit one frame
        const { assert!(CHUNK * 2 + 256 < ipc::MAX_FRAME) };
        let json = serde_json::to_string(&Request::Begin { kind: Kind::ScriptReport, size: 3 }).unwrap();
        assert_eq!(json, r#"{"op":"begin","kind":"script_report","size":3}"#);
        // A kind not on the list, or a path, does not decode
        assert!(serde_json::from_str::<Request>(r#"{"op":"begin","kind":"capture","size":3}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"read","path":"C:/x"}"#).is_err());
    }

    #[test]
    fn content_must_be_of_its_kind() {
        let mut zip = b"PK\x03\x04".to_vec();
        zip.extend([0u8; 40]);
        zip.extend(b"PK\x05\x06");
        zip.extend([0u8; 18]);
        assert!(Kind::Bundle.content_ok(&zip));
        assert!(!Kind::Bundle.content_ok(b"PK\x03\x04 no end record at all, just text......"));
        assert!(!Kind::Bundle.content_ok(b"MZ\x90\x00"));
        assert!(!Kind::Bundle.content_ok(br#"{"a":1}"#));
        assert!(Kind::ProbeReport.content_ok(br#"{"chip":"x"}"#));
        assert!(!Kind::ProbeReport.content_ok(b"[1,2]"));
        assert!(!Kind::ScriptReport.content_ok(&zip));
        assert!(!Kind::ScriptReport.content_ok(b"MZ"));
    }
}
