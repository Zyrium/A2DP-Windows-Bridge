//! Group `shell`: the tray icon with its menu, balloons and starting the UI,
//! for a core without a tray of its own (`bluecaster-core.exe --headless`).
//!
//! The core decides what the tray shows and what a click does; the agent
//! only shows it and reports what the user did. Everything goes as events
//! (no replies), so nothing waits on the agent:
//!
//! - core to agent ([`ToAgent`]): the tray's model ([`TrayModel`]: tooltip
//!   and menu), a balloon, "start the UI" with the page to open.
//! - agent to core ([`ToCore`]): [`ToCore::Ready`] when a connection's
//!   handler starts (the core sends its current model then: a new agent
//!   shows the tray at once), a menu item chosen, a click on the icon,
//!   Quick Settings opened.
//!
//! The agent shows a tray only after the core sent a model: an agent that
//! serves a core with its own tray (the normal resident mode) shows none.
//! The tray goes when the connection ends (the core exited or crashed) and
//! comes back with the next core's model.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{LocalHost, RemoteHost};

pub const GROUP: &str = "shell";

/// One entry of the tray menu
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrayItem {
    Item { id: String, label: String, enabled: bool },
    Check { id: String, label: String, enabled: bool, checked: bool },
    Separator,
}

/// What the tray shows. No items: no icon at all (service mode, after the
/// user's Exit).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayModel {
    pub tooltip: String,
    pub items: Vec<TrayItem>,
}

impl TrayModel {
    /// The menu's selectable items, by id
    pub fn item_ids(&self) -> impl Iterator<Item = &str> {
        self.items.iter().filter_map(|i| match i {
            TrayItem::Item { id, .. } | TrayItem::Check { id, .. } => Some(id.as_str()),
            TrayItem::Separator => None,
        })
    }
}

/// A balloon from the tray icon
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalloonModel {
    pub title: String,
    pub text: String,
    /// Opened by a click on it (http or https only)
    pub url: Option<String>,
}

/// What the UI opens with: the agent starts `bluecaster.exe` next to its
/// own program with this page's argument only (never one the core names)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiPage {
    Main,
    Flyout,
    QuickSettings,
}

impl UiPage {
    pub fn arg(self) -> Option<&'static str> {
        match self {
            UiPage::Main => None,
            UiPage::Flyout => Some("--flyout"),
            UiPage::QuickSettings => Some("--quick-settings"),
        }
    }
}

/// Events of the core to the agent
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ToAgent {
    Tray { model: TrayModel },
    Balloon { balloon: BalloonModel },
    LaunchUi { page: UiPage },
}

/// Events of the agent to the core
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ToCore {
    /// A new connection: the core sends its model
    Ready,
    /// A menu item of the model was chosen
    Menu { id: String },
    /// A left click on the tray icon
    Click,
    /// Windows' Quick Settings opened (the flyout may open above them)
    QuickSettingsOpened,
}

/// The facade's shell methods. They do not wait: false when there is no
/// one to show it (this process has no shell of its own, or no agent).
pub trait ShellHost {
    /// Shows the tray with `model`, or updates it
    fn show_tray(&self, model: &TrayModel) -> bool;
    /// A balloon from the tray icon
    fn show_balloon(&self, balloon: &BalloonModel) -> bool;
    /// Starts the UI (a running UI gets the page instead)
    fn launch_ui(&self, page: UiPage) -> bool;
}

/// Where the UI's program is, as this process decides it (never from the
/// core: the core sends only the page)
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UiSource {
    /// `bluecaster.exe` next to this program (the per-user installation, a
    /// test build)
    Beside(PathBuf),
    /// This program is the service folder's core (the agent of the real
    /// service): the UI of the user's own per-user installation, from the
    /// folder Setup registered under HKCU
    Installed(PathBuf),
}

/// [`UiSource`] from this program (`own`), the service folder's core and
/// the installation folder the registry names (`registered`: read only
/// when needed)
pub(crate) fn ui_source(
    own: &Path,
    service_core: Option<&Path>,
    registered: impl FnOnce() -> Option<String>,
) -> Option<UiSource> {
    use win_sys::service_files::{UI_EXE, same_path};
    if service_core.is_some_and(|s| same_path(s, own)) {
        let folder = registered()?;
        ipc::service_state::ui_in_install_folder(&folder).map(UiSource::Installed)
    } else {
        own.parent().map(|d| UiSource::Beside(d.join(UI_EXE)))
    }
}

/// The per-user installation's folder, as Setup registered it for this user
fn registered_install_folder() -> Option<String> {
    use ipc::service_state::{INSTALL_LOCATION_VALUES, UNINSTALL_KEY};
    INSTALL_LOCATION_VALUES.iter().find_map(|v| win_sys::service_files::hkcu_string(UNINSTALL_KEY, v))
}

/// Whether the UI found through the registry may be started: a plain file
/// (no link), and in a signed build signed by this program's publisher
/// (the same certificate). HKCU is the user's own, so this only keeps the
/// agent from starting just any program; the agent runs as the user and
/// gains nothing by it.
fn installed_ui_ok(ui: &Path) -> Result<(), &'static str> {
    use win_sys::authenticode::{Policy, check_path, own_signature};
    match win_sys::service_files::is_reparse_point(ui) {
        Ok(false) if ui.is_file() => {}
        Ok(_) => return Err("not a plain file"),
        Err(_) => return Err("not there"),
    }
    let policy = Policy::from_own(&own_signature().map_err(|_| "this program's signature is not readable")?);
    if !policy.checks() {
        return Ok(());
    }
    policy.admits(&check_path(ui).map_err(|_| "its signature is not readable")?)
}

/// The UI's program: next to this program, or for the service's agent the
/// user's per-user installation (checked: [`installed_ui_ok`])
fn ui_program() -> Option<PathBuf> {
    let own = std::env::current_exe().ok()?;
    let service_core = win_sys::service_files::service_core().ok();
    match ui_source(&own, service_core.as_deref(), registered_install_folder) {
        Some(UiSource::Beside(ui)) => Some(ui),
        Some(UiSource::Installed(ui)) => match installed_ui_ok(&ui) {
            Ok(()) => Some(ui),
            Err(why) => {
                tracing::warn!("shell: the installed UI is not used: {why}");
                None
            }
        },
        None => {
            tracing::warn!("shell: no installed UI is registered for this user");
            None
        }
    }
}

/// Starts the UI ([`ui_program`]) with `page`'s argument
pub fn start_ui(page: UiPage) -> bool {
    let Some(ui) = ui_program() else { return false };
    if !ui.exists() {
        tracing::warn!("shell: the UI's program is not there");
        return false;
    }
    let mut cmd = std::process::Command::new(ui);
    cmd.args(page.arg());
    match cmd.spawn() {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("shell: cannot start the UI ({})", e.kind());
            false
        }
    }
}

/// In this process: the resident mode's own tray (resident.rs) shows the
/// tray and balloons; only the UI starts from here
impl ShellHost for LocalHost {
    fn show_tray(&self, _model: &TrayModel) -> bool {
        false
    }

    fn show_balloon(&self, _balloon: &BalloonModel) -> bool {
        false
    }

    fn launch_ui(&self, page: UiPage) -> bool {
        start_ui(page)
    }
}

impl RemoteHost {
    fn shell_event(&self, event: &ToAgent) -> bool {
        self.hub().event(GROUP, crate::wire::to_body(event))
    }
}

impl ShellHost for RemoteHost {
    fn show_tray(&self, model: &TrayModel) -> bool {
        self.shell_event(&ToAgent::Tray { model: model.clone() })
    }

    fn show_balloon(&self, balloon: &BalloonModel) -> bool {
        self.shell_event(&ToAgent::Balloon { balloon: balloon.clone() })
    }

    fn launch_ui(&self, page: UiPage) -> bool {
        self.shell_event(&ToAgent::LaunchUi { page })
    }
}

/// [`ShellHost::show_tray`] of this process's host
pub fn show_tray(model: &TrayModel) -> bool {
    crate::host().show_tray(model)
}

/// [`ShellHost::show_balloon`] of this process's host
pub fn show_balloon(balloon: &BalloonModel) -> bool {
    crate::host().show_balloon(balloon)
}

/// [`ShellHost::launch_ui`] of this process's host
pub fn launch_ui(page: UiPage) -> bool {
    crate::host().launch_ui(page)
}

/// Calls `f` with each event of the agent's shell (on the hub's thread:
/// `f` must not block). Nothing when this process has no agent.
pub fn on_agent_event(f: impl Fn(ToCore) + Send + Sync + 'static) -> bool {
    let Some(hub) = crate::hub() else { return false };
    hub.on_event(GROUP, move |body| match serde_json::from_value::<ToCore>(body) {
        Ok(e) => f(e),
        Err(_) => tracing::debug!("shell: an event of the agent that does not decode"),
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The service's agent starts the user's installed UI, found through
    /// the registry; any other agent the UI beside it. The core never names
    /// the program.
    #[test]
    fn the_ui_is_found_by_the_agent_itself() {
        let svc = Path::new(r"C:\Program Files\BlueCaster Service\bluecaster-core.exe");
        let user = Path::new(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe");
        let reg = || Some(r"C:\Users\u\AppData\Local\BlueCaster\".to_string());
        assert_eq!(
            ui_source(svc, Some(svc), reg),
            Some(UiSource::Installed(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster.exe".into()))
        );
        // Not registered, or registered somewhere odd: no UI
        assert_eq!(ui_source(svc, Some(svc), || None), None);
        assert_eq!(ui_source(svc, Some(svc), || Some(r"\\host\share".into())), None);
        // The per-user installation (or a test build): beside, the
        // registry not even read
        assert_eq!(
            ui_source(user, Some(svc), || panic!("not read")),
            Some(UiSource::Beside(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster.exe".into()))
        );
        assert!(matches!(ui_source(user, None, || panic!("not read")), Some(UiSource::Beside(_))));
    }

    #[test]
    fn a_registered_ui_must_be_a_plain_file() {
        let dir = std::env::temp_dir().join(format!("bc-ui-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(installed_ui_ok(&dir.join("bluecaster.exe")).is_err(), "missing");
        std::fs::create_dir_all(dir.join("bluecaster.exe")).unwrap();
        assert!(installed_ui_ok(&dir.join("bluecaster.exe")).is_err(), "a folder");
        std::fs::remove_dir(dir.join("bluecaster.exe")).unwrap();
        std::fs::write(dir.join("bluecaster.exe"), b"MZ").unwrap();
        // The test program is unsigned: a developer build checks no signature
        assert_eq!(installed_ui_ok(&dir.join("bluecaster.exe")), Ok(()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn events_have_a_stable_shape() {
        let model = TrayModel {
            tooltip: "BlueCaster".into(),
            items: vec![
                TrayItem::Item { id: "open".into(), label: "Open".into(), enabled: true },
                TrayItem::Separator,
                TrayItem::Check { id: "profile:0".into(), label: "X".into(), enabled: true, checked: false },
            ],
        };
        let json = serde_json::to_value(ToAgent::Tray { model: model.clone() }).unwrap();
        assert_eq!(json["op"], "tray");
        assert_eq!(json["model"]["items"][1]["kind"], "separator");
        assert_eq!(serde_json::from_value::<ToAgent>(json).unwrap(), ToAgent::Tray { model: model.clone() });
        assert_eq!(model.item_ids().collect::<Vec<_>>(), ["open", "profile:0"]);
        assert_eq!(serde_json::to_value(ToCore::Menu { id: "exit".into() }).unwrap()["op"], "menu");
        assert_eq!(
            serde_json::to_value(ToAgent::LaunchUi { page: UiPage::QuickSettings }).unwrap()["page"],
            "quick_settings"
        );
        assert_eq!(UiPage::Flyout.arg(), Some("--flyout"));
        assert_eq!(UiPage::Main.arg(), None);
    }

    #[test]
    fn the_local_host_has_no_tray() {
        assert!(!LocalHost.show_tray(&TrayModel::default()));
        assert!(!LocalHost.show_balloon(&BalloonModel { title: String::new(), text: "x".into(), url: None }));
    }
}
