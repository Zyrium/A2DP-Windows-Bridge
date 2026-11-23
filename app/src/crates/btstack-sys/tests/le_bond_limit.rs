//! The number of LE bonds (btstack_config.h): a bonds file of a build with 16
//! of them loads as it is, and with all taken the oldest bond not in use
//! makes room for a new one, with everything kept beside it.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use btstack_sys::consts::*;
use btstack_sys::{
    App, EvictedLeBond, HciBackend, ImportedLeBond, LeBondAdded, MemoryTlv, Remote, Source, Stack, TlvStore, Upstream,
    ccc_slots, ccc_tag, service_changed_tag,
};

const CON: u16 = 0x0040;
/// le_device_db entries of earlier builds
const OLD_BONDS: usize = 16;
/// Their CCC values
const OLD_CCCS: u32 = 256;

/// Answers every command, as in att_server.rs
#[derive(Clone, Default)]
struct Controller(Arc<Mutex<Option<Upstream>>>);

impl Controller {
    fn deliver(&self, packet: &[u8]) {
        self.0.lock().unwrap().as_ref().unwrap().deliver(HCI_EVENT_PACKET, packet);
    }
}

impl HciBackend for Controller {
    fn is_async(&self) -> bool {
        false
    }
    fn open(&mut self, upstream: Upstream) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(upstream);
        Ok(())
    }
    fn close(&mut self) {}
    fn send(&mut self, packet_type: u8, packet: &[u8]) -> bool {
        if packet_type == HCI_COMMAND_DATA_PACKET {
            let opcode = u16::from_le_bytes([packet[0], packet[1]]);
            let params: Vec<u8> = match opcode {
                0x1001 => vec![0x0C, 0, 0, 0x0C, 0xF1, 0x05, 0, 0],
                0x1002 => vec![0; 64],
                // LE Supported (Controller)
                0x1003 => vec![0, 0, 0, 0, 0x40, 0, 0, 0],
                0x1005 => vec![0xFD, 0x03, 0, 8, 0, 0, 0],
                0x1009 => vec![0x11, 0x00, 0x00, 0xD0, 0xA2, 0x00],
                0x2002 => vec![0xFB, 0x00, 1],
                0x2003 => vec![0; 8],
                0x200F => vec![8],
                _ => vec![],
            };
            let mut cc = vec![HCI_EVENT_COMMAND_COMPLETE, (4 + params.len()) as u8, 1];
            cc.extend_from_slice(&opcode.to_le_bytes());
            cc.push(0);
            cc.extend_from_slice(&params);
            self.deliver(&cc);
        }
        true
    }
}

type Job = Box<dyn FnOnce(&Stack) + Send>;
/// Index and identity of each
type Deleted = Arc<Mutex<Vec<(u8, [u8; 6])>>>;

/// Runs jobs on the BTstack thread and keeps what BONDING_DELETED told
struct Jobs {
    jobs: Arc<Mutex<Vec<Job>>>,
    deleted: Deleted,
}

impl App for Jobs {
    fn on_packet(&mut self, _s: &Stack, _source: Source, _packet_type: u8, _channel: u16, p: &[u8]) {
        if let Some(b) = btstack_sys::events::bonding_deleted(p) {
            self.deleted.lock().unwrap().push((b.index, b.addr));
        }
    }
    fn on_wake(&mut self, s: &Stack) {
        let jobs = std::mem::take(&mut *self.jobs.lock().unwrap());
        for job in jobs {
            job(s);
        }
    }
    fn on_timer(&mut self, _s: &Stack, _id: u32) {}
}

struct Run {
    remote: Remote,
    controller: Controller,
    jobs: Arc<Mutex<Vec<Job>>>,
    deleted: Deleted,
    thread: std::thread::JoinHandle<()>,
}

impl Run {
    fn start(tlv: &MemoryTlv) -> Run {
        let (tlv, controller) = (tlv.clone(), Controller::default());
        let (jobs, deleted) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let app = Jobs { jobs: jobs.clone(), deleted: deleted.clone() };
        let c = controller.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            btstack_sys::run(move |s, remote| {
                assert!(s.set_le_hidden(false));
                s.hci_init(Box::new(c));
                s.set_link_key_store(None);
                s.l2cap_init();
                s.add_hci_event_handler();
                s.le_init(Box::new(tlv));
                s.power_on().unwrap();
                tx.send(remote).unwrap();
                Box::new(app)
            })
            .unwrap();
        });
        let remote = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Run { remote, controller, jobs, deleted, thread }
    }

    /// Runs `f` on the BTstack thread
    fn on_stack<T: Send + 'static>(&self, f: impl FnOnce(&Stack) -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        self.jobs.lock().unwrap().push(Box::new(move |s| tx.send(f(s)).unwrap()));
        self.remote.wake();
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    fn end(self) {
        self.remote.request_exit();
        self.thread.join().unwrap();
        assert!(!btstack_sys::poisoned());
    }
}

/// BTstack runs once per process: one test at a time
static ONE_RUN: Mutex<()> = Mutex::new(());

fn bond_tag(index: usize) -> u32 {
    u32::from_be_bytes([b'B', b'T', b'D', index as u8])
}

fn classic_tag(index: usize) -> u32 {
    u32::from_be_bytes([b'A', b'W', b'K', index as u8])
}

const DB_HASH: u32 = u32::from_be_bytes(*b"BTDB");

/// The identity of the bond at `index` (public)
fn identity(index: usize) -> [u8; 6] {
    [0x00, 0xA2, 0xD0, 0, 0, index as u8 + 1]
}

/// le_device_db_entry_t
fn bond(index: usize, seq_nr: u32) -> Vec<u8> {
    let mut v = seq_nr.to_le_bytes().to_vec();
    v.extend(0i32.to_le_bytes());
    v.extend(identity(index));
    v.extend([0x20 + index as u8; 16]);
    v.extend([0x80 + index as u8; 16]);
    v.extend(0u16.to_le_bytes());
    v.extend([0; 8]);
    v.extend([16, 0, 0, 1]);
    v
}

/// persistent_ccc_entry_t
fn ccc(seq_nr: u32, handle: u16, device_index: u8) -> Vec<u8> {
    let mut v = seq_nr.to_le_bytes().to_vec();
    v.extend(handle.to_le_bytes());
    v.extend([1, device_index]);
    v
}

/// A device that is not bonded yet, of the classic device `n`
fn add_new(s: &Stack, n: u8) -> LeBondAdded {
    s.add_le_bond_of_classic(
        &[0x58, 0x18, 0x62, 0, 0, n],
        &ImportedLeBond {
            identity_addr_type: 1,
            identity_addr: [0xC0, 0x18, 0x62, 0, 0, n],
            irk: [0x11; 16],
            ltk: [0x12; 16],
            key_size: 16,
            ediv: 0,
            rand: [0; 8],
            authenticated: false,
            secure_connection: true,
        },
    )
}

/// The index of the bond of [`add_new`]
fn index_of_new(s: &Stack, n: u8) -> Option<usize> {
    (0..s.le_device_db_max_count())
        .find(|&i| s.le_device_db_info(i).is_some_and(|e| e.addr == [0xC0, 0x18, 0x62, 0, 0, n]))
}

#[test]
fn a_file_with_16_bonds_loads_unchanged() {
    let _one = ONE_RUN.lock().unwrap_or_else(|e| e.into_inner());
    let mut tlv = MemoryTlv::default();
    let mut file: Vec<(u32, Vec<u8>)> = (0..OLD_BONDS).map(|i| (bond_tag(i), bond(i, 40 - i as u32))).collect();
    // Every CCC slot of then
    file.extend((0..OLD_CCCS).map(|n| {
        let tag = u32::from_be_bytes([b'B', b'T', b'C', n as u8]);
        (tag, ccc(n + 1, 9 + n as u16 / 16, (n % 16) as u8))
    }));
    file.push((DB_HASH, vec![0xAB; 16]));
    file.push((service_changed_tag(3), vec![1]));
    for (tag, value) in &file {
        tlv.store(*tag, value);
    }

    let run = Run::start(&tlv);
    let (count, held) = run.on_stack(|s| {
        let held: Vec<_> = (0..s.le_device_db_max_count())
            .filter_map(|i| Some((i, s.le_device_db_info(i)?, s.le_device_db_encryption(i)?)))
            .collect();
        (s.le_device_db_max_count(), held)
    });
    assert_eq!(count, 64);
    assert_eq!(held.len(), OLD_BONDS);
    for (i, info, encryption) in held {
        assert_eq!((info.addr_type, info.addr, info.irk), (0, identity(i), [0x20 + i as u8; 16]));
        assert_eq!((encryption.ltk, encryption.key_size), ([0x80 + i as u8; 16], 16));
    }
    // The 17th bond takes a free entry
    let (added, new, evicted) = run.on_stack(|s| (add_new(s, 1), index_of_new(s, 1), s.take_evicted_le_bonds()));
    assert_eq!(added, LeBondAdded::Added);
    assert!(new.is_some_and(|i| i >= OLD_BONDS), "{new:?}");
    assert!(evicted.is_empty());
    run.end();
    for (tag, value) in &file {
        assert_eq!(tlv.get(*tag).as_deref(), Some(&value[..]), "tag {tag:08X}");
    }
}

/// Every entry taken, the bond at 5 stored first and the one at 9 second,
/// with CCC values of 5 (one in a slot earlier builds did not have), 6 and 9
fn full() -> MemoryTlv {
    let mut tlv = MemoryTlv::default();
    for i in 0..64 {
        let seq_nr = match i {
            5 => 1,
            9 => 2,
            _ => 10 + i as u32,
        };
        tlv.store(bond_tag(i), &bond(i, seq_nr));
    }
    tlv.store(ccc_tag(0), &ccc(1, 9, 5));
    tlv.store(ccc_tag(1), &ccc(2, 9, 6));
    tlv.store(ccc_tag(2), &ccc(3, 9, 9));
    tlv.store(ccc_tag(ccc_slots() - 1), &ccc(4, 12, 5));
    tlv.store(service_changed_tag(5), &[1]);
    tlv.store(service_changed_tag(6), &[1]);
    tlv.store(DB_HASH, &[0xAB; 16]);
    // Derived from the link key of a classic device
    let mut mapping = vec![0x58, 0x18, 0x62, 0, 0, 0x77, 0];
    mapping.extend(identity(5));
    tlv.store(classic_tag(5), &mapping);
    tlv
}

#[test]
fn the_oldest_bond_makes_room_with_what_is_kept_beside_it() {
    let _one = ONE_RUN.lock().unwrap_or_else(|e| e.into_inner());
    let mut tlv = full();
    let run = Run::start(&tlv);
    let (added, new, evicted) = run.on_stack(|s| {
        assert!(s.le_keys_of_classic(&[0x58, 0x18, 0x62, 0, 0, 0x77]).is_some());
        (add_new(s, 1), index_of_new(s, 1), s.take_evicted_le_bonds())
    });
    assert_eq!(added, LeBondAdded::Added);
    assert_eq!(new, Some(5));
    assert_eq!(evicted, [EvictedLeBond { addr_type: 0, addr: identity(5) }]);
    // Taken once; the classic device it came from has no keys, and
    // forgetting it leaves the bond now in its place alone
    let (evicted, keys, deleted, new) = run.on_stack(|s| {
        let classic = [0x58, 0x18, 0x62, 0, 0, 0x77];
        (
            s.take_evicted_le_bonds(),
            s.le_keys_of_classic(&classic),
            s.delete_le_keys_of_classic(&classic),
            index_of_new(s, 1),
        )
    });
    assert!(evicted.is_empty());
    assert!(keys.is_none());
    assert!(!deleted);
    assert_eq!(new, Some(5));
    assert_eq!(*run.deleted.lock().unwrap(), [(5, identity(5))]);

    // The next one takes the second oldest
    let (new, evicted) = run.on_stack(|s| {
        add_new(s, 2);
        (index_of_new(s, 2), s.take_evicted_le_bonds())
    });
    assert_eq!(new, Some(9));
    assert_eq!(evicted, [EvictedLeBond { addr_type: 0, addr: identity(9) }]);
    run.end();

    // Nothing of the old devices at 5 and 9, the rest as it was
    for slot in [0, 2, ccc_slots() - 1] {
        assert_eq!(tlv.get(ccc_tag(slot)), None, "slot {slot}");
    }
    assert_eq!(tlv.get(service_changed_tag(5)), None);
    assert_eq!(tlv.get(ccc_tag(1)), Some(ccc(2, 9, 6)));
    assert_eq!(tlv.get(service_changed_tag(6)), Some(vec![1]));
    assert_eq!(tlv.get(DB_HASH), Some(vec![0xAB; 16]));
    for i in (0..64).filter(|i| ![5, 9].contains(i)) {
        assert_eq!(tlv.get(bond_tag(i)), Some(bond(i, 10 + i as u32)), "bond {i}");
    }
}

#[test]
fn the_bond_of_a_connected_device_stays() {
    let _one = ONE_RUN.lock().unwrap_or_else(|e| e.into_inner());
    let mut tlv = full();
    let run = Run::start(&tlv);
    // LE Connection Complete: central, the peer is the device of bond 5
    let mut lcc = vec![HCI_EVENT_LE_META, 19, 0x01, 0];
    lcc.extend_from_slice(&CON.to_le_bytes());
    lcc.extend_from_slice(&[0, 0]);
    lcc.extend(identity(5).iter().rev());
    lcc.extend_from_slice(&[0x18, 0, 0, 0, 0x48, 0, 0]);
    let deadline = Instant::now() + Duration::from_secs(5);
    run.controller.deliver(&lcc);
    while run.on_stack(|s| s.sm_le_device_index(CON)) != Some(5) {
        assert!(Instant::now() < deadline, "SM did not find the bond");
        std::thread::sleep(Duration::from_millis(10));
    }
    let (new, evicted) = run.on_stack(|s| {
        add_new(s, 1);
        (index_of_new(s, 1), s.take_evicted_le_bonds())
    });
    assert_eq!(new, Some(9));
    assert_eq!(evicted, [EvictedLeBond { addr_type: 0, addr: identity(9) }]);
    run.end();
    assert_eq!(tlv.get(bond_tag(5)), Some(bond(5, 1)));
    assert_eq!(tlv.get(ccc_tag(0)), Some(ccc(1, 9, 5)));
}
