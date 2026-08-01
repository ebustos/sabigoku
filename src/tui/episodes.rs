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
use crate::resolve::{self, Exhausted, Hop, ResolveTarget, ResolveWorld, Walk};
use crate::resolver;
use crate::store::{ProviderAvailability, Store};
use crate::tui::clock::{AsyncStart, Debounce};
use crate::tui::event::{EventTx, FetchClass};
use crate::tui::workers::{self, Drain, EpisodeFetch, Generation, ProviderSearch};

/// `v` settle window (ROD-524): matches the search debounce cadence.
pub const PIN_SETTLE: Duration = Duration::from_millis(300);

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

    fn last_used(&self, anilist_id: i64) -> Option<String> {
        self.store.get_last_used(anilist_id).ok().flatten()
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
    /// The walk exhausted the circle with nothing landed. The one exhaust
    /// (ROD-525): silence is never an outcome.
    DeadEnd,
    /// `v` before the first grid resolves.
    CyclePending,
    /// `v` with no episode source to walk from.
    CycleNothing,
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
    /// Last-used + per-provider availability, refreshed on engage and on
    /// every bind/absence write (03 §6.1 step 2) so draw never reads the
    /// store.
    last_used: Option<String>,
    /// The `v` cycle's aim, in memory only (ROD-524/525): a burst advances
    /// it paying no fetches, the settle starts the manual walk there, the
    /// landing writes last-used. Abandoning the show just drops it.
    cycle_aim: Option<String>,
    cycle_debounce: Debounce,
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
        self.last_used = None;
        self.cycle_aim = None;
        self.cycle_debounce.disarm();
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
                let walk = Walk::auto(&deps.world(), canonical.clone(), None);
                self.walk = Some(walk);
                self.advance_walk(false, deps)
            }
        }
    }

    /// Tier 0: an unexpired cached listing paints without a fetch (03 §6.1
    /// step 7); the DB cache is the one listing cache, no in-memory twin.
    /// True when answered or a worker is in flight.
    fn open_bound(&mut self, provider: String, id: String, deps: &EpisodeDeps) -> bool {
        let aid = self.for_id.unwrap_or_default();
        if let Ok(Some(cached)) =
            deps.store
                .get_cached_episodes(aid, &provider, deps.translation, deps.unix_now)
        {
            self.land(provider, cached, deps);
            return true;
        }
        self.fire_fetch(provider, id, false, deps)
    }

    fn fire_fetch(
        &mut self,
        provider: String,
        provider_id: String,
        mint: bool,
        deps: &EpisodeDeps,
    ) -> bool {
        let Some(canonical) = self.canonical.as_ref() else {
            return false;
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
        spawned
    }

    fn fire_search(&mut self, provider: String, deps: &EpisodeDeps) -> bool {
        let Some(canonical) = self.canonical.as_ref() else {
            return false;
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
        spawned
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
                let running = if bind.is_some() {
                    self.fire_fetch(provider.clone(), id, true, deps)
                } else {
                    self.open_bound(provider.clone(), id, deps)
                };
                if !running {
                    fb.extend(self.hop_unreachable(deps));
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
                if !self.fire_search(provider.clone(), deps) {
                    fb.extend(self.hop_unreachable(deps));
                }
                fb
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

    /// A hop whose worker never started: no event will ever advance the walk,
    /// so walk on to the next provider instead of leaving it dangling
    /// (ROD-525: a spawn failure is one more miss, never a stop). Bounded by
    /// the circle; the exhaust is the ordinary DeadEnd.
    fn hop_unreachable(&mut self, deps: &EpisodeDeps) -> Vec<Feedback> {
        self.advance_walk(false, deps)
    }

    /// Failed or empty fetch: begin/continue the fallback walk (03 §6.4). A
    /// fresh walk pre-marks the failed provider; a live walk is already past
    /// its hop.
    fn fail_over(&mut self, failed: &str, deps: &EpisodeDeps) -> Vec<Feedback> {
        if self.walk.is_none() {
            let Some(canonical) = self.canonical.clone() else {
                return Vec::new();
            };
            self.walk = Some(Walk::auto(&deps.world(), canonical, Some(failed)));
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
        let mut fb = Vec::new();
        // Unsupported is silent here for the same reason as in the search
        // arm: a capability gap is routine, not a failure the user must see.
        // Unreachable today (every provider implements episodes), kept
        // symmetric so a future provider can't regress the copy.
        if class != FetchClass::Unsupported {
            fb.push(Feedback::Fail {
                provider: provider.to_string(),
                class,
            });
        }
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
        // The single walk retirement point (05 §10.3): a hop can land
        // synchronously from the episode cache with no worker event, so any
        // clear that lives only on the event path leaks an armed walk, which
        // silently eats play fail-overs and holds prewarm off all session.
        self.walk = None;
        self.walk_provider = None;
        self.no_source = false;
        self.loading = None;
        // Last-used is a confirmation write (03 §5.1, ROD-525): minted only
        // here, only when it differs from the walk-order head, best-effort.
        let head = if deps.global_pref.is_empty() {
            deps.registry.primary().name()
        } else {
            deps.global_pref
        };
        let _ = deps
            .store
            .set_last_used(aid, (provider != head).then_some(provider.as_str()));
        self.last_used = deps.store.get_last_used(aid).ok().flatten();
        // A landing resolves the aim, except one still inside its window: a
        // burst in flight is the user's live press, never the walk's to eat.
        if !self.cycle_debounce.is_armed() {
            self.cycle_aim = None;
        }
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

    /// Last-used + availability snapshot for the meta rail (03 §6.1 step 2).
    /// Read failures degrade to unchecked; the rail dims, nothing wedges.
    /// The cycle aim lives beside this, never in it, so a landing mid-window
    /// cannot clobber the user's cycle.
    fn refresh_meta(&mut self, deps: &EpisodeDeps) {
        let Some(aid) = self.for_id else { return };
        self.last_used = deps.store.get_last_used(aid).ok().flatten();
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

    /// `v` walk (03 §5.2, DESIGN 6.1, ROD-525): advance the aim one provider
    /// through the registry circle, wrapping past the end. Each press moves
    /// the aim in memory and re-arms the settle window; the walk waits for
    /// `maybe_commit_cycle`, so a burst pays no intermediate fetches. There
    /// is no store write anywhere in the cycle: last-used is minted by the
    /// landing. Only a grid-less resolve blocks the cycle; a walk in flight
    /// is cycled past freely and superseded at settle.
    pub fn cycle_provider(&mut self, deps: &EpisodeDeps) -> Vec<Feedback> {
        if self.for_id.is_none() {
            return vec![Feedback::CycleNothing];
        }
        if self.serving.is_none() {
            return if self.loading.is_some() {
                vec![Feedback::CyclePending]
            } else {
                vec![Feedback::CycleNothing]
            };
        }
        let order: Vec<String> = deps.registry.iter().map(|p| p.name().to_string()).collect();
        // Base: the live aim inside a burst, else the serving provider. A
        // serving name outside the registry degrades to the circle head.
        let base = self.cycle_aim.as_deref().or(self.serving.as_deref());
        let ix = base.and_then(|b| order.iter().position(|n| n == b));
        let next = match ix {
            Some(i) => order[(i + 1) % order.len()].clone(),
            None => order.first().cloned().unwrap_or_default(),
        };
        self.cycle_debounce.arm(deps.now, PIN_SETTLE);
        self.cycle_aim = Some(next.clone());
        vec![Feedback::Hop { provider: next }]
    }

    /// Settle side of the `v` walk, ticked from `App::on_tick`: at most one
    /// manual walk per burst, started at the settled aim. The walk wraps the
    /// circle on its own; a miss walks on (ROD-525).
    pub fn maybe_commit_cycle(&mut self, deps: &EpisodeDeps) -> Vec<Feedback> {
        if !self.cycle_debounce.fire(deps.now) {
            return Vec::new();
        }
        // Clone, not take: the aim outlives the settle so a press during the
        // walk it started advances PAST the hanging provider instead of
        // re-aiming at it. The landing clears it.
        let Some(target) = self.cycle_aim.clone() else {
            return Vec::new();
        };
        if self.serving.as_deref() == Some(target.as_str()) {
            // Wrapped back onto the serving provider: nothing to walk; a
            // stale walk from an earlier settle dies instead of landing a
            // provider the user cycled past.
            self.cycle_aim = None;
            self.drop_stale_walk();
            return Vec::new();
        }
        let Some(canonical) = self.canonical.clone() else {
            return Vec::new();
        };
        self.drop_stale_walk();
        self.remap_from = self
            .episodes
            .get(self.cursor)
            .map(|label| (label.clone(), self.cursor as u32 + 1));
        self.walk = Some(Walk::manual(&deps.world(), canonical, &target));
        self.advance_walk(true, deps)
    }

    /// A settle that needs no walk while an earlier one is still in flight:
    /// letting it land would flip the grid to a provider the user already
    /// cycled past, so it dies here.
    fn drop_stale_walk(&mut self) {
        if self.walk.is_none() && self.loading.is_none() {
            return;
        }
        self.generation.bump();
        self.walk = None;
        self.walk_provider = None;
        self.loading = None;
        self.mint_pending = false;
        self.remap_from = None;
    }

    /// Play-fallback hop (03 §6.4): a failed play walks to a sibling exactly
    /// like a failed listing, except no absence is marked (a transient play
    /// failure says nothing about stock) and the walk inherits every provider
    /// the continuation already burned. Single-flight: a live fetch or walk
    /// wins and the ask is dropped. `remap` keeps the cursor on the
    /// in-progress episode across the hop landing (05 §10.5).
    pub fn play_fail_over(
        &mut self,
        tried: &[String],
        remap: (String, u32),
        deps: &EpisodeDeps,
    ) -> Vec<Feedback> {
        if self.loading.is_some() || self.walk.is_some() {
            return Vec::new();
        }
        let Some(canonical) = self.canonical.clone() else {
            return Vec::new();
        };
        let mut walk = Walk::auto(&deps.world(), canonical, None);
        for provider in tried {
            walk.mark_tried(provider);
        }
        self.remap_from = Some((remap.0, remap.1));
        self.walk_provider = None;
        self.walk = Some(walk);
        self.advance_walk(true, deps)
    }

    /// Post-play refresh (05 §11, DESIGN 4.6): a recorded finish re-derives
    /// the watched high-water and resume point from the store. A completed
    /// watch advances the cursor off the played cell, but only when it still
    /// sits there; the grid stayed navigable during play and a moved cursor
    /// holds. Cross-show playback never touches this session (the id gate).
    pub fn on_play_recorded(
        &mut self,
        anilist_id: i64,
        episode_ix: u32,
        completed: bool,
        deps: &EpisodeDeps,
    ) {
        if !self.is_for(anilist_id) || self.episodes.is_empty() {
            return;
        }
        self.watched = deps
            .store
            .get_show(anilist_id)
            .ok()
            .flatten()
            .map_or(self.watched, |s| s.progress);
        self.resume_ix = deps
            .store
            .latest_resume(anilist_id, deps.translation)
            .ok()
            .flatten()
            .and_then(|(label, _)| self.episodes.iter().position(|e| *e == label));
        let played = episode_ix.saturating_sub(1) as usize;
        if completed && self.cursor == played {
            self.cursor = (played + 1).min(self.episodes.len() - 1);
        }
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

    /// The remembered (last-used) provider for the rail, or, inside a live
    /// settle window, the cycle aim the user is driving.
    pub fn remembered(&self) -> Option<&str> {
        if self.cycle_debounce.is_armed() {
            return self.cycle_aim.as_deref();
        }
        self.last_used.as_deref()
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

    /// A fallback walk is armed (03 §6.4); the prewarm walk yields to it.
    pub fn walk_active(&self) -> bool {
        self.walk.is_some()
    }

    /// A background write changed availability for `anilist_id` (prewarm,
    /// 05 §10.4): refresh the rail when it is the engaged show.
    pub fn on_availability_write(&mut self, anilist_id: i64, deps: &EpisodeDeps) {
        if self.for_id == Some(anilist_id) {
            self.refresh_meta(deps);
        }
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
        last_used: Option<&str>,
        avail: Vec<(String, ProviderAvailability)>,
        episodes: Vec<String>,
    ) -> EpisodeSession {
        EpisodeSession {
            for_id: Some(for_id),
            serving: serving.map(str::to_string),
            last_used: last_used.map(str::to_string),
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
            .recompute_progress(5, Translation::Sub, 170)
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
            .recompute_progress(5, Translation::Sub, 170)
            .unwrap();
        done.session.for_id = Some(5);
        done.session.canonical = Some(c);
        done.session
            .land("megaplay".into(), labels, &done.world.deps(""));
        assert_eq!(done.session.cursor(), 0, "completed wraps to episode one");
    }

    /// ROD-477 repro: an abandoned partial behind the watched frontier must
    /// not seed the resume cell or steal the cursor from next-unwatched.
    #[test]
    fn stale_partial_behind_the_frontier_never_seeds_resume() {
        let labels: Vec<String> = (1..=12).map(|i| i.to_string()).collect();
        let c = canonical(5);
        let mut rig = Rig::new(vec![StubProvider::new("megaplay")]);
        rig.world.store.add_to_library(&c, 100).unwrap();
        rig.world
            .store
            .save_progress(5, Translation::Sub, "9", 40.0, 100.0, None, 500)
            .unwrap();
        for (ep, ix, at) in [("10", 10, 600), ("11", 11, 700)] {
            rig.world
                .store
                .record_finish(5, Translation::Sub, ep, ix, 96.0, 100.0, None, at)
                .unwrap();
        }
        rig.session.for_id = Some(5);
        rig.session.canonical = Some(c);
        rig.session
            .land("megaplay".into(), labels, &rig.world.deps(""));
        assert_eq!(rig.session.watched(), 11);
        assert_eq!(rig.session.resume_ix(), None, "ep 9 partial is dead");
        assert_eq!(rig.session.cursor(), 11, "seeds next-unwatched, ep 12");
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
    fn cycle_aims_walks_and_remembers_on_landing() {
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

        // v: aims the next provider in the circle; nothing is written or
        // fired until the window settles.
        let fb = rig.session.cycle_provider(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "senshi".into()
            }]
        );
        assert_eq!(
            rig.session.remembered(),
            Some("senshi"),
            "rail follows the aim"
        );
        assert_eq!(
            rig.world.store.get_last_used(5).unwrap(),
            None,
            "no write inside the window"
        );
        assert!(
            rig.session
                .maybe_commit_cycle(&rig.world.deps(""))
                .is_empty(),
            "window not elapsed"
        );
        rig.world.now += PIN_SETTLE;
        let fb = rig.session.maybe_commit_cycle(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "senshi".into()
            }]
        );
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.is_empty());
        assert_eq!(rig.session.serving(), Some("senshi"));
        assert_eq!(rig.session.cursor(), 1, "walk landing keeps the cursor");
        assert_eq!(rig.world.store.bindings_for(5).unwrap().len(), 2);
        assert_eq!(
            rig.world.store.get_last_used(5).unwrap().as_deref(),
            Some("senshi"),
            "the landing is the confirmation write"
        );
        assert_eq!(rig.session.remembered(), Some("senshi"), "aim resolved");

        // v again: wraps past the end of the circle back to megaplay; the
        // landing matches the walk-order head, so the row clears.
        let fb = rig.session.cycle_provider(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "megaplay".into()
            }]
        );
        rig.world.now += PIN_SETTLE;
        rig.session.maybe_commit_cycle(&rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));
        assert_eq!(
            rig.world.store.get_last_used(5).unwrap(),
            None,
            "landing on the head clears the memory"
        );
    }

    /// ROD-524/525: a burst of presses is one walk on settle; providers
    /// cycled past inside the window are never fetched, and nothing is
    /// written until the landing.
    #[test]
    fn cycle_burst_walks_once_and_skips_intermediates() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi"),
            StubProvider::new("anibd")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));

        // Two presses 100ms apart: senshi, then anibd; each re-arms.
        rig.session.cycle_provider(&rig.world.deps(""));
        rig.world.now += Duration::from_millis(100);
        let fb = rig.session.cycle_provider(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "anibd".into()
            }]
        );

        // 200ms later the last press's window is still open.
        rig.world.now += Duration::from_millis(200);
        assert!(
            rig.session
                .maybe_commit_cycle(&rig.world.deps(""))
                .is_empty()
        );
        assert_eq!(rig.world.store.get_last_used(5).unwrap(), None);

        rig.world.now += Duration::from_millis(100);
        let fb = rig.session.maybe_commit_cycle(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "anibd".into()
            }],
            "one walk, straight to the settled aim"
        );
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.is_empty(), "senshi was never touched");
        assert_eq!(rig.session.serving(), Some("anibd"));
        assert_eq!(
            rig.world.store.get_last_used(5).unwrap().as_deref(),
            Some("anibd"),
            "the landing writes the memory"
        );
    }

    /// ROD-524 core, ROD-525 shape: a walk in flight never blocks the
    /// cycle, and a settle whose aim wrapped back to serving kills the stale
    /// walk instead of letting it land.
    #[test]
    fn cycle_during_inflight_walk_supersedes() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));

        // Aim senshi and let its walk fire.
        rig.session.cycle_provider(&rig.world.deps(""));
        rig.world.now += PIN_SETTLE;
        rig.session.maybe_commit_cycle(&rig.world.deps(""));
        assert!(rig.session.loading().is_some(), "senshi walk in flight");

        // A walk in flight never blocks the cycle; the aim advances PAST
        // the hanging provider and wraps to serving.
        let fb = rig.session.cycle_provider(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "megaplay".into()
            }],
            "the press advances from the aim, not from serving"
        );
        rig.world.now += PIN_SETTLE;
        assert!(
            rig.session
                .maybe_commit_cycle(&rig.world.deps(""))
                .is_empty()
        );
        assert!(rig.session.loading().is_none(), "stale walk dropped");
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.is_empty(), "superseded landing is silent");
        assert_eq!(
            rig.session.serving(),
            Some("megaplay"),
            "grid never flips to the provider the user cycled past"
        );
        assert_eq!(rig.world.store.get_last_used(5).unwrap(), None);
    }

    /// ROD-525: a walk hop can land synchronously from the episode cache
    /// with no worker event; the walk must retire at the landing. Leaked
    /// armed, it silently eats every later play fail-over and holds prewarm
    /// off for the rest of the session.
    #[test]
    fn cache_hit_walk_landing_retires_the_walk() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.world.store.add_to_library(&c, 50).unwrap();
        // Both bound + fresh-cached: engage and the flip hop both land
        // without a worker.
        for p in ["megaplay", "senshi"] {
            rig.world.store.bind_provider(&c, p, "505", 100).unwrap();
            rig.world
                .store
                .set_episode_cache(5, p, Translation::Sub, &["1".into()], Some("FINISHED"), 100)
                .unwrap();
        }
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));

        rig.session.cycle_provider(&rig.world.deps(""));
        rig.world.now += PIN_SETTLE;
        let fb = rig.session.maybe_commit_cycle(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "senshi".into()
            }]
        );
        assert_eq!(rig.session.serving(), Some("senshi"), "landed from cache");
        assert!(rig.session.loading().is_none());
        assert!(
            !rig.session.walk_active(),
            "a synchronous landing retires the walk"
        );
    }

    /// ROD-525 acceptance: a play failure on the serving provider always
    /// answers, a hop while a sibling is reachable, DeadEnd when none is.
    /// Never silence. Original repro: the flip landed from cache, the leaked
    /// walk swallowed the fail-over.
    #[test]
    fn play_fail_over_after_cache_landing_walks_never_silent() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.world.store.add_to_library(&c, 50).unwrap();
        // Both bound + fresh-cached: engage and the flip hop both land
        // without a worker.
        for p in ["megaplay", "senshi"] {
            rig.world.store.bind_provider(&c, p, "505", 100).unwrap();
            rig.world
                .store
                .set_episode_cache(5, p, Translation::Sub, &["1".into()], Some("FINISHED"), 100)
                .unwrap();
        }
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        rig.session.cycle_provider(&rig.world.deps(""));
        rig.world.now += PIN_SETTLE;
        rig.session.maybe_commit_cycle(&rig.world.deps(""));
        assert_eq!(rig.session.serving(), Some("senshi"));

        // senshi's play failed; the fail-over must hop to megaplay, and the
        // hop lands synchronously off megaplay's own cached grid.
        let fb =
            rig.session
                .play_fail_over(&["senshi".into()], ("1".into(), 1), &rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "megaplay".into()
            }],
            "the fail-over answers, never silence"
        );
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));
        assert!(!rig.session.walk_active());

        // With the sibling freshly absent instead, the answer is DeadEnd.
        let mut dead = Rig::new(vec![
            StubProvider::new("megaplay").with_key("505"),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(6);
        dead.world.store.add_to_library(&c, 50).unwrap();
        dead.world
            .store
            .mark_provider_absent(&c, "megaplay", 900)
            .unwrap();
        dead.session.engage(&c, &dead.world.deps(""));
        dead.world.settle(&mut dead.session, "");
        assert_eq!(dead.session.serving(), Some("senshi"));
        let fb =
            dead.session
                .play_fail_over(&["senshi".into()], ("1".into(), 1), &dead.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::DeadEnd],
            "an exhausted fail-over says so, never an empty vec"
        );
    }

    /// ROD-524/525: `v` during a live play fail-over is accepted, and a
    /// settle whose aim wrapped back to serving kills the recovery walk: the
    /// user's explicit choice outranks the hunt, no stuck spinner, no toast
    /// from the discarded walk.
    #[test]
    fn cycle_to_serving_during_play_fail_over_supersedes_the_recovery() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1", "2"])),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1", "2"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));

        let fb =
            rig.session
                .play_fail_over(&["megaplay".into()], ("1".into(), 1), &rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "senshi".into()
            }]
        );
        assert!(rig.session.loading().is_some(), "recovery fetch in flight");

        // Two presses wrap the aim back onto serving; the settle kills the
        // recovery instead of walking.
        rig.session.cycle_provider(&rig.world.deps(""));
        let fb = rig.session.cycle_provider(&rig.world.deps(""));
        assert_eq!(
            fb,
            vec![Feedback::Hop {
                provider: "megaplay".into()
            }]
        );
        rig.world.now += PIN_SETTLE;
        assert!(
            rig.session
                .maybe_commit_cycle(&rig.world.deps(""))
                .is_empty()
        );
        assert!(rig.session.loading().is_none(), "no stuck spinner");
        let fb = rig.world.settle(&mut rig.session, "");
        assert!(fb.is_empty(), "the discarded walk lands silently");
        assert_eq!(rig.session.serving(), Some("megaplay"));
    }

    /// ROD-524/525: an earlier walk landing inside a newer window must not
    /// eat the live aim; the burst is the user's press, and its settle still
    /// fires.
    #[test]
    fn landing_mid_window_never_eats_the_live_aim() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));

        // Aim senshi, let the walk fire, then press again (wraps to
        // megaplay) while the senshi fetch is still in flight.
        rig.session.cycle_provider(&rig.world.deps(""));
        rig.world.now += PIN_SETTLE;
        rig.session.maybe_commit_cycle(&rig.world.deps(""));
        rig.session.cycle_provider(&rig.world.deps(""));

        // The senshi landing arrives inside the new window: current token,
        // grid flips, but the armed aim survives.
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("senshi"));
        assert_eq!(
            rig.session.remembered(),
            Some("megaplay"),
            "the landing resolves its own walk, never the live press"
        );

        // The settle then honors the press: back to megaplay off its cache.
        rig.world.now += PIN_SETTLE;
        rig.session.maybe_commit_cycle(&rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));
    }

    /// ROD-525: leaving the show inside the window drops the aim whole:
    /// nothing fired, nothing written, nothing remembered for a show the
    /// user walked away from.
    #[test]
    fn show_switch_drops_the_cycle_aim() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"]))
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(eps(&["1"])),
        ]);
        let c = canonical(5);
        rig.session.engage(&c, &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"));

        rig.session.cycle_provider(&rig.world.deps(""));
        assert_eq!(rig.session.remembered(), Some("senshi"));
        rig.session.engage(&canonical(6), &rig.world.deps(""));
        rig.world.settle(&mut rig.session, "");
        assert_eq!(rig.session.serving(), Some("megaplay"), "show 6 resolves");
        assert_eq!(
            rig.world.store.get_last_used(5).unwrap(),
            None,
            "the abandoned aim never wrote"
        );
        rig.world.now += PIN_SETTLE;
        assert!(
            rig.session
                .maybe_commit_cycle(&rig.world.deps(""))
                .is_empty(),
            "the dropped window never fires"
        );
        assert!(!rig.session.walk_active());
    }

    #[test]
    fn cycle_gates_on_source_and_first_resolve() {
        // Nothing engaged: nothing to walk from.
        let mut rig = Rig::new(vec![StubProvider::new("megaplay")]);
        let fb = rig.session.cycle_provider(&rig.world.deps(""));
        assert_eq!(fb, vec![Feedback::CycleNothing]);

        // First resolve in flight: still resolving.
        let mut busy = Rig::new(vec![StubProvider::new("megaplay").with_key("505")]);
        busy.session.engage(&canonical(5), &busy.world.deps(""));
        assert!(busy.session.loading().is_some());
        let fb = busy.session.cycle_provider(&busy.world.deps(""));
        assert_eq!(fb, vec![Feedback::CyclePending]);
        busy.world.settle(&mut busy.session, "");

        // Exhausted no-source state: nothing to walk from either.
        assert!(busy.session.no_source());
        let fb = busy.session.cycle_provider(&busy.world.deps(""));
        assert_eq!(fb, vec![Feedback::CycleNothing]);
    }

    #[test]
    fn engage_refreshes_last_used_and_availability_for_the_rail() {
        let mut rig = Rig::new(vec![
            StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(eps(&["1"])),
            StubProvider::new("senshi"),
        ]);
        let c = canonical(5);
        rig.world.store.add_to_library(&c, 50).unwrap();
        rig.world.store.set_last_used(5, Some("senshi")).unwrap();
        rig.session.engage(&c, &rig.world.deps(""));
        assert_eq!(rig.session.remembered(), Some("senshi"));
        assert_eq!(
            rig.session.avail(),
            [
                ("megaplay".to_string(), ProviderAvailability::Unchecked),
                ("senshi".to_string(), ProviderAvailability::Unchecked),
            ]
        );
        rig.world.settle(&mut rig.session, "");
        // senshi cannot answer (no key, no search hit), the walk lands
        // megaplay, and the landing REWRITES the memory: megaplay is the
        // walk-order head, so the row clears (ROD-525 confirmation write).
        assert_eq!(
            rig.session.avail()[0],
            ("megaplay".to_string(), ProviderAvailability::Bound)
        );
        assert_eq!(rig.world.store.get_last_used(5).unwrap(), None);
        assert_eq!(rig.session.remembered(), None);
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
