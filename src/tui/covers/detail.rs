//! Detail cover state machine (05 §12, zigoku ROD-110/160): one selection's
//! poster art. Keys are `anilist_id` plus url per the 05 §12 port note, never
//! provider ids. Worker spawn and event wiring belong to the caller; this is
//! the policy. zigoku's `halfBlockFit` does not port: ratatui-image owns
//! halfblock fitting.

use std::time::Instant;

use super::RETRY_COOLDOWN;

/// Outcome of reconciling held state with the current selection (05 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Held art belongs to a different id and the target has no art.
    Clear,
    /// Same-id same-url failure still inside cooldown.
    Suppress,
    /// Already loading or holding pixels for this id.
    UpToDate,
    /// Fresh fetch; supersedes any failure record.
    Fetch,
}

#[derive(Debug)]
struct Failure {
    id: i64,
    url: Option<String>,
    at: Instant,
}

#[derive(Debug, Default)]
pub struct CoverState {
    /// Flag only; the pixels themselves live in the caches and the protocol
    /// pool. A copy here would sit outside every byte cap (04 §7.3 RAM rail).
    has_pixels: bool,
    for_id: Option<i64>,
    loading: bool,
    failed: Option<Failure>,
    /// Attributes a failure to the url actually fetched when the selection
    /// moves mid-flight.
    inflight_url: Option<String>,
}

impl CoverState {
    pub fn has_pixels(&self) -> bool {
        self.has_pixels
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn for_id(&self) -> Option<i64> {
        self.for_id
    }

    /// The 05 §12 decision table, pure. Order is law: a live cover wins over
    /// a stale failure record, so up-to-date is decided before suppress.
    pub fn decide(&self, target_id: Option<i64>, target_url: Option<&str>, now: Instant) -> Action {
        let Some(target_id) = target_id else {
            return Action::None;
        };
        let Some(target_url) = target_url else {
            // Target has no art: clear only if held state is another id's.
            return match self.for_id {
                Some(id) if id != target_id => Action::Clear,
                _ => Action::None,
            };
        };
        if self.for_id == Some(target_id) && (self.loading || self.has_pixels) {
            return Action::UpToDate;
        }
        // Failure records survive navigation; only cooldown expiry, a url
        // change, or a successful fetch end the suppression.
        if let Some(failure) = &self.failed
            && failure.id == target_id
            && failure.url.as_deref() == Some(target_url)
            && now.saturating_duration_since(failure.at) < RETRY_COOLDOWN
        {
            return Action::Suppress;
        }
        Action::Fetch
    }

    /// The `Fetch` transition; the caller spawns the worker. On spawn failure
    /// the caller must `clear` so no spinner strands.
    pub fn begin_fetch(&mut self, id: i64, url: &str) {
        self.failed = None;
        self.clear();
        self.for_id = Some(id);
        self.inflight_url = Some(url.to_string());
        self.loading = true;
    }

    /// Worker success. False means stale (wrong id): state untouched, pixels
    /// dropped by the caller (04 §6). True commits the caller to installing
    /// the image in the render store; this flag is the only record of it.
    pub fn on_done(&mut self, for_id: i64) -> bool {
        if self.for_id != Some(for_id) {
            return false;
        }
        self.loading = false;
        self.failed = None;
        self.inflight_url = None;
        self.has_pixels = true;
        true
    }

    /// Worker failure. False means stale drop. Records the id+url cooldown,
    /// then clears held art so a later sync can retry (05 §12: error clears).
    pub fn on_error(&mut self, for_id: i64, now: Instant) -> bool {
        if self.for_id != Some(for_id) {
            return false;
        }
        self.failed = Some(Failure {
            id: for_id,
            url: self.inflight_url.take(),
            at: now,
        });
        self.clear();
        true
    }

    /// Drop held art and in-flight attribution; the failure record survives
    /// (it has its own lifecycle, see `decide`).
    pub fn clear(&mut self) {
        self.has_pixels = false;
        self.for_id = None;
        self.inflight_url = None;
        self.loading = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const URL: &str = "https://cdn.example/a.png";

    #[test]
    fn no_target_is_none_regardless_of_state() {
        let now = Instant::now();
        let mut state = CoverState::default();
        assert_eq!(state.decide(None, None, now), Action::None);
        state.begin_fetch(7, URL);
        assert_eq!(state.decide(None, Some(URL), now), Action::None);
    }

    #[test]
    fn urlless_target_clears_only_foreign_art() {
        let now = Instant::now();
        let mut state = CoverState::default();
        assert_eq!(state.decide(Some(7), None, now), Action::None);
        state.begin_fetch(7, URL);
        assert!(state.on_done(7));
        assert_eq!(state.decide(Some(7), None, now), Action::None);
        assert_eq!(state.decide(Some(8), None, now), Action::Clear);
    }

    #[test]
    fn loading_or_pixels_for_target_is_up_to_date() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert_eq!(state.decide(Some(7), Some(URL), now), Action::UpToDate);
        assert!(state.on_done(7));
        assert_eq!(state.decide(Some(7), Some(URL), now), Action::UpToDate);
        assert_eq!(state.decide(Some(8), Some(URL), now), Action::Fetch);
    }

    #[test]
    fn failure_suppresses_same_id_and_url_within_cooldown() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert!(state.on_error(7, now));
        assert!(!state.is_loading(), "error must clear loading for retry");
        assert_eq!(state.decide(Some(7), Some(URL), now), Action::Suppress);
        // Url change recovers immediately; another id never suppresses.
        assert_eq!(
            state.decide(Some(7), Some("https://cdn.example/b.png"), now),
            Action::Fetch
        );
        assert_eq!(state.decide(Some(8), Some(URL), now), Action::Fetch);
    }

    #[test]
    fn cooldown_expiry_readmits_at_the_boundary() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert!(state.on_error(7, now));
        let just_inside = now + RETRY_COOLDOWN - Duration::from_millis(1);
        assert_eq!(
            state.decide(Some(7), Some(URL), just_inside),
            Action::Suppress
        );
        assert_eq!(
            state.decide(Some(7), Some(URL), now + RETRY_COOLDOWN),
            Action::Fetch
        );
    }

    #[test]
    fn failure_record_survives_navigation_but_not_success() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert!(state.on_error(7, now));
        // Navigate away and back: the record still suppresses.
        state.clear();
        assert_eq!(state.decide(Some(7), Some(URL), now), Action::Suppress);
        // A successful fetch supersedes it (live pixels win, 05 §12).
        state.begin_fetch(7, URL);
        assert!(state.on_done(7));
        state.clear();
        assert_eq!(state.decide(Some(7), Some(URL), now), Action::Fetch);
    }

    #[test]
    fn begin_fetch_supersedes_the_failure_record() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert!(state.on_error(7, now));
        state.begin_fetch(7, URL);
        state.clear();
        assert_eq!(state.decide(Some(7), Some(URL), now), Action::Fetch);
    }

    #[test]
    fn failure_record_never_leaks_into_the_urlless_branch() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert!(state.on_error(7, now));
        // No target art: the record must not suppress or clear anything.
        assert_eq!(state.decide(Some(7), None, now), Action::None);
        assert_eq!(state.decide(Some(8), None, now), Action::None);
    }

    #[test]
    fn stale_done_and_error_are_dropped_untouched() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert!(!state.on_done(9), "stale cover_done discarded");
        assert!(state.is_loading());
        assert!(!state.has_pixels());
        assert!(!state.on_error(9, now), "stale cover_error discarded");
        assert!(state.is_loading());
        assert_eq!(state.for_id(), Some(7));
    }

    #[test]
    fn error_attributes_the_inflight_url_not_the_current_one() {
        let now = Instant::now();
        let mut state = CoverState::default();
        state.begin_fetch(7, URL);
        assert!(state.on_error(7, now));
        // The record names URL, so a different url for the same id fetches.
        assert_eq!(
            state.decide(Some(7), Some("https://cdn.example/moved.png"), now),
            Action::Fetch
        );
        assert_eq!(state.decide(Some(7), Some(URL), now), Action::Suppress);
    }
}
