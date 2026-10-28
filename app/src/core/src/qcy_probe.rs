//! `a2dpwb-core --cli --qcy-probe [seconds] [-d <address>]`: phase 0 of
//! controlling QCY earbuds (MeloBuds N70) from A2DPWB.
//!
//! It answers the open questions of the port with the real hardware, and
//! only reads: nothing here changes a setting of the earbuds.
//!
//! 1. Scan LE and list the advertisers carrying QCY manufacturer data
//!    (company id 0x521C): vendor id, battery, control address. Whether the
//!    earbuds advertise at all while the A2DP link is up is the first
//!    question.
//! 2. Connect to the first N70 (or to the address of `-d`) for GATT only:
//!    no pairing, as Windows does for them. Whether the control service
//!    answers without bonding is the second.
//! 3. Print the GATT cache, subscribe to the notification characteristic
//!    and send the read-only requests of OpenQCY's refresh (`FF 03 FE <op>`).
//!    Whether the link coexists with A2DP on the same adapter is the third.
//!
//! The protocol is the `qcy-protocol` crate; the GATT calls are
//! `BtHost::le_connect_plain` / `le_gatt_*`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use bt_host::{BtHost, GattChar, GattService, LeScanResult};
use qcy_protocol::advertisement::{self, Advertisement};
use qcy_protocol::commands;
use qcy_protocol::packet::{self, to_hex};
use qcy_protocol::state::QcyState;
use qcy_protocol::uuids::{self, uuid16_matches};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const GATT_TIMEOUT: Duration = Duration::from_secs(5);
/// OpenQCY waits 2.5 s for a response
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(2500);
/// How long notifications are collected after the last query
const SETTLE: Duration = Duration::from_millis(500);

/// An advertiser with QCY manufacturer data
struct Found {
    result: LeScanResult,
    advertisement: Advertisement,
}

pub fn run(host: &BtHost, seconds: u32, address: Option<[u8; 6]>) -> i32 {
    println!("Scanning for QCY earbuds ({seconds} s)...");
    let found = scan(host, seconds);
    if found.is_empty() {
        println!("No QCY advertiser seen.");
        println!("The earbuds advertise their control channel when the case is opened or a bud is taken out;");
        println!("if they only advertise while no audio link is up, that is a finding too.");
        return 1;
    }
    for f in &found {
        println!("{}", describe(f));
    }

    let Some(target) = choose(&found, address) else {
        match address {
            Some(a) => println!("{} is not among the QCY advertisers above.", advertisement::format_address(to_u64(a))),
            None => println!("No N70 (vendor id 23872 / 23877) among them; pass -d <address> to try another."),
        }
        return 1;
    };

    let addr = target.result.addr;
    let name = if target.result.name.is_empty() { "QCY".to_string() } else { target.result.name.clone() };
    println!("\nConnecting to {addr} (address type {}), GATT only, no pairing...", addr.addr_type);
    let id = match host.le_connect_plain(addr, &name, CONNECT_TIMEOUT) {
        Ok(id) => id,
        Err(e) => {
            println!("Connection failed: {e}");
            return 1;
        }
    };
    println!("Connected.");

    let code = probe(host, id);
    host.le_disconnect(id);
    println!("\nDisconnected.");
    code
}

fn probe(host: &BtHost, id: bt_host::LeDeviceId) -> i32 {
    let services = host.le_gatt_services(id);
    println!("\nGATT cache:");
    print_services(&services);

    let Some(command) = find_char(&services, uuids::MAIN_SERVICE, uuids::COMMAND) else {
        println!("\nService A001 / characteristic 1001 (commands) not found: not the QCY control channel.");
        return 1;
    };
    let Some(notification) = find_char(&services, uuids::MAIN_SERVICE, uuids::NOTIFICATION) else {
        println!("\nCharacteristic 1002 (responses) not found.");
        return 1;
    };

    let mut state = QcyState::default();
    let (tx, responses) = mpsc::channel::<Vec<u8>>();
    if let Err(e) = host.le_gatt_subscribe(id, notification.clone(), tx, GATT_TIMEOUT) {
        println!("\nCannot subscribe to 1002: {e}");
        println!("(ATT 0x05 / 0x0F would mean the control channel wants encryption: bonding is needed.)");
        return 1;
    }
    println!("\nSubscribed to 1002.");

    let (battery_tx, battery_values) = mpsc::channel::<Vec<u8>>();
    if let Some(battery) = find_char(&services, uuids::MAIN_SERVICE, uuids::BATTERY)
        && battery.properties & 0x10 != 0
    {
        match host.le_gatt_subscribe(id, battery, battery_tx, GATT_TIMEOUT) {
            Ok(()) => println!("Subscribed to 0008 (battery)."),
            Err(e) => println!("0008 notifications not available: {e}"),
        }
    }

    println!("\nDirect reads:");
    for (name, uuid) in [
        ("firmware 0007", uuids::FIRMWARE_VERSION),
        ("battery 0008", uuids::BATTERY),
        ("key functions 000D", uuids::KEY_FUNCTIONS),
    ] {
        let Some(chr) = find_char(&services, uuids::MAIN_SERVICE, uuid) else {
            println!("  {name}: not present");
            continue;
        };
        if chr.properties & 0x02 == 0 {
            println!("  {name}: not readable");
            continue;
        }
        match host.le_gatt_read(id, chr.value_handle, GATT_TIMEOUT) {
            Ok(value) => {
                println!("  {name}: {}", to_hex(&value));
                match uuid {
                    uuids::FIRMWARE_VERSION => state.apply_firmware_characteristic(&value),
                    uuids::BATTERY => state.apply_battery_characteristic(&value),
                    _ => state.apply_key_functions_characteristic(&value),
                };
            }
            Err(e) => println!("  {name}: {e}"),
        }
    }

    println!("\nRead-only queries (tx = request on 1001, rx = response on 1002):");
    let mut answered = 0;
    for op in commands::STATE_QUERIES {
        let frame = commands::request(op);
        while responses.try_recv().is_ok() {}
        println!("  query 0x{op:02X}  tx {}", to_hex(&frame));
        if let Err(e) = host.le_gatt_write(id, command.value_handle, frame, GATT_TIMEOUT) {
            println!("    write failed: {e}");
            if command.properties & 0x08 == 0 {
                println!("    (1001 does not list Write with response; properties 0x{:02X})", command.properties);
            }
            continue;
        }
        if wait_for(&responses, op, &mut state) {
            answered += 1;
        } else {
            println!("    no response");
        }
    }

    // Late notifications and the battery characteristic's own
    let until = Instant::now() + SETTLE;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match responses.recv_timeout(left) {
            Ok(value) => report(&value, &mut state),
            Err(_) => break,
        }
    }
    while let Ok(value) = battery_values.try_recv() {
        println!("  battery notification 0008: {}", to_hex(&value));
        state.apply_battery_characteristic(&value);
    }

    println!("\nState ({answered} of {} queries answered):", commands::STATE_QUERIES.len());
    for line in state.summary() {
        println!("  {line}");
    }
    if answered == 0 {
        println!("\nNo query was answered.");
        return 1;
    }
    0
}

/// Collects responses until one carries `opcode` (or the time is up); true
/// if it came
fn wait_for(responses: &mpsc::Receiver<Vec<u8>>, opcode: u8, state: &mut QcyState) -> bool {
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    let mut got = false;
    while !got {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
        match responses.recv_timeout(left) {
            Ok(value) => got = report(&value, state).contains(&opcode),
            Err(_) => break,
        }
    }
    got
}

/// Prints a received frame, applies it, and returns its opcodes
fn report(value: &[u8], state: &mut QcyState) -> Vec<u8> {
    println!("    rx {}", to_hex(value));
    let commands = packet::parse(value);
    if commands.is_empty() {
        println!("    (not a well-formed 0xFF frame)");
    }
    for c in &commands {
        state.apply(c);
    }
    commands.iter().map(|c| c.opcode).collect()
}

fn scan(host: &BtHost, seconds: u32) -> Vec<Found> {
    host.le_scan(Duration::from_secs(seconds.into()))
        .into_iter()
        .filter_map(|result| {
            let advertisement =
                advertisement::find_in_ad(&result.adv).or_else(|| advertisement::find_in_ad(&result.scan_rsp))?;
            Some(Found { result, advertisement })
        })
        .collect()
}

fn describe(f: &Found) -> String {
    let a = &f.advertisement;
    let pct = |level: u8, charging: bool| format!("{level}%{}", if charging { "+" } else { "" });
    let control = a.control_address.map_or_else(|| "none".to_string(), advertisement::format_address);
    let other =
        a.other_address.map_or_else(String::new, |o| format!(", other bud {}", advertisement::format_address(o)));
    format!(
        "  {}  {}  RSSI {}  vendor {}{}  battery L {} R {} case {}  control address {control}{other}",
        f.result.addr,
        if f.result.addr.addr_type == bt_host::LeAddr::PUBLIC { "public" } else { "random" },
        f.result.rssi,
        a.vendor_id,
        if uuids::is_n70(a.vendor_id) { " (N70)" } else { "" },
        pct(a.left_battery, a.left_charging),
        pct(a.right_battery, a.right_charging),
        pct(a.case_battery, a.case_charging),
    )
}

/// The address of `-d` (as advertised or as the control address), else the
/// first N70. Unknown vendor ids are never picked on their own.
fn choose(found: &[Found], address: Option<[u8; 6]>) -> Option<&Found> {
    match address {
        Some(a) => found.iter().find(|f| {
            f.result.addr.addr == a
                || f.advertisement.control_address.is_some_and(|c| advertisement::address_bytes(c) == a)
        }),
        None => found.iter().find(|f| uuids::is_n70(f.advertisement.vendor_id)),
    }
}

fn to_u64(a: [u8; 6]) -> u64 {
    a.iter().fold(0, |acc, b| (acc << 8) | u64::from(*b))
}

fn find_char(services: &[GattService], service: u16, characteristic: u16) -> Option<GattChar> {
    services
        .iter()
        .filter(|s| uuid16_matches(s.uuid16, &s.uuid128, service))
        .flat_map(|s| s.chars.iter())
        .find(|c| uuid16_matches(c.uuid16, &c.uuid128, characteristic))
        .cloned()
}

fn uuid_text(uuid16: u16, uuid128: &[u8; 16]) -> String {
    if uuid16 != 0 {
        return format!("{uuid16:04X}");
    }
    let hex = to_hex(uuid128);
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

fn properties_text(properties: u16) -> String {
    let names = [(0x02, "read"), (0x04, "write-no-response"), (0x08, "write"), (0x10, "notify"), (0x20, "indicate")];
    let list: Vec<&str> = names.iter().filter(|(bit, _)| properties & bit != 0).map(|(_, name)| *name).collect();
    if list.is_empty() { "-".to_string() } else { list.join(", ") }
}

fn print_services(services: &[GattService]) {
    for s in services {
        println!(
            "  service {}  handles {:04X}-{:04X}{}",
            uuid_text(s.uuid16, &s.uuid128),
            s.start_handle,
            s.end_handle,
            if s.primary { "" } else { "  (secondary)" }
        );
        for c in &s.chars {
            println!(
                "    characteristic {}  value handle {:04X}  {}",
                uuid_text(c.uuid16, &c.uuid128),
                c.value_handle,
                properties_text(c.properties)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chr(uuid16: u16, properties: u16, handle: u16) -> GattChar {
        GattChar { uuid16, value_handle: handle, properties, ..GattChar::default() }
    }

    fn service(uuid16: u16, chars: Vec<GattChar>) -> GattService {
        GattService {
            uuid16,
            uuid128: [0; 16],
            start_handle: 1,
            end_handle: 0xFF,
            primary: true,
            included_by: 0,
            chars,
        }
    }

    #[test]
    fn finds_characteristics_of_the_control_service_only() {
        let services = [
            service(0x1800, vec![chr(0x1001, 0x08, 0x03)]),
            service(0xA001, vec![chr(0x1001, 0x08, 0x10), chr(0x1002, 0x12, 0x12)]),
        ];
        // The 1001 of the generic service is not the command characteristic
        assert_eq!(find_char(&services, uuids::MAIN_SERVICE, uuids::COMMAND).unwrap().value_handle, 0x10);
        assert_eq!(find_char(&services, uuids::MAIN_SERVICE, uuids::NOTIFICATION).unwrap().value_handle, 0x12);
        assert!(find_char(&services, uuids::MAIN_SERVICE, uuids::BATTERY).is_none());
        assert!(find_char(&services[..1], uuids::MAIN_SERVICE, uuids::COMMAND).is_none());
    }

    #[test]
    fn finds_a_service_listed_as_a_128_bit_uuid() {
        let mut s = service(0, vec![GattChar { uuid128: uuids::base_uuid128(uuids::COMMAND), ..GattChar::default() }]);
        s.uuid128 = uuids::base_uuid128(uuids::MAIN_SERVICE);
        assert!(find_char(&[s], uuids::MAIN_SERVICE, uuids::COMMAND).is_some());
    }

    #[test]
    fn uuids_and_properties_print_readably() {
        assert_eq!(uuid_text(0xA001, &[0; 16]), "A001");
        assert_eq!(uuid_text(0, &uuids::base_uuid128(0xA001)), "0000A001-0000-1000-8000-00805F9B34FB");
        assert_eq!(properties_text(0x12), "read, notify");
        assert_eq!(properties_text(0x0A), "read, write");
        assert_eq!(properties_text(0), "-");
    }

    fn found(vendor_id: u16, addr: [u8; 6], control: Option<u64>) -> Found {
        let mut result = LeScanResult::default();
        result.addr.addr = addr;
        Found {
            result,
            advertisement: Advertisement {
                vendor_id,
                left_battery: 80,
                right_battery: 75,
                case_battery: 40,
                left_charging: false,
                right_charging: false,
                case_charging: false,
                control_address: control,
                other_address: None,
            },
        }
    }

    #[test]
    fn chooses_the_first_n70_and_never_an_unknown_vendor() {
        let list = [found(1234, [1; 6], None), found(23877, [2; 6], None), found(23872, [3; 6], None)];
        assert_eq!(choose(&list, None).unwrap().result.addr.addr, [2; 6]);
        assert!(choose(&list[..1], None).is_none());
        assert!(choose(&[], None).is_none());
    }

    #[test]
    fn an_explicit_address_matches_the_advertiser_or_its_control_address() {
        let control = 0xC4_33_96_11_0C_83_u64;
        let list = [found(1234, [1; 6], None), found(23877, [2; 6], Some(control))];
        // The address of the advertiser, even of an unknown vendor
        assert_eq!(choose(&list, Some([1; 6])).unwrap().result.addr.addr, [1; 6]);
        // The control address named in the manufacturer data
        let c = advertisement::address_bytes(control);
        assert_eq!(choose(&list, Some(c)).unwrap().result.addr.addr, [2; 6]);
        assert!(choose(&list, Some([9; 6])).is_none());
        assert_eq!(to_u64(c), control);
    }

    #[test]
    fn describes_an_advertiser_in_one_line() {
        let f = found(23877, [0xC4, 0x33, 0x96, 0x11, 0x0C, 0x83], Some(0xC4_33_96_11_0C_83));
        let line = describe(&f);
        assert!(line.contains("C4:33:96:11:0C:83"), "{line}");
        assert!(line.contains("vendor 23877 (N70)"), "{line}");
        assert!(line.contains("battery L 80% R 75% case 40%"), "{line}");
        assert!(line.contains("control address C4:33:96:11:0C:83"), "{line}");
    }

    #[test]
    fn collects_responses_until_the_asked_opcode() {
        let (tx, rx) = mpsc::channel();
        let mut state = QcyState::default();
        // A notification of another opcode first, then the answer
        tx.send(vec![0xFF, 0x03, 0x09, 0x01, 0x02]).unwrap();
        tx.send(vec![0xFF, 0x03, 0x23, 0x01, 0x01]).unwrap();
        assert!(wait_for(&rx, 0x23, &mut state));
        assert_eq!(state.game_mode, Some(false));
        assert_eq!(state.ldac, Some(true));
        // Nothing more is queued: the next opcode times out (not waited here)
        drop(tx);
        assert!(!wait_for(&rx, 0x24, &mut state));
    }
}
