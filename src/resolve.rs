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

    /// The provider this show last successfully served from (03 §5.1,
    /// ROD-525). A confirmation record, written only at a landing.
    fn last_used(&self, anilist_id: i64) -> Option<String>;

    /// Global config `preferred_provider`; empty = follow-leader / unset.
    fn global_pref(&self) -> String;
}

/// Effective preference for a show: last-used overrides the global walk-order
/// head (03 §5.1). Empty global config reads as unset.
pub fn effective_pref(world: &dyn ResolveWorld, anilist_id: i64) -> Option<String> {
    if let Some(last) = world.last_used(anilist_id) {
        return Some(last);
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
/// (last-used first) breaks ties.
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

/// Walk origin (03 §5.2, ROD-525): decides only the absence-probe scope.
/// Both origins exhaust the same way, a dead end that says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkOrigin {
    /// User-driven `v` walk: full registry circle from the aimed provider,
    /// probing through fresh absence on that first hop only.
    Manual,
    /// Post-failure fallback (03 §6.4): effective order, respects absence.
    Auto,
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

/// What an exhausted walk means for the caller. One meaning (ROD-525): the
/// circle is out, and the caller says so; silence is never an outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Exhausted {
    DeadEnd,
}

/// The walk state machine (03 §5.2, §6.4). Forward-only cursor over a
/// provider snapshot with a skip mask; one hop per `advance`. A manual walk
/// wraps by construction: its snapshot is the registry circle rotated to the
/// aimed provider. Mid-walk preference changes never reshuffle a live walk
/// (the snapshot is taken at construction).
#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    canonical: Enrichment,
    anilist_id: i64,
    providers: Vec<String>,
    origin: WalkOrigin,
    /// Providers to skip entirely (the failed one). Bit i is providers[i].
    tried: u64,
    next: usize,
}

impl Walk {
    /// Manual `v` walk (03 §5.2, ROD-525): the full registry circle in
    /// construction order, rotated so `target` is hop one. Probes through
    /// fresh absence on that first hop only; the continuation respects it.
    ///
    /// PRECONDITION: `target` must be a live registry name; the cycle UI only
    /// offers registry members.
    pub fn manual(world: &dyn ResolveWorld, canonical: Enrichment, target: &str) -> Walk {
        let anilist_id = canonical.anilist_id;
        let mut providers = world.ordered(None);
        debug_assert!(providers.len() <= MAX_TRACKED_PROVIDERS);
        let start = providers.iter().position(|p| p == target).unwrap_or(0);
        providers.rotate_left(start);
        Walk {
            canonical,
            anilist_id,
            providers,
            origin: WalkOrigin::Manual,
            tried: 0,
            next: 0,
        }
    }

    /// Post-failure fallback (03 §6.4): full effective-order snapshot with
    /// the failed provider pre-marked tried. Respects fresh absence.
    pub fn auto(
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
            origin: WalkOrigin::Auto,
            tried,
            next: 0,
        }
    }

    pub fn origin(&self) -> WalkOrigin {
        self.origin
    }

    /// Mark a provider tried post-construction. The play continuation
    /// (03 §6.4) keeps its walk's memory across relaunches: every provider
    /// that already failed a play stays skipped in the successor walk, or a
    /// two-provider registry would ping-pong between the same pair forever.
    pub fn mark_tried(&mut self, provider: &str) {
        self.tried |= mark(&self.providers, Some(provider));
    }

    /// Skip-mask read that can never overflow the shift. A provider past
    /// `MAX_TRACKED_PROVIDERS` is simply not skip-tracked (visited rather than
    /// aliased onto an earlier bit); the constructors `debug_assert` the
    /// registry stays within range, so this only degrades in an absurd build.
    fn is_tried(&self, idx: usize) -> bool {
        idx < MAX_TRACKED_PROVIDERS && self.tried & (1 << idx) != 0
    }

    /// Advance one hop, or report the exhaust. Per hop (03 §6.4): a bound id
    /// fetches; else fresh absence is skipped (except a manual walk's first
    /// hop, the provider the user aimed at, 03 §5.2); else a tier-A key
    /// fetches (minting on success); else a tier-C search.
    pub fn advance(&mut self, world: &dyn ResolveWorld) -> Result<Hop, Exhausted> {
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
            let aimed = self.origin == WalkOrigin::Manual && idx == 0;
            if !aimed && world.absent_fresh(self.anilist_id, p) {
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
        Err(Exhausted::DeadEnd)
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
        last_useds: HashMap<i64, String>,
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
        fn last(mut self, aid: i64, p: &str) -> Self {
            self.last_useds.insert(aid, p.to_string());
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
        fn last_used(&self, aid: i64) -> Option<String> {
            self.last_useds.get(&aid).cloned()
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
    fn classify_last_used_without_binding_never_shadows_a_real_binding() {
        // Last-used folds into preference only: a remembered megaplay with no
        // binding must not shadow senshi's real binding.
        let w = FakeWorld::new(&REG)
            .last(1, "megaplay")
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
        // Last-used senshi leads the effective order, so its key wins the tie.
        let remembered = FakeWorld::new(&REG)
            .last(1, "senshi")
            .key("senshi", 1, "s-key")
            .key("allanime", 1, "aa-key");
        assert_eq!(
            classify_open(&remembered, &canon(1)),
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

    // ── the walk ────────────────────────────────────────────────────────────

    #[test]
    fn auto_walk_is_provider_major_binding_then_absence_then_key_then_search() {
        // megaplay failed (pre-tried); senshi absent-fresh (skipped);
        // allanime has a key → fetch-with-bind.
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .absent(1, "senshi")
            .key("allanime", 1, "aa-key");
        let mut walk = Walk::auto(&w, c, Some("megaplay"));
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
    fn mark_tried_skips_extra_providers_for_a_continuation_walk() {
        // Play continuation (03 §6.4): megaplay and senshi already burned a
        // play each; the successor walk must go straight to allanime.
        let c = canon(1);
        let w = FakeWorld::new(&REG).key("senshi", 1, "s-key");
        let mut walk = Walk::auto(&w, c, Some("megaplay"));
        walk.mark_tried("senshi");
        walk.mark_tried("not-registered"); // no-op, never a panic
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
    fn auto_walk_respects_fresh_absence() {
        let c = canon(1);
        // senshi could answer via a key, but it is absent-fresh; the auto
        // walk respects that and searches allanime instead.
        let w = FakeWorld::new(&REG)
            .absent(1, "senshi")
            .key("senshi", 1, "s-key");
        let mut walk = Walk::auto(&w, c, Some("megaplay"));
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
    fn manual_walk_probes_absence_on_first_hop_only() {
        let c = canon(1);
        // The aimed provider is absent-fresh but probed anyway; the
        // continuation respects allanime's absence and moves on (03 5.2).
        let w = FakeWorld::new(&REG)
            .absent(1, "senshi")
            .absent(1, "allanime");
        let mut walk = Walk::manual(&w, c, "senshi");
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Search {
                provider: "senshi".into(),
                anilist_id: 1
            })
        );
        assert_eq!(
            walk.advance(&w),
            Ok(Hop::Search {
                provider: "megaplay".into(),
                anilist_id: 1
            }),
            "allanime's fresh absence is respected past the first hop"
        );
        assert_eq!(walk.advance(&w), Err(Exhausted::DeadEnd));
    }

    #[test]
    fn manual_walk_wraps_the_registry_circle_from_its_start() {
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .key("megaplay", 1, "mp")
            .key("senshi", 1, "s")
            .key("allanime", 1, "aa");
        let mut walk = Walk::manual(&w, c, "senshi");
        let mut order = Vec::new();
        while let Ok(Hop::Fetch { provider, .. }) = walk.advance(&w) {
            order.push(provider);
        }
        assert_eq!(
            order,
            ["senshi", "allanime", "megaplay"],
            "starts at the aim, wraps past the end, visits the full circle"
        );
        assert_eq!(walk.advance(&w), Err(Exhausted::DeadEnd));
    }

    #[test]
    fn effective_pref_last_used_overrides_global() {
        let w = FakeWorld::new(&REG).global("senshi").last(1, "allanime");
        assert_eq!(effective_pref(&w, 1).as_deref(), Some("allanime"));
        assert_eq!(effective_pref(&w, 2).as_deref(), Some("senshi"));
        assert_eq!(
            effective_pref(&FakeWorld::new(&REG), 1),
            None,
            "empty global reads as unset"
        );
    }

    #[test]
    fn walk_snapshot_is_stable_across_a_pref_change_mid_walk() {
        // The provider order is fixed at construction; a later last-used or
        // pref change in the world must not reshuffle a live walk (03 §6.4).
        let c = canon(1);
        let w = FakeWorld::new(&REG)
            .key("megaplay", 1, "mp")
            .key("allanime", 1, "aa");
        let mut walk = Walk::auto(&w, c, None);
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
        let w2 = FakeWorld::new(&REG)
            .last(1, "allanime")
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
        let mut walk = Walk::auto(&w, canon(1), Some("p0"));
        let mut hops = 0;
        while walk.advance(&w).is_ok() {
            hops += 1;
        }
        assert_eq!(hops, MAX_TRACKED_PROVIDERS - 1); // all but the pre-tried p0
        // The skip mask short-circuits past the ceiling instead of shifting.
        assert!(!walk.is_tried(MAX_TRACKED_PROVIDERS));
        assert!(!walk.is_tried(1_000));
    }
}
