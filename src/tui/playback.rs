//! Playback session: the transport around `player::play` (04 §7.7-7.8). Owns
//! the active play, its staleness token, the 30s checkpoint cadence, and the
//! finish writes; App dispatches events here and turns the returned
//! `PlayFeedback` into toasts. Lives with its subsystem per the ROD-439
//! rules, never on App.
//!
//! The play worker is never joined and never drained on quit: mpv's lifetime
//! is the user's, not the app's, so teardown leaves the worker parked in
//! `child.wait()` and its posts vanish with the dropped receiver (04 §11).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::domain::{self, Translation};
use crate::player::Position;
use crate::providers::ProviderRegistry;
use crate::store::Store;
use crate::tui::clock::AsyncStart;
use crate::tui::event::{EventTx, PlayFailure};
use crate::tui::workers::{self, Drain, Generation, PlaySpec};

/// Persist a resume checkpoint every 30s of observed playback (04 §7.7), so
/// a crash mid-episode costs at most this much progress.
const CHECKPOINT_PERIOD: Duration = Duration::from_secs(30);

/// Everything one session step needs from the app, borrowed per call so the
/// session never holds store or registry references across events.
pub struct PlaybackDeps<'a> {
    pub store: &'a Store,
    pub registry: &'a Arc<ProviderRegistry>,
    pub tx: &'a EventTx,
    pub mpv_path: &'a str,
    pub socket_dir: &'a Path,
    pub resume_offset_sec: u32,
    pub translation: Translation,
    pub unix_now: i64,
    pub now: Instant,
}

/// One play ask, assembled by App from the engaged episode session.
pub struct PlayRequest {
    pub anilist_id: i64,
    pub provider: String,
    pub provider_id: String,
    pub episode_label: String,
    /// 1-based: the store's episode index and the toast's N.
    pub episode_ix: u32,
    /// Playing the last playable episode: a completed watch reads
    /// `all caught up` instead of `episode N done` (DESIGN 4.10).
    pub finale: bool,
    pub title: String,
}

/// User-visible outcomes of a session step; App maps these to the DESIGN
/// 4.10 play rows. The session itself never touches `Toasts`.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayFeedback {
    /// Warn toast during the CDN backoff (the one mid-operation toast the
    /// matrix allows).
    Retry { attempt: u32 },
    /// Completed watch (natural end); partial watches record silently.
    Done { episode_ix: u32, finale: bool },
    Failed {
        provider: String,
        failure: PlayFailure,
    },
    /// The finish write failed: the watch is lost and the user should know.
    SaveFailed,
}

/// A recorded finish for the app to fan out (history reload, grid refresh).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recorded {
    pub anilist_id: i64,
    pub episode_ix: u32,
    pub completed: bool,
}

/// The §4.6 launching cell: which cell spins, and since when. Tracks the
/// session, never the cursor; the grid stays navigable during the launch.
#[derive(Debug, Clone, Copy)]
pub struct PlayGlance {
    pub anilist_id: i64,
    /// 0-based cell index.
    pub cell: usize,
    pub started: AsyncStart,
}

struct Active {
    anilist_id: i64,
    provider: String,
    episode_label: String,
    episode_ix: u32,
    finale: bool,
    /// Snapshot at fire: a mid-play `:dub` flip must not re-key the writes.
    translation: Translation,
    started: AsyncStart,
    /// First position event = mpv is up; the launching cell ends here.
    opened: bool,
    last_checkpoint: Instant,
    token: u64,
}

#[derive(Default)]
pub struct PlaybackSession {
    active: Option<Active>,
    generation: Generation,
    drain: Drain,
}

impl PlaybackSession {
    /// Fire one play. The double-play guard lives here (04 §7.7): a fire
    /// while one is active is ignored silently; the launching cell and the
    /// mpv window already show the truth.
    pub fn fire(&mut self, req: PlayRequest, deps: &PlaybackDeps) -> Vec<PlayFeedback> {
        if self.active.is_some() {
            return Vec::new();
        }
        let token = self.generation.bump();
        // Resume start rule (03 §6.3.1); read errors restart at 0 rather
        // than blocking the play.
        let start_secs = deps
            .store
            .get_resume(req.anilist_id, deps.translation, &req.episode_label)
            .ok()
            .flatten()
            .map_or(0.0, |r| r.start_secs(deps.resume_offset_sec));
        let spawned = workers::spawn_play(
            &self.drain,
            deps.tx.clone(),
            Arc::clone(deps.registry),
            PlaySpec {
                anilist_id: req.anilist_id,
                provider: req.provider.clone(),
                provider_id: req.provider_id,
                episode_label: req.episode_label.clone(),
                translation: deps.translation,
                title: req.title,
                start_secs,
                mpv_path: deps.mpv_path.to_string(),
                socket_dir: deps.socket_dir.to_path_buf(),
                token,
            },
        );
        if !spawned {
            return vec![PlayFeedback::Failed {
                provider: req.provider,
                failure: PlayFailure::Internal,
            }];
        }
        self.active = Some(Active {
            anilist_id: req.anilist_id,
            provider: req.provider,
            episode_label: req.episode_label,
            episode_ix: req.episode_ix,
            finale: req.finale,
            translation: deps.translation,
            started: AsyncStart::new(deps.now),
            opened: false,
            last_checkpoint: deps.now,
            token,
        });
        Vec::new()
    }

    /// Live position: flips the launching cell off and lands the periodic
    /// checkpoint. Only meaningful positions write (a 0/NaN checkpoint would
    /// clobber a real resume); the write itself is best-effort, the finish
    /// write is the one that counts.
    pub fn on_position(
        &mut self,
        anilist_id: i64,
        position: Position,
        token: u64,
        deps: &PlaybackDeps,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if active.token != token || active.anilist_id != anilist_id {
            return;
        }
        active.opened = true;
        if position.secs.is_finite()
            && position.secs > 0.0
            && deps.now.saturating_duration_since(active.last_checkpoint) >= CHECKPOINT_PERIOD
        {
            let _ = deps.store.save_progress(
                active.anilist_id,
                active.translation,
                &active.episode_label,
                position.secs,
                position.duration.unwrap_or(0.0),
                Some(&active.provider),
                deps.unix_now,
            );
            active.last_checkpoint = deps.now;
        }
    }

    /// A relaunch re-resolves and respawns mpv, so the cell goes back to
    /// launching for the backoff + retry window.
    pub fn on_retry(&mut self, anilist_id: i64, attempt: u32, token: u64) -> Vec<PlayFeedback> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        if active.token != token || active.anilist_id != anilist_id {
            return Vec::new();
        }
        active.opened = false;
        vec![PlayFeedback::Retry { attempt }]
    }

    /// Terminal outcome: the 02 §4b gate (`finish_playback`) decides whether
    /// anything is written; the completed/partial split (natural end 0.80)
    /// decides whether anything is said.
    pub fn on_finished(
        &mut self,
        anilist_id: i64,
        position: Option<Position>,
        failure: Option<PlayFailure>,
        token: u64,
        deps: &PlaybackDeps,
    ) -> (Vec<PlayFeedback>, Option<Recorded>) {
        let stale = self
            .active
            .as_ref()
            .is_none_or(|a| a.token != token || a.anilist_id != anilist_id);
        if stale {
            return (Vec::new(), None);
        }
        let active = self.active.take().expect("checked above");
        let mut fb = Vec::new();
        let mut recorded = None;
        match workers::finish_playback(
            deps.store,
            active.anilist_id,
            active.translation,
            &active.episode_label,
            active.episode_ix,
            position,
            Some(&active.provider),
            deps.unix_now,
        ) {
            Ok(true) => {
                let completed = position
                    .is_some_and(|p| domain::natural_end(p.secs, p.duration.unwrap_or(0.0)));
                recorded = Some(Recorded {
                    anilist_id: active.anilist_id,
                    episode_ix: active.episode_ix,
                    completed,
                });
                if completed {
                    fb.push(PlayFeedback::Done {
                        episode_ix: active.episode_ix,
                        finale: active.finale,
                    });
                }
            }
            Ok(false) => {}
            Err(_) => fb.push(PlayFeedback::SaveFailed),
        }
        if let Some(failure) = failure {
            fb.push(PlayFeedback::Failed {
                provider: active.provider,
                failure,
            });
        }
        (fb, recorded)
    }

    /// Some while resolving/launching (no position seen yet); None once mpv
    /// is up or nothing plays.
    pub fn glance(&self) -> Option<PlayGlance> {
        let active = self.active.as_ref()?;
        (!active.opened).then(|| PlayGlance {
            anilist_id: active.anilist_id,
            cell: active.episode_ix.saturating_sub(1) as usize,
            started: active.started,
        })
    }

    pub fn is_playing(&self) -> bool {
        self.active.is_some()
    }

    /// Tests only: quit deliberately never drains this family (module doc).
    #[cfg(test)]
    pub(crate) fn drain(&self, timeout: Duration) -> bool {
        self.drain.drain(timeout)
    }

    #[cfg(test)]
    pub(crate) fn active_token(&self) -> Option<u64> {
        self.active.as_ref().map(|a| a.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Enrichment;
    use crate::tui::episodes::teststub;
    use crate::tui::event::{self, Event, FetchClass};

    /// Split from the session so `deps()` borrows stay disjoint from
    /// `&mut session` at the call sites.
    struct World {
        store: Store,
        registry: Arc<ProviderRegistry>,
        tx: EventTx,
        rx: event::EventRx,
        now: Instant,
    }

    struct Rig {
        world: World,
        session: PlaybackSession,
    }

    impl World {
        fn deps(&self) -> PlaybackDeps<'_> {
            self.deps_at(self.now)
        }

        fn deps_at(&self, now: Instant) -> PlaybackDeps<'_> {
            PlaybackDeps {
                store: &self.store,
                registry: &self.registry,
                tx: &self.tx,
                mpv_path: "/definitely/not/mpv",
                socket_dir: Path::new("/tmp"),
                resume_offset_sec: 5,
                translation: Translation::Sub,
                unix_now: 1000,
                now,
            }
        }

        fn seed_bound(&self, aid: i64) {
            let e = Enrichment {
                anilist_id: aid,
                title_romaji: format!("Show {aid}"),
                ..Default::default()
            };
            self.store
                .bind_provider(&e, "megaplay", "m-1", 100)
                .unwrap();
        }
    }

    impl Rig {
        fn new() -> Rig {
            let (tx, rx) = event::channel();
            Rig {
                world: World {
                    store: Store::open_memory().unwrap(),
                    registry: teststub::inert_registry(),
                    tx,
                    rx,
                    now: Instant::now(),
                },
                session: PlaybackSession::default(),
            }
        }

        /// Fire and settle the worker (the stub registry resolve answers
        /// Unsupported, so mpv is never spawned); drop the worker's own
        /// finish event, tests inject their own.
        fn fire_settled(&mut self, aid: i64, ix: u32) -> u64 {
            let fb = self
                .session
                .fire(request(aid, ix, false), &self.world.deps());
            assert!(fb.is_empty());
            assert!(self.session.drain.drain(Duration::from_secs(5)));
            while self.world.rx.try_recv().is_ok() {}
            self.session.active.as_ref().unwrap().token
        }
    }

    fn request(aid: i64, ix: u32, finale: bool) -> PlayRequest {
        PlayRequest {
            anilist_id: aid,
            provider: "megaplay".into(),
            provider_id: "m-1".into(),
            episode_label: ix.to_string(),
            episode_ix: ix,
            finale,
            title: "t".into(),
        }
    }

    fn pos(secs: f64, duration: Option<f64>) -> Position {
        Position { secs, duration }
    }

    #[test]
    fn fire_spawns_and_worker_posts_the_resolve_failure() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let fb = rig.session.fire(request(7, 3, false), &rig.world.deps());
        assert!(fb.is_empty());
        assert!(rig.session.is_playing());
        assert!(rig.session.drain.drain(Duration::from_secs(5)));
        let finished = rig.world.rx.try_recv().unwrap();
        let token = rig.session.active.as_ref().unwrap().token;
        assert_eq!(
            finished,
            Event::PlayFinished {
                anilist_id: 7,
                position: None,
                failure: Some(PlayFailure::Resolve(FetchClass::Unsupported)),
                token,
            }
        );
    }

    #[test]
    fn double_play_guard_ignores_a_second_fire() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let token = rig.fire_settled(7, 3);
        let fb = rig.session.fire(request(7, 4, false), &rig.world.deps());
        assert!(fb.is_empty(), "second fire is a silent no-op");
        assert_eq!(rig.session.active.as_ref().unwrap().token, token);
        assert_eq!(rig.session.active.as_ref().unwrap().episode_ix, 3);
    }

    #[test]
    fn glance_tracks_the_launching_window() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let token = rig.fire_settled(7, 3);
        let glance = rig.session.glance().unwrap();
        assert_eq!((glance.anilist_id, glance.cell), (7, 2));
        rig.session
            .on_position(7, pos(0.0, None), token, &rig.world.deps());
        assert!(rig.session.glance().is_none(), "first position ends launch");
        // A retry re-resolves: the cell goes back to launching.
        let fb = rig.session.on_retry(7, 2, token);
        assert_eq!(fb, vec![PlayFeedback::Retry { attempt: 2 }]);
        assert!(rig.session.glance().is_some());
    }

    #[test]
    fn stale_or_cross_show_events_are_dropped() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let token = rig.fire_settled(7, 3);
        rig.session
            .on_position(7, pos(5.0, None), token + 1, &rig.world.deps());
        rig.session
            .on_position(9, pos(5.0, None), token, &rig.world.deps());
        assert!(rig.session.glance().is_some(), "neither event applied");
        assert!(rig.session.on_retry(7, 2, token + 1).is_empty());
        let (fb, recorded) =
            rig.session
                .on_finished(7, Some(pos(9.0, None)), None, token + 1, &rig.world.deps());
        assert!(fb.is_empty() && recorded.is_none());
        assert!(rig.session.is_playing(), "stale finish must not clear");
    }

    #[test]
    fn checkpoint_lands_on_cadence_and_only_meaningfully() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let token = rig.fire_settled(7, 3);
        // Inside the period: no write.
        rig.session
            .on_position(7, pos(10.0, Some(1400.0)), token, &rig.world.deps());
        assert!(
            rig.world
                .store
                .get_resume(7, Translation::Sub, "3")
                .unwrap()
                .is_none()
        );
        let later = rig.world.now + CHECKPOINT_PERIOD;
        // Past the period but meaningless: no write, cadence holds.
        rig.session
            .on_position(7, pos(0.0, Some(1400.0)), token, &rig.world.deps_at(later));
        assert!(
            rig.world
                .store
                .get_resume(7, Translation::Sub, "3")
                .unwrap()
                .is_none()
        );
        rig.session.on_position(
            7,
            pos(300.0, Some(1400.0)),
            token,
            &rig.world.deps_at(later),
        );
        let resume = rig
            .world
            .store
            .get_resume(7, Translation::Sub, "3")
            .unwrap()
            .unwrap();
        assert_eq!(resume.position_secs, 300.0);
        // Cadence reset: the next write waits a full period again.
        rig.session.on_position(
            7,
            pos(310.0, Some(1400.0)),
            token,
            &rig.world.deps_at(later),
        );
        assert_eq!(
            rig.world
                .store
                .get_resume(7, Translation::Sub, "3")
                .unwrap()
                .unwrap()
                .position_secs,
            300.0
        );
    }

    #[test]
    fn finished_partial_records_silently() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let token = rig.fire_settled(7, 3);
        let (fb, recorded) = rig.session.on_finished(
            7,
            Some(pos(300.0, Some(1400.0))),
            None,
            token,
            &rig.world.deps(),
        );
        assert!(fb.is_empty(), "partial watch earns no toast");
        assert_eq!(
            recorded,
            Some(Recorded {
                anilist_id: 7,
                episode_ix: 3,
                completed: false,
            })
        );
        assert!(!rig.session.is_playing());
        assert_eq!(rig.world.store.list_history().unwrap()[0].play_count, 1);
    }

    #[test]
    fn finished_completed_toasts_done_or_finale() {
        for (finale, expected) in [(false, false), (true, true)] {
            let mut rig = Rig::new();
            rig.world.seed_bound(7);
            let fb = rig.session.fire(request(7, 12, finale), &rig.world.deps());
            assert!(fb.is_empty());
            assert!(rig.session.drain.drain(Duration::from_secs(5)));
            let token = rig.session.active.as_ref().unwrap().token;
            let (fb, recorded) = rig.session.on_finished(
                7,
                Some(pos(1200.0, Some(1400.0))),
                None,
                token,
                &rig.world.deps(),
            );
            assert_eq!(
                fb,
                vec![PlayFeedback::Done {
                    episode_ix: 12,
                    finale: expected,
                }]
            );
            assert!(recorded.unwrap().completed);
        }
    }

    #[test]
    fn finished_failure_reports_the_class_with_no_writes() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let token = rig.fire_settled(7, 3);
        let (fb, recorded) = rig.session.on_finished(
            7,
            None,
            Some(PlayFailure::MpvNotFound),
            token,
            &rig.world.deps(),
        );
        assert_eq!(
            fb,
            vec![PlayFeedback::Failed {
                provider: "megaplay".into(),
                failure: PlayFailure::MpvNotFound,
            }]
        );
        assert!(recorded.is_none());
        assert!(rig.world.store.list_history().unwrap().is_empty());
        assert!(
            !rig.session.is_playing(),
            "a failed play clears the session"
        );
    }

    #[test]
    fn finish_clears_and_the_next_fire_is_live() {
        let mut rig = Rig::new();
        rig.world.seed_bound(7);
        let token = rig.fire_settled(7, 3);
        rig.session
            .on_finished(7, None, None, token, &rig.world.deps());
        assert!(!rig.session.is_playing());
        let fb = rig.session.fire(request(7, 4, false), &rig.world.deps());
        assert!(fb.is_empty());
        assert!(rig.session.is_playing());
        assert!(rig.session.active.as_ref().unwrap().token > token);
    }
}
