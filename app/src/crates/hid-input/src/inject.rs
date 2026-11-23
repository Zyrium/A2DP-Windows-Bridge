//! Input events sent to Windows

use win_sys::input_inject::{self, Inject, MouseButton};

use crate::InputEvent;
use crate::keymap::{self, KeyTarget};

pub use win_sys::input_inject::{INJECTED_TAG, InjectError};

/// One HID wheel detent in Windows wheel units (WHEEL_DELTA)
pub const WHEEL_DELTA: i32 = 120;

/// The Windows input for one event; None when Windows has no key or
/// button for it
pub fn to_inject(event: InputEvent) -> Option<Inject> {
    match event {
        InputEvent::Key { usage, down } => Some(match keymap::keyboard(usage)? {
            KeyTarget::Scan { scan, extended } => Inject::Key { scan, extended, up: !down },
            KeyTarget::VirtualKey(vk) => Inject::VirtualKey { vk, up: !down },
        }),
        InputEvent::Consumer { usage, down } => Some(Inject::VirtualKey { vk: keymap::consumer(usage)?, up: !down }),
        InputEvent::Button { index, down } => {
            let button = match index {
                1 => MouseButton::Left,
                2 => MouseButton::Right,
                3 => MouseButton::Middle,
                4 => MouseButton::X1,
                5 => MouseButton::X2,
                _ => return None,
            };
            Some(Inject::Button { button, up: !down })
        }
        InputEvent::Move { dx, dy } => Some(Inject::Move { dx, dy }),
        InputEvent::Wheel { delta } => Some(Inject::Wheel(delta.saturating_mul(WHEEL_DELTA))),
        InputEvent::HWheel { delta } => Some(Inject::HWheel(delta.saturating_mul(WHEEL_DELTA))),
    }
}

/// The Windows inputs for `events`, in order, without those Windows has
/// no key or button for
pub fn to_injects(events: &[InputEvent]) -> Vec<Inject> {
    events.iter().filter_map(|&e| to_inject(e)).collect()
}

/// Sends `events` to Windows as one batch. Returns how many Windows inputs
/// went in (events with no key or button are left out).
pub fn inject(events: &[InputEvent]) -> Result<usize, InjectError> {
    input_inject::send(&to_injects(events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_consumer() {
        assert_eq!(
            to_inject(InputEvent::Key { usage: 0xE6, down: true }),
            Some(Inject::Key { scan: 0x38, extended: true, up: false })
        );
        assert_eq!(
            to_inject(InputEvent::Key { usage: 0x04, down: false }),
            Some(Inject::Key { scan: 0x1E, extended: false, up: true })
        );
        assert_eq!(
            to_inject(InputEvent::Consumer { usage: 0xEA, down: true }),
            Some(Inject::VirtualKey { vk: 0xAE, up: false })
        );
        assert_eq!(to_inject(InputEvent::Key { usage: 0x01, down: true }), None);
        assert_eq!(to_inject(InputEvent::Consumer { usage: 0x01, down: true }), None);
    }

    #[test]
    fn buttons_and_wheel() {
        assert_eq!(
            to_inject(InputEvent::Button { index: 4, down: false }),
            Some(Inject::Button { button: MouseButton::X1, up: true })
        );
        assert_eq!(to_inject(InputEvent::Button { index: 6, down: true }), None);
        assert_eq!(to_inject(InputEvent::Wheel { delta: -2 }), Some(Inject::Wheel(-240)));
        assert_eq!(to_inject(InputEvent::HWheel { delta: i32::MAX }), Some(Inject::HWheel(i32::MAX)));
        assert_eq!(to_inject(InputEvent::Move { dx: 3, dy: -4 }), Some(Inject::Move { dx: 3, dy: -4 }));
    }

    #[test]
    fn release_all_maps_to_ups_only() {
        let mut d = crate::Decoder::boot_keyboard();
        d.decode(&[0xFF, 0, 0x04, 0x48, 0x53, 0x90, 0x05, 0]);
        let injects = to_injects(&d.release_all());
        assert_eq!(injects.len(), 8 + 5);
        assert!(
            injects.iter().all(|i| matches!(i, Inject::Key { up: true, .. } | Inject::VirtualKey { up: true, .. }))
        );
    }
}
