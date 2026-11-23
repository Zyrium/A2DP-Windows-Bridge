//! Authenticode: whether a program file carries a valid signature that
//! Windows trusts, and who signed it (the leaf certificate's SHA-256
//! thumbprint and its simple name). The service set-up tool compares each
//! file it copies with its own signer; the session agent compares the UI
//! it starts with its own.
//!
//! The file is checked through a handle the caller opened (WinVerifyTrust
//! with `hFile`), so the caller can keep that handle open, with writes
//! denied, and read the very bytes that were checked. Revocation is not
//! checked (no network access from an elevated tool, and an offline PC
//! would fail); the chain must lead to a root Windows trusts.

use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::path::Path;

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Security::Cryptography::{
    CERT_CONTEXT, CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_SHA256_HASH_PROP_ID, CertGetCertificateContextProperty,
    CertGetNameStringW,
};
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
    WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};
use windows::core::PCWSTR;

/// Who signed a file: the leaf (signing) certificate
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signer {
    /// SHA-256 of the certificate, upper-case hex
    pub thumbprint: String,
    /// The certificate's simple display name (usually its CN)
    pub subject: String,
}

/// What WinVerifyTrust says of a file
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    /// Signed, the signature valid and trusted
    Valid(Signer),
    /// No signature at all (TRUST_E_NOSIGNATURE, TRUST_E_SUBJECT_FORM_UNKNOWN,
    /// TRUST_E_PROVIDER_UNKNOWN): an unsigned developer build
    Unsigned,
    /// A signature that is broken, untrusted, expired or not readable: the
    /// HRESULT
    Invalid(i32),
}

const TRUST_E_NOSIGNATURE: u32 = 0x800B_0100;
const TRUST_E_SUBJECT_FORM_UNKNOWN: u32 = 0x800B_0003;
const TRUST_E_PROVIDER_UNKNOWN: u32 = 0x800B_0001;

/// How a WinVerifyTrust result reads
pub fn classify(result: i32) -> Option<Signature> {
    match result as u32 {
        0 => None,
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => Some(Signature::Unsigned),
        _ => Some(Signature::Invalid(result)),
    }
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain([0]).collect()
}

/// The signature of the file open as `file` (read access); `path` is the
/// same file's name (WinVerifyTrust wants both; it reads through the
/// handle)
pub fn check_handle(file: BorrowedHandle<'_>, path: &Path) -> io::Result<Signature> {
    let name = wide(path.as_os_str());
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(name.as_ptr()),
        hFile: HANDLE(file.as_raw_handle()),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_NONE,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` and `info` (and the name it points to) live for the
    // call; no window (no UI). The state it opens is closed below.
    let result = unsafe { WinVerifyTrust(HWND(-1isize as *mut _), &mut action, (&raw mut data).cast()) };
    let outcome = match classify(result) {
        Some(s) => Ok(s),
        None => signer_of(&data).map(Signature::Valid),
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: the same structure, closing the state the verify opened
    unsafe { WinVerifyTrust(HWND(-1isize as *mut _), &mut action, (&raw mut data).cast()) };
    outcome
}

/// The signature of the file at `path` (opened for this check only, with
/// writes denied while it runs)
pub fn check_path(path: &Path) -> io::Result<Signature> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsHandle;
    const FILE_SHARE_READ: u32 = 1;
    let f = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path)?;
    check_handle(f.as_handle(), path)
}

/// The signature of this process's own program
pub fn own_signature() -> io::Result<Signature> {
    check_path(&std::env::current_exe()?)
}

/// The leaf certificate of a verified file's first signer
fn signer_of(data: &WINTRUST_DATA) -> io::Result<Signer> {
    let missing = || io::Error::other("the signature has no signer certificate");
    // SAFETY: the state WinVerifyTrust opened (not closed yet)
    let prov = unsafe { WTHelperProvDataFromStateData(data.hWVTStateData) };
    if prov.is_null() {
        return Err(missing());
    }
    // SAFETY: the provider data above; signer 0, no counter signer
    let sgnr = unsafe { WTHelperGetProvSignerFromChain(prov, 0, false, 0) };
    if sgnr.is_null() {
        return Err(missing());
    }
    // SAFETY: a signer structure owned by the state, alive until it closes
    let sgnr = unsafe { &*sgnr };
    if sgnr.csCertChain == 0 || sgnr.pasCertChain.is_null() {
        return Err(missing());
    }
    // SAFETY: the chain has csCertChain (> 0) entries; entry 0 is the leaf
    let cert = unsafe { (*sgnr.pasCertChain).pCert };
    if cert.is_null() {
        return Err(missing());
    }
    Ok(Signer { thumbprint: thumbprint(cert)?, subject: subject(cert) })
}

fn thumbprint(cert: *const CERT_CONTEXT) -> io::Result<String> {
    let mut hash = [0u8; 32];
    let mut len = hash.len() as u32;
    // SAFETY: a live certificate context; the buffer holds `len` bytes
    unsafe {
        CertGetCertificateContextProperty(cert, CERT_SHA256_HASH_PROP_ID, Some(hash.as_mut_ptr().cast()), &mut len)
    }
    .map_err(io::Error::other)?;
    Ok(hash[..(len as usize).min(32)].iter().map(|b| format!("{b:02X}")).collect())
}

fn subject(cert: *const CERT_CONTEXT) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: a live certificate context; the buffer and its length
    let n = unsafe { CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf)) } as usize;
    // n counts the terminator
    String::from_utf16_lossy(&buf[..n.saturating_sub(1).min(buf.len())])
}

/// What the set-up tool and the agent require of a file, given their own
/// program's signature
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Signed: every file must be signed, validly, by this same leaf
    /// certificate
    SameSigner(Signer),
    /// The running program is not signed: a developer build. Files are not
    /// checked (and the caller says so in its output).
    Unchecked,
    /// The running program's own signature is broken: nothing is trusted
    Refuse,
}

impl Policy {
    pub fn from_own(own: &Signature) -> Policy {
        match own {
            Signature::Valid(s) => Policy::SameSigner(s.clone()),
            Signature::Unsigned => Policy::Unchecked,
            Signature::Invalid(_) => Policy::Refuse,
        }
    }

    /// Whether a file with `signature` passes
    pub fn admits(&self, signature: &Signature) -> Result<(), &'static str> {
        match (self, signature) {
            (Policy::Unchecked, _) => Ok(()),
            (Policy::Refuse, _) => Err("this program's own signature is not valid"),
            (Policy::SameSigner(own), Signature::Valid(s)) if s.thumbprint.eq_ignore_ascii_case(&own.thumbprint) => {
                Ok(())
            }
            (Policy::SameSigner(_), Signature::Valid(_)) => Err("the file is signed by someone else"),
            (Policy::SameSigner(_), Signature::Unsigned) => Err("the file is not signed"),
            (Policy::SameSigner(_), Signature::Invalid(_)) => Err("the file's signature is not valid"),
        }
    }

    /// Whether files are checked at all
    pub fn checks(&self) -> bool {
        !matches!(self, Policy::Unchecked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer(t: &str) -> Signer {
        Signer { thumbprint: t.into(), subject: "Seiya Funaoka".into() }
    }

    #[test]
    fn results_are_classified() {
        assert_eq!(classify(0), None);
        assert_eq!(classify(TRUST_E_NOSIGNATURE as i32), Some(Signature::Unsigned));
        assert_eq!(classify(TRUST_E_SUBJECT_FORM_UNKNOWN as i32), Some(Signature::Unsigned));
        // TRUST_E_BAD_DIGEST: changed after signing
        assert_eq!(classify(0x8009_6010u32 as i32), Some(Signature::Invalid(0x8009_6010u32 as i32)));
        // CERT_E_UNTRUSTEDROOT
        assert!(matches!(classify(0x800B_0109u32 as i32), Some(Signature::Invalid(_))));
    }

    #[test]
    fn the_policy_follows_the_running_program() {
        let signed = Policy::from_own(&Signature::Valid(signer("AB")));
        assert_eq!(signed.admits(&Signature::Valid(signer("ab"))), Ok(()));
        assert!(signed.admits(&Signature::Valid(signer("CD"))).is_err());
        assert!(signed.admits(&Signature::Unsigned).is_err());
        assert!(signed.admits(&Signature::Invalid(1)).is_err());
        assert!(signed.checks());
        let dev = Policy::from_own(&Signature::Unsigned);
        assert_eq!(dev, Policy::Unchecked);
        assert_eq!(dev.admits(&Signature::Unsigned), Ok(()));
        assert!(!dev.checks());
        let broken = Policy::from_own(&Signature::Invalid(5));
        assert!(broken.admits(&Signature::Valid(signer("AB"))).is_err());
    }

    /// A plain file has no signature; Windows' own notepad is signed (by
    /// catalog on many builds, which a file check does not see: either
    /// answer is fine, but the call must work)
    #[test]
    fn real_files_can_be_checked() {
        let dir = std::env::temp_dir().join(format!("bc-sig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("plain.exe");
        std::fs::write(&f, b"MZ not a program").unwrap();
        assert!(matches!(check_path(&f).unwrap(), Signature::Unsigned | Signature::Invalid(_)));
        let _ = std::fs::remove_dir_all(&dir);
        // The test program itself: unsigned
        assert_eq!(own_signature().unwrap(), Signature::Unsigned);
    }
}
