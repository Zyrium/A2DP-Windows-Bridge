//! Which Windows session gets the agent.
//!
//! A core running as a service (session 0) starts the agent in the active
//! console session once a user has signed in there, and only there: that
//! agent feeds the audio and shows the tray. Before anyone signs in (the
//! sign-in screen) there is no agent, and the core runs without one, so
//! keyboards and mice connect anyway. With fast user switching, the
//! console moves to another session: the old session's agent goes and the
//! new one's comes (the hub stops a running stream first, see
//! [`crate::Hub::on_session_change`]).
//!
//! [`SessionTracker`] is fed by the service's session-change controls
//! ([`SessionEvent::from_wts`]) and by polled facts ([`SessionEvent::Facts`],
//! the truth when they disagree); an ordinary process ([`SessionSource::Own`])
//! always uses its own session, where its agent is spawned.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// What changed in which session (the WTS_* codes of a session change)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    ConsoleConnect(u32),
    ConsoleDisconnect(u32),
    RemoteConnect(u32),
    RemoteDisconnect(u32),
    Logon(u32),
    Logoff(u32),
    Lock(u32),
    Unlock(u32),
    Terminate(u32),
    /// Read from Windows: the console's session, and whether a user is
    /// signed in to it
    Facts {
        console: Option<u32>,
        user_at_console: bool,
    },
}

impl SessionEvent {
    /// A SERVICE_CONTROL_SESSIONCHANGE's event code and session (None: a
    /// code that does not matter here, e.g. WTS_SESSION_CREATE)
    pub fn from_wts(event: u32, session: u32) -> Option<SessionEvent> {
        Some(match event {
            1 => SessionEvent::ConsoleConnect(session),
            2 => SessionEvent::ConsoleDisconnect(session),
            3 => SessionEvent::RemoteConnect(session),
            4 => SessionEvent::RemoteDisconnect(session),
            5 => SessionEvent::Logon(session),
            6 => SessionEvent::Logoff(session),
            7 => SessionEvent::Lock(session),
            8 => SessionEvent::Unlock(session),
            11 => SessionEvent::Terminate(session),
            _ => return None,
        })
    }
}

/// The console session and the sessions with a user, as far as known
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionTracker {
    console: Option<u32>,
    signed_in: BTreeSet<u32>,
}

impl SessionTracker {
    pub fn new() -> SessionTracker {
        SessionTracker::default()
    }

    /// Takes `e` in; true when [`SessionTracker::wanted`] changed
    pub fn apply(&mut self, e: SessionEvent) -> bool {
        let before = self.wanted();
        match e {
            SessionEvent::ConsoleConnect(s) => self.console = Some(s),
            SessionEvent::ConsoleDisconnect(s) => {
                if self.console == Some(s) {
                    self.console = None;
                }
            }
            SessionEvent::Logon(s) | SessionEvent::Unlock(s) => {
                self.signed_in.insert(s);
            }
            SessionEvent::Logoff(s) | SessionEvent::Terminate(s) => {
                self.signed_in.remove(&s);
                if e == SessionEvent::Terminate(s) && self.console == Some(s) {
                    self.console = None;
                }
            }
            // A remote session never gets the agent; locking keeps the user
            SessionEvent::RemoteConnect(_) | SessionEvent::RemoteDisconnect(_) | SessionEvent::Lock(_) => {}
            SessionEvent::Facts { console, user_at_console } => {
                self.console = console;
                if let Some(c) = console {
                    if user_at_console {
                        self.signed_in.insert(c);
                    } else {
                        self.signed_in.remove(&c);
                    }
                }
            }
        }
        self.wanted() != before
    }

    /// The session the agent should run in: the console's, once a user
    /// signed in there (never session 0)
    pub fn wanted(&self) -> Option<u32> {
        self.console.filter(|c| *c != 0 && self.signed_in.contains(c))
    }
}

/// Where the hub takes the agent's session from
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionSource {
    /// An ordinary process: its own session, always (the agent is spawned
    /// there)
    Own(u32),
    /// A service: the active console session with a user
    /// ([`SessionTracker`], polled and fed with session changes)
    Console,
    /// Tests, debug builds only: a file holding the session's number, or
    /// `none` (nobody signed in). A missing file counts as `none`.
    TestFile(PathBuf),
}

/// The facts of [`SessionSource::TestFile`]'s file
pub fn test_file_facts(text: &str) -> SessionEvent {
    match text.trim().parse::<u32>() {
        Ok(s) => SessionEvent::Facts { console: Some(s), user_at_console: true },
        Err(_) => SessionEvent::Facts { console: None, user_at_console: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SessionEvent::*;

    #[test]
    fn the_agent_waits_for_a_sign_in_at_the_console() {
        let mut t = SessionTracker::new();
        assert_eq!(t.wanted(), None);
        // Boot: the sign-in screen in session 1, nobody signed in
        assert!(!t.apply(Facts { console: Some(1), user_at_console: false }));
        assert_eq!(t.wanted(), None);
        assert!(t.apply(Logon(1)));
        assert_eq!(t.wanted(), Some(1));
        // Locking keeps the agent, unlocking changes nothing
        assert!(!t.apply(Lock(1)));
        assert!(!t.apply(Unlock(1)));
        assert_eq!(t.wanted(), Some(1));
        // A remote session never gets it
        assert!(!t.apply(RemoteConnect(3)));
        assert!(!t.apply(Logon(3)));
        assert_eq!(t.wanted(), Some(1));
        assert!(t.apply(Logoff(1)));
        assert_eq!(t.wanted(), None);
    }

    /// Fast user switching: the console moves to a new session with the
    /// sign-in screen, the second user signs in, then the first comes back
    #[test]
    fn fast_user_switching_moves_the_agent() {
        let mut t = SessionTracker::new();
        t.apply(ConsoleConnect(1));
        t.apply(Logon(1));
        assert_eq!(t.wanted(), Some(1));
        // "Switch user": session 1 leaves the console, session 2 comes
        assert!(t.apply(ConsoleDisconnect(1)));
        assert_eq!(t.wanted(), None, "nobody at the console: no agent");
        assert!(!t.apply(ConsoleConnect(2)));
        assert!(t.apply(Logon(2)));
        assert_eq!(t.wanted(), Some(2));
        // Back to the first user, still signed in
        t.apply(ConsoleDisconnect(2));
        assert!(t.apply(ConsoleConnect(1)) || t.wanted() == Some(1));
        assert_eq!(t.wanted(), Some(1));
        // The second user signs out in the background: no change
        assert!(!t.apply(Logoff(2)));
        assert!(!t.apply(Terminate(2)));
        assert_eq!(t.wanted(), Some(1));
        // A disconnect of another session does not touch the console
        assert!(!t.apply(ConsoleDisconnect(5)));
        assert_eq!(t.wanted(), Some(1));
    }

    #[test]
    fn polled_facts_are_the_truth() {
        let mut t = SessionTracker::new();
        t.apply(ConsoleConnect(1));
        t.apply(Logon(1));
        // A missed logoff: the poll sees the sign-in screen
        assert!(t.apply(Facts { console: Some(1), user_at_console: false }));
        assert_eq!(t.wanted(), None);
        // A missed switch
        assert!(t.apply(Facts { console: Some(4), user_at_console: true }));
        assert_eq!(t.wanted(), Some(4));
        // No console at all (headless server)
        assert!(t.apply(Facts { console: None, user_at_console: false }));
        assert_eq!(t.wanted(), None);
        // Session 0 is never wanted
        t.apply(Facts { console: Some(0), user_at_console: true });
        assert_eq!(t.wanted(), None);
    }

    #[test]
    fn wts_codes() {
        assert_eq!(SessionEvent::from_wts(1, 2), Some(ConsoleConnect(2)));
        assert_eq!(SessionEvent::from_wts(5, 2), Some(Logon(2)));
        assert_eq!(SessionEvent::from_wts(8, 2), Some(Unlock(2)));
        assert_eq!(SessionEvent::from_wts(11, 2), Some(Terminate(2)));
        assert_eq!(SessionEvent::from_wts(10, 2), None, "create");
        assert_eq!(SessionEvent::from_wts(9, 2), None, "remote control");
    }

    #[test]
    fn test_files() {
        assert_eq!(test_file_facts("3\n"), Facts { console: Some(3), user_at_console: true });
        assert_eq!(test_file_facts("none"), Facts { console: None, user_at_console: false });
        assert_eq!(test_file_facts(""), Facts { console: None, user_at_console: false });
    }
}
