//! Eager sibling-provider prewarm walk (03 §6.5, 04 §7.6): after a successful
//! add or play, probe unchecked providers in the background so a later flip
//! or fallback is tier-0. Silent (no toast, no spinner). One walk app-wide;
//! user-facing resolve gates new starts, and only an advancing fallback
//! cancels a live walk (03 §6.4: rescue owns the CDN budget).

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::domain::Enrichment;
use crate::tui::episodes::EpisodeDeps;
use crate::tui::event::PrewarmVerdict;
use crate::tui::workers::{self, Drain, Generation, PrewarmProbe};

/// App-wide floor between walk starts: the ring alone can burst after
/// eviction (04 §8).
pub const SPACING: Duration = Duration::from_secs(30);
/// Gap between candidate probes: pure background work must not read as a CDN
/// burst (03 §6.5).
pub const HOP_GAP: Duration = Duration::from_millis(1500);
const RING_SLOTS: usize = 32;

/// User-facing resolve signals sampled at fire time (04 §7.6). They gate walk
/// starts only; a gate rising mid-walk does not cancel it. The freeze's
/// add_resolving gate has no port: P-save is a synchronous store write here.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gates {
    /// Play launching window: resolve runs inside the play worker, so the
    /// warm waits for mpv to open (glance clears).
    pub play_resolving: bool,
    pub fallback_active: bool,
}

#[derive(Debug)]
struct Run {
    canonical: Enrichment,
    candidates: Vec<String>,
    /// Next candidate to spawn.
    ix: usize,
    inflight: bool,
    /// `HOP_GAP` pacing deadline for the next spawn.
    gap_until: Option<Instant>,
    /// Cancelled: honor the in-flight result (its network spend is sunk),
    /// spawn nothing more.
    yielding: bool,
}

#[derive(Debug, Default)]
pub struct PrewarmState {
    run: Option<Run>,
    /// Session ring of warmed ids. Soft dedup: round-robin eviction lets a
    /// show re-attempt after `RING_SLOTS` others, pairing with absence TTL
    /// expiry. `Option`, never a 0 sentinel: nothing enforces
    /// `anilist_id > 0`.
    attempted: [Option<i64>; RING_SLOTS],
    attempted_next: usize,
    last_start: Option<Instant>,
    generation: Generation,
    drain: Drain,
}

impl PrewarmState {
    /// Start a walk for `canonical` unless gated. Empty candidates mark
    /// nothing, so a later-gained key or aged-out absence still gets its warm;
    /// ring and spacing stamp only when a probe actually spawned (03 §6.5).
    pub fn fire(&mut self, canonical: &Enrichment, gates: Gates, deps: &EpisodeDeps) {
        if self.blocked(canonical.anilist_id, deps.now, gates) {
            return;
        }
        let candidates = candidates(deps, canonical.anilist_id);
        if candidates.is_empty() {
            return;
        }
        let mut run = Run {
            canonical: canonical.clone(),
            candidates,
            ix: 0,
            inflight: false,
            gap_until: None,
            yielding: false,
        };
        if !spawn_next(&mut run, &self.drain, &self.generation, deps) {
            return;
        }
        self.mark_attempted(canonical.anilist_id, deps.now);
        self.run = Some(run);
    }

    /// Route one settled probe: mint what it learned, then pace the next
    /// spawn. Returns true when an availability row changed (the caller
    /// refreshes the open show's rail). At most one probe is ever in flight,
    /// so a stale token means a superseded run: discard.
    pub fn on_result(
        &mut self,
        provider: &str,
        verdict: &PrewarmVerdict,
        token: u64,
        deps: &EpisodeDeps,
    ) -> bool {
        if !self.generation.is_current(token) {
            return false;
        }
        let Some(run) = self.run.as_mut() else {
            return false;
        };
        // Best-effort mints (05 §10.4): binding rows only, no library chrome.
        // A failed write costs a re-probe after ring eviction, nothing else.
        let wrote = match verdict {
            PrewarmVerdict::Found { provider_id } => deps
                .store
                .bind_provider(&run.canonical, provider, provider_id, deps.unix_now)
                .is_ok(),
            PrewarmVerdict::Absent => deps
                .store
                .mark_provider_absent(&run.canonical, provider, deps.unix_now)
                .is_ok(),
            PrewarmVerdict::Nothing => false,
        };
        run.inflight = false;
        if run.yielding || run.ix >= run.candidates.len() {
            self.run = None;
        } else {
            run.gap_until = Some(deps.now + HOP_GAP);
        }
        wrote
    }

    /// Tick hook: fire the paced next spawn once the gap passes.
    pub fn tick(&mut self, deps: &EpisodeDeps) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        if run.inflight || run.yielding {
            return;
        }
        if let Some(t) = run.gap_until
            && deps.now < t
        {
            return;
        }
        if !spawn_next(run, &self.drain, &self.generation, deps) {
            self.run = None;
        }
    }

    pub fn active(&self) -> bool {
        self.run.is_some()
    }

    /// An advancing fallback owns the CDN budget (03 §6.4).
    pub fn cancel(&mut self) {
        match self.run.as_mut() {
            Some(run) if run.inflight => run.yielding = true,
            Some(_) => self.run = None,
            None => {}
        }
    }

    pub fn drain(&self, timeout: Duration) -> bool {
        self.drain.drain(timeout)
    }

    fn blocked(&self, anilist_id: i64, now: Instant, gates: Gates) -> bool {
        if self.run.is_some() || gates.play_resolving || gates.fallback_active {
            return true;
        }
        if self.attempted.iter().flatten().any(|&a| a == anilist_id) {
            return true;
        }
        match self.last_start {
            Some(t) => now.duration_since(t) < SPACING,
            None => false,
        }
    }

    fn mark_attempted(&mut self, anilist_id: i64, now: Instant) {
        self.attempted[self.attempted_next] = Some(anilist_id);
        self.attempted_next = (self.attempted_next + 1) % RING_SLOTS;
        self.last_start = Some(now);
    }
}

/// Unchecked providers in construction order (05 §10.4): no binding, no fresh
/// absence. Store read errors degrade to unchecked, matching the resolve
/// world's lean (a re-probe is cheaper than a missed warm).
fn candidates(deps: &EpisodeDeps, anilist_id: i64) -> Vec<String> {
    let bound: Vec<String> = deps
        .store
        .bindings_for(anilist_id)
        .map(|bs| bs.into_iter().map(|b| b.provider).collect())
        .unwrap_or_default();
    deps.registry
        .ordered(None)
        .iter()
        .map(|p| p.name())
        .filter(|name| !bound.iter().any(|b| b == name))
        .filter(|name| {
            !deps
                .store
                .provider_absent_fresh(anilist_id, name, deps.unix_now)
                .unwrap_or(false)
        })
        .map(str::to_string)
        .collect()
}

/// Spawn the probe for `run.ix`, skipping past spawn failures; false when no
/// worker started (exhausted).
fn spawn_next(run: &mut Run, drain: &Drain, generation: &Generation, deps: &EpisodeDeps) -> bool {
    while run.ix < run.candidates.len() {
        let provider = run.candidates[run.ix].clone();
        run.ix += 1;
        let token = generation.bump();
        if workers::spawn_prewarm_probe(
            drain,
            deps.tx.clone(),
            Arc::clone(deps.registry),
            PrewarmProbe {
                provider,
                canonical: run.canonical.clone(),
                translation: deps.translation,
                token,
            },
        ) {
            run.inflight = true;
            run.gap_until = None;
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Translation;
    use crate::providers::{ProviderError, SearchHit};
    use crate::store::Store;
    use crate::tui::episodes::teststub::{StubProvider, registry};
    use crate::tui::event::{self, Event};

    fn canonical(aid: i64) -> Enrichment {
        Enrichment {
            anilist_id: aid,
            title_romaji: format!("Show {aid}"),
            mal_id: Some(500 + aid),
            total_episodes: Some(3),
            status: Some("FINISHED".into()),
            ..Enrichment::default()
        }
    }

    fn hit(aid: i64, id: &str) -> SearchHit {
        SearchHit {
            provider_id: id.into(),
            title: format!("Show {aid}"),
            title_english: None,
            title_native: None,
            anilist_id: Some(aid),
            mal_id: None,
            total_episodes: Some(3),
            eps_sub: 3,
            eps_dub: 0,
            year: None,
        }
    }

    struct World {
        store: Store,
        registry: std::sync::Arc<crate::providers::ProviderRegistry>,
        tx: event::EventTx,
        rx: event::EventRx,
        t0: Instant,
    }

    impl World {
        fn new(providers: Vec<StubProvider>) -> World {
            let (tx, rx) = event::channel();
            World {
                store: Store::open_memory().unwrap(),
                registry: registry(providers),
                tx,
                rx,
                t0: Instant::now(),
            }
        }

        fn deps_at(&self, now: Instant) -> EpisodeDeps<'_> {
            EpisodeDeps {
                store: &self.store,
                registry: &self.registry,
                tx: &self.tx,
                global_pref: "",
                translation: Translation::Sub,
                unix_now: 1_000,
                now,
            }
        }

        /// Drain the in-flight probe, route its result, hop the gap; loop
        /// until the walk clears.
        fn settle(&self, state: &mut PrewarmState) {
            let mut now = self.t0;
            while state.run.is_some() {
                assert!(state.drain(Duration::from_secs(5)));
                while let Ok(ev) = self.rx.try_recv() {
                    let Event::PrewarmResult {
                        provider,
                        verdict,
                        token,
                        ..
                    } = ev
                    else {
                        panic!("unexpected event on the prewarm channel");
                    };
                    state.on_result(&provider, &verdict, token, &self.deps_at(now));
                }
                now += HOP_GAP;
                state.tick(&self.deps_at(now));
            }
        }

        fn bound_id(&self, aid: i64, provider: &str) -> Option<String> {
            self.store
                .bindings_for(aid)
                .unwrap()
                .into_iter()
                .find(|b| b.provider == provider)
                .map(|b| b.provider_id)
        }
    }

    #[test]
    fn fire_probes_every_candidate_and_mints() {
        let w = World::new(vec![
            StubProvider::new("a")
                .with_key("ka")
                .with_episodes(Ok(vec!["1".into()])),
            StubProvider::new("b")
                .with_key("kb")
                .with_episodes(Ok(vec![])),
            StubProvider::new("c")
                .with_search(Ok(vec![hit(7, "c1")]))
                .with_episodes(Ok(vec!["1".into()])),
        ]);
        let mut state = PrewarmState::default();
        state.fire(&canonical(7), Gates::default(), &w.deps_at(w.t0));
        assert!(state.run.is_some());
        w.settle(&mut state);
        assert_eq!(w.bound_id(7, "a").as_deref(), Some("ka"));
        assert!(w.store.provider_absent_fresh(7, "b", 1_000).unwrap());
        assert_eq!(w.bound_id(7, "c").as_deref(), Some("c1"));
        assert!(state.attempted.iter().flatten().any(|&a| a == 7));
    }

    #[test]
    fn errors_and_search_misses_mint_nothing() {
        let w = World::new(vec![
            StubProvider::new("a")
                .with_key("ka")
                .with_episodes(Err(ProviderError::Network)),
            StubProvider::new("b").with_search(Ok(vec![])),
            StubProvider::new("c").with_search(Err(ProviderError::Unsupported)),
        ]);
        let mut state = PrewarmState::default();
        state.fire(&canonical(7), Gates::default(), &w.deps_at(w.t0));
        w.settle(&mut state);
        for p in ["a", "b", "c"] {
            assert_eq!(w.bound_id(7, p), None);
            assert!(!w.store.provider_absent_fresh(7, p, 1_000).unwrap());
        }
    }

    #[test]
    fn search_match_with_empty_listing_marks_absence() {
        let w = World::new(vec![
            StubProvider::new("c")
                .with_search(Ok(vec![hit(7, "c1")]))
                .with_episodes(Ok(vec![])),
        ]);
        let mut state = PrewarmState::default();
        state.fire(&canonical(7), Gates::default(), &w.deps_at(w.t0));
        w.settle(&mut state);
        assert_eq!(w.bound_id(7, "c"), None);
        assert!(w.store.provider_absent_fresh(7, "c", 1_000).unwrap());
    }

    #[test]
    fn candidates_skip_bound_and_fresh_absent() {
        let w = World::new(vec![
            StubProvider::new("a")
                .with_key("ka")
                .with_episodes(Ok(vec!["1".into()])),
            StubProvider::new("b")
                .with_key("kb")
                .with_episodes(Ok(vec!["1".into()])),
            StubProvider::new("c")
                .with_key("kc")
                .with_episodes(Ok(vec!["1".into()])),
        ]);
        let show = canonical(7);
        w.store.bind_provider(&show, "a", "x", 900).unwrap();
        w.store.mark_provider_absent(&show, "b", 999).unwrap();
        let mut state = PrewarmState::default();
        state.fire(&show, Gates::default(), &w.deps_at(w.t0));
        w.settle(&mut state);
        assert_eq!(w.bound_id(7, "a").as_deref(), Some("x"));
        assert_eq!(w.bound_id(7, "b"), None);
        assert_eq!(w.bound_id(7, "c").as_deref(), Some("kc"));
    }

    #[test]
    fn empty_candidates_mark_nothing() {
        let w = World::new(vec![
            StubProvider::new("a").with_key("ka"),
            StubProvider::new("b").with_key("kb"),
        ]);
        let show = canonical(7);
        w.store.bind_provider(&show, "a", "x", 900).unwrap();
        w.store.mark_provider_absent(&show, "b", 999).unwrap();
        let mut state = PrewarmState::default();
        state.fire(&show, Gates::default(), &w.deps_at(w.t0));
        assert!(state.run.is_none());
        assert!(state.attempted.iter().all(Option::is_none));
        assert!(state.last_start.is_none());
    }

    #[test]
    fn gates_block_fire() {
        let state = PrewarmState::default();
        let t = Instant::now();
        for gates in [
            Gates {
                play_resolving: true,
                ..Gates::default()
            },
            Gates {
                fallback_active: true,
                ..Gates::default()
            },
        ] {
            assert!(state.blocked(7, t, gates));
        }
        assert!(!state.blocked(7, t, Gates::default()));
    }

    #[test]
    fn ring_blocks_reattempt_until_evicted() {
        let mut state = PrewarmState::default();
        let t = Instant::now();
        state.mark_attempted(7, t);
        let later = t + SPACING;
        assert!(state.blocked(7, later, Gates::default()));
        for i in 0..RING_SLOTS as i64 {
            state.mark_attempted(1_000 + i, t);
        }
        assert!(!state.blocked(7, later, Gates::default()));
    }

    #[test]
    fn spacing_floor_blocks_second_walk() {
        let mut state = PrewarmState::default();
        let t = Instant::now();
        state.mark_attempted(1, t);
        assert!(state.blocked(2, t + SPACING - Duration::from_secs(1), Gates::default()));
        assert!(!state.blocked(2, t + SPACING, Gates::default()));
    }

    #[test]
    fn cancel_mid_flight_honors_result_then_stops() {
        let w = World::new(vec![
            StubProvider::new("a")
                .with_key("ka")
                .with_episodes(Ok(vec!["1".into()])),
            StubProvider::new("b")
                .with_key("kb")
                .with_episodes(Ok(vec!["1".into()])),
        ]);
        let mut state = PrewarmState::default();
        state.fire(&canonical(7), Gates::default(), &w.deps_at(w.t0));
        state.cancel();
        w.settle(&mut state);
        assert_eq!(w.bound_id(7, "a").as_deref(), Some("ka"));
        assert_eq!(w.bound_id(7, "b"), None);
    }

    #[test]
    fn cancel_between_hops_drops_walk() {
        let w = World::new(vec![
            StubProvider::new("a")
                .with_key("ka")
                .with_episodes(Err(ProviderError::Network)),
            StubProvider::new("b")
                .with_key("kb")
                .with_episodes(Ok(vec!["1".into()])),
        ]);
        let mut state = PrewarmState::default();
        state.fire(&canonical(7), Gates::default(), &w.deps_at(w.t0));
        assert!(state.drain(Duration::from_secs(5)));
        let Ok(Event::PrewarmResult {
            provider,
            verdict,
            token,
            ..
        }) = w.rx.try_recv()
        else {
            panic!("no probe result");
        };
        state.on_result(&provider, &verdict, token, &w.deps_at(w.t0));
        state.cancel();
        assert!(state.run.is_none());
        assert_eq!(w.bound_id(7, "b"), None);
    }

    #[test]
    fn active_walk_blocks_second_fire() {
        let w = World::new(vec![
            StubProvider::new("a")
                .with_key("ka")
                .with_episodes(Ok(vec!["1".into()])),
        ]);
        let mut state = PrewarmState::default();
        state.fire(&canonical(7), Gates::default(), &w.deps_at(w.t0));
        state.fire(&canonical(8), Gates::default(), &w.deps_at(w.t0));
        assert!(!state.attempted.iter().flatten().any(|&a| a == 8));
        w.settle(&mut state);
    }
}
