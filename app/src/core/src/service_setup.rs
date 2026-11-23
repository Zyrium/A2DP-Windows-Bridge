//! `bluecaster-core.exe --service-setup install|update|remove
//! [--remove-data]`: the service add-on's set-up tool. The app starts it
//! elevated (one administrator prompt) from the per-user installation
//! folder. It replaces stage 4's `--install-service` / `--uninstall-service`,
//! which now run `install` and `remove` the same way.
//!
//! - **install**: copies what the service side needs ([`SERVICE_PROGRAMS`],
//!   [`SERVICE_DIRS`]) from this program's folder into
//!   `<Program Files>\BlueCaster Service\` (created with an explicit,
//!   protected DACL: SYSTEM and the Administrators full, Users read and
//!   execute), registers the service with the program in that folder and
//!   starts it. A service that exists already (an earlier install, stage
//!   4's in the user's folder) is stopped, gets the new files and is
//!   pointed at that folder.
//! - **update**: stops the service (its agent and input helper end with
//!   it), replaces the files the same way, starts it again.
//! - **remove**: stops and deletes the service and deletes the folder;
//!   `--remove-data` also `%ProgramData%\BlueCaster`. Links inside are
//!   removed, never followed.
//!
//! Each copy goes to a temporary name in the target folder, is read back
//! and compared (size and SHA-256) with the source as read once, and only
//! then renamed into place; the file it replaces is kept until the service
//! started. If a step fails, what was done is undone: the files put back,
//! a service this run created deleted, a folder this run created removed.
//! The target folder and everything in it must not be a reparse point.
//!
//! # What the signature check protects against
//!
//! The tool runs elevated from a folder the user can write. Between the
//! administrator's approval and the copy, software running as the user
//! could replace the files beside it. So when this program is signed,
//! every program it copies must carry a valid Authenticode signature with
//! the same leaf certificate as this program ([`win_sys::authenticode`]),
//! checked through a handle that stays open with writes and deletion
//! denied, and the bytes copied are read from that same handle: the file
//! checked is the file copied. An unsigned tool is a developer build: it
//! copies without checking and says so.
//!
//! It does not protect against a replaced tool: the tool itself is what
//! the administrator approved, and a tool swapped before the prompt runs
//! whatever it likes, as with any installer started from a user's folder.
//! The UAC prompt shows the publisher of the signed tool (an unsigned one
//! shows "Unknown publisher"), which is the administrator's check. Revocation
//! is not checked. Licence files are data: compared, not signed.
//!
//! Output: one line per step, counts and states only (no paths). Exit
//! codes: see `EXIT_*`.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use win_sys::authenticode::{Policy, Signature};
use win_sys::service::InstallConfig;

/// The argument that runs the tool
pub const SETUP_ARG: &str = "--service-setup";
/// `remove`: also delete the service's data folder
pub const REMOVE_DATA_ARG: &str = "--remove-data";

/// Done
pub const EXIT_OK: i32 = 0;
/// Something else failed (the output says what); everything was undone
pub const EXIT_FAILED: i32 = 1;
/// Bad arguments
pub const EXIT_ARGS: i32 = 2;
/// Not run by an administrator (ERROR_ACCESS_DENIED's number)
pub const EXIT_NOT_ELEVATED: i32 = 5;
/// `update`: there is no service to update (ERROR_SERVICE_DOES_NOT_EXIST)
pub const EXIT_NOT_INSTALLED: i32 = 1060;
/// A file to copy is missing beside this program
pub const EXIT_FILES_MISSING: i32 = 20;
/// A program to copy is not signed by this program's publisher (or this
/// program's own signature is broken)
pub const EXIT_SOURCE_UNTRUSTED: i32 = 21;
/// The service folder, or something in it, is a link or not a folder
pub const EXIT_TARGET_UNSAFE: i32 = 22;
/// Writing or checking a copy failed
pub const EXIT_COPY_FAILED: i32 = 23;
/// The service could not be registered, stopped, started or deleted
pub const EXIT_SERVICE_FAILED: i32 = 24;
/// The tool runs from the service folder itself
pub const EXIT_WRONG_FOLDER: i32 = 25;

/// The programs the service side runs from its own folder: the core (the
/// service, the agent, the input helper, the elevated helper) and the
/// driver helper the agent starts from its folder. Nothing else is loaded
/// from the program's folder at run time: strings are built in, firmware,
/// the VB-CABLE pack and the logs live in the data folders.
pub const SERVICE_PROGRAMS: &[&str] = &[win_sys::service_files::CORE_EXE, win_sys::service_files::DRIVER_EXE];
/// Folders copied with everything in them: the licences that go with the
/// programs (data: compared, not signed)
pub const SERVICE_DIRS: &[&str] = &["licenses"];

/// How long a stop may take before the tool gives up
const STOP_WAIT: Duration = Duration::from_secs(30);
/// How long a file still in use (the agent or the helper ending) is tried
const IN_USE_WAIT: Duration = Duration::from_secs(10);

/// What to do
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    Update,
    Remove { data: bool },
}

/// The arguments after `--service-setup`
pub fn parse(args: &[String]) -> Result<Action, String> {
    let mut it = args.iter().map(String::as_str);
    let action = match it.next() {
        Some("install") => Action::Install,
        Some("update") => Action::Update,
        Some("remove") => Action::Remove { data: false },
        Some(other) => return Err(format!("unknown action {other}")),
        None => return Err("an action is needed".into()),
    };
    match (action, it.next(), it.next()) {
        (a, None, None) => Ok(a),
        (Action::Remove { .. }, Some(REMOVE_DATA_ARG), None) => Ok(Action::Remove { data: true }),
        (_, Some(other), _) => Err(format!("unexpected argument {other}")),
        (_, None, Some(_)) => Err("unexpected argument".into()),
    }
}

/// Why a run failed, and its exit code
#[derive(Debug)]
pub struct Failure {
    pub code: i32,
    /// For the output: no paths
    pub what: String,
}

fn fail(code: i32, what: impl Into<String>) -> Failure {
    Failure { code, what: what.into() }
}

fn io_kind(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => format!("{} (error {n})", e.kind()),
        None => e.kind().to_string(),
    }
}

// ---- Sources ---------------------------------------------------------------

/// One file to copy, read once from a handle that stays open with writes
/// and deletion denied
pub struct Source {
    /// The path below the folder ("bluecaster-core.exe", "licenses\x.txt")
    pub rel: PathBuf,
    /// A program (signature checked) or data
    pub program: bool,
    pub data: Vec<u8>,
    pub sha256: [u8; 32],
    /// Held until the copies are checked: nobody can change or replace
    /// the file meanwhile
    _file: File,
}

/// How a source's signature is read (the real one, or a test's)
pub trait Verifier {
    fn check(&self, file: &File, path: &Path) -> io::Result<Signature>;
}

/// WinVerifyTrust through the open handle
pub struct Authenticode;

impl Verifier for Authenticode {
    fn check(&self, file: &File, path: &Path) -> io::Result<Signature> {
        use std::os::windows::io::AsHandle;
        win_sys::authenticode::check_handle(file.as_handle(), path)
    }
}

/// Opens `path` for reading with only reading shared: no one can write,
/// rename or delete it while the handle is open
fn open_locked(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    // Opens the link itself if it is one: refused below
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn is_reparse(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}

/// The relative paths to copy from `dir`: [`SERVICE_PROGRAMS`] (each must
/// be there) and the files of [`SERVICE_DIRS`] (if there). Links are
/// refused.
pub fn source_list(dir: &Path) -> Result<Vec<(PathBuf, bool)>, Failure> {
    let mut out = Vec::new();
    for p in SERVICE_PROGRAMS {
        match std::fs::symlink_metadata(dir.join(p)) {
            Ok(m) if m.is_file() && !is_reparse(&m) => out.push((PathBuf::from(p), true)),
            Ok(_) => return Err(fail(EXIT_SOURCE_UNTRUSTED, "a program to copy is not a plain file")),
            Err(_) => return Err(fail(EXIT_FILES_MISSING, "a program to copy is missing")),
        }
    }
    for d in SERVICE_DIRS {
        let root = dir.join(d);
        if std::fs::symlink_metadata(&root).is_err() {
            continue;
        }
        let mut stack = vec![PathBuf::from(d)];
        while let Some(rel) = stack.pop() {
            let meta = std::fs::symlink_metadata(dir.join(&rel)).map_err(|e| fail(EXIT_FILES_MISSING, io_kind(&e)))?;
            if is_reparse(&meta) {
                return Err(fail(EXIT_SOURCE_UNTRUSTED, "a file to copy is a link"));
            }
            if meta.is_dir() {
                let entries = std::fs::read_dir(dir.join(&rel)).map_err(|e| fail(EXIT_FILES_MISSING, io_kind(&e)))?;
                for e in entries {
                    let e = e.map_err(|e| fail(EXIT_FILES_MISSING, io_kind(&e)))?;
                    stack.push(rel.join(e.file_name()));
                }
            } else {
                out.push((rel, false));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Opens, reads, hashes and (programs) checks every file of `list`
pub fn open_sources(
    dir: &Path,
    list: &[(PathBuf, bool)],
    policy: &Policy,
    verifier: &dyn Verifier,
) -> Result<Vec<Source>, Failure> {
    if matches!(policy, Policy::Refuse) {
        return Err(fail(EXIT_SOURCE_UNTRUSTED, "this program's own signature is not valid"));
    }
    let mut out = Vec::with_capacity(list.len());
    for (rel, program) in list {
        let path = dir.join(rel);
        let mut file = open_locked(&path).map_err(|e| fail(EXIT_FILES_MISSING, io_kind(&e)))?;
        let meta = file.metadata().map_err(|e| fail(EXIT_FILES_MISSING, io_kind(&e)))?;
        if is_reparse(&meta) || !meta.is_file() {
            return Err(fail(EXIT_SOURCE_UNTRUSTED, "a file to copy is a link"));
        }
        let mut data = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut data).map_err(|e| fail(EXIT_FILES_MISSING, io_kind(&e)))?;
        if *program && policy.checks() {
            file.rewind().map_err(|e| fail(EXIT_FILES_MISSING, io_kind(&e)))?;
            let sig = verifier.check(&file, &path).map_err(|e| fail(EXIT_SOURCE_UNTRUSTED, io_kind(&e)))?;
            policy.admits(&sig).map_err(|why| fail(EXIT_SOURCE_UNTRUSTED, why))?;
        }
        let sha256 = Sha256::digest(&data).into();
        out.push(Source { rel: rel.clone(), program: *program, data, sha256, _file: file });
    }
    Ok(out)
}

// ---- The target folder -------------------------------------------------------

/// Refuses a target that is a link, or holds one anywhere inside
pub fn check_target(target: &Path) -> Result<(), Failure> {
    let unsafe_target = || fail(EXIT_TARGET_UNSAFE, "the service folder or a file in it is a link");
    let meta = match std::fs::symlink_metadata(target) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(fail(EXIT_TARGET_UNSAFE, io_kind(&e))),
    };
    if is_reparse(&meta) || !meta.is_dir() {
        return Err(unsafe_target());
    }
    let mut stack = vec![target.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| fail(EXIT_TARGET_UNSAFE, io_kind(&e)))? {
            let e = e.map_err(|e| fail(EXIT_TARGET_UNSAFE, io_kind(&e)))?;
            let m = std::fs::symlink_metadata(e.path()).map_err(|e| fail(EXIT_TARGET_UNSAFE, io_kind(&e)))?;
            if is_reparse(&m) {
                return Err(unsafe_target());
            }
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    Ok(())
}

fn temp_name(dest: &Path, tag: &str) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    dest.with_file_name(format!(".{name}.{tag}-{}", std::process::id()))
}

/// Deletes what earlier runs left beside `dest` (a replaced program that
/// was still running, so not deleted then): `.<name>.old-*`, `.<name>.new-*`
fn remove_leftovers(dest: &Path) {
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else { return };
    let name = name.to_string_lossy();
    let (old, new) = (format!(".{name}.old-"), format!(".{name}.new-"));
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if (n.starts_with(&old) || n.starts_with(&new)) && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Runs `f` until it works or `wait` passed, while it fails with a
/// sharing violation or access denied (a program still ending)
fn retry_in_use<T>(wait: Duration, mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let until = Instant::now() + wait;
    loop {
        match f() {
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32)) && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(250))
            }
            r => return r,
        }
    }
}

/// The copies written next to their places, checked, not yet in place
pub struct Staged {
    /// (temporary copy, its place)
    files: Vec<(PathBuf, PathBuf)>,
    /// Folders this run created inside the target (removed on failure)
    made_dirs: Vec<PathBuf>,
}

impl Staged {
    /// Removes the temporary copies and the folders this run made
    pub fn discard(self) {
        for (tmp, _) in &self.files {
            let _ = std::fs::remove_file(tmp);
        }
        for d in self.made_dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
    }
}

/// Writes each source to a temporary name in `target`, reads it back and
/// compares size and SHA-256 with the source. `target` exists and was
/// checked ([`check_target`]).
pub fn stage(target: &Path, sources: &[Source]) -> Result<Staged, Failure> {
    let mut staged = Staged { files: Vec::new(), made_dirs: Vec::new() };
    let r = (|| {
        for s in sources {
            let dest = target.join(&s.rel);
            if let Some(parent) = dest.parent() {
                let mut missing = Vec::new();
                let mut p = parent;
                while p != target && std::fs::symlink_metadata(p).is_err() {
                    missing.push(p.to_path_buf());
                    p = match p.parent() {
                        Some(q) => q,
                        None => break,
                    };
                }
                for d in missing.into_iter().rev() {
                    std::fs::create_dir(&d).map_err(|e| fail(EXIT_COPY_FAILED, io_kind(&e)))?;
                    staged.made_dirs.push(d);
                }
                let m = std::fs::symlink_metadata(parent).map_err(|e| fail(EXIT_COPY_FAILED, io_kind(&e)))?;
                if is_reparse(&m) || !m.is_dir() {
                    return Err(fail(EXIT_TARGET_UNSAFE, "a folder in the service folder is a link"));
                }
            }
            remove_leftovers(&dest);
            let tmp = temp_name(&dest, "new");
            let _ = std::fs::remove_file(&tmp);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| fail(EXIT_COPY_FAILED, io_kind(&e)))?;
            staged.files.push((tmp.clone(), dest));
            f.write_all(&s.data).map_err(|e| fail(EXIT_COPY_FAILED, io_kind(&e)))?;
            f.sync_all().map_err(|e| fail(EXIT_COPY_FAILED, io_kind(&e)))?;
            drop(f);
            let back = std::fs::read(&tmp).map_err(|e| fail(EXIT_COPY_FAILED, io_kind(&e)))?;
            let hash: [u8; 32] = Sha256::digest(&back).into();
            if back.len() != s.data.len() || hash != s.sha256 {
                return Err(fail(EXIT_COPY_FAILED, "a copy differs from its source"));
            }
        }
        Ok(())
    })();
    match r {
        Ok(()) => Ok(staged),
        Err(f) => {
            staged.discard();
            Err(f)
        }
    }
}

/// The copies in place; the files they replaced kept until [`Committed::finish`]
pub struct Committed {
    /// (place, the replaced file's temporary name if there was one)
    files: Vec<(PathBuf, Option<PathBuf>)>,
    made_dirs: Vec<PathBuf>,
}

impl Committed {
    /// Deletes the replaced files
    pub fn finish(self) {
        for (_, old) in self.files {
            if let Some(old) = old {
                let _ = std::fs::remove_file(old);
            }
        }
    }

    /// Puts the replaced files back (and removes the new ones that
    /// replaced nothing)
    pub fn rollback(self, in_use: Duration) {
        for (dest, old) in self.files.into_iter().rev() {
            match old {
                Some(old) => {
                    let _ = retry_in_use(in_use, || std::fs::remove_file(&dest).or_else(ignore_missing));
                    let _ = retry_in_use(in_use, || std::fs::rename(&old, &dest));
                }
                None => {
                    let _ = retry_in_use(in_use, || std::fs::remove_file(&dest));
                }
            }
        }
        for d in self.made_dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
    }
}

fn ignore_missing(e: io::Error) -> io::Result<()> {
    if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
}

/// Moves the checked copies into place: each file it replaces is renamed
/// aside first. If a rename fails, everything done is put back.
pub fn commit(staged: Staged, in_use: Duration) -> Result<Committed, Failure> {
    let mut done = Committed { files: Vec::new(), made_dirs: staged.made_dirs };
    let mut rest = staged.files.into_iter();
    for (tmp, dest) in rest.by_ref() {
        let old = temp_name(&dest, "old");
        let had_old = match std::fs::symlink_metadata(&dest) {
            Ok(_) => {
                let _ = std::fs::remove_file(&old);
                if let Err(e) = retry_in_use(in_use, || std::fs::rename(&dest, &old)) {
                    let _ = std::fs::remove_file(&tmp);
                    let f = fail(EXIT_COPY_FAILED, io_kind(&e));
                    return Err(undo_commit(done, rest, f, in_use));
                }
                true
            }
            Err(_) => false,
        };
        if let Err(e) = retry_in_use(in_use, || std::fs::rename(&tmp, &dest)) {
            if had_old {
                let _ = std::fs::rename(&old, &dest);
            }
            let _ = std::fs::remove_file(&tmp);
            let f = fail(EXIT_COPY_FAILED, io_kind(&e));
            return Err(undo_commit(done, rest, f, in_use));
        }
        done.files.push((dest, had_old.then_some(old)));
    }
    Ok(done)
}

fn undo_commit(
    done: Committed,
    rest: impl Iterator<Item = (PathBuf, PathBuf)>,
    f: Failure,
    in_use: Duration,
) -> Failure {
    for (tmp, _) in rest {
        let _ = std::fs::remove_file(tmp);
    }
    done.rollback(in_use);
    f
}

/// Deletes `dir` and everything in it without following links: a link
/// inside is removed itself (std's remove_dir_all does not follow
/// symbolic links or junctions). A `dir` that is itself a link is
/// refused. Ok(false): there was nothing.
pub fn delete_tree(dir: &Path) -> Result<bool, Failure> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if is_reparse(&m) || !m.is_dir() => Err(fail(EXIT_TARGET_UNSAFE, "the folder is a link")),
        Ok(_) => retry_in_use(IN_USE_WAIT, || std::fs::remove_dir_all(dir))
            .map(|()| true)
            .map_err(|e| fail(EXIT_FAILED, io_kind(&e))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(fail(EXIT_FAILED, io_kind(&e))),
    }
}

// ---- The service control manager -----------------------------------------------

/// What the tool asks of the service control manager (the real one needs
/// an administrator; tests use a fake)
pub trait Scm {
    /// The configured command line, None when there is no service
    fn command_line(&self) -> io::Result<Option<String>>;
    fn create(&self, config: &InstallConfig) -> io::Result<()>;
    fn set_command_line(&self, command_line: &str) -> io::Result<()>;
    fn start(&self) -> io::Result<()>;
    /// Stops and waits; false: there is no service
    fn stop(&self) -> io::Result<bool>;
    /// false: there is no service
    fn delete(&self) -> io::Result<bool>;
}

/// The real manager, for the service [`ipc::access::SERVICE_NAME`]
pub struct RealScm;

impl Scm for RealScm {
    fn command_line(&self) -> io::Result<Option<String>> {
        use win_sys::service_query::{ServiceInfo, query};
        match query(ipc::access::SERVICE_NAME)? {
            ServiceInfo::NotInstalled => Ok(None),
            ServiceInfo::Denied => Err(io::Error::from_raw_os_error(5)),
            ServiceInfo::Found { command_line, .. } => Ok(Some(command_line)),
        }
    }
    fn create(&self, config: &InstallConfig) -> io::Result<()> {
        win_sys::service::install(config)
    }
    fn set_command_line(&self, command_line: &str) -> io::Result<()> {
        win_sys::service::set_command_line(ipc::access::SERVICE_NAME, command_line)
    }
    fn start(&self) -> io::Result<()> {
        win_sys::service::start(ipc::access::SERVICE_NAME)
    }
    fn stop(&self) -> io::Result<bool> {
        win_sys::service::stop(ipc::access::SERVICE_NAME, STOP_WAIT)
    }
    fn delete(&self) -> io::Result<bool> {
        win_sys::service::delete(ipc::access::SERVICE_NAME)
    }
}

/// How the folder gets its DACL (the real one needs an administrator;
/// tests make a plain folder)
pub trait Folder {
    /// Creates `dir` with the service folder's DACL, or sets it again
    fn protect(&self, dir: &Path) -> io::Result<()>;
}

pub struct ProtectedFolder;

impl Folder for ProtectedFolder {
    fn protect(&self, dir: &Path) -> io::Result<()> {
        win_sys::service_files::protect_service_folder(dir)
    }
}

// ---- The steps ---------------------------------------------------------------

/// Everything a run needs, real or fake
pub struct Setup<'a> {
    /// This program's folder (the per-user installation)
    pub source: PathBuf,
    /// The service folder
    pub target: PathBuf,
    /// `%ProgramData%\BlueCaster` (remove --remove-data)
    pub data: Option<PathBuf>,
    pub policy: Policy,
    pub verifier: &'a dyn Verifier,
    pub scm: &'a dyn Scm,
    pub folder: &'a dyn Folder,
    pub in_use: Duration,
    /// Output lines (counts and states only)
    pub say: &'a dyn Fn(&str),
}

impl Setup<'_> {
    fn core(&self) -> PathBuf {
        self.target.join(win_sys::service_files::CORE_EXE)
    }

    fn command_line(&self) -> String {
        crate::service_host::install_config(&self.core()).command_line
    }

    fn sources(&self) -> Result<Vec<Source>, Failure> {
        if win_sys::service_files::same_path(&self.source, &self.target) {
            return Err(fail(EXIT_WRONG_FOLDER, "run from the per-user installation, not the service folder"));
        }
        let list = source_list(&self.source)?;
        let sources = open_sources(&self.source, &list, &self.policy, self.verifier)?;
        let programs = sources.iter().filter(|s| s.program).count();
        let checked =
            if self.policy.checks() { "signatures checked" } else { "developer build: signatures not checked" };
        (self.say)(&format!("{programs} programs and {} other files read ({checked})", sources.len() - programs));
        Ok(sources)
    }

    /// Copies the files: the folder made or protected again, copies
    /// staged, checked and moved into place. Returns the commit and
    /// whether this run created the folder.
    fn copy(&self, sources: &[Source]) -> Result<(Committed, bool), Failure> {
        check_target(&self.target)?;
        let made = std::fs::symlink_metadata(&self.target).is_err();
        self.folder.protect(&self.target).map_err(|e| fail(EXIT_COPY_FAILED, io_kind(&e)))?;
        // Made a link meanwhile? Checked again.
        check_target(&self.target)?;
        let staged = match stage(&self.target, sources) {
            Ok(s) => s,
            Err(f) => {
                if made {
                    let _ = std::fs::remove_dir_all(&self.target);
                }
                return Err(f);
            }
        };
        match commit(staged, self.in_use) {
            Ok(c) => {
                (self.say)(&format!("{} files copied and checked", sources.len()));
                Ok((c, made))
            }
            Err(f) => {
                if made {
                    let _ = std::fs::remove_dir_all(&self.target);
                }
                Err(f)
            }
        }
    }

    fn undo_files(&self, committed: Committed, made: bool) {
        committed.rollback(self.in_use);
        if made {
            let _ = std::fs::remove_dir_all(&self.target);
        }
        (self.say)("the files were put back");
    }

    pub fn install(&self) -> Result<(), Failure> {
        let sources = self.sources()?;
        let existing = self.scm.command_line().map_err(|e| fail(EXIT_SERVICE_FAILED, io_kind(&e)))?;
        if existing.is_some() {
            self.scm.stop().map_err(|e| fail(EXIT_SERVICE_FAILED, io_kind(&e)))?;
            (self.say)("the service was stopped");
        }
        let (committed, made) = self.copy(&sources)?;
        let wanted = self.command_line();
        let registered = match &existing {
            None => self.scm.create(&crate::service_host::install_config(&self.core())).map(|()| true),
            Some(c) if c == &wanted => Ok(false),
            Some(_) => self.scm.set_command_line(&wanted).map(|()| false),
        };
        let created = match registered {
            Ok(c) => c,
            Err(e) => {
                self.undo_files(committed, made);
                // The old registration and files: leave it running again
                if existing.is_some() {
                    let _ = self.scm.start();
                }
                return Err(fail(EXIT_SERVICE_FAILED, io_kind(&e)));
            }
        };
        (self.say)(if created { "the service was registered" } else { "the service now runs from its folder" });
        if let Err(e) = self.scm.start() {
            if created {
                let _ = self.scm.delete();
                (self.say)("the service was deleted again");
            } else if let Some(old) = &existing
                && old != &wanted
            {
                let _ = self.scm.set_command_line(old);
            }
            self.undo_files(committed, made);
            if existing.is_some() {
                let _ = self.scm.start();
            }
            return Err(fail(EXIT_SERVICE_FAILED, io_kind(&e)));
        }
        committed.finish();
        (self.say)("the service was started");
        Ok(())
    }

    pub fn update(&self) -> Result<(), Failure> {
        let existing = self.scm.command_line().map_err(|e| fail(EXIT_SERVICE_FAILED, io_kind(&e)))?;
        let Some(old) = existing else { return Err(fail(EXIT_NOT_INSTALLED, "the service is not installed")) };
        let sources = self.sources()?;
        self.scm.stop().map_err(|e| fail(EXIT_SERVICE_FAILED, io_kind(&e)))?;
        (self.say)("the service was stopped");
        let (committed, made) = match self.copy(&sources) {
            Ok(c) => c,
            Err(f) => {
                let _ = self.scm.start();
                return Err(f);
            }
        };
        let wanted = self.command_line();
        if old != wanted
            && let Err(e) = self.scm.set_command_line(&wanted)
        {
            self.undo_files(committed, made);
            let _ = self.scm.start();
            return Err(fail(EXIT_SERVICE_FAILED, io_kind(&e)));
        }
        if let Err(e) = self.scm.start() {
            if old != wanted {
                let _ = self.scm.set_command_line(&old);
            }
            self.undo_files(committed, made);
            let _ = self.scm.start();
            return Err(fail(EXIT_SERVICE_FAILED, io_kind(&e)));
        }
        committed.finish();
        (self.say)("the service was started");
        Ok(())
    }

    pub fn remove(&self, data: bool) -> Result<(), Failure> {
        let stopped = self.scm.stop().map_err(|e| fail(EXIT_SERVICE_FAILED, io_kind(&e)))?;
        if stopped {
            (self.say)("the service was stopped");
            self.scm.delete().map_err(|e| fail(EXIT_SERVICE_FAILED, io_kind(&e)))?;
            (self.say)("the service was deleted");
        } else {
            (self.say)("the service is not installed");
        }
        if delete_tree(&self.target)? {
            (self.say)("the service folder was deleted");
        }
        if data
            && let Some(d) = &self.data
            && delete_tree(d)?
        {
            (self.say)("the service's data was deleted");
        }
        Ok(())
    }

    pub fn run(&self, action: Action) -> Result<(), Failure> {
        match action {
            Action::Install => self.install(),
            Action::Update => self.update(),
            Action::Remove { data } => self.remove(data),
        }
    }
}

/// `--service-setup ...` (and stage 4's `--install-service` /
/// `--uninstall-service`): the exit code
pub fn run(args: &[String]) -> i32 {
    win_sys::console::attach_parent();
    let action = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\nusage: {SETUP_ARG} install|update|remove [{REMOVE_DATA_ARG}]");
            return EXIT_ARGS;
        }
    };
    if !win_sys::elevation::is_elevated() {
        eprintln!(
            "{SETUP_ARG} requires administrator rights. Open the command prompt as an administrator and try again."
        );
        return EXIT_NOT_ELEVATED;
    }
    let say = |s: &str| println!("{s}");
    let source = match std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        Some(s) => s,
        None => {
            eprintln!("this program's folder is not known");
            return EXIT_FAILED;
        }
    };
    let target = match win_sys::service_files::service_folder() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Program Files is not known: {}", io_kind(&e));
            return EXIT_FAILED;
        }
    };
    let data = win_sys::service::program_data_dir().ok().map(|d| d.join("BlueCaster"));
    // Removing copies nothing: no signatures needed
    let policy = match action {
        Action::Remove { .. } => Policy::Unchecked,
        _ => match win_sys::authenticode::own_signature() {
            Ok(own) => Policy::from_own(&own),
            Err(e) => {
                eprintln!("this program's signature cannot be read: {}", io_kind(&e));
                return EXIT_SOURCE_UNTRUSTED;
            }
        },
    };
    let setup = Setup {
        source,
        target,
        data,
        policy,
        verifier: &Authenticode,
        scm: &RealScm,
        folder: &ProtectedFolder,
        in_use: IN_USE_WAIT,
        say: &say,
    };
    match setup.run(action) {
        Ok(()) => EXIT_OK,
        Err(f) => {
            eprintln!("{}", f.what);
            f.code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&a(&["install"])), Ok(Action::Install));
        assert_eq!(parse(&a(&["update"])), Ok(Action::Update));
        assert_eq!(parse(&a(&["remove"])), Ok(Action::Remove { data: false }));
        assert_eq!(parse(&a(&["remove", "--remove-data"])), Ok(Action::Remove { data: true }));
        assert!(parse(&a(&["install", "--remove-data"])).is_err());
        assert!(parse(&a(&["remove", "--remove-data", "x"])).is_err());
        assert!(parse(&a(&[])).is_err());
        assert!(parse(&a(&["repair"])).is_err());
    }

    /// The release package (installer/build-release.ps1) puts these
    /// programs beside each other, and the installer ([Files]) installs the
    /// package as it is. Every program but the UI must be copied to the
    /// service folder: a new program in the package fails this test until
    /// it is in [`SERVICE_PROGRAMS`] (or named here as UI-only).
    #[test]
    fn the_list_covers_the_packages_programs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let ps1 = std::fs::read_to_string(root.join("installer/build-release.ps1")).unwrap();
        let line = ps1
            .lines()
            .find(|l| l.trim_start().starts_with("foreach ($name in ") && l.contains("'bluecaster-core'"))
            .expect("the package's program list in build-release.ps1");
        let names: Vec<String> =
            line.split('\'').enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, n)| format!("{n}.exe")).collect();
        assert!(names.len() >= 3, "{names:?}");
        for n in &names {
            if n == win_sys::service_files::UI_EXE {
                assert!(!SERVICE_PROGRAMS.contains(&n.as_str()), "the UI stays in the user's folder");
            } else {
                assert!(SERVICE_PROGRAMS.contains(&n.as_str()), "{n} is in the package but not copied");
            }
        }
        // The licences go into the package as licenses\
        assert!(ps1.contains("$licDir = Join-Path $docDir 'licenses'"));
        // [Files] installs the package folder and nothing else beside it
        let iss = std::fs::read_to_string(root.join("installer/BlueCaster.iss")).unwrap();
        let files: Vec<&str> = iss
            .split("\n[Files]")
            .nth(1)
            .unwrap()
            .lines()
            .skip(1)
            .take_while(|l| !l.trim_start().starts_with('['))
            .map(str::trim)
            .filter(|l| l.starts_with("Source:"))
            .collect();
        for f in &files {
            let ok = f.starts_with(r#"Source: "{#PkgDir}\*"; DestDir: "{app}""#) || f.contains("dontcopy");
            assert!(ok, "a new [Files] line: does the service need it? {f}");
        }
        assert!(!files.is_empty());
    }

    struct Dirs {
        root: PathBuf,
    }

    impl Dirs {
        fn new(tag: &str) -> Dirs {
            let root = std::env::temp_dir().join(format!("bc-setup-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src/licenses/third-party")).unwrap();
            for (f, c) in [
                ("bluecaster.exe", "ui"),
                ("bluecaster-core.exe", "core v2"),
                ("bluecaster-driver.exe", "driver v2"),
                ("licenses/README.txt", "readme"),
                ("licenses/third-party/x.txt", "x"),
            ] {
                std::fs::write(root.join("src").join(f), c).unwrap();
            }
            Dirs { root }
        }
        fn src(&self) -> PathBuf {
            self.root.join("src")
        }
        fn dst(&self) -> PathBuf {
            self.root.join("BlueCaster Service")
        }
        fn data(&self) -> PathBuf {
            self.root.join("ProgramData-BlueCaster")
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    struct FakeVerifier(Signature);
    impl Verifier for FakeVerifier {
        fn check(&self, _: &File, _: &Path) -> io::Result<Signature> {
            Ok(self.0.clone())
        }
    }

    /// A per-file signature: the file's first bytes name its signer
    struct ByContent;
    impl Verifier for ByContent {
        fn check(&self, file: &File, _: &Path) -> io::Result<Signature> {
            let mut s = String::new();
            (&*file).read_to_string(&mut s)?;
            Ok(if s.starts_with("evil") { Signature::Valid(signer("EE")) } else { Signature::Valid(signer("AA")) })
        }
    }

    fn signer(t: &str) -> win_sys::authenticode::Signer {
        win_sys::authenticode::Signer { thumbprint: t.into(), subject: "Seiya Funaoka".into() }
    }

    #[derive(Default)]
    struct FakeScm {
        command_line: RefCell<Option<String>>,
        calls: RefCell<Vec<String>>,
        fail_start: bool,
        fail_create: bool,
    }

    impl Scm for FakeScm {
        fn command_line(&self) -> io::Result<Option<String>> {
            Ok(self.command_line.borrow().clone())
        }
        fn create(&self, c: &InstallConfig) -> io::Result<()> {
            self.calls.borrow_mut().push("create".into());
            if self.fail_create {
                return Err(io::Error::from_raw_os_error(1073));
            }
            *self.command_line.borrow_mut() = Some(c.command_line.clone());
            Ok(())
        }
        fn set_command_line(&self, c: &str) -> io::Result<()> {
            self.calls.borrow_mut().push(format!("set {c}"));
            *self.command_line.borrow_mut() = Some(c.into());
            Ok(())
        }
        fn start(&self) -> io::Result<()> {
            self.calls.borrow_mut().push("start".into());
            if self.fail_start { Err(io::Error::from_raw_os_error(1053)) } else { Ok(()) }
        }
        fn stop(&self) -> io::Result<bool> {
            self.calls.borrow_mut().push("stop".into());
            Ok(self.command_line.borrow().is_some())
        }
        fn delete(&self) -> io::Result<bool> {
            self.calls.borrow_mut().push("delete".into());
            Ok(self.command_line.borrow_mut().take().is_some())
        }
    }

    struct PlainFolder;
    impl Folder for PlainFolder {
        fn protect(&self, dir: &Path) -> io::Result<()> {
            std::fs::create_dir_all(dir)
        }
    }

    fn setup<'a>(d: &Dirs, scm: &'a FakeScm, v: &'a dyn Verifier, policy: Policy) -> Setup<'a> {
        Setup {
            source: d.src(),
            target: d.dst(),
            data: Some(d.data()),
            policy,
            verifier: v,
            scm,
            folder: &PlainFolder,
            in_use: Duration::ZERO,
            say: &|_| {},
        }
    }

    fn read(p: PathBuf) -> String {
        std::fs::read_to_string(p).unwrap_or_else(|_| "<none>".into())
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    out.push(name);
                }
                if e.path().is_dir() {
                    stack.push(e.path());
                }
            }
        }
        out
    }

    #[test]
    fn the_source_list_has_the_programs_and_the_licences() {
        let d = Dirs::new("list");
        let list = source_list(&d.src()).unwrap();
        let names: Vec<(String, bool)> = list.iter().map(|(p, b)| (p.display().to_string(), *b)).collect();
        assert_eq!(
            names,
            [
                ("bluecaster-core.exe".to_string(), true),
                ("bluecaster-driver.exe".to_string(), true),
                (r"licenses\README.txt".to_string(), false),
                (r"licenses\third-party\x.txt".to_string(), false),
            ]
        );
        // The UI is never copied
        assert!(!names.iter().any(|(n, _)| n == "bluecaster.exe"));
        std::fs::remove_file(d.src().join("bluecaster-driver.exe")).unwrap();
        assert_eq!(source_list(&d.src()).unwrap_err().code, EXIT_FILES_MISSING);
    }

    #[test]
    fn install_copies_checks_registers_and_starts() {
        let d = Dirs::new("install");
        let scm = FakeScm::default();
        let v = FakeVerifier(Signature::Unsigned);
        setup(&d, &scm, &v, Policy::Unchecked).install().unwrap();
        assert_eq!(read(d.dst().join("bluecaster-core.exe")), "core v2");
        assert_eq!(read(d.dst().join("bluecaster-driver.exe")), "driver v2");
        assert_eq!(read(d.dst().join(r"licenses\third-party\x.txt")), "x");
        assert!(!d.dst().join("bluecaster.exe").exists());
        assert_eq!(*scm.calls.borrow(), ["create", "start"]);
        let cmd = scm.command_line.borrow().clone().unwrap();
        assert_eq!(cmd, format!("\"{}\" --service", d.dst().join("bluecaster-core.exe").display()));
        assert!(leftovers(&d.dst()).is_empty(), "{:?}", leftovers(&d.dst()));
    }

    #[test]
    fn install_over_the_stage_4_service_points_it_at_the_folder() {
        let d = Dirs::new("repoint");
        let old = r#""C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe" --service"#.to_string();
        let scm = FakeScm { command_line: RefCell::new(Some(old)), ..Default::default() };
        let v = FakeVerifier(Signature::Unsigned);
        setup(&d, &scm, &v, Policy::Unchecked).install().unwrap();
        let calls = scm.calls.borrow();
        assert_eq!(calls[0], "stop");
        assert!(calls[1].starts_with("set \"") && calls[1].contains("BlueCaster Service"), "{calls:?}");
        assert_eq!(calls[2], "start");
    }

    #[test]
    fn a_failed_start_undoes_the_install() {
        let d = Dirs::new("install-fail");
        let scm = FakeScm { fail_start: true, ..Default::default() };
        let v = FakeVerifier(Signature::Unsigned);
        let e = setup(&d, &scm, &v, Policy::Unchecked).install().unwrap_err();
        assert_eq!(e.code, EXIT_SERVICE_FAILED);
        assert_eq!(*scm.calls.borrow(), ["create", "start", "delete"]);
        assert!(scm.command_line.borrow().is_none());
        assert!(!d.dst().exists(), "the folder this run made is gone");
    }

    #[test]
    fn a_failed_registration_undoes_the_copy() {
        let d = Dirs::new("create-fail");
        let scm = FakeScm { fail_create: true, ..Default::default() };
        let v = FakeVerifier(Signature::Unsigned);
        assert_eq!(setup(&d, &scm, &v, Policy::Unchecked).install().unwrap_err().code, EXIT_SERVICE_FAILED);
        assert!(!d.dst().exists());
    }

    #[test]
    fn update_replaces_the_files_and_a_failed_start_puts_them_back() {
        let d = Dirs::new("update");
        let v = FakeVerifier(Signature::Unsigned);
        let scm = FakeScm::default();
        setup(&d, &scm, &v, Policy::Unchecked).install().unwrap();
        // Version 3 beside the tool
        std::fs::write(d.src().join("bluecaster-core.exe"), "core v3").unwrap();
        let scm2 = FakeScm {
            command_line: RefCell::new(scm.command_line.borrow().clone()),
            fail_start: true,
            ..Default::default()
        };
        let e = setup(&d, &scm2, &v, Policy::Unchecked).update().unwrap_err();
        assert_eq!(e.code, EXIT_SERVICE_FAILED);
        assert_eq!(read(d.dst().join("bluecaster-core.exe")), "core v2", "the old files are back");
        assert_eq!(*scm2.calls.borrow(), ["stop", "start", "start"], "and the old service is tried again");
        assert!(leftovers(&d.dst()).is_empty(), "{:?}", leftovers(&d.dst()));
        // Now it starts
        let scm3 = FakeScm { command_line: RefCell::new(scm.command_line.borrow().clone()), ..Default::default() };
        setup(&d, &scm3, &v, Policy::Unchecked).update().unwrap();
        assert_eq!(read(d.dst().join("bluecaster-core.exe")), "core v3");
        assert_eq!(*scm3.calls.borrow(), ["stop", "start"]);
        assert!(leftovers(&d.dst()).is_empty());
    }

    #[test]
    fn update_needs_an_installed_service() {
        let d = Dirs::new("update-none");
        let scm = FakeScm::default();
        let v = FakeVerifier(Signature::Unsigned);
        assert_eq!(setup(&d, &scm, &v, Policy::Unchecked).update().unwrap_err().code, EXIT_NOT_INSTALLED);
        assert!(scm.calls.borrow().is_empty());
    }

    #[test]
    fn signatures_must_match_the_tools_own() {
        let d = Dirs::new("signed");
        let scm = FakeScm::default();
        let own = Policy::SameSigner(signer("AA"));
        // Same signer: copied
        setup(&d, &scm, &ByContent, own.clone()).install().unwrap();
        // A program replaced by one signed by someone else: refused before
        // anything changes
        std::fs::write(d.src().join("bluecaster-driver.exe"), "evil driver").unwrap();
        let scm2 = FakeScm { command_line: RefCell::new(scm.command_line.borrow().clone()), ..Default::default() };
        let e = setup(&d, &scm2, &ByContent, own.clone()).update().unwrap_err();
        assert_eq!(e.code, EXIT_SOURCE_UNTRUSTED);
        assert!(scm2.calls.borrow().is_empty(), "the service was not even stopped");
        assert_eq!(read(d.dst().join("bluecaster-driver.exe")), "driver v2");
        // Unsigned programs under a signed tool: refused
        let v = FakeVerifier(Signature::Unsigned);
        assert_eq!(setup(&d, &FakeScm::default(), &v, own.clone()).install().unwrap_err().code, EXIT_SOURCE_UNTRUSTED);
        let v = FakeVerifier(Signature::Invalid(-1));
        assert_eq!(setup(&d, &FakeScm::default(), &v, own).install().unwrap_err().code, EXIT_SOURCE_UNTRUSTED);
        // A tool whose own signature is broken trusts nothing
        let v = FakeVerifier(Signature::Valid(signer("AA")));
        assert_eq!(
            setup(&d, &FakeScm::default(), &v, Policy::Refuse).install().unwrap_err().code,
            EXIT_SOURCE_UNTRUSTED
        );
    }

    /// The file checked is the file copied: while the sources are open,
    /// nobody can write, rename or delete them
    #[test]
    fn sources_cannot_change_while_open() {
        let d = Dirs::new("locked");
        let list = source_list(&d.src()).unwrap();
        let sources = open_sources(&d.src(), &list, &Policy::Unchecked, &FakeVerifier(Signature::Unsigned)).unwrap();
        let core = d.src().join("bluecaster-core.exe");
        assert!(std::fs::OpenOptions::new().write(true).open(&core).is_err(), "write");
        assert!(std::fs::rename(&core, d.src().join("moved.exe")).is_err(), "rename");
        assert!(std::fs::remove_file(&core).is_err(), "delete");
        assert_eq!(sources[0].data, b"core v2");
        drop(sources);
        assert!(std::fs::OpenOptions::new().write(true).open(&core).is_ok());
    }

    #[test]
    fn a_copy_that_differs_is_caught_and_cleaned_up() {
        let d = Dirs::new("differs");
        std::fs::create_dir_all(d.dst()).unwrap();
        let list = source_list(&d.src()).unwrap();
        let mut sources =
            open_sources(&d.src(), &list, &Policy::Unchecked, &FakeVerifier(Signature::Unsigned)).unwrap();
        // What was read once no longer matches the hash: stands for a copy
        // that came out different
        sources[1].sha256 = [0; 32];
        let e = stage(&d.dst(), &sources).err().unwrap();
        assert_eq!(e.code, EXIT_COPY_FAILED);
        assert!(leftovers(&d.dst()).is_empty());
        assert!(!d.dst().join("licenses").exists(), "folders it made are gone");
    }

    #[test]
    fn a_failed_rename_puts_everything_back() {
        let d = Dirs::new("rename");
        let scm = FakeScm::default();
        let v = FakeVerifier(Signature::Unsigned);
        setup(&d, &scm, &v, Policy::Unchecked).install().unwrap();
        std::fs::write(d.src().join("bluecaster-core.exe"), "core v3").unwrap();
        std::fs::write(d.src().join("bluecaster-driver.exe"), "driver v3").unwrap();
        // The driver is in use (open without delete sharing): its rename fails
        let hold = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new().read(true).share_mode(1).open(d.dst().join("bluecaster-driver.exe")).unwrap()
        };
        let list = source_list(&d.src()).unwrap();
        let sources = open_sources(&d.src(), &list, &Policy::Unchecked, &v).unwrap();
        let staged = stage(&d.dst(), &sources).unwrap();
        assert_eq!(commit(staged, Duration::ZERO).err().unwrap().code, EXIT_COPY_FAILED);
        drop(hold);
        assert_eq!(read(d.dst().join("bluecaster-core.exe")), "core v2", "the core went back");
        assert_eq!(read(d.dst().join("bluecaster-driver.exe")), "driver v2");
        assert!(leftovers(&d.dst()).is_empty(), "{:?}", leftovers(&d.dst()));
    }

    /// A junction needs no administrator (mklink /J); a symbolic link
    /// would (or developer mode), so the junction stands for both
    fn junction(link: &Path, to: &Path) -> bool {
        use std::os::windows::process::CommandExt;
        // cmd.exe is a console program: started from a test process
        // without a console, it would open a terminal window on the
        // desktop (tests/service_setup.rs checks every such spawn)
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(to)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn links_in_or_as_the_target_are_refused() {
        let d = Dirs::new("reparse");
        let elsewhere = d.root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep.txt"), "keep").unwrap();
        // The target itself a junction
        assert!(junction(&d.dst(), &elsewhere), "mklink /J");
        let scm = FakeScm::default();
        let v = FakeVerifier(Signature::Unsigned);
        assert_eq!(setup(&d, &scm, &v, Policy::Unchecked).install().unwrap_err().code, EXIT_TARGET_UNSAFE);
        assert!(scm.calls.borrow().is_empty());
        assert_eq!(read(elsewhere.join("keep.txt")), "keep");
        assert!(leftovers(&elsewhere).is_empty());
        std::fs::remove_dir(d.dst()).unwrap();
        // A junction inside the target
        std::fs::create_dir_all(d.dst()).unwrap();
        assert!(junction(&d.dst().join("licenses"), &elsewhere));
        assert_eq!(check_target(&d.dst()).unwrap_err().code, EXIT_TARGET_UNSAFE);
        assert_eq!(setup(&d, &scm, &v, Policy::Unchecked).install().unwrap_err().code, EXIT_TARGET_UNSAFE);
        // Removing deletes the junction, not what it points to
        let removing = setup(&d, &scm, &v, Policy::Unchecked);
        removing.remove(false).unwrap();
        assert!(!d.dst().exists());
        assert_eq!(read(elsewhere.join("keep.txt")), "keep");
        // A data folder that is a junction is refused as a whole
        assert!(junction(&d.data(), &elsewhere));
        assert_eq!(setup(&d, &scm, &v, Policy::Unchecked).remove(true).unwrap_err().code, EXIT_TARGET_UNSAFE);
        assert_eq!(read(elsewhere.join("keep.txt")), "keep");
        std::fs::remove_dir(d.data()).unwrap();
        // A source program that is a junction (folder) is refused
        std::fs::remove_file(d.src().join("bluecaster-driver.exe")).unwrap();
        assert!(junction(&d.src().join("bluecaster-driver.exe"), &elsewhere));
        assert_eq!(source_list(&d.src()).unwrap_err().code, EXIT_SOURCE_UNTRUSTED);
    }

    #[test]
    fn remove_stops_deletes_and_keeps_the_data_unless_asked() {
        let d = Dirs::new("remove");
        let scm = FakeScm::default();
        let v = FakeVerifier(Signature::Unsigned);
        setup(&d, &scm, &v, Policy::Unchecked).install().unwrap();
        std::fs::create_dir_all(d.data().join("logs")).unwrap();
        std::fs::write(d.data().join("logs/x.log"), "x").unwrap();
        let s = setup(&d, &scm, &v, Policy::Unchecked);
        s.remove(false).unwrap();
        assert!(!d.dst().exists());
        assert!(d.data().join("logs/x.log").exists());
        assert!(scm.command_line.borrow().is_none());
        assert_eq!(scm.calls.borrow()[2..], ["stop", "delete"]);
        // Again, with the data: nothing installed is fine
        s.remove(true).unwrap();
        assert!(!d.data().exists());
    }

    #[test]
    fn leftovers_of_earlier_runs_are_cleaned_up() {
        let d = Dirs::new("leftovers");
        std::fs::create_dir_all(d.dst()).unwrap();
        std::fs::write(d.dst().join(".bluecaster-core.exe.old-1234"), "old").unwrap();
        std::fs::write(d.dst().join(".bluecaster-core.exe.new-99"), "half").unwrap();
        std::fs::write(d.dst().join("other.txt"), "kept").unwrap();
        let scm = FakeScm::default();
        let v = FakeVerifier(Signature::Unsigned);
        setup(&d, &scm, &v, Policy::Unchecked).install().unwrap();
        assert!(leftovers(&d.dst()).is_empty(), "{:?}", leftovers(&d.dst()));
        assert_eq!(read(d.dst().join("other.txt")), "kept");
    }

    #[test]
    fn the_tool_does_not_copy_onto_itself() {
        let d = Dirs::new("self");
        let scm = FakeScm::default();
        let v = FakeVerifier(Signature::Unsigned);
        let mut s = setup(&d, &scm, &v, Policy::Unchecked);
        s.target = d.src();
        assert_eq!(s.install().unwrap_err().code, EXIT_WRONG_FOLDER);
    }
}
