//! Discover grid cover coordinator (05 §7/§12, zigoku ROD-240/243): url-keyed
//! slot pool, fetch pump, LRU eviction. Slots adopt results by url with no
//! window stale-drop (04 §4.4): a result for an evicted url recreates its
//! slot. Worker spawn, events, and ratatui-image protocol state are the
//! caller's; zigoku's Kitty-id bookkeeping does not port (the protocol type
//! owns image lifecycles).

use std::time::Instant;

use super::RETRY_COOLDOWN;

/// Slot pool cap, ~two large pages. Visible or in-flight slots are never
/// evicted, so the pool can exceed this while the excess is on screen.
pub const DISCOVER_COVER_CAP: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug)]
pub struct CoverSlot {
    url: String,
    status: SlotStatus,
    /// Flag only; pixels live in the caches and the protocol pool. A copy
    /// per slot would sit outside every byte cap (04 §7.3 RAM rail).
    has_pixels: bool,
    failed_at: Option<Instant>,
    /// Last pump the url was in the window; eviction recency.
    last_seen_frame: u64,
}

impl CoverSlot {
    fn new(url: &str) -> CoverSlot {
        CoverSlot {
            url: url.to_string(),
            status: SlotStatus::Idle,
            has_pixels: false,
            failed_at: None,
            last_seen_frame: 0,
        }
    }

    pub fn status(&self) -> SlotStatus {
        self.status
    }

    pub fn has_pixels(&self) -> bool {
        self.has_pixels
    }
}

/// Embed on App by value (04 §7.4).
#[derive(Debug, Default)]
pub struct DiscoverCovers {
    slots: Vec<CoverSlot>,
    frame: u64,
}

impl DiscoverCovers {
    fn index_of(&self, url: &str) -> Option<usize> {
        // Linear scan; the pool stays within a few dozen slots.
        self.slots.iter().position(|s| s.url == url)
    }

    pub fn get(&self, url: &str) -> Option<&CoverSlot> {
        self.index_of(url).map(|i| &self.slots[i])
    }

    fn ensure_slot(&mut self, url: &str) -> &mut CoverSlot {
        match self.index_of(url) {
            Some(i) => &mut self.slots[i],
            None => {
                self.slots.push(CoverSlot::new(url));
                self.slots.last_mut().unwrap()
            }
        }
    }

    /// Adopt a landed cover for `url`, wherever the grid moved meanwhile.
    /// The caller installs the image in the render store; this is the record.
    pub fn adopt(&mut self, url: &str) {
        let slot = self.ensure_slot(url);
        slot.has_pixels = true;
        slot.status = SlotStatus::Ready;
        slot.failed_at = None;
    }

    /// Failure cooldown per url; the pump re-admits after `RETRY_COOLDOWN`.
    pub fn note_failure(&mut self, url: &str, now: Instant) {
        let slot = self.ensure_slot(url);
        slot.status = SlotStatus::Failed;
        slot.failed_at = Some(now);
    }

    /// Spawn-failure path: a slot the pump marked but no worker serves must
    /// not wedge in `Loading`.
    pub fn reset_loading(&mut self, url: &str) {
        if let Some(i) = self.index_of(url)
            && self.slots[i].status == SlotStatus::Loading
        {
            self.slots[i].status = SlotStatus::Idle;
        }
    }

    pub fn evict(&mut self, url: &str) {
        if let Some(i) = self.index_of(url) {
            self.slots.swap_remove(i);
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// One pump pass (04 §7.4, 05 §7): stamp recency for the windowed urls,
    /// evict LRU slots past the pool cap (never visible, never in-flight),
    /// then pick fetches in window order, at most `cap - busy` so in-flight
    /// work keeps its room; `busy` can exceed `cap` after a live decrease.
    /// Every returned url is already marked `Loading`: the caller must spawn
    /// its worker or call `reset_loading`.
    pub fn pump(&mut self, window: &[&str], now: Instant, cap: usize, busy: usize) -> Vec<String> {
        self.frame += 1;
        let frame = self.frame;
        for url in window {
            if let Some(i) = self.index_of(url) {
                self.slots[i].last_seen_frame = frame;
            }
        }
        self.evict_past_cap(window);
        if busy >= cap {
            return Vec::new();
        }
        let mut budget = cap - busy;
        let mut chosen = Vec::new();
        for url in window {
            if budget == 0 {
                break;
            }
            if !self.needs_fetch(url, now) {
                continue;
            }
            // Marking here makes a duplicate url in the window single-flight.
            self.ensure_slot(url).status = SlotStatus::Loading;
            chosen.push((*url).to_string());
            budget -= 1;
        }
        chosen
    }

    /// Missing, or present without pixels, not in flight, not cooling.
    fn needs_fetch(&self, url: &str, now: Instant) -> bool {
        let Some(slot) = self.get(url) else {
            return true;
        };
        if slot.has_pixels || slot.status == SlotStatus::Loading {
            return false;
        }
        match slot.failed_at {
            Some(at) => now.saturating_duration_since(at) >= RETRY_COOLDOWN,
            None => true,
        }
    }

    /// Farthest-from-viewport first (oldest pump frame). Visible and loading
    /// slots are the floor: the pool only sheds below the cap when the
    /// excess is off screen.
    fn evict_past_cap(&mut self, window: &[&str]) {
        if self.slots.len() <= DISCOVER_COVER_CAP {
            return;
        }
        let over = self.slots.len() - DISCOVER_COVER_CAP;
        let mut candidates: Vec<(u64, String)> = self
            .slots
            .iter()
            .filter(|s| s.status != SlotStatus::Loading && !window.contains(&s.url.as_str()))
            .map(|s| (s.last_seen_frame, s.url.clone()))
            .collect();
        candidates.sort();
        for (_, url) in candidates.into_iter().take(over) {
            self.evict(&url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// urls u0..uN as owned strings; tests borrow windows from this.
    fn urls(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("https://img/{i}.png")).collect()
    }

    fn window(urls: &[String]) -> Vec<&str> {
        urls.iter().map(String::as_str).collect()
    }

    #[test]
    fn pump_fetches_up_to_cap_minus_busy_in_window_order() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let u = urls(6);
        let chosen = dc.pump(&window(&u), now, 4, 2);
        assert_eq!(chosen, vec![u[0].clone(), u[1].clone()]);
        for url in &chosen {
            assert_eq!(dc.get(url).unwrap().status(), SlotStatus::Loading);
        }
        assert!(dc.get(&u[2]).is_none(), "past budget: no slot minted");
    }

    #[test]
    fn pump_leaves_room_for_inflight_even_past_a_live_cap_decrease() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let u = urls(3);
        assert!(dc.pump(&window(&u), now, 4, 4).is_empty());
        // Live cap decrease: busy exceeds cap; still no new spawns.
        assert!(dc.pump(&window(&u), now, 2, 3).is_empty());
    }

    #[test]
    fn pump_skips_ready_loading_and_cooling_slots() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let u = urls(4);
        dc.adopt(&u[0]);
        let first = dc.pump(&window(&u[..2]), now, 8, 0);
        assert_eq!(first, vec![u[1].clone()], "ready slot skipped");
        let again = dc.pump(&window(&u[..2]), now, 8, 0);
        assert!(again.is_empty(), "loading slot skipped");
        dc.note_failure(&u[1], now);
        assert!(dc.pump(&window(&u[..2]), now, 8, 0).is_empty(), "cooling");
        let past = now + RETRY_COOLDOWN;
        assert_eq!(
            dc.pump(&window(&u[..2]), past, 8, 0),
            vec![u[1].clone()],
            "cooldown boundary re-admits"
        );
    }

    #[test]
    fn duplicate_url_in_the_window_is_single_flight() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let u = urls(1);
        let w = vec![u[0].as_str(), u[0].as_str()];
        assert_eq!(dc.pump(&w, now, 8, 0), vec![u[0].clone()]);
    }

    #[test]
    fn reset_loading_only_unwedges_a_loading_slot() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let u = urls(1);
        dc.pump(&window(&u), now, 8, 0);
        dc.reset_loading(&u[0]);
        assert_eq!(dc.get(&u[0]).unwrap().status(), SlotStatus::Idle);
        dc.adopt(&u[0]);
        dc.reset_loading(&u[0]);
        assert_eq!(dc.get(&u[0]).unwrap().status(), SlotStatus::Ready);
    }

    #[test]
    fn adoption_recreates_a_slot_evicted_mid_flight() {
        let mut dc = DiscoverCovers::default();
        let u = urls(1);
        dc.pump(&window(&u), Instant::now(), 8, 0);
        dc.evict(&u[0]);
        assert!(dc.get(&u[0]).is_none());
        dc.adopt(&u[0]);
        let slot = dc.get(&u[0]).unwrap();
        assert_eq!(slot.status(), SlotStatus::Ready);
        assert!(slot.has_pixels());
    }

    #[test]
    fn failure_then_success_clears_the_cooldown() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let u = urls(1);
        dc.note_failure(&u[0], now);
        dc.adopt(&u[0]);
        let slot = dc.get(&u[0]).unwrap();
        assert_eq!(slot.status(), SlotStatus::Ready);
        assert!(slot.failed_at.is_none());
    }

    #[test]
    fn eviction_sheds_oldest_offscreen_past_the_cap() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let old = urls(DISCOVER_COVER_CAP + 2);
        // Two pumps ago: everything minted (cap is generous).
        dc.pump(&window(&old), now, 1000, 0);
        for u in &old {
            dc.reset_loading(u);
        }
        // New window: two fresh urls push the pool past cap; the two
        // oldest-seen off-screen slots go, the window itself is untouched.
        let fresh = vec![
            "https://img/fresh-a.png".to_string(),
            "https://img/fresh-b.png".to_string(),
        ];
        dc.pump(&window(&fresh), now, 1000, 0);
        assert!(dc.len() <= DISCOVER_COVER_CAP + 2);
        assert!(dc.get(&fresh[0]).is_some());
        assert!(dc.get(&fresh[1]).is_some());
        assert!(
            dc.get(&old[0]).is_none() || dc.get(&old[1]).is_none(),
            "some oldest off-screen slot must be gone"
        );
    }

    #[test]
    fn eviction_never_touches_visible_or_loading_slots() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let many = urls(DISCOVER_COVER_CAP + 4);
        // Mint everything as loading: the whole pool is in flight.
        dc.pump(&window(&many), now, 1000, 0);
        dc.pump(&window(&many[..2]), now, 1000, 1000);
        assert_eq!(
            dc.len(),
            DISCOVER_COVER_CAP + 4,
            "loading slots are the eviction floor"
        );
    }

    #[test]
    fn eviction_noop_at_or_under_cap() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let u = urls(DISCOVER_COVER_CAP);
        dc.pump(&window(&u), now, 1000, 0);
        for url in &u {
            dc.reset_loading(url);
        }
        dc.pump(&[], now + Duration::from_millis(1), 8, 0);
        assert_eq!(dc.len(), DISCOVER_COVER_CAP);
    }

    #[test]
    fn pump_recency_protects_recently_seen_slots() {
        let now = Instant::now();
        let mut dc = DiscoverCovers::default();
        let all = urls(DISCOVER_COVER_CAP + 2);
        dc.pump(&window(&all), now, 1000, 0);
        for u in &all {
            dc.reset_loading(u);
        }
        // Re-see everything but the first two, then push two fresh urls in.
        dc.pump(&window(&all[2..]), now, 1000, 1000);
        let fresh = vec![
            "https://img/f1.png".to_string(),
            "https://img/f2.png".to_string(),
        ];
        dc.pump(&window(&fresh), now, 1000, 1000);
        assert!(dc.get(&all[0]).is_none(), "unseen slot evicted first");
        assert!(dc.get(&all[1]).is_none(), "unseen slot evicted first");
        assert!(dc.get(&all[2]).is_some(), "recently seen slot survives");
    }
}
