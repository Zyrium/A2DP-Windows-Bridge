//! Service mode's request rules: which client may ask what, beyond the
//! per-method classes of [`ipc::access`]. The server builds a [`Caller`]
//! from the pipe (win_sys::pipe_peer) and calls [`check`] before every
//! request; without service mode nothing here runs.
//!
//! An elevated caller is one whose token is elevated and whose program is
//! ours ([`elevated_programs`]): for the real service only the service
//! folder's core (the elevated helper runs from there), never a program in
//! the user's folder, which software running as the user can replace; for
//! a test instance the core and the UI beside the core. A normal client that
//! gets ipc.needs_admin asks [`run_as_admin`]: the core starts its
//! elevated helper through the agent (`bluecaster-core.exe --request
//! <call> --ticket <t>`, a UAC prompt), the helper sends that call with
//! the ticket, and the call's result goes back to the client that asked.
//! A ticket admits exactly the call it was made for, once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bluecaster_core::settings::Settings as CoreSettings;
use ipc::access::{ADMIN_DECLINED, Access, NEEDS_ADMIN, NOT_IN_SERVICE, access};
use ipc::{ErrorInfo, Method, Msg};
use serde_json::Value;

use crate::state::err;

/// What the rules know of the client, from the pipe
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Caller {
    /// Elevated token, and our own program
    pub elevated: bool,
    /// The elevated helper's ticket (only an elevated caller's counts)
    pub ticket: Option<String>,
}

/// The programs whose elevated processes count as BlueCaster's. The real
/// service (`real_service`): only the service folder's core
/// (`service_core`; none known: nobody). Otherwise (a test instance, a
/// core run by hand): the core (`core_exe`, this program) and the UI
/// beside it.
pub(crate) fn elevated_programs(core_exe: &Path, service_core: Option<&Path>, real_service: bool) -> Vec<PathBuf> {
    if real_service {
        service_core.map(Path::to_path_buf).into_iter().collect()
    } else {
        vec![core_exe.to_path_buf(), core_exe.with_file_name(win_sys::service_files::UI_EXE)]
    }
}

/// [`elevated_programs`] of this process: the real service is a core in
/// session 0 that is not a test instance (crate::agent::real_service)
pub(crate) fn own_elevated_programs() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else { return Vec::new() };
    let service = ipc::service_state::service_core();
    let session = win_sys::pipe::process_session_id(std::process::id()).unwrap_or(0);
    elevated_programs(&exe, service.as_deref(), crate::agent::real_service(session))
}

fn ours(image: Option<&Path>, programs: &[PathBuf]) -> bool {
    let Some(image) = image else { return false };
    programs.iter().any(|p| win_sys::service_files::same_path(p, image))
}

impl Caller {
    pub(crate) fn of(id: &win_sys::pipe_peer::ClientIdentity, programs: &[PathBuf], ticket: Option<&str>) -> Caller {
        let elevated = id.elevated && ours(id.image.as_deref(), programs);
        Caller { elevated, ticket: ticket.filter(|_| elevated).map(str::to_owned) }
    }
}

/// What the rules need of the core's state
pub(crate) trait RuleState {
    fn current_settings(&self) -> CoreSettings;
    fn hid_listed(&self, address: &str) -> bool;
}

impl RuleState for crate::state::CoreState {
    fn current_settings(&self) -> CoreSettings {
        self.service.settings()
    }

    fn hid_listed(&self, address: &str) -> bool {
        self.service.hid_devices().iter().any(|d| d.address.eq_ignore_ascii_case(address))
    }
}

pub(crate) fn needs_admin() -> ErrorInfo {
    err(NEEDS_ADMIN, "error.admin_needed")
}

fn not_in_service() -> ErrorInfo {
    err(NOT_IN_SERVICE, "error.not_in_service")
}

/// Whether `call` needs an elevated client in service mode, given the
/// state now (Err: never allowed in service mode)
pub(crate) fn wants_admin(call: &Method, state: &dyn RuleState) -> Result<bool, ErrorInfo> {
    match access(call) {
        Access::Open => Ok(false),
        Access::Admin => Ok(true),
        Access::NotInService => Err(not_in_service()),
        Access::Depends => match call {
            Method::SettingsSet { settings } => {
                Ok(crate::state::admin_settings_change(&state.current_settings(), settings))
            }
            Method::ProfileSave { profile, .. } => {
                // A microphone output ending in .wav makes the core write
                // that file (bluecaster_core::voice::resolve_mic_output)
                if profile.mic_device_id.trim().to_ascii_lowercase().ends_with(".wav") {
                    return Err(not_in_service());
                }
                // The names go into the device list, one line per device
                if [&profile.device_name, &profile.name]
                    .iter()
                    .any(|n| n.contains(|c: char| c == '|' || c.is_control()))
                {
                    return Err(err("profile.invalid", "error.invalid_address_format"));
                }
                Ok(false)
            }
            Method::HidConnect { address } => Ok(!state.hid_listed(address)),
            Method::DiagInfo(params) => Ok(params.as_ref().and_then(|p| p.mask) == Some(false)),
            // Classed Depends without a rule here: needs an administrator
            _ => Ok(true),
        },
    }
}

/// Service mode: whether `caller` may send `call` now. A caller with a
/// ticket may send only that ticket's call, once.
pub(crate) fn check(caller: &Caller, call: &Method, state: &dyn RuleState) -> Result<(), ErrorInfo> {
    if let Some(ticket) = &caller.ticket
        && !claim(ticket, call)
    {
        return Err(err("ipc.bad_ticket", "error.admin_needed"));
    }
    if wants_admin(call, state)? && !caller.elevated {
        return Err(needs_admin());
    }
    Ok(())
}

// ---- The elevated helper's tickets -------------------------------------

/// A ticket not used in this time is gone (the user may take a while on
/// the prompt)
const TICKET_LIFE: Duration = Duration::from_secs(30 * 60);

struct Pending {
    call: Value,
    claimed: bool,
    result: Option<Result<Value, ErrorInfo>>,
    made: Instant,
}

static PENDING: Mutex<Option<HashMap<String, Pending>>> = Mutex::new(None);

fn pending<T>(f: impl FnOnce(&mut HashMap<String, Pending>) -> T) -> T {
    let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let map = p.get_or_insert_with(HashMap::new);
    map.retain(|_, v| v.made.elapsed() < TICKET_LIFE);
    f(map)
}

/// A new ticket for `call`
fn begin(call: &Method) -> Result<String, ErrorInfo> {
    let ticket = win_sys::pipe_peer::random_hex(16).map_err(|_| err("internal", "error.admin_needed"))?;
    let value = serde_json::to_value(call).unwrap_or(Value::Null);
    pending(|m| m.insert(ticket.clone(), Pending { call: value, claimed: false, result: None, made: Instant::now() }));
    Ok(ticket)
}

/// The helper of `ticket` sends `call`: true once, and only for the call
/// the ticket was made for
fn claim(ticket: &str, call: &Method) -> bool {
    let value = serde_json::to_value(call).unwrap_or(Value::Null);
    pending(|m| match m.get_mut(ticket) {
        Some(p) if !p.claimed && p.call == value => {
            p.claimed = true;
            true
        }
        _ => false,
    })
}

/// The result of the ticket's call, for the client that asked
pub(crate) fn finish(ticket: &str, result: &Result<Value, ErrorInfo>) {
    pending(|m| {
        if let Some(p) = m.get_mut(ticket) {
            p.result = Some(result.clone());
        }
    });
}

fn take(ticket: &str) -> Option<Result<Value, ErrorInfo>> {
    pending(|m| m.remove(ticket)).and_then(|p| p.result)
}

/// `app.run_as_admin`: runs `call` through the elevated helper and returns
/// its result. Blocks until the helper exits (the UAC prompt first).
pub(crate) fn run_as_admin(call: &Method, state: &dyn RuleState, service: bool) -> Result<Value, ErrorInfo> {
    let bad = |why: &str| crate::state::bad_request("ipc.bad_request", "app.run_as_admin", why.into());
    if !service {
        return Err(bad("not in service mode"));
    }
    if matches!(call, Method::AppRunAsAdmin { .. }) || !wants_admin(call, state)? {
        return Err(bad("needs no administrator"));
    }
    // Only through an agent (whose test variant refuses), never a prompt
    // of this process's own
    if session_host::hub().is_none() {
        return Err(bad("no session agent"));
    }
    let request = serde_json::to_string(call).map_err(|e| bad(&e.to_string()))?;
    let ticket = begin(call)?;
    tracing::info!("run as administrator: the helper starts");
    let ran = session_host::elevation::core_request(&request, &ticket);
    let result = take(&ticket);
    match ran {
        Ok(code) => {
            tracing::info!("run as administrator: helper exit {code}, answered {}", result.is_some());
            result.unwrap_or_else(|| Err(err("ipc.admin_failed", "error.admin_needed")))
        }
        Err(session_host::elevation::ElevatedError::Cancelled) => Err(err(ADMIN_DECLINED, "error.admin_declined")),
        // The reason may name a program's path: not in the log
        Err(_) => {
            tracing::warn!("run as administrator: the helper did not run");
            Err(ErrorInfo {
                code: "ipc.admin_failed".into(),
                message: Msg { key: "error.admin_needed".into(), args: Vec::new() },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        settings: CoreSettings,
        listed: Vec<String>,
    }

    impl RuleState for Fake {
        fn current_settings(&self) -> CoreSettings {
            self.settings.clone()
        }
        fn hid_listed(&self, address: &str) -> bool {
            self.listed.iter().any(|a| a.eq_ignore_ascii_case(address))
        }
    }

    fn fake() -> Fake {
        Fake { settings: CoreSettings::default(), listed: vec!["AA:BB:CC:DD:EE:01".into()] }
    }

    const PLAIN: Caller = Caller { elevated: false, ticket: None };
    const ADMIN: Caller = Caller { elevated: true, ticket: None };

    fn code(r: Result<(), ErrorInfo>) -> Option<String> {
        r.err().map(|e| e.code)
    }

    /// Every method that needs an administrator: refused for a normal
    /// client, allowed for an elevated one
    #[test]
    fn admin_methods_need_an_elevated_client() {
        let s = fake();
        let addr = || "AA:BB:CC:DD:EE:02".to_string();
        let admin_calls = [
            Method::DevicePairing { address: addr() },
            Method::DeviceImportWindowsPairings { adapter: addr(), addresses: None },
            Method::DevCompareWindowsKeys { adapter: addr() },
            Method::AdapterAssignWinusb { instance_id: "x".into() },
            Method::AdapterRestoreDriver { instance_id: "x".into() },
            Method::AdapterProbe { instance_id: "x".into() },
            Method::AdapterRunScript { instance_id: "x".into(), script: String::new(), allow_writes: false },
            Method::AudioInstallVbCable,
            Method::AudioUninstallCable { hifi: false },
            Method::DiagCaptureStart,
            Method::DiagBundle(None),
            Method::ReceiverConnect { target: "auto".into() },
            Method::HidEnable { code: "1".into(), other_input_at_hand: true, risk_understood: true },
            Method::HidAdd { address: addr(), name: "k".into(), le: None },
            Method::HidRemove { address: addr() },
            // Not on the list: a new device
            Method::HidConnect { address: addr() },
            Method::DiagInfo(Some(ipc::DiagInfoParams { mask: Some(false) })),
        ];
        for call in &admin_calls {
            assert_eq!(code(check(&PLAIN, call, &s)).as_deref(), Some(NEEDS_ADMIN), "{call:?}");
            assert_eq!(check(&ADMIN, call, &s), Ok(()), "{call:?}");
        }
        let open = [
            Method::StateSnapshot,
            Method::StreamStop,
            Method::HidDisable,
            Method::HidStatus,
            Method::HidCheckCancel { address: addr() },
            Method::HidCheckDismiss,
            Method::HidConnect { address: "aa:bb:cc:dd:ee:01".into() },
            Method::DiagInfo(None),
            Method::DiagCaptureStop,
            Method::ReceiverDisconnect,
            // The code turns nothing on: hid.enable asks the administrator
            Method::HidCode,
            // The user's Exit: the service runs on
            Method::AppClose,
        ];
        for call in &open {
            assert_eq!(check(&PLAIN, call, &s), Ok(()), "{call:?}");
        }
        // Never in service mode, elevated or not
        for call in [Method::AppQuit, Method::AppQuitForUpdate] {
            assert_eq!(code(check(&ADMIN, &call, &s)).as_deref(), Some(NOT_IN_SERVICE));
        }
    }

    fn settings_call(f: impl FnOnce(&mut ipc::Settings)) -> Method {
        let mut s = crate::state::test_ipc_settings(&CoreSettings::default());
        f(&mut s);
        Method::SettingsSet { settings: Box::new(s) }
    }

    /// settings.set: ordinary settings for anyone, developer mode and what
    /// it sets only for an elevated client
    #[test]
    fn settings_set_looks_at_each_field() {
        let s = fake();
        let unchanged = settings_call(|_| {});
        assert_eq!(check(&PLAIN, &unchanged, &s), Ok(()));
        let theme = settings_call(|x| x.theme = "dark".into());
        assert_eq!(check(&PLAIN, &theme, &s), Ok(()));
        let ordinary = settings_call(|x| {
            x.media_keys = !x.media_keys;
            x.auto_reconnect = !x.auto_reconnect;
            x.language = "ja".into();
            // Among the adapters an administrator gave to BlueCaster
            x.adapter_id = "USB/VID_0001&PID_0002/1".into();
            x.auracast_adapter_id = "USB/VID_0001&PID_0002/2".into();
        });
        assert_eq!(check(&PLAIN, &ordinary, &s), Ok(()));
        let admin_only: Vec<(&str, Method)> = vec![
            ("developer mode", settings_call(|x| x.developer_mode = true)),
            ("dev_debug", settings_call(|x| x.dev_debug.mtk_command = "01 02".into())),
            ("dev_overrides", settings_call(|x| x.dev_overrides.rtp_marker = !x.dev_overrides.rtp_marker)),
            ("dev_log", settings_call(|x| x.dev_log.keep_days += 1)),
            ("custom firmware", settings_call(|x| x.realtek_custom_firmware = !x.realtek_custom_firmware)),
            ("chip", settings_call(|x| x.bt_chip_pid = 0x1234)),
            ("firmware stem", settings_call(|x| x.bt_chip_fw_stem = "x".into())),
            ("afh", settings_call(|x| x.afh = "other".into())),
            ("receiver", settings_call(|x| x.remote_sink = "10.0.0.1".into())),
        ];
        for (what, call) in &admin_only {
            assert_eq!(code(check(&PLAIN, call, &s)).as_deref(), Some(NEEDS_ADMIN), "{what}");
            assert_eq!(check(&ADMIN, call, &s), Ok(()), "{what}");
        }
        // Developer mode on already: the same settings sent again pass
        let on = Fake { settings: CoreSettings { debug_mode: true, ..CoreSettings::default() }, listed: Vec::new() };
        assert_eq!(check(&PLAIN, &settings_call(|x| x.developer_mode = true), &on), Ok(()));
        assert_eq!(
            code(check(&PLAIN, &settings_call(|x| x.developer_mode = false), &on)).as_deref(),
            Some(NEEDS_ADMIN)
        );
    }

    fn profile(mic: &str, name: &str) -> Method {
        let mut p = crate::state::test_ipc_profile();
        p.name = "p".into();
        p.device_address = "AA:BB:CC:DD:EE:03".into();
        p.device_name = name.into();
        p.mic_device_id = mic.into();
        Method::ProfileSave { profile: Box::new(p), original_name: None }
    }

    /// No client names a file for the service to write, elevated or not
    #[test]
    fn file_paths_are_refused() {
        let s = fake();
        assert_eq!(check(&PLAIN, &profile("", "Buds"), &s), Ok(()));
        assert_eq!(check(&PLAIN, &profile("{0.0.0.00000000}.{1234}", "Buds"), &s), Ok(()));
        for mic in [r"C:\Windows\x.wav", "out.WAV ", "mic.wav"] {
            assert_eq!(code(check(&ADMIN, &profile(mic, "Buds"), &s)).as_deref(), Some(NOT_IN_SERVICE), "{mic}");
        }
        for name in ["a|b", "a\nAA:BB:CC:DD:EE:04|x"] {
            assert_eq!(code(check(&PLAIN, &profile("", name), &s)).as_deref(), Some("profile.invalid"), "{name:?}");
        }
    }

    #[test]
    fn a_ticket_admits_its_call_once() {
        let s = fake();
        let call = Method::DiagBundle(None);
        let ticket = begin(&call).unwrap();
        let helper = Caller { elevated: true, ticket: Some(ticket.clone()) };
        // Another call with that ticket: refused (and the ticket stays)
        assert!(check(&helper, &Method::AudioInstallVbCable, &s).is_err());
        assert_eq!(check(&helper, &call, &s), Ok(()));
        assert!(check(&helper, &call, &s).is_err(), "only once");
        finish(&ticket, &Ok(Value::from(7)));
        assert_eq!(take(&ticket), Some(Ok(Value::from(7))));
        assert_eq!(take(&ticket), None);
        // Unknown ticket
        let stranger = Caller { elevated: true, ticket: Some("00".into()) };
        assert!(check(&stranger, &call, &s).is_err());
    }

    #[test]
    fn an_elevated_caller_runs_our_program() {
        // A test instance (or a core run by hand): the core and the UI
        // beside the core
        let core = &elevated_programs(Path::new(r"C:\Program Files\BlueCaster\bluecaster-core.exe"), None, false);
        let id = |elevated: bool, image: &str| win_sys::pipe_peer::ClientIdentity {
            pid: 1,
            session_id: 1,
            user_sid: "S-1-5-21-1".into(),
            elevated,
            integrity: win_sys::pipe_peer::INTEGRITY_HIGH,
            image: Some(image.into()),
        };
        assert!(Caller::of(&id(true, r"c:\program files\bluecaster\bluecaster-core.exe"), core, None).elevated);
        assert!(Caller::of(&id(true, r"C:\Program Files\BlueCaster\bluecaster.exe"), core, None).elevated);
        assert!(!Caller::of(&id(false, r"C:\Program Files\BlueCaster\bluecaster.exe"), core, None).elevated);
        assert!(!Caller::of(&id(true, r"C:\Windows\System32\cmd.exe"), core, None).elevated);
        // A ticket counts only for an elevated caller
        assert_eq!(Caller::of(&id(false, r"C:\x\bluecaster.exe"), core, Some("ab")).ticket, None);
        assert_eq!(
            Caller::of(&id(true, r"C:\Program Files\BlueCaster\bluecaster.exe"), core, Some("ab")).ticket.as_deref(),
            Some("ab")
        );
    }

    /// The real service: only the service folder's core is an elevated
    /// caller. An elevated UI, or an elevated core, from the user's
    /// per-user installation is not: software running as the user could
    /// have replaced those files.
    #[test]
    fn the_real_service_admits_only_the_service_folders_core() {
        let user_core = Path::new(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe");
        let svc = Path::new(r"C:\Program Files\BlueCaster Service\bluecaster-core.exe");
        // The service itself runs from the service folder
        let programs = elevated_programs(svc, Some(svc), true);
        assert_eq!(programs, [svc.to_path_buf()]);
        let id = |elevated: bool, image: &str| win_sys::pipe_peer::ClientIdentity {
            pid: 1,
            session_id: 1,
            user_sid: "S-1-5-21-1".into(),
            elevated,
            integrity: win_sys::pipe_peer::INTEGRITY_HIGH,
            image: Some(image.into()),
        };
        let svc_text = svc.to_str().unwrap();
        assert!(Caller::of(&id(true, svc_text), &programs, None).elevated);
        assert!(Caller::of(&id(true, &svc_text.to_lowercase()), &programs, Some("ab")).ticket.is_some());
        assert!(!Caller::of(&id(false, svc_text), &programs, None).elevated);
        let user_ui = r"C:\Users\u\AppData\Local\BlueCaster\bluecaster.exe";
        assert!(!Caller::of(&id(true, user_ui), &programs, None).elevated, "elevated UI from the user's folder");
        assert!(!Caller::of(&id(true, user_core.to_str().unwrap()), &programs, Some("ab")).elevated);
        // A UI copied into the service folder would not count either
        let svc_ui = r"C:\Program Files\BlueCaster Service\bluecaster.exe";
        assert!(!Caller::of(&id(true, svc_ui), &programs, None).elevated);
        // Even a service started some other way, from the user's folder
        let odd = elevated_programs(user_core, Some(svc), true);
        assert!(!Caller::of(&id(true, user_core.to_str().unwrap()), &odd, None).elevated);
        // Program Files not known: nobody is elevated
        assert!(elevated_programs(svc, None, true).is_empty());
    }

    #[test]
    fn run_as_admin_takes_only_calls_that_need_it() {
        let s = fake();
        let e = run_as_admin(&Method::StateSnapshot, &s, true).unwrap_err();
        assert_eq!(e.code, "ipc.bad_request");
        assert_eq!(run_as_admin(&Method::HidCode, &s, true).unwrap_err().code, "ipc.bad_request");
        assert!(run_as_admin(&Method::HidDisable, &s, true).is_err());
        assert!(run_as_admin(&Method::AudioInstallVbCable, &s, false).is_err());
        assert_eq!(run_as_admin(&Method::AppQuit, &s, true).unwrap_err().code, NOT_IN_SERVICE);
    }
}
