//! `sabigoku update` (06 §6.2): decide how to update from install method +
//! writability. Package ownership before writability: a package-managed binary
//! in a root-owned dir must not report "needs root" when the answer is "use
//! your package manager". Standalone drives install.sh, which stages, verifies
//! the checksum and renames into place. Exit law (06 §7.4): always 0; every
//! expected condition (offline, packaged, root-owned) is an outcome, not an
//! error, and a failed install leaves the existing binary untouched.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use crate::semver;
use crate::updatecheck;

/// Wall-clock bound on the installer child. A stalled fetch to a black-holed
/// host would otherwise wedge `update` with no progress and no way to tell
/// working from dead; install.sh stages then renames, so a kill is safe.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

const REPO: &str = "vantroy/sabigoku";

/// The fourth arm is sabigoku's: zigoku has no cargo channel (08 ledger).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Pacman,
    Brew,
    Cargo,
    Standalone,
}

/// Policy product of `decide` (testable without spawn or fs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    PackageDirections(InstallMethod),
    RefuseUnwritable,
    /// Standalone + writable: the bindir to update into.
    SelfUpdate(PathBuf),
}

/// Package manager wins over writability; standalone self-updates only if
/// its dir is writable.
pub fn decide(method: InstallMethod, bindir: &Path, dir_writable: bool) -> Action {
    match method {
        InstallMethod::Pacman | InstallMethod::Brew | InstallMethod::Cargo => {
            Action::PackageDirections(method)
        }
        InstallMethod::Standalone if dir_writable => Action::SelfUpdate(bindir.to_path_buf()),
        InstallMethod::Standalone => Action::RefuseUnwritable,
    }
}

/// Entry point; prints the whole flow to stdout.
pub fn run(current_version: &str, cache_dir: &Path, now: i64) {
    let Ok(exe) = std::env::current_exe() else {
        println!("couldn't locate the running sabigoku binary.");
        println!("update by hand from https://github.com/{REPO}/releases");
        return;
    };
    let bindir = exe.parent().unwrap_or(&exe).to_path_buf();

    // Fresh check, not the 1h cache: acting on an hour-old answer could
    // reinstall the running version. The tag is pre-sanitized
    // (updatecheck::sanitize_tag).
    let confirmed = match updatecheck::latest_fresh(cache_dir, now) {
        Some(latest) if semver::is_newer(&latest, current_version) => {
            println!("update available: v{current_version} -> {latest}\n");
            Some(latest)
        }
        Some(_) => {
            println!("sabigoku v{current_version} is already the latest release.");
            return;
        }
        None => {
            println!("couldn't reach GitHub to check the latest release.\n");
            None
        }
    };

    match decide(detect_method(&exe), &bindir, dir_writable(&bindir)) {
        // Package-manager directions are inert text, safe to print even when
        // the check failed: the manager resolves latest itself.
        Action::PackageDirections(m) => print!("{}", package_directions(m)),
        Action::RefuseUnwritable => print!("{}", refusal(&bindir)),
        // Never run the installer without a confirmed-newer tag. A failed
        // freshness check (GitHub's 60/hr limit is easy to hit behind a NAT)
        // must not curl|sh a blind, unpinned reinstall over a current binary.
        Action::SelfUpdate(dir) => match confirmed {
            Some(tag) => perform_update(&dir, &tag),
            None => println!("couldn't confirm a newer release; not self-updating."),
        },
    }
}

/// Drive install.sh in place: BINDIR + SABIGOKU_VERSION pin where and what;
/// staging, checksum verification and the rename are the installer's job.
/// `tag` is a confirmed-newer release from `run`.
fn perform_update(bindir: &Path, tag: &str) {
    // safe_ref gates BOTH the URL ref and SABIGOKU_VERSION: a forged tag that
    // clears sanitize_tag (printable ASCII) but carries shell metacharacters
    // is refused here, in this layer, not delegated to install.sh's own
    // charset gate (a separate, editable file). Real vX.Y.Z tags always pass.
    if !safe_ref(tag) {
        println!(
            "the latest release tag is malformed; update by hand from https://github.com/{REPO}/releases"
        );
        return;
    }
    let url = updatecheck::url_override("SABIGOKU_INSTALL_URL")
        .unwrap_or_else(|| format!("https://raw.githubusercontent.com/{REPO}/{tag}/install.sh"));

    println!("updating in place at {} ...\n", bindir.display());

    // Download then run: `curl | sh` reports sh's exit (0 on empty stdin),
    // hiding a fetch failure.
    const SNIPPET: &str = r#"set -e
f=$(mktemp)
trap 'rm -f "$f"' EXIT
if command -v curl >/dev/null 2>&1; then curl -fsSL "$INSTALL_URL" -o "$f"; else wget -qO "$f" "$INSTALL_URL"; fi
sh "$f""#;

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(SNIPPET)
        .env("BINDIR", bindir)
        .env("INSTALL_URL", &url)
        .env("SABIGOKU_VERSION", tag)
        .stdin(std::process::Stdio::null());
    match run_grouped_with_timeout(&mut cmd, INSTALL_TIMEOUT) {
        Ok(Some(status)) if status.success() => {
            println!("\nupdated. restart sabigoku to run the new version.")
        }
        Ok(Some(status)) => match status.code() {
            Some(code) => println!(
                "\nupdate failed (installer exited {code}); your existing binary is untouched."
            ),
            None => println!("\nupdate interrupted; your existing binary is untouched."),
        },
        Ok(None) => println!("\nupdate timed out; your existing binary is untouched."),
        Err(e) => println!("couldn't run the installer: {e}"),
    }
}

/// Run `cmd` in its own process group and, if it outlives `timeout`, kill the
/// WHOLE group, not just the direct child: install.sh is `sh -c` wrapping a
/// second `sh` that downloads and installs, and killing only the wrapper
/// leaves that subtree orphaned to init, still running after we report the
/// binary untouched. A distinct group also means the signal never reaches our
/// own process. kill(1), not libc: this crate ships no unsafe.
/// `Ok(None)` is a timeout; `Ok(Some(status))` is a real exit.
fn run_grouped_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    let pgid = child.id();
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(format!("-{pgid}"))
                .status();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Safe git ref for a raw.githubusercontent URL path (no slash or shell
/// metacharacters).
fn safe_ref(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-'))
}

/// Package-manager ownership. A missing tool falls through to standalone.
/// The pacman/brew probes trust PATH order: a shadowing stub can mis-route
/// detection. Accepted, not a hole, misdetection only changes which advice
/// prints or, at worst, refuses a self-update; it never mints a bad install.
fn detect_method(exe: &Path) -> InstallMethod {
    #[cfg(target_os = "linux")]
    if command_succeeds("pacman", &["-Qo".as_ref(), exe.as_os_str()]) {
        return InstallMethod::Pacman;
    }
    #[cfg(target_os = "macos")]
    {
        // `brew list sabigoku` proves the formula exists, not that THIS binary
        // is it; compare against `brew --prefix`. An unresolvable prefix
        // prefers Brew: harmless advice beats corrupting brew bookkeeping
        // with a self-update.
        if command_succeeds("brew", &["list".as_ref(), "sabigoku".as_ref()]) {
            return match command_output("brew", &["--prefix"]) {
                Some(prefix) => {
                    if has_prefix_dir(exe, Path::new(&prefix)) {
                        InstallMethod::Brew
                    } else {
                        InstallMethod::Standalone
                    }
                }
                None => InstallMethod::Brew,
            };
        }
    }
    if is_cargo_bin(
        exe,
        std::env::var("CARGO_INSTALL_ROOT").ok().as_deref(),
        std::env::var("CARGO_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    ) {
        return InstallMethod::Cargo;
    }
    InstallMethod::Standalone
}

/// A binary under cargo's install bin dir belongs to cargo. Path-based, unlike
/// the pacman/brew probes: cargo has no ownership query. Precedence follows
/// cargo's own: CARGO_INSTALL_ROOT, then CARGO_HOME, then ~/.cargo. The
/// `install.root` config-file key is not read (08 ledger: a documented gap).
fn is_cargo_bin(
    exe: &Path,
    install_root: Option<&str>,
    cargo_home: Option<&str>,
    home: Option<&str>,
) -> bool {
    let bin = if let Some(r) = install_root.filter(|s| !s.is_empty()) {
        PathBuf::from(r).join("bin")
    } else if let Some(ch) = cargo_home.filter(|s| !s.is_empty()) {
        PathBuf::from(ch).join("bin")
    } else if let Some(h) = home.filter(|s| !s.is_empty()) {
        PathBuf::from(h).join(".cargo/bin")
    } else {
        return false;
    };
    has_prefix_dir(exe, &bin)
}

/// True if `path` equals `prefix` or is under it on a directory boundary.
fn has_prefix_dir(path: &Path, prefix: &Path) -> bool {
    prefix.components().next().is_some() && path.starts_with(prefix)
}

fn command_succeeds(program: &str, args: &[&std::ffi::OsStr]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Trimmed stdout, or `None` on spawn failure, nonzero exit or empty output.
/// Production use is the macOS brew probe; other targets keep it for tests.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// A probe write, not a permission-bit read: ACLs and root-squash make
/// metadata lie. A relative path is unresolvable here, so refusal is safer
/// than a false go-ahead.
fn dir_writable(dir: &Path) -> bool {
    if !dir.is_absolute() {
        return false;
    }
    let probe = dir.join(format!(".sabigoku-update-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Returned, not printed, so the copy is unit-testable without capturing
/// stdout. `run` prints it.
fn package_directions(method: InstallMethod) -> &'static str {
    match method {
        InstallMethod::Pacman => {
            "sabigoku was installed via the AUR. Update it with your AUR helper:\n  paru -S sabigoku\n  # or: yay -S sabigoku\n"
        }
        InstallMethod::Brew => {
            "sabigoku was installed via Homebrew. Update it with:\n  brew upgrade sabigoku\n"
        }
        InstallMethod::Cargo => {
            "sabigoku was installed via cargo. Update it with:\n  cargo install sabigoku\n"
        }
        InstallMethod::Standalone => unreachable!(),
    }
}

fn refusal(bindir: &Path) -> String {
    format!(
        "sabigoku lives in {}, which needs elevated permissions to write.\n\
         Refusing to self-update a root-owned install. Either:\n\
         \x20 - re-run the installer with sudo, or\n\
         \x20 - reinstall to a writable dir via BINDIR, e.g.:\n\
         \x20     curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install.sh | BINDIR=$HOME/.local/bin sh\n",
        bindir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_a_package_manager_owns_the_update_regardless_of_writability() {
        for method in [
            InstallMethod::Pacman,
            InstallMethod::Brew,
            InstallMethod::Cargo,
        ] {
            for writable in [false, true] {
                assert_eq!(
                    decide(method, Path::new("/usr/bin"), writable),
                    Action::PackageDirections(method)
                );
            }
        }
    }

    #[test]
    fn decide_standalone_self_updates_only_when_its_dir_is_writable() {
        assert_eq!(
            decide(
                InstallMethod::Standalone,
                Path::new("/home/u/.local/bin"),
                true
            ),
            Action::SelfUpdate(PathBuf::from("/home/u/.local/bin"))
        );
        assert_eq!(
            decide(
                InstallMethod::Standalone,
                Path::new("/usr/local/bin"),
                false
            ),
            Action::RefuseUnwritable
        );
    }

    #[test]
    fn has_prefix_dir_matches_on_a_path_boundary_not_a_bare_string_prefix() {
        let p = |s: &str| Path::new(s).to_path_buf();
        assert!(has_prefix_dir(
            &p("/usr/local/bin/sabigoku"),
            Path::new("/usr/local")
        ));
        assert!(has_prefix_dir(
            &p("/opt/homebrew/bin/sabigoku"),
            Path::new("/opt/homebrew")
        ));
        assert!(has_prefix_dir(&p("/usr/local"), Path::new("/usr/local")));
        assert!(has_prefix_dir(
            &p("/usr/local/bin/sabigoku"),
            Path::new("/usr/local/")
        ));
        assert!(!has_prefix_dir(
            &p("/usr/local-other/bin/sabigoku"),
            Path::new("/usr/local")
        ));
        assert!(!has_prefix_dir(
            &p("/home/u/.local/bin/sabigoku"),
            Path::new("/usr/local")
        ));
        assert!(!has_prefix_dir(&p("/anything"), Path::new("")));
    }

    #[test]
    fn is_cargo_bin_follows_install_root_then_cargo_home_then_home() {
        let home_exe = Path::new("/home/u/.cargo/bin/sabigoku");
        assert!(is_cargo_bin(home_exe, None, None, Some("/home/u")));
        assert!(is_cargo_bin(
            Path::new("/custom/cargo/bin/sabigoku"),
            None,
            Some("/custom/cargo"),
            Some("/home/u")
        ));
        // CARGO_INSTALL_ROOT outranks CARGO_HOME outranks ~/.cargo.
        assert!(is_cargo_bin(
            Path::new("/root-install/bin/sabigoku"),
            Some("/root-install"),
            Some("/custom/cargo"),
            Some("/home/u")
        ));
        // CARGO_HOME set means ~/.cargo is NOT the cargo bin dir.
        assert!(!is_cargo_bin(
            home_exe,
            None,
            Some("/custom/cargo"),
            Some("/home/u")
        ));
        assert!(!is_cargo_bin(
            Path::new("/usr/bin/sabigoku"),
            None,
            None,
            Some("/home/u")
        ));
        assert!(!is_cargo_bin(home_exe, None, None, None));
        assert!(!is_cargo_bin(home_exe, Some(""), Some(""), None));
    }

    #[test]
    fn package_directions_name_the_right_channel() {
        assert!(package_directions(InstallMethod::Pacman).contains("paru -S sabigoku"));
        assert!(package_directions(InstallMethod::Brew).contains("brew upgrade sabigoku"));
        assert!(package_directions(InstallMethod::Cargo).contains("cargo install sabigoku"));
    }

    #[test]
    fn refusal_names_the_dir_and_the_bindir_escape_hatch() {
        let text = refusal(Path::new("/usr/bin"));
        assert!(text.contains("/usr/bin"));
        assert!(text.contains("BINDIR=$HOME/.local/bin sh"));
    }

    #[test]
    fn safe_ref_accepts_version_tags_rejects_url_path_breakouts() {
        assert!(safe_ref("v0.4.1"));
        assert!(safe_ref("0.10.0-rc1"));
        assert!(!safe_ref(""));
        assert!(!safe_ref("v1/../../etc"));
        assert!(!safe_ref("v1.0;rm -rf ~"));
        assert!(!safe_ref("v1.0 x"));
        assert!(!safe_ref(&"v".repeat(65)));
    }

    #[test]
    fn dir_writable_probes_the_real_fs_and_rejects_relative_paths() {
        let dir = std::env::temp_dir().join("sabigoku-update-writable");
        let _ = std::fs::create_dir_all(&dir);
        assert!(dir_writable(&dir));
        assert!(
            !dir.join(format!(".sabigoku-update-probe-{}", std::process::id()))
                .exists(),
            "the probe must clean up after itself"
        );
        assert!(!dir_writable(Path::new("/definitely/not/a/real/dir/zzz")));
        assert!(!dir_writable(Path::new("relative/path")));
    }

    #[test]
    fn run_grouped_with_timeout_kills_the_whole_install_tree() {
        // The leader backgrounds a long sleep into its own group (the exact
        // shape that outlived the old child.kill()), records its pid, then
        // blocks. On timeout the whole group must die, grandchild included.
        let marker = std::env::temp_dir().join(format!("sabigoku-pgkill-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("sleep 60 & echo $! > \"$MARKER\"; sleep 60")
            .env("MARKER", &marker)
            .stdin(std::process::Stdio::null());

        let outcome = run_grouped_with_timeout(&mut cmd, Duration::from_millis(300));
        assert!(matches!(outcome, Ok(None)), "must report a timeout");

        std::thread::sleep(Duration::from_millis(200));
        let pid = std::fs::read_to_string(&marker)
            .expect("leader recorded the grandchild pid")
            .trim()
            .to_string();
        // Probe the state, not signal deliverability: a zombie still answers
        // kill -0, and an orphan reparented to a PID 1 that never reaps (a CI
        // job container's `tail -f /dev/null`) stays one indefinitely. An empty
        // row means reaped, Z means dead and unburied; both are gone.
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_default();
        let _ = std::fs::remove_file(&marker);
        assert!(
            state.is_empty() || state.starts_with('Z'),
            "the backgrounded grandchild (pid {pid}) must be killed with the group (ps stat {state:?})"
        );
    }

    #[test]
    fn run_grouped_with_timeout_returns_a_clean_exit() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("exit 7")
            .stdin(std::process::Stdio::null());
        match run_grouped_with_timeout(&mut cmd, Duration::from_secs(5)) {
            Ok(Some(status)) => assert_eq!(status.code(), Some(7)),
            other => panic!("expected a clean exit 7, got {other:?}"),
        }
    }

    #[test]
    fn command_output_captures_and_trims_null_on_failure() {
        assert_eq!(
            command_output("echo", &["/opt/homebrew"]).as_deref(),
            Some("/opt/homebrew")
        );
        assert_eq!(command_output("false", &[]), None);
        assert_eq!(command_output("zzz-no-such-command-zzz", &[]), None);
    }
}
