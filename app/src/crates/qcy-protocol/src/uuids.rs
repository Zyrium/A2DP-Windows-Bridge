//! GATT numbers of the QCY control channel. All of them are 16-bit UUIDs in
//! the Bluetooth base (`0000xxxx-0000-1000-8000-00805f9b34fb`), so BTstack's
//! `uuid16` fields match them directly.

/// Main service `0000A001-...`
pub const MAIN_SERVICE: u16 = 0xA001;
/// Framed commands (write)
pub const COMMAND: u16 = 0x1001;
/// Framed responses (read, notify)
pub const NOTIFICATION: u16 = 0x1002;
/// Firmware version (read)
pub const FIRMWARE_VERSION: u16 = 0x0007;
/// Battery state (read, notify)
pub const BATTERY: u16 = 0x0008;
/// Legacy equalizer preset (read, write); absent on N70 firmware 3.0.13
pub const EQUALIZER: u16 = 0x000B;
/// Touch mappings (read, write)
pub const KEY_FUNCTIONS: u16 = 0x000D;

/// QCY's company id in the manufacturer-specific advertising data (AD type
/// 0xFF), sent little-endian on air
pub const COMPANY_ID: u16 = 0x521C;
/// N70 vendor ids (the first two bytes of the manufacturer data after the
/// company id, big-endian): black and alternate colour
pub const N70_BLACK_VENDOR_ID: u16 = 23872;
pub const N70_ALTERNATE_VENDOR_ID: u16 = 23877;

/// A 16-bit UUID in the Bluetooth base as 128 bits, most significant byte
/// first (BTstack's `uuid128` order)
pub fn base_uuid128(uuid16: u16) -> [u8; 16] {
    let [hi, lo] = uuid16.to_be_bytes();
    [0, 0, hi, lo, 0, 0, 0x10, 0, 0x80, 0, 0, 0x80, 0x5F, 0x9B, 0x34, 0xFB]
}

/// Whether a discovered UUID is `want`: BTstack reports `uuid16` for 16-bit
/// UUIDs and 0 for 128-bit ones, and a device may list a base-form UUID
/// either way
pub fn uuid16_matches(uuid16: u16, uuid128: &[u8; 16], want: u16) -> bool {
    if uuid16 != 0 {
        return uuid16 == want;
    }
    *uuid128 == base_uuid128(want)
}

/// Unknown vendor ids are discoverable for diagnostics but never selected
/// as an N70 automatically
pub fn is_n70(vendor_id: u16) -> bool {
    matches!(vendor_id, N70_BLACK_VENDOR_ID | N70_ALTERNATE_VENDOR_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_uuid_matching() {
        assert_eq!(base_uuid128(0xA001)[..4], [0, 0, 0xA0, 0x01]);
        assert_eq!(base_uuid128(0xA001)[12..], [0x5F, 0x9B, 0x34, 0xFB]);
        assert!(uuid16_matches(0xA001, &[0; 16], MAIN_SERVICE));
        assert!(!uuid16_matches(0x1001, &[0; 16], MAIN_SERVICE));
        // Listed as a 128-bit UUID in the base
        assert!(uuid16_matches(0, &base_uuid128(COMMAND), COMMAND));
        assert!(!uuid16_matches(0, &base_uuid128(NOTIFICATION), COMMAND));
        // A 128-bit UUID outside the base is nothing of ours
        let mut other = base_uuid128(COMMAND);
        other[15] ^= 1;
        assert!(!uuid16_matches(0, &other, COMMAND));
    }

    #[test]
    fn n70_vendor_ids() {
        assert!(is_n70(23872));
        assert!(is_n70(23877));
        assert!(!is_n70(0));
        assert!(!is_n70(23873));
    }
}
