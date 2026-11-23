//! `bluecaster-core.exe --service-setup`, the real program, only in the
//! ways that change nothing: bad arguments, and (when this test does not
//! run elevated) the refusal without administrator rights. An elevated
//! test run skips the second: it would install the service.

use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_bluecaster-core");

/// No window of its own (CREATE_NO_WINDOW), whatever the program's
/// subsystem
fn code(args: &[&str]) -> i32 {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new(EXE).args(args).creation_flags(CREATE_NO_WINDOW).output().unwrap().status.code().unwrap()
}

#[test]
fn bad_arguments_change_nothing() {
    assert_eq!(code(&["--service-setup"]), 2);
    assert_eq!(code(&["--service-setup", "repair"]), 2);
    assert_eq!(code(&["--service-setup", "install", "--remove-data"]), 2);
    assert_eq!(code(&["--install-service", "x"]), 2);
}

#[test]
fn without_administrator_rights_nothing_runs() {
    if win_sys::elevation::is_elevated() {
        eprintln!("skipped: this test runs elevated and would install the service");
        return;
    }
    for args in [
        &["--service-setup", "install"][..],
        &["--service-setup", "update"],
        &["--service-setup", "remove", "--remove-data"],
        &["--install-service"],
        &["--uninstall-service"],
    ] {
        assert_eq!(code(args), 5, "{args:?}");
    }
}

/// Every console program a test of this package starts (cmd, ping,
/// taskkill, tasklist) gets CREATE_NO_WINDOW: started from a test process
/// without a console, each would open a terminal window on the desktop
#[test]
fn console_programs_in_tests_open_no_window() {
    let dirs = [concat!(env!("CARGO_MANIFEST_DIR"), "/tests"), concat!(env!("CARGO_MANIFEST_DIR"), "/src")];
    let mut missing = Vec::new();
    for dir in dirs {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for prog in ["cmd", "ping", "taskkill", "tasklist"] {
                let needle = format!("Command::new(\"{prog}\")");
                for (at, _) in text.match_indices(&needle) {
                    // The statement the spawn belongs to
                    let rest = &text[at..];
                    let end = rest.find(';').unwrap_or(rest.len());
                    if !rest[..end].contains("CREATE_NO_WINDOW") {
                        missing.push(format!("{}: {prog}", path.display()));
                    }
                }
            }
        }
    }
    assert!(missing.is_empty(), "spawned without CREATE_NO_WINDOW: {missing:?}");
}
