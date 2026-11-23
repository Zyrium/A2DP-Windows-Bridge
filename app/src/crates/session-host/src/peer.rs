//! One end of an agent connection, the same on both sides: numbered
//! requests and their replies, events, and the end of the connection.
//!
//! [`Peer`] is what other threads use: [`Peer::request`] queues the request
//! and blocks the calling thread (never a runtime or audio thread) until
//! the reply, the timeout or the end of the connection, which fails every
//! request still waiting with [`HostError::AgentLost`]. [`run`] drives the
//! connection on a tokio runtime: a writer task sends what is queued, the
//! reader completes replies and hands requests and events to the caller.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::wire::{Frame, HostError, WireError};

type Waiter = std_mpsc::SyncSender<Result<Value, HostError>>;

/// What the other side sent that is not a reply
#[derive(Debug)]
pub(crate) enum Incoming {
    Request {
        id: u64,
        group: String,
        body: Value,
    },
    Event {
        group: String,
        body: Value,
    },
    /// A frame that belongs to the handshake, out of place
    Other,
}

/// How a connection ended
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// The other side closed it (or went away)
    Closed,
    /// The other side said [`Frame::Bye`]
    Bye,
    /// Reading or writing failed
    Failed(String),
}

#[derive(Debug)]
pub struct Peer {
    out: mpsc::UnboundedSender<Frame>,
    /// Frames queued and not written yet
    queued: AtomicUsize,
    pending: Mutex<HashMap<u64, Waiter>>,
    next_id: AtomicU64,
    next_seq: AtomicU64,
    closed: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Peer {
    pub(crate) fn new() -> (Arc<Peer>, mpsc::UnboundedReceiver<Frame>) {
        let (out, rx) = mpsc::unbounded_channel();
        let peer = Peer {
            out,
            queued: AtomicUsize::new(0),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            next_seq: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        };
        (Arc::new(peer), rx)
    }

    pub(crate) fn send(&self, frame: Frame) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        if self.out.send(frame).is_err() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Sends a request and waits for its reply. Blocks the calling thread:
    /// never call it on a tokio runtime thread or an audio thread.
    pub fn request(&self, group: &str, body: Value, timeout: Duration) -> Result<Value, HostError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std_mpsc::sync_channel(1);
        lock(&self.pending).insert(id, tx);
        // After the insert: a close that ran before it is seen here
        if self.closed.load(Ordering::SeqCst) || !self.send(Frame::Request { id, group: group.to_string(), body }) {
            lock(&self.pending).remove(&id);
            return Err(HostError::AgentLost);
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.pending).remove(&id);
                Err(HostError::Timeout)
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(HostError::AgentLost),
        }
    }

    /// Sends a request whose reply nobody waits for (it is dropped when it
    /// comes). False if the connection is closed.
    pub fn notify(&self, group: &str, body: Value) -> bool {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(Frame::Request { id, group: group.to_string(), body })
    }

    /// Sends an event. False if the connection is closed.
    pub fn event(&self, group: &str, body: Value) -> bool {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.send(Frame::Event { seq, group: group.to_string(), body })
    }

    /// Sends an event unless more than `limit` frames wait to be written
    pub(crate) fn event_bounded(&self, group: &str, body: Value, limit: usize) -> bool {
        self.queued.load(Ordering::Relaxed) <= limit && self.event(group, body)
    }

    pub(crate) fn reply(&self, id: u64, result: Result<Value, WireError>) {
        self.send(Frame::Reply { id, result });
    }

    fn complete(&self, id: u64, result: Result<Value, HostError>) {
        // None: timed out already, or a notify
        if let Some(waiter) = lock(&self.pending).remove(&id) {
            let _ = waiter.try_send(result);
        }
    }

    /// Ends the connection for its users: requests still waiting fail with
    /// [`HostError::AgentLost`], later ones at once. The count failed.
    pub(crate) fn close(&self) -> usize {
        self.closed.store(true, Ordering::SeqCst);
        let waiting: Vec<Waiter> = lock(&self.pending).drain().map(|(_, w)| w).collect();
        for w in &waiting {
            let _ = w.try_send(Err(HostError::AgentLost));
        }
        waiting.len()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Requests waiting for their reply
    pub fn waiting(&self) -> usize {
        lock(&self.pending).len()
    }
}

fn is_disconnect(e: &ipc::FrameError) -> bool {
    matches!(e, ipc::FrameError::Io(io) if matches!(
        io.kind(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
    ) || io.raw_os_error() == Some(109) || io.raw_os_error() == Some(233))
}

/// Drives a connection until it ends: writes what `peer` queues (`rx`),
/// completes replies, hands requests and events to `incoming` (on the
/// runtime thread: it must not block). Closes `peer` when it returns.
pub(crate) async fn run<S>(
    peer: Arc<Peer>,
    mut rx: mpsc::UnboundedReceiver<Frame>,
    stream: S,
    mut incoming: impl FnMut(Incoming),
) -> End
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let writer_peer = peer.clone();
    let mut writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let written = ipc::write_frame(&mut writer, &frame).await;
            writer_peer.queued.fetch_sub(1, Ordering::Relaxed);
            if let Err(e) = written {
                return if is_disconnect(&e) { End::Closed } else { End::Failed(format!("write: {e}")) };
            }
        }
        End::Closed
    });
    let end = loop {
        tokio::select! {
            read = ipc::read_frame::<_, Frame>(&mut reader) => match read {
                Ok(Some(Frame::Reply { id, result })) => peer.complete(id, result.map_err(HostError::Remote)),
                Ok(Some(Frame::Request { id, group, body })) => incoming(Incoming::Request { id, group, body }),
                Ok(Some(Frame::Event { group, body, .. })) => incoming(Incoming::Event { group, body }),
                Ok(Some(Frame::Bye)) => break End::Bye,
                Ok(Some(_)) => incoming(Incoming::Other),
                Ok(None) => break End::Closed,
                Err(e) if is_disconnect(&e) => break End::Closed,
                Err(e) => break End::Failed(format!("read: {e}")),
            },
            // The writer ends only when writing failed
            written = &mut writer_task => {
                break written.unwrap_or_else(|e| End::Failed(format!("writer: {e}")));
            }
        }
    };
    peer.close();
    writer_task.abort();
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::ErrorCode;
    use std::time::Instant;

    /// Two peers joined by an in-process pipe, each driven on its own
    /// runtime thread; `serve` answers the second one's incoming requests
    struct Pair {
        a: Arc<Peer>,
        b: Arc<Peer>,
        events: std_mpsc::Receiver<(String, Value)>,
        a_end: std::thread::JoinHandle<End>,
        b_end: std::thread::JoinHandle<End>,
    }

    fn pair(serve: impl Fn(&Arc<Peer>, u64, String, Value) + Send + 'static) -> Pair {
        let (sa, sb) = tokio::io::duplex(1 << 16);
        let (a, a_rx) = Peer::new();
        let (b, b_rx) = Peer::new();
        let (ev_tx, events) = std_mpsc::channel();
        let ra = a.clone();
        let a_end = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(run(ra, a_rx, sa, move |inc| {
                if let Incoming::Event { group, body } = inc {
                    let _ = ev_tx.send((group, body));
                }
            }))
        });
        let rb = b.clone();
        let b_end = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let me = rb.clone();
            rt.block_on(run(rb, b_rx, sb, move |inc| {
                if let Incoming::Request { id, group, body } = inc {
                    serve(&me, id, group, body);
                }
            }))
        });
        Pair { a, b, events, a_end, b_end }
    }

    fn echo(peer: &Arc<Peer>, id: u64, group: String, body: Value) {
        match group.as_str() {
            "echo" => peer.reply(id, Ok(body)),
            // Answered later, from another thread
            "slow" => {
                let peer = peer.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(body.as_u64().unwrap_or(0)));
                    peer.reply(id, Ok(Value::Bool(true)));
                });
            }
            "never" => {}
            "fail" => peer.reply(id, Err(WireError::new(ErrorCode::Failed, "no"))),
            "event" => {
                peer.event("e", body.clone());
                peer.reply(id, Ok(Value::Null));
            }
            _ => peer.reply(id, Err(WireError::new(ErrorCode::UnknownGroup, group))),
        }
    }

    const T: Duration = Duration::from_secs(5);

    #[test]
    fn requests_get_their_own_replies() {
        let p = pair(echo);
        assert_eq!(p.a.request("echo", Value::from(5), T).unwrap(), Value::from(5));
        // Many at once from several threads, answered out of order
        let threads: Vec<_> = (0..8u64)
            .map(|i| {
                let a = p.a.clone();
                std::thread::spawn(move || {
                    let slow = a.request("slow", Value::from(80 - i * 10), T).unwrap();
                    let echoed = a.request("echo", Value::from(i), T).unwrap();
                    (slow, echoed)
                })
            })
            .collect();
        for (i, t) in threads.into_iter().enumerate() {
            assert_eq!(t.join().unwrap(), (Value::Bool(true), Value::from(i as u64)));
        }
        let err = p.a.request("fail", Value::Null, T).unwrap_err();
        assert!(matches!(&err, HostError::Remote(w) if w.code == ErrorCode::Failed), "{err}");
        let err = p.a.request("nothing", Value::Null, T).unwrap_err();
        assert!(matches!(&err, HostError::Remote(w) if w.code == ErrorCode::UnknownGroup), "{err}");
        assert_eq!(p.a.waiting(), 0);
    }

    #[test]
    fn a_request_times_out_and_its_late_reply_is_dropped() {
        let p = pair(echo);
        let started = Instant::now();
        assert_eq!(p.a.request("slow", Value::from(400), Duration::from_millis(100)), Err(HostError::Timeout));
        assert!(started.elapsed() < Duration::from_millis(350));
        assert_eq!(p.a.waiting(), 0);
        assert_eq!(p.a.request("never", Value::Null, Duration::from_millis(50)), Err(HostError::Timeout));
        // The late reply comes and goes; the link still works
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(p.a.request("echo", Value::from(1), T).unwrap(), Value::from(1));
    }

    #[test]
    fn events_arrive_in_order() {
        let p = pair(echo);
        for i in 0..20 {
            assert!(p.b.event("e", Value::from(i)));
        }
        p.a.request("event", Value::from(99), T).unwrap();
        let got: Vec<Value> = (0..21).map(|_| p.events.recv_timeout(T).unwrap().1).collect();
        let want: Vec<Value> = (0..20).map(Value::from).chain([Value::from(99)]).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn the_end_of_the_connection_fails_waiting_requests_as_lost() {
        let p = pair(echo);
        let a = p.a.clone();
        let waiting = std::thread::spawn(move || a.request("never", Value::Null, Duration::from_secs(30)));
        let deadline = Instant::now() + T;
        while p.a.waiting() == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        // The other side goes away: its connection drops
        let started = Instant::now();
        p.b.send(Frame::Bye);
        assert_eq!(waiting.join().unwrap(), Err(HostError::AgentLost));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(p.a_end.join().unwrap(), End::Bye);
        // b's own end: a's half of the pipe went with a's runtime
        assert_eq!(p.b_end.join().unwrap(), End::Closed);
        assert!(p.a.is_closed());
        // Later requests fail at once
        assert_eq!(p.a.request("echo", Value::Null, T), Err(HostError::AgentLost));
        assert!(!p.a.event("e", Value::Null));
    }

    #[test]
    fn bounded_events_stop_at_the_limit() {
        let (peer, _rx) = Peer::new();
        // Nothing writes: the queue only grows
        assert!(peer.event_bounded("log", Value::Null, 2));
        assert!(peer.event_bounded("log", Value::Null, 2));
        assert!(peer.event_bounded("log", Value::Null, 2));
        assert!(!peer.event_bounded("log", Value::Null, 2));
        assert_eq!(peer.close(), 0);
        assert!(!peer.event("log", Value::Null));
    }
}
