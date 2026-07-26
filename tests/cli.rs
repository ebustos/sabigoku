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

/// One-shot local release endpoint. The `update` tests run the binary against
/// this via SABIGOKU_UPDATE_URL, which the binary only reads when built
/// `--features test-seams` (never a distributed build); these tests are gated
/// on that feature so a plain `cargo test` never reaches GitHub or the real
/// installer.
#[cfg(feature = "test-seams")]
fn serve_latest(tag: &str) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let body = format!("{{\"tag_name\":\"{tag}\"}}");
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    url
}

#[cfg(feature = "test-seams")]
#[test]
fn update_at_latest_reports_and_exits_zero() {
    let url = serve_latest(&format!("v{}", env!("CARGO_PKG_VERSION")));
    let out = Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .args(["--debug", "update"])
        .env("SABIGOKU_UPDATE_URL", &url)
        .output()
        .expect("spawn sabigoku");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).contains("is already the latest release"),
        "{}",
        stdout(&out)
    );
}

/// A failed freshness check must NOT self-update: the installer would curl|sh
/// an unpinned reinstall over an already-current binary on any rate-limited
/// API call (ROD-466 review). Dead port + standalone + writable bindir:
/// assert the installer sentinel is never created.
#[cfg(feature = "test-seams")]
#[test]
fn update_with_a_failed_check_does_not_self_update() {
    let dir = std::env::temp_dir().join("sabigoku-cli-update-nocheck");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sentinel = dir.join("installer-ran");
    let installer = dir.join("fake-installer.sh");
    std::fs::write(&installer, "#!/bin/sh\ntouch \"$SENTINEL\"\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        // 127.0.0.1:1 refuses instantly: latest_fresh returns None.
        .args(["update"])
        .env("SABIGOKU_UPDATE_URL", "http://127.0.0.1:1/")
        .env(
            "SABIGOKU_INSTALL_URL",
            format!("file://{}", installer.display()),
        )
        .env("SENTINEL", &sentinel)
        .env_remove("CARGO_HOME")
        .env_remove("CARGO_INSTALL_ROOT")
        .output()
        .expect("spawn sabigoku");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).contains("couldn't confirm a newer release"),
        "{}",
        stdout(&out)
    );
    assert!(
        !sentinel.exists(),
        "the installer must never run on a failed check"
    );
}

/// A tag that parses as newer (Version::parse ignores the prerelease tail's
/// charset) but carries shell metacharacters must be refused before the
/// installer or SABIGOKU_VERSION sees it (ROD-466 review). Pins the
/// safe_ref gate against a future edit that drops it.
#[cfg(feature = "test-seams")]
#[test]
fn update_refuses_a_malformed_newer_tag() {
    let dir = std::env::temp_dir().join("sabigoku-cli-update-malformed");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sentinel = dir.join("installer-ran");
    let installer = dir.join("fake-installer.sh");
    std::fs::write(&installer, "#!/bin/sh\ntouch \"$SENTINEL\"\n").unwrap();

    let url = serve_latest("v999.0.0-;rm -rf ~");
    let out = Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .args(["update"])
        .env("SABIGOKU_UPDATE_URL", &url)
        .env(
            "SABIGOKU_INSTALL_URL",
            format!("file://{}", installer.display()),
        )
        .env("SENTINEL", &sentinel)
        .env_remove("CARGO_HOME")
        .env_remove("CARGO_INSTALL_ROOT")
        .output()
        .expect("spawn sabigoku");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).contains("the latest release tag is malformed"),
        "{}",
        stdout(&out)
    );
    assert!(
        !sentinel.exists(),
        "a malformed tag must never reach the installer"
    );
}

/// The standalone self-update path end to end: newer tag, writable bindir,
/// installer driven with the pinned version. The file:// installer records
/// its env instead of installing, so the whole flow runs with zero network.
#[cfg(feature = "test-seams")]
#[test]
fn update_standalone_drives_the_installer_with_the_pinned_version() {
    let dir = std::env::temp_dir().join("sabigoku-cli-update-standalone");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("installer-env.log");
    let installer = dir.join("fake-installer.sh");
    std::fs::write(
        &installer,
        "#!/bin/sh\nprintf '%s\\n%s\\n' \"$SABIGOKU_VERSION\" \"$BINDIR\" > \"$LOG\"\n",
    )
    .unwrap();

    let url = serve_latest("v999.0.0");
    let out = Command::new(env!("CARGO_BIN_EXE_sabigoku"))
        .args(["update"])
        .env("SABIGOKU_UPDATE_URL", &url)
        .env(
            "SABIGOKU_INSTALL_URL",
            format!("file://{}", installer.display()),
        )
        .env("LOG", &log)
        .env_remove("CARGO_HOME")
        .env_remove("CARGO_INSTALL_ROOT")
        .output()
        .expect("spawn sabigoku");
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(
        text.contains(&format!(
            "update available: v{} -> v999.0.0",
            env!("CARGO_PKG_VERSION")
        )),
        "{text}"
    );
    assert!(text.contains("updated. restart sabigoku"), "{text}");
    let recorded = std::fs::read_to_string(&log).expect("installer ran");
    let mut lines = recorded.lines();
    assert_eq!(
        lines.next(),
        Some("v999.0.0"),
        "version pinned for the installer"
    );
    // Exact equality, not starts_with: a too-shallow bindir (an ancestor of
    // the real one) must fail, which the old prefix check let pass.
    let bindir = lines.next().expect("bindir recorded");
    let expected = std::path::Path::new(env!("CARGO_BIN_EXE_sabigoku"))
        .parent()
        .unwrap();
    assert_eq!(
        std::path::Path::new(bindir),
        expected,
        "BINDIR is exactly the running binary's dir"
    );
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
