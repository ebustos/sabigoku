//! Resolve orchestration (03 §4-5): the tier classifier, the two pin
//! asymmetries, the preferred re-route with its stamp-before-fetch guard, and
//! the fallback walk carrying the origin tag that fixes bug K-2.
//!
//! Headless by design. Every decision is a pure function over a
//! `ResolveWorld` (the store + registry reads it needs) returning an action;
//! the TUI worker layer (ROD-439) performs the fetch/search/stamp the action
//! names. No threads, no network, no `Store` here, so the walk contracts are
//! pinned by offline tests.
//!
//! Two distinct pin rules live here and must not be merged (03 §4.1):
//! Path 1 (canonical open) folds the pin into effective preference, so ANY
//! binding wins tier 0 (a non-pinned binding beats an unbound pin). Path 2
//! (History open) treats the pin as a hard restriction: only the pin's own
//! binding is consulted, never borrowed from another provider.

use crate::domain::Enrichment;

/// Read-only view the engine needs. The real impl reads `Store` + the
/// `ProviderRegistry`; tests fake it. Provider names are the stable
/// persistence keys (03 §2).
pub trait ResolveWorld {
    /// Registry order with `pref` first when live, then construction order
    /// (03 §3.2). Empty/unknown `pref` yields plain construction order.
    fn ordered(&self, pref: Option<&str>) -> Vec<String>;

    /// Whether `provider` is a live registry member (a retired pin name is
    /// not: it must never fetch its foreign id on primary, 03 §5.1).
    fn registered(&self, provider: &str) -> bool;

    /// Stored binding provider_id for `(anilist_id, provider)`.
    fn binding(&self, anilist_id: i64, provider: &str) -> Option<String>;

    /// Fresh (within-TTL) absence (03 §5.2, 7-day TTL is the store's).
    fn absent_fresh(&self, anilist_id: i64, provider: &str) -> bool;

    /// Tier-A key: pure derivation from the canonical (03 §2). None means the
    /// provider does not id-key on canonical, NOT "not stocked".
    fn canonical_key(&self, provider: &str, canonical: &Enrichment) -> Option<String>;

    /// Per-show pin (03 §5.1); at most one provider.
    fn pin(&self, anilist_id: i64) -> Option<String>;

    /// The `preferred_provider` this show last settled under (03 §5.3).
    fn route_pref(&self, anilist_id: i64) -> Option<String>;

    /// Global config `preferred_provider`; empty = follow-leader / unset.
    fn global_pref(&self) -> String;
}

/// Effective preference for a show: pin overrides global (03 §5.1). Empty
/// global config reads as unset.
pub fn effective_pref(world: &dyn ResolveWorld, anilist_id: i64) -> Option<String> {
    if let Some(pin) = world.pin(anilist_id) {
        return Some(pin);
    }
    let g = world.global_pref();
    if g.is_empty() { None } else { Some(g) }
}

/// Classifier verdict for a canonical open (03 §4). Under 02 identity every
/// catalog open is AniList-keyed, so there is no provider-keyed `.direct` arm
/// (03 §11): the input is always a canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveTarget {
    /// Existing binding; fetch with the stored id (03 tier 0).
    Bound {
        provider: String,
        id: String,
        anilist_id: i64,
    },
    /// Provider derives a catalog key; fetch, then mint the binding on
    /// success (03 tier A).
    TierA {
        provider: String,
        key: String,
        anilist_id: i64,
    },
    /// No binding, no key: tier-C title search across the effective order.
    NeedsSearch { anilist_id: i64 },
}

/// Path 1 canonical open (Browse open, Discover zoom, add-to-watchlist).
///
/// TIER-major, not provider-major (03 §4, ROD-343): any existing binding
/// beats a fresh key on an earlier provider. Within a tier, effective order
/// breaks ties. The pin has already folded into `effective_pref`, so a
/// non-pinned binding can win tier 0 over an unbound pin here; that is the
/// Path 1 side of the pin asymmetry.
pub fn classify_open(world: &dyn ResolveWorld, canonical: &Enrichment) -> ResolveTarget {
    let aid = canonical.anilist_id;
    let pref = effective_pref(world, aid);
    let order = world.ordered(pref.as_deref());

    for p in &order {
        if let Some(id) = world.binding(aid, p) {
            return ResolveTarget::Bound {
                provider: p.clone(),
                id,
                anilist_id: aid,
            };
        }
    }
    for p in &order {
        if let Some(key) = world.canonical_key(p, canonical) {
            return ResolveTarget::TierA {
                provider: p.clone(),
                key,
                anilist_id: aid,
            };
        }
    }
    ResolveTarget::NeedsSearch { anilist_id: aid }
}

/// One History row's routing inputs (03 §4.1 Path 2). `source`/`source_id`
/// are the owning provider on the row; `canonical` feeds the unpinned
/// re-route's tier-A/C.
pub struct HistoryRecord<'a> {
    pub anilist_id: Option<i64>,
    pub source: &'a str,
    pub source_id: &'a str,
    pub canonical: &'a Enrichment,
}

/// Path 2 History open decision.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryOpen {
    /// Pin hard-restriction hit: open the pin's own binding, nothing else.
    PinBinding { provider: String, id: String },
    /// Unpinned re-route fired (03 §5.3); the route action carries the rest.
    Routed(RouteOutcome),
    /// Open the record's own provider/id.
    Record { provider: String, id: String },
}

/// Path 2 History record open (03 §4.1). The pin is a HARD restriction here:
/// only the pin's own binding is looked up. An unbound pin never borrows
/// another provider's binding; control falls through to the record's provider
/// (with the pin set, the unpinned re-route no-ops). Unpinned opens run the
/// preferred re-route before the record's provider.
pub fn open_history(world: &dyn ResolveWorld, rec: &HistoryRecord) -> HistoryOpen {
    if let Some(aid) = rec.anilist_id {
        if let Some(pin) = world.pin(aid) {
            // Pin arm: only pin != source, registered, and bound opens the
            // pin. Retired / same-as-source / unbound pin falls through, and
            // with the pin set the re-route stays a no-op.
            if pin != rec.source
                && world.registered(&pin)
                && let Some(id) = world.binding(aid, &pin)
            {
                return HistoryOpen::PinBinding { provider: pin, id };
            }
        } else {
            debug_assert_eq!(aid, rec.canonical.anilist_id);
            let routed = route_preferred(world, rec.canonical);
            if !matches!(routed.action, RouteAction::None) {
                return HistoryOpen::Routed(routed);
            }
        }
    }
    HistoryOpen::Record {
        provider: rec.source.to_string(),
        id: rec.source_id.to_string(),
    }
}

/// Preferred re-route action (03 §5.3, ROD-398). Every forcing outcome
/// carries `stamp`: the transport MUST write `route_pref = stamp` BEFORE it
/// fires the action, so a miss reads non-stale on the next open and cannot
/// loop forever.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteOutcome {
    /// Route pref to persist before firing (stamp-before-fetch). None when
    /// nothing forces (already settled, or no route).
    pub stamp: Option<String>,
    pub action: RouteAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouteAction {
    /// Open this binding under pref (tier 0).
    OpenBinding { provider: String, id: String },
    /// Fetch the tier-A key; mint the binding on success.
    FetchTierA {
        provider: String,
        key: String,
        anilist_id: i64,
    },
    /// Single-provider forced tier-C walk (origin `ForcedPreferred`). Its
    /// miss triggers the K-2 law, not a dead-end. Boxed: a `Walk` owns a
    /// canonical, far larger than the other variants.
    Walk(Box<Walk>),
    /// No route; the caller opens normally.
    None,
}

/// Route an open under the live global preference (03 §5.3). Pin overrides
/// (returns `None`). When the show already settled under the live pref, its
/// binding opens directly with no re-stamp. When stale or never settled, the
/// pref is forced once through its own tier 0 / A / C, stamping first.
pub fn route_preferred(world: &dyn ResolveWorld, canonical: &Enrichment) -> RouteOutcome {
    // Single-source the id from the canonical. A separate id parameter would
    // let the stamp/lookups run under one show while the forced Walk (which
    // reads canonical.anilist_id) runs under another, minting a binding on the
    // wrong show (ROD-436 review).
    let anilist_id = canonical.anilist_id;
    let none = RouteOutcome {
        stamp: None,
        action: RouteAction::None,
    };

    let pref = world.global_pref();
    if pref.is_empty() {
        return none; // follow-leader
    }
    if world.pin(anilist_id).is_some() {
        return none; // pin overrides the route stamp
    }

    if world.route_pref(anilist_id).as_deref() == Some(pref.as_str()) {
        // Settled under the live pref: open its binding, no re-stamp. No
        // binding yet means the caller opens the existing state.
        return match world.binding(anilist_id, &pref) {
            Some(id) => RouteOutcome {
                stamp: None,
                action: RouteAction::OpenBinding { provider: pref, id },
            },
            None => none,
        };
    }

    // Stale or never settled: force pref once. A retired/unknown pref cannot
    // be forced (mis-key guard).
    if !world.registered(&pref) {
        return none;
    }
    let stamp = Some(pref.clone());
    if let Some(id) = world.binding(anilist_id, &pref) {
        return RouteOutcome {
            stamp,
            action: RouteAction::OpenBinding { provider: pref, id },
        };
    }
    if let Some(key) = world.canonical_key(&pref, canonical) {
        return RouteOutcome {
            stamp,
            action: RouteAction::FetchTierA {
                provider: pref,
                key,
                anilist_id,
            },
        };
    }
    RouteOutcome {
        stamp,
        action: RouteAction::Walk(Box::new(Walk::forced_preferred(canonical.clone(), pref))),
    }
}

/// Walk origin (03 §5.3): a required tag, NOT the freeze's single `manual`
/// bit. Origin decides only what an exhausted walk does; probe-through-absence
/// is the orthogonal `manual` field (03 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkOrigin {
    /// §5.3 stale-stamp re-route, pinless. Exhaust → K-2 law.
    ForcedPreferred,
    /// Path 3 `v` flip. Exhaust → stop, pin kept, no full walk, no borrow.
    PinFlip,
    /// Ordinary post-failure fallback (03 §6.4). Exhaust → dead-end.
    Fallback,
}

/// One walk hop for the transport to run.
#[derive(Debug, Clone, PartialEq)]
pub enum Hop {
    /// Fetch episodes; on success mint `bind` (Some for a fresh tier-A key,
    /// None when already bound).
    Fetch {
        provider: String,
        id: String,
        bind: Option<i64>,
    },
    /// Tier-C single-provider search; a miss re-advances the walk.
    Search { provider: String, anilist_id: i64 },
}

/// What an exhausted walk means for the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Exhausted {
    /// ForcedPreferred miss (K-2): run this continuation walk instead of
    /// dead-ending. Bindings-first over every provider, pref already tried,
    /// non-manual. The grid must never stay blank while a binding exists.
    Continue(Box<Walk>),
    /// PinFlip miss: pin kept, pin-kept toast, stop. No full walk, no borrow.
    PinKept,
    /// Ordinary fallback dead-end.
    DeadEnd,
}

/// The fallback walk state machine (03 §5.3, §6.4). Forward-only cursor over
/// a provider snapshot with a skip mask; one hop per `advance`. Mid-walk
/// preference changes never reshuffle a live walk (the snapshot is taken at
/// construction).
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    canonical: Enrichment,
    anilist_id: i64,
    providers: Vec<String>,
    origin: WalkOrigin,
    /// Providers to skip entirely (the failed one; in a K-2 continuation, the
    /// already-tried pref). Bit i corresponds to providers[i].
    tried: u64,
    next: usize,
    /// Probe through fresh absence (03 §5.2 "manual"). True for the flip and
    /// the forced single probe; false for ordinary and K-2-continuation walks.
    probe_through_absence: bool,
    /// K-2 continuation only: sweep every provider for a binding before the
    /// per-provider A/C loop, so an existing binding beats a tier-A key on an
    /// earlier provider (03 §5.3 step 2, "existing bindings first").
    bindings_first: bool,
    swept_bindings: bool,
}

impl Walk {
    /// Single-provider forced-preferred tier-C probe (03 §5.3). Manual
    /// (probes through absence); its miss triggers the K-2 law.
    fn forced_preferred(canonical: Enrichment, pref: String) -> Walk {
        let anilist_id = canonical.anilist_id;
        Walk {
            canonical,
            anilist_id,
            providers: vec![pref],
            origin: WalkOrigin::ForcedPreferred,
            tried: 0,
            next: 0,
            probe_through_absence: true,
            bindings_first: false,
            swept_bindings: false,
        }
    }

    /// Path 3 manual pin flip (03 §5.3): single-provider walk on the target,
    /// probing through fresh absence. A miss keeps the pin.
    ///
    /// PRECONDITION: `target` must be a live registry name. Unlike
    /// `route_preferred`, this takes no world and cannot check `registered`, so
    /// the caller (the `v`-flip UI, ROD-439) must only pass providers the
    /// registry offers, or `advance` will search a name the store cannot key.
    pub fn pin_flip(canonical: Enrichment, target: String) -> Walk {
        let anilist_id = canonical.anilist_id;
        Walk {
            canonical,
            anilist_id,
            providers: vec![target],
            origin: WalkOrigin::PinFlip,
            tried: 0,
            next: 0,
            probe_through_absence: true,
            bindings_first: false,
            swept_bindings: false,
        }
    }

    /// Ordinary post-failure fallback (03 §6.4): full ordered snapshot with
    /// the failed provider pre-marked tried. Respects fresh absence.
    pub fn fallback(
        world: &dyn ResolveWorld,
        canonical: Enrichment,
        failed_provider: Option<&str>,
    ) -> Walk {
        let anilist_id = canonical.anilist_id;
        let providers = world.ordered(effective_pref(world, anilist_id).as_deref());
        debug_assert!(providers.len() <= MAX_TRACKED_PROVIDERS);
        let tried = mark(&providers, failed_provider);
        Walk {
            canonical,
            anilist_id,
            providers,
            origin: WalkOrigin::Fallback,
            tried,
            next: 0,
            probe_through_absence: false,
            bindings_first: false,
            swept_bindings: false,
        }
    }

    pub fn origin(&self) -> WalkOrigin {
        self.origin
    }

    /// Skip-mask read that can never overflow the shift. A provider past
    /// `MAX_TRACKED_PROVIDERS` is simply not skip-tracked (visited rather than
    /// aliased onto an earlier bit); the constructors `debug_assert` the
    /// registry stays within range, so this only degrades in an absurd build.
    fn is_tried(&self, idx: usize) -> bool {
        idx < MAX_TRACKED_PROVIDERS && self.tried & (1 << idx) != 0
    }

    /// Advance one hop, or report why the walk exhausted. Per hop (03 §6.4):
    /// a bound id fetches; else fresh absence is skipped (unless manual); else
    /// a tier-A key fetches (minting on success); else a tier-C search. A K-2
    /// continuation first sweeps every provider for a binding.
    pub fn advance(&mut self, world: &dyn ResolveWorld) -> Result<Hop, Exhausted> {
        if self.bindings_first && !self.swept_bindings {
            self.swept_bindings = true;
            for (i, p) in self.providers.iter().enumerate() {
                if self.is_tried(i) {
                    continue;
                }
                if let Some(id) = world.binding(self.anilist_id, p) {
                    return Ok(Hop::Fetch {
                        provider: p.clone(),
                        id,
                        bind: None,
                    });
                }
            }
        }

        while self.next < self.providers.len() {
            let idx = self.next;
            self.next += 1;
            if self.is_tried(idx) {
                continue;
            }
            let p = &self.providers[idx];
            if let Some(id) = world.binding(self.anilist_id, p) {
                return Ok(Hop::Fetch {
                    provider: p.clone(),
                    id,
                    bind: None,
                });
            }
            if !self.probe_through_absence && world.absent_fresh(self.anilist_id, p) {
                continue;
            }
            if let Some(key) = world.canonical_key(p, &self.canonical) {
                return Ok(Hop::Fetch {
                    provider: p.clone(),
                    id: key,
                    bind: Some(self.anilist_id),
                });
            }
            return Ok(Hop::Search {
                provider: p.clone(),
                anilist_id: self.anilist_id,
            });
        }

        Err(self.exhaust(world))
    }

    /// K-2 law (03 §5.3): a ForcedPreferred miss does not dead-end. It begins
    /// a fresh full ordered walk, bindings-first, pref already tried,
    /// non-manual (respects absence). PinFlip keeps the pin and stops.
    fn exhaust(&self, world: &dyn ResolveWorld) -> Exhausted {
        match self.origin {
            WalkOrigin::ForcedPreferred => {
                let pref = self.providers.first().map(String::as_str);
                let providers = world.ordered(pref);
                debug_assert!(providers.len() <= MAX_TRACKED_PROVIDERS);
                let tried = mark(&providers, pref);
                Exhausted::Continue(Box::new(Walk {
                    canonical: self.canonical.clone(),
                    anilist_id: self.anilist_id,
                    providers,
                    origin: WalkOrigin::Fallback,
                    tried,
                    next: 0,
                    probe_through_absence: false,
                    bindings_first: true,
                    swept_bindings: false,
                }))
            }
            WalkOrigin::PinFlip => Exhausted::PinKept,
            WalkOrigin::Fallback => Exhausted::DeadEnd,
        }
    }
}

/// Providers beyond this cannot be tracked in the u64 skip mask. The registry
/// is a handful (3 at freeze), so this is a sanity ceiling the constructors
/// `debug_assert`, not a real limit; `Walk::is_tried` degrades safely past it.
const MAX_TRACKED_PROVIDERS: usize = 64;

/// Bit mask of `providers` positions equal to `name` (by stable name). Capped
/// at `MAX_TRACKED_PROVIDERS` so the shift cannot overflow.
fn mark(providers: &[String], name: Option<&str>) -> u64 {
    let Some(name) = name else { return 0 };
    let mut m = 0u64;
    for (i, p) in providers.iter().enumerate().take(MAX_TRACKED_PROVIDERS) {
        if p == name {
            m |= 1 << i;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct FakeWorld {
        order: Vec<String>,
        bindings: HashMap<(i64, String), String>,
        absences: HashSet<(i64, String)>,
        keys: HashMap<(String, i64), String>,
        pins: HashMap<i64, String>,
        routes: HashMap<i64, String>,
        global: String,
    }

    impl FakeWorld {
        fn new(order: &[&str]) -> FakeWorld {
            FakeWorld {
                order: order.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }
        }
        fn bind(mut self, aid: i64, p: &str, id: &str) -> Self {
            self.bindings.insert((aid, p.to_string()), id.to_string());
            self
        }
        fn absent(mut self, aid: i64, p: &str) -> Self {
            self.absences.insert((aid, p.to_string()));
            self
        }
        fn key(mut self, p: &str, aid: i64, k: &str) -> Self {
            self.keys.insert((p.to_string(), aid), k.to_string());
            self
        }
        fn pin(mut self, aid: i64, p: &str) -> Self {
            self.pins.insert(aid, p.to_string());
            self
        }
        fn route(mut self, aid: i64, p: &str) -> Self {
            self.routes.insert(aid, p.to_string());
            self
        }
        fn global(mut self, p: &str) -> Self {
            self.global = p.to_string();
            self
        }
    }

    impl ResolveWorld for FakeWorld {
        fn ordered(&self, pref: Option<&str>) -> Vec<String> {
            let mut out = Vec::with_capacity(self.order.len());
            if let Some(pref) = pref.filter(|p| !p.is_empty())
                && let Some(hit) = self.order.iter().find(|p| p.as_str() == pref)
            {
                out.push(hit.clone());
            }
            for p in &self.order {
                if Some(p.as_str()) != out.first().map(String::as_str) {
                    out.push(p.clone());
                }
            }
            out
        }
        fn registered(&self, provider: &str) -> bool {
            self.order.iter().any(|p| p == provider)
        }
        fn binding(&self, aid: i64, provider: &str) -> Option<String> {
            self.bindings.get(&(aid, provider.to_string())).cloned()
        }
        fn absent_fresh(&self, aid: i64, provider: &str) -> bool {
            self.absences.contains(&(aid, provider.to_string()))
        }
        fn canonical_key(&self, provider: &str, canonical: &Enrichment) -> Option<String> {
            self.keys
                .get(&(provider.to_string(), canonical.anilist_id))
                .cloned()
        }
        fn pin(&self, aid: i64) -> Option<String> {
            self.pins.get(&aid).cloned()
        }
        fn route_pref(&self, aid: i64) -> Option<String> {
            self.routes.get(&aid).cloned()
        }
        fn global_pref(&self) -> String {
            self.global.clone()
        }
    }

    fn canon(aid: i64) -> Enrichment {
        Enrichment {
            anilist_id: aid,
            ..Enrichment::default()
        }
    }

    const REG: [&str; 3] = ["megaplay", "senshi", "allanime"];

    // ── Path 1: canonical open (tier-major) ─────────────────────────────────

    #[test]
    fn classify_is_tier_major_binding_beats_earlier_key() {
        // allanime (last) has a binding; megaplay (first) has a tier-A key.
        // Tier 0 must win over a fresh key on an earlier provider (ROD-343).
        let w = FakeWorld::new(&REG)
            .key("megaplay", 1, "mp-key")
            .bind(1, "allanime", "aa-id");
        assert_eq!(
            classify_open(&w, &canon(1)),
            ResolveTarget::Bound {
                provider: "allanime".into(),
                id: "aa-id".into(),
                anilist_id: 1,
            }
        );
    }

    #[test]
    fn classify_path1_nonpinned_binding_beats_unbound_pin() {
        // The Path 1 asymmetry: pin folds into preference only. A pin on
        // megaplay with no binding must not shadow senshi's real binding.
        let w = FakeWorld::new(&REG)
            .pin(1, "megaplay")
            .bind(1, "senshi", "s-id");
        assert_eq!(
            classify_open(&w, &canon(1)),
            ResolveTarget::Bound {
                provider: "senshi".into(),
                id: "s-id".into(),
                anilist_id: 1,
            }
        );
    }

    #[test]
    fn classify_tier_a_uses_effective_order_then_needs_search() {
        let w = FakeWorld::new(&REG)
            .key("senshi", 1, "s-key")
            .key("allanime", 1, "aa-key");
        // Pin senshi → it leads the effective order, so its key wins the tie.
        let pinned = FakeWorld {
            pins: [(1, "senshi".to_string())].into(),
            ..FakeWorld::new(&REG)
        }
        .key("senshi", 1, "s-key")
        .key("allanime", 1, "aa-key");
        assert_eq!(
            classify_open(&pinned, &canon(1)),
            ResolveTarget::TierA {
                provider: "senshi".into(),
                key: "s-key".into(),
                anilist_id: 1
            }
        );
        // No pref: construction order, senshi (first with a key) wins.
        assert_eq!(
            classify_open(&w, &canon(1)),
            ResolveTarget::TierA {
                provider: "senshi".into(),
                key: "s-key".into(),
                anilist_id: 1
            }
        );
        // Nothing anywhere → tier C.
        assert_eq!(
            classify_open(&FakeWorld::new(&REG), &canon(9)),
            ResolveTarget::NeedsSearch { anilist_id: 9 }
        );
    }

    // ── Path 2: History open (pin hard restriction) ─────────────────────────

    fn rec<'a>(
        aid: i64,
        source: &'a str,
        source_id: &'a str,
        c: &'a Enrichment,
    ) -> HistoryRecord<'a> {
        HistoryRecord {
            anilist_id: Some(aid),
            source,
            source_id,
            canonical: c,
        }
    }

    #[test]
    fn history_pin_opens_only_its_own_binding() {
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .pin(1, "allanime")
            .bind(1, "allanime", "aa-id");
        assert_eq!(
            open_history(&w, &rec(1, "megaplay", "mp-id", &c)),
            HistoryOpen::PinBinding {
                provider: "allanime".into(),
                id: "aa-id".into()
            }
        );
    }

    #[test]
    fn history_unbound_pin_never_borrows_falls_through_to_record() {
        // The Path 2 asymmetry: pin set but unbound, and senshi HAS a binding.
        // Path 1 would take senshi; Path 2 must fall through to the record,
        // never borrowing senshi's binding. With the pin set, the re-route
        // no-ops too.
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .pin(1, "allanime")
            .bind(1, "senshi", "s-id")
            .global("senshi");
        assert_eq!(
            open_history(&w, &rec(1, "megaplay", "mp-id", &c)),
            HistoryOpen::Record {
                provider: "megaplay".into(),
                id: "mp-id".into()
            }
        );
    }

    #[test]
    fn history_pin_equal_to_source_or_retired_falls_through() {
        let c = canon(1);
        // pin == source: no re-route to the same provider.
        let same = FakeWorld::new(&REG)
            .pin(1, "megaplay")
            .bind(1, "megaplay", "x");
        assert_eq!(
            open_history(&same, &rec(1, "megaplay", "mp-id", &c)),
            HistoryOpen::Record {
                provider: "megaplay".into(),
                id: "mp-id".into()
            }
        );
        // retired pin (not in registry) must not fetch a foreign id.
        let retired = FakeWorld::new(&REG).pin(1, "gogo").bind(1, "gogo", "g-id");
        assert_eq!(
            open_history(&retired, &rec(1, "megaplay", "mp-id", &c)),
            HistoryOpen::Record {
                provider: "megaplay".into(),
                id: "mp-id".into()
            }
        );
    }

    #[test]
    fn history_unpinned_routes_before_the_record() {
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .global("senshi")
            .bind(1, "senshi", "s-id");
        match open_history(&w, &rec(1, "megaplay", "mp-id", &c)) {
            HistoryOpen::Routed(o) => {
                assert_eq!(o.stamp.as_deref(), Some("senshi"));
                assert_eq!(
                    o.action,
                    RouteAction::OpenBinding {
                        provider: "senshi".into(),
                        id: "s-id".into()
                    }
                );
            }
            other => panic!("expected route, got {other:?}"),
        }
    }

    // ── ROD-398: preferred re-route + stamp-before-fetch ────────────────────

    #[test]
    fn route_pin_and_empty_pref_are_noops() {
        let c = canon(1);
        assert_eq!(
            route_preferred(&FakeWorld::new(&REG), &c).action,
            RouteAction::None
        );
        let pinned = FakeWorld::new(&REG).global("senshi").pin(1, "megaplay");
        assert_eq!(route_preferred(&pinned, &c).action, RouteAction::None);
    }

    #[test]
    fn route_settled_opens_binding_without_restamp() {
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .global("senshi")
            .route(1, "senshi")
            .bind(1, "senshi", "s-id");
        let o = route_preferred(&w, &c);
        assert_eq!(o.stamp, None);
        assert_eq!(
            o.action,
            RouteAction::OpenBinding {
                provider: "senshi".into(),
                id: "s-id".into()
            }
        );

        // Settled but pref never bound: no route, caller opens existing state.
        let unbound = FakeWorld::new(&REG).global("senshi").route(1, "senshi");
        assert_eq!(route_preferred(&unbound, &c).action, RouteAction::None);
    }

    #[test]
    fn route_stale_forces_once_and_stamps_before_fetch() {
        let c = canon(1);
        // Stale (settled under a different pref): force senshi's tier-A key.
        let w = FakeWorld::new(&REG)
            .global("senshi")
            .route(1, "megaplay")
            .key("senshi", 1, "s-key");
        let o = route_preferred(&w, &c);
        assert_eq!(
            o.stamp.as_deref(),
            Some("senshi"),
            "must stamp before fetch"
        );
        assert_eq!(
            o.action,
            RouteAction::FetchTierA {
                provider: "senshi".into(),
                key: "s-key".into(),
                anilist_id: 1
            }
        );
    }

    #[test]
    fn route_stale_retired_pref_does_not_force() {
        let c = canon(1);
        let w = FakeWorld::new(&REG).global("gogo"); // not in registry
        assert_eq!(route_preferred(&w, &c).action, RouteAction::None);
    }

    #[test]
    fn route_stamp_before_fetch_breaks_the_loop() {
        // End-to-end loop guard: a forced route that MISSES must read
        // non-stale on the second open (the stamp already advanced), so it
        // does not force forever.
        let c = canon(1);
        let mut w = FakeWorld::new(&REG).global("senshi"); // no binding, no key
        let first = route_preferred(&w, &c);
        assert_eq!(first.stamp.as_deref(), Some("senshi"));
        assert!(matches!(first.action, RouteAction::Walk(_)));

        // Transport applies the stamp before firing.
        w.routes.insert(1, "senshi".to_string());
        // Second open: settled under the live pref, pref still unbound → no
        // forcing, no walk. The loop is broken.
        assert_eq!(route_preferred(&w, &c).action, RouteAction::None);
    }

    // ── the walk + K-2 law ──────────────────────────────────────────────────

    #[test]
    fn fallback_walk_is_provider_major_binding_then_absence_then_key_then_search() {
        // megaplay failed (pre-tried); senshi absent-fresh (skipped);
        // allanime has a key → fetch-with-bind.
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .absent(1, "senshi")
            .key("allanime", 1, "aa-key");
        let mut walk = Walk::fallback(&w, c, Some("megaplay"));
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Fetch {
                provider: "allanime".into(),
                id: "aa-key".into(),
                bind: Some(1)
            })
        );
        // Nothing left → dead-end.
        assert_eq!(walk.advance(&w), Err(Exhausted::DeadEnd));
    }

    #[test]
    fn fallback_manual_flag_is_off_so_fresh_absence_is_respected() {
        let c = canon(1);
        // senshi could answer via a key, but it is absent-fresh; ordinary
        // fallback respects that and searches allanime instead.
        let w = FakeWorld::new(&REG)
            .absent(1, "senshi")
            .key("senshi", 1, "s-key");
        let mut walk = Walk::fallback(&w, c, Some("megaplay"));
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Search {
                provider: "allanime".into(),
                anilist_id: 1
            })
        );
        assert_eq!(walk.advance(&w), Err(Exhausted::DeadEnd));
    }

    #[test]
    fn pin_flip_probes_through_absence_and_keeps_pin_on_miss() {
        let c = canon(1);
        // Target is absent-fresh, but a manual flip probes anyway (search).
        let w = FakeWorld::new(&REG).absent(1, "senshi");
        let mut walk = Walk::pin_flip(c, "senshi".into());
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Search {
                provider: "senshi".into(),
                anilist_id: 1
            })
        );
        // Miss: pin kept, no full walk, no borrow of another binding.
        assert_eq!(walk.advance(&w), Err(Exhausted::PinKept));
    }

    #[test]
    fn forced_preferred_miss_triggers_k2_continuation_not_dead_end() {
        // The K-2 bug fix. Forced senshi probe misses; a binding exists on
        // allanime. The continuation must find it, never leave the grid blank.
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .global("senshi")
            .bind(1, "allanime", "aa-id");
        // route_preferred forces a single-provider senshi walk (no binding, no key).
        let RouteAction::Walk(mut walk) = route_preferred(&w, &c).action else {
            panic!("expected forced walk");
        };
        assert_eq!(walk.origin(), WalkOrigin::ForcedPreferred);
        // The senshi probe: a search hop.
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Search {
                provider: "senshi".into(),
                anilist_id: 1
            })
        );
        // Miss → K-2 continuation, not a dead-end.
        let Err(Exhausted::Continue(mut cont)) = walk.advance(&w) else {
            panic!("expected K-2 continuation");
        };
        assert_eq!(cont.origin(), WalkOrigin::Fallback);
        // Bindings-first sweep finds allanime's binding even though senshi
        // (the pref) leads the order and is already tried.
        assert_eq!(
            cont.advance(&w),
            Ok(Hop::Fetch {
                provider: "allanime".into(),
                id: "aa-id".into(),
                bind: None
            })
        );
    }

    #[test]
    fn k2_continuation_skips_the_already_tried_pref_and_respects_absence() {
        // No binding anywhere; senshi (pref) already tried; megaplay absent;
        // allanime has a key. The continuation is non-manual, so megaplay's
        // fresh absence is respected, and allanime answers via its key.
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .global("senshi")
            .absent(1, "megaplay")
            .key("allanime", 1, "aa-key");
        let RouteAction::Walk(mut walk) = route_preferred(&w, &c).action else {
            panic!("expected forced walk");
        };
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Search {
                provider: "senshi".into(),
                anilist_id: 1
            })
        );
        let Err(Exhausted::Continue(mut cont)) = walk.advance(&w) else {
            panic!("expected continuation");
        };
        // megaplay skipped (absent), senshi skipped (tried), allanime keyed.
        assert_eq!(
            cont.advance(&w),
            Ok(Hop::Fetch {
                provider: "allanime".into(),
                id: "aa-key".into(),
                bind: Some(1)
            })
        );
        assert_eq!(cont.advance(&w), Err(Exhausted::DeadEnd));
    }

    #[test]
    fn walk_snapshot_is_stable_across_a_pref_change_mid_walk() {
        // The provider order is fixed at construction; a later pin/pref change
        // in the world must not reshuffle a live walk (03 §6.4).
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .key("megaplay", 1, "mp")
            .key("allanime", 1, "aa");
        let mut walk = Walk::fallback(&w, c, None);
        // First hop: megaplay (construction order, no pref).
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Fetch {
                provider: "megaplay".into(),
                id: "mp".into(),
                bind: Some(1)
            })
        );
        // Even if the world now prefers allanime, the walk keeps its snapshot:
        // next is senshi (search), then allanime.
        let w2 = FakeWorld {
            pins: [(1, "allanime".to_string())].into(),
            ..FakeWorld::new(&REG)
        }
        .key("megaplay", 1, "mp")
        .key("allanime", 1, "aa");
        assert_eq!(
            walk.advance(&w2),
            Ok(Hop::Search {
                provider: "senshi".into(),
                anilist_id: 1
            })
        );
    }

    #[test]
    fn walk_bitmask_is_overflow_safe_at_the_registry_ceiling() {
        // Walking the full 64-provider ceiling must not overflow the shift
        // (ROD-436 review: the old `1 << idx` panicked in debug / aliased in
        // release past index 63). p0 is pre-tried; the rest search-miss.
        let names: Vec<String> = (0..MAX_TRACKED_PROVIDERS)
            .map(|i| format!("p{i}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let w = FakeWorld::new(&refs);
        let mut walk = Walk::fallback(&w, canon(1), Some("p0"));
        let mut hops = 0;
        loop {
            match walk.advance(&w) {
                Ok(_) => hops += 1,
                Err(Exhausted::DeadEnd) => break,
                Err(other) => panic!("unexpected exhaust: {other:?}"),
            }
        }
        assert_eq!(hops, MAX_TRACKED_PROVIDERS - 1); // all but the pre-tried p0
        // The skip mask short-circuits past the ceiling instead of shifting.
        assert!(!walk.is_tried(MAX_TRACKED_PROVIDERS));
        assert!(!walk.is_tried(1_000));
    }
}
