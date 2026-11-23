//! The core's end of the agent link: the pipe server, the one connected
//! agent, and the agent's start when none is connected.
//!
//! The hub runs a small tokio runtime on its own thread ("session-hub").
//! It serves the pipe; each client that connects says [`Hello`], is checked
//! by [`authorize`] and becomes the agent unless one is connected already
//! (then it is told [`RejectReason::Busy`] and tries again later). When the
//! agent goes, everything waiting on it fails with [`HostError::AgentLost`],
//! the PCM channels it served are marked lost, and the next agent that
//! connects serves new requests. With a [`LaunchConfig`] the hub starts an
//! agent itself ([`launch_agent`]) whenever none is connected.
//!
//! Users call [`Hub::request`] from ordinary threads: it blocks until the
//! reply, up to the timeout, and waits for an agent to connect first when
//! none is.
//!
//! Service mode (stage 4): the hub starts the agent in the session
//! [`crate::sessions`] picks (the active console session with a user), with
//! that user's token (win_sys::session_launch), and accepts only that
//! process ([`authorize`]). It serves the service's agent pipe (one name
//! for every user, interactive users may connect but not serve it) and
//! passes PCM channels as handles duplicated into the agent
//! ([`Hub::request_with_link`]): a service in session 0 and an agent in
//! the user's session share no `Local\` names.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::net::windows::named_pipe::NamedPipeServer;
use win_sys::session_launch::{Child, Identity, Launch};

use crate::peer::{self, Incoming, Peer};
use crate::sessions::{SessionEvent, SessionSource, SessionTracker};
use crate::wire::{ErrorCode, Frame, Hello, HostError, LOG_GROUP, LogLine, PROTOCOL_VERSION, RejectReason, WireError};

/// A request that gets no reply in this time fails with [`HostError::Timeout`]
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The agent's hello must come this soon after it connected
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// No agent connected for this long: the hub starts one (with a launcher)
const LAUNCH_GRACE: Duration = Duration::from_millis(500);
/// An agent the hub started that has not connected after this is ended
const LAUNCH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// After this many starts in a row without a connection, wait longer
const LAUNCH_QUICK_TRIES: u32 = 3;
const LAUNCH_BACKOFF: Duration = Duration::from_secs(5);
/// How long the hub waits for an agent it started to exit at shutdown
const CHILD_EXIT_WAIT: Duration = Duration::from_secs(2);
/// How often the hub reads the console's session from Windows (service
/// mode; the service's session changes come in between)
const SESSION_POLL: Duration = Duration::from_secs(1);

/// How the hub starts an agent when none is connected
#[derive(Clone, Debug)]
pub struct LaunchConfig {
    /// The program (bluecaster-core.exe)
    pub exe: PathBuf,
    /// Its arguments (`--agent --child`)
    pub args: Vec<String>,
    /// Environment variables it gets beside the core's own (tests: another
    /// instance's pipe)
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct HubConfig {
    /// The pipe ([`crate::pipe_name`])
    pub pipe_name: String,
    /// None: wait for an agent started by someone else
    pub launch: Option<LaunchConfig>,
    pub request_timeout: Duration,
    /// Service mode: what the agent must be ([`authorize`]); None: the
    /// hello's own process is enough (both run as the same user). With a
    /// check the hub also passes PCM channels as duplicated handles.
    pub agent_check: Option<AgentCheck>,
    /// Which session the launcher starts the agent in
    pub sessions: SessionSource,
    /// Serve the pipe as the service's (win_sys::pipe_peer::
    /// create_service_server: interactive users connect, nobody else
    /// serves the name) instead of this user's
    pub service_pipe: bool,
}

impl HubConfig {
    pub fn new(pipe_name: String, launch: Option<LaunchConfig>) -> HubConfig {
        let own = win_sys::session_launch::own_session().unwrap_or(u32::MAX);
        HubConfig {
            pipe_name,
            launch,
            request_timeout: REQUEST_TIMEOUT,
            agent_check: None,
            sessions: SessionSource::Own(own),
            service_pipe: false,
        }
    }
}

/// Service mode: the agent the core accepts
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCheck {
    /// The agent's program: the core's own (`bluecaster-core.exe --agent`),
    /// so it sits beside the core under the expected name
    pub exe: PathBuf,
    /// The core's own session
    pub core_session: u32,
}

/// The connected agent
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub pid: u32,
    pub session_id: u32,
    /// Counts the agents connected so far, from 1
    pub generation: u64,
}

/// What [`authorize`] knows of the process on the pipe's other end
#[derive(Clone, Debug)]
pub struct ClientInfo {
    /// From the pipe itself (GetNamedPipeClientProcessId), not the hello
    pub pid: u32,
    pub session_id: Option<u32>,
    /// With an [`AgentCheck`]: the client's token and program, read from
    /// the pipe after the hello (None: not asked, or not readable)
    pub identity: Option<win_sys::pipe_peer::ClientIdentity>,
}

/// The one place that decides whether the process on the other end of
/// the pipe may be this core's session agent. A rejected client is told
/// [`RejectReason::Unauthorized`].
///
/// Always: the hello comes from the process that opened the pipe, in its
/// session. The pipe's DACL (`win_sys::pipe::create_server`) admits only
/// this user and SYSTEM.
///
/// Service mode ([`AgentCheck`]) adds, from the client's token and
/// process, never from the hello: the client is the very process the hub
/// started (`launched`: its pid and the session it was started in; the
/// hub holds that process's handle, so the pid cannot have been reused),
/// its token is in that session (never session 0 for a core in session
/// 0), and its program is the core's own program, beside the core under
/// the same name. An agent started by anyone else is refused.
///
/// The program check is only as strong as the folder. The real service
/// runs from the service folder under Program Files, and the core makes
/// that its agent program (`check.exe`) only after checking that it is
/// that folder's core (bluecaster-core-exe's agent.rs): only an
/// administrator can put another program there, so a client whose image
/// is that path runs our code. A core run from a folder the user can
/// write (a test instance, a core run by hand) gets the check against its
/// own program, which software running as the user can replace or plant. Neither stops code
/// injected into a genuine agent process of the same user; the pid check
/// at least makes sure the process is one the hub started, with the
/// token it chose, from the program file it named.
fn authorize(
    client: &ClientInfo,
    hello: &Hello,
    check: Option<&AgentCheck>,
    launched: Option<(u32, u32)>,
) -> Result<(), &'static str> {
    if client.pid != hello.pid {
        return Err("the hello names another process");
    }
    if client.session_id.is_some_and(|s| s != hello.session_id) {
        return Err("the hello names another session");
    }
    let Some(check) = check else { return Ok(()) };
    let Some(id) = &client.identity else { return Err("the client's token is not readable") };
    if id.pid != client.pid {
        return Err("another process on the pipe");
    }
    let Some((pid, session)) = launched else { return Err("no agent was started") };
    if id.pid != pid {
        return Err("not the agent the core started");
    }
    if id.session_id != session || (check.core_session == 0 && session == 0) {
        return Err("not an interactive session");
    }
    let ours = id.image.as_deref().is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(check.exe.as_os_str()));
    if !ours {
        return Err("not the agent program");
    }
    Ok(())
}

/// The one place that starts an agent, in `session`: as a service, with
/// the token of the user signed in there and a clean environment (only
/// `config.env` added); as an ordinary process, a child in the core's own
/// session (win_sys::session_launch). The agent keeps `--child`: it exits
/// when the core goes.
fn launch_agent(config: &LaunchConfig, session: u32) -> std::io::Result<Child> {
    let launch = Launch { exe: config.exe.clone(), args: config.args.clone(), session, identity: Identity::User };
    let child = win_sys::session_launch::launch_with_env(&launch, &config.env)?;
    tracing::info!("session agent: started (process {}), session {session}", child.pid());
    Ok(child)
}

type EventFn = Arc<dyn Fn(Value) + Send + Sync>;
type ServeFn = Arc<dyn Fn(Value) -> Result<Value, WireError> + Send + Sync>;
type AgentFn = Arc<dyn Fn(AgentInfo) + Send + Sync>;
type SessionFn = Arc<dyn Fn(Option<u32>, Option<u32>) + Send + Sync>;

struct Conn {
    peer: Arc<Peer>,
    info: AgentInfo,
    /// The agent's process, when the hub started it (service mode: PCM
    /// channels are duplicated into it)
    process: Option<Arc<Child>>,
}

/// An agent process the hub started, and the session it was started in
#[derive(Clone)]
struct Launched {
    child: Arc<Child>,
    session: u32,
}

#[derive(Default)]
struct State {
    conn: Option<Conn>,
    generation: u64,
    /// When the last agent went (or the hub started)
    idle_since: Option<Instant>,
    /// The agent process the launcher started last, while it runs
    launched: Option<Launched>,
}

/// A PCM channel's state as the hub sees it: lost when its agent goes
#[derive(Debug)]
pub(crate) struct ChannelState {
    pub(crate) generation: u64,
    pub(crate) lost: Arc<AtomicBool>,
    pub(crate) link: Arc<pcm_link::Link>,
}

impl ChannelState {
    fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
        self.link.wake();
    }
}

pub struct Hub {
    config: HubConfig,
    state: Mutex<State>,
    changed: Condvar,
    events: Mutex<HashMap<String, EventFn>>,
    services: Mutex<HashMap<String, ServeFn>>,
    channels: Mutex<Vec<Weak<ChannelState>>>,
    stop: AtomicBool,
    shutdown: tokio::sync::Notify,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    sessions: Mutex<SessionTracker>,
    on_connected: Mutex<Vec<AgentFn>>,
    on_session_change: Mutex<Vec<SessionFn>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").field("agent", &self.agent()).finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Hub {
    /// Serves the pipe from now on (and starts the agent, with a
    /// launcher). Fails if another process serves the pipe already.
    pub fn start(config: HubConfig) -> std::io::Result<Arc<Hub>> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        // The pipe registers with the runtime's reactor
        let first = {
            let _rt = rt.enter();
            create_pipe(&config, true)?
        };
        let hub = Arc::new(Hub {
            config,
            state: Mutex::new(State { idle_since: Some(Instant::now()), ..State::default() }),
            changed: Condvar::new(),
            events: Mutex::new(HashMap::new()),
            services: Mutex::new(HashMap::new()),
            channels: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            shutdown: tokio::sync::Notify::new(),
            threads: Mutex::new(Vec::new()),
            sessions: Mutex::new(SessionTracker::new()),
            on_connected: Mutex::new(Vec::new()),
            on_session_change: Mutex::new(Vec::new()),
        });
        let me = hub.clone();
        let server = std::thread::Builder::new().name("session-hub".into()).spawn(move || {
            rt.block_on(accept_loop(me, first));
        })?;
        lock(&hub.threads).push(server);
        if let Some(launch) = hub.config.launch.clone() {
            let me = Arc::downgrade(&hub);
            let launcher =
                std::thread::Builder::new().name("session-launcher".into()).spawn(move || launcher(&me, &launch))?;
            lock(&hub.threads).push(launcher);
        }
        tracing::info!("session agent: serving the agent pipe (start an agent: {})", hub.config.launch.is_some());
        Ok(hub)
    }

    /// How long a request may take, waiting for an agent included
    pub fn request_timeout(&self) -> Duration {
        self.config.request_timeout
    }

    /// A session change the service received (SERVICE_CONTROL_SESSIONCHANGE);
    /// the launcher acts on it at its next round
    pub fn session_event(&self, e: SessionEvent) {
        if lock(&self.sessions).apply(e) {
            tracing::debug!("session agent: the console's session changed");
        }
    }

    /// The session the agent should run in now (None: nobody signed in at
    /// the console, no agent)
    pub fn wanted_session(&self) -> Option<u32> {
        match &self.config.sessions {
            SessionSource::Own(s) => Some(*s),
            SessionSource::Console | SessionSource::TestFile(_) => lock(&self.sessions).wanted(),
        }
    }

    /// Calls `f` (on a thread of its own) each time an agent connected
    pub fn on_agent_connected(&self, f: impl Fn(AgentInfo) + Send + Sync + 'static) {
        lock(&self.on_connected).push(Arc::new(f));
    }

    /// Calls `f(old, new)` before the agent of session `old` is ended
    /// because the console moved to `new` (fast user switching, sign-out,
    /// a disconnected console), on the launcher's thread: what the core
    /// must stop first (a stream fed from the old user's session)
    pub fn on_session_change(&self, f: impl Fn(Option<u32>, Option<u32>) + Send + Sync + 'static) {
        lock(&self.on_session_change).push(Arc::new(f));
    }

    /// A request to `group` with a new PCM channel of `ring_bytes`: the
    /// channel is created for the agent the request goes to (as handles
    /// duplicated into that agent's process when the hub started it in
    /// service mode, else under a `Local\` name), `body` builds the request
    /// from the channel's number and text. The reply, the agent, the
    /// channel's number and its link.
    pub(crate) fn request_with_link(
        &self,
        group: &str,
        ring_bytes: usize,
        body: impl FnOnce(u64, String) -> Value,
    ) -> Result<(Value, AgentInfo, u64, Arc<pcm_link::Link>), HostError> {
        let started = Instant::now();
        let timeout = self.config.request_timeout;
        let (peer, info, process) = self.connection_with_process(timeout).ok_or(HostError::NoAgent)?;
        let (id, link, text) = match process.filter(|_| self.config.agent_check.is_some()) {
            Some(p) => {
                let id = NEXT_CHANNEL.fetch_add(1, Ordering::Relaxed);
                let (link, text) = pcm_link::Link::create_for(p.handle(), ring_bytes)
                    .map_err(|e| HostError::Protocol(format!("audio link: {e}")))?;
                tracing::debug!("session agent: channel {id} passed as handles");
                (id, link, text)
            }
            None => {
                let (id, name) = new_channel();
                let link = pcm_link::Link::create(&name, ring_bytes)
                    .map_err(|e| HostError::Protocol(format!("audio link: {e}")))?;
                (id, link, name)
            }
        };
        let left = timeout.saturating_sub(started.elapsed()).max(Duration::from_millis(1));
        let reply = peer.request(group, body(id, text), left)?;
        Ok((reply, info, id, link))
    }

    /// The connected agent
    pub fn agent(&self) -> Option<AgentInfo> {
        lock(&self.state).conn.as_ref().map(|c| c.info)
    }

    /// Waits up to `timeout` for an agent to be connected
    pub fn wait_agent(&self, timeout: Duration) -> Option<AgentInfo> {
        self.connection(timeout).map(|(_, info)| info)
    }

    /// Waits until no agent is connected, or `timeout`; true if none is
    pub fn wait_no_agent(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.conn.is_some() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self.changed.wait_timeout(state, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }

    fn connection(&self, timeout: Duration) -> Option<(Arc<Peer>, AgentInfo)> {
        self.connection_with_process(timeout).map(|(p, i, _)| (p, i))
    }

    fn connection_with_process(&self, timeout: Duration) -> Option<(Arc<Peer>, AgentInfo, Option<Arc<Child>>)> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        loop {
            if let Some(c) = &state.conn {
                return Some((c.peer.clone(), c.info, c.process.clone()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.stop.load(Ordering::Acquire) {
                return None;
            }
            state = self.changed.wait_timeout(state, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Sends a request to the agent's `group` and waits for the reply, up
    /// to the hub's timeout (waiting for an agent to connect counts). Not
    /// for a runtime, BTstack or audio thread.
    pub fn request(&self, group: &str, body: Value) -> Result<Value, HostError> {
        self.request_on(group, body, self.config.request_timeout).map(|(v, _)| v)
    }

    /// [`Hub::request`] with its own timeout; also says which agent answered
    pub fn request_on(&self, group: &str, body: Value, timeout: Duration) -> Result<(Value, AgentInfo), HostError> {
        let started = Instant::now();
        let (peer, info) = self.connection(timeout).ok_or(HostError::NoAgent)?;
        let left = timeout.saturating_sub(started.elapsed()).max(Duration::from_millis(1));
        peer.request(group, body, left).map(|v| (v, info))
    }

    /// Sends a request without waiting for its reply, only to the agent of
    /// `generation` (a channel's close: a newer agent never had it)
    pub fn notify(&self, generation: u64, group: &str, body: Value) -> bool {
        let peer = match &lock(&self.state).conn {
            Some(c) if c.info.generation == generation => c.peer.clone(),
            _ => return false,
        };
        peer.notify(group, body)
    }

    /// Sends an event to the agent, if one is connected
    pub fn event(&self, group: &str, body: Value) -> bool {
        let peer = lock(&self.state).conn.as_ref().map(|c| c.peer.clone());
        peer.is_some_and(|p| p.event(group, body))
    }

    /// Calls `f` with each event the agent sends for `group` (on the hub's
    /// thread: `f` must not block)
    pub fn on_event(&self, group: &str, f: impl Fn(Value) + Send + Sync + 'static) {
        lock(&self.events).insert(group.to_string(), Arc::new(f));
    }

    /// Answers the agent's requests for `group` with `f` (on a blocking
    /// worker thread)
    pub fn serve(&self, group: &str, f: impl Fn(Value) -> Result<Value, WireError> + Send + Sync + 'static) {
        lock(&self.services).insert(group.to_string(), Arc::new(f));
    }

    /// Tells the agent to exit (it does not come back by itself; a hub with
    /// a launcher starts a new one)
    pub fn tell_agent_to_exit(&self) -> bool {
        let peer = lock(&self.state).conn.as_ref().map(|c| c.peer.clone());
        peer.is_some_and(|p| p.send(Frame::Bye))
    }

    /// A channel opened on the agent of `generation`: marked lost when that
    /// agent goes (at once if it went already)
    pub(crate) fn register_channel(&self, channel: &Arc<ChannelState>) {
        let current = lock(&self.state).conn.as_ref().map(|c| c.info.generation);
        let mut channels = lock(&self.channels);
        channels.retain(|w| w.strong_count() > 0);
        channels.push(Arc::downgrade(channel));
        drop(channels);
        if current != Some(channel.generation) {
            channel.mark_lost();
        }
    }

    /// Stops serving the pipe; the agent's connection closes (an agent the
    /// hub started exits). Waits for the hub's threads.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        // Its requests and channels end now, not with the runtime
        if let Some(agent) = self.agent() {
            self.disconnected(agent.generation, &peer::End::Closed);
        }
        self.shutdown.notify_one();
        self.changed.notify_all();
        let threads: Vec<_> = lock(&self.threads).drain(..).collect();
        for t in threads {
            let _ = t.join();
        }
    }

    /// The agent's number, or Err if another agent is connected
    fn connected(&self, conn: Conn) -> Result<u64, ()> {
        let mut state = lock(&self.state);
        if state.conn.is_some() {
            return Err(());
        }
        state.generation += 1;
        let mut conn = conn;
        conn.info.generation = state.generation;
        tracing::info!(
            "session agent: connected (process {}, session {}, agent {})",
            conn.info.pid,
            conn.info.session_id,
            conn.info.generation
        );
        let generation = state.generation;
        let info = conn.info;
        state.conn = Some(conn);
        state.idle_since = None;
        drop(state);
        self.changed.notify_all();
        for f in lock(&self.on_connected).iter().cloned() {
            let spawned = std::thread::Builder::new().name("agent-connected".into()).spawn(move || f(info));
            if let Err(e) = spawned {
                tracing::warn!("session agent: cannot run what follows a connection: {e}");
            }
        }
        Ok(generation)
    }

    fn disconnected(&self, generation: u64, end: &peer::End) {
        let mut state = lock(&self.state);
        let Some(conn) = state.conn.take_if(|c| c.info.generation == generation) else {
            return;
        };
        state.idle_since = Some(Instant::now());
        drop(state);
        let failed = conn.peer.close();
        let mut lost = 0;
        for ch in lock(&self.channels).iter().filter_map(Weak::upgrade) {
            if ch.generation == generation && !ch.lost.load(Ordering::Acquire) {
                ch.mark_lost();
                lost += 1;
            }
        }
        match end {
            peer::End::Failed(e) => tracing::warn!(
                "session agent: lost (process {}, {e}), {failed} request(s) failed, {lost} channel(s) lost",
                conn.info.pid
            ),
            _ => tracing::info!(
                "session agent: lost (process {}), {failed} request(s) failed, {lost} channel(s) lost",
                conn.info.pid
            ),
        }
        self.changed.notify_all();
    }

    fn incoming(self: &Arc<Self>, peer: &Arc<Peer>, msg: Incoming) {
        match msg {
            Incoming::Event { group, body } if group == LOG_GROUP => agent_log(body),
            Incoming::Event { group, body } => {
                let f = lock(&self.events).get(&group).cloned();
                match f {
                    Some(f) => f(body),
                    None => tracing::debug!("session agent: event of an unknown group"),
                }
            }
            Incoming::Request { id, group, body } => {
                let f = lock(&self.services).get(&group).cloned();
                let peer = peer.clone();
                match f {
                    Some(f) => {
                        tokio::task::spawn_blocking(move || peer.reply(id, f(body)));
                    }
                    None => peer.reply(id, Err(WireError::new(ErrorCode::UnknownGroup, "no such group"))),
                }
            }
            Incoming::Other => tracing::debug!("session agent: unexpected frame, ignored"),
        }
    }
}

/// An agent's log line into the core's log, at its level, marked as the
/// agent's. Reduced again (crate::redact): the core does not trust the
/// agent's text, one line stays one line.
fn agent_log(body: Value) {
    let Ok(line) = serde_json::from_value::<LogLine>(body) else { return };
    let m = crate::redact::reduce(&line.message);
    match line.level.as_str() {
        "ERROR" => tracing::error!(target: "session_agent", "[agent] {m}"),
        "WARN" => tracing::warn!(target: "session_agent", "[agent] {m}"),
        "INFO" => tracing::info!(target: "session_agent", "[agent] {m}"),
        "DEBUG" => tracing::debug!(target: "session_agent", "[agent] {m}"),
        _ => tracing::trace!(target: "session_agent", "[agent] {m}"),
    }
}

/// One instance of the agent pipe: this user's (`win_sys::pipe`), or the
/// service's ([`HubConfig::service_pipe`])
fn create_pipe(config: &HubConfig, first: bool) -> std::io::Result<NamedPipeServer> {
    if config.service_pipe {
        win_sys::pipe_peer::create_service_server(&config.pipe_name, first)
    } else {
        win_sys::pipe::create_server(&config.pipe_name, first)
    }
}

async fn accept_loop(hub: Arc<Hub>, first: NamedPipeServer) {
    let mut server = first;
    loop {
        tokio::select! {
            connected = server.connect() => {
                if let Err(e) = connected {
                    tracing::debug!("session agent: a client did not connect: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                } else {
                    let me = hub.clone();
                    tokio::spawn(async move { serve_client(me, server).await });
                }
                // The next instance at once: the name stays ours
                server = loop {
                    match create_pipe(&hub.config, false) {
                        Ok(s) => break s,
                        Err(e) => {
                            tracing::warn!("session agent: cannot serve the pipe: {e}");
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                                _ = hub.shutdown.notified() => return,
                            }
                        }
                    }
                };
            }
            _ = hub.shutdown.notified() => return,
        }
        if hub.stop.load(Ordering::Acquire) {
            return;
        }
    }
}

async fn serve_client(hub: Arc<Hub>, mut pipe: NamedPipeServer) {
    use std::os::windows::io::AsHandle;
    let pid = match win_sys::pipe::client_process_id(pipe.as_handle()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("session agent: the client's process is not known: {e}");
            return;
        }
    };
    let mut client = ClientInfo { pid, session_id: win_sys::pipe::process_session_id(pid).ok(), identity: None };
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, ipc::read_frame::<_, Frame>(&mut pipe)).await {
        Ok(Ok(Some(Frame::Hello(h)))) => h,
        Ok(Ok(Some(_))) => {
            tracing::warn!("session agent: process {pid} did not say hello first");
            return;
        }
        Ok(Ok(None)) | Ok(Err(_)) => return,
        Err(_) => {
            tracing::warn!("session agent: no hello from process {pid} in time");
            return;
        }
    };
    // After the first read: Windows lets the server read the client's token
    let check = hub.config.agent_check.as_ref();
    if check.is_some() {
        client.identity = win_sys::pipe_peer::client_identity(pipe.as_handle()).ok();
    }
    let launched = lock(&hub.state).launched.clone();
    let expected = launched.as_ref().map(|l| (l.child.pid(), l.session));
    let reject = if hello.protocol != PROTOCOL_VERSION {
        tracing::warn!("session agent: process {pid} speaks protocol {}, not {PROTOCOL_VERSION}", hello.protocol);
        Some(RejectReason::Protocol)
    } else if let Err(why) = authorize(&client, &hello, check, expected) {
        tracing::warn!("session agent: process {pid} refused: {why}");
        Some(RejectReason::Unauthorized)
    } else {
        None
    };
    if let Some(reason) = reject {
        let _ = ipc::write_frame(&mut pipe, &Frame::Reject { reason }).await;
        return;
    }
    let (peer, rx) = Peer::new();
    let info = AgentInfo { pid, session_id: hello.session_id, generation: 0 };
    let process = launched.filter(|l| l.child.pid() == pid).map(|l| l.child);
    let generation = match hub.connected(Conn { peer: peer.clone(), info, process }) {
        Ok(g) => g,
        Err(()) => {
            tracing::debug!("session agent: process {pid} waits, another agent is connected");
            let _ = ipc::write_frame(&mut pipe, &Frame::Reject { reason: RejectReason::Busy }).await;
            return;
        }
    };
    if ipc::write_frame(&mut pipe, &Frame::Welcome { protocol: PROTOCOL_VERSION }).await.is_err() {
        hub.disconnected(generation, &peer::End::Closed);
        return;
    }
    let me = hub.clone();
    let p = peer.clone();
    let end = tokio::select! {
        end = peer::run(peer, rx, pipe, move |msg| me.incoming(&p, msg)) => end,
        _ = wait_stop(&hub) => peer::End::Closed,
    };
    hub.disconnected(generation, &end);
}

async fn wait_stop(hub: &Hub) {
    while !hub.stop.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Reads the console's session from Windows (or a test's file) into the
/// tracker
fn poll_sessions(hub: &Hub) {
    let facts = match &hub.config.sessions {
        SessionSource::Own(_) => return,
        SessionSource::Console => {
            let console = win_sys::session_launch::active_session();
            SessionEvent::Facts {
                console,
                user_at_console: console.is_some_and(win_sys::session_launch::session_has_user),
            }
        }
        SessionSource::TestFile(path) => {
            crate::sessions::test_file_facts(&std::fs::read_to_string(path).unwrap_or_default())
        }
    };
    hub.session_event(facts);
}

/// Starts an agent in the wanted session ([`Hub::wanted_session`])
/// whenever none is connected for [`LAUNCH_GRACE`], ends it when the
/// wanted session changes, and collects the ones it started when they exit
fn launcher(hub: &Weak<Hub>, config: &LaunchConfig) {
    let mut child: Option<(Launched, Instant)> = None;
    let mut failed_starts = 0u32;
    let mut next_start = Instant::now();
    let mut next_poll = Instant::now();
    // None: not decided yet (the first round logs the session)
    let mut last_wanted: Option<Option<u32>> = None;
    loop {
        let Some(hub) = hub.upgrade() else { break };
        if hub.stop.load(Ordering::Acquire) {
            if let Some((c, _)) = child.take() {
                end_child(&c.child, CHILD_EXIT_WAIT);
            }
            lock(&hub.state).launched = None;
            break;
        }
        if Instant::now() >= next_poll {
            poll_sessions(&hub);
            next_poll = Instant::now() + SESSION_POLL;
        }
        let wanted = hub.wanted_session();
        if last_wanted != Some(wanted) {
            tracing::info!(
                "session agent: the agent's session is now {}",
                wanted.map_or_else(|| "none (nobody signed in at the console)".to_string(), |s| s.to_string())
            );
            if let Some(Some(old)) = last_wanted {
                // What fed from the old session stops before its agent goes
                for f in lock(&hub.on_session_change).iter().cloned() {
                    f(Some(old), wanted);
                }
            }
            let stale = child.as_ref().is_some_and(|(c, _)| Some(c.session) != wanted);
            if stale && let Some((c, _)) = child.take() {
                tracing::info!("session agent: ending the agent of the previous session");
                hub.tell_agent_to_exit();
                end_child(&c.child, CHILD_EXIT_WAIT);
                lock(&hub.state).launched = None;
            }
            last_wanted = Some(wanted);
            failed_starts = 0;
            next_start = Instant::now();
        }
        let (connected, idle_since) = {
            let s = lock(&hub.state);
            (s.conn.as_ref().map(|c| c.info.pid), s.idle_since)
        };
        if let Some((c, started)) = &child {
            let pid = c.child.pid();
            match c.child.try_wait() {
                Ok(Some(code)) => {
                    tracing::info!("session agent: process {pid} exited (exit code {code})");
                    child = None;
                    lock(&hub.state).launched = None;
                }
                Ok(None) if connected == Some(pid) => failed_starts = 0,
                Ok(None) if connected.is_none() && started.elapsed() > LAUNCH_CONNECT_TIMEOUT => {
                    tracing::warn!("session agent: process {pid} did not connect, ending it");
                    failed_starts += 1;
                    if let Some((c, _)) = child.take() {
                        end_child(&c.child, Duration::ZERO);
                    }
                    lock(&hub.state).launched = None;
                }
                _ => {}
            }
        }
        let idle_long = connected.is_none() && idle_since.is_some_and(|t| t.elapsed() >= LAUNCH_GRACE);
        if let Some(session) = wanted
            && idle_long
            && child.is_none()
            && Instant::now() >= next_start
        {
            // Under the state's lock: the agent's hello waits for its record
            let mut state = lock(&hub.state);
            match launch_agent(config, session) {
                Ok(c) => {
                    let l = Launched { child: Arc::new(c), session };
                    state.launched = Some(l.clone());
                    child = Some((l, Instant::now()));
                }
                Err(e) => {
                    tracing::warn!("session agent: cannot start it: {e}");
                    failed_starts += 1;
                }
            }
            drop(state);
            if failed_starts >= LAUNCH_QUICK_TRIES {
                next_start = Instant::now() + LAUNCH_BACKOFF;
            }
        }
        drop(hub);
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits up to `wait` for an agent the hub started to exit, then ends it
fn end_child(child: &Child, wait: Duration) {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
}

static NEXT_CHANNEL: AtomicU64 = AtomicU64::new(1);

/// A new PCM channel: its number and the name of its shared memory
/// (`Local\BlueCaster_Pcm<instance>-<core pid>-<n>`), for an agent in the
/// core's own session.
///
/// `Local\` names are per Windows session: a core running as a service
/// (session 0) and an agent in the user's session do not share them, so
/// in service mode [`Hub::request_with_link`] duplicates unnamed memory
/// and events into the agent's process instead (no name anyone else
/// could open).
pub(crate) fn new_channel() -> (u64, String) {
    let n = NEXT_CHANNEL.fetch_add(1, Ordering::Relaxed);
    (n, format!(r"Local\BlueCaster_Pcm{}-{}-{n}", ipc::instance_suffix(), std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(pid: u32, session_id: Option<u32>) -> ClientInfo {
        ClientInfo { pid, session_id, identity: None }
    }

    #[test]
    fn only_the_process_on_the_pipe_is_authorized() {
        let hello = Hello { protocol: PROTOCOL_VERSION, pid: 40, session_id: 1 };
        assert!(authorize(&info(40, Some(1)), &hello, None, None).is_ok());
        assert!(authorize(&info(40, None), &hello, None, None).is_ok());
        assert!(authorize(&info(44, Some(1)), &hello, None, None).is_err());
        assert!(authorize(&info(40, Some(2)), &hello, None, None).is_err());
    }

    /// Service mode: an interactive session and the core's own program,
    /// from the token and the process
    #[test]
    fn service_mode_wants_our_program_in_an_interactive_session() {
        let exe = PathBuf::from(r"C:\Program Files\BlueCaster\bluecaster-core.exe");
        let id = |session_id: u32, image: Option<&str>| win_sys::pipe_peer::ClientIdentity {
            pid: 40,
            session_id,
            user_sid: "S-1-5-21-1".into(),
            elevated: false,
            integrity: win_sys::pipe_peer::INTEGRITY_MEDIUM,
            image: image.map(PathBuf::from),
        };
        let client = |session: u32, image: Option<&str>| ClientInfo {
            pid: 40,
            session_id: Some(session),
            identity: Some(id(session, image)),
        };
        let hello = |session_id| Hello { protocol: PROTOCOL_VERSION, pid: 40, session_id };
        let ours = Some(r"c:\program files\bluecaster\BLUECASTER-CORE.exe");
        // The service in session 0 started process 40 in session 1
        let service = AgentCheck { exe: exe.clone(), core_session: 0 };
        let started = Some((40, 1));
        assert!(authorize(&client(1, ours), &hello(1), Some(&service), started).is_ok());
        // Another session than the one it was started in (another user
        // over Remote Desktop, or the previous console user)
        assert!(authorize(&client(2, ours), &hello(2), Some(&service), started).is_err());
        // Session 0 is never interactive, not even when asked for
        assert!(authorize(&client(0, ours), &hello(0), Some(&service), Some((40, 0))).is_err());
        // Nothing started, or another process started: an agent started by
        // anyone else is refused
        assert!(authorize(&client(1, ours), &hello(1), Some(&service), None).is_err());
        assert!(authorize(&client(1, ours), &hello(1), Some(&service), Some((41, 1))).is_err());
        // Another program, or none known
        let planted = Some(r"C:\Users\u\AppData\Local\Temp\bluecaster-core.exe");
        assert!(authorize(&client(1, planted), &hello(1), Some(&service), started).is_err());
        assert!(authorize(&client(1, None), &hello(1), Some(&service), started).is_err());
        // No token read: refused
        assert!(authorize(&info(40, Some(1)), &hello(1), Some(&service), started).is_err());
        // A token of another process than the pipe's
        let mut other = client(1, ours);
        other.identity.as_mut().unwrap().pid = 44;
        assert!(authorize(&other, &hello(1), Some(&service), started).is_err());
        // A core that is not in session 0 (the tests) starts it in its own
        let test_core = AgentCheck { exe, core_session: 2 };
        assert!(authorize(&client(2, ours), &hello(2), Some(&test_core), Some((40, 2))).is_ok());
        assert!(authorize(&client(3, ours), &hello(3), Some(&test_core), Some((40, 2))).is_err());
        // Without service mode the started process does not matter
        assert!(authorize(&info(40, Some(1)), &hello(1), None, Some((41, 1))).is_ok());
    }

    /// The launcher's session in each mode, and what it sees of the
    /// service's session changes
    #[test]
    fn the_wanted_session_follows_its_source() {
        let pipe = |n: &str| format!(r"\\.\pipe\bluecaster-test-hub-{n}-{}", std::process::id());
        let own = Hub::start(HubConfig::new(pipe("own"), None)).unwrap();
        assert_eq!(own.wanted_session(), win_sys::session_launch::own_session());
        own.session_event(SessionEvent::ConsoleDisconnect(1));
        assert_eq!(own.wanted_session(), win_sys::session_launch::own_session(), "an ordinary core keeps its own");
        own.shutdown();
        let console =
            Hub::start(HubConfig { sessions: SessionSource::Console, ..HubConfig::new(pipe("console"), None) })
                .unwrap();
        assert_eq!(console.wanted_session(), None, "no launcher: nothing polled");
        console.session_event(SessionEvent::ConsoleConnect(3));
        console.session_event(SessionEvent::Logon(3));
        assert_eq!(console.wanted_session(), Some(3));
        console.session_event(SessionEvent::ConsoleDisconnect(3));
        assert_eq!(console.wanted_session(), None);
        console.shutdown();
    }

    #[test]
    fn channel_names_are_valid_and_new() {
        let (a, name_a) = new_channel();
        let (b, name_b) = new_channel();
        assert_ne!(a, b);
        assert_ne!(name_a, name_b);
        assert!(audio_capture::valid_channel(&name_a), "{name_a}");
    }
}
