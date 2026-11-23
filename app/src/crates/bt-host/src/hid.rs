//! HID host (experimental, developer mode): keyboards and mice connected to
//! BlueCaster's adapter, their input passed to Windows (issue #413).
//!
//! Dangerous by design: the input exists only while this process runs and
//! the stack is up. A device must be on the list the user made; anything
//! else that opens HID channels (classic) is refused, and an LE device that
//! is not on the list is never connected to. Every way out of a connection
//! (closed, switched off, stack exit) releases the keys and buttons still
//! held, or Windows would keep them pressed.
//!
//! [`HidEngine`] is what classic Bluetooth ([`HidHost`], BTstack's
//! hid_host) and LE ([`crate::le::modules::hogp`], HID over GATT) share: the
//! list, the counters, passing input to Windows or only counting it, the
//! connect / disconnect events and the report descriptors kept per device.
//!
//! Nothing here logs what was typed or moved, nor an address: counts only.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

use btstack_sys::Stack;
use btstack_sys::consts::*;
use hid_input::sink::{DryRunSink, InputSink, LocalSink};
use hid_input::{Decoder, InputEvent, InputKind};
use win_sys::hid_guard::HeldSection;

use crate::HostEvent;
use crate::events::{addr_at, le16, u8_at};
use crate::le::LeAddr;

mod pairing;
pub use pairing::Passkey;
pub(crate) use pairing::{Pairing, next_pairing_generation};

/// A report descriptor file larger than this is not read (the largest
/// descriptors of real devices are a few hundred bytes)
const DESCRIPTOR_FILE_MAX: u64 = 16 * 1024;

/// See [`crate::HostConfig::hid`]
#[derive(Debug, Clone, Default)]
pub struct HidConfig {
    /// The classic keyboards and mice the user added: only these may connect
    pub devices: Vec<[u8; 6]>,
    /// The LE ones (HID over GATT), with their address type: only these are
    /// connected to, in the background whenever they advertise
    pub le_devices: Vec<LeAddr>,
    /// Input is decoded and counted, not passed to Windows (the emulator
    /// test, which must not type into the machine it runs on)
    pub dry_run: bool,
    /// Where each device's report descriptor is kept ([`descriptor_file`]),
    /// so that input decodes from the first report of the next connection;
    /// None: not kept
    pub descriptor_dir: Option<PathBuf>,
    /// Test hook: the keyboard lights sent to every keyboard (a mask of
    /// `hid_input::leds`) instead of Windows' Num, Caps and Scroll Lock
    /// state. Used in dry run only; in dry run without it no lights are
    /// sent.
    pub leds: Option<u8>,
    /// Where the keys and buttons injected and still held are kept for the
    /// guard process ([`win_sys::hid_guard`]); None: not kept. Used by the
    /// local sink only.
    pub held: Option<Arc<HeldSection>>,
    /// Where the input goes instead of this process's `SendInput` (the
    /// input helper's sink); None: [`LocalSink`], or [`DryRunSink`] in dry
    /// run. A sink given here is used in dry run too: only one that never
    /// passes input to Windows may be given with `dry_run`.
    pub sink: Option<Arc<dyn InputSink>>,
}

/// How often Windows' Num, Caps and Scroll Lock state is read, and the
/// keyboard lights brought up to date
pub(crate) const LEDS_TICK_MS: u32 = 150;

/// A keyboard or mouse connected or went away
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidEvent {
    Connected([u8; 6]),
    Disconnected([u8; 6]),
    /// [`crate::BtHost::hid_connect`] did not get its channels (HCI or
    /// L2CAP status)
    ConnectFailed([u8; 6], u8),
    /// A device connected and is ready for input: its report descriptor is
    /// read (or known from before) and, over LE, its input reports are
    /// subscribed to. `kind` from the descriptor; all false when none is
    /// usable. Sent once per connection.
    Ready([u8; 6], InputKind),
    /// Pairing the device the user adds: the user types this number on it
    /// ([`crate::BtHost::hid_pairing_begin`])
    Passkey([u8; 6], Passkey),
    /// That pairing ended (done, failed or timed out): the number goes away
    PasskeyDone([u8; 6]),
    /// An LE device listed with a resolvable private address told its
    /// identity address when it paired: listed with the identity from now on
    /// (the stack's list is changed already), so that it is found again
    /// once its private address changes
    Identity {
        listed: LeAddr,
        identity: LeAddr,
    },
}

pub use hid_input::sink::HidCounters;

/// The sink a stack with `config` passes input to: the one given, else
/// [`LocalSink`] (with the guard's section), or [`DryRunSink`] in dry run
pub fn sink_for(config: &HidConfig) -> Arc<dyn InputSink> {
    match (&config.sink, config.dry_run) {
        (Some(sink), _) => sink.clone(),
        (None, true) => Arc::new(DryRunSink::default()),
        (None, false) => Arc::new(LocalSink::new(config.held.clone())),
    }
}

/// The file of a device's report descriptor in `dir`: "<address>.bin", and
/// "<address>-<n>.bin" for the further HID services of an LE device that has
/// several. Not secret, but read back as untrusted input (the parser bounds
/// everything).
pub fn descriptor_file(dir: &Path, addr: &[u8; 6], instance: usize) -> PathBuf {
    let name: String = addr.iter().map(|b| format!("{b:02X}")).collect();
    if instance == 0 { dir.join(format!("{name}.bin")) } else { dir.join(format!("{name}-{instance}.bin")) }
}

/// Removes every report descriptor kept of a device (and its kept report
/// handles, [`reports_file`])
pub fn forget_descriptors(dir: &Path, addr: &[u8; 6]) {
    let name: String = addr.iter().map(|b| format!("{b:02X}")).collect();
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        if kept_file_of(&file, &name).is_some() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The part after the address of a file kept of the device `name` (upper
/// case hex): ".bin", "-<n>.bin" or ".reports"; None for other files
fn kept_file_of<'a>(file: &'a str, name: &str) -> Option<&'a str> {
    let rest = file.strip_prefix(name)?;
    let ok = rest == ".bin" || rest == ".reports" || (rest.starts_with('-') && rest.ends_with(".bin"));
    ok.then_some(rest)
}

/// Moves the files kept of `from` to `to` (a device listed with its
/// private address that told its identity)
fn move_kept_files(dir: &Path, from: &[u8; 6], to: &[u8; 6]) {
    let from: String = from.iter().map(|b| format!("{b:02X}")).collect();
    let to: String = to.iter().map(|b| format!("{b:02X}")).collect();
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        if let Some(rest) = kept_file_of(&file, &from)
            && let Err(err) = std::fs::rename(e.path(), dir.join(format!("{to}{rest}")))
        {
            tracing::debug!("hid: a kept file was not moved ({})", err.kind());
        }
    }
}

/// The report handles kept of an LE device in `dir`: "<address>.reports",
/// beside its report descriptors. One line per Report characteristic:
/// "<HID service> <value handle> <report ID> <report type>", decimal. Read
/// back as untrusted input (bounded, bad lines dropped).
pub fn reports_file(dir: &Path, addr: &[u8; 6]) -> PathBuf {
    let name: String = addr.iter().map(|b| format!("{b:02X}")).collect();
    dir.join(format!("{name}.reports"))
}

/// A Report characteristic of an LE device, kept for its next connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeptReport {
    /// Which HID service (0 the first)
    pub instance: usize,
    pub value_handle: u16,
    pub id: u8,
    /// Report Reference: 1 input, 2 output, 3 feature
    pub kind: u8,
}

/// At most this many kept reports are read (real devices have a few)
const KEPT_REPORTS_MAX: usize = 64;

fn parse_reports(text: &str) -> Vec<KeptReport> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_ascii_whitespace();
            let r = KeptReport {
                instance: f.next()?.parse().ok().filter(|i| *i < 16)?,
                value_handle: f.next()?.parse().ok().filter(|h| *h != 0)?,
                id: f.next()?.parse().ok()?,
                kind: f.next()?.parse().ok().filter(|k| (1..=3).contains(k))?,
            };
            f.next().is_none().then_some(r)
        })
        .take(KEPT_REPORTS_MAX)
        .collect()
}

fn format_reports(reports: &[KeptReport]) -> String {
    reports.iter().map(|r| format!("{} {} {} {}\n", r.instance, r.value_handle, r.id, r.kind)).collect()
}

/// The decoder of a report descriptor; None (logged) when it is not usable
pub(crate) fn decoder_from(descriptor: &[u8]) -> Option<Decoder> {
    match Decoder::from_descriptor(descriptor) {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::warn!("hid: the report descriptor ({} bytes) cannot be used: {e}", descriptor.len());
            None
        }
    }
}

/// A device's decoder and the descriptor it was made of
#[derive(Default)]
pub(crate) struct DeviceDecoder {
    descriptor: Vec<u8>,
    pub decoder: Option<Decoder>,
}

/// The engine both HID transports share (on the BTstack thread)
pub(crate) type SharedHid = Rc<RefCell<HidEngine>>;

pub(crate) struct HidEngine {
    dry_run: bool,
    /// Classic devices allowed
    pub classic: Vec<[u8; 6]>,
    /// LE devices allowed
    pub le: Vec<LeAddr>,
    counters: Arc<HidCounters>,
    events: Sender<HostEvent>,
    descriptor_dir: Option<PathBuf>,
    /// Connected now, both kinds
    connected: Vec<[u8; 6]>,
    /// The device the user adds, which may show a passkey (pairing.rs)
    pairing: Pairing,
    /// The keyboard lights to show (a hid_input::leds mask); None: none
    /// known yet, nothing is sent
    leds: Option<u8>,
    /// The lights follow Windows' lock keys (input goes to Windows)
    leds_from_windows: bool,
    /// Where the input goes ([`InputSink`])
    sink: Arc<dyn InputSink>,
}

impl HidEngine {
    pub fn new(config: Option<&HidConfig>, counters: Arc<HidCounters>, events: Sender<HostEvent>) -> SharedHid {
        let c = config.cloned().unwrap_or_default();
        let sink = sink_for(&c);
        Rc::new(RefCell::new(HidEngine {
            dry_run: c.dry_run,
            classic: c.devices,
            le: c.le_devices,
            counters,
            events,
            descriptor_dir: c.descriptor_dir,
            connected: Vec::new(),
            pairing: Pairing::default(),
            leds: if c.dry_run { c.leds.map(|m| m & hid_input::leds::ALL) } else { None },
            leds_from_windows: !c.dry_run,
            sink,
        }))
    }

    /// The keyboard lights to show; None: nothing to send
    pub fn leds(&self) -> Option<u8> {
        self.leds
    }

    /// Reads Windows' lock keys from the sink (when the lights follow
    /// them; the input helper reports them, the local sink reads them):
    /// true when the lights changed
    pub fn poll_leds(&mut self) -> bool {
        if !self.leds_from_windows {
            return false;
        }
        let Some(now) = self.sink.lock_keys().map(|m| m & hid_input::leds::ALL) else { return false };
        self.leds.replace(now) != Some(now)
    }

    /// `n` light reports went out
    pub fn leds_sent(&self, n: usize) {
        if n > 0 {
            self.counters.leds.fetch_add(n as u64, Ordering::Relaxed);
            tracing::debug!("hid: keyboard lights sent to {n} keyboard(s)");
        }
    }

    pub fn counters(&self) -> &HidCounters {
        &self.counters
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn connected(&self) -> Vec<[u8; 6]> {
        self.connected.clone()
    }

    /// An LE device on the list: the one it is listed as
    pub fn le_listed(&self, addr: &[u8; 6]) -> Option<LeAddr> {
        self.le.iter().find(|a| a.addr == *addr).copied()
    }

    /// An LE device on the list as `listed` (a resolvable private address)
    /// has `identity`: listed with that from now on, here and (the event)
    /// in the saved list. Its kept report descriptors move along.
    pub fn on_identity(&mut self, listed: LeAddr, identity: LeAddr) {
        if listed.addr == identity.addr || self.le_listed(&listed.addr).is_none() {
            return;
        }
        let known = self.le_listed(&identity.addr).is_some();
        self.le.retain(|a| a.addr != listed.addr || !known);
        for a in self.le.iter_mut().filter(|a| a.addr == listed.addr) {
            *a = identity;
        }
        if let Some(dir) = &self.descriptor_dir {
            move_kept_files(dir, &listed.addr, &identity.addr);
        }
        tracing::info!("hid: an LE keyboard or mouse is listed with its identity address from now on");
        let _ = self.events.send(HostEvent::Hid(HidEvent::Identity { listed, identity }));
    }

    /// A device is connected and its input is taken from now on
    pub fn on_connected(&mut self, addr: [u8; 6]) {
        if !self.connected.contains(&addr) {
            self.connected.push(addr);
        }
        let _ = self.events.send(HostEvent::Hid(HidEvent::Connected(addr)));
    }

    pub fn on_disconnected(&mut self, addr: [u8; 6]) {
        self.connected.retain(|a| *a != addr);
        let _ = self.events.send(HostEvent::Hid(HidEvent::Disconnected(addr)));
    }

    /// A device is ready for input (see [`HidEvent::Ready`])
    pub fn on_ready(&self, addr: [u8; 6], kind: InputKind) {
        tracing::info!(
            "hid: ready for input ({})",
            match (kind.keyboard, kind.pointer) {
                (true, true) => "keys and pointer",
                (true, false) => "keys",
                (false, true) => "pointer",
                (false, false) => "no usable report",
            }
        );
        let _ = self.events.send(HostEvent::Hid(HidEvent::Ready(addr, kind)));
    }

    pub fn on_connect_failed(&self, addr: [u8; 6], status: u8) {
        let _ = self.events.send(HostEvent::Hid(HidEvent::ConnectFailed(addr, status)));
    }

    /// The stack stops: nobody is connected any more (the service learns it
    /// from the stack stopping)
    pub fn clear_connected(&mut self) {
        self.connected.clear();
        self.sink.release_all();
    }

    /// An input report of `from`: decoded and passed on, or counted as
    /// undecoded
    pub fn input(
        &self,
        from: [u8; 6],
        decoder: Option<&mut Decoder>,
        report: impl FnOnce(&mut Decoder) -> Vec<InputEvent>,
    ) {
        self.counters.reports.fetch_add(1, Ordering::Relaxed);
        let Some(decoder) = decoder else {
            self.counters.undecoded.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let events = report(decoder);
        if !events.is_empty() {
            self.pass(Some(from), &events);
        }
    }

    /// Lets go of everything a decoder still holds
    pub fn release(&self, decoder: Option<&mut Decoder>) {
        let events = decoder.map(|d| d.release_all()).unwrap_or_default();
        if !events.is_empty() {
            tracing::debug!("hid: releasing {} held key(s) or button(s)", events.len());
            self.pass(None, &events);
        }
    }

    pub fn refused(&self) {
        self.counters.refused.fetch_add(1, Ordering::Relaxed);
    }

    /// Windows gets the events through the sink (or they are only counted);
    /// `from`: the device whose report they were (None: a release)
    pub fn pass(&self, from: Option<[u8; 6]>, events: &[InputEvent]) {
        self.counters.events.fetch_add(events.len() as u64, Ordering::Relaxed);
        let injects = hid_input::inject::to_injects(events);
        if !injects.is_empty() {
            self.sink.pass(from, &injects, &self.counters);
        }
    }

    /// The decoder of the descriptor kept from an earlier connection
    pub fn stored(&self, addr: &[u8; 6], instance: usize) -> DeviceDecoder {
        let Some(file) = self.descriptor_dir.as_ref().map(|d| descriptor_file(d, addr, instance)) else {
            return DeviceDecoder::default();
        };
        let small = std::fs::metadata(&file).is_ok_and(|m| m.len() <= DESCRIPTOR_FILE_MAX);
        let Some(descriptor) = std::fs::read(&file).ok().filter(|_| small) else { return DeviceDecoder::default() };
        let decoder = decoder_from(&descriptor);
        tracing::debug!(
            "hid: kept report descriptor ({} bytes) {}",
            descriptor.len(),
            if decoder.is_some() { "used until the device sends its own" } else { "not usable" }
        );
        DeviceDecoder { descriptor, decoder }
    }

    /// The device's report descriptor as it sent it: a new decoder when it
    /// differs from the one in use (whose held keys are released first), and
    /// kept for the next connection
    pub fn learn(&self, addr: &[u8; 6], instance: usize, current: &mut DeviceDecoder, descriptor: &[u8]) {
        if current.decoder.is_some() && current.descriptor == descriptor {
            return;
        }
        self.release(current.decoder.as_mut());
        current.decoder = decoder_from(descriptor);
        current.descriptor = descriptor.to_vec();
        if let Some(dir) = &self.descriptor_dir
            && current.decoder.is_some()
        {
            let saved = std::fs::create_dir_all(dir)
                .and_then(|_| std::fs::write(descriptor_file(dir, addr, instance), descriptor));
            if let Err(e) = saved {
                tracing::debug!("hid: the report descriptor was not kept ({})", e.kind());
            }
        }
    }

    /// The report handles kept from the device's last connection; empty
    /// when none are
    pub(crate) fn kept_reports(&self, addr: &[u8; 6]) -> Vec<KeptReport> {
        let Some(file) = self.descriptor_dir.as_ref().map(|d| reports_file(d, addr)) else { return Vec::new() };
        let small = std::fs::metadata(&file).is_ok_and(|m| m.len() <= DESCRIPTOR_FILE_MAX);
        std::fs::read_to_string(&file).ok().filter(|_| small).map(|t| parse_reports(&t)).unwrap_or_default()
    }

    /// Keeps the device's report handles for its next connection (unless
    /// they are kept already)
    pub(crate) fn keep_reports(&self, addr: &[u8; 6], reports: &[KeptReport]) {
        let Some(dir) = &self.descriptor_dir else { return };
        if self.kept_reports(addr) == reports {
            return;
        }
        let saved =
            std::fs::create_dir_all(dir).and_then(|_| std::fs::write(reports_file(dir, addr), format_reports(reports)));
        match saved {
            Ok(()) => tracing::debug!("hid: {} report handle(s) kept", reports.len()),
            Err(e) => tracing::debug!("hid: the report handles were not kept ({})", e.kind()),
        }
    }
}

struct Link {
    addr: [u8; 6],
    opened: bool,
    decoder: DeviceDecoder,
    /// HidEvent::Ready went out for this connection
    ready: bool,
}

impl Link {
    fn new(addr: [u8; 6]) -> Link {
        Link { addr, opened: false, decoder: DeviceDecoder::default(), ready: false }
    }

    /// Ready for input once its channels are open and its descriptor is
    /// known (or turned out to be unusable): told once
    fn ready_now(&mut self, engine: &HidEngine, descriptor_done: bool) {
        if self.ready || !self.opened || (self.decoder.decoder.is_none() && !descriptor_done) {
            return;
        }
        self.ready = true;
        engine.on_ready(self.addr, self.decoder.decoder.as_ref().map(Decoder::kind).unwrap_or_default());
    }
}

/// Classic Bluetooth keyboards and mice (BTstack's hid_host)
pub(crate) struct HidHost {
    /// hid_host_init ran on this stack
    initialized: bool,
    engine: SharedHid,
    /// By hid_cid
    links: HashMap<u16, Link>,
    /// The keyboard lights each link was sent last, by hid_cid
    leds_sent: HashMap<u16, u8>,
}

impl HidHost {
    pub fn new(engine: SharedHid) -> HidHost {
        engine.borrow().counters().active.store(false, Ordering::Release);
        HidHost { initialized: false, engine, links: HashMap::new(), leds_sent: HashMap::new() }
    }

    /// Sends the keyboard lights to every connected keyboard that was not
    /// sent them yet (or other ones): once after it connects, and whenever
    /// they change
    pub fn sync_leds(&mut self, stack: &Stack) {
        let Some(mask) = self.engine.borrow().leds() else { return };
        let mut sent = 0;
        for (&cid, link) in self.links.iter().filter(|(_, l)| l.opened) {
            if self.leds_sent.get(&cid) == Some(&mask) {
                continue;
            }
            let Some(out) = link.decoder.decoder.as_ref().and_then(|d| d.leds()) else { continue };
            let id = (out.report_id != 0).then_some(out.report_id);
            match stack.hid_host_send_output(cid, id, &out.report(mask)) {
                0 => {
                    self.leds_sent.insert(cid, mask);
                    sent += 1;
                }
                // Tried again at the next tick
                status => tracing::debug!("hid: the keyboard lights were not sent (0x{status:02X})"),
            }
        }
        self.engine.borrow().leds_sent(sent);
    }

    /// After l2cap_init and sdp_init
    pub fn stack_init(&mut self, stack: &Stack) {
        stack.hid_host_init();
        self.initialized = true;
        let e = self.engine.borrow();
        e.counters().active.store(true, Ordering::Release);
        tracing::warn!(
            "HID host is on (experimental): {} keyboard(s) or mice may connect ({} over LE), their input {}",
            e.classic.len() + e.le.len(),
            e.le.len(),
            if e.dry_run() { "is only counted" } else { "goes to Windows" }
        );
    }

    pub fn connected(&self) -> Vec<[u8; 6]> {
        self.engine.borrow().connected()
    }

    fn allowed(&self, addr: &[u8; 6]) -> bool {
        self.engine.borrow().classic.contains(addr)
    }

    /// The classic devices that may connect from now on; the links of the
    /// others are closed
    pub fn set_devices(&mut self, stack: &Stack, devices: Vec<[u8; 6]>) {
        self.engine.borrow_mut().classic = devices;
        let gone: Vec<u16> = self.links.iter().filter(|(_, l)| !self.allowed(&l.addr)).map(|(&cid, _)| cid).collect();
        tracing::debug!(
            "hid: {} classic device(s) allowed, closing {} link(s)",
            self.engine.borrow().classic.len(),
            gone.len()
        );
        for cid in gone {
            self.release(cid);
            stack.hid_host_disconnect(cid);
        }
    }

    /// Opens the channels to a device on the list
    pub fn connect(&mut self, stack: &Stack, addr: [u8; 6]) -> Result<(), u8> {
        if !self.initialized || !self.allowed(&addr) {
            return Err(ERROR_CODE_COMMAND_DISALLOWED);
        }
        if self.links.values().any(|l| l.addr == addr) {
            return Ok(());
        }
        let cid = stack.hid_host_connect(&addr)?;
        self.links.insert(cid, Link::new(addr));
        Ok(())
    }

    pub fn disconnect(&mut self, stack: &Stack, addr: [u8; 6]) {
        let cids: Vec<u16> = self.links.iter().filter(|(_, l)| l.addr == addr).map(|(&cid, _)| cid).collect();
        for cid in cids {
            self.release(cid);
            stack.hid_host_disconnect(cid);
        }
    }

    /// The stack is going down: nothing may stay pressed
    pub fn stop(&mut self) {
        let cids: Vec<u16> = self.links.keys().copied().collect();
        for cid in cids {
            self.release(cid);
        }
        self.links.clear();
        self.leds_sent.clear();
        let mut e = self.engine.borrow_mut();
        e.clear_connected();
        e.counters().active.store(false, Ordering::Release);
    }

    /// Lets go of everything the device at `cid` still holds
    fn release(&mut self, cid: u16) {
        if let Some(link) = self.links.get_mut(&cid) {
            self.engine.borrow().release(link.decoder.decoder.as_mut());
        }
    }

    /// HCI_EVENT_HID_META
    pub fn on_event(&mut self, stack: &Stack, p: &[u8]) {
        let (Some(sub), Some(cid)) = (u8_at(p, 2), le16(p, 3)) else { return };
        match sub {
            HID_SUBEVENT_REPORT => self.on_report(cid, p),
            HID_SUBEVENT_INCOMING_CONNECTION => {
                let Some(addr) = addr_at(p, 5) else { return };
                let known = self.allowed(&addr);
                if !known {
                    self.engine.borrow().refused();
                }
                tracing::info!(
                    "hid: a device opens its channels, {}",
                    if known { "accepted" } else { "refused (not on the list)" }
                );
                let status = stack.hid_host_accept(cid, known);
                if known && status == 0 {
                    self.links.insert(cid, Link::new(addr));
                } else if status != 0 {
                    tracing::warn!("hid: answering the device failed (0x{status:02X})");
                }
            }
            HID_SUBEVENT_CONNECTION_OPENED => {
                let (Some(status), Some(addr)) = (u8_at(p, 5), addr_at(p, 6)) else { return };
                if status != 0 {
                    tracing::warn!("hid: connection failed (0x{status:02X})");
                    self.links.remove(&cid);
                    self.engine.borrow().on_connect_failed(addr, status);
                    return;
                }
                // Refused devices never get here; one closed in between does
                if !self.allowed(&addr) {
                    stack.hid_host_disconnect(cid);
                    return;
                }
                let engine = self.engine.clone();
                let link = self.links.entry(cid).or_insert_with(|| Link::new(addr));
                link.opened = true;
                // The descriptor can be there before the channels are; else
                // the one kept from the last connection decodes until it is
                // (a device that connects by itself sends input at once)
                match stack.hid_host_descriptor(cid) {
                    Some(d) => engine.borrow().learn(&addr, 0, &mut link.decoder, &d),
                    None if link.decoder.decoder.is_none() => link.decoder = engine.borrow().stored(&addr, 0),
                    None => {}
                }
                let usable = link.decoder.decoder.is_some();
                tracing::info!(
                    "hid: connected ({} link(s)), report descriptor {}",
                    self.links.len(),
                    if usable { "known" } else { "not read yet" }
                );
                engine.borrow_mut().on_connected(addr);
                if let Some(link) = self.links.get_mut(&cid) {
                    link.ready_now(&engine.borrow(), false);
                }
                self.sync_leds(stack);
            }
            HID_SUBEVENT_DESCRIPTOR_AVAILABLE => {
                let status = u8_at(p, 5).unwrap_or(0xFF);
                let descriptor = if status == 0 { stack.hid_host_descriptor(cid) } else { None };
                let engine = self.engine.clone();
                if let Some(link) = self.links.get_mut(&cid) {
                    if let Some(d) = &descriptor {
                        engine.borrow().learn(&link.addr, 0, &mut link.decoder, d);
                    }
                    tracing::info!(
                        "hid: report descriptor status 0x{status:02X}, {}",
                        if link.decoder.decoder.is_some() {
                            "usable"
                        } else {
                            "not usable: input of this device is dropped"
                        }
                    );
                    link.ready_now(&engine.borrow(), true);
                }
                self.sync_leds(stack);
            }
            HID_SUBEVENT_CONNECTION_CLOSED => {
                self.release(cid);
                self.leds_sent.remove(&cid);
                stack.hid_host_output_closed(cid);
                if let Some(link) = self.links.remove(&cid) {
                    tracing::info!("hid: disconnected ({} link(s) left)", self.links.len());
                    if link.opened {
                        self.engine.borrow_mut().on_disconnected(link.addr);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_report(&mut self, cid: u16, p: &[u8]) {
        let Some(len) = le16(p, 5) else { return };
        let Some(report) = p.get(7..7 + len as usize) else { return };
        // The HID header: DATA, input report
        let Some((&0xA1, data)) = report.split_first() else { return };
        let Some(link) = self.links.get_mut(&cid).filter(|l| l.opened) else {
            self.engine.borrow().counters().reports.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.engine.borrow().input(link.addr, link.decoder.decoder.as_mut(), |d| d.decode(data));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_are_kept_per_device_and_forgotten() {
        let dir = std::env::temp_dir().join(format!("bc-hid-desc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, _rx) = std::sync::mpsc::channel();
        let config = HidConfig { dry_run: true, descriptor_dir: Some(dir.clone()), ..HidConfig::default() };
        let engine = HidEngine::new(Some(&config), Arc::default(), tx);
        let a = [0, 0x11, 0x22, 0x33, 0x44, 0x55];
        // A keyboard without report IDs: modifiers, reserved, 6 keys
        let keyboard = [
            0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01,
            0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x26, 0xFF,
            0x00, 0x05, 0x07, 0x19, 0x00, 0x2A, 0xFF, 0x00, 0x81, 0x00, 0xC0,
        ];
        assert!(engine.borrow().stored(&a, 0).decoder.is_none());
        let mut d = DeviceDecoder::default();
        engine.borrow().learn(&a, 0, &mut d, &keyboard);
        assert!(d.decoder.is_some());
        engine.borrow().learn(&a, 1, &mut DeviceDecoder::default(), &keyboard);
        assert!(engine.borrow().stored(&a, 0).decoder.is_some());
        assert!(descriptor_file(&dir, &a, 1).exists());
        // A key held, then the same descriptor again: the decoder stays
        engine.borrow().input(a, d.decoder.as_mut(), |d| d.decode(&[0, 0, 4, 0, 0, 0, 0, 0]));
        engine.borrow().learn(&a, 0, &mut d, &keyboard);
        assert!(d.decoder.as_ref().is_some_and(|d| d.any_held()));
        // Garbage in the file is not used
        std::fs::write(descriptor_file(&dir, &a, 0), [0xFF; 3]).unwrap();
        assert!(engine.borrow().stored(&a, 0).decoder.is_none());
        forget_descriptors(&dir, &a);
        assert!(!descriptor_file(&dir, &a, 0).exists() && !descriptor_file(&dir, &a, 1).exists());
        let c = engine.borrow().counters().events.load(Ordering::Relaxed);
        assert_eq!(c, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_handles_are_kept_moved_to_the_identity_and_forgotten() {
        let dir = std::env::temp_dir().join(format!("bc-hid-reports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        let rpa = LeAddr { addr: [0x4A, 0x11, 0x22, 0x33, 0x44, 0x55], addr_type: LeAddr::RANDOM };
        let identity = LeAddr { addr: [0x00, 0xA2, 0xD0, 0, 0, 5], addr_type: LeAddr::PUBLIC };
        let config = HidConfig {
            dry_run: true,
            le_devices: vec![rpa],
            descriptor_dir: Some(dir.clone()),
            ..HidConfig::default()
        };
        let engine = HidEngine::new(Some(&config), Arc::default(), tx);
        assert!(engine.borrow().kept_reports(&rpa.addr).is_empty());
        let reports = [
            KeptReport { instance: 0, value_handle: 0x12, id: 1, kind: 1 },
            KeptReport { instance: 0, value_handle: 0x16, id: 1, kind: 2 },
            KeptReport { instance: 1, value_handle: 0x31, id: 0, kind: 1 },
        ];
        engine.borrow().keep_reports(&rpa.addr, &reports);
        assert_eq!(engine.borrow().kept_reports(&rpa.addr), reports);
        std::fs::write(descriptor_file(&dir, &rpa.addr, 0), [1, 2, 3]).unwrap();
        // Paired: listed with its identity, the files move along
        engine.borrow_mut().on_identity(rpa, identity);
        assert_eq!(engine.borrow().le, [identity]);
        assert!(matches!(rx.try_recv(), Ok(HostEvent::Hid(HidEvent::Identity { listed, identity: i }))
            if listed == rpa && i == identity));
        assert!(engine.borrow().kept_reports(&rpa.addr).is_empty());
        assert_eq!(engine.borrow().kept_reports(&identity.addr), reports);
        assert!(descriptor_file(&dir, &identity.addr, 0).exists());
        // Bad lines are dropped, good ones read
        std::fs::write(reports_file(&dir, &identity.addr), "0 18 1 1\nx\n0 0 1 1\n0 20 1 9\n1 49 0 1 7\n2 50 3 1\n")
            .unwrap();
        let read = engine.borrow().kept_reports(&identity.addr);
        assert_eq!(read, [reports[0], KeptReport { instance: 2, value_handle: 50, id: 3, kind: 1 }]);
        forget_descriptors(&dir, &identity.addr);
        assert!(!reports_file(&dir, &identity.addr).exists());
        assert!(!descriptor_file(&dir, &identity.addr, 0).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The engine tells the sink whose input a batch is: only that
    /// device's delivery is recorded; releases and undecoded reports are
    /// nobody's
    #[test]
    fn delivery_is_tracked_per_device() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let config = HidConfig { dry_run: true, ..HidConfig::default() };
        let sink = sink_for(&config);
        let config = HidConfig { sink: Some(sink.clone()), ..config };
        let engine = HidEngine::new(Some(&config), Arc::default(), tx);
        let (a, b) = ([0, 1, 2, 3, 4, 5], [0, 1, 2, 3, 4, 6]);
        let mut d = Decoder::boot_keyboard();
        engine.borrow().input(b, None, |d| d.decode(&[0, 0, 4, 0, 0, 0, 0, 0]));
        assert_eq!(sink.delivered(&b), None, "undecoded");
        engine.borrow().input(a, Some(&mut d), |d| d.decode(&[0, 0, 4, 0, 0, 0, 0, 0]));
        assert!(sink.delivered(&a).is_some());
        assert_eq!(sink.delivered(&b), None);
        engine.borrow().release(Some(&mut d));
        assert_eq!(sink.delivered(&b), None);
        // A report with no change passes nothing
        let before = sink.delivered(&a);
        engine.borrow().input(a, Some(&mut d), |d| d.decode(&[0, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(sink.delivered(&a), before);
    }

    #[test]
    fn a_link_is_ready_once_with_the_kind_of_its_descriptor() {
        let (tx, rx) = std::sync::mpsc::channel();
        let engine = HidEngine::new(Some(&HidConfig { dry_run: true, ..HidConfig::default() }), Arc::default(), tx);
        let a = [0, 1, 2, 3, 4, 5];
        let mut link = Link::new(a);
        // Not open yet, then open without a descriptor: not ready
        link.ready_now(&engine.borrow(), true);
        link.opened = true;
        link.ready_now(&engine.borrow(), false);
        assert!(rx.try_recv().is_err());
        link.decoder.decoder = Some(Decoder::boot_mouse());
        link.ready_now(&engine.borrow(), false);
        link.ready_now(&engine.borrow(), true);
        let ready: Vec<_> = rx.try_iter().collect();
        assert!(matches!(&ready[..], [HostEvent::Hid(HidEvent::Ready(x, k))] if *x == a && k.pointer && !k.keyboard));
        // An unusable descriptor: ready, as neither
        let mut bad = Link::new(a);
        bad.opened = true;
        bad.ready_now(&engine.borrow(), true);
        assert!(matches!(rx.try_recv(), Ok(HostEvent::Hid(HidEvent::Ready(_, k))) if k == InputKind::default()));
    }

    #[test]
    fn dry_run_lights_come_from_the_test_hook_and_no_keys_are_kept() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let section = Arc::new(HeldSection::create().unwrap());
        let config = HidConfig { dry_run: true, leds: Some(0xFD), held: Some(section.clone()), ..HidConfig::default() };
        let engine = HidEngine::new(Some(&config), Arc::default(), tx.clone());
        // Only the lights there are
        assert_eq!(engine.borrow().leds(), Some(0x05));
        // Windows' lock keys are never read in dry run
        assert!(!engine.borrow_mut().poll_leds());
        assert_eq!(engine.borrow().leds(), Some(0x05));
        engine.borrow().pass(Some([1; 6]), &[InputEvent::Key { usage: 0x04, down: true }]);
        assert!(section.load().is_empty());
        engine.borrow().leds_sent(2);
        assert_eq!(engine.borrow().counters().leds.load(Ordering::Relaxed), 2);
        // Without the hook nothing is sent
        let plain = HidEngine::new(Some(&HidConfig { dry_run: true, ..HidConfig::default() }), Arc::default(), tx);
        assert_eq!(plain.borrow().leds(), None);
    }
}
