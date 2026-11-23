//! The Windows service itself: the service control manager's entry point
//! ([`run_dispatcher`]), the status this process reports to it
//! ([`StatusMachine`], start-pending and stop-pending with checkpoints),
//! the controls it sends ([`Control`]: stop, shutdown, session changes,
//! power events), and installing or removing the service ([`install`],
//! [`uninstall`]). Also the machine-wide data folder from the known-folder
//! API ([`program_data_dir`]).
//!
//! The pure parts (the status machine, decoding a control, the
//! configuration strings) are tested; [`run_dispatcher`]'s service side,
//! [`install`] and [`uninstall`] need the service control manager (and an
//! administrator) and are never called by tests.

use std::ffi::c_void;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

use windows::Win32::Foundation::{ERROR_CALL_NOT_IMPLEMENTED, NO_ERROR};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, QueryServiceStatus, RegisterServiceCtrlHandlerExW, SC_ACTION, SC_ACTION_RESTART, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_MANAGER_CREATE_SERVICE, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
    SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_CONFIG_DESCRIPTION, SERVICE_CONFIG_FAILURE_ACTIONS,
    SERVICE_CONFIG_FAILURE_ACTIONS_FLAG, SERVICE_CONFIG_PRESHUTDOWN_INFO, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
    SERVICE_CONFIG_SERVICE_SID_INFO, SERVICE_CONTROL_STOP, SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DESCRIPTIONW,
    SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONS_FLAG, SERVICE_FAILURE_ACTIONSW, SERVICE_PRESHUTDOWN_INFO,
    SERVICE_QUERY_STATUS, SERVICE_REQUIRED_PRIVILEGES_INFOW, SERVICE_SID_INFO, SERVICE_STATUS,
    SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows::core::{PCWSTR, PWSTR};

fn io_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

// --- Status ---------------------------------------------------------------

/// SERVICE_START_PENDING .. SERVICE_STOPPED
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    StartPending,
    Running,
    StopPending,
    Stopped,
}

impl State {
    fn code(self) -> u32 {
        match self {
            State::Stopped => 1,
            State::StartPending => 2,
            State::StopPending => 3,
            State::Running => 4,
        }
    }
}

/// SERVICE_ACCEPT_STOP | SHUTDOWN | POWEREVENT | SESSIONCHANGE | PRESHUTDOWN
pub const ACCEPTS_RUNNING: u32 = 0x1 | 0x4 | 0x40 | 0x80 | 0x100;

/// What the service reports (SERVICE_STATUS without the service type)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    /// Controls it accepts now (none while pending)
    pub accepts: u32,
    /// Win32 exit code once stopped (0: fine; ERROR_SERVICE_SPECIFIC_ERROR
    /// with [`Status::specific_exit_code`])
    pub exit_code: u32,
    /// The body's own exit code when it failed
    pub specific_exit_code: u32,
    /// Counts up while pending, 0 otherwise
    pub checkpoint: u32,
    /// How long the manager should wait for the next report (ms)
    pub wait_hint_ms: u32,
}

/// ERROR_SERVICE_SPECIFIC_ERROR: the exit code is the service's own
pub const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

/// How long the start may take between two reports
pub const START_WAIT: Duration = Duration::from_secs(20);
/// How long the stop may take between two reports
pub const STOP_WAIT: Duration = Duration::from_secs(10);

/// The status a service reports, step by step: start pending (checkpoints
/// counting up), running, stop pending (counting up again), stopped. A
/// report never goes back (no "running" after a stop began).
#[derive(Debug)]
pub struct StatusMachine {
    current: Status,
}

impl Default for StatusMachine {
    fn default() -> Self {
        StatusMachine::new()
    }
}

impl StatusMachine {
    /// Start pending, first checkpoint
    pub fn new() -> StatusMachine {
        StatusMachine {
            current: Status {
                state: State::StartPending,
                accepts: 0,
                exit_code: 0,
                specific_exit_code: 0,
                checkpoint: 1,
                wait_hint_ms: START_WAIT.as_millis() as u32,
            },
        }
    }

    pub fn current(&self) -> Status {
        self.current
    }

    /// Still working (start or stop): the next checkpoint. None while
    /// running or stopped (nothing to report).
    pub fn progress(&mut self) -> Option<Status> {
        match self.current.state {
            State::StartPending | State::StopPending => {
                self.current.checkpoint += 1;
                Some(self.current)
            }
            _ => None,
        }
    }

    /// Started. None if a stop began already.
    pub fn running(&mut self) -> Option<Status> {
        if self.current.state != State::StartPending {
            return None;
        }
        self.current = Status { state: State::Running, accepts: ACCEPTS_RUNNING, ..StatusMachine::idle() };
        Some(self.current)
    }

    /// A stop began (a control, or the body ends by itself). None if it
    /// began before.
    pub fn stopping(&mut self) -> Option<Status> {
        if matches!(self.current.state, State::StopPending | State::Stopped) {
            return None;
        }
        self.current = Status {
            state: State::StopPending,
            checkpoint: 1,
            wait_hint_ms: STOP_WAIT.as_millis() as u32,
            ..StatusMachine::idle()
        };
        Some(self.current)
    }

    /// Stopped with the body's `code` (0: fine; else reported as
    /// ERROR_SERVICE_SPECIFIC_ERROR with `code`). None if stopped before.
    pub fn stopped(&mut self, code: u32) -> Option<Status> {
        if self.current.state == State::Stopped {
            return None;
        }
        let exit_code = if code == 0 { 0 } else { ERROR_SERVICE_SPECIFIC_ERROR };
        self.current = Status { state: State::Stopped, exit_code, specific_exit_code: code, ..StatusMachine::idle() };
        Some(self.current)
    }

    fn idle() -> Status {
        Status {
            state: State::Stopped,
            accepts: 0,
            exit_code: 0,
            specific_exit_code: 0,
            checkpoint: 0,
            wait_hint_ms: 0,
        }
    }
}

/// Where status reports go: the manager, or a test's list
pub trait StatusSink: Send + Sync {
    fn report(&self, status: &Status);
}

// --- Controls ---------------------------------------------------------------

/// A control the manager sent
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Stop,
    /// The PC shuts down (SERVICE_CONTROL_PRESHUTDOWN or _SHUTDOWN)
    Shutdown,
    /// WTS_CONSOLE_CONNECT .. WTS_SESSION_TERMINATE (`event`) for `session`
    SessionChange {
        event: u32,
        session: u32,
    },
    /// PBT_APMSUSPEND, PBT_APMRESUMEAUTOMATIC, ... (`event`)
    Power {
        event: u32,
    },
}

const CONTROL_STOP: u32 = 1;
const CONTROL_INTERROGATE: u32 = 4;
const CONTROL_SHUTDOWN: u32 = 5;
const CONTROL_POWEREVENT: u32 = 13;
const CONTROL_SESSIONCHANGE: u32 = 14;
const CONTROL_PRESHUTDOWN: u32 = 15;

/// What the handler does with a control: hand it on, answer it (the
/// status), or refuse it. `session`: the session of a session change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded {
    Control(Control),
    /// SERVICE_CONTROL_INTERROGATE: report the status again
    Interrogate,
    NotImplemented,
}

pub fn decode_control(control: u32, event_type: u32, session: Option<u32>) -> Decoded {
    match control {
        CONTROL_STOP => Decoded::Control(Control::Stop),
        CONTROL_SHUTDOWN | CONTROL_PRESHUTDOWN => Decoded::Control(Control::Shutdown),
        CONTROL_INTERROGATE => Decoded::Interrogate,
        CONTROL_SESSIONCHANGE => match session {
            Some(session) => Decoded::Control(Control::SessionChange { event: event_type, session }),
            None => Decoded::NotImplemented,
        },
        CONTROL_POWEREVENT => Decoded::Control(Control::Power { event: event_type }),
        _ => Decoded::NotImplemented,
    }
}

// --- The dispatcher ---------------------------------------------------------

/// What the service's body gets: the controls, and the status to report
pub struct ServiceContext {
    controls: Mutex<mpsc::Receiver<Control>>,
    machine: Mutex<StatusMachine>,
    sink: Box<dyn StatusSink>,
}

impl std::fmt::Debug for ServiceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceContext").finish_non_exhaustive()
    }
}

impl ServiceContext {
    /// A context reporting to `sink` and receiving from `controls` (the
    /// dispatcher's; tests pass their own). Reports start pending at once.
    pub fn new(sink: Box<dyn StatusSink>, controls: mpsc::Receiver<Control>) -> ServiceContext {
        let machine = StatusMachine::new();
        sink.report(&machine.current());
        ServiceContext { controls: Mutex::new(controls), machine: Mutex::new(machine), sink }
    }

    fn apply(&self, f: impl FnOnce(&mut StatusMachine) -> Option<Status>) {
        let mut m = self.machine.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = f(&mut m) {
            self.sink.report(&s);
        }
    }

    /// The next control (blocks; None when the dispatcher is gone)
    pub fn next_control(&self) -> Option<Control> {
        self.controls.lock().unwrap_or_else(|e| e.into_inner()).recv().ok()
    }

    /// The next control, waiting up to `timeout`
    pub fn next_control_timeout(&self, timeout: Duration) -> Result<Control, mpsc::RecvTimeoutError> {
        self.controls.lock().unwrap_or_else(|e| e.into_inner()).recv_timeout(timeout)
    }

    pub fn progress(&self) {
        self.apply(StatusMachine::progress);
    }

    pub fn running(&self) {
        self.apply(StatusMachine::running);
    }

    pub fn stopping(&self) {
        self.apply(StatusMachine::stopping);
    }

    pub fn stopped(&self, exit_code: u32) {
        self.apply(|m| m.stopped(exit_code));
    }

    pub fn status(&self) -> Status {
        self.machine.lock().unwrap_or_else(|e| e.into_inner()).current()
    }

    /// Interrogate: the current status again
    fn report_again(&self) {
        let s = self.status();
        self.sink.report(&s);
    }
}

type Body = Box<dyn Fn(&ServiceContext) -> u32 + Send + Sync>;

struct Dispatch {
    name: Vec<u16>,
    body: Body,
}

static DISPATCH: OnceLock<Dispatch> = OnceLock::new();
static CONTROLS: Mutex<Option<mpsc::Sender<Control>>> = Mutex::new(None);
static CONTEXT: OnceLock<&'static ServiceContext> = OnceLock::new();

/// The dispatcher refused: this process was not started by the service
/// control manager (ERROR_FAILED_SERVICE_CONTROLLER_CONNECT)
pub const NOT_A_SERVICE: i32 = 1063;

/// Hands this process to the service control manager as the service
/// `name` and runs `body` as the service's main function; returns when the
/// service stopped. `body` gets the controls and reports through the
/// context; it should report running once started, and return its exit
/// code (0: fine) after it stopped. Fails with [`NOT_A_SERVICE`] when
/// the process was started otherwise (then nothing ran).
pub fn run_dispatcher(name: &str, body: impl Fn(&ServiceContext) -> u32 + Send + Sync + 'static) -> io::Result<()> {
    if DISPATCH.set(Dispatch { name: wide(name), body: Box::new(body) }).is_err() {
        return Err(io::Error::other("the dispatcher ran already"));
    }
    let Some(d) = DISPATCH.get() else { return Err(io::Error::other("no dispatcher")) };
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(d.name.as_ptr().cast_mut()), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: a table ending in a null entry; the name lives in a static.
    // Blocks until the service stopped; service_main runs on a thread of
    // the dispatcher's.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.map_err(io_err)
}

/// The manager's status handle as a sink
struct ScmSink(SERVICE_STATUS_HANDLE);

// SAFETY: the status handle may be used from any thread (SetServiceStatus
// is thread-safe for one service's handle).
unsafe impl Send for ScmSink {}
// SAFETY: as above.
unsafe impl Sync for ScmSink {}

impl StatusSink for ScmSink {
    fn report(&self, s: &Status) {
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: SERVICE_STATUS_CURRENT_STATE(s.state.code()),
            dwControlsAccepted: s.accepts,
            dwWin32ExitCode: s.exit_code,
            dwServiceSpecificExitCode: s.specific_exit_code,
            dwCheckPoint: s.checkpoint,
            dwWaitHint: s.wait_hint_ms,
        };
        // SAFETY: the handle RegisterServiceCtrlHandlerExW returned (never
        // closed); a valid status.
        if let Err(e) = unsafe { SetServiceStatus(self.0, &status) } {
            tracing::warn!("service: the status was not reported: {e}");
        }
    }
}

/// WTSSESSION_NOTIFICATION
#[repr(C)]
struct SessionNotification {
    size: u32,
    session: u32,
}

unsafe extern "system" fn control_handler(
    control: u32,
    event_type: u32,
    event_data: *mut c_void,
    _: *mut c_void,
) -> u32 {
    let session = if control == CONTROL_SESSIONCHANGE && !event_data.is_null() {
        // SAFETY: for SERVICE_CONTROL_SESSIONCHANGE the manager passes a
        // WTSSESSION_NOTIFICATION, valid for the call.
        let n = unsafe { &*event_data.cast::<SessionNotification>() };
        (n.size as usize >= size_of::<SessionNotification>()).then_some(n.session)
    } else {
        None
    };
    match decode_control(control, event_type, session) {
        Decoded::Control(c) => {
            if let Some(tx) = CONTROLS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                let _ = tx.send(c);
            }
            NO_ERROR.0
        }
        Decoded::Interrogate => {
            if let Some(ctx) = CONTEXT.get() {
                ctx.report_again();
            }
            NO_ERROR.0
        }
        Decoded::NotImplemented => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let Some(d) = DISPATCH.get() else { return };
    // SAFETY: the name lives in a static; the handler is a plain function
    // that only reads its arguments as documented.
    let handle = match unsafe { RegisterServiceCtrlHandlerExW(PCWSTR(d.name.as_ptr()), Some(control_handler), None) } {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("service: no control handler: {e}");
            return;
        }
    };
    let (tx, rx) = mpsc::channel();
    *CONTROLS.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    // One service per process, for the process's lifetime
    let ctx: &'static ServiceContext = Box::leak(Box::new(ServiceContext::new(Box::new(ScmSink(handle)), rx)));
    let _ = CONTEXT.set(ctx);
    let code = (d.body)(ctx);
    ctx.stopping();
    ctx.stopped(code);
}

// --- Install and remove -------------------------------------------------------

/// SERVICE_SID_TYPE_UNRESTRICTED: the service gets its own SID
/// (NT SERVICE\<name>), which ACLs can name, without a write-restricted
/// token
pub const SID_TYPE_UNRESTRICTED: u32 = 1;
/// SERVICE_SID_TYPE_RESTRICTED: write access also needs the service SID
pub const SID_TYPE_RESTRICTED: u32 = 3;

/// How the service is installed
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallConfig {
    pub name: String,
    /// In services.msc
    pub display_name: String,
    /// In services.msc, one sentence
    pub description: String,
    /// The program and its arguments, the program quoted
    pub command_line: String,
    /// Automatic start; delayed (after the other automatic services) or not
    pub delayed_start: bool,
    /// SERVICE_SID_TYPE_*
    pub sid_type: u32,
    /// The privileges the service keeps; every other one is removed from
    /// its token
    pub privileges: Vec<String>,
    /// Restart delays after the first, second and later failures
    pub restart_delays: Vec<Duration>,
    /// The failure count starts again after this long without one
    pub failure_reset: Duration,
    /// Also restart when the service stops with an error code (not only
    /// when the process dies)
    pub restart_on_error_exit: bool,
    /// How long the manager waits at shutdown (SERVICE_CONTROL_PRESHUTDOWN)
    pub preshutdown_timeout: Duration,
}

/// `items` as a REG_MULTI_SZ-style list: each NUL-terminated, one more NUL
pub fn multi_sz(items: &[String]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for i in items {
        out.extend(i.encode_utf16());
        out.push(0);
    }
    out.push(0);
    if items.is_empty() {
        out.push(0);
    }
    out
}

struct Sc(SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        // SAFETY: a handle this module opened, closed once.
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn change(service: &Sc, level: u32, info: *const c_void) -> io::Result<()> {
    // SAFETY: `info` points to the structure `level` names, alive for the
    // call (the callers keep it and its strings on their stack).
    unsafe { ChangeServiceConfig2W(service.0, windows::Win32::System::Services::SERVICE_CONFIG(level), Some(info)) }
        .map_err(io_err)
}

/// Creates the service as LocalSystem (the account: see the core's
/// service_host). Needs an administrator. If a setting cannot be applied,
/// the service is removed again.
pub fn install(c: &InstallConfig) -> io::Result<()> {
    // SAFETY: local manager, default database.
    let scm =
        Sc(unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE) }
            .map_err(io_err)?);
    let name = wide(&c.name);
    let display = wide(&c.display_name);
    let path = wide(&c.command_line);
    // SAFETY: NUL-terminated strings alive for the call; no account (None)
    // means LocalSystem; no dependencies or load order group.
    let service = Sc(unsafe {
        CreateServiceW(
            scm.0,
            PCWSTR(name.as_ptr()),
            PCWSTR(display.as_ptr()),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
    }
    .map_err(io_err)?);
    let configured = configure(&service, c);
    if configured.is_err() {
        // SAFETY: the service just created, opened with all access.
        let _ = unsafe { DeleteService(service.0) };
    }
    configured
}

fn configure(service: &Sc, c: &InstallConfig) -> io::Result<()> {
    let mut description = wide(&c.description);
    let d = SERVICE_DESCRIPTIONW { lpDescription: PWSTR(description.as_mut_ptr()) };
    change(service, SERVICE_CONFIG_DESCRIPTION.0, std::ptr::from_ref(&d).cast())?;

    let delayed = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: c.delayed_start.into() };
    change(service, SERVICE_CONFIG_DELAYED_AUTO_START_INFO.0, std::ptr::from_ref(&delayed).cast())?;

    let mut actions: Vec<SC_ACTION> =
        c.restart_delays.iter().map(|d| SC_ACTION { Type: SC_ACTION_RESTART, Delay: d.as_millis() as u32 }).collect();
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: c.failure_reset.as_secs() as u32,
        lpRebootMsg: PWSTR::null(),
        lpCommand: PWSTR::null(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    change(service, SERVICE_CONFIG_FAILURE_ACTIONS.0, std::ptr::from_ref(&failure).cast())?;
    let flag = SERVICE_FAILURE_ACTIONS_FLAG { fFailureActionsOnNonCrashFailures: c.restart_on_error_exit.into() };
    change(service, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG.0, std::ptr::from_ref(&flag).cast())?;

    let sid = SERVICE_SID_INFO { dwServiceSidType: c.sid_type };
    change(service, SERVICE_CONFIG_SERVICE_SID_INFO.0, std::ptr::from_ref(&sid).cast())?;

    let mut privileges = multi_sz(&c.privileges);
    let p = SERVICE_REQUIRED_PRIVILEGES_INFOW { pmszRequiredPrivileges: PWSTR(privileges.as_mut_ptr()) };
    change(service, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO.0, std::ptr::from_ref(&p).cast())?;

    let pre = SERVICE_PRESHUTDOWN_INFO { dwPreshutdownTimeout: c.preshutdown_timeout.as_millis() as u32 };
    change(service, SERVICE_CONFIG_PRESHUTDOWN_INFO.0, std::ptr::from_ref(&pre).cast())?;
    Ok(())
}

/// Stops the service `name` (waits up to `wait`) and removes it. Needs an
/// administrator. Ok(false): it was not installed.
pub fn uninstall(name: &str, wait: Duration) -> io::Result<bool> {
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
    // SAFETY: local manager, default database.
    let scm = Sc(unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }.map_err(io_err)?);
    let n = wide(name);
    // SAFETY: NUL-terminated name.
    let service = match unsafe {
        OpenServiceW(scm.0, PCWSTR(n.as_ptr()), SERVICE_STOP | SERVICE_QUERY_STATUS | 0x0001_0000 /* DELETE */)
    } {
        Ok(h) => Sc(h),
        Err(e) if (e.code().0 & 0xFFFF) == ERROR_SERVICE_DOES_NOT_EXIST => return Ok(false),
        Err(e) => return Err(io_err(e)),
    };
    let mut status = SERVICE_STATUS::default();
    // SAFETY: an open service handle with SERVICE_STOP; valid out pointer.
    match unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } {
        Ok(()) => {}
        Err(e) if (e.code().0 & 0xFFFF) == ERROR_SERVICE_NOT_ACTIVE => {}
        Err(e) => return Err(io_err(e)),
    }
    let deadline = std::time::Instant::now() + wait;
    loop {
        // SAFETY: an open handle with SERVICE_QUERY_STATUS; valid out pointer.
        unsafe { QueryServiceStatus(service.0, &mut status) }.map_err(io_err)?;
        if status.dwCurrentState == SERVICE_STOPPED || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // SAFETY: an open handle with DELETE. The service goes once every
    // handle to it is closed.
    unsafe { DeleteService(service.0) }.map_err(io_err)?;
    Ok(true)
}

// --- Control (the service set-up tool) ----------------------------------------

const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
const DELETE: u32 = 0x0001_0000;

/// Opens the service `name` with `access`; None when there is none
fn open_service(name: &str, access: u32) -> io::Result<Option<Sc>> {
    // SAFETY: local manager, default database.
    let scm = Sc(unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }.map_err(io_err)?);
    let n = wide(name);
    // SAFETY: NUL-terminated name. The manager's handle may close once the
    // service's is open.
    match unsafe { OpenServiceW(scm.0, PCWSTR(n.as_ptr()), access) } {
        Ok(h) => Ok(Some(Sc(h))),
        Err(e) if (e.code().0 & 0xFFFF) == ERROR_SERVICE_DOES_NOT_EXIST => Ok(None),
        Err(e) => Err(io_err(e)),
    }
}

/// Whether the service `name` exists (needs no administrator)
pub fn exists(name: &str) -> io::Result<bool> {
    Ok(open_service(name, SERVICE_QUERY_STATUS)?.is_some())
}

/// Starts the service `name` (already running: fine). Needs an
/// administrator. Does not wait for it to report running.
pub fn start(name: &str) -> io::Result<()> {
    use windows::Win32::System::Services::{SERVICE_START, StartServiceW};
    let service = open_service(name, SERVICE_START)?.ok_or_else(|| io::Error::from_raw_os_error(1060))?;
    // SAFETY: an open handle with SERVICE_START; no arguments.
    match unsafe { StartServiceW(service.0, None) } {
        Ok(()) => Ok(()),
        Err(e) if (e.code().0 & 0xFFFF) == ERROR_SERVICE_ALREADY_RUNNING => Ok(()),
        Err(e) => Err(io_err(e)),
    }
}

/// Stops the service `name` and waits up to `wait` until it reports
/// stopped. Ok(false): there is no such service. Needs an administrator.
pub fn stop(name: &str, wait: Duration) -> io::Result<bool> {
    let Some(service) = open_service(name, SERVICE_STOP | SERVICE_QUERY_STATUS)? else { return Ok(false) };
    let mut status = SERVICE_STATUS::default();
    // SAFETY: an open service handle with SERVICE_STOP; valid out pointer.
    match unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } {
        Ok(()) => {}
        Err(e) if (e.code().0 & 0xFFFF) == ERROR_SERVICE_NOT_ACTIVE => {}
        Err(e) => return Err(io_err(e)),
    }
    let deadline = std::time::Instant::now() + wait;
    loop {
        // SAFETY: an open handle with SERVICE_QUERY_STATUS; valid out pointer.
        unsafe { QueryServiceStatus(service.0, &mut status) }.map_err(io_err)?;
        if status.dwCurrentState == SERVICE_STOPPED {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the service did not stop"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Changes only the service's program and arguments (lpBinaryPathName).
/// Needs an administrator.
pub fn set_command_line(name: &str, command_line: &str) -> io::Result<()> {
    use windows::Win32::System::Services::{
        ChangeServiceConfigW, ENUM_SERVICE_TYPE, SERVICE_CHANGE_CONFIG, SERVICE_ERROR, SERVICE_NO_CHANGE,
        SERVICE_START_TYPE,
    };
    let service = open_service(name, SERVICE_CHANGE_CONFIG)?.ok_or_else(|| io::Error::from_raw_os_error(1060))?;
    let path = wide(command_line);
    // SAFETY: an open handle with SERVICE_CHANGE_CONFIG; SERVICE_NO_CHANGE
    // and null strings leave everything else as it is.
    unsafe {
        ChangeServiceConfigW(
            service.0,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_START_TYPE(SERVICE_NO_CHANGE),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
    }
    .map_err(io_err)
}

/// Marks the service `name` for deletion (it goes once every handle to it
/// is closed). Ok(false): there is no such service. Needs an
/// administrator.
pub fn delete(name: &str) -> io::Result<bool> {
    let Some(service) = open_service(name, DELETE)? else { return Ok(false) };
    // SAFETY: an open handle with DELETE.
    unsafe { DeleteService(service.0) }.map_err(io_err)?;
    Ok(true)
}

// --- Folders ----------------------------------------------------------------

/// %ProgramData% from the known-folder API (FOLDERID_ProgramData), never
/// from an environment variable a caller could set
pub fn program_data_dir() -> io::Result<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_ProgramData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: a known folder id; the returned string is freed below.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KF_FLAG_DEFAULT, None) }.map_err(io_err)?;
    // SAFETY: SHGetKnownFolderPath returns a NUL-terminated string.
    let s = unsafe { p.to_string() };
    // SAFETY: allocated by SHGetKnownFolderPath with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(p.0.cast())) };
    s.map(PathBuf::from).map_err(|_| io::Error::other("the folder name is not UTF-16"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn the_status_goes_forward_only() {
        let mut m = StatusMachine::new();
        let s = m.current();
        assert_eq!((s.state, s.checkpoint, s.accepts), (State::StartPending, 1, 0));
        assert_eq!(s.wait_hint_ms, 20_000);
        assert_eq!(m.progress().unwrap().checkpoint, 2);
        assert_eq!(m.progress().unwrap().checkpoint, 3);
        let r = m.running().unwrap();
        assert_eq!((r.state, r.checkpoint, r.wait_hint_ms, r.accepts), (State::Running, 0, 0, ACCEPTS_RUNNING));
        assert!(m.progress().is_none(), "nothing pending while running");
        assert!(m.running().is_none());
        let s = m.stopping().unwrap();
        assert_eq!((s.state, s.checkpoint, s.accepts, s.wait_hint_ms), (State::StopPending, 1, 0, 10_000));
        assert_eq!(m.progress().unwrap().checkpoint, 2);
        assert!(m.stopping().is_none());
        assert!(m.running().is_none(), "never running again after a stop began");
        let s = m.stopped(0).unwrap();
        assert_eq!((s.state, s.exit_code), (State::Stopped, 0));
        assert!(m.stopped(1).is_none());
        assert!(m.progress().is_none());
        // A start that fails: from start pending straight to stopping
        let mut m = StatusMachine::new();
        assert!(m.stopping().is_some());
        let s = m.stopped(3).unwrap();
        assert_eq!((s.exit_code, s.specific_exit_code), (ERROR_SERVICE_SPECIFIC_ERROR, 3));
    }

    #[test]
    fn controls_are_decoded() {
        assert_eq!(decode_control(1, 0, None), Decoded::Control(Control::Stop));
        assert_eq!(decode_control(5, 0, None), Decoded::Control(Control::Shutdown));
        assert_eq!(decode_control(15, 0, None), Decoded::Control(Control::Shutdown));
        assert_eq!(decode_control(4, 0, None), Decoded::Interrogate);
        assert_eq!(decode_control(14, 5, Some(3)), Decoded::Control(Control::SessionChange { event: 5, session: 3 }));
        assert_eq!(decode_control(14, 5, None), Decoded::NotImplemented);
        assert_eq!(decode_control(13, 4, None), Decoded::Control(Control::Power { event: 4 }));
        assert_eq!(decode_control(2, 0, None), Decoded::NotImplemented, "pause is not accepted");
        assert_eq!(decode_control(200, 0, None), Decoded::NotImplemented);
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Status>>);
    impl StatusSink for Arc<Recorder> {
        fn report(&self, s: &Status) {
            self.0.lock().unwrap().push(*s);
        }
    }

    /// A body run against a fake manager: start pending, running, a stop
    /// control, stop pending with checkpoints, stopped
    #[test]
    fn a_body_reports_through_its_context() {
        let rec = Arc::new(Recorder::default());
        let (tx, rx) = mpsc::channel();
        let ctx = ServiceContext::new(Box::new(rec.clone()), rx);
        ctx.progress();
        ctx.running();
        tx.send(Control::SessionChange { event: 5, session: 1 }).unwrap();
        tx.send(Control::Stop).unwrap();
        assert_eq!(ctx.next_control(), Some(Control::SessionChange { event: 5, session: 1 }));
        assert_eq!(ctx.next_control(), Some(Control::Stop));
        ctx.stopping();
        ctx.progress();
        ctx.report_again();
        ctx.stopped(0);
        ctx.stopped(0);
        drop(tx);
        assert_eq!(ctx.next_control(), None);
        let states: Vec<(State, u32)> = rec.0.lock().unwrap().iter().map(|s| (s.state, s.checkpoint)).collect();
        assert_eq!(
            states,
            [
                (State::StartPending, 1),
                (State::StartPending, 2),
                (State::Running, 0),
                (State::StopPending, 1),
                (State::StopPending, 2),
                (State::StopPending, 2),
                (State::Stopped, 0),
            ]
        );
    }

    #[test]
    fn multi_strings_end_in_two_nuls() {
        let v = multi_sz(&["SeTcbPrivilege".into(), "A".into()]);
        let text: String = String::from_utf16(&v).unwrap();
        assert_eq!(text, "SeTcbPrivilege\0A\0\0");
        assert_eq!(multi_sz(&[]), vec![0, 0]);
    }

    /// Outside the manager the dispatcher refuses at once and runs nothing
    #[test]
    fn the_dispatcher_refuses_an_ordinary_process() {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = ran.clone();
        let e = run_dispatcher("BlueCasterTestNotInstalled", move |_| {
            r.store(true, std::sync::atomic::Ordering::SeqCst);
            0
        })
        .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(NOT_A_SERVICE));
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// Read-only, and only a name no service has: the real service is
    /// never touched by a test
    #[test]
    fn a_service_that_does_not_exist_does_not_exist() {
        let name = format!("BlueCaster-test-none-{}", std::process::id());
        assert!(!exists(&name).unwrap());
    }

    #[test]
    fn program_data_is_a_known_folder() {
        let p = program_data_dir().unwrap();
        assert!(p.is_absolute() && p.is_dir(), "{}", p.display());
    }
}
