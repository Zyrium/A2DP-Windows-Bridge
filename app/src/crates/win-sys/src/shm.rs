//! Named shared memory and events between two processes of the same user:
//! the audio link between the core and its capture agent (`pcm-link`).
//!
//! [`SharedMemory`] is a pagefile-backed file mapping. Its first
//! [`SharedMemory::HEADER`] bytes are 64-bit atomics ([`SharedMemory::word`]),
//! the rest is a byte area read and written with bounds-checked copies
//! ([`SharedMemory::write`] / [`SharedMemory::read`]). Which process owns
//! which bytes when is the caller's protocol (pcm-link's ring: the writer
//! publishes a position with release ordering after copying, the reader
//! acquires it before copying out). [`Event`] is a named auto-reset event.

use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::sync::atomic::AtomicU64;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Foundation::{DUPLICATE_HANDLE_OPTIONS, DuplicateHandle};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION,
    MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery,
};
use windows::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, GetCurrentProcess, OpenEventW, SYNCHRONIZATION_SYNCHRONIZE, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::HSTRING;

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn io_error(e: windows::core::Error) -> std::io::Error {
    std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// A mapped view of a named file mapping. Unmapped and closed on drop.
#[derive(Debug)]
pub struct SharedMemory {
    mapping: HANDLE,
    base: *mut u8,
    len: usize,
}

// SAFETY: the view stays mapped for the value's lifetime and is only reached
// through atomics (header) or bounds-checked copies (data), from any thread.
unsafe impl Send for SharedMemory {}
// SAFETY: as above; no method hands out a non-atomic reference.
unsafe impl Sync for SharedMemory {}

impl SharedMemory {
    /// Bytes at the start that are 64-bit atomic words
    pub const HEADER: usize = 512;

    /// Creates the mapping `name` with `len` bytes (zeroed by Windows; at
    /// least [`Self::HEADER`] + 1). Fails if the name exists already.
    pub fn create(name: &str, len: usize) -> std::io::Result<SharedMemory> {
        if len <= Self::HEADER {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "mapping too small"));
        }
        let size = len as u64;
        // SAFETY: a pagefile-backed mapping (no file handle), default
        // security, NUL-terminated name.
        let mapping = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                &HSTRING::from(name),
            )
        }
        .map_err(io_error)?;
        // SAFETY: reads this thread's last error, set by the call above (an
        // existing mapping comes back with a valid handle and this error).
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: the handle was just opened here.
            let _ = unsafe { CloseHandle(mapping) };
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "mapping exists"));
        }
        Self::map(mapping, Some(len), FILE_MAP_ALL_ACCESS)
    }

    /// Opens the existing mapping `name` and maps all of it
    pub fn open(name: &str) -> std::io::Result<SharedMemory> {
        // SAFETY: NUL-terminated name; the handle is owned by the result.
        let mapping =
            unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &HSTRING::from(name)) }.map_err(io_error)?;
        Self::map(mapping, None, FILE_MAP_ALL_ACCESS)
    }

    /// Creates a mapping without a name, with `len` bytes (as
    /// [`SharedMemory::create`]): another process reaches it only through
    /// a handle duplicated into it ([`SharedMemory::duplicate_into`]),
    /// whatever its session
    pub fn create_unnamed(len: usize) -> std::io::Result<SharedMemory> {
        if len <= Self::HEADER {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "mapping too small"));
        }
        let size = len as u64;
        // SAFETY: a pagefile-backed mapping (no file handle), default
        // security, no name.
        let mapping = unsafe {
            CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, (size >> 32) as u32, size as u32, None)
        }
        .map_err(io_error)?;
        Self::map(mapping, Some(len), FILE_MAP_ALL_ACCESS)
    }

    /// A handle to this mapping in `process` (which needs
    /// PROCESS_DUP_HANDLE), allowed only to map it for reading and
    /// writing; its value there. The other process takes it with
    /// [`SharedMemory::from_handle_value`].
    pub fn duplicate_into(&self, process: BorrowedHandle<'_>) -> std::io::Result<u64> {
        duplicate_into(self.mapping, process, SECTION_ACCESS)
    }

    /// Takes a mapping handle another process duplicated into this one
    /// ([`SharedMemory::duplicate_into`]) and maps all of it. The handle is
    /// owned from here (closed on drop, also when mapping fails), so pass
    /// only a value the peer says it duplicated into this process.
    pub fn from_handle_value(value: u64) -> std::io::Result<SharedMemory> {
        let mapping = handle_from_value(value)?;
        Self::map(mapping, None, FILE_MAP_READ | FILE_MAP_WRITE)
    }

    fn map(mapping: HANDLE, len: Option<usize>, access: FILE_MAP) -> std::io::Result<SharedMemory> {
        // SAFETY: a mapping handle owned here; 0 bytes maps the whole section.
        let view = unsafe { MapViewOfFile(mapping, access, 0, 0, len.unwrap_or(0)) };
        if view.Value.is_null() {
            let e = last_error();
            // SAFETY: owned here, not used after.
            let _ = unsafe { CloseHandle(mapping) };
            return Err(e);
        }
        let len = match len {
            Some(l) => l,
            None => {
                let mut info = MEMORY_BASIC_INFORMATION::default();
                // SAFETY: queries the view just mapped into a local struct.
                let n = unsafe {
                    VirtualQuery(Some(view.Value), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>())
                };
                if n == 0 {
                    let e = last_error();
                    // SAFETY: the view and handle are owned here, not used after.
                    unsafe {
                        let _ = UnmapViewOfFile(view);
                        let _ = CloseHandle(mapping);
                    }
                    return Err(e);
                }
                info.RegionSize
            }
        };
        if len <= Self::HEADER {
            // SAFETY: owned here, not used after.
            unsafe {
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(mapping);
            }
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "mapping too small"));
        }
        Ok(SharedMemory { mapping, base: view.Value.cast(), len })
    }

    /// Mapped bytes (header included; page-rounded for an opened mapping)
    pub fn len(&self) -> usize {
        self.len
    }

    /// Never true: a mapping has at least the header
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The header's 64-bit word `index` (< HEADER / 8)
    pub fn word(&self, index: usize) -> &AtomicU64 {
        assert!(index < Self::HEADER / 8, "header word {index} out of range");
        // SAFETY: in bounds (checked above), 8-byte aligned (views are
        // page-aligned), alive as long as &self; AtomicU64 has u64's layout
        // and every access to these bytes goes through atomics.
        unsafe { &*(self.base.add(index * 8) as *const AtomicU64) }
    }

    /// Bytes after the header
    pub fn data_len(&self) -> usize {
        self.len - Self::HEADER
    }

    /// Copies `data` to data offset `offset`. Panics if it does not fit.
    pub fn write(&self, offset: usize, data: &[u8]) {
        let end = offset.checked_add(data.len()).expect("offset overflow");
        assert!(end <= self.data_len(), "write {offset}+{} beyond {}", data.len(), self.data_len());
        // SAFETY: the destination is inside the view (checked above) and
        // cannot overlap `data`, which is process memory outside it.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.base.add(Self::HEADER + offset), data.len()) };
    }

    /// Copies data offset `offset` into `out`. Panics if it does not fit.
    pub fn read(&self, offset: usize, out: &mut [u8]) {
        let end = offset.checked_add(out.len()).expect("offset overflow");
        assert!(end <= self.data_len(), "read {offset}+{} beyond {}", out.len(), self.data_len());
        // SAFETY: the source is inside the view (checked above); `out` is
        // process memory outside it.
        unsafe { std::ptr::copy_nonoverlapping(self.base.add(Self::HEADER + offset), out.as_mut_ptr(), out.len()) };
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: the view and handle are owned by this value, released once.
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.base.cast() });
            let _ = CloseHandle(self.mapping);
        }
    }
}

/// What [`Event::wait_or_process`] saw
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Event,
    /// The process ended
    ProcessExited,
    Timeout,
}

/// A named auto-reset event
#[derive(Debug)]
pub struct Event {
    handle: HANDLE,
}

// SAFETY: an event handle may be set and waited on from any thread.
unsafe impl Send for Event {}
// SAFETY: as above.
unsafe impl Sync for Event {}

impl Event {
    /// Creates an auto-reset event without a name, not set (see
    /// [`SharedMemory::create_unnamed`])
    pub fn create_unnamed() -> std::io::Result<Event> {
        // SAFETY: default security, no name.
        let handle = unsafe { CreateEventW(None, false, false, None) }.map_err(io_error)?;
        Ok(Event { handle })
    }

    /// A handle to this event in `process`, allowed only to set and wait
    /// on it; its value there ([`Event::from_handle_value`])
    pub fn duplicate_into(&self, process: BorrowedHandle<'_>) -> std::io::Result<u64> {
        duplicate_into(self.handle, process, EVENT_ACCESS)
    }

    /// Takes an event handle another process duplicated into this one;
    /// owned from here (see [`SharedMemory::from_handle_value`])
    pub fn from_handle_value(value: u64) -> std::io::Result<Event> {
        Ok(Event { handle: handle_from_value(value)? })
    }

    /// Creates the auto-reset event `name`, not set. Fails if it exists.
    pub fn create(name: &str) -> std::io::Result<Event> {
        // SAFETY: default security, NUL-terminated name.
        let handle = unsafe { CreateEventW(None, false, false, &HSTRING::from(name)) }.map_err(io_error)?;
        // SAFETY: the last error of the call just above.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: opened here.
            let _ = unsafe { CloseHandle(handle) };
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "event exists"));
        }
        Ok(Event { handle })
    }

    /// Opens the existing event `name`
    pub fn open(name: &str) -> std::io::Result<Event> {
        // SAFETY: NUL-terminated name; the handle is owned by the result.
        let handle =
            unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE, false, &HSTRING::from(name)) }
                .map_err(io_error)?;
        Ok(Event { handle })
    }

    pub fn set(&self) {
        // SAFETY: an event handle owned by self.
        let _ = unsafe { SetEvent(self.handle) };
    }

    /// Waits up to `timeout_ms` for the event; true if it was set
    pub fn wait(&self, timeout_ms: u32) -> bool {
        // SAFETY: an event handle owned by self.
        unsafe { WaitForSingleObject(self.handle, timeout_ms) == WAIT_OBJECT_0 }
    }

    /// Waits up to `timeout_ms` for the event or for `process` to end
    pub fn wait_or_process(&self, process: BorrowedHandle<'_>, timeout_ms: u32) -> Wake {
        let handles = [self.handle, HANDLE(process.as_raw_handle())];
        // SAFETY: both handles are valid for the call (self's, and one the
        // caller borrows to us); waits for either.
        let r = unsafe { WaitForMultipleObjects(&handles, false, timeout_ms) };
        if r == WAIT_OBJECT_0 {
            Wake::Event
        } else if r.0 == WAIT_OBJECT_0.0 + 1 {
            Wake::ProcessExited
        } else if r == WAIT_TIMEOUT {
            Wake::Timeout
        } else {
            // WAIT_FAILED (a bad handle): treat as gone rather than spin
            Wake::ProcessExited
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: owned by self, closed once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// A duplicated mapping: map for reading and writing, and query its size
/// (SECTION_QUERY | SECTION_MAP_WRITE | SECTION_MAP_READ)
const SECTION_ACCESS: u32 = 0x0001 | FILE_MAP_WRITE.0 | FILE_MAP_READ.0;
/// A duplicated event: set it and wait on it
const EVENT_ACCESS: u32 = EVENT_MODIFY_STATE.0 | SYNCHRONIZATION_SYNCHRONIZE.0;

fn duplicate_into(handle: HANDLE, process: BorrowedHandle<'_>, access: u32) -> std::io::Result<u64> {
    let mut target = HANDLE::default();
    // SAFETY: `handle` is owned by the caller's value and live; `process`
    // is a process handle the caller borrows to us; the new handle lives
    // in that process (not inheritable) and is that process's to close.
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            HANDLE(process.as_raw_handle()),
            &mut target,
            access,
            false,
            DUPLICATE_HANDLE_OPTIONS(0),
        )
    }
    .map_err(io_error)?;
    Ok(target.0 as usize as u64)
}

/// A handle value from a peer, as this process's handle
fn handle_from_value(value: u64) -> std::io::Result<HANDLE> {
    // Handle values are multiples of 4 below 2^32 (kernel handle tables);
    // zero and the pseudo handles (-1, -2, ...) are never duplicated ones
    if value == 0 || !value.is_multiple_of(4) || value > u64::from(u32::MAX) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a duplicated handle"));
    }
    Ok(HANDLE(value as usize as *mut std::ffi::c_void))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// Unnamed objects reach another process (here: this one again) only
    /// through duplicated handles, with the access a peer needs
    #[test]
    fn unnamed_objects_travel_as_duplicated_handles() {
        // SAFETY: the current process's pseudo handle is always valid.
        let me = unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess().0) };
        let a = SharedMemory::create_unnamed(64 * 1024).unwrap();
        let value = a.duplicate_into(me).unwrap();
        let b = SharedMemory::from_handle_value(value).unwrap();
        assert_eq!(b.len(), 64 * 1024);
        a.word(5).store(7, Ordering::Release);
        assert_eq!(b.word(5).load(Ordering::Acquire), 7);
        b.write(0, b"back");
        let mut out = [0u8; 4];
        a.read(0, &mut out);
        assert_eq!(&out, b"back");
        let e = Event::create_unnamed().unwrap();
        let f = Event::from_handle_value(e.duplicate_into(me).unwrap()).unwrap();
        f.set();
        assert!(e.wait(100));
        // Values that are no duplicated handle
        for v in [0, 3, u64::MAX, 1 << 40] {
            assert!(SharedMemory::from_handle_value(v).is_err());
        }
    }

    fn name(what: &str) -> String {
        format!(r"Local\BlueCaster_test_shm_{what}_{}", std::process::id())
    }

    #[test]
    fn two_views_share_bytes_and_words() {
        let n = name("views");
        let a = SharedMemory::create(&n, 64 * 1024).unwrap();
        assert!(SharedMemory::create(&n, 64 * 1024).is_err());
        let b = SharedMemory::open(&n).unwrap();
        assert_eq!(b.len(), 64 * 1024);
        a.word(3).store(42, Ordering::Release);
        assert_eq!(b.word(3).load(Ordering::Acquire), 42);
        a.write(10, b"hello");
        let mut out = [0u8; 5];
        b.read(10, &mut out);
        assert_eq!(&out, b"hello");
        assert_eq!(a.word(4).load(Ordering::Relaxed), 0, "zeroed");
    }

    #[test]
    #[should_panic]
    fn writes_past_the_end_panic() {
        let a = SharedMemory::create(&name("past"), 4096).unwrap();
        a.write(a.data_len() - 2, b"abc");
    }

    #[test]
    fn event_wakes_and_times_out() {
        let n = name("event");
        let e = Event::create(&n).unwrap();
        let o = Event::open(&n).unwrap();
        assert!(!e.wait(0));
        o.set();
        assert!(e.wait(100));
        assert!(!e.wait(0), "auto-reset");
        // CREATE_NO_WINDOW: no terminal window pops up on the desktop
        let mut child = std::os::windows::process::CommandExt::creation_flags(
            std::process::Command::new("cmd").args(["/C", "exit 0"]),
            0x0800_0000,
        )
        .spawn()
        .unwrap();
        use std::os::windows::io::AsHandle;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match e.wait_or_process(child.as_handle(), 50) {
                Wake::ProcessExited => break,
                Wake::Event => panic!("not set"),
                Wake::Timeout => assert!(std::time::Instant::now() < deadline),
            }
        }
        child.wait().unwrap();
    }
}
