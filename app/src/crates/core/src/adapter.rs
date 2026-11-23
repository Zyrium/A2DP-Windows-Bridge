//! Which controller to open and how to bring it up: the Realtek chip for the
//! firmware download (setting, or the detected adapter as v1's "Auto"), the
//! Intel bootloader download and Broadcom PatchRAM, and the
//! AFH classification from the PC's Wi-Fi.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bt_host::afh;
use bt_host::chips::{self, ChipVendor};
use bt_host::{AfhClassification, AfhSource, HostConfig, TransportConfig};
use win_sys::usb_adapters::{self, UsbDevice};

use crate::settings::Settings;

/// A USB Bluetooth adapter as the UI and the stack setup see it
#[derive(Debug, Clone)]
pub struct Adapter {
    pub device: UsbDevice,
    /// Realtek PID for the firmware lookup (also for OEM adapters)
    pub realtek_pid: Option<u16>,
    pub vendor: ChipVendor,
    pub display_name: String,
}

/// Present USB devices that look like Bluetooth adapters (any driver).
/// `probe_winusb` opens WinUSB-bound ones for a moment (not while the stack
/// has the adapter open).
pub fn adapters(probe_winusb: bool) -> Vec<Adapter> {
    scan(probe_winusb).0
}

/// `adapters`, and how many USB devices Windows could not read at all
/// (`UsbDevice::unreadable`): one may be an adapter whose chip hung
pub fn scan(probe_winusb: bool) -> (Vec<Adapter>, usize) {
    let ids = bt_host::realtek_usb_firmware();
    let devices = match usb_adapters::enumerate(probe_winusb) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("cannot list USB devices: {e}");
            return (Vec::new(), 0);
        }
    };
    let unreadable = devices.iter().filter(|d| d.unreadable()).count();
    if unreadable > 0 {
        tracing::info!("{unreadable} USB device(s) Windows cannot read");
    }
    let adapters = devices
        .into_iter()
        .filter_map(|d| {
            let realtek_pid = chips::resolve_realtek_pid(d.vid, d.pid, ids);
            let vendor = chips::vendor_for_usb(d.vid, d.pid, realtek_pid);
            // A chip maker's VID alone (a vendor-class adapter without a
            // driver) unless the device says it is something else: a CSR
            // headset charging, a MediaTek phone
            let by_vid = chips::is_bluetooth_vendor_vid(d.vid, d.pid);
            let bluetooth = d.looks_like_bluetooth() || realtek_pid.is_some() || (by_vid && !d.other_class());
            if by_vid && !bluetooth {
                tracing::debug!(
                    "adapter: {:04X}:{:04X} {:?} skipped: {} VID, but class {:?} / functions {:?}",
                    d.vid,
                    d.pid,
                    d.name,
                    vendor.name(),
                    d.usb_class,
                    d.function_classes
                );
            }
            if bluetooth {
                tracing::debug!(
                    "adapter: {:04X}:{:04X} {:?} driver {:?}, realtek pid {realtek_pid:04X?}, vendor {}, class {:?}, problem {:?}, probe {:?}",
                    d.vid,
                    d.pid,
                    d.name,
                    d.driver_kind,
                    vendor.name(),
                    d.usb_class,
                    d.problem_code,
                    d.probe
                );
            } else {
                tracing::trace!("adapter: {:04X}:{:04X} {:?} skipped, not Bluetooth", d.vid, d.pid, d.name);
            }
            bluetooth.then(|| Adapter {
                display_name: chips::display_name(d.vid, d.pid, realtek_pid, vendor),
                device: d,
                realtek_pid,
                vendor,
            })
        })
        .collect();
    (adapters, unreadable)
}

/// A USB device Windows cannot read while no adapter is on WinUSB, after a
/// stack of BlueCaster ran on one: most likely that adapter, its chip hung
/// (issue #86, after a crash). Only a power cycle brings it back.
pub fn hung_adapter(unreadable: usize, winusb_present: bool, used_before: bool) -> bool {
    unreadable > 0 && !winusb_present && used_before
}

/// A stack of BlueCaster ran on an adapter of this PC at some time (it
/// remembered the controller's HCI version)
pub fn used_before(dir: &Path) -> bool {
    load_caps(dir).values().any(|c| c.hci_version.is_some())
}

/// Vendor-class (FF/01/01) controllers v1's WinUSB transport knew built in
/// (BTstack's list): they do not identify as Bluetooth class
const BUILTIN_VENDOR_CLASS: &[(u16, u16)] = &[
    (0x0A5C, 0x21E8), // BCM20702A0, DeLOCK Bluetooth 4.0
    (0x0B05, 0x17CB), // BCM20702A0, ASUS BT400
    (0x0A5C, 0x22BE), // BCM20702B0, generic class 1
    (0x2FE3, 0x0100), // nRF5x Zephyr USB HCI
    (0x2FE3, 0x000B),
];

/// VID:PIDs the USB transport opens besides Bluetooth-class devices (bt-host
/// adds BTstack's Realtek table, CSR and Intel): v1's built-in vendor-class
/// list, the OEM adapters with a Realtek chip, the adapters of Broadcom's
/// Windows driver (bt_host::broadcom), and the WinUSB adapters whose
/// interface 0 was found to be vendor-class Bluetooth with the HCI pipes
pub fn usb_known_ids(vendor_class: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut ids: Vec<(u16, u16)> = BUILTIN_VENDOR_CLASS.to_vec();
    ids.extend(chips::OEM_TABLE.iter().map(|m| (m.vid, m.pid)));
    // The adapters of Broadcom's Windows driver, some of them vendor-class
    let broadcom = bt_host::broadcom::ADAPTERS.iter().map(|a| (a.0, a.1));
    for id in broadcom.chain(vendor_class.iter().copied()) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// The firmware of Realtek's Windows driver that stands in for the firmware
/// folder's `fw_stem` ("rtl8761bu_fw", ".bin" allowed) where the driver on
/// this PC has the chip's in a format BTstack's download reads (issue #86).
/// No config file is needed with it.
pub fn realtek_driver_firmware(fw_stem: &str) -> Option<std::path::PathBuf> {
    bt_host::realtek_btnic::driver_firmware(fw_stem.strip_suffix(".bin").unwrap_or(fw_stem))
}

/// The same, from the driver package whose INF names the
/// adapter's USB VID:PID ([`bt_host::realtek_btnic::own_driver_firmware`])
pub fn realtek_own_driver_firmware(fw_stem: &str, vid: u16, pid: u16) -> Option<std::path::PathBuf> {
    bt_host::realtek_btnic::own_driver_firmware(fw_stem.strip_suffix(".bin").unwrap_or(fw_stem), vid, pid)
}

/// The adapter of `id` (Settings::adapter_id, any case) among those on
/// WinUSB, if no other program has it; None for an empty `id`
fn chosen_adapter<'a>(winusb: &[&'a Adapter], id: &str) -> Option<&'a Adapter> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    winusb.iter().copied().find(|a| a.device.instance_id.eq_ignore_ascii_case(id) && !a.device.in_use())
}

/// The adapter `id` (Settings::adapter_id's form) is on WinUSB and free,
/// or none is chosen: a stack started now opens that one, not the first
/// adapter found
pub fn chosen_present(id: &str) -> bool {
    if id.trim().is_empty() {
        return true;
    }
    let present = adapters(false);
    let winusb: Vec<&Adapter> =
        present.iter().filter(|a| a.device.driver_kind == win_sys::driver::DriverKind::WinUsb).collect();
    chosen_adapter(&winusb, id).is_some()
}

/// The adapter a stack opens first (Settings::adapter_id's form): for a
/// broadcast in developer mode the Auracast adapter when one is set,
/// otherwise the adapter to use
pub fn adapter_for(settings: &Settings, broadcast: bool) -> &str {
    let auracast = settings.auracast_adapter_id.trim();
    if broadcast && settings.debug_mode && !auracast.is_empty() { auracast } else { settings.adapter_id.trim() }
}

/// Fills the chipset part of `config` from the settings and the present
/// adapters
pub fn configure(config: &mut HostConfig, settings: &Settings, config_dir: &Path) {
    config.afh = Some(Box::new(WifiAfh::default()));
    if matches!(config.transport, TransportConfig::Tcp(_)) {
        return; // a virtual controller: no chipset, no firmware
    }
    let auto = chips::is_auto_chip(settings.bt_chip_pid, &settings.bt_chip_fw_stem);
    // The stack is not running yet (start and restart shut the old one
    // down first): the probe reads interface 0 of each WinUSB adapter, as
    // v1 did before opening one, and tells one another program holds
    let present = adapters(true);
    let winusb: Vec<&Adapter> =
        present.iter().filter(|a| a.device.driver_kind == win_sys::driver::DriverKind::WinUsb).collect();
    tracing::debug!(
        "adapter: {} present, {} on WinUSB, chip setting pid {:04X} stem {:?} (auto {auto})",
        present.len(),
        winusb.len(),
        settings.bt_chip_pid,
        settings.bt_chip_fw_stem
    );
    if let TransportConfig::Usb(sel) = &mut config.transport {
        let vendor_class: Vec<(u16, u16)> = winusb
            .iter()
            .filter(|a| a.device.winusb_bluetooth() == Some(true))
            .map(|a| (a.device.vid, a.device.pid))
            .collect();
        for (vid, pid) in &vendor_class {
            tracing::info!("vendor-class (FF/01/01) adapter {vid:04X}:{pid:04X}: the transport may open it");
        }
        for id in usb_known_ids(&vendor_class) {
            if !sel.known.contains(&id) {
                sel.known.push(id);
            }
        }
        tracing::debug!("adapter: usb selector {sel:?}");
    }
    // The adapter chosen in the settings, when it is on WinUSB and free: it
    // is opened first, and only its chip counts for the Realtek setup and
    // the vendor firmware loaders (an Intel card beside it on WinUSB would
    // otherwise send the Intel bootloader commands to the chosen one)
    let chosen = chosen_adapter(&winusb, &settings.adapter_id);
    if !settings.adapter_id.is_empty() && chosen.is_none() {
        // Not its instance ID: that ends in the adapter's serial number or
        // address, and logs are sent to others
        tracing::warn!("adapter: the chosen adapter is not on WinUSB or not free: the first one found is used");
    }
    if let (TransportConfig::Usb(sel), Some(a)) = (&mut config.transport, chosen) {
        tracing::info!("adapter: {:04X}:{:04X} (chosen) is opened first", a.device.vid, a.device.pid);
        sel.preferred = Some(a.device.instance_id.clone());
    }
    let detected: Vec<&Adapter> = chosen.map_or_else(|| winusb.clone(), |a| vec![a]);
    let pid = chips::resolve_chip_pid(
        settings.bt_chip_pid,
        &settings.bt_chip_fw_stem,
        detected.iter().map(|a| a.realtek_pid),
    );
    if pid != 0 {
        let chip_name = chips::realtek_chip_name(pid);
        // The adapter's own VID:PID when it differs from the PID BTstack is
        // given (a family's other PID, an OEM adapter): "0BDA:4853 -> 0xB85B"
        let usb = winusb
            .iter()
            .find(|a| a.realtek_pid == Some(pid) && (a.device.vid, a.device.pid) != (chips::REALTEK_VID, pid))
            .map(|a| format!("{:04X}:{:04X} -> ", a.device.vid, a.device.pid))
            .unwrap_or_default();
        tracing::info!(
            "chip: {} ({usb}0x{pid:04X}){}",
            chip_name.unwrap_or("unknown"),
            if auto { " (auto)" } else { "" }
        );
        if chip_name.is_none() && settings.bt_chip_fw_stem.is_empty() {
            tracing::warn!(
                "chip 0x{pid:04X} is not in BlueCaster's chip table: choose its firmware on the Adapter screen (Firmware), else no firmware file is checked or prepared for it"
            );
        }
        config.realtek_pid = pid;
        // The transport opens the first WinUSB adapter it can: with another
        // one on WinUSB too (a built-in Intel chosen by mistake), BTstack
        // would bring that one up with this chip's setup, and its quick init
        // read as "firmware not loaded". The adapter of this chip goes first.
        if let TransportConfig::Usb(sel) = &mut config.transport
            && chosen.is_none()
            && winusb.len() > 1
            && let Some(a) = winusb.iter().find(|a| a.realtek_pid == Some(pid) && !a.device.in_use())
        {
            tracing::info!(
                "{} adapters on WinUSB: {:04X}:{:04X} (the Realtek chip's) is opened first",
                winusb.len(),
                a.device.vid,
                a.device.pid
            );
            sel.preferred = Some(a.device.instance_id.clone());
        }
    } else {
        tracing::info!(
            "no Realtek chip {}, skipping the Realtek chipset init",
            if auto { "detected" } else { "selected" }
        );
    }
    config.realtek_files = chips::firmware_files(pid, &settings.bt_chip_fw_stem).map(|(fw, cfg)| {
        let stem = |s: String| s.strip_suffix(".bin").map(str::to_owned).unwrap_or(s);
        (stem(fw), stem(cfg))
    });
    tracing::debug!("adapter: realtek pid {pid:04X}, firmware files {:?}", config.realtek_files);
    // Realtek's Windows driver's firmware first (issue #86), else the
    // firmware folder's; custom firmware: the folder's only
    if pid != 0
        && let Some((fw, _)) = &config.realtek_files
    {
        if settings.realtek_custom_firmware {
            tracing::info!("custom firmware: the firmware folder's {fw}");
        } else {
            // The adapter's own driver package first (its
            // vendor's board-tuned firmware), else the newest generic one.
            // The adapter is the one the transport opens first (see above).
            let own = detected
                .iter()
                .find(|a| a.realtek_pid == Some(pid) && !a.device.in_use())
                .and_then(|a| realtek_own_driver_firmware(fw, a.device.vid, a.device.pid));
            config.realtek_firmware_file = own.or_else(|| realtek_driver_firmware(fw));
            if config.realtek_firmware_file.is_none() {
                tracing::info!("no firmware for this chip in Realtek's Windows driver on this PC: the folder's {fw}");
            }
        }
    }
    if pid == 0 {
        // Other vendors: Intel needs its bootloader
        // firmware, Broadcom may take a PatchRAM
        // One held by another program cannot be loaded either (v1 skipped
        // it too)
        config.intel_loader = detected.iter().any(|a| a.vendor == ChipVendor::Intel && !a.device.in_use());
        for a in detected.iter().filter(|a| {
            matches!(a.vendor, ChipVendor::Intel | ChipVendor::MediaTek | ChipVendor::Qualcomm) && a.device.in_use()
        }) {
            tracing::info!(
                "{} adapter {:04X}:{:04X} is in use by another program: no firmware download for it",
                a.vendor.name(),
                a.device.vid,
                a.device.pid
            );
        }
        // MediaTek: only the chips its loader knows, never one in use
        config.mediatek_loader = detected.iter().any(|a| {
            a.vendor == ChipVendor::MediaTek
                && chips::chip_supported(a.vendor, a.device.vid, a.device.pid)
                && !a.device.in_use()
        });
        // Qualcomm: the same; the AR30xx have a loader of their own
        let ar30xx = |a: &Adapter| chips::QUALCOMM_AR30XX.contains(&(a.device.vid, a.device.pid));
        config.qualcomm_loader = detected.iter().any(|a| {
            a.vendor == ChipVendor::Qualcomm
                && !ar30xx(a)
                && chips::chip_supported(a.vendor, a.device.vid, a.device.pid)
                && !a.device.in_use()
        });
        config.atheros_loader =
            detected.iter().any(|a| a.vendor == ChipVendor::Qualcomm && ar30xx(a) && !a.device.in_use());
        // A Realtek chip whose firmware comes from Realtek's Windows driver
        // (the RTL8852BD): the first one present, never one in use
        config.realtek_driver_chip = detected
            .iter()
            .find(|a| {
                a.vendor == ChipVendor::Realtek
                    && bt_host::realtek_btnic::chip(a.device.vid, a.device.pid).is_some()
                    && !a.device.in_use()
            })
            .map(|a| (a.device.vid, a.device.pid));
        config.broadcom_ids = detected
            .iter()
            .filter(|a| a.vendor == ChipVendor::Broadcom)
            .map(|a| (a.device.vid, a.device.pid))
            .collect();
        config.broadcom_marks = Some(std::sync::Arc::new(BroadcomPatchMarks { dir: config_dir.to_path_buf() }));
        tracing::debug!(
            "adapter: intel bootloader download {}, mediatek download {}, qualcomm download {}, atheros download {}, realtek driver chip {:04X?}, broadcom patchram ids {:04X?}",
            config.intel_loader,
            config.mediatek_loader,
            config.qualcomm_loader,
            config.atheros_loader,
            config.realtek_driver_chip,
            config.broadcom_ids
        );
        for a in &detected {
            if !chips::chip_supported(a.vendor, a.device.vid, a.device.pid) {
                tracing::warn!(
                    "{} adapter {:04X}:{:04X}: firmware loading for this chip is not implemented; it may not work",
                    a.vendor.name(),
                    a.device.vid,
                    a.device.pid
                );
            }
        }
    }
}

/// AFH host channel classification from the 2.4 GHz channel this PC's Wi-Fi
/// is connected on ("auto"), and for "blend-*" (developer mode) also from
/// Wi-Fi scans and the controller's map
#[derive(Default)]
struct WifiAfh {
    scans: Arc<Mutex<BlendScans>>,
    /// What blend passed last time (its hysteresis)
    blend_previous: Option<[u8; 10]>,
}

fn connected_24ghz() -> Vec<u8> {
    match win_sys::wlan::survey() {
        Ok(s) => s.connected_channels_24ghz(),
        Err(e) => {
            tracing::debug!("Wi-Fi survey failed: {e}");
            Vec::new()
        }
    }
}

/// A fresh Wi-Fi scan this often while "blend" runs. A scan takes the
/// PC's Wi-Fi off its channel for a moment, so not more often.
const BLEND_SCAN_EVERY: Duration = Duration::from_secs(120);
/// How long a scan waits for the access points to answer
const BLEND_SCAN_WAIT: Duration = Duration::from_secs(4);
/// A scan older than this no longer counts
const BLEND_SCAN_MAX_AGE: Duration = Duration::from_secs(600);
/// The scanning thread stops when "blend" has not asked for this long
const BLEND_SCAN_IDLE: Duration = Duration::from_secs(180);

/// The latest scans for "blend", filled by a thread of their own: a scan
/// waits seconds, and the BTstack thread that classifies must not
#[derive(Default)]
struct BlendScans {
    latest: Option<(Instant, Vec<afh::ApReading>)>,
    asked: Option<Instant>,
    running: bool,
    /// The last scan failure logged (logged once per kind)
    failed: Option<win_sys::wlan::ScanError>,
}

/// The latest scan for "blend"; starts the scanning thread when it is not
/// running. Only "blend" (developer mode, chosen by the user) gets here:
/// a scan may meet Windows' location setting.
fn blend_scan(scans: &Arc<Mutex<BlendScans>>) -> Option<Vec<afh::ApReading>> {
    let mut s = scans.lock().unwrap_or_else(|e| e.into_inner());
    s.asked = Some(Instant::now());
    if !s.running {
        s.running = true;
        let scans = Arc::clone(scans);
        let spawned = std::thread::Builder::new().name("afh-scan".into()).spawn(move || blend_scan_loop(&scans));
        if let Err(e) = spawned {
            tracing::warn!("AFH blend: no scanning thread ({e})");
            s.running = false;
        }
    }
    s.latest.as_ref().filter(|(at, _)| at.elapsed() < BLEND_SCAN_MAX_AGE).map(|(_, aps)| aps.clone())
}

fn blend_scan_loop(scans: &Mutex<BlendScans>) {
    tracing::debug!("AFH blend: Wi-Fi scans every {} s", BLEND_SCAN_EVERY.as_secs());
    loop {
        let result = win_sys::wlan::scan_24ghz(BLEND_SCAN_WAIT);
        {
            let mut s = scans.lock().unwrap_or_else(|e| e.into_inner());
            match result {
                Ok(aps) => {
                    tracing::debug!(
                        "AFH blend: scan heard {}",
                        aps.iter().map(|a| format!("ch {} {} dBm", a.channel, a.rssi)).collect::<Vec<_>>().join(", ")
                    );
                    let aps = aps.iter().map(|a| afh::ApReading { channel: a.channel, rssi: a.rssi }).collect();
                    s.latest = Some((Instant::now(), aps));
                    s.failed = None;
                }
                Err(e) => {
                    if s.failed != Some(e) {
                        tracing::info!("AFH blend: no Wi-Fi scan ({e:?}), the controller's map alone decides");
                    }
                    s.failed = Some(e);
                    s.latest = None;
                }
            }
        }
        std::thread::sleep(BLEND_SCAN_EVERY);
        let mut s = scans.lock().unwrap_or_else(|e| e.into_inner());
        if s.asked.is_none_or(|t| t.elapsed() > BLEND_SCAN_IDLE) {
            tracing::debug!("AFH blend: Wi-Fi scans stopped");
            s.running = false;
            s.latest = None;
            return;
        }
    }
}

impl AfhSource for WifiAfh {
    fn classify(&mut self, policy: &str, controller: &afh::ControllerHistory) -> Option<AfhClassification> {
        let c = match afh::Policy::parse(policy) {
            afh::Policy::Blend(strictness) => {
                let connected = connected_24ghz();
                let aps = blend_scan(&self.scans);
                let c = afh::blend(&afh::BlendInput {
                    strictness,
                    connected: &connected,
                    aps: aps.as_deref(),
                    controller,
                    previous: self.blend_previous.as_ref(),
                });
                self.blend_previous = c.passed.then_some(c.map);
                c
            }
            _ => {
                self.blend_previous = None;
                afh::classify(policy, connected_24ghz)
            }
        };
        tracing::debug!(
            "afh: policy {policy:?}: passed {}, usable {}, avoided Wi-Fi {:?}",
            c.passed,
            c.usable,
            c.avoided_wifi
        );
        c.passed.then(|| AfhClassification {
            map: c.map,
            usable: c.usable as u32,
            avoided: afh::describe(&c.avoided_wifi),
        })
    }
}

// ---- What a stack learnt about an adapter (adapter_caps.json) ----

/// Writes adapter_caps.json (in `adapters\`)
fn write_caps(dir: &Path, caps: &BTreeMap<String, AdapterCaps>) {
    let text = serde_json::to_string_pretty(caps).unwrap_or_default();
    let path = crate::paths::adapter_caps_file(dir);
    if let Err(e) = std::fs::create_dir_all(crate::paths::adapters_dir(dir)).and_then(|()| std::fs::write(&path, text))
    {
        tracing::warn!("cannot write {}: {e}", path.display());
    }
}

/// What BlueCaster learnt about an adapter when a stack ran on it
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AdapterCaps {
    /// LE Audio's isochronous channels (issue #86)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    le_audio: Option<bool>,
    /// The adapter's own Bluetooth address
    #[serde(default, skip_serializing_if = "Option::is_none")]
    address: Option<String>,
    /// The HCI version the controller reports (Bluetooth core version)
    /// under a stack of BlueCaster, with the firmware it loaded
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hci_version: Option<u8>,
    /// The HCI version Windows' Bluetooth read, with the firmware its
    /// driver loaded (issue #427: a Realtek driver's may be newer)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    windows_hci_version: Option<u8>,
    /// Device Manager's name while Windows' Bluetooth ran it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    windows_name: Option<String>,
    /// The app version Broadcom's patch failed with on it (bt-host's
    /// broadcom loader): while it is this app's, the ROM runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    broadcom_patch_failed: Option<String>,
}

/// The failed-patch marks of bt-host's Broadcom loader, in
/// adapter_caps.json (per VID:PID, with the app version)
pub struct BroadcomPatchMarks {
    pub dir: std::path::PathBuf,
}

impl bt_host::broadcom::PatchMarks for BroadcomPatchMarks {
    fn failed_with(&self, vid: u16, pid: u16) -> Option<String> {
        load_caps(&self.dir).get(&caps_key(vid, pid)).and_then(|c| c.broadcom_patch_failed.clone())
    }

    fn set_failed(&self, vid: u16, pid: u16, version: Option<&str>) {
        let mut caps = load_caps(&self.dir);
        let entry = caps.entry(caps_key(vid, pid)).or_default();
        if entry.broadcom_patch_failed.as_deref() == version {
            return;
        }
        entry.broadcom_patch_failed = version.map(str::to_owned);
        tracing::debug!(
            "adapter caps: {} Broadcom patch failed {version:?}, saving adapter_caps.json",
            caps_key(vid, pid)
        );
        write_caps(&self.dir, &caps);
    }
}

fn caps_key(vid: u16, pid: u16) -> String {
    format!("{vid:04X}:{pid:04X}")
}

fn load_caps(dir: &Path) -> BTreeMap<String, AdapterCaps> {
    std::fs::read_to_string(crate::paths::adapter_caps_file(dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Whether the adapter with this VID:PID has LE Audio's isochronous
/// channels, as a stack found out; None if none ran on it yet
pub fn le_audio(dir: &Path, vid: u16, pid: u16) -> Option<bool> {
    load_caps(dir).get(&caps_key(vid, pid)).and_then(|c| c.le_audio)
}

/// Remembers what a stack found out about the adapter's LE Audio
pub fn save_le_audio(dir: &Path, vid: u16, pid: u16, capable: bool) {
    let mut caps = load_caps(dir);
    let entry = caps.entry(caps_key(vid, pid)).or_default();
    if entry.le_audio == Some(capable) {
        return;
    }
    entry.le_audio = Some(capable);
    tracing::debug!("adapter caps: {} LE Audio {capable}, saving adapter_caps.json", caps_key(vid, pid));
    write_caps(dir, &caps);
}

/// The adapter's own Bluetooth address, as a stack running on it read it
pub fn address(dir: &Path, vid: u16, pid: u16) -> Option<String> {
    load_caps(dir).get(&caps_key(vid, pid)).and_then(|c| c.address.clone())
}

/// Remembers the adapter's own address
pub fn save_address(dir: &Path, vid: u16, pid: u16, address: &str) {
    let mut caps = load_caps(dir);
    let entry = caps.entry(caps_key(vid, pid)).or_default();
    if entry.address.as_deref() == Some(address) {
        return;
    }
    entry.address = Some(address.to_owned());
    tracing::debug!("adapter caps: {} address {address}, saving adapter_caps.json", caps_key(vid, pid));
    write_caps(dir, &caps);
}

/// The HCI version the controller on this VID:PID reported, as a stack of
/// BlueCaster running on it read it
pub fn hci_version(dir: &Path, vid: u16, pid: u16) -> Option<u8> {
    load_caps(dir).get(&caps_key(vid, pid)).and_then(|c| c.hci_version)
}

/// Remembers the controller's HCI version
pub fn save_hci_version(dir: &Path, vid: u16, pid: u16, version: u8) {
    let mut caps = load_caps(dir);
    let entry = caps.entry(caps_key(vid, pid)).or_default();
    if entry.hci_version == Some(version) {
        return;
    }
    entry.hci_version = Some(version);
    tracing::debug!("adapter caps: {} HCI version {version}, saving adapter_caps.json", caps_key(vid, pid));
    write_caps(dir, &caps);
}

/// Device Manager's name for the adapter's radio, as Windows' Bluetooth
/// named it when it last ran the adapter
pub fn windows_name(dir: &Path, vid: u16, pid: u16) -> Option<String> {
    load_caps(dir).get(&caps_key(vid, pid)).and_then(|c| c.windows_name.clone())
}

/// The HCI version Windows' Bluetooth read when it last ran the adapter
pub fn windows_hci_version(dir: &Path, vid: u16, pid: u16) -> Option<u8> {
    load_caps(dir).get(&caps_key(vid, pid)).and_then(|c| c.windows_hci_version)
}

/// What Windows' Bluetooth tells about the adapter while it runs it: its
/// name, the address a stack of BlueCaster would read, and the version of
/// the firmware Windows' driver loaded (kept apart from BlueCaster's own)
pub fn save_windows_radio(dir: &Path, vid: u16, pid: u16, name: &str, address: Option<&str>, hci_version: Option<u8>) {
    let mut caps = load_caps(dir);
    let entry = caps.entry(caps_key(vid, pid)).or_default();
    let before = entry.clone();
    if !name.is_empty() {
        entry.windows_name = Some(name.to_owned());
    }
    if let Some(a) = address {
        entry.address = Some(a.to_owned());
    }
    if hci_version.is_some() {
        entry.windows_hci_version = hci_version;
    }
    if *entry == before {
        return;
    }
    tracing::debug!(
        "adapter caps: {} Windows radio {name:?} {address:?} HCI {hci_version:?}, saving adapter_caps.json",
        caps_key(vid, pid)
    );
    write_caps(dir, &caps);
}

/// The HCI version the Adapters screen shows: the one of the firmware that
/// runs the adapter. On WinUSB that is what BlueCaster's stack read after
/// loading its firmware; under Windows' Bluetooth what Windows reads now.
/// `radio` is Windows' radio of the device, or what Windows stored on the
/// device node (issue #427: it stays after the move to WinUSB, with the
/// version of the firmware Windows' driver had loaded).
pub fn shown_hci_version(winusb: bool, radio: Option<u8>, ours: Option<u8>, windows_saved: Option<u8>) -> Option<u8> {
    if winusb { ours.or(radio).or(windows_saved) } else { radio.or(windows_saved).or(ours) }
}

/// The Bluetooth address a Realtek adapter reports as its USB serial number
/// ("E848B8C82000" on a UB500 whose controller answers E8:48:B8:C8:20:00),
/// from the device's instance ID (`USB\VID_2357&PID_0604\E848B8C82000`).
/// Windows tells it whatever the driver and whether its Bluetooth runs the
/// adapter (a second radio it does not use answers no IOCTL and stores no
/// radio properties: issue #86). None for another vendor, or a device with
/// no serial (Windows then makes up an ID with `&`s) or another kind of one.
pub fn serial_address(vendor: ChipVendor, instance_id: &str) -> Option<String> {
    if vendor != ChipVendor::Realtek {
        return None;
    }
    let serial = instance_id.rsplit('\\').next()?;
    if serial.len() != 12 || !serial.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let addr = u64::from_str_radix(serial, 16).ok()?;
    if addr == 0 || addr == 0xFFFF_FFFF_FFFF {
        return None;
    }
    Some(win_sys::bt_radios::format_address(addr))
}

/// The Bluetooth core specification of an HCI version ("5.3" for 12);
/// None for one the table does not know
pub fn bt_core_version(hci_version: u8) -> Option<&'static str> {
    const VERSIONS: [&str; 16] = [
        "1.0b", "1.1", "1.2", "2.0", "2.1", "3.0", "4.0", "4.1", "4.2", "5.0", "5.1", "5.2", "5.3", "5.4", "6.0", "6.1",
    ];
    VERSIONS.get(hci_version as usize).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use win_sys::driver::{DriverInfo, DriverKind};
    use win_sys::usb_adapters::{InterfaceProbe, UsbClass};

    fn device(vid: u16, pid: u16, probe: Option<Result<InterfaceProbe, String>>) -> UsbDevice {
        UsbDevice {
            instance_id: format!(r"USB\VID_{vid:04X}&PID_{pid:04X}\1"),
            device_path: String::new(),
            vid,
            pid,
            name: String::new(),
            bus_reported_name: None,
            manufacturer: String::new(),
            hardware_ids: Vec::new(),
            compatible_ids: Vec::new(),
            usb_class: None,
            function_classes: Vec::new(),
            driver: DriverInfo::default(),
            driver_kind: DriverKind::WinUsb,
            problem_code: None,
            probe,
        }
    }

    #[test]
    fn a_broadcast_goes_on_the_auracast_adapter() {
        let s = |adapter: &str, auracast: &str, debug_mode: bool| Settings {
            adapter_id: adapter.into(),
            auracast_adapter_id: auracast.into(),
            debug_mode,
            ..Settings::default()
        };
        let both = s("BT600", "NRF", true);
        assert_eq!(adapter_for(&both, true), "NRF");
        assert_eq!(adapter_for(&both, false), "BT600");
        // None set for Auracast: the same adapter as everything else
        assert_eq!(adapter_for(&s("BT600", "", true), true), "BT600");
        assert_eq!(adapter_for(&s("", " ", true), true), "");
        // Developer mode off: Auracast is not offered, its adapter not used
        assert_eq!(adapter_for(&s("BT600", "NRF", false), true), "BT600");
    }

    #[test]
    fn the_chosen_adapter_is_found_when_free() {
        let adapter = |d: UsbDevice| Adapter {
            device: d,
            realtek_pid: None,
            vendor: ChipVendor::Intel,
            display_name: String::new(),
        };
        let intel = adapter(device(0x8087, 0x0033, None));
        let realtek = adapter(device(0x0BDA, 0x8771, None));
        let busy = adapter(device(0x0B05, 0x190E, Some(Err("open failed (error 5): taken".into()))));
        let all = [&intel, &realtek, &busy];
        let found = |id: &str| chosen_adapter(&all, id).map(|a| a.device.pid);
        assert_eq!(found(""), None);
        assert_eq!(found(r"usb\vid_0bda&pid_8771\1"), Some(0x8771));
        assert_eq!(found(r" USB\VID_8087&PID_0033\1 "), Some(0x0033));
        // Another program has it, or it is not on WinUSB (not in the list)
        assert_eq!(found(r"USB\VID_0B05&PID_190E\1"), None);
        assert_eq!(found(r"USB\VID_1234&PID_5678\1"), None);
    }

    #[test]
    fn le_audio_remembered_per_adapter() {
        let dir = std::env::temp_dir().join(format!("bluecaster-caps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(le_audio(&dir, 0x0BDA, 0xA729), None);
        save_le_audio(&dir, 0x0BDA, 0xA729, false);
        save_le_audio(&dir, 0x8087, 0x0032, true);
        assert_eq!(le_audio(&dir, 0x0BDA, 0xA729), Some(false));
        assert_eq!(le_audio(&dir, 0x8087, 0x0032), Some(true));
        assert_eq!(address(&dir, 0x0BDA, 0xA729), None);
        save_address(&dir, 0x0BDA, 0xA729, "00:11:22:33:44:55");
        assert_eq!(address(&dir, 0x0BDA, 0xA729).as_deref(), Some("00:11:22:33:44:55"));
        assert_eq!(le_audio(&dir, 0x0BDA, 0xA729), Some(false));
        assert!(std::fs::read_to_string(crate::paths::adapter_caps_file(&dir)).unwrap().contains("\"0BDA:A729\""));
        assert_eq!(hci_version(&dir, 0x0BDA, 0xA729), None);
        assert!(!used_before(&dir));
        save_hci_version(&dir, 0x0BDA, 0xA729, 10);
        assert!(used_before(&dir));
        assert_eq!(hci_version(&dir, 0x0BDA, 0xA729), Some(10));
        assert_eq!(hci_version(&dir, 0x8087, 0x0032), None);
        // Windows' read, with its driver's firmware, is kept apart (#427)
        save_windows_radio(&dir, 0x0BDA, 0xA729, "Realtek Bluetooth 5.4 Adapter", None, Some(13));
        assert_eq!(hci_version(&dir, 0x0BDA, 0xA729), Some(10));
        assert_eq!(windows_hci_version(&dir, 0x0BDA, 0xA729), Some(13));
        // The other values stay
        assert_eq!(address(&dir, 0x0BDA, 0xA729).as_deref(), Some("00:11:22:33:44:55"));
        assert_eq!(le_audio(&dir, 0x0BDA, 0xA729), Some(false));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broadcom_patch_marks_are_kept_per_adapter() {
        use bt_host::broadcom::PatchMarks;
        let dir = std::env::temp_dir().join(format!("bluecaster-bcm-marks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save_hci_version(&dir, 0x0B05, 0x17CB, 6);
        let marks = BroadcomPatchMarks { dir: dir.clone() };
        assert_eq!(marks.failed_with(0x0B05, 0x17CB), None);
        marks.set_failed(0x0B05, 0x17CB, Some("3.0.2"));
        assert_eq!(marks.failed_with(0x0B05, 0x17CB).as_deref(), Some("3.0.2"));
        assert_eq!(marks.failed_with(0x0A5C, 0x21E8), None);
        // The adapter's other values stay; dropping the mark leaves them
        assert_eq!(hci_version(&dir, 0x0B05, 0x17CB), Some(6));
        marks.set_failed(0x0B05, 0x17CB, None);
        assert_eq!(marks.failed_with(0x0B05, 0x17CB), None);
        assert_eq!(hci_version(&dir, 0x0B05, 0x17CB), Some(6));
        let text = std::fs::read_to_string(crate::paths::adapter_caps_file(&dir)).unwrap();
        assert!(!text.contains("broadcom_patch_failed"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Issue #86: after a crash the MT7925 showed as an unknown USB device
    #[test]
    fn hung_adapter_only_when_ours_is_missing() {
        assert!(hung_adapter(1, false, true));
        // An adapter on WinUSB works: the unreadable device is another one
        assert!(!hung_adapter(1, true, true));
        // No adapter ever ran here: nothing to say it was ours
        assert!(!hung_adapter(1, false, false));
        assert!(!hung_adapter(0, false, true));
    }

    #[test]
    fn realtek_serial_is_its_address() {
        // A UB500 (Realtek under TP-Link's VID) whose stack read E8:48:B8:C8:20:00
        assert_eq!(
            serial_address(ChipVendor::Realtek, r"USB\VID_2357&PID_0604\E848B8C82000").as_deref(),
            Some("E8:48:B8:C8:20:00")
        );
        assert_eq!(
            serial_address(ChipVendor::Realtek, r"USB\VID_0BDA&PID_A729\00e04c239987").as_deref(),
            Some("00:E0:4C:23:99:87")
        );
        // No serial: Windows' made-up ID
        assert_eq!(serial_address(ChipVendor::Realtek, r"USB\VID_2357&PID_0604\6&28E8DB3B&0&1"), None);
        // Another kind of serial
        assert_eq!(serial_address(ChipVendor::Realtek, r"USB\VID_0BDA&PID_A729\0000000001"), None);
        assert_eq!(serial_address(ChipVendor::Realtek, r"USB\VID_0BDA&PID_A729\00E04C23998Z"), None);
        assert_eq!(serial_address(ChipVendor::Realtek, r"USB\VID_0BDA&PID_A729\000000000000"), None);
        assert_eq!(serial_address(ChipVendor::Realtek, r"USB\VID_0BDA&PID_A729\FFFFFFFFFFFF"), None);
        // Other vendors' serials are not addresses
        assert_eq!(serial_address(ChipVendor::MediaTek, r"USB\VID_0E8D&PID_7925\C0E3502E0009"), None);
        assert_eq!(serial_address(ChipVendor::Intel, r"USB\VID_8087&PID_0033\5&3B72E4CF&0&14"), None);
    }

    #[test]
    fn shown_version_is_the_running_firmwares() {
        // WinUSB: BlueCaster's read (linux-firmware 5.1) over the 5.4 Windows'
        // driver left on the device node, and the other way round
        assert_eq!(shown_hci_version(true, Some(13), Some(10), Some(13)), Some(10));
        assert_eq!(shown_hci_version(true, Some(10), Some(13), None), Some(13));
        // No stack of BlueCaster ran it yet
        assert_eq!(shown_hci_version(true, None, None, Some(13)), Some(13));
        // Windows' Bluetooth runs it
        assert_eq!(shown_hci_version(false, Some(13), Some(10), None), Some(13));
        assert_eq!(shown_hci_version(false, None, Some(10), Some(13)), Some(13));
        assert_eq!(shown_hci_version(false, None, Some(10), None), Some(10));
        assert_eq!(shown_hci_version(false, None, None, None), None);
    }

    #[test]
    fn hci_version_to_core_version() {
        assert_eq!(bt_core_version(0), Some("1.0b"));
        assert_eq!(bt_core_version(6), Some("4.0"));
        assert_eq!(bt_core_version(9), Some("5.0"));
        assert_eq!(bt_core_version(12), Some("5.3"));
        assert_eq!(bt_core_version(14), Some("6.0"));
        assert_eq!(bt_core_version(15), Some("6.1"));
        assert_eq!(bt_core_version(16), None);
        assert_eq!(bt_core_version(0xFF), None);
    }

    #[test]
    fn known_ids_cover_v1s_vendor_class_adapters() {
        let ids = usb_known_ids(&[]);
        // v1's built-in list (BTstack's WinUSB transport)
        for id in [(0x0A5C, 0x21E8), (0x0B05, 0x17CB), (0x0A5C, 0x22BE), (0x2FE3, 0x0100), (0x2FE3, 0x000B)] {
            assert!(ids.contains(&id), "{id:04X?}");
        }
        // The OEM table (TP-Link UB500)
        assert!(ids.contains(&(0x2357, 0x0604)));
        // An enumerated vendor-class adapter, once
        let ids = usb_known_ids(&[(0x1234, 0x5678), (0x0B05, 0x17CB)]);
        assert_eq!(ids.iter().filter(|&&id| id == (0x1234, 0x5678)).count(), 1);
        assert_eq!(ids.iter().filter(|&&id| id == (0x0B05, 0x17CB)).count(), 1);
    }

    #[test]
    fn probe_tells_vendor_class_and_in_use() {
        let vendor = InterfaceProbe { class: UsbClass::VENDOR_BLUETOOTH, num_endpoints: 3, hci_endpoints: true };
        assert_eq!(device(0x1234, 0x5678, Some(Ok(vendor.clone()))).winusb_bluetooth(), Some(true));
        let no_pipes = InterfaceProbe { hci_endpoints: false, ..vendor };
        assert_eq!(device(0x1234, 0x5678, Some(Ok(no_pipes))).winusb_bluetooth(), None);
        assert!(device(0x8087, 0x0029, Some(Err("open failed (error 5)".into()))).in_use());
        assert!(!device(0x8087, 0x0029, Some(Err("open failed (error 2)".into()))).in_use());
        assert!(!device(0x8087, 0x0029, None).in_use());
    }
}
