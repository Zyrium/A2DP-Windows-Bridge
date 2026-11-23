//! What the TLV keeps beside an LE bond of le_device_db, by the bond's index
//! (GATT server client configurations, a Service Changed that is due, the
//! classic device of derived keys), and bonds that make room for new ones.
//!
//! le_device_db holds NVM_NUM_DEVICE_DB_ENTRIES bonds. With all of them
//! taken, a new one replaces the one stored first (not the one of a connected
//! device); the patched le_device_db_tlv.c says so
//! (`bluecaster_rs_le_bond_evicted`), and everything kept for the old device
//! goes with its bond: the new device has the same index. Unlike a bond the
//! user forgets, nobody asked for this, so it is a warning in the log.

use std::cell::RefCell;
use std::ffi::c_int;

use crate::callbacks::guard;
use crate::stack::addr_str;
use crate::{Stack, ctkd, ffi};

thread_local! {
    /// For the application ([`Stack::take_evicted_le_bonds`])
    static EVICTED: RefCell<Vec<EvictedLeBond>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn reset() {
    EVICTED.with(|e| e.borrow_mut().clear());
}

/// The identity of a bond a new one took the place of
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvictedLeBond {
    pub addr_type: u8,
    pub addr: [u8; 6],
}

/// sizeof(persistent_ccc_entry_t) of att_server.c: seq_nr u32, att_handle
/// u16, value u8, device_index u8
const CCC_ENTRY_LEN: usize = 8;

/// CCC values att_server keeps, for all bonded clients together
pub fn ccc_slots() -> u32 {
    // SAFETY: returns a constant.
    unsafe { ffi::bluecaster_ccc_slots() }
}

/// The TLV tag of the CCC value in `slot` (below [`ccc_slots`])
pub fn ccc_tag(slot: u32) -> u32 {
    // SAFETY: computes the tag from its argument alone.
    unsafe { ffi::bluecaster_ccc_tag(slot) }
}

/// The le_device_db index of the client a stored CCC value belongs to; None
/// for another value
pub fn ccc_device_index(entry: &[u8]) -> Option<u8> {
    (entry.len() == CCC_ENTRY_LEN).then(|| entry[7])
}

/// Ours: a Service Changed indication is due for the client with this
/// le_device_db index
pub fn service_changed_tag(index: usize) -> u32 {
    (u32::from(b'A') << 24) | (u32::from(b'W') << 16) | (u32::from(b'S') << 8) | (index as u32 & 0xFF)
}

impl Stack {
    /// The bonds new ones took the place of since the last call
    /// (GAP_SUBEVENT_BONDING_DELETED comes for each)
    pub fn take_evicted_le_bonds(&self) -> Vec<EvictedLeBond> {
        EVICTED.with(|e| std::mem::take(&mut *e.borrow_mut()))
    }

    /// Deletes what the TLV keeps beside the bond `index`. att_server does
    /// so for its part on GAP_SUBEVENT_BONDING_DELETED, but runs only with
    /// LE Audio on, and bonds come without it too (CTKD).
    fn delete_tlv_beside_bond(&self, index: usize) {
        let ccc: Vec<u32> = (0..ccc_slots())
            .map(ccc_tag)
            .filter(|&t| self.tlv_get(t).is_some_and(|e| ccc_device_index(&e).is_some_and(|i| usize::from(i) == index)))
            .collect();
        for tag in ccc.into_iter().chain([service_changed_tag(index), ctkd::tag(index)]) {
            self.tlv_delete(tag);
        }
    }
}

/// le_device_db_add dropped the bond `index` for a new one, not stored yet
#[unsafe(no_mangle)]
extern "C" fn bluecaster_rs_le_bond_evicted(index: c_int, addr_type: c_int, addr: *const u8) {
    guard((), || {
        let (Ok(index), false) = (usize::try_from(index), addr.is_null()) else { return };
        let mut a = [0u8; 6];
        // SAFETY: C passes a bd_addr_t (6 bytes).
        unsafe { std::ptr::copy_nonoverlapping(addr, a.as_mut_ptr(), 6) };
        let stack = Stack::new();
        tracing::warn!(
            target: "le",
            "all {} LE bonds are in use: the oldest one, of {} (type {addr_type}, index {index}), makes room for a new \
             device and has to be paired again",
            stack.le_device_db_max_count(),
            addr_str(&a)
        );
        stack.delete_tlv_beside_bond(index);
        let addr_type = u8::try_from(addr_type).unwrap_or(u8::MAX);
        EVICTED.with(|e| e.borrow_mut().push(EvictedLeBond { addr_type, addr: a }));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tags of everything the bonds file holds
    #[test]
    fn tags_do_not_clash() {
        // The first 256 as BTstack has them: existing files stay valid
        assert_eq!(ccc_tag(0), u32::from_be_bytes(*b"BTC\x00"));
        assert_eq!(ccc_tag(255), u32::from_be_bytes(*b"BTC\xFF"));
        assert_eq!(ccc_tag(256), u32::from_be_bytes(*b"AW1\x00"));
        assert_eq!(ccc_tag(0x5A7), u32::from_be_bytes(*b"AW5\xA7"));
        // SAFETY: returns a constant.
        let bonds = unsafe { ffi::le_device_db_max_count() } as usize;
        let mut tags: Vec<u32> = (0..ccc_slots()).map(ccc_tag).collect();
        for i in 0..bonds {
            tags.push(u32::from_be_bytes([b'B', b'T', b'D', i as u8]));
            tags.push(service_changed_tag(i));
            tags.push(ctkd::tag(i));
        }
        // att_server's Database Hash
        tags.push(u32::from_be_bytes(*b"BTDB"));
        let all = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), all);
    }

    #[test]
    fn ccc_entry_layout() {
        assert_eq!(ccc_device_index(&[1, 0, 0, 0, 0x10, 0, 1, 7]), Some(7));
        assert_eq!(ccc_device_index(&[0; 16]), None);
    }
}
