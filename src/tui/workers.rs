//! Worker accounting and staleness kit, the ThreadDrain port (04 §5, §6).
//! Law: begin BEFORE spawn; finish only after the worker's last touch of the
//! queue; drain only at teardown. Supersede is detach + stale-token drop,
//! NEVER a join on the hot path.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
    use crate::tui::event::{self, Event};
    use std::sync::mpsc;

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
