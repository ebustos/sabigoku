//! `sabigoku update` (06 §6.2): decide how to update from install method +
//! writability. Package ownership before writability: a package-managed binary
//! in a root-owned dir must not report "needs root" when the answer is "use
//! your package manager". Standalone drives install.sh, which stages, verifies
//! the checksum and renames into place. Exit law (06 §7.4): always 0; every
//! expected condition (offline, packaged, root-owned) is an outcome, not an
//! error, and a failed install leaves the existing binary untouched.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::semver;
use crate::updatecheck;

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
    // reinstall the version already running. Offline still continues by
    // install method; the tag is kept so the installer pins the compared
    // release. Arrives pre-sanitized (updatecheck::sanitize_tag).
    let mut latest_tag = None;
    match updatecheck::latest_fresh(cache_dir, now) {
        Some(latest) => {
            if !semver::is_newer(&latest, current_version) {
                println!("sabigoku v{current_version} is already the latest release.");
                return;
            }
            println!("update available: v{current_version} -> {latest}\n");
            latest_tag = Some(latest);
        }
        None => println!("couldn't reach GitHub to check the latest release; continuing anyway.\n"),
    }

    let method = detect_method(&exe);
    match decide(method, &bindir, dir_writable(&bindir)) {
        Action::PackageDirections(m) => print_package_directions(m),
        Action::RefuseUnwritable => print_refusal(&bindir),
        Action::SelfUpdate(dir) => perform_update(&dir, latest_tag.as_deref()),
    }
}

/// Drive install.sh in place: BINDIR + SABIGOKU_VERSION pin where and what;
/// staging, checksum verification and the rename are the installer's job.
fn perform_update(bindir: &Path, latest_tag: Option<&str>) {
    // Pin install.sh itself to the release tag, not master, so a compromised
    // master cannot ignore the version pin. Env only, never shell-interpolated.
    let git_ref = match latest_tag {
        Some(t) if safe_ref(t) => t,
        _ => "master",
    };
    // Test seam (tests/cli.rs): lets the suite exercise this whole path
    // against a file:// installer instead of executing the real one.
    let url = std::env::var("SABIGOKU_INSTALL_URL").unwrap_or_else(|_| {
        format!("https://raw.githubusercontent.com/{REPO}/{git_ref}/install.sh")
    });

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
        .stdin(std::process::Stdio::null());
    if let Some(tag) = latest_tag {
        cmd.env("SABIGOKU_VERSION", tag);
    }
    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            println!("couldn't launch the installer: {e}");
            return;
        }
    };
    if status.success() {
        println!("\nupdated. restart sabigoku to run the new version.");
    } else {
        match status.code() {
            Some(code) => println!(
                "\nupdate failed (installer exited {code}); your existing binary is untouched."
            ),
            None => println!("\nupdate interrupted; your existing binary is untouched."),
        }
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
        std::env::var("CARGO_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    ) {
        return InstallMethod::Cargo;
    }
    InstallMethod::Standalone
}

/// A binary under `$CARGO_HOME/bin` (or `~/.cargo/bin`) belongs to cargo.
/// Path-based, unlike the pacman/brew probes: cargo has no ownership query.
fn is_cargo_bin(exe: &Path, cargo_home: Option<&str>, home: Option<&str>) -> bool {
    let bin = match (cargo_home, home) {
        (Some(ch), _) if !ch.is_empty() => PathBuf::from(ch).join("bin"),
        (_, Some(h)) if !h.is_empty() => PathBuf::from(h).join(".cargo/bin"),
        _ => return false,
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

fn print_package_directions(method: InstallMethod) {
    match method {
        InstallMethod::Pacman => println!(
            "sabigoku was installed via the AUR. Update it with your AUR helper:\n  paru -S sabigoku\n  # or: yay -S sabigoku"
        ),
        InstallMethod::Brew => println!(
            "sabigoku was installed via Homebrew. Update it with:\n  brew upgrade sabigoku"
        ),
        InstallMethod::Cargo => {
            println!("sabigoku was installed via cargo. Update it with:\n  cargo install sabigoku")
        }
        InstallMethod::Standalone => unreachable!(),
    }
}

fn print_refusal(bindir: &Path) {
    println!(
        "sabigoku lives in {}, which needs elevated permissions to write.\n\
         Refusing to self-update a root-owned install. Either:\n\
         \x20 - re-run the installer with sudo, or\n\
         \x20 - reinstall to a writable dir via BINDIR, e.g.:\n\
         \x20     curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install.sh | BINDIR=$HOME/.local/bin sh",
        bindir.display()
    );
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
    fn is_cargo_bin_prefers_cargo_home_falls_back_to_home() {
        let exe = Path::new("/home/u/.cargo/bin/sabigoku");
        assert!(is_cargo_bin(exe, None, Some("/home/u")));
        assert!(is_cargo_bin(
            Path::new("/custom/cargo/bin/sabigoku"),
            Some("/custom/cargo"),
            Some("/home/u")
        ));
        // CARGO_HOME set means ~/.cargo is NOT the cargo bin dir.
        assert!(!is_cargo_bin(exe, Some("/custom/cargo"), Some("/home/u")));
        assert!(!is_cargo_bin(
            Path::new("/usr/bin/sabigoku"),
            None,
            Some("/home/u")
        ));
        assert!(!is_cargo_bin(exe, None, None));
        assert!(!is_cargo_bin(exe, Some(""), None));
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
    fn command_output_captures_and_trims_null_on_failure() {
        assert_eq!(
            command_output("echo", &["/opt/homebrew"]).as_deref(),
            Some("/opt/homebrew")
        );
        assert_eq!(command_output("false", &[]), None);
        assert_eq!(command_output("zzz-no-such-command-zzz", &[]), None);
    }
}
