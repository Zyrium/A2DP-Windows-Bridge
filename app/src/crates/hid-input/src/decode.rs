//! Input reports to input events
//!
//! The decoder remembers what each field of each report holds down and
//! emits only changes. A key held through two fields or two reports (a
//! keyboard that lists a key in both its boot and NKRO reports) goes up
//! only when no field holds it any more. [`Decoder::release_all`] lifts
//! everything, for a disconnect or when input is switched off: a key left
//! down in Windows keeps repeating until the user presses it again.

use std::collections::{BTreeMap, BTreeSet};

use crate::descriptor::{
    Field, MAX_VALUE_BITS, PAGE_BUTTON, PAGE_CONSUMER, PAGE_GENERIC_DESKTOP, PAGE_KEYBOARD, ReportDescriptor,
};

const GD_X: u16 = 0x30;
const GD_Y: u16 = 0x31;
const GD_WHEEL: u16 = 0x38;
const AC_PAN: u16 = 0x0238;
// Keyboard ErrorRollOver, POSTFail, ErrorUndefined: the array is not valid
const KEY_ERRORS: std::ops::RangeInclusive<u16> = 0x01..=0x03;
const KEY_LEFT_CONTROL: u16 = 0xE0;
const KEY_RIGHT_GUI: u16 = 0xE7;

/// One change of input
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// Keyboard/Keypad page usage, modifiers 0xE0 to 0xE7 included
    Key { usage: u16, down: bool },
    /// Consumer page usage (media and browser keys)
    Consumer { usage: u16, down: bool },
    /// Mouse button, 1-based (1 = primary)
    Button { index: u8, down: bool },
    /// Relative pointer move in device units
    Move { dx: i32, dy: i32 },
    /// Vertical wheel in detents, positive = away from the user
    Wheel { delta: i32 },
    /// Horizontal wheel in detents, positive = right
    HWheel { delta: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Press {
    Key(u16),
    Consumer(u16),
    Button(u8),
}

impl Press {
    fn is_modifier(self) -> bool {
        matches!(self, Press::Key(u) if (KEY_LEFT_CONTROL..=KEY_RIGHT_GUI).contains(&u))
    }

    fn event(self, down: bool) -> InputEvent {
        match self {
            Press::Key(usage) => InputEvent::Key { usage, down },
            Press::Consumer(usage) => InputEvent::Consumer { usage, down },
            Press::Button(index) => InputEvent::Button { index, down },
        }
    }
}

/// Decodes the input reports of one device
#[derive(Debug, Clone)]
pub struct Decoder {
    reports: BTreeMap<u8, Vec<Field>>,
    uses_report_ids: bool,
    // What each field last held down, per report ID and field index
    fields: BTreeMap<(u8, usize), BTreeSet<Press>>,
    // How many fields hold each press
    held: BTreeMap<Press, u32>,
    leds: Option<crate::LedOutput>,
}

/// Boot protocol keyboard: modifiers, reserved byte, 6 key array
const BOOT_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
    0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x19,
    0x00, 0x2A, 0xFF, 0x00, 0x81, 0x00, 0xC0,
];

/// Boot protocol mouse: 5 buttons, X, Y and a wheel byte that many mice
/// add. The wheel is its own field, so a 3-byte report still moves.
const BOOT_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, // Mouse, Pointer
    0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, // buttons
    0x95, 0x01, 0x75, 0x03, 0x81, 0x01, // padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06, // X, Y
    0x09, 0x38, 0x95, 0x01, 0x81, 0x06, // wheel
    0xC0, 0xC0,
];

/// Reads `size` bits (1 to 32) at bit `offset`, least significant first;
/// None when they are not all in `data`.
fn read_bits(data: &[u8], offset: u32, size: u32) -> Option<u32> {
    let end = u64::from(offset) + u64::from(size);
    if size == 0 || size > MAX_VALUE_BITS || end > data.len() as u64 * 8 {
        return None;
    }
    let first = (offset / 8) as usize;
    let last = ((end - 1) / 8) as usize;
    let mut raw = 0u64;
    for (i, &b) in data[first..=last].iter().enumerate() {
        raw |= u64::from(b) << (8 * i);
    }
    let value = (raw >> (offset % 8)) & ((1u64 << size) - 1);
    Some(value as u32)
}

fn to_value(raw: u32, size: u32, signed: bool) -> i64 {
    if signed && size < 32 && raw & (1 << (size - 1)) != 0 {
        i64::from(raw) - (1i64 << size)
    } else if signed {
        i64::from(raw as i32)
    } else {
        i64::from(raw)
    }
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[derive(Default)]
struct Motion {
    dx: i64,
    dy: i64,
    wheel: i64,
    hwheel: i64,
}

impl Decoder {
    pub fn new(descriptor: &ReportDescriptor) -> Self {
        Decoder {
            reports: descriptor.reports.clone(),
            uses_report_ids: descriptor.uses_report_ids,
            fields: BTreeMap::new(),
            held: BTreeMap::new(),
            leds: descriptor.led_output,
        }
    }

    /// The device's keyboard lights output report; None: it has none
    pub fn leds(&self) -> Option<&crate::LedOutput> {
        self.leds.as_ref()
    }

    pub fn from_descriptor(bytes: &[u8]) -> Result<Self, crate::DescriptorError> {
        Ok(Self::new(&ReportDescriptor::parse(bytes)?))
    }

    /// Boot protocol keyboard (8-byte reports, no report ID)
    pub fn boot_keyboard() -> Self {
        // A fixed descriptor that the tests parse
        Self::new(&ReportDescriptor::parse(BOOT_KEYBOARD).unwrap_or_default())
    }

    /// Boot protocol mouse (3 or 4-byte reports, no report ID)
    pub fn boot_mouse() -> Self {
        Self::new(&ReportDescriptor::parse(BOOT_MOUSE).unwrap_or_default())
    }

    pub fn uses_report_ids(&self) -> bool {
        self.uses_report_ids
    }

    /// Whether anything is held down
    pub fn any_held(&self) -> bool {
        !self.held.is_empty()
    }

    /// Decodes one input report as it arrives on the interrupt channel:
    /// starting with the report ID byte when the descriptor has report IDs.
    pub fn decode(&mut self, report: &[u8]) -> Vec<InputEvent> {
        if self.uses_report_ids {
            match report.split_first() {
                Some((&id, data)) => self.decode_report(id, data),
                None => Vec::new(),
            }
        } else {
            self.decode_report(0, report)
        }
    }

    /// Decodes one input report given with its ID apart from the data. A
    /// descriptor without report IDs has one report: `id` is then ignored.
    pub fn decode_with_id(&mut self, id: u8, data: &[u8]) -> Vec<InputEvent> {
        let id = if self.uses_report_ids { id } else { 0 };
        self.decode_report(id, data)
    }

    fn decode_report(&mut self, id: u8, data: &[u8]) -> Vec<InputEvent> {
        let Some(fields) = self.reports.get(&id) else {
            return Vec::new();
        };
        let mut motion = Motion::default();
        let mut changes: Vec<(usize, BTreeSet<Press>)> = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            if let Some(pressed) = decode_field(field, data, &mut motion) {
                changes.push((index, pressed));
            }
        }

        let mut ups = Vec::new();
        let mut downs = Vec::new();
        for (index, pressed) in changes {
            let old = self.fields.remove(&(id, index)).unwrap_or_default();
            for &p in old.difference(&pressed) {
                if release(&mut self.held, p) {
                    ups.push(p);
                }
            }
            for &p in pressed.difference(&old) {
                let count = self.held.entry(p).or_insert(0);
                *count += 1;
                if *count == 1 {
                    downs.push(p);
                }
            }
            if !pressed.is_empty() {
                self.fields.insert((id, index), pressed);
            }
        }

        // Keys go up before modifiers, modifiers go down before keys
        ups.sort_by_key(|p| p.is_modifier());
        downs.sort_by_key(|p| !p.is_modifier());
        let mut events: Vec<InputEvent> = ups.into_iter().map(|p| p.event(false)).collect();
        events.extend(downs.into_iter().map(|p| p.event(true)));
        if motion.dx != 0 || motion.dy != 0 {
            events.push(InputEvent::Move { dx: clamp_i32(motion.dx), dy: clamp_i32(motion.dy) });
        }
        if motion.wheel != 0 {
            events.push(InputEvent::Wheel { delta: clamp_i32(motion.wheel) });
        }
        if motion.hwheel != 0 {
            events.push(InputEvent::HWheel { delta: clamp_i32(motion.hwheel) });
        }
        events
    }

    /// Up events for everything held down, and the state cleared. After
    /// this nothing is held, whatever reports came before.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        self.fields.clear();
        let mut ups: Vec<Press> = std::mem::take(&mut self.held).into_keys().collect();
        ups.sort_by_key(|p| p.is_modifier());
        ups.into_iter().map(|p| p.event(false)).collect()
    }

    /// What the device's input reports carry: keys, a pointer, or both
    pub fn kind(&self) -> InputKind {
        let mut kind = InputKind::default();
        let page_of = |u: u32| (u >> 16) as u16;
        let id_of = |u: u32| (u & 0xFFFF) as u16;
        for field in self.reports.values().flatten().filter(|f| !f.flags.constant) {
            for spec in &field.usages {
                let (lo, hi) = match *spec {
                    crate::descriptor::UsageSpec::Single(u) => (u, u),
                    crate::descriptor::UsageSpec::Range { min, max } => (min, max),
                };
                match page_of(lo) {
                    PAGE_KEYBOARD => kind.keyboard = true,
                    PAGE_BUTTON => kind.pointer = true,
                    PAGE_GENERIC_DESKTOP if id_of(lo) <= GD_Y && id_of(hi) >= GD_X => kind.pointer = true,
                    _ => {}
                }
            }
        }
        kind
    }
}

/// What a device's reports carry ([`Decoder::kind`])
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputKind {
    /// Keys of the keyboard page
    pub keyboard: bool,
    /// Pointer motion or mouse buttons
    pub pointer: bool,
}

// Drops one holder of `p`; true when none is left
fn release(held: &mut BTreeMap<Press, u32>, p: Press) -> bool {
    match held.get_mut(&p) {
        Some(count) if *count > 1 => {
            *count -= 1;
            false
        }
        Some(_) => {
            held.remove(&p);
            true
        }
        None => false,
    }
}

/// The presses one field reports, adding its motion to `motion`. None
/// leaves the field's previous presses as they were: the field is not all
/// in the report, carries no keys or buttons, or reports a rollover error.
fn decode_field(field: &Field, data: &[u8], motion: &mut Motion) -> Option<BTreeSet<Press>> {
    if field.flags.constant || field.bit_size == 0 || field.bit_size > MAX_VALUE_BITS {
        return None;
    }
    let end = u64::from(field.bit_offset) + u64::from(field.bit_size) * u64::from(field.count);
    if end > data.len() as u64 * 8 {
        return None;
    }
    let mut pressed = BTreeSet::new();
    let mut has_presses = false;
    let mut rollover = false;
    for i in 0..field.count {
        let raw = read_bits(data, field.bit_offset + i * field.bit_size, field.bit_size)?;
        let value = to_value(raw, field.bit_size, field.signed());
        if field.flags.variable {
            let Some(usage) = field.variable_usage(i) else { continue };
            let (page, id) = ((usage >> 16) as u16, usage as u16);
            match page {
                PAGE_KEYBOARD if !KEY_ERRORS.contains(&id) && id != 0 => {
                    has_presses = true;
                    if value != 0 {
                        pressed.insert(Press::Key(id));
                    }
                }
                PAGE_BUTTON if (1..=255).contains(&id) => {
                    has_presses = true;
                    if value != 0 {
                        pressed.insert(Press::Button(id as u8));
                    }
                }
                PAGE_CONSUMER if id == AC_PAN => {
                    if field.flags.relative {
                        motion.hwheel += value;
                    }
                }
                PAGE_CONSUMER if id != 0 && !field.flags.relative => {
                    has_presses = true;
                    if value != 0 {
                        pressed.insert(Press::Consumer(id));
                    }
                }
                // Absolute pointers (tablets, touch) are not mice
                PAGE_GENERIC_DESKTOP if field.flags.relative => match id {
                    GD_X => motion.dx += value,
                    GD_Y => motion.dy += value,
                    GD_WHEEL => motion.wheel += value,
                    _ => {}
                },
                _ => {}
            }
        } else {
            // An array of selectors: each value picks one usage, 0 none
            let Some(usage) = field.array_usage(value) else {
                has_presses = true;
                continue;
            };
            let (page, id) = ((usage >> 16) as u16, usage as u16);
            match page {
                PAGE_KEYBOARD if KEY_ERRORS.contains(&id) => rollover = true,
                PAGE_KEYBOARD if id != 0 => {
                    pressed.insert(Press::Key(id));
                }
                PAGE_CONSUMER if id != 0 => {
                    pressed.insert(Press::Consumer(id));
                }
                PAGE_BUTTON if (1..=255).contains(&id) => {
                    pressed.insert(Press::Button(id as u8));
                }
                _ => {}
            }
            has_presses = true;
        }
    }
    // Too many keys at once: the device does not know which are down, so
    // the keys stay as the last valid report had them
    if rollover || !has_presses {
        return None;
    }
    Some(pressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::tests::{COMBO, KEYBOARD, MOUSE};

    use InputEvent::*;

    fn key(usage: u16, down: bool) -> InputEvent {
        Key { usage, down }
    }

    #[test]
    fn boot_descriptors_parse() {
        let k = ReportDescriptor::parse(BOOT_KEYBOARD).unwrap();
        assert_eq!(k.report_len(0), Some(8));
        let m = ReportDescriptor::parse(BOOT_MOUSE).unwrap();
        assert_eq!(m.report_len(0), Some(4));
        assert!(!Decoder::boot_keyboard().reports.is_empty());
        assert!(!Decoder::boot_mouse().reports.is_empty());
    }

    #[test]
    fn kind_comes_from_the_descriptor() {
        let kind = |d: &[u8]| Decoder::from_descriptor(d).unwrap().kind();
        assert_eq!(kind(KEYBOARD), InputKind { keyboard: true, pointer: false });
        assert_eq!(kind(MOUSE), InputKind { keyboard: false, pointer: true });
        assert_eq!(kind(COMBO), InputKind { keyboard: true, pointer: true });
        assert_eq!(Decoder::boot_mouse().kind(), InputKind { keyboard: false, pointer: true });
        assert_eq!(Decoder::boot_keyboard().kind(), InputKind { keyboard: true, pointer: false });
        // Consumer keys alone are neither
        let consumer = [0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x75, 0x10, 0x95, 0x01, 0x09, 0xE9, 0x81, 0x02, 0xC0];
        assert_eq!(kind(&consumer), InputKind::default());
    }

    #[test]
    fn read_bits_across_bytes() {
        assert_eq!(read_bits(&[0b1010_0000, 0b0000_0011], 5, 5), Some(0b11101));
        assert_eq!(read_bits(&[0xFF; 4], 0, 32), Some(u32::MAX));
        assert_eq!(read_bits(&[0x12, 0x34, 0x56, 0x78, 0x9A], 4, 32), Some(0xA785_6341));
        assert_eq!(read_bits(&[0xFF], 4, 5), None);
        assert_eq!(read_bits(&[0xFF], 0, 0), None);
        assert_eq!(to_value(0xFF, 8, true), -1);
        assert_eq!(to_value(0x7F, 8, true), 127);
        assert_eq!(to_value(u32::MAX, 32, true), -1);
        assert_eq!(to_value(0xFF, 8, false), 255);
    }

    #[test]
    fn keyboard_press_and_release() {
        let mut d = Decoder::from_descriptor(KEYBOARD).unwrap();
        // Shift + A
        assert_eq!(d.decode(&[0x02, 0, 0x04, 0, 0, 0, 0, 0]), vec![key(0xE1, true), key(0x04, true)]);
        // Same report again: nothing new
        assert!(d.decode(&[0x02, 0, 0x04, 0, 0, 0, 0, 0]).is_empty());
        // A held, B added in another slot
        assert_eq!(d.decode(&[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]), vec![key(0x05, true)]);
        // Slots shuffled: still the same keys
        assert!(d.decode(&[0x02, 0, 0x05, 0x04, 0, 0, 0, 0]).is_empty());
        // Shift and A released, B stays: keys first, then the modifier
        assert_eq!(d.decode(&[0, 0, 0x05, 0, 0, 0, 0, 0]), vec![key(0x04, false), key(0xE1, false)]);
        assert_eq!(d.decode(&[0x02, 0, 0, 0, 0, 0, 0, 0]), vec![key(0x05, false), key(0xE1, true)]);
        assert_eq!(d.decode(&[0, 0, 0, 0, 0, 0, 0, 0]), vec![key(0xE1, false)]);
        assert!(!d.any_held());
    }

    #[test]
    fn up_before_down_and_modifier_order() {
        let mut d = Decoder::boot_keyboard();
        d.decode(&[0x01, 0, 0x06, 0, 0, 0, 0, 0]); // Ctrl + C
        // Ctrl + C to Shift + V in one report
        assert_eq!(
            d.decode(&[0x02, 0, 0x19, 0, 0, 0, 0, 0]),
            vec![key(0x06, false), key(0xE0, false), key(0xE1, true), key(0x19, true)]
        );
    }

    #[test]
    fn rollover_keeps_keys() {
        let mut d = Decoder::boot_keyboard();
        d.decode(&[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]);
        // Phantom state: keys unchanged, the modifier byte still counts
        assert_eq!(d.decode(&[0, 0, 1, 1, 1, 1, 1, 1]), vec![key(0xE1, false)]);
        assert_eq!(d.decode(&[0, 0, 0x05, 0, 0, 0, 0, 0]), vec![key(0x04, false)]);
        assert_eq!(d.release_all(), vec![key(0x05, false)]);
    }

    #[test]
    fn short_and_long_reports() {
        let mut d = Decoder::boot_keyboard();
        assert!(d.decode(&[]).is_empty());
        // Only the modifier byte is there: the key array keeps its state
        assert_eq!(d.decode(&[0x01]), vec![key(0xE0, true)]);
        d.decode(&[0x01, 0, 0x04, 0, 0, 0, 0, 0]);
        assert!(d.decode(&[0x01, 0, 0x04]).is_empty());
        // Extra bytes after the report are ignored
        assert_eq!(d.decode(&[0, 0, 0x04, 0, 0, 0, 0, 0, 0xFF, 0xFF]), vec![key(0xE0, false)]);
        // Out-of-range array values select nothing
        let mut d = Decoder::from_descriptor(KEYBOARD).unwrap();
        assert_eq!(d.decode(&[0, 0, 0x04, 0, 0, 0, 0, 0]), vec![key(0x04, true)]);
        assert_eq!(d.decode(&[0, 0, 0xFF, 0, 0, 0, 0, 0]), vec![key(0x04, false)]);
    }

    #[test]
    fn mouse_buttons_move_wheel_pan() {
        let mut d = Decoder::from_descriptor(MOUSE).unwrap();
        assert!(d.uses_report_ids());
        // Left + X2 down, dx -2, dy 300, wheel -1, pan +2
        let r = [0x02, 0b1_0001, 0xFE, 0xFF, 0x2C, 0x01, 0xFF, 0x02];
        assert_eq!(
            d.decode(&r),
            vec![
                Button { index: 1, down: true },
                Button { index: 5, down: true },
                Move { dx: -2, dy: 300 },
                Wheel { delta: -1 },
                HWheel { delta: 2 },
            ]
        );
        // Moves repeat, buttons do not
        assert_eq!(d.decode(&[0x02, 0b1_0001, 1, 0, 0, 0, 0, 0]), vec![Move { dx: 1, dy: 0 }]);
        assert_eq!(d.decode(&[0x02, 0b0_0001, 0, 0, 0, 0, 0, 0]), vec![Button { index: 5, down: false }]);
        // Unknown report ID
        assert!(d.decode(&[0x09, 0, 0, 0, 0, 0, 0, 0]).is_empty());
        // The ID given apart, as on LE
        assert_eq!(
            d.decode_with_id(2, &[0, 0, 0, 0, 0, 1, 0]),
            vec![Button { index: 1, down: false }, Wheel { delta: 1 }]
        );
    }

    #[test]
    fn boot_mouse_with_and_without_wheel() {
        let mut d = Decoder::boot_mouse();
        assert_eq!(d.decode(&[0x04, 0x05, 0xFB]), vec![Button { index: 3, down: true }, Move { dx: 5, dy: -5 }]);
        assert_eq!(d.decode(&[0x04, 0, 0, 0x01]), vec![Wheel { delta: 1 }]);
        // The ID is ignored without report IDs
        assert_eq!(d.decode_with_id(7, &[0, 0, 0]), vec![Button { index: 3, down: false }]);
    }

    #[test]
    fn absolute_pointer_ignored() {
        // X/Y absolute (Data,Var,Abs)
        let mut d = Decoder::from_descriptor(&[
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02,
        ])
        .unwrap();
        assert!(d.decode(&[0x10, 0x00, 0x20, 0x00]).is_empty());
    }

    #[test]
    fn combined_device_reports() {
        let mut d = Decoder::from_descriptor(COMBO).unwrap();
        assert_eq!(d.decode(&[1, 0, 0, 0x04, 0, 0, 0, 0, 0]), vec![key(0x04, true)]);
        // Volume Up on the consumer report
        assert_eq!(d.decode(&[2, 0xE9, 0x00]), vec![Consumer { usage: 0xE9, down: true }]);
        // The mouse report leaves keys alone
        assert_eq!(d.decode(&[3, 0b010, 3, 0, 0]), vec![Button { index: 2, down: true }, Move { dx: 3, dy: 0 }]);
        assert_eq!(d.decode(&[2, 0, 0]), vec![Consumer { usage: 0xE9, down: false }]);
        // NKRO: A (bit 0) and Z (0x1D, bit 25)
        let mut nkro = [0u8; 15];
        nkro[0] = 4;
        nkro[1] = 0x01;
        nkro[1 + 3] = 0x02;
        assert_eq!(d.decode(&nkro), vec![key(0x1D, true)]);
        // A leaves the boot report but the NKRO report still holds it
        assert!(d.decode(&[1, 0, 0, 0, 0, 0, 0, 0, 0]).is_empty());
        let mut none = [0u8; 15];
        none[0] = 4;
        assert_eq!(d.decode(&none), vec![key(0x04, false), key(0x1D, false)]);
        assert_eq!(d.release_all(), vec![Button { index: 2, down: false }]);
    }

    #[test]
    fn release_all_lifts_everything_once() {
        let mut d = Decoder::from_descriptor(COMBO).unwrap();
        d.decode(&[1, 0x81, 0, 0x04, 0x05, 0, 0, 0, 0]); // LCtrl + RGUI + A + B
        d.decode(&[2, 0xCD, 0x00]); // Play/Pause
        d.decode(&[3, 0b111, 0, 0, 0]);
        let mut nkro = [0u8; 15];
        nkro[0] = 4;
        nkro[1] = 0x01; // A again, from a second field
        d.decode(&nkro);
        let ups = d.release_all();
        let expected = [
            key(0x04, false),
            key(0x05, false),
            Consumer { usage: 0xCD, down: false },
            Button { index: 1, down: false },
            Button { index: 2, down: false },
            Button { index: 3, down: false },
            key(0xE0, false),
            key(0xE7, false),
        ];
        assert_eq!(ups, expected);
        assert!(
            ups.iter().all(|e| !matches!(
                e,
                Key { down: true, .. } | Consumer { down: true, .. } | Button { down: true, .. }
            ))
        );
        assert!(!d.any_held());
        assert!(d.release_all().is_empty());
        // The same reports after release press the keys anew
        assert_eq!(d.decode(&[1, 0, 0, 0x04, 0, 0, 0, 0, 0]), vec![key(0x04, true)]);
        // A key that stays down in a later report is not lifted twice
        d.release_all();
        assert!(d.decode(&[1, 0, 0, 0, 0, 0, 0, 0, 0]).is_empty());
        assert!(d.release_all().is_empty());
    }

    #[test]
    fn duplicate_array_entries() {
        let mut d = Decoder::boot_keyboard();
        assert_eq!(d.decode(&[0, 0, 0x04, 0x04, 0, 0, 0, 0]), vec![key(0x04, true)]);
        assert_eq!(d.decode(&[0, 0, 0x04, 0, 0, 0, 0, 0]), Vec::new());
        assert_eq!(d.release_all(), vec![key(0x04, false)]);
    }

    // Small deterministic generator, so failures repeat
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) as u8
        }
    }

    #[test]
    fn random_descriptors_and_reports_never_panic() {
        let mut rng = Lcg(0x5EED);
        let mut parsed = 0;
        for round in 0..3000 {
            let len = (rng.next() as usize) % 200;
            // Mostly item prefixes that matter, so many parse
            let picks =
                [0x05, 0x09, 0x15, 0x19, 0x25, 0x29, 0x75, 0x81, 0x85, 0x95, 0xA1, 0xA4, 0xB4, 0xC0, 0x0B, 0x27];
            let bytes: Vec<u8> =
                (0..len)
                    .map(|i| {
                        if i % 2 == 0 && round % 3 != 0 { picks[rng.next() as usize % picks.len()] } else { rng.next() }
                    })
                    .collect();
            let Ok(desc) = ReportDescriptor::parse(&bytes) else { continue };
            parsed += 1;
            let mut d = Decoder::new(&desc);
            for _ in 0..20 {
                let n = (rng.next() as usize) % 40;
                let report: Vec<u8> = (0..n).map(|_| rng.next()).collect();
                d.decode(&report);
                d.decode_with_id(rng.next(), &report);
            }
            d.release_all();
            assert!(!d.any_held());
        }
        assert!(parsed > 100, "only {parsed} parsed");
    }

    #[test]
    fn widest_values_decode() {
        // 32-bit signed relative X
        let mut d = Decoder::from_descriptor(&[
            0x05, 0x01, 0x09, 0x30, 0x17, 0x00, 0x00, 0x00, 0x80, 0x27, 0xFF, 0xFF, 0xFF, 0x7F, 0x75, 0x20, 0x95, 0x01,
            0x81, 0x06,
        ])
        .unwrap();
        assert_eq!(d.decode(&[0x00, 0x00, 0x00, 0x80]), vec![Move { dx: i32::MIN, dy: 0 }]);
        // Wider fields are padding only
        let mut d = Decoder::from_descriptor(&[0x05, 0x01, 0x09, 0x30, 0x75, 0x40, 0x95, 0x01, 0x81, 0x06]).unwrap();
        assert!(d.decode(&[0xFF; 8]).is_empty());
    }
}
