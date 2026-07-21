//! AniList sync orchestration (06 §5). Imports: anilist, auth, domain, store.
//! Every TUI entry funnels through [`run_sync`], whose first act is the pull,
//! so push-first can never wipe never-synced rows (06 §5.2). The one push-only
//! path is [`flush_push`] (quit flush), gated by the caller on no pull inflight.
//!
//! Store-keyed on `anilist_id`, so every library row is linked; the freeze's
//! engaged-but-unlinked summary (06 §5.3/§5.6) has no rows to report here.

use std::time::Duration;

use crate::anilist::{AniList, RemoteEntry};
use crate::auth::Auth;
use crate::domain::ListStatus;
use crate::error::Error;
use crate::providers::CatalogError;
use crate::store::{PullOutcome, Store};

/// Spacing between push calls (06 §5.3, AniList rate).
const PUSH_SPACING: Duration = Duration::from_secs(2);
/// One-time backoff on a 429 before retrying the row (06 §5.3).
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

/// The AniList calls sync needs, behind a trait so tests script pull/push
/// without a socket. Distinct names from the inherent methods they delegate to.
pub trait AniListSync {
    fn fetch_list(&self, token: &str, user_id: i64) -> Result<Vec<RemoteEntry>, CatalogError>;
    fn save_entry(
        &self,
        token: &str,
        media_id: i64,
        status: ListStatus,
        progress: u32,
    ) -> Result<i64, CatalogError>;
}

impl AniListSync for AniList {
    fn fetch_list(&self, token: &str, user_id: i64) -> Result<Vec<RemoteEntry>, CatalogError> {
        self.pull_list(token, user_id)
    }
    fn save_entry(
        &self,
        token: &str,
        media_id: i64,
        status: ListStatus,
        progress: u32,
    ) -> Result<i64, CatalogError> {
        self.push_entry(token, media_id, status, progress)
    }
}

/// Blocking waits, behind a trait so tests assert the 2s/60s schedule without
/// real sleeps.
pub trait Sleeper {
    fn sleep(&self, dur: Duration);
}

pub struct ThreadSleeper;
impl Sleeper for ThreadSleeper {
    fn sleep(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

/// Terminal reason for a sync run. Store-level failures are `Err`, not a
/// variant here.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncOutcome {
    Completed,
    /// Master switch off or not connected: a no-op run.
    Disabled,
    NoToken,
    Expired,
    /// Pull hit 401/429/network; push skipped (06 §5.2).
    PullFailed,
    /// 401 during push; run stopped (06 §5.3).
    Unauthorized,
    /// Second 429 during push; run stopped, the rest stay dirty (06 §5.3).
    RateLimited,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyncSummary {
    pub outcome: SyncOutcome,
    pub pulled: PullOutcome,
    pub pushed: u32,
    pub push_failed: u32,
}

impl SyncSummary {
    fn terminal(outcome: SyncOutcome) -> Self {
        SyncSummary {
            outcome,
            pulled: PullOutcome::default(),
            pushed: 0,
            push_failed: 0,
        }
    }
}

/// A usable bearer + user id, or the terminal reason it is not.
fn usable_auth(auth: &Auth, now: i64) -> Result<(&str, i64), SyncOutcome> {
    let Some(token) = auth.anilist.bearer() else {
        return Err(SyncOutcome::NoToken);
    };
    if auth.anilist.is_expired(now) {
        return Err(SyncOutcome::Expired);
    }
    if auth.anilist.user_id <= 0 {
        return Err(SyncOutcome::NoToken);
    }
    Ok((token, auth.anilist.user_id))
}

/// The sync funnel (06 §5.2): pull, reconcile, then push unless `pull_only`.
/// `enabled` is the master switch; the caller passes connected + enabled.
pub fn run_sync<A: AniListSync, S: Sleeper>(
    client: &A,
    auth: &Auth,
    store: &Store,
    now: i64,
    enabled: bool,
    pull_only: bool,
    sleeper: &S,
) -> Result<SyncSummary, Error> {
    if !enabled {
        return Ok(SyncSummary::terminal(SyncOutcome::Disabled));
    }
    let (token, user_id) = match usable_auth(auth, now) {
        Ok(v) => v,
        Err(o) => return Ok(SyncSummary::terminal(o)),
    };
    let remote = match client.fetch_list(token, user_id) {
        Ok(r) => r,
        Err(_) => return Ok(SyncSummary::terminal(SyncOutcome::PullFailed)),
    };
    let pulled = store.reconcile_pull(&remote)?;
    let mut summary = SyncSummary {
        outcome: SyncOutcome::Completed,
        pulled,
        pushed: 0,
        push_failed: 0,
    };
    if pull_only {
        return Ok(summary);
    }
    push_dirty(client, token, store, sleeper, &mut summary)?;
    Ok(summary)
}

/// Quit flush (06 §5.2): push only, no pull. The caller skips this while a pull
/// is inflight and bounds it (04 §11) via the `sleeper` it passes.
pub fn flush_push<A: AniListSync, S: Sleeper>(
    client: &A,
    auth: &Auth,
    store: &Store,
    now: i64,
    enabled: bool,
    sleeper: &S,
) -> Result<SyncSummary, Error> {
    if !enabled {
        return Ok(SyncSummary::terminal(SyncOutcome::Disabled));
    }
    let token = match usable_auth(auth, now) {
        Ok((t, _)) => t,
        Err(o) => return Ok(SyncSummary::terminal(o)),
    };
    let mut summary = SyncSummary::terminal(SyncOutcome::Completed);
    push_dirty(client, token, store, sleeper, &mut summary)?;
    Ok(summary)
}

/// Push the dirty set (06 §5.3): 2s between rows, 401 stops the run, a first 429
/// backs off once and retries the row, a second 429 stops the run leaving the
/// rest dirty. Other per-row errors count and continue. A success advances the
/// snapshot only after AniList returned a non-null id (enforced in `save_entry`).
fn push_dirty<A: AniListSync, S: Sleeper>(
    client: &A,
    token: &str,
    store: &Store,
    sleeper: &S,
    summary: &mut SyncSummary,
) -> Result<(), Error> {
    let dirty = store.list_dirty_for_sync()?;
    let mut backed_off = false;
    for (i, row) in dirty.iter().enumerate() {
        if i > 0 {
            sleeper.sleep(PUSH_SPACING);
        }
        loop {
            match client.save_entry(token, row.anilist_id, row.list_status, row.progress) {
                Ok(_) => {
                    store.mark_synced(row.anilist_id, row.list_status, row.progress)?;
                    summary.pushed += 1;
                    break;
                }
                Err(CatalogError::Http { status: 401 }) => {
                    summary.outcome = SyncOutcome::Unauthorized;
                    return Ok(());
                }
                Err(CatalogError::RateLimited) => {
                    if backed_off {
                        summary.outcome = SyncOutcome::RateLimited;
                        return Ok(());
                    }
                    backed_off = true;
                    sleeper.sleep(RATE_LIMIT_BACKOFF);
                    // retry the same row
                }
                Err(_) => {
                    summary.push_failed += 1;
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Enrichment;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct FakeAni {
        remote: RefCell<Option<Result<Vec<RemoteEntry>, CatalogError>>>,
        pushes: RefCell<VecDeque<Result<i64, CatalogError>>>,
        pull_calls: RefCell<u32>,
        push_calls: RefCell<Vec<(i64, ListStatus, u32)>>,
    }

    impl FakeAni {
        fn new(remote: Result<Vec<RemoteEntry>, CatalogError>, pushes: Vec<Result<i64, CatalogError>>) -> Self {
            FakeAni {
                remote: RefCell::new(Some(remote)),
                pushes: RefCell::new(pushes.into()),
                pull_calls: RefCell::new(0),
                push_calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl AniListSync for FakeAni {
        fn fetch_list(&self, _t: &str, _u: i64) -> Result<Vec<RemoteEntry>, CatalogError> {
            *self.pull_calls.borrow_mut() += 1;
            self.remote.borrow_mut().take().unwrap_or(Ok(vec![]))
        }
        fn save_entry(&self, _t: &str, media_id: i64, status: ListStatus, progress: u32) -> Result<i64, CatalogError> {
            self.push_calls.borrow_mut().push((media_id, status, progress));
            self.pushes.borrow_mut().pop_front().unwrap_or(Ok(1))
        }
    }

    struct RecordingSleeper {
        slept: RefCell<Vec<Duration>>,
    }
    impl RecordingSleeper {
        fn new() -> Self {
            RecordingSleeper { slept: RefCell::new(Vec::new()) }
        }
    }
    impl Sleeper for RecordingSleeper {
        fn sleep(&self, dur: Duration) {
            self.slept.borrow_mut().push(dur);
        }
    }

    fn connected(now_offset: i64) -> Auth {
        let mut auth = Auth::default();
        auth.anilist.access_token = "token-value".into();
        auth.anilist.user_id = 7;
        auth.anilist.expires_at = if now_offset == 0 { 0 } else { now_offset };
        auth
    }

    fn dirty_lib(store: &Store, id: i64) {
        let e = Enrichment {
            anilist_id: id,
            title_romaji: format!("Show {id}"),
            ..Enrichment::default()
        };
        store.add_to_library(&e, 100).unwrap();
    }

    #[test]
    fn disabled_is_a_noop() {
        let client = FakeAni::new(Ok(vec![]), vec![]);
        let store = Store::open_memory().unwrap();
        let out = run_sync(&client, &connected(0), &store, 0, false, false, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::Disabled);
        assert_eq!(*client.pull_calls.borrow(), 0);
        assert!(client.push_calls.borrow().is_empty());
    }

    #[test]
    fn no_token_and_expired_short_circuit_before_any_call() {
        let store = Store::open_memory().unwrap();
        let client = FakeAni::new(Ok(vec![]), vec![]);
        let out = run_sync(&client, &Auth::default(), &store, 0, true, false, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::NoToken);
        assert_eq!(*client.pull_calls.borrow(), 0);

        let client = FakeAni::new(Ok(vec![]), vec![]);
        // Token dated at 1000, now is 2000: expired.
        let out = run_sync(&client, &connected(1000), &store, 2000, true, false, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::Expired);
        assert_eq!(*client.pull_calls.borrow(), 0);
    }

    #[test]
    fn pull_only_reconciles_and_never_pushes() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 10); // a dirty row exists, but pull_only must not push it
        let client = FakeAni::new(Ok(vec![RemoteEntry { anilist_id: 10, status: ListStatus::Watching, progress: 3 }]), vec![]);
        let out = run_sync(&client, &connected(0), &store, 0, true, true, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::Completed);
        assert_eq!(out.pulled.reconciled, 1);
        assert_eq!(*client.pull_calls.borrow(), 1);
        assert!(client.push_calls.borrow().is_empty(), "pull_only must not push");
    }

    #[test]
    fn pull_failure_skips_push() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 11);
        let client = FakeAni::new(Err(CatalogError::Http { status: 401 }), vec![Ok(1)]);
        let out = run_sync(&client, &connected(0), &store, 0, true, false, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::PullFailed);
        assert!(client.push_calls.borrow().is_empty(), "push must not run after a failed pull");
    }

    #[test]
    fn push_after_pull_advances_the_snapshot() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 12);
        let client = FakeAni::new(Ok(vec![]), vec![Ok(555)]);
        let out = run_sync(&client, &connected(0), &store, 0, true, false, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::Completed);
        assert_eq!(out.pushed, 1);
        assert_eq!(client.push_calls.borrow()[0], (12, ListStatus::Planning, 0));
        // Snapshot advanced: the row is no longer dirty.
        assert!(store.list_dirty_for_sync().unwrap().is_empty());
    }

    #[test]
    fn push_401_stops_the_run_and_leaves_rows_dirty() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 13);
        dirty_lib(&store, 14);
        let client = FakeAni::new(Ok(vec![]), vec![Err(CatalogError::Http { status: 401 }), Ok(1)]);
        let out = run_sync(&client, &connected(0), &store, 0, true, false, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::Unauthorized);
        assert_eq!(out.pushed, 0);
        assert_eq!(client.push_calls.borrow().len(), 1, "stops at the first 401");
        assert_eq!(store.list_dirty_for_sync().unwrap().len(), 2, "both rows stay dirty");
    }

    #[test]
    fn push_429_backs_off_once_then_a_second_stops_the_run() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 15);
        dirty_lib(&store, 16);
        // Row15: 429 -> backoff -> Ok. Row16: 429 -> second, stop.
        let client = FakeAni::new(
            Ok(vec![]),
            vec![Err(CatalogError::RateLimited), Ok(1), Err(CatalogError::RateLimited)],
        );
        let sleeper = RecordingSleeper::new();
        let out = run_sync(&client, &connected(0), &store, 0, true, false, &sleeper).unwrap();
        assert_eq!(out.outcome, SyncOutcome::RateLimited);
        assert_eq!(out.pushed, 1);
        // 60s backoff on row15, then 2s spacing before row16.
        assert_eq!(*sleeper.slept.borrow(), vec![RATE_LIMIT_BACKOFF, PUSH_SPACING]);
        // Row16 stays dirty for the next run.
        assert_eq!(store.list_dirty_for_sync().unwrap().len(), 1);
    }

    #[test]
    fn push_spacing_is_between_rows_only() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 17);
        dirty_lib(&store, 18);
        dirty_lib(&store, 19);
        let client = FakeAni::new(Ok(vec![]), vec![Ok(1), Ok(1), Ok(1)]);
        let sleeper = RecordingSleeper::new();
        let out = run_sync(&client, &connected(0), &store, 0, true, false, &sleeper).unwrap();
        assert_eq!(out.pushed, 3);
        // Three rows -> two gaps, no leading sleep.
        assert_eq!(*sleeper.slept.borrow(), vec![PUSH_SPACING, PUSH_SPACING]);
    }

    #[test]
    fn per_row_error_counts_and_continues() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 20);
        dirty_lib(&store, 21);
        let client = FakeAni::new(Ok(vec![]), vec![Err(CatalogError::Network), Ok(1)]);
        let out = run_sync(&client, &connected(0), &store, 0, true, false, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::Completed);
        assert_eq!(out.push_failed, 1);
        assert_eq!(out.pushed, 1);
        assert_eq!(client.push_calls.borrow().len(), 2, "both rows attempted");
    }

    #[test]
    fn flush_push_is_push_only() {
        let store = Store::open_memory().unwrap();
        dirty_lib(&store, 22);
        let client = FakeAni::new(Ok(vec![RemoteEntry { anilist_id: 99, status: ListStatus::Watching, progress: 1 }]), vec![Ok(1)]);
        let out = flush_push(&client, &connected(0), &store, 0, true, &RecordingSleeper::new()).unwrap();
        assert_eq!(out.outcome, SyncOutcome::Completed);
        assert_eq!(out.pushed, 1);
        assert_eq!(*client.pull_calls.borrow(), 0, "quit flush never pulls");
    }
}
