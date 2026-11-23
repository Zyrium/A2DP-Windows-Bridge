//! The profile modules (v1 le_audio_modules.cpp), one file each, one work
//! package each (WP5). A module file changes only itself; the registry
//! below is fixed.

pub mod bas;
pub mod ccp;
pub mod csip;
pub mod hogp;
pub mod mcp;
pub mod tmap_gmap;
pub mod vcp;

use super::module::{LeModuleEntry, LeModuleSetup};

/// All modules in v1's fixed order: CSIP, VCP, MCP, CCP, TMAP / GMAP, then
/// BAS (the headset's battery level), with LE Audio on; then HID over GATT
/// with keyboards and mice on (issue #413). Without LE Audio none of its
/// modules exists: no LE Audio service is added to the PC's GATT server.
/// Called by the LE core on the BTstack thread when the stack starts; hooks
/// run in this order.
pub fn create_all(setup: &mut LeModuleSetup) -> Vec<LeModuleEntry> {
    let mut all = Vec::new();
    if setup.audio {
        all.extend([
            csip::create(setup),
            vcp::create(setup),
            mcp::create(setup),
            ccp::create(setup),
            tmap_gmap::create(setup),
            bas::create(setup),
        ]);
    }
    if let Some(hid) = setup.hid.clone() {
        all.push(hogp::create(hid));
    }
    all
}
