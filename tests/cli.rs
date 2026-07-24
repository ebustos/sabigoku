//! Process-level exit-code contract (06 §7.4): every non-play CLI path exits 0,
//! proven here by spawning the real binary. The play path's own exit table (0
//! on quit/no-results, 1 on failure) is unit-tested in main.rs::play_flow with
//! a fake provider, needing no network or mpv. Parse rules live in cli.rs unit
//! tests. No test spawns the bare binary: that arm launches the TUI.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .args(args)
        .output()
        .expect("spawn sabigoku")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn version_prints_a_clean_line_and_exits_zero() {
    for flags in [&["--version"][..], &["-V"][..], &["frieren", "-V"][..]] {
        let out = run(flags);
        assert_eq!(out.status.code(), Some(0), "{flags:?}");
        let text = stdout(&out);
        assert!(text.starts_with("sabigoku v"), "{text}");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(!text.contains("usage:"), "{text}");
    }
}

#[test]
fn unknown_flag_and_missing_quality_value_print_usage_and_exit_zero() {
    for flags in [&["--nope"][..], &["frieren", "--quality"][..]] {
        let out = run(flags);
        assert_eq!(out.status.code(), Some(0), "{flags:?}");
        assert!(stdout(&out).contains("usage: sabigoku"), "{flags:?}");
    }
}

#[test]
fn subcommand_stubs_exit_zero() {
    let out = run(&["--debug", "update"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("update"));
}

fn run_isolated(args: &[&str], dir: &std::path::Path, stdin: std::process::Stdio) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .args(args)
        .env("HOME", dir)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CACHE_HOME")
        .stdin(stdin)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn sabigoku")
}

// stdin is closed, so the paste prompt reads EOF: the abort branch, exit 0.
// No network is touched (the flow dies before any verify).
#[test]
fn login_paste_with_no_input_aborts_and_exits_zero() {
    let dir = std::env::temp_dir().join("sabigoku-cli-test-login-eof");
    let _ = std::fs::remove_dir_all(&dir);
    let out = run_isolated(&["login", "--paste"], &dir, std::process::Stdio::null());
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("redirect URL>"), "{text}");
    assert!(text.contains("no input"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

// A garbage paste short-circuits at token extract: no verify, no network.
#[test]
fn login_paste_with_garbage_reports_no_token_and_exits_zero() {
    use std::io::Write;
    let dir = std::env::temp_dir().join("sabigoku-cli-test-login-garbage");
    let _ = std::fs::remove_dir_all(&dir);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .args(["login", "--paste"])
        .env("HOME", &dir)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CACHE_HOME")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sabigoku");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"not-a-redirect-url\n")
        .unwrap();
    let out = child.wait_with_output().expect("wait sabigoku");
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("couldn't find an access_token"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

// Holding the registered port forces the bind-failure path: the fallback note
// then the paste flow (which aborts on EOF). If some other process already
// holds 8766, the child still can't bind it, so the assertion holds either way.
#[test]
fn login_with_the_port_taken_falls_back_to_paste() {
    let _hold = std::net::TcpListener::bind(("127.0.0.1", 8766));
    let dir = std::env::temp_dir().join("sabigoku-cli-test-login-bind");
    let _ = std::fs::remove_dir_all(&dir);
    let out = run_isolated(&["login"], &dir, std::process::Stdio::null());
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("falling back to paste"), "{text}");
    assert!(text.contains("no input"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sync_without_a_token_reports_not_connected_and_exits_zero() {
    let dir = std::env::temp_dir().join("sabigoku-cli-test-sync");
    let _ = std::fs::remove_dir_all(&dir);
    let out = Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .arg("sync")
        .env("HOME", &dir)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CACHE_HOME")
        .output()
        .expect("spawn sabigoku");
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("not connected"), "{text}");
    assert!(!text.contains("syncing with AniList"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn paths_flag_prints_locations_and_exits_zero() {
    let dir = std::env::temp_dir().join("sabigoku-cli-test-paths");
    let _ = std::fs::remove_dir_all(&dir);
    let out = Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .arg("--paths")
        .env("HOME", &dir)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CACHE_HOME")
        .output()
        .expect("spawn sabigoku");
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("sabigoku paths"), "{text}");
    assert!(text.contains("config"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
