//! The service's data folder: created with a protected DACL that lets in
//! SYSTEM, the Administrators and the account the process runs as (the
//! service's own) and nobody else. Ordinary users get no access, not even
//! read: the folder holds pairing keys and the keyboard and mouse list,
//! and its logs can name devices. A user reaches the logs through the
//! core (a diagnostic bundle), not through the folder.
//!
//! The DACL is inherited by everything inside, and applied again to a
//! folder that exists already (its files included), so a folder made by an
//! earlier build, or by hand, ends up the same.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::core::{BOOL, PCWSTR, PWSTR};

/// The folder's DACL as SDDL: protected (nothing inherited from above),
/// full access for SYSTEM, the Administrators and `own_sid`, inherited by
/// sub folders and files
pub fn service_dir_sddl(own_sid: &str) -> String {
    format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{own_sid})")
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// A security descriptor made from SDDL, freed on drop
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Descriptor> {
        let text: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: text is NUL-terminated; sd receives a LocalAlloc'ed
        // descriptor that Drop frees.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(text.as_ptr()), SDDL_REVISION_1, &mut sd, None)
        }
        .map_err(io::Error::other)?;
        Ok(Descriptor(sd))
    }

    /// Its DACL (inside the descriptor)
    fn dacl(&self) -> io::Result<*mut ACL> {
        let (mut present, mut defaulted) = (BOOL::default(), BOOL::default());
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: self.0 is a valid descriptor; the out pointers are live.
        unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            .map_err(io::Error::other)?;
        if !present.as_bool() || dacl.is_null() {
            return Err(io::Error::other("the descriptor has no DACL"));
        }
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: allocated with LocalAlloc by the SDDL conversion or
            // GetNamedSecurityInfoW, freed once.
            unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }
}

/// Creates `dir` (its parents as usual) with [`service_dir_sddl`] for the
/// account this process runs as, or applies that DACL to it, and to what it
/// holds, when it exists already
pub fn create_service_dir(dir: &Path) -> io::Result<()> {
    let sid = crate::pipe::current_user_sid()?;
    protect_dir(dir, &service_dir_sddl(&sid))
}

/// Creates `dir` with the DACL of `sddl`, or sets it on an existing one
/// (inherited by its contents)
pub fn protect_dir(dir: &Path, sddl: &str) -> io::Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let sd = Descriptor::from_sddl(sddl)?;
    let path = wide(dir.as_os_str());
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0.0,
        bInheritHandle: false.into(),
    };
    // Made with the DACL: no moment in which it has the parent's
    // SAFETY: path is NUL-terminated; attrs and its descriptor live for the
    // call.
    match unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&attrs)) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() && dir.is_dir() => {}
        Err(e) => return Err(io::Error::other(e)),
    }
    // An existing folder (and its contents) get it too
    let dacl = sd.dacl()?;
    // SAFETY: path is NUL-terminated; dacl points into sd, alive for the
    // call.
    let r = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl.cast_const()),
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(r.0 as i32));
    }
    Ok(())
}

/// The DACL of a file or folder as SDDL ("D:P(A;OICI;FA;;;SY)...")
pub fn dacl_sddl(path: &Path) -> io::Result<String> {
    let name = wide(path.as_os_str());
    let mut sd = Descriptor(PSECURITY_DESCRIPTOR::default());
    // SAFETY: name is NUL-terminated; sd.0 receives a LocalAlloc'ed
    // descriptor that Descriptor's Drop frees.
    let r = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            &mut sd.0,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(r.0 as i32));
    }
    let mut text = PWSTR::null();
    // SAFETY: sd.0 is a valid descriptor; text receives a LocalAlloc'ed
    // string freed below.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd.0,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        )
    }
    .map_err(io::Error::other)?;
    // SAFETY: a NUL-terminated string from the call above.
    let s = unsafe { text.to_string() }.map_err(io::Error::other);
    // SAFETY: allocated by the conversion with LocalAlloc.
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ACEs of an SDDL DACL: "(A;OICI;FA;;;SY)" -> ["A;OICI;FA;;;SY"]
    fn aces(sddl: &str) -> Vec<String> {
        sddl.split('(').skip(1).map(|a| a.trim_end_matches(')').to_string()).collect()
    }

    /// The trustee of an ACE (its last field)
    fn trustee(ace: &str) -> &str {
        ace.rsplit(';').next().unwrap_or_default()
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bluecaster-acl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_new_folder_is_for_system_administrators_and_its_account_only() {
        let base = temp("new");
        let dir = base.join("data");
        create_service_dir(&dir).unwrap();
        let sid = crate::pipe::current_user_sid().unwrap();
        let sddl = dacl_sddl(&dir).unwrap();
        assert!(sddl.starts_with("D:P"), "{sddl}");
        let list = aces(&sddl);
        assert_eq!(list.len(), 3, "{sddl}");
        let mut who: Vec<&str> = list.iter().map(|a| trustee(a)).collect();
        who.sort_unstable();
        // The built-in Administrator account (RID 500, the user of some
        // build machines) is written as "LA", not as its SID
        let own = if sid.ends_with("-500") { "LA" } else { sid.as_str() };
        let mut want = vec!["BA", "SY", own];
        want.sort_unstable();
        assert_eq!(who, want, "{sddl}");
        // Full access, inherited by folders and files, nothing denied
        assert!(list.iter().all(|a| a.starts_with("A;OICI;FA;")), "{sddl}");
        // Users, Everyone, Authenticated Users: nothing
        assert!(!list.iter().any(|a| ["BU", "WD", "AU", "IU"].contains(&trustee(a))), "{sddl}");
        // A file made inside inherits exactly that
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        let file = dacl_sddl(&dir.join("f.txt")).unwrap();
        assert_eq!(aces_of_kind(&file, "ID"), 3, "{file}");
        assert_eq!(aces(&file).len(), 3, "{file}");
        let _ = std::fs::remove_dir_all(&base);
    }

    fn aces_of_kind(sddl: &str, flag: &str) -> usize {
        aces(sddl).iter().filter(|a| a.split(';').nth(1).is_some_and(|f| f.contains(flag))).count()
    }

    #[test]
    fn an_existing_folder_and_its_files_get_the_dacl() {
        let dir = temp("existing");
        std::fs::create_dir_all(dir.join("pairing")).unwrap();
        std::fs::write(dir.join("pairing").join("link_keys.txt"), "x").unwrap();
        // Inherited from %TEMP% before: the user's own entries, maybe more
        create_service_dir(&dir).unwrap();
        let sddl = dacl_sddl(&dir).unwrap();
        assert!(sddl.starts_with("D:P") && aces(&sddl).len() == 3, "{sddl}");
        let file = dacl_sddl(&dir.join("pairing").join("link_keys.txt")).unwrap();
        let file_aces = aces(&file);
        assert_eq!(file_aces.len(), 3, "{file}");
        assert!(file_aces.iter().all(|a| a.contains("ID")), "{file}");
        // Again: the same, nothing added
        create_service_dir(&dir).unwrap();
        assert_eq!(dacl_sddl(&dir).unwrap(), sddl);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_in_the_way_is_an_error() {
        let base = temp("file");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("data"), "not a folder").unwrap();
        assert!(create_service_dir(&base.join("data")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
