//! `bluecaster-core.exe --headless --service-data <dir>`: service mode with
//! a temp folder as the service's, a temp folder as the user's
//! (BLUECASTER_CONFIG_DIR) and a test instance of its own. The core's real
//! child agent reads the user's files, the core takes them into its folder
//! once; a second start takes nothing. Only processes this test starts are
//! ended; nothing outside the temp folders is written.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");
const WAIT: Duration = Duration::from_secs(40);

const KEY_LINE: &str = "00-A2-D0-00-00-02 00112233445566778899aabbccddeeff 4\n";

fn log_text(root: &Path) -> String {
    fn walk(dir: &Path, out: &mut String) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "log") {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    let mut text = String::new();
    walk(&root.join("logs"), &mut text);
    text
}

fn start(inst: &str, user: &Path, service: &Path) -> Child {
    Command::new(EXE)
        .args(["--headless", "--test-shell", "--service-data"])
        .arg(service)
        .env("BLUECASTER_INSTANCE", inst)
        .env("BLUECASTER_CONFIG_DIR", user)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the headless core starts")
}

/// Waits for one of `lines` in the service's log, then ends the core; its child
/// agent exits with the connection
fn run_until(inst: &str, user: &Path, service: &Path, lines: &[&str]) -> String {
    let mut core = start(inst, user, service);
    let deadline = Instant::now() + WAIT;
    loop {
        let log = log_text(service);
        if lines.iter().any(|l| log.contains(l)) {
            break;
        }
        if let Some(s) = core.try_wait().unwrap() {
            panic!("the core exited ({s}) before {lines:?}:\n{log}");
        }
        assert!(Instant::now() < deadline, "timed out waiting for {lines:?}:\n{log}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = log_text(service);
    let agent_pid: Option<u32> = log
        .split("session agent: started (process ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|n| n.parse().ok());
    let _ = core.kill();
    let _ = core.wait();
    if let Some(pid) = agent_pid {
        while win_sys::process::ProcessWatch::open(pid).is_some_and(|w| !w.exited()) {
            assert!(Instant::now() < deadline, "the child agent outlived its core");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    log
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bluecaster-svc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "--test-shell exists in debug builds only")]
fn the_users_data_moves_into_the_service_folder_once() {
    let inst = format!("svcdata-{}", std::process::id());
    let user = temp("user");
    let service = temp("service").join("BlueCaster");
    std::fs::create_dir_all(user.join("devices")).unwrap();
    // The user's folder as the installed app leaves it
    std::fs::write(
        user.join("settings.json"),
        r#"{"check_updates_on_startup": false, "language": "de", "debug_mode": true, "hid_host": true}"#,
    )
    .unwrap();
    std::fs::write(user.join("devices").join("devices.txt"), "00:A2:D0:00:00:02|Headphones\n").unwrap();
    std::fs::write(user.join("devices").join("hid_devices.txt"), "00:A2:D0:00:00:09|Keyboard\n").unwrap();
    // Encrypted for this user, as the app writes it
    win_sys::dpapi::write_secret(&user.join("pairing").join("link_keys.txt"), KEY_LINE.as_bytes()).unwrap();
    let user_keys_before = std::fs::read(user.join("pairing").join("link_keys.txt")).unwrap();

    let log = run_until(&inst, &user, &service, &["files taken from the user's folder", "migration: not done"]);
    assert!(log.contains("migration: 3 files taken from the user's folder"), "{log}");

    // The service's folder: for SYSTEM, the Administrators and its account
    let sddl = win_sys::acl::dacl_sddl(&service).unwrap();
    assert!(sddl.starts_with("D:P") && !sddl.contains(";;;BU)") && !sddl.contains(";;;WD)"), "{sddl}");
    // The settings, with the keyboard and mouse feature off
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(service.join("settings.json")).unwrap()).unwrap();
    // On a failure: what the core did and what the two folders hold
    let told = || {
        format!(
            "service settings: {s}
user settings: {}
core log:
{}",
            std::fs::read_to_string(user.join("settings.json")).unwrap_or_default(),
            log_text(&service)
        )
    };
    assert_eq!(s["language"], "de", "{}", told());
    assert_eq!(s["hid_host"], false, "{}", told());
    assert_eq!(std::fs::read_to_string(service.join("devices").join("devices.txt")).unwrap().lines().count(), 1);
    // The keyboard list never moves by itself
    assert!(!service.join("devices").join("hid_devices.txt").exists());
    // The keys: encrypted for the service's scope, not the user's
    let on_disk = std::fs::read(service.join("pairing").join("link_keys.txt")).unwrap();
    let blob = on_disk.strip_prefix(win_sys::dpapi::MAGIC.as_slice()).expect("encrypted");
    assert!(win_sys::dpapi::unprotect_in(win_sys::dpapi::Scope::User, blob).is_err());
    let keys = win_sys::dpapi::unprotect_in(win_sys::dpapi::Scope::Service, blob).unwrap();
    assert!(String::from_utf8(keys).unwrap().contains("00112233445566778899aabbccddeeff"));
    assert!(service.join("migration.json").is_file());
    // The user's files are left as they were
    assert_eq!(std::fs::read(user.join("pairing").join("link_keys.txt")).unwrap(), user_keys_before);
    assert!(user.join("devices").join("hid_devices.txt").is_file());
    // No key and no address in the log
    assert!(!log.contains("00112233445566778899aabbccddeeff") && !log.contains("00:A2:D0:00:00:02"), "{log}");
    assert!(!log.contains("00-A2-D0-00-00-02"), "{log}");
    // The core's log is in the service's folder, not the user's
    assert!(!user.join("logs").exists());

    // Again, with newer user data: nothing moves twice
    std::fs::write(user.join("devices").join("devices.txt"), "00:A2:D0:00:00:03|Other\n").unwrap();
    let devices_before = std::fs::read(service.join("devices").join("devices.txt")).unwrap();
    let log = run_until(&inst, &user, &service, &["migration: done before", "files taken from the user's folder, 0"]);
    assert_eq!(log.matches("files taken from the user's folder").count(), 1, "{log}");
    assert_eq!(std::fs::read(service.join("devices").join("devices.txt")).unwrap(), devices_before);

    let _ = std::fs::remove_dir_all(&user);
    let _ = std::fs::remove_dir_all(service.parent().unwrap());
}
