//! spike_mpv: the capstone. Resolve a stream, hand it to mpv, end to end.
//! Parity: zigoku spike #5 (mpv_play.zig, ROD-57).
//!
//! The resolver is proven offline in spike_stream and the live provider is
//! flaky, so this spike stands in a deterministic stream URL (libavfilter's
//! testsrc: no network, no file) for the resolver's output and focuses on the
//! genuinely new capability: spawning mpv as a child, passing flags through,
//! and reading its exit status.
//!
//! Run:  cargo run --bin spike_mpv                                   # opens a window
//!       cargo run --bin spike_mpv -- --frames=1 --vo=null --no-audio # headless probe

use std::process::{Command, ExitCode};

// In the real app this URL comes from the resolver (see spike_stream). A
// libavfilter virtual source keeps the capstone deterministic and offline.
const STREAM_URL: &str = "av://lavfi:testsrc=size=320x240:rate=1";

fn main() -> ExitCode {
    // Everything after the binary name passes straight through to mpv. This is
    // why one program serves both a human ("open a window") and a CI probe
    // ("decode one frame headless and exit 0").
    let passthrough: Vec<String> = std::env::args().skip(1).collect();

    // stdin/stdout/stderr inherit by default, so mpv takes the terminal/display
    // with no plumbing. "mpv" resolves via the parent's PATH.
    let mut cmd = Command::new("mpv");
    cmd.arg(STREAM_URL).args(&passthrough);
    println!("spawning: mpv {STREAM_URL} {}", passthrough.join(" "));

    match cmd.status() {
        Ok(status) if status.success() => {
            println!("mpv exited 0");
            ExitCode::SUCCESS
        }
        Ok(status) => {
            println!("mpv exited: {status}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("failed to spawn mpv: {e}");
            ExitCode::FAILURE
        }
    }
}
