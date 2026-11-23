//! The input helper: keyboards and mice of BlueCaster's adapter typed and
//! pointed on every Windows desktop, including the sign-in screen, the
//! lock screen and UAC prompts.
//!
//! A process in a service's session 0 cannot pass input to the user's
//! desktop, and `GetKeyState` has no desktop there. So the core starts a
//! helper (`bluecaster-core.exe --input-helper <pipe>`) in the console
//! session; in the service it runs as SYSTEM, which may attach to the
//! secure desktop. As an ordinary process (tests, a headless core run by
//! hand) it is a plain child of the core, running as the same user.
//!
//! - [`protocol`]: the pipe's frames, every field checked.
//! - [`queue`]: the core's queue (never blocks the BTstack thread) and its
//!   record of what may be held down.
//! - [`client`]: the core's sink ([`client::HelperSink`]): starts the
//!   helper, checks it, writes the queue, restarts it.
//! - [`helper`]: the helper process.
//! - [`engine`], [`desktop`]: the helper's injecting thread: held keys,
//!   desktop switches, lock keys, Ctrl+Alt+Del.
//!
//! Logs: counts and states only, never keys, text or names.

#![forbid(unsafe_code)]

pub mod client;
pub mod desktop;
pub mod engine;
pub mod helper;
pub mod protocol;
pub mod queue;

pub use client::{HelperConfig, HelperSink, Launcher, SessionLauncher};
pub use helper::{HELPER_ARG, RECORD_ARG};
pub use hid_input::sink::{HidCounters, InputSink};
pub use hid_input::sink::{SelfTest, SelfTestFailure};
