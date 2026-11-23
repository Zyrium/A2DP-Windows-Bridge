//! Group `migration`: the one-time move of the signed-in user's data into
//! the service's folder. The core in service mode cannot read the user's
//! folder, nor decrypt the user's pairing keys (DPAPI, the user's scope);
//! the agent runs as the user and can. The core asks once
//! ([`Request::Collect`]); the agent reads the files of the fixed list
//! [`Item::ALL`] in the user's folder, decrypts the secret ones and sends
//! their content; the core checks each as untrusted, encrypts the secrets
//! for its own account and records that the move ran
//! (bluecaster_core::service_data).
//!
//! Only what [`Item`] names crosses: the request has no paths, and the
//! agent cannot offer anything else (an unknown item fails to decode).
//! The keyboard and mouse list and the feature's state are not on the list
//! on purpose: a user-level process must not be able to add a keyboard to
//! the service. Neither are inputs of the elevated helpers or firmware.
//!
//! The decrypted keys cross the agent pipe once, in this reply. Nothing
//! logs a frame's body; the hub's pipe admits only this user and SYSTEM.

use serde::{Deserialize, Serialize};

use crate::{LocalHost, RemoteHost};

pub const GROUP: &str = "migration";

/// A file the move takes, by its place in the user's folder
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Item {
    Settings,
    Profiles,
    Devices,
    LeDevices,
    LeSets,
    DeviceCodecs,
    DeviceServices,
    LinkKeys,
    LeBonds,
    LeCsip,
    WindowsLeKeys,
}

impl Item {
    pub const ALL: [Item; 11] = [
        Item::Settings,
        Item::Profiles,
        Item::Devices,
        Item::LeDevices,
        Item::LeSets,
        Item::DeviceCodecs,
        Item::DeviceServices,
        Item::LinkKeys,
        Item::LeBonds,
        Item::LeCsip,
        Item::WindowsLeKeys,
    ];

    /// Its place below the folder, `/`-separated (bluecaster_core::paths
    /// has the same layout; its tests compare the two)
    pub fn relative_path(self) -> &'static str {
        match self {
            Item::Settings => "settings.json",
            Item::Profiles => "profiles.json",
            Item::Devices => "devices/devices.txt",
            Item::LeDevices => "devices/le_audio_devices.txt",
            Item::LeSets => "devices/le_audio_sets.txt",
            Item::DeviceCodecs => "devices/device_codecs.json",
            Item::DeviceServices => "devices/device_services.json",
            Item::LinkKeys => "pairing/link_keys.txt",
            Item::LeBonds => "pairing/le_audio_bonds.tlv",
            Item::LeCsip => "pairing/le_audio_csip.txt",
            Item::WindowsLeKeys => "pairing/windows_le_keys.txt",
        }
    }

    /// Encrypted for the user (win_sys::dpapi): the agent decrypts it
    pub fn secret(self) -> bool {
        matches!(self, Item::LinkKeys | Item::LeBonds | Item::LeCsip | Item::WindowsLeKeys)
    }

    /// The most it may hold (decrypted). Together they fit one frame
    /// (ipc::MAX_FRAME) as hex.
    pub fn max_bytes(self) -> usize {
        match self {
            Item::Settings | Item::DeviceCodecs | Item::DeviceServices => 256 << 10,
            Item::Profiles => 512 << 10,
            Item::Devices | Item::LeDevices | Item::LeSets => 64 << 10,
            Item::LinkKeys | Item::LeCsip | Item::WindowsLeKeys => 16 << 10,
            Item::LeBonds => 128 << 10,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Item::Settings => "settings",
            Item::Profiles => "profiles",
            Item::Devices => "devices",
            Item::LeDevices => "le_devices",
            Item::LeSets => "le_sets",
            Item::DeviceCodecs => "device_codecs",
            Item::DeviceServices => "device_services",
            Item::LinkKeys => "link_keys",
            Item::LeBonds => "le_bonds",
            Item::LeCsip => "le_csip",
            Item::WindowsLeKeys => "windows_le_keys",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Reply: `Vec<Entry>`, one per [`Item::ALL`]
    Collect,
}

/// What the agent found for one item
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Found {
    Missing,
    /// Larger than [`Item::max_bytes`]: not sent
    TooLarge,
    /// Not readable, or not decryptable by the user
    Unreadable,
    /// The content (decrypted), in hex
    Data {
        hex: String,
    },
}

impl std::fmt::Debug for Found {
    // Never the content: it may be keys
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Found::Missing => f.write_str("Missing"),
            Found::TooLarge => f.write_str("TooLarge"),
            Found::Unreadable => f.write_str("Unreadable"),
            Found::Data { hex } => write!(f, "Data({} bytes)", hex.len() / 2),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub item: Item,
    #[serde(flatten)]
    pub found: Found,
}

pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// The bytes of [`to_hex`]'s text; None for anything else (odd length,
/// another character)
pub fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let b = hex.as_bytes();
    let (pairs, rest) = b.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    let digit = |c: u8| char::from(c).to_digit(16).map(|d| d as u8);
    pairs.iter().map(|[hi, lo]| Some(digit(*hi)? << 4 | digit(*lo)?)).collect()
}

/// The facade's method. Blocks until the agent answers: not for the
/// BTstack thread or an audio thread.
pub trait MigrationHost {
    /// The user's data of the fixed list. Err: no agent, or it failed.
    fn migration_collect(&self) -> Result<Vec<Entry>, crate::HostError>;
}

impl MigrationHost for LocalHost {
    /// In-process there is no other folder to move from
    fn migration_collect(&self) -> Result<Vec<Entry>, crate::HostError> {
        Ok(Vec::new())
    }
}

impl MigrationHost for RemoteHost {
    fn migration_collect(&self) -> Result<Vec<Entry>, crate::HostError> {
        self.call(GROUP, &Request::Collect)
    }
}

/// [`MigrationHost::migration_collect`] of this process's host
pub fn collect() -> Result<Vec<Entry>, crate::HostError> {
    crate::host().migration_collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_refuses_other_text() {
        assert_eq!(to_hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(from_hex("00AB10").unwrap(), [0, 0xab, 0x10]);
        assert_eq!(from_hex("").unwrap(), Vec::<u8>::new());
        assert!(from_hex("abc").is_none() && from_hex("zz").is_none() && from_hex("é1").is_none());
    }

    #[test]
    fn the_list_is_fixed_and_fits_one_frame() {
        let total: usize = Item::ALL.iter().map(|i| i.max_bytes() * 2 + 64).sum();
        assert!(total < ipc::MAX_FRAME, "{total}");
        for i in Item::ALL {
            let p = i.relative_path();
            assert!(!p.contains("..") && !p.starts_with('/') && !p.contains(':') && !p.contains('\\'), "{p}");
            assert!(!p.contains("hid"), "the keyboard and mouse files never move by themselves: {p}");
        }
        // The shape on the wire; the content never in Debug
        let e = Entry { item: Item::LinkKeys, found: Found::Data { hex: "0a0b".into() } };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"item":"link_keys","state":"data","hex":"0a0b"}"#);
        assert_eq!(serde_json::from_str::<Entry>(&json).unwrap(), e);
        assert!(!format!("{e:?}").contains("0a0b"));
        // Something not on the list does not decode
        assert!(serde_json::from_str::<Entry>(r#"{"item":"hid_devices","state":"missing"}"#).is_err());
    }
}
