//! The manufacturer-specific data the earbuds advertise (AD type 0xFF,
//! company id 0x521C): product identity, battery state and a separate
//! control address to connect to.
//!
//! Two entry points: [`Advertisement::parse`] takes the manufacturer data
//! *after* the two company-id bytes (what Windows' advertisement watcher
//! hands over), [`find_in_ad`] walks raw advertising data (what BTstack's
//! advertising reports carry) and finds the block itself.
//!
//! Layout after the company id (same indexes as OpenQCY's
//! `QcyAdvertisement.Parse`):
//!
//! | index | content |
//! |:------|:--------|
//! | 0-1 | vendor id, big-endian |
//! | 5, 6, 7 | left, right, case battery: bit 7 charging, low 7 bits percent |
//! | 11-16 | control address (scrambled order, see [`control_address`]) |
//! | 18-23 | the other bud's address, same scrambling |

use crate::uuids::COMPANY_ID;

const AD_MANUFACTURER_DATA: u8 = 0xFF;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    pub vendor_id: u16,
    pub left_battery: u8,
    pub right_battery: u8,
    pub case_battery: u8,
    pub left_charging: bool,
    pub right_charging: bool,
    pub case_charging: bool,
    /// The address to open the control channel on (see [`address_bytes`])
    pub control_address: Option<u64>,
    /// The other bud's address, when advertised and different
    pub other_address: Option<u64>,
}

impl Advertisement {
    /// `data`: the manufacturer data after the company id. None when it is
    /// shorter than 8 bytes.
    pub fn parse(data: &[u8]) -> Option<Advertisement> {
        if data.len() < 8 {
            return None;
        }
        let vendor_id = u16::from_be_bytes([data[0], data[1]]);
        let (left, right, case) = (data[5], data[6], data[7]);

        let mut control =
            (data.len() >= 17).then(|| build_address(&[data[12], data[11], data[13], data[16], data[15], data[14]]));
        let mut other =
            (data.len() >= 24).then(|| build_address(&[data[19], data[18], data[20], data[23], data[22], data[21]]));
        if control == Some(0) {
            control = None;
        }
        if other == Some(0) || other == control {
            other = None;
        }

        Some(Advertisement {
            vendor_id,
            left_battery: percentage(left),
            right_battery: percentage(right),
            case_battery: percentage(case),
            left_charging: left & 0x80 != 0,
            right_charging: right & 0x80 != 0,
            case_charging: case & 0x80 != 0,
            control_address: control,
            other_address: other,
        })
    }
}

/// Finds the QCY manufacturer block in raw advertising (or scan response)
/// data and parses it
pub fn find_in_ad(ad: &[u8]) -> Option<Advertisement> {
    let mut i = 0usize;
    while i < ad.len() {
        let len = ad[i] as usize;
        if len == 0 {
            i += 1;
            continue;
        }
        let item = ad.get(i + 1..i + 1 + len)?;
        i += 1 + len;
        if item[0] == AD_MANUFACTURER_DATA
            && item.len() >= 3
            && u16::from_le_bytes([item[1], item[2]]) == COMPANY_ID
            && let Some(found) = Advertisement::parse(&item[3..])
        {
            return Some(found);
        }
    }
    None
}

/// "C4:33:96:11:0C:82", most significant octet first
pub fn format_address(address: u64) -> String {
    address_bytes(address).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// The 6 octets, most significant first (the order of `bt_host::parse_addr`)
pub fn address_bytes(address: u64) -> [u8; 6] {
    let b = address.to_be_bytes();
    [b[2], b[3], b[4], b[5], b[6], b[7]]
}

fn percentage(value: u8) -> u8 {
    (value & 0x7F).min(100)
}

/// Bytes in the given order, most significant first. The advertisement
/// stores the address with its bytes permuted; the callers pass them already
/// permuted the way OpenQCY does (`data[12], [11], [13], [16], [15], [14]`).
fn build_address(bytes: &[u8; 6]) -> u64 {
    bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 24 bytes of manufacturer data (after the company id) for
    /// vendor 23877 (0x5D45): left 80 % charging, right 75 %, case 40 %
    fn sample() -> Vec<u8> {
        let mut d = vec![0u8; 24];
        d[0] = 0x5D;
        d[1] = 0x45;
        d[5] = 0x80 | 80;
        d[6] = 75;
        d[7] = 40;
        // control address bytes at 12, 11, 13, 16, 15, 14 -> C4:33:96:11:0C:83
        d[12] = 0xC4;
        d[11] = 0x33;
        d[13] = 0x96;
        d[16] = 0x11;
        d[15] = 0x0C;
        d[14] = 0x83;
        // the other address at 19, 18, 20, 23, 22, 21 -> C4:33:96:11:1B:A6
        d[19] = 0xC4;
        d[18] = 0x33;
        d[20] = 0x96;
        d[23] = 0x11;
        d[22] = 0x1B;
        d[21] = 0xA6;
        d
    }

    #[test]
    fn parses_identity_and_battery() {
        let a = Advertisement::parse(&sample()).unwrap();
        assert_eq!(a.vendor_id, 23877);
        assert!(crate::uuids::is_n70(a.vendor_id));
        assert_eq!((a.left_battery, a.right_battery, a.case_battery), (80, 75, 40));
        assert_eq!((a.left_charging, a.right_charging, a.case_charging), (true, false, false));
    }

    #[test]
    fn parses_addresses() {
        let a = Advertisement::parse(&sample()).unwrap();
        assert_eq!(format_address(a.control_address.unwrap()), "C4:33:96:11:0C:83");
        assert_eq!(format_address(a.other_address.unwrap()), "C4:33:96:11:1B:A6");
        assert_eq!(address_bytes(a.control_address.unwrap()), [0xC4, 0x33, 0x96, 0x11, 0x0C, 0x83]);
    }

    #[test]
    fn short_data_has_no_addresses() {
        let a = Advertisement::parse(&sample()[..8]).unwrap();
        assert_eq!(a.control_address, None);
        assert_eq!(a.other_address, None);
        assert!(Advertisement::parse(&sample()[..7]).is_none());
    }

    #[test]
    fn zero_and_duplicate_addresses_are_dropped() {
        let mut d = sample();
        d[19..24].fill(0);
        d[18] = 0;
        assert_eq!(Advertisement::parse(&d).unwrap().other_address, None);
        // The other address equal to the control address
        let mut d = sample();
        d[19] = d[12];
        d[18] = d[11];
        d[20] = d[13];
        d[23] = d[16];
        d[22] = d[15];
        d[21] = d[14];
        assert_eq!(Advertisement::parse(&d).unwrap().other_address, None);
        // No control address at all
        let mut d = sample();
        d[11..17].fill(0);
        assert_eq!(Advertisement::parse(&d).unwrap().control_address, None);
    }

    #[test]
    fn battery_is_capped_at_100() {
        let mut d = sample();
        d[5] = 0x7F;
        assert_eq!(Advertisement::parse(&d).unwrap().left_battery, 100);
    }

    #[test]
    fn finds_the_block_in_raw_advertising_data() {
        let mut ad = vec![2, 0x01, 0x06]; // flags
        ad.extend([8, 0x09, b'M', b'e', b'l', b'o', b'B', b'u', b's']); // name
        let data = sample();
        ad.push((data.len() + 3) as u8);
        ad.push(0xFF);
        ad.extend(0x521Cu16.to_le_bytes());
        ad.extend(&data);
        assert_eq!(find_in_ad(&ad), Advertisement::parse(&data));
        // Another company id is not ours
        let mut other = ad.clone();
        let at = other.len() - data.len() - 2;
        other[at] = 0x4C;
        other[at + 1] = 0x00;
        assert_eq!(find_in_ad(&other), None);
        // A truncated structure ends the walk
        assert_eq!(find_in_ad(&ad[..ad.len() - 1]), None);
    }
}
