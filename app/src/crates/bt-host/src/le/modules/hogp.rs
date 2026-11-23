//! HID over GATT host (keyboards and mice over LE, experimental, developer
//! mode; issue #413). Only for devices on the HID list (the LE core hands
//! it those and nothing else); LE Audio devices never get here.
//!
//! Per HID service (a device may have several): the Report Map (read long:
//! it is usually larger than one ATT packet) gives the decoder, the Report
//! Reference descriptor of each Report characteristic its report ID and
//! type, and every Input report is subscribed to. A notification carries
//! the report without its ID: decoded with the ID of its characteristic.
//! The device stays in Report mode (its default; the boot protocol is not
//! used).
//!
//! Input goes through the HID engine shared with classic Bluetooth
//! ([`crate::hid::HidEngine`]): the same counters, passing to Windows or
//! only counting, and every key or button still held is released when the
//! link goes down or the stack stops. Logs: counts only.
//!
//! The report handles (with ID and type) are kept per device beside its
//! report descriptor ([`crate::hid::reports_file`]). A bonded device keeps
//! its notifications on across connections, and may send input as soon as
//! the link is encrypted again: the kept input handles are listened to from
//! then on ([`LeModule::on_encrypted`]), and the services read afterwards
//! correct them.

use std::collections::{BTreeMap, HashMap, HashSet};

use tracing::{info, warn};

use super::super::LeDeviceId;
use super::super::core::LeCtx;
use super::super::gatt_client::{ATT_ERROR_HCI_DISCONNECT_RECEIVED, GattChar, GattDone, GattService, PROPERTY_NOTIFY};
use super::super::module::{LeModule, LeModuleEntry};
use crate::hid::{DeviceDecoder, KeptReport, SharedHid};

const UUID_SERVICE_HID: u16 = 0x1812;
const UUID_REPORT_MAP: u16 = 0x2A4B;
const UUID_REPORT: u16 = 0x2A4D;
const UUID_REPORT_REFERENCE: u16 = 0x2908;
/// Report Reference: report type of an input report
const REPORT_TYPE_INPUT: u8 = 1;
/// Report Reference: report type of an output report (the keyboard lights)
const REPORT_TYPE_OUTPUT: u8 = 2;
/// Characteristic properties: Write Without Response, Write
const PROPERTY_WRITE_WITHOUT_RESPONSE: u16 = 0x04;
const PROPERTY_WRITE: u16 = 0x08;
/// ATT "Attribute Not Found": a Read By Type that found nothing
const ATT_ERROR_ATTRIBUTE_NOT_FOUND: u8 = 0x0A;

/// One HID service of a device
#[derive(Default)]
struct Instance {
    decoder: DeviceDecoder,
    /// Input report value handle -> report ID
    inputs: HashMap<u16, u8>,
    /// Output report ID -> value handle and properties (writable ones only)
    outputs: HashMap<u8, (u16, u16)>,
    /// The keyboard lights written last
    leds_sent: Option<u8>,
    /// The Report characteristics this connection found (value handle,
    /// ID, type); None until their references are read
    found: Option<Vec<(u16, u8, u8)>>,
}

struct Device {
    /// The address it is listed with
    addr: [u8; 6],
    con: u16,
    instances: Vec<Instance>,
    /// Input value handles listened to on this link (once each: a listener
    /// looks the handle up in every instance)
    listened: HashSet<u16>,
    /// Set up from the kept report handles when the link was encrypted,
    /// before its services were read
    early: bool,
    /// HidEvent::Ready went out for this connection
    ready: bool,
}

impl Device {
    /// What its HID services' descriptors carry
    fn kind(&self) -> hid_input::InputKind {
        self.instances.iter().filter_map(|i| i.decoder.decoder.as_ref()).map(|d| d.kind()).fold(
            hid_input::InputKind::default(),
            |a, b| hid_input::InputKind { keyboard: a.keyboard || b.keyboard, pointer: a.pointer || b.pointer },
        )
    }
}

pub struct HogpModule {
    hid: SharedHid,
    devices: BTreeMap<u32, Device>,
}

/// The ID and type of each Report characteristic, from the Report Reference
/// descriptors found (handle, value): a descriptor belongs to the
/// characteristic whose handles hold it. A single Report characteristic
/// without one is the device's only report (ID 0), an input report when it
/// notifies.
fn report_ids(reports: &[GattChar], references: &[(u16, Vec<u8>)]) -> Vec<(GattChar, u8, u8)> {
    let mut out = Vec::new();
    for c in reports {
        let reference = references
            .iter()
            .find(|(h, v)| *h > c.value_handle && *h <= c.end_handle && v.len() >= 2)
            .map(|(_, v)| (v[0], v[1]));
        match reference {
            Some((id, kind)) => out.push((c.clone(), id, kind)),
            None if reports.len() == 1 && c.properties & PROPERTY_NOTIFY != 0 => {
                out.push((c.clone(), 0, REPORT_TYPE_INPUT))
            }
            None => {}
        }
    }
    out
}

impl HogpModule {
    /// Lets go of everything the device still holds (its link is being
    /// closed)
    pub(crate) fn release(&mut self, dev: LeDeviceId) {
        let HogpModule { hid, devices } = self;
        if let Some(d) = devices.get_mut(&dev.0) {
            for i in &mut d.instances {
                hid.borrow().release(i.decoder.decoder.as_mut());
            }
        }
    }

    fn release_all(&mut self) {
        let HogpModule { hid, devices } = self;
        for d in devices.values_mut() {
            for i in &mut d.instances {
                hid.borrow().release(i.decoder.decoder.as_mut());
            }
        }
    }

    /// An input report notified: decoded by the HID service whose input it
    /// is now (the handles may have been corrected since it was listened to)
    fn on_input(ctx: &mut LeCtx<'_>, dev: LeDeviceId, con: u16, handle: u16, value: &[u8]) {
        let Some(m) = ctx.module_mut::<HogpModule>() else { return };
        let HogpModule { hid, devices } = m;
        let Some(d) = devices.get_mut(&dev.0).filter(|d| d.con == con) else { return };
        let Some((inst, id)) = d.instances.iter_mut().find_map(|i| i.inputs.get(&handle).copied().map(|id| (i, id)))
        else {
            return;
        };
        let from = d.addr;
        hid.borrow().input(from, inst.decoder.decoder.as_mut(), |decoder| decoder.decode_with_id(id, value));
    }

    /// Listens to these input handles on the device's link (each once)
    fn listen(ctx: &mut LeCtx<'_>, dev: LeDeviceId, con: u16, handles: impl IntoIterator<Item = u16>) {
        let Some(d) = ctx.module_mut::<HogpModule>().and_then(|m| m.devices.get_mut(&dev.0)).filter(|d| d.con == con)
        else {
            return;
        };
        let new: Vec<u16> = handles.into_iter().filter(|h| d.listened.insert(*h)).collect();
        let gatt = ctx.gatt();
        for handle in new {
            gatt.listen(con, handle, Box::new(move |ctx, value| HogpModule::on_input(ctx, dev, con, handle, value)));
        }
    }

    fn on_report_map(ctx: &mut LeCtx<'_>, dev: LeDeviceId, con: u16, instance: usize, status: u8, value: &[u8]) {
        let Some(m) = ctx.module_mut::<HogpModule>() else { return };
        let HogpModule { hid, devices } = m;
        let Some(d) = devices.get_mut(&dev.0).filter(|d| d.con == con) else { return };
        let addr = d.addr;
        let Some(inst) = d.instances.get_mut(instance) else { return };
        if status != 0 {
            if status != ATT_ERROR_HCI_DISCONNECT_RECEIVED {
                warn!("hid: reading the Report Map failed (0x{status:02x}): {}", kept(inst));
            }
            return;
        }
        hid.borrow().learn(&addr, instance, &mut inst.decoder, value);
        info!(
            "hid: Report Map of {} bytes, {}",
            value.len(),
            if inst.decoder.decoder.is_some() { "usable" } else { "not usable: input of this device is dropped" }
        );
        HogpModule::sync_leds(ctx);
    }

    /// Writes the keyboard lights to every LE keyboard that was not sent
    /// them yet (or other ones): once its output report is known, and
    /// whenever they change. Write Without Response when the
    /// characteristic has it, else Write.
    pub(crate) fn sync_leds(ctx: &mut LeCtx<'_>) {
        let Some(m) = ctx.module_mut::<HogpModule>() else { return };
        let Some(mask) = m.hid.borrow().leds() else { return };
        let mut writes = Vec::new();
        for (&key, d) in &m.devices {
            for (i, inst) in d.instances.iter().enumerate() {
                if inst.leds_sent == Some(mask) {
                    continue;
                }
                let Some(out) = inst.decoder.decoder.as_ref().and_then(|x| x.leds()) else { continue };
                let Some(&(handle, properties)) = inst.outputs.get(&out.report_id) else { continue };
                writes.push((key, i, d.con, handle, properties, out.report(mask)));
            }
        }
        if writes.is_empty() {
            return;
        }
        let hid = m.hid.clone();
        let mut sent = Vec::new();
        for (key, i, con, handle, properties, value) in writes {
            if properties & PROPERTY_WRITE_WITHOUT_RESPONSE != 0 {
                let status = ctx.stack().gatt_write_without_response(con, handle, &value);
                if status != 0 {
                    // Tried again at the next tick
                    tracing::debug!("hid: the keyboard lights were not written (0x{status:02x})");
                    continue;
                }
            } else {
                let done: GattDone = Box::new(|_, status| {
                    if status != 0 && status != ATT_ERROR_HCI_DISCONNECT_RECEIVED {
                        tracing::debug!("hid: an LE keyboard refused its lights (0x{status:02x})");
                    }
                });
                ctx.gatt().write(con, handle, value, Some(done));
            }
            sent.push((key, i));
        }
        if let Some(m) = ctx.module_mut::<HogpModule>() {
            for (key, i) in &sent {
                if let Some(inst) = m.devices.get_mut(key).and_then(|d| d.instances.get_mut(*i)) {
                    inst.leds_sent = Some(mask);
                }
            }
        }
        hid.borrow().leds_sent(sent.len());
    }

    #[allow(clippy::too_many_arguments)]
    fn on_references(
        ctx: &mut LeCtx<'_>,
        dev: LeDeviceId,
        con: u16,
        instance: usize,
        reports: Vec<GattChar>,
        status: u8,
        references: Vec<(u16, Vec<u8>)>,
    ) {
        if status != 0 && status != ATT_ERROR_ATTRIBUTE_NOT_FOUND {
            if status != ATT_ERROR_HCI_DISCONNECT_RECEIVED {
                warn!("hid: reading the Report References failed (0x{status:02x})");
            }
            return;
        }
        let found = report_ids(&reports, &references);
        let handles: Vec<(u16, u8, u8)> = found.iter().map(|(c, id, kind)| (c.value_handle, *id, *kind)).collect();
        let outputs: HashMap<u8, (u16, u16)> = found
            .iter()
            .filter(|(c, _, kind)| {
                *kind == REPORT_TYPE_OUTPUT && c.properties & (PROPERTY_WRITE | PROPERTY_WRITE_WITHOUT_RESPONSE) != 0
            })
            .map(|(c, id, _)| (*id, (c.value_handle, c.properties)))
            .collect();
        let inputs: Vec<(GattChar, u8)> = found
            .into_iter()
            .filter(|(c, _, kind)| *kind == REPORT_TYPE_INPUT && c.properties & PROPERTY_NOTIFY != 0)
            .map(|(c, id, _)| (c, id))
            .collect();
        {
            let Some(m) = ctx.module_mut::<HogpModule>() else { return };
            let Some(d) = m.devices.get_mut(&dev.0).filter(|d| d.con == con) else { return };
            let Some(inst) = d.instances.get_mut(instance) else { return };
            let before: HashMap<u16, u8> = std::mem::take(&mut inst.inputs);
            inst.inputs = inputs.iter().map(|(c, id)| (c.value_handle, *id)).collect();
            inst.outputs = outputs;
            inst.leds_sent = None;
            inst.found = Some(handles);
            if d.early && before != inst.inputs {
                info!("hid: the kept input report handles of HID service {} changed: corrected", instance + 1);
            }
            // Every HID service read: kept for the next connection
            let all: Option<Vec<KeptReport>> = d
                .instances
                .iter()
                .enumerate()
                .map(|(i, inst)| {
                    inst.found.as_ref().map(|f| {
                        f.iter().map(move |&(value_handle, id, kind)| KeptReport {
                            instance: i,
                            value_handle,
                            id,
                            kind,
                        })
                    })
                })
                .collect::<Option<Vec<_>>>()
                .map(|v| v.into_iter().flatten().collect());
            if let Some(all) = all {
                m.hid.borrow().keep_reports(&d.addr, &all);
            }
        }
        info!(
            "hid: {} report(s) in HID service {}, subscribing to {} input report(s)",
            reports.len(),
            instance + 1,
            inputs.len()
        );
        HogpModule::listen(ctx, dev, con, inputs.iter().map(|(c, _)| c.value_handle));
        let gatt = ctx.gatt();
        for (c, _) in &inputs {
            gatt.subscribe(
                con,
                c,
                false,
                Some(Box::new(|_, status| {
                    if status != 0 && status != ATT_ERROR_HCI_DISCONNECT_RECEIVED {
                        warn!("hid: an input report refused notifications (0x{status:02x})");
                    }
                })),
            );
        }
        HogpModule::ready_when_subscribed(ctx, dev, con);
        HogpModule::sync_leds(ctx);
    }

    /// Ready for input once every HID service's reports are subscribed to:
    /// told once per connection
    fn ready_when_subscribed(ctx: &mut LeCtx<'_>, dev: LeDeviceId, con: u16) {
        let Some(m) = ctx.module_mut::<HogpModule>() else { return };
        let Some(d) = m.devices.get_mut(&dev.0).filter(|d| d.con == con) else { return };
        if d.ready || d.instances.iter().any(|i| i.found.is_none()) {
            return;
        }
        d.ready = true;
        let (addr, kind) = (d.addr, d.kind());
        m.hid.borrow().on_ready(addr, kind);
    }
}

/// What decodes the input when the Report Map could not be read
fn kept(inst: &Instance) -> &'static str {
    if inst.decoder.decoder.is_some() { "the kept one is used" } else { "input of this device is dropped" }
}

impl LeModule for HogpModule {
    fn name(&self) -> &'static str {
        "HOGP"
    }

    fn for_hid(&self) -> bool {
        true
    }

    /// A bonded device keeps its notifications on: with the report handles
    /// kept from its last connection, its input is taken from now on, not
    /// only once its services are read
    fn on_encrypted(&mut self, ctx: &mut LeCtx<'_>, dev: LeDeviceId) {
        let Some(d) = ctx.device(dev).filter(|d| d.hid && d.connected()) else { return };
        let con = d.acl;
        let Some(addr) = [d.addr.addr, d.peer_addr.addr].into_iter().find(|a| self.hid.borrow().le_listed(a).is_some())
        else {
            return;
        };
        let kept: Vec<KeptReport> =
            self.hid.borrow().kept_reports(&addr).into_iter().filter(|r| r.kind == REPORT_TYPE_INPUT).collect();
        let Some(count) = kept.iter().map(|r| r.instance + 1).max() else { return };
        let mut instances: Vec<Instance> = (0..count)
            .map(|i| Instance { decoder: self.hid.borrow().stored(&addr, i), ..Instance::default() })
            .collect();
        for r in &kept {
            instances[r.instance].inputs.insert(r.value_handle, r.id);
        }
        let listened: HashSet<u16> = kept.iter().map(|r| r.value_handle).collect();
        let gatt = ctx.gatt();
        for &handle in &listened {
            gatt.listen(con, handle, Box::new(move |ctx, value| HogpModule::on_input(ctx, dev, con, handle, value)));
        }
        info!(
            "hid: an LE keyboard or mouse is connected again, its {} kept input report(s) are listened to",
            listened.len()
        );
        // Its notifications are still on from the last connection: ready
        let device = Device { addr, con, instances, listened, early: true, ready: true };
        let kind = device.kind();
        self.devices.insert(dev.0, device);
        self.hid.borrow_mut().on_connected(addr);
        self.hid.borrow().on_ready(addr, kind);
    }

    fn on_gatt_ready(&mut self, ctx: &mut LeCtx<'_>, dev: LeDeviceId) {
        let Some(d) = ctx.device(dev).filter(|d| d.hid && d.connected()) else { return };
        let con = d.acl;
        let Some(addr) = [d.addr.addr, d.peer_addr.addr].into_iter().find(|a| self.hid.borrow().le_listed(a).is_some())
        else {
            return;
        };
        let services: Vec<GattService> = d.services.iter().filter(|s| s.uuid16 == UUID_SERVICE_HID).cloned().collect();
        if services.is_empty() {
            warn!("hid: an LE keyboard or mouse has no HID service: its input cannot be read");
            return;
        }
        match self.devices.get_mut(&dev.0).filter(|d| d.con == con && d.early) {
            // Listened to since the encryption with the kept handles: its
            // decoders and listeners stay, the references correct them
            Some(d) => {
                for gone in d.instances.iter_mut().skip(services.len()) {
                    self.hid.borrow().release(gone.decoder.decoder.as_mut());
                }
                d.instances.truncate(services.len());
                while d.instances.len() < services.len() {
                    let i = d.instances.len();
                    d.instances.push(Instance { decoder: self.hid.borrow().stored(&addr, i), ..Instance::default() });
                }
                info!("hid: the LE keyboard or mouse has {} HID service(s), reading its reports", services.len());
            }
            None => {
                let instances: Vec<Instance> = (0..services.len())
                    .map(|i| Instance { decoder: self.hid.borrow().stored(&addr, i), ..Instance::default() })
                    .collect();
                self.devices.insert(
                    dev.0,
                    Device { addr, con, instances, listened: HashSet::new(), early: false, ready: false },
                );
                info!(
                    "hid: an LE keyboard or mouse is connected ({} HID service(s)), reading its reports",
                    services.len()
                );
                self.hid.borrow_mut().on_connected(addr);
            }
        }
        let gatt = ctx.gatt();
        for (i, s) in services.into_iter().enumerate() {
            match s.chars.iter().find(|c| c.uuid16 == UUID_REPORT_MAP) {
                Some(map) => gatt.read(
                    con,
                    map.value_handle,
                    Box::new(move |ctx, status, value| HogpModule::on_report_map(ctx, dev, con, i, status, value)),
                ),
                None => warn!("hid: HID service {} has no Report Map", i + 1),
            }
            let reports: Vec<GattChar> = s.chars.iter().filter(|c| c.uuid16 == UUID_REPORT).cloned().collect();
            gatt.read_by_type(
                con,
                s.start_handle,
                s.end_handle,
                UUID_REPORT_REFERENCE,
                Box::new(move |ctx, status, values| {
                    HogpModule::on_references(ctx, dev, con, i, reports, status, values)
                }),
            );
        }
    }

    fn on_disconnected(&mut self, _ctx: &mut LeCtx<'_>, dev: LeDeviceId, _reason: u8) {
        self.release(dev);
        if let Some(d) = self.devices.remove(&dev.0) {
            self.hid.borrow_mut().on_disconnected(d.addr);
            info!("hid: an LE keyboard or mouse disconnected ({} LE one(s) left)", self.devices.len());
        }
    }

    fn on_stack_deinit(&mut self) {
        self.release_all();
        self.devices.clear();
        self.hid.borrow_mut().clear_connected();
    }
}

pub fn create(hid: SharedHid) -> LeModuleEntry {
    LeModuleEntry { module: Box::new(HogpModule { hid, devices: BTreeMap::new() }), hooks: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(value_handle: u16, end_handle: u16, properties: u16) -> GattChar {
        GattChar { uuid16: UUID_REPORT, value_handle, end_handle, properties, ..GattChar::default() }
    }

    #[test]
    fn report_references_are_matched_by_handle() {
        // Keyboard input (CCC + reference), output (reference only), mouse
        // input
        let reports = [report(0x12, 0x14, 0x12), report(0x16, 0x17, 0x0E), report(0x19, 0x1B, 0x12)];
        let references = vec![(0x14, vec![1, 1]), (0x17, vec![1, 2]), (0x1B, vec![2, 1]), (0x30, vec![9, 1])];
        let found: Vec<(u16, u8, u8)> =
            report_ids(&reports, &references).into_iter().map(|(c, id, kind)| (c.value_handle, id, kind)).collect();
        assert_eq!(found, [(0x12, 1, 1), (0x16, 1, 2), (0x19, 2, 1)]);
        // One report and no reference: ID 0, input when it notifies
        let one = [report(0x12, 0x13, 0x12)];
        assert_eq!(report_ids(&one, &[]).into_iter().map(|(_, id, kind)| (id, kind)).collect::<Vec<_>>(), [(0, 1)]);
        // Several without references: nothing can be told
        assert!(report_ids(&reports, &[]).is_empty());
        // A reference too short is no reference
        assert!(report_ids(&reports[..2], &[(0x14, vec![1])]).is_empty());
    }
}
