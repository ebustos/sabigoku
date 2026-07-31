//! Download session: the transport around `downloader::download`, one
//! ffmpeg remux at a time. Mirrors `tui::playback` (the mpv session), minus
//! the retry/continuation machinery a download has no use for — a failed
//! download is re-fired with `d`, never auto-hopped across providers.
//!
//! Owns its own `Drain`/`Generation` per the same ROD-439 rule playback
//! follows: this subsystem's worker accounting never lives on `App`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::domain::{Quality, Translation};
use crate::providers::ProviderRegistry;
use crate::tui::clock::AsyncStart;
use crate::tui::event::{DownloadFailure, EventTx};
use crate::tui::workers::{self, DownloadSpec, Drain, Generation};

/// Everything one download step needs, borrowed per call (same shape as
/// `PlaybackDeps`). `output_dir` is owned, not borrowed: unlike
/// `PlaybackDeps::socket_dir` (a stable field on `App`), the download dir is
/// resolved fresh from config every call (empty config = `$HOME/Videos/sabigoku`
/// at call-site), so there is nothing on `App` to borrow it from.
pub struct DownloadDeps<'a> {
    pub registry: &'a Arc<ProviderRegistry>,
    pub tx: &'a EventTx,
    pub ffmpeg_path: &'a str,
    pub output_dir: PathBuf,
    pub translation: Translation,
    pub quality: Quality,
    pub now: Instant,
}

/// One download ask, assembled by App from the engaged episode session.
pub struct DownloadRequest {
    pub anilist_id: i64,
    pub provider: String,
    pub provider_id: String,
    pub episode_label: String,
    /// 1-based: the store's episode index and the toast's N.
    pub episode_ix: u32,
    /// Filesystem-safe (`domain::sanitize_filename` already applied).
    pub base_name: String,
}

/// User-visible outcomes; App maps these to toasts.
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadFeedback {
    Started {
        episode_ix: u32,
    },
    /// A fire while one is already active: unlike Play (the mpv window is
    /// its own "still running" cue), a download has no visible surface, so
    /// this must be said out loud rather than silently ignored.
    Busy,
    Done {
        episode_ix: u32,
        path: PathBuf,
    },
    Failed {
        episode_ix: u32,
        provider: String,
        failure: DownloadFailure,
    },
}

/// The launching cell for a download, mirrors `PlayGlance` — except it
/// never clears until the job actually finishes: a download has no
/// second-phase surface (like mpv's own window) to hand the "still going"
/// signal off to.
#[derive(Debug, Clone, Copy)]
pub struct DownloadGlance {
    pub anilist_id: i64,
    /// 0-based cell index.
    pub cell: usize,
    pub started: AsyncStart,
}

struct Active {
    anilist_id: i64,
    provider: String,
    episode_ix: u32,
    started: AsyncStart,
    token: u64,
}

#[derive(Default)]
pub struct DownloadSession {
    active: Option<Active>,
    generation: Generation,
    drain: Drain,
}

impl DownloadSession {
    /// Fire one download. The single-flight guard lives here: a fire while
    /// one is active earns a `Busy` toast rather than a silent no-op.
    pub fn fire(&mut self, req: DownloadRequest, deps: &DownloadDeps) -> Vec<DownloadFeedback> {
        if self.active.is_some() {
            return vec![DownloadFeedback::Busy];
        }
        let token = self.generation.bump();
        let episode_ix = req.episode_ix;
        let spawned = workers::spawn_download(
            &self.drain,
            deps.tx.clone(),
            Arc::clone(deps.registry),
            DownloadSpec {
                anilist_id: req.anilist_id,
                provider: req.provider.clone(),
                provider_id: req.provider_id,
                episode_label: req.episode_label,
                episode_ix,
                translation: deps.translation,
                quality: deps.quality,
                base_name: req.base_name,
                ffmpeg_path: deps.ffmpeg_path.to_string(),
                output_dir: deps.output_dir.clone(),
                token,
            },
        );
        if !spawned {
            return vec![DownloadFeedback::Failed {
                episode_ix,
                provider: req.provider,
                failure: DownloadFailure::Internal,
            }];
        }
        self.active = Some(Active {
            anilist_id: req.anilist_id,
            provider: req.provider,
            episode_ix,
            started: AsyncStart::new(deps.now),
            token,
        });
        vec![DownloadFeedback::Started { episode_ix }]
    }

    /// Terminal outcome. A stale token (superseded by a newer fire, though
    /// the single-flight guard means that can only happen after this one
    /// already finished) is dropped silently.
    pub fn on_finished(
        &mut self,
        anilist_id: i64,
        episode_ix: u32,
        path: Option<PathBuf>,
        failure: Option<DownloadFailure>,
        token: u64,
    ) -> Vec<DownloadFeedback> {
        let stale = self
            .active
            .as_ref()
            .is_none_or(|a| a.token != token || a.anilist_id != anilist_id);
        if stale {
            return Vec::new();
        }
        let active = self.active.take().expect("checked above");
        match (path, failure) {
            (Some(path), _) => vec![DownloadFeedback::Done { episode_ix, path }],
            (None, Some(failure)) => vec![DownloadFeedback::Failed {
                episode_ix,
                provider: active.provider,
                failure,
            }],
            (None, None) => Vec::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Some for the whole run, unlike `PlaybackSession::glance` (which
    /// clears once mpv's window opens): ffmpeg never hands off to a
    /// second visible surface, so the grid spinner is the only signal
    /// there is until the job finishes.
    pub fn glance(&self) -> Option<DownloadGlance> {
        let active = self.active.as_ref()?;
        Some(DownloadGlance {
            anilist_id: active.anilist_id,
            cell: active.episode_ix.saturating_sub(1) as usize,
            started: active.started,
        })
    }

    /// Tests only: quit deliberately never drains this family (module doc).
    #[cfg(test)]
    pub(crate) fn drain(&self, timeout: std::time::Duration) -> bool {
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
    use crate::tui::episodes::teststub;
    use crate::tui::event::{self, Event, FetchClass};
    use std::time::Duration;

    struct World {
        registry: Arc<ProviderRegistry>,
        tx: EventTx,
        rx: event::EventRx,
    }

    struct Rig {
        world: World,
        session: DownloadSession,
    }

    impl World {
        fn deps(&self) -> DownloadDeps<'_> {
            DownloadDeps {
                registry: &self.registry,
                tx: &self.tx,
                ffmpeg_path: "/definitely/not/ffmpeg",
                output_dir: PathBuf::from("/tmp"),
                translation: Translation::Sub,
                quality: Quality::Best,
                now: Instant::now(),
            }
        }
    }

    impl Rig {
        fn new() -> Rig {
            let (tx, rx) = event::channel();
            Rig {
                world: World {
                    registry: teststub::inert_registry(),
                    tx,
                    rx,
                },
                session: DownloadSession::default(),
            }
        }

        fn settle(&mut self) -> u64 {
            assert!(self.session.drain.drain(Duration::from_secs(5)));
            self.session.active.as_ref().unwrap().token
        }
    }

    fn request(aid: i64, ix: u32) -> DownloadRequest {
        DownloadRequest {
            anilist_id: aid,
            provider: "megaplay".into(),
            provider_id: "m-1".into(),
            episode_label: ix.to_string(),
            episode_ix: ix,
            base_name: format!("Show {aid} - {ix}"),
        }
    }

    #[test]
    fn fire_spawns_and_worker_posts_the_resolve_failure() {
        let mut rig = Rig::new();
        let fb = rig.session.fire(request(7, 3), &rig.world.deps());
        assert_eq!(fb, vec![DownloadFeedback::Started { episode_ix: 3 }]);
        assert!(rig.session.is_active());
        assert!(rig.session.drain.drain(Duration::from_secs(5)));
        let finished = rig.world.rx.try_recv().unwrap();
        let token = rig.session.active.as_ref().unwrap().token;
        assert_eq!(
            finished,
            Event::DownloadFinished {
                anilist_id: 7,
                episode_ix: 3,
                path: None,
                failure: Some(DownloadFailure::Resolve(FetchClass::Unsupported)),
                token,
            }
        );
    }

    #[test]
    fn double_fire_returns_busy_and_never_supersedes() {
        let mut rig = Rig::new();
        let fb = rig.session.fire(request(7, 3), &rig.world.deps());
        assert_eq!(fb, vec![DownloadFeedback::Started { episode_ix: 3 }]);
        let first_token = rig.settle();
        while rig.world.rx.try_recv().is_ok() {}

        let fb = rig.session.fire(request(7, 4), &rig.world.deps());
        assert_eq!(fb, vec![DownloadFeedback::Busy]);
        assert_eq!(rig.session.active.as_ref().unwrap().token, first_token);
    }

    #[test]
    fn stale_finish_is_dropped_and_session_stays_active() {
        let mut rig = Rig::new();
        rig.session.fire(request(7, 3), &rig.world.deps());
        let token = rig.settle();
        let fb = rig.session.on_finished(7, 3, None, None, token + 1);
        assert!(fb.is_empty());
        assert!(rig.session.is_active(), "stale finish must not clear");
    }

    #[test]
    fn finish_clears_and_the_next_fire_is_live() {
        let mut rig = Rig::new();
        rig.session.fire(request(7, 3), &rig.world.deps());
        let token = rig.settle();
        let fb = rig
            .session
            .on_finished(7, 3, None, Some(DownloadFailure::FfmpegNotFound), token);
        assert_eq!(
            fb,
            vec![DownloadFeedback::Failed {
                episode_ix: 3,
                provider: "megaplay".into(),
                failure: DownloadFailure::FfmpegNotFound,
            }]
        );
        assert!(!rig.session.is_active());

        let fb = rig.session.fire(request(7, 4), &rig.world.deps());
        assert_eq!(fb, vec![DownloadFeedback::Started { episode_ix: 4 }]);
        assert!(rig.session.active_token().unwrap() > token);
    }

    #[test]
    fn glance_tracks_the_whole_download() {
        let mut rig = Rig::new();
        rig.session.fire(request(7, 3), &rig.world.deps());
        let glance = rig.session.glance().unwrap();
        assert_eq!((glance.anilist_id, glance.cell), (7, 2));
        let token = rig.settle();
        // Unlike a play launch, resolving/spawning being done does not
        // clear it — only `on_finished` does.
        assert!(rig.session.glance().is_some());
        rig.session
            .on_finished(7, 3, None, Some(DownloadFailure::FfmpegNotFound), token);
        assert!(rig.session.glance().is_none());
    }

    #[test]
    fn finish_with_a_path_reports_done() {
        let mut rig = Rig::new();
        rig.session.fire(request(7, 3), &rig.world.deps());
        let token = rig.settle();
        let path = PathBuf::from("/tmp/Show 7 - 3.mkv");
        let fb = rig
            .session
            .on_finished(7, 3, Some(path.clone()), None, token);
        assert_eq!(
            fb,
            vec![DownloadFeedback::Done {
                episode_ix: 3,
                path
            }]
        );
    }
}
