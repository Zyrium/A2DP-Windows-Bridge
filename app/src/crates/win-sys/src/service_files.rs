//! The service add-on's files: where they live (`<Program Files>\BlueCaster
//! Service`, from the known-folder API), the folder's protected DACL, the
//! version of a program there, and which program a trust check requires
//! ([`required_core`]). Signatures are in [`crate::authenticode`]; copying
//! is the set-up tool's (bluecaster-core.exe `--service-setup`).
//!
//! The per-user installation stays where it is; the service runs its own
//! copy of the core from this folder, which only an administrator can
//! change. The UI is not copied.

use std::io;
use std::path::{Path, PathBuf};

/// The service folder's name under Program Files
pub const SERVICE_FOLDER_NAME: &str = "BlueCaster Service";
/// The core's program (the service, the agent, the input helper, the
/// elevated helper and the set-up tool are all this program)
pub const CORE_EXE: &str = "bluecaster-core.exe";
/// The driver helper, which the agent starts elevated from its own folder
pub const DRIVER_EXE: &str = "bluecaster-driver.exe";
/// The UI, which stays in the user's own installation
pub const UI_EXE: &str = "bluecaster.exe";

/// The service folder's DACL as SDDL: protected (nothing inherited from
/// Program Files), full access for SYSTEM and the Administrators, read and
/// execute for Users, inherited by everything inside
pub const SERVICE_FOLDER_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

fn io_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// Program Files (the native one: FOLDERID_ProgramFiles in a 64-bit
/// process) from the known-folder API, never from an environment variable
pub fn program_files_dir() -> io::Result<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: a known folder id; the returned string is freed below.
    let p = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, None) }.map_err(io_err)?;
    // SAFETY: SHGetKnownFolderPath returns a NUL-terminated string.
    let s = unsafe { p.to_string() };
    // SAFETY: allocated by SHGetKnownFolderPath with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(p.0.cast())) };
    s.map(PathBuf::from).map_err(|_| io::Error::other("the folder name is not UTF-16"))
}

/// `<program_files>\BlueCaster Service`
pub fn service_folder_in(program_files: &Path) -> PathBuf {
    program_files.join(SERVICE_FOLDER_NAME)
}

/// The service folder on this PC
pub fn service_folder() -> io::Result<PathBuf> {
    program_files_dir().map(|p| service_folder_in(&p))
}

/// The service folder's core: the only program the real service may run
pub fn service_core() -> io::Result<PathBuf> {
    service_folder().map(|f| f.join(CORE_EXE))
}

/// The two paths name the same file (Windows compares names without case)
pub fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// The core program a trust check requires. For the real service
/// (`real_service`: no test instance, or a process in session 0) only the
/// service folder's core counts, whatever folder the checking program
/// runs from; otherwise (a test instance, a core run by hand) the core
/// beside the checking program (`beside`), as before the service folder.
pub fn required_core<'a>(
    beside: &'a Path,
    service_core: Option<&'a Path>,
    real_service: bool,
) -> Result<&'a Path, &'static str> {
    if real_service { service_core.ok_or("the service folder is not known") } else { Ok(beside) }
}

/// Whether `path` itself (not what it points to) is a reparse point: a
/// symbolic link, a junction, a mount point
pub fn is_reparse_point(path: &Path) -> io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    Ok(std::fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Creates the service folder with [`SERVICE_FOLDER_SDDL`], or sets that
/// DACL on it again (inherited by what it holds). Needs an administrator.
/// The caller checks first that the folder is not a reparse point.
pub fn protect_service_folder(dir: &Path) -> io::Result<()> {
    crate::acl::protect_dir(dir, SERVICE_FOLDER_SDDL)
}

/// A REG_SZ value under HKEY_CURRENT_USER, or None when the key or the
/// value is missing (the agent reads the per-user installation's place)
pub fn hkcu_string(subkey: &str, value_name: &str) -> Option<String> {
    crate::autostart::get_in(subkey, value_name).ok().flatten()
}

/// The ProductVersion string of a program ("3.0.3", "3.0.0-beta.4"), the
/// app's full version as the build wrote it, or None
pub fn product_version(path: &Path) -> Option<String> {
    let s = crate::process::version_string(path.to_str()?, "ProductVersion")?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_files_is_a_known_folder() {
        let p = program_files_dir().unwrap();
        assert!(p.is_absolute() && p.is_dir(), "{}", p.display());
    }

    #[test]
    fn paths_are_built_under_program_files() {
        let pf = Path::new(r"C:\Program Files");
        assert_eq!(service_folder_in(pf), Path::new(r"C:\Program Files\BlueCaster Service"));
        let core = service_folder_in(pf).join(CORE_EXE);
        assert_eq!(core, Path::new(r"C:\Program Files\BlueCaster Service\bluecaster-core.exe"));
        assert!(same_path(&core, Path::new(r"c:\program files\bluecaster service\BLUECASTER-CORE.EXE")));
        assert!(!same_path(&core, Path::new(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe")));
        // The real folder ends the same way
        assert!(service_core().unwrap().ends_with(r"BlueCaster Service\bluecaster-core.exe"));
    }

    #[test]
    fn the_real_service_requires_the_service_folder() {
        let beside = Path::new(r"C:\Users\u\AppData\Local\BlueCaster\bluecaster-core.exe");
        let svc = Path::new(r"C:\Program Files\BlueCaster Service\bluecaster-core.exe");
        assert_eq!(required_core(beside, Some(svc), true), Ok(svc));
        assert!(required_core(beside, None, true).is_err());
        assert_eq!(required_core(beside, Some(svc), false), Ok(beside));
        assert_eq!(required_core(beside, None, false), Ok(beside));
    }

    #[test]
    fn the_folder_dacl_is_protected_and_lets_users_read_only() {
        assert!(SERVICE_FOLDER_SDDL.starts_with("D:P"));
        assert!(SERVICE_FOLDER_SDDL.contains("(A;OICI;FA;;;SY)") && SERVICE_FOLDER_SDDL.contains("(A;OICI;FA;;;BA)"));
        // FILE_GENERIC_READ | FILE_GENERIC_EXECUTE: no write, no delete
        assert!(SERVICE_FOLDER_SDDL.contains("(A;OICI;0x1200a9;;;BU)"));
        assert_eq!(SERVICE_FOLDER_SDDL.matches("(A;").count(), 3);
    }

    /// protect_dir on a temp folder of this user, with the service
    /// folder's DACL plus this user (so that the test can delete it again)
    #[test]
    fn a_folder_gets_the_dacl() {
        let dir = std::env::temp_dir().join(format!("bc-svcdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let me = crate::pipe::current_user_sid().unwrap();
        let sddl = format!("{SERVICE_FOLDER_SDDL}(A;OICI;FA;;;{me})");
        crate::acl::protect_dir(&dir, &sddl).unwrap();
        let got = crate::acl::dacl_sddl(&dir).unwrap();
        assert!(got.starts_with("D:P"), "{got}");
        assert!(got.contains(";;;BU)"), "{got}");
        std::fs::write(dir.join("f"), b"x").unwrap();
        assert!(!is_reparse_point(&dir.join("f")).unwrap());
        assert!(!is_reparse_point(&dir).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_without_a_version_has_none() {
        let dir = std::env::temp_dir().join(format!("bc-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("plain.txt");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(product_version(&f), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
