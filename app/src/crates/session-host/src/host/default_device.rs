//! Group `default_device`: switching Windows' default output and input and
//! restoring them, and reading the default input.
//!
//! The agent holds what the core switched on the user's behalf: "the
//! default output is ours, the device before was R" and "the default
//! input is our cable for these roles, each role's device before was ...".
//! When the core goes away (exits or dies), the agent restores both, as the
//! core's own guards do when a session ends. The core keeps the same
//! ([`Mirror`]) and gives it to each new agent: an agent that died while a
//! switch was held is replaced by one that applies the switch again and
//! holds it, and a restore the dead agent did not get is done by the next.
//!
//! The requests wait at most [`OP_TIMEOUT`]: one that does not reach an
//! agent is kept in the mirror and done when the next agent connects (its
//! handler says [`Event::Ready`]).

use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use audio_capture::{CaptureError, EndpointRole};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::devices::capture_error;
use crate::hub::Hub;
use crate::wire::{decode, decode_reply, to_body};
use crate::{HostError, LocalHost, RemoteHost};

pub const GROUP: &str = "default_device";

/// How long a switch or restore waits for the agent (a session start waits
/// on it); one that did not get through is done by the next agent
pub const OP_TIMEOUT: Duration = Duration::from_secs(3);

/// A default-device role on the wire
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Console,
    Multimedia,
    Communications,
}

impl From<EndpointRole> for Role {
    fn from(r: EndpointRole) -> Role {
        match r {
            EndpointRole::Console => Role::Console,
            EndpointRole::Multimedia => Role::Multimedia,
            EndpointRole::Communications => Role::Communications,
        }
    }
}

impl From<Role> for EndpointRole {
    fn from(r: Role) -> EndpointRole {
        match r {
            Role::Console => EndpointRole::Console,
            Role::Multimedia => EndpointRole::Multimedia,
            Role::Communications => EndpointRole::Communications,
        }
    }
}

/// The default output switched to `to`; `restore` is the device before
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderHold {
    pub to: String,
    pub restore: String,
}

/// The default input switched to `ours` for the roles in `restore`, each
/// with its device before (restored only while ours is still the default)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureHold {
    pub ours: String,
    pub restore: Vec<(Role, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Makes `to` the default output and holds `restore` (None: nothing to
    /// restore). Reply: null, or a failure with the capture error
    SwitchRender { to: String, restore: Option<String> },
    /// Makes `id` the default output again; nothing held any more. Reply
    /// as for `switch_render`
    RestoreRender { id: String },
    /// Reply: `Option<String>`
    DefaultCapture { role: Role },
    /// The recording side of a render endpoint. Reply: `Option<String>`
    PairedCapture { render: String },
    /// Reply as for `switch_render`
    SetCapture { id: String, role: Role },
    /// Holds `hold` (None: nothing). `apply`: makes `ours` the default for
    /// its roles again first. Reply: null
    HoldCapture { hold: Option<CaptureHold>, apply: bool },
    /// Restores `hold`'s roles that are still ours; nothing held any more.
    /// Reply: null
    ReleaseCapture { hold: CaptureHold },
}

/// What the agent's handler sends unasked
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A new connection's handler is up: the core gives it what it holds
    Ready,
}

/// The facade's default-device methods. They block until Windows (or the
/// agent) answers: not for the BTstack thread or an audio thread.
pub trait DefaultDeviceHost {
    /// Makes `to` the default output device (Console and Multimedia).
    /// `restore`: the device to restore if the core goes away (None:
    /// nothing to restore). Changes the user's setting: the caller restores
    /// it with [`DefaultDeviceHost::restore_render_default`].
    fn switch_render_default(&self, to: &str, restore: Option<&str>) -> Result<(), CaptureError>;
    /// Makes `id` the default output device again, ending the switch
    fn restore_render_default(&self, id: &str) -> Result<(), CaptureError>;
    /// The default input device's ID for `role`
    fn default_capture_device(&self, role: EndpointRole) -> Option<String>;
    /// The capture endpoint paired with the render endpoint `render_id`
    fn paired_capture_device(&self, render_id: &str) -> Option<String>;
    /// Makes `id` the default input device for `role`
    fn set_default_capture_device(&self, id: &str, role: EndpointRole) -> Result<(), CaptureError>;
    /// What the default-input switch holds after each change: `ours` for
    /// the roles in `restore`, each with its device before (None: nothing
    /// switched). Kept to be restored if the core goes away.
    fn hold_capture_default(&self, ours: Option<&str>, restore: &[(EndpointRole, String)]);
}

impl DefaultDeviceHost for LocalHost {
    fn switch_render_default(&self, to: &str, _restore: Option<&str>) -> Result<(), CaptureError> {
        audio_capture::set_default_render_device(to)
    }

    fn restore_render_default(&self, id: &str) -> Result<(), CaptureError> {
        audio_capture::set_default_render_device(id)
    }

    fn default_capture_device(&self, role: EndpointRole) -> Option<String> {
        audio_capture::default_capture_device(role)
    }

    fn paired_capture_device(&self, render_id: &str) -> Option<String> {
        audio_capture::paired_capture_device(render_id)
    }

    fn set_default_capture_device(&self, id: &str, role: EndpointRole) -> Result<(), CaptureError> {
        audio_capture::set_default_capture_device(id, role)
    }

    // In this process the core's own guards restore (nothing outlives it)
    fn hold_capture_default(&self, _ours: Option<&str>, _restore: &[(EndpointRole, String)]) {}
}

/// How the mirror reaches the agent (the hub, or a fake in tests)
pub(crate) trait Agent {
    fn call(&self, request: &Request) -> Result<Value, HostError>;
}

struct HubAgent<'a>(&'a Hub);

impl Agent for HubAgent<'_> {
    fn call(&self, request: &Request) -> Result<Value, HostError> {
        self.0.request_on(GROUP, to_body(request), OP_TIMEOUT).map(|(v, _)| v)
    }
}

/// Whether a request got to an agent (its handler answered, even with an
/// error); one that timed out may have, and is given to the next agent too
fn reached(r: &Result<Value, HostError>) -> bool {
    matches!(r, Ok(_) | Err(HostError::Remote(_)))
}

fn unit(r: Result<Value, HostError>) -> Result<(), CaptureError> {
    r.and_then(decode_reply::<()>).map_err(capture_error)
}

/// What the core holds through the agent, to give to a new agent
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Mirror {
    render: Option<RenderHold>,
    /// A restore that did not reach an agent: the next one does it, unless
    /// the next switch takes it as its device to restore
    lost_restore: Option<String>,
    capture: Option<CaptureHold>,
    /// A release of which a step did not reach an agent
    lost_release: Option<CaptureHold>,
    /// A capture switch or restore did not reach an agent since the last
    /// hold
    capture_failed: bool,
}

impl Mirror {
    const fn new() -> Mirror {
        Mirror { render: None, lost_restore: None, capture: None, lost_release: None, capture_failed: false }
    }

    fn switch_render(&mut self, agent: &dyn Agent, to: &str, restore: Option<&str>) -> Result<(), CaptureError> {
        // Held even when the switch fails: Windows may have switched one
        // role before the other failed (the guard restores it too)
        self.render = restore.map(|r| RenderHold { to: to.to_string(), restore: r.to_string() });
        let request = Request::SwitchRender { to: to.to_string(), restore: restore.map(str::to_string) };
        unit(agent.call(&request))
    }

    fn restore_render(&mut self, agent: &dyn Agent, id: &str) -> Result<(), CaptureError> {
        self.render = None;
        let r = agent.call(&Request::RestoreRender { id: id.to_string() });
        if !reached(&r) {
            tracing::info!("default output: the restore waits for the next session agent");
            self.lost_restore = Some(id.to_string());
        }
        unit(r)
    }

    fn set_capture(&mut self, agent: &dyn Agent, id: &str, role: Role) -> Result<(), CaptureError> {
        let r = agent.call(&Request::SetCapture { id: id.to_string(), role });
        if !reached(&r) {
            self.capture_failed = true;
        }
        unit(r)
    }

    fn hold_capture(&mut self, agent: &dyn Agent, hold: Option<CaptureHold>) {
        let before = std::mem::replace(&mut self.capture, hold.clone());
        let failed = std::mem::take(&mut self.capture_failed);
        match (hold, before) {
            // A release that did not get through in full: the agent
            // restores what is still ours (this one, or the next)
            (None, Some(before)) if failed => {
                if !reached(&agent.call(&Request::ReleaseCapture { hold: before.clone() })) {
                    tracing::info!("default input: the restore waits for the next session agent");
                    self.lost_release = Some(before);
                }
            }
            (hold, _) => {
                // `failed`: a switch that did not get through is applied
                // again (by this agent, or the next one when it connects)
                let _ = agent.call(&Request::HoldCapture { hold, apply: failed });
            }
        }
    }

    /// A new agent connected: it does what the last one did not get, and
    /// holds (and applies again) what is switched
    fn resync(&mut self, agent: &dyn Agent) {
        if self.render.is_none() && self.lost_restore.is_none() && self.capture.is_none() && self.lost_release.is_none()
        {
            return;
        }
        let mut lost = 0;
        if let Some(id) = self.lost_restore.take()
            && !reached(&agent.call(&Request::RestoreRender { id: id.clone() }))
        {
            self.lost_restore = Some(id);
            lost += 1;
        }
        if let Some(h) = &self.render {
            let request = Request::SwitchRender { to: h.to.clone(), restore: Some(h.restore.clone()) };
            if !reached(&agent.call(&request)) {
                lost += 1;
            }
        }
        if let Some(hold) = self.lost_release.take()
            && !reached(&agent.call(&Request::ReleaseCapture { hold: hold.clone() }))
        {
            self.lost_release = Some(hold);
            lost += 1;
        }
        if let Some(hold) = &self.capture {
            if reached(&agent.call(&Request::HoldCapture { hold: Some(hold.clone()), apply: true })) {
                self.capture_failed = false;
            } else {
                lost += 1;
            }
        }
        tracing::info!(
            "default devices: given to the new session agent (output held: {}, input held: {}, {lost} not delivered)",
            self.render.is_some(),
            self.capture.is_some()
        );
    }
}

static MIRROR: Mutex<Mirror> = Mutex::new(Mirror::new());
/// The hub whose agents get [`MIRROR`]
static ATTACHED: Mutex<Option<Weak<Hub>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Gives the mirror to each new agent of `hub` (once per hub; a new hub
/// starts with an empty mirror)
fn attach(hub: &Arc<Hub>) {
    let mut attached = lock(&ATTACHED);
    if attached.as_ref().and_then(Weak::upgrade).is_some_and(|h| Arc::ptr_eq(&h, hub)) {
        return;
    }
    *attached = Some(Arc::downgrade(hub));
    *lock(&MIRROR) = Mirror::default();
    let weak = Arc::downgrade(hub);
    // On the hub's thread: the requests run on a thread of their own
    hub.on_event(GROUP, move |body| {
        if decode::<Event>(body) != Ok(Event::Ready) {
            return;
        }
        let weak = weak.clone();
        let spawned = std::thread::Builder::new().name("default-device-sync".into()).spawn(move || {
            if let Some(hub) = weak.upgrade() {
                lock(&MIRROR).resync(&HubAgent(&hub));
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("default devices: not given to the new session agent: {e}");
        }
    });
}

impl RemoteHost {
    fn mirror(&self, f: impl FnOnce(&mut Mirror, &dyn Agent) -> Result<(), CaptureError>) -> Result<(), CaptureError> {
        attach(&self.hub);
        f(&mut lock(&MIRROR), &HubAgent(&self.hub))
    }
}

impl DefaultDeviceHost for RemoteHost {
    fn switch_render_default(&self, to: &str, restore: Option<&str>) -> Result<(), CaptureError> {
        self.mirror(|m, a| m.switch_render(a, to, restore))
    }

    fn restore_render_default(&self, id: &str) -> Result<(), CaptureError> {
        self.mirror(|m, a| m.restore_render(a, id))
    }

    fn default_capture_device(&self, role: EndpointRole) -> Option<String> {
        let request = Request::DefaultCapture { role: role.into() };
        HubAgent(&self.hub).call(&request).and_then(decode_reply).unwrap_or_else(|e| {
            tracing::warn!("session host: default input not read: {e}");
            None
        })
    }

    fn paired_capture_device(&self, render_id: &str) -> Option<String> {
        let request = Request::PairedCapture { render: render_id.to_string() };
        HubAgent(&self.hub).call(&request).and_then(decode_reply).unwrap_or_else(|e| {
            tracing::warn!("session host: recording side not read: {e}");
            None
        })
    }

    fn set_default_capture_device(&self, id: &str, role: EndpointRole) -> Result<(), CaptureError> {
        self.mirror(|m, a| m.set_capture(a, id, role.into()))
    }

    fn hold_capture_default(&self, ours: Option<&str>, restore: &[(EndpointRole, String)]) {
        let hold = ours.map(|ours| CaptureHold {
            ours: ours.to_string(),
            restore: restore.iter().map(|(r, id)| ((*r).into(), id.clone())).collect(),
        });
        let _ = self.mirror(|m, a| {
            m.hold_capture(a, hold);
            Ok(())
        });
    }
}

/// [`DefaultDeviceHost::switch_render_default`] of this process's host
pub fn switch_render_default(to: &str, restore: Option<&str>) -> Result<(), CaptureError> {
    DefaultDeviceHost::switch_render_default(crate::host(), to, restore)
}

/// [`DefaultDeviceHost::restore_render_default`] of this process's host
pub fn restore_render_default(id: &str) -> Result<(), CaptureError> {
    DefaultDeviceHost::restore_render_default(crate::host(), id)
}

/// A restore of the default output that no agent got yet (the agent was
/// lost), taken by the caller as the device to restore after its own switch
pub fn take_lost_restore() -> Option<String> {
    lock(&MIRROR).lost_restore.take()
}

/// [`DefaultDeviceHost::default_capture_device`] of this process's host
pub fn default_capture_device(role: EndpointRole) -> Option<String> {
    DefaultDeviceHost::default_capture_device(crate::host(), role)
}

/// [`DefaultDeviceHost::paired_capture_device`] of this process's host
pub fn paired_capture_device(render_id: &str) -> Option<String> {
    DefaultDeviceHost::paired_capture_device(crate::host(), render_id)
}

/// [`DefaultDeviceHost::set_default_capture_device`] of this process's host
pub fn set_default_capture_device(id: &str, role: EndpointRole) -> Result<(), CaptureError> {
    DefaultDeviceHost::set_default_capture_device(crate::host(), id, role)
}

/// [`DefaultDeviceHost::hold_capture_default`] of this process's host
pub fn hold_capture_default(ours: Option<&str>, restore: &[(EndpointRole, String)]) {
    DefaultDeviceHost::hold_capture_default(crate::host(), ours, restore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::WireError;
    use std::cell::RefCell;

    /// Records the requests; answers null, or fails as told
    #[derive(Default)]
    struct Fake {
        sent: RefCell<Vec<Request>>,
        lost: bool,
    }

    impl Agent for Fake {
        fn call(&self, request: &Request) -> Result<Value, HostError> {
            self.sent.borrow_mut().push(request.clone());
            if self.lost { Err(HostError::AgentLost) } else { Ok(Value::Null) }
        }
    }

    fn lost() -> Fake {
        Fake { lost: true, ..Fake::default() }
    }

    fn switch(to: &str, restore: Option<&str>) -> Request {
        Request::SwitchRender { to: to.into(), restore: restore.map(Into::into) }
    }

    fn cable(restore: &[(Role, &str)]) -> CaptureHold {
        CaptureHold {
            ours: "{cable-out}".into(),
            restore: restore.iter().map(|(r, id)| (*r, id.to_string())).collect(),
        }
    }

    #[test]
    fn requests_have_a_stable_shape() {
        let json = serde_json::to_string(&switch("{a}", Some("{b}"))).unwrap();
        assert_eq!(json, r#"{"op":"switch_render","to":"{a}","restore":"{b}"}"#);
        let json = serde_json::to_string(&Request::DefaultCapture { role: Role::Communications }).unwrap();
        assert_eq!(json, r#"{"op":"default_capture","role":"communications"}"#);
        assert_eq!(serde_json::to_string(&Event::Ready).unwrap(), r#"{"kind":"ready"}"#);
        for r in EndpointRole::ALL {
            assert_eq!(EndpointRole::from(Role::from(r)), r);
        }
    }

    #[test]
    fn a_held_switch_is_applied_again_by_the_next_agent() {
        let mut m = Mirror::default();
        m.switch_render(&Fake::default(), "{bt}", Some("{speakers}")).unwrap();
        // The agent died; the next one switches again and holds the same
        let next = Fake::default();
        m.resync(&next);
        assert_eq!(*next.sent.borrow(), [switch("{bt}", Some("{speakers}"))]);
        // Restored: the agent after that gets nothing
        m.restore_render(&next, "{speakers}").unwrap();
        let after = Fake::default();
        m.resync(&after);
        assert!(after.sent.borrow().is_empty());
    }

    #[test]
    fn a_switch_that_did_not_get_through_is_held_for_the_next_agent() {
        let mut m = Mirror::default();
        assert!(matches!(m.switch_render(&lost(), "{bt}", Some("{speakers}")), Err(CaptureError::Agent(_))));
        let next = Fake::default();
        m.resync(&next);
        assert_eq!(*next.sent.borrow(), [switch("{bt}", Some("{speakers}"))]);
        // Nothing to restore: nothing held
        let mut m = Mirror::default();
        m.switch_render(&Fake::default(), "{bt}", None).unwrap();
        let next = Fake::default();
        m.resync(&next);
        assert!(next.sent.borrow().is_empty());
    }

    #[test]
    fn a_lost_restore_is_done_by_the_next_agent() {
        let mut m = Mirror::default();
        m.switch_render(&Fake::default(), "{bt}", Some("{speakers}")).unwrap();
        assert!(m.restore_render(&lost(), "{speakers}").is_err());
        // No agent for this one either: kept
        m.resync(&lost());
        let next = Fake::default();
        m.resync(&next);
        assert_eq!(*next.sent.borrow(), [Request::RestoreRender { id: "{speakers}".into() }]);
        assert_eq!(m, Mirror::default());
    }

    #[test]
    fn the_input_hold_follows_and_a_failed_release_is_finished() {
        let mut m = Mirror::default();
        let agent = Fake::default();
        let hold = cable(&[(Role::Console, "{pc-mic}"), (Role::Communications, "{usb}")]);
        m.hold_capture(&agent, Some(hold.clone()));
        assert_eq!(*agent.sent.borrow(), [Request::HoldCapture { hold: Some(hold.clone()), apply: false }]);
        // The release's restores did not get through, nor the release
        assert!(m.set_capture(&lost(), "{pc-mic}", Role::Console).is_err());
        m.hold_capture(&lost(), None);
        assert_eq!(m.lost_release, Some(hold.clone()));
        let next = Fake::default();
        m.resync(&next);
        assert_eq!(*next.sent.borrow(), [Request::ReleaseCapture { hold }]);
        assert_eq!(m, Mirror::default());
    }

    #[test]
    fn a_switched_input_is_applied_again_by_the_next_agent() {
        let mut m = Mirror::default();
        // The switch's requests did not get through
        assert!(m.set_capture(&lost(), "{cable-out}", Role::Console).is_err());
        let hold = cable(&[(Role::Console, "{pc-mic}")]);
        let agent = lost();
        m.hold_capture(&agent, Some(hold.clone()));
        assert_eq!(*agent.sent.borrow(), [Request::HoldCapture { hold: Some(hold.clone()), apply: true }]);
        let next = Fake::default();
        m.resync(&next);
        assert_eq!(*next.sent.borrow(), [Request::HoldCapture { hold: Some(hold), apply: true }]);
        // Released in full: the agent gets the empty hold
        let agent = Fake::default();
        m.hold_capture(&agent, None);
        assert_eq!(*agent.sent.borrow(), [Request::HoldCapture { hold: None, apply: false }]);
        assert_eq!(m, Mirror::default());
    }

    #[test]
    fn the_agents_errors_are_capture_errors() {
        struct Refuses;
        impl Agent for Refuses {
            fn call(&self, _: &Request) -> Result<Value, HostError> {
                let e = audio_capture::CaptureWireError::from(&CaptureError::DeviceNotFound("x".into()));
                Err(HostError::Remote(WireError::failed("switch", &e)))
            }
        }
        let mut m = Mirror::default();
        assert!(matches!(m.switch_render(&Refuses, "{bt}", Some("{a}")), Err(CaptureError::DeviceNotFound(_))));
        // It got to the agent: no restore left for the next one
        assert!(m.restore_render(&Refuses, "{a}").is_err());
        assert_eq!(m.lost_restore, None);
    }
}
