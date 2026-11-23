//! The headset microphone's cable as Windows' default input while its
//! device is connected (issue #86): apps that record from the default
//! microphone then record the headphones' microphone, and Auto opens it for
//! them. The counterpart of "Set as Windows default output while connected"
//! (stream.rs DefaultDeviceGuard), for the recording side ("CABLE Output")
//! of the output the microphone plays into ("CABLE Input").
//!
//! The core's state follows each change of the stream and the profiles with
//! a [`Want`]: switched while connected, kept while connecting again (a
//! profile started again with new settings, a lost link), released once
//! the device is disconnected. A release waits [`RELEASE_GRACE`], so a
//! profile started again does not flip the default input for a moment.
//!
//! Unlike the output switch (Console and Multimedia), all three roles are
//! switched: call apps (Teams, Discord, ...) record from the default
//! communications device, and the microphone must reach them for Auto to
//! open it. Each role's device before is kept and restored on its own, only
//! while ours is still that role's default: one the user chose meanwhile
//! stays.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub use audio_capture::EndpointRole as Role;

/// How long a release waits for the device to connect again
pub const RELEASE_GRACE: Duration = Duration::from_secs(3);

/// Windows' capture endpoints, behind a trait so the logic is tested with
/// fakes
pub trait InputEndpoints: Send + Sync {
    /// The default input device's ID for `role`
    fn default_input(&self, role: Role) -> Option<String>;
    /// The recording side of the render endpoint `render_id`
    fn capture_side(&self, render_id: &str) -> Option<String>;
    /// Makes `id` the default input for `role`; false when it failed
    fn set_default_input(&self, id: &str, role: Role) -> bool;
    /// What is switched after each change: `ours` for the roles in
    /// `restore`, each with its device before (None: nothing). The session
    /// agent restores it if the core goes away.
    fn held(&self, ours: Option<&str>, restore: &[(Role, String)]) {
        let _ = (ours, restore);
    }
}

/// Windows' own endpoints (through the session host)
pub struct WindowsInputs;

impl InputEndpoints for WindowsInputs {
    fn default_input(&self, role: Role) -> Option<String> {
        session_host::default_device::default_capture_device(role)
    }

    fn capture_side(&self, render_id: &str) -> Option<String> {
        session_host::default_device::paired_capture_device(render_id)
    }

    fn set_default_input(&self, id: &str, role: Role) -> bool {
        match session_host::default_device::set_default_capture_device(id, role) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("default input: could not switch ({role:?}): {e}");
                false
            }
        }
    }

    fn held(&self, ours: Option<&str>, restore: &[(Role, String)]) {
        session_host::default_device::hold_capture_default(ours, restore);
    }
}

/// What the default input should do now
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// The recording side of this render endpoint (the microphone's output)
    /// is the default input
    Switch(String),
    /// As it is (connecting, reconnecting)
    Keep,
    /// The device before is the default input again (after the grace)
    Release,
}

#[derive(Debug, Default)]
struct State {
    /// The microphone output switched to (also when it has no recording
    /// side, so that it is not looked up again)
    render: Option<String>,
    /// Its recording side made the default
    capture: Option<String>,
    /// Each role's default input before (a role already on the cable has
    /// nothing to restore)
    restore: Vec<(Role, String)>,
    /// Counts the changes: a release waiting for its grace is dropped when
    /// anything else came after it
    generation: u64,
}

/// Switches and restores the default input ([`Want`])
pub struct DefaultInput {
    state: Arc<Mutex<State>>,
    endpoints: Arc<dyn InputEndpoints>,
    grace: Duration,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DefaultInput {
    pub fn new(endpoints: Arc<dyn InputEndpoints>, grace: Duration) -> DefaultInput {
        DefaultInput { state: Arc::new(Mutex::new(State::default())), endpoints, grace }
    }

    /// Windows' endpoints, releasing after [`RELEASE_GRACE`]
    pub fn windows() -> DefaultInput {
        DefaultInput::new(Arc::new(WindowsInputs), RELEASE_GRACE)
    }

    /// Follows what the default input should do now
    pub fn follow(&self, want: Want) {
        let mut s = lock(&self.state);
        match want {
            Want::Keep => s.generation += 1,
            Want::Switch(render) => {
                s.generation += 1;
                if s.render.as_deref() != Some(render.as_str()) {
                    switch(&mut s, self.endpoints.as_ref(), render);
                }
            }
            Want::Release => {
                if s.render.is_none() {
                    return;
                }
                s.generation += 1;
                if self.grace.is_zero() {
                    release(&mut s, self.endpoints.as_ref());
                    return;
                }
                let generation = s.generation;
                drop(s);
                let state = self.state.clone();
                let endpoints = self.endpoints.clone();
                let grace = self.grace;
                let spawned = std::thread::Builder::new().name("default-input".into()).spawn(move || {
                    std::thread::sleep(grace);
                    let mut s = lock(&state);
                    if s.generation == generation {
                        release(&mut s, endpoints.as_ref());
                    }
                });
                if spawned.is_err() {
                    self.release_now();
                }
            }
        }
    }

    /// Restores the device before at once (the core exits)
    pub fn release_now(&self) {
        let mut s = lock(&self.state);
        s.generation += 1;
        release(&mut s, self.endpoints.as_ref());
    }
}

fn switch(s: &mut State, endpoints: &dyn InputEndpoints, render: String) {
    let Some(capture) = endpoints.capture_side(&render) else {
        tracing::info!("default input: the microphone output has no recording side, left as it is");
        // One switched to before stays switched until the release
        s.render = Some(render);
        return;
    };
    // Another cable chosen while switched: the devices from the start are
    // restored
    let first = s.capture.is_none();
    for role in Role::ALL {
        let before = endpoints.default_input(role);
        if before.as_deref() == Some(capture.as_str()) {
            continue;
        }
        if first && let Some(b) = before {
            s.restore.push((role, b));
        }
        tracing::info!("default input ({role:?}): the microphone's recording side while connected");
        endpoints.set_default_input(&capture, role);
    }
    endpoints.held(Some(&capture), &s.restore);
    s.render = Some(render);
    s.capture = Some(capture);
}

fn release(s: &mut State, endpoints: &dyn InputEndpoints) {
    s.render = None;
    let restore = std::mem::take(&mut s.restore);
    let Some(ours) = s.capture.take() else { return };
    for (role, before) in restore {
        if endpoints.default_input(role).as_deref() == Some(ours.as_str()) {
            tracing::info!("default input ({role:?}): restoring the device before");
            endpoints.set_default_input(&before, role);
        } else {
            tracing::info!("default input ({role:?}): changed meanwhile, left as it is");
        }
    }
    endpoints.held(None, &[]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        default: Mutex<HashMap<Role, String>>,
        sets: Mutex<Vec<(String, Role)>>,
        held: Mutex<Vec<(Option<String>, usize)>>,
    }

    impl InputEndpoints for Fake {
        fn default_input(&self, role: Role) -> Option<String> {
            self.default.lock().unwrap().get(&role).cloned()
        }
        fn capture_side(&self, render_id: &str) -> Option<String> {
            match render_id {
                "{cable-in}" => Some("{cable-out}".into()),
                "{hifi-in}" => Some("{hifi-out}".into()),
                _ => None,
            }
        }
        fn set_default_input(&self, id: &str, role: Role) -> bool {
            self.default.lock().unwrap().insert(role, id.into());
            self.sets.lock().unwrap().push((id.into(), role));
            true
        }
        fn held(&self, ours: Option<&str>, restore: &[(Role, String)]) {
            self.held.lock().unwrap().push((ours.map(Into::into), restore.len()));
        }
    }

    /// The PC's microphone for Console and Multimedia, a USB headset for calls
    fn fake() -> Arc<Fake> {
        let f = Fake::default();
        {
            let mut d = f.default.lock().unwrap();
            d.insert(Role::Console, "{pc-mic}".into());
            d.insert(Role::Multimedia, "{pc-mic}".into());
            d.insert(Role::Communications, "{usb-headset}".into());
        }
        Arc::new(f)
    }

    fn now(f: &Arc<Fake>) -> [String; 3] {
        Role::ALL.map(|r| f.default_input(r).unwrap_or_default())
    }

    fn all(id: &str) -> [String; 3] {
        [id.into(), id.into(), id.into()]
    }

    fn before() -> [String; 3] {
        ["{pc-mic}".into(), "{pc-mic}".into(), "{usb-headset}".into()]
    }

    #[test]
    fn every_role_is_switched_while_connected_and_restored_after() {
        let f = fake();
        let d = DefaultInput::new(f.clone(), Duration::ZERO);
        d.follow(Want::Switch("{cable-in}".into()));
        // Calls too: Communications is the call apps' default
        assert_eq!(now(&f), all("{cable-out}"));
        // The same again (every stream event): no second switch
        d.follow(Want::Switch("{cable-in}".into()));
        d.follow(Want::Keep);
        assert_eq!(f.sets.lock().unwrap().len(), 3);
        // Each role gets its own device back
        d.follow(Want::Release);
        assert_eq!(now(&f), before());
        // Nothing switched: nothing to release
        d.follow(Want::Release);
        assert_eq!(f.sets.lock().unwrap().len(), 6);
    }

    #[test]
    fn another_cable_restores_the_first_devices() {
        let f = fake();
        let d = DefaultInput::new(f.clone(), Duration::ZERO);
        d.follow(Want::Switch("{cable-in}".into()));
        d.follow(Want::Switch("{hifi-in}".into()));
        assert_eq!(now(&f), all("{hifi-out}"));
        d.follow(Want::Release);
        assert_eq!(now(&f), before());
    }

    #[test]
    fn a_role_changed_meanwhile_stays() {
        let f = fake();
        let d = DefaultInput::new(f.clone(), Duration::ZERO);
        d.follow(Want::Switch("{cable-in}".into()));
        f.default.lock().unwrap().insert(Role::Communications, "{webcam}".into());
        d.follow(Want::Release);
        assert_eq!(now(&f), ["{pc-mic}".to_string(), "{pc-mic}".into(), "{webcam}".into()]);
    }

    #[test]
    fn a_role_already_on_the_cable_is_left_so() {
        let f = fake();
        f.default.lock().unwrap().insert(Role::Communications, "{cable-out}".into());
        let d = DefaultInput::new(f.clone(), Duration::ZERO);
        d.follow(Want::Switch("{cable-in}".into()));
        assert_eq!(f.sets.lock().unwrap().len(), 2);
        d.follow(Want::Release);
        assert_eq!(now(&f), ["{pc-mic}".to_string(), "{pc-mic}".into(), "{cable-out}".into()]);
    }

    #[test]
    fn an_output_without_a_recording_side_changes_nothing() {
        let f = fake();
        let d = DefaultInput::new(f.clone(), Duration::ZERO);
        d.follow(Want::Switch("{speakers}".into()));
        d.follow(Want::Release);
        assert_eq!(now(&f), before());
        assert!(f.sets.lock().unwrap().is_empty());
    }

    #[test]
    fn a_release_waits_and_a_reconnect_cancels_it() {
        let f = fake();
        let d = DefaultInput::new(f.clone(), Duration::from_millis(100));
        d.follow(Want::Switch("{cable-in}".into()));
        // Started again with new settings: Idle, then connecting and streaming
        d.follow(Want::Release);
        d.follow(Want::Keep);
        d.follow(Want::Switch("{cable-in}".into()));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(now(&f), all("{cable-out}"));
        assert_eq!(f.sets.lock().unwrap().len(), 3);
        // Disconnected for good: restored after the grace
        d.follow(Want::Release);
        assert_eq!(now(&f), all("{cable-out}"));
        // The grace runs on its own thread, which a busy machine starts late
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while now(&f) != before() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(now(&f), before());
    }

    #[test]
    fn the_switch_is_held_until_released() {
        let f = fake();
        let d = DefaultInput::new(f.clone(), Duration::ZERO);
        d.follow(Want::Switch("{cable-in}".into()));
        d.follow(Want::Switch("{hifi-in}".into()));
        d.follow(Want::Release);
        // Nothing switched: nothing held or released
        d.follow(Want::Switch("{speakers}".into()));
        d.follow(Want::Release);
        let held = f.held.lock().unwrap().clone();
        assert_eq!(held, [(Some("{cable-out}".into()), 3), (Some("{hifi-out}".into()), 3), (None, 0)]);
    }

    #[test]
    fn released_at_once_when_the_core_exits() {
        let f = fake();
        let d = DefaultInput::new(f.clone(), Duration::from_secs(60));
        d.follow(Want::Switch("{cable-in}".into()));
        d.follow(Want::Release);
        d.release_now();
        assert_eq!(now(&f), before());
    }
}
