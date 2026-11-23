//! The service: owns the Bluetooth stack (started on demand, restarted when
//! it stops working or after sleep), runs streaming sessions on their own
//! thread and reports what happens as [`ServiceEvent`]s.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bt_host::{
    BtHost, DiscoveredDevice, HciCapture, HeadsetMic, HostConfig, HostEvent, InitFailure, LeConfig, LeDeviceId,
    LeScanResult, LinkStats, TransportConfig,
};
use hci_transport::UsbSelector;

use crate::devices::SavedDevice;
use crate::messages::Msg;
use crate::settings::Settings;
use crate::stream::{self, SourceRequest, StreamInfo, StreamRequest, StreamState};

/// Wait for the controller to answer when checking a stack before use
const CONTROLLER_PROBE: Duration = Duration::from_secs(2);
/// Time for a USB adapter to come back after resume before it is opened
const RESUME_SETTLE: Duration = Duration::from_secs(3);
/// Waiting for the resume after the adapter went away for sleep: ticks of
/// [`RESUME_TICK`], so about a minute while the PC stays awake (a wait across
/// sleep ends once on wake, whatever the clock says)
const RESUME_TICK: Duration = Duration::from_millis(200);
const RESUME_WAIT_TICKS: u32 = 300;
/// LE Audio (experimental): the shortest LE scan of a scan that is not
/// stopped (v1: 5 s after the classic one)
const LE_SCAN: Duration = Duration::from_secs(5);
/// A link younger than this keeps the stack ([`Service::host_for_stream`])
const FRESH_LINK: Duration = Duration::from_secs(60);
/// A link given up on is gone by then, or by its supervision timeout
pub(crate) const LINK_RELEASE_WAIT: Duration = Duration::from_secs(2);
/// A connection started during a scan waits this long for the scan to end
/// (a name request being cancelled takes up to ~2 s)
const SCAN_END_WAIT: Duration = Duration::from_secs(5);
/// ... once the LE core is ready (HCI works, so at once or a moment later)
const LE_SCAN_READY_WAIT: Duration = Duration::from_secs(1);

/// MMCSS "Pro Audio" for the calling thread (the priority hooks). Said in
/// the log either way: a thread that runs without it is one place a
/// stutter can come from (issue #84).
fn pro_audio(thread: &str) -> Box<dyn std::any::Any> {
    let guard = match win_sys::mmcss::MmcssGuard::register(win_sys::mmcss::MmcssTask::ProAudio) {
        Ok(g) => {
            tracing::info!("{thread}: MMCSS Pro Audio (task {})", g.task_index());
            Some(g)
        }
        Err(e) => {
            tracing::warn!("{thread}: MMCSS registration failed, normal priority: {e}");
            None
        }
    };
    Box::new(guard)
}

/// LE Audio (experimental) from the command line (`--le-audio` and the
/// test options): LE is on whatever the setting says
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeCliOptions {
    /// `--mcp-dry-run` (implied by `--hci-tcp`): MCP answers from a
    /// simulated player and only logs the commands
    pub mcp_dry_run: bool,
    /// `--ccp-sim "<t>:<cmd>;..."`
    pub ccp_sim: Option<String>,
    /// `--ccp-console`
    pub ccp_console: bool,
}

/// A device of a scan ([`Service::scan_devices`]): the classic ones, then
/// the LE Audio ones (experimental). A dual-mode device has two entries: its
/// classic entry is never an LE Audio one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScannedDevice {
    /// XX:XX:XX:XX:XX:XX
    pub address: String,
    pub name: String,
    pub rssi: i8,
    /// Classic: the class of device; 0 for LE Audio entries
    pub class_of_device: u32,
    /// Classic: Audio/Video class; LE Audio entries are audio devices
    pub audio: bool,
    /// An LE Audio entry
    pub le_audio: bool,
    /// LE Audio: member of a coordinated set (advertises a CSIP RSI);
    /// connecting it streams to the whole set
    pub le_audio_set: bool,
    /// Keyboards and mice (experimental, issue #413): an LE advertiser that
    /// looks like a keyboard or mouse, with its address type. Only listed
    /// while that feature is on; never an audio or LE Audio entry.
    pub hid_le: Option<u8>,
}

/// One device, not two (issue #86): a scan that finds the other side
/// (classic / LE Audio, the same address) of a saved device saves that side
/// too, under the saved name, so the device stays one entry that "Use LE
/// Audio when available" connects either way. The sides saved.
fn save_twins(dir: &std::path::Path, found: &[ScannedDevice]) -> usize {
    let saved = crate::devices::load_all(dir);
    let mut n = 0;
    // A keyboard or mouse is neither side of headphones
    for d in found.iter().filter(|d| d.hid_le.is_none()) {
        let has = |le: bool| saved.iter().find(|s| s.le_audio == le && s.address.eq_ignore_ascii_case(&d.address));
        if has(d.le_audio).is_some() {
            continue;
        }
        // The other side saved, and this side found by the scan
        let Some(other) = has(!d.le_audio) else { continue };
        let name = if other.name.is_empty() { d.name.as_str() } else { other.name.as_str() };
        let r = if d.le_audio {
            crate::devices::save_le(dir, &d.address, name)
        } else {
            crate::devices::save(dir, &d.address, name)
        };
        match r {
            Ok(()) => {
                n += 1;
                tracing::info!(
                    "scan: the {} side of a saved device saved with it",
                    if d.le_audio { "LE Audio" } else { "classic" }
                );
            }
            Err(e) => tracing::warn!("scan: saving the other side of a saved device: {e}"),
        }
    }
    n
}

impl ScannedDevice {
    fn classic(d: &DiscoveredDevice) -> ScannedDevice {
        ScannedDevice {
            address: bt_host::addr_str(&d.addr),
            name: d.name.clone(),
            rssi: d.rssi,
            class_of_device: d.class_of_device,
            audio: d.is_audio(),
            le_audio: false,
            le_audio_set: false,
            hid_le: None,
        }
    }
}

/// Appearance categories (the value's upper 10 bits)
const APPEARANCE_PHONE: u16 = 0x001;
const APPEARANCE_COMPUTER: u16 = 0x002;
/// Human Interface Device, 0x03C0-0x03FF (keyboard, mouse, ...)
const APPEARANCE_HID: u16 = 0x00F;
/// The HID service, advertised by LE keyboards and mice
const UUID_SERVICE_HID: u16 = 0x1812;

/// Keyboards and mice (experimental, issue #413): the connectable LE
/// advertisers that look like one (the HID service advertised, or an
/// appearance of the HID category), listed by their identity when known,
/// with its address type. Never audio entries.
fn hid_le_entries(results: &[LeScanResult]) -> Vec<ScannedDevice> {
    let mut out: Vec<ScannedDevice> = Vec::new();
    for r in results.iter().filter(|r| r.connectable) {
        if !r.uuids16.contains(&UUID_SERVICE_HID) && r.appearance >> 6 != APPEARANCE_HID {
            continue;
        }
        let a = r.identity.unwrap_or(r.addr);
        let address = a.to_string();
        match out.iter_mut().find(|e| e.address == address) {
            Some(e) => {
                if !r.name.is_empty() {
                    e.name = r.name.clone();
                }
            }
            None => out.push(ScannedDevice {
                address,
                name: r.name.clone(),
                rssi: r.rssi,
                class_of_device: 0,
                audio: false,
                le_audio: false,
                le_audio_set: false,
                hid_le: Some(a.addr_type),
            }),
        }
    }
    out
}

/// The addresses of advertisers listed as LE Audio only for their audio
/// appearance that are a classic device (`classic`: found or saved): a
/// classic headset's LE side for its app (a Sennheiser HDR 275 advertises
/// name, battery and the appearance "headset", no LE Audio service). Not
/// listed, and a saved LE side of them is removed: else "Use LE Audio when
/// available" connected it over LE, where nothing answers.
fn classic_appearance_only(results: &[LeScanResult], classic: &[String]) -> Vec<String> {
    results
        .iter()
        .filter(|r| r.le_audio && r.appearance_only)
        .map(|r| r.identity.unwrap_or(r.addr).to_string())
        .filter(|a| classic.iter().any(|c| c.eq_ignore_ascii_case(a)))
        .collect()
}

/// What [`le_scan_entries`] knows of an entry beyond what it lists
#[derive(Debug, Clone, Copy)]
struct EntryFacts {
    /// A saved LE Audio device (or a member of a saved one's set)
    saved: bool,
    /// The coordinated set a known SIRK resolved its RSI to
    set_id: Option<u32>,
    /// It advertises an RSI that no known set resolves (a set never bonded)
    unknown_set: bool,
    appearance: u16,
}

/// The LE Audio entries of an LE scan (v1 A2dpService scan thread): LE
/// Audio advertisers only; a saved LE Audio device is listed (with its new
/// name), and so is every member of its coordinated set, as that device
/// (`sets`: the sets learned); of a coordinated set nobody saved, one
/// member (connecting it finds the others). Classic entries of the same
/// address are another list. Returns the entries and the number of LE
/// Audio advertisers seen. A PC or phone is a source, not something to play
/// to: this PC's own Windows adapters (`own`, advertising LE Audio) are
/// left out with them.
fn le_scan_entries(
    results: &[LeScanResult],
    saved_le: &[SavedDevice],
    own: &[String],
    sets: &crate::le_sets::KnownSets,
) -> (Vec<ScannedDevice>, usize) {
    let mut out: Vec<(ScannedDevice, EntryFacts)> = Vec::new();
    let mut found = 0;
    for r in results.iter().filter(|r| r.le_audio) {
        let category = r.appearance >> 6;
        if matches!(category, APPEARANCE_PHONE | APPEARANCE_COMPUTER)
            || own.iter().any(|a| a.eq_ignore_ascii_case(&r.addr.to_string()))
        {
            continue;
        }
        found += 1;
        // A bonded device advertising under a random address is listed by
        // its identity: its saved entry and its classic side's name match
        // it, and its next random address is the same entry (issue #86)
        let own_address = r.identity.unwrap_or(r.addr).to_string();
        let set_member = r.rsi.is_some();
        // Another member of a saved device's set is that device
        let group = sets.group_of(&own_address).or_else(|| r.set_id.and_then(|id| sets.group_of_set(id)));
        let saved = saved_le.iter().find(|d| d.address.eq_ignore_ascii_case(&own_address)).or_else(|| {
            group.and_then(|g| saved_le.iter().find(|d| g.iter().any(|a| a.eq_ignore_ascii_case(&d.address))))
        });
        let address = saved.map_or(own_address, |d| d.address.clone());
        if let Some((e, facts)) = out.iter_mut().find(|(e, _)| e.address == address) {
            // Seen twice (advertising and scan response), or another member
            if !r.name.is_empty() {
                e.name = r.name.clone();
            }
            e.le_audio_set |= set_member;
            facts.set_id = facts.set_id.or(r.set_id);
            facts.unknown_set |= set_member && r.set_id.is_none();
            continue;
        }
        let facts = EntryFacts {
            saved: saved.is_some(),
            set_id: r.set_id,
            unknown_set: set_member && r.set_id.is_none(),
            appearance: r.appearance,
        };
        out.push((
            ScannedDevice {
                address,
                name: r.name.clone(),
                rssi: r.rssi,
                class_of_device: 0,
                audio: true,
                le_audio: true,
                le_audio_set: set_member,
                hid_le: None,
            },
            facts,
        ));
    }
    // Of a set a known SIRK resolves: its saved entry, else one member
    let mut keep: Vec<bool> = vec![true; out.len()];
    for i in 0..out.len() {
        let Some(set) = out[i].1.set_id else { continue };
        let same: Vec<usize> = (0..out.len()).filter(|&j| out[j].1.set_id == Some(set)).collect();
        let any_saved = same.iter().any(|&j| out[j].1.saved);
        let first = same[0];
        keep[i] = if any_saved { out[i].1.saved } else { i == first };
    }
    // A set never bonded, so nothing resolves it yet
    for (drop, kept) in unbonded_set_twins(&out) {
        keep[drop] = false;
        tracing::debug!("le scan: two advertisers of one name and appearance listed as one set ({kept} kept)");
    }
    let entries = out.into_iter().zip(keep).filter(|(_, k)| *k).map(|((e, _), _)| e).collect();
    (entries, found)
}

/// Advertisers of a coordinated set that no known SIRK resolves (the first
/// scan of new earbuds): both earbuds advertise an RSI under their own
/// address, and nothing tells the two apart from two devices until a
/// connection reads the set's key. Two such entries are taken as one set
/// when they are the only ones of their name (not empty, compared in full:
/// "Buds NC" and "Buds SO" are other products) and appearance; three or
/// more (two pairs of one model, or a pair and a lone earbud) cannot be
/// told apart and stay listed. Of the two, a saved one stays, else the one
/// with the lower address. Returns (index dropped, index kept).
fn unbonded_set_twins(entries: &[(ScannedDevice, EntryFacts)]) -> Vec<(usize, usize)> {
    let candidate =
        |(e, f): &(ScannedDevice, EntryFacts)| f.unknown_set && f.set_id.is_none() && !e.name.trim().is_empty();
    let mut out = Vec::new();
    for (i, a) in entries.iter().enumerate() {
        if !candidate(a) {
            continue;
        }
        let same: Vec<usize> = (0..entries.len())
            .filter(|&j| {
                let b = &entries[j];
                candidate(b) && b.0.name.trim() == a.0.name.trim() && b.1.appearance == a.1.appearance
            })
            .collect();
        // Each pair once, from its first entry
        if same.len() != 2 || same[0] != i {
            continue;
        }
        let (x, y) = (same[0], same[1]);
        let (ex, ey) = (&entries[x], &entries[y]);
        let pair = match (ex.1.saved, ey.1.saved) {
            (true, true) => continue,
            (true, false) => (y, x),
            (false, true) => (x, y),
            (false, false) if ex.0.address <= ey.0.address => (y, x),
            (false, false) => (x, y),
        };
        out.push(pair);
    }
    out
}

/// LE entries whose advertising carries no name (issue #86: an EarFun's LE
/// side listed by its address) take the name of a device with the same
/// address in `known` (address and name: the scan's classic devices and the
/// saved ones), the classic side of the same earbuds
fn fill_le_names(entries: &mut [ScannedDevice], known: &[(String, String)]) {
    for e in entries.iter_mut().filter(|e| e.name.trim().is_empty()) {
        if let Some((_, name)) = known.iter().find(|(a, n)| a.eq_ignore_ascii_case(&e.address) && !n.trim().is_empty())
        {
            e.name = name.clone();
        }
    }
}

/// The advertisers without LE Audio that look like the LE side of a classic
/// device (`known`: address and name of the scan's and the saved ones): the
/// same address, or the same name. For the log only: such a device is not
/// listed (many classic headphones advertise over LE for their app).
fn classic_le_sides<'a>(results: &'a [LeScanResult], known: &[(String, String)]) -> Vec<&'a LeScanResult> {
    results
        .iter()
        .filter(|r| !r.le_audio)
        .filter(|r| {
            let addr = r.addr.to_string();
            let ids = std::iter::once(addr).chain(r.identity.map(|i| i.to_string()));
            let same_addr = ids.into_iter().any(|a| known.iter().any(|(k, _)| k.eq_ignore_ascii_case(&a)));
            let same_name = !r.name.trim().is_empty() && known.iter().any(|(_, n)| n.trim() == r.name.trim());
            same_addr || same_name
        })
        .collect()
}

/// Sleep and resume as the service sees them (set by the power
/// notifications, read by the session)
#[derive(Debug, Default)]
pub(crate) struct PowerState {
    /// Between the going-to-sleep notification and the resume: the adapter
    /// goes away, so transport failures and a dropped link are expected and
    /// nothing is restarted until the PC is back
    suspending: AtomicBool,
    /// The PC resumed from sleep since the stack started: the adapter lost
    /// power or was re-enumerated, so the stack is restarted before the
    /// next use (v1 needed an app restart)
    stale: AtomicBool,
    resumed_at: Mutex<Option<Instant>>,
}

impl PowerState {
    /// The going-to-sleep notification; false when it was seen already
    pub fn suspend(&self) -> bool {
        !self.suspending.swap(true, Ordering::AcqRel)
    }

    /// A resume notification; true for the first of one resume (Windows
    /// sends two), which marks the stack stale
    pub fn resume(&self) -> bool {
        let was_suspending = self.suspending.swap(false, Ordering::AcqRel);
        let was_stale = self.stale.swap(true, Ordering::AcqRel);
        let first = was_suspending || !was_stale;
        if first {
            *self.resumed_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
        first
    }

    /// Going to sleep: the adapter is going away
    pub fn suspending(&self) -> bool {
        self.suspending.load(Ordering::Acquire)
    }

    /// The running stack is left from before sleep
    pub fn stale(&self) -> bool {
        self.stale.load(Ordering::Acquire)
    }

    /// A new stack starts
    fn clear_stale(&self) {
        self.stale.store(false, Ordering::Release);
    }

    /// No resume came after all: losses count as failures again
    fn end_suspend(&self) {
        self.suspending.store(false, Ordering::Release);
    }

    fn resumed_at(&self) -> Option<Instant> {
        *self.resumed_at.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServiceEvent {
    State(StreamState, Msg),
    StreamInfo(StreamInfo),
    /// The headphones reported their volume (0..127)
    Volume(u8),
    /// The headphones reported their battery level (percent)
    Battery(u8),
    /// The headphones refused to report their battery level (HFP refused,
    /// another device may hold it)
    BatteryRefused,
    /// The voice codecs the headphones listed on their HFP connection
    /// ("cvsd", "msbc", "lc3_swb"; StreamStatus.voice_codecs)
    VoiceCodecs(Vec<String>),
    /// LE Audio: the microphone's sampling rates the headphones allow (Hz),
    /// Auto's first (StreamStatus.mic_rates); empty without a microphone
    MicRates(Vec<u32>),
    /// While the session waited for its headphones that went off, another
    /// saved device connected to the PC (its address): its session may
    /// start instead
    OtherDeviceBack([u8; 6]),
    /// A firmware loader's step, percent or end while the adapter starts
    Loader(bt_host::bringup::LoaderProgress),
    /// Headset mode: the voice link carries audio (Some), or no longer
    /// (None)
    Voice(Option<ipc::VoiceStatus>),
    /// LE Audio: a connection learned a coordinated set
    /// (le_audio_sets.txt): saved entries of one set become one
    LeSetLearned,
    /// The USB adapter (VID, PID) a stack started on, or None: the stack
    /// stopped (also a stack over TCP, which has no adapter)
    Adapter(Option<(u16, u16)>),
    /// A headphone button arrived (for the IPC clients; the media key, if
    /// any, went to Windows already)
    Button(ipc::ButtonPress),
    /// Keyboards and mice (experimental): one was added, removed, connected
    /// or went away ([`Service::hid_devices`])
    HidDevices,
    /// The number to type on a keyboard being added appeared or went away
    /// ([`Service::hid_passkey`])
    HidPasskey,
    /// A device being added started or ended its countdown, or one was
    /// removed again ([`Service::hid_checks`], [`Service::hid_rolled_back`])
    HidCheck,
    /// The self-test of the path to Windows has a new result
    /// ([`Service::hid_health`])
    HidHealth,
}

/// Headset mode switched for the running classic stream
/// ([`Service::set_headset_mic`])
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChange {
    pub mode: HeadsetMic,
    /// As [`StreamRequest::mic_output`]
    pub mic_output: Option<String>,
    /// As [`StreamRequest::voice_codec`]
    pub voice_codec: Option<voice_link::VoiceCodec>,
}

pub struct ServiceOptions {
    pub config_dir: PathBuf,
    /// H4 over TCP to a virtual controller instead of USB (tools/emu)
    pub hci_tcp: Option<String>,
    pub capture: HciCapture,
    /// AFH policy instead of the settings' (command line)
    pub afh: Option<String>,
    /// The USB adapter to open (the command line's -u); default: the
    /// first one that works. Not used with hci_tcp.
    pub usb: UsbSelector,
}

/// A host's button as the IPC names it
fn ipc_button(b: bt_host::Button) -> ipc::MediaButton {
    use bt_host::Button as B;
    use ipc::MediaButton as M;
    match b {
        B::Play => M::Play,
        B::Pause => M::Pause,
        B::Stop => M::Stop,
        B::Next => M::Next,
        B::Previous => M::Previous,
        B::FastForward => M::FastForward,
        B::Rewind => M::Rewind,
        B::VolumeUp => M::VolumeUp,
        B::VolumeDown => M::VolumeDown,
        B::Mute => M::Mute,
    }
}

/// The LE Audio configuration of the next stack, None: LE off
fn le_config(le_on: bool, cli: Option<&LeCliOptions>, opts: &ServiceOptions) -> Option<LeConfig> {
    if !le_on {
        return None;
    }
    // --mcp-dry-run, implied by --hci-tcp (the emulator): the simulated
    // player answers, nothing in Windows is read or controlled
    let dry_run = cli.is_some_and(|c| c.mcp_dry_run) || opts.hci_tcp.is_some();
    Some(LeConfig {
        bonds_file: Some(crate::paths::le_bonds_file(&opts.config_dir)),
        imported_bonds_file: Some(crate::paths::windows_le_keys_file(&opts.config_dir)),
        bond_dir: Some(crate::paths::pairing_dir(&opts.config_dir)),
        // Windows' media session, here or in the session agent (a dry run
        // with the agent: its simulated player)
        media: session_host::media::media_backend(dry_run),
        ccp_sim: cli.and_then(|c| c.ccp_sim.clone()),
        ccp_console: cli.is_some_and(|c| c.ccp_console),
    })
}

/// The lock behind [`Service::settings_tx`]: one thread at a time, which
/// may enter again
#[derive(Default)]
pub(crate) struct SettingsTx {
    owner: Mutex<Option<(std::thread::ThreadId, usize)>>,
    free: Condvar,
}

impl SettingsTx {
    pub(crate) fn enter(&self) -> SettingsTxGuard<'_> {
        let me = std::thread::current().id();
        let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match owner.as_mut() {
                None => {
                    *owner = Some((me, 1));
                    break;
                }
                Some((id, depth)) if *id == me => {
                    *depth += 1;
                    break;
                }
                Some(_) => owner = self.free.wait(owner).unwrap_or_else(|e| e.into_inner()),
            }
        }
        SettingsTxGuard(self)
    }
}

/// See [`Service::settings_tx`]
pub struct SettingsTxGuard<'a>(&'a SettingsTx);

impl Drop for SettingsTxGuard<'_> {
    fn drop(&mut self) {
        let mut owner = self.0.owner.lock().unwrap_or_else(|e| e.into_inner());
        match owner.as_mut() {
            Some((_, depth)) if *depth > 1 => *depth -= 1,
            _ => {
                *owner = None;
                self.0.free.notify_one();
            }
        }
    }
}

/// A broadcast session's claim on the Auracast adapter
/// ([`Inner::broadcast_wanted`]): dropped when the session ends, so the
/// next scan or connection goes back on the adapter to use
pub(crate) struct BroadcastGuard(Arc<Inner>);

impl Drop for BroadcastGuard {
    fn drop(&mut self) {
        self.0.broadcast.store(false, Ordering::Release);
        self.0.broadcast_adapter.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.0.leave_broadcast_adapter();
    }
}

pub(crate) struct Inner {
    opts: ServiceOptions,
    pub settings: Mutex<Settings>,
    /// One change of the saved settings at a time ([`Service::settings_tx`])
    settings_tx: SettingsTx,
    events: Mutex<Sender<ServiceEvent>>,
    pub(crate) host: Mutex<Option<Arc<BtHost>>>,
    stats: Arc<LinkStats>,
    stop: AtomicBool,
    running: AtomicBool,
    pub power: PowerState,
    tick: (Mutex<()>, Condvar),
    state: Mutex<StreamState>,
    /// What each device (address) offered at its last connection, also
    /// in device_codecs.json
    codecs: Mutex<BTreeMap<String, stream::KnownCodecs>>,
    /// The chip or firmware setting changed while the stack was in use:
    /// the next use starts a new stack (which loads the firmware again)
    reload: AtomicBool,
    /// LE Audio forced on by the command line
    le_cli: Mutex<Option<LeCliOptions>>,
    /// The developer CLI's --headset-mic-toggle: stacks offer HFP audio
    /// from the start, for headset mode switched on later
    voice_offered: AtomicBool,
    /// The session asks for headset mode (its request's `headset_mic` is
    /// On or Auto): stacks offer HFP audio
    voice_wanted: AtomicBool,
    /// Headset mode switched for the running session's next tick
    voice_change: Mutex<Option<VoiceChange>>,
    /// The HFP audio connection's events, for the session
    voice_events: Mutex<Vec<bt_host::VoiceEvent>>,
    /// The microphone test (Diagnostics)
    pub mic_test: crate::mic_test::MicTest,
    /// Keyboards and mice (experimental, crate::hid)
    pub(crate) hid: crate::hid::HidState,
    /// The microphone's processing in force (the request's, changed in
    /// place by [`Service::set_mic_processing`])
    pub mic_dsp: voice_link::MicDsp,
    /// The running stack was started with LE Audio
    le_active: AtomicBool,
    /// A broadcast session wants the stack (le_stream::run_broadcast): it
    /// opens the Auracast adapter (adapter::adapter_for)
    broadcast: AtomicBool,
    /// The adapter the running stack was started for (adapter::adapter_for;
    /// empty: automatic)
    host_adapter: Mutex<String>,
    /// The adapter the running broadcast's stack opened (its instance ID):
    /// a broadcast set up again goes on the same one, also when none is
    /// chosen; empty: no broadcast, or none opened yet
    broadcast_adapter: Mutex<String>,
    /// A device scan runs
    scanning: AtomicBool,
    /// The LE Audio stream being sent (le_stream)
    le_stream: Mutex<Option<crate::le_stream::LeStreamDetails>>,
    /// An A2DP stream ran on the running stack (issue #84)
    streamed: AtomicBool,
    /// The running adapter has LE Audio's isochronous channels: known once
    /// a stack started on it (issue #86)
    le_support: Mutex<Option<bool>>,
    /// The session waits for headphones that were turned off: a scan may
    /// end it
    waiting: AtomicBool,
    /// The headphones of the last session were turned off and did not come
    /// back while it ran: a Bluetooth restart does not reconnect them
    /// (issues #86, #117)
    turned_off: AtomicBool,
    /// The source the running A2DP session changes to at its next tick
    source_change: Mutex<Option<SourceRequest>>,
    /// The source a broadcast went on with after the one it was changed to
    /// could not be captured: the request is put back to it
    /// ([`Service::change_source`], [`Service::restart_adapter`])
    source_kept: Mutex<Option<SourceRequest>>,
    /// Other bitpools for the session to take in place
    /// ([`Service::change_bitpools`])
    bitpool_change: Mutex<Option<stream::Bitpools>>,
    /// The running A2DP stream would be set up the same with "Show only
    /// options it supports" off / on ([`Service::change_unlisted`]); None
    /// while none is set up
    unlisted_keeps: Mutex<Option<[bool; 2]>>,
}

impl Inner {
    /// The microphone's processing in force from the next samples on
    pub(crate) fn apply_mic_processing(&self, p: &ipc::MicProcessing) {
        let s = crate::voice::mic_settings(p);
        if s != self.mic_dsp.get() {
            tracing::info!(
                "microphone processing: {}, gain {} dB, noise suppression {}, gate {}",
                if p.enabled { "on" } else { "off" },
                p.gain_db,
                if s.suppresses() { format!("{} %", p.suppression.min(100)) } else { "off".into() },
                if p.gate_db > ipc::MicProcessing::GATE_OFF_DB { format!("{} dB", p.gate_db) } else { "off".into() }
            );
        }
        self.mic_dsp.set(s);
    }

    /// An event for the state machine (the LE stream's headset volume)
    /// No stack runs, nothing is about to start one, and the PC is not
    /// going to sleep: crate::hid may start one for its keyboards and mice
    pub(crate) fn hid_may_start(&self) -> bool {
        self.host.lock().unwrap_or_else(|e| e.into_inner()).is_none()
            && !self.running.load(Ordering::Acquire)
            && !self.power.suspending()
    }

    pub(crate) fn send_event(&self, e: ServiceEvent) {
        let _ = self.events.lock().unwrap_or_else(|e| e.into_inner()).send(e);
    }

    /// A sender of service events, for threads of their own
    pub(crate) fn event_sender(&self) -> Sender<ServiceEvent> {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub fn take_source_change(&self) -> Option<SourceRequest> {
        self.source_change.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// The session could not capture the source it was changed to and plays
    /// `source` again
    pub fn keep_source(&self, source: SourceRequest) {
        *self.source_kept.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    pub fn take_bitpool_change(&self) -> Option<stream::Bitpools> {
        self.bitpool_change.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn take_voice_change(&self) -> Option<VoiceChange> {
        self.voice_change.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// The HFP audio connection's events since the last call, oldest first
    pub fn take_voice_events(&self) -> Vec<bt_host::VoiceEvent> {
        std::mem::take(&mut *self.voice_events.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Headset mode's voice link carries audio (Some) or no longer
    pub fn voice_status(&self, status: Option<ipc::VoiceStatus>) {
        let _ = self.events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::Voice(status));
    }

    /// LE Audio: the microphone rates the headphones allow, Auto's first
    /// (StreamStatus.mic_rates)
    pub fn mic_rates(&self, rates: Vec<u32>) {
        let _ = self.events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::MicRates(rates));
    }

    pub fn set_unlisted_keeps(&self, keeps: [bool; 2]) {
        *self.unlisted_keeps.lock().unwrap_or_else(|e| e.into_inner()) = Some(keeps);
    }

    /// The session waits for headphones that were turned off
    pub fn waiting_for_device(&self) {
        self.turned_off.store(true, Ordering::Release);
        self.waiting.store(true, Ordering::Release);
    }

    /// The wait ended: `back`, the headphones connected again
    pub fn done_waiting_for_device(&self, back: bool) {
        if back {
            self.turned_off.store(false, Ordering::Release);
        }
        self.waiting.store(false, Ordering::Release);
    }

    pub fn notify(&self, state: StreamState, msg: Msg) {
        tracing::info!("state {state:?}: {}", msg.english());
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        // What went over the air before a failure or a lost connection; not
        // for a refusal by a setting, which sent nothing (a tester's clicks
        // on a refused device saved 7 and pushed the useful ones out)
        let refusal = matches!(msg.key, "error.le_audio_disabled" | "error.le_audio_developer");
        if matches!(state, StreamState::Error | StreamState::Reconnecting) && !refusal {
            let dir = crate::paths::recorder_dir(&self.opts.config_dir);
            let what = format!("{state:?}-{}", msg.key.trim_start_matches("error."));
            self.opts.capture.save_recent(&dir, &crate::paths::local_stamp(), &what, false);
        }
        let _ = self.events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::State(state, msg));
    }

    /// The settings folder (devices, profiles, pairings)
    pub fn config_dir(&self) -> &std::path::Path {
        &self.opts.config_dir
    }

    pub fn other_device_back(&self, addr: [u8; 6]) {
        let _ = self.events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::OtherDeviceBack(addr));
    }

    pub fn stream_info(&self, info: StreamInfo) {
        let _ = self.events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::StreamInfo(info));
    }

    /// Sleeps up to `d`, returning early on a stop
    /// Ends a session's [`Inner::wait_tick`] now
    pub(crate) fn wake_tick(&self) {
        self.tick.1.notify_all();
    }

    pub fn wait_tick(&self, d: Duration) {
        let guard = self.tick.0.lock().unwrap_or_else(|e| e.into_inner());
        if !self.stop_requested() {
            let _ = self.tick.1.wait_timeout(guard, d);
        }
    }

    fn wait_after_resume(&self) {
        let Some(at) = self.power.resumed_at() else { return };
        let since = at.elapsed();
        if since >= RESUME_SETTLE {
            return;
        }
        tracing::info!("waiting {} ms for the adapter after resume", (RESUME_SETTLE - since).as_millis());
        while at.elapsed() < RESUME_SETTLE && !self.stop_requested() {
            std::thread::sleep(Duration::from_millis(100));
        }
        tracing::debug!(
            "resume settle: done {} ms after the resume (stop requested {})",
            at.elapsed().as_millis(),
            self.stop_requested()
        );
    }

    /// LE Audio for the next stack: the setting in developer mode
    /// ([`Settings::le_audio`]), or the command line's --le-audio
    pub fn le_wanted(&self) -> bool {
        self.le_cli.lock().unwrap_or_else(|e| e.into_inner()).is_some()
            || self.settings.lock().unwrap_or_else(|e| e.into_inner()).le_audio()
    }

    /// Developer mode is on
    pub fn developer(&self) -> bool {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).debug_mode
    }

    /// The running stack has LE Audio
    pub fn le_active(&self) -> bool {
        self.le_active.load(Ordering::Acquire)
    }

    /// The adapter the next stack opens first: the Auracast adapter while a
    /// broadcast wants the stack, else the adapter to use
    fn wanted_adapter(&self) -> String {
        let s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        crate::adapter::adapter_for(&s, self.broadcast.load(Ordering::Acquire)).to_string()
    }

    /// The adapter a broadcast set up again goes on: the chosen one, else
    /// the one its stack opened before (empty: none opened yet)
    fn broadcast_adapter(&self) -> String {
        let wanted = self.wanted_adapter();
        if wanted.is_empty() {
            self.broadcast_adapter.lock().unwrap_or_else(|e| e.into_inner()).clone()
        } else {
            wanted
        }
    }

    /// The adapter the broadcast's next stack opens first is there (always,
    /// when none is chosen and none was opened yet, or with a virtual
    /// controller)
    pub(crate) fn wanted_adapter_present(&self) -> bool {
        self.opts.hci_tcp.is_some() || crate::adapter::chosen_present(&self.broadcast_adapter())
    }

    /// The Auracast adapter is set and not there: a broadcast would go on
    /// the first other adapter found
    pub(crate) fn auracast_adapter_missing(&self) -> bool {
        if self.opts.hci_tcp.is_some() {
            return false;
        }
        let id = {
            let s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            if s.debug_mode { s.auracast_adapter_id.trim().to_string() } else { String::new() }
        };
        !id.is_empty() && !crate::adapter::chosen_present(&id)
    }

    /// The broadcast's stack is up: the adapter it opened is the one a
    /// set-up after a loss waits for and opens again
    pub(crate) fn note_broadcast_adapter(&self) {
        if self.opts.hci_tcp.is_none()
            && let Some(id) = hci_transport::usb::last_opened_instance()
        {
            *self.broadcast_adapter.lock().unwrap_or_else(|e| e.into_inner()) = id;
        }
    }

    /// A broadcast session wants the stack, until the guard drops: the
    /// next stack started or checked (ensure_host) is the Auracast
    /// adapter's
    pub(crate) fn broadcast_wanted(self: &Arc<Self>) -> BroadcastGuard {
        self.broadcast_adapter.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.broadcast.store(true, Ordering::Release);
        BroadcastGuard(self.clone())
    }

    /// The broadcast ended on an adapter of its own: that stack is shut
    /// down, so the Auracast adapter is free and not shown as in use; the
    /// next scan or connection starts one on the adapter to use
    fn leave_broadcast_adapter(&self) {
        let mut slot = self.host.lock().unwrap_or_else(|e| e.into_inner());
        let same =
            self.host_adapter.lock().unwrap_or_else(|e| e.into_inner()).eq_ignore_ascii_case(&self.wanted_adapter());
        if same {
            return;
        }
        if let Some(h) = slot.take() {
            tracing::info!("Auracast: the broadcast ended, leaving the Auracast adapter");
            h.shutdown();
            self.adapter_changed(false);
        }
    }

    fn host_config(&self, le_on: bool) -> HostConfig {
        let mut s = self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // The Auracast adapter for a broadcast: configure opens it first
        s.adapter_id = self.wanted_adapter();
        *self.host_adapter.lock().unwrap_or_else(|e| e.into_inner()) = s.adapter_id.clone();
        // A broadcast set up again with no adapter chosen: the one it ran on
        // before, not the first one found (host_adapter stays "automatic")
        if self.broadcast.load(Ordering::Acquire) {
            s.adapter_id = self.broadcast_adapter();
        }
        let transport = match &self.opts.hci_tcp {
            Some(addr) => TransportConfig::Tcp(addr.clone()),
            // bt-host adds the adapters it knows to `known`
            None => TransportConfig::Usb(self.opts.usb.clone()),
        };
        let mut c = HostConfig::new(transport);
        c.link_key_file = Some(crate::paths::link_keys_file(&self.opts.config_dir));
        c.firmware_dir = Some(crate::paths::firmware_dir(&self.opts.config_dir));
        c.capture = self.opts.capture.clone();
        crate::adapter::configure(&mut c, &s, &self.opts.config_dir);
        let cli = self.le_cli.lock().unwrap_or_else(|e| e.into_inner()).clone();
        c.le_audio = le_config(le_on, cli.as_ref(), &self.opts);
        // The LE keys of classic devices do not depend on LE Audio (issue
        // #86: Windows and Linux get them of headphones without it)
        c.le_keys = Some(bt_host::LeKeysConfig {
            bonds_file: crate::paths::le_bonds_file(&self.opts.config_dir),
            imported_bonds_file: Some(crate::paths::windows_le_keys_file(&self.opts.config_dir)),
        });
        c.battery = true;
        // Keyboards and mice (experimental, developer mode; issue #413)
        c.hid = self.hid_config();
        // Headset microphone: the session's request, or the developer
        // CLI's --headset-mic-toggle
        c.voice = self.voice_wanted.load(Ordering::Acquire)
            || self.voice_offered.load(Ordering::Acquire)
            || self.mic_test.needs_voice();
        // The computer's name, as Windows' own Bluetooth (issue #86)
        c.local_name = bt_host::local_name(&win_sys::computer::computer_name().unwrap_or_default());
        // The Adapters screen shows a loader's progress while the adapter
        // starts (the stack's handle comes only after)
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let events = Mutex::new(events);
        c.on_loader = Some(Arc::new(move |p| {
            let _ = events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::Loader(p.clone()));
        }));
        tracing::debug!(
            "host config: transport {:?}, firmware dir {:?}, realtek pid {:04x} files {:?}, intel loader {}, mediatek loader {}, qualcomm loader {}, atheros loader {}, realtek driver chip {:04X?}, broadcom ids {}, LE {} (cli {:?})",
            c.transport,
            c.firmware_dir,
            c.realtek_pid,
            c.realtek_files,
            c.intel_loader,
            c.mediatek_loader,
            c.qualcomm_loader,
            c.atheros_loader,
            c.realtek_driver_chip,
            c.broadcom_ids.len(),
            c.le_audio.is_some(),
            cli
        );
        c
    }

    fn start_host(self: &Arc<Self>) -> Result<Arc<BtHost>, Msg> {
        // A controller that did not answer at all is opened once more right
        // away: opening it resets its USB pipes, which can be enough
        let attempts = 2;
        let mut last = InitFailure::Failed;
        let le_on = self.le_wanted();
        if le_on {
            tracing::info!("LE Audio is on: BTstack starts with LE");
        }
        for attempt in 1..=attempts {
            if attempt > 1 {
                tracing::warn!("the Bluetooth adapter did not answer, opening it again ({attempt}/{attempts})");
            }
            let t = Instant::now();
            let started = BtHost::start(self.host_config(le_on), self.stats.clone());
            tracing::debug!(
                "host start: attempt {attempt}/{attempts} {} after {} ms",
                if started.is_ok() { "ok" } else { "failed" },
                t.elapsed().as_millis()
            );
            match started {
                Ok(h) => {
                    let h = Arc::new(h);
                    let s = self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    h.set_media_keys(s.media_keys);
                    h.set_afh_policy(self.opts.afh.as_deref().unwrap_or(afh_policy(&s)));
                    self.le_active.store(le_on, Ordering::Release);
                    self.hid.stack_started();
                    self.streamed.store(false, Ordering::Release);
                    self.note_le_capable(&h);
                    self.pump_events(&h);
                    self.adapter_changed(true);
                    return Ok(h);
                }
                Err(f) => {
                    tracing::debug!("host start: {f:?}");
                    last = f;
                    if f != InitFailure::NoAnswer {
                        break;
                    }
                }
            }
        }
        // An adapter a firmware loader left needing a power cycle: the same
        // advice as for one that does not answer (unplug it, or shut down)
        let needs_replug = matches!(last, InitFailure::NoAnswer | InitFailure::NeedsReplug);
        Err(Msg::new(if needs_replug { "error.btstack_no_answer" } else { "error.btstack_init" }))
    }

    /// The state's adapter in use: the one the stack opened (`running`), or
    /// none
    fn adapter_changed(&self, running: bool) {
        let ids = if running && self.opts.hci_tcp.is_none() { hci_transport::usb::last_opened() } else { None };
        self.send_event(ServiceEvent::Adapter(ids));
    }

    /// Remembers whether the adapter the stack opened has LE Audio's
    /// isochronous channels, also for the next start (adapter_caps.json)
    fn note_le_capable(&self, host: &Arc<BtHost>) {
        let capable = host.le_audio_capable();
        tracing::debug!("adapter: LE Audio capable {capable}, opened {:?}", hci_transport::usb::last_opened());
        *self.le_support.lock().unwrap_or_else(|e| e.into_inner()) = Some(capable);
        if self.opts.hci_tcp.is_none()
            && let Some((vid, pid)) = hci_transport::usb::last_opened()
        {
            crate::adapter::save_le_audio(&self.opts.config_dir, vid, pid, capable);
            // The address comes with the controller's answer, a moment after
            // init; a Realtek adapter's version once its download was read
            // back (issue #427: the first read is the ROM code's)
            let dir = self.opts.config_dir.clone();
            let host = host.clone();
            let _ = std::thread::Builder::new().name("adapter-address".into()).spawn(move || {
                let mut address_saved = false;
                for _ in 0..40 {
                    if !address_saved && let Some(a) = host.local_address() {
                        crate::adapter::save_address(&dir, vid, pid, &a);
                        address_saved = true;
                    }
                    if address_saved && let Some(v) = host.hci_version() {
                        crate::adapter::save_hci_version(&dir, vid, pid, v);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                tracing::debug!("adapter: address saved {address_saved}, no HCI version after 10 s, not saved");
            });
        }
    }

    /// Headphone buttons to Windows media keys, volume reports to the
    /// service's events
    fn pump_events(self: &Arc<Self>, host: &Arc<BtHost>) {
        let Some(rx) = host.take_events() else { return };
        let weak: Weak<Inner> = Arc::downgrade(self);
        // The emulator's buttons press no key on this PC
        let keys_dry_run = self.opts.hci_tcp.is_some();
        let _ = std::thread::Builder::new().name("host-events".into()).spawn(move || {
            while let Ok(e) = rx.recv() {
                match e {
                    HostEvent::MediaKey(k) => {
                        tracing::debug!("host event: media key {k:?}");
                        let key = match k {
                            bt_host::MediaKey::PlayPause => session_host::media::Key::PlayPause,
                            bt_host::MediaKey::Stop => session_host::media::Key::Stop,
                            bt_host::MediaKey::Next => session_host::media::Key::Next,
                            bt_host::MediaKey::Previous => session_host::media::Key::Previous,
                        };
                        // In the user's session; never waits for the agent
                        session_host::media::send_media_key(key, keys_dry_run);
                    }
                    HostEvent::Button { button, pressed, le_audio } => {
                        tracing::debug!(
                            "host event: button {button:?} {}",
                            if pressed { "pressed" } else { "released" }
                        );
                        if let Some(inner) = weak.upgrade() {
                            inner.send_event(ServiceEvent::Button(ipc::ButtonPress {
                                button: ipc_button(button),
                                pressed,
                                le_audio,
                            }));
                        }
                    }
                    HostEvent::Volume(v) => {
                        tracing::debug!("host event: device volume {v}");
                        if let Some(inner) = weak.upgrade() {
                            let _ =
                                inner.events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::Volume(v));
                        }
                    }
                    HostEvent::Battery(v) => {
                        tracing::debug!("host event: battery {v}%");
                        if let Some(inner) = weak.upgrade() {
                            let _ =
                                inner.events.lock().unwrap_or_else(|e| e.into_inner()).send(ServiceEvent::Battery(v));
                        }
                    }
                    HostEvent::BatteryRefused => {
                        tracing::debug!("host event: battery refused");
                        if let Some(inner) = weak.upgrade() {
                            let _ = inner
                                .events
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .send(ServiceEvent::BatteryRefused);
                        }
                    }
                    HostEvent::VoiceCodecs(codecs) => {
                        tracing::debug!("host event: voice codecs {codecs:?}");
                        if let Some(inner) = weak.upgrade() {
                            let codecs = codecs.into_iter().map(String::from).collect();
                            let _ = inner
                                .events
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .send(ServiceEvent::VoiceCodecs(codecs));
                        }
                    }
                    // A keyboard or mouse (experimental): no address in the log
                    HostEvent::Hid(e) => {
                        if let Some(inner) = weak.upgrade() {
                            inner.on_hid_event(e);
                        }
                    }
                    HostEvent::ConnectionLost | HostEvent::TransportFailed => {
                        tracing::debug!("host event: {e:?}");
                    }
                    // The headset microphone: the session acts on it
                    HostEvent::Voice(v) => {
                        tracing::debug!("host event: voice {v:?}");
                        if let Some(inner) = weak.upgrade() {
                            inner.voice_events.lock().unwrap_or_else(|e| e.into_inner()).push(v);
                            inner.tick.1.notify_all();
                        }
                    }
                }
            }
            tracing::debug!("host events: channel closed");
        });
    }

    /// The running stack, started (or restarted) as needed
    pub fn ensure_host(self: &Arc<Self>) -> Result<Arc<BtHost>, Msg> {
        let mut slot = self.host.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = slot.as_ref() {
            // A stack that stopped working stays so until it is started anew
            // (v1 issues #42, #43: only an app restart helped)
            let le_wanted = self.le_wanted();
            let why = if self.power.stale() {
                Some(stream::RESUMED)
            } else if self.reload.load(Ordering::Acquire) {
                Some("the adapter settings changed")
            } else if self.le_active() != le_wanted {
                // Turned on or off while streaming or scanning: applied now,
                // before the next scan or connection (v1)
                Some(if le_wanted { "LE Audio was turned on" } else { "LE Audio was turned off" })
            } else if !self
                .host_adapter
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .eq_ignore_ascii_case(&self.wanted_adapter())
            {
                // A broadcast on the Auracast adapter, or back from it
                Some(if self.broadcast.load(Ordering::Acquire) {
                    "the broadcast goes on the Auracast adapter"
                } else {
                    "back on the adapter to use after a broadcast"
                })
            } else if (self.voice_wanted.load(Ordering::Acquire) || self.mic_test.needs_voice()) && !h.voice_enabled() {
                Some("headset mode needs the stack with voice")
            } else if self.hid_wanted() != h.hid_enabled() {
                Some(if h.hid_enabled() {
                    "keyboards and mice were turned off"
                } else {
                    "keyboards and mice were turned on"
                })
            } else if !h.controller_responds(CONTROLLER_PROBE) {
                Some("the Bluetooth adapter stopped answering")
            } else {
                None
            };
            match why {
                None => {
                    tracing::trace!("ensure host: the running stack is fine");
                    return Ok(h.clone());
                }
                Some(why) => {
                    tracing::warn!("restarting BTstack: {why}");
                    h.shutdown();
                    *slot = None;
                    self.hid_stack_stopped();
                    self.adapter_changed(false);
                }
            }
        }
        tracing::debug!("ensure host: starting a new stack");
        self.power.clear_stale();
        self.reload.store(false, Ordering::Release);
        self.wait_after_resume();
        let t = Instant::now();
        let h = self.start_host()?;
        tracing::debug!("ensure host: stack up in {} ms", t.elapsed().as_millis());
        *slot = Some(h.clone());
        Ok(h)
    }

    /// The stack for a new A2DP stream. One a stream already ran on is
    /// replaced: streams after a Disconnect / Connect on the same stack
    /// stuttered until the app was restarted (issue #84), and a new stack
    /// (HCI Reset; a running Realtek firmware is kept) is what a restart
    /// gave. While idle the old stack stays up, for headphones that connect
    /// by themselves. Kept when the device to stream connected to the PC by
    /// itself on it: the HCI Reset would drop that link, and headphones that
    /// just connected then refused the PC's page for 30 s to 2 min (issue
    /// #86). So is a link that came up moments ago ([`FRESH_LINK`]): a codec
    /// chosen right after connecting. An older link left from the stream
    /// that just ended does not count. Also kept while a keyboard or mouse
    /// is connected through BlueCaster: losing input for the reconnection
    /// is worse than the stutter.
    pub fn host_for_stream(self: &Arc<Self>, device: [u8; 6]) -> Result<Arc<BtHost>, Msg> {
        let old = self.host.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let streamed = self.streamed.swap(false, Ordering::AcqRel);
        if let Some(h) = old {
            // Claimed by this stream either way: the next one replaces the stack
            let own_link = h.take_link_from_device(device);
            if streamed && own_link {
                tracing::info!("keeping BTstack: {} connected to the PC by itself on it", bt_host::addr_str(&device));
            } else if streamed && link_is_fresh(h.link_age(device)) {
                tracing::info!("keeping BTstack: the link to {} just came up", bt_host::addr_str(&device));
            } else if streamed && self.hid_links_up() {
                // Untested on hardware: the stutter of issue #84 may come
                // back on this kept stack (docs/hid-host-dev.md)
                tracing::info!("keeping BTstack: a keyboard or mouse is connected");
            } else if streamed {
                tracing::info!("restarting BTstack: a stream ran on it before");
                self.shutdown_host();
            }
        }
        self.ensure_host()
    }

    /// The running stack carried an A2DP stream to `device`: the next one
    /// starts on a new stack, also when the device opened its link again
    /// during the stream (turned off and on)
    pub fn mark_streamed(&self, device: [u8; 6]) {
        if let Some(h) = self.host.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            h.take_link_from_device(device);
        }
        self.streamed.store(true, Ordering::Release);
    }

    /// Shuts the stack down and starts a new one (the session thread, for a
    /// stack that stopped working or is left from before sleep)
    pub fn restart_host(self: &Arc<Self>, why: &str, max_media_payload: u16) -> Result<Arc<BtHost>, Msg> {
        if why == stream::RESUMED {
            tracing::info!("restarting BTstack: {why}");
        } else {
            tracing::warn!("restarting BTstack: {why}");
        }
        self.shutdown_host();
        let h = self.ensure_host()?;
        tracing::debug!("restart host: media payload limit {max_media_payload}");
        h.set_media_payload_limit(max_media_payload);
        Ok(h)
    }

    /// The PC is going to sleep and the stack lost (or is losing) its
    /// adapter: shut down without the noise of a failure; after the resume
    /// a new one starts
    pub fn shutdown_host_for_sleep(&self) {
        let old = self.host.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = old {
            tracing::info!("stopping BTstack until the PC resumes");
            h.shutdown_lost();
            self.hid_stack_stopped();
            self.adapter_changed(false);
        }
    }

    /// Waits for the resume notification while the PC goes to sleep. Gives
    /// up after about a minute awake without one (the stack is then
    /// restarted as after a failure). False when the session was stopped.
    pub fn wait_for_resume(&self) -> bool {
        tracing::debug!("waiting for the resume (up to {RESUME_WAIT_TICKS} ticks awake)");
        let mut ticks = 0;
        while self.power.suspending() && !self.stop_requested() {
            if ticks == RESUME_WAIT_TICKS {
                tracing::warn!("no resume notification after going to sleep");
                self.power.end_suspend();
                bt_host::expect_transport_loss(false);
                break;
            }
            self.wait_tick(RESUME_TICK);
            ticks += 1;
        }
        tracing::debug!("resume wait over after {ticks} tick(s), stop requested {}", self.stop_requested());
        !self.stop_requested()
    }

    /// [`Service::probe_adapter`]: holds the stack slot meanwhile, so no
    /// stack starts on the adapter during the probe
    fn probe(&self, instance_id: Option<String>) -> Result<bt_host::probe::ProbeReport, Msg> {
        self.tool(instance_id, "chip probe", BtHost::probe)
    }

    /// [`Service::run_script`], as [`Inner::probe`]
    fn script(
        &self,
        instance_id: Option<String>,
        script: bt_host::script::Script,
    ) -> Result<bt_host::probe::ProbeReport, Msg> {
        self.tool(instance_id, "chip script", move |c| BtHost::run_script(c, script))
    }

    /// A tool in place of a stack on the adapter `instance_id` (None: the one
    /// a stack would open), holding the stack slot meanwhile
    fn tool(
        &self,
        instance_id: Option<String>,
        what: &str,
        run: impl FnOnce(HostConfig) -> Result<bt_host::probe::ProbeReport, InitFailure>,
    ) -> Result<bt_host::probe::ProbeReport, Msg> {
        let mut slot = self.host.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = slot.take() {
            h.shutdown();
            self.adapter_changed(false);
        }
        let mut c = self.host_config(false);
        if let (TransportConfig::Usb(sel), Some(id)) = (&mut c.transport, instance_id) {
            sel.instance_id = Some(id);
            sel.vid_pid = None;
            sel.preferred = None;
        }
        tracing::info!("{what}: transport {:?}", c.transport);
        run(c).map_err(|f| {
            tracing::error!("{what}: no stack could run ({f:?})");
            Msg::new("error.btstack_init")
        })
    }

    pub fn shutdown_host(&self) {
        let old = self.host.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = old {
            let t = Instant::now();
            // Also when the session still holds the Arc: the adapter must be
            // free for the next stack
            h.shutdown();
            tracing::debug!("host shutdown: took {} ms", t.elapsed().as_millis());
            self.hid_stack_stopped();
            self.adapter_changed(false);
        }
    }

    /// Whether the headphones' buttons act as media keys (one setting for
    /// every device, issue #86)
    pub fn media_keys(&self) -> bool {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).media_keys
    }

    pub fn auto_reconnect(&self) -> bool {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).auto_reconnect
    }

    pub fn saved_volume(&self, addr: &str) -> Option<u8> {
        let s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        s.saved_volumes.get(&addr.to_uppercase()).and_then(|&v| u8::try_from(v).ok())
    }

    /// Keeps the headphones' volume for the next connection (saved with the
    /// settings when it changes)
    pub fn remember_volume(&self, addr: &str, volume: Option<u8>) {
        let Some(v) = volume else { return };
        let _tx = self.settings_tx.enter();
        let mut s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        let key = addr.to_uppercase();
        if s.saved_volumes.get(&key) == Some(&(v as i32)) {
            return;
        }
        tracing::debug!("volume: remembering {v} for {key} (was {:?})", s.saved_volumes.get(&key));
        s.saved_volumes.insert(key, v as i32);
        if let Err(e) = s.save(&self.opts.config_dir) {
            tracing::warn!("cannot save the settings: {e}");
        }
    }

    /// What a device offered (after capability discovery)
    pub fn record_codecs(&self, addr: &str, mut codecs: stream::KnownCodecs) {
        tracing::debug!("codecs of {addr}: {:?}, sbc {:?}", codecs.keys, codecs.sbc);
        let mut known = self.codecs.lock().unwrap_or_else(|e| e.into_inner());
        // Kept with what it offers until the record is cleared
        codecs.hi_res_asked |= known.get(&addr.to_uppercase()).is_some_and(|k| k.hi_res_asked);
        if known.get(&addr.to_uppercase()) == Some(&codecs) {
            return;
        }
        known.insert(addr.to_uppercase(), codecs);
        self.save_codecs(&known);
    }

    /// Asks a classic device that just connected for the services it lists
    /// and keeps them (crate::devices::keep_services): its Services tag
    /// has them once it disconnects, also when the tag was never opened
    /// while it was connected (issue #86). On a thread of its own: the
    /// session does not wait for it. Given up when another services query
    /// runs (the dialog's, which keeps its answer too) or the link went
    /// down meanwhile; an answer cut short is never kept.
    pub fn learn_services(&self, host: Arc<BtHost>, device: [u8; 6]) {
        let dir = self.opts.config_dir.clone();
        let spawned =
            std::thread::Builder::new().name("services".into()).spawn(move || match host.device_services(device) {
                Ok(records) => {
                    let services: Vec<ipc::DeviceService> =
                        records.into_iter().map(|r| ipc::DeviceService { uuids: r.classes, name: r.name }).collect();
                    let changed = crate::devices::keep_services(&dir, &bt_host::addr_str(&device), &services);
                    tracing::info!(
                        "services: {} listed after connecting (kept list changed: {changed})",
                        services.len()
                    );
                }
                Err(e) => tracing::debug!("services: not asked after connecting ({e:?})"),
            });
        if let Err(e) = spawned {
            tracing::warn!("services: no thread to ask for them ({e})");
        }
    }

    fn save_codecs(&self, known: &BTreeMap<String, stream::KnownCodecs>) {
        if let Err(e) = crate::devices::save_codecs(&self.opts.config_dir, known) {
            tracing::warn!("cannot save the devices' codecs: {e}");
        }
    }

    /// OPPO earbuds took the Hi-Res request already (issue #237)
    pub fn hi_res_asked(&self, addr: &str) -> bool {
        let known = self.codecs.lock().unwrap_or_else(|e| e.into_inner());
        known.get(&addr.to_uppercase()).is_some_and(|k| k.hi_res_asked)
    }

    pub fn set_hi_res_asked(&self, addr: &str) {
        let mut known = self.codecs.lock().unwrap_or_else(|e| e.into_inner());
        known.entry(addr.to_uppercase()).or_default().hi_res_asked = true;
        self.save_codecs(&known);
    }

    pub fn save_device(&self, addr: &str, name: &str) {
        if let Err(e) = crate::devices::save(&self.opts.config_dir, addr, name) {
            tracing::warn!("cannot save the device: {e}");
        }
    }

    /// The LE Audio stream now (set before its StreamInfo is sent)
    pub fn set_le_stream(&self, details: Option<crate::le_stream::LeStreamDetails>) {
        *self.le_stream.lock().unwrap_or_else(|e| e.into_inner()) = details;
    }

    /// An LE Audio device that streamed (le_audio_devices.txt)
    pub fn save_le_device(&self, addr: &str, name: &str) {
        if let Err(e) = crate::devices::save_le(&self.opts.config_dir, addr, name) {
            tracing::warn!("cannot save the LE Audio device: {e}");
        }
    }

    /// The coordinated sets known: those learned (le_audio_sets.txt), those
    /// of the running stack's CSIP and those kept with the bonds
    pub fn known_le_sets(&self, host: Option<&BtHost>) -> crate::le_sets::KnownSets {
        let dir = &self.opts.config_dir;
        let mut sets = crate::le_sets::KnownSets::of(crate::le_sets::load(dir));
        for members in bt_host::le_stored_sets(&crate::paths::pairing_dir(dir)) {
            sets.add(members.iter().map(bt_host::addr_str), None);
        }
        for (id, members) in host.map(BtHost::le_known_sets).unwrap_or_default() {
            sets.add(members.iter().map(bt_host::addr_str), Some(id));
        }
        sets
    }

    /// An LE Audio stream started: the coordinated set of its devices (its
    /// key read on connecting) is kept with the saved device, so the other
    /// earbud is never saved as a device of its own, and the core merges
    /// saved entries of one set ([`ServiceEvent::LeSetLearned`]). `device`:
    /// the address the session was asked for; `alone`: no other device was
    /// asked for, so every device streamed came with it.
    pub fn learn_le_set(&self, host: &BtHost, device: &[u8; 6], streamed: &[LeDeviceId], alone: bool) {
        let addrs: Vec<[u8; 6]> = match alone {
            true => host.le_devices().into_iter().filter(|d| streamed.contains(&d.id)).map(|d| d.addr.addr).collect(),
            false => Vec::new(),
        };
        let mut members: Vec<String> = Vec::new();
        for (_, set) in host.le_known_sets() {
            if set.iter().any(|a| addrs.contains(a) || a == device) {
                members.extend(set.iter().map(bt_host::addr_str));
            }
        }
        if members.is_empty() {
            return;
        }
        // Asked for under another address (an RPA before the bond): that
        // one is the set's too
        let asked = bt_host::addr_str(device);
        if !members.iter().any(|m| m.eq_ignore_ascii_case(&asked)) {
            members.push(asked);
        }
        match crate::le_sets::remember(&self.opts.config_dir, &members) {
            Ok(true) => tracing::info!("LE Audio: coordinated set of {} members kept with the device", members.len()),
            Ok(false) => {}
            Err(e) => tracing::warn!("LE Audio: cannot keep the coordinated set: {e}"),
        }
        self.send_event(ServiceEvent::LeSetLearned);
    }
}

pub struct Service {
    pub(crate) inner: Arc<Inner>,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// Held while a session is stopped and the next one started: one start
    /// or stop at a time (issue #86: two codec switches in quick succession
    /// ran two sessions on one stack, no sound or crackling)
    control: Mutex<()>,
    /// The latest start or stop asked for ([`Service::ticket`]); an older
    /// one still waiting for `control` is dropped
    latest: AtomicU64,
    /// What the last session was started with: a Bluetooth restart
    /// connects it again (issue #86)
    last_request: Mutex<Option<StreamRequest>>,
    _power: Option<win_sys::power::PowerNotifier>,
}

/// The saved AFH policy, as a stack runs it: the experimental "blend-*"
/// only in developer mode (the CLI's --afh is taken as given)
fn afh_policy(settings: &Settings) -> &str {
    bt_host::afh::effective_policy(&settings.afh, settings.debug_mode)
}

/// The developer overrides (and the health log's interval) count only while
/// developer mode is on. Classic devices are asked for their LE keys always
/// (issue #86: one pairing for both, as Windows does; it runs only with LE
/// Audio on, where SM is set up).
fn apply_dev_overrides(settings: &Settings) {
    bt_host::set_ctkd(true);
    codec::dev::set_overrides(if settings.debug_mode { settings.dev_overrides } else { Default::default() });
    crate::vendor::set_overrides(if settings.debug_mode { settings.dev_vendors.clone() } else { Default::default() });
    crate::health::set_every_interval(settings.log().health_every_interval);
    // Developer > Debug, read when the adapter opens next
    let debug = if settings.debug_mode { settings.dev_debug.clone() } else { Default::default() };
    hci_transport::iso_intr::set_config(hci_transport::iso_intr::IsoInterfaceConfig {
        bulk: debug.mtk_iso_bulk,
        pad_to: debug.mtk_iso_pad.into(),
    });
    let command = bt_host::debug_command::parse(&debug.mtk_command).unwrap_or_else(|e| {
        tracing::warn!("Developer > Debug: command not sent: {e}");
        None
    });
    bt_host::debug_command::set(command);
    bt_host::debug_command::set_iso_queue(debug.mtk_iso_queue);
    bt_host::debug_command::set_before_reset(debug.mtk_command_before_reset);
    let register = bt_host::debug_command::parse_register(&debug.mtk_register).unwrap_or_else(|e| {
        tracing::warn!("Developer > Debug: register write not sent: {e}");
        None
    });
    bt_host::debug_command::set_register(register);
    // LE Audio, read when the next stream is set up
    bt_host::set_cig_overrides(bt_host::CigOverrides {
        latency_c_to_p_ms: debug.le_latency_c_to_p_ms,
        latency_p_to_c_ms: debug.le_latency_p_to_c_ms,
        rtn_c_to_p: debug.le_rtn_c_to_p,
        rtn_p_to_c: debug.le_rtn_p_to_c,
        interleaved: debug.le_interleaved,
    });
    crate::le_stream::set_debug(crate::le_stream::LeDebug {
        mic_preset: debug.le_mic_preset.clone(),
        retry_same_link: debug.le_mic_retry_same_link,
    });
}

impl Service {
    pub fn new(opts: ServiceOptions, events: Sender<ServiceEvent>) -> Service {
        // A folder v1 used is backed up and brought to v2's schema first
        tracing::debug!(
            "service: config dir {}, hci tcp {:?}, usb {:?}, afh override {:?}",
            opts.config_dir.display(),
            opts.hci_tcp,
            opts.usb,
            opts.afh
        );
        crate::migrate::prepare(&opts.config_dir);
        let settings = Settings::load(&opts.config_dir);
        if settings.debug_mode {
            tracing::debug!("service: developer overrides {:?}", settings.dev_overrides);
        }
        apply_dev_overrides(&settings);
        let codecs = crate::devices::load_codecs(&opts.config_dir);
        let inner = Arc::new(Inner {
            opts,
            settings: Mutex::new(settings),
            settings_tx: SettingsTx::default(),
            events: Mutex::new(events),
            host: Mutex::new(None),
            stats: Arc::new(LinkStats::default()),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            power: PowerState::default(),
            tick: (Mutex::new(()), Condvar::new()),
            state: Mutex::new(StreamState::Idle),
            codecs: Mutex::new(codecs),
            reload: AtomicBool::new(false),
            le_cli: Mutex::new(None),
            voice_offered: AtomicBool::new(false),
            voice_wanted: AtomicBool::new(false),
            voice_change: Mutex::new(None),
            voice_events: Mutex::new(Vec::new()),
            mic_test: Default::default(),
            hid: Default::default(),
            mic_dsp: voice_link::MicDsp::new(),
            le_active: AtomicBool::new(false),
            broadcast: AtomicBool::new(false),
            host_adapter: Mutex::new(String::new()),
            broadcast_adapter: Mutex::new(String::new()),
            scanning: AtomicBool::new(false),
            le_stream: Mutex::new(None),
            le_support: Mutex::new(None),
            streamed: AtomicBool::new(false),
            waiting: AtomicBool::new(false),
            turned_off: AtomicBool::new(false),
            source_change: Mutex::new(None),
            source_kept: Mutex::new(None),
            bitpool_change: Mutex::new(None),
            unlisted_keeps: Mutex::new(None),
        });
        // Without a window, also while only the tray runs
        let weak = Arc::downgrade(&inner);
        let power = win_sys::power::PowerNotifier::new(move |e| {
            let Some(inner) = weak.upgrade() else { return };
            match e {
                win_sys::power::PowerEvent::Suspend => {
                    if inner.power.suspend() {
                        tracing::info!("the PC is going to sleep");
                        // The USB transfers get cancelled now: not an error
                        bt_host::expect_transport_loss(true);
                    } else {
                        tracing::debug!("power: repeated suspend notification ignored");
                    }
                }
                win_sys::power::PowerEvent::Resume => {
                    // Both resume kinds may come for one resume
                    if inner.power.resume() {
                        tracing::info!("the PC resumed from sleep, Bluetooth will be restarted");
                    } else {
                        tracing::debug!("power: second resume notification ignored");
                    }
                    bt_host::expect_transport_loss(false);
                }
            }
        });
        let power = match power {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("no sleep / resume notifications: {e}");
                None
            }
        };
        pipeline::set_thread_priority_hook(|| pro_audio("encode thread"));
        hci_transport::set_acl_out_priority_hook(|| pro_audio("USB ACL writer"));
        bt_host::set_runloop_priority_hook(|| pro_audio("BTstack thread"));
        // Keyboards and mice (experimental): devices added and not confirmed
        // before the last exit are removed, then their stack runs from the
        // start
        inner.hid_remove_unconfirmed();
        inner.hid_keep();
        Service {
            inner,
            worker: Mutex::new(None),
            control: Mutex::new(()),
            latest: AtomicU64::new(0),
            last_request: Mutex::new(None),
            _power: power,
        }
    }

    pub fn stats(&self) -> Arc<LinkStats> {
        self.inner.stats.clone()
    }

    pub fn config_dir(&self) -> &std::path::Path {
        &self.inner.opts.config_dir
    }

    pub fn capture(&self) -> &HciCapture {
        &self.inner.opts.capture
    }

    /// Replaces the settings (saved by the caller) and applies what affects
    /// a running stack.
    ///
    /// LE Audio (experimental) on / off (v1 OnToggleLeAudio): LE (SM, GATT,
    /// ISO) is set up when BTstack starts, so an idle stack is stopped and
    /// the next scan or connection starts one with the new setting; while
    /// streaming or scanning the stack keeps running and is restarted
    /// before the next scan or connection ([`Service::le_audio_deferred`]
    /// says so). The low-latency setting applies to the next LE stream.
    pub fn set_settings(&self, settings: Settings) {
        tracing::debug!(
            "settings applied: media keys {}, volume sync {}, afh {:?}, LE {}, debug mode {}",
            settings.media_keys,
            settings.volume_sync,
            settings.afh,
            settings.le_audio(),
            settings.debug_mode
        );
        apply_dev_overrides(&settings);
        let media_keys = settings.media_keys;
        let afh = self.inner.opts.afh.clone().unwrap_or_else(|| afh_policy(&settings).to_owned());
        let old = std::mem::replace(&mut *self.inner.settings.lock().unwrap_or_else(|e| e.into_inner()), settings);
        let new_le = self.inner.settings.lock().unwrap_or_else(|e| e.into_inner()).le_audio();
        // A stack starting or stopping holds the slot, for up to tens of
        // seconds with an adapter that does not answer: the settings are not
        // kept waiting. A stack that starts reads them once it is up, and
        // the next scan or connection checks LE Audio (ensure_host)
        let host = match self.inner.host.try_lock() {
            Ok(slot) => slot.clone(),
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().clone(),
            Err(std::sync::TryLockError::WouldBlock) => {
                tracing::debug!("settings applied: the stack is starting or stopping, it reads them when up");
                None
            }
        };
        if let Some(h) = &host {
            h.set_media_keys(media_keys);
            h.set_afh_policy(&afh);
        }
        if old.le_audio() != new_le && host.is_some() && self.inner.le_active() != self.inner.le_wanted() {
            if self.is_running() || self.is_scanning() {
                tracing::info!(
                    "LE Audio turned {}: BTstack restarts before the next scan or connection",
                    if new_le { "on" } else { "off" }
                );
            } else {
                tracing::info!(
                    "LE Audio turned {}: stopping BTstack, the next scan or connection starts it anew",
                    if new_le { "on" } else { "off" }
                );
                self.inner.shutdown_host();
            }
        }
        self.hid_setting_changed(old.hid_host());
    }

    /// Service mode, after the one-time move put the user's files into the
    /// folder (crate::service_data): the settings and the devices' codecs
    /// read again, and an idle stack released so that the next one reads
    /// the moved pairing keys and device lists. A stack in use (a stream,
    /// a scan) is replaced at its next use. Returns the settings read.
    pub fn reload_saved(&self) -> Settings {
        let dir = self.inner.opts.config_dir.clone();
        let settings = Settings::load(&dir);
        *self.inner.codecs.lock().unwrap_or_else(|e| e.into_inner()) = crate::devices::load_codecs(&dir);
        self.set_settings(settings.clone());
        let idle = !self.is_running() && !self.is_scanning();
        let released = self.reload_adapter(idle);
        tracing::info!("service: the moved data read again (stack released now: {released})");
        settings
    }

    /// HFP audio for every stack this service starts, so that headset mode
    /// can be switched on in place later (the developer CLI's
    /// --headset-mic-toggle; before the first stack starts)
    pub fn set_voice_offered(&self, on: bool) {
        self.inner.voice_offered.store(on, Ordering::Release);
    }

    /// LE Audio (experimental) for every stack this service starts, whatever
    /// the setting says (the command line's --le-audio)
    pub fn set_le_audio_cli(&self, options: LeCliOptions) {
        *self.inner.le_cli.lock().unwrap_or_else(|e| e.into_inner()) = Some(options);
    }

    /// The running stack has LE Audio (experimental)
    pub fn le_audio_active(&self) -> bool {
        self.host().is_some() && self.inner.le_active()
    }

    /// The LE Audio setting changed while the stack was in use: it is
    /// applied before the next scan or connection (settings.le_audio_deferred)
    pub fn le_audio_deferred(&self) -> bool {
        self.host().is_some() && self.inner.le_active() != self.inner.le_wanted()
    }

    /// What the LE Audio (experimental) stream was set up with, while one
    /// runs (with its [`ServiceEvent::StreamInfo`]; codec "LC3 (LE Audio)")
    pub fn le_stream(&self) -> Option<crate::le_stream::LeStreamDetails> {
        self.inner.le_stream.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A device scan runs
    pub fn is_scanning(&self) -> bool {
        self.inner.scanning.load(Ordering::Acquire)
    }

    /// The running stack, started (or restarted, as for a connection) as
    /// needed: the command line's LE Audio flow
    pub fn ensure_host(&self) -> Result<Arc<BtHost>, Msg> {
        self.inner.ensure_host()
    }

    /// Stops the stack, releasing the adapter (e.g. before its driver is
    /// changed); the next use starts a new one
    pub fn release_adapter(&self) {
        // Keyboards and mice (experimental) do not take the adapter back
        self.inner.hid.hold();
        self.forget_session();
        self.shutdown();
    }

    /// Disconnect (the window, the tray): the session stops, and a
    /// Bluetooth restart does not connect it again (issue #86)
    pub fn disconnect(&self) {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        self.forget_session();
        self.stop_session();
        self.release_link();
    }

    /// After Disconnect: the headphones are free for another device, with
    /// the stack still up (issue #270)
    fn release_link(&self) {
        if let Some(h) = self.host() {
            h.drop_acl_link(LINK_RELEASE_WAIT);
        }
    }

    /// Changes the source of the running A2DP stream or Auracast broadcast:
    /// the link (the BIGs) stays up. False when none is streaming (it
    /// connects or reconnects, a start or stop is under way, an LE Audio
    /// unicast stream, application capture for a broadcast): the caller
    /// starts it again instead.
    pub fn change_source(&self, source: SourceRequest) -> bool {
        let Ok(_control) = self.control.try_lock() else { return false };
        if !self.is_running() || self.state() != StreamState::Streaming {
            return false;
        }
        self.follow_kept_source();
        match self.last_request.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            Some(req) if req.le.is_none() => req.source = source.clone(),
            Some(req)
                if req.le.as_ref().is_some_and(|l| l.broadcast.is_some())
                    && !matches!(source, SourceRequest::Application { .. }) =>
            {
                req.source = source.clone()
            }
            _ => return false,
        }
        *self.inner.source_change.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
        self.inner.tick.1.notify_all();
        true
    }

    /// A change of source the session could not make
    /// ([`Inner::keep_source`]): the request names the source that plays
    fn follow_kept_source(&self) {
        let kept = self.inner.source_kept.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let (Some(source), Some(req)) = (kept, self.last_request.lock().unwrap_or_else(|e| e.into_inner()).as_mut())
        {
            req.source = source;
        }
    }

    /// Headset mode off, on or auto, its output and the best call codec
    /// (`voice_codec`, None: Auto) for the classic stream running, the
    /// link kept (the session's next tick). False when none streams, LE
    /// Audio runs, or the stack was started without voice (on or auto): the
    /// caller starts the stream again instead.
    ///
    /// Unlike the source and bitpool changes it waits for `control`: a
    /// short holder (another change) must not cost a restart of the
    /// profile. A start or stop under way holds it until the old session
    /// has ended (never the session thread itself, which takes no
    /// `control`), and the stream is then not Streaming; so without a
    /// stream streaming it answers at once.
    pub fn set_headset_mic(
        &self,
        mode: HeadsetMic,
        mic_output: Option<String>,
        voice_codec: Option<voice_link::VoiceCodec>,
    ) -> bool {
        let on = mode.is_on();
        if !self.is_running() || self.state() != StreamState::Streaming {
            return false;
        }
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if !self.is_running() || self.state() != StreamState::Streaming {
            return false;
        }
        if on && !self.host().is_some_and(|h| h.voice_enabled()) {
            tracing::debug!("headset mode: the stack has no voice, starting the stream again");
            return false;
        }
        match self.last_request.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            Some(req) if req.le.is_none() => {
                req.headset_mic = mode;
                req.mic_output = mic_output.clone();
                req.voice_codec = voice_codec;
            }
            _ => return false,
        }
        // What the request wants now. Off: the running stack keeps offering
        // voice (ensure_host restarts only for voice wanted and missing), a
        // stack started later goes without it.
        self.inner.voice_wanted.store(on, Ordering::Release);
        *self.inner.voice_change.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(VoiceChange { mode, mic_output, voice_codec });
        self.inner.tick.1.notify_all();
        true
    }

    /// The microphone test records (Diagnostics): the classic stream's
    /// session opens the voice link for it. Ok(true): the running stack has
    /// no voice, the caller starts the stream again. Err: the message key
    /// (no classic stream streaming).
    pub fn mic_test_record(&self) -> Result<bool, &'static str> {
        if !self.is_running() || self.state() != StreamState::Streaming {
            return Err("mic_test.not_streaming");
        }
        match self.last_request.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(req) if req.le.is_none() => {}
            Some(_) => return Err("mic_test.le_audio"),
            None => return Err("mic_test.not_streaming"),
        }
        let restart = !self.host().is_some_and(|h| h.voice_enabled());
        self.inner.mic_test.record(restart);
        self.inner.tick.1.notify_all();
        Ok(restart)
    }

    /// The microphone test: stops its recording or playback
    pub fn mic_test_stop(&self) {
        self.inner.mic_test.stop();
        self.inner.tick.1.notify_all();
    }

    /// The microphone test: plays the recording to the headphones
    pub fn mic_test_play(&self) {
        self.inner.mic_test.play();
    }

    /// The microphone test ends: the voice link closes unless headset mode
    /// wants it
    pub fn mic_test_end(&self) {
        self.inner.mic_test.end();
        self.inner.tick.1.notify_all();
    }

    /// The microphone test's state (keeps it alive)
    pub fn mic_test_status(&self) -> ipc::MicTestState {
        self.inner.mic_test.status()
    }

    /// Other processing for the headset microphone: the running microphone
    /// (classic or LE Audio) takes it with its next samples, and a stream
    /// started again for the same request keeps it
    pub fn set_mic_processing(&self, p: ipc::MicProcessing) {
        if let Some(req) = self.last_request.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            req.mic_processing = p;
        }
        self.inner.apply_mic_processing(&p);
    }

    /// Other bitpools for the A2DP stream running, without touching the link
    /// (the session's next tick). False when none runs: the caller starts it
    /// again instead.
    pub fn change_bitpools(&self, bitpools: stream::Bitpools) -> bool {
        let Ok(_control) = self.control.try_lock() else { return false };
        if !self.is_running() || self.state() != StreamState::Streaming {
            return false;
        }
        match self.last_request.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            Some(req) if req.le.is_none() => req.set_bitpools(bitpools),
            _ => return false,
        }
        *self.inner.bitpool_change.lock().unwrap_or_else(|e| e.into_inner()) = Some(bitpools);
        self.inner.tick.1.notify_all();
        true
    }

    /// "Show only options it supports" turned for the A2DP stream running:
    /// taken without touching the link when the stream would be set up the
    /// same either way (issue #86). False otherwise or when none runs: the
    /// caller starts it again instead.
    pub fn change_unlisted(&self, on: bool) -> bool {
        let Ok(_control) = self.control.try_lock() else { return false };
        if !self.is_running() || self.state() != StreamState::Streaming {
            return false;
        }
        let keeps = *self.inner.unlisted_keeps.lock().unwrap_or_else(|e| e.into_inner());
        if !keeps.is_some_and(|k| k[usize::from(on)]) {
            return false;
        }
        match self.last_request.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            Some(req) if req.le.is_none() => req.unlisted_options = on,
            _ => return false,
        }
        true
    }

    /// What the service keeps of a device that is forgotten: no Bluetooth
    /// restart connects it again, and (classic devices) what it offered and
    /// its link key go, so it cannot connect by itself either
    pub fn forget_device(&self, address: &str, le_audio: bool) {
        let Some(addr) = bt_host::parse_addr(address) else { return };
        {
            let mut last = self.last_request.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_ref().is_some_and(|r| r.device == addr && r.le.is_some() == le_audio) {
                *last = None;
            }
        }
        if le_audio {
            // A link key from its LE pairing (issue #86) goes with it unless
            // its classic entry is still saved and connects with that key
            let dir = &self.inner.opts.config_dir;
            if !crate::devices::load(dir).iter().any(|d| d.address.eq_ignore_ascii_case(address)) {
                self.forget_link_key_only(addr);
            }
            // Its LE bond too: nothing reconnects it, and adding it again
            // pairs anew. A running stack with LE holds the bonds and
            // deletes them itself.
            if !self.host().is_some_and(|h| h.forget_le_bond(addr)) {
                let pairing = crate::paths::pairing_dir(dir);
                match bt_host::le_bond_file::forget(&crate::paths::le_bonds_file(dir), &addr) {
                    Ok(n) => tracing::debug!("device forgotten: {n} LE bond(s) removed from the file"),
                    Err(e) => tracing::warn!("device forgotten: the LE bonds file cannot be changed ({})", e.kind()),
                }
                bt_host::le_forget_stored_sirk(&pairing, addr);
            }
            return;
        }
        {
            let mut known = self.inner.codecs.lock().unwrap_or_else(|e| e.into_inner());
            if known.remove(&address.to_uppercase()).is_some()
                && let Err(e) = crate::devices::save_codecs(&self.inner.opts.config_dir, &known)
            {
                tracing::warn!("cannot save the devices' codecs: {e}");
            }
        }
        // A running stack holds the keys in memory and writes them in full
        if !self.host().is_some_and(|h| h.forget_link_key(addr)) {
            bt_host::link_keys::forget(&crate::paths::link_keys_file(&self.inner.opts.config_dir), &addr);
        }
    }

    /// Drops `addr`'s link key alone (its LE keys stay with its LE Audio
    /// entry's own bond)
    pub(crate) fn forget_link_key_only(&self, addr: [u8; 6]) {
        if !self.host().is_some_and(|h| h.forget_link_key_only(addr)) {
            bt_host::link_keys::forget(&crate::paths::link_keys_file(&self.inner.opts.config_dir), &addr);
        }
    }

    /// A ticket for a start or stop run later on its own thread
    /// ([`Service::start_streaming_for`], [`Service::disconnect_for`]): the
    /// latest one taken is the one that runs
    pub fn ticket(&self) -> u64 {
        self.latest.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Takes `control` for the request with this ticket; None when a newer
    /// one was asked for meanwhile (it runs instead)
    fn control_for(&self, ticket: u64, what: &str) -> Option<std::sync::MutexGuard<'_, ()>> {
        let control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if self.latest.load(Ordering::Acquire) != ticket {
            tracing::debug!("{what}: skipped, a newer start or stop was asked for");
            return None;
        }
        Some(control)
    }

    /// [`Service::start_streaming`] unless a newer start or stop was asked
    /// for since the ticket was taken
    pub fn start_streaming_for(&self, req: StreamRequest, ticket: u64) {
        if let Some(_control) = self.control_for(ticket, "start streaming") {
            self.start_session(req);
        }
    }

    /// [`Service::disconnect`] unless a newer start or stop was asked for
    /// since the ticket was taken
    pub fn disconnect_for(&self, ticket: u64) {
        if let Some(_control) = self.control_for(ticket, "disconnect") {
            self.forget_session();
            // A start skipped for this stop left "connecting" shown
            if !self.stop_session() {
                self.inner.notify(StreamState::Idle, Msg::new("status.ready"));
            }
            self.release_link();
        }
    }

    fn forget_session(&self) {
        *self.last_request.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Restarts Bluetooth (the Adapters page): the stack is stopped, and
    /// the last session's request is returned to be started again on a new
    /// stack (the same device, profile and codec), as a session reconnects
    /// after the other stack restarts (sleep, a stopped adapter): while it
    /// connects or streams, and also after it ended on its own (an error, a
    /// lost link), which a restart is often meant to fix. Not after
    /// Disconnect, and not when the headphones were turned off: a session
    /// that waits for them just ends (issue #86).
    pub fn restart_adapter(&self) -> Option<StreamRequest> {
        self.follow_kept_source();
        let last = self.last_request.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let again = reconnect_after_restart(
            self.is_running() && self.inner.stop_requested(),
            self.inner.turned_off.load(Ordering::Acquire),
            last,
        );
        tracing::info!(
            "restarting Bluetooth (Adapters page): session running {}, {}",
            self.is_running(),
            match &again {
                Some(r) => format!("reconnecting {} afterwards", bt_host::addr_str(&r.device)),
                None => "nothing to reconnect".into(),
            }
        );
        self.shutdown();
        again
    }

    /// The chip or firmware setting changed (v1 reset_btstack): an idle
    /// stack is released now, one in use is replaced at its next use (the
    /// next connection or scan), so the firmware is loaded again. `idle`:
    /// nothing streams or scans. True when it was released now.
    pub fn reload_adapter(&self, idle: bool) -> bool {
        // Maybe another adapter: its LE Audio support is not known yet
        *self.inner.le_support.lock().unwrap_or_else(|e| e.into_inner()) = None;
        tracing::debug!(
            "reload adapter: idle {idle}, streaming {}, stack running {}",
            self.is_running(),
            self.host().is_some()
        );
        if idle && !self.is_running() {
            self.inner.reload.store(false, Ordering::Release);
            self.inner.shutdown_host();
            true
        } else {
            self.inner.reload.store(self.host().is_some(), Ordering::Release);
            false
        }
    }

    /// The running adapter has LE Audio's isochronous channels; None until a
    /// stack started on it since the adapter settings last changed
    pub fn le_audio_supported(&self) -> Option<bool> {
        *self.inner.le_support.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What each device (upper-case address) offered when it last
    /// connected
    pub fn device_codecs(&self) -> BTreeMap<String, stream::KnownCodecs> {
        self.inner.codecs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The coordinated sets known ([`Inner::known_le_sets`])
    pub fn known_le_sets(&self) -> crate::le_sets::KnownSets {
        self.inner.known_le_sets(self.host().as_deref())
    }

    /// Forgets what a device offered, and that it was asked for Hi-Res: its
    /// next connection reads and asks again (issue #237). False: nothing kept.
    pub fn forget_device_codecs(&self, address: &str) -> bool {
        let mut known = self.inner.codecs.lock().unwrap_or_else(|e| e.into_inner());
        if known.remove(&address.to_uppercase()).is_none() {
            return false;
        }
        tracing::info!("device {address}: what it offered forgotten");
        self.inner.save_codecs(&known);
        true
    }

    /// Held while the saved settings change: read them ([`Service::settings`]),
    /// change the copy, save it and apply it ([`Service::set_settings`]) under
    /// one guard. Without it, a copy read before another change is saved
    /// after it and takes that change back (the user's settings moved into
    /// the service's folder were replaced by the defaults this way). The
    /// same thread may take it again.
    pub fn settings_tx(&self) -> SettingsTxGuard<'_> {
        self.inner.settings_tx.enter()
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn state(&self) -> StreamState {
        *self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Acquire)
    }

    /// The running stack, if any (for diagnostics)
    pub fn host(&self) -> Option<Arc<BtHost>> {
        self.inner.host.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Starts a session; a running one is stopped first
    pub fn start_streaming(&self, req: StreamRequest) {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        self.start_session(req);
    }

    fn start_session(&self, req: StreamRequest) {
        tracing::debug!(
            "start streaming: {} ({}), stopping any session first",
            bt_host::addr_str(&req.device),
            if req.le.is_some() { "LE Audio" } else { "A2DP" }
        );
        self.end_scan();
        // The running session leaves Windows' default output switched when
        // this one switches it to the same device (issue #203)
        stream::expect_default_switch(req.source.default_switch());
        crate::mute_output::expect_session(req.mutes_output(&req.source));
        self.stop_session();
        stream::expect_default_switch(None);
        crate::mute_output::expect_session(false);
        // Saved in the profile, so in `req` too: left over from a session
        // that ended before it took it
        self.inner.take_source_change();
        self.inner.take_bitpool_change();
        self.inner.take_voice_change();
        self.inner.take_voice_events();
        // Before the stack starts: one that offers HFP audio
        self.inner.voice_wanted.store(req.headset_mic.is_on() && req.le.is_none(), Ordering::Release);
        self.inner.unlisted_keeps.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.inner.apply_mic_processing(&req.mic_processing);
        *self.last_request.lock().unwrap_or_else(|e| e.into_inner()) = Some(req.clone());
        self.inner.turned_off.store(false, Ordering::Release);
        self.inner.stop.store(false, Ordering::Release);
        self.inner.running.store(true, Ordering::Release);
        self.inner.notify(StreamState::Connecting, Msg::new("status.initializing"));
        let inner = self.inner.clone();
        let t = std::thread::Builder::new()
            .name("session".into())
            .spawn(move || {
                // A panic in the session must not take the process down: the
                // state says what happened
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // LE Audio (experimental) profiles have their own session
                    if req.le.is_some() { crate::le_stream::run(&inner, req) } else { stream::run(&inner, req) }
                }));
                if r.is_err() {
                    tracing::error!("the streaming session panicked");
                    inner.notify(StreamState::Error, Msg::new("error.btstack_crash_stream"));
                }
                stream::restore_unclaimed_default();
                crate::mute_output::unmute_unclaimed();
                inner.running.store(false, Ordering::Release);
            })
            .expect("spawning the session thread");
        *self.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
    }

    /// Stops the session and waits for it (bounded by the stack's own
    /// timeouts: blocking waits are cancelled)
    pub fn stop_streaming(&self) {
        let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        self.stop_session();
    }

    /// False when no session was there to stop
    fn stop_session(&self) -> bool {
        let Some(t) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() else { return false };
        tracing::debug!("stop streaming: requested (state {:?})", self.state());
        let started = Instant::now();
        self.inner.stop.store(true, Ordering::Release);
        self.inner.tick.1.notify_all();
        if let Some(h) = self.host() {
            h.cancel_pending_waits();
        }
        let _ = t.join();
        tracing::debug!("stop streaming: session ended in {} ms", started.elapsed().as_millis());
        true
    }

    /// A session that waits for turned-off headphones holds nothing worth
    /// keeping: a scan ends it
    fn end_wait_for_device(&self) {
        if self.inner.waiting.load(Ordering::Acquire) {
            tracing::info!("scan: no longer waiting for the headphones");
            self.stop_streaming();
        }
    }

    /// The chip probe (bt_host::probe) on the adapter `instance_id` (None:
    /// the one a stack would open): read commands only, for adapters
    /// BlueCaster cannot bring up yet. The stack stops first; the next use
    /// starts a new one.
    pub fn probe_adapter(&self, instance_id: Option<String>) -> Result<bt_host::probe::ProbeReport, Msg> {
        self.end_wait_for_device();
        if self.is_running() {
            return Err(Msg::new("status.streaming"));
        }
        if self.is_scanning() {
            return Err(Msg::new("status.scanning_devices"));
        }
        self.inner.probe(instance_id)
    }

    /// A chip script (bt_host::script) on the adapter `instance_id`, as
    /// [`Service::probe_adapter`]. The caller has checked it: written
    /// requests allowed, no denied opcode.
    pub fn run_script(
        &self,
        instance_id: Option<String>,
        script: bt_host::script::Script,
    ) -> Result<bt_host::probe::ProbeReport, Msg> {
        self.end_wait_for_device();
        if self.is_running() {
            return Err(Msg::new("status.streaming"));
        }
        if self.is_scanning() {
            return Err(Msg::new("status.scanning_devices"));
        }
        self.inner.script(instance_id, script)
    }

    /// Inquiry for nearby (classic) devices
    pub fn scan(&self, seconds: u32) -> Result<Vec<DiscoveredDevice>, Msg> {
        self.end_wait_for_device();
        if self.is_running() {
            return Err(Msg::new("status.streaming"));
        }
        let _scanning = ScanningGuard::new(&self.inner.scanning);
        self.inner.notify(StreamState::Idle, Msg::new("status.initializing_scan"));
        let host = self.inner.ensure_host()?;
        self.inner.notify(StreamState::Idle, Msg::new("status.scanning_devices"));
        let found = host.scan(seconds);
        for d in &found {
            tracing::debug!(
                "scan: {} \"{}\" rssi {} class {:06x}",
                bt_host::addr_str(&d.addr),
                d.name,
                d.rssi,
                d.class_of_device
            );
        }
        self.inner.notify(StreamState::Idle, Msg::with("status.scan_complete", &[&found.len()]));
        Ok(found)
    }

    /// Inquiry for nearby devices and, when the stack has LE Audio
    /// (experimental), an LE scan for LE Audio devices beside it: the
    /// classic devices, then the LE Audio ones ([`ScannedDevice`]).
    /// `on_found` gets the list each time a device answers (or its name
    /// comes, or an LE Audio advertiser shows up), long before the scan
    /// ends. The LE scan runs from the start (issue #86: run after the
    /// inquiry, it never ran when the scan was stopped once the classic
    /// entry showed, nor in the first-run setup's long scan), and at least
    /// [`LE_SCAN`] when nobody stops the scan.
    pub fn scan_devices(
        &self,
        seconds: u32,
        on_found: &mut dyn FnMut(&[ScannedDevice]),
    ) -> Result<Vec<ScannedDevice>, Msg> {
        self.end_wait_for_device();
        if self.is_running() {
            return Err(Msg::new("status.streaming"));
        }
        let _scanning = ScanningGuard::new(&self.inner.scanning);
        self.inner.notify(StreamState::Idle, Msg::new("status.initializing_scan"));
        let host = self.inner.ensure_host()?;
        let mut le_scan = false;
        // Keyboards and mice (experimental): LE ones are listed too, also
        // without LE Audio (the stack runs LE for them)
        let hid_le = self.inner.hid_wanted() && host.hid_enabled();
        if self.inner.le_active() {
            let until = Instant::now() + LE_SCAN_READY_WAIT;
            while !host.le_ready() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(100));
            }
            le_scan = host.le_scan_start();
            if !le_scan {
                tracing::warn!("LE Audio not ready, LE scan skipped");
            }
        } else if hid_le {
            let until = Instant::now() + LE_SCAN_READY_WAIT;
            while !host.hid_le_ready() && !host.hid_le_unavailable() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(100));
            }
            le_scan = host.hid_le_ready() && host.le_scan_start();
            if !le_scan {
                tracing::info!("keyboards and mice: no LE scan (the adapter has no LE, or it is not ready)");
            }
        }
        // LE Audio's entries only with LE Audio on
        let audio_le = le_scan && self.inner.le_active();
        let le_started = Instant::now();
        let (saved_le, own, sets) = if audio_le {
            let own: Vec<String> = win_sys::bt_radios::radios().into_iter().filter_map(|r| r.address).collect();
            (crate::devices::load_le(&self.inner.opts.config_dir), own, self.inner.known_le_sets(Some(&host)))
        } else {
            (Vec::new(), Vec::new(), Default::default())
        };
        let saved = crate::devices::load(&self.inner.opts.config_dir);
        self.inner.notify(StreamState::Idle, Msg::new("status.scanning_devices"));
        let classic = host.scan_with(seconds, &mut |found| {
            let mut list: Vec<ScannedDevice> = found.iter().map(ScannedDevice::classic).collect();
            let results = if le_scan { host.le_scan_results() } else { Vec::new() };
            if audio_le {
                let mut le = le_scan_entries(&results, &saved_le, &own, &sets).0;
                let classic: Vec<String> =
                    list.iter().map(|d| d.address.clone()).chain(saved.iter().map(|d| d.address.clone())).collect();
                let drop = classic_appearance_only(&results, &classic);
                le.retain(|e| !drop.contains(&e.address));
                let known: Vec<(String, String)> = list
                    .iter()
                    .map(|d| (d.address.clone(), d.name.clone()))
                    .chain(saved.iter().map(|d| (d.address.clone(), d.name.clone())))
                    .chain(saved_le.iter().map(|d| (d.address.clone(), d.name.clone())))
                    .collect();
                fill_le_names(&mut le, &known);
                list.extend(le);
            }
            if hid_le {
                list.extend(hid_le_entries(&results));
            }
            // A device is listed once its name is known: most follow within
            // seconds (name request, scan response), and "Unknown" turning
            // into the name looked like a second device (issue #86). Those
            // still nameless are in the scan's result.
            list.retain(|d| !d.name.trim().is_empty());
            on_found(&list);
        });
        let mut list: Vec<ScannedDevice> = classic.iter().map(ScannedDevice::classic).collect();
        for d in &list {
            tracing::debug!(
                "scan: {} \"{}\" rssi {} class {:06x} audio {}",
                d.address,
                d.name,
                d.rssi,
                d.class_of_device,
                d.audio
            );
        }
        let mut le_found = 0;
        if le_scan {
            let left = LE_SCAN.saturating_sub(le_started.elapsed());
            if !host.scan_stopped() && !left.is_zero() {
                self.inner.notify(StreamState::Idle, Msg::new("status.le_scanning"));
                let until = Instant::now() + left;
                while Instant::now() < until && !host.scan_stopped() {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            let results = host.le_scan_stop();
            let hid = if hid_le { hid_le_entries(&results) } else { Vec::new() };
            if hid_le {
                tracing::info!("LE scan: {} keyboard(s) or mice", hid.len());
            }
            if !audio_le {
                le_found = hid.len();
                list.extend(hid);
                self.inner.notify(StreamState::Idle, Msg::with("status.scan_complete", &[&(classic.len() + le_found)]));
                return Ok(list);
            }
            for r in results.iter().filter(|r| r.le_audio) {
                tracing::debug!(
                    "le scan: advertiser {} {} appearance 0x{:04X}, connectable {}, UUIDs {:04X?}",
                    r.addr,
                    crate::diag::quoted(&r.name),
                    r.appearance,
                    r.connectable,
                    r.uuids16
                );
            }
            let (mut entries, found) = le_scan_entries(&results, &saved_le, &own, &sets);
            let classic: Vec<String> =
                list.iter().map(|d| d.address.clone()).chain(saved.iter().map(|d| d.address.clone())).collect();
            let drop = classic_appearance_only(&results, &classic);
            entries.retain(|e| !drop.contains(&e.address));
            for a in drop.iter().filter(|a| saved_le.iter().any(|d| d.address.eq_ignore_ascii_case(a))) {
                tracing::info!(
                    "LE scan: a classic device's LE side without LE Audio removed from the LE Audio devices"
                );
                if let Err(e) = crate::devices::remove_le(&self.inner.opts.config_dir, a) {
                    tracing::warn!("LE scan: removing that LE side: {e}");
                }
            }
            tracing::info!(
                "LE scan: {} advertiser(s) in {:.1} s, {found} LE Audio, {} listed",
                results.len(),
                le_started.elapsed().as_secs_f32(),
                entries.len()
            );
            // A classic device's LE side without an LE Audio service: what
            // tells "LE Audio is off on the device" from a missed one
            let known: Vec<(String, String)> = list
                .iter()
                .map(|d| (d.address.clone(), d.name.clone()))
                .chain(saved.iter().map(|d| (d.address.clone(), d.name.clone())))
                .chain(saved_le.iter().map(|d| (d.address.clone(), d.name.clone())))
                .collect();
            fill_le_names(&mut entries, &known);
            for r in classic_le_sides(&results, &known) {
                tracing::info!(
                    "LE scan: {} {} advertises over LE without an LE Audio service (type {}, connectable {}, \
                     appearance 0x{:04X}, UUIDs {:04X?}, {} + {} bytes)",
                    r.addr,
                    crate::diag::quoted(&r.name),
                    r.addr.addr_type,
                    r.connectable,
                    r.appearance,
                    r.uuids16,
                    r.adv.len(),
                    r.scan_rsp.len()
                );
            }
            for e in &entries {
                tracing::debug!(
                    "le scan: {} {} rssi {} set {}",
                    e.address,
                    crate::diag::quoted(&e.name),
                    e.rssi,
                    e.le_audio_set
                );
            }
            le_found = found + hid.len();
            list.extend(entries);
            list.extend(hid);
            save_twins(&self.inner.opts.config_dir, &list);
        }
        self.inner.notify(StreamState::Idle, Msg::with("status.scan_complete", &[&(classic.len() + le_found)]));
        Ok(list)
    }

    /// Ends a running scan early ([`Service::scan_devices`] returns what it
    /// found so far)
    pub fn stop_scan(&self) {
        if self.is_scanning()
            && let Some(h) = self.host()
        {
            tracing::info!("scan: stopped");
            h.stop_scan();
        }
    }

    /// A connection while the scan inquires and pages on the same radio
    /// would be slow or fail: the scan ends first (a device it listed was
    /// picked)
    fn end_scan(&self) {
        if !self.is_scanning() {
            return;
        }
        self.stop_scan();
        let until = Instant::now() + SCAN_END_WAIT;
        while self.is_scanning() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        tracing::debug!("start streaming: scan ended first (still running: {})", self.is_scanning());
    }

    /// Stops streaming and the stack (the adapter is released)
    pub fn shutdown(&self) {
        self.inner.hid.hold();
        self.stop_streaming();
        self.inner.shutdown_host();
    }

    pub fn saved_volumes(&self) -> BTreeMap<String, i32> {
        self.settings().saved_volumes
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The request to start again after a Bluetooth restart: the last
/// session's (None after Disconnect), unless it is being stopped or its
/// headphones were turned off
fn reconnect_after_restart(stopping: bool, turned_off: bool, last: Option<StreamRequest>) -> Option<StreamRequest> {
    last.filter(|_| !stopping && !turned_off)
}

/// Marks a scan while it runs (also when it returns early)
struct ScanningGuard<'a>(&'a AtomicBool);

impl<'a> ScanningGuard<'a> {
    fn new(flag: &'a AtomicBool) -> ScanningGuard<'a> {
        flag.store(true, Ordering::Release);
        ScanningGuard(flag)
    }
}

impl Drop for ScanningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The link to the device came up moments ago: headphones that just
/// connected may refuse the PC's page for a while once it is dropped
fn link_is_fresh(age: Option<Duration>) -> bool {
    age.is_some_and(|a| a < FRESH_LINK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_thread_at_a_time_changes_the_saved_settings() {
        let tx = Arc::new(SettingsTx::default());
        let outer = tx.enter();
        // The same thread again: no deadlock
        let inner = tx.enter();
        let (t2, entered) = (tx.clone(), Arc::new(AtomicBool::new(false)));
        let seen = entered.clone();
        let other = std::thread::spawn(move || {
            let _g = t2.enter();
            seen.store(true, Ordering::Release);
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!entered.load(Ordering::Acquire), "another thread waits");
        drop(inner);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!entered.load(Ordering::Acquire), "still held once");
        drop(outer);
        other.join().unwrap();
        assert!(entered.load(Ordering::Acquire));
    }

    #[test]
    fn afh_blend_runs_only_in_developer_mode() {
        let s = |afh: &str, debug_mode| Settings { afh: afh.into(), debug_mode, ..Default::default() };
        assert_eq!(afh_policy(&s("blend-medium", true)), "blend-medium");
        assert_eq!(afh_policy(&s("blend-medium", false)), "auto");
        assert_eq!(afh_policy(&s("6,11", false)), "6,11");
        assert_eq!(afh_policy(&s("off", false)), "off");
    }

    #[test]
    fn only_a_link_that_just_came_up_keeps_the_stack() {
        assert!(link_is_fresh(Some(Duration::from_secs(5))));
        assert!(!link_is_fresh(Some(FRESH_LINK)));
        assert!(!link_is_fresh(None));
    }

    #[test]
    fn a_bluetooth_restart_reconnects_the_running_session() {
        let req = crate::profiles::Profile {
            device_address: "00:11:22:33:44:55".into(),
            codec: "ldac".into(),
            ..Default::default()
        }
        .to_request(1000)
        .unwrap();
        let last = || Some(req.clone());
        // Streaming, connecting, or ended on its own: the same request again
        assert_eq!(reconnect_after_restart(false, false, last()), last());
        // Being stopped, the headphones are off, or nothing to reconnect
        // (none ran, or Disconnect forgot it)
        assert_eq!(reconnect_after_restart(true, false, last()), None);
        assert_eq!(reconnect_after_restart(false, true, last()), None);
        assert_eq!(reconnect_after_restart(false, false, None), None);
    }

    #[test]
    fn a_scan_saves_the_other_side_of_a_saved_device() {
        let dir = std::env::temp_dir().join(format!("bluecaster-twins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(crate::paths::devices_dir(&dir)).unwrap();
        crate::devices::save(&dir, "AA:BB:CC:DD:EE:01", "Buds").unwrap();
        let found = |address: &str, le_audio| ScannedDevice {
            address: address.into(),
            name: String::new(),
            rssi: -50,
            class_of_device: 0,
            audio: true,
            le_audio,
            le_audio_set: false,
            hid_le: None,
        };
        let list =
            [found("AA:BB:CC:DD:EE:01", false), found("aa:bb:cc:dd:ee:01", true), found("AA:BB:CC:DD:EE:02", true)];
        // The LE Audio side of the saved one, under its name; not the stranger
        assert_eq!(save_twins(&dir, &list), 1);
        let le = crate::devices::load_le(&dir);
        assert_eq!(le.len(), 1);
        assert!(le[0].address.eq_ignore_ascii_case("AA:BB:CC:DD:EE:01"));
        assert_eq!(le[0].name, "Buds");
        // Once
        assert_eq!(save_twins(&dir, &list), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn service() -> Service {
        let dir = std::env::temp_dir().join(format!("bluecaster-service-restart-{}", std::process::id()));
        let opts = ServiceOptions {
            config_dir: dir,
            hci_tcp: None,
            capture: bt_host::HciCapture::new(),
            afh: None,
            usb: Default::default(),
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        Service::new(opts, tx)
    }

    #[test]
    fn a_broadcast_set_up_again_goes_on_the_adapter_it_ran_on() {
        const OPENED: &str = r"USB\VID_1234&PID_5678\NOT-THERE";
        const AURACAST: &str = r"USB\VID_1234&PID_5678\NOT-THERE-EITHER";
        let s = service();
        let inner = &s.inner;
        // Nothing chosen, none opened yet: automatic, nothing to wait for
        assert_eq!(inner.broadcast_adapter(), "");
        assert!(inner.wanted_adapter_present());
        assert!(!inner.auracast_adapter_missing());
        let guard = inner.broadcast_wanted();
        // The adapter the stack opened is the one to wait for
        *inner.broadcast_adapter.lock().unwrap() = OPENED.into();
        assert_eq!(inner.broadcast_adapter(), OPENED);
        assert!(!inner.wanted_adapter_present());
        // The Auracast adapter comes first, in developer mode only
        inner.settings.lock().unwrap().auracast_adapter_id = AURACAST.into();
        assert!(!inner.auracast_adapter_missing());
        assert_eq!(inner.broadcast_adapter(), OPENED);
        inner.settings.lock().unwrap().debug_mode = true;
        assert!(inner.auracast_adapter_missing());
        assert_eq!(inner.broadcast_adapter(), AURACAST);
        // The session ended: the next broadcast starts anew
        drop(guard);
        assert_eq!(*inner.broadcast_adapter.lock().unwrap(), "");
    }

    #[test]
    fn the_output_mute_takes_developer_mode_and_spares_the_test_tone() {
        let settings = |debug_mode, volume_mixer, mute_output| crate::settings::Settings {
            debug_mode,
            volume_mixer,
            mute_output,
            ..Default::default()
        };
        assert!(settings(true, true, true).mutes_output());
        assert!(!settings(true, true, false).mutes_output());
        assert!(!settings(false, true, true).mutes_output());
        // The mixer turned off in Developer takes the mute with it
        assert!(!settings(true, false, true).mutes_output());

        let req = StreamRequest { mute_output: true, ..request() };
        let virt = SourceRequest::VirtualDevice { id: "{x}".into(), name: "CABLE".into(), auto_switch: false };
        let app = SourceRequest::Application { exe: "a.exe".into() };
        assert!(req.mutes_output(&SourceRequest::SystemLoopback));
        assert!(req.mutes_output(&virt) && req.mutes_output(&app));
        assert!(!req.mutes_output(&SourceRequest::TestTone));

        let off = StreamRequest { mute_output: false, ..req };
        assert!(!off.mutes_output(&SourceRequest::SystemLoopback) && !off.mutes_output(&virt));
    }

    fn request() -> StreamRequest {
        crate::profiles::Profile {
            device_address: "00:11:22:33:44:55".into(),
            codec: "sbc".into(),
            ..Default::default()
        }
        .to_request(1000)
        .unwrap()
    }

    #[test]
    fn the_logged_request_has_no_address_or_broadcast_code() {
        let mut req = request();
        req.le = Some(bt_host::LeRequest {
            extra_devices: vec![[0x66; 6]],
            broadcast: Some(bt_host::LeBroadcast { code: Some("secret12".into()), ..Default::default() }),
            ..Default::default()
        });
        let logged = format!("{:?}", req.redacted());
        for leak in ["17, 34, 51, 68, 85", "102, 102", "secret12"] {
            assert!(!logged.contains(leak), "{leak} in {logged}");
        }
        assert_eq!(req.redacted().device_name, req.device_name);
    }

    #[test]
    fn mic_processing_applies_now_and_stays_with_the_request() {
        let s = service();
        let p = ipc::MicProcessing { enabled: true, gain_db: 9, suppression: 50, gate_db: -50 };
        // Nothing runs: the next microphone takes it
        s.set_mic_processing(p);
        let got = s.inner.mic_dsp.get();
        assert_eq!((got.gain_db, got.suppression, got.gate_db), (9.0, 0.5, -50.0));
        *s.last_request.lock().unwrap() = Some(request());
        s.set_mic_processing(ipc::MicProcessing::default());
        assert!(!s.inner.mic_dsp.get().is_active());
        s.set_mic_processing(p);
        assert_eq!(s.last_request.lock().unwrap().as_ref().unwrap().mic_processing, p);
    }

    #[test]
    fn bitpools_change_in_place_only_on_a_running_a2dp_stream() {
        let s = service();
        let new = crate::stream::Bitpools { sbc: 30, sbcxq: 0, sbc_min: 20, sbcxq_min: 0 };
        // Nothing runs
        assert!(!s.change_bitpools(new));
        assert_eq!(s.inner.take_bitpool_change(), None);
        s.inner.running.store(true, Ordering::Release);
        *s.inner.state.lock().unwrap() = StreamState::Streaming;
        // No session asked for
        assert!(!s.change_bitpools(new));
        *s.last_request.lock().unwrap() = Some(request());
        assert!(s.change_bitpools(new));
        assert_eq!(s.inner.take_bitpool_change(), Some(new));
        // A Bluetooth restart keeps them
        let last = s.last_request.lock().unwrap().clone().unwrap();
        assert_eq!((last.sbc_bitpool, last.sbc_min_bitpool), (30, 20));
        s.inner.running.store(false, Ordering::Release);
    }

    #[test]
    fn the_unlisted_switch_keeps_a_stream_set_up_the_same_either_way() {
        let s = service();
        s.inner.running.store(true, Ordering::Release);
        *s.inner.state.lock().unwrap() = StreamState::Streaming;
        *s.last_request.lock().unwrap() = Some(request());
        // Not known yet (still setting up): started again
        assert!(!s.change_unlisted(true));
        // Off gives this stream, on another one
        s.inner.set_unlisted_keeps([true, false]);
        assert!(!s.change_unlisted(true));
        assert!(s.change_unlisted(false));
        // The same either way (issue #86: no reconnect), kept for a restart
        s.inner.set_unlisted_keeps([true, true]);
        assert!(s.change_unlisted(true));
        assert!(s.last_request.lock().unwrap().as_ref().unwrap().unlisted_options);
        s.inner.running.store(false, Ordering::Release);
    }

    #[test]
    fn forgetting_a_device_ends_what_would_connect_it_again() {
        let s = service();
        let req = request();
        let address = bt_host::addr_str(&req.device);
        let dir = s.inner.opts.config_dir.clone();
        std::fs::create_dir_all(crate::paths::pairing_dir(&dir)).unwrap();
        let line = format!("{} {} 4\n", address.replace(':', "-"), "00".repeat(16));
        std::fs::write(crate::paths::link_keys_file(&dir), &line).unwrap();
        s.inner.codecs.lock().unwrap().insert(address.clone(), Default::default());
        *s.last_request.lock().unwrap() = Some(req);
        crate::devices::save(&dir, &address, "Classic").unwrap();
        let keys = || bt_host::link_keys::FileLinkKeyStore::open(&crate::paths::link_keys_file(&dir)).entries().len();
        // Another device, or the same address's other kind while its classic
        // entry is saved, changes nothing
        s.forget_device("00:11:22:33:44:66", false);
        s.forget_device(&address, true);
        assert!(s.last_request.lock().unwrap().is_some());
        assert_eq!(keys(), 1);

        s.forget_device(&address, false);
        assert!(s.last_request.lock().unwrap().is_none());
        assert!(s.device_codecs().is_empty());
        assert_eq!(keys(), 0);

        // An LE Audio entry without a saved classic one: the link key from
        // its LE pairing (issue #86) goes with it
        crate::devices::remove(&dir, &address).unwrap();
        std::fs::write(crate::paths::link_keys_file(&dir), line).unwrap();
        s.forget_device(&address, true);
        assert_eq!(keys(), 0);
    }

    #[test]
    fn headset_mode_waits_for_a_short_control_holder_and_follows_off() {
        let s = std::sync::Arc::new(service());
        *s.last_request.lock().unwrap() = Some(StreamRequest { headset_mic: HeadsetMic::On, ..request() });
        // No session: at once
        assert!(!s.set_headset_mic(HeadsetMic::Off, None, None));
        s.inner.running.store(true, Ordering::Release);
        s.inner.notify(StreamState::Streaming, Msg::new("status.connected"));
        s.inner.voice_wanted.store(true, Ordering::Release);
        // Another change holds control for a moment: waited for, not a restart
        let holder = {
            let s = s.clone();
            let (held_tx, held_rx) = std::sync::mpsc::channel();
            let t = std::thread::spawn(move || {
                let _control = s.control.lock().unwrap();
                held_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(100));
            });
            held_rx.recv().unwrap();
            t
        };
        assert!(s.set_headset_mic(HeadsetMic::Off, None, Some(voice_link::VoiceCodec::Msbc)));
        holder.join().unwrap();
        // Off: no longer wanted (the running stack keeps its voice)
        assert!(!s.inner.voice_wanted.load(Ordering::Acquire));
        let codec = Some(voice_link::VoiceCodec::Msbc);
        let change = VoiceChange { mode: HeadsetMic::Off, mic_output: None, voice_codec: codec };
        assert_eq!(s.inner.take_voice_change(), Some(change));
        let last = s.last_request.lock().unwrap().clone().unwrap();
        assert_eq!((last.headset_mic, last.voice_codec), (HeadsetMic::Off, codec));
        // On and Auto need a stack with voice: none runs here
        assert!(!s.set_headset_mic(HeadsetMic::On, Some("{cable}".into()), None));
        assert!(!s.set_headset_mic(HeadsetMic::Auto, Some("{cable}".into()), None));
        assert!(!s.inner.voice_wanted.load(Ordering::Acquire));
    }

    #[test]
    fn a_source_changes_in_place_only_on_a_running_a2dp_stream() {
        let s = service();
        let tone = || SourceRequest::TestTone;
        // No session
        *s.last_request.lock().unwrap() = Some(request());
        assert!(!s.change_source(tone()));
        // A session that connects
        s.inner.running.store(true, Ordering::Release);
        s.inner.notify(StreamState::Connecting, Msg::new("status.initializing"));
        assert!(!s.change_source(tone()));
        // LE Audio
        s.inner.notify(StreamState::Streaming, Msg::new("status.connected"));
        let le = bt_host::LeRequest { low_latency: true, ..bt_host::LeRequest::default() };
        *s.last_request.lock().unwrap() = Some(StreamRequest { le: Some(le), ..request() });
        assert!(!s.change_source(tone()));
        assert_eq!(s.inner.take_source_change(), None);
        // An Auracast broadcast: in place too, but not to an application
        let b = bt_host::LeRequest { broadcast: Some(bt_host::LeBroadcast::default()), ..Default::default() };
        *s.last_request.lock().unwrap() = Some(StreamRequest { le: Some(b), ..request() });
        assert!(!s.change_source(SourceRequest::Application { exe: "a.exe".into() }));
        assert!(s.change_source(tone()));
        assert_eq!(s.inner.take_source_change(), Some(tone()));
        // The broadcast could not capture it and kept the old source: so
        // does the request
        s.inner.keep_source(SourceRequest::SystemLoopback);
        s.follow_kept_source();
        let kept = s.last_request.lock().unwrap().as_ref().map(|r| r.source.clone());
        assert_eq!(kept, Some(SourceRequest::SystemLoopback));
        // A start or stop under way
        *s.last_request.lock().unwrap() = Some(request());
        let control = s.control.lock().unwrap();
        assert!(!s.change_source(tone()));
        drop(control);
        // Streaming: the session takes it, and a Bluetooth restart keeps it
        assert!(s.change_source(tone()));
        assert_eq!(s.inner.take_source_change(), Some(tone()));
        assert_eq!(s.last_request.lock().unwrap().as_ref().unwrap().source, tone());
    }

    #[test]
    fn a_bluetooth_restart_follows_disconnect_and_turned_off_headphones() {
        let s = service();
        // Nothing ran yet
        assert_eq!(s.restart_adapter(), None);
        // A session that ended on its own (an error): connected again, also
        // after a second restart that found nothing running
        *s.last_request.lock().unwrap() = Some(request());
        assert_eq!(s.restart_adapter(), Some(request()));
        assert_eq!(s.restart_adapter(), Some(request()));
        // Its headphones were turned off and did not come back
        s.inner.waiting_for_device();
        s.inner.done_waiting_for_device(false);
        assert_eq!(s.restart_adapter(), None);
        // ... or did
        s.inner.waiting_for_device();
        s.inner.done_waiting_for_device(true);
        assert_eq!(s.restart_adapter(), Some(request()));
        // Disconnect
        s.disconnect();
        assert_eq!(s.restart_adapter(), None);
    }

    #[test]
    fn only_the_latest_start_or_stop_asked_for_runs() {
        let s = service();
        // Two codec switches in quick succession, then a Disconnect: each
        // thread runs late, the first two find a newer one asked for
        let (first, second) = (s.ticket(), s.ticket());
        s.start_streaming_for(request(), first);
        assert!(s.worker.lock().unwrap().is_none());
        *s.last_request.lock().unwrap() = Some(request());
        let stop = s.ticket();
        s.start_streaming_for(request(), second);
        assert!(s.worker.lock().unwrap().is_none());
        // The Disconnect older than a later start is skipped too
        let start = s.ticket();
        s.disconnect_for(stop);
        assert_eq!(s.restart_adapter(), Some(request()));
        let _ = start;
        // The latest runs
        let stop = s.ticket();
        s.disconnect_for(stop);
        assert_eq!(s.restart_adapter(), None);
        assert_eq!(s.state(), StreamState::Idle);
    }

    #[test]
    fn sleep_marks_suspending_until_resume() {
        let p = PowerState::default();
        assert!(!p.suspending() && !p.stale());
        assert!(p.suspend());
        // A repeated notification
        assert!(!p.suspend());
        assert!(p.suspending());
        assert!(!p.stale());
        assert!(p.resumed_at().is_none());
        assert!(p.resume());
        assert!(!p.suspending());
        assert!(p.stale());
        assert!(p.resumed_at().is_some());
    }

    #[test]
    fn the_second_resume_notification_is_ignored() {
        let p = PowerState::default();
        p.suspend();
        assert!(p.resume());
        let at = p.resumed_at();
        assert!(!p.resume());
        assert_eq!(p.resumed_at(), at);
        assert!(p.stale() && !p.suspending());
    }

    #[test]
    fn a_resume_without_suspend_still_marks_the_stack_stale() {
        let p = PowerState::default();
        assert!(p.resume());
        assert!(p.stale() && !p.suspending());
    }

    #[test]
    fn a_new_sleep_counts_while_the_stack_is_still_stale() {
        // Slept and woke, nothing used the stack since, then slept again
        let p = PowerState::default();
        p.suspend();
        p.resume();
        let first = p.resumed_at().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(p.suspend());
        assert!(p.suspending() && p.stale());
        assert!(p.resume());
        assert!(p.resumed_at().unwrap() > first);
    }

    #[test]
    fn a_classic_devices_le_side_without_le_audio_is_told() {
        let known = [
            ("00:A2:D0:00:00:50".to_string(), "EarFun Air Pro 4".to_string()),
            ("00:A2:D0:00:00:60".to_string(), String::new()),
        ];
        let identity = bt_host::LeAddr { addr: bt_host::parse_addr("00:A2:D0:00:00:60").unwrap(), addr_type: 0 };
        let results = [
            // Its own address, its name under an RPA, an RPA resolving to it
            LeScanResult { le_audio: false, ..le("00:A2:D0:00:00:50", "", None) },
            LeScanResult { le_audio: false, ..le("5A:01:02:03:04:05", "EarFun Air Pro 4", None) },
            LeScanResult { le_audio: false, identity: Some(identity), ..le("4B:01:02:03:04:05", "", None) },
            // LE Audio advertisers are listed anyway; others are not its
            le("00:A2:D0:00:00:50", "EarFun Air Pro 4", None),
            LeScanResult { le_audio: false, ..le("00:A2:D0:00:00:70", "Mouse", None) },
            LeScanResult { le_audio: false, ..le("00:A2:D0:00:00:71", "", None) },
        ];
        let sides: Vec<String> = classic_le_sides(&results, &known).iter().map(|r| r.addr.to_string()).collect();
        assert_eq!(sides, ["00:A2:D0:00:00:50", "5A:01:02:03:04:05", "4B:01:02:03:04:05"]);
    }

    fn le(addr: &str, name: &str, set: Option<u32>) -> LeScanResult {
        LeScanResult {
            addr: bt_host::LeAddr { addr: bt_host::parse_addr(addr).unwrap(), addr_type: 0 },
            name: name.into(),
            le_audio: true,
            rsi: set.map(|_| [1, 2, 3, 4, 5, 6]),
            set_id: set,
            ..LeScanResult::default()
        }
    }

    #[test]
    fn keyboards_and_mice_are_listed_apart_and_never_saved_as_audio() {
        let hid = |addr: &str, uuids: Vec<u16>, appearance: u16, connectable| LeScanResult {
            uuids16: uuids,
            appearance,
            connectable,
            le_audio: false,
            ..le(addr, "Keys", None)
        };
        let rpa = bt_host::LeAddr { addr: [0x5A, 1, 2, 3, 4, 5], addr_type: 1 };
        let identity = bt_host::LeAddr { addr: [0xC0, 1, 2, 3, 4, 5], addr_type: 1 };
        let results = [
            hid("00:A2:D0:00:00:05", vec![0x1812], 0, true),
            // A mouse by its appearance only (0x03C2)
            hid("00:A2:D0:00:00:06", vec![], 0x03C2, true),
            // Not connectable, or neither: not listed
            hid("00:A2:D0:00:00:07", vec![0x1812], 0x03C1, false),
            hid("00:A2:D0:00:00:08", vec![0x180F], 0x0941, true),
            // A bonded one under an RPA: by its identity
            LeScanResult { addr: rpa, identity: Some(identity), ..hid("00:00:00:00:00:00", vec![0x1812], 0, true) },
        ];
        let list = hid_le_entries(&results);
        let listed: Vec<(&str, Option<u8>, bool, bool)> =
            list.iter().map(|e| (e.address.as_str(), e.hid_le, e.audio, e.le_audio)).collect();
        assert_eq!(
            listed,
            [
                ("00:A2:D0:00:00:05", Some(0), false, false),
                ("00:A2:D0:00:00:06", Some(0), false, false),
                ("C0:01:02:03:04:05", Some(1), false, false)
            ]
        );
        // An LE Audio device saved under the same address: the keyboard
        // entry is not taken as its classic side
        let dir = std::env::temp_dir().join(format!("bluecaster-hid-twins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(crate::paths::devices_dir(&dir)).unwrap();
        crate::devices::save_le(&dir, "00:A2:D0:00:00:05", "Buds").unwrap();
        assert_eq!(save_twins(&dir, &list), 0);
        assert!(crate::devices::load(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn le_scan_lists_one_member_per_new_set() {
        let results = [
            le("00:A2:D0:00:00:12", "Buds L", Some(0)),
            le("00:A2:D0:00:00:13", "Buds R", Some(0)),
            le("00:A2:D0:00:00:20", "Speaker", None),
            LeScanResult { le_audio: false, ..le("00:A2:D0:00:00:30", "Mouse", None) },
            le("00:A2:D0:00:00:20", "Speaker 2", None),
            le("00:A2:D0:00:00:40", "Other L", Some(1)),
        ];
        let (list, found) = le_scan_entries(&results, &[], &[], &no_sets());
        let got: Vec<_> = list.iter().map(|d| (d.address.as_str(), d.name.as_str(), d.le_audio_set)).collect();
        assert_eq!(
            got,
            [
                ("00:A2:D0:00:00:12", "Buds L", true),
                ("00:A2:D0:00:00:20", "Speaker 2", false),
                ("00:A2:D0:00:00:40", "Other L", true)
            ]
        );
        assert_eq!(found, 5);
        assert!(list.iter().all(|d| d.le_audio && d.audio && d.class_of_device == 0));
    }

    fn no_sets() -> crate::le_sets::KnownSets {
        crate::le_sets::KnownSets::default()
    }

    /// An advertiser of a set no known SIRK resolves (never bonded)
    fn unbonded(addr: &str, name: &str) -> LeScanResult {
        LeScanResult { rsi: Some([9, 9, 9, 9, 9, 9]), appearance: 0x0941, ..le(addr, name, None) }
    }

    #[test]
    fn le_scan_lists_a_saved_set_once() {
        // The right earbud is saved: the left one is the same device
        let saved = [SavedDevice { address: "00:A2:D0:00:00:13".into(), name: "R".into(), le_audio: true }];
        let results = [le("00:A2:D0:00:00:12", "Buds L", Some(0)), le("00:A2:D0:00:00:13", "Buds R", Some(0))];
        let (list, _) = le_scan_entries(&results, &saved, &[], &no_sets());
        let got: Vec<_> = list.iter().map(|d| d.address.as_str()).collect();
        assert_eq!(got, ["00:A2:D0:00:00:13"]);
        // A third member too
        let results = [
            le("00:A2:D0:00:00:12", "Buds L", Some(0)),
            le("00:A2:D0:00:00:14", "Buds C", Some(0)),
            le("00:A2:D0:00:00:13", "Buds R", Some(0)),
        ];
        let (list, _) = le_scan_entries(&results, &saved, &[], &no_sets());
        let got: Vec<_> = list.iter().map(|d| d.address.as_str()).collect();
        assert_eq!(got, ["00:A2:D0:00:00:13"]);
    }

    #[test]
    fn le_scan_lists_a_learned_sets_member_as_the_saved_device() {
        let saved = [SavedDevice { address: "00:A2:D0:00:00:13".into(), name: "Buds".into(), le_audio: true }];
        let mut sets = no_sets();
        sets.add(["00:A2:D0:00:00:12".to_string(), "00:A2:D0:00:00:13".into()], None);
        // Only the other earbud in range, its RSI not resolved: still the
        // saved device, found nearby
        let (list, _) = le_scan_entries(&[unbonded("00:A2:D0:00:00:12", "Buds")], &saved, &[], &sets);
        let got: Vec<_> = list.iter().map(|d| (d.address.as_str(), d.le_audio_set)).collect();
        assert_eq!(got, [("00:A2:D0:00:00:13", true)]);
        // A member the stack resolves (set 3) under an address not learned
        sets.add(["00:A2:D0:00:00:13".to_string()], Some(3));
        let (list, _) = le_scan_entries(&[le("00:A2:D0:00:00:77", "Buds", Some(3))], &saved, &[], &sets);
        let got: Vec<_> = list.iter().map(|d| d.address.as_str()).collect();
        assert_eq!(got, ["00:A2:D0:00:00:13"]);
        // Nothing of the set saved: listed as found
        let (list, _) = le_scan_entries(&[unbonded("00:A2:D0:00:00:12", "Buds")], &[], &[], &sets);
        assert_eq!(list[0].address, "00:A2:D0:00:00:12");
    }

    #[test]
    fn a_never_bonded_pair_is_listed_once() {
        let results = [
            unbonded("00:A2:D0:00:00:22", "Buds"),
            le("00:A2:D0:00:00:30", "Speaker", None),
            unbonded("00:A2:D0:00:00:21", "Buds"),
        ];
        let (list, found) = le_scan_entries(&results, &[], &[], &no_sets());
        let got: Vec<_> = list.iter().map(|d| (d.address.as_str(), d.le_audio_set)).collect();
        assert_eq!(got, [("00:A2:D0:00:00:30", false), ("00:A2:D0:00:00:21", true)]);
        assert_eq!(found, 3);
        // One of them saved (added before connecting): the other is it
        let saved = [SavedDevice { address: "00:A2:D0:00:00:22".into(), name: "Buds".into(), le_audio: true }];
        let (list, _) = le_scan_entries(&results, &saved, &[], &no_sets());
        let got: Vec<_> = list.iter().map(|d| d.address.as_str()).collect();
        assert_eq!(got, ["00:A2:D0:00:00:22", "00:A2:D0:00:00:30"]);
    }

    #[test]
    fn unbonded_advertisers_merge_only_when_nothing_else_fits() {
        let listed = |results: &[LeScanResult]| le_scan_entries(results, &[], &[], &no_sets()).0.len();
        // Other products of one series
        assert_eq!(listed(&[unbonded("00:A2:D0:00:00:21", "Buds NC"), unbonded("00:A2:D0:00:00:22", "Buds SO")]), 2);
        // Another appearance
        let speaker = LeScanResult { appearance: 0x0841, ..unbonded("00:A2:D0:00:00:22", "Buds") };
        assert_eq!(listed(&[unbonded("00:A2:D0:00:00:21", "Buds"), speaker]), 2);
        // No name: nothing to compare
        assert_eq!(listed(&[unbonded("00:A2:D0:00:00:21", ""), unbonded("00:A2:D0:00:00:22", "")]), 2);
        // No RSI: two devices of one model, not a set
        assert_eq!(listed(&[le("00:A2:D0:00:00:21", "Buds", None), le("00:A2:D0:00:00:22", "Buds", None)]), 2);
        // Two pairs of one model (or a pair and a lone earbud): which two
        // belong together is not known, all listed
        let four: Vec<_> = (1..=4).map(|i| unbonded(&format!("00:A2:D0:00:00:2{i}"), "Buds")).collect();
        assert_eq!(listed(&four), 4);
        assert_eq!(listed(&four[..3]), 3);
        // A pair already known (resolved) beside a new one of the model:
        // each listed once
        let results = [
            le("00:A2:D0:00:00:11", "Buds", Some(0)),
            le("00:A2:D0:00:00:12", "Buds", Some(0)),
            unbonded("00:A2:D0:00:00:21", "Buds"),
            unbonded("00:A2:D0:00:00:22", "Buds"),
        ];
        assert_eq!(listed(&results), 2);
    }

    #[test]
    fn le_scan_leaves_out_this_pc_and_other_sources() {
        let appearing = |addr, name, appearance| LeScanResult { appearance, ..le(addr, name, None) };
        let results = [
            le("00:A2:D0:00:00:50", "THIS-PC", None),
            appearing("00:A2:D0:00:00:51", "Other PC", 0x0080),
            appearing("00:A2:D0:00:00:52", "Laptop", 0x0083),
            appearing("00:A2:D0:00:00:53", "Phone", 0x0040),
            appearing("00:A2:D0:00:00:54", "Earbuds", 0x0941),
            le("00:A2:D0:00:00:55", "Speaker", None),
        ];
        let (list, found) = le_scan_entries(&results, &[], &["00:a2:d0:00:00:50".into()], &no_sets());
        let got: Vec<_> = list.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(got, ["Earbuds", "Speaker"]);
        assert_eq!(found, 2);
    }

    #[test]
    fn a_classic_headsets_appearance_only_le_side_is_not_le_audio() {
        let guess = |addr, name| LeScanResult { appearance: 0x0942, appearance_only: true, ..le(addr, name, None) };
        let results = [
            guess("00:A2:D0:00:00:60", "HDR 275"),
            guess("00:A2:D0:00:00:61", "LE-only buds"),
            le("00:A2:D0:00:00:62", "Dual-mode buds", None),
        ];
        let classic = ["00:a2:d0:00:00:60".to_string(), "00:A2:D0:00:00:62".to_string()];
        assert_eq!(classic_appearance_only(&results, &classic), ["00:A2:D0:00:00:60"]);
    }

    #[test]
    fn a_dual_mode_device_keeps_its_classic_entry() {
        let classic = DiscoveredDevice {
            addr: bt_host::parse_addr("00:A2:D0:00:00:12").unwrap(),
            name: "Buds".into(),
            class_of_device: 0x240404,
            rssi: -40,
        };
        let c = ScannedDevice::classic(&classic);
        assert!(!c.le_audio && c.audio);
        let (le_list, _) = le_scan_entries(&[le("00:A2:D0:00:00:12", "Buds", None)], &[], &[], &no_sets());
        assert_eq!(le_list[0].address, c.address);
        assert!(le_list[0].le_audio);
    }

    #[test]
    fn an_unnamed_le_entry_takes_its_classic_name() {
        let (mut list, _) = le_scan_entries(
            &[le("00:A2:D0:00:00:12", "", None), le("00:A2:D0:00:00:13", "", None)],
            &[],
            &[],
            &no_sets(),
        );
        let known = vec![("00:a2:d0:00:00:12".to_string(), "Buds".to_string())];
        fill_le_names(&mut list, &known);
        assert_eq!(list[0].name, "Buds");
        // No device with its address: still unnamed (the UI shows the address)
        assert_eq!(list[1].name, "");
    }

    #[test]
    fn a_bonded_device_under_random_addresses_is_listed_by_its_identity() {
        let identity = bt_host::LeAddr { addr: bt_host::parse_addr("00:A2:D0:00:00:12").unwrap(), addr_type: 0 };
        let rpa = |a| LeScanResult { identity: Some(identity), ..le(a, "", None) };
        // Two random addresses of the same earbuds, one entry
        let (mut list, found) =
            le_scan_entries(&[rpa("4B:01:02:03:04:05"), rpa("5C:01:02:03:04:05")], &[], &[], &no_sets());
        assert_eq!(found, 2);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].address, "00:A2:D0:00:00:12");
        // It takes the classic side's name
        fill_le_names(&mut list, &[("00:a2:d0:00:00:12".to_string(), "Buds".to_string())]);
        assert_eq!(list[0].name, "Buds");
        // Saved as LE Audio: it is the saved entry, not a new set member
        let saved = [SavedDevice { address: "00:A2:D0:00:00:12".into(), name: "Buds".into(), le_audio: true }];
        let (list, _) = le_scan_entries(&[rpa("4B:01:02:03:04:05")], &saved, &[], &no_sets());
        assert_eq!(list[0].address, saved[0].address);
    }

    #[test]
    fn le_config_from_the_setting_or_the_command_line() {
        let opts = ServiceOptions {
            config_dir: PathBuf::from("cfg"),
            hci_tcp: None,
            capture: HciCapture::new(),
            afh: None,
            usb: UsbSelector::default(),
        };
        assert!(le_config(false, None, &opts).is_none());
        let cli = LeCliOptions { mcp_dry_run: true, ccp_sim: Some("1:incoming x".into()), ccp_console: true };
        let c = le_config(true, Some(&cli), &opts).unwrap();
        assert_eq!(c.bond_dir.as_deref(), Some(std::path::Path::new(r"cfg\pairing")));
        assert_eq!(c.bonds_file.as_deref(), Some(std::path::Path::new(r"cfg\pairing\le_audio_bonds.tlv")));
        assert!(c.media.is_none());
        assert_eq!(c.ccp_sim.as_deref(), Some("1:incoming x"));
        assert!(c.ccp_console);
        // The emulator transport implies the MCP dry run
        let tcp = ServiceOptions { hci_tcp: Some("127.0.0.1:9510".into()), ..opts };
        let c = le_config(true, Some(&LeCliOptions::default()), &tcp).unwrap();
        assert!(c.media.is_none() && c.ccp_sim.is_none() && !c.ccp_console);
    }

    #[test]
    fn a_new_stack_clears_stale_but_not_suspending() {
        let p = PowerState::default();
        p.suspend();
        p.resume();
        p.clear_stale();
        assert!(!p.stale());
        p.suspend();
        p.clear_stale();
        assert!(p.suspending());
        p.end_suspend();
        assert!(!p.suspending() && !p.stale());
    }
}
