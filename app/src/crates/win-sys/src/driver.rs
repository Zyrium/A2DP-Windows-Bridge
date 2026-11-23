//! Assigning Windows' WinUSB driver to a USB Bluetooth adapter, and putting
//! the adapter back on the driver Windows picks for it (design.md "ドライバ
//! （WinUSB）")
//!
//! What Zadig does with a generated, self-signed INF is done here with the
//! INF Windows ships: `%SystemRoot%\INF\winusb.inf` (Microsoft-signed, so no
//! certificate is installed). It is the same as Device Manager's "Update
//! driver > Browse > Let me pick > Have Disk > winusb.inf > WinUsb Device":
//!
//! 1. open the device by its instance ID
//! 2. write `DeviceInterfaceGUIDs` (REG_MULTI_SZ, BlueCaster's own GUID) into
//!    its `Device Parameters` key, the value Zadig's INF writes: WinUSB
//!    registers a device interface only for the GUIDs found there
//! 3. point the device's install parameters at winusb.inf alone
//!    (DI_ENUMSINGLEINF + DriverPath), build the class driver list from it
//!    and select its "WinUsb Device" model (install section WINUSB)
//! 4. DiInstallDevice
//!
//! Restoring puts the driver recorded by `assign_winusb` (INF name, install
//! section, class) back the same way, on the running device; Zadig's
//! library (libwdi) installs like that too and never uninstalls. On a
//! MediaTek adapter (issue #86) DiUninstallDevice alone took 20 s, while
//! installing WinUSB took about 2 s. When the recorded driver cannot be
//! installed (no record, its package gone, another driver after the
//! install), the device is removed instead: CM_Query_And_Remove_SubTree (the
//! stop, timed on its own) and DiUninstallDevice, then CM_Reenumerate_DevNode
//! on the parent (the hub): the device comes back as new and Windows ranks
//! its drivers again, picking BTHUSB or the vendor driver. WinUSB was a
//! manual choice (winusb.inf matches only `USB\MS_COMP_WINUSB`), so it is
//! not picked again. Either way the driver afterwards is compared with the
//! record.
//!
//! When Windows cannot stop the old driver to load the new one (Windows'
//! Bluetooth with a device connected, issue #86), the adapter's USB port is
//! reset instead of asking for a restart of Windows: Windows handles that as
//! unplugging the adapter, which no driver can refuse. Before the switch,
//! Windows' Bluetooth is turned off if devices were connected, so they cannot
//! reconnect; the restore turns it on again.
//!
//! Both need administrator rights (`bluecaster-driver.exe`, started elevated);
//! `driver_state` is read-only and needs none.
//!
//! Whether SetupDiBuildDriverInfoList offers winusb.inf (class USBDevice)
//! for a device of another class (Bluetooth) varies (both paths have run on
//! real adapters). If it does not, the class is changed to USBDevice first
//! (what installing a driver of another class does anyway) and the list is
//! built again; if that fails too, the device is put back as by `restore`.

use std::time::{Duration, Instant};

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_DevNode_PropertyW, CM_Get_Device_IDW,
    CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW,
};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CM_Query_And_Remove_SubTreeW,
    CM_REENUMERATE_ASYNCHRONOUS, CM_REENUMERATE_FLAGS, CM_REENUMERATE_SYNCHRONOUS, CM_REMOVE_NO_RESTART,
    CM_REMOVE_UI_NOT_OK, CM_Reenumerate_DevNode, CM_Set_DevNode_PropertyW, CMP_WaitNoPendingInstallEvents, CR_SUCCESS,
    DI_ENUMSINGLEINF, DI_FLAGSEX_ALLOWEXCLUDEDDRVS, DI_NEEDREBOOT, DI_NEEDRESTART, DICS_FLAG_GLOBAL, DICS_PROPCHANGE,
    DIF_PROPERTYCHANGE, DIIDFLAG_NOFINISHINSTALLUI, DIREG_DEV, DiInstallDevice, DiUninstallDevice, PNP_VETO_TYPE,
    SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SP_DRVINFO_DATA_V2_W, SP_DRVINFO_DETAIL_DATA_W,
    SP_PROPCHANGE_PARAMS, SPDIT_CLASSDRIVER, SPDRP_CLASSGUID, SetupDiBuildDriverInfoList, SetupDiCallClassInstaller,
    SetupDiCreateDevRegKeyW, SetupDiDestroyDriverInfoList, SetupDiEnumDriverInfoW, SetupDiGetDeviceInstallParamsW,
    SetupDiGetDriverInfoDetailW, SetupDiOpenDevRegKey, SetupDiSetClassInstallParamsW, SetupDiSetDeviceInstallParamsW,
    SetupDiSetDeviceRegistryPropertyW, SetupDiSetSelectedDriverW,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_Address, DEVPKEY_Device_SafeRemovalRequiredOverride, DEVPROP_TYPE_BOOLEAN, DEVPROP_TYPE_EMPTY,
    DEVPROP_TYPE_UINT32, DEVPROPTYPE,
};
use windows::Win32::Devices::Usb::{GUID_DEVINTERFACE_USB_HUB, IOCTL_USB_HUB_CYCLE_PORT, USB_CYCLE_PORT_PARAMS};
use windows::Win32::Foundation::{
    ERROR_INVALID_DEVINST_NAME, ERROR_NO_MORE_ITEMS, ERROR_NO_SUCH_DEVINST, GENERIC_WRITE, HWND,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Registry::{KEY_READ, REG_DWORD, REG_MULTI_SZ};
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::core::{BOOL, GUID, HSTRING, PCWSTR};

use crate::autostart::RegKey;
use crate::elevation::is_elevated;
use crate::usb_adapters::{DevInfoSet, OwnedHandle, format_guid, parse_multi_sz, problem_code};

/// The device interface GUID BlueCaster registers for adapters it puts on
/// WinUSB (`DeviceInterfaceGUIDs`), so its own adapters can be told apart
/// from other WinUSB devices
pub const INTERFACE_GUID: &str = "{03D8E127-01B1-4A98-9F22-AC8E41AA0CFD}";
/// The registry value WinUSB reads its interface GUIDs from
pub const INTERFACE_GUIDS_VALUE: &str = "DeviceInterfaceGUIDs";
/// Setup class of winusb.inf ("USBDevice", Universal Serial Bus devices)
pub const USB_DEVICE_CLASS: GUID = GUID::from_u128(0x88BAE032_5A81_49F0_BC3D_A4FF138216D6);
/// Install section of winusb.inf's "WinUsb Device" model. Matched instead of
/// the model's description, which is translated on non-English Windows.
const WINUSB_SECTION: &str = "WINUSB";
/// How long a restore waits for Windows to install the adapter again
const RESTORE_WAIT: Duration = Duration::from_secs(30);
/// How often the restored device is looked at meanwhile
const RESTORE_POLL: Duration = Duration::from_millis(250);
/// How long a restored device's problem code must stay to be the result
const PROBLEM_SETTLE: Duration = Duration::from_secs(3);
/// How long a restore waits for driver installs Windows is running already
const PENDING_INSTALL_WAIT_MS: u32 = 10_000;
/// How long a USB port reset may take to remove the adapter
const REPLUG_GONE_WAIT: Duration = Duration::from_secs(3);
/// Hardware key value: the switch to WinUSB turned Windows' Bluetooth off
const RADIO_OFF_VALUE: &str = "BlueCasterSwitchedBluetoothOff";
/// How long turning Windows' Bluetooth back on waits for its radio
const RADIO_BACK_WAIT: Duration = Duration::from_secs(15);

/// Which kind of driver owns a device
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    /// WINUSB: BlueCaster can use the adapter
    WinUsb,
    /// BTHUSB or another driver of the Bluetooth class: Windows uses it
    Bluetooth,
    /// Some other driver (a vendor driver, usbccgp for a composite device)
    Other,
    /// No driver installed
    None,
}

impl DriverKind {
    /// From the device's service name and setup class name
    pub fn classify(service: &str, class_name: &str) -> DriverKind {
        if service.eq_ignore_ascii_case("WinUSB") {
            DriverKind::WinUsb
        } else if service.eq_ignore_ascii_case("BTHUSB") || class_name.eq_ignore_ascii_case("Bluetooth") {
            DriverKind::Bluetooth
        } else if service.is_empty() {
            DriverKind::None
        } else {
            DriverKind::Other
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DriverKind::WinUsb => "winusb",
            DriverKind::Bluetooth => "bluetooth",
            DriverKind::Other => "other",
            DriverKind::None => "none",
        }
    }
}

/// The driver installed on a device (empty strings where Windows has none)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverInfo {
    /// Service name: BTHUSB, WINUSB, a vendor's
    pub service: String,
    /// INF name in %SystemRoot%\INF: `bth.inf`, `winusb.inf`, `oem12.inf`
    pub inf_name: String,
    pub inf_section: String,
    /// `10.0.26100.1`
    pub version: String,
    pub provider: String,
    pub description: String,
    /// Setup class name: Bluetooth, USBDevice, USB
    pub class_name: String,
    /// Setup class GUID, `{...}`
    pub class_guid: String,
}

/// What `driver_state` reports
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverState {
    pub instance_id: String,
    /// Plugged in (a removed adapter keeps its settings in Windows)
    pub present: bool,
    pub kind: DriverKind,
    pub driver: DriverInfo,
    /// Device Manager's problem code, None if it works
    pub problem_code: Option<u32>,
    /// `DeviceInterfaceGUIDs` in its Device Parameters
    pub interface_guids: Vec<String>,
}

impl DriverState {
    /// On WinUSB with BlueCaster's interface GUID
    pub fn has_bluecaster_interface(&self) -> bool {
        self.interface_guids.iter().any(|g| g.eq_ignore_ascii_case(INTERFACE_GUID))
    }
}

/// The driver a device had before WinUSB was assigned; the caller keeps it
/// (the helper prints it as JSON) and passes it back to `restore`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverRecord {
    pub inf_name: String,
    /// Install section as Windows recorded it (`Composite.Dev.NT`); empty in
    /// records from 3.0.0-beta.3 and earlier
    pub inf_section: String,
    pub version: String,
    pub service: String,
    pub provider: String,
    pub description: String,
    pub class_guid: String,
}

impl DriverRecord {
    pub fn from_info(info: &DriverInfo) -> DriverRecord {
        DriverRecord {
            inf_name: info.inf_name.clone(),
            inf_section: info.inf_section.clone(),
            version: info.version.clone(),
            service: info.service.clone(),
            provider: info.provider.clone(),
            description: info.description.clone(),
            class_guid: info.class_guid.clone(),
        }
    }

    /// The same driver package is installed again: INF name, version and
    /// service (oemNN.inf names stay the same for one package in the store)
    pub fn matches(&self, info: &DriverInfo) -> bool {
        self.inf_name.eq_ignore_ascii_case(&info.inf_name)
            && self.version == info.version
            && self.service.eq_ignore_ascii_case(&info.service)
    }

    /// The recorded driver is back, perhaps in a newer version: the same INF
    /// and service. Windows Update replaces inbox INFs (usb.inf, bth.inf) in
    /// place while the adapter is on WinUSB, so `restore` installs a newer
    /// build of the same driver (#86)
    pub fn is_back(&self, info: &DriverInfo) -> bool {
        self.inf_name.eq_ignore_ascii_case(&info.inf_name) && self.service.eq_ignore_ascii_case(&info.service)
    }
}

/// Whether a failed assignment was undone
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollback {
    /// Nothing had changed but BlueCaster's interface GUID, which was removed
    NotNeeded,
    /// The device was put back as by `restore`
    Restored,
    /// Putting it back failed too: the user must reinstall the driver in
    /// Device Manager (or unplug and replug the adapter)
    Failed,
}

impl Rollback {
    fn note(self) -> &'static str {
        match self {
            Rollback::NotNeeded => "",
            Rollback::Restored => " (the adapter was put back on its previous driver)",
            Rollback::Failed => " (putting the adapter back failed as well)",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("administrator rights are needed to change a device's driver")]
    NotElevated,
    #[error("no device with the instance ID {0}")]
    DeviceNotFound(String),
    #[error("{0} is not a USB device")]
    NotUsb(String),
    #[error("Windows' WinUSB driver is missing ({0})")]
    WinUsbInfMissing(String),
    #[error("winusb.inf offered no \"WinUsb Device\" driver for this device (class {class}){}", .rollback.note())]
    WinUsbModelNotFound { class: String, rollback: Rollback },
    #[error("{step} failed: {source}{}", .rollback.note())]
    Win32 {
        step: &'static str,
        #[source]
        source: windows::core::Error,
        rollback: Rollback,
    },
}

impl DriverError {
    fn win32(step: &'static str) -> impl FnOnce(windows::core::Error) -> DriverError {
        move |source| DriverError::Win32 { step, source, rollback: Rollback::NotNeeded }
    }
}

/// What `assign_winusb` did
#[derive(Debug, Clone)]
pub struct AssignOutcome {
    /// The driver before; pass it to `restore` later
    pub previous: DriverRecord,
    /// It was on WinUSB already; nothing was changed
    pub already_winusb: bool,
    /// The device's setup class had to be changed to USBDevice first
    pub class_changed: bool,
    pub reboot_required: bool,
    /// The driver now
    pub state: DriverState,
    /// The devices Windows had connected through the adapter, disconnected
    /// first
    pub disconnected: Vec<String>,
}

/// How long installing WinUSB (with the restart of the device) may take.
/// Windows stops the old driver first; when it cannot (its Bluetooth still
/// holds the radio), DiInstallDevice does not return until Windows
/// restarts. Normally it takes under a second, with the restart up to ~15 s.
const INSTALL_LIMIT: Duration = Duration::from_secs(45);

/// The device set and element, handed to the install thread
struct InstallJob(DevInfoSet, SP_DEVINFO_DATA);
// SAFETY: the set is used by one thread at a time: the install thread owns
// it, and gives it back only when done (or keeps it if Windows never
// returns; the process then exits with it).
unsafe impl Send for InstallJob {}

/// What `restore` did
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    /// It was not on WinUSB; nothing was changed
    pub already_restored: bool,
    pub reboot_required: bool,
    /// The driver now; None if the device did not come back in time
    pub state: Option<DriverState>,
    /// The driver now is the recorded one (None without a record or state)
    pub matches_previous: Option<bool>,
}

/// The driver a device has now. Read-only; no administrator rights needed.
pub fn driver_state(instance_id: &str) -> Result<DriverState, DriverError> {
    let (set, data) = open(instance_id)?;
    let driver = set.driver_info(&data);
    let present = locate(instance_id).is_some();
    let state = DriverState {
        instance_id: instance_id.to_string(),
        present,
        kind: DriverKind::classify(&driver.service, &driver.class_name),
        problem_code: if present { problem_code(data.DevInst) } else { None },
        interface_guids: read_interface_guids(&set, &data),
        driver,
    };
    tracing::debug!(
        "driver: state of {instance_id}: present {}, {:?}, service {:?}, {} {} ({}), class {} {}, problem {:?}, interface GUIDs {:?}",
        state.present,
        state.kind,
        state.driver.service,
        state.driver.inf_name,
        state.driver.version,
        state.driver.provider,
        state.driver.class_name,
        state.driver.class_guid,
        state.problem_code,
        state.interface_guids
    );
    Ok(state)
}

/// Puts the device on WinUSB (see the module documentation). Needs
/// administrator rights; changes the driver of a real device, so it is
/// never called from tests.
pub fn assign_winusb(instance_id: &str) -> Result<AssignOutcome, DriverError> {
    if !is_elevated() {
        return Err(DriverError::NotElevated);
    }
    require_usb(instance_id)?;
    let (set, data) = open(instance_id)?;
    // Windows remembers unplugged devices; installing on one would only
    // take effect when it is plugged in, and could not be checked
    if locate(instance_id).is_none() {
        return Err(DriverError::DeviceNotFound(instance_id.to_string()));
    }
    let before = set.driver_info(&data);
    let previous = DriverRecord::from_info(&before);
    if DriverKind::classify(&before.service, &before.class_name) == DriverKind::WinUsb {
        tracing::info!("driver: {instance_id} is on WinUSB already");
        hide_from_safe_removal(instance_id, true);
        return Ok(AssignOutcome {
            previous,
            already_winusb: true,
            class_changed: false,
            reboot_required: false,
            state: driver_state(instance_id)?,
            disconnected: Vec::new(),
        });
    }
    let inf = inf_path("winusb.inf").map_err(DriverError::WinUsbInfMissing)?;
    // Windows cannot stop its Bluetooth on a radio with a connected device
    // (an audio link above all): the install would hang
    let disconnected = crate::bt_radios::disconnect_through(instance_id);
    // Earbuds often reconnect right after the disconnect (issue #86). With
    // Windows' Bluetooth off they cannot; the restore turns it on again.
    let radio_off = !disconnected.is_empty() && crate::bt_radios::set_switch(false);
    if radio_off {
        mark_radio_off(instance_id, true);
    }
    // While Windows still runs the adapter (a restart is needed, or the
    // switch failed), its Bluetooth goes back on
    let radio_back_on = || {
        if radio_off {
            mark_radio_off(instance_id, false);
            // After a rollback that reinstalled the adapter, the radio
            // comes back a moment later
            crate::bt_radios::switch_on_when_back(RADIO_BACK_WAIT);
        }
    };
    tracing::info!(
        "driver: assigning WinUSB to {instance_id} (now {} {} {})",
        before.service,
        before.inf_name,
        before.version
    );
    tracing::debug!(
        "driver: previous driver {:?} ({}), section {}, class {} {}; INF {}",
        before.description,
        before.provider,
        before.inf_section,
        before.class_name,
        before.class_guid,
        crate::autostart::utf16_string(&inf)
    );

    // Step 2: the interface GUID, kept beside any GUIDs already there
    let guids_before = write_interface_guid(&set, &data).map_err(DriverError::win32("writing DeviceInterfaceGUIDs"))?;

    // Steps 3 and 4, on a thread: Windows may not return (INSTALL_LIMIT)
    let (tx, rx) = std::sync::mpsc::channel();
    let job = InstallJob(set, data);
    std::thread::spawn(move || {
        let InstallJob(set, mut data) = job;
        let mut class_changed = false;
        let mut result = install(&set, &mut data, &inf, &mut class_changed);
        if let Ok(true) = result {
            // Windows could not stop the device to load WinUSB (issue #81:
            // the core then fails with "failed to initialize WinUSB (error
            // 50)" until the adapter is replugged or Windows restarts).
            // Restart it as Device Manager does; only what still needs a
            // restart after that is reported
            result = Ok(restart_device(&set, &mut data));
        }
        let _ = tx.send((result, class_changed, InstallJob(set, data)));
    });
    let (result, class_changed, InstallJob(set, _)) = match rx.recv_timeout(INSTALL_LIMIT) {
        Ok(done) => done,
        Err(_) => {
            // WinUSB is on the device, but its old driver never stopped:
            // a port reset loads it, otherwise the next restart of Windows
            tracing::warn!(
                "driver: installing WinUSB on {instance_id} did not finish in {INSTALL_LIMIT:?}; resetting the USB port"
            );
            let replugged = replug(instance_id).filter(running_on_winusb);
            let reboot_required = replugged.is_none();
            let state = match replugged {
                Some(state) => state,
                None => driver_state(instance_id)?,
            };
            if reboot_required {
                tracing::warn!(
                    "driver: {instance_id} now {} ({:?}), problem {:?}: restart needed",
                    state.driver.service,
                    state.kind,
                    state.problem_code
                );
                radio_back_on();
            }
            let class_changed = !state.driver.class_guid.eq_ignore_ascii_case(&before.class_guid);
            hide_from_safe_removal(instance_id, true);
            return Ok(AssignOutcome {
                previous,
                already_winusb: false,
                class_changed,
                reboot_required,
                state,
                disconnected,
            });
        }
    };
    tracing::debug!(
        "driver: install on {instance_id}: {}, class changed {class_changed}",
        match &result {
            Ok(reboot) => format!("ok (reboot flag {reboot})"),
            Err(Failure::ModelNotFound(class)) => format!("no WINUSB model for class {class}"),
            Err(Failure::Win32(step, e)) =>
                format!("{step} failed: {e} (error {})", crate::usb_adapters::win32_code(e)),
        }
    );
    match result {
        Ok(mut reboot_required) => {
            drop(set); // the port reset removes the device
            if reboot_required {
                // Windows could not restart the device, and a driver
                // cannot refuse a port reset
                reboot_required = replug(instance_id).filter(running_on_winusb).is_none();
                if reboot_required {
                    radio_back_on();
                }
            }
            hide_from_safe_removal(instance_id, true);
            let state = driver_state(instance_id)?;
            tracing::info!(
                "driver: {instance_id} is now on {} ({:?}){}",
                state.driver.service,
                state.kind,
                if reboot_required { ", restart needed" } else { "" }
            );
            Ok(AssignOutcome { previous, already_winusb: false, class_changed, reboot_required, state, disconnected })
        }
        Err(failure) => {
            drop(set); // release the device before putting it back
            let rollback = undo_assign(instance_id, &previous, class_changed, guids_before);
            radio_back_on();
            tracing::debug!("driver: rollback of {instance_id}: {rollback:?}");
            Err(match failure {
                Failure::ModelNotFound(class) => DriverError::WinUsbModelNotFound { class, rollback },
                Failure::Win32(step, source) => DriverError::Win32 { step, source, rollback },
            })
        }
    }
}

/// Puts a WinUSB device back on the driver Windows picks for it (see the
/// module documentation); `previous` from `assign_winusb` is compared with
/// the result. Needs administrator rights; never called from tests.
///
/// An adapter Zadig put on WinUSB is different: Zadig's generated INF stays
/// in the driver store and matches the adapter's hardware ID, which ranks
/// above Windows' Bluetooth driver (a compatible-ID match), so Windows picks
/// WinUSB again. The outcome then shows kind WinUsb; removing that package
/// (`pnputil /delete-driver oemNN.inf`) is not done here.
pub fn restore(instance_id: &str, previous: Option<&DriverRecord>) -> Result<RestoreOutcome, DriverError> {
    if !is_elevated() {
        return Err(DriverError::NotElevated);
    }
    require_usb(instance_id)?;
    let (set, data) = open(instance_id)?;
    let now = set.driver_info(&data);
    if DriverKind::classify(&now.service, &now.class_name) != DriverKind::WinUsb {
        // Not ours to undo (also when the user already fixed it by hand)
        tracing::info!("driver: {instance_id} is not on WinUSB ({}); nothing to restore", now.service);
        return Ok(RestoreOutcome {
            already_restored: true,
            reboot_required: false,
            matches_previous: previous.map(|p| p.is_back(&now)),
            state: Some(driver_state(instance_id)?),
        });
    }
    tracing::info!("driver: restoring {instance_id} from WinUSB");
    hide_from_safe_removal(instance_id, false);
    // Read before the restore: removing the device deletes its key
    let radio_was_switched_off = take_radio_off_mark(instance_id);
    tracing::debug!(
        "driver: now {} {} {} (class {}), record {:?}",
        now.service,
        now.inf_name,
        now.version,
        now.class_guid,
        previous
    );
    let started = Instant::now();
    let in_place = match previous.filter(|p| !p.inf_name.is_empty()) {
        Some(p) => reinstall_previous(set, data, p),
        None => {
            tracing::info!("driver: no record of the driver before; removing the device");
            InPlace::Failed(set, data)
        }
    };
    let (mut reboot_required, mut state) = match in_place {
        InPlace::Done(reboot) => {
            let state = wait_for_driver(instance_id);
            let recorded = state.as_ref().is_some_and(|s| {
                s.kind != DriverKind::WinUsb
                    && previous.is_some_and(|p| p.service.eq_ignore_ascii_case(&s.driver.service))
            });
            if recorded {
                remove_interface_guid(instance_id);
                tracing::info!("driver: put back in place in {} ms", started.elapsed().as_millis());
                // Read again: the state above still lists the GUID
                (reboot, driver_state(instance_id).ok().or(state))
            } else {
                tracing::warn!(
                    "driver: after the install {instance_id} is on {:?}, not the recorded driver; removing the device",
                    state.as_ref().map(|s| s.driver.service.as_str())
                );
                let (set, data) = open(instance_id)?;
                let reboot = uninstall_and_reenumerate(&set, &data, instance_id)?;
                drop(set);
                (reboot, wait_for_driver(instance_id))
            }
        }
        InPlace::Hung => {
            // As in assign_winusb: Windows finishes it at the next restart
            tracing::warn!(
                "driver: installing the previous driver did not finish in {INSTALL_LIMIT:?}: restart needed"
            );
            (true, driver_state(instance_id).ok())
        }
        InPlace::Failed(set, data) => {
            let reboot = uninstall_and_reenumerate(&set, &data, instance_id)?;
            drop(set);
            (reboot, wait_for_driver(instance_id))
        }
    };
    if reboot_required {
        tracing::info!("driver: Windows asks for a restart; resetting the USB port");
        if let Some(back) = replug(instance_id).filter(|s| s.kind != DriverKind::WinUsb && s.problem_code.is_none()) {
            reboot_required = false;
            state = Some(back);
        }
    }
    if radio_was_switched_off {
        if reboot_required {
            tracing::info!("driver: Windows' Bluetooth was switched off for WinUSB; it stays off until the restart");
        } else {
            crate::bt_radios::switch_on_when_back(RADIO_BACK_WAIT);
        }
    }
    let matches_previous = match (previous, &state) {
        (Some(p), Some(s)) => Some(p.is_back(&s.driver)),
        _ => None,
    };
    if let (Some(p), Some(s)) = (previous, &state)
        && p.is_back(&s.driver)
        && p.version != s.driver.version
    {
        tracing::info!("driver: recorded version {}, Windows installed {}", p.version, s.driver.version);
    }
    if let Some(s) = &state {
        tracing::info!(
            "driver: {instance_id} is now on {} {} {} (matches the record: {matches_previous:?})",
            s.driver.service,
            s.driver.inf_name,
            s.driver.version
        );
    } else {
        tracing::warn!("driver: {instance_id} did not come back within {RESTORE_WAIT:?}");
    }
    Ok(RestoreOutcome { already_restored: false, reboot_required, state, matches_previous })
}

/// How putting the recorded driver back in place went
enum InPlace {
    /// Installed; whether Windows still asks for a restart
    Done(bool),
    /// DiInstallDevice did not return within INSTALL_LIMIT
    Hung,
    /// Not installed (the device is given back for the removal)
    Failed(DevInfoSet, SP_DEVINFO_DATA),
}

/// Installs the recorded driver on the running device, as `assign_winusb`
/// installs WinUSB (on a thread for the same reason: Windows may not return)
fn reinstall_previous(set: DevInfoSet, data: SP_DEVINFO_DATA, previous: &DriverRecord) -> InPlace {
    let inf = match inf_path(&previous.inf_name) {
        Ok(inf) => inf,
        Err(missing) => {
            tracing::info!("driver: the previous driver's INF is gone ({missing}); removing the device");
            return InPlace::Failed(set, data);
        }
    };
    let class = parse_guid(&previous.class_guid);
    wait_for_pending_installs();
    let record = previous.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let job = InstallJob(set, data);
    std::thread::spawn(move || {
        let InstallJob(set, mut data) = job;
        let mut result = install_previous(&set, &mut data, &inf, &record, class);
        if let Ok(true) = result {
            result = Ok(restart_device(&set, &mut data));
        }
        let _ = tx.send((result, InstallJob(set, data)));
    });
    match rx.recv_timeout(INSTALL_LIMIT) {
        Ok((Ok(reboot), _)) => InPlace::Done(reboot),
        Ok((Err(failure), InstallJob(set, data))) => {
            match failure {
                Failure::ModelNotFound(class) => tracing::info!(
                    "driver: {} offers no model of the record (class {class}); removing the device",
                    previous.inf_name
                ),
                Failure::Win32(step, e) => tracing::warn!(
                    "driver: putting {} back in place: {step} failed: {e} (error {}); removing the device",
                    previous.inf_name,
                    crate::usb_adapters::win32_code(&e)
                ),
            }
            InPlace::Failed(set, data)
        }
        Err(_) => InPlace::Hung,
    }
}

/// The class of the record first (the device is in winusb.inf's class now),
/// then its model from its INF
fn install_previous(
    set: &DevInfoSet,
    data: &mut SP_DEVINFO_DATA,
    inf: &[u16; 260],
    record: &DriverRecord,
    class: Option<GUID>,
) -> Result<bool, Failure> {
    if let Some(class) = class
        && data.ClassGuid != class
    {
        set_class(set, data, class)?;
    }
    let found = find_driver(set, data, inf, |section, description| is_recorded_model(record, section, description))?;
    let Some(mut driver) = found else {
        return Err(Failure::ModelNotFound(format_guid(&data.ClassGuid)));
    };
    install_selected(set, data, &mut driver, &record.inf_name)
}

/// Whether a model of the record's INF is the recorded driver: by install
/// section (Windows records it with its .NT decoration: `Composite.Dev.NT`
/// for the model section `Composite.Dev`), or by description in records
/// without one
fn is_recorded_model(record: &DriverRecord, section: &str, description: &str) -> bool {
    if record.inf_section.is_empty() {
        return !record.description.is_empty() && record.description == description;
    }
    let installed = record.inf_section.to_ascii_lowercase();
    let section = section.to_ascii_lowercase();
    !section.is_empty()
        && (installed == section || installed.strip_prefix(&section).is_some_and(|rest| rest.starts_with(".nt")))
}

/// Moves the device to another setup class (the class driver list holds
/// only drivers of the device's class)
fn set_class(set: &DevInfoSet, data: &mut SP_DEVINFO_DATA, class: GUID) -> Result<(), Failure> {
    tracing::info!("driver: changing the class from {} to {}", format_guid(&data.ClassGuid), format_guid(&class));
    // SAFETY: valid set and element; a list that does not exist is no error.
    let _ = unsafe { SetupDiDestroyDriverInfoList(set.0, Some(data), SPDIT_CLASSDRIVER) };
    let text: Vec<u16> = multi_sz(&[format_guid(&class)]);
    let bytes = u16_bytes(&text[..text.len() - 1]); // one string, one terminator
    // SAFETY: valid set and element; REG_SZ data with its terminator.
    unsafe { SetupDiSetDeviceRegistryPropertyW(set.0, data, SPDRP_CLASSGUID, Some(&bytes)) }
        .map_err(step("changing the device class"))?;
    tracing::debug!("driver: element class now {}", format_guid(&data.ClassGuid));
    Ok(())
}

/// `{...}` as `DriverInfo::class_guid` holds it
fn parse_guid(text: &str) -> Option<GUID> {
    let inner = text.trim().trim_start_matches('{').trim_end_matches('}');
    GUID::try_from(inner).ok()
}

/// As Zadig's library does before installing: lets installs Windows runs
/// already finish first
fn wait_for_pending_installs() {
    let start = Instant::now();
    // SAFETY: no pointers; waits at most the given time.
    let r = unsafe { CMP_WaitNoPendingInstallEvents(PENDING_INSTALL_WAIT_MS) };
    let ms = start.elapsed().as_millis();
    if r != 0 || ms >= 100 {
        tracing::info!("driver: waited {ms} ms for installs Windows was running (result {r})");
    }
}

/// After a restore in place: BlueCaster's GUID out of DeviceInterfaceGUIDs
/// (the removal deletes the whole key; in place it would stay)
fn remove_interface_guid(instance_id: &str) {
    let Ok((set, data)) = open(instance_id) else { return };
    // SAFETY: valid set and element; the existing hardware key.
    let key = unsafe { SetupDiCreateDevRegKeyW(set.0, &data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, None, PCWSTR::null()) };
    let Ok(key) = key else { return };
    let key = RegKey(key);
    let guids = match key.query(INTERFACE_GUIDS_VALUE) {
        Ok(Some((_, d))) => parse_multi_sz(&d),
        _ => return,
    };
    let rest: Vec<String> = guids.iter().filter(|g| !g.eq_ignore_ascii_case(INTERFACE_GUID)).cloned().collect();
    if rest.len() == guids.len() {
        return;
    }
    let r = if rest.is_empty() {
        key.delete_value(INTERFACE_GUIDS_VALUE).map(|_| ())
    } else {
        key.set(INTERFACE_GUIDS_VALUE, REG_MULTI_SZ, &multi_sz(&rest)).map(|_| ())
    };
    tracing::debug!("driver: BlueCaster's {INTERFACE_GUIDS_VALUE} entry removed: {r:?}");
}

/// A REG_MULTI_SZ value: each string NUL-terminated, then one more NUL
pub fn multi_sz<S: AsRef<str>>(strings: &[S]) -> Vec<u16> {
    let mut out = Vec::new();
    for s in strings {
        out.extend(s.as_ref().encode_utf16());
        out.push(0);
    }
    out.push(0);
    out
}

/// `a.b.c.d` from a driver version packed in 64 bits (SP_DRVINFO_DATA)
pub fn format_driver_version(v: u64) -> String {
    format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF, v & 0xFFFF)
}

enum Failure {
    ModelNotFound(String),
    Win32(&'static str, windows::core::Error),
}

fn step(name: &'static str) -> impl FnOnce(windows::core::Error) -> Failure {
    move |e| Failure::Win32(name, e)
}

/// Steps 3 and 4; returns whether Windows asks for a restart
fn install(
    set: &DevInfoSet,
    data: &mut SP_DEVINFO_DATA,
    inf: &[u16; 260],
    class_changed: &mut bool,
) -> Result<bool, Failure> {
    let mut found = find_driver(set, data, inf, is_winusb_model)?;
    if found.is_none() && data.ClassGuid != USB_DEVICE_CLASS {
        // The class driver list may hold only drivers of the device's own
        // class. Move the device to winusb.inf's class first, as
        // installing it would; undone by the rollback if this fails.
        tracing::info!("driver: winusb.inf not offered for class {}", format_guid(&data.ClassGuid));
        set_class(set, data, USB_DEVICE_CLASS)?;
        *class_changed = true;
        found = find_driver(set, data, inf, is_winusb_model)?;
    }
    let Some(mut driver) = found else {
        return Err(Failure::ModelNotFound(format_guid(&data.ClassGuid)));
    };
    install_selected(set, data, &mut driver, "winusb.inf")
}

/// Selects `driver` from the element's class driver list and installs it;
/// returns whether Windows asks for a restart
fn install_selected(
    set: &DevInfoSet,
    data: &mut SP_DEVINFO_DATA,
    driver: &mut SP_DRVINFO_DATA_V2_W,
    inf_name: &str,
) -> Result<bool, Failure> {
    tracing::info!(
        "driver: selected \"{}\" {} from {inf_name}",
        crate::autostart::utf16_string(&driver.Description),
        format_driver_version(driver.DriverVersion)
    );
    // SAFETY: the driver came from this element's class driver list.
    unsafe { SetupDiSetSelectedDriverW(set.0, Some(data), Some(driver)) }.map_err(step("SetupDiSetSelectedDriver"))?;
    tracing::debug!("driver: driver selected; calling DiInstallDevice");
    let started = Instant::now();
    let mut reboot = BOOL(0);
    // SAFETY: valid set, element and its selected driver; no window (no
    // UI is shown with DIIDFLAG_NOFINISHINSTALLUI).
    unsafe { DiInstallDevice(None, set.0, data, Some(driver), DIIDFLAG_NOFINISHINSTALLUI, Some(&mut reboot)) }
        .map_err(step("DiInstallDevice"))?;
    tracing::info!("driver: installed in {} ms, reboot flag {}", started.elapsed().as_millis(), reboot.as_bool());
    Ok(reboot.as_bool())
}

/// Restarts the device (DIF_PROPERTYCHANGE / DICS_PROPCHANGE, as Device
/// Manager and devcon restart do) so that the driver just installed loads.
/// Returns whether Windows still needs a restart for it.
fn restart_device(set: &DevInfoSet, data: &mut SP_DEVINFO_DATA) -> bool {
    tracing::debug!("driver: Windows asks for a restart; restarting the device (DIF_PROPERTYCHANGE)");
    let params = SP_PROPCHANGE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_PROPERTYCHANGE,
        },
        StateChange: DICS_PROPCHANGE,
        Scope: DICS_FLAG_GLOBAL,
        HwProfile: 0,
    };
    // SAFETY: valid set and element; the header starts the parameters,
    // which are passed with their full size.
    let restarted = unsafe {
        SetupDiSetClassInstallParamsW(
            set.0,
            Some(data),
            Some(&params.ClassInstallHeader),
            size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        )
    }
    .and_then(|()| {
        // SAFETY: valid set and element with its class install parameters.
        unsafe { SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.0, Some(data)) }
    });
    if let Err(e) = restarted {
        tracing::warn!("driver: restarting the device failed: {e}");
        return true;
    }
    let mut install =
        SP_DEVINSTALL_PARAMS_W { cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32, ..Default::default() };
    // SAFETY: valid set and element; out structure with its size set.
    if let Err(e) = unsafe { SetupDiGetDeviceInstallParamsW(set.0, Some(data), &mut install) } {
        tracing::warn!("driver: reading the install parameters failed: {e}");
        return true;
    }
    let still = (install.Flags & (DI_NEEDRESTART | DI_NEEDREBOOT)).0 != 0;
    tracing::debug!("driver: install flags after the restart 0x{:08X}", install.Flags.0);
    tracing::info!("driver: device restarted{}", if still { ", Windows still asks for a restart" } else { "" });
    still
}

fn is_winusb_model(section: &str, _description: &str) -> bool {
    section.eq_ignore_ascii_case(WINUSB_SECTION)
}

/// Step 3: the class driver list from one INF alone (winusb.inf, or the
/// recorded driver's INF for a restore); the first model `wanted` takes by
/// its install section and description
fn find_driver(
    set: &DevInfoSet,
    data: &mut SP_DEVINFO_DATA,
    inf: &[u16; 260],
    wanted: impl Fn(&str, &str) -> bool,
) -> Result<Option<SP_DRVINFO_DATA_V2_W>, Failure> {
    let mut params =
        SP_DEVINSTALL_PARAMS_W { cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32, ..Default::default() };
    // SAFETY: valid set and element; out structure with its size set.
    unsafe { SetupDiGetDeviceInstallParamsW(set.0, Some(data), &mut params) }
        .map_err(step("SetupDiGetDeviceInstallParams"))?;
    params.Flags |= DI_ENUMSINGLEINF;
    // winusb.inf's models are not ExcludeFromSelect today; harmless if so
    params.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
    params.DriverPath = *inf;
    // SAFETY: as above, with the structure filled in.
    unsafe { SetupDiSetDeviceInstallParamsW(set.0, Some(data), &params) }
        .map_err(step("SetupDiSetDeviceInstallParams"))?;
    tracing::debug!(
        "driver: building the driver list from {} for class {} (flags 0x{:08X}, ex 0x{:08X})",
        crate::autostart::utf16_string(inf),
        format_guid(&data.ClassGuid),
        params.Flags.0,
        params.FlagsEx.0
    );
    // SAFETY: valid set and element.
    unsafe { SetupDiBuildDriverInfoList(set.0, Some(data), SPDIT_CLASSDRIVER) }
        .map_err(step("SetupDiBuildDriverInfoList"))?;
    for index in 0.. {
        let mut driver =
            SP_DRVINFO_DATA_V2_W { cbSize: size_of::<SP_DRVINFO_DATA_V2_W>() as u32, ..Default::default() };
        // SAFETY: valid set and element; out structure with its size set.
        if let Err(e) = unsafe { SetupDiEnumDriverInfoW(set.0, Some(data), SPDIT_CLASSDRIVER, index, &mut driver) } {
            if e.code() == ERROR_NO_MORE_ITEMS.to_hresult() {
                break;
            }
            return Err(Failure::Win32("SetupDiEnumDriverInfo", e));
        }
        let section = driver_section(set, data, &driver).unwrap_or_default();
        let description = crate::autostart::utf16_string(&driver.Description);
        tracing::debug!("driver: the INF offers \"{description}\" (section {section:?})");
        if wanted(&section, &description) {
            return Ok(Some(driver));
        }
    }
    tracing::debug!("driver: no wanted model in the list for class {}", format_guid(&data.ClassGuid));
    Ok(None)
}

/// Install section name of a driver in the list
fn driver_section(set: &DevInfoSet, data: &SP_DEVINFO_DATA, driver: &SP_DRVINFO_DATA_V2_W) -> Option<String> {
    let mut size = 0u32;
    // SAFETY: size query without a buffer.
    let _ = unsafe { SetupDiGetDriverInfoDetailW(set.0, Some(data), driver, None, 0, Some(&mut size)) };
    let size = (size as usize).max(size_of::<SP_DRVINFO_DETAIL_DATA_W>());
    // u64 units keep the structure aligned
    let mut buf = vec![0u64; size.div_ceil(8)];
    let detail = buf.as_mut_ptr().cast::<SP_DRVINFO_DETAIL_DATA_W>();
    // SAFETY: the buffer holds `size` bytes, aligned; cbSize is the fixed
    // structure size, as the API requires.
    unsafe { (*detail).cbSize = size_of::<SP_DRVINFO_DETAIL_DATA_W>() as u32 };
    // SAFETY: buffer of `size` bytes.
    unsafe { SetupDiGetDriverInfoDetailW(set.0, Some(data), driver, Some(detail), size as u32, None) }.ok()?;
    // SAFETY: filled in by the call above.
    Some(crate::autostart::utf16_string(unsafe { &(*detail).SectionName }))
}

/// Adds INTERFACE_GUID to DeviceInterfaceGUIDs in the hardware key
/// (Device Parameters); returns the value as it was, None if absent
fn write_interface_guid(set: &DevInfoSet, data: &SP_DEVINFO_DATA) -> windows::core::Result<Option<Vec<u16>>> {
    // SAFETY: valid set and element; the hardware key of the device
    // (created if missing), no INF section to run.
    let key = RegKey(unsafe {
        SetupDiCreateDevRegKeyW(set.0, data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, None, PCWSTR::null())
    }?);
    let before = key.query(INTERFACE_GUIDS_VALUE)?.map(|(_, d)| d);
    let mut guids = before.as_deref().map(parse_multi_sz).unwrap_or_default();
    if !guids.iter().any(|g| g.eq_ignore_ascii_case(INTERFACE_GUID)) {
        guids.push(INTERFACE_GUID.to_string());
        key.set(INTERFACE_GUIDS_VALUE, REG_MULTI_SZ, &multi_sz(&guids))?;
        tracing::debug!("driver: {INTERFACE_GUIDS_VALUE} written: {guids:?}");
    } else {
        tracing::debug!("driver: {INTERFACE_GUIDS_VALUE} has BlueCaster's GUID already: {guids:?}");
    }
    Ok(before)
}

fn read_interface_guids(set: &DevInfoSet, data: &SP_DEVINFO_DATA) -> Vec<String> {
    // SAFETY: valid set and element; read access only. Fails (nothing to
    // read) when the device has no hardware key.
    let Ok(key) = (unsafe { SetupDiOpenDevRegKey(set.0, data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, KEY_READ.0) }) else {
        return Vec::new();
    };
    let key = RegKey(key);
    match key.query(INTERFACE_GUIDS_VALUE) {
        Ok(Some((ty, d))) if ty == REG_MULTI_SZ => parse_multi_sz(&d),
        Ok(Some((_, d))) => vec![crate::autostart::utf16_string(&d)], // REG_SZ written by hand
        _ => Vec::new(),
    }
}

/// After a failed assignment: if the driver is still the one from before
/// (nothing but the interface GUID changed), put the GUID value back as it
/// was; otherwise uninstall and re-enumerate as `restore` does
fn undo_assign(
    instance_id: &str,
    previous: &DriverRecord,
    class_changed: bool,
    guids_before: Option<Vec<u16>>,
) -> Rollback {
    let (set, data) = match open(instance_id) {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("driver: undo: cannot open {instance_id}: {e}");
            return Rollback::Failed;
        }
    };
    let now = set.driver_info(&data);
    tracing::debug!(
        "driver: undo: now {} {} {}, class changed {class_changed}, same driver as before {}",
        now.service,
        now.inf_name,
        now.version,
        previous.matches(&now)
    );
    if !class_changed && previous.matches(&now) {
        // SAFETY: valid set and element; the existing hardware key.
        let key =
            unsafe { SetupDiCreateDevRegKeyW(set.0, &data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, None, PCWSTR::null()) };
        match key {
            Ok(key) => {
                let key = RegKey(key);
                let had = guids_before.is_some();
                let r = match guids_before {
                    Some(d) => key.set(INTERFACE_GUIDS_VALUE, REG_MULTI_SZ, &d).map(|_| true),
                    None => key.delete_value(INTERFACE_GUIDS_VALUE),
                };
                tracing::debug!(
                    "driver: undo: {INTERFACE_GUIDS_VALUE} {}: {r:?}",
                    if had { "put back" } else { "removed" }
                );
            }
            Err(e) => tracing::debug!("driver: undo: cannot open the device key: {e}"),
        }
        return Rollback::NotNeeded;
    }
    tracing::warn!("driver: assignment failed half way; putting {instance_id} back");
    match uninstall_and_reenumerate(&set, &data, instance_id) {
        Ok(_) => {
            drop(set);
            if wait_for_driver(instance_id).is_some() { Rollback::Restored } else { Rollback::Failed }
        }
        Err(e) => {
            tracing::error!("driver: putting {instance_id} back failed: {e}");
            Rollback::Failed
        }
    }
}

/// Removes the device and lets Windows find it again; returns whether a
/// restart is needed
fn uninstall_and_reenumerate(set: &DevInfoSet, data: &SP_DEVINFO_DATA, instance_id: &str) -> Result<bool, DriverError> {
    // The parent (the hub port) must be found before the device is gone
    let devnode = locate(instance_id);
    let parent = devnode.and_then(|dev| {
        let mut parent = 0u32;
        // SAFETY: valid devinst and out pointer.
        (unsafe { CM_Get_Parent(&mut parent, dev, 0) } == CR_SUCCESS).then_some(parent)
    });
    tracing::debug!("driver: uninstalling {instance_id} (parent devnode {parent:?})");
    if let Some(devnode) = devnode {
        stop_device(devnode);
    }
    let mut reboot = BOOL(0);
    let start = Instant::now();
    // SAFETY: valid set and element; no window, default flags (the driver
    // package stays in the driver store).
    unsafe { DiUninstallDevice(HWND::default(), set.0, data, 0, Some(&mut reboot)) }
        .map_err(DriverError::win32("DiUninstallDevice"))?;
    tracing::info!("driver: uninstalled in {} ms, reboot {}", start.elapsed().as_millis(), reboot.as_bool());
    let start = Instant::now();
    let reenumerated = parent.is_some_and(reenumerate);
    tracing::info!(
        "driver: re-enumeration of the parent requested in {} ms: {reenumerated}",
        start.elapsed().as_millis()
    );
    if !reenumerated {
        // No parent known (the device was not present) or it failed: the
        // whole tree
        if !rescan_devices(false) {
            tracing::warn!("driver: re-enumerating devices failed; replugging the adapter brings it back");
        }
    }
    Ok(reboot.as_bool())
}

/// Takes the running device out before DiUninstallDevice, which otherwise
/// does it inside: done apart, the log shows whether a slow restore (issue
/// #86, MediaTek) is spent stopping the WinUSB stack or in the uninstall
/// itself, and who refuses the removal if anyone does. A refusal is only
/// logged: DiUninstallDevice still runs and asks for a restart if needed.
fn stop_device(devnode: u32) {
    let mut veto = PNP_VETO_TYPE(0);
    let mut name = [0u16; 260];
    let start = Instant::now();
    // SAFETY: a devinst from CM_Locate_DevNode; out pointer and buffer valid
    // for the call. No UI; the device stays removed until it is uninstalled.
    let cr = unsafe {
        CM_Query_And_Remove_SubTreeW(
            devnode,
            Some(&mut veto),
            Some(&mut name),
            CM_REMOVE_UI_NOT_OK | CM_REMOVE_NO_RESTART,
        )
    };
    let ms = start.elapsed().as_millis();
    if cr == CR_SUCCESS {
        tracing::info!("driver: device stopped in {ms} ms");
        return;
    }
    // App and service names are logged; device instance IDs and paths are not
    let name = crate::autostart::utf16_string(&name);
    let by = if name.is_empty() {
        "-"
    } else if name.contains('\\') {
        "a device or file"
    } else {
        name.as_str()
    };
    tracing::info!("driver: stopping the device refused after {ms} ms: CR {}, veto type {}, by {by}", cr.0, veto.0);
}

/// Asks Windows to look for devices under `devinst` again, without waiting:
/// `wait_for_driver` watches for the adapter itself. A synchronous request
/// returns only once the whole subtree is done, which on a composite adapter
/// (MediaTek's, issue #86) is the interfaces, the radio and a node per
/// paired device.
fn reenumerate(devinst: u32) -> bool {
    reenumerate_with(devinst, CM_REENUMERATE_ASYNCHRONOUS)
}

fn reenumerate_with(devinst: u32, flags: CM_REENUMERATE_FLAGS) -> bool {
    // SAFETY: a devinst from CM_Locate_DevNode / CM_Get_Parent.
    let cr = unsafe { CM_Reenumerate_DevNode(devinst, flags) };
    if cr != CR_SUCCESS {
        tracing::debug!("driver: CM_Reenumerate_DevNode({devinst}) returned CR {}", cr.0);
    }
    cr == CR_SUCCESS
}

/// Device Manager's "Scan for hardware changes": the whole device tree
/// enumerated again; `wait` returns only once that is done. A device whose
/// driver was removed leaves Windows' lists then, without a restart (issue
/// #86: the Hi-Fi Cable's output stayed in Sound until a rescan). Needs
/// administrator rights (CR_ACCESS_DENIED otherwise).
pub fn rescan_devices(wait: bool) -> bool {
    let mut root = 0u32;
    // SAFETY: a NULL device ID locates the root of the device tree.
    let cr = unsafe { CM_Locate_DevNodeW(&mut root, PCWSTR::null(), CM_LOCATE_DEVNODE_NORMAL) };
    if cr != CR_SUCCESS {
        tracing::debug!("driver: locating the device tree's root returned CR {}", cr.0);
        return false;
    }
    reenumerate_with(root, if wait { CM_REENUMERATE_SYNCHRONOUS } else { CM_REENUMERATE_ASYNCHRONOUS })
}

/// After a re-enumeration: waits until Windows has installed the device
/// again and returns its state, None if it did not come back in time.
///
/// Only the device itself is waited for, not every install it sets off
/// (issue #86: on a composite adapter, such as MediaTek's, the interfaces,
/// the radio and a device node per paired device follow it, and waiting
/// for all of them took most of the restore's time).
fn wait_for_driver(instance_id: &str) -> Option<DriverState> {
    let start = Instant::now();
    let mut problem_since: Option<Instant> = None;
    loop {
        // A device back without a driver (problem 28) may only be waiting
        // for its install to start, so that is accepted only at the end
        if let Ok(state) = driver_state(instance_id)
            && state.present
            && state.kind != DriverKind::None
        {
            match state.problem_code {
                None => {
                    tracing::info!("driver: back after {} ms", start.elapsed().as_millis());
                    return Some(state);
                }
                // A problem may only last while the driver starts; one that
                // stays is the answer (Code 31: Windows runs another radio)
                Some(code) => {
                    let since = *problem_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= PROBLEM_SETTLE {
                        tracing::info!(
                            "driver: back after {} ms with problem code {code}",
                            start.elapsed().as_millis()
                        );
                        return Some(state);
                    }
                }
            }
        } else {
            problem_since = None;
        }
        if start.elapsed() >= RESTORE_WAIT {
            let last = driver_state(instance_id);
            tracing::debug!("driver: waited {:?} for {instance_id}; last state {last:?}", start.elapsed());
            return last.ok().filter(|s| s.present);
        }
        std::thread::sleep(RESTORE_POLL);
    }
}

/// Whether Windows lists the device under "Safely Remove Hardware". An
/// adapter on WinUSB is listed (a removable USB device whose driver does
/// not say that pulling it out is fine, as Windows' Bluetooth driver does),
/// with an Eject entry that only takes the adapter away from BlueCaster
/// (issue #86). `hide` takes it off the list, `!hide` leaves it to Windows
/// again. Best effort: the list is all that depends on it.
fn hide_from_safe_removal(instance_id: &str, hide: bool) {
    let Some(devinst) = locate(instance_id) else { return };
    // DEVPROP_FALSE: safe removal not required. No value (type empty)
    // deletes the property.
    let (ty, value): (_, Option<&[u8]>) =
        if hide { (DEVPROP_TYPE_BOOLEAN, Some(&[0])) } else { (DEVPROP_TYPE_EMPTY, None) };
    // SAFETY: valid devinst, a key constant and a buffer of the type's size.
    let cr = unsafe { CM_Set_DevNode_PropertyW(devinst, &DEVPKEY_Device_SafeRemovalRequiredOverride, ty, value, 0) };
    tracing::debug!("driver: safe removal override {}: {}", if hide { "set" } else { "removed" }, cr.0);
}

/// Marks (or unmarks) in the device's hardware key that the switch to WinUSB
/// turned Windows' Bluetooth off, so that the restore turns it on again
fn mark_radio_off(instance_id: &str, marked: bool) {
    let Ok((set, data)) = open(instance_id) else { return };
    // SAFETY: valid set and element; the hardware key (created if missing).
    let key = unsafe { SetupDiCreateDevRegKeyW(set.0, &data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, None, PCWSTR::null()) };
    let Ok(key) = key else { return };
    let key = RegKey(key);
    let r = if marked {
        key.set(RADIO_OFF_VALUE, REG_DWORD, &[1, 0])
    } else {
        key.delete_value(RADIO_OFF_VALUE).map(|_| ())
    };
    tracing::debug!("driver: {RADIO_OFF_VALUE} {}: {r:?}", if marked { "set" } else { "removed" });
}

/// Whether the switch to WinUSB turned Windows' Bluetooth off; the mark is
/// removed
fn take_radio_off_mark(instance_id: &str) -> bool {
    let Ok((set, data)) = open(instance_id) else { return false };
    // SAFETY: valid set and element; read access only.
    let marked = unsafe { SetupDiOpenDevRegKey(set.0, &data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, KEY_READ.0) }
        .ok()
        .map(RegKey)
        .is_some_and(|key| matches!(key.query(RADIO_OFF_VALUE), Ok(Some(_))));
    drop(set);
    if marked {
        mark_radio_off(instance_id, false);
    }
    marked
}

/// Resets the adapter's USB port (IOCTL_USB_HUB_CYCLE_PORT), which Windows
/// handles as unplugging and plugging the adapter in again. A driver that
/// would not stop for a reinstall (Windows' Bluetooth with a device
/// connected) cannot refuse that, so the driver just installed loads
/// without a restart of Windows. Returns the state once the device is back.
fn replug(instance_id: &str) -> Option<DriverState> {
    let devinst = locate(instance_id)?;
    let Some((hub, port)) = hub_port(devinst) else {
        tracing::info!("driver: no USB hub port found for the adapter; cannot reset it");
        return None;
    };
    // SAFETY: NUL-terminated interface path; the hub's IOCTLs need write
    // access (administrator rights).
    let handle = unsafe {
        CreateFileW(
            PCWSTR(hub.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    let handle = match handle {
        Ok(h) => OwnedHandle(h),
        Err(e) => {
            tracing::warn!("driver: cannot open the USB hub: {e}");
            return None;
        }
    };
    let mut params = USB_CYCLE_PORT_PARAMS { ConnectionIndex: port, StatusReturned: 0 };
    let mut returned = 0u32;
    let size = size_of::<USB_CYCLE_PORT_PARAMS>() as u32;
    let started = Instant::now();
    // SAFETY: valid handle; the parameters are both input and output, with
    // their size passed.
    let r = unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_USB_HUB_CYCLE_PORT,
            Some(std::ptr::from_ref(&params).cast()),
            size,
            Some(std::ptr::from_mut(&mut params).cast()),
            size,
            Some(&mut returned),
            None,
        )
    };
    drop(handle);
    let status = params.StatusReturned;
    if let Err(e) = r {
        tracing::warn!("driver: resetting USB port {port} failed: {e} (status {status})");
        return None;
    }
    tracing::info!("driver: USB port {port} reset (status {status}); waiting for the adapter");
    // The adapter leaves first; the state before that is the old one
    while locate(instance_id).is_some() && started.elapsed() < REPLUG_GONE_WAIT {
        std::thread::sleep(Duration::from_millis(100));
    }
    tracing::debug!(
        "driver: adapter {} after {} ms",
        if locate(instance_id).is_some() { "still listed" } else { "gone" },
        started.elapsed().as_millis()
    );
    wait_for_driver(instance_id)
}

/// The interface path of the USB hub the device is plugged into (NUL
/// included) and the port number. A composite adapter's interface sits
/// below its USB device, so the tree is walked up to the hub.
fn hub_port(devinst: u32) -> Option<(Vec<u16>, u32)> {
    let mut dev = devinst;
    for _ in 0..4 {
        let mut parent = 0u32;
        // SAFETY: valid devinst and out pointer.
        if unsafe { CM_Get_Parent(&mut parent, dev, 0) } != CR_SUCCESS {
            return None;
        }
        if let Some(path) = hub_interface(parent) {
            // A USB device's address is its port number on the hub
            let mut ty = DEVPROPTYPE::default();
            let mut buf = [0u8; 4];
            let mut size = 4u32;
            // SAFETY: valid key, out type and a buffer of the size passed.
            let cr = unsafe {
                CM_Get_DevNode_PropertyW(dev, &DEVPKEY_Device_Address, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0)
            };
            if cr != CR_SUCCESS || ty != DEVPROP_TYPE_UINT32 {
                tracing::debug!("driver: no port number (CR {}, type {})", cr.0, ty.0);
                return None;
            }
            return Some((path, u32::from_le_bytes(buf)));
        }
        dev = parent;
    }
    None
}

/// The first USB hub interface of a device node, None if it is no hub
fn hub_interface(devinst: u32) -> Option<Vec<u16>> {
    let mut id = [0u16; 512];
    // SAFETY: valid devinst; the buffer length is passed.
    if unsafe { CM_Get_Device_IDW(devinst, &mut id, 0) } != CR_SUCCESS {
        return None;
    }
    let mut len = 0u32;
    // SAFETY: NUL-terminated ID that lives for the call; out pointer.
    let cr = unsafe {
        CM_Get_Device_Interface_List_SizeW(
            &mut len,
            &GUID_DEVINTERFACE_USB_HUB,
            PCWSTR(id.as_ptr()),
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if cr != CR_SUCCESS || len <= 1 {
        return None;
    }
    let mut list = vec![0u16; len as usize];
    // SAFETY: as above, with a buffer of the size Windows gave.
    let cr = unsafe {
        CM_Get_Device_Interface_ListW(
            &GUID_DEVINTERFACE_USB_HUB,
            PCWSTR(id.as_ptr()),
            &mut list,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
        )
    };
    if cr != CR_SUCCESS {
        return None;
    }
    let end = list.iter().position(|&c| c == 0)?;
    if end == 0 {
        return None;
    }
    list.truncate(end + 1);
    Some(list)
}

/// Present on WinUSB and working: the switch is done
fn running_on_winusb(state: &DriverState) -> bool {
    state.present && state.kind == DriverKind::WinUsb && state.problem_code.is_none()
}

/// The devinst of a present device
fn locate(instance_id: &str) -> Option<u32> {
    let mut devinst = 0u32;
    // SAFETY: NUL-terminated ID and out pointer.
    let cr = unsafe { CM_Locate_DevNodeW(&mut devinst, &HSTRING::from(instance_id), CM_LOCATE_DEVNODE_NORMAL) };
    (cr == CR_SUCCESS).then_some(devinst)
}

fn open(instance_id: &str) -> Result<(DevInfoSet, SP_DEVINFO_DATA), DriverError> {
    DevInfoSet::open(instance_id).map_err(|e| {
        tracing::debug!(
            "driver: SetupDiOpenDeviceInfo({instance_id}): {e} (error {})",
            crate::usb_adapters::win32_code(&e)
        );
        if e.code() == ERROR_NO_SUCH_DEVINST.to_hresult() || e.code() == ERROR_INVALID_DEVINST_NAME.to_hresult() {
            DriverError::DeviceNotFound(instance_id.to_string())
        } else {
            DriverError::Win32 { step: "SetupDiOpenDeviceInfo", source: e, rollback: Rollback::NotNeeded }
        }
    })
}

/// Only USB devices: a guard against an instance ID of anything else
/// (a PCI controller, a system device) reaching the install code
fn require_usb(instance_id: &str) -> Result<(), DriverError> {
    if instance_id.len() > 4 && instance_id[..4].eq_ignore_ascii_case("USB\\") {
        Ok(())
    } else {
        Err(DriverError::NotUsb(instance_id.to_string()))
    }
}

/// `%SystemRoot%\INF\<name>` as SP_DEVINSTALL_PARAMS.DriverPath wants it;
/// the error names what is missing
fn inf_path(name: &str) -> Result<[u16; 260], String> {
    if name.is_empty() || name.contains(['\\', '/', ':']) {
        return Err(format!("not an INF name: {name:?}"));
    }
    let mut dir = [0u16; 260];
    // SAFETY: the buffer length is passed.
    let n = unsafe { GetWindowsDirectoryW(Some(&mut dir)) } as usize;
    if n == 0 || n >= dir.len() {
        return Err("%SystemRoot% not found".into());
    }
    let path = format!(r"{}\INF\{name}", String::from_utf16_lossy(&dir[..n]));
    if !std::path::Path::new(&path).is_file() {
        return Err(path);
    }
    let mut out = [0u16; 260];
    let units: Vec<u16> = path.encode_utf16().collect();
    if units.len() >= out.len() {
        return Err(path);
    }
    out[..units.len()].copy_from_slice(&units);
    Ok(out)
}

fn u16_bytes(data: &[u16]) -> Vec<u8> {
    data.iter().flat_map(|u| u.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz_layout() {
        let v = multi_sz(&[INTERFACE_GUID]);
        assert_eq!(v.len(), INTERFACE_GUID.len() + 2);
        assert_eq!(&v[v.len() - 2..], &[0, 0]);
        assert_eq!(parse_multi_sz(&v), [INTERFACE_GUID]);
        let two = multi_sz(&["{A}", "{B}"]);
        assert_eq!(String::from_utf16(&two).unwrap(), "{A}\0{B}\0\0");
        assert_eq!(multi_sz::<&str>(&[]), [0]);
    }

    #[test]
    fn interface_guid_is_well_formed() {
        let inner = INTERFACE_GUID.trim_start_matches('{').trim_end_matches('}');
        let g = GUID::try_from(inner).expect("valid GUID");
        assert_eq!(format_guid(&g), INTERFACE_GUID);
    }

    #[test]
    fn driver_version_unpacks() {
        let v = (10u64 << 48) | (26100u64 << 16) | 9444; // 10.0.26100.9444
        assert_eq!(format_driver_version(v), "10.0.26100.9444");
    }

    #[test]
    fn classify_drivers() {
        assert_eq!(DriverKind::classify("WINUSB", "USBDevice"), DriverKind::WinUsb);
        assert_eq!(DriverKind::classify("WinUSB", "Bluetooth"), DriverKind::WinUsb);
        assert_eq!(DriverKind::classify("BTHUSB", "Bluetooth"), DriverKind::Bluetooth);
        assert_eq!(DriverKind::classify("ibtusb", "Bluetooth"), DriverKind::Bluetooth);
        assert_eq!(DriverKind::classify("usbccgp", "USB"), DriverKind::Other);
        assert_eq!(DriverKind::classify("", ""), DriverKind::None);
    }

    #[test]
    fn record_matching() {
        let info = DriverInfo {
            service: "BTHUSB".into(),
            inf_name: "bth.inf".into(),
            version: "10.0.26100.1".into(),
            class_name: "Bluetooth".into(),
            ..Default::default()
        };
        let rec = DriverRecord::from_info(&info);
        assert!(rec.matches(&info));
        assert!(rec.matches(&DriverInfo { inf_name: "BTH.INF".into(), service: "bthusb".into(), ..info.clone() }));
        assert!(!rec.matches(&DriverInfo { version: "10.0.26100.2".into(), ..info.clone() }));
        assert!(!rec.matches(&DriverInfo { service: "WINUSB".into(), inf_name: "winusb.inf".into(), ..info.clone() }));
        // Windows Update replaced the inbox INF while the adapter was on WinUSB
        assert!(rec.is_back(&DriverInfo { version: "10.0.26100.2".into(), ..info.clone() }));
        assert!(!rec.is_back(&DriverInfo { service: "WINUSB".into(), inf_name: "winusb.inf".into(), ..info.clone() }));
        assert!(!rec.is_back(&DriverInfo { inf_name: "oem12.inf".into(), ..info }));
    }

    #[test]
    fn recorded_model_by_section_or_description() {
        let record = DriverRecord {
            inf_name: "usb.inf".into(),
            inf_section: "Composite.Dev.NT".into(),
            description: "USB Composite Device".into(),
            ..Default::default()
        };
        assert!(is_recorded_model(&record, "Composite.Dev", "anything"));
        assert!(is_recorded_model(&record, "composite.dev.nt", ""));
        assert!(!is_recorded_model(&record, "Composite.DevX", "USB Composite Device"));
        assert!(!is_recorded_model(&record, "Composite", "USB Composite Device"));
        assert!(!is_recorded_model(&record, "", "USB Composite Device"));
        // Records from before the section was kept
        let old = DriverRecord { inf_section: String::new(), ..record };
        assert!(is_recorded_model(&old, "Composite.Dev", "USB Composite Device"));
        assert!(!is_recorded_model(&old, "Composite.Dev", "USB Generic Parent"));
        assert!(!is_recorded_model(&DriverRecord::default(), "Composite.Dev", ""));
    }

    #[test]
    fn class_guids_parse() {
        assert_eq!(parse_guid("{88BAE032-5A81-49F0-BC3D-A4FF138216D6}"), Some(USB_DEVICE_CLASS));
        assert_eq!(parse_guid("88bae032-5a81-49f0-bc3d-a4ff138216d6"), Some(USB_DEVICE_CLASS));
        assert_eq!(parse_guid(""), None);
    }

    #[test]
    fn inf_names_only() {
        assert!(inf_path("usb.inf").is_ok());
        assert!(inf_path("").is_err());
        assert!(inf_path(r"..\system32\x.inf").is_err());
        assert!(inf_path("C:x.inf").is_err());
        assert!(inf_path("oem99999.inf").is_err());
    }

    #[test]
    fn only_usb_instance_ids_pass_the_guard() {
        assert!(require_usb(r"USB\VID_0BDA&PID_8771\00E04C239987").is_ok());
        assert!(require_usb(r"usb\vid_0bda&pid_8771\1").is_ok());
        assert!(matches!(require_usb(r"PCI\VEN_8086&DEV_A0F0\3"), Err(DriverError::NotUsb(_))));
        assert!(matches!(require_usb("USB\\"), Err(DriverError::NotUsb(_))));
    }

    #[test]
    fn winusb_inf_is_found() {
        let p = inf_path("winusb.inf").expect("winusb.inf ships with Windows 8 and later");
        let s = crate::autostart::utf16_string(&p);
        assert!(s.to_ascii_lowercase().ends_with(r"\inf\winusb.inf"), "{s}");
    }

    #[test]
    fn unknown_device_is_not_found() {
        assert!(matches!(driver_state(r"USB\VID_FFFF&PID_FFFF\BLUECASTER_TEST"), Err(DriverError::DeviceNotFound(_))));
    }

    /// Read-only: the driver state of every present USB device that looks
    /// like Bluetooth
    #[test]
    fn driver_state_of_present_devices() {
        for d in crate::usb_adapters::enumerate(false).unwrap().iter().filter(|d| d.looks_like_bluetooth()) {
            let s = driver_state(&d.instance_id).unwrap();
            assert!(s.present);
            assert_eq!(s.driver, d.driver);
            eprintln!(
                "{} -> {:?} {:?} guids={:?} bluecaster={}",
                s.instance_id,
                s.kind,
                s.driver,
                s.interface_guids,
                s.has_bluecaster_interface()
            );
        }
    }
}
