//! QCY earbud control protocol (first target: the MeloBuds N70 / HT18), the
//! logic that needs neither BTstack nor Windows. Everything here is plain
//! data in, data out, so that it can be unit-tested without an adapter.
//!
//! It is a Rust port of the protocol layer of OpenQCY Desktop (MIT,
//! github.com/GiorgioRafael/OpenQCY-Desktop: `Protocol/` and the state
//! handling of `Device/QcyDeviceClient.cs`). The test vectors are the
//! redacted hardware transcripts of that project's `docs/protocol-research.md`
//! (firmware 3.0.13).
//!
//! - [`uuids`]: the GATT service and characteristic numbers, company id
//! - [`packet`]: the `FF <body length> <opcode> <parameter length> <params>`
//!   framing
//! - [`advertisement`]: the manufacturer data the earbuds advertise (battery,
//!   vendor id, control address)
//! - [`commands`]: typed commands and the values they carry (ANC scenes,
//!   wearing detection, the ten-band equalizer, key functions)
//! - [`state`]: the device state, updated from responses and notifications
//! - [`write`]: the allowlist / blocklist and the confirmed write flow (only
//!   game mode in this version)
//!
//! Safety rule, as in OpenQCY: no command is guessed. A command is only built
//! here when a captured transcript or public interoperability evidence backs
//! it, and a write is only considered applied when the device confirms it.

#![forbid(unsafe_code)]

pub mod advertisement;
pub mod commands;
pub mod packet;
pub mod state;
pub mod uuids;
pub mod write;

#[cfg(test)]
pub(crate) mod test_util {
    /// "FF03FE0117" -> bytes
    pub fn hex(text: &str) -> Vec<u8> {
        let text: String = text.split_whitespace().collect();
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }
}
