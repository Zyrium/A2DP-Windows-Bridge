//! Group `endpoints`: Windows' audio endpoints changing (one came, went, or
//! a default device changed), and the apps that have an audio session (the
//! Application source's list).
//!
//! The watch runs where the endpoints are: in this process
//! ([`LocalHost`]) or in the agent, which sends each change as an
//! [`Event::Changed`]. A new agent says [`Event::Ready`]: the core watches
//! through it again and takes it as a change (one may have come while no
//! agent watched).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use audio_capture::AudioApp;
use serde::{Deserialize, Serialize};

use crate::hub::Hub;
use crate::wire::{decode, to_body};
use crate::{LocalHost, RemoteHost};

pub const GROUP: &str = "endpoints";

/// How long the watch's start waits for the agent; without one, the next
/// agent starts it
const WATCH_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Sends [`Event::Changed`] for each change from now on (again: no
    /// second watch). Reply: null
    Watch,
    /// Reply: null
    Unwatch,
    /// Reply: `Vec<App>`
    AudioApps,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A new connection's handler is up
    Ready,
    /// An endpoint came, went, was enabled or disabled, or a default
    /// device changed
    Changed,
}

/// [`AudioApp`] on the wire
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub process_id: u32,
    pub exe: String,
    pub name: String,
}

impl From<AudioApp> for App {
    fn from(a: AudioApp) -> App {
        App { process_id: a.process_id, exe: a.exe, name: a.name }
    }
}

impl From<App> for AudioApp {
    fn from(a: App) -> AudioApp {
        AudioApp { process_id: a.process_id, exe: a.exe, name: a.name }
    }
}

/// Calls the watch's function until dropped
#[must_use = "the watch ends when this is dropped"]
pub struct EndpointsWatch {
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for EndpointsWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EndpointsWatch")
    }
}

impl Drop for EndpointsWatch {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

pub type Changed = Box<dyn Fn() + Send + Sync>;

pub trait EndpointsHost {
    /// Calls `changed` for each change of Windows' audio endpoints, on a
    /// thread of its own (it may block; changes meanwhile wait), until the
    /// watch is dropped. One watch per process.
    fn watch_endpoints(&self, changed: Changed) -> Result<EndpointsWatch, String>;
    /// Apps with an audio session, one per executable, sorted by name
    /// (without this process). Blocks: not for an audio thread.
    fn audio_apps(&self) -> Vec<AudioApp>;
}

enum Msg {
    Changed,
    Stop,
}

/// Joins `thread` after `tx` said stop
fn stopper(tx: mpsc::Sender<Msg>, thread: JoinHandle<()>) -> Box<dyn FnOnce() + Send> {
    Box::new(move || {
        let _ = tx.send(Msg::Stop);
        let _ = thread.join();
    })
}

impl EndpointsHost for LocalHost {
    fn watch_endpoints(&self, changed: Changed) -> Result<EndpointsWatch, String> {
        let (tx, rx) = mpsc::channel::<Msg>();
        let (started_tx, started) = mpsc::sync_channel::<Result<(), String>>(1);
        let from_windows = tx.clone();
        // COM and the registration live on this thread, from start to end
        let thread = std::thread::Builder::new()
            .name("audio-endpoints-watch".into())
            .spawn(move || {
                let _com = match win_sys::audio_policy::ComApartment::init_mta() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = started_tx.send(Err(format!("COM: HRESULT 0x{:08x}", e.code().0 as u32)));
                        return;
                    }
                };
                let watch = win_sys::audio_notify::EndpointWatch::new(move || {
                    let _ = from_windows.send(Msg::Changed);
                });
                let watch = match watch {
                    Ok(w) => w,
                    Err(e) => {
                        let _ = started_tx.send(Err(format!("HRESULT 0x{:08x}", e.code().0 as u32)));
                        return;
                    }
                };
                let _ = started_tx.send(Ok(()));
                while let Ok(Msg::Changed) = rx.recv() {
                    changed();
                }
                drop(watch);
            })
            .map_err(|e| e.to_string())?;
        match started.recv() {
            Ok(Ok(())) => Ok(EndpointsWatch { stop: Some(stopper(tx, thread)) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the watch thread ended".into()),
        }
    }

    fn audio_apps(&self) -> Vec<AudioApp> {
        audio_capture::audio_apps()
    }
}

/// Asks the agent to watch; false if it did not get there
fn watch_through(hub: &Hub) -> bool {
    match hub.request_on(GROUP, to_body(&Request::Watch), WATCH_TIMEOUT) {
        Ok(_) => true,
        Err(e) => {
            tracing::info!("audio endpoints: not watched yet ({e}), again when a session agent connects");
            false
        }
    }
}

impl EndpointsHost for RemoteHost {
    fn watch_endpoints(&self, changed: Changed) -> Result<EndpointsWatch, String> {
        let (tx, rx) = mpsc::channel::<Msg>();
        let active = Arc::new(AtomicBool::new(true));
        let weak = Arc::downgrade(&self.hub);
        let (on, signal) = (active.clone(), tx.clone());
        // On the hub's thread: never blocks
        self.hub.on_event(GROUP, move |body| {
            if !on.load(Ordering::Acquire) {
                return;
            }
            match decode::<Event>(body) {
                Ok(Event::Changed) => {
                    let _ = signal.send(Msg::Changed);
                }
                Ok(Event::Ready) => {
                    let (weak, signal) = (weak.clone(), signal.clone());
                    let spawned = std::thread::Builder::new().name("audio-endpoints-sync".into()).spawn(move || {
                        if let Some(hub) = weak.upgrade()
                            && watch_through(&hub)
                        {
                            tracing::debug!("audio endpoints: watched by the new session agent");
                        }
                        // A change may have come while no agent watched
                        let _ = signal.send(Msg::Changed);
                    });
                    if let Err(e) = spawned {
                        tracing::warn!("audio endpoints: not watched by the new session agent: {e}");
                    }
                }
                Err(_) => tracing::debug!("audio endpoints: an event not understood"),
            }
        });
        let thread = std::thread::Builder::new()
            .name("audio-endpoints-events".into())
            .spawn(move || {
                while let Ok(Msg::Changed) = rx.recv() {
                    changed();
                }
            })
            .map_err(|e| e.to_string())?;
        watch_through(&self.hub);
        let hub = self.hub.clone();
        let stop = stopper(tx, thread);
        Ok(EndpointsWatch {
            stop: Some(Box::new(move || {
                active.store(false, Ordering::Release);
                stop();
                if let Some(agent) = hub.agent() {
                    hub.notify(agent.generation, GROUP, to_body(&Request::Unwatch));
                }
            })),
        })
    }

    fn audio_apps(&self) -> Vec<AudioApp> {
        let me = std::process::id();
        match self.call::<Vec<App>>(GROUP, &Request::AudioApps) {
            Ok(list) => list.into_iter().filter(|a| a.process_id != me).map(AudioApp::from).collect(),
            Err(e) => {
                tracing::warn!("session host: audio apps not listed: {e}");
                Vec::new()
            }
        }
    }
}

/// [`EndpointsHost::watch_endpoints`] of this process's host
pub fn watch_endpoints(changed: impl Fn() + Send + Sync + 'static) -> Result<EndpointsWatch, String> {
    EndpointsHost::watch_endpoints(crate::host(), Box::new(changed))
}

/// [`EndpointsHost::audio_apps`] of this process's host
pub fn audio_apps() -> Vec<AudioApp> {
    EndpointsHost::audio_apps(crate::host())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_have_a_stable_shape() {
        assert_eq!(serde_json::to_string(&Request::AudioApps).unwrap(), r#"{"op":"audio_apps"}"#);
        assert_eq!(serde_json::to_string(&Event::Changed).unwrap(), r#"{"kind":"changed"}"#);
        let app = AudioApp { process_id: 4, exe: "a.exe".into(), name: "A (a.exe)".into() };
        assert_eq!(AudioApp::from(App::from(app.clone())), app);
    }

    /// Registers with Windows and stops (nothing changes on this PC)
    #[test]
    fn the_local_watch_starts_and_stops() {
        let watch = LocalHost.watch_endpoints(Box::new(|| {})).unwrap();
        drop(watch);
    }
}
