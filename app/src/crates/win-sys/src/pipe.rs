//! The user's Named Pipe for the UI connection: the user's SID (part of
//! the pipe name, so each logged-on user has their own core) and the server
//! end with a DACL that lets only this user (and SYSTEM) in. The default
//! DACL of a named pipe grants read access to Everyone.

use std::ffi::c_void;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

/// The SID of the user this process runs as, "S-1-5-21-..."
pub fn current_user_sid() -> std::io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no
    // closing; `token` is a valid out pointer.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(std::io::Error::other)?;
    let result = (|| {
        let mut needed = 0u32;
        // SAFETY: size query with no buffer; fails with
        // ERROR_INSUFFICIENT_BUFFER and sets `needed`.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
        // u64 elements keep the TOKEN_USER (pointers) aligned
        let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: the buffer has at least `needed` bytes, its size is passed.
        unsafe {
            GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), (buf.len() * 8) as u32, &mut needed)
        }
        .map_err(std::io::Error::other)?;
        // SAFETY: GetTokenInformation(TokenUser) filled the buffer with a
        // TOKEN_USER at its start; the buffer is aligned for it and lives
        // until the end of this closure.
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let mut sid_str = PWSTR::null();
        // SAFETY: user.User.Sid points into `buf`, a valid SID; sid_str
        // receives a LocalAlloc'ed string freed below.
        unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_str) }.map_err(std::io::Error::other)?;
        // SAFETY: sid_str is a NUL-terminated wide string from the call above.
        let s = unsafe { sid_str.to_string() }.map_err(std::io::Error::other);
        // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
        unsafe { LocalFree(Some(HLOCAL(sid_str.0.cast()))) };
        s
    })();
    // SAFETY: `token` is a handle this function opened and owns.
    let _ = unsafe { CloseHandle(token) };
    result
}

/// Creates one instance of the pipe `name` with access for this user and
/// SYSTEM only. `first`: the first instance (fails if another process owns
/// the name already, i.e. another core runs for this user).
pub fn create_server(name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
    let sid = current_user_sid()?;
    // Protected DACL: generic all for the user and SYSTEM, nobody else
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)").encode_utf16().chain([0]).collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: sddl is NUL-terminated; sd receives a LocalAlloc'ed security
    // descriptor freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None)
    }
    .map_err(std::io::Error::other)?;
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let mut options = ServerOptions::new();
    options.first_pipe_instance(first).reject_remote_clients(true);
    // SAFETY: attrs is a valid SECURITY_ATTRIBUTES whose descriptor lives
    // until after the call (CreateNamedPipeW copies it).
    let server =
        unsafe { options.create_with_security_attributes_raw(name, std::ptr::from_mut(&mut attrs).cast::<c_void>()) };
    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
    unsafe { LocalFree(Some(HLOCAL(sd.0))) };
    match &server {
        Ok(_) => tracing::debug!("pipe: instance of {name} created (first {first}, owner {sid})"),
        Err(e) => {
            tracing::debug!("pipe: creating {name} (first {first}) failed: {e} (os error {:?})", e.raw_os_error())
        }
    }
    server
}

/// The process ID of the client connected to the server end `pipe`
/// (GetNamedPipeClientProcessId): who is on the other side, for the
/// session agent's authorization
pub fn client_process_id(pipe: std::os::windows::io::BorrowedHandle<'_>) -> std::io::Result<u32> {
    use std::os::windows::io::AsRawHandle;
    let mut pid = 0u32;
    // SAFETY: the handle is a pipe handle borrowed for the call; `pid` is a
    // valid out pointer.
    unsafe { windows::Win32::System::Pipes::GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(std::io::Error::other)?;
    Ok(pid)
}

/// The Remote Desktop Services session (logon session number) process
/// `pid` runs in (ProcessIdToSessionId); 0 is the services' session
pub fn process_session_id(pid: u32) -> std::io::Result<u32> {
    let mut session = 0u32;
    // SAFETY: plain value in, valid out pointer.
    unsafe { windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut session) }
        .map_err(std::io::Error::other)?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_looks_like_one() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[test]
    fn this_process_has_a_session() {
        // Tests run in the signed-in user's session or a service's (0)
        assert!(process_session_id(std::process::id()).is_ok());
        assert!(process_session_id(3).is_err(), "PIDs are multiples of 4");
    }
}
