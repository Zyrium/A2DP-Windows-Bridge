//! Who is on the other end of a named pipe, from the pipe and the
//! process itself, never from what the peer says: the client's token (by
//! impersonating it for a moment), its process and image; the server's
//! process, session and image. Also the service-mode pipe: one name for
//! every user, a DACL that lets interactive users connect but not create
//! instances of their own, and a client open that asks only for what a
//! client needs.

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, RawHandle};
use std::path::PathBuf;

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, RevertToSelf,
    SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
    TokenElevation, TokenIntegrityLevel, TokenSessionId, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_SHARE_NONE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
    OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{PCWSTR, PWSTR};

/// Integrity levels (the last part of the token's label SID)
pub const INTEGRITY_MEDIUM: u32 = 0x2000;
pub const INTEGRITY_HIGH: u32 = 0x3000;
pub const INTEGRITY_SYSTEM: u32 = 0x4000;

/// The SID of LocalSystem
pub const SYSTEM_SID: &str = "S-1-5-18";

/// The process that connected to a pipe server, as Windows knows it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIdentity {
    /// GetNamedPipeClientProcessId
    pub pid: u32,
    /// The client token's session
    pub session_id: u32,
    /// The client token's user, "S-1-5-21-..."
    pub user_sid: String,
    /// The client token is elevated (the user answered a UAC prompt, or an
    /// administrator without UAC)
    pub elevated: bool,
    /// [`INTEGRITY_MEDIUM`], [`INTEGRITY_HIGH`], ...
    pub integrity: u32,
    /// The client process's program (None: it exited, or no access)
    pub image: Option<PathBuf>,
}

/// The process serving a pipe a client opened
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerProcess {
    /// GetNamedPipeServerProcessId
    pub pid: u32,
    /// The server process's session (0: a service's)
    pub session_id: Option<u32>,
    /// Its program (None: no access)
    pub image: Option<PathBuf>,
}

fn io_err(e: windows::core::Error) -> std::io::Error {
    std::io::Error::other(e)
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle this module opened and owns.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A token information class into an aligned buffer
fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> std::io::Result<Vec<u64>> {
    let mut needed = 0u32;
    // SAFETY: size query with no buffer; fails with
    // ERROR_INSUFFICIENT_BUFFER and sets `needed`.
    let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut needed) };
    // u64 elements keep the structures (pointers) aligned
    let mut buf = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // Fixed-size classes want their exact size, the others at least theirs
    let len = if needed == 0 { (buf.len() * 8) as u32 } else { needed };
    // SAFETY: the buffer has at least `len` bytes, `len` is passed.
    unsafe { GetTokenInformation(token, class, Some(buf.as_mut_ptr().cast()), len, &mut needed) }.map_err(io_err)?;
    Ok(buf)
}

fn sid_string(sid: PSID) -> std::io::Result<String> {
    let mut s = PWSTR::null();
    // SAFETY: `sid` points to a valid SID inside a live token buffer; `s`
    // receives a LocalAlloc'ed string freed below.
    unsafe { ConvertSidToStringSidW(sid, &mut s) }.map_err(io_err)?;
    // SAFETY: a NUL-terminated wide string from the call above.
    let out = unsafe { s.to_string() }.map_err(std::io::Error::other);
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
    unsafe { LocalFree(Some(HLOCAL(s.0.cast()))) };
    out
}

/// What a token says of its user
struct TokenFacts {
    user_sid: String,
    elevated: bool,
    integrity: u32,
    session_id: u32,
}

fn token_facts(token: HANDLE) -> std::io::Result<TokenFacts> {
    let user = token_info(token, TokenUser)?;
    // SAFETY: GetTokenInformation(TokenUser) filled the aligned buffer with
    // a TOKEN_USER at its start; it lives while `user` does.
    let user_sid = sid_string(unsafe { &*(user.as_ptr() as *const TOKEN_USER) }.User.Sid)?;
    let elevation = token_info(token, TokenElevation)?;
    // SAFETY: as above, a TOKEN_ELEVATION.
    let elevated = unsafe { &*(elevation.as_ptr() as *const TOKEN_ELEVATION) }.TokenIsElevated != 0;
    let label = token_info(token, TokenIntegrityLevel)?;
    // SAFETY: as above, a TOKEN_MANDATORY_LABEL whose SID points into the
    // buffer; the SID's sub-authority count is read before the last one.
    let integrity = unsafe {
        let sid = (*(label.as_ptr() as *const TOKEN_MANDATORY_LABEL)).Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 { 0 } else { *GetSidSubAuthority(sid, u32::from(count) - 1) }
    };
    let session = token_info(token, TokenSessionId)?;
    // SAFETY: TokenSessionId fills a DWORD.
    let session_id = unsafe { *(session.as_ptr() as *const u32) };
    Ok(TokenFacts { user_sid, elevated, integrity, session_id })
}

/// The program process `pid` runs (QueryFullProcessImageNameW)
pub fn process_image(pid: u32) -> std::io::Result<PathBuf> {
    // SAFETY: plain values in; the handle is closed by `Handle`.
    let process = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(io_err)?);
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` UTF-16 units; `len` receives the length.
    unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }
        .map_err(io_err)?;
    Ok(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// Whether this process's token is elevated
pub fn current_process_elevated() -> std::io::Result<bool> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess is a pseudo handle; `token` is closed by
    // `Handle`.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(io_err)?;
    let token = Handle(token);
    token_facts(token.0).map(|f| f.elevated)
}

/// The session of the physical console (None: no one is attached to it)
pub fn active_console_session() -> Option<u32> {
    // SAFETY: no arguments.
    let s = unsafe { windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId() };
    (s != u32::MAX).then_some(s)
}

/// The client on the server end `pipe`, from its token: Windows hands the
/// server the client's token while it impersonates the client, here only
/// for the few calls that read it. Call it after the first read from the
/// pipe (Windows lets a server impersonate only after that). Fails for a
/// client that allowed only anonymous impersonation.
pub fn client_identity(pipe: BorrowedHandle<'_>) -> std::io::Result<ClientIdentity> {
    let pid = crate::pipe::client_process_id(pipe)?;
    let raw = HANDLE(pipe.as_raw_handle());
    // SAFETY: a connected pipe handle borrowed for the call. From here
    // until RevertToSelf this thread runs as the client; nothing in
    // between can fail without reverting.
    unsafe { windows::Win32::System::Pipes::ImpersonateNamedPipeClient(raw) }.map_err(io_err)?;
    let mut token = HANDLE::default();
    // SAFETY: the current thread's pseudo handle; OpenAsSelf checks the
    // access against this process's own token, not the client's.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) };
    // SAFETY: no arguments. A thread that cannot stop running as the
    // client must not run anything else.
    if unsafe { RevertToSelf() }.is_err() {
        std::process::abort();
    }
    opened.map_err(io_err)?;
    let token = Handle(token);
    let facts = token_facts(token.0)?;
    Ok(ClientIdentity {
        pid,
        session_id: facts.session_id,
        user_sid: facts.user_sid,
        elevated: facts.elevated,
        integrity: facts.integrity,
        image: process_image(pid).ok(),
    })
}

/// The process serving the pipe this client end `pipe` opened
pub fn server_process(pipe: BorrowedHandle<'_>) -> std::io::Result<ServerProcess> {
    let mut pid = 0u32;
    // SAFETY: a pipe handle borrowed for the call; valid out pointer.
    unsafe { windows::Win32::System::Pipes::GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
        .map_err(io_err)?;
    Ok(ServerProcess { pid, session_id: crate::pipe::process_session_id(pid).ok(), image: process_image(pid).ok() })
}

/// Read and write for the pipe's clients, but not FILE_APPEND_DATA, which
/// on a pipe means "create another instance": FILE_GENERIC_READ,
/// FILE_WRITE_DATA, FILE_WRITE_EA, FILE_WRITE_ATTRIBUTES
const CLIENT_ACCESS: u32 = 0x0012_019B;

/// One instance of the service-mode pipe `name`: interactive users may
/// connect (read and write) but not serve the name themselves; SYSTEM,
/// administrators and this process's own user have full access (the core
/// creates the next instances). `first`: fails if another process owns
/// the name already. Clients open it with [`open_client`].
pub fn create_service_server(name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
    let own = crate::pipe::current_user_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{own})(A;;0x{CLIENT_ACCESS:x};;;IU)")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: sddl is NUL-terminated; sd receives a LocalAlloc'ed security
    // descriptor freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None)
    }
    .map_err(io_err)?;
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
    server
}

/// Opens the client end of `name` with the access a client needs (no
/// FILE_APPEND_DATA: the service-mode pipe grants no more) and lets the
/// server only identify this process, not act as it. Call inside a tokio
/// runtime.
pub fn open_client(name: &str) -> std::io::Result<NamedPipeClient> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_WRITE_DATA.0 | FILE_WRITE_ATTRIBUTES.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            None,
        )
    }
    .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    // SAFETY: a pipe handle just opened for overlapped I/O, owned from here
    // by the NamedPipeClient (which closes it).
    unsafe { NamedPipeClient::from_raw_handle(handle.0 as RawHandle) }
}

/// `bytes` random bytes from the system's generator, as hex
pub fn random_hex(bytes: usize) -> std::io::Result<String> {
    use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut buf = vec![0u8; bytes];
    // SAFETY: the buffer is passed with its length.
    let status = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() {
        return Err(std::io::Error::other(format!("BCryptGenRandom 0x{:08X}", status.0)));
    }
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    /// This process connects to itself: the server sees this process, its
    /// session, its user and program, and its elevation as it is
    #[test]
    fn the_client_is_this_process() {
        let name = format!(r"\\.\pipe\bluecaster-test-peer-{}", std::process::id());
        rt().block_on(async {
            let mut server = crate::pipe::create_server(&name, true).unwrap();
            let mut client = open_client(&name).unwrap();
            server.connect().await.unwrap();
            client.write_all(b"x").await.unwrap();
            let mut b = [0u8; 1];
            server.read_exact(&mut b).await.unwrap();
            use std::os::windows::io::AsHandle;
            let id = client_identity(server.as_handle()).unwrap();
            assert_eq!(id.pid, std::process::id());
            assert_eq!(id.session_id, crate::pipe::process_session_id(std::process::id()).unwrap());
            assert_eq!(id.user_sid, crate::pipe::current_user_sid().unwrap());
            assert_eq!(id.elevated, current_process_elevated().unwrap());
            assert!(id.integrity >= INTEGRITY_MEDIUM, "{:#x}", id.integrity);
            let me = std::env::current_exe().unwrap();
            assert_eq!(
                id.image.as_deref().map(|p| p.as_os_str().to_ascii_lowercase()),
                Some(me.as_os_str().to_ascii_lowercase())
            );
            let srv = server_process(client.as_handle()).unwrap();
            assert_eq!((srv.pid, srv.session_id), (std::process::id(), Some(id.session_id)));
            assert!(srv.image.is_some());
        });
    }

    /// The service-mode pipe: this user serves it and connects through
    /// [`open_client`]; a second first instance fails
    #[test]
    fn the_service_pipe_takes_clients() {
        let name = format!(r"\\.\pipe\bluecaster-test-service-{}", std::process::id());
        rt().block_on(async {
            let mut server = create_service_server(&name, true).unwrap();
            assert!(create_service_server(&name, true).is_err());
            let _second = create_service_server(&name, false).unwrap();
            let mut client = open_client(&name).unwrap();
            server.connect().await.unwrap();
            client.write_all(b"hi").await.unwrap();
            let mut b = [0u8; 2];
            server.read_exact(&mut b).await.unwrap();
            assert_eq!(&b, b"hi");
            assert!(open_client(&format!("{name}-none")).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound));
        });
    }

    #[test]
    fn random_hex_is_random() {
        let (a, b) = (random_hex(16).unwrap(), random_hex(16).unwrap());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(active_console_session().is_none_or(|s| s < u32::MAX));
    }
}
