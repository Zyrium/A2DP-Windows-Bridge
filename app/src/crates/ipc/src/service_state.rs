//! The service add-on as an unelevated program sees it, and the checks
//! every client makes of the service pipe.
//!
//! The service's files live in their own folder under Program Files
//! (`<Program Files>\BlueCaster Service`), a copy placed there by
//! `bluecaster-core.exe --service-setup` with an administrator's approval;
//! only an administrator can change them. The UI stays in the user's own
//! installation, which software running as the user can change. So for the
//! real service every trust check requires the service folder's core,
//! never "the core beside me" ([`expected_core`]).
//!
//! [`service_state`] is read-only: the service control manager's record
//! (read-only queries), the folder's core's version resource.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use crate::access::ServiceRecord;

pub use win_sys::service_files::{CORE_EXE, SERVICE_FOLDER_NAME, same_path, service_folder_in};

/// What an unelevated program can know about the service add-on
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceState {
    NotInstalled,
    Installed {
        /// The service folder's core's version ("3.0.3", "3.0.0-beta.4");
        /// empty when it cannot be read
        version: String,
        running: bool,
        /// The registered program is exactly the service folder's core
        trusted_path: bool,
    },
}

/// `<program_files>\BlueCaster Service\bluecaster-core.exe`
pub fn service_core_in(program_files: &Path) -> PathBuf {
    service_folder_in(program_files).join(CORE_EXE)
}

/// The service folder (Program Files from the known-folder API), or None
/// when Windows does not say where Program Files is
pub fn service_folder() -> Option<PathBuf> {
    win_sys::service_files::service_folder().ok()
}

/// The service folder's core, the only program the real service may run
pub fn service_core() -> Option<PathBuf> {
    win_sys::service_files::service_core().ok()
}

/// The state from what the manager says (`record`), the expected program
/// and the version read from the service folder's core (`version_of`:
/// called only when the service is installed)
pub fn decide(
    record: &ServiceRecord,
    expected: Option<&Path>,
    version_of: impl FnOnce(&Path) -> Option<String>,
) -> ServiceState {
    match record {
        ServiceRecord::NotInstalled => ServiceState::NotInstalled,
        ServiceRecord::Unreadable => ServiceState::Installed {
            version: expected.and_then(version_of).unwrap_or_default(),
            running: false,
            trusted_path: false,
        },
        ServiceRecord::Installed { command_line, pid } => {
            let program = crate::access::service_program(command_line);
            let trusted_path = matches!((program, expected), (Some(p), Some(e)) if same_path(p, e));
            ServiceState::Installed {
                version: expected.and_then(version_of).unwrap_or_default(),
                running: pid.is_some(),
                trusted_path,
            }
        }
    }
}

/// What the service control manager says of the BlueCaster service
/// (read-only queries)
pub fn service_record() -> ServiceRecord {
    use win_sys::service_query::{ServiceInfo, query};
    match query(crate::access::SERVICE_NAME) {
        Ok(ServiceInfo::NotInstalled) => ServiceRecord::NotInstalled,
        Ok(ServiceInfo::Denied) | Err(_) => ServiceRecord::Unreadable,
        Ok(ServiceInfo::Found { command_line, pid }) => ServiceRecord::Installed { command_line, pid },
    }
}

/// The service add-on's state on this PC (read-only)
pub fn service_state() -> ServiceState {
    let record = service_record();
    let expected = service_core();
    decide(&record, expected.as_deref(), win_sys::service_files::product_version)
}

// ---- Which core a client trusts ------------------------------------------

/// Whether this process is a test instance (BLUECASTER_INSTANCE): it talks
/// to a core of its own, never to the real service
pub fn test_instance() -> bool {
    !crate::instance_suffix().is_empty()
}

/// The core a check requires: for the real service (not a test instance)
/// the service folder's core; for a test instance `beside` (the core next
/// to the checking program, as before the service folder)
pub fn expected_core(beside: &Path) -> Result<PathBuf, &'static str> {
    let service = service_core();
    win_sys::service_files::required_core(beside, service.as_deref(), !test_instance()).map(Path::to_path_buf)
}

/// Every check a client makes of the service pipe it opened, before it
/// trusts anything the server sends: the server's program is
/// [`expected_core`] (`beside`: the core next to the client's own
/// program, used only by a test instance), and for the real service it
/// runs in session 0 and is the process the service control manager runs
/// as the service, configured with that same program
/// ([`crate::access::service_server_trusted`]). The UI, the elevated
/// helper and anyone else use this one function.
pub fn check_service_pipe(pipe: std::os::windows::io::BorrowedHandle<'_>, beside: &Path) -> Result<(), String> {
    let server = win_sys::pipe_peer::server_process(pipe).map_err(|e| e.to_string())?;
    let test = test_instance();
    let expected = expected_core(beside)?;
    // A test instance has no service: not asked
    let record = if test { ServiceRecord::NotInstalled } else { service_record() };
    crate::access::service_server_trusted(
        server.image.as_deref(),
        server.session_id,
        server.pid,
        &record,
        &expected,
        test,
    )
    .map_err(str::to_owned)
}

// ---- The version rule ----------------------------------------------------

/// The reject reason when the service is older than the app: the service
/// needs an update (the administrator prompt of `--service-setup update`)
pub const SERVICE_NEEDS_UPDATE: &str = "ipc.service_needs_update";
/// The reject reason when the app is older than the service: the app needs
/// an update (or the service a matching version)
pub const APP_OLDER_THAN_SERVICE: &str = "ipc.app_older_than_service";

/// The version in a hello name ("bluecaster-ui/3.0.3" -> "3.0.3")
pub fn client_version(client: &str) -> Option<&str> {
    client.split_once('/').map(|(_, v)| v).filter(|v| !v.is_empty())
}

/// SemVer precedence of two versions ("3.0.3", "3.0.0-beta.4",
/// "3.0.0-beta.4.alpha.1"): numbers numerically, a pre-release before its
/// release, pre-release fields one by one (numeric below alphanumeric, a
/// longer list above its prefix: beta.4.alpha.1 comes after beta.4, as
/// the project numbers its alphas). Build metadata (+...) is ignored. None
/// when either is not a version.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    fn parse(v: &str) -> Option<([u64; 3], Vec<&str>)> {
        let v = v.trim().split('+').next()?;
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, p.split('.').collect::<Vec<_>>()),
            None => (v, Vec::new()),
        };
        let mut nums = [0u64; 3];
        let mut parts = core.split('.');
        for n in &mut nums {
            *n = parts.next()?.parse().ok()?;
        }
        if parts.next().is_some() || pre.iter().any(|p| p.is_empty()) {
            return None;
        }
        Some((nums, pre))
    }
    let (an, ap) = parse(a)?;
    let (bn, bp) = parse(b)?;
    let by_core = an.cmp(&bn);
    if by_core != Ordering::Equal {
        return Some(by_core);
    }
    Some(match (ap.is_empty(), bp.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (x, y) in ap.iter().zip(&bp) {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return Some(o);
                }
            }
            ap.len().cmp(&bp.len())
        }
    })
}

/// The service's answer to a UI of `app` version: None when it is the
/// service's own version, else the reject reason. Anything but the same
/// version is refused; a version that cannot be compared counts as an
/// older app.
pub fn version_reject(service: &str, app: &str) -> Option<&'static str> {
    if service == app {
        return None;
    }
    match compare_versions(service, app) {
        Some(Ordering::Equal) => None,
        Some(Ordering::Less) => Some(SERVICE_NEEDS_UPDATE),
        Some(Ordering::Greater) | None => Some(APP_OLDER_THAN_SERVICE),
    }
}

/// Whether a reject reason is one of the version rule's, and which
pub fn version_reject_code(reason: &str) -> Option<&'static str> {
    [SERVICE_NEEDS_UPDATE, APP_OLDER_THAN_SERVICE].into_iter().find(|c| reason == *c)
}

// ---- The per-user installation (for the agent) ---------------------------

/// The per-user installation's registration under HKCU: the uninstall key
/// Inno Setup writes for BlueCaster's AppId (installer/BlueCaster.iss,
/// AppGuid)
pub const UNINSTALL_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{C7A109ED-F618-4214-8186-E4472C3BDD10}_is1";
/// Its values that name the folder, in the order they are read
pub const INSTALL_LOCATION_VALUES: [&str; 2] = ["InstallLocation", "Inno Setup: App Path"];

/// `<folder>\bluecaster.exe` from an installation folder read from the
/// registry: an absolute path on a local drive letter (no `..`, not a UNC
/// or device path), or None
pub fn ui_in_install_folder(folder: &str) -> Option<PathBuf> {
    use std::path::{Component, Prefix};
    let folder = folder.trim().trim_matches('"');
    let p = Path::new(folder);
    let local_disk =
        matches!(p.components().next(), Some(Component::Prefix(pre)) if matches!(pre.kind(), Prefix::Disk(_)));
    if !local_disk || !p.has_root() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(p.join(win_sys::service_files::UI_EXE))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVC: &str = r"C:\Program Files\BlueCaster Service\bluecaster-core.exe";

    fn installed(cmd: &str, pid: Option<u32>) -> ServiceRecord {
        ServiceRecord::Installed { command_line: cmd.into(), pid }
    }

    #[test]
    fn the_state_from_the_managers_record() {
        let exp = Path::new(SVC);
        let v = |_: &Path| Some("3.0.3".to_string());
        assert_eq!(decide(&ServiceRecord::NotInstalled, Some(exp), v), ServiceState::NotInstalled);
        let quoted = format!("\"{SVC}\" --service");
        assert_eq!(
            decide(&installed(&quoted, Some(4)), Some(exp), v),
            ServiceState::Installed { version: "3.0.3".into(), running: true, trusted_path: true }
        );
        assert_eq!(
            decide(&installed(&quoted, None), Some(exp), |_| None),
            ServiceState::Installed { version: String::new(), running: false, trusted_path: true }
        );
        // The stage 4 install: the program in the user's folder
        let user = r#""C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe" --service"#;
        assert_eq!(
            decide(&installed(user, Some(4)), Some(exp), v),
            ServiceState::Installed { version: "3.0.3".into(), running: true, trusted_path: false }
        );
        assert!(matches!(
            decide(&ServiceRecord::Unreadable, Some(exp), v),
            ServiceState::Installed { trusted_path: false, running: false, .. }
        ));
        // Program Files unknown: nothing is the trusted path
        assert!(matches!(
            decide(&installed(&quoted, None), None, v),
            ServiceState::Installed { trusted_path: false, .. }
        ));
    }

    #[test]
    fn service_core_paths() {
        assert_eq!(service_core_in(Path::new(r"C:\Program Files")), Path::new(SVC));
        // On this PC (read-only)
        assert!(service_core().unwrap().ends_with(r"BlueCaster Service\bluecaster-core.exe"));
        // The real state can be read without rights (whatever it is)
        let _ = service_state();
    }

    #[test]
    fn versions_compare_as_the_project_numbers_them() {
        use Ordering::*;
        let c = |a, b| compare_versions(a, b);
        assert_eq!(c("3.0.3", "3.0.3"), Some(Equal));
        assert_eq!(c("3.0.3", "3.0.10"), Some(Less));
        assert_eq!(c("3.1.0", "3.0.10"), Some(Greater));
        assert_eq!(c("3.0.0-beta.4", "3.0.0"), Some(Less));
        assert_eq!(c("3.0.0-beta.10", "3.0.0-beta.9"), Some(Greater));
        // Alphas come after their beta
        assert_eq!(c("3.0.0-beta.4.alpha.1", "3.0.0-beta.4"), Some(Greater));
        assert_eq!(c("3.0.0-beta.4.alpha.1", "3.0.0-beta.5"), Some(Less));
        assert_eq!(c("3.0.0-alpha", "3.0.0-beta"), Some(Less));
        assert_eq!(c("3.0.0-1", "3.0.0-alpha"), Some(Less));
        assert_eq!(c("3.0.3+abc", "3.0.3"), Some(Equal));
        assert_eq!(c("test", "3.0.3"), None);
        assert_eq!(c("3.0", "3.0.3"), None);
        assert_eq!(c("3.0.3.1", "3.0.3"), None);
        assert_eq!(c("3.0.0-", "3.0.0"), None);
    }

    #[test]
    fn only_the_same_version_is_admitted() {
        assert_eq!(version_reject("3.0.3", "3.0.3"), None);
        assert_eq!(version_reject("3.0.3", "3.0.4"), Some(SERVICE_NEEDS_UPDATE));
        assert_eq!(version_reject("3.0.3", "3.0.2"), Some(APP_OLDER_THAN_SERVICE));
        assert_eq!(version_reject("3.0.0-beta.4", "3.0.0-beta.4.alpha.1"), Some(SERVICE_NEEDS_UPDATE));
        assert_eq!(version_reject("3.0.3", "test"), Some(APP_OLDER_THAN_SERVICE));
        assert_eq!(version_reject_code(SERVICE_NEEDS_UPDATE), Some(SERVICE_NEEDS_UPDATE));
        assert_eq!(version_reject_code(APP_OLDER_THAN_SERVICE), Some(APP_OLDER_THAN_SERVICE));
        assert_eq!(version_reject_code("expected hello"), None);
        assert_eq!(client_version("bluecaster-ui/3.0.3"), Some("3.0.3"));
        assert_eq!(client_version("bluecaster-ui/"), None);
        assert_eq!(client_version("encodeck"), None);
    }

    #[test]
    fn the_expected_core_of_a_test_instance_is_beside_it() {
        let beside = Path::new(r"C:\dev\target\debug\bluecaster-core.exe");
        let got = expected_core(beside).unwrap();
        if test_instance() {
            assert_eq!(got, beside);
        } else {
            assert!(got.ends_with(r"BlueCaster Service\bluecaster-core.exe"), "{}", got.display());
        }
    }

    #[test]
    fn install_folders_from_the_registry() {
        let ui = |s: &str| ui_in_install_folder(s).map(|p| p.display().to_string());
        assert_eq!(
            ui(r"C:\Users\u\AppData\Local\BlueCaster\").as_deref(),
            Some(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster.exe")
        );
        assert_eq!(ui(r#""D:\Apps\BlueCaster""#).as_deref(), Some(r"D:\Apps\BlueCaster\bluecaster.exe"));
        assert_eq!(ui(r"\\server\share\BlueCaster"), None);
        assert_eq!(ui(r"BlueCaster"), None);
        assert_eq!(ui(r"C:BlueCaster"), None);
        assert_eq!(ui(r"C:\Users\u\..\x"), None);
        assert_eq!(ui(r"\\?\C:\x"), None);
        assert_eq!(ui(""), None);
    }

    /// The key the agent reads is the one Setup writes for its AppId
    #[test]
    fn the_uninstall_key_matches_the_installer() {
        let iss = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../installer/BlueCaster.iss"))
            .unwrap();
        let guid = iss
            .lines()
            .find_map(|l| l.trim().strip_prefix("#define AppGuid \""))
            .and_then(|r| r.strip_suffix('"'))
            .unwrap();
        assert_eq!(UNINSTALL_KEY, format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{{{guid}}}_is1"));
        assert!(iss.contains("PrivilegesRequired=lowest"), "Setup writes the key under HKCU only when per user");
    }
}
