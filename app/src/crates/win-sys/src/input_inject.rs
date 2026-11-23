//! Keyboard and mouse input injected into Windows, for keyboards and mice
//! connected to BlueCaster (HID host)
//!
//! Keys go in as scan codes, so Windows applies the user's keyboard layout
//! as for a wired keyboard. Mouse moves are relative, so pointer speed and
//! acceleration settings apply too. Every event carries [`INJECTED_TAG`] in
//! `dwExtraInfo`, so a low-level hook can tell our input from a real device.

use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL, VK_NUMLOCK, VK_SCROLL};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};

/// Windows' Num Lock, Caps Lock and Scroll Lock state as a mask: bit 0 Num
/// Lock, bit 1 Caps Lock, bit 2 Scroll Lock (the boot keyboard's LED byte)
pub fn lock_keys() -> u8 {
    let on = |vk: VIRTUAL_KEY| {
        // SAFETY: plain call; the low bit of the result is the toggle state.
        let state = unsafe { GetKeyState(i32::from(vk.0)) };
        state & 1 != 0
    };
    u8::from(on(VK_NUMLOCK)) | u8::from(on(VK_CAPITAL)) << 1 | u8::from(on(VK_SCROLL)) << 2
}

/// `dwExtraInfo` of every event sent here ("BCHI")
pub const INJECTED_TAG: usize = 0x4243_4849;

// winuser.h XBUTTON1 / XBUTTON2, the mouseData of an X button event
const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

/// A mouse button
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Usually "Back"
    X1,
    /// Usually "Forward"
    X2,
}

/// One input event for Windows
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inject {
    /// A key by its PS/2 set 1 scan code; `extended` is the E0 prefix
    Key {
        scan: u16,
        extended: bool,
        up: bool,
    },
    /// A key by virtual key code (media and browser keys, Pause)
    VirtualKey {
        vk: u16,
        up: bool,
    },
    /// A relative pointer move in mouse units
    Move {
        dx: i32,
        dy: i32,
    },
    Button {
        button: MouseButton,
        up: bool,
    },
    /// Vertical wheel in `WHEEL_DELTA` units (120 = one notch, positive = away from the user)
    Wheel(i32),
    /// Horizontal wheel in `WHEEL_DELTA` units (positive = right)
    HWheel(i32),
}

#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    /// Fewer events went in than were sent: another desktop is active (the
    /// lock screen, a UAC prompt) or a higher-integrity window has the focus
    #[error("SendInput inserted {inserted} of {requested} events: {source}")]
    Partial { inserted: usize, requested: usize, source: windows::core::Error },
}

// Media and browser keys are extended keys, as on a real keyboard
fn vk_is_extended(vk: u16) -> bool {
    (0xA6..=0xB7).contains(&vk)
}

fn to_input(inject: Inject) -> INPUT {
    let key = |ki: KEYBDINPUT| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } };
    let mouse = |flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: u32| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: INJECTED_TAG },
        },
    };
    let up_flag = |up: bool| if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    match inject {
        Inject::Key { scan, extended, up } => {
            let ext = if extended { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
            key(KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: KEYEVENTF_SCANCODE | ext | up_flag(up),
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            })
        }
        Inject::VirtualKey { vk, up } => {
            let ext = if vk_is_extended(vk) { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
            key(KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: ext | up_flag(up),
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            })
        }
        Inject::Move { dx, dy } => mouse(MOUSEEVENTF_MOVE, dx, dy, 0),
        Inject::Button { button, up } => {
            let (flags, data) = match (button, up) {
                (MouseButton::Left, false) => (MOUSEEVENTF_LEFTDOWN, 0),
                (MouseButton::Left, true) => (MOUSEEVENTF_LEFTUP, 0),
                (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTUP, 0),
                (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEUP, 0),
                (MouseButton::X1, false) => (MOUSEEVENTF_XDOWN, XBUTTON1),
                (MouseButton::X1, true) => (MOUSEEVENTF_XUP, XBUTTON1),
                (MouseButton::X2, false) => (MOUSEEVENTF_XDOWN, XBUTTON2),
                (MouseButton::X2, true) => (MOUSEEVENTF_XUP, XBUTTON2),
            };
            mouse(flags, 0, 0, data)
        }
        // mouseData holds the signed wheel delta as a DWORD
        Inject::Wheel(delta) => mouse(MOUSEEVENTF_WHEEL, 0, 0, delta as u32),
        Inject::HWheel(delta) => mouse(MOUSEEVENTF_HWHEEL, 0, 0, delta as u32),
    }
}

/// The system tick count (GetTickCount, milliseconds, wraps after 49 days)
pub fn tick_now() -> u32 {
    // SAFETY: plain call without arguments.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

/// Whether Windows registered input in this session at or after `tick`
/// (a [`tick_now`] value): GetLastInputInfo's time is not older. None when
/// the call failed. Microsoft documents the value as per session; it does
/// not say whether injected input updates it, so this is a hint only.
pub fn input_since(tick: u32) -> Option<bool> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: `info` is a valid LASTINPUTINFO with cbSize set.
    let ok = unsafe { GetLastInputInfo(&mut info) }.as_bool();
    ok.then(|| tick_not_older(info.dwTime, tick))
}

/// `last` is at or after `since`, across the tick count's wrap
pub fn tick_not_older(last: u32, since: u32) -> bool {
    (last.wrapping_sub(since) as i32) >= 0
}

/// Sends `inputs` in order as one uninterrupted batch. Returns how many went
/// in; when Windows took only part of them, the error says how many.
pub fn send(inputs: &[Inject]) -> Result<usize, InjectError> {
    if inputs.is_empty() {
        return Ok(0);
    }
    let batch: Vec<INPUT> = inputs.iter().map(|&i| to_input(i)).collect();
    // SAFETY: `batch` is a valid slice of fully initialised INPUTs and the
    // size passed is that of one INPUT, as SendInput requires.
    let inserted = unsafe { SendInput(&batch, size_of::<INPUT>() as i32) } as usize;
    if inserted != batch.len() {
        let source = windows::core::Error::from_thread();
        tracing::debug!("input_inject: SendInput inserted {inserted} of {} ({source})", batch.len());
        return Err(InjectError::Partial { inserted, requested: batch.len(), source });
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_by_scan_code_is_tagged() {
        let input = to_input(Inject::Key { scan: 0x1D, extended: true, up: true });
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        // SAFETY: a keyboard INPUT was built, so `ki` is the active field.
        let ki = unsafe { input.Anonymous.ki };
        assert_eq!(ki.wScan, 0x1D);
        assert_eq!(ki.dwFlags, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
        assert_eq!(ki.dwExtraInfo, INJECTED_TAG);
    }

    #[test]
    fn negative_wheel_is_twos_complement() {
        let input = to_input(Inject::Wheel(-120));
        // SAFETY: a mouse INPUT was built, so `mi` is the active field.
        let mi = unsafe { input.Anonymous.mi };
        assert_eq!(mi.mouseData as i32, -120);
        assert_eq!(mi.dwFlags, MOUSEEVENTF_WHEEL);
        assert_eq!(mi.dwExtraInfo, INJECTED_TAG);
    }

    #[test]
    fn x2_button_names_xbutton2() {
        let input = to_input(Inject::Button { button: MouseButton::X2, up: false });
        // SAFETY: a mouse INPUT was built, so `mi` is the active field.
        let mi = unsafe { input.Anonymous.mi };
        assert_eq!((mi.dwFlags, mi.mouseData), (MOUSEEVENTF_XDOWN, XBUTTON2));
    }

    #[test]
    fn tick_comparison_survives_the_wrap() {
        assert!(tick_not_older(100, 100));
        assert!(tick_not_older(101, 100));
        assert!(!tick_not_older(99, 100));
        assert!(tick_not_older(5, u32::MAX - 5));
        assert!(!tick_not_older(u32::MAX - 5, 5));
    }

    #[test]
    fn empty_batch_sends_nothing() {
        assert_eq!(send(&[]).unwrap(), 0);
    }
}
