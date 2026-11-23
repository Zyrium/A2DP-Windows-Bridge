//! Audio between two processes of the same user: the capture agent writes
//! what its capture delivers, the core reads it.
//!
//! - Data: a single-producer single-consumer ring of records in a named
//!   shared memory ([`win_sys::shm::SharedMemory`]). The writer never
//!   blocks and never allocates: a record that does not fit is dropped and
//!   counted. Positions are monotonic byte counts in the header, published
//!   with release ordering after the bytes are copied. After each record the
//!   writer sets a named auto-reset event the reader waits on.
//! - State: header words beside the positions ([`Stat`]), written by one
//!   side only, for counters and the end state.
//! - Control: length-prefixed JSON frames ([`write_frame`] / [`read_frame`],
//!   the wire format of the `ipc` crate, but blocking) over any byte stream.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;
pub use win_sys::shm::Wake;
use win_sys::shm::{Event, SharedMemory};

/// "BCPCM" and layout version 1
const MAGIC: u64 = 0x0001_4D43_5043_4200;
const W_MAGIC: usize = 0;
const W_CAPACITY: usize = 1;
/// The writer's position: its own cache line
const W_WRITE: usize = 8;
/// The reader's position: its own cache line
const W_READ: usize = 16;
const W_STATS: usize = 24;

/// Bytes of a record header: kind u32, frames u32, payload length u32,
/// reserved u32, lost frames u64 (LE)
pub const RECORD_HEADER: usize = 24;
/// Largest control frame
pub const MAX_FRAME: usize = 64 << 10;
/// `lost` of a discontinuity whose size is not known
pub const LOST_UNKNOWN: u64 = u64::MAX;

/// Header words beside the ring positions. The agent writes all but
/// [`Stat::ReaderRecords`]; the core reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Stat {
    /// The source's own counters, mirrored from its running capture
    SourceFrames,
    SourceSilenceFrames,
    SourceFilledFrames,
    SourceDiscontinuities,
    SourceLostFrames,
    /// Longest packet gap of the source since the reader last took it (µs)
    SourcePacketGapUs,
    /// Records the writer dropped because the ring was full, and their frames
    DroppedRecords,
    DroppedFrames,
    /// Records written
    WriterRecords,
    /// Records read (written by the reader)
    ReaderRecords,
    /// 0 while running; else the end code, see the agent protocol
    EndCode,
    /// A detail of the end (an HRESULT)
    EndDetail,
}

const STAT_COUNT: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Audio = 1,
    Silence = 2,
    Discontinuity = 3,
}

impl Kind {
    fn from_u32(v: u32) -> Option<Kind> {
        match v {
            1 => Some(Kind::Audio),
            2 => Some(Kind::Silence),
            3 => Some(Kind::Discontinuity),
            _ => None,
        }
    }
}

/// A record read by [`Reader::pop`]; the payload is in the reader's buffer
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    pub frames: u32,
    /// Discontinuity: frames lost, or [`LOST_UNKNOWN`]
    pub lost: u64,
    pub len: usize,
}

/// The shared memory and the wake event of one channel, either side
#[derive(Debug)]
pub struct Link {
    shm: SharedMemory,
    wake: Event,
    capacity: u64,
}

fn event_name(channel: &str) -> String {
    format!("{channel}_wake")
}

/// The channel text of a channel passed as handles ([`Link::create_for`]):
/// `#<memory handle>.<event handle>`, decimal, the values in the process
/// they were duplicated into
pub fn handle_channel(section: u64, event: u64) -> String {
    format!("#{section}.{event}")
}

/// The two handle values of a [`handle_channel`] text
pub fn parse_handle_channel(channel: &str) -> Option<(u64, u64)> {
    let (a, b) = channel.strip_prefix('#')?.split_once('.')?;
    let num = |s: &str| {
        (!s.is_empty() && s.len() <= 10 && s.bytes().all(|c| c.is_ascii_digit())).then(|| s.parse().ok()).flatten()
    };
    Some((num(a)?, num(b)?))
}

fn bad(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string())
}

impl Link {
    /// The reader's side creates the channel: `ring_bytes` of ring (rounded
    /// up to whole pages with the header)
    pub fn create(channel: &str, ring_bytes: usize) -> std::io::Result<Arc<Link>> {
        let total = (SharedMemory::HEADER + ring_bytes).next_multiple_of(4096);
        let shm = SharedMemory::create(channel, total)?;
        let wake = Event::create(&event_name(channel))?;
        let capacity = shm.data_len() as u64;
        shm.word(W_CAPACITY).store(capacity, Ordering::Relaxed);
        shm.word(W_MAGIC).store(MAGIC, Ordering::Release);
        Ok(Arc::new(Link { shm, wake, capacity }))
    }

    /// A channel without names, for `process` (a core serving an agent in
    /// another session, where `Local\` names are not shared): the memory
    /// and the event are duplicated into that process, and the returned
    /// channel text ([`handle_channel`]) names their handles there. The
    /// other side passes the text to [`Link::open`] as usual.
    pub fn create_for(
        process: std::os::windows::io::BorrowedHandle<'_>,
        ring_bytes: usize,
    ) -> std::io::Result<(Arc<Link>, String)> {
        let total = (SharedMemory::HEADER + ring_bytes).next_multiple_of(4096);
        let shm = SharedMemory::create_unnamed(total)?;
        let wake = Event::create_unnamed()?;
        let capacity = shm.data_len() as u64;
        shm.word(W_CAPACITY).store(capacity, Ordering::Relaxed);
        shm.word(W_MAGIC).store(MAGIC, Ordering::Release);
        let section = shm.duplicate_into(process)?;
        // Not undone if this fails: the process then holds a handle it was
        // never told of (as when its request is lost), freed when it exits
        let event = wake.duplicate_into(process)?;
        Ok((Arc::new(Link { shm, wake, capacity }), handle_channel(section, event)))
    }

    /// The writer's side opens what the reader created: by name, or by the
    /// handles of a [`handle_channel`] (taken first, so they are closed
    /// whatever fails next)
    pub fn open(channel: &str) -> std::io::Result<Arc<Link>> {
        if let Some((section, event)) = parse_handle_channel(channel) {
            let shm = SharedMemory::from_handle_value(section);
            let wake = Event::from_handle_value(event);
            return Self::check(shm?, wake?);
        }
        let shm = SharedMemory::open(channel)?;
        Self::check_then(shm, || Event::open(&event_name(channel)))
    }

    fn check(shm: SharedMemory, wake: Event) -> std::io::Result<Arc<Link>> {
        Self::check_then(shm, || Ok(wake))
    }

    fn check_then(shm: SharedMemory, wake: impl FnOnce() -> std::io::Result<Event>) -> std::io::Result<Arc<Link>> {
        if shm.word(W_MAGIC).load(Ordering::Acquire) != MAGIC {
            return Err(bad("not a pcm link"));
        }
        let capacity = shm.word(W_CAPACITY).load(Ordering::Relaxed);
        if capacity == 0 || capacity > shm.data_len() as u64 {
            return Err(bad("bad ring capacity"));
        }
        let wake = wake()?;
        Ok(Arc::new(Link { shm, wake, capacity }))
    }

    /// Ring bytes
    pub fn capacity(&self) -> usize {
        self.capacity as usize
    }

    pub fn stat(&self, s: Stat) -> &AtomicU64 {
        debug_assert!((s as usize) < STAT_COUNT);
        self.shm.word(W_STATS + s as usize)
    }

    /// Sets the wake event (after a record, or a change of the end state)
    pub fn wake(&self) {
        self.wake.set();
    }

    /// Waits for the wake event or the end of `process`
    pub fn wait_or_process(&self, process: std::os::windows::io::BorrowedHandle<'_>, timeout_ms: u32) -> Wake {
        self.wake.wait_or_process(process, timeout_ms)
    }

    /// Waits for the wake event alone
    pub fn wait(&self, timeout_ms: u32) -> bool {
        self.wake.wait(timeout_ms)
    }

    fn copy_in(&self, pos: u64, bytes: &[u8]) {
        let off = (pos % self.capacity) as usize;
        let first = bytes.len().min(self.capacity as usize - off);
        self.shm.write(off, &bytes[..first]);
        if first < bytes.len() {
            self.shm.write(0, &bytes[first..]);
        }
    }

    fn copy_out(&self, pos: u64, out: &mut [u8]) {
        let off = (pos % self.capacity) as usize;
        let first = out.len().min(self.capacity as usize - off);
        self.shm.read(off, &mut out[..first]);
        if first < out.len() {
            self.shm.read(0, &mut out[first..]);
        }
    }
}

/// The writing end (one per channel)
#[derive(Debug)]
pub struct Writer {
    link: Arc<Link>,
    pos: u64,
}

impl Writer {
    pub fn new(link: Arc<Link>) -> Writer {
        let pos = link.shm.word(W_WRITE).load(Ordering::Relaxed);
        Writer { link, pos }
    }

    pub fn link(&self) -> &Arc<Link> {
        &self.link
    }

    /// Appends a record and wakes the reader. False (and counted as
    /// dropped, with `frames`) if the ring has no room for it.
    pub fn push(&mut self, kind: Kind, frames: u32, lost: u64, payload: &[u8]) -> bool {
        let need = (RECORD_HEADER + payload.len()) as u64;
        let read = self.link.shm.word(W_READ).load(Ordering::Acquire);
        let used = self.pos.wrapping_sub(read);
        if need > self.link.capacity.saturating_sub(used) {
            self.link.stat(Stat::DroppedRecords).fetch_add(1, Ordering::Relaxed);
            self.link.stat(Stat::DroppedFrames).fetch_add(u64::from(frames), Ordering::Relaxed);
            return false;
        }
        let mut header = [0u8; RECORD_HEADER];
        header[0..4].copy_from_slice(&(kind as u32).to_le_bytes());
        header[4..8].copy_from_slice(&frames.to_le_bytes());
        header[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        header[16..24].copy_from_slice(&lost.to_le_bytes());
        self.link.copy_in(self.pos, &header);
        self.link.copy_in(self.pos + RECORD_HEADER as u64, payload);
        self.pos += need;
        self.link.shm.word(W_WRITE).store(self.pos, Ordering::Release);
        self.link.stat(Stat::WriterRecords).fetch_add(1, Ordering::Relaxed);
        self.link.wake();
        true
    }
}

/// What [`Reader::pop`] returns when the ring holds something it cannot
/// read: the reader skips everything written so far
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Corrupt;

/// The reading end (one per channel). Holds a buffer of the ring's size,
/// allocated once.
#[derive(Debug)]
pub struct Reader {
    link: Arc<Link>,
    pos: u64,
    buf: Vec<u8>,
}

impl Reader {
    pub fn new(link: Arc<Link>) -> Reader {
        let pos = link.shm.word(W_READ).load(Ordering::Relaxed);
        let buf = vec![0; link.capacity()];
        Reader { link, pos, buf }
    }

    pub fn link(&self) -> &Arc<Link> {
        &self.link
    }

    /// The next record (its payload: [`Reader::payload`]), None when empty
    pub fn pop(&mut self) -> Result<Option<Record>, Corrupt> {
        let write = self.link.shm.word(W_WRITE).load(Ordering::Acquire);
        let avail = write.wrapping_sub(self.pos);
        if avail == 0 {
            return Ok(None);
        }
        let mut header = [0u8; RECORD_HEADER];
        let parsed = if avail >= RECORD_HEADER as u64 && avail <= self.link.capacity {
            self.link.copy_out(self.pos, &mut header);
            let word = |i: usize| u32::from_le_bytes(header[i..i + 4].try_into().expect("4 bytes"));
            let len = word(8) as usize;
            Kind::from_u32(word(0)).filter(|_| (RECORD_HEADER + len) as u64 <= avail).map(|kind| Record {
                kind,
                frames: word(4),
                lost: u64::from_le_bytes(header[16..24].try_into().expect("8 bytes")),
                len,
            })
        } else {
            None
        };
        let Some(record) = parsed else {
            self.pos = write;
            self.link.shm.word(W_READ).store(self.pos, Ordering::Release);
            return Err(Corrupt);
        };
        self.link.copy_out(self.pos + RECORD_HEADER as u64, &mut self.buf[..record.len]);
        self.pos += (RECORD_HEADER + record.len) as u64;
        self.link.shm.word(W_READ).store(self.pos, Ordering::Release);
        self.link.stat(Stat::ReaderRecords).fetch_add(1, Ordering::Relaxed);
        Ok(Some(record))
    }

    /// The payload of the record [`Reader::pop`] just returned
    pub fn payload(&self, record: &Record) -> &[u8] {
        &self.buf[..record.len]
    }
}

/// Writes one control frame: length (u32 LE) and the JSON
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> std::io::Result<()> {
    let json = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    if json.len() > MAX_FRAME {
        return Err(bad("control frame too large"));
    }
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one control frame; Ok(None) at the end of the stream
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> std::io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(bad("control frame too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn channel() -> String {
        format!(r"Local\BlueCaster_test_pcm_{}_{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed))
    }

    fn pair(ring: usize) -> (Writer, Reader) {
        let ch = channel();
        let reader = Link::create(&ch, ring).unwrap();
        let writer = Link::open(&ch).unwrap();
        (Writer::new(writer), Reader::new(reader))
    }

    /// A channel passed as handles: here duplicated into this process
    /// itself, as a core does into its agent
    #[test]
    fn a_channel_travels_as_handles() {
        let (reader, text) = Link::create_for(win_sys::session_launch::current_process(), 4096).unwrap();
        assert!(parse_handle_channel(&text).is_some(), "{text}");
        let writer = Link::open(&text).unwrap();
        let (mut w, mut r) = (Writer::new(writer), Reader::new(reader));
        assert!(w.push(Kind::Audio, 1, 0, &[9, 8, 7, 6]));
        let a = r.pop().unwrap().unwrap();
        assert_eq!(r.payload(&a), &[9, 8, 7, 6]);
        w.link().wake();
        assert!(r.link().wait(100), "one event for both ends");
        // Not a handle channel, or handles this process does not have
        assert_eq!(parse_handle_channel("#12.x"), None);
        assert_eq!(parse_handle_channel(r"Local\x"), None);
        assert_eq!(parse_handle_channel("#4.8"), Some((4, 8)));
        assert_eq!(parse_handle_channel("#99999999999.8"), None);
        assert!(Link::open("#0.0").is_err());
    }

    #[test]
    fn records_keep_kind_frames_and_payload() {
        let (mut w, mut r) = pair(4096);
        assert_eq!(r.pop(), Ok(None));
        assert!(w.push(Kind::Audio, 2, 0, &[1, 2, 3, 4, 5, 6, 7, 8]));
        assert!(w.push(Kind::Silence, 480, 0, &[]));
        assert!(w.push(Kind::Discontinuity, 0, 96, &[]));
        assert!(w.push(Kind::Discontinuity, 0, LOST_UNKNOWN, &[]));
        let a = r.pop().unwrap().unwrap();
        assert_eq!((a.kind, a.frames, a.len), (Kind::Audio, 2, 8));
        assert_eq!(r.payload(&a), &[1, 2, 3, 4, 5, 6, 7, 8]);
        let s = r.pop().unwrap().unwrap();
        assert_eq!((s.kind, s.frames, s.len), (Kind::Silence, 480, 0));
        assert_eq!(r.pop().unwrap().unwrap().lost, 96);
        assert_eq!(r.pop().unwrap().unwrap().lost, LOST_UNKNOWN);
        assert_eq!(r.pop(), Ok(None));
        assert_eq!(r.link().stat(Stat::WriterRecords).load(Ordering::Relaxed), 4);
        assert_eq!(r.link().stat(Stat::ReaderRecords).load(Ordering::Relaxed), 4);
    }

    #[test]
    fn records_wrap_around_the_end() {
        // 4096 - 512 header = 3584 ring bytes; 1000-byte records wrap at
        // ever-changing offsets
        let (mut w, mut r) = pair(3584);
        assert_eq!(r.link().capacity(), 3584);
        for i in 0..50u32 {
            let payload: Vec<u8> = (0..1000u32).map(|b| (b + i) as u8).collect();
            assert!(w.push(Kind::Audio, i, 0, &payload), "record {i}");
            let rec = r.pop().unwrap().unwrap();
            assert_eq!(rec.frames, i);
            assert_eq!(r.payload(&rec), &payload[..], "record {i}");
        }
        // The header itself split across the end
        let (mut w, mut r) = pair(3584);
        assert!(w.push(Kind::Audio, 0, 0, &[0; 3584 - 24 - 10]));
        r.pop().unwrap().unwrap();
        assert!(w.push(Kind::Discontinuity, 0, 1234, &[]));
        assert_eq!(r.pop().unwrap().unwrap().lost, 1234);
    }

    #[test]
    fn a_full_ring_drops_and_counts() {
        let (mut w, mut r) = pair(3584);
        let block = [7u8; 1000];
        assert!(w.push(Kind::Audio, 250, 0, &block));
        assert!(w.push(Kind::Audio, 250, 0, &block));
        assert!(w.push(Kind::Audio, 250, 0, &block));
        // 3 x 1024 used, 512 left: dropped, the writer does not wait
        assert!(!w.push(Kind::Audio, 250, 0, &block));
        assert!(!w.push(Kind::Audio, 100, 0, &block));
        // A small record still fits
        assert!(w.push(Kind::Discontinuity, 0, 350, &[]));
        let link = r.link().clone();
        assert_eq!(link.stat(Stat::DroppedRecords).load(Ordering::Relaxed), 2);
        assert_eq!(link.stat(Stat::DroppedFrames).load(Ordering::Relaxed), 350);
        // Reading frees the room again
        r.pop().unwrap().unwrap();
        assert!(w.push(Kind::Audio, 250, 0, &block));
        let mut kinds = Vec::new();
        while let Some(rec) = r.pop().unwrap() {
            kinds.push(rec.kind);
        }
        assert_eq!(kinds, [Kind::Audio, Kind::Audio, Kind::Discontinuity, Kind::Audio]);
        // A record larger than the ring never fits
        assert!(!w.push(Kind::Audio, 1, 0, &[0; 4000]));
    }

    #[test]
    fn stats_and_wake_cross_the_link() {
        let (w, r) = pair(4096);
        w.link().stat(Stat::SourceFilledFrames).store(960, Ordering::Relaxed);
        w.link().stat(Stat::EndCode).store(2, Ordering::Release);
        assert_eq!(r.link().stat(Stat::SourceFilledFrames).load(Ordering::Relaxed), 960);
        assert_eq!(r.link().stat(Stat::EndCode).load(Ordering::Acquire), 2);
        assert!(!r.link().wait(0));
        w.link().wake();
        assert!(r.link().wait(100));
    }

    #[test]
    fn concurrent_writer_and_reader_lose_nothing_that_fit() {
        let (mut w, mut r) = pair(16 << 10);
        const N: u32 = 20_000;
        let t = std::thread::spawn(move || {
            let mut written = Vec::new();
            for i in 0..N {
                let payload = [(i % 251) as u8; 40];
                if w.push(Kind::Audio, i, 0, &payload) {
                    written.push(i);
                }
            }
            written
        });
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            match r.pop().unwrap() {
                Some(rec) => {
                    assert!(r.payload(&rec).iter().all(|&b| b == (rec.frames % 251) as u8));
                    got.push(rec.frames);
                }
                None if t.is_finished() && r.pop().unwrap().is_none() => break,
                None => {
                    r.link().wait(1);
                }
            }
            assert!(std::time::Instant::now() < deadline);
        }
        let written = t.join().unwrap();
        // Late records after the last empty check
        while let Some(rec) = r.pop().unwrap() {
            got.push(rec.frames);
        }
        assert_eq!(got, written);
        let dropped = r.link().stat(Stat::DroppedRecords).load(Ordering::Relaxed);
        assert_eq!(written.len() as u64 + dropped, u64::from(N));
    }

    #[test]
    fn a_bad_record_is_skipped() {
        let (mut w, mut r) = pair(4096);
        assert!(w.push(Kind::Audio, 1, 0, &[1, 2, 3, 4]));
        // Overwrite the kind with garbage
        r.link().copy_in(0, &[9, 0, 0, 0]);
        assert_eq!(r.pop(), Err(Corrupt));
        assert_eq!(r.pop(), Ok(None));
        assert!(w.push(Kind::Silence, 5, 0, &[]));
        assert_eq!(r.pop().unwrap().unwrap().frames, 5);
    }

    #[test]
    fn open_needs_a_created_channel() {
        assert!(Link::open(&channel()).is_err());
    }

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &serde_json::json!({"open": 1})).unwrap();
        write_frame(&mut buf, &"start").unwrap();
        let mut cur = std::io::Cursor::new(buf);
        let a: serde_json::Value = read_frame(&mut cur).unwrap().unwrap();
        assert_eq!(a["open"], 1);
        assert_eq!(read_frame::<_, String>(&mut cur).unwrap().as_deref(), Some("start"));
        assert!(read_frame::<_, String>(&mut cur).unwrap().is_none());
        let mut huge = std::io::Cursor::new(((MAX_FRAME + 1) as u32).to_le_bytes().to_vec());
        assert!(read_frame::<_, String>(&mut huge).is_err());
    }
}
