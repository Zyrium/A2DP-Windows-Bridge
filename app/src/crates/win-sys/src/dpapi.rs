//! Files only this Windows user can read: the pairing keys at rest,
//! encrypted with DPAPI (CryptProtectData, current-user scope). In service
//! mode the core encrypts for the service's own account instead
//! ([`Scope`], [`use_service_scope`]).
//!
//! An encrypted file is [`MAGIC`] followed by the DPAPI blob. A file without
//! it is an older plain-text one: [`read_secret`] returns its bytes and
//! rewrites it encrypted. A file that cannot be decrypted (a profile copied
//! to another user or PC, a reset password) is set aside as
//! `<name>.undecryptable` and read as missing, so the keys are paired anew
//! instead of the file being overwritten.
//!
//! Logs name the file only (no folder: it holds the user name), never its
//! content.

use std::io;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

/// Start of an encrypted file (none of the plain-text formats starts so)
pub const MAGIC: &[u8; 8] = b"BCDPAPI1";

/// Bound into every blob: another program's DPAPI blobs of this user do not
/// decrypt as ours, nor ours as theirs
const ENTROPY: &[u8] = b"BlueCaster pairing keys";
/// The same for the service's files: a blob of the user's folder copied
/// into the service's does not decrypt there, even where both run as one
/// account (tests), as it would not for a real service account
const SERVICE_ENTROPY: &[u8] = b"BlueCaster service pairing keys";

/// Whose files the secrets are: the one place that decides how they are
/// protected. Both scopes encrypt for the account the process runs as
/// (DPAPI's user scope) and never with CRYPTPROTECT_LOCAL_MACHINE, which
/// lets any process on the PC decrypt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The signed-in user's folder (the app, the session agent)
    User,
    /// The service's data folder (the core in service mode, for the
    /// service's own account)
    Service,
}

static SERVICE_SCOPE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This process keeps its secrets in the service's scope from now on (the
/// core in service mode, at start, before any secret is read)
pub fn use_service_scope() {
    SERVICE_SCOPE.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// The scope [`protect`], [`unprotect`] and the files use in this process
pub fn scope() -> Scope {
    if SERVICE_SCOPE.load(std::sync::atomic::Ordering::SeqCst) { Scope::Service } else { Scope::User }
}

fn entropy(scope: Scope) -> &'static [u8] {
    match scope {
        Scope::User => ENTROPY,
        Scope::Service => SERVICE_ENTROPY,
    }
}

/// CryptProtectData's flags of a scope: no UI, and never the machine-wide
/// flag
fn flags(_scope: Scope) -> u32 {
    CRYPTPROTECT_UI_FORBIDDEN
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr().cast_mut() }
}

/// Copies DPAPI's output and frees it
fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI returned cbData bytes at pbData, allocated with
    // LocalAlloc; they are copied before LocalFree.
    unsafe {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        LocalFree(Some(HLOCAL(out.pbData.cast())));
        v
    }
}

/// `data` encrypted for the account this process runs as, in this
/// process's [`scope`]
pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    protect_in(scope(), data)
}

/// `data` encrypted for the account this process runs as, in `scope`
pub fn protect_in(scope: Scope, data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(entropy(scope));
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input and entropy point to live slices for the call; out
    // receives a LocalAlloc'd buffer that take() frees.
    unsafe { CryptProtectData(&input, PCWSTR::null(), Some(&entropy), None, None, flags(scope), &mut out) }
        .map_err(io::Error::other)?;
    Ok(take(out))
}

/// The data of a [`protect`]ed blob of this process's [`scope`]; an error
/// when another user (or PC, or scope) encrypted it, or it is damaged
pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    unprotect_in(scope(), data)
}

/// The data of a blob [`protect_in`] `scope` made
pub fn unprotect_in(scope: Scope, data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data);
    let entropy = blob(entropy(scope));
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in protect_in()
    unsafe { CryptUnprotectData(&input, None, Some(&entropy), None, None, flags(scope), &mut out) }
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(take(out))
}

/// The content of the secret file `path` of the signed-in user's folder,
/// whatever this process's scope (the session agent reads it for the move
/// into the service's folder): Ok(None) when there is no file or it cannot
/// be decrypted (nothing is renamed or rewritten), a plain-text file as it
/// is
pub fn read_user_secret(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    match bytes.strip_prefix(MAGIC.as_slice()) {
        Some(blob) => Ok(unprotect_in(Scope::User, blob).ok()),
        None => Ok(Some(bytes)),
    }
}

/// Whether the file's bytes are an encrypted file
pub fn is_encrypted(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The content of the secret file `path`. A plain-text file is returned as
/// it is and rewritten encrypted. Errors: NotFound when there is no file
/// (also after one that cannot be decrypted was set aside), other I/O errors
/// as they come.
pub fn read_secret(path: &Path) -> io::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    let Some(blob) = bytes.strip_prefix(MAGIC.as_slice()) else {
        // Written by an older version: encrypt it now
        if bytes.is_empty() {
            return Ok(bytes);
        }
        match write_secret(path, &bytes) {
            Ok(()) => tracing::info!("{}: encrypted (was plain text)", name(path)),
            Err(e) => tracing::warn!("{}: still plain text, cannot encrypt it ({})", name(path), e.kind()),
        }
        return Ok(bytes);
    };
    match unprotect(blob) {
        Ok(data) => Ok(data),
        Err(e) => {
            let aside = with_suffix(path, ".undecryptable");
            let moved = std::fs::rename(path, &aside).is_ok();
            tracing::warn!(
                "{}: cannot be decrypted by this Windows user ({e}); its keys are not used{}",
                name(path),
                if moved { ", file kept as .undecryptable" } else { "" }
            );
            Err(io::Error::new(io::ErrorKind::NotFound, "secret file not decryptable"))
        }
    }
}

/// Writes `data` encrypted to `path` (folder made, temp file + rename)
pub fn write_secret(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut out = MAGIC.to_vec();
    out.extend(protect(data)?);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = with_suffix(path, ".tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bluecaster-dpapi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip() {
        let secret = b"00-A2-D0-00-00-02 00112233445566778899aabbccddeeff 4\n";
        let blob = protect(secret).unwrap();
        assert!(!blob.windows(secret.len()).any(|w| w == secret));
        assert_eq!(unprotect(&blob).unwrap(), secret);
        assert_eq!(unprotect(&protect(b"").unwrap()).unwrap(), b"");
    }

    #[test]
    fn the_scopes_do_not_read_each_others_blobs() {
        let secret = b"a link key";
        let user = protect_in(Scope::User, secret).unwrap();
        let service = protect_in(Scope::Service, secret).unwrap();
        assert_eq!(unprotect_in(Scope::User, &user).unwrap(), secret);
        assert_eq!(unprotect_in(Scope::Service, &service).unwrap(), secret);
        // A user's blob copied into the service's folder does not decrypt
        // there, nor the service's in the user's
        assert!(unprotect_in(Scope::Service, &user).is_err());
        assert!(unprotect_in(Scope::User, &service).is_err());
        // Never the machine-wide flag (any process on the PC could decrypt)
        for s in [Scope::User, Scope::Service] {
            assert_eq!(flags(s) & windows::Win32::Security::Cryptography::CRYPTPROTECT_LOCAL_MACHINE, 0);
        }
        // This test process never switched: the user's scope
        assert_eq!(scope(), Scope::User);
    }

    #[test]
    fn the_users_secrets_are_read_without_touching_the_file() {
        let d = dir("user");
        let file = d.join("keys.txt");
        assert_eq!(read_user_secret(&file).unwrap(), None);
        let mut enc = MAGIC.to_vec();
        enc.extend(protect_in(Scope::User, b"k").unwrap());
        std::fs::write(&file, &enc).unwrap();
        assert_eq!(read_user_secret(&file).unwrap().as_deref(), Some(&b"k"[..]));
        // Plain text as it is, not rewritten
        std::fs::write(&file, "plain").unwrap();
        assert_eq!(read_user_secret(&file).unwrap().as_deref(), Some(&b"plain"[..]));
        assert_eq!(std::fs::read(&file).unwrap(), b"plain");
        // Not decryptable: none, and the file stays where it is
        let mut foreign = MAGIC.to_vec();
        foreign.extend(protect_in(Scope::Service, b"k").unwrap());
        std::fs::write(&file, &foreign).unwrap();
        assert_eq!(read_user_secret(&file).unwrap(), None);
        assert_eq!(std::fs::read(&file).unwrap(), foreign);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn plain_files_are_migrated() {
        let d = dir("migrate");
        let file = d.join("keys.txt");
        std::fs::write(&file, "plain key\n").unwrap();
        assert_eq!(read_secret(&file).unwrap(), b"plain key\n");
        let on_disk = std::fs::read(&file).unwrap();
        assert!(is_encrypted(&on_disk));
        assert!(!String::from_utf8_lossy(&on_disk).contains("plain key"));
        assert_eq!(read_secret(&file).unwrap(), b"plain key\n");
        write_secret(&file, b"new").unwrap();
        assert_eq!(read_secret(&file).unwrap(), b"new");
        assert!(!d.join("keys.txt.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn undecryptable_files_are_set_aside() {
        let d = dir("foreign");
        let file = d.join("keys.txt");
        let mut foreign = MAGIC.to_vec();
        foreign.extend([1, 2, 3, 4]);
        std::fs::write(&file, &foreign).unwrap();
        assert_eq!(read_secret(&file).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!file.exists());
        assert_eq!(std::fs::read(d.join("keys.txt.undecryptable")).unwrap(), foreign);
        assert_eq!(read_secret(&file).unwrap_err().kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&d);
    }
}
