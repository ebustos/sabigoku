//! Process-level exit-code contract (06 §7.4): the play path owns the only
//! deliberate nonzero exit; every other CLI path exits 0. Parse rules live in
//! cli.rs unit tests; here the real binary earns the table. No test spawns
//! the bare binary: that arm launches the TUI.

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
    for (args, word) in [
        (&["login"][..], "login"),
        (&["login", "--paste"][..], "login"),
        (&["--debug", "update"][..], "update"),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(stdout(&out).contains(word), "{args:?}");
    }
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
fn query_play_stub_exits_two() {
    let out = run(&["frieren"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("isn't supported yet"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn subcommand_after_a_query_word_routes_to_the_play_path() {
    // "login" demotes to search text, so the whole line is the play stub.
    let out = run(&["frieren", "login"]);
    assert_eq!(out.status.code(), Some(2));
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
