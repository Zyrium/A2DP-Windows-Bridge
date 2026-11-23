//! What the input helper (crate input-helper) needs from Windows besides
//! `SendInput` (crate::input_inject): the input desktop it follows (the
//! default desktop, the sign-in and lock screen, the UAC prompt's secure
//! desktop), its window station, its parent process (the only process it
//! takes input from), the record channel of its test switch, and the
//! Secure Attention Sequence (Ctrl+Alt+Del) through `SendSAS`.
//!
//! The decisions (when to attach again, what to release) are in the
//! input-helper crate and tested there against a fake; this module only
//! makes the calls.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::{FILETIME, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, DESKTOP_JOURNALPLAYBACK,
    DESKTOP_READOBJECTS, DESKTOP_WRITEOBJECTS, GetProcessWindowStation, GetUserObjectInformationW, HDESK, HWINSTA,
    OpenInputDesktop, OpenWindowStationW, SetProcessWindowStation, SetThreadDesktop, UOI_NAME,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};
use windows::core::{BOOL, s, w};

fn io_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// What the injecting thread asks of the input desktop: read and write
/// its objects, and DESKTOP_JOURNALPLAYBACK, without which `SendInput`
/// from a thread on that desktop fails with access denied
const DESKTOP_ACCESS: DESKTOP_ACCESS_FLAGS =
    DESKTOP_ACCESS_FLAGS(DESKTOP_READOBJECTS.0 | DESKTOP_WRITEOBJECTS.0 | DESKTOP_JOURNALPLAYBACK.0);

/// The desktop that receives input right now (OpenInputDesktop): the
/// user's default desktop, or Winlogon's (sign-in, lock screen, UAC
/// prompt). Closed on drop; keep it while a thread is attached to it.
#[derive(Debug)]
pub struct InputDesktop {
    handle: HDESK,
}

// SAFETY: a desktop handle is a process-wide kernel handle; nothing in this
// type is tied to the thread that opened it.
unsafe impl Send for InputDesktop {}

impl InputDesktop {
    /// Opens the input desktop. Fails with access denied for a process
    /// that may not open it (an ordinary user process while the secure
    /// desktop is shown).
    pub fn open() -> io::Result<InputDesktop> {
        // SAFETY: plain values; the handle is owned by the result.
        let handle = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS) }.map_err(io_err)?;
        Ok(InputDesktop { handle })
    }

    /// Its name ("Default", "Winlogon", ...): the decision logic compares
    /// names to tell a switch
    pub fn name(&self) -> io::Result<String> {
        let mut buf = [0u16; 256];
        let mut needed = 0u32;
        // SAFETY: a live desktop handle; the buffer and its byte size are
        // passed; `needed` is a valid out pointer.
        unsafe {
            GetUserObjectInformationW(
                HANDLE(self.handle.0),
                UOI_NAME,
                Some(buf.as_mut_ptr().cast::<c_void>()),
                (buf.len() * 2) as u32,
                Some(&mut needed),
            )
        }
        .map_err(io_err)?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(String::from_utf16_lossy(&buf[..len]))
    }

    /// Makes it the desktop of the calling thread (SetThreadDesktop), so
    /// that the thread's `SendInput` goes there. Fails for a thread that
    /// owns windows or hooks on its current desktop.
    pub fn attach_current_thread(&self) -> io::Result<()> {
        // SAFETY: a live desktop handle opened by this process.
        unsafe { SetThreadDesktop(self.handle) }.map_err(io_err)
    }
}

impl Drop for InputDesktop {
    fn drop(&mut self) {
        // SAFETY: the handle this value owns. Fails (harmlessly) if a
        // thread is still attached to it; the caller attaches the next
        // desktop first.
        let _ = unsafe { CloseDesktop(self.handle) };
    }
}

/// The name of this process's window station
fn station_name(station: HWINSTA) -> io::Result<String> {
    let mut buf = [0u16; 256];
    let mut needed = 0u32;
    // SAFETY: a window station handle; the buffer and its byte size are
    // passed.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(station.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        )
    }
    .map_err(io_err)?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(&buf[..len]))
}

/// Makes sure this process is on the interactive window station WinSta0,
/// where the input desktops are (a process started by a service with
/// `winsta0\default` is there already). Ok(true): it was moved there.
/// Call before opening any desktop.
pub fn ensure_interactive_station() -> io::Result<bool> {
    // SAFETY: no arguments; the handle returned must not be closed.
    let current = unsafe { GetProcessWindowStation() }.map_err(io_err)?;
    if station_name(current)?.eq_ignore_ascii_case("WinSta0") {
        return Ok(false);
    }
    // WINSTA_ENUMDESKTOPS | WINSTA_READATTRIBUTES | WINSTA_READSCREEN
    // | WINSTA_ACCESSGLOBALATOMS | WINSTA_EXITWINDOWS... only what
    // switching desktops on it needs: enumerate desktops and read
    // attributes, and WINSTA_ACCESSGLOBALATOMS for SendInput's atoms
    const ACCESS: u32 = 0x0001 | 0x0002 | 0x0020;
    // SAFETY: a NUL-terminated literal; the handle is kept for the life of
    // the process (SetProcessWindowStation requires it to stay open).
    let station = unsafe { OpenWindowStationW(w!("WinSta0"), false, ACCESS) }.map_err(io_err)?;
    // SAFETY: a window station handle just opened.
    if let Err(e) = unsafe { SetProcessWindowStation(station) } {
        // SAFETY: the handle opened above, not in use.
        let _ = unsafe { CloseWindowStation(station) };
        return Err(io_err(e));
    }
    Ok(true)
}

fn file_time(t: FILETIME) -> u64 {
    u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)
}

fn creation_time(process: HANDLE) -> io::Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    // SAFETY: a live process handle with query access; valid out pointers.
    unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }.map_err(io_err)?;
    Ok(file_time(created))
}

/// The process that started this one, held open so that its process id
/// cannot be reused while this process runs
#[derive(Debug)]
pub struct ParentProcess {
    pid: u32,
    handle: OwnedHandle,
}

impl ParentProcess {
    /// The parent of this process as Windows recorded it at creation. Fails
    /// if the parent has exited, or if the process with that id now is a
    /// later one (created after this process: the id was reused).
    pub fn open() -> io::Result<ParentProcess> {
        let me = std::process::id();
        let entries = crate::process::processes().map_err(io_err)?;
        let pid = entries
            .iter()
            .find(|e| e.pid == me)
            .map(|e| e.parent_pid)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "this process is not in the snapshot"))?;
        // SAFETY: plain values; the handle is owned below.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(io_err)?;
        // SAFETY: a handle just opened and owned from here.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        // SAFETY: the pseudo handle of this process.
        let own = creation_time(unsafe { GetCurrentProcess() })?;
        if creation_time(HANDLE(handle.as_raw_handle()))? > own {
            return Err(io::Error::new(io::ErrorKind::NotFound, "the parent exited and its id was reused"));
        }
        let parent = ParentProcess { pid, handle };
        if parent.exited(0) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "the parent has exited"));
        }
        Ok(parent)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Waits up to `timeout_ms` for it to exit; true when it has
    pub fn exited(&self, timeout_ms: u32) -> bool {
        // SAFETY: a live process handle with SYNCHRONIZE access.
        unsafe { WaitForSingleObject(HANDLE(self.handle.as_raw_handle()), timeout_ms) == WAIT_OBJECT_0 }
    }
}

/// Whether `SendInput`'s failure was access denied (the thread's desktop
/// is not the input desktop any more, or it lacks the access)
pub fn is_access_denied(e: &crate::input_inject::InjectError) -> bool {
    let crate::input_inject::InjectError::Partial { source, .. } = e;
    // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)
    source.code().0 as u32 == 0x8007_0005
}

/// An anonymous pipe whose write end the next child processes inherit
/// (the input helper's test switch: the test reads what the helper
/// records). The read end is not inheritable.
pub fn inheritable_pipe() -> io::Result<(File, OwnedHandle)> {
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: BOOL::from(true),
    };
    // SAFETY: valid out pointers and attributes; both handles are owned below.
    unsafe { CreatePipe(&mut read, &mut write, Some(&attrs), 0) }.map_err(io_err)?;
    // SAFETY: handles just created by CreatePipe, owned from here.
    let (read, write) = unsafe { (File::from_raw_handle(read.0), OwnedHandle::from_raw_handle(write.0)) };
    // SAFETY: a live handle of this process.
    unsafe { SetHandleInformation(HANDLE(read.as_raw_handle()), HANDLE_FLAG_INHERIT.0, Default::default()) }
        .map_err(io_err)?;
    Ok((read, write))
}

/// The handle number of an inheritable handle, for a child's command line
pub fn handle_number(h: BorrowedHandle<'_>) -> usize {
    h.as_raw_handle() as usize
}

/// Takes an inherited handle `number` as a writable file, if it is a pipe
/// (anything else is left alone: the number may name a handle this
/// process did not inherit)
pub fn inherited_pipe(number: usize) -> Option<File> {
    if number == 0 || !number.is_multiple_of(4) {
        return None;
    }
    let raw = HANDLE(number as *mut c_void);
    // SAFETY: GetFileType only reads the handle's type; an invalid number
    // makes it fail (FILE_TYPE_UNKNOWN).
    if unsafe { GetFileType(raw) } != FILE_TYPE_PIPE {
        return None;
    }
    // SAFETY: a pipe handle this process inherited and owns from here.
    Some(unsafe { File::from_raw_handle(raw.0) })
}

/// HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System
/// SoftwareSASGeneration: 0 none, 1 services, 2 Ease of Access
/// applications, 3 both. None: not configured (Windows then lets only
/// Ease of Access applications on the secure desktop generate it). Read
/// only; nothing here ever writes it.
pub fn software_sas_policy() -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: NUL-terminated literals; a u32 buffer with its size.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System"),
            w!("SoftwareSASGeneration"),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut value).cast::<c_void>()),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(value)
}

/// Generates the Secure Attention Sequence as a service (`SendSAS(FALSE)`,
/// sas.dll, loaded from System32 only when called). Windows does it only
/// for a caller running as a service, and only when
/// [`software_sas_policy`] admits services (1 or 3); otherwise nothing
/// happens. The caller checks both first.
pub fn send_sas_as_service() -> io::Result<()> {
    type SendSas = unsafe extern "system" fn(BOOL);
    // SAFETY: a NUL-terminated literal; System32 only, so no DLL planted
    // next to the executable is loaded.
    let module = unsafe { LoadLibraryExW(w!("sas.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(io_err)?;
    // SAFETY: a module just loaded (kept loaded: it is tiny, and the call
    // may still run in it); a NUL-terminated literal.
    let Some(proc) = (unsafe { GetProcAddress(module, s!("SendSAS")) }) else {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "SendSAS not found"));
    };
    // SAFETY: SendSAS is `VOID WINAPI SendSAS(BOOL AsUser)` (sas.h); the
    // pointer is that export.
    let send: SendSas = unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, SendSas>(proc) };
    // SAFETY: a valid function pointer of the documented signature;
    // FALSE: the caller is a service, not the signed-in user.
    unsafe { send(BOOL::from(false)) };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn the_parent_is_open_and_running() {
        let p = ParentProcess::open().unwrap();
        assert!(p.pid() != 0 && p.pid() != std::process::id());
        assert!(!p.exited(0));
    }

    #[test]
    fn this_process_is_on_the_interactive_station() {
        // Tests run in the user's session: already on WinSta0, nothing moves
        assert!(!ensure_interactive_station().unwrap());
    }

    /// Read-only: the input desktop's name when this user may open it (not
    /// while the secure desktop is shown); attaching a fresh thread to it
    /// changes nothing for anyone else
    #[test]
    fn the_input_desktop_can_be_opened_and_named() {
        if let Ok(d) = InputDesktop::open() {
            assert!(!d.name().unwrap().is_empty());
            std::thread::spawn(move || d.attach_current_thread().unwrap()).join().unwrap();
        }
    }

    #[test]
    fn an_inherited_pipe_carries_bytes_and_other_numbers_are_refused() {
        let (mut read, write) = inheritable_pipe().unwrap();
        let n = handle_number(std::os::windows::io::AsHandle::as_handle(&write));
        std::mem::forget(write);
        let mut file = inherited_pipe(n).unwrap();
        file.write_all(b"ok").unwrap();
        drop(file);
        // Not to the end: a child another test starts meanwhile inherits
        // the write end too and keeps it open
        let mut b = [0u8; 2];
        read.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ok");
        assert!(inherited_pipe(0).is_none() && inherited_pipe(6).is_none() && inherited_pipe(0x7FFF_FFF0).is_none());
        let _ = software_sas_policy();
    }
}
