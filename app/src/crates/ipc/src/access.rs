//! Service mode: who may ask what. Once the core runs as a service, it
//! runs with more privilege than the processes that connect to it, so
//! every request is classified ([`access`]):
//!
//! - [`Access::Open`]: any client connected from an interactive session.
//! - [`Access::Admin`]: only a client whose token is elevated and whose
//!   program is BlueCaster's own (the core checks both from the pipe, see
//!   `win_sys::pipe_peer`). The UAC prompt the user answered to start that
//!   client is the consent: Windows shows it on the secure desktop, where
//!   software running as the user cannot click it. A normal client gets
//!   [`NEEDS_ADMIN`] and asks the core to run the same request through the
//!   elevated helper (`app.run_as_admin`, `bluecaster-core.exe --request`).
//! - [`Access::NotInService`]: refused in service mode ([`NOT_IN_SERVICE`]).
//! - [`Access::Depends`]: the parameters decide (the core's rules: which
//!   settings change, whether a profile names a file, whether a keyboard
//!   is on the list already, whether the text is masked).
//!
//! [`access`] matches every method without a wildcard: a new method does
//! not compile until it has a class. Without service mode nothing here
//! applies.

use std::path::Path;

use crate::Method;

/// The error code: the request needs an elevated client
pub const NEEDS_ADMIN: &str = "ipc.needs_admin";
/// The error code: the request is not available in service mode
pub const NOT_IN_SERVICE: &str = "ipc.not_in_service";
/// The error code: the administrator prompt was declined
pub const ADMIN_DECLINED: &str = "ipc.admin_declined";

/// How a request is allowed in service mode
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Open,
    Admin,
    NotInService,
    Depends,
}

/// The class of `m` in service mode
pub fn access(m: &Method) -> Access {
    use Access::*;
    match m {
        Method::StateSnapshot
        | Method::StreamStart { .. }
        | Method::StreamStop
        | Method::StreamStartCustom { .. }
        // Developer mode only, and developer mode needs an elevated client
        | Method::StreamStartBroadcast { .. }
        | Method::StreamSetVolume { .. }
        | Method::StreamVolumeStep { .. } => Open,
        Method::DeviceScan { .. }
        | Method::DeviceScanStop
        | Method::DeviceWindowsPaired
        | Method::DeviceServices { .. }
        | Method::DeviceWindowsPairedThrough { .. }
        // Audio devices: losing one costs a pairing, nothing more
        | Method::DeviceForget { .. }
        | Method::DeviceRename { .. }
        | Method::DeviceForgetCodecs { .. } => Open,
        // Returns link keys
        Method::DevicePairing { .. }
        // Driver flows: the driver helper, Windows' keys
        | Method::DeviceImportWindowsPairings { .. }
        | Method::DevCompareWindowsKeys { .. } => Admin,
        // A .wav microphone output makes the core write that file
        Method::ProfileSave { .. } => Depends,
        Method::ProfileDelete { .. } | Method::MediaEqPreview { .. } | Method::ProfileSetActive { .. } => Open,
        // For tools: a switch of a saved profile, by its name. No path, no
        // device, nothing an ordinary client could not set with the window
        Method::ProfileSetHeadsetMic { .. } | Method::ProfileSetMicMuted { .. } | Method::ProfileSetMediaEq { .. } => {
            Open
        }
        // Developer mode only, as stream.start_broadcast: the mix of a
        // broadcast and its source (the test tone or system audio)
        Method::AuracastMixGet { .. }
        | Method::AuracastMixSet { .. }
        | Method::AuracastMixPresetSave { .. }
        | Method::AuracastMixPresetDelete { .. }
        | Method::AuracastSetSource { .. } => Open,
        // Developer mode and what it sets, the chip and firmware settings
        Method::SettingsSet { .. } => Depends,
        Method::AdapterList
        | Method::AdapterWindowsDevices { .. }
        | Method::AdapterRestart
        // Reads a script the client sends; runs nothing
        | Method::AdapterCheckScript { .. }
        | Method::AdapterFirmwareFiles
        // Only the files of the known list, from the known source
        | Method::AdapterDownloadFirmware { .. } => Open,
        Method::AdapterAssignWinusb { .. }
        | Method::AdapterRestoreDriver { .. }
        // Raw commands to the chip
        | Method::AdapterProbe { .. }
        | Method::AdapterRunScript { .. } => Admin,
        Method::AudioDevices | Method::AudioApps | Method::AudioMixer | Method::AudioMixerSet { .. } => Open,
        // Installs or removes a driver
        Method::AudioInstallVbCable | Method::AudioUninstallCable { .. } => Admin,
        // Log lines hold counts and states only
        Method::DiagSubscribe { .. } => Open,
        // HCI captures hold link keys and typed input
        Method::DiagCaptureStart | Method::DiagBundle(_) => Admin,
        // Stopping is always safe
        Method::DiagCaptureStop => Open,
        // Unmasked: addresses, names, the user name, serial numbers
        Method::DiagInfo(_) => Depends,
        Method::DiagWifiScan
        | Method::DiagFlow
        | Method::DiagRecentLogs { .. }
        | Method::DiagSetVerbose { .. }
        | Method::DiagCrashesSeen
        | Method::DiagMark { .. } => Open,
        Method::ReceiverDiscover => Open,
        // Connects the service to any host the client names
        Method::ReceiverConnect { .. } => Admin,
        Method::ReceiverDisconnect | Method::ReceiverConfigure { .. } | Method::ReceiverStatus => Open,
        Method::MicTestRecord
        | Method::MicTestStop
        | Method::MicTestPlay
        | Method::MicTestStatus
        | Method::MicTestEnd => Open,
        // Only the device being streamed to, never the headset's own links
        Method::RfcommOpen { .. } | Method::RfcommSend { .. } | Method::RfcommClose { .. } => Open,
        // QCY earbuds control (Fones screen)
        Method::QcyState | Method::QcyRefresh | Method::QcyReconnect | Method::QcySet { .. } => Open,
        // The code only proves that someone read the dialog: it turns
        // nothing on. hid.enable checks it, and that request needs an
        // administrator, so the user is asked once, when the feature is
        // turned on, after the dialog's checks.
        Method::HidCode => Open,
        // Turning keyboards and mice on
        Method::HidEnable { .. } => Admin,
        // The way out: the user can always turn the feature off
        Method::HidDisable | Method::HidStatus => Open,
        // A device not on the list yet would be a new keyboard
        Method::HidAdd { .. } => Admin,
        Method::HidConnect { .. } => Depends,
        // Forgets the pairing; adding it back needs an administrator
        Method::HidRemove { .. } => Admin,
        // Only removes a device being added (the way out, as hid.disable);
        // nothing confirms a device but its input reaching Windows
        Method::HidCheckCancel { .. } | Method::HidCheckDismiss => Open,
        Method::AppCheckUpdate | Method::AppHiddenToTray => Open,
        // Windows starts and stops a service; the UI's Exit closes the UI
        Method::AppQuit | Method::AppQuitForUpdate => NotInService,
        // The user's Exit: the UI and the tray close, the service runs on
        Method::AppClose => Open,
        // The call inside must be one that needs an administrator
        Method::AppRunAsAdmin { .. } => Open,
    }
}

/// The hello name of the elevated helper (`bluecaster-core.exe --request`)
/// that runs the request of `ticket` (from `app.run_as_admin`)
pub fn helper_client_name(version: &str, ticket: &str) -> String {
    format!("bluecaster-request/{version}{TICKET_MARK}{ticket}")
}

const TICKET_MARK: &str = " ticket=";

/// A hello name and the ticket it carries (only the elevated helper's has
/// one); the name alone is what the logs show
pub fn split_client_name(client: &str) -> (&str, Option<&str>) {
    match client.split_once(TICKET_MARK) {
        Some((name, ticket)) if !ticket.is_empty() => (name, Some(ticket)),
        Some((name, _)) => (name, None),
        None => (client, None),
    }
}

/// Whether the process serving the service-mode pipe is the core a client
/// expects: its program is `expected` (for the real service the service
/// folder's core, never the one beside the client: see
/// [`crate::service_state::expected_core`]), and, for the real service
/// (not a test instance), it runs in session 0, where only services run:
/// no program the user starts gets
/// there. A fixed pipe name can be taken by any process that comes first,
/// so a client checks this before it trusts what the server sends.
pub fn server_trusted(
    image: Option<&Path>,
    session: Option<u32>,
    expected: &Path,
    test_instance: bool,
) -> Result<(), &'static str> {
    let same = |a: &Path| a.as_os_str().eq_ignore_ascii_case(expected.as_os_str());
    if !image.is_some_and(same) {
        return Err("another program serves the pipe");
    }
    if !test_instance && session != Some(0) {
        return Err("the pipe's server is not a service");
    }
    Ok(())
}

/// The Windows service's name (the service control manager's key)
pub const SERVICE_NAME: &str = "BlueCaster";

/// What the service control manager says of [`SERVICE_NAME`] (read-only
/// queries, win_sys::service_query)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceRecord {
    NotInstalled,
    /// Installed, but its configuration or state may not be read
    Unreadable,
    Installed {
        /// The configured command line
        command_line: String,
        /// Its process while it runs
        pid: Option<u32>,
    },
}

/// The program of a service's command line: the quoted part, or (not
/// quoted) everything up to the first ".exe" that ends a word, as the
/// service control manager reads it
pub fn service_program(command_line: &str) -> Option<&Path> {
    let s = command_line.trim_start();
    let program = if let Some(rest) = s.strip_prefix('"') {
        &rest[..rest.find('"')?]
    } else {
        let lower = s.to_ascii_lowercase();
        let end = lower
            .match_indices(".exe")
            .map(|(i, _)| i + 4)
            .find(|&e| s[e..].is_empty() || s[e..].starts_with(' '))
            .unwrap_or(s.find(' ').unwrap_or(s.len()));
        &s[..end]
    };
    (!program.is_empty()).then(|| Path::new(program))
}

/// [`server_trusted`], and for the real service (not a test instance) the
/// service control manager's word as well: the service is installed with
/// the program `expected`, and the process serving the pipe (`server_pid`)
/// is the service's running process. A process that only took the pipe's
/// name, even one of our own program started by the user, is not the
/// service's process.
pub fn service_server_trusted(
    image: Option<&Path>,
    session: Option<u32>,
    server_pid: u32,
    service: &ServiceRecord,
    expected: &Path,
    test_instance: bool,
) -> Result<(), &'static str> {
    server_trusted(image, session, expected, test_instance)?;
    if test_instance {
        return Ok(());
    }
    match service {
        ServiceRecord::NotInstalled => Err("the service is not installed"),
        ServiceRecord::Unreadable => Err("the service's record cannot be read"),
        ServiceRecord::Installed { command_line, pid } => {
            let program = service_program(command_line).ok_or("the service has no program")?;
            if !program.as_os_str().eq_ignore_ascii_case(expected.as_os_str()) {
                return Err("the service runs another program");
            }
            if *pid != Some(server_pid) {
                return Err("the pipe's server is not the service's process");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_of_a_command_line() {
        let p = |s: &str| service_program(s).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(
            p(r#""C:\Program Files\BlueCaster\bluecaster-core.exe" --service"#).as_deref(),
            Some(r"C:\Program Files\BlueCaster\bluecaster-core.exe")
        );
        assert_eq!(
            p(r"C:\Program Files\BlueCaster\bluecaster-core.exe --service").as_deref(),
            Some(r"C:\Program Files\BlueCaster\bluecaster-core.exe")
        );
        assert_eq!(p(r"C:\x\a.EXE").as_deref(), Some(r"C:\x\a.EXE"));
        // ".exe" inside a folder name does not end the program
        assert_eq!(p(r"C:\a.exe.d\core.exe -x").as_deref(), Some(r"C:\a.exe.d\core.exe"));
        assert_eq!(p(r#""C:\no end"#), None);
        assert_eq!(p(""), None);
    }

    #[test]
    fn only_the_services_own_process_is_trusted() {
        let core = Path::new(r"C:\Program Files\BlueCaster\bluecaster-core.exe");
        let installed = |pid: Option<u32>| ServiceRecord::Installed {
            command_line: r#""C:\Program Files\BlueCaster\bluecaster-core.exe" --service"#.into(),
            pid,
        };
        let ok = |svc: &ServiceRecord, pid: u32| service_server_trusted(Some(core), Some(0), pid, svc, core, false);
        assert_eq!(ok(&installed(Some(40)), 40), Ok(()));
        // Another process of the same program (session 0 by some other way)
        assert!(ok(&installed(Some(40)), 41).is_err());
        // The service stopped: whoever serves the pipe is not it
        assert!(ok(&installed(None), 40).is_err());
        assert!(ok(&ServiceRecord::NotInstalled, 40).is_err());
        assert!(ok(&ServiceRecord::Unreadable, 40).is_err());
        let other = ServiceRecord::Installed { command_line: r"C:\Temp\bluecaster-core.exe".into(), pid: Some(40) };
        assert!(ok(&other, 40).is_err());
        // The pipe's own checks still come first
        assert!(service_server_trusted(Some(core), Some(1), 40, &installed(Some(40)), core, false).is_err());
        // A test instance has no service
        assert_eq!(service_server_trusted(Some(core), Some(1), 40, &ServiceRecord::NotInstalled, core, true), Ok(()));
    }

    #[test]
    fn some_classes() {
        assert_eq!(access(&Method::StateSnapshot), Access::Open);
        assert_eq!(access(&Method::HidCode), Access::Open);
        let enable = Method::HidEnable { code: "1".into(), other_input_at_hand: true, risk_understood: true };
        assert_eq!(access(&enable), Access::Admin);
        assert_eq!(access(&Method::AppClose), Access::Open);
        assert_eq!(access(&Method::HidDisable), Access::Open);
        assert_eq!(access(&Method::HidCheckCancel { address: String::new() }), Access::Open);
        assert_eq!(access(&Method::HidCheckDismiss), Access::Open);
        assert_eq!(access(&Method::AppQuit), Access::NotInService);
        assert_eq!(access(&Method::HidConnect { address: String::new() }), Access::Depends);
    }

    #[test]
    fn ticket_names() {
        let n = helper_client_name("3.0.3", "ab12");
        assert_eq!(split_client_name(&n), ("bluecaster-request/3.0.3", Some("ab12")));
        assert_eq!(split_client_name("bluecaster-ui/3.0.3"), ("bluecaster-ui/3.0.3", None));
        assert_eq!(split_client_name("x ticket="), ("x", None));
    }

    #[test]
    fn only_our_core_in_session_0_is_trusted() {
        let core = Path::new(r"C:\Program Files\BlueCaster\bluecaster-core.exe");
        let lower = Path::new(r"c:\program files\bluecaster\BLUECASTER-CORE.EXE");
        let other = Path::new(r"C:\Users\u\AppData\Local\Temp\bluecaster-core.exe");
        assert!(server_trusted(Some(lower), Some(0), core, false).is_ok());
        assert!(server_trusted(Some(core), Some(1), core, false).is_err());
        assert!(server_trusted(Some(core), Some(1), core, true).is_ok());
        assert!(server_trusted(Some(other), Some(0), core, false).is_err());
        assert!(server_trusted(None, Some(0), core, true).is_err());
    }
}
