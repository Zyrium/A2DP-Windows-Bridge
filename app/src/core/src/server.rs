//! The IPC server: the user's Named Pipe, one task per UI connection. The
//! same server under the pipe's 2.0.x name too (third-party clients written
//! for A2DPWB; see ipc::old_pipe_name).
//!
//! Service mode ([`serve_service`]): one pipe for every user, no 2.0.x
//! name, and every client is identified from the pipe (win_sys::pipe_peer)
//! and its requests checked against the rules (crate::rules).
//!
//! A connection says Hello, gets Welcome and a Snapshot, then the events of
//! every state change; requests run concurrently and their responses go out
//! through the same writer. A client that falls behind on events gets a new
//! snapshot instead (it sees the revision gap).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ipc::{ClientMessage, ErrorInfo, Event, Method, Msg, PROTOCOL_VERSION, ServerMessage, read_frame, write_frame};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::{broadcast, mpsc};

use crate::state::CoreState;

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// While the old name cannot be had (a 2.0.x core that did not exit owns
/// it): tries again this often
const OLD_NAME_RETRY: Duration = Duration::from_secs(5);

/// Accepts connections on `name` and `old_name` (same DACL, protocol and
/// handler) until the process ends. Err if another core of this user owns
/// `name`; the old name is served once it is free.
pub async fn serve(state: Arc<CoreState>, name: String, old_name: String) -> std::io::Result<()> {
    let server = win_sys::pipe::create_server(&name, true)?;
    tracing::info!("IPC: listening on {name}");
    tokio::spawn(serve_old(state.clone(), old_name));
    accept(state, &name, server).await
}

/// The 2.0.x name: not served while a 2.0.x core still owns it, tried again
/// until it is free; a failure there does not end the core
async fn serve_old(state: Arc<CoreState>, name: String) {
    let mut told = false;
    loop {
        match win_sys::pipe::create_server(&name, true) {
            Ok(server) => {
                tracing::info!("IPC: listening on {name} too (the 2.0.x name)");
                told = false;
                if let Err(e) = accept(state.clone(), &name, server).await {
                    tracing::warn!("IPC: {name}: {e}");
                }
            }
            Err(e) if !told => {
                tracing::warn!("IPC: cannot serve {name} (a 2.0.x core may still run): {e}");
                told = true;
            }
            Err(_) => {}
        }
        tokio::time::sleep(OLD_NAME_RETRY).await;
    }
}

/// Service mode: accepts connections on the service pipe `name`
/// (ipc::service_pipe_name) until the process ends; Err if another
/// process owns the name
pub async fn serve_service(state: Arc<CoreState>, name: String) -> std::io::Result<()> {
    let server = win_sys::pipe_peer::create_service_server(&name, true)?;
    tracing::info!("IPC: listening on the service pipe");
    accept_with(state, &name, server, true).await
}

async fn accept(state: Arc<CoreState>, name: &str, server: NamedPipeServer) -> std::io::Result<()> {
    accept_with(state, name, server, false).await
}

async fn accept_with(
    state: Arc<CoreState>,
    name: &str,
    mut server: NamedPipeServer,
    service: bool,
) -> std::io::Result<()> {
    loop {
        server.connect().await?;
        let client = server;
        server = if service {
            win_sys::pipe_peer::create_service_server(name, false)?
        } else {
            win_sys::pipe::create_server(name, false)?
        };
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = session(state, client, service).await {
                tracing::debug!("IPC: client ended: {e}");
            }
        });
    }
}

/// Tests: the identity the server sees instead of the pipe's (an
/// elevated client without a UAC prompt). Not in a release build.
#[cfg(test)]
pub(crate) static TEST_IDENTITY: Mutex<Option<win_sys::pipe_peer::ClientIdentity>> = Mutex::new(None);

/// The client on `pipe`, from its token and process
fn identify(pipe: &NamedPipeServer) -> std::io::Result<win_sys::pipe_peer::ClientIdentity> {
    #[cfg(test)]
    if let Some(id) = TEST_IDENTITY.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Ok(id);
    }
    use std::os::windows::io::AsHandle;
    win_sys::pipe_peer::client_identity(pipe.as_handle())
}

/// Service mode: the rules' view of the client, or why it may not connect
fn service_caller(
    id: std::io::Result<win_sys::pipe_peer::ClientIdentity>,
    ticket: Option<&str>,
) -> Result<crate::rules::Caller, &'static str> {
    let id = id.map_err(|_| "the client is not known")?;
    let programs = crate::rules::own_elevated_programs();
    let caller = crate::rules::Caller::of(&id, &programs, ticket);
    if id.session_id == 0 && !caller.elevated {
        return Err("not an interactive session");
    }
    Ok(caller)
}

/// Service mode's version rule for a hello `name`: BlueCaster's own UI
/// ("bluecaster-ui/<version>") must be the service's `own` version. Other
/// clients (the elevated helper runs from the service folder itself, pipe
/// clients of other authors) are not asked.
fn ui_version_reject(name: &str, own: &str) -> Option<&'static str> {
    let version = name.strip_prefix("bluecaster-ui/")?;
    ipc::service_state::version_reject(own, version)
}

async fn session(state: Arc<CoreState>, mut pipe: NamedPipeServer, service: bool) -> Result<(), ipc::FrameError> {
    let hello = tokio::time::timeout(HELLO_TIMEOUT, read_frame::<_, ClientMessage>(&mut pipe)).await;
    // After the first read: Windows lets the server read the client's token
    let identity = service.then(|| identify(&pipe));
    let (mut rd, mut wr) = tokio::io::split(pipe);
    let client = match hello {
        Ok(Ok(Some(ClientMessage::Hello { protocol, client }))) => {
            if protocol != PROTOCOL_VERSION {
                let reason = format!("protocol {protocol} not supported (core speaks {PROTOCOL_VERSION})");
                write_frame(&mut wr, &ServerMessage::Reject { reason }).await?;
                return Ok(());
            }
            client
        }
        _ => {
            let reason = "expected hello".to_string();
            write_frame(&mut wr, &ServerMessage::Reject { reason }).await?;
            return Ok(());
        }
    };
    // The elevated helper's ticket is not part of its name
    let (name, ticket) = ipc::access::split_client_name(&client);
    let caller = match identity.map(|id| service_caller(id, ticket)) {
        Some(Err(why)) => {
            tracing::info!("IPC: a client was refused: {why}");
            write_frame(&mut wr, &ServerMessage::Reject { reason: why.to_string() }).await?;
            return Ok(());
        }
        Some(Ok(c)) => Some(c),
        None => None,
    };
    // Service mode: the app and the service are one version; another
    // version is told which side needs the update (the UI reads it from the
    // reason, ipc::service_state::version_reject_code)
    if caller.is_some()
        && let Some(why) = ui_version_reject(name, env!("CARGO_PKG_VERSION"))
    {
        tracing::info!("IPC: a client was refused: {why}");
        write_frame(&mut wr, &ServerMessage::Reject { reason: why.to_string() }).await?;
        return Ok(());
    }
    let client = name.to_string();
    match &caller {
        Some(c) => tracing::info!("IPC: {client} connected (elevated {})", c.elevated),
        None => tracing::info!("IPC: {client} connected"),
    }
    lock_clients().push(client.clone());
    // Service mode: BlueCaster's window started again after an Exit brings
    // the tray icon back
    if caller.is_some() && client.starts_with("bluecaster-ui/") {
        crate::headless::user_returned();
    }
    write_frame(
        &mut wr,
        &ServerMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            server: format!("bluecaster-core/{}", env!("CARGO_PKG_VERSION")),
            features: ipc::FEATURES.iter().map(|f| f.to_string()).collect(),
        },
    )
    .await?;
    // Subscribe before the snapshot so no change falls in between
    let mut events = state.subscribe();
    let (rev, snap) = state.snapshot();
    write_frame(&mut wr, &ServerMessage::Snapshot { rev, state: Box::new(snap) }).await?;

    // Responses come from request tasks through this channel
    let (resp_tx, mut resp_rx) = mpsc::channel::<ServerMessage>(32);
    let mut link = false;
    let mut receiver = false;
    // Log lines while subscribed; the sender stays alive for the session so
    // recv never ends
    let (log_tx, mut log_rx) = mpsc::channel::<ipc::LogLine>(1024);
    let mut log_sub: Option<crate::logging::Subscription> = None;
    // RFCOMM channels this client opened: their events and the answers to
    // its rfcomm requests, in order
    let (rfcomm_tx, mut rfcomm_rx) = mpsc::unbounded_channel::<crate::rfcomm::Queued>();
    let rfcomm = crate::rfcomm::Channels::new(state.clone(), client.clone(), rfcomm_tx);
    let result = loop {
        tokio::select! {
            msg = read_frame::<_, serde_json::Value>(&mut rd) => {
                let msg = match msg.map(|m| m.map(|v| parse(&client, v))) {
                    Ok(Some(Ok(Frame::Message(m)))) => Ok(Some(m)),
                    Ok(Some(Ok(Frame::Unreadable(answer)))) => {
                        if let Err(e) = write_frame(&mut wr, &answer).await { break Err(e); }
                        continue;
                    }
                    Ok(Some(Err(e))) => Err(e.into()),
                    Ok(None) => Ok(None),
                    Err(e) => Err(e),
                };
                // Service mode: the request rules first
                let msg = match (msg, &caller) {
                    (Ok(Some(ClientMessage::Request { id, call })), Some(c)) => {
                        match crate::rules::check(c, &call, &*state) {
                            Ok(()) => Ok(Some(ClientMessage::Request { id, call })),
                            Err(e) => {
                                tracing::info!("IPC: {client} #{id} {} refused: {}", method_name(&call), e.code);
                                let m = ServerMessage::Response { id, result: None, error: Some(e) };
                                if let Err(e) = write_frame(&mut wr, &m).await { break Err(e); }
                                continue;
                            }
                        }
                    }
                    (msg, _) => msg,
                };
                match msg {
                    Ok(Some(ClientMessage::Request { id, call })) if rfcomm.request(id, &call) => {}
                    Ok(Some(ClientMessage::Request { id, call })) => {
                        if let Method::DiagSubscribe { logs: l, link: k, receiver: r } = &call {
                            log_sub = l.map(|level| {
                                let tx = log_tx.clone();
                                crate::logging::hub().subscribe_with(level, move |line| {
                                    !matches!(tx.try_send(line.clone()), Err(mpsc::error::TrySendError::Closed(_)))
                                })
                            });
                            set_subscribed(&state.link_subscribers, &mut link, *k);
                            let r = r.unwrap_or(false);
                            if set_subscribed(&state.receiver_subscribers, &mut receiver, r) && r {
                                // The state now, not only at the next tick
                                state.publish_receiver();
                            }
                        }
                        let (state, tx) = (state.clone(), resp_tx.clone());
                        let client = client.clone();
                        let ticket = caller.as_ref().and_then(|c| c.ticket.clone());
                        tokio::spawn(async move {
                            let method = method_name(&call);
                            tracing::trace!("IPC: {client} #{id} {call:?}");
                            let started = std::time::Instant::now();
                            let r = state.handle(call).await;
                            // The elevated helper's call: its result goes to
                            // the client that asked (app.run_as_admin) too
                            if let Some(t) = &ticket {
                                crate::rules::finish(t, &r);
                            }
                            let ms = started.elapsed().as_millis();
                            let msg = match r {
                                Ok(v) => {
                                    tracing::debug!("IPC: {client} #{id} {method} ok in {ms} ms");
                                    ServerMessage::Response { id, result: Some(v), error: None }
                                }
                                Err(e) => {
                                    tracing::info!("IPC: {client} #{id} {method} failed in {ms} ms: {} ({})", e.code, e.message.key);
                                    ServerMessage::Response { id, result: None, error: Some(e) }
                                }
                            };
                            let _ = tx.send(msg).await;
                        });
                    }
                    Ok(Some(ClientMessage::Hello { .. })) => {}
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                }
            }
            Some(resp) = resp_rx.recv() => {
                if let Err(e) = write_frame(&mut wr, &resp).await { break Err(e); }
            }
            ev = events.recv() => {
                match ev {
                    Ok((rev, event)) => {
                        let wanted = match &*event {
                            Event::Link(_) => link,
                            Event::Receiver(_) => receiver,
                            Event::Log(_) => false, // logs come from the log hub
                            _ => true,
                        };
                        if wanted {
                            let m = ServerMessage::Event { rev, event: Box::new((*event).clone()) };
                            if let Err(e) = write_frame(&mut wr, &m).await { break Err(e); }
                        }
                        if matches!(&*event, Event::Bye) { break Ok(()); }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let (rev, snap) = state.snapshot();
                        let m = ServerMessage::Snapshot { rev, state: Box::new(snap) };
                        if let Err(e) = write_frame(&mut wr, &m).await { break Err(e); }
                    }
                    Err(broadcast::error::RecvError::Closed) => break Ok(()),
                }
            }
            Some(q) = rfcomm_rx.recv() => {
                if let Err(e) = write_frame(&mut wr, &q.msg).await { break Err(e); }
                rfcomm.written(&q);
            }
            Some(line) = log_rx.recv() => {
                let (rev, _) = state.snapshot();
                let m = ServerMessage::Event { rev, event: Box::new(Event::Log(line)) };
                if let Err(e) = write_frame(&mut wr, &m).await { break Err(e); }
            }
        }
    };
    drop(log_sub);
    drop(rfcomm);
    set_subscribed(&state.link_subscribers, &mut link, false);
    set_subscribed(&state.receiver_subscribers, &mut receiver, false);
    tracing::info!("IPC: {client} disconnected");
    {
        let mut clients = lock_clients();
        if let Some(i) = clients.iter().position(|c| *c == client) {
            clients.remove(i);
        }
    }
    result
}

/// The clients connected now, by their hello's name ("bluecaster-ui/2.0.2",
/// "encodeck/1.14.0"), for the diagnostics
static CLIENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn lock_clients() -> std::sync::MutexGuard<'static, Vec<String>> {
    CLIENTS.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn clients() -> Vec<String> {
    lock_clients().clone()
}

/// A client's frame after the hello
enum Frame {
    Message(ClientMessage),
    /// A request this core cannot read: the answer to it
    Unreadable(ServerMessage),
}

/// A request that does not parse (a method this core does not know, from a
/// newer client, or bad parameters) is answered with ipc.bad_request rather
/// than ending the connection. Anything else unreadable ends it.
fn parse(client: &str, v: serde_json::Value) -> Result<Frame, serde_json::Error> {
    let id = (v.get("type").and_then(|t| t.as_str()) == Some("request"))
        .then(|| v.get("id").and_then(|i| i.as_u64()))
        .flatten();
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("?").to_owned();
    match serde_json::from_value::<ClientMessage>(v) {
        Ok(m) => Ok(Frame::Message(m)),
        Err(e) => {
            let Some(id) = id else { return Err(e) };
            // serde lists every method it knows: not for the client
            let why =
                if e.to_string().starts_with("unknown variant") { "unknown method".into() } else { e.to_string() };
            tracing::info!("IPC: {client} #{id} {method} not understood: {why}");
            let error = ErrorInfo {
                code: "ipc.bad_request".into(),
                message: Msg { key: "ipc.error.bad_request".into(), args: vec![method, why] },
            };
            Ok(Frame::Unreadable(ServerMessage::Response { id, result: None, error: Some(error) }))
        }
    }
}

/// "stream.start": the method's name on the wire
fn method_name(call: &Method) -> String {
    serde_json::to_value(call)
        .ok()
        .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

/// Counts this connection in or out of a subscription; true if it changed
fn set_subscribed(count: &AtomicUsize, current: &mut bool, wanted: bool) -> bool {
    if *current == wanted {
        return false;
    }
    *current = wanted;
    if wanted {
        count.fetch_add(1, Ordering::AcqRel);
    } else {
        count.fetch_sub(1, Ordering::AcqRel);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ui_of_another_version_is_told_which_side_is_older() {
        use ipc::service_state::{APP_OLDER_THAN_SERVICE, SERVICE_NEEDS_UPDATE};
        assert_eq!(ui_version_reject("bluecaster-ui/3.0.3", "3.0.3"), None);
        assert_eq!(ui_version_reject("bluecaster-ui/3.0.4", "3.0.3"), Some(SERVICE_NEEDS_UPDATE));
        assert_eq!(ui_version_reject("bluecaster-ui/3.0.2", "3.0.3"), Some(APP_OLDER_THAN_SERVICE));
        assert_eq!(ui_version_reject("bluecaster-ui/x", "3.0.3"), Some(APP_OLDER_THAN_SERVICE));
        // Not the UI: not asked
        assert_eq!(ui_version_reject("bluecaster-request/3.0.2", "3.0.3"), None);
        assert_eq!(ui_version_reject("encodeck/1.14.0", "3.0.3"), None);
    }

    /// The core answers under both names (test names, not the user's pipe)
    #[test]
    fn serves_the_new_and_the_old_name() {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("bluecaster-server-names-{pid}"));
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state = CoreState::new(
            Arc::new(bluecaster_core::Service::new(options, tx)),
            Box::new(|_| {}),
            Box::new(|| {}),
            Box::new(|_| {}),
        );
        let name = format!(r"\\.\pipe\bluecaster-test-server-{pid}");
        let old_name = format!(r"\\.\pipe\a2dpwb-test-server-{pid}");
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            tokio::spawn(serve(state, name.clone(), old_name.clone()));
            for n in [&name, &old_name] {
                let started = std::time::Instant::now();
                let pipe = loop {
                    match ClientOptions::new().open(n) {
                        Ok(p) => break p,
                        Err(e) if started.elapsed() > Duration::from_secs(5) => panic!("{n}: {e}"),
                        Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                    }
                };
                let (mut rd, mut wr) = tokio::io::split(pipe);
                let hello = ClientMessage::Hello { protocol: PROTOCOL_VERSION, client: "test".into() };
                write_frame(&mut wr, &hello).await.unwrap();
                let welcome: serde_json::Value = read_frame(&mut rd).await.unwrap().unwrap();
                assert_eq!(welcome["type"], "welcome", "{n}: {welcome}");
            }
            // A second core cannot have the new name
            assert!(win_sys::pipe::create_server(&name, true).is_err());
        });
        rt.shutdown_background();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Service mode through a real pipe, with the identity the test seam
    /// gives: a normal client is refused hid.code, an elevated one of our
    /// program gets past the rules (developer mode is off: developer.off),
    /// a client in session 0 that is not elevated may not connect
    #[test]
    fn service_mode_checks_each_client() {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("bluecaster-server-service-{pid}"));
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = bluecaster_core::ServiceOptions {
            config_dir: dir.clone(),
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let state = CoreState::new(
            Arc::new(bluecaster_core::Service::new(options, tx)),
            Box::new(|_| {}),
            Box::new(|| {}),
            Box::new(|_| {}),
        );
        let name = format!(r"\\.\pipe\bluecaster-test-service-rules-{pid}");
        let identity = |elevated: bool, session_id: u32| win_sys::pipe_peer::ClientIdentity {
            pid,
            session_id,
            user_sid: "S-1-5-21-1".into(),
            elevated,
            integrity: win_sys::pipe_peer::INTEGRITY_HIGH,
            image: std::env::current_exe().ok(),
        };
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            tokio::spawn(serve_service(state, name.clone()));
            // One request on a new connection: the response's error code, or
            // the reject
            let ask = |id: win_sys::pipe_peer::ClientIdentity, call: Method| {
                let name = name.clone();
                async move {
                    *TEST_IDENTITY.lock().unwrap() = Some(id);
                    let started = std::time::Instant::now();
                    let pipe = loop {
                        match ClientOptions::new().open(&name) {
                            Ok(p) => break p,
                            Err(e) if started.elapsed() > Duration::from_secs(5) => panic!("{e}"),
                            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                        }
                    };
                    let (mut rd, mut wr) = tokio::io::split(pipe);
                    let hello = ClientMessage::Hello { protocol: PROTOCOL_VERSION, client: "test".into() };
                    write_frame(&mut wr, &hello).await.unwrap();
                    // A refused client may find the pipe already closed
                    // here; the reject is still there to read
                    let _ = write_frame(&mut wr, &ClientMessage::Request { id: 1, call }).await;
                    loop {
                        let m: serde_json::Value = read_frame(&mut rd).await.unwrap().unwrap();
                        match m["type"].as_str() {
                            Some("reject") => return "reject".to_string(),
                            Some("response") => {
                                return m["error"]["code"].as_str().unwrap_or("ok").to_string();
                            }
                            _ => {}
                        }
                    }
                }
            };
            // hid.enable needs an administrator (hid.code is open)
            let enable = || Method::HidEnable { code: "1".into(), other_input_at_hand: true, risk_understood: true };
            assert_eq!(ask(identity(false, 1), enable()).await, ipc::access::NEEDS_ADMIN);
            assert_ne!(ask(identity(true, 1), enable()).await, ipc::access::NEEDS_ADMIN);
            assert_eq!(ask(identity(false, 1), Method::HidCode).await, "developer.off");
            assert_eq!(ask(identity(false, 1), Method::HidStatus).await, "ok");
            assert_eq!(ask(identity(true, 1), Method::AppQuit).await, ipc::access::NOT_IN_SERVICE);
            assert_eq!(ask(identity(false, 0), Method::HidStatus).await, "reject");
            // Another program's elevated process is a normal client
            let other =
                win_sys::pipe_peer::ClientIdentity { image: Some("C:\\x\\cmd.exe".into()), ..identity(true, 1) };
            assert_eq!(ask(other, enable()).await, ipc::access::NEEDS_ADMIN);
        });
        *TEST_IDENTITY.lock().unwrap() = None;
        rt.shutdown_background();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subscriptions_count_once_per_connection() {
        let count = AtomicUsize::new(0);
        let (mut a, mut b) = (false, false);
        assert!(set_subscribed(&count, &mut a, true));
        assert!(!set_subscribed(&count, &mut a, true));
        assert!(set_subscribed(&count, &mut b, true));
        assert_eq!(count.load(Ordering::Acquire), 2);
        assert!(set_subscribed(&count, &mut a, false));
        assert!(!set_subscribed(&count, &mut a, false));
        assert_eq!(count.load(Ordering::Acquire), 1);
    }

    /// A request of a method this core does not know is answered, not the
    /// end of the connection; other unreadable frames still end it
    #[test]
    fn unknown_requests_are_answered() {
        let v = serde_json::json!({"type": "request", "id": 7, "method": "future.thing", "params": {}});
        let Ok(Frame::Unreadable(ServerMessage::Response { id, result: None, error: Some(e) })) = parse("t", v) else {
            panic!("not answered");
        };
        assert_eq!(
            (id, e.code.as_str(), e.message.args.as_slice()),
            (7, "ipc.bad_request", ["future.thing".to_string(), "unknown method".to_string()].as_slice())
        );
        let v = serde_json::json!({"type": "request", "id": 8, "method": "rfcomm.close", "params": {}});
        assert!(matches!(parse("t", v), Ok(Frame::Unreadable(ServerMessage::Response { id: 8, .. }))));
        let v = serde_json::json!({"type": "request", "id": 9, "method": "state.snapshot"});
        assert!(matches!(parse("t", v), Ok(Frame::Message(ClientMessage::Request { id: 9, .. }))));
        assert!(parse("t", serde_json::json!({"type": "goodbye"})).is_err());
        assert!(parse("t", serde_json::json!({"type": "request", "method": "x"})).is_err());
    }
}
