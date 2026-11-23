//! HID usages to Windows key codes
//!
//! Keyboard keys map to PS/2 set 1 scan codes, the codes Windows' keyboard
//! layouts are written for, so the user's layout decides the character.
//! A few keys have no single scan code that SendInput can send and use a
//! virtual key instead. Consumer keys (media, browser) map to virtual keys.

/// What to send to Windows for one key
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTarget {
    /// Set 1 make code; `extended` is the E0 prefix
    Scan {
        scan: u16,
        extended: bool,
    },
    VirtualKey(u16),
}

const fn s(scan: u16) -> Option<KeyTarget> {
    Some(KeyTarget::Scan { scan, extended: false })
}

const fn e(scan: u16) -> Option<KeyTarget> {
    Some(KeyTarget::Scan { scan, extended: true })
}

const fn vk(code: u16) -> Option<KeyTarget> {
    Some(KeyTarget::VirtualKey(code))
}

const VK_PAUSE: u16 = 0x13;
const VK_HANGUL: u16 = 0x15;
const VK_HANJA: u16 = 0x19;
const VK_BROWSER_BACK: u16 = 0xA6;
const VK_BROWSER_FORWARD: u16 = 0xA7;
const VK_BROWSER_REFRESH: u16 = 0xA8;
const VK_BROWSER_STOP: u16 = 0xA9;
const VK_BROWSER_SEARCH: u16 = 0xAA;
const VK_BROWSER_FAVORITES: u16 = 0xAB;
const VK_BROWSER_HOME: u16 = 0xAC;
const VK_VOLUME_MUTE: u16 = 0xAD;
const VK_VOLUME_DOWN: u16 = 0xAE;
const VK_VOLUME_UP: u16 = 0xAF;
const VK_MEDIA_NEXT_TRACK: u16 = 0xB0;
const VK_MEDIA_PREV_TRACK: u16 = 0xB1;
const VK_MEDIA_STOP: u16 = 0xB2;
const VK_MEDIA_PLAY_PAUSE: u16 = 0xB3;
const VK_LAUNCH_MAIL: u16 = 0xB4;
const VK_LAUNCH_MEDIA_SELECT: u16 = 0xB5;
const VK_LAUNCH_APP1: u16 = 0xB6;
const VK_LAUNCH_APP2: u16 = 0xB7;

/// Keyboard/Keypad page (0x07) usage to a Windows key; None for usages
/// with no key (reserved, error codes, keys Windows has no code for)
pub fn keyboard(usage: u16) -> Option<KeyTarget> {
    match usage {
        // Letters a to z
        0x04 => s(0x1E),
        0x05 => s(0x30),
        0x06 => s(0x2E),
        0x07 => s(0x20),
        0x08 => s(0x12),
        0x09 => s(0x21),
        0x0A => s(0x22),
        0x0B => s(0x23),
        0x0C => s(0x17),
        0x0D => s(0x24),
        0x0E => s(0x25),
        0x0F => s(0x26),
        0x10 => s(0x32),
        0x11 => s(0x31),
        0x12 => s(0x18),
        0x13 => s(0x19),
        0x14 => s(0x10),
        0x15 => s(0x13),
        0x16 => s(0x1F),
        0x17 => s(0x14),
        0x18 => s(0x16),
        0x19 => s(0x2F),
        0x1A => s(0x11),
        0x1B => s(0x2D),
        0x1C => s(0x15),
        0x1D => s(0x2C),
        // 1 to 9, then 0
        0x1E..=0x26 => s(usage - 0x1E + 0x02),
        0x27 => s(0x0B),
        0x28 => s(0x1C),                       // Enter
        0x29 => s(0x01),                       // Escape
        0x2A => s(0x0E),                       // Backspace
        0x2B => s(0x0F),                       // Tab
        0x2C => s(0x39),                       // Space
        0x2D => s(0x0C),                       // - _
        0x2E => s(0x0D),                       // = +
        0x2F => s(0x1A),                       // [ {
        0x30 => s(0x1B),                       // ] }
        0x31 => s(0x2B),                       // \ |
        0x32 => s(0x2B),                       // Non-US # ~, the same key position
        0x33 => s(0x27),                       // ; :
        0x34 => s(0x28),                       // ' "
        0x35 => s(0x29),                       // ` ~ (Hankaku/Zenkaku on JIS)
        0x36 => s(0x33),                       // ,
        0x37 => s(0x34),                       // .
        0x38 => s(0x35),                       // /
        0x39 => s(0x3A),                       // Caps Lock
        0x3A..=0x43 => s(usage - 0x3A + 0x3B), // F1 to F10
        0x44 => s(0x57),                       // F11
        0x45 => s(0x58),                       // F12
        0x46 => e(0x37),                       // Print Screen
        0x47 => s(0x46),                       // Scroll Lock
        // Pause is an E1 sequence, which SendInput cannot send by scan code
        0x48 => vk(VK_PAUSE),
        0x49 => e(0x52), // Insert
        0x4A => e(0x47), // Home
        0x4B => e(0x49), // Page Up
        0x4C => e(0x53), // Delete
        0x4D => e(0x4F), // End
        0x4E => e(0x51), // Page Down
        0x4F => e(0x4D), // Right
        0x50 => e(0x4B), // Left
        0x51 => e(0x50), // Down
        0x52 => e(0x48), // Up
        // Num Lock as Windows reports it: scan 0x45 with the extended flag
        0x53 => e(0x45),
        0x54 => e(0x35), // Keypad /
        0x55 => s(0x37), // Keypad *
        0x56 => s(0x4A), // Keypad -
        0x57 => s(0x4E), // Keypad +
        0x58 => e(0x1C), // Keypad Enter
        0x59 => s(0x4F), // Keypad 1
        0x5A => s(0x50),
        0x5B => s(0x51),
        0x5C => s(0x4B),
        0x5D => s(0x4C),
        0x5E => s(0x4D),
        0x5F => s(0x47),
        0x60 => s(0x48),
        0x61 => s(0x49),                       // Keypad 9
        0x62 => s(0x52),                       // Keypad 0
        0x63 => s(0x53),                       // Keypad .
        0x64 => s(0x56),                       // Non-US \ | (ISO key left of Z)
        0x65 => e(0x5D),                       // Application (menu)
        0x66 => e(0x5E),                       // Power
        0x67 => s(0x59),                       // Keypad =
        0x68..=0x6F => s(usage - 0x68 + 0x64), // F13 to F20
        0x70 => s(0x6C),                       // F21
        0x71 => s(0x6D),                       // F22
        0x72 => s(0x6E),                       // F23
        0x73 => s(0x76),                       // F24
        0x7F => vk(VK_VOLUME_MUTE),
        0x80 => vk(VK_VOLUME_UP),
        0x81 => vk(VK_VOLUME_DOWN),
        0x85 => s(0x7E), // Keypad , (Brazilian)
        0x87 => s(0x73), // International1: JIS \ _ (Ro), Brazilian / ?
        0x88 => s(0x70), // International2: Katakana/Hiragana
        0x89 => s(0x7D), // International3: JIS Yen
        0x8A => s(0x79), // International4: Henkan
        0x8B => s(0x7B), // International5: Muhenkan
        // Lang1/Lang2: Korean Hangul and Hanja keys. Their set 1 codes look
        // like key releases, so the virtual keys go instead.
        0x90 => vk(VK_HANGUL),
        0x91 => vk(VK_HANJA),
        0x92 => s(0x78), // Lang3: Katakana
        0x93 => s(0x77), // Lang4: Hiragana
        0x94 => s(0x76), // Lang5: Zenkaku/Hankaku
        0xE0 => s(0x1D), // Left Control
        0xE1 => s(0x2A), // Left Shift
        0xE2 => s(0x38), // Left Alt
        0xE3 => e(0x5B), // Left Windows
        0xE4 => e(0x1D), // Right Control
        0xE5 => s(0x36), // Right Shift
        0xE6 => e(0x38), // Right Alt (AltGr)
        0xE7 => e(0x5C), // Right Windows
        _ => None,
    }
}

/// Consumer page (0x0C) usage to a virtual key
pub fn consumer(usage: u16) -> Option<u16> {
    Some(match usage {
        0xE2 => VK_VOLUME_MUTE,
        0xE9 => VK_VOLUME_UP,
        0xEA => VK_VOLUME_DOWN,
        // Windows has one Play/Pause key
        0xB0 | 0xB1 | 0xCD => VK_MEDIA_PLAY_PAUSE,
        0xB5 => VK_MEDIA_NEXT_TRACK,
        0xB6 => VK_MEDIA_PREV_TRACK,
        0xB7 | 0xCC => VK_MEDIA_STOP,
        0x183 => VK_LAUNCH_MEDIA_SELECT,
        0x18A => VK_LAUNCH_MAIL,
        0x192 => VK_LAUNCH_APP2, // Calculator
        0x194 => VK_LAUNCH_APP1, // Local browser (This PC)
        0x221 => VK_BROWSER_SEARCH,
        0x223 => VK_BROWSER_HOME,
        0x224 => VK_BROWSER_BACK,
        0x225 => VK_BROWSER_FORWARD,
        0x226 => VK_BROWSER_STOP,
        0x227 => VK_BROWSER_REFRESH,
        0x22A => VK_BROWSER_FAVORITES,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_function_keys() {
        assert_eq!(keyboard(0x04), s(0x1E)); // A
        assert_eq!(keyboard(0x1D), s(0x2C)); // Z
        assert_eq!(keyboard(0x1E), s(0x02)); // 1
        assert_eq!(keyboard(0x26), s(0x0A)); // 9
        assert_eq!(keyboard(0x27), s(0x0B)); // 0
        assert_eq!(keyboard(0x3A), s(0x3B)); // F1
        assert_eq!(keyboard(0x43), s(0x44)); // F10
        assert_eq!(keyboard(0x68), s(0x64)); // F13
        assert_eq!(keyboard(0x6F), s(0x6B)); // F20
        assert_eq!(keyboard(0x73), s(0x76)); // F24
    }

    #[test]
    fn extended_keys() {
        for usage in [0x46, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x58, 0x65] {
            assert!(matches!(keyboard(usage), Some(KeyTarget::Scan { extended: true, .. })), "{usage:#x}");
        }
        assert_eq!(keyboard(0x48), vk(VK_PAUSE));
    }

    #[test]
    fn modifiers_distinct_left_right() {
        let mods: Vec<_> = (0xE0..=0xE7).map(keyboard).collect();
        assert!(mods.iter().all(Option::is_some));
        for (i, a) in mods.iter().enumerate() {
            for b in &mods[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn every_key_of_a_full_keyboard_maps() {
        // 104/105-key set, keypad and the JIS/Korean extras
        let usages = (0x04..=0x67).chain(0x87..=0x8B).chain(0x90..=0x94).chain(0xE0..=0xE7);
        for usage in usages {
            assert!(keyboard(usage).is_some(), "{usage:#x}");
        }
        for none in [0x00, 0x01, 0x02, 0x03, 0xA5, 0xDF, 0xE8, 0xFFFF] {
            assert_eq!(keyboard(none), None, "{none:#x}");
        }
    }

    #[test]
    fn consumer_keys() {
        assert_eq!(consumer(0xE9), Some(VK_VOLUME_UP));
        assert_eq!(consumer(0xCD), Some(VK_MEDIA_PLAY_PAUSE));
        assert_eq!(consumer(0x224), Some(VK_BROWSER_BACK));
        assert_eq!(consumer(0x192), Some(VK_LAUNCH_APP2));
        assert_eq!(consumer(0x238), None); // AC Pan is a wheel
        assert_eq!(consumer(0), None);
    }
}
