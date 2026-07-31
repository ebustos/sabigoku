//! ffmpeg remux-to-disk transport (episode download, `d` shortcut). Shares
//! the resolve → guard → proxy-engage discipline with `player.rs`, but skips
//! its retry policy and push-based IPC entirely: a download is one shot
//! (resolve once, remux once), and `-loglevel error` keeps ffmpeg's stderr
//! small enough to fully capture with `Command::output()` rather than
//! player.rs's bespoke bounded-pipe reader.
//!
//! `StreamLink` in, a landed file on disk out. The decloak proxy
//! (`proxy::engage`) is reused verbatim: segments cloaked with a decoy
//! header need the same local stripping hop ffmpeg cannot see through any
//! more than mpv can (proxy.rs's doc comment: "no ffmpeg flag reaches the
//! inner segment demuxer").

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::domain::StreamLink;
use crate::fetchguard::{GuardError, guard_fetch_url};
use crate::player::{arg_clean, ua_clean};
use crate::proxy::{self, ProxyStartError};

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("unsafe stream url: {0}")]
    UnsafeUrl(#[from] GuardError),

    #[error("unsafe argv field: {0}")]
    UnsafeArg(&'static str),

    #[error("proxy: {0}")]
    Proxy(#[from] ProxyStartError),

    #[error("create download dir {0}: {1}")]
    Dir(PathBuf, #[source] io::Error),

    #[error("spawn {ffmpeg}: {source}")]
    Spawn {
        ffmpeg: String,
        #[source]
        source: io::Error,
    },

    #[error("ffmpeg exited ({status}): {stderr}")]
    Failed { status: ExitStatus, stderr: String },
}

pub struct DownloadOpts<'a> {
    pub ffmpeg_path: &'a str,
    pub output_dir: &'a Path,
    /// Filesystem-safe (caller applies `domain::sanitize_filename`); the
    /// extension is ours to pick (`.mkv`), never the caller's.
    pub base_name: &'a str,
}

/// `ffmpeg -version` probe: cheap enough to run at startup and after a
/// Settings commit, but callers must cache the result rather than
/// re-spawning it per keypress (04 §7 "never re-spawned on every `d` press").
pub fn ffmpeg_available(ffmpeg_path: &str) -> bool {
    Command::new(ffmpeg_path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// One full download: guard the upstream url, engage the decloak proxy if
/// flagged, remux the main stream, then best-effort remux the soft subtitle
/// (if any) alongside it. A subtitle failure never fails the download — it
/// is silently skipped.
pub fn download(opts: &DownloadOpts, link: &StreamLink) -> Result<PathBuf, DownloadError> {
    guard_fetch_url(&link.url)?;
    let decloak = proxy::engage(link)?;
    std::fs::create_dir_all(opts.output_dir)
        .map_err(|e| DownloadError::Dir(opts.output_dir.to_path_buf(), e))?;
    let output = unique_path(opts.output_dir, opts.base_name, "mkv");
    run_ffmpeg(opts.ffmpeg_path, link, decloak.url(), &output)?;

    if let Some(sub_url) = &link.sub_url {
        // Same referer/UA, no decloak (subtitle tracks are plain https,
        // never segment-cloaked); a guard failure or ffmpeg hiccup here is
        // silent, not surfaced — the video already landed.
        if guard_fetch_url(sub_url).is_ok() {
            let sub_link = StreamLink {
                url: sub_url.clone(),
                referer: link.referer.clone(),
                user_agent: link.user_agent.clone(),
                ..StreamLink::default()
            };
            let sub_out = output.with_extension("vtt");
            let _ = run_ffmpeg(opts.ffmpeg_path, &sub_link, sub_url, &sub_out);
        }
    }
    Ok(output)
}

/// First free `{dir}/{base_name}.{ext}`, else `{base_name} (2).{ext}`, `(3)`,
/// ... A prior download of the same episode is never silently clobbered.
fn unique_path(dir: &Path, base_name: &str, ext: &str) -> PathBuf {
    let plain = dir.join(format!("{base_name}.{ext}"));
    if !plain.exists() {
        return plain;
    }
    let mut n = 2u32;
    loop {
        let candidate = dir.join(format!("{base_name} ({n}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

fn run_ffmpeg(
    ffmpeg_path: &str,
    link: &StreamLink,
    play_url: &str,
    output: &Path,
) -> Result<(), DownloadError> {
    let argv = build_ffmpeg_argv(link, play_url, output)?;
    let out = Command::new(ffmpeg_path)
        .args(&argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| DownloadError::Spawn {
            ffmpeg: ffmpeg_path.to_string(),
            source,
        })?;
    if !out.status.success() {
        return Err(DownloadError::Failed {
            status: out.status,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

// ── argv (mirrors player.rs's build_argv table, minus mpv-only flags) ──────

fn build_ffmpeg_argv(
    link: &StreamLink,
    play_url: &str,
    output: &Path,
) -> Result<Vec<String>, DownloadError> {
    if !arg_clean(play_url) || play_url.starts_with('-') {
        return Err(DownloadError::UnsafeArg("url"));
    }
    let mut argv = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-nostats".to_string(),
    ];
    if let Some(referer) = &link.referer {
        if !arg_clean(referer) {
            return Err(DownloadError::UnsafeArg("referer"));
        }
        argv.push("-headers".into());
        argv.push(format!("Referer: {referer}\r\n"));
    }
    if let Some(ua) = &link.user_agent {
        if !ua_clean(ua) {
            return Err(DownloadError::UnsafeArg("user_agent"));
        }
        argv.push("-user_agent".into());
        argv.push(ua.clone());
    }
    if link.cloaked_segments {
        argv.push("-allowed_extensions".into());
        argv.push("ALL".into());
    }
    argv.push("-i".into());
    argv.push(play_url.to_string());
    argv.push("-c".into());
    argv.push("copy".into());
    // stdin is nulled; without -y a pre-existing output (should never
    // happen past unique_path, but a subtitle re-run races nothing else)
    // would have ffmpeg block reading a y/n prompt from a closed stdin.
    argv.push("-y".into());
    argv.push(output.to_string_lossy().into_owned());
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_link() -> StreamLink {
        StreamLink {
            url: "https://cdn.example/x.m3u8".into(),
            resolution: Some(1080),
            referer: Some("https://ref.example/".into()),
            user_agent: Some("Mozilla/5.0 (X11; Linux) Gecko".into()),
            cloaked_segments: true,
            decloak_segments: false,
            sub_url: Some("https://sub.example/s.vtt".into()),
        }
    }

    #[test]
    fn argv_full_house_matches_the_table() {
        let link = full_link();
        let out = Path::new("/tmp/out.mkv");
        let argv = build_ffmpeg_argv(&link, &link.url, out).unwrap();
        assert_eq!(
            argv,
            vec![
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostats",
                "-headers",
                "Referer: https://ref.example/\r\n",
                "-user_agent",
                "Mozilla/5.0 (X11; Linux) Gecko",
                "-allowed_extensions",
                "ALL",
                "-i",
                "https://cdn.example/x.m3u8",
                "-c",
                "copy",
                "-y",
                "/tmp/out.mkv",
            ]
        );
    }

    #[test]
    fn argv_minimal_link_skips_every_optional_flag() {
        let link = StreamLink {
            url: "https://cdn.example/plain.mp4".into(),
            ..StreamLink::default()
        };
        let out = Path::new("/tmp/out.mkv");
        let argv = build_ffmpeg_argv(&link, &link.url, out).unwrap();
        assert_eq!(
            argv,
            vec![
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostats",
                "-i",
                "https://cdn.example/plain.mp4",
                "-c",
                "copy",
                "-y",
                "/tmp/out.mkv",
            ]
        );
    }

    #[test]
    fn argv_rejects_injection_in_provider_fields() {
        let out = Path::new("/tmp/out.mkv");

        let mut link = full_link();
        link.referer = Some("https://e/\r\nX-Evil: 1".into());
        assert!(matches!(
            build_ffmpeg_argv(&link, &link.url.clone(), out),
            Err(DownloadError::UnsafeArg("referer"))
        ));

        let mut link = full_link();
        link.user_agent = Some("UA\nUA".into());
        assert!(matches!(
            build_ffmpeg_argv(&link, &link.url.clone(), out),
            Err(DownloadError::UnsafeArg("user_agent"))
        ));

        let link = full_link();
        assert!(matches!(
            build_ffmpeg_argv(&link, "-i evil.conf", out),
            Err(DownloadError::UnsafeArg("url"))
        ));
    }

    #[test]
    fn unique_path_avoids_clobbering_existing_files() {
        let dir = std::env::temp_dir().join(format!(
            "sabigoku-downloader-unique-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "Show - 1", "mkv");
        assert_eq!(first, dir.join("Show - 1.mkv"));
        std::fs::write(&first, b"x").unwrap();

        let second = unique_path(&dir, "Show - 1", "mkv");
        assert_eq!(second, dir.join("Show - 1 (2).mkv"));
        std::fs::write(&second, b"x").unwrap();

        let third = unique_path(&dir, "Show - 1", "mkv");
        assert_eq!(third, dir.join("Show - 1 (3).mkv"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ffmpeg_available_is_false_for_a_missing_binary() {
        assert!(!ffmpeg_available("/definitely/not/ffmpeg"));
    }

    /// End-to-end against a fake ffmpeg: proves `download` engages the
    /// decloak proxy and points the spawned argv at the loopback url, never
    /// the raw upstream (the same ROD-445 seam player.rs guards for mpv).
    #[test]
    fn decloak_link_reaches_the_spawned_argv_via_loopback() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "sabigoku-downloader-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let argv_dump = dir.join("argv");
        let fake_ffmpeg = dir.join("ffmpeg.sh");
        std::fs::write(
            &fake_ffmpeg,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nexit 0\n",
                argv_dump.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out_dir = dir.join("out");
        let mut link = full_link();
        link.decloak_segments = true;
        link.sub_url = None; // keep the dump to one ffmpeg invocation
        let opts = DownloadOpts {
            ffmpeg_path: fake_ffmpeg.to_str().unwrap(),
            output_dir: &out_dir,
            base_name: "Show - 1",
        };
        let path = download(&opts, &link).unwrap();
        assert_eq!(path, out_dir.join("Show - 1.mkv"));

        let dumped = std::fs::read_to_string(&argv_dump).unwrap();
        let lines: Vec<&str> = dumped.lines().collect();
        let i_pos = lines.iter().position(|l| *l == "-i").unwrap();
        let play_url = lines[i_pos + 1];
        assert!(
            play_url.starts_with("http://127.0.0.1:"),
            "positional was {play_url:?}, expected the loopback proxy url"
        );
        assert_ne!(play_url, link.url, "raw upstream must not reach ffmpeg");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn private_stream_url_is_blocked_before_spawn() {
        let mut link = full_link();
        link.url = "http://192.168.1.10/x.m3u8".into();
        let opts = DownloadOpts {
            ffmpeg_path: "/definitely/not/ffmpeg",
            output_dir: Path::new("/tmp"),
            base_name: "x",
        };
        let err = download(&opts, &link).unwrap_err();
        assert!(matches!(
            err,
            DownloadError::UnsafeUrl(GuardError::BlockedHost)
        ));
    }

    #[test]
    fn ffmpeg_failure_surfaces_stderr() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "sabigoku-downloader-fail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fake_ffmpeg = dir.join("ffmpeg.sh");
        std::fs::write(
            &fake_ffmpeg,
            "#!/bin/sh\necho 'boom: 403 forbidden' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out_dir = dir.join("out");
        let mut link = full_link();
        link.sub_url = None;
        let opts = DownloadOpts {
            ffmpeg_path: fake_ffmpeg.to_str().unwrap(),
            output_dir: &out_dir,
            base_name: "Show - 1",
        };
        let err = download(&opts, &link).unwrap_err();
        match err {
            DownloadError::Failed { stderr, .. } => {
                assert!(stderr.contains("403 forbidden"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
