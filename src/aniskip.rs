//! AniSkip intro/outro auto-skip for mpv (03 §9). Community OP/ED timestamps
//! keyed on MAL id: fetch intervals, drop skip.lua into cache, hand mpv
//! `--script` + `--script-opts`. Best-effort: any failure (no MAL id,
//! network down, empty data, unwritable cache) collapses to plain play with
//! no error shown. Network runs on the playback worker, never the UI thread.
//!
//! The freeze falls back to a Jikan title lookup when enrichment carries no
//! MAL id; sabigoku ships without a jikan module (deliberate lean, ROD-439):
//! AniList supplies `mal_id` on the canonical path, absent id = no skip.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::player::SkipScript;
use crate::providers::http::{Accept, HttpClient, Method, Request};

const ENDPOINT: &str = "https://api.aniskip.com/v2/skip-times";
const UA: &str = "sabigoku";

/// Which segments to auto-skip. Mirrors `config.skip_mode` (06 §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipMode {
    None,
    Intro,
    Outro,
    Both,
}

impl SkipMode {
    /// Unrecognized config string → `Both`. A typo must not silently
    /// disable skip (03 §9).
    pub fn parse(s: &str) -> SkipMode {
        match s {
            "none" => SkipMode::None,
            "intro" => SkipMode::Intro,
            "outro" => SkipMode::Outro,
            _ => SkipMode::Both,
        }
    }
}

/// Skip window for one episode. None = AniSkip had no timestamp for that
/// segment.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct SkipTimes {
    pub op: Option<(f64, f64)>,
    pub ed: Option<(f64, f64)>,
}

#[derive(Deserialize, Default)]
struct Interval {
    #[serde(rename = "startTime", default)]
    start_time: f64,
    #[serde(rename = "endTime", default)]
    end_time: f64,
}

#[derive(Deserialize)]
struct ResultItem {
    #[serde(default)]
    interval: Interval,
    #[serde(rename = "skipType", default)]
    skip_type: String,
}

#[derive(Deserialize)]
struct Resp {
    #[serde(default)]
    results: Vec<ResultItem>,
}

/// Min OP/ED length. The script announces then seeks 0.4s later; a
/// sub-second window can be overrun by natural playback and turn the
/// absolute seek into a rewind.
const MIN_INTERVAL_SECS: f64 = 1.0;

fn valid_interval(iv: &Interval) -> bool {
    iv.start_time >= 0.0 && iv.end_time - iv.start_time >= MIN_INTERVAL_SECS
}

/// First op/ed wins. Degenerate intervals are dropped (the community DB has
/// them); a 0→0 window would make mpv seek-loop at the start.
fn times_from_body(body: &[u8]) -> SkipTimes {
    let Ok(resp) = serde_json::from_slice::<Resp>(body) else {
        return SkipTimes::default();
    };
    let mut t = SkipTimes::default();
    for r in &resp.results {
        if !valid_interval(&r.interval) {
            continue;
        }
        let window = (r.interval.start_time, r.interval.end_time);
        match r.skip_type.as_str() {
            "op" if t.op.is_none() => t.op = Some(window),
            "ed" if t.ed.is_none() => t.ed = Some(window),
            _ => {}
        }
    }
    t
}

/// Fetch OP/ED for `(mal_id, episode)`. Never errors: every failure returns
/// empty times.
fn fetch(mal_id: i64, episode: u32) -> SkipTimes {
    let Ok(client) = HttpClient::new() else {
        return SkipTimes::default();
    };
    let url = format!("{ENDPOINT}/{mal_id}/{episode}?types[]=op&types[]=ed&episodeLength=0");
    let req = Request {
        method: Method::Get,
        url: &url,
        payload: None,
        user_agent: UA,
        extra_headers: &[("Accept", "application/json")],
        accept: Accept::OkOnly,
        deadline: None,
    };
    match client.fetch(&req) {
        Ok(body) => times_from_body(&body),
        Err(_) => SkipTimes::default(),
    }
}

/// `--script-opts` for `times` under `mode`, or None (mode `none` / no
/// relevant interval). Missing segments emit as `-1` (Lua: disabled).
fn build_opts(t: SkipTimes, mode: SkipMode) -> Option<String> {
    if mode == SkipMode::None {
        return None;
    }
    let want_op = matches!(mode, SkipMode::Intro | SkipMode::Both);
    let want_ed = matches!(mode, SkipMode::Outro | SkipMode::Both);
    let op = t.op.filter(|_| want_op);
    let ed = t.ed.filter(|_| want_ed);
    if op.is_none() && ed.is_none() {
        return None;
    }
    let (op_start, op_end) = op.unwrap_or((-1.0, -1.0));
    let (ed_start, ed_end) = ed.unwrap_or((-1.0, -1.0));
    let mode = match mode {
        SkipMode::Intro => "intro",
        SkipMode::Outro => "outro",
        _ => "both",
    };
    Some(format!(
        "aniskip-op_start={op_start},aniskip-op_end={op_end},aniskip-ed_start={ed_start},aniskip-ed_end={ed_end},aniskip-mode={mode}"
    ))
}

/// Integer from the provider label; else the 1-based ordinal. Non-integer
/// labels (OVA, "1.5") or gapped numbering can disagree with the real
/// episode number: a wrong AniSkip lookup at worst, which yields no skip.
pub fn episode_number(raw: &str, ordinal: u32) -> u32 {
    raw.trim().parse().unwrap_or(ordinal)
}

/// Everything mpv needs to auto-skip, or None for plain play. Runs network:
/// call on the playback worker only. Mode `none` returns before any I/O.
pub fn prepare(
    mal_id: Option<i64>,
    episode: u32,
    mode: SkipMode,
    cache_dir: &Path,
) -> Option<SkipScript> {
    if mode == SkipMode::None {
        return None;
    }
    let mal_id = mal_id?;
    let opts = build_opts(fetch(mal_id, episode), mode)?;
    let path = ensure_script(cache_dir)?;
    Some(SkipScript { path, opts })
}

/// mpv user-script: OP/ED from `--script-opts`; `-1` disables a segment;
/// `mode` gates intro/outro. Announce before the seek so the jump reads
/// intentional, not a glitch. `skipped` flags debounce the high-frequency
/// time-pos observer; `file-loaded` resets them if episodes chain in one
/// mpv.
const LUA_SCRIPT: &str = r#"local opts = require("mp.options")
local o = { op_start = -1, op_end = -1, ed_start = -1, ed_end = -1, mode = "both" }
opts.read_options(o, "aniskip")

local skipped = { op = false, ed = false }

local function skip_section(target, label)
    mp.osd_message(label, 2.0)
    mp.add_timeout(0.4, function()
        mp.commandv("seek", target, "absolute")
    end)
end

mp.observe_property("time-pos", "number", function(_, pos)
    if not pos then return end
    if (o.mode == "intro" or o.mode == "both") and o.op_start >= 0
        and not skipped.op and pos >= o.op_start and pos < o.op_end then
        skipped.op = true
        skip_section(o.op_end, "Skipping intro...")
    end
    if (o.mode == "outro" or o.mode == "both") and o.ed_start >= 0
        and not skipped.ed and pos >= o.ed_start and pos < o.ed_end then
        skipped.ed = true
        skip_section(o.ed_end, "Skipping ending...")
    end
end)

mp.register_event("file-loaded", function()
    skipped.op = false
    skipped.ed = false
end)
"#;

/// Write skip.lua into the cache dir; absolute path back. Always rewrite:
/// the script evolves across versions and ~900B once per play is cheaper
/// than staleness tracking. Failures collapse to None (plain play).
fn ensure_script(cache_dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(cache_dir).ok()?;
    let path = cache_dir.join("skip.lua");
    std::fs::write(&path, LUA_SCRIPT).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_mode_parses_with_typo_fallback_to_both() {
        assert_eq!(SkipMode::parse("none"), SkipMode::None);
        assert_eq!(SkipMode::parse("intro"), SkipMode::Intro);
        assert_eq!(SkipMode::parse("outro"), SkipMode::Outro);
        assert_eq!(SkipMode::parse("both"), SkipMode::Both);
        assert_eq!(SkipMode::parse("bth"), SkipMode::Both);
        assert_eq!(SkipMode::parse(""), SkipMode::Both);
    }

    #[test]
    fn times_parse_real_api_shape_first_of_each_wins() {
        let body = br#"{"found":true,"results":[
            {"interval":{"startTime":3.221,"endTime":93.221},"skipType":"op","skipId":"x"},
            {"interval":{"startTime":1417.135,"endTime":1507.135},"skipType":"ed","skipId":"y"},
            {"interval":{"startTime":5,"endTime":9},"skipType":"op","skipId":"z"}]}"#;
        let t = times_from_body(body);
        assert_eq!(t.op, Some((3.221, 93.221)));
        assert_eq!(t.ed, Some((1417.135, 1507.135)));
    }

    #[test]
    fn times_leave_missing_segments_none_and_survive_garbage() {
        let t = times_from_body(
            br#"{"results":[{"interval":{"startTime":12.5,"endTime":84.3},"skipType":"op"}]}"#,
        );
        assert_eq!(t.ed, None);
        assert_eq!(times_from_body(b"not json"), SkipTimes::default());
        assert_eq!(times_from_body(b"{}"), SkipTimes::default());
    }

    #[test]
    fn times_drop_degenerate_and_subsecond_intervals() {
        let body = br#"{"results":[
            {"interval":{"startTime":0,"endTime":0},"skipType":"op"},
            {"interval":{"startTime":100,"endTime":50},"skipType":"ed"},
            {"interval":{"startTime":10,"endTime":10.5},"skipType":"op"}]}"#;
        assert_eq!(times_from_body(body), SkipTimes::default());
    }

    #[test]
    fn build_opts_emits_all_keys_gated_by_mode() {
        let both = SkipTimes {
            op: Some((12.5, 84.3)),
            ed: Some((1340.0, 1412.0)),
        };
        assert_eq!(
            build_opts(both, SkipMode::Both).unwrap(),
            "aniskip-op_start=12.5,aniskip-op_end=84.3,aniskip-ed_start=1340,aniskip-ed_end=1412,aniskip-mode=both"
        );
        let op_only = SkipTimes {
            op: Some((12.5, 84.3)),
            ed: None,
        };
        assert_eq!(
            build_opts(op_only, SkipMode::Intro).unwrap(),
            "aniskip-op_start=12.5,aniskip-op_end=84.3,aniskip-ed_start=-1,aniskip-ed_end=-1,aniskip-mode=intro"
        );
    }

    #[test]
    fn build_opts_none_when_no_relevant_interval_or_disabled() {
        let op_only = SkipTimes {
            op: Some((12.5, 84.3)),
            ed: None,
        };
        assert_eq!(build_opts(op_only, SkipMode::Outro), None);
        assert_eq!(build_opts(SkipTimes::default(), SkipMode::Both), None);
        assert_eq!(build_opts(op_only, SkipMode::None), None);
    }

    #[test]
    fn episode_number_parses_label_falls_back_to_ordinal() {
        assert_eq!(episode_number("12", 5), 12);
        assert_eq!(episode_number(" 1 ", 9), 1);
        assert_eq!(episode_number("12.5", 7), 7);
        assert_eq!(episode_number("OVA", 3), 3);
    }

    #[test]
    fn prepare_early_outs_never_touch_the_network() {
        let dir = std::env::temp_dir().join("sabigoku-aniskip-test");
        assert!(prepare(Some(1), 1, SkipMode::None, &dir).is_none());
        assert!(prepare(None, 1, SkipMode::Both, &dir).is_none());
    }

    #[test]
    fn ensure_script_writes_and_rewrites() {
        let dir = std::env::temp_dir().join("sabigoku-aniskip-script-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = ensure_script(&dir).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(r#"read_options(o, "aniskip")"#));
        std::fs::write(&path, "stale").unwrap();
        ensure_script(&dir).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("file-loaded"),
            "always rewritten, never trusted stale"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
