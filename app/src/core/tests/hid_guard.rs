//! The keyboards and mice guard (win_sys::hid_guard) run as the real
//! executable, with its test switch: it writes what it would release to a
//! file instead of sending it to Windows. Nothing here injects input.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use win_sys::hid_guard::{Guard, HeldSection, HeldSet};
use win_sys::input_inject::{Inject, MouseButton};

/// Console programs started from a test get no terminal window
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");

/// A process that waits about a minute: the "core" the guard watches
fn dummy_core() -> Child {
    Command::new("ping")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["-n", "60", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ping starts")
}

fn report_file(name: &str) -> PathBuf {
    let file = std::env::temp_dir().join(format!("bc-hid-guard-{}-{name}.txt", std::process::id()));
    let _ = std::fs::remove_file(&file);
    file
}

fn wait_exit(guard: &mut Guard) -> bool {
    let until = Instant::now() + Duration::from_secs(10);
    while guard.running() {
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "the guard's --report exists in debug builds only")]
fn releases_what_is_held_when_the_core_dies() {
    let section = HeldSection::create().unwrap();
    let mut held = HeldSet::default();
    held.apply(&[
        Inject::Key { scan: 0x2A, extended: false, up: false },
        Inject::Key { scan: 0x1E, extended: false, up: false },
        Inject::Key { scan: 0x1D, extended: true, up: false },
        Inject::Button { button: MouseButton::Left, up: false },
        Inject::Move { dx: 5, dy: 5 },
        // Pressed and let go: not held
        Inject::Key { scan: 0x30, extended: false, up: false },
        Inject::Key { scan: 0x30, extended: false, up: true },
    ]);
    section.store(&held);
    let file = report_file("held");
    let mut core = dummy_core();
    let mut guard = Guard::spawn(EXE.as_ref(), Some(&core), &section, Some(&file)).unwrap();
    // Still waiting while the core runs
    std::thread::sleep(Duration::from_millis(300));
    assert!(guard.running());
    assert!(!file.exists());
    // Held a moment later: the guard reads the section only once the core is gone
    held.apply(&[Inject::VirtualKey { vk: 0xB3, up: false }]);
    section.store(&held);
    core.kill().unwrap();
    let _ = core.wait();
    assert!(wait_exit(&mut guard), "the guard did not exit after the core died");
    let text = std::fs::read_to_string(&file).expect("the guard wrote its report");
    let _ = std::fs::remove_file(&file);
    // Keys, then modifiers, then buttons
    assert_eq!(text, "key 0x1e\nvk 0xb3\nkey 0x2a\nkey 0x1d extended\nbutton Left\n");
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "the guard's --report exists in debug builds only")]
fn nothing_held_nothing_released() {
    let section = HeldSection::create().unwrap();
    let file = report_file("empty");
    let mut core = dummy_core();
    let mut guard = Guard::spawn(EXE.as_ref(), Some(&core), &section, Some(&file)).unwrap();
    core.kill().unwrap();
    let _ = core.wait();
    assert!(wait_exit(&mut guard));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
    let _ = std::fs::remove_file(&file);
}

#[test]
#[cfg_attr(not(debug_assertions), ignore = "the guard's --report exists in debug builds only")]
fn stopped_guard_releases_nothing() {
    let section = HeldSection::create().unwrap();
    let mut held = HeldSet::default();
    held.apply(&[Inject::Key { scan: 0x1E, extended: false, up: false }]);
    section.store(&held);
    let file = report_file("stopped");
    let mut core = dummy_core();
    let guard = Guard::spawn(EXE.as_ref(), Some(&core), &section, Some(&file)).unwrap();
    // The feature turned off: the guard goes, the core lives on
    drop(guard);
    core.kill().unwrap();
    let _ = core.wait();
    std::thread::sleep(Duration::from_millis(300));
    assert!(!file.exists(), "a stopped guard released keys");
}
