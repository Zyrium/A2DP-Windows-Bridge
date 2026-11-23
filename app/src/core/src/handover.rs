//! Service mode: files the user asked for, handed to the signed-in user.
//! The core's data folder is protected (win_sys::acl), so a diagnostic
//! bundle or a chip report written there is out of the user's reach. The
//! core sends the file's content to the session agent (session_host's
//! `handover` group), which writes it into the user's own folder and
//! answers with the new path: that path goes into the result the UI shows
//! and opens.
//!
//! Which files: only those of requests that need an administrator in
//! service mode (diag.bundle, adapter.probe, adapter.run_script;
//! ipc::access), so each file the user receives is the result of a
//! request an administrator approved. HCI captures are not handed over
//! on their own: they hold pairing keys and typed input, and stopping a
//! capture needs no administrator; they reach the user inside a bundle.
//! The core never reads anything from the user's folder.

use std::path::Path;

use session_host::handover::Kind;

/// The path the user sees for a file the core wrote at `core_path` with
/// `data`: outside service mode that path, in service mode the copy handed
/// to the user. Err: the agent did not take it (`error`: the request's own
/// error).
pub(crate) fn for_user<E>(kind: Kind, core_path: &Path, data: &[u8], error: impl FnOnce() -> E) -> Result<String, E> {
    if !bluecaster_core::paths::service_mode() {
        return Ok(core_path.display().to_string());
    }
    hand_over(kind, data).map_err(|why| {
        tracing::warn!("handover: the file was not given to the user ({why})");
        error()
    })
}

/// The agent's copy of `data` -> its path; Err: why not, for the log (no
/// path, no name)
fn hand_over(kind: Kind, data: &[u8]) -> Result<String, &'static str> {
    use session_host::HostError;
    session_host::handover::hand_over(kind, data).map_err(|e| match e {
        HostError::AgentLost => "the session agent was lost",
        HostError::NoAgent => "no session agent",
        HostError::Timeout => "the session agent did not answer",
        HostError::Remote(_) => "the session agent refused it",
        HostError::Protocol(_) => "the session agent's answer was malformed",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_service_mode_the_core_path_is_the_users() {
        // The unit tests never enter service mode
        assert!(!bluecaster_core::paths::service_mode());
        let p = Path::new(r"C:\Users\u\AppData\Roaming\BlueCaster\logs\probe\probe-1.json");
        let got = for_user(Kind::ProbeReport, p, b"{}", || ()).unwrap();
        assert_eq!(got, p.display().to_string());
    }
}
