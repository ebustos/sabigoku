//! mpv spawn, push-based IPC, and the play retry policy (03 §6.3.1, 04 §7.8).
//! StreamLink in, position events out. Progress writes stay caller-side so the
//! 02 §4b gate has one owner (tui::workers glue, 01 §3). Owns the proxy engage
//! guard for exactly the mpv process lifetime (08 §10).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use crate::domain::StreamLink;
use crate::fetchguard::{GuardError, guard_fetch_url};
use crate::proxy::{self, ProxyStartError};

pub const MAX_PLAY_ATTEMPTS: u32 = 3;
/// One entry per retry gap; indexing by attempt - 1 is safe only while
/// len == MAX_PLAY_ATTEMPTS - 1. Keep them in lockstep.
const BACKOFF: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(4)];

/// Connect budget ~2s (03 §6.3.1): mpv creates the socket after argv parse.
const IPC_CONNECT_TRIES: u32 = 40;
const IPC_CONNECT_STEP: Duration = Duration::from_millis(50);
/// Ceiling on one IPC line. Real property-change events are ~100 bytes; a peer
/// that streams an endless line would otherwise grow the read buffer to a
/// process-wide OOM abort. proxy.rs caps its request head for the same reason.
const MAX_IPC_LINE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub secs: f64,
    /// None until mpv learns it (never, for some live streams).
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayerEvent {
    Position(Position),
    /// A retry is scheduled (04 §7.8 PlayRetry toast); fired before the backoff
    /// sleep so the UI shows it during the wait.
    Retry {
        attempt: u32,
    },
}

#[derive(Debug)]
pub struct PlayOutcome {
    /// Last meaningful position (finite, > 0). None = the recordPlay gate
    /// stays shut (02 §4b); zero store writes for this play.
    pub position: Option<Position>,
    pub attempts: u32,
}

pub struct PlayOpts<'a> {
    pub mpv_path: &'a str,
    /// IPC socket dir (paths.runtime); passed in so player stays path-agnostic.
    pub socket_dir: &'a Path,
    pub title: &'a str,
    /// Resume start (03 §6.3.1 rule computed caller-side); emitted only when > 0.
    pub start_secs: f64,
    /// AniSkip adjunct (03 §9), prepared caller-side; None = plain play.
    pub skip: Option<&'a SkipScript>,
}

/// mpv auto-skip wiring: the script path and its `--script-opts` payload.
/// Both are app-constructed (cache path + formatted floats), never provider
/// bytes, so they ride argv without the provider-field vetting.
#[derive(Debug, Clone, PartialEq)]
pub struct SkipScript {
    pub path: PathBuf,
    pub opts: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PlayError {
    #[error("resolve: {0}")]
    Resolve(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("unsafe stream url: {0}")]
    UnsafeUrl(#[from] GuardError),

    #[error("unsafe argv field: {0}")]
    UnsafeArg(&'static str),

    #[error("proxy: {0}")]
    Proxy(#[from] ProxyStartError),

    #[error("spawn {mpv}: {source}")]
    Spawn {
        mpv: String,
        #[source]
        source: io::Error,
    },

    #[error("wait on mpv: {0}")]
    Wait(#[source] io::Error),

    #[error("mpv could not open the stream after {attempts} attempts")]
    OpenFailed { attempts: u32 },

    #[error("mpv exited ({status}) before any playback")]
    Exit { status: ExitStatus },
}

/// One full play: resolve, guard, engage, spawn, observe, retry per 04 §7.8.
/// `resolve` runs once per attempt so every retry fires on a fresh URL.
/// `on_event` is cloned per attempt because the IPC watcher thread takes it
/// by value; channel senders and their wrappers all satisfy the bound.
pub fn play<F>(
    opts: &PlayOpts,
    mut resolve: impl FnMut() -> Result<StreamLink, Box<dyn std::error::Error + Send + Sync>>,
    on_event: F,
) -> Result<PlayOutcome, PlayError>
where
    F: Fn(PlayerEvent) + Send + Clone,
{
    run_attempts(
        |_| {
            let link = resolve().map_err(PlayError::Resolve)?;
            attempt_play(&link, opts, on_event.clone())
        },
        |attempt| on_event(PlayerEvent::Retry { attempt }),
        thread::sleep,
    )
}

struct AttemptResult {
    position: Option<Position>,
    exit: ExitStatus,
}

/// Retry law (04 §7.8): only exit 2 (MpvOpenFailed) with no meaningful play
/// yet retries; hard errors from the attempt abort immediately.
fn run_attempts(
    mut attempt: impl FnMut(u32) -> Result<AttemptResult, PlayError>,
    mut on_retry: impl FnMut(u32),
    mut backoff: impl FnMut(Duration),
) -> Result<PlayOutcome, PlayError> {
    for n in 1..=MAX_PLAY_ATTEMPTS {
        let result = attempt(n)?;
        let open_failed = result.exit.code() == Some(2) && result.position.is_none();
        if !open_failed {
            return match (&result.position, result.exit.success()) {
                (None, false) => Err(PlayError::Exit {
                    status: result.exit,
                }),
                _ => Ok(PlayOutcome {
                    position: result.position,
                    attempts: n,
                }),
            };
        }
        if n < MAX_PLAY_ATTEMPTS {
            on_retry(n + 1);
            backoff(BACKOFF[(n - 1) as usize]);
        }
    }
    Err(PlayError::OpenFailed {
        attempts: MAX_PLAY_ATTEMPTS,
    })
}

fn attempt_play<F>(
    link: &StreamLink,
    opts: &PlayOpts,
    on_event: F,
) -> Result<AttemptResult, PlayError>
where
    F: Fn(PlayerEvent) + Send,
{
    // Guard the upstream url BEFORE engage: the decloak loopback url would
    // (rightly) fail the guard, and the proxy re-guards its own hops.
    guard_fetch_url(&link.url)?;
    let decloak = proxy::engage(link)?;
    let socket = socket_path(opts.socket_dir);
    let argv = build_argv(link, decloak.url(), opts, &socket)?;

    // Clear a stale or pre-planted file so mpv binds its own socket rather than
    // failing on an existing path. Squatter forgery is caught separately by the
    // peer-cred check in connect_ipc, not by this unlink.
    let _ = std::fs::remove_file(&socket);

    // Null stdio or mpv fights the TUI for the terminal it inherited.
    let mut child = Command::new(opts.mpv_path)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| PlayError::Spawn {
            mpv: opts.mpv_path.to_string(),
            source,
        })?;

    let child_pid = child.id();
    let observed = Mutex::new(Observed::default());
    let gone = AtomicBool::new(false);
    let exit = thread::scope(|s| {
        s.spawn(|| {
            if let Some(stream) = connect_ipc(&socket, &gone, child_pid) {
                watch_ipc(stream, &observed, on_event);
            }
        });
        let exit = child.wait();
        gone.store(true, Ordering::Relaxed);
        exit
    });
    // Best-effort: the socket is dead once mpv exits; a leftover file is inert.
    let _ = std::fs::remove_file(&socket);
    let exit = exit.map_err(PlayError::Wait)?;
    let observed = observed.into_inner().unwrap();
    Ok(AttemptResult {
        position: observed.final_position(),
        exit,
    })
    // decloak drops here: proxy lifetime == mpv lifetime (ROD-445 seam).
}

// ── argv (03 §6.3.1, the table is law) ──────────────────────────────────────

fn build_argv(
    link: &StreamLink,
    play_url: &str,
    opts: &PlayOpts,
    socket: &Path,
) -> Result<Vec<String>, PlayError> {
    // Positional: any leading '-' reads as a flag (stricter than the spike's
    // '--' check; a real http(s) url never starts with one).
    if !arg_clean(play_url) || play_url.starts_with('-') {
        return Err(PlayError::UnsafeArg("url"));
    }
    let mut argv = Vec::new();
    if let Some(referer) = &link.referer {
        if !arg_clean(referer) {
            return Err(PlayError::UnsafeArg("referer"));
        }
        argv.push(format!("--http-header-fields-append=Referer: {referer}"));
    }
    if let Some(ua) = &link.user_agent {
        // Dedicated flag, never header-append: two UAs = Cloudflare 403.
        if !ua_clean(ua) {
            return Err(PlayError::UnsafeArg("user_agent"));
        }
        argv.push(format!("--user-agent={ua}"));
    }
    if play_url.starts_with("http://") || play_url.starts_with("https://") {
        argv.push("--stream-lavf-o=multiple_requests=1,icy=0".into());
    }
    if let Some(sub) = &link.sub_url {
        if !arg_clean(sub) {
            return Err(PlayError::UnsafeArg("sub_url"));
        }
        argv.push(format!("--sub-file={sub}"));
        argv.push("--sub-pos=92".into());
        argv.push("--sub-bold=yes".into());
    }
    if link.cloaked_segments {
        argv.push("--demuxer-lavf-o=allowed_extensions=ALL".into());
    }
    let title: String = opts.title.chars().filter(|c| !c.is_control()).collect();
    if !title.is_empty() {
        argv.push(format!("--force-media-title={title}"));
        // ${media-title} expands mpv-side; the raw title never re-parses.
        argv.push("--title=sabigoku - ${media-title}".into());
    }
    argv.push(format!("--input-ipc-server={}", socket.display()));
    if opts.start_secs.is_finite() && opts.start_secs > 0.0 {
        argv.push(format!("--start={}", opts.start_secs));
    }
    if let Some(skip) = opts.skip {
        argv.push(format!("--script={}", skip.path.display()));
        argv.push(format!("--script-opts={}", skip.opts));
    }
    argv.push(play_url.to_string());
    Ok(argv)
}

/// Provider bytes an argv element may carry: printable ASCII, no space. Catches
/// CR/LF (header injection, ROD-92) and >= 0x80 in one range check. Empty is
/// allowed: a blank optional field (referer/sub) yields an inert flag value.
fn arg_clean(s: &str) -> bool {
    s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// UAs are the one field with legitimate spaces, and must be non-empty: an
/// empty `--user-agent=` blanks mpv's UA and defeats the CF bot-score workaround.
fn ua_clean(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

fn socket_path(dir: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let uid = unsafe { libc::getuid() };
    let pid = std::process::id();
    dir.join(format!("sabigoku-mpv-{uid}-{pid}-{counter}.sock"))
}

// ── IPC (push-based, 03 §6.3.1) ─────────────────────────────────────────────

#[derive(Default)]
struct Observed {
    /// Last meaningful time-pos; the whole recordPlay gate hangs off this.
    meaningful_secs: Option<f64>,
    duration: Option<f64>,
}

impl Observed {
    fn final_position(&self) -> Option<Position> {
        self.meaningful_secs.map(|secs| Position {
            secs,
            duration: self.duration,
        })
    }
}

fn meaningful(secs: f64) -> bool {
    secs.is_finite() && secs > 0.0
}

fn connect_ipc(path: &Path, gone: &AtomicBool, child_pid: u32) -> Option<UnixStream> {
    for _ in 0..IPC_CONNECT_TRIES {
        if let Ok(stream) = UnixStream::connect(path) {
            // Bind trust to the child, not the path: a same-uid squatter that
            // won the socket would otherwise feed the watcher forged positions
            // straight into the history DB. Reject any peer that is not our mpv.
            if peer_pid(&stream) == Some(child_pid) {
                return Some(stream);
            }
            // An impostor holds the path; keep probing until it or the budget
            // gives out (mpv failed to bind, so a real peer will not appear,
            // but rejecting still beats adopting the forger).
        }
        // A fast exit-2 mpv never creates the socket; stop burning the budget.
        if gone.load(Ordering::Relaxed) {
            return None;
        }
        thread::sleep(IPC_CONNECT_STEP);
    }
    None
}

/// PID of the process on the other end of a Unix socket (Linux SO_PEERCRED).
/// None if the kernel cannot answer, which is treated as "not our child".
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes a ucred no larger than `len` into `cred` and
    // updates `len`; both outlive the call and the fd is owned by `stream`.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc == 0 && cred.pid > 0 {
        Some(cred.pid as u32)
    } else {
        None
    }
}

/// Subscribe then blocking-read property-change events until EOF (mpv exit
/// closes the socket; no read timeout on purpose, a paused player is silent
/// for minutes). Malformed lines are skipped, never fatal.
fn watch_ipc(stream: UnixStream, observed: &Mutex<Observed>, on_event: impl Fn(PlayerEvent)) {
    let mut writer = &stream;
    for (id, prop) in [(1, "time-pos"), (2, "duration")] {
        let cmd = serde_json::json!({ "command": ["observe_property", id, prop] });
        if writeln!(writer, "{cmd}").is_err() {
            return;
        }
    }
    let mut reader = BufReader::new(&stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        // Per-line cap, not a whole-session one: a Take over &mut reader bounds
        // only this read_until while the BufReader position persists across
        // iterations, so a long playback's many small events still flow.
        let n = match (&mut reader)
            .take(MAX_IPC_LINE_BYTES)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => return, // EOF (mpv exit) or a dead socket
            Ok(n) => n,
        };
        // A full-cap read with no newline is an oversize or newline-starved
        // line; drop the peer rather than keep reading it.
        if n as u64 == MAX_IPC_LINE_BYTES && !line.ends_with(b"\n") {
            return;
        }
        let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        if msg.get("event").and_then(|e| e.as_str()) != Some("property-change") {
            continue;
        }
        let data = msg.get("data").and_then(|d| d.as_f64());
        match msg.get("id").and_then(|i| i.as_u64()) {
            Some(1) => {
                let Some(secs) = data else { continue };
                let duration = {
                    let mut observed = observed.lock().unwrap();
                    if meaningful(secs) {
                        observed.meaningful_secs = Some(secs);
                    }
                    observed.duration
                };
                on_event(PlayerEvent::Position(Position { secs, duration }));
            }
            Some(2) => {
                if let Some(duration) = data {
                    observed.lock().unwrap().duration = Some(duration);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::os::unix::process::ExitStatusExt;

    fn exit(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

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

    fn opts<'a>(title: &'a str, start: f64) -> PlayOpts<'a> {
        PlayOpts {
            mpv_path: "mpv",
            socket_dir: Path::new("/run/user/1000/sabigoku"),
            title,
            start_secs: start,
            skip: None,
        }
    }

    // ── argv ────────────────────────────────────────────────────────────

    #[test]
    fn argv_full_house_matches_the_table() {
        let link = full_link();
        let socket = Path::new("/run/user/1000/sabigoku/s.sock");
        let argv = build_argv(&link, &link.url, &opts("Frieren 冒険", 42.5), socket).unwrap();
        assert_eq!(
            argv,
            vec![
                "--http-header-fields-append=Referer: https://ref.example/",
                "--user-agent=Mozilla/5.0 (X11; Linux) Gecko",
                "--stream-lavf-o=multiple_requests=1,icy=0",
                "--sub-file=https://sub.example/s.vtt",
                "--sub-pos=92",
                "--sub-bold=yes",
                "--demuxer-lavf-o=allowed_extensions=ALL",
                "--force-media-title=Frieren 冒険",
                "--title=sabigoku - ${media-title}",
                "--input-ipc-server=/run/user/1000/sabigoku/s.sock",
                "--start=42.5",
                "https://cdn.example/x.m3u8",
            ]
        );
    }

    #[test]
    fn argv_minimal_link_skips_every_optional_flag() {
        let link = StreamLink {
            url: "https://cdn.example/plain.mp4".into(),
            resolution: None,
            referer: None,
            user_agent: None,
            cloaked_segments: false,
            decloak_segments: false,
            sub_url: None,
        };
        let socket = Path::new("/tmp/s.sock");
        let argv = build_argv(&link, &link.url, &opts("", 0.0), socket).unwrap();
        assert_eq!(
            argv,
            vec![
                "--stream-lavf-o=multiple_requests=1,icy=0",
                "--input-ipc-server=/tmp/s.sock",
                "https://cdn.example/plain.mp4",
            ]
        );
        assert!(!argv.iter().any(|a| a.contains("user-agent")));
    }

    #[test]
    fn argv_rejects_injection_in_provider_fields() {
        let socket = Path::new("/tmp/s.sock");
        let mut link = full_link();
        link.referer = Some("https://e/\r\nX-Evil: 1".into());
        assert!(matches!(
            build_argv(&link, &link.url.clone(), &opts("t", 0.0), socket),
            Err(PlayError::UnsafeArg("referer"))
        ));

        let mut link = full_link();
        link.user_agent = Some("UA\nUA".into());
        assert!(matches!(
            build_argv(&link, &link.url.clone(), &opts("t", 0.0), socket),
            Err(PlayError::UnsafeArg("user_agent"))
        ));

        let mut link = full_link();
        link.sub_url = Some("https://e/s.vtt\u{80}".into());
        assert!(matches!(
            build_argv(&link, &link.url.clone(), &opts("t", 0.0), socket),
            Err(PlayError::UnsafeArg("sub_url"))
        ));

        let link = full_link();
        assert!(matches!(
            build_argv(&link, "--script=/tmp/evil.lua", &opts("t", 0.0), socket),
            Err(PlayError::UnsafeArg("url"))
        ));
    }

    #[test]
    fn argv_title_strips_control_chars_and_empty_title_skips_flags() {
        let link = full_link();
        let socket = Path::new("/tmp/s.sock");
        let argv = build_argv(&link, &link.url, &opts("A\x1b[31mB\r\n", 0.0), socket).unwrap();
        assert!(argv.contains(&"--force-media-title=A[31mB".to_string()));

        let argv = build_argv(&link, &link.url, &opts("\r\n", 0.0), socket).unwrap();
        assert!(!argv.iter().any(|a| a.contains("title")));
    }

    #[test]
    fn argv_start_only_when_positive_and_finite() {
        let link = full_link();
        let socket = Path::new("/tmp/s.sock");
        for start in [0.0, -3.0, f64::NAN] {
            let argv = build_argv(&link, &link.url, &opts("t", start), socket).unwrap();
            assert!(!argv.iter().any(|a| a.starts_with("--start=")));
        }
    }

    #[test]
    fn argv_skip_script_lands_in_table_position() {
        let link = full_link();
        let socket = Path::new("/tmp/s.sock");
        let skip = SkipScript {
            path: PathBuf::from("/cache/skip.lua"),
            opts: "aniskip-op_start=12.5,aniskip-mode=both".into(),
        };
        let mut o = opts("t", 42.5);
        o.skip = Some(&skip);
        let argv = build_argv(&link, &link.url, &o, socket).unwrap();
        let at = |needle: &str| argv.iter().position(|a| a == needle).unwrap();
        assert_eq!(
            at("--script=/cache/skip.lua") + 1,
            at("--script-opts=aniskip-op_start=12.5,aniskip-mode=both"),
        );
        // Table order (03 §6.3.1): aniskip after --start, before the url.
        assert!(at("--script=/cache/skip.lua") > at("--start=42.5"));
        assert_eq!(argv.last().unwrap(), &link.url);
    }

    #[test]
    fn socket_paths_are_unique_per_launch() {
        let dir = Path::new("/tmp");
        let a = socket_path(dir);
        let b = socket_path(dir);
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("sabigoku-mpv-") && name.ends_with(".sock"));
    }

    // ── retry policy ────────────────────────────────────────────────────

    fn scripted(
        results: Vec<Result<AttemptResult, PlayError>>,
    ) -> impl FnMut(u32) -> Result<AttemptResult, PlayError> {
        let mut iter = results.into_iter();
        move |_| iter.next().expect("more attempts than scripted")
    }

    fn open_failed() -> Result<AttemptResult, PlayError> {
        Ok(AttemptResult {
            position: None,
            exit: exit(2),
        })
    }

    #[test]
    fn open_failed_retries_with_backoff_then_succeeds() {
        let mut sleeps = Vec::new();
        let mut retries = Vec::new();
        let out = run_attempts(
            scripted(vec![
                open_failed(),
                open_failed(),
                Ok(AttemptResult {
                    position: Some(Position {
                        secs: 3.0,
                        duration: Some(24.0),
                    }),
                    exit: exit(0),
                }),
            ]),
            |n| retries.push(n),
            |d| sleeps.push(d),
        )
        .unwrap();
        assert_eq!(out.attempts, 3);
        assert_eq!(out.position.unwrap().secs, 3.0);
        assert_eq!(sleeps, [Duration::from_secs(2), Duration::from_secs(4)]);
        assert_eq!(retries, [2, 3]);
    }

    #[test]
    fn exit_two_with_meaningful_play_never_retries() {
        let out = run_attempts(
            scripted(vec![Ok(AttemptResult {
                position: Some(Position {
                    secs: 300.0,
                    duration: None,
                }),
                exit: exit(2),
            })]),
            |_| panic!("no retry"),
            |_| panic!("no backoff"),
        )
        .unwrap();
        assert_eq!(out.attempts, 1);
        assert_eq!(out.position.unwrap().secs, 300.0);
    }

    #[test]
    fn exhausted_budget_is_open_failed() {
        let mut sleeps = Vec::new();
        let err = run_attempts(
            scripted(vec![open_failed(), open_failed(), open_failed()]),
            |_| {},
            |d| sleeps.push(d),
        )
        .unwrap_err();
        assert!(matches!(err, PlayError::OpenFailed { attempts: 3 }));
        assert_eq!(sleeps, [Duration::from_secs(2), Duration::from_secs(4)]);
    }

    #[test]
    fn other_exit_codes_fail_without_retry() {
        let err = run_attempts(
            scripted(vec![Ok(AttemptResult {
                position: None,
                exit: exit(1),
            })]),
            |_| panic!("no retry"),
            |_| panic!("no backoff"),
        )
        .unwrap_err();
        assert!(matches!(err, PlayError::Exit { .. }));
    }

    #[test]
    fn clean_exit_without_playback_is_ok_with_gate_shut() {
        let out = run_attempts(
            scripted(vec![Ok(AttemptResult {
                position: None,
                exit: exit(0),
            })]),
            |_| {},
            |_| panic!("no backoff"),
        )
        .unwrap();
        assert!(out.position.is_none());
    }

    #[test]
    fn hard_attempt_error_aborts_immediately() {
        let err = run_attempts(
            scripted(vec![Err(PlayError::UnsafeArg("url"))]),
            |_| panic!("no retry"),
            |_| panic!("no backoff"),
        )
        .unwrap_err();
        assert!(matches!(err, PlayError::UnsafeArg("url")));
    }

    // ── IPC watcher ─────────────────────────────────────────────────────

    #[test]
    fn ipc_handshake_events_and_final_position() {
        let (client, server) = UnixStream::pair().unwrap();
        {
            let mut srv = &server;
            for line in [
                r#"{"event":"property-change","id":1,"name":"time-pos","data":5.5}"#,
                r#"{"event":"property-change","id":2,"name":"duration","data":24.0}"#,
                r#"{"event":"property-change","id":1,"name":"time-pos","data":6.5}"#,
                r#"{"event":"property-change","id":1,"name":"time-pos","data":null}"#,
                "not json at all",
                r#"{"request_id":0,"error":"success"}"#,
                r#"{"event":"property-change","id":1,"name":"time-pos","data":0.0}"#,
            ] {
                writeln!(srv, "{line}").unwrap();
            }
        }
        server.shutdown(Shutdown::Write).unwrap();

        let observed = Mutex::new(Observed::default());
        let events = Mutex::new(Vec::new());
        watch_ipc(client, &observed, |e| events.lock().unwrap().push(e));

        let mut handshake = String::new();
        let mut reader = BufReader::new(&server);
        for expected in [
            serde_json::json!({ "command": ["observe_property", 1, "time-pos"] }),
            serde_json::json!({ "command": ["observe_property", 2, "duration"] }),
        ] {
            handshake.clear();
            reader.read_line(&mut handshake).unwrap();
            let sent: serde_json::Value = serde_json::from_str(&handshake).unwrap();
            assert_eq!(sent, expected);
        }

        let events = events.into_inner().unwrap();
        assert_eq!(
            events,
            [
                PlayerEvent::Position(Position {
                    secs: 5.5,
                    duration: None
                }),
                PlayerEvent::Position(Position {
                    secs: 6.5,
                    duration: Some(24.0)
                }),
                PlayerEvent::Position(Position {
                    secs: 0.0,
                    duration: Some(24.0)
                }),
            ]
        );
        assert_eq!(
            observed.into_inner().unwrap().final_position(),
            Some(Position {
                secs: 6.5,
                duration: Some(24.0)
            })
        );
    }

    #[test]
    fn ipc_without_meaningful_position_keeps_the_gate_shut() {
        let (client, server) = UnixStream::pair().unwrap();
        {
            let mut srv = &server;
            writeln!(
                srv,
                r#"{{"event":"property-change","id":1,"name":"time-pos","data":0.0}}"#
            )
            .unwrap();
        }
        server.shutdown(Shutdown::Write).unwrap();
        let observed = Mutex::new(Observed::default());
        watch_ipc(client, &observed, |_| {});
        assert_eq!(observed.into_inner().unwrap().final_position(), None);
    }

    #[test]
    fn meaningful_is_finite_and_positive() {
        for bad in [0.0, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!meaningful(bad));
        }
        assert!(meaningful(0.001));
    }

    #[test]
    fn ipc_rejects_a_peer_that_is_not_our_child() {
        use std::os::unix::net::UnixListener;

        let path = std::env::temp_dir().join(format!("sabigoku-peer-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // A same-uid squatter: the socket exists and answers, but its peer pid
        // is this test process, never the mpv child we would be expecting.
        let _squatter = UnixListener::bind(&path).unwrap();

        // gone=true bounds the loop to one reject-then-bail pass.
        let gone = AtomicBool::new(true);
        assert!(
            connect_ipc(&path, &gone, std::process::id().wrapping_add(1)).is_none(),
            "a peer whose pid is not the child must be refused"
        );
        // The same socket is adopted when the expected pid matches its peer.
        let gone = AtomicBool::new(false);
        assert!(connect_ipc(&path, &gone, std::process::id()).is_some());

        std::fs::remove_file(&path).ok();
    }

    // ── play() edges ────────────────────────────────────────────────────

    #[test]
    fn private_stream_url_is_blocked_before_spawn() {
        let mut link = full_link();
        link.url = "http://192.168.1.10/x.m3u8".into();
        let opts = PlayOpts {
            mpv_path: "/definitely/not/mpv",
            socket_dir: Path::new("/tmp"),
            title: "t",
            start_secs: 0.0,
            skip: None,
        };
        let err = play(&opts, || Ok(link.clone()), |_| {}).unwrap_err();
        assert!(matches!(err, PlayError::UnsafeUrl(GuardError::BlockedHost)));
    }

    #[test]
    fn decloak_link_engages_and_reaches_spawn_unguarded_loopback() {
        let mut link = full_link();
        link.decloak_segments = true;
        let opts = PlayOpts {
            mpv_path: "/definitely/not/mpv",
            socket_dir: Path::new("/tmp"),
            title: "t",
            start_secs: 0.0,
            skip: None,
        };
        // Spawn (not UnsafeUrl) proves the guard ran on the upstream url and
        // the engaged loopback url was handed to mpv untouched.
        let err = play(&opts, || Ok(link.clone()), |_| {}).unwrap_err();
        assert!(matches!(err, PlayError::Spawn { .. }));
    }

    #[test]
    fn resolve_failure_propagates_without_attempting() {
        let opts = PlayOpts {
            mpv_path: "/definitely/not/mpv",
            socket_dir: Path::new("/tmp"),
            title: "t",
            start_secs: 0.0,
            skip: None,
        };
        let err = play(&opts, || Err("hash rotated".into()), |_| {}).unwrap_err();
        assert!(matches!(err, PlayError::Resolve(_)));
    }

    /// The ROD-445 seam under real spawn: a decloak link must reach mpv with
    /// the loopback proxy url as the positional, never the raw upstream. A fake
    /// mpv records its argv so a future `link.url` regression here fails loudly
    /// instead of resting on the pure-`build_argv` tests (which never see the
    /// engage wiring).
    #[test]
    fn decloak_url_not_upstream_reaches_the_spawned_argv() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "sabigoku-e2e-{}-{}",
            std::process::id(),
            socket_path(Path::new("/"))
                .file_name()
                .unwrap()
                .to_string_lossy(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let argv_dump = dir.join("argv");
        let fake_mpv = dir.join("mpv.sh");
        std::fs::write(
            &fake_mpv,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nexit 0\n",
                argv_dump.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_mpv, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut link = full_link();
        link.decloak_segments = true;
        let opts = PlayOpts {
            mpv_path: fake_mpv.to_str().unwrap(),
            socket_dir: &dir,
            title: "t",
            start_secs: 0.0,
            skip: None,
        };
        let outcome = play(&opts, || Ok(link.clone()), |_| {}).unwrap();
        assert_eq!(outcome.attempts, 1);

        let dumped = std::fs::read_to_string(&argv_dump).unwrap();
        let positional = dumped.lines().last().unwrap();
        assert!(
            positional.starts_with("http://127.0.0.1:"),
            "positional was {positional:?}, expected the loopback proxy url"
        );
        assert_ne!(positional, link.url, "raw upstream must not reach mpv");
        std::fs::remove_dir_all(&dir).ok();
    }
}
