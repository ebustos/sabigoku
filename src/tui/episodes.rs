//! Episode session: the transport around the headless resolve engine
//! (03 §4-6, 05 §10). Owns the grid state, the walk, and the store writes the
//! engine's actions name; App dispatches events here and turns the returned
//! `Feedback` into toasts. Lives with its subsystem per the ROD-439 rules,
//! never on App.
//!
//! Every fire bumps the generation, so any earlier in-flight result is stale
//! by construction; a superseded fetch can never clear a live load or toast
//! (03 §10). Supersede is detach + drop, never a join (04 §6).

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::domain::{self, Enrichment, Translation};
use crate::providers::ProviderRegistry;
use crate::resolve::{self, Exhausted, Hop, ResolveTarget, ResolveWorld, RouteAction, Walk};
use crate::resolver;
use crate::store::{ProviderAvailability, Store};
use crate::tui::clock::AsyncStart;
use crate::tui::event::{EventTx, FetchClass};
use crate::tui::workers::{self, Drain, EpisodeFetch, Generation, ProviderSearch};

/// Everything one session step needs from the app, borrowed per call so the
/// session never holds store or registry references across events.
pub struct EpisodeDeps<'a> {
    pub store: &'a Store,
    pub registry: &'a Arc<ProviderRegistry>,
    pub tx: &'a EventTx,
    pub global_pref: &'a str,
    pub translation: Translation,
    pub unix_now: i64,
    pub now: Instant,
}

impl EpisodeDeps<'_> {
    fn world(&self) -> WorldView<'_> {
        WorldView {
            store: self.store,
            registry: self.registry,
            global_pref: self.global_pref,
            unix_now: self.unix_now,
        }
    }
}

/// `ResolveWorld` over the live store + registry. Store read errors degrade to
/// absent/unset: resolve then walks a longer road, which beats wedging an open
/// on a transient DB error.
struct WorldView<'a> {
    store: &'a Store,
    registry: &'a ProviderRegistry,
    global_pref: &'a str,
    unix_now: i64,
}

impl ResolveWorld for WorldView<'_> {
    fn ordered(&self, pref: Option<&str>) -> Vec<String> {
        self.registry
            .ordered(pref)
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    fn registered(&self, provider: &str) -> bool {
        self.registry.by_name(provider).is_some()
    }

    fn binding(&self, anilist_id: i64, provider: &str) -> Option<String> {
        self.store
            .bindings_for(anilist_id)
            .ok()?
            .into_iter()
            .find(|b| b.provider == provider)
            .map(|b| b.provider_id)
    }

    fn absent_fresh(&self, anilist_id: i64, provider: &str) -> bool {
        self.store
            .provider_absent_fresh(anilist_id, provider, self.unix_now)
            .unwrap_or(false)
    }

    fn canonical_key(&self, provider: &str, canonical: &Enrichment) -> Option<String> {
        self.registry.by_name(provider)?.canonical_key(canonical)
    }

    fn pin(&self, anilist_id: i64) -> Option<String> {
        self.store.get_provider_pin(anilist_id).ok().flatten()
    }

    fn route_pref(&self, anilist_id: i64) -> Option<String> {
        self.store.get_route_pref(anilist_id).ok().flatten()
    }

    fn global_pref(&self) -> String {
        self.global_pref.to_string()
    }
}

/// User-visible outcomes of a session step; App maps these to the DESIGN 4.10
/// toast rows. The session itself never touches `Toasts`.
#[derive(Debug, Clone, PartialEq)]
pub enum Feedback {
    /// Provider failure class row (`episodes_error` / search failure).
    Fail { provider: String, class: FetchClass },
    /// The walk moved: `trying {provider}…`.
    Hop { provider: String },
    /// Forced-preferred miss (K-2 step 5): distinct copy, never "pin kept".
    NoMatch { provider: String },
    /// Pin-flip miss: pin kept, stop (03 §5.3).
    PinKept { provider: String },
    /// Ordinary walk exhausted with nothing landed.
    DeadEnd,
    /// `v` pinned the provider already serving the grid; no hop.
    PinSet { provider: String },
    /// `v` cycled past the last provider back to unpinned.
    PinCleared,
    /// `v` while the resolve is still in flight.
    PinPending,
    /// `v` with no episode source to pin against.
    PinNothing,
    /// The pin write itself failed.
    PinSaveFailed { clearing: bool },
}

#[derive(Default)]
pub struct EpisodeSession {
    for_id: Option<i64>,
    /// Snapshot the walks and scorers run against; pushed in with the target,
    /// never read back out of a list view.
    canonical: Option<Enrichment>,
    track: Option<Translation>,
    episodes: Vec<String>,
    serving: Option<String>,
    cursor: usize,
    resume_ix: Option<usize>,
    watched: u32,
    no_source: bool,
    loading: Option<AsyncStart>,
    /// Mint the binding when the in-flight fetch lands non-empty (03 §10
    /// pending_bind). Token-guarded like everything else in flight.
    mint_pending: bool,
    walk: Option<Walk>,
    /// Name behind a live single-provider walk, for the miss copy.
    walk_provider: Option<String>,
    /// Pre-flip cursor identity `(raw label, 1-based ordinal)`: a flip
    /// landing keeps the cursor on the in-progress episode (05 §10.5).
    remap_from: Option<(String, u32)>,
    /// Pin + per-provider availability, refreshed on engage and on every
    /// bind/absence write (03 §6.1 step 2) so draw never reads the store.
    pin: Option<String>,
    avail: Vec<(String, ProviderAvailability)>,
    generation: Generation,
    drain: Drain,
}

impl EpisodeSession {
    pub fn reset(&mut self) {
        self.generation.bump();
        self.for_id = None;
        self.canonical = None;
        self.track = None;
        self.episodes.clear();
        self.serving = None;
        self.cursor = 0;
        self.resume_ix = None;
        self.watched = 0;
        self.no_source = false;
        self.loading = None;
        self.mint_pending = false;
        self.walk = None;
        self.walk_provider = None;
        self.remap_from = None;
        self.pin = None;
        self.avail.clear();
    }

    /// Detail entry (03 §6.1): lazy, never fired by list scroll (05 §10.1).
    /// Re-entering the same show on the same track is a no-op while the
    /// session already holds an answer or a fetch.
    pub fn engage(&mut self, canonical: &Enrichment, deps: &EpisodeDeps) -> Vec<Feedback> {
        let aid = canonical.anilist_id;
        let held = self.for_id == Some(aid) && self.track == Some(deps.translation);
        if held && (self.loading.is_some() || !self.episodes.is_empty() || self.no_source) {
            return Vec::new();
        }
        self.reset();
        self.for_id = Some(aid);
        self.canonical = Some(canonical.clone());
        self.track = Some(deps.translation);
        self.refresh_meta(deps);

        // 03 §6.1 step 4: the preferred re-route runs before the classifier.
        let routed = resolve::route_preferred(&deps.world(), canonical);
        let action = match &routed.stamp {
            // Stamp-before-fetch (03 §5.3): a forced probe that fired without
            // its stamp would re-force on every open. If the stamp write
            // fails, open normally instead of forcing unstamped.
            Some(stamp) if deps.store.set_route_pref(canonical, stamp).is_err() => {
                RouteAction::None
            }
            _ => routed.action,
        };
        match action {
            RouteAction::None => self.engage_classify(canonical, deps),
            RouteAction::OpenBinding { provider, id } => {
                self.open_bound(provider, id, deps);
                Vec::new()
            }
            RouteAction::FetchTierA { provider, key, .. } => {
                self.fire_fetch(provider, key, true, deps);
                Vec::new()
            }
            RouteAction::Walk(walk) => {
                self.walk_provider = Some(deps.global_pref.to_string());
                self.walk = Some(*walk);
                self.advance_walk(false, deps)
            }
        }
    }

    fn engage_classify(&mut self, canonical: &Enrichment, deps: &EpisodeDeps) -> Vec<Feedback> {
        match resolve::classify_open(&deps.world(), canonical) {
            ResolveTarget::Bound { provider, id, .. } => {
                self.open_bound(provider, id, deps);
                Vec::new()
            }
            ResolveTarget::TierA { provider, key, .. } => {
                self.fire_fetch(provider, key, true, deps);
                Vec::new()
            }
            ResolveTarget::NeedsSearch { .. } => {
                let walk = Walk::fallback(&deps.world(), canonical.clone(), None);
                self.walk = Some(walk);
                self.advance_walk(false, deps)
            }
        }
    }

    /// Tier 0: an unexpired cached listing paints without a fetch (03 §6.1
    /// step 7); the DB cache is the one listing cache, no in-memory twin.
    fn open_bound(&mut self, provider: String, id: String, deps: &EpisodeDeps) {
        let aid = self.for_id.unwrap_or_default();
        if let Ok(Some(cached)) =
            deps.store
                .get_cached_episodes(aid, &provider, deps.translation, deps.unix_now)
        {
            self.land(provider, cached, deps);
            return;
        }
        self.fire_fetch(provider, id, false, deps);
    }

    fn fire_fetch(
        &mut self,
        provider: String,
        provider_id: String,
        mint: bool,
        deps: &EpisodeDeps,
    ) {
        let Some(canonical) = self.canonical.as_ref() else {
            return;
        };
        let token = self.generation.bump();
        self.mint_pending = mint;
        self.loading = Some(AsyncStart::new(deps.now));
        let count_hint = domain::expected_episode_count(
            canonical.status.as_deref(),
            canonical.total_episodes,
            canonical.next_airing_episode,
        );
        let spawned = workers::spawn_episodes(
            &self.drain,
            deps.tx.clone(),
            Arc::clone(deps.registry),
            EpisodeFetch {
                anilist_id: canonical.anilist_id,
                provider,
                provider_id,
                translation: deps.translation,
                count_hint,
                token,
            },
        );
        if !spawned {
            // No worker will ever answer; blank beats a stranded spinner.
            self.loading = None;
        }
    }

    fn fire_search(&mut self, provider: String, deps: &EpisodeDeps) {
        let Some(canonical) = self.canonical.as_ref() else {
            return;
        };
        let token = self.generation.bump();
        self.mint_pending = false;
        self.loading = Some(AsyncStart::new(deps.now));
        let spawned = workers::spawn_provider_search(
            &self.drain,
            deps.tx.clone(),
            Arc::clone(deps.registry),
            ProviderSearch {
                anilist_id: canonical.anilist_id,
                provider,
                query: canonical.title_romaji.clone(),
                translation: deps.translation,
                token,
            },
        );
        if !spawned {
            self.loading = None;
        }
    }

    /// One hop per call (03 §6.4). `announce` gates the hop toast: the walk's
    /// opening attempt is not a hop, only a move after a miss/failure is.
    fn advance_walk(&mut self, announce: bool, deps: &EpisodeDeps) -> Vec<Feedback> {
        let step = {
            let world = deps.world();
            self.walk.as_mut().map(|w| w.advance(&world))
        };
        let Some(step) = step else {
            if self.episodes.is_empty() {
                self.no_source = true;
            }
            return Vec::new();
        };
        match step {
            Ok(Hop::Fetch { provider, id, bind }) => {
                let mut fb = Vec::new();
                if announce {
                    fb.push(Feedback::Hop {
                        provider: provider.clone(),
                    });
                }
                // A bound hop paints from an unexpired cache like a bound
                // open; only a fresh tier-A key must go to the network.
                if bind.is_some() {
                    self.fire_fetch(provider, id, true, deps);
                } else {
                    self.open_bound(provider, id, deps);
                }
                fb
            }
            Ok(Hop::Search { provider, .. }) => {
                let mut fb = Vec::new();
                if announce {
                    fb.push(Feedback::Hop {
                        provider: provider.clone(),
                    });
                }
                self.fire_search(provider, deps);
                fb
            }
            Err(Exhausted::Continue(cont)) => {
                // K-2 law (03 §5.3): the forced-preferred miss rolls into a
                // bindings-first full walk; the grid must never stay blank
                // while a binding exists.
                let missed = self.walk_provider.take().unwrap_or_default();
                self.walk = Some(*cont);
                let mut fb = vec![Feedback::NoMatch { provider: missed }];
                fb.extend(self.advance_walk(true, deps));
                fb
            }
            Err(Exhausted::PinKept) => {
                let target = self.walk_provider.take().unwrap_or_default();
                self.walk = None;
                self.loading = None;
                vec![Feedback::PinKept { provider: target }]
            }
            Err(Exhausted::DeadEnd) => {
                self.walk = None;
                self.loading = None;
                if self.episodes.is_empty() {
                    self.no_source = true;
                }
                vec![Feedback::DeadEnd]
            }
        }
    }

    /// Failed or empty fetch: begin/continue the fallback walk (03 §6.4). A
    /// fresh walk pre-marks the failed provider; a live walk is already past
    /// its hop.
    fn fail_over(&mut self, failed: &str, deps: &EpisodeDeps) -> Vec<Feedback> {
        if self.walk.is_none() {
            let Some(canonical) = self.canonical.clone() else {
                return Vec::new();
            };
            self.walk = Some(Walk::fallback(&deps.world(), canonical, Some(failed)));
        }
        self.advance_walk(true, deps)
    }

    pub fn on_done(
        &mut self,
        anilist_id: i64,
        provider: &str,
        provider_id: &str,
        episodes: Vec<String>,
        token: u64,
        deps: &EpisodeDeps,
    ) -> Vec<Feedback> {
        if !self.generation.is_current(token) || self.for_id != Some(anilist_id) {
            return Vec::new();
        }
        self.loading = None;
        let Some(canonical) = self.canonical.clone() else {
            return Vec::new();
        };
        if episodes.is_empty() {
            // Authoritative not stocked (03 §4.3): negative-cache and walk
            // on; never bind an empty grid (05 §10.3).
            let _ = deps
                .store
                .mark_provider_absent(&canonical, provider, deps.unix_now);
            self.mint_pending = false;
            self.refresh_meta(deps);
            return self.fail_over(provider, deps);
        }
        // ROD-327 FK order: identity + binding land before the episode cache
        // row. Both writes are best-effort; a miss costs a refetch, not the
        // grid.
        if self.mint_pending {
            let _ = deps
                .store
                .bind_provider(&canonical, provider, provider_id, deps.unix_now);
            self.mint_pending = false;
        }
        let _ = deps.store.set_episode_cache(
            anilist_id,
            provider,
            deps.translation,
            &episodes,
            canonical.status.as_deref(),
            deps.unix_now,
        );
        // Landed hop clears the walk (05 §10.3); no ping-pong of fresh walks.
        self.walk = None;
        self.walk_provider = None;
        self.refresh_meta(deps);
        self.land(provider.to_string(), episodes, deps);
        Vec::new()
    }

    pub fn on_error(
        &mut self,
        anilist_id: i64,
        provider: &str,
        class: FetchClass,
        token: u64,
        deps: &EpisodeDeps,
    ) -> Vec<Feedback> {
        if !self.generation.is_current(token) || self.for_id != Some(anilist_id) {
            return Vec::new();
        }
        self.loading = None;
        let mut fb = vec![Feedback::Fail {
            provider: provider.to_string(),
            class,
        }];
        // Transient failure: no absence mark (03 §4.3), just the next hop.
        fb.extend(self.fail_over(provider, deps));
        fb
    }

    pub fn on_search_done(
        &mut self,
        anilist_id: i64,
        provider: &str,
        hits: &[crate::providers::SearchHit],
        token: u64,
        deps: &EpisodeDeps,
    ) -> Vec<Feedback> {
        if !self.generation.is_current(token) || self.for_id != Some(anilist_id) {
            return Vec::new();
        }
        self.loading = None;
        let Some(canonical) = self.canonical.as_ref() else {
            return Vec::new();
        };
        // Id agreement outranks fuzzy (03 §4.2); below the floors is a miss,
        // never a bind.
        let hit = resolver::best_id_match(canonical, hits)
            .or_else(|| resolver::best_provider_match(canonical, hits));
        match hit {
            Some(ix) => {
                let id = hits[ix].provider_id.clone();
                self.fire_fetch(provider.to_string(), id, true, deps);
                Vec::new()
            }
            None => self.advance_walk(true, deps),
        }
    }

    pub fn on_search_error(
        &mut self,
        anilist_id: i64,
        provider: &str,
        class: FetchClass,
        token: u64,
        deps: &EpisodeDeps,
    ) -> Vec<Feedback> {
        if !self.generation.is_current(token) || self.for_id != Some(anilist_id) {
            return Vec::new();
        }
        self.loading = None;
        let mut fb = Vec::new();
        if class != FetchClass::Unsupported {
            fb.push(Feedback::Fail {
                provider: provider.to_string(),
                class,
            });
        }
        fb.extend(self.advance_walk(true, deps));
        fb
    }

    /// Grid landing + cursor seeding (05 §10.7, ROD-163): next-unwatched from
    /// the store high-water, a live resume point overrides, a completed show
    /// wraps to episode one.
    fn land(&mut self, provider: String, episodes: Vec<String>, deps: &EpisodeDeps) {
        let Some(aid) = self.for_id else { return };
        self.no_source = false;
        self.loading = None;
        self.serving = Some(provider);
        self.watched = deps
            .store
            .get_show(aid)
            .ok()
            .flatten()
            .map_or(0, |s| s.progress);
        let next = self.watched as usize;
        self.cursor = if next >= episodes.len() { 0 } else { next };
        self.resume_ix = deps
            .store
            .latest_resume(aid, deps.translation)
            .ok()
            .flatten()
            .and_then(|(label, _)| episodes.iter().position(|e| *e == label));
        if let Some(ix) = self.resume_ix {
            self.cursor = ix;
        }
        // A flip/hop landing keeps the cursor on the in-progress episode
        // (05 §10.5): exact raw label, else 1-based ordinal (03 §6.6).
        if let Some((label, ordinal)) = self.remap_from.take()
            && let Some(ix) = domain::map_episode_index(&episodes, &label, ordinal)
        {
            self.cursor = ix;
        }
        self.episodes = episodes;
    }

    /// Pin + availability snapshot for the meta rail (03 §6.1 step 2). Read
    /// failures degrade to unchecked; the rail dims, nothing wedges.
    fn refresh_meta(&mut self, deps: &EpisodeDeps) {
        let Some(aid) = self.for_id else { return };
        self.pin = deps.store.get_provider_pin(aid).ok().flatten();
        self.avail = deps
            .registry
            .iter()
            .map(|p| {
                let availability = deps
                    .store
                    .provider_availability(aid, p.name(), deps.unix_now)
                    .unwrap_or(ProviderAvailability::Unchecked);
                (p.name().to_string(), availability)
            })
            .collect();
    }

    /// `v` cycle (03 §5.1, DESIGN 6.1): unpinned, then each live registry
    /// provider in construction order, then unpinned. Pinning a provider that
    /// is not serving the grid re-routes through a Path 3 single-provider
    /// walk; its miss keeps the pin.
    pub fn cycle_pin(&mut self, deps: &EpisodeDeps) -> Vec<Feedback> {
        let Some(aid) = self.for_id else {
            return vec![Feedback::PinNothing];
        };
        if self.loading.is_some() {
            return vec![Feedback::PinPending];
        }
        if self.serving.is_none() {
            return vec![Feedback::PinNothing];
        }
        let order: Vec<String> = deps.registry.iter().map(|p| p.name().to_string()).collect();
        let next = match self.pin.as_deref() {
            None => order.first().cloned(),
            Some(pin) => match order.iter().position(|n| n == pin) {
                Some(i) => order.get(i + 1).cloned(),
                // A retired pin name wraps straight to unpinned (05 §10.5).
                None => None,
            },
        };
        let Some(target) = next else {
            if deps.store.set_provider_pin(aid, None).is_err() {
                return vec![Feedback::PinSaveFailed { clearing: true }];
            }
            self.pin = None;
            return vec![Feedback::PinCleared];
        };
        if deps.store.set_provider_pin(aid, Some(&target)).is_err() {
            return vec![Feedback::PinSaveFailed { clearing: false }];
        }
        self.pin = Some(target.clone());
        if self.serving.as_deref() == Some(target.as_str()) {
            return vec![Feedback::PinSet { provider: target }];
        }
        let Some(canonical) = self.canonical.clone() else {
            return vec![Feedback::PinSet { provider: target }];
        };
        // Path 3 (03 §4.1): single-provider walk on the target, probing
        // through fresh absence. `target` comes from the live registry, which
        // is the pin_flip precondition.
        self.remap_from = self
            .episodes
            .get(self.cursor)
            .map(|label| (label.clone(), self.cursor as u32 + 1));
        self.walk_provider = Some(target.clone());
        self.walk = Some(Walk::pin_flip(canonical, target));
        self.advance_walk(true, deps)
    }

    /// Whether the session's answer belongs to the shown entry; render gates
    /// on this so a stale grid can never draw under another show (ROD-329).
    pub fn is_for(&self, anilist_id: i64) -> bool {
        self.for_id == Some(anilist_id)
    }

    /// Engaged = holding an answer or a fetch for this show; distinguishes a
    /// visited detail surface from a merely-scrolled-past selection.
    pub fn engaged_for(&self, anilist_id: i64) -> bool {
        self.is_for(anilist_id) && (self.has_grid() || self.loading.is_some() || self.no_source)
    }

    pub fn grid(&self) -> &[String] {
        &self.episodes
    }

    pub fn has_grid(&self) -> bool {
        !self.episodes.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn watched(&self) -> u32 {
        self.watched
    }

    pub fn resume_ix(&self) -> Option<usize> {
        self.resume_ix
    }

    pub fn serving(&self) -> Option<&str> {
        self.serving.as_deref()
    }

    pub fn pin(&self) -> Option<&str> {
        self.pin.as_deref()
    }

    pub fn avail(&self) -> &[(String, ProviderAvailability)] {
        &self.avail
    }

    pub fn loading(&self) -> Option<AsyncStart> {
        self.loading
    }

    pub fn no_source(&self) -> bool {
        self.no_source
    }

    /// Linear grid cursor (freeze parity: j/k step episodes, not rows).
    pub fn cursor_by(&mut self, delta: i64) {
        if self.episodes.is_empty() {
            return;
        }
        let max = self.episodes.len() as i64 - 1;
        self.cursor = (self.cursor as i64 + delta).clamp(0, max) as usize;
    }

    pub fn cursor_end(&mut self, top: bool) {
        if self.episodes.is_empty() {
            return;
        }
        self.cursor = if top { 0 } else { self.episodes.len() - 1 };
    }

    pub fn drain(&self, timeout: Duration) -> bool {
        self.drain.drain(timeout)
    }
}

#[cfg(test)]
impl EpisodeSession {
    /// Render-test seed: a landed session without transport.
    pub(crate) fn seeded(
        for_id: i64,
        serving: Option<&str>,
        pin: Option<&str>,
        avail: Vec<(String, ProviderAvailability)>,
        episodes: Vec<String>,
    ) -> EpisodeSession {
        EpisodeSession {
            for_id: Some(for_id),
            serving: serving.map(str::to_string),
            pin: pin.map(str::to_string),
            avail,
            episodes,
            ..EpisodeSession::default()
        }
    }
}

#[cfg(test)]
pub(crate) mod teststub {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use crate::domain::{Enrichment, Quality, StreamLink, Translation};
    use crate::providers::{
        CoverRequest, ProviderError, ProviderRegistry, SearchHit, SearchOptions, StreamProvider,
    };

    /// Scripted stream provider: each call pops the next result; an empty
    /// script answers Network so nothing leaves the process.
    pub struct StubProvider {
        name: &'static str,
        key: Option<String>,
        episodes: Mutex<VecDeque<Result<Vec<String>, ProviderError>>>,
        search: Mutex<VecDeque<Result<Vec<SearchHit>, ProviderError>>>,
    }

    impl StubProvider {
        pub fn new(name: &'static str) -> StubProvider {
            StubProvider {
                name,
                key: None,
                episodes: Mutex::new(VecDeque::new()),
                search: Mutex::new(VecDeque::new()),
            }
        }

        pub fn with_key(mut self, key: &str) -> Self {
            self.key = Some(key.to_string());
            self
        }

        pub fn with_episodes(self, result: Result<Vec<String>, ProviderError>) -> Self {
            self.episodes.lock().unwrap().push_back(result);
            self
        }

        pub fn with_search(self, result: Result<Vec<SearchHit>, ProviderError>) -> Self {
            self.search.lock().unwrap().push_back(result);
            self
        }
    }

    impl StreamProvider for StubProvider {
        fn name(&self) -> &'static str {
            self.name
        }
        fn display_name(&self) -> &'static str {
            self.name
        }
        fn canonical_key(&self, _show: &Enrichment) -> Option<String> {
            self.key.clone()
        }
        fn search(
            &self,
            _query: &str,
            _opts: &SearchOptions,
        ) -> Result<Vec<SearchHit>, ProviderError> {
            self.search
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(ProviderError::Network))
        }
        fn episodes(
            &self,
            _provider_id: &str,
            _translation: Translation,
            _count_hint: Option<u32>,
        ) -> Result<Vec<String>, ProviderError> {
            self.episodes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(ProviderError::Network))
        }
        fn resolve(
            &self,
            _provider_id: &str,
            _episode: &str,
            _translation: Translation,
            _quality: Quality,
        ) -> Result<StreamLink, ProviderError> {
            Err(ProviderError::Unsupported)
        }
        fn cover_request(&self, cover_ref: &str) -> Result<CoverRequest, ProviderError> {
            Ok(CoverRequest {
                url: cover_ref.to_string(),
                referer: None,
                user_agent: None,
            })
        }
    }

    pub fn registry(providers: Vec<StubProvider>) -> Arc<ProviderRegistry> {
        Arc::new(ProviderRegistry::new(
            providers
                .into_iter()
                .map(|p| Box::new(p) as Box<dyn StreamProvider>)
                .collect(),
        ))
    }

    /// Three inert providers under the live registry names.
    pub fn inert_registry() -> Arc<ProviderRegistry> {
        registry(vec![
            StubProvider::new("megaplay"),
            StubProvider::new("senshi"),
            StubProvider::new("allanime"),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::teststub::{StubProvider, registry};
    use super::*;
    use crate::providers::{ProviderError, SearchHit};
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

    /// Session vs world in separate fields so `&mut rig.session` and
    /// `&rig.world` borrow disjointly.
    struct Rig {
        world: World,
        session: EpisodeSession,
    }

    struct World {
        store: Store,
        registry: Arc<ProviderRegistry>,
        tx: EventTx,
        rx: event::EventRx,
        now: Instant,
    }

    impl Rig {
        fn new(providers: Vec<StubProvider>) -> Rig {
            let (tx, rx) = event::channel();
            Rig {
                world: World {
                    store: Store::open_memory().unwrap(),
                    registry: registry(providers),
                    tx,
                    rx,
                    now: Instant::now(),
                },
                session: EpisodeSession::default(),
            }
        }
    }

    impl World {
        fn deps<'a>(&'a self, pref: &'a str) -> EpisodeDeps<'a> {
            EpisodeDeps {
                store: &self.store,
                registry: &self.registry,
                tx: &self.tx,
                global_pref: pref,
                translation: Translation::Sub,
                unix_now: 1_000,
                now: self.now,
            }
        }

        /// Drain workers, then route their events back into the session,
        /// collecting feedback; loops until the queue is quiet.
        fn settle(&self, session: &mut EpisodeSession, pref: &str) -> Vec<Feedback> {
            let mut fb = Vec::new();
            loop {
                assert!(session.drain(Duration::from_secs(5)));
                let Ok(ev) = self.rx.try_recv() else { break };
                let deps = self.deps(pref);
                let step = match ev {
                    Event::EpisodesDone {
                        anilist_id,
                        provider,
                        provider_id,
                        episodes,
                        token,
                    } => {
                        session.on_done(anilist_id, &provider, &provider_id, episodes, token, &deps)
                    }
                    Event::EpisodesError {
                        anilist_id,
                        provider,
                        class,
                        token,
                    } => session.on_error(anilist_id, &provider, class, token, &deps),
                    Event::ProviderSearchDone {
                        anilist_id,
                        provider,
                        hits,
                        token,
                    } => session.on_search_done(anilist_id, &provider, &hits, token, &deps),
                    Event::ProviderSearchError {
                        anilist_id,
                        provider,
                        class,
                        token,
                    } => session.on_search_error(anilist_id, &provider, class, token, &deps),
                    other => panic!("unexpected event {other:?}"),
                };
                fb.extend(step);
            }
            fb
        }
    }

    fn eps(labels: &[&str]) -> Result<Vec<String>, ProviderError> {
        Ok(labels.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn tier_a_fetch_mints_binding_and_caches_in_fk_order() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1", "2", "3"])),
            StubProvider::new("senshi"),
            StubProvider::new("allanime"),
        ]);
        let c = canonical(5);
        let fb = rig.session.engage(&c, &rig.world.deps(""));
        assert!(fb.is_empty());
        assert!(rig.session.loading().is_some());
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.is_empty(), "clean land is silent (DESIGN 4.10)");
        assert_eq!(rig.session.grid(), ["1", "2", "3"]);
        assert_eq!(rig.session.serving(), Some("megaplay"));
        let bindings = rig.world.store.bindings_for(5).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].provider_id, "505");
        assert_eq!(
            rig.world
                .store
                .get_cached_episodes(5, "megaplay", Translation::Sub, 1_000)
                .unwrap()
                .unwrap(),
            vec!["1", "2", "3"]
        );
    }

    #[test]
    fn bound_cache_hit_paints_without_a_fetch() {
        let mut rig = Rig::new(vec![StubProvider::new("megaplay")]);
        let c = canonical(5);
        rig.world
            .store
            .bind_provider(&c, "megaplay", "505", 100)
            .unwrap();
        rig.world
            .store
            .set_episode_cache(
                5,
                "megaplay",
                Translation::Sub,
                &["1".into(), "2".into()],
                None,
                900,
            )
            .unwrap();
        let fb = rig.session.engage(&c, &rig.world.deps(""));
        assert!(fb.is_empty());
        assert_eq!(rig.session.grid(), ["1", "2"]);
        assert!(rig.session.loading().is_none(), "no worker fired");
        assert_eq!(rig.session.drain.inflight(), 0);
    }

    #[test]
    fn stale_cache_is_a_miss_and_refetches() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay").with_episodes(eps(&["1", "2"])),
        ]);
        let c = canonical(5);
        rig.world
            .store
            .bind_provider(&c, "megaplay", "505", 100)
            .unwrap();
        // FINISHED TTL is 7 days; fetched_at far in the past expires it.
        rig.world
            .store
            .set_episode_cache(
                5,
                "megaplay",
                Translation::Sub,
                &["old".into()],
                Some("FINISHED"),
                -EP_TTL_GUARD,
            )
            .unwrap();
        rig.session.engage(&c, &rig.world.deps(""));
        assert!(rig.session.loading().is_some(), "expired cache refetches");
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.grid(), ["1", "2"]);
    }

    const EP_TTL_GUARD: i64 = 8 * 24 * 60 * 60;

    #[test]
    fn empty_listing_marks_absence_and_walks_the_ladder() {
        // megaplay keyed but answers empty; senshi keyed and stocked.
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(Vec::new())),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(
            fb.contains(&Feedback::Hop {
                provider: "senshi".into()
            }),
            "the move to senshi is announced: {fb:?}"
        );
        assert!(
            rig.world
                .store
                .provider_absent_fresh(5, "megaplay", 1_000)
                .unwrap(),
            "authoritative empty marks absence (03 4.3)"
        );
        assert_eq!(rig.session.grid(), ["1"]);
        assert_eq!(rig.session.serving(), Some("senshi"));
        // Landed under senshi: bound there, and the walk is gone.
        assert!(rig.session.walk.is_none());
    }

    #[test]
    fn transient_error_hops_without_marking_absence() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Err(ProviderError::Server { status: 503 })),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1", "2"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.contains(&Feedback::Fail {
            provider: "megaplay".into(),
            class: FetchClass::Down
        }));
        assert!(
            !rig.world
                .store
                .provider_absent_fresh(5, "megaplay", 1_000)
                .unwrap(),
            "errors never mark absence (03 4.3)"
        );
        assert_eq!(rig.session.grid(), ["1", "2"]);
    }

    #[test]
    fn exhausted_walk_dead_ends_into_no_source() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Err(ProviderError::Network)),
            StubProvider::new("senshi").with_search(Err(ProviderError::Network)),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.contains(&Feedback::DeadEnd), "{fb:?}");
        assert!(rig.session.no_source());
        assert!(!rig.session.has_grid());
        assert!(rig.session.loading().is_none());
    }

    #[test]
    fn needs_search_binds_via_id_match_then_fetches() {
        let hit = SearchHit {
            provider_id: "aa-77".into(),
            title: "Show 5".into(),
            mal_id: Some(505),
            ..SearchHit::default()
        };
        // No keys anywhere: tier C. megaplay search is Unsupported (silent),
        // senshi search hits by MAL id.
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay").with_search(Err(ProviderError::Unsupported)),
            StubProvider::new("senshi")
                .with_search(Ok(vec![hit]))
                .with_episodes(eps(&["1", "2"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(
            !fb.iter().any(|f| matches!(
                f,
                Feedback::Fail {
                    class: FetchClass::Unsupported,
                    ..
                }
            )),
            "unsupported search is silent: {fb:?}"
        );
        assert_eq!(rig.session.grid(), ["1", "2"]);
        let bindings = rig.world.store.bindings_for(5).unwrap();
        assert_eq!(bindings[0].provider, "senshi");
        assert_eq!(bindings[0].provider_id, "aa-77");
    }

    #[test]
    fn stale_stamp_forces_pref_and_stamps_before_fetch() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay").with_key("505"),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.world.store.set_route_pref(&c, "megaplay").unwrap();
        rig.session.engage(&c, &rig.world.deps("senshi"));
        // The write-then-fire ordering contract: the stamp is on disk while
        // the forced fetch is still in flight.
        assert_eq!(
            rig.world.store.get_route_pref(5).unwrap().as_deref(),
            Some("senshi"),
            "stamp lands BEFORE the fetch (03 5.3)"
        );
        rig.world.settle(&mut rig.session, "senshi");
        assert_eq!(rig.session.serving(), Some("senshi"));
    }

    #[test]
    fn forced_preferred_miss_continues_bindings_first_k2() {
        // Pref senshi is search-only and misses; a binding exists on
        // allanime. The K-2 continuation must land it, with the distinct
        // no-match copy and no pin-kept path.
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay"),
            StubProvider::new("senshi").with_search(Ok(Vec::new())),
            StubProvider::new("allanime").with_episodes(eps(&["1", "2", "3"])),
        ]);
        let c = canonical(5);
        rig.world
            .store
            .bind_provider(&c, "allanime", "aa-9", 100)
            .unwrap();
        rig.session.engage(&c, &rig.world.deps("senshi"));
        let fb = rig.world.settle(&mut rig.session, "senshi");
        assert!(
            fb.contains(&Feedback::NoMatch {
                provider: "senshi".into()
            }),
            "{fb:?}"
        );
        assert!(!fb.iter().any(|f| matches!(f, Feedback::PinKept { .. })));
        assert_eq!(rig.session.serving(), Some("allanime"));
        assert_eq!(rig.session.grid().len(), 3);
        // Stamp stayed (already advanced); the second open must not re-force.
        assert_eq!(
            rig.world.store.get_route_pref(5).unwrap().as_deref(),
            Some("senshi")
        );
    }

    #[test]
    fn superseded_result_is_dropped_not_installed() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["old"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        let stale = rig.session.generation.current();
        assert!(rig.session.drain(Duration::from_secs(5)));
        // A new show supersedes before the old result is applied.
        rig.session.reset();
        rig.session.for_id = Some(5);
        rig.session.canonical = Some(c.clone());
        let ev = rig.world.rx.try_recv().unwrap();
        if let Event::EpisodesDone {
            anilist_id,
            provider,
            provider_id,
            episodes,
            ..
        } = ev
        {
            let deps = rig.world.deps("");
            let fb =
                rig.session
                    .on_done(anilist_id, &provider, &provider_id, episodes, stale, &deps);
            assert!(fb.is_empty());
        } else {
            panic!("expected EpisodesDone");
        }
        assert!(!rig.session.has_grid(), "stale grid never installs");
        assert!(rig.session.loading().is_none());
    }

    #[test]
    fn cursor_seeds_from_progress_resume_and_completion() {
        let labels: Vec<String> = (1..=5).map(|i| i.to_string()).collect();
        let c = canonical(5);
        // Progress 2 -> next unwatched is index 2.
        let mut rig = Rig::new(vec![StubProvider::new("megaplay")]);
        rig.world.store.add_to_library(&c, 100).unwrap();
        rig.world
            .store
            .save_progress(5, Translation::Sub, "1", 1400.0, 1420.0, None, 150)
            .unwrap();
        rig.world
            .store
            .save_progress(5, Translation::Sub, "2", 1400.0, 1420.0, None, 160)
            .unwrap();
        rig.world
            .store
            .recompute_progress(5, Translation::Sub)
            .unwrap();
        rig.session.for_id = Some(5);
        rig.session.canonical = Some(c.clone());
        rig.session
            .land("megaplay".into(), labels.clone(), &rig.world.deps(""));
        assert_eq!(rig.session.cursor(), 2);
        assert_eq!(rig.session.watched(), 2);
        assert_eq!(rig.session.resume_ix(), None);

        // A partial watch on "4" overrides the next-episode cursor.
        rig.world
            .store
            .save_progress(5, Translation::Sub, "4", 300.0, 1420.0, None, 200)
            .unwrap();
        rig.session
            .land("megaplay".into(), labels.clone(), &rig.world.deps(""));
        assert_eq!(rig.session.resume_ix(), Some(3));
        assert_eq!(rig.session.cursor(), 3, "resume overrides next-episode");

        // Progress past the end wraps the cursor to episode one.
        let mut done = Rig::new(vec![StubProvider::new("megaplay")]);
        done.world.store.add_to_library(&c, 100).unwrap();
        for i in 1..=5 {
            done.world
                .store
                .save_progress(
                    5,
                    Translation::Sub,
                    &i.to_string(),
                    1400.0,
                    1420.0,
                    None,
                    150,
                )
                .unwrap();
        }
        done.world
            .store
            .recompute_progress(5, Translation::Sub)
            .unwrap();
        done.session.for_id = Some(5);
        done.session.canonical = Some(c);
        done.session
            .land("megaplay".into(), labels, &done.world.deps(""));
        assert_eq!(done.session.cursor(), 0, "completed wraps to episode one");
    }

    #[test]
    fn re_engage_same_show_and_track_is_a_noop() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        rig.session.cursor_by(0);
        let before = rig.session.generation.current();
        rig.session.engage(&c, &rig.world.deps(""));
        assert_eq!(rig.session.generation.current(), before, "held, no refire");
        assert_eq!(rig.session.grid(), ["1"]);
    }

    #[test]
    fn track_change_refires_the_session() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"]))
                .with_episodes(eps(&["1", "2"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.grid(), ["1"]);
        let deps = EpisodeDeps {
            translation: Translation::Dub,
            ..rig.world.deps("")
        };
        rig.session.engage(&c, &deps);
        assert!(rig.session.loading().is_some(), "dub flip re-resolves");
    }

    #[test]
    fn pin_cycle_sets_flips_and_clears() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1", "2", "3"])),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1", "2", "3"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));
        rig.session.cursor_by(1);

        // v #1: pins the provider already serving; no hop.
        let fb = rig.session.cycle_pin(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::PinSet {
                provider: "megaplay".into()
            }]
        );
        assert_eq!(
            rig.world.store.get_provider_pin(5).unwrap().as_deref(),
            Some("megaplay")
        );
        assert!(rig.session.loading().is_none(), "no re-route fired");

        // v #2: pins senshi and re-routes through the one-provider flip;
        // the landing keeps the cursor on the in-progress episode.
        let fb = rig.session.cycle_pin(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "senshi".into()
            }]
        );
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.is_empty());
        assert_eq!(rig.session.serving(), Some("senshi"));
        assert_eq!(rig.session.cursor(), 1, "flip landing keeps the cursor");
        assert_eq!(rig.world.store.bindings_for(5).unwrap().len(), 2);

        // v #3: past the last provider wraps to unpinned; never re-routes.
        let fb = rig.session.cycle_pin(&rig.world.deps(""));
        assert_eq!(fb, vec![Feedback::PinCleared]);
        assert_eq!(rig.world.store.get_provider_pin(5).unwrap(), None);
        assert_eq!(rig.session.serving(), Some("senshi"));
    }

    #[test]
    fn pin_flip_miss_keeps_pin_and_grid() {
        // Pre-pinned megaplay; the cycle moves to senshi, which has no key
        // and a failing search. The miss keeps the senshi pin AND the
        // megaplay grid.
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi"),
        ]);
        let c = canonical(5);
        rig.world.store.add_to_library(&c, 50).unwrap();
        rig.world
            .store
            .set_provider_pin(5, Some("megaplay"))
            .unwrap();
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));

        let fb = rig.session.cycle_pin(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "senshi".into()
            }]
        );
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(
            fb.contains(&Feedback::PinKept {
                provider: "senshi".into()
            }),
            "{fb:?}"
        );
        assert_eq!(
            rig.world.store.get_provider_pin(5).unwrap().as_deref(),
            Some("senshi"),
            "the miss keeps the pin (03 5.1)"
        );
        assert_eq!(rig.session.serving(), Some("megaplay"), "grid survives");
        assert_eq!(rig.session.grid(), ["1"]);
    }

    #[test]
    fn retired_pin_wraps_straight_to_unpinned() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.world.store.add_to_library(&c, 50).unwrap();
        rig.world.store.set_provider_pin(5, Some("gogo")).unwrap();
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        let fb = rig.session.cycle_pin(&rig.world.deps(""));
        assert_eq!(fb, vec![Feedback::PinCleared]);
        assert_eq!(rig.world.store.get_provider_pin(5).unwrap(), None);
        assert!(rig.session.loading().is_none(), "no re-route on a retire");
    }

    #[test]
    fn pin_gates_on_source_and_inflight() {
        // Nothing engaged: nothing to pin.
        let mut rig = Rig::new(vec![StubProvider::new("megaplay")]);
        let fb = rig.session.cycle_pin(&rig.world.deps(""));
        assert_eq!(fb, vec![Feedback::PinNothing]);

        // Fetch in flight: still resolving.
        let mut busy = Rig::new(vec![StubProvider::new("megaplay").with_key("505")]);
        busy.session.engage(&canonical(5), &busy.world.deps(""));
        assert!(busy.session.loading().is_some());
        let fb = busy.session.cycle_pin(&busy.world.deps(""));
        assert_eq!(fb, vec![Feedback::PinPending]);
        busy.world.settle(&mut busy.session, "");

        // Exhausted no-source state: nothing to pin either.
        assert!(busy.session.no_source());
        let fb = busy.session.cycle_pin(&busy.world.deps(""));
        assert_eq!(fb, vec![Feedback::PinNothing]);
    }

    #[test]
    fn engage_refreshes_pin_and_availability_for_the_rail() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi"),
        ]);
        let c = canonical(5);
        rig.world.store.add_to_library(&c, 50).unwrap();
        rig.world.store.set_provider_pin(5, Some("senshi")).unwrap();
        rig.session.engage(&c, &rig.world.deps(""));
        assert_eq!(rig.session.pin(), Some("senshi"));
        assert_eq!(
            rig.session.avail(),
            [
                ("megaplay".to_string(), ProviderAvailability::Unchecked),
                ("senshi".to_string(), ProviderAvailability::Unchecked),
            ]
        );
        rig.world.settle(&mut rig.session, "");
        // The landing minted the megaplay binding; availability follows.
        assert_eq!(
            rig.session.avail()[0],
            ("megaplay".to_string(), ProviderAvailability::Bound)
        );
    }

    #[test]
    fn cursor_nav_clamps_and_jumps() {
        let mut s = EpisodeSession {
            episodes: vec!["1".into(), "2".into(), "3".into()],
            ..EpisodeSession::default()
        };
        s.cursor_by(1);
        s.cursor_by(1);
        s.cursor_by(5);
        assert_eq!(s.cursor(), 2, "clamps at the end");
        s.cursor_by(-9);
        assert_eq!(s.cursor(), 0);
        s.cursor_end(false);
        assert_eq!(s.cursor(), 2);
        s.cursor_end(true);
        assert_eq!(s.cursor(), 0);
        let mut empty = EpisodeSession::default();
        empty.cursor_by(1);
        assert_eq!(empty.cursor(), 0);
    }
}
