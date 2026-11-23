//! Pairing a keyboard that asks for a passkey: BlueCaster shows a six-digit
//! number, the user types it on the keyboard and presses Enter.
//!
//! Only for the device the user is adding as a keyboard or mouse
//! ([`crate::BtHost::hid_pairing_begin`]), and only until that pairing ends
//! (done, failed, or the window runs out). Every other device keeps
//! BlueCaster's usual "no input, no output" pairing: classic answers the IO
//! capability per device (shim.c's handler_hci), LE switches SM's IO
//! capability right before it asks a link for security.
//!
//! MITM is never asked for here: a device that does not want it pairs with
//! Just Works as before, and one that wants it gets Passkey Entry, BlueCaster
//! displaying.
//!
//! The number is never logged: it goes to the UI only ([`HidEvent::Passkey`]).

use std::sync::atomic::{AtomicU32, Ordering};

use btstack_sys::Stack;

use super::{HidEngine, HidEvent};
use crate::HostEvent;

static GENERATION: AtomicU32 = AtomicU32::new(1);

/// Tells one pairing window from the next (a timeout of an old one must
/// not end a new one)
pub(crate) fn next_pairing_generation() -> u32 {
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// A passkey to show: six digits. Its Debug form hides it, so that a log
/// of a whole event never holds it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Passkey(u32);

impl Passkey {
    pub fn new(value: u32) -> Passkey {
        Passkey(value % 1_000_000)
    }

    /// A fresh one (legacy pairing's PIN, which BlueCaster chooses)
    pub fn random() -> Passkey {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        Passkey::new((h.finish() % 1_000_000) as u32)
    }

    /// The six digits, leading zeros included
    pub fn digits(&self) -> String {
        format!("{:06}", self.0)
    }
}

impl std::fmt::Debug for Passkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Passkey(hidden)")
    }
}

struct Target {
    addr: [u8; 6],
    le: bool,
    generation: u32,
    /// A number was shown (the UI is told when it goes away)
    shown: bool,
}

#[derive(Default)]
pub(crate) struct Pairing {
    target: Option<Target>,
}

impl HidEngine {
    /// The user adds `addr`: its pairing may show a number. One device at a
    /// time; one before it ends here.
    pub fn pairing_begin(&mut self, stack: &Stack, addr: [u8; 6], le: bool, generation: u32) {
        self.pairing_end(stack, None, None);
        self.pairing.target = Some(Target { addr, le, generation, shown: false });
        if !le {
            stack.hid_pairing_target(Some(&addr));
        }
        tracing::info!("hid: pairing the new {} device may show a number", if le { "LE" } else { "classic" });
    }

    /// The device being added is one of `addrs` (as listed, or as connected)
    pub fn pairing_with(&self, addrs: &[[u8; 6]]) -> bool {
        self.pairing.target.as_ref().is_some_and(|t| addrs.contains(&t.addr))
    }

    /// LE, right before a link is asked for security: a display for the
    /// device being added, no input and no output for every other one
    pub fn pairing_io_for(&self, stack: &Stack, addrs: &[[u8; 6]]) {
        let display = self.pairing.target.as_ref().is_some_and(|t| t.le && addrs.contains(&t.addr));
        stack.sm_passkey_display(display);
    }

    /// The stack has a number for the device being added: to the UI. False
    /// when it is not that device (nothing shown).
    pub fn passkey_shown(&mut self, addrs: &[[u8; 6]], passkey: Passkey) -> bool {
        let Some(t) = self.pairing.target.as_mut().filter(|t| addrs.contains(&t.addr)) else { return false };
        t.shown = true;
        let addr = t.addr;
        tracing::info!("hid: showing the number to type on the new device");
        let _ = self.events.send(HostEvent::Hid(HidEvent::Passkey(addr, passkey)));
        true
    }

    /// The pairing with `addr` ended (None: whichever runs); with
    /// `generation`, only that window's. The usual IO capability is back.
    pub fn pairing_end(&mut self, stack: &Stack, addr: Option<&[u8; 6]>, generation: Option<u32>) {
        let Some(t) = self.pairing.target.as_ref() else { return };
        if addr.is_some_and(|a| *a != t.addr) || generation.is_some_and(|g| g != t.generation) {
            return;
        }
        let Some(t) = self.pairing.target.take() else { return };
        if t.le {
            stack.sm_passkey_display(false);
        } else {
            stack.hid_pairing_target(None);
        }
        if t.shown {
            let _ = self.events.send(HostEvent::Hid(HidEvent::PasskeyDone(t.addr)));
        }
        tracing::debug!(
            "hid: pairing window of the new device closed (a number was {}shown)",
            if t.shown { "" } else { "not " }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passkeys_are_six_digits_and_hidden_in_debug() {
        assert_eq!(Passkey::new(42).digits(), "000042");
        assert_eq!(Passkey::new(1_234_567).digits(), "234567");
        for _ in 0..20 {
            let p = Passkey::random().digits();
            assert!(p.len() == 6 && p.bytes().all(|b| b.is_ascii_digit()));
        }
        let e = HidEvent::Passkey([1, 2, 3, 4, 5, 6], Passkey::new(123456));
        assert!(!format!("{e:?}").contains("123456"));
    }
}
