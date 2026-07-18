//! Pure domain types (02 §4). Imports nothing; no I/O ever (01 §5).
//! Semantics CLONE zigoku domain.zig @ freeze unless a 02 note says otherwise.

use std::cmp::Ordering;

/// Cap for grid allocs driven by untrusted AniList counts (ROD-359 / ROD-92).
pub const MAX_EPISODE_HINT: u32 = 10_000;

/// Resume thresholds (02 §4b table is the single authority). fully_watched past
/// 0.95; natural end (0.80) ratchets progress but does not mark fully_watched.
pub const WATCHED_RATIO: f64 = 0.95;
pub const NATURAL_END_RATIO: f64 = 0.80;

/// Scheme check only, no site knowledge (ROD-267). Case-sensitive on purpose:
/// it must agree with the store's `GLOB 'http*'` cover guard, and GLOB is
/// case-sensitive.
pub fn is_absolute_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// Sub/dub track. Rides search, episode lists, progress rows, and resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Translation {
    Sub,
    Dub,
}

impl Translation {
    pub fn as_str(self) -> &'static str {
        match self {
            Translation::Sub => "sub",
            Translation::Dub => "dub",
        }
    }

    pub fn parse(s: &str) -> Option<Translation> {
        match s {
            "sub" => Some(Translation::Sub),
            "dub" => Some(Translation::Dub),
            _ => None,
        }
    }
}

/// Watchlist state. Persisted as the lowercase `as_str` form in
/// `show.list_status` (no SQL CHECK; the column trusts this enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ListStatus {
    #[default]
    Planning,
    Watching,
    Paused,
    Completed,
    Dropped,
}

impl ListStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ListStatus::Planning => "planning",
            ListStatus::Watching => "watching",
            ListStatus::Paused => "paused",
            ListStatus::Completed => "completed",
            ListStatus::Dropped => "dropped",
        }
    }

    /// Unknown or empty maps to planning: never invent an active state.
    pub fn parse(s: &str) -> ListStatus {
        match s {
            "watching" => ListStatus::Watching,
            "paused" => ListStatus::Paused,
            "completed" => ListStatus::Completed,
            "dropped" => ListStatus::Dropped,
            _ => ListStatus::Planning,
        }
    }

    /// Auto-status after a play (ROD-139). `still_airing`: total may be
    /// aired-so-far, so catching up to the latest ep must not complete
    /// (ROD-296). Manual pause/drop/force goes through setListStatus instead.
    pub fn after_play(self, progress: u32, total: Option<u32>, still_airing: bool) -> ListStatus {
        if self == ListStatus::Completed {
            return ListStatus::Completed;
        }
        if still_airing {
            return ListStatus::Watching;
        }
        if let Some(t) = total
            && t > 0
            && progress >= t
        {
            return ListStatus::Completed;
        }
        ListStatus::Watching
    }

    /// History group rank; lower = higher in the list. planning before paused
    /// by design (ROD-139).
    pub fn group_rank(self) -> u8 {
        match self {
            ListStatus::Watching => 0,
            ListStatus::Planning => 1,
            ListStatus::Paused => 2,
            ListStatus::Completed => 3,
            ListStatus::Dropped => 4,
        }
    }

    /// Top-to-bottom History group order, the `group_rank` inverse.
    pub const GROUP_ORDER: [ListStatus; 5] = [
        ListStatus::Watching,
        ListStatus::Planning,
        ListStatus::Paused,
        ListStatus::Completed,
        ListStatus::Dropped,
    ];
}

/// True when total_episodes may be aired-so-far rather than the finale
/// (ROD-296); `after_play` gates auto-complete on this.
///
/// DENYLIST: only FINISHED/CANCELLED are settled. Everything else (RELEASING,
/// HIATUS, NOT_YET_RELEASED, None, unknown) is still airing for completion.
/// Gate on status, not next_airing_episode: an upsert cannot null that back out.
pub fn is_still_airing(status: Option<&str>) -> bool {
    let Some(s) = status else { return true };
    !(s.eq_ignore_ascii_case("FINISHED") || s.eq_ignore_ascii_case("CANCELLED"))
}

/// Episode-grid count from canonical data (ROD-359). While airing:
/// `next_airing_episode - 1` floor (over-list ok; under-list hides eps), except
/// next == 1 means nothing aired yet, which is None, not 0. Settled: total.
/// Every branch clamps to MAX_EPISODE_HINT.
pub fn expected_episode_count(
    status: Option<&str>,
    total_episodes: Option<u32>,
    next_airing_episode: Option<u32>,
) -> Option<u32> {
    let raw = if is_still_airing(status) {
        match next_airing_episode {
            Some(next) if next <= 1 => return None,
            Some(next) => {
                let aired = next - 1;
                Some(total_episodes.map_or(aired, |t| aired.min(t)))
            }
            None => total_episodes,
        }
    } else {
        total_episodes
    };
    raw.map(|n| n.min(MAX_EPISODE_HINT))
}

/// Stream quality pref (ROD-152). best/worst pick the extremum; rungs are a
/// pixel cap. Providers with no variants ignore this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Best,
    P1080,
    P720,
    P480,
    Worst,
}

impl Quality {
    /// Config string form; unknown falls to best (safe default).
    pub fn parse(s: &str) -> Quality {
        match s {
            "worst" => Quality::Worst,
            "480" => Quality::P480,
            "720" => Quality::P720,
            "1080" => Quality::P1080,
            _ => Quality::Best,
        }
    }

    /// Pixel ceiling, or None for best/worst.
    pub fn cap(self) -> Option<u32> {
        match self {
            Quality::P1080 => Some(1080),
            Quality::P720 => Some(720),
            Quality::P480 => Some(480),
            Quality::Best | Quality::Worst => None,
        }
    }
}

/// Broadcast cour. AniList spellings fold here; render maps to kanji (ROD-141).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Season {
    Winter,
    Spring,
    Summer,
    Fall,
}

impl Season {
    /// Case-insensitive; autumn folds to fall. Unknown is None.
    pub fn parse(s: &str) -> Option<Season> {
        if s.eq_ignore_ascii_case("winter") {
            Some(Season::Winter)
        } else if s.eq_ignore_ascii_case("spring") {
            Some(Season::Spring)
        } else if s.eq_ignore_ascii_case("summer") {
            Some(Season::Summer)
        } else if s.eq_ignore_ascii_case("fall") || s.eq_ignore_ascii_case("autumn") {
            Some(Season::Fall)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Season::Winter => "winter",
            Season::Spring => "spring",
            Season::Summer => "summer",
            Season::Fall => "fall",
        }
    }

    /// DESIGN §2.3 season chip (ROD-141).
    pub fn kanji(self) -> &'static str {
        match self {
            Season::Winter => "冬",
            Season::Spring => "春",
            Season::Summer => "夏",
            Season::Fall => "秋",
        }
    }

    /// Month 1..=12 to AniList cour (ROD-186). December is next-year Winter;
    /// the year roll is the caller's. Out-of-range folds to winter.
    pub fn from_month(month: u32) -> Season {
        match month {
            3..=5 => Season::Spring,
            6..=8 => Season::Summer,
            9..=11 => Season::Fall,
            _ => Season::Winter,
        }
    }
}

/// Calendar date at available precision; year always set when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: u32,
    pub month: Option<u32>,
    pub day: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cour {
    pub season: Season,
    pub year: u32,
}

/// Cour at a wall-clock instant; anchors the This Season discover axis.
/// December carries the from_month year roll into next-year winter (ROD-186).
pub fn current_cour(unix_secs: i64) -> Cour {
    let days = unix_secs.max(0).div_euclid(86_400);
    let (year, month, _) = civil_from_days(days);
    let year = if month == 12 { year + 1 } else { year };
    Cour {
        season: Season::from_month(month),
        year,
    }
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian
/// (Howard Hinnant's civil_from_days). Valid for any date this app can see.
fn civil_from_days(z: i64) -> (u32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as u32, month as u32, day as u32)
}

/// Primary title form (ROD-205). No separate Auto: english already falls back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleLanguage {
    Romaji,
    English,
    Native,
}

impl TitleLanguage {
    /// Config string form; unknown falls to romaji (the universal backstop).
    pub fn parse(s: &str) -> TitleLanguage {
        match s {
            "english" => TitleLanguage::English,
            "native" => TitleLanguage::Native,
            _ => TitleLanguage::Romaji,
        }
    }
}

/// Non-empty present, else None: a blank string never wins the fallback chain.
fn present(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.is_empty())
}

/// Primary title under pref (ROD-205, DESIGN §9.1a); romaji is the universal
/// backstop. Empty romaji is still returned last; render may show a placeholder.
pub fn preferred_title<'a>(
    romaji: &'a str,
    english: Option<&'a str>,
    native: Option<&'a str>,
    pref: TitleLanguage,
) -> &'a str {
    let rom = present(Some(romaji));
    match pref {
        TitleLanguage::Romaji => rom.or(present(english)).or(present(native)),
        TitleLanguage::English => present(english).or(rom).or(present(native)),
        TitleLanguage::Native => present(native).or(rom).or(present(english)),
    }
    .unwrap_or(romaji)
}

/// Sort key for a raw episode label ("1", "1.5", "SP1"): leading digits and
/// dots parse as f64; non-numeric labels go to +inf so specials sort after the
/// numbered run. Store recompute and grid ordering both key on this (02 §4b).
pub fn episode_sort_key(label: &str) -> f64 {
    let end = label
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(label.len());
    label[..end].parse().unwrap_or(f64::INFINITY)
}

/// Total order over labels. Ties (all specials are +inf) compare Equal, so a
/// stable sort keeps their incoming order, matching the grid.
pub fn episode_label_cmp(a: &str, b: &str) -> Ordering {
    episode_sort_key(a).total_cmp(&episode_sort_key(b))
}

/// The AniList-shaped enrichment fieldset, shared verbatim by the library
/// `show` row and `catalog_cache` (02 §3.3: column parity is intentional so
/// promote-to-library is a straight copy). No user state lives here, ever.
///
/// `score` is AniList community averageScore (0..=100); the user's own 0..=10
/// rating is `Show::user_rating`. Never conflate (02 §4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Enrichment {
    pub anilist_id: i64,
    /// Secondary bridge id (AniSkip); non-unique in the wild, never a key.
    pub mal_id: Option<i64>,
    pub title_romaji: String,
    pub title_english: Option<String>,
    pub title_native: Option<String>,
    pub cover_url: Option<String>,
    pub total_episodes: Option<u32>,
    pub duration_minutes: Option<u32>,
    pub year: Option<u32>,
    pub season: Option<Season>,
    /// Raw AniList media status; `is_still_airing` consumes it un-normalized.
    pub status: Option<String>,
    pub description: Option<String>,
    pub score: Option<u32>,
    pub kind: Option<String>,
    pub start_date: Option<Date>,
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub source_material: Option<String>,
    pub rank: Option<u32>,
    pub rank_type: Option<String>,
    pub rank_year: Option<u32>,
    pub next_airing_at: Option<i64>,
    pub next_airing_episode: Option<u32>,
    pub country: Option<String>,
}

/// One library show: enrichment plus the user state that must never be
/// clobbered by it (02 §5). `library_added_at` None = identity row only,
/// not in History (02 §3.7).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Show {
    pub enrichment: Enrichment,
    pub enrichment_fetched_at: Option<i64>,
    pub enrichment_fieldset_version: Option<u32>,
    pub list_status: ListStatus,
    pub user_rating: Option<f64>,
    pub notes: Option<String>,
    pub play_count: u32,
    /// Unclamped on purpose: overshoot past total still counts as completed;
    /// the 14/2 clamp is render-time only (02 §4b).
    pub progress: u32,
    pub library_added_at: Option<i64>,
    pub last_watched_at: Option<i64>,
    pub synced_status: Option<ListStatus>,
    pub synced_progress: Option<u32>,
}

/// Playable stream for mpv. Every field is provider-derived and untrusted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamLink {
    pub url: String,
    pub resolution: Option<u32>,
    pub referer: Option<String>,
    /// Browser-shaped UA for CDN bot scoring (ROD-309); None = player default.
    pub user_agent: Option<String>,
    /// HLS segments cloaked as .jpg (ROD-301); player must relax its demuxer gate.
    pub cloaked_segments: bool,
    /// External WebVTT softsub (ROD-354); None if hardsub or none.
    pub sub_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_play_completed_sticks() {
        assert_eq!(
            ListStatus::Completed.after_play(1, Some(12), false),
            ListStatus::Completed
        );
        assert_eq!(
            ListStatus::Completed.after_play(0, None, true),
            ListStatus::Completed
        );
    }

    #[test]
    fn after_play_still_airing_never_auto_completes() {
        assert_eq!(
            ListStatus::Watching.after_play(24, Some(24), true),
            ListStatus::Watching
        );
    }

    #[test]
    fn after_play_completes_only_at_known_positive_total() {
        assert_eq!(
            ListStatus::Watching.after_play(12, Some(12), false),
            ListStatus::Completed
        );
        assert_eq!(
            ListStatus::Planning.after_play(13, Some(12), false),
            ListStatus::Completed
        );
        assert_eq!(
            ListStatus::Watching.after_play(11, Some(12), false),
            ListStatus::Watching
        );
        assert_eq!(
            ListStatus::Watching.after_play(5, Some(0), false),
            ListStatus::Watching
        );
        assert_eq!(
            ListStatus::Watching.after_play(5, None, false),
            ListStatus::Watching
        );
    }

    #[test]
    fn still_airing_settles_only_on_finished_or_cancelled() {
        assert!(!is_still_airing(Some("FINISHED")));
        assert!(!is_still_airing(Some("finished")));
        assert!(!is_still_airing(Some("CANCELLED")));
        assert!(is_still_airing(Some("RELEASING")));
        assert!(is_still_airing(Some("HIATUS")));
        assert!(is_still_airing(Some("NOT_YET_RELEASED")));
        assert!(is_still_airing(Some("whatever")));
        assert!(is_still_airing(None));
    }

    #[test]
    fn expected_episode_count_matches_rod_359_table() {
        assert_eq!(
            expected_episode_count(Some("FINISHED"), Some(28), None),
            Some(28)
        );
        assert_eq!(
            expected_episode_count(Some("RELEASING"), Some(24), Some(14)),
            Some(13)
        );
        assert_eq!(
            expected_episode_count(Some("RELEASING"), Some(24), Some(1)),
            None
        );
        assert_eq!(
            expected_episode_count(Some("RELEASING"), None, Some(14)),
            Some(13)
        );
        assert_eq!(
            expected_episode_count(Some("RELEASING"), Some(24), None),
            Some(24)
        );
        assert_eq!(expected_episode_count(None, None, None), None);
        // Aired floor never exceeds a known total.
        assert_eq!(
            expected_episode_count(Some("RELEASING"), Some(12), Some(50)),
            Some(12)
        );
    }

    #[test]
    fn expected_episode_count_clamps_untrusted_counts() {
        assert_eq!(
            expected_episode_count(Some("FINISHED"), Some(50_000), None),
            Some(MAX_EPISODE_HINT)
        );
        assert_eq!(
            expected_episode_count(Some("RELEASING"), None, Some(50_000)),
            Some(MAX_EPISODE_HINT)
        );
    }

    #[test]
    fn episode_sort_key_exact_values() {
        assert_eq!(episode_sort_key("1"), 1.0);
        assert_eq!(episode_sort_key("1.5"), 1.5);
        assert_eq!(episode_sort_key("13.5"), 13.5);
        assert_eq!(episode_sort_key("10"), 10.0);
        assert_eq!(episode_sort_key("01"), 1.0);
        assert_eq!(episode_sort_key("001"), 1.0);
        assert_eq!(episode_sort_key("12v2"), 12.0);
        assert_eq!(episode_sort_key("SP1"), f64::INFINITY);
        assert_eq!(episode_sort_key("OVA"), f64::INFINITY);
        assert_eq!(episode_sort_key(""), f64::INFINITY);
    }

    #[test]
    fn episode_labels_sort_numerically_with_specials_last() {
        let mut labels = ["2", "1.5", "SP1", "1", "10"];
        labels.sort_by(|a, b| episode_label_cmp(a, b));
        assert_eq!(labels, ["1", "1.5", "2", "10", "SP1"]);
    }

    #[test]
    fn preferred_title_fallback_chains() {
        let t = |p| {
            preferred_title(
                "Sousou no Frieren",
                Some("Frieren"),
                Some("葬送のフリーレン"),
                p,
            )
        };
        assert_eq!(t(TitleLanguage::Romaji), "Sousou no Frieren");
        assert_eq!(t(TitleLanguage::English), "Frieren");
        assert_eq!(t(TitleLanguage::Native), "葬送のフリーレン");

        assert_eq!(
            preferred_title("Romaji", None, None, TitleLanguage::English),
            "Romaji"
        );
        assert_eq!(
            preferred_title(
                "Romaji",
                Some(""),
                Some("ネイティブ"),
                TitleLanguage::English
            ),
            "Romaji"
        );
        // Everything empty: romaji comes back anyway for the render placeholder.
        assert_eq!(
            preferred_title("", Some(""), None, TitleLanguage::Native),
            ""
        );
    }

    #[test]
    fn list_status_round_trip_and_unknown() {
        for s in ListStatus::GROUP_ORDER {
            assert_eq!(ListStatus::parse(s.as_str()), s);
        }
        assert_eq!(ListStatus::parse("garbage"), ListStatus::Planning);
        assert_eq!(ListStatus::parse(""), ListStatus::Planning);
    }

    #[test]
    fn group_order_is_group_rank_inverse() {
        for (i, s) in ListStatus::GROUP_ORDER.iter().enumerate() {
            assert_eq!(s.group_rank() as usize, i);
        }
    }

    #[test]
    fn quality_parse_and_cap() {
        assert_eq!(Quality::parse("1080"), Quality::P1080);
        assert_eq!(Quality::parse("720"), Quality::P720);
        assert_eq!(Quality::parse("480"), Quality::P480);
        assert_eq!(Quality::parse("worst"), Quality::Worst);
        assert_eq!(Quality::parse("nonsense"), Quality::Best);
        assert_eq!(Quality::P720.cap(), Some(720));
        assert_eq!(Quality::Best.cap(), None);
        assert_eq!(Quality::Worst.cap(), None);
    }

    #[test]
    fn season_parse_kanji_and_months() {
        assert_eq!(Season::parse("FALL"), Some(Season::Fall));
        assert_eq!(Season::parse("autumn"), Some(Season::Fall));
        assert_eq!(Season::parse("nope"), None);
        assert_eq!(Season::Fall.kanji(), "秋");
        assert_eq!(Season::from_month(12), Season::Winter);
        assert_eq!(Season::from_month(1), Season::Winter);
        assert_eq!(Season::from_month(4), Season::Spring);
        assert_eq!(Season::from_month(7), Season::Summer);
        assert_eq!(Season::from_month(10), Season::Fall);
        assert_eq!(Season::from_month(0), Season::Winter);
    }

    #[test]
    fn absolute_url_is_scheme_and_case_sensitive() {
        assert!(is_absolute_url("https://x/y.png"));
        assert!(is_absolute_url("http://x/y.png"));
        assert!(!is_absolute_url("//x/y.png"));
        assert!(!is_absolute_url("images/y.png"));
        assert!(!is_absolute_url("HTTPS://x/y.png"));
    }

    #[test]
    fn translation_round_trip() {
        assert_eq!(Translation::parse("sub"), Some(Translation::Sub));
        assert_eq!(Translation::parse("dub"), Some(Translation::Dub));
        assert_eq!(Translation::parse("raw"), None);
        assert_eq!(Translation::Sub.as_str(), "sub");
    }

    #[test]
    fn current_cour_mid_year() {
        // 2026-07-18
        let c = current_cour(1_784_332_800);
        assert_eq!(
            c,
            Cour {
                season: Season::Summer,
                year: 2026
            }
        );
    }

    #[test]
    fn current_cour_december_rolls_into_next_winter() {
        // 2025-12-15
        let c = current_cour(1_765_756_800);
        assert_eq!(
            c,
            Cour {
                season: Season::Winter,
                year: 2026
            }
        );
    }

    #[test]
    fn current_cour_pre_epoch_clamps_to_1970_winter() {
        let c = current_cour(-1);
        assert_eq!(
            c,
            Cour {
                season: Season::Winter,
                year: 1970
            }
        );
    }

    #[test]
    fn current_cour_january_stays_in_its_year() {
        // 2026-01-05
        let c = current_cour(1_767_571_200);
        assert_eq!(
            c,
            Cour {
                season: Season::Winter,
                year: 2026
            }
        );
    }
}
