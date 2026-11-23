//! The LE core on the BTstack thread (v1 le_audio::Core, le_audio_core.cpp):
//! stack init / deinit of the LE part, LE scan (extended scan fallback),
//! connect with RPA / identity merge, SM LE Secure Connections Just Works
//! bonding (a bond the device lost is deleted and pairing repeated once),
//! bonds in le_audio_bonds.tlv (LeConfig::bonds_file), GATT discovery (included /
//! secondary services too), PACS / ASCS reads, refusing a stream without
//! ISO buffers ("adapter does not support LE Audio (no ISO buffers)"), the
//! BAP stream, and the module dispatch.
//!
//! [`LeCtx`] is what module hooks and GATT / timer callbacks get: the core's
//! BTstack-thread state for one call. Every entry point (an HCI / SM / GATT
//! / ATT event, a timer, a job of the worker API) runs through
//! [`LeCore::run`], which afterwards starts the queued GATT operations, runs
//! the callbacks of finished ones, hands notifications to att_server and
//! arms the LE timer.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use btstack_sys::consts::*;
use btstack_sys::{AttWriteRequest, CigParams, ConnectionType, Remote, Stack, events};
use le_audio::defs::{
    AD_APPEARANCE, AD_NAME_COMPLETE, AD_NAME_SHORT, AD_RSI, AD_SERVICE_DATA_UUID16, AD_UUID16_COMPLETE,
    AD_UUID16_INCOMPLETE, UUID_SERVICE_ASCS, UUID_SERVICE_CAS, UUID_SERVICE_GMAS, UUID_SERVICE_PACS, UUID_SERVICE_TMAS,
};
use le_audio::pacs_ascs::{Ase, PacRecord};
use le_audio::plan::{IsoLimits, PlanDevice, StreamPlan, plan_stream};
use le_audio::presets::PresetTable;
use le_audio::sdu_queue::SduQueue;

use super::bap::{self, Unicast, UnicastEnv, UnicastTimer};
use super::broadcast::Broadcast;
use super::gatt_client::{GattChar, GattClient, GattService, PROPERTY_INDICATE};
use super::gatt_server::{AttRead, AttWrite, GattServer, WriteAction};
use super::iso_recv::{IsoRx, LeRxStats, RxSdu};
use super::iso_send::IsoSender;
use super::module::{LeGroupHooks, LeModule, LeModuleSetup};
use super::modules::tmap_gmap::TmapGmapModule;
use super::{LeAddr, LeConfig, LeDeviceId, LeDeviceInfo, LeScanResult, LeStreamRequest, LeStreamStats, LeStreamStatus};
use crate::Controls;
use crate::app::{HostApp, Job};
use crate::hid::SharedHid;

/// hci_con_handle_t of no connection
pub const CON_HANDLE_INVALID: u16 = 0xFFFF;

/// The background connection to keyboards and mice listens 15 ms of every
/// 160 ms (0.625 ms units): the radio is shared with an audio stream, and a
/// device that wakes up advertises often. LE Audio's own connects listen
/// half the time, as before.
const BACKGROUND_SCAN_INTERVAL: u16 = 0x0100;
const BACKGROUND_SCAN_WINDOW: u16 = 0x0018;
const CONNECT_SCAN_INTERVAL: u16 = 0x0060;
const CONNECT_SCAN_WINDOW: u16 = 0x0030;
/// The passive scan for bonded keyboards and mice that advertise with a
/// resolvable private address ([`LeCtx::hid_watch_seen`]): as often as the
/// background connection listens
const HID_WATCH_SCAN_INTERVAL: u16 = BACKGROUND_SCAN_INTERVAL;
const HID_WATCH_SCAN_WINDOW: u16 = BACKGROUND_SCAN_WINDOW;
/// A connect to a keyboard or mouse found by its resolvable private
/// address is given up after this long (it advertised just now)
const HID_DIRECT_TIMEOUT_MS: u32 = 5000;
/// SMP Pairing Failed reasons of a device that wants a passkey or more
/// than Just Works gives: Passkey Entry Failed, Authentication
/// Requirements, Pairing Not Supported
const SMP_NEEDS_MORE_THAN_JUST_WORKS: [u8; 3] = [0x01, 0x03, 0x05];

/// Addresses resolved / looked up are used for connecting at most this long
/// after they were seen (RPAs change every ~15 min); at most this many kept
const RPA_FRESH_MS: u32 = 5 * 60 * 1000;
const RPA_CACHE_MAX: usize = 512;

/// The quiet scan of [`BtHost::le_find_any_quietly`](crate::BtHost): a
/// 20 ms window every 200 ms (0.625 ms units), the rest of the radio time
/// left to a running stream
pub const QUIET_SCAN_INTERVAL: u16 = 0x0140;
pub const QUIET_SCAN_WINDOW: u16 = 0x0020;

/// What a failed re-encryption with the stored LE keys leads to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReencryptionFailure {
    /// The device has no key for us (PIN or Key Missing: the LL answer of a
    /// peripheral whose host has no LTK, and BTstack's report of a Pairing
    /// Request from a device we hold an LTK for): forget the bond, pair anew
    KeyMissing,
    /// The device refused (Authentication Failure: an SMP Pairing Failed
    /// during the re-encryption, reason unknown): pair anew on this link,
    /// the stored bond stays until a new one replaces it
    Refused,
    /// The link dropped or timed out, the device ended it, the controller
    /// was busy...: says nothing about the key. The bond stays and the
    /// connect fails (the caller may try again).
    Transient,
}

fn reencryption_failure(status: u8) -> ReencryptionFailure {
    match status {
        ERROR_CODE_PIN_OR_KEY_MISSING => ReencryptionFailure::KeyMissing,
        ERROR_CODE_AUTHENTICATION_FAILURE => ReencryptionFailure::Refused,
        _ => ReencryptionFailure::Transient,
    }
}

/// The address to connect a device with. An RPA that resolves to the
/// device is used only when the latest scan saw it (age at most
/// `scan_age`, and not older than RPA_FRESH_MS): an RPA from before cannot
/// be told from one the device stopped using (a device that restarts
/// advertising often takes a new one). `rpas`: (RPA, ms since seen) of the
/// RPAs resolved to the device with a bond held now. The device's address
/// (`device`: its identity once SM told it) seen after the freshest RPA
/// wins (the device switched back to it). Else the address given, with the
/// device's address type when it is the device's address.
fn pick_connect_address(
    given: LeAddr,
    device: LeAddr,
    rpas: &[([u8; 6], u32)],
    identity_age: Option<u32>,
    scan_age: Option<u32>,
) -> LeAddr {
    let best = rpas
        .iter()
        .filter(|(_, age)| *age <= RPA_FRESH_MS && scan_age.is_some_and(|s| *age <= s))
        .min_by_key(|(_, age)| *age);
    match best {
        // The device advertises with its identity again (seen after the
        // RPA): an RPA it no longer uses gets no answer
        Some((_, age)) if identity_age.is_some_and(|i| i < *age) => device,
        Some((rpa, _)) => LeAddr { addr: *rpa, addr_type: LeAddr::RANDOM },
        None if given.addr == device.addr => device,
        None => given,
    }
}

/// A device's identity address (upper case) and type, once SM learned it
/// (bonded): what its kept services are filed under
fn d_identity(ctx: &LeCtx<'_>, id: LeDeviceId) -> Option<(String, u8)> {
    let d = ctx.device(id)?;
    d.identity_known.then(|| (crate::events::addr_str(&d.addr.addr).to_uppercase(), d.addr.addr_type))
}

/// One LE Audio peer (e.g. a headset, or one earbud of a pair), owned by
/// the core, only touched on the BTstack thread (v1 LeAudioDevice). Profile
/// modules read it and keep their own state keyed by `id`.
#[derive(Debug, Clone)]
pub struct LeDevice {
    pub id: LeDeviceId,
    /// The identity address (public or static random) once SM learned it
    /// (pairing, or an RPA resolved with the stored IRK); before that the
    /// address it was first connected with. Devices are keyed by it.
    pub addr: LeAddr,
    /// `addr` is the identity from SM
    pub identity_known: bool,
    /// Address of the current / last link (maybe an RPA)
    pub peer_addr: LeAddr,
    pub name: String,
    /// A keyboard or mouse on the HID list (issue #413): only HID over GATT
    /// acts on it, LE Audio's profiles leave it alone, and its address is
    /// not logged
    pub hid: bool,

    // ---- link ----
    /// ACL handle, CON_HANDLE_INVALID when down
    pub acl: u16,
    pub encrypted: bool,
    /// SM pairing in progress
    pub pairing: bool,
    pub pair_attempts: u32,
    /// Security done, discovery started
    pub secured: bool,
    /// GATT cache + PACS + ASCS read
    pub gatt_ready: bool,
    /// `services` came from the kept list without a Database Hash check
    /// (gatt_cache): dropped from it when the setup fails
    pub services_kept: bool,
    /// `le_connect` may return
    pub ready: bool,
    /// A plain link ([`crate::BtHost::le_connect_plain`]): GATT only. No
    /// pairing, no modules, no PACS / ASCS; the GATT cache is all that is
    /// read. For peripherals with their own control channel (QCY earbuds).
    pub plain: bool,
    /// A plain link's GATT cache from an earlier link (same device, same
    /// firmware): used instead of a full discovery when not empty
    pub cached_services: Vec<GattService>,
    /// Why it went down / failed (HCI / ATT / SM)
    pub last_error: u8,

    // ---- GATT cache (filled once per connection) ----
    pub services: Vec<GattService>,

    // ---- PACS (sink direction: we are the audio source) ----
    pub sink_pacs: Vec<PacRecord>,
    pub sink_locations: u32,
    /// Source direction (the headset microphone)
    pub source_pacs: Vec<PacRecord>,
    pub source_locations: u32,
    pub available_sink_contexts: u16,
    pub available_source_contexts: u16,
    pub supported_sink_contexts: u16,
    pub supported_source_contexts: u16,
    pub has_pacs: bool,
    pub has_ascs: bool,

    // ---- ASCS ----
    pub ases: Vec<Ase>,
    pub ase_cp_handle: u16,
}

impl LeDevice {
    pub(crate) fn new(id: LeDeviceId, addr: LeAddr) -> LeDevice {
        LeDevice {
            id,
            addr,
            identity_known: false,
            peer_addr: addr,
            name: String::new(),
            hid: false,
            acl: CON_HANDLE_INVALID,
            encrypted: false,
            pairing: false,
            pair_attempts: 0,
            secured: false,
            gatt_ready: false,
            services_kept: false,
            ready: false,
            plain: false,
            cached_services: Vec::new(),
            last_error: 0,
            services: Vec::new(),
            sink_pacs: Vec::new(),
            sink_locations: 0,
            source_pacs: Vec::new(),
            source_locations: 0,
            available_sink_contexts: 0,
            available_source_contexts: 0,
            supported_sink_contexts: 0,
            supported_source_contexts: 0,
            has_pacs: false,
            has_ascs: false,
            ases: Vec::new(),
            ase_cp_handle: 0,
        }
    }

    pub fn connected(&self) -> bool {
        self.acl != CON_HANDLE_INVALID
    }

    /// For the log: the address, but not a keyboard's or mouse's
    pub fn shown(&self) -> String {
        if self.hid { "a keyboard or mouse".into() } else { self.addr.to_string() }
    }

    /// The first service with this UUID (primary or secondary)
    pub fn find_service(&self, uuid16: u16) -> Option<&GattService> {
        self.services.iter().find(|s| s.uuid16 == uuid16)
    }

    /// All characteristics with this UUID in all services with
    /// `service_uuid16` (0 = any service)
    pub fn find_chars(&self, service_uuid16: u16, char_uuid16: u16) -> Vec<&GattChar> {
        self.services
            .iter()
            .filter(|s| service_uuid16 == 0 || s.uuid16 == service_uuid16)
            .flat_map(|s| s.chars.iter().filter(|c| c.uuid16 == char_uuid16))
            .collect()
    }

    pub fn find_char(&self, service_uuid16: u16, char_uuid16: u16) -> Option<&GattChar> {
        self.find_chars(service_uuid16, char_uuid16).into_iter().next()
    }

    pub fn ase(&self, ase_id: u8) -> Option<&Ase> {
        self.ases.iter().find(|a| a.id == ase_id)
    }

    #[cfg(test)]
    pub fn ase_mut(&mut self, ase_id: u8) -> Option<&mut Ase> {
        self.ases.iter_mut().find(|a| a.id == ase_id)
    }

    pub fn sink_ase_count(&self) -> usize {
        self.ases.iter().filter(|a| a.sink).count()
    }

    pub fn source_ase_count(&self) -> usize {
        self.ases.iter().filter(|a| !a.sink).count()
    }
}

/// A timer of [`LeCtx::run_after`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeTimerId(pub u32);

/// A timer / posted callback
pub type LeCallback = Box<dyn FnOnce(&mut LeCtx<'_>)>;

/// Completion of a `BtHost::le_call` operation: true = success
pub type LeDone = Box<dyn FnOnce(bool) + Send>;

/// What the worker API reads without a round trip to the BTstack thread
#[derive(Default)]
pub(crate) struct LeShared {
    /// stack_init ran (v1 initialized_)
    pub initialized: AtomicBool,
    /// HCI working (v1 hci_working_)
    pub hci_working: AtomicBool,
    /// The controller has LE (known once HCI works)
    pub le_supported: AtomicBool,
    /// LE Read Buffer Size v2: ISO_Data_Packet_Length, Total_Num_ISO_Data_Packets
    pub iso_len: AtomicU32,
    pub iso_count: AtomicU32,
    /// "Low latency (GMAP)"
    pub low_latency: AtomicBool,
    /// Set once when a device or CIS of the running stream was lost
    pub stream_lost: AtomicBool,
    /// Members of the stream's coordinated sets that were not found or
    /// went down: the stream plays on the others (CSIP prepare_group)
    pub set_members_missing: AtomicU32,
    /// How long CSIP prepare_group looks for the members of a set not
    /// connected (None: its own time; zero: not at all)
    pub set_member_wait: Mutex<Option<std::time::Duration>>,
    /// Devices of the running stream that lost their link or CIS while
    /// the others play on
    pub stream_dropped: Mutex<Vec<LeDeviceId>>,
    /// Counters of the running (or last) stream, published by the sender
    pub stats: Arc<Mutex<LeStreamStats>>,
    /// The modules' worker / any-thread parts (registry order)
    pub hooks: Mutex<Vec<Arc<dyn LeGroupHooks>>>,
}

impl LeShared {
    pub fn iso_buffers(&self) -> (u16, u8) {
        (self.iso_len.load(Ordering::Acquire) as u16, self.iso_count.load(Ordering::Acquire) as u8)
    }

    pub fn hooks(&self) -> Vec<Arc<dyn LeGroupHooks>> {
        self.hooks.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// RPA resolution (BTstack SM, IRKs of le_device_db)
#[derive(Debug, Clone, Copy, Default)]
struct RpaEntry {
    done: bool,
    ok: bool,
    identity: LeAddr,
    /// Last advertising report (run loop ms), None: never seen
    seen_ms: Option<u32>,
}

/// The result of a connection attempt for the worker (the id moves when
/// on_identity merges the link into a known device)
#[derive(Debug, Default)]
pub(crate) struct ConnectResult {
    pub id: Option<u32>,
    pub error: String,
}

/// The gap_connect in progress
struct ConnectAttempt {
    device: LeDeviceId,
    /// The address gap_connect was given
    target: LeAddr,
    result: Arc<Mutex<ConnectResult>>,
    done: LeDone,
    error: String,
}

struct Timer {
    id: u32,
    due: u32,
    cb: LeCallback,
}

/// The LE core's BTstack-thread state (devices, scan results, RPA cache,
/// GATT client and server, presets, unicast, ISO sender, modules, timers).
/// Lives in HostApp while LE Audio is on.
pub struct LeCore {
    pub(crate) shared: Arc<LeShared>,
    controls: Arc<Controls>,
    poster: LePoster,
    events: std::sync::mpsc::Sender<crate::HostEvent>,
    battery: bool,
    /// The GAP Device Name (HostConfig::local_name)
    local_name: String,
    /// LE Audio is on: its profile modules, services and logs. False: the
    /// core runs only for keyboards and mice.
    audio: bool,
    /// Keyboards and mice (issue #413): the list and what classic and LE
    /// share; None: no HID
    hid: Option<SharedHid>,
    /// The LE keyboards and mice the controller connects to in the
    /// background ([`LeCtx::refresh_background`]); empty: not running
    background: Vec<LeAddr>,
    /// LE keyboards and mice that refused pairing: left out of the
    /// background connection until they are connected again on purpose
    hid_blocked: Vec<[u8; 6]>,
    /// The passive scan for bonded keyboards and mice with resolvable
    /// private addresses runs (or would, beside a scan of LeCore's own)
    hid_watch: bool,
    /// The connect to a keyboard or mouse found by its resolvable private
    /// address: the address connected to, and the timer that gives up
    hid_direct: Option<(LeAddr, LeTimerId)>,
    /// Taken at stack init (the modules get it)
    config: Option<LeConfig>,
    initialized: bool,
    /// The modules in registry order (None while one of its hooks runs)
    modules: Vec<Option<Box<dyn LeModule>>>,
    devices: Vec<LeDevice>,
    scan_results: Vec<LeScanResult>,
    scanning: bool,
    /// When the latest scan started (run loop ms): only RPAs seen since
    /// are connected with
    scan_started_ms: Option<u32>,
    /// Counts the LE Audio advertisers a scan listed (HostApp wakes the
    /// classic scan's waiters on a change: the list shows them at once)
    scan_found: u64,
    connect: Option<ConnectAttempt>,
    /// By RPA
    rpa_cache: HashMap<[u8; 6], RpaEntry>,
    /// Identity (non-RPA) addresses by when they were last seen advertising
    /// (run loop ms): a device that switched back from an RPA to its
    /// identity is connected with the identity
    identity_seen: HashMap<[u8; 6], u32>,
    pub(crate) gatt: GattClient,
    server: GattServer,
    presets: PresetTable,
    /// None while one of its calls runs, and after stack deinit
    unicast: Option<Unicast>,
    unicast_timers: [Option<LeTimerId>; UnicastTimer::ALL],
    pub(crate) iso: IsoSender,
    pub(crate) queue: Option<SharedSduQueue>,
    pub(crate) status: Option<LeStreamStatus>,
    /// The broadcast (Auracast source) while it runs or stops
    pub(crate) broadcast: Option<Broadcast>,
    timers: Vec<Timer>,
    next_timer: u32,
    timers_changed: bool,
    gatt_retry: Option<LeTimerId>,
    /// `<bond_dir>\le_gatt_cache.txt` (crate::le::gatt_cache); None: no
    /// bond folder, every connection discovers
    gatt_cache_file: Option<PathBuf>,
    /// Sink PAC records per characteristic (con, value handle): a device may
    /// have several
    pub(crate) sink_pacs_by_char: BTreeMap<(u16, u16), Vec<PacRecord>>,
    /// Source PAC records per characteristic, as `sink_pacs_by_char`
    pub(crate) source_pacs_by_char: BTreeMap<(u16, u16), Vec<PacRecord>>,
    /// The microphone: the ring's producer from le_start_stream until the
    /// stream receives, then the receiver
    pub(crate) rx_producer: Option<rtrb::Producer<RxSdu>>,
    pub(crate) rx_stats: Arc<LeRxStats>,
    pub(crate) rx: Option<IsoRx>,
}

/// The LE core for one hook or callback (BTstack thread)
pub struct LeCtx<'a> {
    pub(crate) stack: &'a Stack,
    pub(crate) core: &'a mut LeCore,
}

impl<'a> LeCtx<'a> {
    pub fn stack(&self) -> &Stack {
        self.stack
    }

    pub fn device(&self, id: LeDeviceId) -> Option<&LeDevice> {
        self.core.devices.get(id.0 as usize)
    }

    pub fn device_mut(&mut self, id: LeDeviceId) -> Option<&mut LeDevice> {
        self.core.devices.get_mut(id.0 as usize)
    }

    /// The device with this ACL handle (connected)
    pub fn device_by_handle(&self, acl: u16) -> Option<&LeDevice> {
        if acl == CON_HANDLE_INVALID {
            return None;
        }
        self.core.devices.iter().find(|d| d.acl == acl)
    }

    /// All devices the core knows (connected or not)
    pub fn device_ids(&self) -> Vec<LeDeviceId> {
        self.core.devices.iter().map(|d| d.id).collect()
    }

    /// The GATT client queue (never call BTstack's gatt_client directly)
    pub fn gatt(&mut self) -> &mut GattClient {
        &mut self.core.gatt
    }

    /// The PC's GATT server at run time (notify / indicate / CCC state)
    pub fn gatt_server(&mut self) -> &mut GattServer {
        &mut self.core.server
    }

    /// The stream presets of this session (v1 add_stream_preset /
    /// find_stream_preset)
    pub fn presets(&self) -> &PresetTable {
        &self.core.presets
    }

    /// ISO buffers of the controller (LE Read Buffer Size v2):
    /// (ISO_Data_Packet_Length, Total_Num_ISO_Data_Packets), (0, 0) without
    /// ISO support
    pub fn iso_buffers(&self) -> (u16, u8) {
        self.core.shared.iso_buffers()
    }

    /// The running stream, None when idle
    pub fn stream_status(&self) -> Option<LeStreamStatus> {
        self.core.status.clone()
    }

    /// The "media keys" setting (MCP refuses commands while it is off, as
    /// AVRCP buttons are ignored then)
    pub fn media_keys(&self) -> bool {
        self.core.controls.media_keys.load(Ordering::Acquire)
    }

    /// Another module (or this one, from a GATT / timer callback); None
    /// while its own hook runs or when it is not registered
    pub fn module_mut<T: LeModule>(&mut self) -> Option<&mut T> {
        self.core.modules.iter_mut().flatten().find_map(|m| {
            let any: &mut dyn Any = m.as_mut();
            any.downcast_mut::<T>()
        })
    }

    /// Runs `f` on the BTstack thread after `ms` (v1 btstack timers of the
    /// modules); dropped when the session ends
    pub fn run_after(&mut self, ms: u32, f: LeCallback) -> LeTimerId {
        let id = self.core.next_timer;
        self.core.next_timer = self.core.next_timer.wrapping_add(1);
        let due = self.stack.time_ms().wrapping_add(ms);
        self.core.timers.push(Timer { id, due, cb: f });
        self.core.timers_changed = true;
        LeTimerId(id)
    }

    pub fn cancel_timer(&mut self, timer: LeTimerId) {
        let before = self.core.timers.len();
        self.core.timers.retain(|t| t.id != timer.0);
        if self.core.timers.len() != before {
            self.core.timers_changed = true;
        }
    }

    // ======================================================================
    // Crate-internal helpers
    // ======================================================================

    /// The device if it is still on the link `con` (for callbacks)
    pub(crate) fn device_on(&self, id: LeDeviceId, con: u16) -> Option<&LeDevice> {
        self.device(id).filter(|d| d.acl == con && con != CON_HANDLE_INVALID)
    }

    pub(crate) fn device_on_mut(&mut self, id: LeDeviceId, con: u16) -> Option<&mut LeDevice> {
        self.device_mut(id).filter(|d| d.acl == con && con != CON_HANDLE_INVALID)
    }

    /// Calls `f` on every module in registry order (each taken out of the
    /// registry while its hook runs)
    pub(crate) fn each_module(&mut self, mut f: impl FnMut(&mut dyn LeModule, &mut LeCtx<'_>)) {
        for i in 0..self.core.modules.len() {
            let Some(mut m) = self.core.modules[i].take() else { continue };
            f(m.as_mut(), self);
            self.core.modules[i] = Some(m);
        }
    }

    /// [`LeCtx::each_module`] for a device's hooks: a keyboard or mouse goes
    /// to the HID module(s) only, every other device to the others
    pub(crate) fn each_module_for(&mut self, dev: LeDeviceId, mut f: impl FnMut(&mut dyn LeModule, &mut LeCtx<'_>)) {
        let hid = self.device(dev).is_some_and(|d| d.hid);
        self.each_module(|m, ctx| {
            if m.for_hid() == hid {
                f(m, ctx)
            }
        });
    }

    fn with_module<R>(&mut self, i: usize, f: impl FnOnce(&mut dyn LeModule, &mut LeCtx<'_>) -> R) -> Option<R> {
        let mut m = self.core.modules.get_mut(i)?.take()?;
        let r = f(m.as_mut(), self);
        self.core.modules[i] = Some(m);
        Some(r)
    }

    /// Calls `f` with the unicast state machine (taken out of the core while
    /// it runs; nothing happens when it is busy already or gone)
    pub(crate) fn with_unicast(&mut self, f: impl FnOnce(&mut Unicast, &mut LeCtx<'_>)) {
        let Some(mut u) = self.core.unicast.take() else { return };
        f(&mut u, self);
        if self.core.unicast.is_none() && self.core.initialized {
            self.core.unicast = Some(u);
        }
    }

    /// After an entry point: start queued GATT operations, run finished
    /// callbacks (which may queue more), send notifications, arm the timer
    fn settle(&mut self) {
        loop {
            self.core.gatt.pump_all(self.stack);
            let ready = self.core.gatt.take_ready();
            if self.core.server.has_outbox() {
                for (con, handle, value, indication) in self.core.server.take_outbox() {
                    self.stack.att_server_send(con, handle, &value, indication);
                }
            }
            if ready.is_empty() {
                break;
            }
            for c in ready {
                c.run(self);
            }
        }
        if self.core.gatt.wants_retry() && self.core.gatt_retry.is_none() {
            // gatt_client was busy (e.g. with the MTU exchange): try shortly
            let id = self.run_after(5, Box::new(|ctx| ctx.core.gatt_retry = None));
            self.core.gatt_retry = Some(id);
        }
        if self.core.timers_changed {
            self.core.timers_changed = false;
            match self.core.timers.iter().map(|t| t.due.wrapping_sub(self.stack.time_ms()) as i32).min() {
                Some(wait) => {
                    self.stack.timer_start(TIMER_LE, wait.max(0) as u32);
                }
                None => self.stack.timer_stop(TIMER_LE),
            }
        }
    }

    // ======================================================================
    // Addresses
    // ======================================================================

    fn lookup_rpa(&mut self, addr: LeAddr, seen: bool) {
        if !addr.is_rpa() {
            if seen {
                if self.core.identity_seen.len() >= RPA_CACHE_MAX && !self.core.identity_seen.contains_key(&addr.addr) {
                    self.core.identity_seen.clear();
                }
                let now = self.stack.time_ms();
                self.core.identity_seen.insert(addr.addr, now);
            }
            return;
        }
        if !self.core.rpa_cache.contains_key(&addr.addr) {
            if self.core.rpa_cache.len() >= RPA_CACHE_MAX {
                // Forget the addresses that resolved to nothing
                self.core.rpa_cache.retain(|_, e| !(e.done && !e.ok));
                if self.core.rpa_cache.len() >= RPA_CACHE_MAX {
                    return;
                }
            }
            let mut e = RpaEntry::default();
            // SM checks it against the IRKs of all bonds (software AES); the
            // answer is SM_EVENT_IDENTITY_RESOLVING_SUCCEEDED / _FAILED
            let status = self.stack.sm_address_resolution_lookup(BD_ADDR_TYPE_LE_RANDOM, &addr.addr);
            tracing::trace!("LE Audio: RPA {addr} resolution lookup ({status})");
            if status != 0 && status != BTSTACK_BUSY as i32 {
                e.done = true;
            }
            self.core.rpa_cache.insert(addr.addr, e);
        }
        if seen {
            let now = self.stack.time_ms();
            if let Some(e) = self.core.rpa_cache.get_mut(&addr.addr) {
                e.seen_ms = Some(now);
            }
        }
    }

    fn resolved_identity(&self, addr: &[u8; 6]) -> Option<LeAddr> {
        self.core.rpa_cache.get(addr).filter(|e| e.ok).map(|e| e.identity)
    }

    /// The RPA the device advertised in the latest scan (find just before):
    /// a saved RPA or the identity is not what it answers to now
    /// (pick_connect_address)
    fn connect_address(&self, dev: LeDeviceId, given: LeAddr) -> LeAddr {
        let Some(d) = self.device(dev) else { return given };
        let now = self.stack.time_ms();
        // rpa_cache holds resolutions with the bonds held now (delete_bond
        // drops those of a bond it deletes)
        let rpas: Vec<([u8; 6], u32)> = self
            .core
            .rpa_cache
            .iter()
            .filter_map(|(rpa, e)| {
                let seen = e.seen_ms?;
                (e.ok && e.identity.addr == d.addr.addr).then_some((*rpa, now.wrapping_sub(seen)))
            })
            .collect();
        let identity_age = self.core.identity_seen.get(&d.addr.addr).map(|&t| now.wrapping_sub(t));
        let scan_age = self.core.scan_started_ms.map(|t| now.wrapping_sub(t));
        let target = pick_connect_address(given, d.addr, &rpas, identity_age, scan_age);
        tracing::debug!(
            "LE Audio: connect address for device {dev:?}: {} resolved RPA(s) seen {:?} ms ago, identity seen {identity_age:?} ms ago, scan started {scan_age:?} ms ago: {}",
            rpas.len(),
            rpas.iter().map(|(_, age)| *age).collect::<Vec<_>>(),
            if target == given {
                "the address given"
            } else if target.addr == given.addr {
                "the address given, with the device's address type"
            } else if target.is_rpa() {
                "an RPA of the latest scan"
            } else {
                "the device's address"
            }
        );
        target
    }

    /// Deletes the LE bond of `identity` (as SM reported it) and the RPAs
    /// resolved with its IRK: they no longer stand for the device
    fn delete_bond(&mut self, identity: LeAddr) {
        self.stack.le_delete_bonding(identity.addr_type, &identity.addr);
        self.forget_identity(&identity.addr);
    }

    /// The RPAs resolved with the IRK of `identity`, whose bond is gone
    fn forget_identity(&mut self, identity: &[u8; 6]) {
        self.core.rpa_cache.retain(|_, e| !(e.ok && e.identity.addr == *identity));
        for r in self.core.scan_results.iter_mut().filter(|r| r.identity.is_some_and(|i| i.addr == *identity)) {
            r.identity = None;
        }
    }

    /// The device of an address (an RPA SM resolved stands for its
    /// identity), created if asked
    pub(crate) fn device_for_addr(&mut self, addr: LeAddr, create: bool) -> Option<LeDeviceId> {
        let resolved = self.resolved_identity(&addr.addr);
        let key = resolved.unwrap_or(addr);
        if let Some(d) = self.core.devices.iter_mut().find(|d| d.addr.addr == key.addr || d.peer_addr.addr == addr.addr)
        {
            if !d.identity_known && resolved.is_none() {
                d.addr.addr_type = addr.addr_type;
            }
            return Some(d.id);
        }
        if !create {
            return None;
        }
        let id = LeDeviceId(self.core.devices.len() as u32);
        if self.hid_listed(&key.addr) || self.hid_listed(&addr.addr) {
            tracing::debug!("LE Audio: new device {} (a keyboard or mouse)", id.0);
        } else {
            tracing::debug!(
                "LE Audio: new device {} for {addr} (identity {:?})",
                id.0,
                resolved.map(|r| r.to_string())
            );
        }
        let mut d = LeDevice::new(id, key);
        d.identity_known = resolved.is_some();
        d.peer_addr = addr;
        self.core.devices.push(d);
        Some(id)
    }

    /// SM told the identity of the device on a link
    fn on_identity(&mut self, dev: LeDeviceId, identity: LeAddr) {
        let Some(d) = self.device(dev) else { return };
        if d.identity_known && d.addr == identity {
            return;
        }
        // A keyboard or mouse listed with a private address: listed with
        // its identity from now on (found again when its address changes)
        if d.hid
            && let Some(hid) = self.core.hid.clone()
        {
            let listed = [d.addr.addr, d.peer_addr.addr].into_iter().find_map(|a| hid.borrow().le_listed(&a));
            if let Some(listed) = listed.filter(|l| l.addr != identity.addr) {
                hid.borrow_mut().on_identity(listed, identity);
                for a in self.core.hid_blocked.iter_mut().filter(|a| **a == listed.addr) {
                    *a = identity.addr;
                }
            }
        }
        let Some(d) = self.device(dev) else { return };
        let other = self.core.devices.iter().find(|o| o.id != dev && o.addr.addr == identity.addr).map(|o| o.id);
        let other_connected = other.and_then(|o| self.device(o)).is_some_and(|o| o.connected());
        let Some(other) = other.filter(|_| !other_connected && !d.secured) else {
            let Some(d) = self.device_mut(dev) else { return };
            if d.addr != identity && !d.hid {
                tracing::info!(
                    "LE Audio: {} has the identity address {identity} (type {})",
                    d.addr,
                    identity.addr_type
                );
            }
            d.addr = identity;
            d.identity_known = true;
            return;
        };
        // A device known under its identity came back with an address that
        // was not resolved yet: the link belongs to the known device (its id,
        // which CSIP and the worker hold, survives). Nothing but on_connected
        // has run for the link yet (security is not done).
        let from = d.clone();
        if from.hid {
            tracing::info!("LE Audio: a keyboard or mouse is device {other}: link moved to it");
        } else {
            tracing::info!("LE Audio: {} is {identity} (device {}): link moved to it", from.peer_addr, other);
        }
        if let Some(o) = self.device_mut(other) {
            o.addr = identity;
            o.identity_known = true;
            o.peer_addr = from.peer_addr;
            o.hid |= from.hid;
            o.acl = from.acl;
            o.encrypted = from.encrypted;
            o.pairing = from.pairing;
            o.pair_attempts = from.pair_attempts;
            o.secured = false;
            o.gatt_ready = false;
            o.ready = false;
            o.last_error = 0;
            o.services.clear();
            if o.name.is_empty() {
                o.name = from.name.clone();
            }
        }
        if let Some(d) = self.device_mut(dev) {
            d.acl = CON_HANDLE_INVALID;
            d.encrypted = false;
            d.pairing = false;
            d.secured = false;
            d.gatt_ready = false;
            d.ready = false;
        }
        if let Some(c) = self.core.connect.as_mut()
            && c.device == dev
        {
            c.device = other;
            c.result.lock().unwrap_or_else(|e| e.into_inner()).id = Some(other.0);
        }
        self.each_module_for(other, |m, ctx| m.on_connected(ctx, other));
    }

    // ======================================================================
    // HCI events
    // ======================================================================

    pub(crate) fn on_hci(&mut self, p: &[u8]) {
        let Some(&ev) = p.first() else { return };
        match ev {
            BTSTACK_EVENT_STATE => {
                if p.get(2) == Some(&2) {
                    self.core.shared.hci_working.store(true, Ordering::Release);
                    let le = self.stack.le_supported();
                    self.core.shared.le_supported.store(le, Ordering::Release);
                    if self.core.audio {
                        let (len, count) = self.iso_buffers();
                        tracing::info!("LE Audio: ready (controller ISO buffers: {count} x {len} bytes)");
                        if count == 0 || len == 0 {
                            tracing::warn!(
                                "LE Audio: the controller reports no ISO buffers: it does not support LE Audio streams \
                                 (CIS); streaming will be refused"
                            );
                        }
                    } else if le {
                        tracing::info!(
                            "LE: ready for keyboards and mice ({})",
                            if self.stack.controller_address_resolution() == Some(true) {
                                "the controller resolves private addresses, the PC does too"
                            } else {
                                "the PC resolves private addresses"
                            }
                        );
                    }
                    if self.core.hid.is_some() && !le {
                        tracing::warn!("LE: the adapter has no Bluetooth LE: LE keyboards and mice cannot connect");
                    }
                    self.refresh_background();
                } else {
                    self.core.shared.hci_working.store(false, Ordering::Release);
                    self.core.background.clear();
                    self.core.hid_watch = false;
                    self.core.hid_direct = None;
                }
            }
            HCI_EVENT_COMMAND_COMPLETE => {
                if let Some(b) = events::le_read_buffer_size_v2(p) {
                    tracing::debug!(
                        "LE Audio: LE Read Buffer Size v2 status 0x{:02X}: ISO {} x {} bytes",
                        b.status,
                        b.iso_packets,
                        b.iso_packet_length
                    );
                    if b.status == 0 {
                        self.core.shared.iso_len.store(b.iso_packet_length as u32, Ordering::Release);
                        self.core.shared.iso_count.store(b.iso_packets as u32, Ordering::Release);
                    }
                } else if events::command_complete_opcode(p) == Some(HCI_OPCODE_LE_REMOVE_CIG) {
                    tracing::debug!("LE Audio: LE Remove CIG complete: {:02X?}", p.get(5..));
                    self.with_unicast(|u, env| u.on_cig_removed(env));
                }
            }
            GAP_EVENT_ADVERTISING_REPORT | GAP_EVENT_EXTENDED_ADVERTISING_REPORT => {
                if let Some(r) = events::adv_report(p) {
                    self.on_adv_report(&r);
                }
            }
            HCI_EVENT_META_GAP => {
                if let Some(c) = events::le_connection_complete(p) {
                    // 0x02 / 0x03: an identity the controller resolved
                    // (its resolving list, keyboards and mice only)
                    let addr = LeAddr { addr: c.peer_addr, addr_type: c.peer_addr_type & 0x01 };
                    self.on_le_connected(c.status, c.handle, addr, c.role);
                } else if events::big_created(p).is_some() || events::big_terminated(p).is_some() {
                    self.on_big_event(p);
                } else if events::cig_created(p).is_some() {
                    self.with_unicast(|u, env| u.on_cig_created(env, p));
                } else if events::cis_created(p).is_some() {
                    self.with_unicast(|u, env| u.on_cis_created(env, p));
                } else if let Some(b) = events::bonding_deleted(p) {
                    tracing::debug!("LE Audio: bonding deleted (le_device_db index {})", b.index);
                    self.core.server.on_bonding_deleted(self.stack, b.index as usize);
                    // A bond that made room for a new one (the warning is
                    // logged there): as if the user had forgotten it
                    for e in self.stack.take_evicted_le_bonds() {
                        self.forget_identity(&e.addr);
                        super::modules::csip::forget_stored(self, e.addr);
                    }
                }
            }
            HCI_EVENT_ENCRYPTION_CHANGE | HCI_EVENT_ENCRYPTION_CHANGE_V2 => {
                if let Some(e) = events::encryption_change(p) {
                    self.on_encryption(e.handle, e.status, e.enabled);
                    if e.status == 0
                        && e.enabled
                        && self.stack.connection_type(e.handle) == ConnectionType::Le
                        && let Some(index) = self.stack.sm_le_device_index(e.handle)
                    {
                        self.core.server.on_encrypted(self.stack, e.handle, index);
                    }
                }
            }
            HCI_EVENT_DISCONNECTION_COMPLETE => {
                if let Some((_, handle, reason)) = events::disconnection_complete(p) {
                    tracing::debug!(
                        "LE Audio: disconnection complete 0x{handle:04X} (reason 0x{reason:02X}), LE device {:?}",
                        self.device_by_handle(handle).map(|d| d.id)
                    );
                    self.on_disconnected(handle, reason);
                }
            }
            _ => {}
        }
    }

    // ======================================================================
    // Scanning
    // ======================================================================

    pub(crate) fn start_scan(&mut self) {
        self.start_scan_with(0x0030, 0x0030);
    }

    /// Active scan with `interval` / `window` (0.625 ms units): a window
    /// shorter than the interval leaves the radio to a BR/EDR stream or a
    /// classic inquiry running beside it
    pub(crate) fn start_scan_with(&mut self, interval: u16, window: u16) {
        tracing::debug!("LE Audio: scan start (active, interval 0x{interval:04X}, window 0x{window:04X})");
        self.core.scan_results.clear();
        self.core.scanning = true;
        self.core.scan_started_ms = Some(self.stack.time_ms());
        self.stack.le_set_scan_params(true, interval, window, 0);
        self.stack.le_start_scan();
    }

    /// The running scan's advertisers so far
    pub(crate) fn scan_results(&self) -> Vec<LeScanResult> {
        self.core.scan_results.clone()
    }

    pub(crate) fn stop_scan(&mut self) -> Vec<LeScanResult> {
        self.stack.le_stop_scan();
        tracing::debug!(
            "LE Audio: scan stop, {} advertiser(s), {} LE Audio",
            self.core.scan_results.len(),
            self.core.scan_results.iter().filter(|r| r.le_audio).count()
        );
        self.core.scanning = false;
        if self.core.hid_watch {
            // The scan for keyboards and mice goes on (passive)
            self.stack.le_set_scan_params(false, HID_WATCH_SCAN_INTERVAL, HID_WATCH_SCAN_WINDOW, 0);
            self.stack.le_start_scan();
        }
        self.core.scan_results.clone()
    }

    fn on_adv_report(&mut self, r: &events::AdvReport<'_>) {
        // 0x02 / 0x03: an identity the controller resolved (also in legacy
        // reports, which BTstack makes of extended ones with legacy PDUs)
        let addr = LeAddr { addr: r.addr, addr_type: r.addr_type & 0x01 };
        if self.core.hid_watch {
            self.hid_watch_seen(addr);
        }
        if !self.core.scanning {
            return;
        }
        let (idx, fresh) = match self.core.scan_results.iter().position(|e| e.addr == addr) {
            Some(i) => (i, false),
            None => {
                self.core.scan_results.push(LeScanResult { addr, connectable: true, ..LeScanResult::default() });
                (self.core.scan_results.len() - 1, true)
            }
        };
        self.lookup_rpa(addr, true);
        let identity = self.resolved_identity(&addr.addr);
        let mut res = std::mem::take(&mut self.core.scan_results[idx]);
        if res.identity.is_none() {
            res.identity = identity;
        }
        take_report(&mut res, r);
        res.rssi = r.rssi;
        let was_le_audio = res.le_audio;
        let (adv, rsp) = (res.adv.clone(), res.scan_rsp.clone());
        parse_ad(&mut res, &adv);
        parse_ad(&mut res, &rsp);
        self.each_module(|m, ctx| m.on_scan_result(ctx, &mut res, r.data));
        if (fresh || !was_le_audio) && res.le_audio {
            self.core.scan_found += 1;
            tracing::info!(
                "LE Audio: found {} ({}, type {}) RSSI {}{}{}{}",
                if res.name.is_empty() { "?" } else { res.name.as_str() },
                res.addr,
                res.addr.addr_type,
                res.rssi,
                res.identity.map(|i| format!(", identity {i}")).unwrap_or_default(),
                if res.rsi.is_some() { ", set member (RSI)" } else { "" },
                if res.note.is_empty() { String::new() } else { format!(", {}", res.note) }
            );
        }
        if let Some(slot) = self.core.scan_results.get_mut(idx) {
            *slot = res;
        }
    }

    /// le_find: a saved RPA of a bonded device is looked for by its identity
    pub(crate) fn start_find(&mut self, wants: &[LeAddr], quiet: bool) {
        for &want in wants {
            self.lookup_rpa(want, false);
        }
        if quiet {
            self.start_scan_with(QUIET_SCAN_INTERVAL, QUIET_SCAN_WINDOW);
        } else {
            self.start_scan();
        }
    }

    /// le_find: an advertiser matching `want` (the address itself, one
    /// whose RPA resolves to it, or to the identity `want` resolves to)
    pub(crate) fn find_match(&mut self, want: LeAddr) -> Option<LeScanResult> {
        let want_identity = self.resolved_identity(&want.addr);
        let mut found = None;
        for i in 0..self.core.scan_results.len() {
            if self.core.scan_results[i].identity.is_none() {
                let a = self.core.scan_results[i].addr.addr;
                self.core.scan_results[i].identity = self.resolved_identity(&a);
            }
            let r = &self.core.scan_results[i];
            let matches = r.addr.addr == want.addr
                || r.identity
                    .is_some_and(|id| id.addr == want.addr || want_identity.is_some_and(|w| w.addr == id.addr));
            if matches && (!r.name.is_empty() || !r.scan_rsp.is_empty() || r.le_audio) {
                if r.addr.addr != want.addr {
                    tracing::info!(
                        "LE Audio: {want} advertises as {} now (identity {})",
                        r.addr,
                        r.identity.map(|i| i.to_string()).unwrap_or_default()
                    );
                }
                found = Some(r.clone());
            }
        }
        found
    }

    // ======================================================================
    // Connection
    // ======================================================================

    /// le_connect's work: `done(true)` once the device is ready
    pub(crate) fn start_connect(
        &mut self,
        addr: LeAddr,
        name: String,
        result: Arc<Mutex<ConnectResult>>,
        done: LeDone,
    ) {
        self.start_connect_as(addr, name, false, result, done);
    }

    /// le_connect_plain's work: the same, for a GATT-only link (no pairing)
    pub(crate) fn start_connect_plain(
        &mut self,
        addr: LeAddr,
        name: String,
        result: Arc<Mutex<ConnectResult>>,
        done: LeDone,
    ) {
        self.start_connect_as(addr, name, true, result, done);
    }

    fn start_connect_as(
        &mut self,
        addr: LeAddr,
        name: String,
        plain: bool,
        result: Arc<Mutex<ConnectResult>>,
        done: LeDone,
    ) {
        tracing::debug!("LE Audio: connect request {addr} (type {}) name “{name}”", addr.addr_type);
        // BTstack has one LE connect at a time: also not while a keyboard or
        // mouse found by its private address is being connected (seconds)
        if self.core.connect.is_some() || self.core.hid_direct.is_some() {
            tracing::debug!("LE Audio: connect refused, another attempt is in progress");
            result.lock().unwrap_or_else(|e| e.into_inner()).error = "another LE connection is being set up".into();
            done(false);
            return;
        }
        let Some(id) = self.device_for_addr(addr, true) else {
            done(false);
            return;
        };
        if !name.is_empty()
            && let Some(d) = self.device_mut(id)
        {
            d.name = name;
        }
        result.lock().unwrap_or_else(|e| e.into_inner()).id = Some(id.0);
        let target = self.connect_address(id, addr);
        let Some(d) = self.device_mut(id) else { return };
        if d.connected() && d.ready {
            tracing::debug!("LE Audio: device {} already connected and ready (0x{:04X})", id.0, d.acl);
            done(true);
            return;
        }
        let in_progress = d.connected();
        tracing::debug!(
            "LE Audio: device {} connect: target {target}, link {} (acl 0x{:04X}, encrypted {}, secured {})",
            id.0,
            if in_progress { "in setup" } else { "down" },
            d.acl,
            d.encrypted,
            d.secured
        );
        if !in_progress {
            d.pair_attempts = 0;
            d.last_error = 0;
            d.plain = plain;
            if target != addr || d.addr != addr {
                tracing::info!(
                    "LE Audio: device {id} ({}): connecting with {target} (address type {})",
                    d.addr,
                    target.addr_type
                );
            }
        }
        self.core.connect =
            Some(ConnectAttempt { device: id, target, result: result.clone(), done, error: String::new() });
        if in_progress {
            // Set up in progress: wait for it
            return;
        }
        if plain {
            // A plain GATT link next to an A2DP stream: a short initiator
            // window, a long connection interval and short connection
            // events, so the LE link takes as little airtime as possible
            let p = PLAIN_LINK_PARAMS;
            self.stack.le_set_connection_params(
                p.scan_interval,
                p.scan_window,
                p.interval_min,
                p.interval_max,
                p.latency,
                p.supervision_timeout,
                p.min_ce,
                p.max_ce,
            );
        }
        // One LE connection attempt at a time: the background connection to
        // keyboards and mice waits until this one is done
        self.stop_background();
        let status = self.stack.le_connect(&target.addr, target.addr_type);
        if plain {
            // gap_connect copied them; LE Audio links keep the defaults
            restore_default_connection_params(self.stack);
        }
        tracing::debug!("LE Audio: gap_connect {target} -> 0x{status:02X}");
        if status != ERROR_CODE_SUCCESS
            && let Some(c) = self.core.connect.take()
        {
            result.lock().unwrap_or_else(|e| e.into_inner()).error =
                format!("LE connect could not start (0x{status:02x})");
            (c.done)(false);
            self.refresh_background();
        }
    }

    /// The worker gave up on a connection attempt (timeout / cancel): cancel
    /// the connect or drop the link. A link that comes up after this is
    /// dropped by on_le_connected.
    pub(crate) fn abandon_connect(&mut self, id: Option<u32>) {
        tracing::debug!(
            "LE Audio: connect abandoned (device {id:?}, link {:?})",
            id.and_then(|i| self.device(LeDeviceId(i))).map(|d| d.acl)
        );
        let Some(id) = id.map(LeDeviceId) else {
            self.stack.le_connect_cancel();
            self.refresh_background();
            return;
        };
        if self.core.connect.as_ref().is_some_and(|c| c.device == id) {
            self.core.connect = None;
        }
        match self.device(id) {
            Some(d) if d.connected() => {
                self.stack.disconnect(d.acl);
            }
            _ => {
                self.stack.le_connect_cancel();
            }
        }
        self.refresh_background();
    }

    fn on_le_connected(&mut self, status: u8, handle: u16, addr: LeAddr, role: u8) {
        let attempt = self.core.connect.as_ref().map(|c| (c.device, c.target));
        let hid = self.hid_listed_link(&addr.addr);
        tracing::debug!(
            "LE Audio: LE connection complete 0x{status:02X}: handle 0x{handle:04X}, {} (type {}), role {role}, attempt {:?}",
            if hid || self.core.hid_direct.is_some() { "a keyboard or mouse".to_string() } else { addr.to_string() },
            addr.addr_type,
            attempt.map(|(d, t)| (d.0, if hid { String::new() } else { t.to_string() }))
        );
        // Any outcome ends BTstack's background connection (it is started
        // again below, for the devices still away)
        self.core.background.clear();
        // The connect to a keyboard or mouse found by its private address:
        // its link (the address connected to, or the identity the
        // controller resolved it to), or its end (LeCore's own connect never
        // runs beside it)
        let direct = self.core.hid_direct.filter(|(t, _)| {
            status != ERROR_CODE_SUCCESS
                || t.addr == addr.addr
                || self.resolved_identity(&t.addr).is_some_and(|i| i.addr == addr.addr)
        });
        if role == 0
            && attempt.is_none()
            && let Some((_, timer)) = direct
        {
            self.core.hid_direct = None;
            self.cancel_timer(timer);
            if status == ERROR_CODE_SUCCESS {
                self.adopt_background(handle, addr, true);
            } else {
                tracing::info!("LE: a keyboard or mouse found by its private address did not connect (0x{status:02x})");
            }
            self.refresh_background();
            return;
        }
        // The controller connected a keyboard or mouse of its accept list
        let background =
            status == ERROR_CODE_SUCCESS && role == 0 && hid && attempt.is_none_or(|(_, t)| t.addr != addr.addr);
        if background {
            self.adopt_background(handle, addr, false);
            self.refresh_background();
            return;
        }
        let stray = match attempt {
            None => true,
            Some((dev, target)) => {
                status == ERROR_CODE_SUCCESS
                    && (self.device(dev).is_some_and(|d| d.connected()) || target.addr != addr.addr)
            }
        };
        if stray {
            // BlueCaster never advertises: this is the connect of an attempt
            // given up (timeout / cancel) that completed before the cancel
            // did. Nobody manages the link: drop it.
            if status == ERROR_CODE_SUCCESS {
                tracing::warn!(
                    "LE Audio: unexpected LE connection 0x{handle:04x} with {addr} (role {role}): disconnecting"
                );
                self.stack.disconnect(handle);
            }
            self.refresh_background();
            return;
        }
        let Some((dev, _)) = attempt else { return };
        if status != ERROR_CODE_SUCCESS {
            let what = if status == ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER {
                "LE connection cancelled"
            } else {
                "LE connection failed"
            };
            self.fail_connect(dev, format!("{what} (0x{status:02x})"));
            return;
        }
        self.link_up(dev, handle, addr);
    }

    /// The link of a device is up: secure it (pairing, or encryption with
    /// the stored keys), then GATT
    fn link_up(&mut self, dev: LeDeviceId, handle: u16, addr: LeAddr) {
        let hid = self.hid_listed(&addr.addr);
        let Some(d) = self.device_mut(dev) else { return };
        d.acl = handle;
        d.peer_addr = addr;
        d.hid |= hid;
        d.encrypted = false;
        d.pairing = false;
        d.secured = false;
        d.gatt_ready = false;
        d.services_kept = false;
        d.ready = false;
        d.services.clear();
        let plain = d.plain;
        if plain {
            tracing::info!("LE: link to {} up (handle 0x{handle:04x}), plain GATT (no pairing)", d.shown());
        } else {
            tracing::info!("LE Audio: LE link to {} up (handle 0x{handle:04x}), securing it", d.shown());
        }
        self.core.gatt.attach(handle);
        if !self.stack.gatt_listen(handle) {
            tracing::warn!("LE Audio: cannot listen for notifications on 0x{handle:04x} (all slots in use)");
        }
        if plain {
            self.plain_discover(dev);
            return;
        }
        self.each_module_for(dev, |m, ctx| m.on_connected(ctx, dev));
        tracing::debug!("LE Audio: requesting security on 0x{handle:04X}");
        self.hid_pairing_io(dev);
        self.stack.sm_request_pairing(handle);
    }

    /// A plain link is up: nothing secures it and no profile reads it, so
    /// discover the GATT cache and finish the connect
    fn plain_discover(&mut self, id: LeDeviceId) {
        let Some(d) = self.device_mut(id) else { return };
        // maybe_secured must not start a profile set-up if the peer encrypts
        d.secured = true;
        let con = d.acl;
        if !d.cached_services.is_empty() {
            d.services = d.cached_services.clone();
            d.gatt_ready = true;
            d.ready = true;
            tracing::info!("LE: {}: GATT handles from the cache, no discovery", d.addr);
            self.finish_connect(id, true);
            return;
        }
        self.core.gatt.discover_all(
            con,
            Box::new(move |ctx, status, services| {
                if ctx.device_on(id, con).is_none() {
                    return;
                }
                if status != 0 {
                    ctx.fail_connect(id, format!("GATT discovery failed (0x{status:02x})"));
                    return;
                }
                let Some(d) = ctx.device_on_mut(id, con) else { return };
                d.services = services;
                d.gatt_ready = true;
                d.ready = true;
                let chars: usize = d.services.iter().map(|s| s.chars.len()).sum();
                tracing::info!("LE: {}: {} services, {chars} characteristics", d.addr, d.services.len());
                ctx.finish_connect(id, true);
            }),
        );
    }

    /// Before a link is asked for security: SM offers a display only for
    /// the keyboard the user is adding, "no input, no output" for every
    /// other device (headphones pair exactly as before)
    fn hid_pairing_io(&self, dev: LeDeviceId) {
        let Some(hid) = self.core.hid.as_ref() else { return };
        let addrs = self.device(dev).map(|d| [d.addr.addr, d.peer_addr.addr]).unwrap_or_default();
        hid.borrow().pairing_io_for(self.stack, &addrs);
    }

    // ======================================================================
    // Keyboards and mice (issue #413): the background connection
    // ======================================================================

    /// An LE keyboard or mouse on the HID list (its address as connected or
    /// as listed)
    pub(crate) fn hid_listed(&self, addr: &[u8; 6]) -> bool {
        self.core.hid.as_ref().is_some_and(|h| h.borrow().le_listed(addr).is_some())
    }

    /// [`LeCtx::hid_listed`], or a resolvable private address SM resolved
    /// to the identity of one on the list
    pub(crate) fn hid_listed_link(&self, addr: &[u8; 6]) -> bool {
        self.hid_listed(addr) || self.resolved_identity(addr).is_some_and(|i| self.hid_listed(&i.addr))
    }

    /// A link the controller made from its accept list (or the connect to
    /// one `found` by its private address): a device of it
    fn adopt_background(&mut self, handle: u16, addr: LeAddr, found: bool) {
        let Some(dev) = self.device_for_addr(addr, true) else {
            self.stack.disconnect(handle);
            return;
        };
        if let Some(d) = self.device_mut(dev) {
            d.hid = true;
            d.pair_attempts = 0;
            d.last_error = 0;
        }
        let n = self.core.devices.iter().filter(|d| d.hid && d.connected()).count() + 1;
        if found {
            tracing::info!("LE: a keyboard or mouse found by its private address connected ({n} connected)");
        } else {
            tracing::info!("LE: a keyboard or mouse connected by itself ({n} connected)");
        }
        self.link_up(dev, handle, addr);
    }

    /// Whether a device on the list has an LE bond with an IRK: one that may
    /// advertise with resolvable private addresses, which only the IRK
    /// tells apart
    fn hid_has_irk(&self, addr: &[u8; 6]) -> bool {
        (0..self.stack.le_device_db_max_count())
            .filter_map(|i| self.stack.le_device_db_info(i))
            .any(|e| e.addr == *addr && e.irk != [0u8; 16])
    }

    /// The passive scan for bonded keyboards and mice with private
    /// addresses: on while one of them is away (the controller's accept list
    /// matches an RPA only when the controller resolves it), off otherwise.
    /// Beside a scan of LeCore's own, that scan's reports serve
    /// ([`LeCtx::stop_scan`] starts this one again).
    fn set_hid_watch(&mut self, on: bool) {
        if on == self.core.hid_watch {
            return;
        }
        self.core.hid_watch = on;
        tracing::debug!("LE: scan for keyboards and mice with private addresses {}", if on { "on" } else { "off" });
        if self.core.scanning {
            return;
        }
        if on {
            self.stack.le_set_scan_params(false, HID_WATCH_SCAN_INTERVAL, HID_WATCH_SCAN_WINDOW, 0);
            self.stack.le_start_scan();
        } else {
            self.stack.le_stop_scan();
        }
    }

    /// An advertiser the HID scan saw: an RPA is looked up with the IRKs of
    /// the bonds (once, [`LeCtx::lookup_rpa`]); one that resolves to a
    /// keyboard or mouse that is away is connected to
    fn hid_watch_seen(&mut self, addr: LeAddr) {
        if !addr.is_rpa() {
            // An identity (or one the controller resolved): the accept list
            // has it
            return;
        }
        self.lookup_rpa(addr, true);
        if let Some(identity) = self.resolved_identity(&addr.addr) {
            self.hid_found(addr, identity);
        }
    }

    /// `rpa` resolved to `identity`: connected to when that is a keyboard or
    /// mouse on the list that is away
    fn hid_found(&mut self, rpa: LeAddr, identity: LeAddr) {
        if !self.core.hid_watch || self.core.hid_direct.is_some() || self.core.connect.is_some() {
            return;
        }
        if !self.hid_away().iter().any(|a| a.addr == identity.addr) {
            return;
        }
        // One LE connect at a time: the accept list pauses
        self.stop_background();
        let status = self.stack.le_connect(&rpa.addr, rpa.addr_type);
        if status != ERROR_CODE_SUCCESS {
            tracing::warn!("LE: a keyboard or mouse found by its private address cannot be connected (0x{status:02x})");
            self.refresh_background();
            return;
        }
        tracing::info!("LE: a keyboard or mouse advertises with a private address, connecting");
        let timer = self.run_after(
            HID_DIRECT_TIMEOUT_MS,
            Box::new(|ctx| {
                if ctx.core.hid_direct.take().is_some() {
                    tracing::info!("LE: a keyboard or mouse found by its private address did not connect in time");
                    ctx.stack.le_connect_cancel();
                    ctx.refresh_background();
                }
            }),
        );
        self.core.hid_direct = Some((rpa, timer));
        self.refresh_background();
    }

    /// The LE keyboards and mice on the list that are not connected, and
    /// not left out for refusing pairing
    fn hid_away(&self) -> Vec<LeAddr> {
        let Some(hid) = self.core.hid.as_ref() else { return Vec::new() };
        let listed = hid.borrow().le.clone();
        listed
            .into_iter()
            .filter(|a| !self.core.hid_blocked.contains(&a.addr))
            .filter(|a| {
                !self
                    .core
                    .devices
                    .iter()
                    .any(|d| d.connected() && (d.addr.addr == a.addr || d.peer_addr.addr == a.addr))
            })
            .collect()
    }

    /// The controller connects by itself to every LE keyboard or mouse on
    /// the list that is away, whenever it advertises: started, changed or
    /// stopped as the list and the links change. Not while LeCore's own
    /// connect runs (one LE connection attempt at a time). A device that
    /// advertises with a resolvable private address matches the accept
    /// list (its identity) only when the controller resolves addresses (its
    /// resolving list, keyboards and mice without LE Audio); for every
    /// other case the HID scan runs beside it ([`LeCtx::set_hid_watch`]).
    pub(crate) fn refresh_background(&mut self) {
        if self.core.hid.is_none() {
            return;
        }
        let ready = self.core.initialized
            && self.core.shared.hci_working.load(Ordering::Acquire)
            && self.core.shared.le_supported.load(Ordering::Acquire);
        let wanted = if ready && self.core.connect.is_none() && self.core.hid_direct.is_none() {
            self.hid_away()
        } else {
            Vec::new()
        };
        let watch = wanted.iter().any(|a| self.hid_has_irk(&a.addr));
        self.set_hid_watch(watch);
        if wanted == self.core.background {
            return;
        }
        if !ready {
            self.core.background.clear();
            return;
        }
        self.stack.le_auto_connect_stop_all();
        if !wanted.is_empty() {
            self.stack.le_connection_scan(BACKGROUND_SCAN_INTERVAL, BACKGROUND_SCAN_WINDOW);
            for a in &wanted {
                let status = self.stack.le_auto_connect_start(&a.addr, a.addr_type);
                if status != ERROR_CODE_SUCCESS {
                    tracing::warn!("LE: a keyboard or mouse cannot be waited for (0x{status:02x})");
                }
            }
        }
        tracing::debug!("LE: waiting in the background for {} keyboard(s) or mice", wanted.len());
        self.core.background = wanted;
    }

    /// Before LeCore's own connect: the background connection pauses (it
    /// restarts when the connect is done)
    fn stop_background(&mut self) {
        if self.core.hid.is_none() {
            return;
        }
        if !self.core.background.is_empty() {
            self.stack.le_auto_connect_stop_all();
            self.core.background.clear();
        }
        self.stack.le_connection_scan(CONNECT_SCAN_INTERVAL, CONNECT_SCAN_WINDOW);
    }

    /// The LE keyboards and mice on the list from now on: the links of the
    /// others are closed (their keys released at once)
    pub(crate) fn hid_set_le(&mut self, devices: Vec<LeAddr>) {
        let Some(hid) = self.core.hid.clone() else { return };
        hid.borrow_mut().le = devices;
        self.core.hid_blocked.retain(|a| hid.borrow().le_listed(a).is_some());
        let gone: Vec<(LeDeviceId, u16)> = self
            .core
            .devices
            .iter()
            .filter(|d| d.hid && d.connected())
            .filter(|d| {
                hid.borrow().le_listed(&d.addr.addr).is_none() && hid.borrow().le_listed(&d.peer_addr.addr).is_none()
            })
            .map(|d| (d.id, d.acl))
            .collect();
        tracing::debug!("hid: {} LE device(s) allowed, closing {} link(s)", hid.borrow().le.len(), gone.len());
        for (id, acl) in gone {
            if let Some(m) = self.module_mut::<super::modules::hogp::HogpModule>() {
                m.release(id);
            }
            self.stack.disconnect(acl);
        }
        for d in self.core.devices.iter_mut().filter(|d| !d.connected()) {
            d.hid = hid.borrow().le_listed(&d.addr.addr).is_some();
        }
        self.refresh_background();
    }

    /// Connect an LE keyboard or mouse on the list: in the background (also
    /// one that refused pairing before)
    pub(crate) fn hid_le_connect(&mut self, addr: LeAddr) -> Result<(), u8> {
        let Some(hid) = self.core.hid.clone() else { return Err(ERROR_CODE_COMMAND_DISALLOWED) };
        if hid.borrow().le_listed(&addr.addr).is_none() {
            return Err(ERROR_CODE_COMMAND_DISALLOWED);
        }
        if !self.core.shared.le_supported.load(Ordering::Acquire) {
            return Err(0x11); // Unsupported Feature or Parameter Value
        }
        self.core.hid_blocked.retain(|a| *a != addr.addr);
        // Started again even when unchanged (BTstack may have ended it)
        self.core.background.clear();
        self.refresh_background();
        Ok(())
    }

    fn fail_connect(&mut self, dev: LeDeviceId, why: String) {
        if self.device(dev).is_some_and(|d| d.services_kept && !d.ready) {
            self.forget_kept_services(dev, "the setup with them failed");
        }
        if let Some(d) = self.device(dev) {
            tracing::warn!("LE Audio: {}: {why}", d.shown());
            if d.connected() {
                self.stack.disconnect(d.acl);
            }
        }
        if let Some(c) = self.core.connect.as_mut() {
            c.error = why;
        }
        self.finish_connect(dev, false);
    }

    fn finish_connect(&mut self, dev: LeDeviceId, ok: bool) {
        if self.core.connect.as_ref().is_none_or(|c| c.device != dev) {
            return;
        }
        let Some(c) = self.core.connect.take() else { return };
        self.refresh_background();
        tracing::debug!("LE Audio: connect of device {} finished: ok {ok} {:?}", dev.0, c.error);
        {
            let mut r = c.result.lock().unwrap_or_else(|e| e.into_inner());
            r.id = Some(dev.0);
            if !ok {
                r.error = c.error;
            }
        }
        (c.done)(ok);
    }

    fn on_encryption(&mut self, handle: u16, status: u8, enabled: bool) {
        let Some(id) = self.device_by_handle(handle).map(|d| d.id) else { return };
        if let Some(d) = self.device_mut(id) {
            d.encrypted = status == ERROR_CODE_SUCCESS && enabled;
            tracing::info!(
                "LE Audio: {} encryption {} (0x{status:02x})",
                d.shown(),
                if d.encrypted { "on" } else { "off" }
            );
        }
        self.maybe_secured(id);
    }

    fn maybe_secured(&mut self, id: LeDeviceId) {
        let Some(d) = self.device_mut(id) else { return };
        if !d.encrypted || d.pairing || d.secured || !d.connected() {
            tracing::debug!(
                "LE Audio: device {} not secured yet (encrypted {}, pairing {}, secured {}, acl 0x{:04X})",
                id.0,
                d.encrypted,
                d.pairing,
                d.secured,
                d.acl
            );
            return;
        }
        d.secured = true;
        let con = d.acl;
        tracing::debug!("LE Audio: device {} secured, GATT discovery on 0x{con:04X}", id.0);
        let hid = d.hid;
        self.each_module_for(id, |m, ctx| m.on_encrypted(ctx, id));
        // Kept services, when the device's Database Hash says they are
        // still its services (crate::le::gatt_cache); not of a keyboard or
        // mouse, whose address would be in that file
        let cache_file = self.core.gatt_cache_file.clone().filter(|_| !hid);
        let cached = cache_file.clone().zip(d_identity(self, id)).and_then(|(file, (addr, kind))| {
            let db = super::gatt_cache::load(&file, &addr)?;
            (db.address_type == kind
                && (db.hash.is_some() || super::gatt_cache::service_changed(&db.services).is_some()))
            .then_some((file, db))
        });
        let hash_handle = cached.as_ref().and_then(|(_, db)| super::gatt_cache::hash_handle(&db.services));
        match (cached, hash_handle) {
            // No usable Database Hash, but Service Changed tells of changes
            (Some((_, db)), _) if db.hash.is_none() => {
                tracing::info!(
                    "LE Audio: {}: kept services used (Service Changed, no Database Hash), discovery skipped",
                    db.address
                );
                if let Some(d) = self.device_on_mut(id, con) {
                    d.services_kept = true;
                }
                // Not stored again: kept as they are until a discovery
                self.services_known(id, con, db.services, None);
            }
            (Some((file, db)), Some(handle)) => {
                tracing::debug!("LE Audio: device {} has kept services, reading its Database Hash", id.0);
                self.core.gatt.read(
                    con,
                    handle,
                    Box::new(move |ctx, status, value| {
                        if ctx.device_on(id, con).is_none() {
                            return;
                        }
                        if status == 0 && db.hash.is_some_and(|h| value == h) {
                            tracing::info!(
                                "LE Audio: {}: services unchanged (Database Hash), discovery skipped",
                                db.address
                            );
                            ctx.services_known(id, con, db.services, None);
                        } else {
                            tracing::info!(
                                "LE Audio: {}: Database Hash {} (0x{status:02x}), discovering the services",
                                db.address,
                                if status == 0 { "changed" } else { "not read" }
                            );
                            ctx.discover(id, con, Some(file));
                        }
                    }),
                );
            }
            _ => {
                self.discover(id, con, cache_file);
            }
        }
    }

    /// Drops a device's kept services (the next connection discovers)
    fn forget_kept_services(&mut self, id: LeDeviceId, why: &str) {
        let (Some(file), Some((addr, _))) = (self.core.gatt_cache_file.clone(), d_identity(self, id)) else {
            return;
        };
        if super::gatt_cache::remove(&file, &addr) {
            tracing::info!("LE Audio: {addr}: kept services dropped ({why})");
        }
    }

    /// Service Changed indications of the device: on every connection, so
    /// that a bonded device tells of a change on a later one too
    fn watch_service_changed(&mut self, id: LeDeviceId, con: u16, services: &[GattService]) {
        let Some(chr) = super::gatt_cache::service_changed(services).cloned() else { return };
        if chr.properties & PROPERTY_INDICATE == 0 {
            return;
        }
        self.core.gatt.listen(
            con,
            chr.value_handle,
            Box::new(move |ctx, value| {
                tracing::info!("LE Audio: device {}: Service Changed {value:02X?}", id.0);
                ctx.forget_kept_services(id, "Service Changed");
                // Set up with handles that may be gone: again, with a
                // discovery
                if ctx.device_on(id, con).is_some_and(|d| d.services_kept && !d.ready) {
                    ctx.fail_connect(id, "its services changed, connect again".into());
                }
            }),
        );
        self.core.gatt.subscribe(con, &chr, true, None);
    }

    /// GATT discovery, then the services kept (`file`) and the setup goes on
    fn discover(&mut self, id: LeDeviceId, con: u16, file: Option<PathBuf>) {
        self.core.gatt.discover_all(
            con,
            Box::new(move |ctx, status, services| {
                if ctx.device_on(id, con).is_none() {
                    return;
                }
                if status != 0 {
                    ctx.fail_connect(id, format!("GATT discovery failed (0x{status:02x})"));
                    return;
                }
                for s in &services {
                    tracing::debug!(
                        "LE Audio: service 0x{:04X} {} 0x{:04X}-0x{:04X}{}: {}",
                        s.uuid16,
                        if s.primary { "primary" } else { "secondary" },
                        s.start_handle,
                        s.end_handle,
                        if s.included_by != 0 {
                            format!(" (included by 0x{:04X})", s.included_by)
                        } else {
                            String::new()
                        },
                        s.chars
                            .iter()
                            .map(|c| format!("0x{:04X}@0x{:04X}", c.uuid16, c.value_handle))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
                ctx.services_known(id, con, services, file);
            }),
        );
    }

    /// The device's services known (discovered, or kept and unchanged):
    /// kept for later connections when `file` is set, at once (issue #86:
    /// a link lost or a stack stopped before the Database Hash came back
    /// lost the list), and again with its Database Hash once that is read,
    /// if it has one; then PACS / ASCS and the modules
    fn services_known(&mut self, id: LeDeviceId, con: u16, services: Vec<GattService>, file: Option<PathBuf>) {
        if let (Some(file), Some((address, address_type))) = (file, d_identity(self, id)) {
            let db = super::gatt_cache::CachedDb { address, address_type, hash: None, services: services.clone() };
            let kept_without = if super::gatt_cache::service_changed(&services).is_some() {
                "kept, Service Changed tells of changes"
            } else {
                "discovered on every connection"
            };
            if !super::gatt_cache::store(&file, db.clone()) && !services.iter().any(|s| s.primary) {
                tracing::info!("LE Audio: device {}: discovery found no service, the kept ones stay", id.0);
            }
            match super::gatt_cache::hash_handle(&services) {
                Some(handle) => self.core.gatt.read(
                    con,
                    handle,
                    Box::new(move |_, status, value| {
                        let hash = <[u8; 16]>::try_from(value).ok().filter(|_| status == 0);
                        match hash {
                            Some(_) => {
                                tracing::info!("LE Audio: {}: services kept with their Database Hash", db.address);
                                super::gatt_cache::store(&file, super::gatt_cache::CachedDb { hash, ..db });
                            }
                            None => tracing::info!(
                                "LE Audio: {}: no usable Database Hash (0x{status:02x}, {} bytes): {kept_without}",
                                db.address,
                                value.len()
                            ),
                        }
                    }),
                ),
                None => {
                    tracing::info!("LE Audio: {}: no Database Hash: {kept_without}", db.address);
                }
            }
        }
        self.watch_service_changed(id, con, &services);
        let audio = self.core.audio;
        let Some(d) = self.device_on_mut(id, con) else { return };
        d.services = services;
        let chars: usize = d.services.iter().map(|s| s.chars.len()).sum();
        tracing::info!("LE Audio: {}: {} services, {chars} characteristics", d.shown(), d.services.len());
        if d.hid || !audio {
            // No PACS / ASCS: a keyboard or mouse goes to HID over GATT
            d.gatt_ready = true;
            self.each_module_for(id, |m, ctx| m.on_gatt_ready(ctx, id));
            let Some(d) = self.device_on_mut(id, con) else { return };
            d.ready = true;
            self.finish_connect(id, true);
            return;
        }
        bap::read_device(
            self,
            id,
            Box::new(move |ctx, _| {
                let Some(d) = ctx.device_on_mut(id, con) else { return };
                tracing::debug!(
                    "LE Audio: device {} GATT ready: pacs {}, ascs {}, {} sink PAC(s), {} ASE(s), locations 0x{:08X}",
                    id.0,
                    d.has_pacs,
                    d.has_ascs,
                    d.sink_pacs.len(),
                    d.ases.len(),
                    d.sink_locations
                );
                d.gatt_ready = true;
                ctx.each_module_for(id, |m, ctx| m.on_gatt_ready(ctx, id));
                let Some(d) = ctx.device_on_mut(id, con) else { return };
                d.ready = true;
                ctx.finish_connect(id, true);
            }),
        );
    }

    fn on_disconnected(&mut self, handle: u16, reason: u8) {
        if let Some(id) = self.device_by_handle(handle).map(|d| d.id) {
            self.core.gatt.detach(handle);
            self.core.server.on_disconnected(handle);
            self.core.sink_pacs_by_char.retain(|(c, _), _| *c != handle);
            self.core.source_pacs_by_char.retain(|(c, _), _| *c != handle);
            if let Some(d) = self.device_mut(id) {
                tracing::info!("LE Audio: {} disconnected (0x{reason:02x})", d.shown());
                d.acl = CON_HANDLE_INVALID;
                d.encrypted = false;
                d.pairing = false;
                d.secured = false;
                d.gatt_ready = false;
                d.ready = false;
                d.last_error = reason;
                for a in d.ases.iter_mut() {
                    a.state = le_audio::defs::AseState::Unknown;
                }
            }
            self.each_module_for(id, |m, ctx| m.on_disconnected(ctx, id, reason));
            if let Some(c) = self.core.connect.as_mut()
                && c.device == id
            {
                if c.error.is_empty() {
                    c.error = format!("the link went down during setup (0x{reason:02x})");
                }
                self.finish_connect(id, false);
            }
            // A keyboard or mouse that went away is waited for again
            self.refresh_background();
        }
        self.with_unicast(|u, env| u.on_link_lost(env, handle));
    }

    /// le_disconnect's work: HCI Disconnect, then wait (polling) until the
    /// link is down
    pub(crate) fn disconnect_device(&mut self, id: LeDeviceId, done: LeDone) {
        let Some(d) = self.device(id).filter(|d| d.connected()) else {
            tracing::debug!("LE Audio: disconnect device {}: not connected", id.0);
            done(true);
            return;
        };
        let status = self.stack.disconnect(d.acl);
        tracing::debug!("LE Audio: disconnect device {} (0x{:04X}) -> 0x{status:02X}", id.0, d.acl);
        if status != ERROR_CODE_SUCCESS {
            done(false);
            return;
        }
        // Disconnection Complete arrives later
        poll_disconnected(self, id, 0, done);
    }

    // ======================================================================
    // SM events
    // ======================================================================

    pub(crate) fn on_sm(&mut self, p: &[u8]) {
        let Some(&ty) = p.first() else { return };
        let Some(handle) = events::sm_handle(p) else { return };
        tracing::debug!("LE Audio: SM event 0x{ty:02X} on 0x{handle:04X}");
        match ty {
            SM_EVENT_IDENTITY_RESOLVING_SUCCEEDED | SM_EVENT_IDENTITY_CREATED => {
                // An address resolved with a bond's IRK (an RPA seen while
                // scanning, or a link), or the identity a new pairing
                // distributed
                let Some(i) = events::sm_identity(p) else { return };
                let a = LeAddr { addr: i.addr, addr_type: i.addr_type };
                let identity = LeAddr { addr: i.identity_addr, addr_type: i.identity_addr_type & 0x01 };
                if ty == SM_EVENT_IDENTITY_CREATED {
                    // A new IRK: addresses that resolved to nothing may
                    // resolve now
                    self.core.rpa_cache.retain(|_, e| !(e.done && !e.ok));
                }
                let hid = self.hid_listed(&identity.addr);
                if a.is_rpa() {
                    let e = self.core.rpa_cache.entry(a.addr).or_default();
                    if !e.ok && !hid {
                        tracing::info!("LE Audio: {a} resolves to {identity}");
                    }
                    e.done = true;
                    e.ok = true;
                    e.identity = identity;
                    for r in self.core.scan_results.iter_mut().filter(|r| r.addr.addr == a.addr) {
                        r.identity = Some(identity);
                    }
                }
                if handle != CON_HANDLE_INVALID
                    && let Some(id) = self.device_by_handle(handle).map(|d| d.id)
                {
                    self.on_identity(id, identity);
                } else if handle == CON_HANDLE_INVALID && hid && a.is_rpa() {
                    // A keyboard or mouse seen by the HID scan
                    self.hid_found(a, identity);
                }
                return;
            }
            SM_EVENT_IDENTITY_RESOLVING_FAILED => {
                if let Some((_, _, a)) = events::sm_identity_resolving_failed(p)
                    && let Some(e) = self.core.rpa_cache.get_mut(&a)
                    && !e.ok
                {
                    e.done = true;
                }
                return;
            }
            _ => {}
        }
        let Some(id) = self.device_by_handle(handle).map(|d| d.id) else { return };
        let addr = self.device(id).map(|d| d.shown()).unwrap_or_default();
        // A plain GATT link (le_connect_plain) never pairs or bonds: a
        // pairing request from the peer is declined, every other SM event is
        // ignored, and nothing below (confirm, retry) runs for it
        if self.device(id).is_some_and(|d| d.plain) {
            match plain_link_sm_action(ty) {
                PlainSm::Decline => {
                    tracing::warn!("LE: {addr}: peer asked to pair on a plain link, declined");
                    self.stack.sm_bonding_decline(handle);
                }
                PlainSm::Ignore => tracing::info!("LE: {addr}: SM event 0x{ty:02x} on a plain link, ignored"),
            }
            return;
        }
        let hid = self.device(id).is_some_and(|d| d.hid);
        match ty {
            SM_EVENT_JUST_WORKS_REQUEST => {
                tracing::info!("LE Audio: {addr}: pairing (Just Works)");
                self.stack.sm_just_works_confirm(handle);
            }
            SM_EVENT_NUMERIC_COMPARISON_REQUEST => {
                tracing::info!("LE Audio: {addr}: pairing (numeric comparison, accepted)");
                self.stack.sm_numeric_comparison_confirm(handle);
            }
            SM_EVENT_PASSKEY_INPUT_NUMBER => {
                // No keyboard: such a device cannot pair with BlueCaster
                tracing::warn!("LE Audio: {addr} asks for a passkey, which BlueCaster cannot enter");
                self.stack.sm_bonding_decline(handle);
            }
            SM_EVENT_PASSKEY_DISPLAY_NUMBER => {
                // Offered only to the keyboard being added (hid_pairing_io):
                // the user types the number on it. Never logged.
                let addrs = self.device(id).map(|d| [d.addr.addr, d.peer_addr.addr]).unwrap_or_default();
                let passkey =
                    p.get(12..16).map(|b| crate::hid::Passkey::new(u32::from_le_bytes([b[0], b[1], b[2], b[3]])));
                let shown = hid
                    && passkey.is_some_and(|k| {
                        self.core.hid.as_ref().is_some_and(|h| h.borrow_mut().passkey_shown(&addrs, k))
                    });
                if !shown {
                    tracing::warn!(
                        "LE: a device wants a number shown, which BlueCaster only does for a keyboard being added"
                    );
                    self.stack.sm_bonding_decline(handle);
                }
            }
            SM_EVENT_PAIRING_STARTED => {
                if let Some(d) = self.device_mut(id) {
                    d.pairing = true;
                }
            }
            SM_EVENT_PAIRING_COMPLETE => {
                let Some(c) = events::sm_pairing_complete(p) else { return };
                // The keyboard being added: its pairing window ends with
                // this pairing (unless it is tried again below)
                let addrs = self.device(id).map(|d| [d.addr.addr, d.peer_addr.addr]).unwrap_or_default();
                let adding = hid && self.core.hid.as_ref().is_some_and(|h| h.borrow().pairing_with(&addrs));
                let end_adding = |me: &mut Self| {
                    if adding && let Some(h) = me.core.hid.clone() {
                        for a in &addrs {
                            h.borrow_mut().pairing_end(me.stack, Some(a), None);
                        }
                    }
                };
                let Some(d) = self.device_mut(id) else { return };
                tracing::debug!(
                    "LE Audio: {addr}: pairing complete status 0x{:02X} reason 0x{:02X} (attempts {})",
                    c.status,
                    c.reason,
                    d.pair_attempts
                );
                d.pairing = false;
                if c.status == ERROR_CODE_SUCCESS {
                    tracing::info!("LE Audio: {addr} paired and bonded");
                    end_adding(self);
                    self.maybe_secured(id);
                } else {
                    d.pair_attempts += 1;
                    if hid && SMP_NEEDS_MORE_THAN_JUST_WORKS.contains(&c.reason) {
                        // A wrong number typed, a device that wants a
                        // passkey while it is not being added (it lost its
                        // bond), or more security than BlueCaster offers:
                        // asking again would not change that. Not waited
                        // for in the background until added or connected
                        // anew.
                        if adding {
                            tracing::warn!("LE: the new keyboard or mouse refused pairing (reason 0x{:02x})", c.reason);
                        } else {
                            tracing::warn!(
                                "LE: a keyboard or mouse refused pairing without a number (reason 0x{:02x}): add it again",
                                c.reason
                            );
                        }
                        let a = d.addr.addr;
                        self.core.hid_blocked.push(a);
                        end_adding(self);
                        self.fail_connect(id, format!("pairing refused (reason 0x{:02x})", c.reason));
                    } else if d.pair_attempts < 2 {
                        tracing::warn!(
                            "LE Audio: {addr}: pairing failed (0x{:02x}, reason 0x{:02x}), trying again",
                            c.status,
                            c.reason
                        );
                        self.hid_pairing_io(id);
                        self.stack.sm_request_pairing(handle);
                    } else {
                        if hid {
                            let a = d.addr.addr;
                            self.core.hid_blocked.push(a);
                        }
                        end_adding(self);
                        self.fail_connect(id, format!("pairing failed (0x{:02x})", c.status));
                    }
                }
            }
            SM_EVENT_REENCRYPTION_COMPLETE => {
                let Some((_, identity_type, identity, status)) = events::sm_reencryption_complete(p) else { return };
                tracing::debug!("LE Audio: {addr}: re-encryption complete 0x{status:02X}");
                if status == ERROR_CODE_SUCCESS {
                    return;
                }
                match reencryption_failure(status) {
                    ReencryptionFailure::KeyMissing => {
                        // The device lost its bond (reset or paired
                        // elsewhere): pair anew
                        tracing::warn!(
                            "LE Audio: the device has no key for the stored bond (0x{status:02x}): pairing again"
                        );
                        self.delete_bond(LeAddr { addr: identity, addr_type: identity_type });
                    }
                    ReencryptionFailure::Refused => {
                        // SM allows pairing after a failed re-encryption; a
                        // pairing that succeeds replaces the stored keys
                        tracing::warn!(
                            "LE Audio: the device refused the stored key (0x{status:02x}): pairing again, the stored bond stays until then"
                        );
                    }
                    ReencryptionFailure::Transient => {
                        tracing::warn!(
                            "LE Audio: encryption with the stored key did not complete (0x{status:02x}): bond kept"
                        );
                        self.fail_connect(id, format!("encryption failed (0x{status:02x})"));
                        return;
                    }
                }
                let Some(d) = self.device_mut(id) else { return };
                d.pair_attempts += 1;
                if d.pair_attempts < 2 && d.connected() {
                    self.stack.sm_request_pairing(handle);
                } else {
                    self.fail_connect(id, format!("encryption failed (0x{status:02x})"));
                }
            }
            _ => {}
        }
    }

    // ======================================================================
    // Streaming
    // ======================================================================

    /// The plan for these devices (connected and ready). One that went
    /// away since the group was put together (an earbud put back in its
    /// case) is left out while another one is ready: the stream plays on
    /// the others, as for a set member not found.
    pub(crate) fn plan_for(&self, ids: &[LeDeviceId], request: &LeStreamRequest) -> Result<StreamPlan, String> {
        let ready: Vec<LeDeviceId> =
            ids.iter().copied().filter(|&id| self.device(id).is_some_and(|d| d.ready)).collect();
        if ready.is_empty() {
            return Err("a device of the group is not connected".into());
        }
        if ready.len() < ids.len() {
            let gone = ids.len() - ready.len();
            tracing::warn!(
                "LE Audio: {gone} device(s) of the group went away before the stream: streaming without them"
            );
            self.core.shared.set_members_missing.fetch_add(gone as u32, Ordering::AcqRel);
        }
        let mut devs = Vec::new();
        for &id in &ready {
            let Some(d) = self.device(id) else { continue };
            devs.push(PlanDevice {
                id: d.id.0,
                name: if d.name.is_empty() { d.addr.to_string() } else { d.name.clone() },
                acl: d.acl,
                sink_pacs: d.sink_pacs.clone(),
                sink_locations: d.sink_locations,
                available_sink_contexts: d.available_sink_contexts,
                source_pacs: d.source_pacs.clone(),
                source_locations: d.source_locations,
                available_source_contexts: d.available_source_contexts,
                ases: d.ases.clone(),
            });
        }
        let (packet_length, packet_count) = self.iso_buffers();
        let plan = plan_stream(&devs, request, self.presets(), IsoLimits { packet_length, packet_count });
        tracing::debug!(
            "LE Audio: plan for devices {:?} ({request:?}, ISO {packet_count} x {packet_length}): {plan:?}",
            ids.iter().map(|i| i.0).collect::<Vec<_>>()
        );
        plan
    }

    /// le_start_stream's BAP part: `done(true)` once audio may flow
    pub(crate) fn start_unicast(
        &mut self,
        plan: StreamPlan,
        queue: SharedSduQueue,
        rx: Option<(rtrb::Producer<RxSdu>, Arc<LeRxStats>)>,
        status: LeStreamStatus,
        why: Arc<Mutex<String>>,
        done: LeDone,
    ) {
        let set_why = |why: &Arc<Mutex<String>>, s: &str| *why.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
        let Some(u) = self.core.unicast.as_ref() else {
            // stack deinit ran
            set_why(&why, "LE Audio stopped");
            done(false);
            return;
        };
        if self.core.broadcast.is_some() {
            set_why(&why, "a broadcast is running");
            done(false);
            return;
        }
        if u.busy() {
            tracing::debug!("LE Audio: stream start refused, unicast busy");
            set_why(&why, "a stream is still being stopped");
            done(false);
            return;
        }
        tracing::debug!("LE Audio: unicast start: {}", plan.describe());
        self.core.queue = Some(queue);
        self.core.status = Some(status);
        self.core.rx = None;
        match rx {
            Some((producer, stats)) => {
                self.core.rx_producer = Some(producer);
                self.core.rx_stats = stats;
            }
            None => self.core.rx_producer = None,
        }
        self.with_unicast(|u, env| {
            u.start(
                env,
                plan,
                Box::new(move |r| match r {
                    Ok(()) => done(true),
                    Err(e) => {
                        set_why(&why, &e);
                        done(false);
                    }
                }),
            )
        });
    }

    /// le_stop_stream's work: stop sending, Disable / Release, remove the
    /// CIG
    pub(crate) fn stop_unicast(&mut self, done: LeDone) {
        tracing::debug!("LE Audio: unicast stop (state machine present {})", self.core.unicast.is_some());
        self.core.iso.stop(self.stack);
        self.core.rx = None;
        self.core.rx_producer = None;
        if self.core.unicast.is_none() {
            done(true);
            return;
        }
        self.with_unicast(|u, env| u.stop(env, Box::new(move || done(true))));
    }

    /// For BtHost::le_devices (v1 Core::devices)
    pub(crate) fn device_infos(&mut self) -> Vec<LeDeviceInfo> {
        let mut list: Vec<LeDeviceInfo> = self
            .core
            .devices
            .iter()
            .filter(|d| !d.hid)
            .map(|d| LeDeviceInfo {
                id: d.id,
                addr: d.addr,
                name: d.name.clone(),
                connected: d.connected(),
                ready: d.ready,
                last_error: d.last_error,
                sink_locations: d.sink_locations,
                sink_ases: d.sink_ase_count(),
                pacs: describe_pacs(&d.sink_pacs),
                source_locations: d.source_locations,
                source_ases: d.source_ase_count(),
                source_pacs: describe_pacs(&d.source_pacs),
                roles: String::new(),
            })
            .collect();
        if let Some(m) = self.module_mut::<TmapGmapModule>() {
            for d in &mut list {
                d.roles = m.info(d.id).map(|i| i.summary()).unwrap_or_default();
            }
        }
        list
    }

    // ======================================================================
    // GATT server (att_server callbacks)
    // ======================================================================

    fn att_read(&mut self, con: u16, handle: u16, offset: u16) -> Result<Vec<u8>, u8> {
        if let Some(v) = self.core.server.read_own(con, handle) {
            return Ok(v);
        }
        let Some(owner) = self.core.server.owner(handle) else { return Ok(Vec::new()) };
        let r = self
            .with_module(owner, |m, ctx| m.on_att_read(ctx, &AttRead { con, handle, offset }))
            .unwrap_or(Err(ATT_ERROR_UNLIKELY_ERROR));
        tracing::debug!(
            "LE Audio: ATT read 0x{handle:04X} offset {offset} on 0x{con:04X} (module {owner}): {:?}",
            r.as_ref().map(|v| v.len())
        );
        r
    }

    fn att_write(&mut self, w: &AttWriteRequest<'_>) -> u8 {
        tracing::debug!(
            "LE Audio: ATT write 0x{:04X} on 0x{:04X} (mode {}, offset {}): {:02X?}",
            w.handle,
            w.con,
            w.mode,
            w.offset,
            w.value
        );
        match self.core.server.write(w.con, w.handle, w.mode, w.offset, w.value) {
            WriteAction::Done(status) => status,
            WriteAction::Modules(list) => {
                let mut status = 0;
                for (owner, handle, value) in list {
                    let r = self
                        .with_module(owner, |m, ctx| {
                            m.on_att_write(
                                ctx,
                                &AttWrite { con: w.con, handle, value: &value, without_response: false },
                            )
                        })
                        .unwrap_or(Err(ATT_ERROR_UNLIKELY_ERROR));
                    if let Err(e) = r
                        && status == 0
                    {
                        status = e;
                    }
                }
                if status != 0 {
                    tracing::debug!("LE Audio: ATT write 0x{:04X} refused (0x{status:02X})", w.handle);
                }
                status
            }
        }
    }
}

/// The LC3 PAC records for LeDeviceInfo, e.g. "LC3 rates 0x00b4 ch 0x03
/// octets 26-155" (records joined by "; ")
fn describe_pacs(pacs: &[PacRecord]) -> String {
    pacs.iter()
        .filter(|r| r.lc3)
        .map(|r| {
            format!(
                "LC3 rates 0x{:04x} ch 0x{:02x} octets {}-{}",
                r.caps.sampling_frequencies, r.caps.channel_counts, r.caps.min_octets, r.caps.max_octets
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Polls every 20 ms (at most 2 s) until the device's link is down
fn poll_disconnected(ctx: &mut LeCtx<'_>, id: LeDeviceId, ticks: u32, done: LeDone) {
    if ticks > 100 || ctx.device(id).is_none_or(|d| !d.connected()) {
        done(true);
        return;
    }
    ctx.run_after(20, Box::new(move |ctx| poll_disconnected(ctx, id, ticks + 1, done)));
}

/// The report's data into `r`: advertising data or scan response, the
/// parts of chained extended data joined
fn take_report(res: &mut LeScanResult, r: &events::AdvReport<'_>) {
    let (scan_rsp, connectable) = if r.extended {
        (r.event_type & 0x08 != 0, r.event_type & 0x01 != 0)
    } else {
        (r.event_type == 4, r.event_type == 0 || r.event_type == 1)
    };
    // Extended advertising data may come in several reports (data
    // status 01: more follows): the parts are joined, as a new report
    // that starts mid-way through an AD structure would not parse
    let more = r.extended && (r.event_type >> 5) & 0x03 == 0x01;
    let continued = if scan_rsp { res.rsp_more } else { res.adv_more };
    let target = if scan_rsp { &mut res.scan_rsp } else { &mut res.adv };
    if continued {
        target.extend_from_slice(r.data);
    } else {
        *target = r.data.to_vec();
    }
    if scan_rsp {
        res.rsp_more = more;
    } else {
        res.adv_more = more;
        if !continued {
            res.connectable = connectable;
        }
    }
}

/// Fills the parsed fields of `r` from AD data (v1 parse_ad)
fn parse_ad(r: &mut LeScanResult, data: &[u8]) {
    let add_uuid = |r: &mut LeScanResult, u: u16| {
        if !r.uuids16.contains(&u) {
            r.uuids16.push(u);
        }
    };
    let mut i = 0usize;
    while i < data.len() {
        let len = data[i] as usize;
        if len == 0 {
            i += 1;
            continue;
        }
        let Some(item) = data.get(i + 1..i + 1 + len) else { break };
        i += 1 + len;
        let (ty, v) = (item[0], &item[1..]);
        let rd16 = |p: &[u8]| u16::from_le_bytes([p[0], p[1]]);
        match ty {
            AD_NAME_SHORT | AD_NAME_COMPLETE => {
                if ty == AD_NAME_COMPLETE || r.name.is_empty() {
                    r.name = String::from_utf8_lossy(v).into_owned();
                }
            }
            AD_APPEARANCE if v.len() >= 2 => r.appearance = rd16(v),
            AD_UUID16_INCOMPLETE | AD_UUID16_COMPLETE => {
                for u in v.as_chunks::<2>().0 {
                    add_uuid(r, u16::from_le_bytes(*u));
                }
            }
            AD_SERVICE_DATA_UUID16 if v.len() >= 2 => {
                let u = rd16(v);
                add_uuid(r, u);
                if u == UUID_SERVICE_ASCS && v.len() >= 5 {
                    r.targeted = v[2] == 0x01;
                    r.ascs_sink_contexts = rd16(&v[3..]);
                } else if u == UUID_SERVICE_CAS && v.len() >= 3 {
                    r.targeted |= v[2] == 0x01;
                }
            }
            AD_RSI if v.len() >= 6 => {
                let mut rsi = [0u8; 6];
                rsi.copy_from_slice(&v[..6]);
                r.rsi = Some(rsi);
            }
            _ => {}
        }
    }
    // CAS alone and no name: another advertising set of a device listed
    // under its own address (issue #86: an EarFun's Fast Pair set for its
    // bonded phones, on a random address), nothing to connect to
    let audio_service = r
        .uuids16
        .iter()
        .any(|u| matches!(*u, UUID_SERVICE_ASCS | UUID_SERVICE_PACS | UUID_SERVICE_TMAS | UUID_SERVICE_GMAS));
    let named_cas = r.uuids16.contains(&UUID_SERVICE_CAS) && !r.name.trim().is_empty();
    if audio_service || named_cas || r.rsi.is_some() {
        r.appearance_only = false;
    }
    if audio_service || named_cas {
        r.le_audio = true;
    }
    // No LE Audio UUID, but an audio appearance (Audio Sink 0x0840-0x087F,
    // Wearable Audio Device 0x0940-0x097F, e.g. earbuds): list it as a
    // candidate; connecting tells (no ASCS: "no sink ASE"). Not for the LE
    // side of a classic device (the service drops those, a Sennheiser
    // HDR 275 advertises so for its app)
    let category = r.appearance >> 6;
    if !r.le_audio && r.connectable && (category == 0x021 || category == 0x025) {
        r.le_audio = true;
        r.appearance_only = r.rsi.is_none();
        if r.note.is_empty() {
            r.note = "audio appearance, LE Audio not advertised".into();
        }
    }
}

// ============================================================================
// The unicast state machine's view of the core
// ============================================================================

impl UnicastEnv for LeCtx<'_> {
    fn device(&self, id: u32) -> Option<&LeDevice> {
        self.core.devices.get(id as usize)
    }

    fn cp_write(&mut self, device: u32, value: Vec<u8>, what: &'static str) {
        let Some(d) = self.core.devices.get(device as usize) else { return };
        if d.ase_cp_handle == 0 {
            tracing::error!("LE Audio: {} has no ASE control point", d.addr);
            return;
        }
        let (addr, acl, cp) = (d.addr, d.acl, d.ase_cp_handle);
        tracing::debug!("LE Audio: {addr}: ASE control point ({what}) 0x{cp:04X} <- {value:02X?}");
        self.core.gatt.write(
            acl,
            cp,
            value,
            Some(Box::new(move |_, status| {
                if status != 0 {
                    tracing::warn!("LE Audio: {addr}: ASE control point write ({what}) failed (ATT 0x{status:02x})");
                }
            })),
        );
    }

    fn ccids(&mut self, contexts: u16) -> Vec<u8> {
        let mut ccids = Vec::new();
        for m in self.core.modules.iter().flatten() {
            m.add_ccids(contexts, &mut ccids);
        }
        ccids
    }

    fn cig_create(&mut self, params: &CigParams) -> u8 {
        let status = self.stack.cig_create(params);
        tracing::debug!("LE Audio: LE Set CIG Parameters {params:?} -> 0x{status:02X}");
        status
    }

    fn cig_remove(&mut self, cig_id: u8) -> u8 {
        let status = self.stack.cig_remove(cig_id);
        tracing::debug!("LE Audio: LE Remove CIG {cig_id} -> 0x{status:02X}");
        status
    }

    fn cis_create(&mut self, cig_id: u8, cis_handles: &[u16], acl_handles: &[u16]) -> u8 {
        let status = self.stack.cis_create(cig_id, cis_handles, acl_handles);
        tracing::debug!(
            "LE Audio: LE Create CIS (CIG {cig_id}, CIS {cis_handles:04X?}, ACL {acl_handles:04X?}) -> 0x{status:02X}"
        );
        status
    }

    fn can_send_command(&self) -> bool {
        self.stack.can_send_command_now()
    }

    fn disconnect_cis(&mut self, handle: u16) -> u8 {
        let status = self.stack.cmd_disconnect(handle, ERROR_CODE_REMOTE_USER_TERMINATED_CONNECTION);
        tracing::debug!("LE Audio: disconnect CIS 0x{handle:04X} -> 0x{status:02X}");
        status
    }

    fn iso_packet_count(&self) -> u8 {
        self.core.shared.iso_buffers().1
    }

    fn start_sending(&mut self, cis_handles: &[u16], queue_share: u8, frame_us: u32, max_latency_ms: u16) {
        let queue = self.core.queue.get_or_insert_with(|| Arc::new(Mutex::new(SduQueue::new()))).clone();
        tracing::debug!(
            "LE Audio: start sending on CIS {cis_handles:04X?}, up to {queue_share} queued per CIS, frame {frame_us} us"
        );
        self.core.iso.start(self.stack, cis_handles, queue, frame_us, queue_share, max_latency_ms);
    }

    fn stop_sending(&mut self) {
        tracing::debug!("LE Audio: stop sending");
        self.core.iso.stop(self.stack);
    }

    fn start_receiving(&mut self, cis_handles: &[u16]) {
        if self.core.rx.is_some() {
            return;
        }
        let Some(producer) = self.core.rx_producer.take() else {
            tracing::debug!("LE Audio: CIS {cis_handles:04X?} carry a microphone, no receiver asked for");
            return;
        };
        self.core.rx = Some(IsoRx::new(producer, cis_handles, self.core.rx_stats.clone()));
    }

    fn stop_receiving(&mut self) {
        self.core.rx = None;
    }

    fn cis_down(&mut self, handle: u16) {
        tracing::debug!("LE Audio: CIS 0x{handle:04X} down");
        self.core.iso.on_cis_down(self.stack, handle);
    }

    fn stream_lost(&mut self, why: &str) {
        if !self.core.shared.stream_lost.swap(true, Ordering::AcqRel) {
            tracing::warn!("LE Audio: stream lost: {why}");
        }
        self.core.iso.stop(self.stack);
        self.core.rx = None;
    }

    fn member_lost(&mut self, device: u32, why: &str) {
        let addr = self.core.devices.get(device as usize).map(|d| d.addr.to_string()).unwrap_or_default();
        tracing::warn!("LE Audio: {addr} left the stream ({why}), the others play on");
        self.device_left(device);
    }

    fn device_left(&mut self, device: u32) {
        let mut dropped = self.core.shared.stream_dropped.lock().unwrap_or_else(|e| e.into_inner());
        if !dropped.contains(&LeDeviceId(device)) {
            dropped.push(LeDeviceId(device));
        }
    }

    fn stream_state(&mut self, plan: &StreamPlan, streaming: bool) {
        tracing::debug!("LE Audio: stream state: streaming {streaming} ({})", plan.describe());
        let status = match self.core.status.as_mut() {
            Some(s) => {
                s.active = streaming;
                s.plan = plan.clone();
                s.summary = plan.describe();
                s.clone()
            }
            None => {
                LeStreamStatus { active: streaming, plan: plan.clone(), devices: Vec::new(), summary: plan.describe() }
            }
        };
        self.each_module(|m, ctx| m.on_stream_state(ctx, &status, streaming));
        if !streaming {
            self.core.status = None;
        }
    }

    fn arm(&mut self, timer: UnicastTimer, ms: u32) {
        self.disarm(timer);
        tracing::trace!("LE Audio: unicast timer {timer:?} in {ms} ms");
        let slot = timer as usize;
        let id = self.run_after(
            ms,
            Box::new(move |ctx| {
                ctx.core.unicast_timers[slot] = None;
                ctx.with_unicast(|u, env| match timer {
                    UnicastTimer::Step => u.on_timeout(env),
                    UnicastTimer::Drop => u.drop_cis(env),
                    UnicastTimer::Refresh => u.on_refresh(env),
                    UnicastTimer::Remove => u.on_remove_timer(env),
                });
            }),
        );
        self.core.unicast_timers[slot] = Some(id);
    }

    fn disarm(&mut self, timer: UnicastTimer) {
        if let Some(id) = self.core.unicast_timers[timer as usize].take() {
            self.cancel_timer(id);
        }
    }

    fn read_ase(&mut self, device: u32, ase_id: u8) {
        let Some(d) = self.core.devices.get(device as usize) else { return };
        let Some(h) = d.ase(ase_id).map(|a| a.value_handle) else { return };
        let (dev, con) = (d.id, d.acl);
        tracing::debug!("LE Audio: device {device}: reading ASE {ase_id} (0x{h:04X})");
        self.core.gatt.read(
            con,
            h,
            Box::new(move |ctx, status, v| {
                if status == 0 {
                    bap::on_ase_value(ctx, dev, con, h, v);
                } else {
                    tracing::debug!("LE Audio: ASE 0x{h:04X} read failed (ATT 0x{status:02X})");
                }
            }),
        );
    }
}

// ============================================================================
// LeCore: the entry points (HostApp)
// ============================================================================

impl LeCore {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: LeConfig,
        shared: Arc<LeShared>,
        controls: Arc<Controls>,
        poster: LePoster,
        events: std::sync::mpsc::Sender<crate::HostEvent>,
        battery: bool,
        local_name: String,
        audio: bool,
        hid: Option<SharedHid>,
    ) -> LeCore {
        LeCore {
            events,
            battery,
            local_name,
            audio,
            hid,
            background: Vec::new(),
            hid_blocked: Vec::new(),
            hid_watch: false,
            hid_direct: None,
            iso: IsoSender::new(shared.stats.clone()),
            shared,
            controls,
            poster,
            config: Some(config),
            initialized: false,
            modules: Vec::new(),
            devices: Vec::new(),
            scan_results: Vec::new(),
            scanning: false,
            scan_started_ms: None,
            scan_found: 0,
            connect: None,
            rpa_cache: HashMap::new(),
            identity_seen: HashMap::new(),
            gatt: GattClient::new(),
            server: GattServer::new(),
            presets: PresetTable::new(),
            unicast: None,
            unicast_timers: [None; UnicastTimer::ALL],
            queue: None,
            status: None,
            broadcast: None,
            timers: Vec::new(),
            next_timer: 1,
            timers_changed: false,
            gatt_retry: None,
            gatt_cache_file: None,
            sink_pacs_by_char: BTreeMap::new(),
            source_pacs_by_char: BTreeMap::new(),
            rx_producer: None,
            rx_stats: Arc::default(),
            rx: None,
        }
    }

    pub(crate) fn initialized(&self) -> bool {
        self.initialized
    }

    /// The LE link of the running stream's first device (its RSSI for Link
    /// Quality); None while idle
    pub(crate) fn streaming_acl(&self) -> Option<u16> {
        let status = self.status.as_ref().filter(|s| s.active)?;
        status.devices.iter().find_map(|id| self.devices.iter().find(|d| d.id == *id && d.connected())).map(|d| d.acl)
    }

    /// Runs `f` with an LeCtx, then settles (GATT queue, callbacks,
    /// notifications, timer)
    pub(crate) fn run<R>(&mut self, stack: &Stack, f: impl FnOnce(&mut LeCtx<'_>) -> R) -> R {
        let mut ctx = LeCtx { stack, core: self };
        let r = f(&mut ctx);
        ctx.settle();
        r
    }

    /// After hci_init / l2cap_init, before HCI power on (v1
    /// Core::stack_init): bonds TLV, SM, GATT client, ISO, the modules'
    /// on_stack_init, the GATT server with their services
    pub(crate) fn stack_init(&mut self, stack: &Stack) {
        if self.initialized {
            return;
        }
        if self.audio {
            tracing::info!("LE Audio: initializing (SM, GATT client / server, ISO)");
        } else {
            tracing::info!("LE: initializing for keyboards and mice (SM, GATT client)");
        }
        let path: PathBuf = self
            .config
            .as_ref()
            .and_then(|c| c.bonds_file.clone())
            .unwrap_or_else(|| std::env::temp_dir().join("le_audio_bonds.tlv"));
        stack.le_init(Box::new(crate::le_bond_file::TlvFile::open(&path)));
        if let Some(imported) = self.config.as_ref().and_then(|c| c.imported_bonds_file.as_deref()) {
            crate::le_import::apply(stack, imported);
        }

        // The modules (once per BtHost)
        if let Some(cfg) = self.config.take() {
            self.gatt_cache_file = cfg.bond_dir.as_ref().map(|d| d.join(super::gatt_cache::FILE_NAME));
            let mut setup = LeModuleSetup {
                bond_dir: cfg.bond_dir,
                media: cfg.media,
                ccp_sim: cfg.ccp_sim,
                ccp_console: cfg.ccp_console,
                poster: self.poster.clone(),
                events: self.events.clone(),
                battery: self.battery,
                audio: self.audio,
                hid: self.hid.clone(),
            };
            let mut hooks = Vec::new();
            let mut names = Vec::new();
            for e in super::modules::create_all(&mut setup) {
                names.push(e.module.name());
                self.modules.push(Some(e.module));
                hooks.extend(e.hooks);
            }
            tracing::debug!("{}: modules {}", if self.audio { "LE Audio" } else { "LE" }, names.join(", "));
            *self.shared.hooks.lock().unwrap_or_else(|e| e.into_inner()) = hooks;
        }

        self.devices.clear();
        self.scan_results.clear();
        self.scanning = false;
        self.scan_started_ms = None;
        self.connect = None;
        self.background.clear();
        self.hid_blocked.clear();
        self.hid_watch = false;
        self.hid_direct = None;
        self.rpa_cache.clear();
        self.identity_seen.clear();
        self.gatt = GattClient::new();
        self.sink_pacs_by_char.clear();
        self.source_pacs_by_char.clear();
        self.shared.iso_len.store(0, Ordering::Release);
        self.shared.iso_count.store(0, Ordering::Release);
        self.unicast = Some(Unicast::default());
        self.status = None;
        self.broadcast = None;
        self.initialized = true;

        self.run(stack, |ctx| ctx.each_module(|m, ctx| m.on_stack_init(ctx)));

        self.server = GattServer::new();
        self.server.build_base(&self.local_name);
        for i in 0..self.modules.len() {
            if let Some(mut m) = self.modules[i].take() {
                m.add_gatt_services(&mut self.server.builder(i));
                self.modules[i] = Some(m);
            }
        }
        self.server.start(stack);
        self.shared.initialized.store(true, Ordering::Release);
    }

    /// The run loop has stopped (v1 Core::stack_deinit): the modules'
    /// on_stack_deinit, a connect waiting gets "Bluetooth stopped", all
    /// session state goes
    pub(crate) fn stack_deinit(&mut self, stack: &Stack) {
        if !self.initialized {
            return;
        }
        self.shared.initialized.store(false, Ordering::Release);
        self.shared.hci_working.store(false, Ordering::Release);
        self.shared.le_supported.store(false, Ordering::Release);
        self.background.clear();
        self.hid_watch = false;
        self.hid_direct = None;
        tracing::debug!(
            "LE Audio: deinit ({} device(s), connect pending {}, {} timer(s))",
            self.devices.len(),
            self.connect.is_some(),
            self.timers.len()
        );
        for m in self.modules.iter_mut().flatten() {
            m.on_stack_deinit();
        }
        self.iso.stop(stack);
        self.rx = None;
        self.rx_producer = None;
        if let Some(c) = self.connect.take() {
            c.result.lock().unwrap_or_else(|e| e.into_inner()).error = "Bluetooth stopped".into();
            (c.done)(false);
        }
        if let Some(b) = self.broadcast.take() {
            b.abandon();
        }
        self.initialized = false;
        self.unicast = None;
        self.gatt.detach_all();
        // Callbacks of operations cut short are dropped: no BTstack calls
        // any more
        drop(self.gatt.take_ready());
        self.timers.clear();
        self.devices.clear();
        self.scan_results.clear();
        self.scan_started_ms = None;
        self.rpa_cache.clear();
        self.identity_seen.clear();
        self.queue = None;
        self.status = None;
        tracing::info!("{}: stopped", if self.audio { "LE Audio" } else { "LE" });
    }

    /// Changes when a scan lists another LE Audio advertiser
    pub(crate) fn scan_found(&self) -> u64 {
        self.scan_found
    }

    pub(crate) fn on_hci(&mut self, stack: &Stack, packet: &[u8]) {
        if self.initialized {
            // LE CIS Established, completed packets, packet sent
            self.iso.on_hci_event(stack, packet);
            self.after_last_round(stack);
            self.run(stack, |ctx| ctx.on_hci(packet));
        }
    }

    pub(crate) fn on_sm(&mut self, stack: &Stack, packet: &[u8]) {
        if self.initialized {
            self.run(stack, |ctx| ctx.on_sm(packet));
        }
    }

    pub(crate) fn on_gatt(&mut self, stack: &Stack, packet: &[u8]) {
        // GATT_EVENT_MTU: con, MTU (the bearer's, shared with att_server)
        if packet.first() == Some(&GATT_EVENT_MTU)
            && let Some(&[_, _, c0, c1, m0, m1]) = packet.get(..6)
        {
            tracing::debug!(
                "LE Audio: GATT MTU {} on 0x{:04X}",
                u16::from_le_bytes([m0, m1]),
                u16::from_le_bytes([c0, c1])
            );
            self.server.on_mtu(u16::from_le_bytes([c0, c1]), u16::from_le_bytes([m0, m1]));
        }
        if self.initialized {
            self.run(stack, |ctx| ctx.core.gatt.on_event(stack, packet));
        }
    }

    pub(crate) fn on_att_event(&mut self, packet: &[u8]) {
        if let Some((con, mtu)) = events::att_mtu_exchange_complete(packet) {
            tracing::debug!("LE Audio: ATT server MTU {mtu} on 0x{con:04X}");
            self.server.on_mtu(con, mtu);
        }
    }

    /// The ISO handler: HCI_EVENT_BIS_CAN_SEND_NOW (CIS sending does not use
    /// can-send-now), and HCI ISO data (the microphone of source ASEs). The
    /// packet type comes first: an ISO data packet's first byte is the
    /// handle's low byte and could look like an event code.
    pub(crate) fn on_iso(&mut self, stack: &Stack, packet_type: u8, packet: &[u8]) {
        if !self.initialized {
            return;
        }
        if super::iso_recv::is_iso_data(packet_type) {
            if let Some(rx) = self.rx.as_mut() {
                rx.on_packet(packet);
            }
            return;
        }
        if packet_type == HCI_EVENT_PACKET {
            self.iso.on_bis_can_send_now(stack, packet);
            self.after_last_round(stack);
        }
    }

    /// A stopping broadcast's last round is out: the stop goes on
    fn after_last_round(&mut self, stack: &Stack) {
        if self.iso.take_round_finished() {
            self.run(stack, |ctx| ctx.on_last_round_sent());
        }
    }

    pub(crate) fn on_iso_timer(&mut self, stack: &Stack) {
        self.iso.on_timer(stack);
        self.after_last_round(stack);
        // No SDU completed on any CIS for a while: as a link lost (the
        // stream is set up again)
        if let Some(why) = self.iso.take_stall() {
            self.run(stack, |ctx| ctx.stream_lost(&why));
        }
    }

    /// TIMER_LE: the due timers of run_after
    pub(crate) fn on_timer(&mut self, stack: &Stack) {
        self.timers_changed = true;
        self.run(stack, |ctx| {
            let now = ctx.stack.time_ms();
            let mut due: Vec<(u32, u32)> = ctx
                .core
                .timers
                .iter()
                .filter(|t| now.wrapping_sub(t.due) as i32 >= 0)
                .map(|t| (t.due.wrapping_sub(now), t.id))
                .collect();
            tracing::trace!("LE Audio: timer tick, {} due of {}", due.len(), ctx.core.timers.len());
            // Earliest first
            due.sort_by_key(|(d, _)| *d as i32);
            for (_, id) in due {
                let Some(pos) = ctx.core.timers.iter().position(|t| t.id == id) else { continue };
                let t = ctx.core.timers.remove(pos);
                ctx.core.timers_changed = true;
                (t.cb)(ctx);
            }
        });
    }

    pub(crate) fn att_read(&mut self, stack: &Stack, con: u16, handle: u16, offset: u16) -> Result<Vec<u8>, u8> {
        if !self.initialized {
            return Err(ATT_ERROR_UNLIKELY_ERROR);
        }
        self.run(stack, |ctx| ctx.att_read(con, handle, offset))
    }

    pub(crate) fn att_write(&mut self, stack: &Stack, write: &AttWriteRequest<'_>) -> u8 {
        if !self.initialized {
            return ATT_ERROR_UNLIKELY_ERROR;
        }
        self.run(stack, |ctx| ctx.att_write(write))
    }
}

/// Queues closures for the BTstack thread from any thread without waiting
/// (v1 Core::post). A closure still queued when the session ends never
/// runs.
#[derive(Clone)]
pub struct LePoster {
    jobs: mpsc::Sender<Job>,
    remote: Option<Remote>,
}

impl LePoster {
    pub(crate) fn new(jobs: mpsc::Sender<Job>, remote: Remote) -> LePoster {
        LePoster { jobs, remote: Some(remote) }
    }

    /// A poster whose closures never run (tests of modules)
    #[cfg(test)]
    pub(crate) fn detached() -> LePoster {
        let (jobs, _) = mpsc::channel();
        LePoster { jobs, remote: None }
    }

    pub fn post(&self, f: impl FnOnce(&mut LeCtx<'_>) + Send + 'static) {
        let job: Job = Box::new(move |app: &mut HostApp, stack: &Stack| {
            if let Some(le) = app.le.as_mut()
                && le.initialized
            {
                le.run(stack, f);
            }
        });
        if self.jobs.send(job).is_ok()
            && let Some(r) = self.remote
        {
            r.wake();
        }
    }
}

/// The stream's jitter buffer, shared by [`super::LeSduSink`] (encode
/// thread) and the ISO sender (BTstack thread)
pub type SharedSduQueue = Arc<Mutex<SduQueue>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chained_extended_advertising_data_is_joined() {
        // CAS service data, then the name; split across two reports (data
        // status 01, then complete) in the name's middle
        const AD: [u8; 12] = [3, 0x16, 0x53, 0x18, 7, 0x09, b'E', b'a', b'r', b'F', b'u', b'n'];
        let report = |event_type: u16, data: &'static [u8]| events::AdvReport {
            event_type,
            addr_type: 1,
            addr: [1, 2, 3, 4, 5, 6],
            rssi: -50,
            data,
            extended: true,
        };
        let mut r = LeScanResult::default();
        // Connectable undirected, more data follows
        take_report(&mut r, &report(0x0001 | 0x20, &AD[..7]));
        assert!(r.adv_more && r.connectable);
        take_report(&mut r, &report(0x0001, &AD[7..]));
        assert!(!r.adv_more);
        assert_eq!(r.adv, AD);
        let adv = r.adv.clone();
        parse_ad(&mut r, &adv);
        assert!(r.le_audio);
        assert_eq!(r.name, "EarFun");
        // The next advertising event replaces the data
        take_report(&mut r, &report(0x0001, &AD[..4]));
        assert_eq!(r.adv, AD[..4]);
        // A scan response leaves the advertising data alone
        take_report(&mut r, &report(0x0001 | 0x08, &AD[4..]));
        assert_eq!((r.adv.as_slice(), r.scan_rsp.as_slice()), (&AD[..4], &AD[4..]));
    }

    #[test]
    fn advertising_data_is_parsed_as_v1() {
        let mut r = LeScanResult { connectable: true, ..LeScanResult::default() };
        // Flags, ASCS service data (targeted, sink contexts 0x0006), CAS
        // service data, RSI, short name
        let mut ad = vec![2, 0x01, 0x06, 8, 0x16, 0x4E, 0x18, 0x01, 0x06, 0x00, 0x00, 0x00];
        ad.extend_from_slice(&[4, 0x16, 0x53, 0x18, 0x00]);
        ad.extend_from_slice(&[7, 0x2E, 1, 2, 3, 4, 5, 6]);
        ad.extend_from_slice(&[4, 0x08, b'B', b'u', b'd']);
        parse_ad(&mut r, &ad);
        assert!(r.le_audio && r.targeted);
        assert_eq!(r.uuids16, [0x184E, 0x1853]);
        assert_eq!(r.ascs_sink_contexts, 0x0006);
        assert_eq!(r.rsi, Some([1, 2, 3, 4, 5, 6]));
        assert_eq!(r.name, "Bud");
        // The complete name wins over the short one
        parse_ad(&mut r, &[5, 0x09, b'B', b'u', b'd', b's']);
        parse_ad(&mut r, &[3, 0x08, b'X', b'X']);
        assert_eq!(r.name, "Buds");
        // An earbud appearance without LE Audio UUIDs is a candidate
        let mut e = LeScanResult { connectable: true, ..LeScanResult::default() };
        parse_ad(&mut e, &[3, 0x19, 0x41, 0x09, 3, 0x03, 0x0F, 0x18]);
        assert!(e.le_audio && e.appearance_only);
        assert_eq!(e.note, "audio appearance, LE Audio not advertised");
        // ... until its other report advertises PACS
        parse_ad(&mut e, &[3, 0x03, 0x50, 0x18]);
        assert!(e.le_audio && !e.appearance_only);
        // Fast Pair data and a bare CAS scan response, no name (issue #86):
        // not listed; with a name it is
        let mut c = LeScanResult { connectable: true, ..LeScanResult::default() };
        parse_ad(&mut c, &[2, 0x01, 0x18, 6, 0x16, 0x2C, 0xFE, 0x10, 0x40, 0x00, 2, 0x0A, 0xF6]);
        parse_ad(&mut c, &[3, 0x16, 0x53, 0x18]);
        assert!(!c.le_audio);
        parse_ad(&mut c, &[4, 0x09, b'B', b'u', b'd']);
        assert!(c.le_audio);
        // Malformed: a length beyond the data ends the parsing
        let mut m = LeScanResult::default();
        parse_ad(&mut m, &[9, 0x09, b'A']);
        assert!(m.name.is_empty() && !m.le_audio);
    }

    #[test]
    fn only_a_missing_key_deletes_the_bond() {
        use ReencryptionFailure::*;
        assert_eq!(reencryption_failure(ERROR_CODE_PIN_OR_KEY_MISSING), KeyMissing);
        assert_eq!(reencryption_failure(ERROR_CODE_AUTHENTICATION_FAILURE), Refused);
        // BTstack's reports of a link that went down (0x13 for any
        // disconnection during the procedure) or an SM timeout (0x08), and
        // what a controller may answer: the key is fine
        for status in [0x08, 0x0C, 0x13, 0x16, 0x1A, 0x1F, 0x22, 0x23, 0x28, 0x2A, 0x3B, 0x3D, 0x3E] {
            assert_eq!(reencryption_failure(status), Transient, "0x{status:02x}");
        }
    }

    #[test]
    fn connects_with_an_rpa_of_the_latest_scan_only() {
        let identity = LeAddr { addr: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66], addr_type: LeAddr::PUBLIC };
        let random_identity = LeAddr { addr: [0xC1, 0x22, 0x33, 0x44, 0x55, 0x66], addr_type: LeAddr::RANDOM };
        let old = [0x41, 1, 1, 1, 1, 1];
        let new = [0x42, 2, 2, 2, 2, 2];
        let rpa = |a: [u8; 6]| LeAddr { addr: a, addr_type: LeAddr::RANDOM };
        // Seen during the find just before: the freshest one
        assert_eq!(pick_connect_address(identity, identity, &[(old, 9_000), (new, 500)], None, Some(10_000)), rpa(new));
        // The find did not see the device: an RPA from an earlier scan, even
        // seconds old, is not used; the identity is
        assert_eq!(pick_connect_address(identity, identity, &[(old, 11_000)], None, Some(10_000)), identity);
        // No scan at all
        assert_eq!(pick_connect_address(identity, identity, &[(old, 1_000)], None, None), identity);
        // A scan long running: RPA_FRESH_MS still applies
        assert_eq!(
            pick_connect_address(identity, identity, &[(old, RPA_FRESH_MS + 1)], None, Some(RPA_FRESH_MS + 5_000)),
            identity
        );
        // The identity seen after the RPA wins, with its own address type
        let given = LeAddr { addr: random_identity.addr, addr_type: LeAddr::PUBLIC };
        assert_eq!(
            pick_connect_address(given, random_identity, &[(new, 3_000)], Some(1_000), Some(10_000)),
            random_identity
        );
        // ... and also without a fresh RPA
        assert_eq!(pick_connect_address(given, random_identity, &[], None, Some(10_000)), random_identity);
        // An RPA given (a scan result) of a device not resolved to anything
        // here stays as given
        assert_eq!(pick_connect_address(rpa(new), identity, &[], None, Some(10_000)), rpa(new));
    }

    #[test]
    fn device_lookups() {
        let mut d = LeDevice::new(LeDeviceId(0), LeAddr::default());
        d.services.push(GattService {
            uuid16: 0x1850,
            uuid128: [0; 16],
            start_handle: 1,
            end_handle: 9,
            primary: true,
            included_by: 0,
            chars: vec![
                GattChar { uuid16: 0x2BC9, value_handle: 3, ..GattChar::default() },
                GattChar { uuid16: 0x2BC9, value_handle: 5, ..GattChar::default() },
            ],
        });
        assert_eq!(d.find_chars(0x1850, 0x2BC9).len(), 2);
        assert_eq!(d.find_chars(0, 0x2BC9).len(), 2);
        assert_eq!(d.find_char(0x184E, 0x2BC9), None);
        assert_eq!(d.find_char(0x1850, 0x2BC9).map(|c| c.value_handle), Some(3));
        d.ases.push(Ase { id: 2, sink: true, ..Ase::default() });
        d.ases.push(Ase { id: 3, sink: false, ..Ase::default() });
        assert_eq!(d.sink_ase_count(), 1);
        assert!(d.ase(3).is_some_and(|a| !a.sink));
        assert!(!d.connected());
        assert!(LeAddr { addr: [0x5A, 1, 2, 3, 4, 5], addr_type: LeAddr::RANDOM }.is_rpa());
        assert!(!LeAddr { addr: [0xDA, 1, 2, 3, 4, 5], addr_type: LeAddr::RANDOM }.is_rpa());
        assert!(!LeAddr { addr: [0x5A, 1, 2, 3, 4, 5], addr_type: LeAddr::PUBLIC }.is_rpa());
    }
}

/// LE connection parameters (BTstack units, see
/// `Stack::le_set_connection_params`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConnParams {
    pub scan_interval: u16,
    pub scan_window: u16,
    pub interval_min: u16,
    pub interval_max: u16,
    pub latency: u16,
    pub supervision_timeout: u16,
    pub min_ce: u16,
    pub max_ce: u16,
}

/// BTstack's defaults (hci.c): what LE Audio links use
pub(crate) const DEFAULT_CONNECTION_PARAMS: ConnParams = ConnParams {
    scan_interval: 0x0060,
    scan_window: 0x0030,
    interval_min: 0x0008,
    interval_max: 0x0018,
    latency: 4,
    supervision_timeout: 0x0048,
    min_ce: 0,
    max_ce: 0,
};

/// Plain GATT links (QCY reads) while A2DP may stream: initiator window
/// 15 ms of every 60 ms (25 % instead of 50 %), interval 45-60 ms instead
/// of 10-30 ms, no latency (the central talks only when it has to anyway),
/// 1 s supervision, connection events of at most 2.5 ms
pub(crate) const PLAIN_LINK_PARAMS: ConnParams = ConnParams {
    scan_interval: 0x0060,
    scan_window: 0x0018,
    interval_min: 0x0024,
    interval_max: 0x0030,
    latency: 0,
    supervision_timeout: 0x0064,
    min_ce: 0,
    max_ce: 0x0004,
};

fn restore_default_connection_params(stack: &Stack) {
    let p = DEFAULT_CONNECTION_PARAMS;
    stack.le_set_connection_params(
        p.scan_interval,
        p.scan_window,
        p.interval_min,
        p.interval_max,
        p.latency,
        p.supervision_timeout,
        p.min_ce,
        p.max_ce,
    );
}

#[cfg(test)]
mod plain_link_params_tests {
    use super::*;

    #[test]
    fn plain_links_are_gentler_than_the_defaults_and_valid() {
        let (p, d) = (PLAIN_LINK_PARAMS, DEFAULT_CONNECTION_PARAMS);
        assert!(p.scan_window < d.scan_window && p.scan_window <= p.scan_interval);
        assert!(p.interval_min > d.interval_max && p.interval_min <= p.interval_max);
        // Core spec ranges: interval 7.5 ms..4 s, timeout 100 ms..32 s
        assert!((0x0006..=0x0C80).contains(&p.interval_max));
        assert!((0x000A..=0x0C80).contains(&p.supervision_timeout));
        // timeout (10 ms) > (1 + latency) * interval_max (1.25 ms) * 2
        let timeout_ms = u32::from(p.supervision_timeout) * 10;
        let needed_ms = (1 + u32::from(p.latency)) * u32::from(p.interval_max) * 125 * 2 / 100;
        assert!(timeout_ms > needed_ms, "{timeout_ms} <= {needed_ms}");
    }
}

/// What a plain GATT link does with a Security Manager event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlainSm {
    /// The peer asks to pair: refuse
    Decline,
    /// Anything else: no answer, no retry
    Ignore,
}

/// The no-pairing guard of plain links: every pairing request is declined,
/// nothing is ever confirmed
pub(crate) fn plain_link_sm_action(ty: u8) -> PlainSm {
    match ty {
        SM_EVENT_JUST_WORKS_REQUEST
        | SM_EVENT_NUMERIC_COMPARISON_REQUEST
        | SM_EVENT_PASSKEY_INPUT_NUMBER
        | SM_EVENT_PASSKEY_DISPLAY_NUMBER => PlainSm::Decline,
        _ => PlainSm::Ignore,
    }
}

#[cfg(test)]
mod plain_link_guard_tests {
    use super::*;

    #[test]
    fn a_plain_link_declines_every_pairing_request() {
        for ty in [
            SM_EVENT_JUST_WORKS_REQUEST,
            SM_EVENT_NUMERIC_COMPARISON_REQUEST,
            SM_EVENT_PASSKEY_INPUT_NUMBER,
            SM_EVENT_PASSKEY_DISPLAY_NUMBER,
        ] {
            assert_eq!(plain_link_sm_action(ty), PlainSm::Decline, "0x{ty:02x}");
        }
    }

    #[test]
    fn a_plain_link_ignores_the_other_sm_events() {
        for ty in [SM_EVENT_PAIRING_STARTED, SM_EVENT_PAIRING_COMPLETE] {
            assert_eq!(plain_link_sm_action(ty), PlainSm::Ignore, "0x{ty:02x}");
        }
    }
}
