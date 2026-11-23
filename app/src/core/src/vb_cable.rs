//! VB-CABLE installed when the user asks for it: VB-Audio's own driver pack,
//! fetched from VB-Audio's server at that moment (pinned by SHA-256, so a
//! pack VB-Audio replaced is refused rather than run), unpacked unmodified
//! into a folder of its own and its setup run silently as administrator.
//! BlueCaster itself ships none of it. The UI names VB-CABLE's origin and
//! that it is VB-Audio's donationware beside the Install button.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ipc::{ErrorInfo, Msg};
use session_host::elevation::ElevatedError;
use sha2::{Digest, Sha256};

const PACK_URL: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack46.zip";
const PACK_NAME: &str = "VBCABLE_Driver_Pack46.zip";
const PACK_SHA256: &str = "bc44e8974191a5a2427d6c979e2dab08dbb09925f3f435dfa2f0ba3e45b6eb90";
/// The pack is 1.3 MB
const MAX_PACK: u64 = 8 << 20;
const TIMEOUT: Duration = Duration::from_secs(60);
/// 64-bit Windows: x64, and ARM64 through emulation (the pack has the ARM64
/// driver)
const SETUP: &str = "VBCABLE_Setup_x64.exe";
/// How long the new outputs may take to show up after the setup ends
const APPEAR_WAIT: Duration = Duration::from_secs(10);

fn err(code: &str, key: &str) -> ErrorInfo {
    ErrorInfo { code: code.into(), message: Msg::new(key) }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// The pack: from the download cache when it is the pinned one there (a
/// second try needs no download), else from VB-Audio
fn pack() -> Result<Vec<u8>, ErrorInfo> {
    let cache = bluecaster_core::firmware::cache_dir().join(PACK_NAME);
    if let Ok(data) = std::fs::read(&cache)
        && sha256_hex(&data) == PACK_SHA256
    {
        tracing::debug!("VB-CABLE: pack from the cache");
        return Ok(data);
    }
    let failed = || err("vb_cable.download_failed", "error.vb_cable_download_failed");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("BlueCaster/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let started = Instant::now();
    let mut response = agent.get(PACK_URL).call().map_err(|e| {
        tracing::warn!("VB-CABLE: download failed: {e}");
        failed()
    })?;
    let data = response.body_mut().with_config().limit(MAX_PACK).read_to_vec().map_err(|e| {
        tracing::warn!("VB-CABLE: download failed: {e}");
        failed()
    })?;
    let hash = sha256_hex(&data);
    tracing::info!("VB-CABLE: pack downloaded, {} bytes in {} ms", data.len(), started.elapsed().as_millis());
    if hash != PACK_SHA256 {
        // VB-Audio changed the file: not run; the UI offers VB-Audio's page
        tracing::warn!("VB-CABLE: the pack is not the expected one (SHA-256 {hash})");
        return Err(err("vb_cable.pack_changed", "error.vb_cable_pack_changed"));
    }
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&cache, &data);
    Ok(data)
}

/// Unpacks the pack, as it is, into a new folder of its own; the setup's path
fn unpack(data: &[u8]) -> Result<PathBuf, ErrorInfo> {
    let failed = || err("vb_cable.install_failed", "error.vb_cable_install_failed");
    let files = crate::zip::read_all(data).ok_or_else(|| {
        tracing::warn!("VB-CABLE: the pack could not be read");
        failed()
    })?;
    let dir = std::env::temp_dir().join(format!("bluecaster-vbcable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| {
        tracing::warn!("VB-CABLE: folder not made: {e}");
        failed()
    })?;
    for (name, contents) in &files {
        // The pack is flat: a name with a path in it is not VB-Audio's
        if name.is_empty() || name.contains(['/', '\\', ':']) || name.starts_with('.') {
            tracing::warn!("VB-CABLE: unexpected entry in the pack");
            return Err(failed());
        }
        std::fs::write(dir.join(name), contents).map_err(|e| {
            tracing::warn!("VB-CABLE: file not written: {e}");
            failed()
        })?;
    }
    let setup = dir.join(SETUP);
    if !setup.is_file() {
        tracing::warn!("VB-CABLE: no {SETUP} in the pack");
        return Err(failed());
    }
    Ok(setup)
}

fn vb_cable_listed() -> bool {
    session_host::devices::render_devices()
        .is_ok_and(|list| list.iter().any(|d| bluecaster_core::profiles::is_vb_cable(&d.name, d.is_virtual)))
}

/// Fetches the pack, runs its setup ("-i -h": install, no window) as
/// administrator and waits for VB-CABLE's output to appear. Ok(false): it
/// did not appear (yet); VB-Audio's setup may want Windows restarted first.
pub fn install() -> Result<bool, ErrorInfo> {
    let setup = unpack(&pack()?)?;
    let result = run(&setup);
    if let Some(dir) = setup.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
    result?;
    let started = Instant::now();
    loop {
        if vb_cable_listed() {
            return Ok(true);
        }
        if started.elapsed() >= APPEAR_WAIT {
            tracing::info!("VB-CABLE: not among the outputs {APPEAR_WAIT:?} after its setup");
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The setup as administrator, through the session host (in the agent when
/// it does the session's work: a service shows no UAC prompt)
fn run(setup: &Path) -> Result<(), ErrorInfo> {
    let args = ["-i".to_string(), "-h".to_string()];
    let code = session_host::elevation::run_setup(setup, &args).map_err(|e| match e {
        ElevatedError::Cancelled => err("vb_cable.uac_cancelled", "error.vb_cable_uac_cancelled"),
        e => {
            tracing::error!("VB-CABLE setup: {e}");
            err("vb_cable.install_failed", "error.vb_cable_install_failed")
        }
    })?;
    tracing::info!("VB-CABLE setup -> exit {code}");
    Ok(())
}

/// VB-Audio's uninstall entries, as the BlueCaster uninstaller reads them
/// (installer/BlueCaster.iss: RemoveCables). VB-CABLE's setup registers
/// itself in the 64-bit view. The Hi-Fi Cable comes on its own (its key) or
/// with ASIO Bridge (32-bit view: the package's setup, which removes both;
/// the Hi-Fi Cable's own setup is in its folder, or in VB\CABLEHiFi)
const VB_CABLE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\VB:VBCABLE {87459874-1236-4469}";
const HIFI_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\VB:VBHIFI {87459874-1236-4469}";
const ASIO_BRIDGE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\VB:ASIOBridge {17359A74-1236-5467}";

/// The program of an UninstallString: the quoted path, else the string up
/// to ".exe"
fn program_of(command: &str) -> Option<PathBuf> {
    let s = command.trim();
    let path = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest).to_string(),
        None => match s.to_ascii_lowercase().find(".exe") {
            Some(i) => s[..i + 4].to_string(),
            None => s.to_string(),
        },
    };
    let path = PathBuf::from(path);
    path.is_file().then_some(path)
}

fn uninstall_entry(key: &str, view_32: bool) -> Option<PathBuf> {
    program_of(&win_sys::autostart::hklm_string(key, "UninstallString", view_32)?)
}

/// The Hi-Fi Cable's driver file is in place
fn hifi_driver_installed() -> bool {
    let dir = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| r"C:\Windows".into());
    std::fs::read_dir(dir.join(r"System32\drivers")).is_ok_and(|list| {
        list.flatten().any(|f| {
            let n = f.file_name().to_string_lossy().to_lowercase();
            n.starts_with("vbaudio_hfvaio") && n.ends_with(".sys")
        })
    })
}

fn hifi_remover() -> Option<PathBuf> {
    if !hifi_driver_installed() {
        return None;
    }
    // The package it came in: a full removal, as from Windows' installed
    // apps (issue #86)
    let bridge = uninstall_entry(ASIO_BRIDGE_KEY, true);
    if bridge.is_some() {
        return bridge;
    }
    if let Some(p) = uninstall_entry(HIFI_KEY, false) {
        return Some(p);
    }
    let pf = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| r"C:\Program Files".into());
    let copy = pf.join(r"VB\CABLEHiFi").join(SETUP);
    copy.is_file().then_some(copy)
}

fn listed(hifi: bool) -> bool {
    session_host::devices::render_devices().is_ok_and(|list| {
        list.iter().any(|d| {
            if hifi {
                bluecaster_core::profiles::is_hifi_cable(&d.name)
            } else {
                bluecaster_core::profiles::is_vb_cable(&d.name, d.is_virtual)
            }
        })
    })
}

/// Has VB-Audio's own setup remove VB-CABLE or the Hi-Fi Cable (`hifi`),
/// without a window, then Windows scan for hardware changes: both in the
/// elevated helper (bluecaster-driver.exe `remove-cable`), as the scan needs
/// administrator rights too, so the user sees one UAC prompt. Without the
/// scan the removed output stayed in Windows' list until a restart (issue
/// #86). Then waits for the output to go. Ok(false): it is still listed;
/// Windows needs a restart.
pub fn uninstall(config: &Path, hifi: bool) -> Result<bool, ErrorInfo> {
    let name = if hifi { "Hi-Fi Cable" } else { "VB-CABLE" };
    let failed = || err("vb_cable.uninstall_failed", "error.vb_cable_uninstall_failed");
    let setup = if hifi { hifi_remover() } else { uninstall_entry(VB_CABLE_KEY, false) };
    let Some(setup) = setup else {
        tracing::warn!("{name}: no setup found to remove it");
        return Err(failed());
    };
    let setup = setup.to_string_lossy().into_owned();
    let mut args = vec!["remove-cable", setup.as_str()];
    if hifi {
        args.push("--hifi");
    }
    let (code, result) = crate::driver::run_with(config, &args, |e| match e {
        ElevatedError::Cancelled => err("vb_cable.uac_cancelled", "error.vb_cable_uninstall_uac_cancelled"),
        e => {
            tracing::error!("{name} removal: {e}");
            failed()
        }
    })?;
    if code != 0 {
        tracing::warn!("{name} removal: helper exit {code}");
        return Err(failed());
    }
    wait_until_gone(name, &result, || listed(hifi), APPEAR_WAIT, Duration::from_millis(500))
}

/// After the helper: Ok(true) once `listed` says the output is gone, Ok(false)
/// if it still is after `wait`
fn wait_until_gone(
    name: &str,
    result: &serde_json::Value,
    mut listed: impl FnMut() -> bool,
    wait: Duration,
    every: Duration,
) -> Result<bool, ErrorInfo> {
    let rescanned = result.get("rescanned").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let started = Instant::now();
    loop {
        if !listed() {
            return Ok(true);
        }
        if started.elapsed() >= wait {
            tracing::info!("{name}: still among the outputs {wait:?} after its removal (rescanned {rescanned})");
            return Ok(false);
        }
        std::thread::sleep(every);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network: VB-Audio still serves the pinned pack, and it unpacks with
    /// its setup (the setup is not run)
    #[test]
    fn gone_once_no_longer_listed() {
        let result = serde_json::json!({ "ok": true, "rescanned": true });
        let mut calls = 0;
        let gone = wait_until_gone(
            "X",
            &result,
            || {
                calls += 1;
                calls < 3
            },
            Duration::from_secs(5),
            Duration::ZERO,
        );
        assert!(gone.unwrap());
        assert_eq!(calls, 3);
        // Still listed: a restart is needed
        let gone = wait_until_gone("X", &serde_json::Value::Null, || true, Duration::ZERO, Duration::ZERO);
        assert!(!gone.unwrap());
    }

    #[test]
    #[ignore]
    fn fetches_and_unpacks_the_pack() {
        let setup = unpack(&pack().unwrap()).unwrap();
        let dir = setup.parent().unwrap().to_path_buf();
        assert!(std::fs::read_dir(&dir).unwrap().count() > 20);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
