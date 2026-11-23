//! Group `default_device` in the agent: switches Windows' default devices
//! for the core and holds what it switched. Dropped with its connection
//! (the core exited or died): restores the default output, and each role
//! of the default input that is still ours, as the core's guards do.

use audio_capture::{CaptureError, EndpointRole};
use serde_json::Value;

use crate::host::default_device::{CaptureHold, DefaultDeviceHost, Event, GROUP, RenderHold, Request, Role};
use crate::wire::{WireError, decode, reply, to_body};
use crate::{AgentContext, Handler, LocalHost};

pub(crate) fn new(ctx: AgentContext) -> Box<dyn Handler> {
    // The core gives a new agent what it holds
    ctx.event(GROUP, to_body(&Event::Ready));
    Box::new(DefaultDevices::new(Windows))
}

/// Windows' default devices, behind a trait so the holding is tested with
/// fakes
pub(crate) trait Backend: Send + 'static {
    fn set_render(&self, id: &str) -> Result<(), CaptureError>;
    fn default_capture(&self, role: EndpointRole) -> Option<String>;
    fn paired_capture(&self, render_id: &str) -> Option<String>;
    fn set_capture(&self, id: &str, role: EndpointRole) -> Result<(), CaptureError>;
}

struct Windows;

impl Backend for Windows {
    fn set_render(&self, id: &str) -> Result<(), CaptureError> {
        LocalHost.restore_render_default(id)
    }

    fn default_capture(&self, role: EndpointRole) -> Option<String> {
        LocalHost.default_capture_device(role)
    }

    fn paired_capture(&self, render_id: &str) -> Option<String> {
        LocalHost.paired_capture_device(render_id)
    }

    fn set_capture(&self, id: &str, role: EndpointRole) -> Result<(), CaptureError> {
        LocalHost.set_default_capture_device(id, role)
    }
}

pub(crate) struct DefaultDevices<B: Backend> {
    backend: B,
    render: Option<RenderHold>,
    capture: Option<CaptureHold>,
}

fn done(what: &str, r: Result<(), CaptureError>) -> Result<Value, WireError> {
    match r {
        Ok(()) => reply(()),
        Err(e) => {
            tracing::warn!("default devices: {what} failed (HRESULT {:08x?})", e.hresult());
            Err(WireError::failed(what, &audio_capture::CaptureWireError::from(&e)))
        }
    }
}

impl<B: Backend> DefaultDevices<B> {
    pub(crate) fn new(backend: B) -> DefaultDevices<B> {
        DefaultDevices { backend, render: None, capture: None }
    }

    /// Restores `hold`'s roles that are still ours; the count restored
    fn release(&self, hold: &CaptureHold) -> usize {
        let mut restored = 0;
        for (role, before) in &hold.restore {
            let role = EndpointRole::from(*role);
            if self.backend.default_capture(role).as_deref() == Some(hold.ours.as_str())
                && self.backend.set_capture(before, role).is_ok()
            {
                restored += 1;
            }
        }
        restored
    }
}

impl<B: Backend> Handler for DefaultDevices<B> {
    fn request(&mut self, body: Value) -> Result<Value, WireError> {
        match decode(body)? {
            Request::SwitchRender { to, restore } => {
                self.render = restore.map(|restore| RenderHold { to: to.clone(), restore });
                done("switch output", self.backend.set_render(&to))
            }
            Request::RestoreRender { id } => {
                self.render = None;
                done("restore output", self.backend.set_render(&id))
            }
            Request::DefaultCapture { role } => reply(self.backend.default_capture(role.into())),
            Request::PairedCapture { render } => reply(self.backend.paired_capture(&render)),
            Request::SetCapture { id, role } => done("switch input", self.backend.set_capture(&id, role.into())),
            Request::HoldCapture { hold, apply } => {
                if apply && let Some(h) = &hold {
                    let roles = h.restore.iter().map(|(r, _)| *r).collect::<Vec<Role>>();
                    let failed =
                        roles.iter().filter(|r| self.backend.set_capture(&h.ours, (**r).into()).is_err()).count();
                    tracing::info!("default input: switched again for {} role(s), {failed} failed", roles.len());
                }
                self.capture = hold;
                reply(())
            }
            Request::ReleaseCapture { hold } => {
                self.capture = None;
                let restored = self.release(&hold);
                tracing::info!("default input: {restored} of {} role(s) restored", hold.restore.len());
                reply(())
            }
        }
    }
}

impl<B: Backend> Drop for DefaultDevices<B> {
    fn drop(&mut self) {
        if let Some(h) = self.render.take() {
            let ok = self.backend.set_render(&h.restore).is_ok();
            tracing::info!("default output: the core went away, the device before restored: {ok}");
        }
        if let Some(h) = self.capture.take() {
            let restored = self.release(&h);
            tracing::info!("default input: the core went away, {restored} of {} role(s) restored", h.restore.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Default devices in memory: the output, and the input per role
    #[derive(Default)]
    struct Defaults {
        render: Option<String>,
        capture: HashMap<EndpointRole, String>,
        sets: usize,
    }

    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Defaults>>);

    impl Backend for Fake {
        fn set_render(&self, id: &str) -> Result<(), CaptureError> {
            if id == "{gone}" {
                return Err(CaptureError::DeviceNotFound(id.into()));
            }
            let mut d = self.0.lock().unwrap();
            d.render = Some(id.into());
            d.sets += 1;
            Ok(())
        }
        fn default_capture(&self, role: EndpointRole) -> Option<String> {
            self.0.lock().unwrap().capture.get(&role).cloned()
        }
        fn paired_capture(&self, render_id: &str) -> Option<String> {
            (render_id == "{cable-in}").then(|| "{cable-out}".into())
        }
        fn set_capture(&self, id: &str, role: EndpointRole) -> Result<(), CaptureError> {
            let mut d = self.0.lock().unwrap();
            d.capture.insert(role, id.into());
            d.sets += 1;
            Ok(())
        }
    }

    fn fake() -> Fake {
        let f = Fake::default();
        {
            let mut d = f.0.lock().unwrap();
            d.render = Some("{speakers}".into());
            d.capture.insert(EndpointRole::Console, "{pc-mic}".into());
            d.capture.insert(EndpointRole::Multimedia, "{pc-mic}".into());
            d.capture.insert(EndpointRole::Communications, "{usb}".into());
        }
        f
    }

    fn render(f: &Fake) -> Option<String> {
        f.0.lock().unwrap().render.clone()
    }

    fn inputs(f: &Fake) -> [Option<String>; 3] {
        EndpointRole::ALL.map(|r| f.default_capture(r))
    }

    fn held_input() -> Value {
        json!({"op": "hold_capture", "apply": false, "hold": {"ours": "{cable-out}", "restore": [
            ["console", "{pc-mic}"], ["multimedia", "{pc-mic}"], ["communications", "{usb}"]]}})
    }

    fn switch_input(h: &mut DefaultDevices<Fake>) {
        for role in ["console", "multimedia", "communications"] {
            h.request(json!({"op": "set_capture", "id": "{cable-out}", "role": role})).unwrap();
        }
        h.request(held_input()).unwrap();
    }

    #[test]
    fn the_core_going_away_restores_what_it_switched() {
        let f = fake();
        let mut h = DefaultDevices::new(f.clone());
        h.request(json!({"op": "switch_render", "to": "{bt}", "restore": "{speakers}"})).unwrap();
        assert_eq!(render(&f).as_deref(), Some("{bt}"));
        switch_input(&mut h);
        // The user picked another microphone for calls meanwhile: it stays
        f.0.lock().unwrap().capture.insert(EndpointRole::Communications, "{webcam}".into());
        drop(h);
        assert_eq!(render(&f).as_deref(), Some("{speakers}"));
        assert_eq!(inputs(&f), [Some("{pc-mic}".into()), Some("{pc-mic}".into()), Some("{webcam}".into())]);
    }

    #[test]
    fn nothing_held_nothing_restored() {
        let f = fake();
        let mut h = DefaultDevices::new(f.clone());
        // Switched and restored by the core itself
        h.request(json!({"op": "switch_render", "to": "{bt}", "restore": "{speakers}"})).unwrap();
        h.request(json!({"op": "restore_render", "id": "{speakers}"})).unwrap();
        switch_input(&mut h);
        h.request(json!({"op": "hold_capture", "apply": false, "hold": null})).unwrap();
        // Switched with nothing to restore (it was the default already)
        h.request(json!({"op": "switch_render", "to": "{bt}", "restore": null})).unwrap();
        let sets = f.0.lock().unwrap().sets;
        drop(h);
        assert_eq!(f.0.lock().unwrap().sets, sets);
        assert_eq!(render(&f).as_deref(), Some("{bt}"));
    }

    #[test]
    fn a_new_agent_applies_the_held_switch_again() {
        let f = fake();
        let mut h = DefaultDevices::new(f.clone());
        let mut held = held_input();
        held["apply"] = json!(true);
        h.request(held).unwrap();
        assert_eq!(inputs(&f), [Some("{cable-out}".into()), Some("{cable-out}".into()), Some("{cable-out}".into())]);
        // And a release the last agent did not get
        let release = json!({"op": "release_capture", "hold": held_input()["hold"]});
        h.request(release).unwrap();
        assert_eq!(inputs(&f), [Some("{pc-mic}".into()), Some("{pc-mic}".into()), Some("{usb}".into())]);
        let sets = f.0.lock().unwrap().sets;
        drop(h);
        assert_eq!(f.0.lock().unwrap().sets, sets);
    }

    #[test]
    fn reads_and_failures() {
        let f = fake();
        let mut h = DefaultDevices::new(f.clone());
        let got = h.request(json!({"op": "default_capture", "role": "communications"})).unwrap();
        assert_eq!(got, json!("{usb}"));
        assert_eq!(h.request(json!({"op": "paired_capture", "render": "{cable-in}"})).unwrap(), json!("{cable-out}"));
        assert_eq!(h.request(json!({"op": "paired_capture", "render": "{speakers}"})).unwrap(), Value::Null);
        let e = h.request(json!({"op": "switch_render", "to": "{gone}", "restore": "{speakers}"})).unwrap_err();
        assert_eq!(e.code, ErrorCode::Failed);
        assert!(matches!(
            e.detail_as::<audio_capture::CaptureWireError>(),
            Some(audio_capture::CaptureWireError::DeviceNotFound(_))
        ));
        assert_eq!(h.request(json!({"op": "nonsense"})).unwrap_err().code, ErrorCode::BadRequest);
        // The failed switch is still restored (one role may have switched)
        drop(h);
        assert_eq!(render(&f).as_deref(), Some("{speakers}"));
    }

    /// The real handler reads this PC's default input like the local host
    /// (read-only)
    #[test]
    fn answers_reads_as_the_local_host() {
        let mut h = new(AgentContext::detached());
        let got = h.request(json!({"op": "default_capture", "role": "console"})).unwrap();
        assert_eq!(
            serde_json::from_value::<Option<String>>(got).unwrap(),
            LocalHost.default_capture_device(EndpointRole::Console)
        );
    }
}
