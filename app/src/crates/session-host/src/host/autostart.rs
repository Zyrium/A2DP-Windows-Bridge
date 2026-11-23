//! Group `autostart`: the Run values under HKEY_CURRENT_USER that start the
//! core at logon (`win_sys::autostart`). A core running as a service has
//! another user's HKEY_CURRENT_USER, so the agent, in the user's session,
//! reads and writes them.
//!
//! The core still decides what to write (`state.rs`: the start's plan, the
//! settings toggle); the agent takes only the two value names BlueCaster
//! uses and only command lines that start a `bluecaster-core.exe`.
//! Errors are text: Windows' own error, or what went wrong with the agent.

use serde::{Deserialize, Serialize};

use crate::{LocalHost, RemoteHost};

pub const GROUP: &str = "autostart";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Reply: `Option<String>`
    Get { name: String },
    /// Reply: `()`
    Set { name: String, command: String },
    /// Reply: `bool` (it was there)
    Remove { name: String },
    /// Reply: `bool` (Task Manager had a state for `from`)
    CarryStartupApproved { from: String, to: String },
}

/// The Run values the agent writes for a core
pub fn known_name(name: &str) -> bool {
    name == win_sys::autostart::VALUE_NAME || name == win_sys::autostart::OLD_VALUE_NAME
}

/// A command line the agent writes for a core: one starting the core
pub fn core_command(command: &str) -> bool {
    win_sys::autostart::program_of(command)
        .and_then(|p| p.file_name().map(|n| n.eq_ignore_ascii_case("bluecaster-core.exe")))
        .unwrap_or(false)
}

/// The facade's Run value methods. They block until Windows (or the agent)
/// answers: not for the BTstack thread or an audio thread.
pub trait AutostartHost {
    /// The command line under `name`; None if there is none
    fn autostart_get(&self, name: &str) -> Result<Option<String>, String>;
    fn autostart_set(&self, name: &str, command: &str) -> Result<(), String>;
    /// Ok(false) if there was none
    fn autostart_remove(&self, name: &str) -> Result<bool, String>;
    /// Task Manager's enabled/disabled state of `from` carried over to `to`
    fn autostart_carry_startup_approved(&self, from: &str, to: &str) -> Result<bool, String>;
}

impl AutostartHost for LocalHost {
    fn autostart_get(&self, name: &str) -> Result<Option<String>, String> {
        win_sys::autostart::get(name).map_err(|e| e.to_string())
    }

    fn autostart_set(&self, name: &str, command: &str) -> Result<(), String> {
        win_sys::autostart::set(name, command).map_err(|e| e.to_string())
    }

    fn autostart_remove(&self, name: &str) -> Result<bool, String> {
        win_sys::autostart::remove(name).map_err(|e| e.to_string())
    }

    fn autostart_carry_startup_approved(&self, from: &str, to: &str) -> Result<bool, String> {
        win_sys::autostart::carry_startup_approved(from, to).map_err(|e| e.to_string())
    }
}

/// A failed request as text: the agent's (Windows') error when it sent
/// one, else what went wrong with the agent
fn text(e: crate::HostError) -> String {
    match &e {
        crate::HostError::Remote(w) => w.detail_as::<String>().unwrap_or_else(|| e.to_string()),
        _ => e.to_string(),
    }
}

impl AutostartHost for RemoteHost {
    fn autostart_get(&self, name: &str) -> Result<Option<String>, String> {
        self.call(GROUP, &Request::Get { name: name.into() }).map_err(text)
    }

    fn autostart_set(&self, name: &str, command: &str) -> Result<(), String> {
        self.call(GROUP, &Request::Set { name: name.into(), command: command.into() }).map_err(text)
    }

    fn autostart_remove(&self, name: &str) -> Result<bool, String> {
        self.call(GROUP, &Request::Remove { name: name.into() }).map_err(text)
    }

    fn autostart_carry_startup_approved(&self, from: &str, to: &str) -> Result<bool, String> {
        self.call(GROUP, &Request::CarryStartupApproved { from: from.into(), to: to.into() }).map_err(text)
    }
}

/// [`AutostartHost::autostart_get`] of this process's host
pub fn get(name: &str) -> Result<Option<String>, String> {
    crate::host().autostart_get(name)
}

/// [`AutostartHost::autostart_set`] of this process's host
pub fn set(name: &str, command: &str) -> Result<(), String> {
    crate::host().autostart_set(name, command)
}

/// [`AutostartHost::autostart_remove`] of this process's host
pub fn remove(name: &str) -> Result<bool, String> {
    crate::host().autostart_remove(name)
}

/// [`AutostartHost::autostart_carry_startup_approved`] of this process's host
pub fn carry_startup_approved(from: &str, to: &str) -> Result<bool, String> {
    crate::host().autostart_carry_startup_approved(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_cores_values_and_commands() {
        assert!(known_name("BlueCaster") && known_name("A2DPWB"));
        assert!(!known_name("OneDrive"));
        assert!(core_command(r#""C:\Program Files\BlueCaster\bluecaster-core.exe" --background"#));
        assert!(core_command(r#""C:\x\BlueCaster-Core.EXE""#));
        assert!(!core_command(r#""C:\x\other.exe" bluecaster-core.exe"#));
        assert!(!core_command(""));
    }
}
