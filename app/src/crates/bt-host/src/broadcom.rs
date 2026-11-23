//! Broadcom USB adapters (BCM20702, BCM20703, BCM43142 and the Bluetooth of
//! the BCM4335 / 4350 / 4356 / 4371 Wi-Fi combos): the PatchRAM of
//! Broadcom's Windows driver, untested on hardware
//!
//! These chips run from ROM; Broadcom's Windows driver (bcbtums, versions
//! 12.0.1.410 to 12.0.1.1012 and 6.5.1.6812) downloads a patch into their
//! RAM at every start. Its INF names the patch for each USB ID
//! (`RAMPatchFileName`, a .hex file; [`ADAPTERS`]), and its install copies
//! the files into System32\drivers. Everything below comes from those INFs,
//! the .hex files and the driver binary; no other driver's source was used.
//! A user's own .hcd in the firmware folder still goes BTstack's way (app.rs
//! does not start this loader then).
//!
//! The .hex file is Intel HEX: records `:LLAAAATT<data>CC` with a checksum
//! (the two's complement of the sum of the other bytes), type 00 (data at
//! AAAA plus the upper address), 04 (the upper 16 address bits) and 01
//! (end). The driver turns each data record into one Write RAM command
//! (0xFC4C: the address, u32, then the data); records hold at most 251
//! bytes, so a command fits in 255. The number after the last dot of the
//! file name is the patch's build ("...1443.1467.hex": 1467).
//!
//! The sequence, right after the USB configuration is selected (no HCI
//! Reset before it), each command answered by its Command Complete with
//! status 0, each answer awaited at most 2 s:
//! 1. 0xFC79 (no parameters): the build running is the u16 at return byte
//!    4. When it is the patch file's build, the patch runs already: done
//! 2. BCM43142A0 only (the INF's `UseCM3Workaround`, set for exactly that
//!    chip's patches): Write RAM {0x00200470: 00 00 00 00}, then 0xFC4F
//!    with 14 fixed bytes, then Write RAM {0x000B63E0: 20 fixed bytes}
//! 3. 0xFC2E (no parameters): the download mode; then 100 ms
//! 4. the Write RAM commands of the file, in its order, on the bulk OUT
//!    (ACL) pipe; the others go on the control pipe
//! 5. 0xFC4E {0xFFFFFFFF}: start the patch; after its Command Complete the
//!    vendor event FF {1E 00}
//! 6. 0xFC79 again, whose answer the driver only logs
//!
//! Broadcom adapters work on their ROM, and this sequence is untested, so
//! a failure must never cost the user the adapter:
//! - before anything changed in the chip (the device descriptor, the patch
//!   file, the first 0xFC79): no error, the ROM runs as before
//! - from the first command that changes it on (the BCM43142A0's RAM
//!   writes, else 0xFC2E): [`LoadError::Replug`], whose text tells the
//!   user to unplug the adapter (or shut a built-in one down fully), and a
//!   mark ([`PatchMarks`]) so the next starts run the ROM. RAM, and with it
//!   the download mode, is gone after a power cycle. A mark of another app
//!   version is dropped, as is the mark after a successful download.
//!
//! Not done here: the driver's later behaviour on the host's commands
//! (`LEScanParametersFix`, `VsMsftOpCode`), which is not part of the
//! bring-up.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::bringup::{
    Action, ChipLoader, ControlRequest, FileReader, HciEvent, LoadError, Outcome, hci_command, parse_event,
};
use crate::driver_files::{self, file_name};

/// The USB IDs of Broadcom's Windows driver INFs: VID, PID, the patch files
/// the INFs name for it, newest driver first (none: the INFs load no patch,
/// the chip runs from ROM). 13D3:3517, in Qualcomm's driver too, is left to
/// crate::qualcomm.
pub const ADAPTERS: &[(u16, u16, &[&str])] = &[
    (0x03F0, 0x231D, &[]),
    (0x03F0, 0x2A1D, &[]),
    (0x044E, 0x3017, &[]),
    (0x044E, 0x301B, &[]),
    (0x0489, 0xE00D, &[]),
    (0x0489, 0xE00E, &[]),
    (0x0489, 0xE00F, &[]),
    (0x0489, 0xE010, &[]),
    (0x0489, 0xE011, &[]),
    (0x0489, 0xE012, &[]),
    (0x0489, 0xE013, &[]),
    (0x0489, 0xE014, &[]),
    (0x0489, 0xE02F, &[]),
    (0x0489, 0xE030, &[]),
    (0x0489, 0xE031, &["BCM20702A0_001.001.024.0156.0204.hex"]),
    (0x0489, 0xE032, &["BCM20702A1_001.002.014.1443.1485.hex", "BCM20702A1_001.002.014.1315.1365.hex"]),
    (0x0489, 0xE033, &[]),
    (0x0489, 0xE039, &[]),
    (0x0489, 0xE042, &["BCM20702A1_001.002.014.1443.1484.hex", "BCM20702A1_001.002.014.1315.1363.hex"]),
    (0x0489, 0xE046, &["BCM20702A1_001.002.014.1443.1465.hex", "BCM20702A1_001.002.014.1315.1327.hex"]),
    (0x0489, 0xE047, &["BCM20702A1_001.002.014.1055.1092.hex", "BCM20702A1_001.002.014.1055.1061.hex"]),
    (0x0489, 0xE04F, &["BCM20702A1_001.002.014.1443.1486.hex", "BCM20702A1_001.002.014.1315.1367.hex"]),
    (
        0x0489,
        0xE052,
        &[
            "BCM20702A1_001.002.014.1502.1758.hex",
            "BCM20702A1_001.002.014.1315.1385.hex",
            "BCM20702A1_001.002.014.1483.1668.hex",
        ],
    ),
    (
        0x0489,
        0xE055,
        &[
            "BCM43142A0_001.001.011.0311.0331.hex",
            "BCM43142A0_001.001.011.0197.0206.hex",
            "BCM43142A0_001.001.011.0249.0267.hex",
        ],
    ),
    (0x0489, 0xE059, &["BCM20702A1_001.002.014.1443.1466.hex", "BCM20702A1_001.002.014.1315.1329.hex"]),
    (0x0489, 0xE062, &["BCM43142A0_001.001.011.0277.0280.hex", "BCM43142A0_001.001.011.0277.0286.hex"]),
    (0x0489, 0xE079, &["BCM4335C0_003.001.009.0066.0115.hex", "BCM4335C0_003.001.009.0067.0116.hex"]),
    (0x0489, 0xE07A, &["BCM20702A1_001.002.014.1483.1651.hex", "BCM20702A1_001.002.014.1315.1724.hex"]),
    (0x0489, 0xE087, &["BCM20702A1_001.002.014.1443.1532.hex", "BCM20702A1_001.002.014.1315.1533.hex"]),
    (
        0x0489,
        0xE096,
        &[
            "BCM43142A0_001.001.011.0311.0340.hex",
            "BCM43142A0_001.001.011.0277.0297.hex",
            "BCM43142A0_001.001.011.0277.0293.hex",
        ],
    ),
    (0x0489, 0xE097, &["BCM4356A2_001.003.015.0046.0104.hex", "BCM4356A2_001.003.015.0046.0105.hex"]),
    (0x0489, 0xE0A1, &["BCM20703A1_001.001.005.0214.0414.hex"]),
    (0x04CA, 0x2003, &["BCM20702A1_001.002.014.1443.1488.hex", "BCM20702A1_001.002.014.1315.1371.hex"]),
    (0x04CA, 0x2004, &["BCM20702A1_001.002.014.1443.1489.hex", "BCM20702A1_001.002.014.1315.1331.hex"]),
    (0x04CA, 0x2005, &["BCM20702A1_001.002.014.1443.1490.hex", "BCM20702A1_001.002.014.1315.1373.hex"]),
    (
        0x04CA,
        0x2006,
        &[
            "BCM43142A0_001.001.011.0311.0327.hex",
            "BCM43142A0_001.001.011.0197.0214.hex",
            "BCM43142A0_001.001.011.0249.0271.hex",
        ],
    ),
    (0x04CA, 0x2007, &["BCM43142A0_001.001.011.0277.0279.hex", "BCM43142A0_001.001.011.0277.0285.hex"]),
    (
        0x04CA,
        0x2009,
        &[
            "BCM43142A0_001.001.011.0311.0330.hex",
            "BCM43142A0_001.001.011.0197.0212.hex",
            "BCM43142A0_001.001.011.0249.0270.hex",
        ],
    ),
    (0x04CA, 0x200A, &["BCM20702A1_001.002.014.1443.1492.hex", "BCM20702A1_001.002.014.1315.1333.hex"]),
    (0x04CA, 0x200B, &["BCM20702A1_001.002.014.1443.1493.hex", "BCM20702A1_001.002.014.1315.1375.hex"]),
    (0x04CA, 0x200C, &["BCM20702A1_001.002.014.1443.1494.hex", "BCM20702A1_001.002.014.1315.1377.hex"]),
    (0x04CA, 0x200E, &["BCM20702A1_001.002.014.1443.1499.hex", "BCM20702A1_001.002.014.1315.1505.hex"]),
    (0x04CA, 0x200F, &["BCM20702A1_001.002.014.1443.1521.hex", "BCM20702A1_001.002.014.1315.1522.hex"]),
    (
        0x04CA,
        0x2012,
        &[
            "BCM43142A0_001.001.011.0311.0339.hex",
            "BCM43142A0_001.001.011.0277.0295.hex",
            "BCM43142A0_001.001.011.0277.0291.hex",
        ],
    ),
    (0x04CA, 0x2013, &["BCM4356A2_001.003.015.0046.0122.hex", "BCM4356A2_001.003.015.0046.0123.hex"]),
    (0x04CA, 0x2014, &["BCM4356A2_001.003.015.0046.0120.hex", "BCM4356A2_001.003.015.0046.0121.hex"]),
    (0x04CA, 0x2016, &["BCM4335C0_003.001.009.0066.0121.hex", "BCM4335C0_003.001.009.0067.0130.hex"]),
    (0x04F2, 0xB49D, &["BCM43142A0_001.001.011.0277.0308.hex", "BCM43142A0_001.001.011.0277.0309.hex"]),
    (0x04F2, 0xB4A1, &["BCM43142A0_001.001.011.0311.0316.hex", "BCM43142A0_001.001.011.0277.0324.hex"]),
    (0x04F2, 0xB4A2, &["BCM4356A2_001.003.015.0046.0183.hex", "BCM4356A2_001.003.015.0046.0184.hex"]),
    (0x050D, 0x065A, &["BCM20702A1_001.002.014.1443.1482.hex", "BCM20702A1_001.002.014.1315.1359.hex"]),
    (0x0930, 0x0200, &[]),
    (0x0930, 0x020F, &[]),
    (0x0930, 0x0213, &[]),
    (0x0930, 0x0214, &[]),
    (0x0930, 0x021A, &["BCM20702A0_001.001.024.0216.0223.hex"]),
    (
        0x0930,
        0x021E,
        &[
            "BCM20702A1_001.002.014.1502.1759.hex",
            "BCM20702A1_001.002.014.1315.1389.hex",
            "BCM20702A1_001.002.014.1483.1672.hex",
        ],
    ),
    (
        0x0930,
        0x021F,
        &[
            "BCM43142A0_001.001.011.0311.0335.hex",
            "BCM43142A0_001.001.011.0277.0287.hex",
            "BCM43142A0_001.001.011.0277.0281.hex",
        ],
    ),
    (
        0x0930,
        0x0221,
        &[
            "BCM20702A1_001.002.014.1502.1762.hex",
            "BCM20702A1_001.002.014.1315.1403.hex",
            "BCM20702A1_001.002.014.1483.1673.hex",
        ],
    ),
    (
        0x0930,
        0x0223,
        &[
            "BCM20702A1_001.002.014.1502.1763.hex",
            "BCM20702A1_001.002.014.1315.1405.hex",
            "BCM20702A1_001.002.014.1483.1674.hex",
        ],
    ),
    (
        0x0930,
        0x0225,
        &[
            "BCM43142A0_001.001.011.0311.0334.hex",
            "BCM43142A0_001.001.011.0277.0288.hex",
            "BCM43142A0_001.001.011.0277.0282.hex",
        ],
    ),
    (
        0x0930,
        0x0226,
        &[
            "BCM43142A0_001.001.011.0311.0334.hex",
            "BCM43142A0_001.001.011.0277.0288.hex",
            "BCM43142A0_001.001.011.0277.0282.hex",
        ],
    ),
    (0x0930, 0x0229, &["BCM4335C0_003.001.009.0066.0104.hex"]),
    (0x0930, 0x0747, &[]),
    (0x0A5C, 0x2119, &[]),
    (0x0A5C, 0x2145, &[]),
    (0x0A5C, 0x214D, &[]),
    (0x0A5C, 0x2150, &[]),
    (0x0A5C, 0x2151, &[]),
    (0x0A5C, 0x2167, &["BCM43142A0_001.001.011.0249.0265.hex", "BCM43142A0_001.001.011.0277.0296.hex"]),
    (0x0A5C, 0x2168, &["BCM4335C0_003.001.009.0066.0108.hex", "BCM4335C0_003.001.009.0067.0109.hex"]),
    (0x0A5C, 0x2169, &["BCM20702A1_001.002.014.1443.1462.hex"]),
    (
        0x0A5C,
        0x216A,
        &[
            "BCM43142A0_001.001.011.0311.0336.hex",
            "BCM43142A0_001.001.011.0277.0284.hex",
            "BCM43142A0_001.001.011.0277.0290.hex",
        ],
    ),
    (
        0x0A5C,
        0x216B,
        &[
            "BCM20702A1_001.002.014.1502.1768.hex",
            "BCM20702A1_001.002.014.1315.1498.hex",
            "BCM20702A1_001.002.014.1483.1646.hex",
        ],
    ),
    (
        0x0A5C,
        0x216C,
        &[
            "BCM43142A0_001.001.011.0311.0328.hex",
            "BCM43142A0_001.001.011.0277.0283.hex",
            "BCM43142A0_001.001.011.0277.0289.hex",
        ],
    ),
    (
        0x0A5C,
        0x216D,
        &[
            "BCM43142A0_001.001.011.0311.0329.hex",
            "BCM43142A0_001.001.011.0277.0284.hex",
            "BCM43142A0_001.001.011.0277.0290.hex",
        ],
    ),
    (0x0A5C, 0x216E, &["BCM4335C0_003.001.009.0066.0105.hex", "BCM4335C0_003.001.009.0067.0107.hex"]),
    (0x0A5C, 0x216F, &["BCM20702A1_001.002.014.1443.1572.hex", "BCM20702A1_001.002.014.1315.1573.hex"]),
    (0x0A5C, 0x217D, &[]),
    (0x0A5C, 0x217F, &[]),
    (0x0A5C, 0x2184, &[]),
    (0x0A5C, 0x218C, &[]),
    (0x0A5C, 0x2198, &[]),
    (0x0A5C, 0x219A, &[]),
    (0x0A5C, 0x219B, &[]),
    (0x0A5C, 0x219C, &[]),
    (0x0A5C, 0x21A2, &[]),
    (0x0A5C, 0x21B1, &[]),
    (0x0A5C, 0x21B4, &[]),
    (0x0A5C, 0x21BC, &[]),
    (0x0A5C, 0x21C0, &["BCM20702A0_001.001.024.0156.0185.hex"]),
    (0x0A5C, 0x21C1, &["BCM20702A0_001.001.024.0156.0187.hex"]),
    (0x0A5C, 0x21C2, &["BCM20702A0_001.001.024.0156.0188.hex"]),
    (0x0A5C, 0x21C3, &["BCM20702A0_001.001.024.0156.0193.hex"]),
    (0x0A5C, 0x21C4, &["BCM20702A0_001.001.024.0156.0177.hex"]),
    (0x0A5C, 0x21C6, &["BCM20702A0_001.001.024.0156.0176.hex"]),
    (0x0A5C, 0x21C9, &["BCM20702A0_001.001.024.0156.0180.hex"]),
    (0x0A5C, 0x21CA, &["BCM20702A0_001.001.024.0156.0181.hex"]),
    (0x0A5C, 0x21CC, &["BCM20702A0_001.001.024.0156.0184.hex"]),
    (0x0A5C, 0x21D3, &["BCM43142A0_001.001.011.0197.0218.hex", "BCM43142A0_001.001.011.0197.0226.hex"]),
    (0x0A5C, 0x21D6, &["BCM43142A0_001.001.011.0197.0220.hex", "BCM43142A0_001.001.011.0197.0221.hex"]),
    (
        0x0A5C,
        0x21D7,
        &[
            "BCM43142A0_001.001.011.0311.0341.hex",
            "BCM43142A0_001.001.011.0277.0283.hex",
            "BCM43142A0_001.001.011.0277.0289.hex",
        ],
    ),
    (0x0A5C, 0x21D8, &["BCM43142A0_001.001.011.0197.0222.hex", "BCM43142A0_001.001.011.0197.0223.hex"]),
    (0x0A5C, 0x21DC, &["BCM43142A0_001.001.011.0161.0187.hex", "BCM43142A0_001.001.011.0084.0102.hex"]),
    (0x0A5C, 0x21DE, &["BCM20702A1_001.002.014.1443.1461.hex", "BCM20702A1_001.002.014.1315.1422.hex"]),
    (
        0x0A5C,
        0x21E1,
        &[
            "BCM20702A1_001.002.014.1502.1770.hex",
            "BCM20702A1_001.002.014.1055.1146.hex",
            "BCM20702A1_001.002.014.1483.1647.hex",
        ],
    ),
    (
        0x0A5C,
        0x21E3,
        &[
            "BCM20702A1_001.002.014.1502.1767.hex",
            "BCM20702A1_001.002.014.1055.1148.hex",
            "BCM20702A1_001.002.014.1483.1649.hex",
        ],
    ),
    (0x0A5C, 0x21E4, &[]),
    (0x0A5C, 0x21E5, &["BCM20702A1_001.002.014.0136.0143.hex"]),
    (
        0x0A5C,
        0x21E6,
        &[
            "BCM20702A1_001.002.014.1502.1757.hex",
            "BCM20702A1_001.002.014.1315.1387.hex",
            "BCM20702A1_001.002.014.1483.1669.hex",
        ],
    ),
    (
        0x0A5C,
        0x21E8,
        &[
            "BCM20702A1_001.002.014.1502.1764.hex",
            "BCM20702A1_001.002.014.1315.1784.hex",
            "BCM20702A1_001.002.014.1443.1459.hex",
        ],
    ),
    (0x0A5C, 0x21EB, &["BCM20702A1_001.002.014.0136.0153.hex"]),
    (0x0A5C, 0x21EC, &["BCM20702A1_001.002.014.1443.1460.hex", "BCM20702A1_001.002.014.1315.1420.hex"]),
    (0x0A5C, 0x21ED, &["BCM20702A1_001.002.014.0337.0347.hex"]),
    (0x0A5C, 0x21EE, &["BCM20702A1_001.002.014.0136.0230.hex"]),
    (
        0x0A5C,
        0x21F1,
        &[
            "BCM20702A1_001.002.014.1502.1765.hex",
            "BCM20702A1_001.002.014.1055.1145.hex",
            "BCM20702A1_001.002.014.1483.1645.hex",
        ],
    ),
    (
        0x0A5C,
        0x21F3,
        &[
            "BCM20702A1_001.002.014.1502.1761.hex",
            "BCM20702A1_001.002.014.1315.1407.hex",
            "BCM20702A1_001.002.014.1483.1671.hex",
        ],
    ),
    (
        0x0A5C,
        0x21F4,
        &[
            "BCM20702A1_001.002.014.1502.1760.hex",
            "BCM20702A1_001.002.014.1315.1391.hex",
            "BCM20702A1_001.002.014.1483.1670.hex",
        ],
    ),
    (
        0x0A5C,
        0x21FB,
        &[
            "BCM20702A1_001.002.014.1502.1766.hex",
            "BCM20702A1_001.002.014.1159.1180.hex",
            "BCM20702A1_001.002.014.1483.1648.hex",
        ],
    ),
    (0x0A5C, 0x21FD, &["BCM20702A1_001.002.014.1443.1463.hex"]),
    (0x0A5C, 0x21FE, &["BCM43142A0_001.001.011.0161.0186.hex", "BCM43142A0_001.001.011.0122.0154.hex"]),
    (0x0A5C, 0x640A, &["BCM4356A2_001.003.015.0080.0225.hex", "BCM4356A2_001.003.015.0037.0074.hex"]),
    (
        0x0A5C,
        0x640B,
        &[
            "BCM20702A1_001.002.014.1502.1769.hex",
            "BCM20702A1_001.002.014.1315.1721.hex",
            "BCM20702A1_001.002.014.1483.1709.hex",
        ],
    ),
    (
        0x0A5C,
        0x640E,
        &[
            "BCM4356A2_001.003.015.0082.0255.hex",
            "BCM4356A2_001.003.015.0046.0098.hex",
            "BCM4356A2_001.003.015.0046.0097.hex",
        ],
    ),
    (0x0A5C, 0x6410, &["BCM20703A1_001.001.005.0214.0422.hex", "BCM20703A1_001.001.005.0214.0423.hex"]),
    (0x0A5C, 0x6412, &["BCM4350C5_003.006.007.0095.1703.hex", "BCM4350C5_003.006.007.0095.1707.hex"]),
    (0x0A5C, 0x6413, &["BCM4350C5_003.006.007.0120.2118.hex"]),
    (
        0x0A5C,
        0x6414,
        &[
            "BCM4350C5_003.006.007.0145.2724.hex",
            "BCM4350C5_003.006.007.0095.1878.hex",
            "BCM4350C5_003.006.007.0120.1947.hex",
        ],
    ),
    (
        0x0A5C,
        0x6417,
        &[
            "BCM20702A1_001.002.014.1502.1780.hex",
            "BCM20702A1_001.002.014.1443.1778.hex",
            "BCM20702A1_001.002.014.1502.1771.hex",
        ],
    ),
    (0x0A5C, 0x6418, &["BCM4371C2_001.003.015.0093.0116.hex", "BCM4371C2_001.003.015.0088.0110.hex"]),
    (0x0A5C, 0x6419, &["BCM4356A2_001.003.015.0095.0292.hex", "BCM4356A2_001.003.015.0097.0302.hex"]),
    (0x0A5C, 0x6420, &["BCM4356A2_001.003.015.0095.0293.hex", "BCM4356A2_001.003.015.0097.0303.hex"]),
    (0x0A5C, 0x745E, &[]),
    (0x0A5C, 0x7460, &["BCM20703A1_001.001.005.0214.0473.hex", "BCM20703A1_001.001.005.0214.0474.hex"]),
    (0x0B05, 0x1788, &[]),
    (0x0B05, 0x1789, &[]),
    (0x0B05, 0x178A, &[]),
    (0x0B05, 0x178B, &[]),
    (0x0B05, 0x17B5, &["BCM20702A1_001.002.014.1443.1468.hex", "BCM20702A1_001.002.014.1315.1349.hex"]),
    (0x0B05, 0x17CB, &["BCM20702A1_001.002.014.1443.1467.hex", "BCM20702A1_001.002.014.1315.1347.hex"]),
    (0x0B05, 0x17CF, &["BCM20702A1_001.002.014.1443.1469.hex", "BCM20702A1_001.002.014.1315.1350.hex"]),
    (0x0B05, 0x180A, &["BCM20702A1_001.002.014.1443.1714.hex", "BCM20702A1_001.002.014.1443.1715.hex"]),
    (0x0B05, 0x181D, &["BCM4356A2_001.003.015.0046.0132.hex", "BCM4356A2_001.003.015.0046.0133.hex"]),
    (
        0x0BB4,
        0x0306,
        &[
            "BCM20703A1_001.001.005.0214.0481.hex",
            "BCM20703A1_001.001.005.0214.0488.hex",
            "BCM20703A1_001.001.005.0214.0409.hex",
        ],
    ),
    (0x0DB0, 0x6898, &[]),
    (0x0DB0, 0x689A, &[]),
    (0x105B, 0xE065, &["BCM43142A0_001.001.011.0311.0312.hex", "BCM43142A0_001.001.011.0277.0294.hex"]),
    (0x105B, 0xE066, &["BCM20702A1_001.002.014.1443.1487.hex", "BCM20702A1_001.002.014.1315.1369.hex"]),
    (0x10AB, 0x0816, &[]),
    (0x10AB, 0x081A, &[]),
    (0x13D3, 0x3286, &[]),
    (0x13D3, 0x3295, &[]),
    (0x13D3, 0x3300, &[]),
    (0x13D3, 0x3301, &[]),
    (0x13D3, 0x3302, &[]),
    (0x13D3, 0x3303, &[]),
    (0x13D3, 0x3384, &["BCM20702A1_001.002.014.1443.1477.hex", "BCM20702A1_001.002.014.1315.1353.hex"]),
    (
        0x13D3,
        0x3388,
        &[
            "BCM43142A0_001.001.011.0311.0332.hex",
            "BCM43142A0_001.001.011.0197.0208.hex",
            "BCM43142A0_001.001.011.0249.0268.hex",
        ],
    ),
    (
        0x13D3,
        0x3389,
        &[
            "BCM43142A0_001.001.011.0311.0333.hex",
            "BCM43142A0_001.001.011.0197.0210.hex",
            "BCM43142A0_001.001.011.0249.0269.hex",
        ],
    ),
    (0x13D3, 0x3392, &["BCM20702A1_001.002.014.1443.1478.hex", "BCM20702A1_001.002.014.1315.1355.hex"]),
    (0x13D3, 0x3404, &["BCM20702A1_001.002.014.1443.1479.hex", "BCM20702A1_001.002.014.1315.1357.hex"]),
    (0x13D3, 0x3411, &["BCM20702A1_001.002.014.1443.1450.hex", "BCM20702A1_001.002.014.1315.1409.hex"]),
    (0x13D3, 0x3413, &["BCM20702A1_001.002.014.1443.1481.hex", "BCM20702A1_001.002.014.1315.1425.hex"]),
    (0x13D3, 0x3418, &["BCM20702A1_001.002.014.1443.1480.hex", "BCM20702A1_001.002.014.1315.1424.hex"]),
    (
        0x13D3,
        0x3427,
        &[
            "BCM43142A0_001.001.011.0311.0334.hex",
            "BCM43142A0_001.001.011.0277.0288.hex",
            "BCM43142A0_001.001.011.0277.0282.hex",
        ],
    ),
    (0x13D3, 0x3435, &["BCM20702A1_001.002.014.1443.1501.hex", "BCM20702A1_001.002.014.1315.1507.hex"]),
    (0x13D3, 0x3456, &["BCM20702A1_001.002.014.1443.1502.hex", "BCM20702A1_001.002.014.1315.1508.hex"]),
    (0x13D3, 0x3473, &["BCM4356A2_001.003.015.0046.0125.hex", "BCM4356A2_001.003.015.0046.0126.hex"]),
    (
        0x13D3,
        0x3482,
        &[
            "BCM43142A0_001.001.011.0311.0346.hex",
            "BCM43142A0_001.001.011.0277.0305.hex",
            "BCM43142A0_001.001.011.0277.0304.hex",
        ],
    ),
    (
        0x13D3,
        0x3484,
        &[
            "BCM43142A0_001.001.011.0311.0347.hex",
            "BCM43142A0_001.001.011.0277.0303.hex",
            "BCM43142A0_001.001.011.0277.0302.hex",
        ],
    ),
    (
        0x13D3,
        0x3485,
        &[
            "BCM4356A2_001.003.015.0046.0197.hex",
            "BCM4356A2_001.003.015.0046.0198.hex",
            "BCM4356A2_001.003.015.0046.0136.hex",
        ],
    ),
    (0x13D3, 0x3488, &["BCM4356A2_001.003.015.0046.0127.hex", "BCM4356A2_001.003.015.0046.0128.hex"]),
    (0x13D3, 0x3492, &["BCM4356A2_001.003.015.0046.0195.hex", "BCM4356A2_001.003.015.0046.0196.hex"]),
    (0x13D3, 0x3504, &["BCM4371C2_001.003.015.0093.0118.hex"]),
    (0x13D3, 0x3508, &["BCM4371C2_001.003.015.0093.0117.hex"]),
    (0x145F, 0x01A3, &["BCM20702A1_001.002.014.1443.1483.hex", "BCM20702A1_001.002.014.1315.1361.hex"]),
    (0x15A9, 0x0016, &[]),
    (0x1690, 0x0737, &[]),
    (0x1690, 0x0742, &[]),
    (0x17EF, 0x7201, &[]),
    (0x185F, 0x2167, &["BCM43142A0_001.001.011.0277.0292.hex", "BCM43142A0_001.001.011.0197.0239.hex"]),
    (0x18E8, 0x6260, &[]),
    (0x19FF, 0x0239, &["BCM20702B0_002.001.014.0527.0557.hex", "BCM20702B0_002.001.014.0527.0607.hex"]),
    (0x1D4D, 0x0004, &[]),
    (0x2B54, 0x5600, &["BCM4356A2_001.003.015.0082.0258.hex", "BCM4356A2_001.003.015.0046.0154.hex"]),
    (0x2B54, 0x5601, &["BCM4356A2_001.003.015.0082.0259.hex"]),
    (0x2B54, 0x5602, &["BCM4356A2_001.003.015.0082.0257.hex"]),
    (0x413C, 0x02B0, &[]),
    (0x413C, 0x8143, &["BCM20702A1_001.002.014.1443.1449.hex", "BCM20702A1_001.002.014.1315.1337.hex"]),
    (0x413C, 0x8156, &[]),
    (0x413C, 0x8160, &[]),
    (0x413C, 0x8187, &[]),
    (0x413C, 0x8197, &["BCM20702A1_001.002.014.1443.1447.hex", "BCM20702A1_001.002.014.1315.1411.hex"]),
];

/// An adapter of Broadcom's Windows driver
pub fn is_broadcom_usb(vid: u16, pid: u16) -> bool {
    ADAPTERS.iter().any(|a| (a.0, a.1) == (vid, pid))
}

/// The patch files for an adapter, newest driver first; None for an
/// adapter the driver loads no patch into
pub fn patch_files(vid: u16, pid: u16) -> Option<&'static [&'static str]> {
    ADAPTERS.iter().find(|a| (a.0, a.1) == (vid, pid)).map(|a| a.2).filter(|f| !f.is_empty())
}

const OP_READ_VERSION: u16 = 0xFC79;
const OP_DOWNLOAD_MODE: u16 = 0xFC2E;
const OP_WRITE_RAM: u16 = 0xFC4C;
const OP_LAUNCH: u16 = 0xFC4E;
const OP_CM3: u16 = 0xFC4F;

/// Launch: the address 0xFFFFFFFF
const LAUNCH_ADDRESS: u32 = 0xFFFF_FFFF;
/// The vendor event after the launch
const STARTED: [u8; 2] = [0x1E, 0x00];
/// The chip whose patch needs the three commands before the download
const CM3_CHIP: &str = "BCM43142A0";
/// Those commands, as the driver has them
const CM3_WRITE_1: [u8; 8] = [0x70, 0x04, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00];
const CM3_COMMAND: [u8; 14] = [0x01, 0x00, 0xD4, 0x7E, 0x02, 0x00, 0x8E, 0xF0, 0x84, 0xBA, 0x00, 0x00, 0x00, 0x00];
const CM3_WRITE_2: [u8; 24] = [
    0xE0, 0x63, 0x0B, 0x00, 0x04, 0x20, 0xC5, 0xF8, 0x84, 0x03, 0x0F, 0xB4, 0x4A, 0xF7, 0x44, 0xF8, 0x0F, 0xBC, 0x71,
    0xF7, 0x74, 0xBD, 0x00, 0x00,
];
/// The longest data record: its Write RAM has 4 + 251 parameter bytes
const MAX_RECORD: usize = 251;

/// Each answer, as the driver waits for it
pub const ANSWER_TIMEOUT_MS: u32 = 2000;
/// After the download mode's answer
pub const DOWNLOAD_MODE_WAIT_MS: u32 = 100;

/// The data records of an Intel HEX patch: address and bytes, in the
/// file's order; an error if it is not one or is cut short
pub fn parse_hex(data: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, &'static str> {
    let mut upper = 0u32;
    let mut records = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let hex = line.strip_prefix(b":").ok_or("a line is not a record")?;
        if hex.len() % 2 != 0 {
            return Err("a record has an odd number of digits");
        }
        let bytes: Vec<u8> = hex
            .chunks(2)
            .map(|d| std::str::from_utf8(d).ok().and_then(|s| u8::from_str_radix(s, 16).ok()))
            .collect::<Option<_>>()
            .ok_or("a record is not hexadecimal")?;
        let [len, hi, lo, kind, ..] = bytes[..] else { return Err("a record is too short") };
        if bytes.len() != 5 + len as usize {
            return Err("a record's length is wrong");
        }
        if bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b)) != 0 {
            return Err("a record's checksum is wrong");
        }
        let body = &bytes[4..bytes.len() - 1];
        match kind {
            0x00 if body.len() > MAX_RECORD => return Err("a record is too long"),
            0x00 => records.push((upper << 16 | u32::from(hi) << 8 | u32::from(lo), body.to_vec())),
            0x04 if body.len() == 2 => upper = u32::from(body[0]) << 8 | u32::from(body[1]),
            0x01 if records.is_empty() => return Err("no data"),
            0x01 => return Ok(records),
            _ => return Err("a record of a type the driver does not take"),
        }
    }
    Err("no end record")
}

/// The build a patch file's name gives: the number after its last dot
/// ("BCM20702A1_001.002.014.1443.1467.hex": 1467)
pub fn build_number(file: &str) -> Option<u16> {
    let stem = file.strip_suffix(".hex").or_else(|| file.strip_suffix(".HEX"))?;
    stem.rsplit('.').next()?.parse().ok()
}

/// The chip a patch file is for, its name's start ("BCM20702A1")
fn chip_of(file: &str) -> &str {
    file.split('_').next().unwrap_or(file)
}

/// Where the loader remembers per adapter (VID:PID) that its patch failed,
/// with the app version (the app keeps it in adapter_caps.json)
pub trait PatchMarks: Send + Sync {
    /// The app version the patch failed with on this adapter, if it did
    fn failed_with(&self, vid: u16, pid: u16) -> Option<String>;
    /// Marks the adapter with this app version; None drops the mark
    fn set_failed(&self, vid: u16, pid: u16, version: Option<&str>);
}

/// The patch to send, read and checked before anything is sent
struct Patch {
    file: String,
    build: u16,
    cm3: bool,
    writes: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Descriptor,
    Version,
    /// The BCM43142A0's commands before the download: which one
    Prepare(usize),
    DownloadMode,
    DownloadModeWait,
    Writing(usize),
    Launch,
    Started,
    Check,
    Done,
}

pub struct Loader {
    read_file: FileReader,
    patch: Option<Patch>,
    state: State,
    started: Instant,
    /// The failed-patch marks and the app version (None: not kept)
    marks: Option<(Arc<dyn PatchMarks>, String)>,
    /// The adapter's VID:PID, once read
    usb: Option<(u16, u16)>,
    /// A command that changes the chip was sent: a failure from here on
    /// needs a power cycle
    changed: bool,
}

impl Loader {
    pub fn new(read_file: FileReader) -> Loader {
        Loader {
            read_file,
            patch: None,
            state: State::Idle,
            started: Instant::now(),
            marks: None,
            usb: None,
            changed: false,
        }
    }

    /// Keeps the failed-patch marks there, for this app version
    pub fn with_marks(mut self, marks: Arc<dyn PatchMarks>, app_version: &str) -> Loader {
        self.marks = Some((marks, app_version.to_owned()));
        self
    }

    fn set_mark(&self, failed: bool) {
        if let (Some((marks, version)), Some((vid, pid))) = (&self.marks, self.usb) {
            marks.set_failed(vid, pid, failed.then_some(version.as_str()));
        }
    }

    /// The patch failed on this adapter with this app version before; a
    /// mark of another version is dropped
    fn marked(&self, vid: u16, pid: u16) -> bool {
        let Some((marks, version)) = &self.marks else { return false };
        match marks.failed_with(vid, pid) {
            Some(v) if v == *version => true,
            Some(v) => {
                tracing::info!("broadcom {vid:04X}:{pid:04X}: the patch failed with version {v}, trying it again");
                marks.set_failed(vid, pid, None);
                false
            }
            None => false,
        }
    }

    /// A failure: before anything changed in the chip, its ROM runs as it
    /// would without this loader; after, the adapter needs a power cycle,
    /// and the next starts skip the patch
    fn fail(&mut self, error: LoadError) -> Vec<Action> {
        if !self.changed {
            tracing::warn!("broadcom {}: {error}; nothing changed in the chip, its ROM runs", self.chip());
            return self.finish(Ok(Outcome::Rom));
        }
        tracing::warn!("broadcom {}: {error}; the next starts run its ROM", self.chip());
        self.set_mark(true);
        self.finish(Err(LoadError::Replug(Box::new(error))))
    }

    fn chip(&self) -> &str {
        self.patch.as_ref().map_or("adapter", |p| chip_of(&p.file))
    }

    fn finish(&mut self, result: Result<Outcome, LoadError>) -> Vec<Action> {
        tracing::debug!(
            "broadcom {}: loader finished after {} ms: {result:?}",
            self.chip(),
            self.started.elapsed().as_millis()
        );
        self.state = State::Done;
        vec![Action::Done(result)]
    }

    fn send(&mut self, state: State, opcode: u16, params: &[u8]) -> Vec<Action> {
        // Every command but the version read changes the chip
        self.changed |= opcode != OP_READ_VERSION;
        self.state = state;
        vec![Action::Command(hci_command(opcode, params)), Action::Timer(ANSWER_TIMEOUT_MS)]
    }

    /// The patch for this adapter: the first of its files (newest driver
    /// first) that is there and reads; None when none is there
    fn load_patch(&mut self, files: &[&str]) -> Option<Result<Patch, LoadError>> {
        let mut bad = None;
        for &file in files {
            let Some(data) = (self.read_file)(file) else { continue };
            match parse_hex(&data) {
                Ok(records) => {
                    let Some(build) = build_number(file) else {
                        bad = Some((file, "no build number in its name"));
                        continue;
                    };
                    let writes = records
                        .iter()
                        .map(|(address, bytes)| {
                            hci_command(OP_WRITE_RAM, &[&address.to_le_bytes()[..], bytes].concat())
                        })
                        .collect();
                    let cm3 = chip_of(file) == CM3_CHIP;
                    return Some(Ok(Patch { file: file.to_owned(), build, cm3, writes }));
                }
                Err(why) => {
                    tracing::warn!("broadcom: {file} ({} bytes) skipped: {why}", data.len());
                    bad = Some((file, why));
                }
            }
        }
        bad.map(|(file, why)| Err(LoadError::BadFile { file: file.to_owned(), why }))
    }

    fn on_descriptor(&mut self, result: Result<Vec<u8>, String>) -> Vec<Action> {
        let (vid, pid) = match result {
            Ok(d) if d.len() >= 12 => (u16::from_le_bytes([d[8], d[9]]), u16::from_le_bytes([d[10], d[11]])),
            other => {
                tracing::warn!("broadcom: the device descriptor could not be read: {other:02X?}");
                return self.fail(LoadError::Host("the USB device descriptor could not be read"));
            }
        };
        let Some(files) = patch_files(vid, pid) else {
            tracing::info!("broadcom {vid:04X}:{pid:04X}: Broadcom's driver loads no patch into it, its ROM runs");
            return self.finish(Ok(Outcome::Rom));
        };
        self.usb = Some((vid, pid));
        if self.marked(vid, pid) {
            tracing::info!("broadcom {vid:04X}:{pid:04X}: the patch failed on this adapter before; its ROM runs");
            return self.finish(Ok(Outcome::Rom));
        }
        let patch = match self.load_patch(files) {
            Some(Ok(p)) => p,
            Some(Err(e)) => return self.fail(e),
            None => {
                tracing::warn!(
                    "broadcom {vid:04X}:{pid:04X}: {} not found (Broadcom's Windows Bluetooth driver); its ROM runs",
                    files[0]
                );
                return self.finish(Ok(Outcome::Rom));
            }
        };
        tracing::info!(
            "broadcom {vid:04X}:{pid:04X}: {}, build {}, {} RAM writes{}",
            patch.file,
            patch.build,
            patch.writes.len(),
            if patch.cm3 { ", with the BCM43142A0 commands" } else { "" }
        );
        self.patch = Some(patch);
        self.send(State::Version, OP_READ_VERSION, &[])
    }

    fn on_version(&mut self, ret: &[u8]) -> Vec<Action> {
        let Some(running) = ret.get(4..6).map(|b| u16::from_le_bytes([b[0], b[1]])) else {
            return self.fail(LoadError::BadVersion);
        };
        let Some(patch) = &self.patch else { return Vec::new() };
        tracing::info!("broadcom {}: version {ret:02X?}, build {running} runs", chip_of(&patch.file));
        if running == patch.build {
            return self.finish(Ok(Outcome::AlreadyRunning));
        }
        if patch.cm3 { self.prepare(0) } else { self.send(State::DownloadMode, OP_DOWNLOAD_MODE, &[]) }
    }

    /// The BCM43142A0's command `step` before the download, then the
    /// download mode
    fn prepare(&mut self, step: usize) -> Vec<Action> {
        match step {
            0 => self.send(State::Prepare(0), OP_WRITE_RAM, &CM3_WRITE_1),
            1 => self.send(State::Prepare(1), OP_CM3, &CM3_COMMAND),
            2 => self.send(State::Prepare(2), OP_WRITE_RAM, &CM3_WRITE_2),
            _ => self.send(State::DownloadMode, OP_DOWNLOAD_MODE, &[]),
        }
    }

    fn write(&mut self, next: usize) -> Vec<Action> {
        let Some(patch) = &self.patch else { return Vec::new() };
        let Some(command) = patch.writes.get(next).cloned() else {
            return self.send(State::Launch, OP_LAUNCH, &LAUNCH_ADDRESS.to_le_bytes());
        };
        self.state = State::Writing(next);
        vec![Action::BulkCommand(command), Action::Timer(ANSWER_TIMEOUT_MS)]
    }

    /// The opcode the state waits for
    fn awaited(&self) -> Option<u16> {
        match self.state {
            State::Version | State::Check => Some(OP_READ_VERSION),
            State::Prepare(1) => Some(OP_CM3),
            State::Prepare(_) | State::Writing(_) => Some(OP_WRITE_RAM),
            State::DownloadMode => Some(OP_DOWNLOAD_MODE),
            State::Launch => Some(OP_LAUNCH),
            _ => None,
        }
    }

    fn on_complete(&mut self, opcode: u16, status: u8, ret: &[u8]) -> Vec<Action> {
        if self.awaited() != Some(opcode) {
            // A late or unrelated answer
            return Vec::new();
        }
        if status != 0 && self.state != State::Check {
            return self.fail(LoadError::Command { opcode, status });
        }
        match self.state {
            State::Version => self.on_version(ret),
            State::Prepare(step) => self.prepare(step + 1),
            State::DownloadMode => {
                self.state = State::DownloadModeWait;
                vec![Action::Timer(DOWNLOAD_MODE_WAIT_MS)]
            }
            State::Writing(next) => self.write(next + 1),
            State::Launch => {
                self.state = State::Started;
                vec![Action::Timer(ANSWER_TIMEOUT_MS)]
            }
            // The driver only logs this answer
            State::Check => {
                let build = ret.get(4..6).map(|b| u16::from_le_bytes([b[0], b[1]]));
                let Some(patch) = self.patch.take() else { return Vec::new() };
                tracing::info!(
                    "broadcom {}: patch started (status 0x{status:02X}, build {build:?} runs)",
                    chip_of(&patch.file)
                );
                self.set_mark(false);
                self.finish(Ok(Outcome::Loaded { firmware: patch.file, detail: None }))
            }
            _ => Vec::new(),
        }
    }
}

impl ChipLoader for Loader {
    fn vendor(&self) -> &'static str {
        "Broadcom"
    }

    fn start(&mut self) -> Vec<Action> {
        self.state = State::Descriptor;
        vec![Action::UsbControl(ControlRequest::device_descriptor())]
    }

    fn on_control(&mut self, result: Result<Vec<u8>, String>) -> Vec<Action> {
        match self.state {
            State::Descriptor => self.on_descriptor(result),
            _ => Vec::new(),
        }
    }

    fn on_event(&mut self, event: &[u8]) -> Vec<Action> {
        match parse_event(event) {
            HciEvent::Complete { opcode, status, ret } => self.on_complete(opcode, status, ret),
            HciEvent::Status { opcode, status } if status != 0 && self.awaited() == Some(opcode) => {
                self.fail(LoadError::Command { opcode, status })
            }
            HciEvent::Vendor(params) if self.state == State::Started && params.first() == Some(&STARTED[0]) => {
                match params.get(1) {
                    Some(&0) => self.send(State::Check, OP_READ_VERSION, &[]),
                    code => self.fail(LoadError::Rejected(code.copied().unwrap_or(0xFF))),
                }
            }
            _ => Vec::new(),
        }
    }

    fn on_timeout(&mut self) -> Vec<Action> {
        let step = match self.state {
            State::DownloadModeWait => return self.write(0),
            State::Version => "read version",
            State::Prepare(_) => "BCM43142A0 preparation",
            State::DownloadMode => "download mode",
            State::Writing(_) => "patch download",
            State::Launch => "patch launch",
            State::Started => "patch start event",
            State::Check => "read version after the patch",
            State::Idle | State::Descriptor | State::Done => return Vec::new(),
        };
        self.fail(LoadError::Timeout(step))
    }

    fn missing_file_hint(&self, file: &str) -> String {
        format!("{file} (from Broadcom's Windows Bluetooth driver)")
    }

    fn step(&self) -> &'static str {
        match self.state {
            State::Writing(_) => "download",
            _ => "",
        }
    }

    fn progress(&self) -> Option<(usize, usize)> {
        match (&self.state, &self.patch) {
            (State::Writing(next), Some(p)) => Some((*next, p.writes.len())),
            _ => None,
        }
    }
}

// ---- Broadcom's Windows driver ----

/// Where Broadcom's Windows driver keeps a patch on this PC, newest first
/// (crate::driver_files): any package of the driver store (OEM packages
/// name their INF differently) and System32\drivers, where its install
/// copies the patches
fn driver_files(windows: &Path, name: &str) -> Vec<PathBuf> {
    let names = [name.to_owned()];
    driver_files::find(windows, driver_files::Names::Exact(&names), &driver_files::any_package, &[])
}

/// A patch file of Broadcom's Windows driver, if it is installed: the
/// newest copy that reads as a patch
pub fn from_windows_driver(name: &str) -> Option<Vec<u8>> {
    if name.contains(['\\', '/']) {
        return None;
    }
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    driver_files(&windows, name).iter().find_map(|path| {
        let data = std::fs::read(path).ok().filter(|d| parse_hex(d).is_ok())?;
        tracing::info!("broadcom: {name} from Broadcom's driver ({}), {} bytes", file_name(path), data.len());
        Some(data)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::bringup::sim::{self, SimController, command_complete, vendor_event};
    use crate::chips::{self, ChipVendor};

    /// One Intel HEX record with its checksum
    fn record(kind: u8, address: u16, data: &[u8]) -> String {
        let mut bytes = vec![data.len() as u8, (address >> 8) as u8, address as u8, kind];
        bytes.extend_from_slice(data);
        let sum = bytes.iter().fold(0u8, |s, b| s.wrapping_add(*b));
        bytes.push(sum.wrapping_neg());
        format!(":{}\r\n", bytes.iter().map(|b| format!("{b:02X}")).collect::<String>())
    }

    /// A patch: data at 0x0009_0000 (300 bytes in two records) and one
    /// record at 0x0021_0010, out of address order as some real files are
    fn sample_hex() -> Vec<u8> {
        let mut f = record(0x04, 0, &[0x00, 0x21]);
        f += &record(0x00, 0x0010, &[0xC1; 12]);
        f += &record(0x04, 0, &[0x00, 0x09]);
        f += &record(0x00, 0x0000, &[0xA1; 251]);
        f += &record(0x00, 0x00FB, &[0xB1; 49]);
        f += &record(0x01, 0, &[]);
        f.into_bytes()
    }

    /// The memory the sample writes
    fn sample_memory() -> BTreeMap<u32, u8> {
        let mut m = BTreeMap::new();
        for i in 0..12 {
            m.insert(0x0021_0010 + i, 0xC1);
        }
        for i in 0..300 {
            m.insert(0x0009_0000 + i, if i < 251 { 0xA1 } else { 0xB1 });
        }
        m
    }

    const BT400: (u16, u16) = (0x0B05, 0x17CB);
    const BT400_FILE: &str = "BCM20702A1_001.002.014.1443.1467.hex";
    const BCM43142: (u16, u16) = (0x0A5C, 0x21D7);
    const BCM43142_FILE: &str = "BCM43142A0_001.001.011.0311.0341.hex";

    /// A Broadcom ROM chip as the driver's checks expect it: it answers
    /// 0xFC79 with its build, takes RAM writes on the bulk pipe only after
    /// 0xFC2E, and after 0xFC4E runs the build of the patch it got
    struct Chip {
        usb: Result<(u16, u16), String>,
        build: u16,
        download_mode: bool,
        memory: BTreeMap<u32, u8>,
        /// The RAM writes taken on the control pipe (address, length)
        control_writes: Vec<(u32, usize)>,
        /// This opcode is answered with this status; or never (None)
        refuse: Option<(u16, Option<u8>)>,
        /// Answer the n-th bulk write never
        silent_write: Option<usize>,
        bulk_writes: usize,
        /// The event after the launch's answer (None: none)
        started: Option<Vec<u8>>,
        /// Its build after the launch
        patch_build: u16,
        /// 0xFC79 answers with fewer return bytes than the driver reads
        short_version: bool,
    }

    impl Chip {
        fn rom(usb: (u16, u16), patch_build: u16) -> Chip {
            Chip {
                usb: Ok(usb),
                build: 0x0F5A,
                download_mode: false,
                memory: BTreeMap::new(),
                control_writes: Vec::new(),
                refuse: None,
                silent_write: None,
                bulk_writes: 0,
                started: Some(vendor_event(&STARTED)),
                patch_build,
                short_version: false,
            }
        }

        fn version(&self) -> Vec<u8> {
            if self.short_version {
                return vec![0x05, 0x01];
            }
            let mut ret = vec![0x05, 0x01, 0x30, 0x03];
            ret.extend_from_slice(&self.build.to_le_bytes());
            ret
        }
    }

    impl SimController for Chip {
        fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
            let (opcode, p) = (sim::opcode(c), sim::params(c));
            match self.refuse {
                Some((o, None)) if o == opcode => return Vec::new(),
                Some((o, Some(status))) if o == opcode => return vec![command_complete(opcode, status, &[])],
                _ => {}
            }
            match opcode {
                OP_READ_VERSION if p.is_empty() => vec![command_complete(opcode, 0, &self.version())],
                OP_DOWNLOAD_MODE if p.is_empty() => {
                    self.download_mode = true;
                    vec![command_complete(opcode, 0, &[])]
                }
                OP_WRITE_RAM if p.len() >= 4 => {
                    self.control_writes.push((u32::from_le_bytes([p[0], p[1], p[2], p[3]]), p.len() - 4));
                    vec![command_complete(opcode, 0, &[])]
                }
                OP_CM3 if p.len() == 14 => vec![command_complete(opcode, 0, &[])],
                OP_LAUNCH if p == [0xFF; 4] && self.download_mode => {
                    self.download_mode = false;
                    self.build = self.patch_build;
                    let mut events = vec![command_complete(opcode, 0, &[])];
                    events.extend(self.started.clone());
                    events
                }
                _ => Vec::new(),
            }
        }

        fn bulk_command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
            let (opcode, p) = (sim::opcode(c), sim::params(c));
            if !self.download_mode || opcode != OP_WRITE_RAM || p.len() < 4 || c[2] as usize != p.len() {
                return Vec::new();
            }
            self.bulk_writes += 1;
            if self.silent_write == Some(self.bulk_writes - 1) {
                return Vec::new();
            }
            if let Some((OP_WRITE_RAM, status)) = self.refuse {
                return status.map(|s| vec![command_complete(opcode, s, &[])]).unwrap_or_default();
            }
            let address = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
            for (i, b) in p[4..].iter().enumerate() {
                self.memory.insert(address + i as u32, *b);
            }
            vec![command_complete(opcode, 0, &[])]
        }

        fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
            assert_eq!(*r, ControlRequest::device_descriptor());
            let (vid, pid) = self.usb.clone()?;
            let mut d = vec![18, 1, 0, 2, 0xFF, 1, 1, 64];
            d.extend_from_slice(&vid.to_le_bytes());
            d.extend_from_slice(&pid.to_le_bytes());
            d.extend_from_slice(&[0x12, 0x01, 1, 2, 3, 1]);
            Ok(d)
        }
    }

    /// Runs the loader with these files of the driver
    fn run(chip: &mut Chip, files: &[(&'static str, Vec<u8>)]) -> sim::SimRun {
        sim::run(&mut loader(files), chip)
    }

    /// A loader whose driver has these files (name, content)
    fn loader(files: &[(&'static str, Vec<u8>)]) -> Loader {
        let files: Vec<(&'static str, Vec<u8>)> = files.to_vec();
        Loader::new(Box::new(move |name| files.iter().find(|(n, _)| *n == name).map(|(_, d)| d.clone())))
    }

    #[test]
    fn the_records_are_read_in_the_files_order() {
        let r = parse_hex(&sample_hex()).unwrap();
        let summary: Vec<(u32, usize)> = r.iter().map(|(a, d)| (*a, d.len())).collect();
        assert_eq!(summary, [(0x0021_0010, 12), (0x0009_0000, 251), (0x0009_00FB, 49)]);
        // LF only, blank lines, and lines after the end record are fine
        let lf = String::from_utf8(sample_hex()).unwrap().replace("\r\n", "\n\n") + "garbage";
        assert_eq!(parse_hex(lf.as_bytes()).unwrap(), r);
    }

    #[test]
    fn a_broken_patch_is_refused() {
        let good = String::from_utf8(sample_hex()).unwrap();
        let end = record(0x01, 0, &[]);
        let cases: Vec<(String, &str)> = vec![
            (good.replace(":0C0010", ":0C0011"), "a record's checksum is wrong"),
            (good.replace(&end, ""), "no end record"),
            (end.clone(), "no data"),
            (record(0x02, 0, &[0x10, 0x00]) + &end, "a record of a type the driver does not take"),
            (record(0x04, 0, &[0x00]) + &end, "a record of a type the driver does not take"),
            (record(0x00, 0, &[0x55; 252]) + &end, "a record is too long"),
            (good.replacen(":", ":0", 1), "a record has an odd number of digits"),
            (good.replacen(":02", ":0G", 1), "a record is not hexadecimal"),
            (format!("x{good}"), "a line is not a record"),
            (":0000\r\n".to_string(), "a record is too short"),
            (":0300000001\r\n".to_string(), "a record's length is wrong"),
        ];
        for (file, why) in cases {
            assert_eq!(parse_hex(file.as_bytes()), Err(why), "{file}");
        }
    }

    #[test]
    fn the_build_is_the_last_number_of_the_name() {
        assert_eq!(build_number(BT400_FILE), Some(1467));
        assert_eq!(build_number("BCM4350C5_003.006.007.0145.2724.hex"), Some(2724));
        assert_eq!(build_number("BCM20702A1.bin"), None);
        assert_eq!(build_number("x.hex"), None);
        assert_eq!(chip_of(BCM43142_FILE), CM3_CHIP);
    }

    #[test]
    fn the_download_follows_the_drivers_sequence() {
        let mut chip = Chip::rom(BT400, 1467);
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: BT400_FILE.into(), detail: None }));
        assert_eq!(
            r.opcodes(),
            [OP_READ_VERSION, OP_DOWNLOAD_MODE, OP_WRITE_RAM, OP_WRITE_RAM, OP_WRITE_RAM, OP_LAUNCH, OP_READ_VERSION]
        );
        // The RAM writes alone on the bulk pipe, one per record, in the
        // file's order
        let bulk: Vec<bool> = r.commands.iter().map(|c| c.bulk).collect();
        assert_eq!(bulk, [false, false, true, true, true, false, false]);
        assert_eq!(r.commands[2].params()[..4], 0x0021_0010u32.to_le_bytes());
        assert_eq!(r.commands[3].params().len(), 255);
        assert_eq!(r.commands[5].params(), [0xFF; 4]);
        assert_eq!(chip.memory, sample_memory());
        assert!(chip.control_writes.is_empty());
        // 100 ms after the download mode, then 2 s for each answer
        assert_eq!(r.timers[..3], [ANSWER_TIMEOUT_MS, ANSWER_TIMEOUT_MS, DOWNLOAD_MODE_WAIT_MS]);
        assert_eq!(r.timeouts, 1);
        assert!(!r.bootloader);
        assert_eq!(r.controls, [ControlRequest::device_descriptor()]);
    }

    #[test]
    fn the_bcm43142a0_gets_its_three_commands_first() {
        let mut chip = Chip::rom(BCM43142, 341);
        let r = run(&mut chip, &[(BCM43142_FILE, sample_hex())]);
        assert!(matches!(r.result, Ok(Outcome::Loaded { .. })), "{:?}", r.result);
        assert_eq!(r.opcodes()[..5], [OP_READ_VERSION, OP_WRITE_RAM, OP_CM3, OP_WRITE_RAM, OP_DOWNLOAD_MODE]);
        assert!(r.commands[..5].iter().all(|c| !c.bulk));
        assert_eq!(chip.control_writes, [(0x0020_0470, 4), (0x000B_63E0, 20)]);
        assert_eq!(r.commands[1].bytes, [0x4C, 0xFC, 0x08, 0x70, 0x04, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(r.commands[2].bytes[..5], [0x4F, 0xFC, 0x0E, 0x01, 0x00]);
        assert_eq!(r.commands[3].bytes.len(), 27);
        assert_eq!(chip.memory, sample_memory());
    }

    #[test]
    fn a_running_patch_is_left_alone() {
        let mut chip = Chip::rom(BT400, 1467);
        chip.build = 1467;
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!(r.result, Ok(Outcome::AlreadyRunning));
        assert_eq!(r.opcodes(), [OP_READ_VERSION]);
    }

    #[test]
    fn without_the_drivers_patch_the_rom_runs_and_nothing_is_sent() {
        let r = run(&mut Chip::rom(BT400, 1467), &[]);
        assert_eq!(r.result, Ok(Outcome::Rom));
        assert!(r.commands.is_empty());
        // An adapter the driver loads no patch into, and one it does not know
        for usb in [(0x0A5C, 0x21E4), (0x0A5C, 0x5800)] {
            let r = run(&mut Chip::rom(usb, 1467), &[(BT400_FILE, sample_hex())]);
            assert_eq!(r.result, Ok(Outcome::Rom));
            assert!(r.commands.is_empty());
        }
    }

    #[test]
    fn an_older_drivers_file_is_taken_when_the_newest_is_not_there() {
        let files = patch_files(BT400.0, BT400.1).unwrap();
        assert!(files.len() > 1, "{files:?}");
        let mut chip = Chip::rom(BT400, build_number(files[1]).unwrap());
        let r = run(&mut chip, &[(files[1], sample_hex())]);
        assert_eq!(r.result, Ok(Outcome::Loaded { firmware: files[1].into(), detail: None }));
    }

    /// A failure from the download mode on: the start fails with the
    /// advice to replug the adapter
    fn replug(e: LoadError) -> Result<Outcome, LoadError> {
        Err(LoadError::Replug(Box::new(e)))
    }

    #[test]
    fn a_broken_file_sends_nothing() {
        let mut broken = sample_hex();
        broken[10] ^= 1;
        // Nothing changed in the chip: its ROM runs
        let r = run(&mut Chip::rom(BT400, 1467), &[(BT400_FILE, broken.clone())]);
        assert_eq!(r.result, Ok(Outcome::Rom));
        assert!(r.commands.is_empty());
        // A good copy of an older driver's file is taken instead
        let files = patch_files(BT400.0, BT400.1).unwrap();
        let r = run(&mut Chip::rom(BT400, 0), &[(files[0], broken), (files[1], sample_hex())]);
        assert!(matches!(&r.result, Ok(Outcome::Loaded { firmware, .. }) if firmware == files[1]), "{:?}", r.result);
    }

    #[test]
    fn a_failed_version_read_leaves_the_rom_running() {
        // Refused, unanswered, of another shape: only the read is sent
        for refuse in [Some(0x01), None] {
            let mut chip = Chip::rom(BT400, 1467);
            chip.refuse = Some((OP_READ_VERSION, refuse));
            let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
            assert_eq!((r.result.clone(), r.opcodes()), (Ok(Outcome::Rom), vec![OP_READ_VERSION]), "{refuse:?}");
        }
        let mut chip = Chip::rom(BT400, 1467);
        chip.short_version = true;
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!((r.result.clone(), r.opcodes()), (Ok(Outcome::Rom), vec![OP_READ_VERSION]));
        // The BCM43142A0 too
        let mut chip = Chip::rom(BCM43142, 341);
        chip.refuse = Some((OP_READ_VERSION, None));
        let r = run(&mut chip, &[(BCM43142_FILE, sample_hex())]);
        assert_eq!((r.result.clone(), r.opcodes()), (Ok(Outcome::Rom), vec![OP_READ_VERSION]));
    }

    #[test]
    fn a_rejected_command_ends_the_download() {
        for (opcode, sent) in [(OP_DOWNLOAD_MODE, 2), (OP_WRITE_RAM, 3), (OP_LAUNCH, 6)] {
            let mut chip = Chip::rom(BT400, 1467);
            chip.refuse = Some((opcode, Some(0x0C)));
            let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
            assert_eq!(r.result, replug(LoadError::Command { opcode, status: 0x0C }), "{opcode:04X}");
            assert_eq!(r.commands.len(), sent, "{opcode:04X}");
        }
        // The BCM43142A0's command before the download: its RAM was
        // written already
        let mut chip = Chip::rom(BCM43142, 341);
        chip.refuse = Some((OP_CM3, Some(0x12)));
        let r = run(&mut chip, &[(BCM43142_FILE, sample_hex())]);
        assert_eq!(r.result, replug(LoadError::Command { opcode: OP_CM3, status: 0x12 }));
        assert!(!r.opcodes().contains(&OP_DOWNLOAD_MODE));
    }

    #[test]
    fn each_wait_times_out() {
        let silent = |opcode: u16| {
            let mut chip = Chip::rom(BT400, 1467);
            chip.refuse = Some((opcode, None));
            run(&mut chip, &[(BT400_FILE, sample_hex())]).result
        };
        assert_eq!(silent(OP_DOWNLOAD_MODE), replug(LoadError::Timeout("download mode")));
        assert_eq!(silent(OP_LAUNCH), replug(LoadError::Timeout("patch launch")));
        let mut chip = Chip::rom(BT400, 1467);
        chip.silent_write = Some(1);
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!(r.result, replug(LoadError::Timeout("patch download")));
        assert!(!r.opcodes().contains(&OP_LAUNCH));
        let mut chip = Chip::rom(BT400, 1467);
        chip.started = None;
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!(r.result, replug(LoadError::Timeout("patch start event")));
        // The version read after the start: the chip falls silent then
        struct Mute(Chip);
        impl SimController for Mute {
            fn command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                if sim::opcode(c) == OP_READ_VERSION && self.0.build == self.0.patch_build {
                    return Vec::new();
                }
                self.0.command(c)
            }
            fn bulk_command(&mut self, c: &[u8]) -> Vec<Vec<u8>> {
                self.0.bulk_command(c)
            }
            fn control(&mut self, r: &ControlRequest) -> Result<Vec<u8>, String> {
                self.0.control(r)
            }
        }
        let mut loader = loader(&[(BT400_FILE, sample_hex())]);
        let r = sim::run(&mut loader, &mut Mute(Chip::rom(BT400, 1467)));
        assert_eq!(r.result, replug(LoadError::Timeout("read version after the patch")));
        let mut chip = Chip::rom(BCM43142, 341);
        chip.refuse = Some((OP_CM3, None));
        let r = run(&mut chip, &[(BCM43142_FILE, sample_hex())]);
        assert_eq!(r.result, replug(LoadError::Timeout("BCM43142A0 preparation")));
    }

    #[test]
    fn a_bad_start_event_is_a_rejection() {
        let mut chip = Chip::rom(BT400, 1467);
        chip.started = Some(vendor_event(&[0x1E, 0x03]));
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!(r.result, replug(LoadError::Rejected(0x03)));
        // Another vendor event is not the one awaited
        let mut chip = Chip::rom(BT400, 1467);
        chip.started = Some(vendor_event(&[0x08, 0x00]));
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!(r.result, replug(LoadError::Timeout("patch start event")));
    }

    #[test]
    fn a_failed_descriptor_read_sends_nothing() {
        let mut chip = Chip::rom(BT400, 1467);
        chip.usb = Err("transfer failed".into());
        let r = run(&mut chip, &[(BT400_FILE, sample_hex())]);
        assert_eq!(r.result, Ok(Outcome::Rom));
        assert!(r.commands.is_empty());
    }

    /// The marks in memory, as adapter_caps.json keeps them
    #[derive(Default)]
    struct Marks(std::sync::Mutex<BTreeMap<(u16, u16), String>>);

    impl PatchMarks for Marks {
        fn failed_with(&self, vid: u16, pid: u16) -> Option<String> {
            self.0.lock().unwrap().get(&(vid, pid)).cloned()
        }

        fn set_failed(&self, vid: u16, pid: u16, version: Option<&str>) {
            let mut m = self.0.lock().unwrap();
            match version {
                Some(v) => m.insert((vid, pid), v.to_owned()),
                None => m.remove(&(vid, pid)),
            };
        }
    }

    fn run_marked(chip: &mut Chip, marks: &Arc<Marks>, version: &str) -> sim::SimRun {
        let mut loader = loader(&[(BT400_FILE, sample_hex())]).with_marks(marks.clone(), version);
        sim::run(&mut loader, chip)
    }

    #[test]
    fn a_late_failure_marks_the_adapter_and_the_next_start_runs_its_rom() {
        let marks = Arc::new(Marks::default());
        let mut chip = Chip::rom(BT400, 1467);
        chip.refuse = Some((OP_LAUNCH, None));
        let r = run_marked(&mut chip, &marks, "3.0.2");
        assert_eq!(r.result, replug(LoadError::Timeout("patch launch")));
        assert_eq!(marks.failed_with(BT400.0, BT400.1).as_deref(), Some("3.0.2"));
        // After the replug: nothing is sent, the ROM runs, the mark stays
        let r = run_marked(&mut Chip::rom(BT400, 1467), &marks, "3.0.2");
        assert_eq!(r.result, Ok(Outcome::Rom));
        assert!(r.commands.is_empty());
        assert_eq!(marks.failed_with(BT400.0, BT400.1).as_deref(), Some("3.0.2"));
    }

    #[test]
    fn an_early_failure_leaves_no_mark() {
        let marks = Arc::new(Marks::default());
        let mut chip = Chip::rom(BT400, 1467);
        chip.refuse = Some((OP_READ_VERSION, Some(0x01)));
        assert_eq!(run_marked(&mut chip, &marks, "3.0.2").result, Ok(Outcome::Rom));
        assert_eq!(marks.failed_with(BT400.0, BT400.1), None);
    }

    #[test]
    fn another_app_version_tries_again_and_success_drops_the_mark() {
        let marks = Arc::new(Marks::default());
        marks.set_failed(BT400.0, BT400.1, Some("3.0.1"));
        let r = run_marked(&mut Chip::rom(BT400, 1467), &marks, "3.0.2");
        assert!(matches!(r.result, Ok(Outcome::Loaded { .. })), "{:?}", r.result);
        assert_eq!(marks.failed_with(BT400.0, BT400.1), None);
        // Another version's mark, then this version fails: its own mark
        marks.set_failed(BT400.0, BT400.1, Some("3.0.1"));
        let mut chip = Chip::rom(BT400, 1467);
        chip.refuse = Some((OP_DOWNLOAD_MODE, Some(0x0C)));
        assert!(matches!(run_marked(&mut chip, &marks, "3.0.2").result, Err(LoadError::Replug(_))));
        assert_eq!(marks.failed_with(BT400.0, BT400.1).as_deref(), Some("3.0.2"));
        // A later release tries again and its success drops the mark
        let r = run_marked(&mut Chip::rom(BT400, 1467), &marks, "3.0.3");
        assert!(matches!(r.result, Ok(Outcome::Loaded { .. })), "{:?}", r.result);
        assert_eq!(marks.failed_with(BT400.0, BT400.1), None);
    }

    #[test]
    fn the_replug_error_tells_the_fix() {
        let text = LoadError::Replug(Box::new(LoadError::Timeout("patch launch"))).to_string();
        assert!(text.contains("Unplug the adapter and plug it back in."), "{text}");
        assert!(text.contains("shut down the PC completely"), "{text}");
    }

    #[test]
    fn the_progress_counts_the_ram_writes() {
        let mut loader = loader(&[(BT400_FILE, sample_hex())]);
        let mut chip = Chip::rom(BT400, 1467);
        let _ = loader.start();
        let _ = loader.on_control(chip.control(&ControlRequest::device_descriptor()));
        let _ = loader.on_event(&command_complete(OP_READ_VERSION, 0, &chip.version()));
        let _ = loader.on_event(&command_complete(OP_DOWNLOAD_MODE, 0, &[]));
        let _ = loader.on_timeout();
        assert_eq!((loader.step(), loader.progress()), ("download", Some((0, 3))));
        let _ = loader.on_event(&command_complete(OP_WRITE_RAM, 0, &[]));
        assert_eq!(loader.progress(), Some((1, 3)));
    }

    #[test]
    fn the_table_is_sorted_unique_and_names_hex_files() {
        assert!(ADAPTERS.windows(2).all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1)));
        for (_, _, files) in ADAPTERS {
            for f in *files {
                assert!(build_number(f).is_some() && f.starts_with("BCM"), "{f}");
            }
        }
    }

    /// Every adapter of the driver's INFs is a Broadcom one, none of them
    /// in another vendor's tables
    #[test]
    fn the_drivers_adapters_are_broadcom() {
        let realtek = crate::realtek_usb_firmware();
        for &(vid, pid, _) in ADAPTERS {
            assert_eq!(chips::resolve_realtek_pid(vid, pid, realtek), None, "{vid:04X}:{pid:04X}");
            assert_eq!(chips::vendor_for_usb(vid, pid, None), ChipVendor::Broadcom, "{vid:04X}:{pid:04X}");
            assert!(chips::chip_supported(ChipVendor::Broadcom, vid, pid));
            assert!(chips::is_bluetooth_vendor_vid(vid, pid));
        }
        // Qualcomm's driver lists it too
        assert!(!is_broadcom_usb(0x13D3, 0x3517));
        assert_eq!(chips::vendor_for_usb(0x13D3, 0x3517, None), ChipVendor::Qualcomm);
    }

    /// Against Broadcom's real files: `BCM_HEX_DIR=<folder with the
    /// driver's .hex files, searched recursively> cargo test -p bt-host
    /// real_hex -- --ignored --nocapture`
    #[test]
    #[ignore = "needs Broadcom's .hex files (BCM_HEX_DIR)"]
    fn real_hex_files() {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("hex")) {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(Path::new(&std::env::var("BCM_HEX_DIR").unwrap()), &mut files);
        let (mut records, mut bytes, mut downloaded) = (0, 0, 0);
        for p in &files {
            let name = file_name(p);
            let data = std::fs::read(p).unwrap();
            let r = parse_hex(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(build_number(&name).is_some(), "{name}");
            records += r.len();
            bytes += r.iter().map(|(_, d)| d.len()).sum::<usize>();
            // The whole download against the simulated chip
            let mut expected = BTreeMap::new();
            for (a, d) in &r {
                for (i, b) in d.iter().enumerate() {
                    expected.insert(a + i as u32, *b);
                }
            }
            // Files no INF names: parsed only
            let Some(usb) = ADAPTERS.iter().find(|a| a.2.contains(&name.as_str())).map(|a| (a.0, a.1)) else {
                continue;
            };
            downloaded += 1;
            let name: &'static str = Box::leak(name.into_boxed_str());
            let mut chip = Chip::rom(usb, build_number(name).unwrap());
            let run = run(&mut chip, &[(name, data)]);
            assert!(matches!(run.result, Ok(Outcome::Loaded { .. })), "{name}: {:?}", run.result);
            assert_eq!(chip.memory, expected, "{name}");
        }
        println!("{} files, {records} records, {bytes} bytes: all parsed, {downloaded} downloaded", files.len());
        assert!(!files.is_empty());
    }
}
