//! Keyboard lights: Num Lock, Caps Lock and Scroll Lock as the keyboard's
//! LED output report
//!
//! The lights are a bit mask, in the order of the boot keyboard's output
//! byte (HID 1.11, Appendix B.1): bit 0 Num Lock, bit 1 Caps Lock, bit 2
//! Scroll Lock.

pub const NUM_LOCK: u8 = 0x01;
pub const CAPS_LOCK: u8 = 0x02;
pub const SCROLL_LOCK: u8 = 0x04;
/// Every light this crate knows
pub const ALL: u8 = NUM_LOCK | CAPS_LOCK | SCROLL_LOCK;

/// Where a keyboard's output report holds its lights (from its report
/// descriptor, [`crate::ReportDescriptor::led_output`])
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LedOutput {
    /// 0 when the descriptor has no report IDs
    pub report_id: u8,
    /// The report's length in bytes, without the ID byte
    pub len: usize,
    /// Bit offset and size of Num Lock, Caps Lock and Scroll Lock (bits 0,
    /// 1 and 2 of the mask); None: the keyboard has no such light
    pub fields: [Option<(u32, u32)>; 3],
}

impl LedOutput {
    /// The boot keyboard's: one byte, the lights in bits 0 to 2
    pub fn boot() -> Self {
        LedOutput { report_id: 0, len: 1, fields: [Some((0, 1)), Some((1, 1)), Some((2, 1))] }
    }

    /// The output report for the lights in `mask`, without the ID byte: each
    /// light that is on gets the value 1, everything else is 0
    pub fn report(&self, mask: u8) -> Vec<u8> {
        let mut out = vec![0u8; self.len];
        for (bit, field) in self.fields.iter().enumerate() {
            let Some((offset, _size)) = field else { continue };
            if mask & (1 << bit) == 0 {
                continue;
            }
            if let Some(byte) = out.get_mut((offset / 8) as usize) {
                *byte |= 1 << (offset % 8);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_report_is_the_mask() {
        let boot = LedOutput::boot();
        for mask in 0..=ALL {
            assert_eq!(boot.report(mask), vec![mask]);
        }
        // Bits the keyboard has no light for are left out
        assert_eq!(boot.report(0xF8), vec![0]);
    }

    #[test]
    fn wide_fields_get_the_value_one() {
        let leds = LedOutput { report_id: 2, len: 2, fields: [Some((8, 8)), None, Some((3, 2))] };
        assert_eq!(leds.report(NUM_LOCK), vec![0x00, 0x01]);
        assert_eq!(leds.report(CAPS_LOCK), vec![0x00, 0x00]);
        assert_eq!(leds.report(ALL), vec![0x08, 0x01]);
    }

    #[test]
    fn fields_past_the_report_are_skipped() {
        let leds = LedOutput { report_id: 0, len: 1, fields: [Some((9, 1)), Some((1, 1)), None] };
        assert_eq!(leds.report(ALL), vec![0x02]);
    }
}
