//! The core's session-bound work behind one facade, [`SessionHost`]: what
//! needs the signed-in user's Windows session (audio devices, capture,
//! playback, later volume, media keys, the tray). The core calls
//! [`host()`] instead of calling Windows directly. Each process selects
//! once, at start, where that work runs:
//!
//! - [`LocalHost`] (the default): in this process, as before.
//! - [`RemoteHost`] ([`use_session_agent`]): in the session agent, a
//!   second process (`bluecaster-core.exe --agent`) in the user's session,
//!   reached over a named pipe that the core serves ([`Hub`]). Today both
//!   run as the same user; later the core becomes a Windows service.
//!
//! Layout: the protocol ([`wire`]: JSON frames with request ids both ways,
//! events, hello), one connection end ([`peer`]), the core's end
//! ([`Hub`]: pipe server, agent start, lost agents) and the agent's
//! ([`agent`]: connect, hello, a worker per group, log forwarding). The
//! work is split into *groups*, each in two files of its own:
//!
//! - `src/host/<group>.rs`, the core's side: `GROUP` (the name on the
//!   wire), the group's request enum and reply types, its facade trait
//!   (`<Group>Host`) with one method per operation, the trait for
//!   [`LocalHost`] (the direct Windows calls) and for [`RemoteHost`]
//!   (requests to the agent), and free functions that call [`host()`].
//! - `src/handlers/<group>.rs`, the agent's side: `new(AgentContext) ->
//!   Box<dyn Handler>`, decoding each request and answering it, usually by
//!   calling the group's [`LocalHost`] method.
//!
//! # Adding a group
//!
//! 1. Create `src/host/<group>.rs`: `pub const GROUP: &str = "<group>";`, a
//!    `#[serde(tag = "op")] pub enum Request { ... }` (one variant per
//!    operation, plain data only: no handles, no paths of the user's files
//!    unless needed), the trait `<Group>Host` and its two impls. A remote
//!    method calls `self.call(GROUP, &Request::X)`. Methods block the
//!    calling thread (up to the hub's timeout): never call them on the
//!    BTstack thread or an audio thread. Unsolicited news from the agent
//!    (a device came) are events: the agent's handler calls
//!    `ctx.event(GROUP, body)`, the core subscribes with
//!    `hub.on_event(GROUP, f)` in the group's setup.
//! 2. Create `src/handlers/<group>.rs` with `pub(crate) fn new(ctx:
//!    AgentContext) -> Box<dyn Handler>`. State that lives as long as the
//!    connection (an open device) belongs in the handler; dropping it must
//!    end that work. PCM goes through a channel (`host::capture`,
//!    `host::render`), never through frames.
//! 3. Add one line `<group> => <Group>Host,` to the [`groups!`] list below.
//!    That is the only shared line: it declares both modules, adds the
//!    trait to [`SessionHost`] and the handler to the agent.
//! 4. Test: a unit test of the handler (call `request` with a JSON body),
//!    and a round trip through a real agent in
//!    `app/src/core/tests/session_agent.rs` (next to the devices test).
//!    Log lines: counts and states only, no Bluetooth addresses, keys,
//!    device instance IDs, user names or file paths. The agent's lines go
//!    to the core's log through `src/redact.rs`, which removes quoted text, IDs
//!    and paths: a line of code that also runs in the agent quotes any
//!    name it logs.
//!
//! Call sites then change from `audio_capture::render_devices()` to
//! `session_host::devices::render_devices()` (or
//! `session_host::host().render_devices()` with [`prelude`] in scope).

#![forbid(unsafe_code)]

pub mod agent;
mod hub;
pub mod peer;
mod redact;
pub mod sessions;
pub mod wire;

use std::sync::{Arc, OnceLock};

pub use agent::{AgentContext, AgentOptions, Handler, LogForwardLayer, run_agent};
pub use hub::{AgentCheck, AgentInfo, Hub, HubConfig, LaunchConfig, REQUEST_TIMEOUT};
pub use sessions::{SessionEvent, SessionSource};
pub use wire::{ErrorCode, HostError, PROTOCOL_VERSION, WireError};

/// The registration list: one line per group, `<module> => <facade trait>`.
/// Expands to the modules `host::<module>` (src/host/<module>.rs) and
/// `handlers::<module>` (src/handlers/<module>.rs), the [`SessionHost`]
/// supertrait over every facade trait, the [`prelude`], and the agent's
/// list of handlers.
macro_rules! groups {
    ($($group:ident => $host:ident),* $(,)?) => {
        /// The core's side of each group: protocol, facade trait, both
        /// implementations
        pub mod host {
            $(pub mod $group;)*
        }
        /// The agent's side of each group: the request handler
        pub mod handlers {
            $(pub mod $group;)*
        }
        $(pub use host::$group;)*

        /// Every group's facade, as one trait object ([`host()`])
        pub trait SessionHost: $(host::$group::$host +)* Send + Sync + 'static {}
        impl<T: $(host::$group::$host +)* Send + Sync + 'static> SessionHost for T {}

        /// The facade traits, for calling methods on [`host()`]
        pub mod prelude {
            $(pub use crate::host::$group::$host;)*
        }

        /// Each group's handler for a new connection
        pub(crate) fn agent_handlers(ctx: &AgentContext) -> Vec<(&'static str, Box<dyn Handler>)> {
            vec![$((host::$group::GROUP, handlers::$group::new(ctx.clone())),)*]
        }

        #[cfg(test)]
        pub(crate) const GROUP_NAMES: &[&str] = &[$(host::$group::GROUP,)*];
    };
}

groups! {
    devices => DevicesHost,
    capture => CaptureHost,
    render => RenderHost,
    media => MediaHost,
    shell => ShellHost,
    autostart => AutostartHost,
    elevation => ElevationHost,
    default_device => DefaultDeviceHost,
    endpoints => EndpointsHost,
    volume => VolumeHost,
    activity => ActivityHost,
    migration => MigrationHost,
    handover => HandoverHost,
}

/// Session-bound work done in this process (Windows called directly)
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalHost;

/// Session-bound work done in the session agent
#[derive(Clone, Debug)]
pub struct RemoteHost {
    hub: Arc<Hub>,
}

impl RemoteHost {
    pub fn new(hub: Arc<Hub>) -> RemoteHost {
        RemoteHost { hub }
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// A request of `group` and its decoded reply
    pub(crate) fn call<T: serde::de::DeserializeOwned>(
        &self,
        group: &str,
        request: &impl serde::Serialize,
    ) -> Result<T, HostError> {
        self.hub.request(group, wire::to_body(request)).and_then(wire::decode_reply)
    }
}

static HOST: OnceLock<Box<dyn SessionHost>> = OnceLock::new();
static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

/// This process's session host: [`LocalHost`] unless
/// [`use_session_agent`] selected the agent
pub fn host() -> &'static dyn SessionHost {
    HOST.get_or_init(|| Box::new(LocalHost)).as_ref()
}

/// The hub, once [`use_session_agent`] selected the agent
pub fn hub() -> Option<&'static Arc<Hub>> {
    HUB.get()
}

/// The agent pipe of this user and instance:
/// `\\.\pipe\bluecaster-agent-<user SID><instance suffix>`
pub fn pipe_name() -> std::io::Result<String> {
    Ok(pipe_name_for(&win_sys::pipe::current_user_sid()?, &ipc::instance_suffix()))
}

pub fn pipe_name_for(user_sid: &str, instance_suffix: &str) -> String {
    format!(r"\\.\pipe\bluecaster-agent-{user_sid}{instance_suffix}")
}

/// The service's agent pipe, one name for every user (service mode):
/// `\\.\pipe\bluecaster-service-agent<instance suffix>`
pub fn service_pipe_name() -> String {
    format!(r"\\.\pipe\bluecaster-service-agent{}", ipc::instance_suffix())
}

/// Selects the session agent for this process: serves the pipe, starts the
/// agent (with a launcher), and routes every [`audio_capture::Capture::open`]
/// through it. Call once, at start, before anything uses [`host()`].
pub fn use_session_agent(config: HubConfig) -> Result<Arc<Hub>, String> {
    if HOST.get().is_some() {
        return Err("the session host was chosen already".into());
    }
    let hub = Hub::start(config).map_err(|e| format!("cannot serve the session agent's pipe: {e}"))?;
    if HOST.set(Box::new(RemoteHost::new(hub.clone()))).is_err() {
        hub.shutdown();
        return Err("the session host was chosen already".into());
    }
    let _ = HUB.set(hub.clone());
    audio_capture::use_capture_agent(Arc::new(host::capture::RemoteCaptureAgent::new(hub.clone())));
    tracing::info!("session host: through the session agent");
    Ok(hub)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_has_its_own_name_and_handler() {
        let mut names = GROUP_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), GROUP_NAMES.len());
        assert!(!names.contains(&wire::LOG_GROUP));
        let handlers = agent_handlers(&AgentContext::detached());
        assert_eq!(handlers.iter().map(|(g, _)| *g).collect::<Vec<_>>(), GROUP_NAMES);
    }

    #[test]
    fn pipe_names() {
        assert_eq!(pipe_name_for("S-1-5-21-1", ""), r"\\.\pipe\bluecaster-agent-S-1-5-21-1");
        assert_eq!(pipe_name_for("S-1-5-21-1", "-dev"), r"\\.\pipe\bluecaster-agent-S-1-5-21-1-dev");
        assert!(pipe_name().unwrap().starts_with(r"\\.\pipe\bluecaster-agent-S-1-"));
    }

    #[test]
    fn the_default_host_is_local() {
        // Nothing in the unit tests selects the agent
        assert!(hub().is_none());
        assert_eq!(host().default_render_device(), audio_capture::default_render_device());
    }
}
