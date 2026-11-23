//! Start a process in a user's session: the agent with the signed-in
//! user's token, the input helper with this service's own (SYSTEM) token
//! moved into that session.
//!
//! In a service (this process runs in session 0) [`launch`] creates the
//! process with CreateProcessAsUserW on the interactive desktop of the
//! given session, with a clean environment block built for the new
//! process's token (only the variables the caller names are added). As an
//! ordinary process (any other session) it is a plain spawn in the own
//! session and the identity is ignored, so tests and the tray build run
//! the same code without privileges.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, TerminateProcess, WaitForSingleObject};

/// Whose token the new process gets
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    /// The user signed in to the session (WTSQueryUserToken)
    User,
    /// This service's own token (LocalSystem), moved into the session
    System,
}

/// What to start, where and as whom
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub session: u32,
    pub identity: Identity,
}

/// A process this module started: closed (not killed) on drop
#[derive(Debug)]
pub struct Child {
    pid: u32,
    process: OwnedHandle,
}

impl Child {
    /// Its process id
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Its process handle (full access: the creator's handle), e.g. to
    /// duplicate handles into it or wait for it
    pub fn handle(&self) -> BorrowedHandle<'_> {
        // SAFETY: the handle is owned by self and lives as long as the borrow.
        unsafe { BorrowedHandle::borrow_raw(self.process.as_raw_handle()) }
    }

    /// Waits until it exits; its exit code
    pub fn wait(&self) -> io::Result<u32> {
        // SAFETY: an owned, live process handle.
        let r = unsafe { WaitForSingleObject(self.raw(), INFINITE) };
        if r != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        self.exit_code()
    }

    /// Its exit code if it has exited
    pub fn try_wait(&self) -> io::Result<Option<u32>> {
        // SAFETY: an owned, live process handle; zero timeout.
        match unsafe { WaitForSingleObject(self.raw(), 0) } {
            WAIT_OBJECT_0 => self.exit_code().map(Some),
            WAIT_TIMEOUT => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Ends it (exit code 1); no error if it has exited already
    pub fn kill(&self) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        // SAFETY: an owned process handle with PROCESS_TERMINATE (the
        // creator's handle has all access).
        unsafe { TerminateProcess(self.raw(), 1) }.map_err(io_err)
    }

    fn exit_code(&self) -> io::Result<u32> {
        let mut code = 0u32;
        // SAFETY: an owned process handle; valid out pointer.
        unsafe { GetExitCodeProcess(self.raw(), &mut code) }.map_err(io_err)?;
        Ok(code)
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.process.as_raw_handle())
    }
}

fn io_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// This process's own handle (the pseudo handle: valid for the process's
/// lifetime, nothing to close)
pub fn current_process() -> BorrowedHandle<'static> {
    // SAFETY: GetCurrentProcess returns a pseudo handle that is always
    // valid in this process and needs no closing.
    unsafe { BorrowedHandle::borrow_raw(windows::Win32::System::Threading::GetCurrentProcess().0) }
}

/// The session of the physical console (None: none attached right now)
pub fn active_session() -> Option<u32> {
    crate::pipe_peer::active_console_session()
}

/// A user is signed in to `session` (its user name is not empty; the name
/// itself is not kept). False when the session does not exist or cannot be
/// queried.
pub fn session_has_user(session: u32) -> bool {
    use windows::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName,
    };
    let mut buf = windows::core::PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: valid out pointers; the buffer is freed with WTSFreeMemory.
    let ok = unsafe {
        WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), session, WTSUserName, &mut buf, &mut bytes)
    };
    if ok.is_err() || buf.is_null() {
        return false;
    }
    // SAFETY: a NUL-terminated string from the call above; its first unit
    // says whether it is empty.
    let has = unsafe { *buf.0 } != 0;
    // SAFETY: allocated by WTSQuerySessionInformationW.
    unsafe { WTSFreeMemory(buf.0.cast()) };
    has
}

/// The session this process runs in
pub fn own_session() -> Option<u32> {
    crate::pipe::process_session_id(std::process::id()).ok()
}

/// This process runs in session 0, where services run: [`launch`] then
/// creates processes in other sessions under other tokens
pub fn in_service_session() -> bool {
    own_session() == Some(0)
}

/// Starts `l`; see the module comment. Variables in `env` are added to the
/// new process's environment (in a service on top of the clean block for
/// its token; as an ordinary process on top of this process's own).
pub fn launch_with_env(l: &Launch, env: &[(String, String)]) -> io::Result<Child> {
    if in_service_session() { privileged::launch(l, env) } else { spawn(l, env) }
}

/// [`launch_with_env`] with no added variables
pub fn launch(l: &Launch) -> io::Result<Child> {
    launch_with_env(l, &[])
}

/// The ordinary-process path: a plain spawn in this session
fn spawn(l: &Launch, env: &[(String, String)]) -> io::Result<Child> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = std::process::Command::new(&l.exe);
    cmd.args(&l.args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    Ok(Child { pid, process: OwnedHandle::from(child) })
}

/// One argument quoted the way CommandLineToArgvW and the C runtime read it
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// The full command line for `l`: the program always quoted, then each
/// argument
pub fn command_line(l: &Launch) -> String {
    let mut s = format!("\"{}\"", l.exe.display());
    for a in &l.args {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    s
}

/// The desktop the new process starts on: the interactive one of the
/// session. The input helper switches to the input desktop itself (sign-in
/// screen, UAC prompt), which a SYSTEM process in WinSta0 may do.
pub const DESKTOP: &str = "winsta0\\default";

/// Adds `extra` to an environment block (`NAME=value\0...\0\0`, UTF-16),
/// replacing variables of the same name (case-insensitive, as Windows
/// compares them); the result is sorted as CreateProcess expects
pub fn merge_env_block(block: &[String], extra: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<String> = block
        .iter()
        .filter(|v| {
            let name = v.split_once('=').map(|(n, _)| n).unwrap_or(v);
            // names starting with '=' are the per-drive current directories
            !extra.iter().any(|(k, _)| !name.is_empty() && k.eq_ignore_ascii_case(name))
        })
        .cloned()
        .collect();
    vars.extend(extra.iter().map(|(k, v)| format!("{k}={v}")));
    vars.sort_by_key(|v| v.to_uppercase());
    let mut out: Vec<u16> = Vec::new();
    for v in &vars {
        out.extend(v.encode_utf16());
        out.push(0);
    }
    if vars.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// Splits an environment block into its `NAME=value` strings
///
/// # Safety
/// `block` must point to a valid UTF-16 environment block ending in two NULs.
unsafe fn read_env_block(block: *const u16) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = block;
    loop {
        let mut len = 0usize;
        // SAFETY: the caller guarantees a double-NUL-terminated block.
        while unsafe { *p.add(len) } != 0 {
            len += 1;
        }
        if len == 0 {
            break;
        }
        // SAFETY: `len` units were just read as non-NUL.
        let s = unsafe { std::slice::from_raw_parts(p, len) };
        out.push(String::from_utf16_lossy(s));
        // SAFETY: skip the string and its NUL, still inside the block.
        p = unsafe { p.add(len + 1) };
    }
    out
}

/// The privileged path: only reached in session 0 (a service). Written
/// against Microsoft's documentation; never executed in tests.
mod privileged {
    use super::*;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ALL_ACCESS,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary, TokenSessionId,
    };
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
    use windows::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, GetCurrentProcess, OpenProcessToken,
        PROCESS_INFORMATION, STARTUPINFOW,
    };
    use windows::core::{PCWSTR, PWSTR};

    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: a token handle this module opened and owns.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// The primary token the new process runs with
    fn token_for(identity: Identity, session: u32) -> io::Result<Token> {
        match identity {
            Identity::User => {
                let mut t = HANDLE::default();
                // SAFETY: valid out pointer. Needs LocalSystem with
                // SeTcbPrivilege; returns a primary token of the user signed
                // in to `session`.
                unsafe { WTSQueryUserToken(session, &mut t) }.map_err(io_err)?;
                Ok(Token(t))
            }
            Identity::System => {
                let mut own = HANDLE::default();
                // SAFETY: the pseudo handle of this process; valid out pointer.
                unsafe {
                    OpenProcessToken(
                        GetCurrentProcess(),
                        TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
                        &mut own,
                    )
                }
                .map_err(io_err)?;
                let own = Token(own);
                let mut dup = HANDLE::default();
                // SAFETY: a token opened with TOKEN_DUPLICATE; valid out pointer.
                unsafe {
                    DuplicateTokenEx(own.0, TOKEN_ALL_ACCESS, None, SecurityIdentification, TokenPrimary, &mut dup)
                }
                .map_err(io_err)?;
                let dup = Token(dup);
                // SAFETY: a primary token with TOKEN_ADJUST_SESSIONID (all
                // access); a u32 as TokenSessionId expects. Needs
                // SeTcbPrivilege (LocalSystem has it).
                unsafe {
                    SetTokenInformation(
                        dup.0,
                        TokenSessionId,
                        std::ptr::from_ref(&session).cast::<c_void>(),
                        size_of::<u32>() as u32,
                    )
                }
                .map_err(io_err)?;
                Ok(dup)
            }
        }
    }

    pub(super) fn launch(l: &Launch, env: &[(String, String)]) -> io::Result<Child> {
        let token = token_for(l.identity, l.session)?;
        // The environment block for the token, not inherited from the
        // service: the user's own variables (profile, temp folder).
        let mut raw_env: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid out pointer; the block is freed below.
        unsafe { CreateEnvironmentBlock(&mut raw_env, Some(token.0), false) }.map_err(io_err)?;
        // SAFETY: CreateEnvironmentBlock returns a double-NUL-terminated
        // UTF-16 block.
        let base = unsafe { read_env_block(raw_env.cast::<u16>()) };
        // SAFETY: allocated by CreateEnvironmentBlock above.
        let _ = unsafe { DestroyEnvironmentBlock(raw_env) };
        let block = merge_env_block(&base, env);

        let exe: Vec<u16> = l.exe.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
        let mut cmdline: Vec<u16> = command_line(l).encode_utf16().chain([0]).collect();
        let mut desktop: Vec<u16> = DESKTOP.encode_utf16().chain([0]).collect();
        let dir: Option<Vec<u16>> =
            l.exe.parent().map(|d| d.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect());
        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: every string is NUL-terminated and outlives the call;
        // the command line buffer is writable as the API requires; the
        // environment block is UTF-16 (CREATE_UNICODE_ENVIRONMENT); no
        // handles are inherited.
        unsafe {
            windows::Win32::System::Threading::CreateProcessAsUserW(
                Some(token.0),
                PCWSTR(exe.as_ptr()),
                Some(PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
                Some(block.as_ptr().cast::<c_void>()),
                dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
                &si,
                &mut pi,
            )
        }
        .map_err(io_err)?;
        // SAFETY: the thread handle CreateProcessAsUserW returned; not needed.
        let _ = unsafe { CloseHandle(pi.hThread) };
        // SAFETY: the process handle it returned, owned from here.
        let process = unsafe { OwnedHandle::from_raw_handle(pi.hProcess.0) };
        Ok(Child { pid: pi.dwProcessId, process })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_like_argv_reads_them() {
        assert_eq!(quote_arg("--agent"), "--agent");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("say \"hi\""), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r"C:\plain\path"), r"C:\plain\path");
    }

    #[test]
    fn the_command_line_quotes_the_program() {
        let l = Launch {
            exe: PathBuf::from(r"C:\Program Files\BlueCaster\bluecaster-core.exe"),
            args: vec!["--agent".into(), "--pipe".into(), r"\\.\pipe\x y".into()],
            session: 1,
            identity: Identity::User,
        };
        assert_eq!(
            command_line(&l),
            r#""C:\Program Files\BlueCaster\bluecaster-core.exe" --agent --pipe "\\.\pipe\x y""#
        );
    }

    #[test]
    fn the_environment_block_adds_replaces_and_ends_in_two_nuls() {
        let base = vec!["Path=C:\\Windows".to_string(), "TEMP=C:\\t".to_string(), "=C:=C:\\".to_string()];
        let block = merge_env_block(&base, &[("temp".into(), "D:\\x".into()), ("NEW".into(), "1".into())]);
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        // SAFETY: merge_env_block produces a double-NUL-terminated block.
        let back = unsafe { read_env_block(block.as_ptr()) };
        assert_eq!(back, vec!["=C:=C:\\", "NEW=1", "Path=C:\\Windows", "temp=D:\\x"]);
        let empty = merge_env_block(&[], &[]);
        assert_eq!(empty, vec![0, 0]);
    }

    #[test]
    fn an_ordinary_process_spawns_in_its_own_session() {
        assert!(!in_service_session(), "tests never run in session 0");
        let l = Launch {
            exe: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            args: vec!["/c".into(), "exit %BC_LAUNCH_TEST%".into()],
            session: 9999,
            identity: Identity::System,
        };
        let child = launch_with_env(&l, &[("BC_LAUNCH_TEST".into(), "7".into())]).unwrap();
        assert!(child.pid() != 0);
        assert_eq!(child.wait().unwrap(), 7);
        assert_eq!(child.try_wait().unwrap(), Some(7));
        child.kill().unwrap();
    }

    #[test]
    fn this_session_has_a_user_and_session_zero_has_none() {
        let own = own_session().unwrap();
        assert!(own != 0);
        assert!(session_has_user(own));
        assert!(!session_has_user(0));
        assert!(!session_has_user(0x7FFF_FFF0));
    }

    #[test]
    fn a_child_can_be_killed() {
        let l = Launch {
            exe: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            args: vec!["/c".into(), "ping -n 30 127.0.0.1 >nul".into()],
            session: 0,
            identity: Identity::User,
        };
        let child = launch(&l).unwrap();
        assert_eq!(child.try_wait().unwrap(), None);
        child.kill().unwrap();
        assert_eq!(child.wait().unwrap(), 1);
    }
}
