//! HID host input: what a Bluetooth keyboard or mouse sends, typed and
//! pointed in Windows.
//!
//! - [`descriptor`]: the device's report descriptor parsed into the layout of
//!   its input reports. The descriptor comes from the remote device, so the
//!   parser bounds everything and never panics.
//! - [`decode`]: input reports turned into key, button, move and wheel
//!   events, as changes against the previous reports.
//!   [`Decoder::release_all`] lifts everything still held.
//! - [`keymap`]: HID usages to Windows scan codes and virtual keys.
//! - [`inject`]: the events sent to Windows (`win_sys::input_inject`).
//! - [`leds`]: Num Lock, Caps Lock and Scroll Lock as the keyboard's LED
//!   output report.
//! - [`sink`]: where the engine's input goes (this process, counted only,
//!   or the input helper) and the counters.
//!
//! Nothing here logs key codes or report contents: this is keyboard input.

#![forbid(unsafe_code)]

pub mod decode;
pub mod descriptor;
pub mod inject;
pub mod keymap;
pub mod leds;
pub mod sink;

pub use decode::{Decoder, InputEvent, InputKind};
pub use descriptor::{DescriptorError, ReportDescriptor};
pub use leds::LedOutput;
