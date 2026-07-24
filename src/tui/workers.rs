//! Worker accounting and staleness kit, the ThreadDrain port (04 §5, §6).
//! Law: begin BEFORE spawn; finish only after the worker's last touch of the
//! queue; drain only at teardown. Supersede is detach + stale-token drop,
//! NEVER a join on the hot path.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::anilist::AniList;
use crate::aniskip::{self, SkipMode};
use crate::auth::Auth;
use crate::domain::{Enrichment, Quality, Translation, expected_episode_count};
use crate::error::Error;
use crate::login::ConnectResult;
use crate::loopback::Loopback;
use crate::player::{self, PlayError, PlayOpts, PlayerEvent, Position};
use crate::providers::{
    CatalogProvider, DiscoverAxis, ProviderError, ProviderRegistry, SEARCH_PAGE_SIZE,
    SearchOptions, StreamProvider,
};
use crate::resolver;
use crate::store::Store;
use crate::sync::{self, SyncOutcome, SyncSummary, ThreadSleeper};
use crate::tui::covers::{self, CoverCaches};
use crate::tui::event::{Event, EventTx, FetchClass, PlayFailure, PrewarmVerdict};

/// The 02 §4b post-play gate, the one owner of the finish writes (01 §3 glue).
/// No meaningful position, no writes of any kind; the player already collapsed
/// that judgment into `PlayOutcome::position`. The resume row and the
/// engagement/ratchet land atomically inside `record_finish`. Returns whether
/// the play was recorded.
#[allow(clippy::too_many_arguments)]
pub fn finish_playback(
    store: &Store,
    anilist_id: i64,
    translation: Translation,
    episode_label: &str,
    episode_index: u32,
    position: Option<Position>,
    provider: Option<&str>,
    now: i64,
) -> Result<bool, Error> {
    let Some(position) = position else {
        return Ok(false);
    };
    store.record_finish(
        anilist_id,
        translation,
        episode_label,
        episode_index,
        position.secs,
        position.duration.unwrap_or(0.0),
        provider,
        now,
    )?;
    Ok(true)
}

/// Detail cover fetch (04 §7.3): pipeline result to `CoverDone`/`CoverError`,
/// keep-checked by `for_id` in tick. Provider-ref resolution (None here) joins
/// when a binding-owned detail path exists (ROD-439).
#[must_use]
pub fn spawn_cover_fetch(
    drain: &Drain,
    tx: EventTx,
    caches: Arc<CoverCaches>,
    covers_dir: PathBuf,
    for_id: i64,
    url: String,
) -> bool {
    drain.spawn("cover", move || {
        let event = match covers::load_cover_pixels(None, &url, &caches, &covers_dir) {
            Ok(img) => Event::CoverDone { for_id, img },
            Err(_) => Event::CoverError { for_id },
        };
        tx.post(event);
    })
}

/// Discover cover fetch (04 §7.4): url-keyed both ways.
#[must_use]
pub fn spawn_discover_cover_fetch(
    drain: &Drain,
    tx: EventTx,
    caches: Arc<CoverCaches>,
    covers_dir: PathBuf,
    url: String,
) -> bool {
    drain.spawn("discover-cover", move || {
        let event = match covers::load_cover_pixels(None, &url, &caches, &covers_dir) {
            Ok(img) => Event::DiscoverCoverDone { url, img },
            Err(_) => Event::DiscoverCoverError { url },
        };
        tx.post(event);
    })
}

/// One discover feed page (04 §7.5): single-flight per axis, slot-filed by
/// (axis, page) on arrival, so no generation token is needed.
#[must_use]
pub fn spawn_discover_feed(
    drain: &Drain,
    tx: EventTx,
    catalog: Arc<dyn CatalogProvider>,
    axis: DiscoverAxis,
    page: u32,
) -> bool {
    drain.spawn("discover-feed", move || {
        let event = match catalog.discover(axis, page) {
            Ok(result) => Event::DiscoverFeed {
                axis,
                page,
                entries: result.entries,
                has_next: result.has_next,
            },
            Err(cause) => Event::DiscoverFeedError {
                axis,
                cause: cause.to_string(),
            },
        };
        tx.post(event);
    })
}

/// Loopback OAuth worker (04 §4.6): runs the blocking callback server to
/// completion, posts the outcome. Skips posting on cancel: the app may already
/// be torn down (06 §4.4). Builds its own AniList client for the verify.
#[must_use]
pub fn spawn_connect(
    drain: &Drain,
    tx: EventTx,
    loopback: Loopback,
    auth_path: PathBuf,
    now: i64,
) -> bool {
    drain.spawn("connect", move || {
        // Warn once per worker, not per hit: a local page hammering the port
        // with forged callbacks must not churn the rotating log sink (the
        // serve loop keeps waiting through every one, by design).
        let mut warned = false;
        let result = match AniList::new() {
            Ok(client) => loopback.serve(&client, &auth_path, now, || {
                if !warned {
                    log::warn!("connect: ignoring callback(s) with a bad state");
                    warned = true;
                }
            }),
            Err(_) => ConnectResult::NetworkError,
        };
        if result != ConnectResult::Canceled {
            tx.post(Event::ConnectResult(result));
        }
    })
}

/// Sync worker (04 §4.6): opens its OWN store connection, since the reconcile
/// CAS guard (06 §5.4) is built for exactly this cross-connection concurrency
/// with the UI thread. Runs the pull-then-push funnel and posts the summary.
#[must_use]
pub fn spawn_sync(
    drain: &Drain,
    tx: EventTx,
    db_path: PathBuf,
    auth: Auth,
    enabled: bool,
    pull_only: bool,
    now: i64,
) -> bool {
    drain.spawn("sync", move || {
        let summary = sync_worker(&db_path, &auth, enabled, pull_only, now);
        tx.post(Event::SyncFlushed(summary));
    })
}

fn sync_worker(
    db_path: &std::path::Path,
    auth: &Auth,
    enabled: bool,
    pull_only: bool,
    now: i64,
) -> SyncSummary {
    let (Ok(client), Ok(store)) = (AniList::new(), Store::open(db_path)) else {
        return SyncSummary::terminal(SyncOutcome::Failed);
    };
    sync::run_sync(
        &client,
        auth,
        &store,
        now,
        enabled,
        pull_only,
        &ThreadSleeper,
    )
    .unwrap_or_else(|_| SyncSummary::terminal(SyncOutcome::Failed))
}

/// Quit flush worker (04 §11): push only, no spacing/backoff (`NoSleep`), so it
/// pushes what it can inside teardown's drain deadline.
#[must_use]
pub fn spawn_flush(
    drain: &Drain,
    tx: EventTx,
    db_path: PathBuf,
    auth: Auth,
    enabled: bool,
    now: i64,
) -> bool {
    drain.spawn("sync-flush", move || {
        let summary = match (AniList::new(), Store::open(&db_path)) {
            (Ok(client), Ok(store)) => {
                sync::flush_push(&client, &auth, &store, now, enabled, &sync::NoSleep)
                    .unwrap_or_else(|_| SyncSummary::terminal(SyncOutcome::Failed))
            }
            _ => SyncSummary::terminal(SyncOutcome::Failed),
        };
        tx.post(Event::SyncFlushed(summary));
    })
}

/// One Browse catalogue-search page (04 §4.2): stale results are dropped in
/// tick by comparing `query` against the live buffer, never by generation.
#[must_use]
pub fn spawn_search(
    drain: &Drain,
    tx: EventTx,
    catalog: Arc<dyn CatalogProvider>,
    query: String,
    page: u32,
) -> bool {
    drain.spawn("search", move || {
        let event = match catalog.search(&query, page) {
            Ok(result) => Event::SearchDone {
                query,
                page,
                results: result.entries,
            },
            Err(cause) => Event::SearchFailed {
                query,
                cause: cause.to_string(),
            },
        };
        tx.post(event);
    })
}

/// Detail refresh-on-view fetch (04 §5.1 `enrich_refresh`), three-state
/// answer (05 §8): metadata, confirmed null, or no answer.
#[must_use]
pub fn spawn_enrich(
    drain: &Drain,
    tx: EventTx,
    catalog: Arc<dyn CatalogProvider>,
    anilist_id: i64,
) -> bool {
    drain.spawn("enrich_refresh", move || {
        let event = match catalog.enrich(anilist_id) {
            Ok(Some(e)) => Event::EnrichmentRefreshed {
                for_id: anilist_id,
                enrichment: Box::new(e),
            },
            Ok(None) => Event::EnrichmentNull { for_id: anilist_id },
            Err(_) => Event::EnrichmentFailed { for_id: anilist_id },
        };
        tx.post(event);
    })
}

/// One episode listing fetch, described as data so the spawn stays under the
/// argument lint and the session can log/replay the spec in tests.
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeFetch {
    pub anilist_id: i64,
    pub provider: String,
    pub provider_id: String,
    pub translation: Translation,
    /// Mints a 1..N grid on listing-less providers (03 §2).
    pub count_hint: Option<u32>,
    pub token: u64,
}

/// Provider episode listing (03 §6.1). Staleness is the session generation
/// `token`; the UI never joins, it drops mismatches in tick.
#[must_use]
pub fn spawn_episodes(
    drain: &Drain,
    tx: EventTx,
    registry: Arc<ProviderRegistry>,
    fetch: EpisodeFetch,
) -> bool {
    drain.spawn("episodes", move || {
        let EpisodeFetch {
            anilist_id,
            provider,
            provider_id,
            translation,
            count_hint,
            token,
        } = fetch;
        let event = match registry.by_name(&provider) {
            Some(p) => match p.episodes(&provider_id, translation, count_hint) {
                Ok(episodes) => Event::EpisodesDone {
                    anilist_id,
                    provider,
                    provider_id,
                    episodes,
                    token,
                },
                Err(cause) => Event::EpisodesError {
                    anilist_id,
                    provider,
                    class: (&cause).into(),
                    token,
                },
            },
            // A retired name can only reach here through a stale binding row;
            // surface it as a data failure, never fetch on primary (03 §3.2).
            None => Event::EpisodesError {
                anilist_id,
                provider,
                class: FetchClass::Data,
                token,
            },
        };
        tx.post(event);
    })
}

/// One tier-C binding search (03 §4.2, page 1 of `SEARCH_PAGE_SIZE`); the
/// scorers run offline on the UI thread when the hits land.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSearch {
    pub anilist_id: i64,
    pub provider: String,
    pub query: String,
    pub translation: Translation,
    pub token: u64,
}

#[must_use]
pub fn spawn_provider_search(
    drain: &Drain,
    tx: EventTx,
    registry: Arc<ProviderRegistry>,
    search: ProviderSearch,
) -> bool {
    drain.spawn("provider-search", move || {
        let ProviderSearch {
            anilist_id,
            provider,
            query,
            translation,
            token,
        } = search;
        let opts = SearchOptions {
            translation,
            limit: SEARCH_PAGE_SIZE,
            page: 1,
        };
        let event = match registry.by_name(&provider) {
            Some(p) => match p.search(&query, &opts) {
                Ok(hits) => Event::ProviderSearchDone {
                    anilist_id,
                    provider,
                    hits,
                    token,
                },
                Err(cause) => Event::ProviderSearchError {
                    anilist_id,
                    provider,
                    class: (&cause).into(),
                    token,
                },
            },
            None => Event::ProviderSearchError {
                anilist_id,
                provider,
                class: FetchClass::Data,
                token,
            },
        };
        tx.post(event);
    })
}

/// One prewarm candidate, described as data (03 §6.5). The worker runs the
/// whole tier-A-or-search chain blocking and always answers with exactly one
/// `PrewarmResult`.
#[derive(Debug, Clone)]
pub struct PrewarmProbe {
    pub provider: String,
    pub canonical: Enrichment,
    pub translation: Translation,
    pub token: u64,
}

#[must_use]
pub fn spawn_prewarm_probe(
    drain: &Drain,
    tx: EventTx,
    registry: Arc<ProviderRegistry>,
    probe: PrewarmProbe,
) -> bool {
    drain.spawn("prewarm", move || {
        let PrewarmProbe {
            provider,
            canonical,
            translation,
            token,
        } = probe;
        let verdict = match registry.by_name(&provider) {
            Some(p) => probe_candidate(p, &canonical, translation),
            None => PrewarmVerdict::Nothing,
        };
        tx.post(Event::PrewarmResult {
            anilist_id: canonical.anilist_id,
            provider,
            verdict,
            token,
        });
    })
}

/// Per-candidate classification, mirroring the user walk (03 §4.3): tier-A
/// key → episodes; else title search → episodes on the match. Only an
/// authoritative empty listing is an absence; a search miss or any transport
/// error learns nothing (03 §6.4: no new absence rule).
fn probe_candidate(
    p: &dyn StreamProvider,
    canonical: &Enrichment,
    translation: Translation,
) -> PrewarmVerdict {
    let count_hint = expected_episode_count(
        canonical.status.as_deref(),
        canonical.total_episodes,
        canonical.next_airing_episode,
    );
    let classify = |id: String, listing: Result<Vec<String>, ProviderError>| match listing {
        Ok(eps) if !eps.is_empty() => PrewarmVerdict::Found { provider_id: id },
        Ok(_) => PrewarmVerdict::Absent,
        Err(_) => PrewarmVerdict::Nothing,
    };
    if let Some(key) = p.canonical_key(canonical) {
        let listing = p.episodes(&key, translation, count_hint);
        return classify(key, listing);
    }
    let opts = SearchOptions {
        translation,
        limit: SEARCH_PAGE_SIZE,
        page: 1,
    };
    let hits = match p.search(&canonical.title_romaji, &opts) {
        Ok(hits) => hits,
        Err(_) => return PrewarmVerdict::Nothing,
    };
    let Some(ix) = resolver::best_id_match(canonical, &hits)
        .or_else(|| resolver::best_provider_match(canonical, &hits))
    else {
        return PrewarmVerdict::Nothing;
    };
    let id = hits[ix].provider_id.clone();
    let listing = p.episodes(&id, translation, count_hint);
    classify(id, listing)
}

/// One play, described as data: everything the worker needs without touching
/// App state (the resolve runs per attempt inside `player::play`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaySpec {
    pub anilist_id: i64,
    pub provider: String,
    pub provider_id: String,
    pub episode_label: String,
    /// 1-based; the AniSkip ordinal fallback (03 §9).
    pub episode_ix: u32,
    pub translation: Translation,
    /// The Settings cap policy, applied at variant selection (DESIGN 5.5).
    pub quality: Quality,
    pub title: String,
    pub start_secs: f64,
    pub mpv_path: String,
    pub socket_dir: PathBuf,
    /// AniSkip inputs (03 §9): MAL key, config mode, skip.lua home.
    pub mal_id: Option<i64>,
    pub skip_mode: SkipMode,
    pub cache_dir: PathBuf,
    pub token: u64,
}

/// Floor between position posts. mpv emits `time-pos` per frame; the UI only
/// needs the launching-cell flip and the 30s checkpoint cadence, so the
/// bridge throttles here rather than flooding the queue (ROD-437 note).
const POSITION_POST_FLOOR: Duration = Duration::from_millis(500);

/// The play worker (04 §7.8): resolve + mpv + IPC live inside `player::play`;
/// this glue forwards its events with the session token and posts the one
/// terminal outcome. The worker outlives supersede checks by design; the UI
/// drops stale tokens in tick and never joins.
#[must_use]
pub fn spawn_play(
    drain: &Drain,
    tx: EventTx,
    registry: Arc<ProviderRegistry>,
    spec: PlaySpec,
) -> bool {
    drain.spawn("play", move || {
        let PlaySpec {
            anilist_id,
            provider,
            provider_id,
            episode_label,
            episode_ix,
            translation,
            quality,
            title,
            start_secs,
            mpv_path,
            socket_dir,
            mal_id,
            skip_mode,
            cache_dir,
            token,
        } = spec;
        // A retired name can only arrive through a stale binding row.
        let Some(p) = registry.by_name(&provider) else {
            tx.post(Event::PlayFinished {
                anilist_id,
                position: None,
                failure: Some(PlayFailure::Resolve(FetchClass::Data)),
                token,
            });
            return;
        };
        // AniSkip prepared once, before the attempts (04 §7.8); best-effort,
        // a miss is a plain play.
        let skip = aniskip::prepare(
            mal_id,
            aniskip::episode_number(&episode_label, episode_ix),
            skip_mode,
            &cache_dir,
        );
        let opts = PlayOpts {
            mpv_path: &mpv_path,
            socket_dir: &socket_dir,
            title: &title,
            start_secs,
            skip: skip.as_ref(),
        };
        let bridge = PositionBridge {
            tx: tx.clone(),
            anilist_id,
            token,
            last: Arc::new(Mutex::new(None)),
        };
        let result = player::play(
            &opts,
            || {
                p.resolve(&provider_id, &episode_label, translation, quality)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            },
            move |event| bridge.forward(event),
        );
        let (position, failure) = match result {
            Ok(outcome) => (outcome.position, None),
            Err(e) => (None, Some(play_failure(&e))),
        };
        tx.post(Event::PlayFinished {
            anilist_id,
            position,
            failure,
            token,
        });
    })
}

/// `PlayError` → the POD classes the toast matrix keys on (DESIGN 4.10).
/// Guard/proxy/wait failures land in `Internal`: residual, `playback failed`.
/// Public so the CLI play path (main) reuses the same classification, including
/// the Resolve downcast, rather than re-deriving it.
pub fn play_failure(e: &PlayError) -> PlayFailure {
    match e {
        PlayError::Resolve(cause) => cause
            .downcast_ref::<ProviderError>()
            .map_or(PlayFailure::Internal, |pe| PlayFailure::Resolve(pe.into())),
        PlayError::Spawn { source, .. } if source.kind() == io::ErrorKind::NotFound => {
            PlayFailure::MpvNotFound
        }
        PlayError::OpenFailed { .. } => PlayFailure::OpenFailed,
        PlayError::Exit { .. } => PlayFailure::MpvFailed,
        _ => PlayFailure::Internal,
    }
}

/// Clone-per-attempt event forwarder (player.rs takes `on_event` by value per
/// attempt); the shared throttle clock keeps the floor across attempts.
#[derive(Clone)]
struct PositionBridge {
    tx: EventTx,
    anilist_id: i64,
    token: u64,
    last: Arc<Mutex<Option<Instant>>>,
}

impl PositionBridge {
    fn forward(&self, event: PlayerEvent) {
        match event {
            PlayerEvent::Retry { attempt } => self.tx.post(Event::PlayRetry {
                anilist_id: self.anilist_id,
                attempt,
                token: self.token,
            }),
            PlayerEvent::Position(position) => {
                let mut last = self.last.lock().unwrap();
                let now = Instant::now();
                if last.is_none_or(|t| now.saturating_duration_since(t) >= POSITION_POST_FLOOR) {
                    *last = Some(now);
                    self.tx.post(Event::PlayPosition {
                        anilist_id: self.anilist_id,
                        position,
                        token: self.token,
                    });
                }
            }
        }
    }
}

/// Inflight accounting for one worker family (04 §5.1).
#[derive(Debug, Clone, Default)]
pub struct Drain {
    state: Arc<(Mutex<usize>, Condvar)>,
    panics: Arc<AtomicUsize>,
}

impl Drain {
    /// Increment inflight on the calling (UI) thread. Move the guard into the
    /// worker; its drop is the finish. A failed spawn drops it on the spot,
    /// which is exactly the required immediate finish (04 §5).
    pub fn begin(&self) -> FinishGuard {
        *self.state.0.lock().unwrap() += 1;
        FinishGuard {
            state: Arc::clone(&self.state),
        }
    }

    /// begin + detached spawn in the contract-correct order. Returns false if
    /// the OS refused the thread (accounting already settled). A panicking
    /// worker is contained: accounting settles, `panics()` counts it, and the
    /// terminal-restoring panic hook must be main-thread-scoped (see tui::run)
    /// or the hook still fires before the unwind reaches the catch here.
    #[must_use]
    pub fn spawn(&self, name: &str, work: impl FnOnce() + Send + 'static) -> bool {
        let guard = self.begin();
        let panics = Arc::clone(&self.panics);
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let _finish_last = guard;
                if let Err(cause) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
                    panics.fetch_add(1, Ordering::Relaxed);
                    // The scoped hook is mute off-main; without this line a
                    // worker panic would leave no trace but the counter.
                    let msg = cause
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| cause.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unknown panic payload");
                    log::error!("worker panicked: {msg}");
                }
            })
            .is_ok()
    }

    pub fn inflight(&self) -> usize {
        *self.state.0.lock().unwrap()
    }

    /// Workers that died by panic; a later ticket surfaces this as a toast.
    pub fn panics(&self) -> usize {
        self.panics.load(Ordering::Relaxed)
    }

    /// Teardown only (04 §5): block until inflight hits zero or the deadline
    /// passes. Returns true on zero. Never call this to supersede work.
    pub fn drain(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (lock, cvar) = &*self.state;
        let mut inflight = lock.lock().unwrap();
        while *inflight > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, timed_out) = cvar.wait_timeout(inflight, left).unwrap();
            inflight = next;
            if timed_out.timed_out() && *inflight > 0 {
                return false;
            }
        }
        true
    }
}

/// Held by the worker for its whole life; dropping it is the finish.
#[derive(Debug)]
pub struct FinishGuard {
    state: Arc<(Mutex<usize>, Condvar)>,
}

impl Drop for FinishGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.state;
        *lock.lock().unwrap() -= 1;
        cvar.notify_all();
    }
}

/// Staleness token per subsystem (04 §6): UI bumps when a new fetch supersedes,
/// worker results carry the token they were spawned with, tick drops mismatches.
#[derive(Debug, Default)]
pub struct Generation(AtomicU64);

impl Generation {
    /// Supersede: invalidates every outstanding token, returns the new one.
    pub fn bump(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn current(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn is_current(&self, token: u64) -> bool {
        self.current() == token
    }
}

/// Cooperative cancellation (04 §7.6): workers poll, UI flips. Reusable via
/// `reset` for single-flight subsystems like the prewarm walk.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Enrichment;
    use crate::tui::event::{self, Event};
    use std::sync::mpsc;

    /// Identity row via the binding mint (02 §3.7): present, bindable, and
    /// carrying NO membership, so History appearance below is record_play's
    /// own set-once stamp.
    fn seed_bound_show(store: &Store, id: i64) {
        let e = Enrichment {
            anilist_id: id,
            title_romaji: format!("Show {id}"),
            ..Default::default()
        };
        store.bind_provider(&e, "senshi", "prov-1", 100).unwrap();
        assert!(store.list_history().unwrap().is_empty());
    }

    fn pos(secs: f64, duration: Option<f64>) -> Option<Position> {
        Some(Position { secs, duration })
    }

    #[test]
    fn finish_without_position_writes_nothing() {
        let store = Store::open_memory().unwrap();
        seed_bound_show(&store, 7);
        let recorded = finish_playback(
            &store,
            7,
            Translation::Sub,
            "1",
            1,
            None,
            Some("senshi"),
            200,
        )
        .unwrap();
        assert!(!recorded);
        assert!(store.list_history().unwrap().is_empty());
        assert_eq!(store.get_resume(7, Translation::Sub, "1").unwrap(), None);
    }

    #[test]
    fn partial_watch_lands_in_history_without_ratchet() {
        let store = Store::open_memory().unwrap();
        seed_bound_show(&store, 7);
        let recorded = finish_playback(
            &store,
            7,
            Translation::Sub,
            "3",
            3,
            pos(300.0, Some(1420.0)),
            Some("senshi"),
            200,
        )
        .unwrap();
        assert!(recorded);
        let history = store.list_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].play_count, 1);
        assert_eq!(history[0].progress, 0, "0.21 ratio must not ratchet");
        assert_eq!(history[0].last_watched_at, Some(200));
        let resume = store.get_resume(7, Translation::Sub, "3").unwrap().unwrap();
        assert_eq!(resume.position_secs, 300.0);
        assert!(!resume.fully_watched);
    }

    #[test]
    fn natural_end_ratchets_but_does_not_mark_watched() {
        let store = Store::open_memory().unwrap();
        seed_bound_show(&store, 7);
        finish_playback(
            &store,
            7,
            Translation::Sub,
            "5",
            5,
            pos(1200.0, Some(1420.0)),
            Some("senshi"),
            200,
        )
        .unwrap();
        let history = store.list_history().unwrap();
        assert_eq!(history[0].progress, 5, "0.845 ratio ratchets");
        let resume = store.get_resume(7, Translation::Sub, "5").unwrap().unwrap();
        assert!(!resume.fully_watched, "0.845 is under WATCHED_RATIO");
    }

    #[test]
    fn watched_tier_marks_fully_watched() {
        let store = Store::open_memory().unwrap();
        seed_bound_show(&store, 7);
        finish_playback(
            &store,
            7,
            Translation::Sub,
            "5",
            5,
            pos(1400.0, Some(1420.0)),
            Some("senshi"),
            200,
        )
        .unwrap();
        assert_eq!(store.list_history().unwrap()[0].progress, 5);
        assert!(
            store
                .get_resume(7, Translation::Sub, "5")
                .unwrap()
                .unwrap()
                .fully_watched
        );
    }

    #[test]
    fn unknown_duration_records_engagement_only() {
        let store = Store::open_memory().unwrap();
        seed_bound_show(&store, 7);
        finish_playback(
            &store,
            7,
            Translation::Sub,
            "5",
            5,
            pos(900.0, None),
            None,
            200,
        )
        .unwrap();
        let history = store.list_history().unwrap();
        assert_eq!(history[0].play_count, 1);
        assert_eq!(history[0].progress, 0, "no duration, no ratchet");
    }

    #[test]
    fn play_failure_maps_every_error_shape() {
        use std::os::unix::process::ExitStatusExt;
        let resolve_err = |e: ProviderError| {
            PlayError::Resolve(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        };
        assert_eq!(
            play_failure(&resolve_err(ProviderError::Forbidden { status: 403 })),
            PlayFailure::Resolve(FetchClass::Blocked)
        );
        assert_eq!(
            play_failure(&resolve_err(ProviderError::Network)),
            PlayFailure::Resolve(FetchClass::Network)
        );
        // A non-provider resolve error is the residual bucket.
        assert_eq!(
            play_failure(&PlayError::Resolve("weird".into())),
            PlayFailure::Internal
        );
        assert_eq!(
            play_failure(&PlayError::Spawn {
                mpv: "mpv".into(),
                source: io::Error::from(io::ErrorKind::NotFound),
            }),
            PlayFailure::MpvNotFound
        );
        assert_eq!(
            play_failure(&PlayError::Spawn {
                mpv: "mpv".into(),
                source: io::Error::from(io::ErrorKind::PermissionDenied),
            }),
            PlayFailure::Internal
        );
        assert_eq!(
            play_failure(&PlayError::OpenFailed { attempts: 3 }),
            PlayFailure::OpenFailed
        );
        assert_eq!(
            play_failure(&PlayError::Exit {
                status: std::process::ExitStatus::from_raw(1 << 8),
            }),
            PlayFailure::MpvFailed
        );
    }

    #[test]
    fn position_bridge_throttles_positions_but_never_retries() {
        let (tx, rx) = event::channel();
        let bridge = PositionBridge {
            tx,
            anilist_id: 7,
            token: 3,
            last: Arc::new(Mutex::new(None)),
        };
        let at = |secs: f64| {
            PlayerEvent::Position(Position {
                secs,
                duration: None,
            })
        };
        bridge.forward(at(1.0));
        bridge.forward(at(1.1));
        bridge.forward(PlayerEvent::Retry { attempt: 2 });
        bridge.forward(at(1.2));
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(
            events,
            [
                Event::PlayPosition {
                    anilist_id: 7,
                    position: Position {
                        secs: 1.0,
                        duration: None
                    },
                    token: 3,
                },
                Event::PlayRetry {
                    anilist_id: 7,
                    attempt: 2,
                    token: 3,
                },
            ],
            "the first position passes, the burst is folded, retry always posts"
        );
    }

    #[test]
    fn spawn_play_with_a_retired_name_posts_the_data_failure() {
        let (tx, rx) = event::channel();
        let drain = Drain::default();
        let registry = crate::tui::episodes::teststub::inert_registry();
        assert!(spawn_play(
            &drain,
            tx,
            registry,
            PlaySpec {
                anilist_id: 7,
                provider: "gone".into(),
                provider_id: "x".into(),
                episode_label: "1".into(),
                episode_ix: 1,
                translation: Translation::Sub,
                quality: Quality::Best,
                title: "t".into(),
                start_secs: 0.0,
                mpv_path: "mpv".into(),
                socket_dir: PathBuf::from("/tmp"),
                mal_id: None,
                skip_mode: SkipMode::None,
                cache_dir: PathBuf::from("/tmp"),
                token: 9,
            },
        ));
        assert!(drain.drain(Duration::from_secs(5)));
        assert_eq!(
            rx.try_recv().unwrap(),
            Event::PlayFinished {
                anilist_id: 7,
                position: None,
                failure: Some(PlayFailure::Resolve(FetchClass::Data)),
                token: 9,
            }
        );
    }

    #[test]
    fn spawn_sync_bridges_a_summary_to_the_queue() {
        let (tx, rx) = event::channel();
        let drain = Drain::default();
        // Disabled short-circuits before any network, so this needs no socket.
        let db = std::env::temp_dir().join("sabigoku-spawn-sync-disabled.db");
        let _ = std::fs::remove_file(&db);
        assert!(spawn_sync(&drain, tx, db, Auth::default(), false, false, 0));
        assert!(drain.drain(Duration::from_secs(5)));
        match rx.try_recv().unwrap() {
            Event::SyncFlushed(s) => assert_eq!(s.outcome, SyncOutcome::Disabled),
            other => panic!("expected SyncFlushed, got {other:?}"),
        }
    }

    #[test]
    fn begin_before_spawn_counts_immediately() {
        let drain = Drain::default();
        let guard = drain.begin();
        assert_eq!(drain.inflight(), 1);
        drop(guard);
        assert_eq!(drain.inflight(), 0);
    }

    #[test]
    fn finish_lands_after_last_post() {
        let (tx, rx) = event::channel();
        let drain = Drain::default();
        assert!(drain.spawn("post-then-finish", move || {
            tx.post(Event::Tick);
        }));
        assert!(drain.drain(Duration::from_secs(5)));
        assert_eq!(rx.try_recv().unwrap(), Event::Tick);
    }

    #[test]
    fn drain_waits_for_slow_worker() {
        let drain = Drain::default();
        assert!(drain.spawn("slow", || std::thread::sleep(Duration::from_millis(50))));
        assert!(drain.drain(Duration::from_secs(5)));
        assert_eq!(drain.inflight(), 0);
    }

    #[test]
    fn drain_times_out_instead_of_hanging() {
        let drain = Drain::default();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        assert!(drain.spawn("stuck", move || {
            let _ = release_rx.recv();
        }));
        assert!(!drain.drain(Duration::from_millis(50)));
        release_tx.send(()).unwrap();
        assert!(drain.drain(Duration::from_secs(5)));
    }

    #[test]
    fn supersede_never_waits() {
        let drain = Drain::default();
        let generation = Generation::default();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let stale_token = generation.current();
        assert!(drain.spawn("superseded", move || {
            let _ = release_rx.recv();
        }));
        let fresh_token = generation.bump();
        assert!(!generation.is_current(stale_token));
        assert!(generation.is_current(fresh_token));
        assert_eq!(drain.inflight(), 1);
        release_tx.send(()).unwrap();
        assert!(drain.drain(Duration::from_secs(5)));
    }

    #[test]
    fn panicking_worker_settles_accounting_and_is_counted() {
        let drain = Drain::default();
        assert!(drain.spawn("kamikaze", || panic!("contained")));
        assert!(drain.drain(Duration::from_secs(5)));
        assert_eq!(drain.inflight(), 0);
        assert_eq!(drain.panics(), 1);
        assert!(drain.spawn("after", || {}));
        assert!(drain.drain(Duration::from_secs(5)));
    }

    #[test]
    fn cancel_stops_a_looping_worker() {
        let drain = Drain::default();
        let cancel = CancelFlag::default();
        let seen = cancel.clone();
        assert!(drain.spawn("loop", move || {
            while !seen.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }));
        cancel.cancel();
        assert!(drain.drain(Duration::from_secs(5)));
        cancel.reset();
        assert!(!cancel.is_cancelled());
    }
}
