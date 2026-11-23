//! What the service control manager tells anyone about an installed
//! service: its configured program (the command line) and, while it runs,
//! its process id. Read-only: the manager is opened with `SC_MANAGER_CONNECT`
//! and the service with `SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS`,
//! which a service's default security descriptor grants to interactive
//! users. Nothing here starts, stops or changes a service.
//!
//! The UI uses it to check the process behind the service's pipe: an
//! unelevated program cannot read a SYSTEM process's token, but it can ask
//! the manager which process the service runs as and compare that with the
//! pipe server's process id.

use std::io;

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_DOES_NOT_EXIST};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW, QueryServiceStatusEx,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_STATUS_PROCESS,
};
use windows::core::PCWSTR;

/// What the manager says of a service
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceInfo {
    /// No service of that name
    NotInstalled,
    /// It exists, but its configuration or state may not be read
    Denied,
    Found {
        /// The configured command line (lpBinaryPathName), as the manager
        /// stores it: the program, quoted or not, and its arguments
        command_line: String,
        /// The service's process while it runs (SERVICE_RUNNING)
        pid: Option<u32>,
    },
}

/// Closes a service handle when dropped
struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle this module opened and owns
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn os(e: &windows::core::Error) -> Option<u32> {
    // HRESULT_FROM_WIN32: 0x8007xxxx
    let code = e.code().0 as u32;
    (code & 0xFFFF_0000 == 0x8007_0000).then_some(code & 0xFFFF)
}

/// The service `name` on this PC (read-only queries)
pub fn query(name: &str) -> io::Result<ServiceInfo> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: null machine and database name: the local active database
    let scm =
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }.map_err(io::Error::other)?;
    let scm = Handle(scm);
    // SAFETY: a valid manager handle and a NUL-terminated name
    let service =
        match unsafe { OpenServiceW(scm.0, PCWSTR(wide.as_ptr()), SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS) } {
            Ok(h) => Handle(h),
            Err(e) if os(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST.0) => return Ok(ServiceInfo::NotInstalled),
            Err(e) if os(&e) == Some(ERROR_ACCESS_DENIED.0) => return Ok(ServiceInfo::Denied),
            Err(e) => return Err(io::Error::other(e)),
        };
    let command_line = match config_command_line(&service) {
        Ok(c) => c,
        Err(e) if os(&e) == Some(ERROR_ACCESS_DENIED.0) => return Ok(ServiceInfo::Denied),
        Err(e) => return Err(io::Error::other(e)),
    };
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0u32;
    // SAFETY: the buffer is a SERVICE_STATUS_PROCESS, its size given
    let bytes = unsafe {
        std::slice::from_raw_parts_mut((&raw mut status).cast::<u8>(), std::mem::size_of::<SERVICE_STATUS_PROCESS>())
    };
    // SAFETY: a valid service handle; the buffer and its length as above
    match unsafe { QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed) } {
        Ok(()) => {}
        Err(e) if os(&e) == Some(ERROR_ACCESS_DENIED.0) => return Ok(ServiceInfo::Denied),
        Err(e) => return Err(io::Error::other(e)),
    }
    let running = status.dwCurrentState == SERVICE_RUNNING && status.dwProcessId != 0;
    Ok(ServiceInfo::Found { command_line, pid: running.then_some(status.dwProcessId) })
}

/// QueryServiceConfigW's lpBinaryPathName (the size asked first)
fn config_command_line(service: &Handle) -> windows::core::Result<String> {
    let mut needed = 0u32;
    // SAFETY: no buffer: only the size needed is written
    match unsafe { QueryServiceConfigW(service.0, None, 0, &mut needed) } {
        Err(e) if os(&e) == Some(ERROR_INSUFFICIENT_BUFFER.0) => {}
        Err(e) => return Err(e),
        Ok(()) => return Ok(String::new()),
    }
    // u64s: the structure's alignment
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    let config = buf.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    // SAFETY: a buffer of at least `needed` bytes, aligned for the struct
    unsafe { QueryServiceConfigW(service.0, Some(config), (buf.len() * 8) as u32, &mut needed) }?;
    // SAFETY: filled by the call above; the string points into `buf`
    let path = unsafe { (*config).lpBinaryPathName };
    if path.is_null() {
        return Ok(String::new());
    }
    // SAFETY: a NUL-terminated string inside `buf`, alive here
    Ok(unsafe { path.to_string() }.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a name no service has: the real BlueCaster service is never
    /// asked about in a test
    #[test]
    fn a_service_that_does_not_exist_is_not_installed() {
        let name = format!("BlueCaster-test-none-{}", std::process::id());
        assert_eq!(query(&name).unwrap(), ServiceInfo::NotInstalled);
    }
}
