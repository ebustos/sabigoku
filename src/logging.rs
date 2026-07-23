//! Log sink (06 §1): the TUI owns the terminal, so lines go to a rotating file
//! under the data dir, never stderr. Call sites use the `log` facade macros;
//! before `init` runs they are no-ops. Debug is gated by SABIGOKU_DEBUG; info
//! and up always emit. zigoku opened its log O_NOFOLLOW (planted-symlink
//! defense); flexi_logger owns the open here, so that check is gone. The data
//! dir is not world-writable, so planting a symlink there needs the owner.

use std::path::Path;

use flexi_logger::{Cleanup, Criterion, FileSpec, FlexiLoggerError, Logger, LoggerHandle, Naming};

const ROTATE_AT_BYTES: u64 = 2 * 1024 * 1024;
const ROTATED_FILES_KEPT: usize = 3;

/// Install the sink: `sabigoku_rCURRENT.log` in `data_dir`, appended across
/// runs, rotated by size. Dropping the handle shuts the sink down (later
/// emits are dropped); hold it for the whole run.
pub fn init(data_dir: &Path) -> Result<LoggerHandle, FlexiLoggerError> {
    let debug = env_debug();
    let handle = Logger::try_with_str(spec(debug))?
        .log_to_file(
            FileSpec::default()
                .directory(data_dir)
                .basename("sabigoku")
                .suppress_timestamp(),
        )
        .append()
        .rotate(
            Criterion::Size(ROTATE_AT_BYTES),
            Naming::Numbers,
            Cleanup::KeepLogFiles(ROTATED_FILES_KEPT),
        )
        .format(flexi_logger::opt_format)
        .start()?;
    // The file writer opens on the first record; this line makes every boot
    // leave a trace (and a session boundary) even when nothing goes wrong.
    log::info!(
        "sabigoku {} started; debug={debug}",
        env!("CARGO_PKG_VERSION")
    );
    Ok(handle)
}

/// Truthy SABIGOKU_DEBUG turns debug lines on; anything else leaves them off.
pub fn env_debug() -> bool {
    debug_requested(std::env::var("SABIGOKU_DEBUG").ok().as_deref())
}

fn debug_requested(value: Option<&str>) -> bool {
    let Some(v) = value else { return false };
    ["1", "true", "yes", "on"]
        .iter()
        .any(|t| v.eq_ignore_ascii_case(t))
}

/// Foreign crates that also speak `log` (rustls et al.) stay at info even in
/// debug mode; only our lines get chattier.
fn spec(debug: bool) -> &'static str {
    if debug {
        "info, sabigoku=debug"
    } else {
        "info"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_requested_honors_truthy_and_treats_everything_else_as_off() {
        assert!(!debug_requested(None));
        for truthy in ["1", "true", "YES", "on"] {
            assert!(debug_requested(Some(truthy)), "{truthy}");
        }
        for falsy in ["0", "false", "no", "off", "", "2", "maybe"] {
            assert!(!debug_requested(Some(falsy)), "{falsy:?}");
        }
    }

    // The process has one global-logger slot; this is the only test in the
    // suite allowed to claim it.
    #[test]
    fn init_lands_lines_in_the_rotating_file_under_the_data_dir() {
        let dir = std::env::temp_dir().join("sabigoku-logging-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let handle = init(&dir).unwrap();
        log::warn!("logging-test sentinel");
        handle.flush();

        let content = std::fs::read_to_string(dir.join("sabigoku_rCURRENT.log")).unwrap();
        assert!(content.contains("WARN"), "{content}");
        assert!(content.contains("logging-test sentinel"), "{content}");

        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
