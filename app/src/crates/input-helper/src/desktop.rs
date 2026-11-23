//! Following the input desktop: the helper's injecting thread must sit on
//! the desktop that receives input, or Windows refuses its `SendInput`.
//! That desktop changes when the lock or sign-in screen, a UAC prompt or
//! Ctrl+Alt+Del's screen comes and goes. The decisions are here, against
//! [`DesktopApi`]; win_sys::input_desktop makes the calls ([`Win32`]).

use std::io;

/// The calls the follower needs
pub trait DesktopApi {
    type Desktop;
    /// Opens the desktop that receives input now
    fn open_input(&mut self) -> io::Result<Self::Desktop>;
    fn name(&self, d: &Self::Desktop) -> io::Result<String>;
    /// Attaches the calling (injecting) thread to `d`
    fn attach(&mut self, d: &Self::Desktop) -> io::Result<()>;
}

/// What a check found
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Still on the input desktop
    Same,
    /// Attached for the first time (or again after it was unavailable)
    Attached,
    /// The input desktop changed and the thread is attached to the new one:
    /// whatever is held must be released now, on the new desktop, or it
    /// stays down
    Switched,
    /// The input desktop cannot be opened or attached: Windows refuses
    /// input until it can
    Unavailable,
}

/// The injecting thread's desktop
pub struct DesktopFollow<A: DesktopApi> {
    api: A,
    current: Option<(String, A::Desktop)>,
    /// A desktop was attached before (a later attach is a switch)
    had: bool,
}

impl<A: DesktopApi> DesktopFollow<A> {
    pub fn new(api: A) -> Self {
        DesktopFollow { api, current: None, had: false }
    }

    /// The name of the desktop followed now (None: none)
    pub fn current(&self) -> Option<&str> {
        self.current.as_ref().map(|(n, _)| n.as_str())
    }

    /// Before injecting, and on every tick: is the thread still on the
    /// input desktop? Attaches it to the new one when not.
    pub fn check(&mut self) -> Check {
        let opened = self.api.open_input().and_then(|d| Ok((self.api.name(&d)?, d)));
        let Ok((name, desk)) = opened else {
            self.current = None;
            return Check::Unavailable;
        };
        if self.current.as_ref().is_some_and(|(n, _)| *n == name) {
            return Check::Same;
        }
        if self.api.attach(&desk).is_err() {
            self.current = None;
            return Check::Unavailable;
        }
        // The old handle closes after the thread left it
        let first = !self.had;
        let was_attached = self.current.replace((name, desk)).is_some();
        self.had = true;
        if first || !was_attached { Check::Attached } else { Check::Switched }
    }

    /// `SendInput` failed with access denied: the desktop may have changed
    /// between two checks. Attaches again (a switch if the name changed).
    pub fn on_access_denied(&mut self) -> Check {
        self.check()
    }
}

/// The real calls (win_sys::input_desktop)
pub struct Win32;

impl DesktopApi for Win32 {
    type Desktop = win_sys::input_desktop::InputDesktop;

    fn open_input(&mut self) -> io::Result<Self::Desktop> {
        win_sys::input_desktop::InputDesktop::open()
    }

    fn name(&self, d: &Self::Desktop) -> io::Result<String> {
        d.name()
    }

    fn attach(&mut self, d: &Self::Desktop) -> io::Result<()> {
        d.attach_current_thread()
    }
}

/// A desktop that never changes (the helper's record mode, which passes
/// nothing to Windows)
pub struct Fixed;

impl DesktopApi for Fixed {
    type Desktop = ();

    fn open_input(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn name(&self, _: &()) -> io::Result<String> {
        Ok("Default".into())
    }

    fn attach(&mut self, _: &()) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The input desktop by name (None: cannot be opened), and whether
    /// attaching fails; what was attached is recorded
    #[derive(Clone, Default)]
    pub struct FakeDesktops {
        pub input: Rc<RefCell<Option<String>>>,
        pub attach_fails: Rc<RefCell<bool>>,
        pub attached: Rc<RefCell<Vec<String>>>,
    }

    impl FakeDesktops {
        pub fn on(name: &str) -> Self {
            let f = FakeDesktops::default();
            f.set(Some(name));
            f
        }

        pub fn set(&self, name: Option<&str>) {
            *self.input.borrow_mut() = name.map(str::to_string);
        }
    }

    impl DesktopApi for FakeDesktops {
        type Desktop = String;

        fn open_input(&mut self) -> io::Result<String> {
            self.input.borrow().clone().ok_or_else(|| io::Error::from(io::ErrorKind::PermissionDenied))
        }

        fn name(&self, d: &String) -> io::Result<String> {
            Ok(d.clone())
        }

        fn attach(&mut self, d: &String) -> io::Result<()> {
            if *self.attach_fails.borrow() {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            self.attached.borrow_mut().push(d.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeDesktops;
    use super::*;

    #[test]
    fn follows_the_input_desktop_through_lock_and_uac() {
        let api = FakeDesktops::on("Default");
        let mut f = DesktopFollow::new(api.clone());
        assert_eq!(f.check(), Check::Attached);
        assert_eq!(f.check(), Check::Same);
        // The lock screen or a UAC prompt
        api.set(Some("Winlogon"));
        assert_eq!(f.check(), Check::Switched);
        assert_eq!(f.current(), Some("Winlogon"));
        assert_eq!(f.check(), Check::Same);
        // Back on the user's desktop, found through a refused SendInput
        api.set(Some("Default"));
        assert_eq!(f.on_access_denied(), Check::Switched);
        assert_eq!(*api.attached.borrow(), ["Default", "Winlogon", "Default"]);
    }

    #[test]
    fn an_unavailable_desktop_is_reported_and_attached_again_later() {
        let api = FakeDesktops::on("Default");
        let mut f = DesktopFollow::new(api.clone());
        assert_eq!(f.check(), Check::Attached);
        // An ordinary process cannot open the secure desktop
        api.set(None);
        assert_eq!(f.check(), Check::Unavailable);
        assert_eq!(f.current(), None);
        // Attaching fails (a thread with windows): unavailable too
        api.set(Some("Winlogon"));
        *api.attach_fails.borrow_mut() = true;
        assert_eq!(f.check(), Check::Unavailable);
        *api.attach_fails.borrow_mut() = false;
        // Nothing is held across an unavailable desktop: the next one is
        // attached, not switched to
        assert_eq!(f.check(), Check::Attached);
        let mut fixed = DesktopFollow::new(Fixed);
        assert_eq!(fixed.check(), Check::Attached);
        assert_eq!(fixed.check(), Check::Same);
    }
}
