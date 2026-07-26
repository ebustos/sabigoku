//! Provider seam (03): `StreamProvider` / `CatalogProvider` traits, registry,
//! error taxonomy. Concrete stream providers land in ROD-436. Multiprovider
//! is day-1 architecture: N providers with per-provider availability, never a
//! single provider with a seam. Imports domain (+ own http helpers); NEVER
//! tui or store (01 §5, keeps backends testable offline).

pub mod allanime;
pub mod hls;
pub mod http;
pub mod megaplay;
pub mod senshi;

use crate::domain::{Enrichment, Quality, StreamLink, Translation};

/// Full page for Browse search and provider tier-C pagination (03 §2.1, ROD-201).
pub const SEARCH_PAGE_SIZE: u32 = 26;
/// Discover feed page (06 §8b).
pub const DISCOVER_PAGE_SIZE: u32 = 20;

/// Provider failure classes; the TUI maps these to toast copy and hop policy
/// (03 §7). `Ok(vec![])` from `episodes` is NOT an error: it is authoritative
/// not-stocked and marks absence, while any `Err` must not (03 §4.3).
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("network unreachable")]
    Network,

    #[error("blocked (http {status})")]
    Forbidden { status: u16 },

    #[error("server error (http {status})")]
    Server { status: u16 },

    #[error("http {status}")]
    Http { status: u16 },

    #[error("malformed response: {0}")]
    Decode(String),

    /// Operation the provider does not offer (megaplay has no search).
    /// Must never poison absence (03 §8.1).
    #[error("unsupported operation")]
    Unsupported,
}

impl ProviderError {
    pub fn from_status(status: u16) -> ProviderError {
        match status {
            403 | 451 => ProviderError::Forbidden { status },
            500..=599 => ProviderError::Server { status },
            _ => ProviderError::Http { status },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchOptions {
    pub translation: Translation,
    pub limit: u32,
    /// 1-indexed.
    pub page: u32,
}

/// Tier-C candidate. Ids, titles, and counts feed the tier-B/C scorers
/// (03 §4.2); they are provider claims, not truth.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchHit {
    pub provider_id: String,
    pub title: String,
    pub title_english: Option<String>,
    pub title_native: Option<String>,
    pub anilist_id: Option<i64>,
    pub mal_id: Option<i64>,
    /// Catalog total claim; per-track listing counts below are the fallback
    /// episode signal when absent (0 = unknown).
    pub total_episodes: Option<u32>,
    pub eps_sub: u32,
    pub eps_dub: u32,
    pub year: Option<u32>,
}

/// Stored cover ref turned into an absolute fetch (03 §2).
#[derive(Debug, Clone, PartialEq)]
pub struct CoverRequest {
    pub url: String,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
}

/// Stream-site surface (03 §2). App code talks only to this trait; no
/// concrete site types past the registry.
pub trait StreamProvider: Send + Sync {
    /// Stable persistence key. Renaming orphans every stored binding.
    fn name(&self) -> &'static str;

    /// User-facing (toasts, UI). Free to change.
    fn display_name(&self) -> &'static str;

    /// Tier A: pure derivation from AniList/MAL metadata. `None` means "does
    /// not id-key on canonical", not "not stocked" (03 §2).
    fn canonical_key(&self, show: &Enrichment) -> Option<String>;

    /// Tier-C binding search only, never Browse (03 §1).
    fn search(&self, query: &str, opts: &SearchOptions) -> Result<Vec<SearchHit>, ProviderError>;

    /// Whether `search` can ever answer. `false` means structurally incapable,
    /// not "failed this time", so a caller may skip the provider without
    /// calling it.
    ///
    /// Defaults true because tier-C search is the norm; a provider that cannot
    /// search MUST override, or the CLI will bind it and die at runtime on a
    /// path it believes unreachable (`cli::fetch_error_rows`). The roster test
    /// over `default_registry` is what keeps the two in step.
    fn supports_search(&self) -> bool {
        true
    }

    /// Sorted labels. Empty = authoritative not stocked; cannot-answer must be
    /// `Err` (03 §4.3). `count_hint` mints a 1..N grid on listing-less
    /// providers; real listings ignore it.
    fn episodes(
        &self,
        provider_id: &str,
        translation: Translation,
        count_hint: Option<u32>,
    ) -> Result<Vec<String>, ProviderError>;

    /// Quality may be ignored when the provider has no variants.
    fn resolve(
        &self,
        provider_id: &str,
        episode: &str,
        translation: Translation,
        quality: Quality,
    ) -> Result<StreamLink, ProviderError>;

    /// Err = ref unusable (empty, oversize, header-injection bytes); callers
    /// skip the fetch, never "sanitize" and proceed.
    fn cover_request(&self, cover_ref: &str) -> Result<CoverRequest, ProviderError>;
}

/// Process-immutable provider set. Construction order IS the default fallback
/// order (03 §3.1); the live lineup is data wired in main, not policy here.
pub struct ProviderRegistry {
    providers: Vec<Box<dyn StreamProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Box<dyn StreamProvider>>) -> ProviderRegistry {
        assert!(
            !providers.is_empty(),
            "registry needs at least one provider"
        );
        ProviderRegistry { providers }
    }

    pub fn primary(&self) -> &dyn StreamProvider {
        self.providers[0].as_ref()
    }

    /// Owner of a persisted binding key. `None` = retired provider; callers
    /// must NOT fall back to `primary()` for bound rows, a foreign id fetched
    /// on the wrong provider silently corrupts the binding (03 §3.2).
    pub fn by_name(&self, name: &str) -> Option<&dyn StreamProvider> {
        self.providers
            .iter()
            .map(|p| p.as_ref())
            .find(|p| p.name() == name)
    }

    /// Named, or `primary()` when empty/unknown (03 §3.2).
    ///
    /// No production caller. Kept because 03 §3.2 lists it as a registry view;
    /// retiring it is a bible change, not a cleanup.
    pub fn preferred(&self, name: Option<&str>) -> &dyn StreamProvider {
        name.filter(|n| !n.is_empty())
            .and_then(|n| self.by_name(n))
            .unwrap_or_else(|| self.primary())
    }

    /// First entry of `ordered` that can search, `None` if none can.
    ///
    /// Deviation from zigoku, ratified ROD-491: zigoku's CLI takes `preferred`
    /// and hard-fails when it cannot search, which on a stock config is always
    /// (the primary has no tier C, 03 §8.1). The walk stays search-only; every
    /// other path still binds to one provider, because a provider id is
    /// meaningless on another.
    pub fn preferred_searchable(&self, pref: Option<&str>) -> Option<&dyn StreamProvider> {
        self.ordered(pref).into_iter().find(|p| p.supports_search())
    }

    /// Preferred first, then construction order for the rest (03 §3.2, ROD-344).
    pub fn ordered(&self, pref: Option<&str>) -> Vec<&dyn StreamProvider> {
        let pref = pref.filter(|n| !n.is_empty()).and_then(|n| self.by_name(n));
        let pref_name = pref.map(|p| p.name());
        let mut out = Vec::with_capacity(self.providers.len());
        out.extend(pref);
        out.extend(
            self.providers
                .iter()
                .map(|p| p.as_ref())
                .filter(|p| Some(p.name()) != pref_name),
        );
        out
    }

    /// Construction-order walk; the tier-C sweep and prewarm candidates in
    /// ROD-436 consume this.
    pub fn iter(&self) -> impl Iterator<Item = &dyn StreamProvider> {
        self.providers.iter().map(|p| p.as_ref())
    }
}

/// The live provider set. Construction order IS the default fallback order (03
/// §3.1): megaplay, senshi, allanime. Building the clients is offline. Both the
/// TUI boot and the CLI play path build from here so the lineup never forks.
pub fn default_registry() -> Result<ProviderRegistry, ProviderError> {
    Ok(ProviderRegistry::new(vec![
        Box::new(megaplay::MegaPlay::new()?) as Box<dyn StreamProvider>,
        Box::new(senshi::Senshi::new()?),
        Box::new(allanime::AllAnime::new()?),
    ]))
}

/// Discover ranking axes; variant order is the freeze enum order and the UI
/// tab order (04 §7, DESIGN §3.8). Rank is positional per axis, never one
/// shared list re-sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiscoverAxis {
    Trending,
    Popular,
    TopRated,
    ThisSeason,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogPage {
    pub entries: Vec<Enrichment>,
    pub has_next: bool,
}

/// Catalog failure classes. `RateLimited` is a deliberate deviation from
/// freeze (zigoku folds 429 into no-answer, 06 §8b OPEN; ratified ROD-435):
/// callers can tell throttling from failure, but the client never sleeps or
/// retries on its own.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("rate limited (http 429)")]
    RateLimited,

    #[error("network unreachable")]
    Network,

    #[error("http {status}")]
    Http { status: u16 },

    #[error("malformed response: {0}")]
    Decode(String),
}

/// User-facing catalog seam. AniList is the only implementation by law
/// (01 §5); stream providers never power Browse/Discover. Results land in
/// catalog_cache caller-side and never stamp membership (02 L4).
pub trait CatalogProvider: Send + Sync {
    /// Browse search, `SEARCH_PAGE_SIZE` per page, 1-indexed.
    fn search(&self, query: &str, page: u32) -> Result<CatalogPage, CatalogError>;

    /// `DISCOVER_PAGE_SIZE` per page, 1-indexed.
    fn discover(&self, axis: DiscoverAxis, page: u32) -> Result<CatalogPage, CatalogError>;

    /// Three-state (05 §8): `Ok(Some)` metadata, `Ok(None)` confirmed null,
    /// `Err` no answer.
    fn enrich(&self, anilist_id: i64) -> Result<Option<Enrichment>, CatalogError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.1` is search capability; the registry's primary genuinely lacks it.
    struct Fake(&'static str, bool);

    impl StreamProvider for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        fn display_name(&self) -> &'static str {
            self.0
        }
        fn canonical_key(&self, _show: &Enrichment) -> Option<String> {
            None
        }
        fn search(
            &self,
            _query: &str,
            _opts: &SearchOptions,
        ) -> Result<Vec<SearchHit>, ProviderError> {
            Err(ProviderError::Unsupported)
        }
        fn supports_search(&self) -> bool {
            self.1
        }
        fn episodes(
            &self,
            _provider_id: &str,
            _translation: Translation,
            _count_hint: Option<u32>,
        ) -> Result<Vec<String>, ProviderError> {
            Ok(Vec::new())
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

    fn registry() -> ProviderRegistry {
        ProviderRegistry::new(vec![
            Box::new(Fake("megaplay", false)),
            Box::new(Fake("senshi", true)),
            Box::new(Fake("allanime", true)),
        ])
    }

    /// Every other test here runs against `Fake`s that MODEL the live lineup.
    /// This one pins the lineup itself: construction order (03 §3.1) and the
    /// precondition the ROD-491 deviation rests on, that the primary cannot
    /// search. Without it, reordering `default_registry` leaves the fakes
    /// passing while the reason for the deviation silently evaporates.
    #[test]
    fn live_registry_leads_with_a_primary_that_cannot_search() {
        let reg = default_registry().expect("offline construction");
        assert_eq!(
            names(&reg.iter().collect::<Vec<_>>()),
            ["megaplay", "senshi", "allanime"]
        );
        assert!(
            !reg.primary().supports_search(),
            "the CLI walk exists because the primary cannot search"
        );
        assert!(
            reg.preferred_searchable(None)
                .is_some_and(|p| p.name() == "senshi"),
            "a stock run binds the first searchable provider"
        );
    }

    fn names(providers: &[&dyn StreamProvider]) -> Vec<&'static str> {
        providers.iter().map(|p| p.name()).collect()
    }

    #[test]
    fn primary_is_first_constructed() {
        assert_eq!(registry().primary().name(), "megaplay");
    }

    #[test]
    fn by_name_finds_owner() {
        let reg = registry();
        assert_eq!(reg.by_name("senshi").unwrap().name(), "senshi");
    }

    #[test]
    fn by_name_retired_is_none() {
        assert!(registry().by_name("gogo").is_none());
    }

    #[test]
    fn preferred_named() {
        assert_eq!(registry().preferred(Some("allanime")).name(), "allanime");
    }

    #[test]
    fn preferred_empty_or_unknown_falls_back_to_primary() {
        let reg = registry();
        assert_eq!(reg.preferred(None).name(), "megaplay");
        assert_eq!(reg.preferred(Some("")).name(), "megaplay");
        assert_eq!(reg.preferred(Some("gogo")).name(), "megaplay");
    }

    #[test]
    fn ordered_default_is_construction_order() {
        let reg = registry();
        assert_eq!(
            names(&reg.ordered(None)),
            vec!["megaplay", "senshi", "allanime"]
        );
    }

    #[test]
    fn ordered_pref_first_then_construction() {
        let reg = registry();
        assert_eq!(
            names(&reg.ordered(Some("senshi"))),
            vec!["senshi", "megaplay", "allanime"]
        );
    }

    #[test]
    fn ordered_unknown_pref_is_construction_order() {
        let reg = registry();
        assert_eq!(
            names(&reg.ordered(Some("gogo"))),
            vec!["megaplay", "senshi", "allanime"]
        );
    }

    #[test]
    fn ordered_pref_primary_does_not_duplicate() {
        let reg = registry();
        assert_eq!(
            names(&reg.ordered(Some("megaplay"))),
            vec!["megaplay", "senshi", "allanime"]
        );
    }

    /// ROD-491: the stock config leaves `preferred_provider` empty, which
    /// resolves to a primary that cannot search. Walking past it is the whole
    /// deviation; drop the `supports_search` filter and this returns "megaplay".
    #[test]
    fn preferred_searchable_skips_a_primary_that_cannot_search() {
        let reg = registry();
        assert!(!reg.primary().supports_search(), "fixture precondition");
        assert_eq!(
            reg.preferred_searchable(Some("")).map(|p| p.name()),
            Some("senshi")
        );
        assert_eq!(
            reg.preferred_searchable(None).map(|p| p.name()),
            Some("senshi")
        );
    }

    #[test]
    fn preferred_searchable_honors_a_capable_preference() {
        let reg = registry();
        assert_eq!(
            reg.preferred_searchable(Some("allanime")).map(|p| p.name()),
            Some("allanime")
        );
    }

    /// An explicit but incapable preference still gets walked past, otherwise
    /// `preferred_provider = "megaplay"` reintroduces the dead CLI.
    #[test]
    fn preferred_searchable_walks_past_an_incapable_preference() {
        let reg = registry();
        assert_eq!(
            reg.preferred_searchable(Some("megaplay")).map(|p| p.name()),
            Some("senshi")
        );
    }

    #[test]
    fn preferred_searchable_is_none_when_nothing_can_search() {
        let reg = ProviderRegistry::new(vec![Box::new(Fake("megaplay", false))]);
        assert!(reg.preferred_searchable(Some("")).is_none());
    }

    #[test]
    fn iter_walks_construction_order() {
        let reg = registry();
        assert_eq!(
            reg.iter().map(|p| p.name()).collect::<Vec<_>>(),
            vec!["megaplay", "senshi", "allanime"]
        );
    }

    #[test]
    fn from_status_classes() {
        assert!(matches!(
            ProviderError::from_status(403),
            ProviderError::Forbidden { status: 403 }
        ));
        assert!(matches!(
            ProviderError::from_status(451),
            ProviderError::Forbidden { status: 451 }
        ));
        assert!(matches!(
            ProviderError::from_status(503),
            ProviderError::Server { status: 503 }
        ));
        assert!(matches!(
            ProviderError::from_status(418),
            ProviderError::Http { status: 418 }
        ));
    }
}
