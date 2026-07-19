//! Worker accounting and staleness kit, the ThreadDrain port (04 §5, §6).
//! Law: begin BEFORE spawn; finish only after the worker's last touch of the
//! queue; drain only at teardown. Supersede is detach + stale-token drop,
//! NEVER a join on the hot path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::domain::Translation;
use crate::error::Error;
use crate::player::Position;
use crate::providers::{CatalogProvider, DiscoverAxis};
use crate::store::Store;
use crate::tui::covers::{self, CoverCaches};
use crate::tui::event::{Event, EventTx};

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
                    eprintln!("worker panicked: {msg}");
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
