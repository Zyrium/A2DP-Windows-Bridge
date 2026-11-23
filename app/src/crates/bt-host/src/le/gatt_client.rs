//! Serialized GATT client operations (v1 le_audio::gatt_client).
//!
//! BTstack's gatt_client runs one query per connection at a time and
//! refuses a second one (GATT_CLIENT_IN_WRONG_STATE). Every GATT client
//! operation of the core and of the modules therefore goes through this
//! queue: a FIFO per connection, the next operation starts when the
//! previous one completed, and its callback gets the result. All on the
//! BTstack thread; callbacks too. Status: 0 = success, else an ATT error
//! code (e.g. [`ATT_ERROR_HCI_DISCONNECT_RECEIVED`] when the link went
//! down) or the BTstack status of a query that could not start. In a
//! callback, check that the device is still the same link
//! (`ctx.device(id)` and its `acl == con`).
//!
//! The queue itself does not call BTstack or the callbacks while a caller
//! adds an operation: the LE core starts the queued operations and runs the
//! callbacks of finished ones when the hook / callback that queued them
//! returned, so a callback never runs inside the call that queued it.

use std::collections::{HashMap, VecDeque};

use btstack_sys::Stack;
use btstack_sys::consts::{
    ERROR_CODE_SUCCESS, GATT_CLIENT_BUSY, GATT_CLIENT_CHARACTERISTICS_CONFIGURATION_INDICATION,
    GATT_CLIENT_CHARACTERISTICS_CONFIGURATION_NOTIFICATION, GATT_CLIENT_IN_WRONG_STATE, GATT_EVENT_QUERY_COMPLETE,
};
use btstack_sys::events;

use super::core::LeCtx;

/// Status of the operations still queued or running when a link goes down
pub const ATT_ERROR_HCI_DISCONNECT_RECEIVED: u8 = 0x1F;

/// ATT_PROPERTY_* of a characteristic declaration (those the core checks)
pub const PROPERTY_NOTIFY: u16 = 0x10;
pub const PROPERTY_INDICATE: u16 = 0x20;

/// A characteristic of the GATT cache
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GattChar {
    /// 0 for a 128-bit UUID
    pub uuid16: u16,
    /// BTstack order
    pub uuid128: [u8; 16],
    /// Declaration
    pub start_handle: u16,
    pub value_handle: u16,
    /// Last descriptor
    pub end_handle: u16,
    /// PROPERTY_*
    pub properties: u16,
}

/// A service of the GATT cache
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GattService {
    /// 0 for a 128-bit UUID
    pub uuid16: u16,
    pub uuid128: [u8; 16],
    pub start_handle: u16,
    pub end_handle: u16,
    /// false: secondary, found as an included service
    pub primary: bool,
    /// Start handle of the including service (secondary)
    pub included_by: u16,
    pub chars: Vec<GattChar>,
}

/// Completion of a write / subscribe: the status
pub type GattDone = Box<dyn FnOnce(&mut LeCtx<'_>, u8)>;
/// Completion of a read: the status and the whole value (long reads
/// included)
pub type GattValue = Box<dyn FnOnce(&mut LeCtx<'_>, u8, &[u8])>;
/// Completion of a read by type: the status and (handle, value) of every
/// attribute found
pub type GattValues = Box<dyn FnOnce(&mut LeCtx<'_>, u8, Vec<(u16, Vec<u8>)>)>;
/// A notification / indication value
pub type GattNotify = Box<dyn FnMut(&mut LeCtx<'_>, &[u8])>;
/// Completion of a discovery: the status and the services
pub type GattDiscovered = Box<dyn FnOnce(&mut LeCtx<'_>, u8, Vec<GattService>)>;

/// What the queue needs of BTstack's gatt_client (the Stack; a fake in the
/// tests). Each starts one query and returns its BTstack status.
pub(crate) trait GattBackend {
    fn read_long(&self, con: u16, value_handle: u16) -> u8;
    /// Every attribute of a type in a handle range (descriptors too)
    fn read_by_type(&self, con: u16, start_handle: u16, end_handle: u16, uuid16: u16) -> u8;
    /// With response; a long write when the value does not fit the MTU
    fn write(&self, con: u16, value_handle: u16, value: &[u8]) -> u8;
    /// Write command (ATT Write Without Response): no answer comes back
    fn write_without_response(&self, con: u16, value_handle: u16, value: &[u8]) -> u8;
    fn write_ccc(&self, con: u16, chr: &GattChar, configuration: u16) -> u8;
    fn discover_primary(&self, con: u16) -> u8;
    fn find_included(&self, con: u16, start_handle: u16, end_handle: u16) -> u8;
    fn discover_characteristics(&self, con: u16, start_handle: u16, end_handle: u16) -> u8;
}

impl GattBackend for Stack {
    fn read_long(&self, con: u16, value_handle: u16) -> u8 {
        self.gatt_read_long_value(con, value_handle)
    }

    fn read_by_type(&self, con: u16, start_handle: u16, end_handle: u16, uuid16: u16) -> u8 {
        self.gatt_read_by_type(con, start_handle, end_handle, uuid16)
    }

    fn write(&self, con: u16, value_handle: u16, value: &[u8]) -> u8 {
        self.gatt_write_value(con, value_handle, value)
    }

    fn write_without_response(&self, con: u16, value_handle: u16, value: &[u8]) -> u8 {
        self.gatt_write_without_response(con, value_handle, value)
    }

    fn write_ccc(&self, con: u16, chr: &GattChar, configuration: u16) -> u8 {
        let c = events::Characteristic {
            start_handle: chr.start_handle,
            value_handle: chr.value_handle,
            end_handle: chr.end_handle,
            properties: chr.properties,
            uuid16: chr.uuid16,
            uuid128: chr.uuid128,
        };
        self.gatt_write_client_configuration(con, &c, configuration)
    }

    fn discover_primary(&self, con: u16) -> u8 {
        self.gatt_discover_primary_services(con)
    }

    fn find_included(&self, con: u16, start_handle: u16, end_handle: u16) -> u8 {
        self.gatt_find_included_services(con, start_handle, end_handle)
    }

    fn discover_characteristics(&self, con: u16, start_handle: u16, end_handle: u16) -> u8 {
        self.gatt_discover_characteristics(con, start_handle, end_handle)
    }
}

/// A finished operation or a notification, for the core to hand to its
/// callback with an LeCtx. `handle` is the value handle of the operation
/// (0 for barriers).
pub(crate) enum Completion {
    Value { done: Option<GattValue>, status: u8, value: Vec<u8>, handle: u16 },
    Done { done: Option<GattDone>, status: u8, handle: u16 },
    Discovered { done: Option<GattDiscovered>, status: u8, services: Vec<GattService> },
    Values { done: Option<GattValues>, status: u8, values: Vec<(u16, Vec<u8>)> },
    Notify { con: u16, handle: u16, value: Vec<u8> },
}

impl Completion {
    /// Runs the callback (notifications: the listeners)
    pub(crate) fn run(self, ctx: &mut LeCtx<'_>) {
        match self {
            Completion::Value { done, status, value, handle } => {
                tracing::trace!("LE Audio: GATT read of 0x{handle:04x} done (0x{status:02x})");
                tracing::trace!("LE Audio: GATT value 0x{handle:04X}: {value:02X?}");
                if let Some(f) = done {
                    f(ctx, status, &value)
                }
            }
            Completion::Done { done, status, handle } => {
                tracing::trace!("LE Audio: GATT operation on 0x{handle:04x} done (0x{status:02x})");
                if let Some(f) = done {
                    f(ctx, status)
                }
            }
            Completion::Discovered { done, status, services } => {
                tracing::debug!("LE Audio: GATT discovery done (0x{status:02X}), {} service(s)", services.len());
                if let Some(f) = done {
                    f(ctx, status, services)
                }
            }
            Completion::Values { done, status, values } => {
                tracing::trace!("LE Audio: GATT read by type done (0x{status:02x}), {} value(s)", values.len());
                if let Some(f) = done {
                    f(ctx, status, values)
                }
            }
            Completion::Notify { con, handle, value } => {
                tracing::trace!("LE Audio: GATT notification 0x{handle:04X} on 0x{con:04X}: {value:02X?}");
                let Some(mut listeners) = ctx.core.gatt.take_listeners(con, handle) else { return };
                for cb in listeners.iter_mut() {
                    cb(ctx, &value);
                }
                ctx.core.gatt.restore_listeners(con, handle, listeners);
            }
        }
    }

    #[cfg(test)]
    fn summary(&self) -> (u16, u8) {
        match self {
            Completion::Value { status, handle, .. } | Completion::Done { status, handle, .. } => (*handle, *status),
            Completion::Discovered { status, .. } | Completion::Values { status, .. } => (0, *status),
            Completion::Notify { handle, .. } => (*handle, 0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Primary,
    Included,
    Chars,
}

/// What follows a successful step of an operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Finished, with this status
    Done(u8),
    /// Its next step is running
    Running,
    /// Its next step found gatt_client busy: started again shortly
    Busy,
}

/// Starts discovery step `step` at `index`, moving past steps with nothing
/// to ask (included services once every primary service was asked); its
/// BTstack status, None when the discovery is complete
fn discover_step(
    b: &dyn GattBackend,
    con: u16,
    step: &mut Step,
    index: &mut usize,
    services: &[GattService],
) -> Option<u8> {
    let primaries = services.iter().filter(|s| s.primary).count();
    loop {
        match *step {
            Step::Primary => return Some(b.discover_primary(con)),
            Step::Included if *index >= primaries => {
                *step = Step::Chars;
                *index = 0;
            }
            Step::Included => {
                let s = &services[*index];
                return Some(b.find_included(con, s.start_handle, s.end_handle));
            }
            Step::Chars if *index >= services.len() => return None,
            Step::Chars => {
                let s = &services[*index];
                return Some(b.discover_characteristics(con, s.start_handle, s.end_handle));
            }
        }
    }
}

/// One queued operation
enum Op {
    Read {
        handle: u16,
        value: Vec<u8>,
        done: Option<GattValue>,
    },
    Write {
        handle: u16,
        value: Vec<u8>,
        done: Option<GattDone>,
    },
    /// Write command: done as soon as BTstack accepted it
    WriteNoResponse {
        handle: u16,
        value: Vec<u8>,
        done: Option<GattDone>,
    },
    ReadByType {
        start: u16,
        end: u16,
        uuid16: u16,
        values: Vec<(u16, Vec<u8>)>,
        done: Option<GattValues>,
    },
    Subscribe {
        chr: GattChar,
        config: u16,
        done: Option<GattDone>,
    },
    /// Primary services -> included services of each -> characteristics of
    /// each
    Discover {
        step: Step,
        index: usize,
        services: Vec<GattService>,
        done: Option<GattDiscovered>,
    },
    /// Completes in queue order with status 0 (crate-internal: "when all
    /// queued before it finished")
    Barrier {
        done: Option<GattDone>,
    },
}

impl Op {
    /// Starts the (first step of the) query; a BTstack status
    fn start(&mut self, b: &dyn GattBackend, con: u16) -> u8 {
        match self {
            Op::Read { handle, value, .. } => {
                value.clear();
                b.read_long(con, *handle)
            }
            Op::Write { handle, value, .. } => b.write(con, *handle, value),
            Op::WriteNoResponse { handle, value, .. } => b.write_without_response(con, *handle, value),
            Op::ReadByType { start, end, uuid16, values, .. } => {
                values.clear();
                b.read_by_type(con, *start, *end, *uuid16)
            }
            // A characteristic with room for one descriptor only: that is
            // its CCC (mandatory with notify / indicate), written without
            // looking for it first (one ATT round trip less)
            Op::Subscribe { chr, config, .. }
                if chr.end_handle == chr.value_handle.wrapping_add(1)
                    && chr.properties & (PROPERTY_NOTIFY | PROPERTY_INDICATE) != 0 =>
            {
                b.write(con, chr.end_handle, &config.to_le_bytes())
            }
            Op::Subscribe { chr, config, .. } => b.write_ccc(con, chr, *config),
            // Its first step, or the step a busy gatt_client held back
            // (always one with a query: a step is only held back once it
            // was found to have one)
            Op::Discover { step, index, services, .. } => {
                discover_step(b, con, step, index, services).unwrap_or(ERROR_CODE_SUCCESS)
            }
            Op::Barrier { .. } => ERROR_CODE_SUCCESS,
        }
    }

    /// Completes without a QUERY_COMPLETE event
    fn immediate(&self) -> bool {
        matches!(self, Op::Barrier { .. } | Op::WriteNoResponse { .. })
    }

    /// A GATT event of the running query (not QUERY_COMPLETE)
    fn on_event(&mut self, packet: &[u8]) {
        match self {
            Op::Read { value, .. } => {
                if let Some(v) = events::gatt_long_value_query_result(packet) {
                    let end = v.offset as usize + v.value.len();
                    if value.len() < end {
                        value.resize(end, 0);
                    }
                    value[v.offset as usize..end].copy_from_slice(v.value);
                }
            }
            Op::ReadByType { values, .. } => {
                if let Some((_, handle, v)) = events::gatt_characteristic_value_query_result(packet) {
                    values.push((handle, v.to_vec()));
                }
            }
            Op::Discover { services, index, .. } => {
                if let Some((_, s)) = events::gatt_service_query_result(packet) {
                    services.push(GattService {
                        uuid16: s.uuid16,
                        uuid128: s.uuid128,
                        start_handle: s.start_handle,
                        end_handle: s.end_handle,
                        primary: true,
                        included_by: 0,
                        chars: Vec::new(),
                    });
                } else if let Some((_, _, s)) = events::gatt_included_service_query_result(packet) {
                    let known = services.iter().any(|e| e.start_handle == s.start_handle);
                    let including = services.get(*index).map_or(0, |e| e.start_handle);
                    if !known {
                        services.push(GattService {
                            uuid16: s.uuid16,
                            uuid128: s.uuid128,
                            start_handle: s.start_handle,
                            end_handle: s.end_handle,
                            primary: false,
                            included_by: including,
                            chars: Vec::new(),
                        });
                    }
                } else if let Some((_, c)) = events::gatt_characteristic_query_result(packet)
                    && let Some(svc) = services.get_mut(*index)
                {
                    svc.chars.push(GattChar {
                        uuid16: c.uuid16,
                        uuid128: c.uuid128,
                        start_handle: c.start_handle,
                        value_handle: c.value_handle,
                        end_handle: c.end_handle,
                        properties: c.properties,
                    });
                }
            }
            _ => {}
        }
    }

    /// QUERY_COMPLETE with success: what follows
    fn on_complete(&mut self, b: &dyn GattBackend, con: u16) -> Next {
        let Op::Discover { step, index, services, .. } = self else { return Next::Done(ERROR_CODE_SUCCESS) };
        if *step == Step::Primary {
            *step = Step::Included;
            *index = 0;
        } else {
            *index += 1;
        }
        let Some(status) = discover_step(b, con, step, index, services) else {
            return Next::Done(ERROR_CODE_SUCCESS);
        };
        match status {
            ERROR_CODE_SUCCESS => {
                tracing::trace!("LE Audio: GATT discovery {step:?} of service {index} on 0x{con:04X}");
                Next::Running
            }
            GATT_CLIENT_BUSY | GATT_CLIENT_IN_WRONG_STATE => Next::Busy,
            // Finished with what it found so far, it would pass for the
            // device's services (issue #86: a partial list replaced the
            // kept one): the discovery fails instead
            s => {
                tracing::warn!("LE Audio: GATT discovery step failed to start (0x{s:02x})");
                Next::Done(s)
            }
        }
    }

    fn finish(self, status: u8) -> Option<Completion> {
        Some(match self {
            Op::Read { handle, value, done } => Completion::Value { done, status, value, handle },
            Op::Write { handle, done, .. } | Op::WriteNoResponse { handle, done, .. } => {
                Completion::Done { done, status, handle }
            }
            Op::ReadByType { values, done, .. } => Completion::Values { done, status, values },
            Op::Subscribe { chr, done, .. } => Completion::Done { done, status, handle: chr.value_handle },
            Op::Discover { services, done, .. } => Completion::Discovered { done, status, services },
            Op::Barrier { done } => Completion::Done { done, status, handle: 0 },
        })
    }
}

#[derive(Default)]
struct Conn {
    queue: VecDeque<Op>,
    /// queue.front() started, waiting for its completion
    running: bool,
    /// The last start found gatt_client busy: try again shortly
    retry: bool,
    listeners: HashMap<u16, Vec<GattNotify>>,
}

/// The per-connection queue (in the LE core; reach it with `ctx.gatt()`)
pub struct GattClient {
    /// Operations queued or running per connection
    conns: HashMap<u16, Conn>,
    /// Finished operations whose callbacks did not run yet
    ready: Vec<Completion>,
}

impl GattClient {
    /// Read a characteristic value (a long read when it does not fit the
    /// MTU)
    pub fn read(&mut self, con: u16, value_handle: u16, done: GattValue) {
        self.enqueue(con, Op::Read { handle: value_handle, value: Vec::new(), done: Some(done) });
    }

    /// Every attribute of type `uuid16` in a handle range, with its handle
    /// (ATT Read By Type: finds descriptors without discovering them). No
    /// such attribute: status 0x0A (Attribute Not Found).
    pub fn read_by_type(&mut self, con: u16, start: u16, end: u16, uuid16: u16, done: GattValues) {
        self.enqueue(con, Op::ReadByType { start, end, uuid16, values: Vec::new(), done: Some(done) });
    }

    /// Write with response (a long write when the value does not fit the
    /// MTU)
    pub fn write(&mut self, con: u16, value_handle: u16, value: Vec<u8>, done: Option<GattDone>) {
        self.enqueue(con, Op::Write { handle: value_handle, value, done });
    }

    /// Write without response (ATT Write Command), for characteristics that
    /// only list that property. `done` gets 0 once BTstack sent it, or the
    /// BTstack status when it could not.
    pub fn write_without_response(&mut self, con: u16, value_handle: u16, value: Vec<u8>, done: Option<GattDone>) {
        self.enqueue(con, Op::WriteNoResponse { handle: value_handle, value, done });
    }

    /// Enable notifications (or indications) of a characteristic: writes its
    /// CCC descriptor. Values arrive at the [`GattClient::listen`]
    /// callbacks.
    pub fn subscribe(&mut self, con: u16, chr: &GattChar, indications: bool, done: Option<GattDone>) {
        let config = if indications {
            GATT_CLIENT_CHARACTERISTICS_CONFIGURATION_INDICATION
        } else {
            GATT_CLIENT_CHARACTERISTICS_CONFIGURATION_NOTIFICATION
        };
        self.enqueue(con, Op::Subscribe { chr: chr.clone(), config, done });
    }

    /// Discover all primary services, their included (secondary) services
    /// and all characteristics. The core runs this once per connection into
    /// `LeDevice::services`; modules use that cache.
    pub fn discover_all(&mut self, con: u16, done: GattDiscovered) {
        self.enqueue(con, Op::Discover { step: Step::Primary, index: 0, services: Vec::new(), done: Some(done) });
    }

    /// Notifications / indications of `value_handle` on `con`. Dropped when
    /// the connection goes down.
    pub fn listen(&mut self, con: u16, value_handle: u16, cb: GattNotify) {
        if let Some(c) = self.conns.get_mut(&con) {
            c.listeners.entry(value_handle).or_default().push(cb);
        }
    }

    /// Operations queued or running on `con`
    #[cfg(test)]
    pub fn pending(&self, con: u16) -> usize {
        self.conns.get(&con).map_or(0, |c| c.queue.len())
    }

    // ---- core only ----

    pub(crate) fn new() -> GattClient {
        GattClient { conns: HashMap::new(), ready: Vec::new() }
    }

    /// `done(0)` once everything queued on `con` before it has completed
    /// (v1 used a custom query that failed to start for this)
    pub(crate) fn barrier(&mut self, con: u16, done: GattDone) {
        self.enqueue(con, Op::Barrier { done: Some(done) });
    }

    /// A new LE connection
    pub(crate) fn attach(&mut self, con: u16) {
        self.conns.entry(con).or_default();
    }

    /// The connection went down: the running and queued operations
    /// complete with ATT_ERROR_HCI_DISCONNECT_RECEIVED and the listeners
    /// are dropped
    pub(crate) fn detach(&mut self, con: u16) {
        let Some(c) = self.conns.remove(&con) else { return };
        tracing::debug!("LE Audio: GATT client detached from 0x{con:04X}, {} operation(s) cut short", c.queue.len());
        for op in c.queue {
            self.ready.extend(op.finish(ATT_ERROR_HCI_DISCONNECT_RECEIVED));
        }
    }

    /// All connections (stack deinit)
    pub(crate) fn detach_all(&mut self) {
        let cons: Vec<u16> = self.conns.keys().copied().collect();
        for con in cons {
            self.detach(con);
        }
    }

    /// Callbacks to run (finished operations), in order
    pub(crate) fn take_ready(&mut self) -> Vec<Completion> {
        std::mem::take(&mut self.ready)
    }

    /// Whether a start found gatt_client busy (the core retries shortly)
    pub(crate) fn wants_retry(&self) -> bool {
        self.conns.values().any(|c| c.retry && !c.queue.is_empty())
    }

    /// Starts the queued operations of every connection (until one runs)
    pub(crate) fn pump_all(&mut self, b: &dyn GattBackend) {
        let cons: Vec<u16> = self.conns.keys().copied().collect();
        for con in cons {
            self.pump(b, con);
        }
    }

    fn pump(&mut self, b: &dyn GattBackend, con: u16) {
        loop {
            let Some(c) = self.conns.get_mut(&con) else { return };
            if c.running {
                return;
            }
            let queued = c.queue.len();
            let Some(op) = c.queue.front_mut() else { return };
            let status = op.start(b, con);
            tracing::trace!(
                "LE Audio: GATT {} on 0x{con:04X} started (0x{status:02X}), {} queued",
                match op {
                    Op::Read { handle, .. } => format!("read 0x{handle:04X}"),
                    Op::Write { handle, value, .. } => format!("write 0x{handle:04X} {value:02X?}"),
                    Op::WriteNoResponse { handle, value, .. } => {
                        format!("write-no-resp 0x{handle:04X} {value:02X?}")
                    }
                    Op::ReadByType { start, end, uuid16, .. } =>
                        format!("read by type 0x{uuid16:04X} in 0x{start:04X}-0x{end:04X}"),
                    Op::Subscribe { chr, config, .. } => format!("CCC 0x{config:04X} of 0x{:04X}", chr.value_handle),
                    Op::Discover { .. } => "discovery".to_string(),
                    Op::Barrier { .. } => "barrier".to_string(),
                },
                queued
            );
            if status == GATT_CLIENT_BUSY || status == GATT_CLIENT_IN_WRONG_STATE {
                // Another gatt_client user (or a query BTstack runs itself,
                // e.g. the MTU exchange) is active: try again shortly
                tracing::debug!("LE Audio: gatt_client busy on 0x{con:04X} (0x{status:02X}), retrying");
                c.retry = true;
                return;
            }
            if status != ERROR_CODE_SUCCESS {
                tracing::debug!("LE Audio: GATT operation on 0x{con:04X} could not start (0x{status:02X})");
            }
            c.retry = false;
            if status != ERROR_CODE_SUCCESS || op.immediate() {
                if let Some(op) = c.queue.pop_front() {
                    self.ready.extend(op.finish(status));
                }
                continue;
            }
            c.running = true;
        }
    }

    fn enqueue(&mut self, con: u16, op: Op) {
        match self.conns.get_mut(&con) {
            Some(c) => c.queue.push_back(op),
            None => self.ready.extend(op.finish(ATT_ERROR_HCI_DISCONNECT_RECEIVED)),
        }
    }

    /// A GATT client event (Source::GattClient)
    pub(crate) fn on_event(&mut self, b: &dyn GattBackend, packet: &[u8]) {
        if let Some(u) = events::gatt_value_update(packet) {
            let listened = self.conns.get(&u.con).is_some_and(|c| c.listeners.contains_key(&u.value_handle));
            if listened {
                self.ready.push(Completion::Notify { con: u.con, handle: u.value_handle, value: u.value.to_vec() });
            }
            return;
        }
        let Some(con) = events::gatt_con(packet) else { return };
        let Some(c) = self.conns.get_mut(&con) else { return };
        if !c.running || c.queue.is_empty() {
            return;
        }
        if packet.first() == Some(&GATT_EVENT_QUERY_COMPLETE) {
            let status = events::gatt_query_complete(packet).map_or(0xFF, |(_, s)| s);
            if status != ERROR_CODE_SUCCESS {
                tracing::debug!("LE Audio: GATT query on 0x{con:04X} completed with ATT 0x{status:02X}");
            }
            self.complete_front(b, con, status);
            return;
        }
        if let Some(op) = c.queue.front_mut() {
            op.on_event(packet);
        }
    }

    fn complete_front(&mut self, b: &dyn GattBackend, con: u16, status: u8) {
        let Some(c) = self.conns.get_mut(&con) else { return };
        let Some(op) = c.queue.front_mut() else { return };
        let next = if status == ERROR_CODE_SUCCESS { op.on_complete(b, con) } else { Next::Done(status) };
        c.running = false;
        match next {
            // The next step of a multi-step operation is running
            Next::Running => c.running = true,
            // pump starts it again (Op::start goes on at that step)
            Next::Busy => {
                tracing::debug!("LE Audio: gatt_client busy on 0x{con:04X} during discovery, retrying");
                c.retry = true;
            }
            Next::Done(status) => {
                if let Some(op) = c.queue.pop_front() {
                    self.ready.extend(op.finish(status));
                }
                self.pump(b, con);
            }
        }
    }

    /// The listeners of a value, taken out while they run
    pub(crate) fn take_listeners(&mut self, con: u16, handle: u16) -> Option<Vec<GattNotify>> {
        self.conns.get_mut(&con)?.listeners.remove(&handle)
    }

    /// Puts them back in front of any registered meanwhile (not if the link
    /// went down meanwhile)
    pub(crate) fn restore_listeners(&mut self, con: u16, handle: u16, mut listeners: Vec<GattNotify>) {
        let Some(c) = self.conns.get_mut(&con) else { return };
        let entry = c.listeners.entry(handle).or_default();
        listeners.append(entry);
        *entry = listeners;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use btstack_sys::consts::*;

    use super::*;

    /// Records the queries started; answers with the queued statuses (0
    /// when none is queued)
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        status: RefCell<VecDeque<u8>>,
    }

    impl Fake {
        fn answer(&self, call: String) -> u8 {
            self.calls.borrow_mut().push(call);
            self.status.borrow_mut().pop_front().unwrap_or(0)
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow_mut().drain(..).collect()
        }
    }

    impl GattBackend for Fake {
        fn read_long(&self, con: u16, h: u16) -> u8 {
            self.answer(format!("read {con:x} {h:x}"))
        }
        fn read_by_type(&self, con: u16, s: u16, e: u16, u: u16) -> u8 {
            self.answer(format!("type {con:x} {s:x}-{e:x} {u:x}"))
        }
        fn write(&self, con: u16, h: u16, v: &[u8]) -> u8 {
            self.answer(format!("write {con:x} {h:x} {v:?}"))
        }
        fn write_without_response(&self, con: u16, h: u16, v: &[u8]) -> u8 {
            self.answer(format!("wnr {con:x} {h:x} {v:?}"))
        }
        fn write_ccc(&self, con: u16, chr: &GattChar, cfg: u16) -> u8 {
            self.answer(format!("ccc {con:x} {:x} {cfg}", chr.value_handle))
        }
        fn discover_primary(&self, con: u16) -> u8 {
            self.answer(format!("primary {con:x}"))
        }
        fn find_included(&self, con: u16, s: u16, e: u16) -> u8 {
            self.answer(format!("included {con:x} {s:x}-{e:x}"))
        }
        fn discover_characteristics(&self, con: u16, s: u16, e: u16) -> u8 {
            self.answer(format!("chars {con:x} {s:x}-{e:x}"))
        }
    }

    fn complete(con: u16, status: u8) -> Vec<u8> {
        vec![GATT_EVENT_QUERY_COMPLETE, 7, con as u8, (con >> 8) as u8, 0, 0, 0, 0, status]
    }

    fn summaries(g: &mut GattClient) -> Vec<(u16, u8)> {
        g.take_ready().iter().map(Completion::summary).collect()
    }

    fn read(g: &mut GattClient, con: u16, h: u16) {
        g.read(con, h, Box::new(|_, _, _| {}));
    }

    /// A 16-bit UUID in the Bluetooth base, on air (least significant
    /// octet first)
    fn uuid_on_air(uuid: u16) -> Vec<u8> {
        let mut u = [0u8, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0x80, 0x5F, 0x9B, 0x34, 0xFB];
        u[2..4].copy_from_slice(&uuid.to_be_bytes());
        u.iter().rev().copied().collect()
    }

    #[test]
    fn a_lone_descriptor_is_written_as_the_ccc() {
        let b = Fake::default();
        let mut g = GattClient::new();
        g.attach(0x40);
        let chr = |value_handle: u16, end_handle: u16, properties: u16| GattChar {
            value_handle,
            end_handle,
            properties,
            ..Default::default()
        };
        // One handle after the value: written directly
        g.subscribe(0x40, &chr(0x21, 0x22, PROPERTY_NOTIFY), false, None);
        g.pump_all(&b);
        assert_eq!(b.calls(), ["write 40 22 [1, 0]"]);
        g.on_event(&b, &complete(0x40, 0));
        // Indications
        g.subscribe(0x40, &chr(0x24, 0x25, PROPERTY_INDICATE), true, None);
        g.pump_all(&b);
        assert_eq!(b.calls(), ["write 40 25 [2, 0]"]);
        g.on_event(&b, &complete(0x40, 0));
        // More room for descriptors: BTstack looks for the CCC
        g.subscribe(0x40, &chr(0x27, 0x29, PROPERTY_NOTIFY), false, None);
        g.pump_all(&b);
        assert_eq!(b.calls(), ["ccc 40 27 1"]);
        g.on_event(&b, &complete(0x40, 0));
        // Without notify / indicate in its properties: left to BTstack
        g.subscribe(0x40, &chr(0x2b, 0x2c, 0), false, None);
        g.pump_all(&b);
        assert_eq!(b.calls(), ["ccc 40 2b 1"]);
    }

    #[test]
    fn one_query_per_connection_in_order() {
        let b = Fake::default();
        let mut g = GattClient::new();
        g.attach(0x40);
        g.attach(0x41);
        read(&mut g, 0x40, 0x10);
        g.write(0x40, 0x11, vec![1, 2], None);
        read(&mut g, 0x40, 0x13);
        read(&mut g, 0x41, 0x20);
        // Nothing starts before the core pumps (the caller's hook returned)
        assert!(b.calls().is_empty());
        assert_eq!(g.pending(0x40), 3);
        g.pump_all(&b);
        let mut calls = b.calls();
        calls.sort();
        assert_eq!(calls, ["read 40 10", "read 41 20"]);
        // The value comes in pieces (long read), then QUERY_COMPLETE
        let mut piece = vec![GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT, 0, 0x40, 0, 0, 0, 0, 0, 0x10, 0];
        piece.extend_from_slice(&[0, 0, 2, 0, 0xAA, 0xBB]);
        g.on_event(&b, &piece);
        g.on_event(&b, &complete(0x40, 0));
        let ready = g.take_ready();
        assert_eq!(ready.len(), 1);
        match &ready[0] {
            Completion::Value { status: 0, value, handle: 0x10, .. } => assert_eq!(value, &[0xAA, 0xBB]),
            _ => panic!("expected the read's value"),
        }
        // Then the write; its completion starts the next read at once
        assert_eq!(b.calls(), ["write 40 11 [1, 2]"]);
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["read 40 13"]);
        assert_eq!(summaries(&mut g), [(0x11, 0)]);
        assert_eq!(g.pending(0x40), 1);
        // An event of another connection does not complete this one
        g.on_event(&b, &complete(0x41, 0x0A));
        assert_eq!(summaries(&mut g), [(0x20, 0x0A)]);
        assert_eq!(g.pending(0x40), 1);
    }

    #[test]
    fn disconnect_completes_running_and_queued_with_0x1f() {
        let b = Fake::default();
        let mut g = GattClient::new();
        g.attach(0x40);
        read(&mut g, 0x40, 0x10);
        g.subscribe(0x40, &GattChar { value_handle: 0x21, ..Default::default() }, false, None);
        g.barrier(0x40, Box::new(|_, _| {}));
        g.listen(0x40, 0x21, Box::new(|_, _| {}));
        g.pump_all(&b);
        assert_eq!(b.calls(), ["read 40 10"]);
        g.detach(0x40);
        assert_eq!(summaries(&mut g), [(0x10, 0x1F), (0x21, 0x1F), (0, 0x1F)]);
        assert_eq!(g.pending(0x40), 0);
        // Late events of the link and new operations on it
        g.on_event(&b, &complete(0x40, 0));
        let notification = [GATT_EVENT_NOTIFICATION, 0, 0x40, 0, 0, 0, 0, 0, 0x21, 0, 1, 0, 7];
        g.on_event(&b, &notification);
        assert!(g.take_ready().is_empty());
        read(&mut g, 0x40, 0x10);
        assert_eq!(summaries(&mut g), [(0x10, 0x1F)]);
        g.pump_all(&b);
        assert!(b.calls().is_empty());
    }

    #[test]
    fn read_by_type_collects_every_attribute_found() {
        let b = Fake::default();
        let mut g = GattClient::new();
        g.attach(0x40);
        g.read_by_type(0x40, 0x10, 0x30, 0x2908, Box::new(|_, _, _| {}));
        g.pump_all(&b);
        assert_eq!(b.calls(), ["type 40 10-30 2908"]);
        let value = |h: u8, v: [u8; 2]| {
            [GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT, 0, 0x40, 0, 0, 0, 0, 0, h, 0, 2, 0, v[0], v[1]]
        };
        g.on_event(&b, &value(0x14, [1, 1]));
        g.on_event(&b, &value(0x18, [2, 1]));
        g.on_event(&b, &complete(0x40, 0));
        match &g.take_ready()[..] {
            [Completion::Values { status: 0, values, .. }] => {
                assert_eq!(values, &[(0x14, vec![1, 1]), (0x18, vec![2, 1])])
            }
            _ => panic!("expected the values"),
        }
    }

    #[test]
    fn notifications_go_to_listened_values_only() {
        let b = Fake::default();
        let mut g = GattClient::new();
        g.attach(0x40);
        g.listen(0x40, 0x21, Box::new(|_, _| {}));
        let notification = |h: u8| [GATT_EVENT_NOTIFICATION, 0, 0x40, 0, 0, 0, 0, 0, h, 0, 1, 0, 7];
        g.on_event(&b, &notification(0x21));
        g.on_event(&b, &notification(0x22));
        assert_eq!(summaries(&mut g), [(0x21, 0)]);
        let taken = g.take_listeners(0x40, 0x21).unwrap();
        g.listen(0x40, 0x21, Box::new(|_, _| {}));
        g.restore_listeners(0x40, 0x21, taken);
        assert_eq!(g.take_listeners(0x40, 0x21).map(|l| l.len()), Some(2));
    }

    #[test]
    fn busy_retries_failed_start_completes() {
        let b = Fake::default();
        let mut g = GattClient::new();
        g.attach(0x40);
        read(&mut g, 0x40, 0x10);
        read(&mut g, 0x40, 0x11);
        b.status.borrow_mut().extend([GATT_CLIENT_BUSY]);
        g.pump_all(&b);
        assert_eq!(b.calls(), ["read 40 10"]);
        assert!(g.wants_retry());
        assert!(g.take_ready().is_empty());
        // Retried: this time the start fails outright, the next one runs
        b.status.borrow_mut().extend([ERROR_CODE_COMMAND_DISALLOWED]);
        g.pump_all(&b);
        assert_eq!(b.calls(), ["read 40 10", "read 40 11"]);
        assert!(!g.wants_retry());
        assert_eq!(summaries(&mut g), [(0x10, ERROR_CODE_COMMAND_DISALLOWED)]);
    }

    #[test]
    fn discovery_walks_included_services_and_characteristics() {
        let b = Fake::default();
        let mut g = GattClient::new();
        g.attach(0x40);
        g.discover_all(0x40, Box::new(|_, _, _| {}));
        g.pump_all(&b);
        assert_eq!(b.calls(), ["primary 40"]);
        let service = |start: u8, end: u8, uuid: u16| {
            let mut p = vec![GATT_EVENT_SERVICE_QUERY_RESULT, 0, 0x40, 0, 0, 0, 0, 0, start, 0, end, 0];
            p.extend(uuid_on_air(uuid));
            p
        };
        g.on_event(&b, &service(0x10, 0x1F, 0x1853));
        g.on_event(&b, &service(0x20, 0x2F, 0x184E));
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["included 40 10-1f"]);
        // CAS includes CSIS 0x30-0x38
        let mut inc = vec![GATT_EVENT_INCLUDED_SERVICE_QUERY_RESULT, 0, 0x40, 0, 0, 0, 0, 0, 0x11, 0, 0x30, 0, 0x38, 0];
        inc.extend(uuid_on_air(0x1846));
        g.on_event(&b, &inc);
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["included 40 20-2f"]);
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["chars 40 10-1f"]);
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["chars 40 20-2f"]);
        let mut chr = vec![GATT_EVENT_CHARACTERISTIC_QUERY_RESULT, 0, 0x40, 0, 0, 0, 0, 0];
        chr.extend_from_slice(&[0x21, 0, 0x22, 0, 0x23, 0, 0x12, 0]);
        chr.extend(uuid_on_air(0x2BC4));
        g.on_event(&b, &chr);
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["chars 40 30-38"]);
        g.on_event(&b, &complete(0x40, 0));
        let ready = g.take_ready();
        match &ready[..] {
            [Completion::Discovered { status: 0, services, .. }] => {
                let s: Vec<_> = services.iter().map(|s| (s.uuid16, s.primary, s.included_by, s.chars.len())).collect();
                assert_eq!(s, [(0x1853, true, 0, 0), (0x184E, true, 0, 1), (0x1846, false, 0x10, 0)]);
                assert_eq!(services[1].chars[0].uuid16, 0x2BC4);
                assert_eq!(services[1].chars[0].value_handle, 0x22);
            }
            _ => panic!("expected one discovery result"),
        }
        assert_eq!(g.pending(0x40), 0);
    }

    /// One primary service found, then its included services are asked
    fn discovery_at_its_second_step(b: &Fake, g: &mut GattClient) {
        g.attach(0x40);
        g.discover_all(0x40, Box::new(|_, _, _| {}));
        g.pump_all(b);
        assert_eq!(b.calls(), ["primary 40"]);
        let mut p = vec![GATT_EVENT_SERVICE_QUERY_RESULT, 0, 0x40, 0, 0, 0, 0, 0, 0x10, 0, 0x1F, 0];
        p.extend(uuid_on_air(0x184E));
        g.on_event(b, &p);
    }

    #[test]
    fn a_step_that_cannot_start_fails_the_discovery() {
        // Issue #86: it ended as a success with what it had found so far,
        // a partial list that replaced the kept services
        let b = Fake::default();
        let mut g = GattClient::new();
        discovery_at_its_second_step(&b, &mut g);
        b.status.borrow_mut().push_back(ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER);
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["included 40 10-1f"]);
        match &g.take_ready()[..] {
            [Completion::Discovered { status, .. }] => assert_eq!(*status, ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER),
            _ => panic!("expected one discovery result"),
        }
        assert_eq!(g.pending(0x40), 0);
    }

    #[test]
    fn a_busy_step_is_started_again() {
        let b = Fake::default();
        let mut g = GattClient::new();
        discovery_at_its_second_step(&b, &mut g);
        b.status.borrow_mut().push_back(GATT_CLIENT_BUSY);
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["included 40 10-1f"]);
        assert!(g.take_ready().is_empty());
        assert!(g.wants_retry());
        // The retry goes on at that step, not from the start
        g.pump_all(&b);
        assert_eq!(b.calls(), ["included 40 10-1f"]);
        assert!(!g.wants_retry());
        g.on_event(&b, &complete(0x40, 0));
        assert_eq!(b.calls(), ["chars 40 10-1f"]);
        g.on_event(&b, &complete(0x40, 0));
        match &g.take_ready()[..] {
            [Completion::Discovered { status: 0, services, .. }] => assert_eq!(services.len(), 1),
            _ => panic!("expected one discovery result"),
        }
    }
}
