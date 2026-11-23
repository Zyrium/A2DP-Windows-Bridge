//! HID report descriptor parser (input reports, and the keyboard lights'
//! output report)
//!
//! Handles short items (main, global, local), Push/Pop and 4-byte extended
//! usages, and skips long items. Of the Output items only the LED page's
//! Num Lock, Caps Lock and Scroll Lock are kept ([`LedOutput`]); Feature
//! items only end the local state. The descriptor comes from the remote
//! device, so fields, report length, Push depth and collection depth are
//! all capped.

use std::collections::BTreeMap;

use crate::leds::LedOutput;

/// Most input fields in one descriptor
pub const MAX_FIELDS: usize = 1024;
/// Longest input report, in bits, without the report ID byte
pub const MAX_REPORT_BITS: u32 = 8 * 1024;
/// Deepest Push nesting
pub const MAX_PUSH_DEPTH: usize = 16;
/// Deepest collection nesting
pub const MAX_COLLECTION_DEPTH: usize = 32;
/// Most Usage items and ranges before one main item
pub const MAX_LOCAL_USAGES: usize = 1024;

/// Widest value decoded; wider data fields are kept only as padding
pub const MAX_VALUE_BITS: u32 = 32;

pub const PAGE_GENERIC_DESKTOP: u16 = 0x01;
pub const PAGE_KEYBOARD: u16 = 0x07;
pub const PAGE_LED: u16 = 0x08;
pub const PAGE_BUTTON: u16 = 0x09;
pub const PAGE_CONSUMER: u16 = 0x0C;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    #[error("item at byte {0} runs past the end of the descriptor")]
    Truncated(usize),
    #[error("more than {MAX_FIELDS} input fields")]
    TooManyFields,
    #[error("input report {0} is longer than {MAX_REPORT_BITS} bits")]
    ReportTooLong(u8),
    #[error("Push nested deeper than {MAX_PUSH_DEPTH}")]
    PushTooDeep,
    #[error("Pop without a Push")]
    PopWithoutPush,
    #[error("collections nested deeper than {MAX_COLLECTION_DEPTH}")]
    CollectionsTooDeep,
    #[error("report ID {0} is not 1 to 255")]
    BadReportId(u32),
    #[error("more than {MAX_LOCAL_USAGES} usages before one main item")]
    TooManyUsages,
}

/// Usages of a field, in order. A usage is 32 bits: page << 16 | ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSpec {
    Single(u32),
    /// Usage Minimum to Usage Maximum, inclusive
    Range {
        min: u32,
        max: u32,
    },
}

impl UsageSpec {
    fn len(self) -> u64 {
        match self {
            UsageSpec::Single(_) => 1,
            UsageSpec::Range { min, max } => u64::from(max - min) + 1,
        }
    }

    fn nth(self, n: u64) -> u32 {
        match self {
            UsageSpec::Single(u) => u,
            UsageSpec::Range { min, .. } => min + n as u32,
        }
    }
}

/// Main item data flags of an Input item that matter for decoding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FieldFlags {
    /// Padding or fixed data
    pub constant: bool,
    /// One value per usage; otherwise an array of usage indices
    pub variable: bool,
    /// Changes since the last report; otherwise absolute values
    pub relative: bool,
}

/// One Input item: `count` values of `bit_size` bits each
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Bit offset in the report, after the report ID byte
    pub bit_offset: u32,
    pub bit_size: u32,
    pub count: u32,
    /// Usage Page in effect at the Input item
    pub usage_page: u16,
    pub usages: Vec<UsageSpec>,
    pub logical_min: i64,
    pub logical_max: i64,
    pub flags: FieldFlags,
}

impl Field {
    fn usage_total(&self) -> u64 {
        self.usages.iter().map(|u| u.len()).sum()
    }

    fn usage_at(&self, mut n: u64) -> Option<u32> {
        for spec in &self.usages {
            let len = spec.len();
            if n < len {
                return Some(spec.nth(n));
            }
            n -= len;
        }
        None
    }

    /// Usage of value `index` of a variable field: past the list, the
    /// last usage repeats (HID 1.11, 6.2.2.8).
    pub fn variable_usage(&self, index: u32) -> Option<u32> {
        let total = self.usage_total();
        if total == 0 {
            return None;
        }
        self.usage_at(u64::from(index).min(total - 1))
    }

    /// Usage that value `value` of an array field selects; None when the
    /// value is outside the logical range or the usage list.
    pub fn array_usage(&self, value: i64) -> Option<u32> {
        if value < self.logical_min || value > self.logical_max {
            return None;
        }
        self.usage_at((value - self.logical_min) as u64)
    }

    /// Whether values are signed (a negative logical minimum)
    pub fn signed(&self) -> bool {
        self.logical_min < 0
    }
}

/// The input reports of a device
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportDescriptor {
    /// Fields per report ID; ID 0 when the descriptor has no Report ID items
    pub reports: BTreeMap<u8, Vec<Field>>,
    /// Each input report starts with its ID byte
    pub uses_report_ids: bool,
    /// Data bits per input report, without the ID byte
    pub report_bits: BTreeMap<u8, u32>,
    /// The output report with the keyboard lights; None: the device has none
    pub led_output: Option<LedOutput>,
}

/// LED page usages of Num Lock, Caps Lock and Scroll Lock, in the order of
/// [`LedOutput::fields`]
const LED_USAGES: [u32; 3] = [0x8_0001, 0x8_0002, 0x8_0003];

/// The output reports seen so far: their length, and where each light is
#[derive(Default)]
struct Outputs {
    bits: BTreeMap<u8, u64>,
    leds: BTreeMap<u8, [Option<(u32, u32)>; 3]>,
}

impl Outputs {
    fn add(&mut self, globals: &Globals, locals: &Locals, data: u32) {
        let id = globals.report_id;
        let offset = self.bits.get(&id).copied().unwrap_or(0);
        let size = u64::from(globals.report_size);
        let count = u64::from(globals.report_count);
        let end = offset.saturating_add(size * count);
        self.bits.insert(id, end);
        let constant = data & 1 != 0;
        let variable = data & 2 != 0;
        if constant || !variable || size == 0 || end > u64::from(MAX_REPORT_BITS) {
            return;
        }
        for (slot, &usage) in LED_USAGES.iter().enumerate() {
            let Some(index) = usage_index(&locals.usages, usage).filter(|&i| i < count) else { continue };
            let field = &mut self.leds.entry(id).or_default()[slot];
            if field.is_none() {
                *field = Some(((offset + index * size) as u32, size as u32));
            }
        }
    }

    /// The first output report (lowest ID) with a light in it
    fn led_output(&self) -> Option<LedOutput> {
        self.leds.iter().find_map(|(&id, fields)| {
            let bits = self.bits.get(&id).copied().filter(|&b| b <= u64::from(MAX_REPORT_BITS))?;
            Some(LedOutput { report_id: id, len: bits.div_ceil(8) as usize, fields: *fields })
        })
    }
}

/// Which value of a variable field carries `usage`
fn usage_index(usages: &[UsageSpec], usage: u32) -> Option<u64> {
    let mut n = 0u64;
    for spec in usages {
        match *spec {
            UsageSpec::Single(u) if u == usage => return Some(n),
            UsageSpec::Range { min, max } if (min..=max).contains(&usage) => return Some(n + u64::from(usage - min)),
            _ => {}
        }
        n += spec.len();
    }
    None
}

#[derive(Debug, Clone, Copy, Default)]
struct Globals {
    usage_page: u16,
    logical_min: i64,
    logical_max: i64,
    // Logical Maximum read as unsigned, for descriptors that write 255 as 0xFF
    logical_max_unsigned: i64,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

#[derive(Debug, Default)]
struct Locals {
    usages: Vec<UsageSpec>,
    usage_min: Option<u32>,
    usage_max: Option<u32>,
}

impl Locals {
    fn push(&mut self, spec: UsageSpec) -> Result<(), DescriptorError> {
        if self.usages.len() >= MAX_LOCAL_USAGES {
            return Err(DescriptorError::TooManyUsages);
        }
        self.usages.push(spec);
        Ok(())
    }

    fn close_range(&mut self) -> Result<(), DescriptorError> {
        if let (Some(min), Some(max)) = (self.usage_min, self.usage_max) {
            self.usage_min = None;
            self.usage_max = None;
            // A reversed range selects nothing
            if min <= max {
                self.push(UsageSpec::Range { min, max })?;
            }
        }
        Ok(())
    }
}

const TYPE_MAIN: u8 = 0;
const TYPE_GLOBAL: u8 = 1;
const TYPE_LOCAL: u8 = 2;

const MAIN_INPUT: u8 = 0x8;
const MAIN_OUTPUT: u8 = 0x9;
const MAIN_COLLECTION: u8 = 0xA;
const MAIN_END_COLLECTION: u8 = 0xC;

const LONG_ITEM: u8 = 0xFE;

fn sign_extend(value: u32, size: usize) -> i64 {
    match size {
        1 => i64::from(value as u8 as i8),
        2 => i64::from(value as u16 as i16),
        4 => i64::from(value as i32),
        _ => 0,
    }
}

impl ReportDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, DescriptorError> {
        let mut out = ReportDescriptor::default();
        let mut globals = Globals::default();
        let mut stack: Vec<Globals> = Vec::new();
        let mut locals = Locals::default();
        let mut depth = 0usize;
        let mut field_count = 0usize;
        let mut outputs = Outputs::default();
        let mut pos = 0usize;

        while pos < bytes.len() {
            let prefix = bytes[pos];
            if prefix == LONG_ITEM {
                // bDataSize, bLongItemTag, data: no long items are defined
                let size = *bytes.get(pos + 1).ok_or(DescriptorError::Truncated(pos))? as usize;
                let end = pos + 3 + size;
                if end > bytes.len() {
                    return Err(DescriptorError::Truncated(pos));
                }
                pos = end;
                continue;
            }
            let size = match prefix & 3 {
                3 => 4,
                n => n as usize,
            };
            let data_bytes = bytes.get(pos + 1..pos + 1 + size).ok_or(DescriptorError::Truncated(pos))?;
            let mut le = [0u8; 4];
            le[..size].copy_from_slice(data_bytes);
            let data = u32::from_le_bytes(le);
            let item_type = (prefix >> 2) & 3;
            let tag = prefix >> 4;
            pos += 1 + size;

            match item_type {
                TYPE_MAIN => {
                    match tag {
                        MAIN_INPUT => {
                            out.add_input(&globals, &locals, data, &mut field_count)?;
                        }
                        MAIN_OUTPUT => outputs.add(&globals, &locals, data),
                        MAIN_COLLECTION => {
                            depth += 1;
                            if depth > MAX_COLLECTION_DEPTH {
                                return Err(DescriptorError::CollectionsTooDeep);
                            }
                        }
                        // Unbalanced End Collections are harmless here
                        MAIN_END_COLLECTION => depth = depth.saturating_sub(1),
                        // Feature and reserved tags carry no input
                        _ => {}
                    }
                    locals = Locals::default();
                }
                TYPE_GLOBAL => match tag {
                    0x0 => globals.usage_page = data as u16,
                    0x1 => globals.logical_min = sign_extend(data, size),
                    0x2 => {
                        globals.logical_max = sign_extend(data, size);
                        globals.logical_max_unsigned = i64::from(data);
                    }
                    0x7 => globals.report_size = data,
                    0x8 => {
                        if data == 0 || data > 255 {
                            return Err(DescriptorError::BadReportId(data));
                        }
                        globals.report_id = data as u8;
                        out.uses_report_ids = true;
                    }
                    0x9 => globals.report_count = data,
                    0xA => {
                        if stack.len() >= MAX_PUSH_DEPTH {
                            return Err(DescriptorError::PushTooDeep);
                        }
                        stack.push(globals);
                    }
                    0xB => globals = stack.pop().ok_or(DescriptorError::PopWithoutPush)?,
                    // Physical range, unit and exponent do not change decoding
                    _ => {}
                },
                TYPE_LOCAL => {
                    // A 4-byte usage carries its own page
                    let usage = if size == 4 { data } else { u32::from(globals.usage_page) << 16 | (data & 0xFFFF) };
                    match tag {
                        0x0 => locals.push(UsageSpec::Single(usage))?,
                        0x1 => {
                            locals.usage_min = Some(usage);
                            locals.close_range()?;
                        }
                        0x2 => {
                            locals.usage_max = Some(usage);
                            locals.close_range()?;
                        }
                        // Designators, strings and delimiters are not needed
                        _ => {}
                    }
                }
                // Reserved item type
                _ => {}
            }
        }
        out.led_output = outputs.led_output();
        Ok(out)
    }

    fn add_input(
        &mut self,
        globals: &Globals,
        locals: &Locals,
        data: u32,
        field_count: &mut usize,
    ) -> Result<(), DescriptorError> {
        let id = globals.report_id;
        let offset = self.report_bits.get(&id).copied().unwrap_or(0);
        let bits = u64::from(globals.report_size) * u64::from(globals.report_count);
        let end = u64::from(offset) + bits;
        if end > u64::from(MAX_REPORT_BITS) {
            return Err(DescriptorError::ReportTooLong(id));
        }
        self.report_bits.insert(id, end as u32);
        if bits == 0 {
            return Ok(());
        }
        *field_count += 1;
        if *field_count > MAX_FIELDS {
            return Err(DescriptorError::TooManyFields);
        }
        let mut logical_max = globals.logical_max;
        if globals.logical_min >= 0 && logical_max < globals.logical_min {
            logical_max = globals.logical_max_unsigned;
        }
        let flags = FieldFlags { constant: data & 1 != 0, variable: data & 2 != 0, relative: data & 4 != 0 };
        self.reports.entry(id).or_default().push(Field {
            bit_offset: offset,
            bit_size: globals.report_size,
            count: globals.report_count,
            usage_page: globals.usage_page,
            usages: locals.usages.clone(),
            logical_min: globals.logical_min,
            logical_max,
            flags,
        });
        Ok(())
    }

    /// Input report length in bytes, without the ID byte
    pub fn report_len(&self, id: u8) -> Option<usize> {
        self.report_bits.get(&id).map(|bits| bits.div_ceil(8) as usize)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// HID 1.11 Appendix B.1 boot keyboard, with the LED output report
    pub const KEYBOARD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, // Generic Desktop, Keyboard, Application
        0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81,
        0x02, // modifiers
        0x95, 0x01, 0x75, 0x08, 0x81, 0x01, // reserved byte
        0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, // LEDs
        0x95, 0x01, 0x75, 0x03, 0x91, 0x01, // LED padding
        0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, // keys
        0xC0,
    ];

    /// Report ID 2: 5 buttons, 16-bit X/Y, wheel, AC Pan
    pub const MOUSE: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xA1, 0x00, // Mouse, ID 2, Pointer
        0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, // buttons
        0x95, 0x01, 0x75, 0x03, 0x81, 0x01, // padding
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81,
        0x06, // X, Y
        0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, // wheel
        0x05, 0x0C, 0x0A, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06, // AC Pan
        0xC0, 0xC0,
    ];

    /// Keyboard (ID 1), consumer array (ID 2), mouse (ID 3), NKRO bitmap (ID 4)
    pub const COMBO: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, // keyboard
        0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01,
        0x75, 0x08, 0x81, 0x01, //
        0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0xFF, 0x19, 0x00, 0x2A, 0xFF, 0x00, 0x81, 0x00, //
        0xC0, //
        0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, // consumer
        0x15, 0x00, 0x26, 0xFF, 0x03, 0x19, 0x00, 0x2A, 0xFF, 0x03, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00, //
        0xC0, //
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x03, 0x09, 0x01, 0xA1, 0x00, // mouse
        0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, //
        0x95, 0x01, 0x75, 0x05, 0x81, 0x03, //
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03, 0x81,
        0x06, //
        0xC0, 0xC0, //
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x04, // NKRO keyboard, 4-byte usages
        0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x70, 0x1B, 0x04, 0x00, 0x07, 0x00, 0x2B, 0x73, 0x00, 0x07, 0x00,
        0x81, 0x02, //
        0xC0,
    ];

    #[test]
    fn boot_keyboard_layout() {
        let d = ReportDescriptor::parse(KEYBOARD).unwrap();
        assert!(!d.uses_report_ids);
        let f = &d.reports[&0];
        assert_eq!(f.len(), 3);
        assert_eq!((f[0].bit_offset, f[0].bit_size, f[0].count), (0, 1, 8));
        assert!(f[0].flags.variable && !f[0].flags.constant);
        assert_eq!(f[0].usages, vec![UsageSpec::Range { min: 0x7_00E0, max: 0x7_00E7 }]);
        assert!(f[1].flags.constant);
        assert_eq!((f[2].bit_offset, f[2].bit_size, f[2].count), (16, 8, 6));
        assert!(!f[2].flags.variable);
        assert_eq!((f[2].logical_min, f[2].logical_max), (0, 0x65));
        assert_eq!(f[2].array_usage(4), Some(0x7_0004));
        assert_eq!(f[2].array_usage(0x66), None);
        // The LED output report adds no input bits
        assert_eq!(d.report_len(0), Some(8));
        assert_eq!(d.led_output, Some(LedOutput::boot()));
    }

    #[test]
    fn led_output_with_report_id_and_single_usages() {
        let d = ReportDescriptor::parse(&[
            0x85, 0x05, 0x05, 0x08, // ID 5, LED page
            0x75, 0x04, 0x95, 0x01, 0x91, 0x01, // 4 bits of padding first
            0x75, 0x01, 0x95, 0x03, 0x09, 0x03, 0x09, 0x01, 0x09, 0x02, 0x91, 0x02, // Scroll, Num, Caps
            0x95, 0x01, 0x91, 0x01, // padding
            0x85, 0x06, 0x19, 0x01, 0x29, 0x03, 0x95, 0x03, 0x91, 0x02, // a second LED report: not used
        ])
        .unwrap();
        let leds = d.led_output.unwrap();
        assert_eq!(leds.report_id, 5);
        assert_eq!(leds.len, 1);
        assert_eq!(leds.fields, [Some((5, 1)), Some((6, 1)), Some((4, 1))]);
        assert_eq!(leds.report(crate::leds::CAPS_LOCK | crate::leds::SCROLL_LOCK), vec![0x50]);
        // Output items add no input report
        assert!(d.reports.is_empty());
    }

    #[test]
    fn led_output_ignores_constant_array_and_other_pages() {
        // Constant, an array, the keyboard page, and lights past the count
        let d = ReportDescriptor::parse(&[
            0x05, 0x08, 0x75, 0x01, 0x95, 0x03, 0x19, 0x01, 0x29, 0x03, 0x91, 0x03, // constant
            0x75, 0x08, 0x95, 0x01, 0x19, 0x01, 0x29, 0x03, 0x91, 0x00, // array
            0x05, 0x07, 0x75, 0x01, 0x95, 0x03, 0x19, 0x01, 0x29, 0x03, 0x91, 0x02, // keyboard page
            0x05, 0x08, 0x95, 0x01, 0x19, 0x02, 0x29, 0x03, 0x91, 0x02, // only Caps Lock fits the count
        ])
        .unwrap();
        let leds = d.led_output.unwrap();
        assert_eq!(leds.fields, [None, Some((14, 1)), None]);
        assert_eq!(leds.len, 2);
        assert_eq!(leds.report(0x07), vec![0x00, 0x40]);
        assert_eq!(ReportDescriptor::parse(COMBO).unwrap().led_output, None);
        assert_eq!(ReportDescriptor::parse(MOUSE).unwrap().led_output, None);
    }

    #[test]
    fn huge_output_report_has_no_lights() {
        let d = ReportDescriptor::parse(&[
            0x05, 0x08, 0x75, 0x01, 0x95, 0x03, 0x19, 0x01, 0x29, 0x03, 0x91, 0x02, // lights
            0x75, 0x08, 0x97, 0xFF, 0xFF, 0xFF, 0xFF, 0x91, 0x01, // then 4 G of padding
        ])
        .unwrap();
        assert_eq!(d.led_output, None);
    }

    #[test]
    fn mouse_with_report_id() {
        let d = ReportDescriptor::parse(MOUSE).unwrap();
        assert!(d.uses_report_ids);
        assert_eq!(d.reports.keys().copied().collect::<Vec<_>>(), vec![2]);
        let f = &d.reports[&2];
        assert_eq!(f.len(), 5);
        let xy = &f[2];
        assert_eq!((xy.bit_offset, xy.bit_size, xy.count), (8, 16, 2));
        assert_eq!((xy.logical_min, xy.logical_max), (-32767, 32767));
        assert!(xy.flags.relative);
        assert_eq!(xy.variable_usage(1), Some(0x1_0031));
        assert_eq!(f[3].logical_min, -127);
        assert_eq!(f[4].variable_usage(0), Some(0xC_0238));
        assert_eq!(f[4].usage_page, PAGE_CONSUMER);
        assert_eq!(d.report_len(2), Some(7));
    }

    #[test]
    fn combined_device() {
        let d = ReportDescriptor::parse(COMBO).unwrap();
        assert_eq!(d.reports.keys().copied().collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        // 0xFF in one byte as Logical Maximum reads as 255 with a zero minimum
        assert_eq!(d.reports[&1][2].logical_max, 255);
        assert_eq!(d.reports[&2][0].logical_max, 0x3FF);
        assert_eq!(d.report_len(3), Some(4));
        let nkro = &d.reports[&4][0];
        assert_eq!(nkro.usages, vec![UsageSpec::Range { min: 0x7_0004, max: 0x7_0073 }]);
        assert_eq!(nkro.variable_usage(0), Some(0x7_0004));
        assert_eq!(d.report_len(4), Some(14));
    }

    #[test]
    fn variable_usage_repeats_last() {
        let d = ReportDescriptor::parse(&[0x05, 0x01, 0x09, 0x30, 0x75, 0x08, 0x95, 0x03, 0x81, 0x06]).unwrap();
        assert_eq!(d.reports[&0][0].variable_usage(2), Some(0x1_0030));
    }

    #[test]
    fn push_pop_restore_globals() {
        let d = ReportDescriptor::parse(&[
            0x05, 0x07, 0x75, 0x01, 0x95, 0x08, 0xA4, // page 7, 8 x 1 bit, Push
            0x05, 0x0C, 0x75, 0x10, 0x95, 0x01, 0x09, 0xE9, 0x81, 0x02, // consumer 16-bit
            0xB4, 0x19, 0xE0, 0x29, 0xE7, 0x81, 0x02, // Pop: keyboard again
        ])
        .unwrap();
        let f = &d.reports[&0];
        assert_eq!((f[0].usage_page, f[0].bit_size), (PAGE_CONSUMER, 16));
        assert_eq!((f[1].usage_page, f[1].bit_size, f[1].count, f[1].bit_offset), (PAGE_KEYBOARD, 1, 8, 16));
    }

    #[test]
    fn long_items_are_skipped() {
        let d = ReportDescriptor::parse(&[0xFE, 0x02, 0x10, 0xAA, 0xBB, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02]).unwrap();
        assert_eq!(d.report_len(0), Some(1));
        assert_eq!(ReportDescriptor::parse(&[0xFE, 0x05, 0x10, 0xAA]), Err(DescriptorError::Truncated(0)));
        assert_eq!(ReportDescriptor::parse(&[0xFE]), Err(DescriptorError::Truncated(0)));
    }

    #[test]
    fn truncated_items() {
        assert_eq!(ReportDescriptor::parse(&[0x05, 0x01, 0x26, 0xFF]), Err(DescriptorError::Truncated(2)));
        assert_eq!(ReportDescriptor::parse(&[0x27, 0x01, 0x02]), Err(DescriptorError::Truncated(0)));
        assert_eq!(ReportDescriptor::parse(&[]), Ok(ReportDescriptor::default()));
    }

    #[test]
    fn huge_report_count() {
        let r = ReportDescriptor::parse(&[0x75, 0x08, 0x97, 0xFF, 0xFF, 0xFF, 0xFF, 0x81, 0x02]);
        assert_eq!(r, Err(DescriptorError::ReportTooLong(0)));
        let r = ReportDescriptor::parse(&[
            0x85, 0x07, 0x77, 0xFF, 0xFF, 0xFF, 0xFF, 0x97, 0xFF, 0xFF, 0xFF, 0xFF, 0x81, 0x02,
        ]);
        assert_eq!(r, Err(DescriptorError::ReportTooLong(7)));
        // Many small items add up to the same limit
        let mut many = vec![0x75, 0x08, 0x96, 0x00, 0x01];
        for _ in 0..5 {
            many.extend_from_slice(&[0x81, 0x02]);
        }
        assert_eq!(ReportDescriptor::parse(&many), Err(DescriptorError::ReportTooLong(0)));
    }

    #[test]
    fn too_many_fields() {
        let mut d = vec![0x75, 0x01, 0x95, 0x01];
        for _ in 0..=MAX_FIELDS {
            d.extend_from_slice(&[0x81, 0x02]);
        }
        assert_eq!(ReportDescriptor::parse(&d), Err(DescriptorError::TooManyFields));
    }

    #[test]
    fn zero_report_size_adds_nothing() {
        let d = ReportDescriptor::parse(&[0x05, 0x07, 0x75, 0x00, 0x95, 0x08, 0x81, 0x02]).unwrap();
        assert!(d.reports.is_empty());
        assert_eq!(d.report_len(0), Some(0));
    }

    #[test]
    fn deep_push_and_stray_pop() {
        let push = vec![0xA4; MAX_PUSH_DEPTH + 1];
        assert_eq!(ReportDescriptor::parse(&push), Err(DescriptorError::PushTooDeep));
        assert!(ReportDescriptor::parse(&[0xA4; MAX_PUSH_DEPTH]).is_ok());
        assert_eq!(ReportDescriptor::parse(&[0xB4]), Err(DescriptorError::PopWithoutPush));
    }

    #[test]
    fn deep_collections() {
        let mut d = Vec::new();
        for _ in 0..=MAX_COLLECTION_DEPTH {
            d.extend_from_slice(&[0xA1, 0x00]);
        }
        assert_eq!(ReportDescriptor::parse(&d), Err(DescriptorError::CollectionsTooDeep));
        // Collections closed in turn never get deep
        let flat: Vec<u8> = (0..1000).flat_map(|_| [0xA1, 0x00, 0xC0]).collect();
        assert!(ReportDescriptor::parse(&flat).is_ok());
    }

    #[test]
    fn bad_report_ids() {
        assert_eq!(ReportDescriptor::parse(&[0x85, 0x00]), Err(DescriptorError::BadReportId(0)));
        assert_eq!(ReportDescriptor::parse(&[0x86, 0x00, 0x01]), Err(DescriptorError::BadReportId(256)));
    }

    #[test]
    fn too_many_usages() {
        let d: Vec<u8> = (0..=MAX_LOCAL_USAGES).flat_map(|_| [0x09, 0x01]).collect();
        assert_eq!(ReportDescriptor::parse(&d), Err(DescriptorError::TooManyUsages));
        // Usages end with each main item
        let ok: Vec<u8> = (0..2000).flat_map(|_| [0x09, 0x01, 0x81, 0x02]).collect();
        assert!(ReportDescriptor::parse(&ok).is_ok());
    }

    #[test]
    fn reversed_and_full_usage_ranges() {
        let d = ReportDescriptor::parse(&[0x75, 0x01, 0x95, 0x01, 0x19, 0x10, 0x29, 0x01, 0x81, 0x02]).unwrap();
        assert!(d.reports[&0][0].usages.is_empty());
        // A range over every 32-bit usage must not overflow
        let d = ReportDescriptor::parse(&[
            0x75, 0x08, 0x95, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x1B, 0, 0, 0, 0, 0x2B, 0xFF, 0xFF, 0xFF, 0xFF, 0x81,
            0x00,
        ])
        .unwrap();
        let f = &d.reports[&0][0];
        assert_eq!(f.array_usage(255), Some(255));
        assert_eq!(f.variable_usage(u32::MAX), Some(u32::MAX));
    }
}
