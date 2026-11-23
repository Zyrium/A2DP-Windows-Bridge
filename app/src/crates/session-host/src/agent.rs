//! The session agent's side: connect to the core's pipe, say hello, serve
//! the core's requests with the group handlers, forward the log.
//!
//! The agent connects and retries until a core serves the pipe, so either
//! side can start first. Each group's handler gets its own worker thread
//! (requests of one group run in order; a slow group does not hold up the
//! others) and lives as long as the connection: when the core goes away
//! the handlers are dropped, which ends their work (captures stop), and
//! the agent waits for the next core. A `child` agent (started by the
//! core) exits instead, as it does when the core says bye.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

use crate::peer::{self, End, Incoming, Peer};
use crate::wire::{ErrorCode, Frame, Hello, HostError, LOG_GROUP, LogLine, PROTOCOL_VERSION, RejectReason, WireError};

/// Between tries to connect while no core serves the pipe
const RETRY: Duration = Duration::from_millis(200);
/// After [`RejectReason::Busy`]: another agent is connected
const BUSY_RETRY: Duration = Duration::from_secs(1);
/// A child agent that finds no core for this long exits
const CHILD_GIVE_UP: Duration = Duration::from_secs(10);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(5);
/// Requests waiting for one group's worker; more are refused as busy
const GROUP_QUEUE: usize = 64;
/// How long the agent waits for a group's worker to finish at the end of
/// a connection before it leaves it behind
const WORKER_JOIN: Duration = Duration::from_secs(5);
/// Log lines waiting to be written; more are dropped (and counted)
const LOG_QUEUE: usize = 1000;

#[derive(Clone, Debug)]
pub struct AgentOptions {
    pub pipe_name: String,
    /// Started by the core: exit when the core goes away (or never comes)
    pub child: bool,
    /// The service's agent pipe (service mode): opened with only a
    /// client's access, and served only by the core's own program
    pub service: bool,
}

/// What a group's handler can reach: the connection to the core
#[derive(Clone, Debug)]
pub struct AgentContext {
    peer: Arc<Peer>,
}

impl AgentContext {
    /// Sends an event of `group` to the core (unsolicited, no reply)
    pub fn event(&self, group: &str, body: Value) -> bool {
        self.peer.event(group, body)
    }

    /// Asks the core (a group it serves, [`crate::Hub::serve`]). Blocks:
    /// call it from the handler's worker thread only.
    pub fn request(&self, group: &str, body: Value, timeout: Duration) -> Result<Value, HostError> {
        self.peer.request(group, body, timeout)
    }

    #[cfg(test)]
    pub(crate) fn detached() -> AgentContext {
        AgentContext { peer: Peer::new().0 }
    }
}

/// One group's request handler in the agent. Created for each connection
/// ([`crate::groups!`]), dropped when it ends: dropping it must end the
/// group's work (close devices, stop threads).
pub trait Handler: Send + 'static {
    /// One request of the core; runs on the group's worker thread, one at
    /// a time, and may block (a Windows call)
    fn request(&mut self, body: Value) -> Result<Value, WireError>;
    /// An event the core sent to the group
    fn event(&mut self, body: Value) {
        let _ = body;
    }
}

enum Job {
    Request { id: u64, body: Value },
    Event(Value),
}

struct Worker {
    jobs: std_mpsc::SyncSender<Job>,
    thread: std::thread::JoinHandle<()>,
}

/// The connection's group workers
struct Dispatcher {
    workers: HashMap<&'static str, Worker>,
}

impl Dispatcher {
    fn new(ctx: &AgentContext) -> Dispatcher {
        let mut workers = HashMap::new();
        for (group, mut handler) in crate::agent_handlers(ctx) {
            let (jobs, rx) = std_mpsc::sync_channel::<Job>(GROUP_QUEUE);
            let peer = ctx.peer.clone();
            let thread = std::thread::Builder::new()
                .name(format!("agent-{group}"))
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        match job {
                            Job::Request { id, body } => {
                                let result = handler.request(body);
                                peer.reply(id, result);
                            }
                            Job::Event(body) => handler.event(body),
                        }
                    }
                    // `handler` drops here: the group's work ends
                })
                .expect("agent worker thread");
            workers.insert(group, Worker { jobs, thread });
        }
        Dispatcher { workers }
    }

    /// On the runtime thread: never blocks
    fn dispatch(&self, peer: &Peer, msg: Incoming) {
        match msg {
            Incoming::Request { id, group, body } => {
                let Some(w) = self.workers.get(group.as_str()) else {
                    peer.reply(id, Err(WireError::new(ErrorCode::UnknownGroup, "no such group")));
                    return;
                };
                if w.jobs.try_send(Job::Request { id, body }).is_err() {
                    peer.reply(id, Err(WireError::new(ErrorCode::Busy, "too many requests waiting")));
                }
            }
            Incoming::Event { group, body } => {
                if let Some(w) = self.workers.get(group.as_str()) {
                    let _ = w.jobs.try_send(Job::Event(body));
                }
            }
            Incoming::Other => tracing::debug!("session agent: unexpected frame, ignored"),
        }
    }

    /// Ends every group's work: waits for the workers (each up to
    /// [`WORKER_JOIN`] in all)
    fn finish(self) {
        let deadline = Instant::now() + WORKER_JOIN;
        for (group, w) in self.workers {
            drop(w.jobs);
            while !w.thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if w.thread.is_finished() {
                let _ = w.thread.join();
            } else {
                tracing::warn!("session agent: the {group} worker is still busy, left behind");
            }
        }
    }
}

/// The agent's main loop; returns the exit code
pub fn run_agent(options: &AgentOptions) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("session agent: no runtime: {e}");
            return 3;
        }
    };
    rt.block_on(agent_loop(options))
}

/// Service mode: the pipe's server is the core's own program (the
/// program this agent runs), in session 0 or this agent's own session (a
/// core run as an ordinary process: the tests). A pipe served by anything
/// else is not used.
fn server_is_ours(pipe: &NamedPipeClient) -> Result<(), &'static str> {
    use std::os::windows::io::AsHandle;
    let server = win_sys::pipe_peer::server_process(pipe.as_handle()).map_err(|_| "the server is not known")?;
    let own_session = win_sys::session_launch::own_session();
    let own = std::env::current_exe().map_err(|_| "this program is not known")?;
    let service_core = win_sys::service_files::service_core().ok();
    let test_instance = !ipc::instance_suffix().is_empty();
    server_check(server.session_id, server.image.as_deref(), own_session, &own, service_core.as_deref(), test_instance)
}

/// [`server_is_ours`] from what the pipe says. A server in session 0 (the
/// real service, unless this is a test instance) must also be the service
/// folder's core: an agent started from the user's folder does not serve
/// the service.
fn server_check(
    server_session: Option<u32>,
    server_image: Option<&std::path::Path>,
    own_session: Option<u32>,
    own: &std::path::Path,
    service_core: Option<&std::path::Path>,
    test_instance: bool,
) -> Result<(), &'static str> {
    use win_sys::service_files::{required_core, same_path};
    let in_session_0 = matches!(server_session, Some(0));
    if !in_session_0 && server_session != own_session {
        return Err("the server runs in another session");
    }
    if !server_image.is_some_and(|p| same_path(p, own)) {
        return Err("the server is another program");
    }
    let required = required_core(own, service_core, in_session_0 && !test_instance)?;
    if !same_path(required, own) {
        return Err("the service does not run from the service folder");
    }
    Ok(())
}

fn open(name: &str, service: bool) -> std::io::Result<NamedPipeClient> {
    if service { win_sys::pipe_peer::open_client(name) } else { ClientOptions::new().open(name) }
}

async fn connect(name: &str, child: bool, service: bool, since: Instant) -> Option<NamedPipeClient> {
    let mut waiting_logged = false;
    loop {
        match open(name, service) {
            Ok(c) if service => match server_is_ours(&c) {
                Ok(()) => return Some(c),
                Err(why) => tracing::warn!("session agent: the pipe is not the core's ({why}), not used"),
            },
            Ok(c) => return Some(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(231) => {
                // No core yet (2), or all instances busy (231)
                if !waiting_logged {
                    tracing::info!("session agent: waiting for a core");
                    waiting_logged = true;
                }
            }
            Err(e) => tracing::warn!("session agent: cannot connect: {e}"),
        }
        if child && since.elapsed() > CHILD_GIVE_UP {
            tracing::info!("session agent: no core came, exiting");
            return None;
        }
        tokio::time::sleep(RETRY).await;
    }
}

async fn agent_loop(options: &AgentOptions) -> i32 {
    let pid = std::process::id();
    let session_id = win_sys::pipe::process_session_id(pid).unwrap_or(u32::MAX);
    let mut since = Instant::now();
    loop {
        let Some(mut pipe) = connect(&options.pipe_name, options.child, options.service, since).await else {
            return 0;
        };
        let hello = Frame::Hello(Hello { protocol: PROTOCOL_VERSION, pid, session_id });
        if ipc::write_frame(&mut pipe, &hello).await.is_err() {
            tokio::time::sleep(RETRY).await;
            continue;
        }
        match tokio::time::timeout(WELCOME_TIMEOUT, ipc::read_frame::<_, Frame>(&mut pipe)).await {
            Ok(Ok(Some(Frame::Welcome { .. }))) => {}
            Ok(Ok(Some(Frame::Reject { reason: RejectReason::Busy }))) => {
                if options.child {
                    tracing::info!("session agent: another agent serves the core, exiting");
                    return 0;
                }
                tokio::time::sleep(BUSY_RETRY).await;
                continue;
            }
            Ok(Ok(Some(Frame::Reject { reason }))) => {
                tracing::error!("session agent: the core refused it ({reason:?})");
                return 4;
            }
            _ => {
                tracing::debug!("session agent: no welcome, trying again");
                tokio::time::sleep(RETRY).await;
                continue;
            }
        }
        tracing::info!("session agent: connected (protocol {PROTOCOL_VERSION})");
        let end = serve(pipe).await;
        since = Instant::now();
        match end {
            End::Bye => {
                tracing::info!("session agent: the core said bye, exiting");
                return 0;
            }
            End::Failed(e) => tracing::warn!("session agent: the connection failed: {e}"),
            End::Closed => {}
        }
        if options.child {
            tracing::info!("session agent: the core went away, exiting");
            return 0;
        }
        tracing::info!("session agent: the core went away, waiting for the next one");
    }
}

/// Serves one connection until it ends
async fn serve(pipe: NamedPipeClient) -> End {
    let (peer, rx) = Peer::new();
    let ctx = AgentContext { peer: peer.clone() };
    let dispatcher = Arc::new(Dispatcher::new(&ctx));
    set_log_target(Some(peer.clone()));
    let d = dispatcher.clone();
    let p = peer.clone();
    let end = peer::run(peer, rx, pipe, move |msg| d.dispatch(&p, msg)).await;
    set_log_target(None);
    drop(ctx);
    // Its work ends off the runtime thread: a worker may still be busy
    if let Ok(dispatcher) = Arc::try_unwrap(dispatcher) {
        let _ = tokio::task::spawn_blocking(move || dispatcher.finish()).await;
    }
    end
}

// --- Log forwarding ---------------------------------------------------------

static LOG_TARGET: Mutex<Option<Arc<Peer>>> = Mutex::new(None);
static LOG_DROPPED: AtomicU64 = AtomicU64::new(0);

fn set_log_target(peer: Option<Arc<Peer>>) {
    *LOG_TARGET.lock().unwrap_or_else(|e| e.into_inner()) = peer;
}

/// Whether the agent's log lines go to a core now (then the console layer
/// can leave them out)
pub fn forwarding() -> bool {
    LOG_TARGET.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|p| !p.is_closed())
}

/// A tracing layer that sends each log line to the connected core as a
/// [`LOG_GROUP`] event (dropped while no core is connected, or when too
/// many wait to be written). Each line goes reduced (`redact.rs`): no
/// names, IDs or paths reach the core's log.
#[derive(Clone, Copy, Debug, Default)]
pub struct LogForwardLayer;

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            self.record_debug(field, &value);
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LogForwardLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let Some(peer) = LOG_TARGET.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        let dropped = LOG_DROPPED.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            let note = LogLine { level: "WARN".into(), message: format!("{dropped} log line(s) dropped") };
            peer.event_bounded(LOG_GROUP, crate::wire::to_body(&note), LOG_QUEUE);
        }
        let line = LogLine { level: event.metadata().level().to_string(), message: crate::redact::reduce(&visitor.0) };
        if !peer.event_bounded(LOG_GROUP, crate::wire::to_body(&line), LOG_QUEUE) && !peer.is_closed() {
            LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn the_agent_serves_only_the_service_folders_core() {
        use std::path::Path;
        let svc = Path::new(r"C:\Program Files\BlueCaster Service\bluecaster-core.exe");
        let user = Path::new(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe");
        // The real service and its agent, both from the service folder
        assert_eq!(server_check(Some(0), Some(svc), Some(1), svc, Some(svc), false), Ok(()));
        // An agent from the user's folder, a server from the service folder
        assert!(server_check(Some(0), Some(svc), Some(1), user, Some(svc), false).is_err());
        // Both from the user's folder (the stage 4 install): not the service
        assert!(server_check(Some(0), Some(user), Some(1), user, Some(svc), false).is_err());
        // A test instance in session 0 keeps its own program
        assert_eq!(server_check(Some(0), Some(user), Some(1), user, Some(svc), true), Ok(()));
        // A core run by hand in the agent's own session
        assert_eq!(server_check(Some(1), Some(user), Some(1), user, Some(svc), false), Ok(()));
        assert!(server_check(Some(2), Some(user), Some(1), user, Some(svc), false).is_err());
        assert!(server_check(Some(1), None, Some(1), user, Some(svc), false).is_err());
    }

    /// What the forward layer sends for lines logged in the agent (the only
    /// test that sets the log target)
    #[test]
    fn forwarded_lines_carry_no_names_or_paths() {
        let (peer, mut rx) = Peer::new();
        set_log_target(Some(peer));
        let subscriber = tracing_subscriber::registry().with(LogForwardLayer);
        tracing::subscriber::with_default(subscriber, || {
            let name = "Speakers (Jane's Headphones)";
            tracing::info!("voice link: rendering the microphone to \u{201C}{name}\u{201D}, 48000 Hz");
            let path = std::path::Path::new(r"C:\Users\Jane Doe\AppData\Local\Temp\mic.wav");
            tracing::info!("voice link: microphone to {} (16000 Hz)", path.display());
            tracing::debug!(device = "{0.0.0.00000000}.{a6d5c3f1-1d2e-4b5a-9c8d-0123456789ab}", "volume: held");
            tracing::warn!("capture agent: channel opened (2 open)");
        });
        set_log_target(None);
        let mut got = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            if let Frame::Event { group, body, .. } = frame {
                assert_eq!(group, LOG_GROUP);
                let line: LogLine = serde_json::from_value(body).unwrap();
                got.push(format!("{} {}", line.level, line.message));
            }
        }
        assert_eq!(
            got,
            [
                "INFO voice link: rendering the microphone to <name>, 48000 Hz",
                "INFO voice link: microphone to <path> (16000 Hz)",
                "DEBUG volume: held device=<name>",
                "WARN capture agent: channel opened (2 open)",
            ]
        );
    }
}
