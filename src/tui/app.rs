//! App state and event dispatch (DESIGN 9.6). Contracts that keep this file
//! from becoming zigoku's app.zig:
//!
//! - `tick`/`on_key` are DISPATCH ONLY (ratified ROD-438): every non-trivial
//!   arm goes through a named handler; an arm may stay inline only as a
//!   single assignment.
//! - Subsystem transport (drains, generations, deadlines) lives WITH the
//!   subsystem that owns it, never accumulating here. Deliberate deviation
//!   from 04 §7 "transport may live on App"; see ROD-439.
//! - View handlers take their own state plus scoped borrows, never `&mut App`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui_image::picker::Picker;

use crate::aniskip::SkipMode;
use crate::config::Config;
use crate::domain::{self, TitleLanguage, Translation};
use crate::paths::Paths;
use crate::player::{self, Position};
use crate::providers::{CatalogProvider, DiscoverAxis, ProviderRegistry};
use crate::store::Store;

use super::chrome::{self, BottomBar, HelpLine, Tab, TopBar};
use super::clock::Debounce;
use super::covers::CoverCaches;
use super::covers::render::ProtocolPool;
use super::episodes::{EpisodeDeps, Feedback};
use super::event::{Event, EventTx, FetchClass, PlayFailure};
use super::layout;
use super::playback::{HopAsk, PlayFeedback, PlayRequest, PlaybackDeps, PlaybackSession};
use super::render;
use super::theme::{self, Palette};
use super::toast::{Kind, Toasts};
use super::view::browse::{self, BrowseState};
use super::view::connect::{self, ConnectView};
use super::view::detail::{self, DetailState};
use super::view::discover::{self, DiscoverState};
use super::view::history::{self, HistoryState};
use super::view::settings::{self, SettingsState};
use super::view::{InputMode, Origin, Pane, View, ViewEnv};
use super::workers::{self, Drain};
use crate::auth::Auth;
use crate::login::ConnectResult;
use crate::loopback::{Canceler, Loopback};
use crate::sync;

/// Unknown-command bottom-bar flash (DESIGN 3.5).
const COMMAND_FLASH: Duration = Duration::from_millis(800);
/// Action-flush debounce window (clock.rs "sync flush 3000ms", ROD-291).
const SYNC_FLUSH_PERIOD: Duration = Duration::from_millis(3000);

pub struct App {
    pub(super) quit: bool,
    pub(super) dirty: bool,
    term: (u16, u16),
    view: View,
    pane: Pane,
    origin: Origin,
    mode: InputMode,
    command: String,
    command_flash: Option<Instant>,
    /// Armed hard-delete, keyed by show identity, never a row index
    /// (DESIGN 6.5: a reload cancels it rather than trusting a stale row).
    confirm_delete: Option<i64>,
    /// Single-level status undo: `(show, status, progress)` captured before
    /// the mutation (05 §4). Keys off the captured id, not the cursor.
    undo: Option<(i64, domain::ListStatus, u32)>,
    /// `last_watched` landing: open this show's detail once the first real
    /// size arrives (05 §10.6); armed only on the initial load.
    resume_pending: bool,
    /// The auto-opened show whose walk exhaust demotes back to the list
    /// (ROD-229); user-driven opens never arm this.
    resume_demote: Option<i64>,
    toasts: Toasts,
    palette: &'static Palette,
    config: Config,
    pub(super) browse: BrowseState,
    history: HistoryState,
    pub(super) discover: DiscoverState,
    settings: SettingsState,
    pub(super) detail: DetailState,
    playback: PlaybackSession,
    store: Store,
    catalog: Arc<dyn CatalogProvider>,
    registry: Arc<ProviderRegistry>,
    caches: Arc<CoverCaches>,
    covers_dir: PathBuf,
    play_dirs: PlayDirs,
    config_file: PathBuf,
    db_file: PathBuf,
    /// Loaded at startup, reloaded after a connect completes (05 reloadAuth).
    auth: Auth,
    auth_file: PathBuf,
    /// Open connect modal; captures every key while present (DESIGN 5.5a).
    connect: Option<ConnectSession>,
    /// Action-flush debounce (ROD-291): armed on a status/play edit, fires the
    /// pull-then-push sync.
    sync_debounce: Debounce,
    /// A sync worker is inflight; gates overlap and the quit flush (04 §11).
    syncing: bool,
    /// Connect and sync workers share one drain, joined at teardown.
    pub(super) sync_drain: Drain,
    pub(super) pool: ProtocolPool,
    pub(super) encode_drain: Drain,
}

/// Live state of an open connect modal. `canceler` wakes the blocked loopback
/// worker on esc/teardown; `started` clocks the spinner and the 20s paste hint.
struct ConnectSession {
    url: String,
    canceler: Canceler,
    started: Instant,
    copied: bool,
}

/// Persistent-toast topic for the catalog brain (DESIGN 8.5).
const ANILIST_TOPIC: &str = "anilist";

impl App {
    /// State only; workers start in `run` (bootstrap order is its job).
    pub fn new(
        config: &Config,
        store: Store,
        catalog: Arc<dyn CatalogProvider>,
        registry: Arc<ProviderRegistry>,
        paths: &Paths,
        picker: Picker,
        tx: &EventTx,
    ) -> App {
        let encode_drain = Drain::default();
        let pool = ProtocolPool::new(picker, tx.clone(), &encode_drain);
        let mut app = App {
            quit: false,
            dirty: true,
            term: (0, 0),
            view: landing_view(&config.landing),
            pane: Pane::List,
            origin: Origin::Browse,
            mode: InputMode::Normal,
            command: String::new(),
            command_flash: None,
            confirm_delete: None,
            undo: None,
            resume_pending: false,
            resume_demote: None,
            toasts: Toasts::default(),
            palette: theme::by_name(&config.palette),
            config: config.clone(),
            browse: BrowseState::default(),
            history: HistoryState::default(),
            discover: DiscoverState::default(),
            settings: SettingsState::default(),
            detail: DetailState::default(),
            playback: PlaybackSession::default(),
            store,
            catalog,
            registry,
            caches: Arc::new(CoverCaches::new()),
            covers_dir: paths.covers_dir(),
            play_dirs: PlayDirs {
                socket: paths.runtime.clone(),
                cache: paths.cache.clone(),
            },
            config_file: paths.config_file(),
            db_file: paths.db_file(),
            auth: Auth::load(&paths.auth_file()),
            auth_file: paths.auth_file(),
            connect: None,
            sync_debounce: Debounce::default(),
            syncing: false,
            sync_drain: Drain::default(),
            pool,
            encode_drain,
        };
        // A synchronous local read, not a worker (a deliberate deviation from
        // 04 §4.2's load events; recorded on the ticket).
        app.history.load(&app.store);
        // Resolved once, on the initial load only (DESIGN 8.3); never-played
        // history simply lands on History. The freeze's second arming call
        // site (the sync fallback, 04 §3) joins with AniList sync (ROD-448).
        app.resume_pending =
            config.landing == "last_watched" && app.history.first_played().is_some();
        app
    }

    /// Mutates; draw is pure (04 §1). Dispatch only.
    pub(super) fn tick(&mut self, event: Event, now: Instant, tx: &EventTx) {
        match event {
            Event::Key(key) => self.on_key(key, now, tx),
            Event::Resize(w, h) => self.on_resize(w, h, now, tx),
            Event::FocusGained | Event::FocusLost => {}
            // No keys can ever arrive again; quit clean instead of zombieing.
            Event::InputClosed => self.quit = true,
            Event::Tick => self.on_tick(now, tx),
            Event::CoverDone { for_id, img } => self.on_cover_done(for_id, img),
            Event::CoverError { for_id } => self.on_cover_error(for_id, now),
            Event::DiscoverCoverDone { url, img } => self.on_discover_cover_done(&url, img),
            Event::DiscoverCoverError { url } => self.on_discover_cover_error(&url, now),
            Event::CoverEncodeReady => self.on_encode_ready(),
            Event::DiscoverFeed {
                axis,
                page,
                entries,
                has_next,
            } => self.on_discover_feed(axis, page, entries, has_next),
            Event::DiscoverFeedError { axis, cause } => self.on_discover_feed_error(axis, cause),
            Event::SearchDone {
                query,
                page: _,
                results,
            } => self.on_search_done(&query, results, now),
            Event::SearchFailed { query, cause: _ } => self.on_search_failed(&query, now),
            e @ (Event::EpisodesDone { .. }
            | Event::EpisodesError { .. }
            | Event::ProviderSearchDone { .. }
            | Event::ProviderSearchError { .. }) => self.on_episode_event(e, now, tx),
            Event::PlayPosition {
                anilist_id,
                position,
                token,
            } => self.on_play_position(anilist_id, position, token, now, tx),
            Event::PlayRetry {
                anilist_id,
                attempt,
                token,
            } => self.on_play_retry(anilist_id, attempt, token, now),
            Event::PlayFinished {
                anilist_id,
                position,
                failure,
                token,
            } => self.on_play_finished(anilist_id, position, failure, token, now, tx),
            Event::ConnectResult(result) => self.on_connect_result(result, now, tx),
            Event::SyncFlushed(summary) => self.on_sync_flushed(summary, now),
            // Wired later in ROD-448: update toast (chunk 9).
            Event::UpdateAvailable { .. } => {}
        }
    }

    /// Key dispatch. Ctrl-C hard-quits from every mode; F-keys are global
    /// aliases that fire in any mode (DESIGN 7.2).
    fn on_key(&mut self, key: KeyEvent, now: Instant, tx: &EventTx) {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            self.quit = true;
            return;
        }
        // The connect modal captures every key but Ctrl-C (DESIGN 5.5a).
        if self.connect.is_some() {
            self.on_connect_key(key, now);
            self.dirty = true;
            return;
        }
        // An armed delete freezes everything below it, F-keys and `q`
        // included; only Ctrl-C stays an emergency exit (DESIGN 6.5).
        if self.confirm_delete.is_some() {
            self.on_confirm_key(key, now);
            self.dirty = true;
            return;
        }
        // Settings owns its keys in normal mode; a field under edit swallows
        // everything (F-keys and view letters are text there, DESIGN 5.5).
        if self.view == View::Settings
            && self.mode == InputMode::Normal
            && self.on_settings_key(key, now, tx)
        {
            self.dirty = true;
            return;
        }
        if let KeyCode::F(n @ 1..=4) = key.code {
            self.on_fkey(n, now);
            return;
        }
        match self.mode {
            InputMode::Normal => self.on_normal_key(key, now, tx),
            InputMode::Search => self.on_search_key(key, now),
            InputMode::Command => self.on_command_key(key, now, tx),
        }
        self.dirty = true;
    }

    fn on_normal_key(&mut self, key: KeyEvent, now: Instant, tx: &EventTx) {
        match key.code {
            KeyCode::Char('q') => self.on_quit(now),
            KeyCode::Char('B') => self.switch_view(View::Browse, now),
            KeyCode::Char('H') => self.switch_view(View::History, now),
            KeyCode::Char('D') => self.switch_view(View::Discover, now),
            KeyCode::Char('S') => self.switch_view(View::Settings, now),
            KeyCode::Char('/') => self.open_search(now),
            KeyCode::Char(':') => self.open_command(),
            KeyCode::Esc => self.on_escape(),
            KeyCode::Char(' ') => self.on_space(now, tx),
            KeyCode::Enter => self.on_enter(now, tx),
            KeyCode::Char('h') | KeyCode::Left => self.on_h(),
            KeyCode::Char('l') | KeyCode::Right => self.on_l(now, tx),
            KeyCode::Char('j') | KeyCode::Down => self.on_j(now),
            KeyCode::Char('k') | KeyCode::Up => self.on_k(now),
            KeyCode::Char('g') => self.on_jump(true, now),
            KeyCode::Char('G') => self.on_jump(false, now),
            KeyCode::Char('v') => self.on_pin_cycle(now, tx),
            KeyCode::Char('P') => self.on_plan(now),
            KeyCode::Char('p') => self.on_status_key(domain::ListStatus::Paused, now),
            KeyCode::Char('x') => self.on_status_key(domain::ListStatus::Dropped, now),
            KeyCode::Char('c') => self.on_status_key(domain::ListStatus::Completed, now),
            KeyCode::Char('w') => self.on_status_key(domain::ListStatus::Watching, now),
            KeyCode::Char('u') => self.on_undo(now),
            KeyCode::Char('r') => self.on_recompute(now),
            KeyCode::Char('X') => self.on_arm_delete(),
            KeyCode::Char(']') => self.on_axis_cycle(1),
            KeyCode::Char('[') => self.on_axis_cycle(-1),
            KeyCode::Char(c @ '1'..='4') => self.on_axis_select(c),
            _ => {}
        }
    }

    /// F1-F4 close any prompt: they are navigation, and unlike the letters
    /// they cannot be typed into a search (DESIGN 7.2).
    fn on_fkey(&mut self, n: u8, now: Instant) {
        self.mode = InputMode::Normal;
        let target = match n {
            1 => View::Browse,
            2 => View::History,
            3 => View::Discover,
            _ => View::Settings,
        };
        self.switch_view(target, now);
        self.dirty = true;
    }

    /// Direct go-to, never a toggle; same-view is a no-op (DESIGN 7.2).
    /// Entering Browse re-pushes its selection so the shared detail surface
    /// never shows another view's show.
    fn switch_view(&mut self, target: View, now: Instant) {
        if self.view == target {
            return;
        }
        if self.view == View::Settings {
            self.persist_settings(now);
        }
        self.view = target;
        self.pane = Pane::List;
        // Entering a list view re-pushes ITS selection so the shared detail
        // never shows another view's show. History also re-reads the store
        // on entry: saves land from any view (P in Browse/Discover) and the
        // in-memory list must not go stale against them.
        match target {
            View::Browse => self.push_browse_selection(true, now),
            View::History => {
                self.history.load(&self.store);
                self.push_history_selection(true, now);
            }
            _ => {}
        }
    }

    fn open_search(&mut self, now: Instant) {
        match self.view {
            View::Browse | View::History => self.mode = InputMode::Search,
            // `/` jumps to Browse; Discover has no in-view filter (DESIGN 7.5).
            View::Discover => self.jump_to_browse_search(now),
            View::Detail | View::Settings => {}
        }
    }

    fn jump_to_browse_search(&mut self, now: Instant) {
        self.switch_view(View::Browse, now);
        self.mode = InputMode::Search;
    }

    fn open_command(&mut self) {
        self.mode = InputMode::Command;
        self.command.clear();
    }

    /// Esc peels one transient layer and never switches base view
    /// (DESIGN 7.4; the exhaustive table lives there and in the tests).
    fn on_escape(&mut self) {
        match self.view {
            View::Browse | View::History if self.pane == Pane::Detail => self.pane = Pane::List,
            View::Detail => self.demote(),
            _ => {}
        }
    }

    /// Space is the symmetric zoom toggle (DESIGN 7.2). At `w < 60` only the
    /// History list opens the zoom directly (DESIGN 7.1).
    fn on_space(&mut self, now: Instant, tx: &EventTx) {
        match self.view {
            View::Detail => self.demote(),
            View::Browse | View::History if self.pane == Pane::Detail => self.promote(),
            View::History if !self.two_pane() => self.open_history_zoom(now, tx),
            _ => {}
        }
    }

    /// Enter drills toward wherever the grid is visible, then plays
    /// (DESIGN 10, ROD-170/259).
    fn on_enter(&mut self, now: Instant, tx: &EventTx) {
        match self.view {
            View::Discover => self.open_discover_detail(now, tx),
            View::Browse | View::History if self.pane == Pane::Detail => self.on_play(now, tx),
            View::Detail => self.on_play(now, tx),
            View::History if !self.two_pane() => self.open_history_zoom(now, tx),
            View::Browse if self.two_pane() => self.enter_browse_pane(now, tx),
            View::History if self.two_pane() => self.enter_history_pane(now, tx),
            _ => {}
        }
    }

    /// Zoom from a Discover card: push the card snapshot, then promote.
    /// Detail entry is what resolves episodes, never card scroll (05 §10.1).
    fn open_discover_detail(&mut self, now: Instant, tx: &EventTx) {
        if let Some(entry) = self.discover.selected_entry() {
            let entry = entry.clone();
            self.detail.set_target(&entry, true, now);
        }
        self.promote();
        self.engage_detail(now, tx);
    }

    fn enter_browse_pane(&mut self, now: Instant, tx: &EventTx) {
        self.pane = Pane::Detail;
        self.push_browse_selection(true, now);
        self.engage_detail(now, tx);
    }

    fn on_h(&mut self) {
        match self.view {
            View::Browse | View::History if self.pane == Pane::Detail => self.pane = Pane::List,
            View::Detail => self.demote(),
            View::Discover => self.nav_discover(-1, 0),
            _ => {}
        }
    }

    fn on_l(&mut self, now: Instant, tx: &EventTx) {
        match self.view {
            View::Browse if self.pane == Pane::List && self.two_pane() => {
                self.enter_browse_pane(now, tx)
            }
            View::History if self.pane == Pane::List && self.two_pane() => {
                self.enter_history_pane(now, tx)
            }
            View::Discover => self.nav_discover(1, 0),
            _ => {}
        }
    }

    fn on_j(&mut self, now: Instant) {
        match self.view {
            View::Discover => self.nav_discover(0, 1),
            View::Browse if self.pane == Pane::List => self.on_browse_nav(1, now),
            View::History if self.pane == Pane::List => self.on_history_nav(1, now),
            View::Browse | View::History if self.pane == Pane::Detail => self.detail.on_vertical(1),
            View::Detail => self.detail.on_vertical(1),
            _ => {}
        }
    }

    fn on_k(&mut self, now: Instant) {
        match self.view {
            View::Discover => self.nav_discover(0, -1),
            View::Browse if self.pane == Pane::List => self.on_browse_nav(-1, now),
            View::History if self.pane == Pane::List => self.on_history_nav(-1, now),
            View::Browse | View::History if self.pane == Pane::Detail => {
                self.detail.on_vertical(-1)
            }
            View::Detail => self.detail.on_vertical(-1),
            _ => {}
        }
    }

    /// g / G jump (DESIGN 6.1): a focused detail surface consumes it for the
    /// grid (freeze parity, inert without one), else the focused list jumps.
    fn on_jump(&mut self, top: bool, now: Instant) {
        let on_detail_surface = self.view == View::Detail
            || (matches!(self.view, View::Browse | View::History) && self.pane == Pane::Detail);
        if on_detail_surface {
            self.detail.jump(top);
            return;
        }
        match self.view {
            View::Browse if self.pane == Pane::List => {
                self.browse.jump(top, self.list_visible());
                self.push_browse_selection(false, now);
            }
            View::History if self.pane == Pane::List => {
                self.history.jump(top, self.list_visible());
                self.push_history_selection(false, now);
            }
            _ => {}
        }
    }

    /// History cursor motion pushes the focused record into the shared detail
    /// preview (DESIGN 5.4a: focus change = immediate update).
    fn on_history_nav(&mut self, dy: i64, now: Instant) {
        self.history.nav(dy, self.list_visible());
        self.push_history_selection(false, now);
    }

    fn push_history_selection(&mut self, discrete: bool, now: Instant) {
        if !self.two_pane() && self.view != View::Detail {
            return;
        }
        match self.history.selected().map(|s| s.enrichment.clone()) {
            Some(entry) => self.detail.set_target(&entry, discrete, now),
            None => self.detail.clear_target(&mut self.pool),
        }
    }

    fn enter_history_pane(&mut self, now: Instant, tx: &EventTx) {
        self.pane = Pane::Detail;
        self.push_history_selection(true, now);
        self.engage_detail(now, tx);
    }

    /// Narrow History (DESIGN 5.4a): no pane exists, so Enter/Space drill
    /// straight into the zoom; the fetch fires with the open.
    fn open_history_zoom(&mut self, now: Instant, tx: &EventTx) {
        self.promote();
        self.push_history_selection(true, now);
        self.engage_detail(now, tx);
    }

    /// Cursor motion is continuous scroll: the metadata updates instantly,
    /// the cover trails by the settle window (DESIGN 6.4).
    fn on_browse_nav(&mut self, dy: i64, now: Instant) {
        self.browse.nav(dy, self.list_visible());
        self.push_browse_selection(false, now);
    }

    /// List rows are 1 cell tall; the visible band is the content height.
    fn list_visible(&self) -> usize {
        self.term.1.saturating_sub(3) as usize
    }

    /// The list-to-detail push contract: the shared surface receives a
    /// snapshot, never a reference into the list's own state.
    fn push_browse_selection(&mut self, discrete: bool, now: Instant) {
        if !self.two_pane() && self.view != View::Detail {
            return;
        }
        match self.browse.selected() {
            Some(entry) => {
                let entry = entry.clone();
                self.detail.set_target(&entry, discrete, now);
            }
            None => self.detail.clear_target(&mut self.pool),
        }
    }

    fn nav_discover(&mut self, dx: i64, dy: i64) {
        let geo = self.discover_geo();
        self.discover.nav(dx, dy, &geo);
    }

    fn on_axis_cycle(&mut self, delta: i64) {
        if self.view == View::Discover {
            self.discover.cycle_axis(delta);
        }
    }

    fn on_axis_select(&mut self, c: char) {
        if self.view == View::Discover {
            self.discover.select_axis((c as u8 - b'1') as usize);
        }
    }

    /// `P` "plan it" (DESIGN 6.1): saves the highlighted Discover card or
    /// Browse result as planning; in the History list it is the fifth manual
    /// transition (re-plan, with undo).
    fn on_plan(&mut self, now: Instant) {
        if self.view == View::History {
            if self.pane == Pane::List {
                self.on_status_key(domain::ListStatus::Planning, now);
            }
            return;
        }
        let entry = match self.view {
            View::Discover => self.discover.selected_entry(),
            View::Browse if self.pane == Pane::List => self.browse.selected(),
            _ => None,
        };
        let Some(entry) = entry else {
            return;
        };
        match self.store.add_to_library(entry, unix_now()) {
            Ok(()) => {
                self.arm_sync(now);
                self.toasts.push(Kind::Success, "added to watchlist", now);
            }
            Err(_) => self
                .toasts
                .push(Kind::Error, "couldn't add to watchlist", now),
        }
    }

    /// History status transitions (05 §4): store + memory move together (the
    /// reload re-groups and the cursor follows the show's identity). The
    /// single-level undo captures the pre-mutation state.
    fn on_status_key(&mut self, status: domain::ListStatus, now: Instant) {
        if self.view != View::History || self.pane != Pane::List {
            return;
        }
        let Some(show) = self.history.selected() else {
            return;
        };
        let aid = show.enrichment.anilist_id;
        let before = (aid, show.list_status, show.progress);
        if self.store.set_list_status(aid, status, unix_now()).is_err() {
            self.toasts.push(Kind::Error, "couldn't update status", now);
            return;
        }
        self.undo = Some(before);
        self.arm_sync(now);
        self.reload_history(now);
    }

    /// `u` undoes the last status mutation (05 §4), keyed off the captured
    /// row id, never the cursor (freeze parity).
    fn on_undo(&mut self, now: Instant) {
        if self.view != View::History || self.pane != Pane::List {
            return;
        }
        let Some((aid, status, progress)) = self.undo.take() else {
            return;
        };
        if self
            .store
            .restore_list_status(aid, status, progress, unix_now())
            .is_ok()
        {
            self.toasts.push(Kind::Info, "undone", now);
            self.arm_sync(now);
        }
        self.reload_history(now);
    }

    /// `r` recomputes progress from episode_progress (05 §4). The recompute
    /// survives a pending undo (c-then-r-then-u law), and recompute-to-0
    /// clears the row's resume marker.
    fn on_recompute(&mut self, now: Instant) {
        if self.view != View::History || self.pane != Pane::List {
            return;
        }
        let Some(show) = self.history.selected() else {
            return;
        };
        let aid = show.enrichment.anilist_id;
        let translation = Translation::parse(&self.config.translation).unwrap_or(Translation::Sub);
        match self.store.recompute_progress(aid, translation) {
            Ok(high_water) => {
                self.undo = None;
                self.reload_history(now);
                if high_water == 0 {
                    self.history.clear_resume_marker(aid);
                }
                self.arm_sync(now);
                self.toasts.push(Kind::Success, "progress reset", now);
            }
            Err(_) => self
                .toasts
                .push(Kind::Error, "couldn't reset progress", now),
        }
    }

    /// `X` arms the hard-delete confirm (DESIGN 6.5); a no-op off the History
    /// list or with no entry under the cursor.
    fn on_arm_delete(&mut self) {
        if self.view != View::History || self.pane != Pane::List {
            return;
        }
        if let Some(show) = self.history.selected() {
            self.confirm_delete = Some(show.enrichment.anilist_id);
        }
    }

    /// Armed-confirm key table (DESIGN 6.5): only `y` fires; a repeat `X`
    /// stays armed so a key storm can't self-confirm; anything else cancels
    /// (`q` included, swallowed).
    fn on_confirm_key(&mut self, key: KeyEvent, now: Instant) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let Some(aid) = self.confirm_delete.take() else {
                    return;
                };
                // ROD-220: the currently-playing show refuses the cascade;
                // the confirm is already disarmed (freeze parity).
                if self.playback.playing_aid() == Some(aid) {
                    self.toasts
                        .push(Kind::Warn, "can't delete, currently playing", now);
                    return;
                }
                let _ = self.store.delete_show(aid);
                // A stale undo pointing at the deleted row is cleared.
                if self.undo.map(|(a, _, _)| a) == Some(aid) {
                    self.undo = None;
                }
                // Cursor holds its ordinal via the reload clamp; deleting the
                // last show falls to the 8.3 empty state naturally.
                self.reload_history(now);
            }
            KeyCode::Char('X') => {}
            _ => self.confirm_delete = None,
        }
    }

    /// One reload path: re-read, cancel any armed confirm (its row identity
    /// can no longer be trusted, DESIGN 6.5), refresh the preview.
    fn reload_history(&mut self, now: Instant) {
        self.history.load(&self.store);
        self.confirm_delete = None;
        if self.view == View::History {
            self.push_history_selection(false, now);
        }
    }

    /// Promote to the zoom, recording the entry point (DESIGN 7.1).
    fn promote(&mut self) {
        self.origin = match self.view {
            View::History => Origin::History,
            View::Discover => Origin::Discover,
            _ => Origin::Browse,
        };
        self.view = View::Detail;
    }

    /// Demote from the zoom: back to the origin's detail pane when there is
    /// room, else its list; Discover has no pane to restore (DESIGN 7.4).
    fn demote(&mut self) {
        self.view = self.origin.view();
        self.pane = if self.origin != Origin::Discover && self.two_pane() {
            Pane::Detail
        } else {
            Pane::List
        };
    }

    fn two_pane(&self) -> bool {
        self.term.0 >= layout::PANE_SPLIT_MIN
    }

    fn on_search_key(&mut self, key: KeyEvent, now: Instant) {
        match key.code {
            KeyCode::Esc => self.close_search(),
            KeyCode::Enter => self.lock_search(),
            KeyCode::Backspace => self.on_search_backspace(now),
            KeyCode::Char(c) => self.on_search_char(c, now),
            _ => {}
        }
    }

    /// Esc clears the query and restores the full list (DESIGN 6.2).
    fn close_search(&mut self) {
        self.search_buffer().clear();
        if self.view == View::History {
            self.history.on_filter_cleared();
        }
        self.mode = InputMode::Normal;
    }

    /// Enter locks the result set and moves focus to the list; the query
    /// survives for the next `/` (DESIGN 6.2).
    fn lock_search(&mut self) {
        self.mode = InputMode::Normal;
        self.pane = Pane::List;
    }

    fn on_search_char(&mut self, c: char, now: Instant) {
        self.search_buffer().push(c);
        self.arm_search(now);
    }

    fn on_search_backspace(&mut self, now: Instant) {
        self.search_buffer().pop();
        self.arm_search(now);
    }

    /// Browse's catalogue search debounces the network fetch (04 §8);
    /// History's filter is local and needs none (chunk 5).
    fn arm_search(&mut self, now: Instant) {
        match self.view {
            View::Browse => self.browse.on_query_edited(now),
            // The filter is local: no debounce, immediate narrowing.
            View::History => self.history.on_filter_edited(),
            _ => {}
        }
    }

    /// Browse owns the catalogue query, History its local filter (DESIGN 8.4).
    fn search_buffer(&mut self) -> &mut String {
        match self.view {
            View::History => &mut self.history.filter,
            _ => &mut self.browse.query,
        }
    }

    fn on_command_key(&mut self, key: KeyEvent, now: Instant, tx: &EventTx) {
        match key.code {
            KeyCode::Esc => self.mode = InputMode::Normal,
            KeyCode::Enter => self.run_command(now, tx),
            KeyCode::Backspace => {
                self.command.pop();
            }
            KeyCode::Char(c) => self.command.push(c),
            _ => {}
        }
    }

    /// DESIGN 6.3. `sync` and `cache clear` are recognized but inert until
    /// their subsystems land (chunks 2-5); see the ROD-439 handoff notes.
    fn run_command(&mut self, now: Instant, tx: &EventTx) {
        let command = std::mem::take(&mut self.command);
        self.mode = InputMode::Normal;
        match command.trim() {
            "q" => self.on_quit(now),
            "dub" => self.toggle_translation(now, tx),
            "sync" | "cache clear" => {}
            _ => self.on_unknown_command(now),
        }
    }

    /// The track flip changes the episode-cache key, so a visible grid
    /// re-resolves once; the reset supersedes any in-flight walk instead of
    /// stacking a second one (config churn must not storm the walk).
    fn toggle_translation(&mut self, now: Instant, tx: &EventTx) {
        self.config.translation = if self.config.translation == "dub" {
            "sub".to_string()
        } else {
            "dub".to_string()
        };
        let engaged = self
            .detail
            .shown()
            .is_some_and(|e| self.detail.episodes.engaged_for(e.anilist_id));
        if engaged {
            self.detail.episodes.reset();
            self.engage_detail(now, tx);
        }
    }

    /// Both feedback channels are specced: the 800ms bar flash (DESIGN 3.5)
    /// and the error toast (DESIGN 6.3).
    fn on_unknown_command(&mut self, now: Instant) {
        self.command_flash = Some(now + COMMAND_FLASH);
        self.toasts.push(Kind::Error, "unknown command", now);
    }

    fn on_resize(&mut self, w: u16, h: u16, now: Instant, tx: &EventTx) {
        self.term = (w, h);
        if !self.two_pane() {
            // Below the split there is only one column (DESIGN 7.3).
            self.pane = Pane::List;
        }
        // The last_watched landing waits for the first real geometry: only
        // then is pane-vs-zoom decidable (DESIGN 8.3).
        if self.resume_pending && w > 0 {
            self.resume_pending = false;
            self.open_resume_landing(now, tx);
        }
        self.dirty = true;
    }

    /// Auto-open the most-recently-watched show parked on its resume episode
    /// (05 §10.6). Arms the demote: only THIS session's walk exhaust falls
    /// back to the list; user-driven opens never arm it.
    fn open_resume_landing(&mut self, now: Instant, tx: &EventTx) {
        let Some(aid) = self.history.first_played() else {
            return;
        };
        self.history.select_aid(aid, self.list_visible());
        self.resume_demote = Some(aid);
        if self.two_pane() {
            self.pane = Pane::Detail;
            self.push_history_selection(true, now);
            self.engage_detail(now, tx);
        } else {
            self.open_history_zoom(now, tx);
        }
        // A cached listing lands synchronously with no episode event to
        // clear the arm; a stranded arm would let a LATER same-show walk
        // exhaust (track flip, failed play) spuriously demote (05 §10.6:
        // only the auto-open's own walk may). The last-watched show's
        // listing is almost always still cached, so this is the common path.
        if self.detail.episodes.has_grid() {
            self.resume_demote = None;
        }
    }

    /// The failed auto-open demotes to the History list (05 §10.6); the walk
    /// already toasted its failure classes on the way down.
    fn demote_resume_landing(&mut self) {
        self.resume_demote = None;
        if self.view == View::Detail {
            self.view = View::History;
        }
        self.pane = Pane::List;
    }

    /// ~100ms cadence (04 §8): toast TTL, the command flash, the search
    /// debounce, the detail cover reconcile, and the Discover pump ride it.
    fn on_tick(&mut self, now: Instant, tx: &EventTx) {
        self.toasts.tick(now);
        if self.command_flash.is_some_and(|until| now >= until) {
            self.command_flash = None;
        }
        self.browse.maybe_fire(now, tx, &self.catalog);
        if self.detail_surface_visible() {
            self.detail.maybe_sync(
                now,
                self.config.cover_art,
                tx,
                &self.caches,
                &self.covers_dir,
                &mut self.pool,
            );
        }
        if self.view == View::Discover {
            self.tick_discover(now, tx);
        }
        if self.sync_debounce.fire(now) {
            self.flush_sync(now, tx, false);
        }
        self.dirty = true;
    }

    /// Where the shared detail surface is actually on screen: the zoom, or a
    /// two-pane view's right column.
    fn detail_surface_visible(&self) -> bool {
        match self.view {
            View::Detail => true,
            View::Browse | View::History => self.two_pane(),
            _ => false,
        }
    }

    /// Feed decision + cover pump for the active axis. The cap is a live
    /// config read each pump, the ROD-240 freeze law (the ROD-438 startup
    /// snapshot compromise is retired).
    fn tick_discover(&mut self, now: Instant, tx: &EventTx) {
        let geo = self.discover_geo();
        if let Some(page) = self.discover.wanted_fetch(&geo) {
            self.discover.fire_fetch(page, now, tx, &self.catalog);
        }
        if self.config.cover_art {
            let cap = self.config.effective_cover_concurrency() as usize;
            self.discover.pump(
                now,
                cap,
                &mut self.pool,
                tx,
                &self.caches,
                &self.covers_dir,
                &geo,
            );
        }
    }

    fn discover_geo(&self) -> discover::GridGeo {
        discover::grid_geo(self.term.0, self.term.1.saturating_sub(3), self.pool.cell())
    }

    fn on_discover_feed(
        &mut self,
        axis: DiscoverAxis,
        page: u32,
        entries: Vec<domain::Enrichment>,
        has_next: bool,
    ) {
        self.discover
            .on_feed(axis, page, entries, has_next, &self.store, unix_now());
        self.dirty = true;
    }

    fn on_discover_feed_error(&mut self, axis: DiscoverAxis, cause: String) {
        self.discover.on_feed_error(axis, cause);
        self.dirty = true;
    }

    fn on_discover_cover_done(&mut self, url: &str, img: image::DynamicImage) {
        self.discover.on_cover_done(url, img, &mut self.pool);
        self.dirty = true;
    }

    fn on_discover_cover_error(&mut self, url: &str, now: Instant) {
        self.discover.on_cover_error(url, now);
        self.dirty = true;
    }

    fn on_cover_done(&mut self, for_id: i64, img: image::DynamicImage) {
        self.detail.on_cover_done(for_id, img, &mut self.pool);
        self.dirty = true;
    }

    fn on_cover_error(&mut self, for_id: i64, now: Instant) {
        self.detail.on_cover_error(for_id, now);
        self.dirty = true;
    }

    /// Applied results are the AniList recovery signal: the persistent
    /// unreachable toast clears on the first success (DESIGN 8.5).
    fn on_search_done(&mut self, query: &str, results: Vec<domain::Enrichment>, now: Instant) {
        if self.browse.on_done(query, results, &self.store, unix_now()) {
            self.toasts.clear_topic(ANILIST_TOPIC);
            self.push_browse_selection(true, now);
        }
        self.dirty = true;
    }

    fn on_search_failed(&mut self, query: &str, now: Instant) {
        if self.browse.on_failed(query) {
            self.toasts
                .push_persistent(Kind::Error, "can't reach AniList", ANILIST_TOPIC, now);
        }
        self.dirty = true;
    }

    /// Encode worker wake: apply routed responses on the UI thread.
    fn on_encode_ready(&mut self) {
        if self.pool.apply_responses() {
            self.dirty = true;
        }
    }

    /// `v` cycles the open show's provider pin (DESIGN 6.1); detail surfaces
    /// only, inert elsewhere.
    fn on_pin_cycle(&mut self, now: Instant, tx: &EventTx) {
        let on_detail_surface = self.view == View::Detail
            || (matches!(self.view, View::Browse | View::History) && self.pane == Pane::Detail);
        if !on_detail_surface || self.detail.shown().is_none() {
            return;
        }
        let fb = {
            let deps = episode_deps(&self.store, &self.registry, &self.config, tx, now);
            self.detail.episodes.cycle_pin(&deps)
        };
        self.apply_episode_feedback(fb, now);
    }

    /// Detail entry resolves episodes (03 §6.1); list scroll never does
    /// (05 §10.1).
    fn engage_detail(&mut self, now: Instant, tx: &EventTx) {
        let Some(entry) = self.detail.shown().cloned() else {
            return;
        };
        let fb = {
            let deps = episode_deps(&self.store, &self.registry, &self.config, tx, now);
            self.detail.episodes.engage(&entry, &deps)
        };
        self.apply_episode_feedback(fb, now);
    }

    /// One handler for the four episode-session results: route into the
    /// session, then toast whatever it reports; the resume-landing demote
    /// rides the outcome (exhaust demotes, a landed grid clears the arm,
    /// intermediate hops keep it).
    fn on_episode_event(&mut self, event: Event, now: Instant, tx: &EventTx) {
        let event_aid = match &event {
            Event::EpisodesDone { anilist_id, .. }
            | Event::EpisodesError { anilist_id, .. }
            | Event::ProviderSearchDone { anilist_id, .. }
            | Event::ProviderSearchError { anilist_id, .. } => Some(*anilist_id),
            _ => None,
        };
        let fb = {
            let deps = episode_deps(&self.store, &self.registry, &self.config, tx, now);
            let session = &mut self.detail.episodes;
            match event {
                Event::EpisodesDone {
                    anilist_id,
                    provider,
                    provider_id,
                    episodes,
                    token,
                } => session.on_done(anilist_id, &provider, &provider_id, episodes, token, &deps),
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
                _ => Vec::new(),
            }
        };
        let dead_end = fb.iter().any(|f| matches!(f, Feedback::DeadEnd));
        self.apply_episode_feedback(fb, now);
        if self.resume_demote.is_some() && self.resume_demote == event_aid {
            if dead_end {
                self.demote_resume_landing();
            } else if self.detail.episodes.has_grid() {
                // Successful load clears the demote arm (05 §10.6).
                self.resume_demote = None;
            }
        }
        self.maybe_continue_play(now, tx);
        self.dirty = true;
    }

    /// Session outcomes to DESIGN 4.10 toast rows.
    fn apply_episode_feedback(&mut self, feedback: Vec<Feedback>, now: Instant) {
        for f in feedback {
            match f {
                Feedback::Fail { provider, class } => {
                    if let Some(copy) = failure_class_copy(class, &self.display_name(&provider)) {
                        self.toasts.push(Kind::Error, &copy, now);
                    }
                }
                Feedback::Hop { provider } => {
                    let copy = format!("trying {}…", self.display_name(&provider));
                    self.toasts.push(Kind::Warn, &copy, now);
                }
                Feedback::NoMatch { provider } => {
                    // K-2 step 5: distinct from the pin-kept copy.
                    let copy = format!("no match on {}", self.display_name(&provider));
                    self.toasts.push(Kind::Warn, &copy, now);
                }
                Feedback::PinKept { provider } => {
                    let copy = format!("no match on {}, pin kept", self.display_name(&provider));
                    self.toasts.push(Kind::Warn, &copy, now);
                }
                Feedback::DeadEnd => self.toasts.push(Kind::Error, "no source found", now),
                Feedback::PinUnreachable { provider } => {
                    let copy = format!("couldn't reach {}", self.display_name(&provider));
                    self.toasts.push(Kind::Warn, &copy, now);
                }
                Feedback::PinSet { provider } => {
                    let copy = format!("pinned to {}", self.display_name(&provider));
                    self.toasts.push(Kind::Success, &copy, now);
                }
                Feedback::PinCleared => self.toasts.push(Kind::Info, "provider pin cleared", now),
                Feedback::PinPending => {
                    self.toasts
                        .push(Kind::Info, "still resolving, try again shortly", now)
                }
                Feedback::PinNothing => {
                    self.toasts
                        .push(Kind::Info, "no source: nothing to pin", now)
                }
                Feedback::PinSaveFailed { clearing } => {
                    let copy = if clearing {
                        "couldn't clear the provider pin"
                    } else {
                        "couldn't save the provider pin"
                    };
                    self.toasts.push(Kind::Error, copy, now);
                }
            }
        }
    }

    fn display_name(&self, provider: &str) -> String {
        self.registry
            .by_name(provider)
            .map(|p| p.display_name().to_string())
            .unwrap_or_else(|| provider.to_string())
    }

    /// The Settings tab needs runtime facts (registry names, cache path)
    /// beyond the palette + state every other view gets.
    fn draw_settings(&mut self, frame: &mut Frame<'_>, area: ratatui::layout::Rect) {
        let names: Vec<&str> = self.registry.iter().map(|p| p.name()).collect();
        let covers_dir = settings::tilde_path(&self.covers_dir);
        let account = self.account_line();
        let env = settings::SettingsEnv {
            config: &self.config,
            providers: &names,
            covers_dir: &covers_dir,
            account: &account,
        };
        settings::draw(frame, area, self.palette, &self.settings, &env);
    }

    /// One Settings keypress (DESIGN 5.5): the state mutates config and
    /// reports; App projects the live pieces and leaves persistence to the
    /// leave/quit path. Returns whether the key was consumed.
    fn on_settings_key(&mut self, key: KeyEvent, now: Instant, tx: &EventTx) -> bool {
        let translation_before = self.config.translation.clone();
        let names: Vec<&str> = self.registry.iter().map(|p| p.name()).collect();
        let outcome = self.settings.on_key(key.code, &mut self.config, &names);
        match outcome {
            settings::KeyOutcome::Ignored => false,
            settings::KeyOutcome::Consumed => true,
            settings::KeyOutcome::ConfigChanged => {
                self.on_settings_config_changed(&translation_before, now, tx);
                true
            }
            settings::KeyOutcome::ConnectRequested => {
                self.open_connect(now, tx);
                true
            }
        }
    }

    /// Raise the connect modal (DESIGN 5.5a): bind the loopback synchronously so
    /// a bind failure is a toast, not a half-open modal; open the browser; then
    /// run the accept loop off the render path.
    fn open_connect(&mut self, now: Instant, tx: &EventTx) {
        let loopback = match Loopback::start() {
            Ok(lp) => lp,
            Err(_) => {
                self.toasts
                    .push(Kind::Error, "could not start login server", now);
                return;
            }
        };
        let url = loopback.authorize_url();
        open_browser(&url);
        let canceler = loopback.canceler();
        let started = workers::spawn_connect(
            &self.sync_drain,
            tx.clone(),
            loopback,
            self.auth_file.clone(),
            unix_now(),
        );
        if !started {
            // The OS refused the thread; nothing will drive the modal.
            canceler.cancel();
            self.toasts
                .push(Kind::Error, "could not start login worker", now);
            return;
        }
        self.connect = Some(ConnectSession {
            url,
            canceler,
            started: now,
            copied: false,
        });
    }

    /// Captured connect-modal keys (DESIGN 5.5a): only `c` (copy) and `esc`
    /// (cancel) act; everything else is swallowed.
    fn on_connect_key(&mut self, key: KeyEvent, _now: Instant) {
        match key.code {
            KeyCode::Esc => {
                if let Some(session) = self.connect.take() {
                    // Wake the blocked accept; the worker skips posting on cancel.
                    session.canceler.cancel();
                }
            }
            KeyCode::Char('c') => {
                if let Some(session) = &mut self.connect {
                    copy_to_clipboard(&session.url);
                    session.copied = true;
                }
            }
            _ => {}
        }
    }

    /// A connect worker finished (04 §4.6): close the modal, reload auth so the
    /// account row and the sync gate see the new token, and toast the outcome.
    fn on_connect_result(&mut self, result: ConnectResult, now: Instant, tx: &EventTx) {
        self.connect = None;
        self.auth = Auth::load(&self.auth_file);
        let (kind, copy) = match result {
            ConnectResult::Ok { user_name } => {
                // Post-connect bootstrap (ROD-292): pull then push (06 §5.2).
                self.flush_sync(now, tx, false);
                (Kind::Success, format!("signed in as {user_name}"))
            }
            ConnectResult::NoToken => (Kind::Error, "no token received".into()),
            ConnectResult::Rejected => (Kind::Error, "AniList rejected the token".into()),
            ConnectResult::NetworkError => (Kind::Error, "could not reach AniList".into()),
            ConnectResult::SaveFailed => (Kind::Error, "signed in, but saving the token failed".into()),
            ConnectResult::BadState => (Kind::Error, "login state mismatch".into()),
            // Never posted by the worker; nothing to report.
            ConnectResult::Canceled => return,
        };
        self.toasts.push(kind, &copy, now);
    }

    /// A live, unexpired token (DESIGN 5.5 `anilist_connected`).
    fn anilist_connected(&self) -> bool {
        let a = &self.auth.anilist;
        a.bearer().is_some() && !a.is_expired(unix_now())
    }

    /// The master switch ANDed with the connection (DESIGN 5.5 `sync_enabled`):
    /// the gate on whether to spawn any sync at all.
    fn sync_enabled(&self) -> bool {
        self.config.anilist_sync_enabled && self.anilist_connected()
    }

    /// Arm the action-flush debounce after a status/play edit (ROD-291). A no-op
    /// when sync is off or disconnected.
    fn arm_sync(&mut self, now: Instant) {
        if self.sync_enabled() {
            self.sync_debounce.arm(now, SYNC_FLUSH_PERIOD);
        }
    }

    /// Spawn a sync run, unless one is already inflight (re-arm to retry) or the
    /// gate is closed. `pull_only` is the launch-refresh path (06 §5.2).
    fn flush_sync(&mut self, now: Instant, tx: &EventTx, pull_only: bool) {
        if !self.sync_enabled() {
            return;
        }
        if self.syncing {
            // A run is going; retry after another period rather than overlap.
            self.sync_debounce.arm(now, SYNC_FLUSH_PERIOD);
            return;
        }
        let started = workers::spawn_sync(
            &self.sync_drain,
            tx.clone(),
            self.db_file.clone(),
            self.auth.clone(),
            self.config.anilist_sync_enabled,
            pull_only,
            unix_now(),
        );
        self.syncing = started;
    }

    /// Launch pull-refresh (04 §3): pull only, so first contact never blind-pushes.
    pub(super) fn bootstrap_sync(&mut self, tx: &EventTx) {
        self.flush_sync(Instant::now(), tx, true);
    }

    /// A sync run finished: clear the inflight flag and toast what moved
    /// (DESIGN 4.10 up/down rows). Failures and no-ops are silent by design.
    fn on_sync_flushed(&mut self, summary: sync::SyncSummary, now: Instant) {
        self.syncing = false;
        if summary.pulled.reconciled > 0 {
            self.toasts.push(
                Kind::Info,
                &format!("↓ {} from AniList", summary.pulled.reconciled),
                now,
            );
        }
        if summary.pushed > 0 {
            self.toasts
                .push(Kind::Info, &format!("↑ {} to AniList", summary.pushed), now);
        }
    }

    /// Quit flush (04 §11): push only, skipped while a pull may be inflight.
    /// Best-effort; teardown's drain deadline bounds it.
    pub(super) fn spawn_quit_flush(&self, tx: &EventTx) {
        if self.syncing || !self.sync_enabled() {
            return;
        }
        let _ = workers::spawn_flush(
            &self.sync_drain,
            tx.clone(),
            self.db_file.clone(),
            self.auth.clone(),
            self.config.anilist_sync_enabled,
            unix_now(),
        );
    }

    /// The account row's live text (DESIGN 5.5): the user name once connected,
    /// the reconnect prompt when a token exists but is expired, else not
    /// connected.
    fn account_line(&self) -> String {
        let a = &self.auth.anilist;
        match a.bearer() {
            None => "not connected".into(),
            Some(_) if a.is_expired(unix_now()) => "reconnect · token expired".into(),
            Some(_) if a.user_name.is_empty() => "connected".into(),
            Some(_) => a.user_name.clone(),
        }
    }

    /// Cancel a pending connect at teardown so the blocked loopback worker can
    /// exit before its drain.
    pub(super) fn shutdown_connect(&self) {
        if let Some(session) = &self.connect {
            session.canceler.cancel();
        }
    }

    /// Live projections after a Settings mutation: the palette repaints on
    /// the next frame; a translation change re-keys an engaged grid exactly
    /// like the `:dub` command (same reset, no walk storm).
    fn on_settings_config_changed(&mut self, translation_before: &str, now: Instant, tx: &EventTx) {
        self.palette = theme::by_name(&self.config.palette);
        if self.config.translation != translation_before {
            let engaged = self
                .detail
                .shown()
                .is_some_and(|e| self.detail.episodes.engaged_for(e.anilist_id));
            if engaged {
                self.detail.episodes.reset();
                self.engage_detail(now, tx);
            }
        }
    }

    /// Quit (`q` / `:q`): a dirty Settings tab persists first (DESIGN 7.2);
    /// Ctrl-C stays the emergency exit that skips this.
    fn on_quit(&mut self, now: Instant) {
        if self.view == View::Settings {
            self.persist_settings(now);
        }
        self.quit = true;
    }

    /// Save-if-dirty on leaving Settings (freeze ROD-210); the §4.10 rows
    /// name the three outcomes. A missing config dir skips the write.
    fn persist_settings(&mut self, now: Instant) {
        if !self.settings.dirty {
            return;
        }
        self.settings.dirty = false;
        if !self.config_file.parent().is_some_and(|dir| dir.is_dir()) {
            self.toasts
                .push(Kind::Warn, "no config dir · not saved", now);
            return;
        }
        match self.config.save(&self.config_file) {
            Ok(()) => self.toasts.push(Kind::Success, "settings saved", now),
            Err(_) => self.toasts.push(Kind::Error, "settings save failed", now),
        }
    }

    /// Enter on a focused grid plays the cursor episode through the serving
    /// binding (03 §6.3); inert without a landed grid. The double-play guard
    /// lives in the session.
    fn on_play(&mut self, now: Instant, tx: &EventTx) {
        let Some(entry) = self.detail.shown() else {
            return;
        };
        let aid = entry.anilist_id;
        let session = &self.detail.episodes;
        if !session.is_for(aid) || !session.has_grid() {
            return;
        }
        let cursor = session.cursor();
        self.fire_play_at(aid, cursor, false, now, tx);
    }

    /// One play fire off the engaged session's grid: `ix` is the 0-based
    /// cell. `continued` keeps the walk's continuation armed (03 §6.4); a
    /// user-driven fire supersedes it inside the session.
    fn fire_play_at(&mut self, aid: i64, ix: usize, continued: bool, now: Instant, tx: &EventTx) {
        let Some(entry) = self.detail.shown() else {
            return;
        };
        if entry.anilist_id != aid {
            return;
        }
        let session = &self.detail.episodes;
        let Some(serving) = session.serving() else {
            return;
        };
        let Some(episode_label) = session.grid().get(ix).cloned() else {
            return;
        };
        // The serving grid minted its binding before caching (ROD-327), so a
        // miss here is a torn store; refuse rather than resolve blind.
        let Some(provider_id) = self
            .store
            .bindings_for(aid)
            .ok()
            .and_then(|bs| bs.into_iter().find(|b| b.provider == serving))
            .map(|b| b.provider_id)
        else {
            return;
        };
        let provider = serving.to_string();
        let episode_ix = ix as u32 + 1;
        // Finale = last playable episode: the aired count clamps an airing
        // show's grid so an unaired tail never blocks `all caught up`.
        let playable = detail::aired_count(entry).map_or(session.grid().len() as u32, |a| {
            a.min(session.grid().len() as u32)
        });
        let finale = playable > 0 && episode_ix >= playable;
        let title = format!(
            "{} · {episode_label}",
            domain::preferred_title(
                &entry.title_romaji,
                entry.title_english.as_deref(),
                entry.title_native.as_deref(),
                TitleLanguage::parse(&self.config.title_language),
            )
        );
        let request = PlayRequest {
            anilist_id: aid,
            provider,
            provider_id,
            episode_label,
            mal_id: entry.mal_id,
            episode_ix,
            finale,
            title,
        };
        let fb = {
            let deps = playback_deps(
                &self.store,
                &self.registry,
                &self.config,
                &self.play_dirs,
                tx,
                now,
            );
            if continued {
                self.playback.fire_continued(request, &deps)
            } else {
                self.playback.fire(request, &deps)
            }
        };
        self.apply_play_feedback(fb, now);
    }

    fn on_play_position(
        &mut self,
        anilist_id: i64,
        position: Position,
        token: u64,
        now: Instant,
        tx: &EventTx,
    ) {
        let deps = playback_deps(
            &self.store,
            &self.registry,
            &self.config,
            &self.play_dirs,
            tx,
            now,
        );
        self.playback
            .on_position(anilist_id, position, token, &deps);
        self.dirty = true;
    }

    fn on_play_retry(&mut self, anilist_id: i64, attempt: u32, token: u64, now: Instant) {
        let fb = self.playback.on_retry(anilist_id, attempt, token);
        self.apply_play_feedback(fb, now);
        self.dirty = true;
    }

    /// Terminal play outcome: session settles the writes, then a recorded
    /// finish fans out (grid progress refresh, history reload), and a
    /// hop-eligible failure walks to a sibling (03 §6.4).
    fn on_play_finished(
        &mut self,
        anilist_id: i64,
        position: Option<Position>,
        failure: Option<PlayFailure>,
        token: u64,
        now: Instant,
        tx: &EventTx,
    ) {
        let out = {
            let deps = playback_deps(
                &self.store,
                &self.registry,
                &self.config,
                &self.play_dirs,
                tx,
                now,
            );
            self.playback
                .on_finished(anilist_id, position, failure, token, &deps)
        };
        self.apply_play_feedback(out.feedback, now);
        if let Some(rec) = out.recorded {
            {
                let deps = episode_deps(&self.store, &self.registry, &self.config, tx, now);
                self.detail.episodes.on_play_recorded(
                    rec.anilist_id,
                    rec.episode_ix,
                    rec.completed,
                    &deps,
                );
            }
            self.arm_sync(now);
            self.reload_history(now);
        }
        if let Some(ask) = out.hop {
            self.on_play_hop(ask, now, tx);
        }
        self.dirty = true;
    }

    /// Fail the episode session over for a hop-eligible play failure, then
    /// try the continuation at once: a cache-hit hop lands synchronously and
    /// never produces an episode event to ride.
    fn on_play_hop(&mut self, ask: HopAsk, now: Instant, tx: &EventTx) {
        let session = &self.detail.episodes;
        // The target left the screen, or the user re-routed mid-play: the
        // walk has nothing to rescue.
        if !session.is_for(ask.anilist_id)
            || session.serving() != ask.tried.last().map(String::as_str)
        {
            self.playback.drop_continuation();
            return;
        }
        let fb = {
            let deps = episode_deps(&self.store, &self.registry, &self.config, tx, now);
            self.detail
                .episodes
                .play_fail_over(&ask.tried, (ask.label, ask.ordinal), &deps)
        };
        self.apply_episode_feedback(fb, now);
        self.maybe_continue_play(now, tx);
    }

    /// The play-continuation consumer (03 §6.4): once the walk lands a
    /// sibling grid, remap the in-progress episode (exact raw label, else
    /// 1-based ordinal) and relaunch. Runs after every episode event and
    /// after the synchronous hop path; a walk still in flight just waits.
    fn maybe_continue_play(&mut self, now: Instant, tx: &EventTx) {
        if self.playback.is_playing() {
            return;
        }
        let Some(cont) = self.playback.continuation() else {
            return;
        };
        let aid = cont.anilist_id;
        let session = &self.detail.episodes;
        let translation = Translation::parse(&self.config.translation).unwrap_or(Translation::Sub);
        if !session.is_for(aid) || cont.translation != translation || session.no_source() {
            self.playback.drop_continuation();
            return;
        }
        if session.loading().is_some() {
            return;
        }
        let Some(serving) = session.serving() else {
            return;
        };
        if cont.tried.iter().any(|t| t == serving) {
            // Nothing in flight and the grid still belongs to a burned
            // provider: the walk could not move, retire the continuation.
            self.playback.drop_continuation();
            return;
        }
        let Some(ix) = domain::map_episode_index(session.grid(), &cont.label, cont.ordinal) else {
            // Remap miss stops the play continuation (03 §7); the label is
            // provider text, stripped before it touches a toast (ROD-435
            // filter: escapes, bidi, zero-width).
            let raw = domain::strip_controls(cont.label.clone());
            let copy = format!("episode {raw} not found on {}", self.display_name(serving));
            self.playback.drop_continuation();
            self.toasts.push(Kind::Error, &copy, now);
            return;
        };
        self.fire_play_at(aid, ix, true, now, tx);
    }

    /// Playback outcomes to the DESIGN 4.10 play rows.
    fn apply_play_feedback(&mut self, feedback: Vec<PlayFeedback>, now: Instant) {
        for f in feedback {
            match f {
                PlayFeedback::Retry { attempt } => {
                    let copy = format!(
                        "stream didn't open · retrying {attempt}/{}",
                        player::MAX_PLAY_ATTEMPTS
                    );
                    self.toasts.push(Kind::Warn, &copy, now);
                }
                PlayFeedback::Done { episode_ix, finale } => {
                    if finale {
                        self.toasts.push(Kind::Success, "all caught up", now);
                    } else {
                        let copy = format!("episode {episode_ix} done");
                        self.toasts.push(Kind::Success, &copy, now);
                    }
                }
                PlayFeedback::Failed { provider, failure } => {
                    if let Some(copy) = play_failure_copy(failure, &self.display_name(&provider)) {
                        self.toasts.push(Kind::Error, &copy, now);
                    }
                }
                PlayFeedback::SaveFailed => {
                    self.toasts.push(Kind::Error, "couldn't save progress", now)
                }
            }
        }
    }

    /// Pure of app state and stores (04 §1); `&mut` is for the protocol
    /// pool's render-owned resize bookkeeping only.
    pub(super) fn draw(&mut self, frame: &mut Frame<'_>, now: Instant) {
        let area = frame.area();
        frame.render_widget(Block::new().style(Style::new().bg(self.palette.bg)), area);
        if layout::is_too_small(area.width, area.height) {
            // Degraded message frame, never a bail (04 §8).
            render::draw_centered(
                frame,
                area,
                area.height / 2,
                Line::from(Span::styled(
                    "terminal too small",
                    Style::new().fg(self.palette.fg2),
                )),
            );
            return;
        }
        let rows = layout::frame_rows(area);
        chrome::draw_top_bar(frame, rows.top, self.palette, &self.top_bar());
        match self.view {
            View::Browse => self.draw_browse(frame, rows.content, now),
            View::History => self.draw_history(frame, rows.content, now),
            View::Detail => self.draw_zoom(frame, rows.content, now),
            View::Discover => self.draw_discover(frame, rows.content, now),
            View::Settings => self.draw_settings(frame, rows.content),
        }
        chrome::draw_bottom_bar(frame, rows.bottom, self.palette, &self.bottom_bar(now));
        if let Some(session) = &self.connect {
            let view = ConnectView {
                url: &session.url,
                elapsed: now.saturating_duration_since(session.started),
                copied: session.copied,
            };
            connect::draw(frame, rows.content, self.palette, &view);
        }
        self.toasts.draw(frame, area, self.palette);
    }

    fn view_env(&self, now: Instant) -> ViewEnv {
        ViewEnv {
            pref: TitleLanguage::parse(&self.config.title_language),
            kanji: self.config.kanji_chips,
            cour: domain::current_cour(unix_now()),
            unix_now: unix_now(),
            now,
            play: self.playback.glance(),
        }
    }

    /// Browse composition (DESIGN 3.2): list column + shared detail pane at
    /// two-pane widths, list only below.
    fn draw_browse(&mut self, frame: &mut Frame<'_>, area: ratatui::layout::Rect, now: Instant) {
        let env = self.view_env(now);
        if self.two_pane() {
            let split = layout::pane_split(area.width);
            let list = ratatui::layout::Rect::new(
                area.x + 2,
                area.y,
                split.list_w.min(area.width.saturating_sub(2)),
                area.height,
            );
            browse::draw_list(
                frame,
                list,
                self.palette,
                &self.browse,
                &env,
                self.pane == Pane::List,
            );
            let pane = ratatui::layout::Rect::new(
                area.x + split.detail_x,
                area.y,
                split
                    .detail_w
                    .min(area.width.saturating_sub(split.detail_x)),
                area.height,
            );
            detail::draw_pane(
                frame,
                pane,
                self.palette,
                &self.detail,
                &env,
                &mut self.pool,
                self.pane == Pane::Detail,
                false,
            );
        } else {
            let list = ratatui::layout::Rect::new(
                area.x + 2,
                area.y,
                area.width.saturating_sub(3),
                area.height,
            );
            browse::draw_list(frame, list, self.palette, &self.browse, &env, true);
        }
    }

    /// History composition (DESIGN 5.4a): the split engages only with a
    /// focused record; empty/failed states keep the full-width column.
    fn draw_history(&mut self, frame: &mut Frame<'_>, area: ratatui::layout::Rect, now: Instant) {
        let env = self.view_env(now);
        if self.two_pane() && self.history.selected().is_some() {
            let split = layout::pane_split(area.width);
            let list = ratatui::layout::Rect::new(
                area.x + 2,
                area.y,
                split.list_w.min(area.width.saturating_sub(2)),
                area.height,
            );
            history::draw_list(
                frame,
                list,
                self.palette,
                &self.history,
                &env,
                self.pane == Pane::List,
            );
            let pane = ratatui::layout::Rect::new(
                area.x + split.detail_x,
                area.y,
                split
                    .detail_w
                    .min(area.width.saturating_sub(split.detail_x)),
                area.height,
            );
            detail::draw_pane(
                frame,
                pane,
                self.palette,
                &self.detail,
                &env,
                &mut self.pool,
                self.pane == Pane::Detail,
                // History's pane is the one in-pane surface that splits to two
                // columns past DETAIL_TWO_COL_MIN (DESIGN 5.3).
                true,
            );
        } else {
            let list = ratatui::layout::Rect::new(
                area.x + 2,
                area.y,
                area.width.saturating_sub(3),
                area.height,
            );
            history::draw_list(frame, list, self.palette, &self.history, &env, true);
        }
    }

    fn draw_zoom(&mut self, frame: &mut Frame<'_>, area: ratatui::layout::Rect, now: Instant) {
        let env = self.view_env(now);
        detail::draw_zoom(frame, area, self.palette, &self.detail, &env, &mut self.pool);
    }

    fn draw_discover(&mut self, frame: &mut Frame<'_>, area: ratatui::layout::Rect, now: Instant) {
        let env = self.view_env(now);
        discover::draw(
            frame,
            area,
            self.palette,
            &self.discover,
            &mut self.pool,
            &env,
        );
    }

    fn top_bar(&self) -> TopBar {
        let tab = match self.view {
            View::Browse => Tab::Browse,
            View::History => Tab::History,
            View::Discover => Tab::Discover,
            View::Settings => Tab::Settings,
            // The zoom is not a tab destination; it highlights its origin
            // (DESIGN 3.4).
            View::Detail => match self.origin {
                Origin::Browse => Tab::Browse,
                Origin::History => Tab::History,
                Origin::Discover => Tab::Discover,
            },
        };
        // Browse and History both mirror the focused row's season with the
        // cour fallback (DESIGN 8.3); Discover tracks the selected card and the
        // zoom its committed show, both with no fallback; Settings shows no
        // chip (DESIGN 3.4, 7.3).
        let season_chip = match self.view {
            View::Browse => self
                .browse
                .selected()
                .and_then(|e| render::season_chip(e.season, e.year))
                .or_else(|| Some(render::cour_chip(domain::current_cour(unix_now())))),
            View::History => self
                .history
                .selected()
                .and_then(|s| render::season_chip(s.enrichment.season, s.enrichment.year))
                .or_else(|| Some(render::cour_chip(domain::current_cour(unix_now())))),
            View::Discover => self
                .discover
                .selected_entry()
                .and_then(|e| render::season_chip(e.season, e.year)),
            View::Detail => self
                .detail
                .shown()
                .and_then(|e| render::season_chip(e.season, e.year)),
            View::Settings => None,
        };
        let dot_lit = match self.view {
            View::Browse | View::History => self.pane == Pane::Detail,
            _ => true,
        };
        TopBar {
            tab,
            season_chip,
            dot_lit,
        }
    }

    fn bottom_bar(&self, now: Instant) -> BottomBar<'_> {
        if let Some(show) = self
            .confirm_delete
            .and_then(|aid| self.history.show_by_aid(aid))
        {
            return BottomBar::Confirm {
                title: domain::preferred_title(
                    &show.enrichment.title_romaji,
                    show.enrichment.title_english.as_deref(),
                    show.enrichment.title_native.as_deref(),
                    TitleLanguage::parse(&self.config.title_language),
                ),
            };
        }
        if self.command_flash.is_some_and(|until| now < until) {
            return BottomBar::CommandError;
        }
        match self.mode {
            InputMode::Search => match self.view {
                View::History => BottomBar::Search {
                    query: &self.history.filter,
                    scope: "history",
                    count: self.history.count(),
                },
                _ => BottomBar::Search {
                    query: &self.browse.query,
                    scope: "catalogue",
                    count: self.browse.count(),
                },
            },
            InputMode::Command => BottomBar::Command {
                input: &self.command,
            },
            InputMode::Normal => BottomBar::Help(self.help_line()),
        }
    }

    fn help_line(&self) -> HelpLine {
        match self.view {
            View::Browse => match self.pane {
                Pane::List => HelpLine::BrowseList,
                Pane::Detail => HelpLine::BrowseDetail,
            },
            View::History if self.history.is_empty() => HelpLine::HistoryEmpty,
            View::History => match self.pane {
                Pane::List => HelpLine::HistoryList,
                Pane::Detail => HelpLine::HistoryDetail,
            },
            View::Detail => HelpLine::Zoom,
            View::Discover => HelpLine::Discover,
            View::Settings if self.settings.editing() => HelpLine::SettingsEdit,
            View::Settings => HelpLine::Settings,
        }
    }
}

/// Landing seeds from config; History is the default and the fallback for any
/// unrecognized value. `last_watched` also lands on History: the resume
/// auto-open then promotes its detail surface once geometry arrives
/// (DESIGN 8.3).
fn landing_view(landing: &str) -> View {
    match landing {
        "browse" => View::Browse,
        _ => View::History,
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Open a URL in the user's browser, best-effort: a failure just means the user
/// falls back to the copy-link key. Detached so it never blocks the render path.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    const OPEN_CMD: &str = "open";
    #[cfg(not(target_os = "macos"))]
    const OPEN_CMD: &str = "xdg-open";
    let _ = std::process::Command::new(OPEN_CMD)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Copy `text` to the system clipboard via OSC 52. Terminal-mediated (works
/// over SSH); a terminal that ignores the sequence just leaves the clipboard
/// untouched. The sequence is out-of-band, so it does not disturb the frame.
fn copy_to_clipboard(text: &str) {
    use base64::Engine;
    use std::io::Write;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{encoded}\x07");
    let _ = out.flush();
}

/// Free function so the deps borrow individual App fields and stay disjoint
/// from `&mut self.detail`.
fn episode_deps<'a>(
    store: &'a Store,
    registry: &'a Arc<ProviderRegistry>,
    config: &'a Config,
    tx: &'a EventTx,
    now: Instant,
) -> EpisodeDeps<'a> {
    EpisodeDeps {
        store,
        registry,
        tx,
        global_pref: &config.preferred_provider,
        translation: Translation::parse(&config.translation).unwrap_or(Translation::Sub),
        unix_now: unix_now(),
        now,
    }
}

/// The one failure-class → copy mapping (DESIGN 4.10); `episodes_error` and
/// the play resolve path share it. `Unsupported` is deliberately silent.
fn failure_class_copy(class: FetchClass, provider: &str) -> Option<String> {
    match class {
        FetchClass::Network => Some("network unreachable".to_string()),
        FetchClass::Blocked => Some(format!("{provider} blocked us")),
        FetchClass::Down => Some(format!("{provider} is down")),
        FetchClass::Http => Some(format!("{provider} returned an error")),
        FetchClass::Data => Some("couldn't load episodes".to_string()),
        FetchClass::Unsupported => None,
    }
}

/// The play-side rows (DESIGN 4.10): the resolve HTTP classes share
/// `failure_class_copy`; the player-spawn classes get their own copy; the
/// residuals collapse into `playback failed`. Unlike the walk, a resolve
/// `Unsupported` here is a real dead play, never silent.
fn play_failure_copy(failure: PlayFailure, provider: &str) -> Option<String> {
    match failure {
        PlayFailure::MpvNotFound => Some("mpv not found · install mpv".to_string()),
        PlayFailure::MpvFailed => Some("mpv exited with error".to_string()),
        PlayFailure::OpenFailed => Some("stream didn't open · try again".to_string()),
        PlayFailure::Resolve(class) => match class {
            FetchClass::Data | FetchClass::Unsupported => Some("playback failed".to_string()),
            _ => failure_class_copy(class, provider),
        },
        PlayFailure::Internal => Some("playback failed".to_string()),
    }
}

/// Free function for the same disjoint-borrow reason as `episode_deps`.
fn playback_deps<'a>(
    store: &'a Store,
    registry: &'a Arc<ProviderRegistry>,
    config: &'a Config,
    dirs: &'a PlayDirs,
    tx: &'a EventTx,
    now: Instant,
) -> PlaybackDeps<'a> {
    PlaybackDeps {
        store,
        registry,
        tx,
        mpv_path: &config.mpv_path,
        socket_dir: &dirs.socket,
        cache_dir: &dirs.cache,
        resume_offset_sec: config.resume_offset_sec,
        translation: Translation::parse(&config.translation).unwrap_or(Translation::Sub),
        quality: domain::Quality::parse(&config.default_quality),
        skip_mode: SkipMode::parse(&config.skip_mode),
        unix_now: unix_now(),
        now,
    }
}

/// The playback dirs (paths.runtime for the IPC socket, paths.cache for
/// skip.lua), bundled so `playback_deps` stays one disjoint borrow.
struct PlayDirs {
    socket: PathBuf,
    cache: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ch(c: char) -> Event {
        key(KeyCode::Char(c))
    }

    use crate::domain::Enrichment;
    use crate::providers::{CatalogError, CatalogPage};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted catalog: each `discover` call pops the next page; an empty
    /// script answers Network so nothing ever leaves the process.
    #[derive(Default)]
    struct StubCatalog {
        discover: Mutex<VecDeque<Result<CatalogPage, CatalogError>>>,
        search: Mutex<VecDeque<Result<CatalogPage, CatalogError>>>,
    }

    impl StubCatalog {
        fn scripted(pages: Vec<Result<CatalogPage, CatalogError>>) -> Arc<dyn CatalogProvider> {
            Arc::new(StubCatalog {
                discover: Mutex::new(pages.into()),
                ..Default::default()
            })
        }

        fn search_scripted(
            pages: Vec<Result<CatalogPage, CatalogError>>,
        ) -> Arc<dyn CatalogProvider> {
            Arc::new(StubCatalog {
                search: Mutex::new(pages.into()),
                ..Default::default()
            })
        }

        fn inert() -> Arc<dyn CatalogProvider> {
            Self::scripted(Vec::new())
        }
    }

    impl CatalogProvider for StubCatalog {
        fn search(&self, _q: &str, _p: u32) -> Result<CatalogPage, CatalogError> {
            self.search
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(CatalogError::Network))
        }
        fn discover(&self, _a: DiscoverAxis, _p: u32) -> Result<CatalogPage, CatalogError> {
            self.discover
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(CatalogError::Network))
        }
        fn enrich(&self, _id: i64) -> Result<Option<Enrichment>, CatalogError> {
            Ok(None)
        }
    }

    fn feed_entry(id: i64) -> Enrichment {
        Enrichment {
            anilist_id: id,
            title_romaji: format!("Show {id}"),
            ..Enrichment::default()
        }
    }

    fn one_page(n: i64) -> Result<CatalogPage, CatalogError> {
        Ok(CatalogPage {
            entries: (1..=n).map(feed_entry).collect(),
            has_next: false,
        })
    }

    use super::super::episodes::teststub;

    fn test_paths(name: &str) -> Paths {
        let dir = std::env::temp_dir().join("sabigoku-app-tests").join(name);
        Paths {
            config: dir.clone(),
            data: dir.clone(),
            cache: dir.clone(),
            runtime: dir,
        }
    }

    fn harness_full(
        name: &str,
        catalog: Arc<dyn CatalogProvider>,
        registry: Arc<ProviderRegistry>,
    ) -> (App, EventTx, super::super::event::EventRx, Instant) {
        let (tx, rx) = super::super::event::channel();
        let app = App::new(
            &Config::default(),
            Store::open_memory().unwrap(),
            catalog,
            registry,
            &test_paths(name),
            Picker::halfblocks(),
            &tx,
        );
        (app, tx, rx, Instant::now())
    }

    fn harness_with(
        name: &str,
        catalog: Arc<dyn CatalogProvider>,
    ) -> (App, EventTx, super::super::event::EventRx, Instant) {
        harness_full(name, catalog, teststub::inert_registry())
    }

    fn harness(name: &str) -> (App, EventTx, Instant) {
        let (app, tx, _rx, now) = harness_with(name, StubCatalog::inert());
        (app, tx, now)
    }

    fn sized(name: &str, w: u16, h: u16) -> (App, EventTx, Instant) {
        let (mut app, tx, now) = harness(name);
        app.tick(Event::Resize(w, h), now, &tx);
        (app, tx, now)
    }

    #[test]
    fn account_line_reflects_auth_state() {
        let (mut app, _tx, _now) = harness("account-line");
        assert_eq!(app.account_line(), "not connected");

        app.auth.anilist.access_token = "abcdefghijklmnopqrstuvwxyz".into();
        app.auth.anilist.user_name = "rod".into();
        app.auth.anilist.expires_at = 0; // undated stays live
        assert_eq!(app.account_line(), "rod");

        // A token with no name still reads as connected, not "not connected".
        app.auth.anilist.user_name = String::new();
        assert_eq!(app.account_line(), "connected");

        // Dated in the past is the reconnect state.
        app.auth.anilist.expires_at = 1;
        assert_eq!(app.account_line(), "reconnect · token expired");
    }

    fn connect(app: &mut App) {
        app.auth.anilist.access_token = "abcdefghijklmnopqrstuvwxyz".into();
        app.auth.anilist.user_id = 7;
        app.auth.anilist.expires_at = 0;
    }

    #[test]
    fn arm_sync_respects_the_connection_and_switch() {
        let (mut app, _tx, now) = harness("arm-sync");
        // Disconnected: arming is a no-op.
        app.arm_sync(now);
        assert!(!app.sync_debounce.is_armed());
        // Connected + switch on: arms.
        connect(&mut app);
        app.arm_sync(now);
        assert!(app.sync_debounce.is_armed());
        // Switch off: no arm.
        app.sync_debounce.disarm();
        app.config.anilist_sync_enabled = false;
        app.arm_sync(now);
        assert!(!app.sync_debounce.is_armed());
    }

    #[test]
    fn sync_flushed_toasts_up_and_down_counts() {
        let (mut app, _tx, now) = harness("sync-flushed");
        app.syncing = true;
        let summary = sync::SyncSummary {
            outcome: sync::SyncOutcome::Completed,
            pulled: crate::store::PullOutcome {
                reconciled: 2,
                ..Default::default()
            },
            pushed: 3,
            push_failed: 0,
        };
        app.on_sync_flushed(summary, now);
        assert!(!app.syncing, "the inflight flag clears");
        let copies: Vec<String> = app.toasts.iter().map(|t| t.copy.clone()).collect();
        assert!(copies.iter().any(|c| c == "↓ 2 from AniList"));
        assert!(copies.iter().any(|c| c == "↑ 3 to AniList"));

        // A no-op run is silent.
        let (mut app, _tx, now) = harness("sync-flushed-noop");
        app.on_sync_flushed(sync::SyncSummary::terminal(sync::SyncOutcome::Completed), now);
        assert_eq!(app.toasts.iter().count(), 0);
    }

    #[test]
    fn connect_result_closes_the_modal_and_toasts() {
        let (mut app, tx, now) = harness("connect-result");
        // No modal open; the handler still reloads auth and toasts the outcome.
        app.on_connect_result(
            ConnectResult::Ok {
                user_name: "rod".into(),
            },
            now,
            &tx,
        );
        assert!(app.connect.is_none());
        assert_eq!(app.toasts.iter().count(), 1);
    }

    /// Settle every worker family, applying one event per drain pass so a
    /// worker spawned WHILE applying (a walk hop) settles too.
    fn settle_feed(app: &mut App, tx: &EventTx, rx: &super::super::event::EventRx, now: Instant) {
        loop {
            assert!(app.discover.drain(Duration::from_secs(5)));
            assert!(app.browse.drain(Duration::from_secs(5)));
            assert!(app.detail.drain(Duration::from_secs(5)));
            let Ok(ev) = rx.try_recv() else { break };
            app.tick(ev, now, tx);
        }
    }

    fn press(app: &mut App, tx: &EventTx, now: Instant, events: &[Event]) {
        for ev in events {
            app.tick(ev.clone(), now, tx);
        }
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buffer = term.backend().buffer();
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn rendered(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|frame| app.draw(frame, Instant::now())).unwrap();
        buffer_text(&term)
    }

    #[test]
    fn landing_seeds_from_config() {
        assert_eq!(landing_view("history"), View::History);
        assert_eq!(landing_view("browse"), View::Browse);
        assert_eq!(landing_view("last_watched"), View::History);
        assert_eq!(landing_view("garbage"), View::History);
    }

    #[test]
    fn q_quits_from_every_view_in_normal_mode() {
        for view in [
            View::Browse,
            View::History,
            View::Detail,
            View::Discover,
            View::Settings,
        ] {
            let (mut app, tx, now) = sized("q-quits", 100, 30);
            app.view = view;
            app.tick(ch('q'), now, &tx);
            assert!(app.quit, "q must quit from {view:?}");
        }
    }

    #[test]
    fn ctrl_c_quits_in_any_mode() {
        for mode in [InputMode::Normal, InputMode::Search, InputMode::Command] {
            let (mut app, tx, now) = sized("ctrl-c", 100, 30);
            app.mode = mode;
            app.tick(
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                now,
                &tx,
            );
            assert!(app.quit, "ctrl-c must quit from {mode:?}");
        }
    }

    #[test]
    fn input_closed_quits() {
        let (mut app, tx, now) = harness("input-closed");
        app.tick(Event::InputClosed, now, &tx);
        assert!(app.quit);
    }

    /// DESIGN 7.4: Esc peels transient layers, never switches base view,
    /// never quits.
    #[test]
    fn esc_chain_table() {
        // search -> close, query cleared, view unchanged
        let (mut app, tx, now) = sized("esc-search", 100, 30);
        app.tick(ch('B'), now, &tx);
        press(
            &mut app,
            &tx,
            now,
            &[ch('/'), ch('a'), ch('b'), key(KeyCode::Esc)],
        );
        assert_eq!(app.mode, InputMode::Normal);
        assert_eq!(app.view, View::Browse);
        assert!(app.browse.query.is_empty(), "Esc clears the query");
        assert!(!app.quit);

        // command -> close
        press(&mut app, &tx, now, &[ch(':'), ch('x'), key(KeyCode::Esc)]);
        assert_eq!(app.mode, InputMode::Normal);
        assert!(!app.quit);

        // Browse detail pane -> back to list
        press(&mut app, &tx, now, &[ch('l')]);
        assert_eq!(app.pane, Pane::Detail);
        app.tick(key(KeyCode::Esc), now, &tx);
        assert_eq!(app.pane, Pane::List);

        // Browse list -> no-op, not a quit
        app.tick(key(KeyCode::Esc), now, &tx);
        assert_eq!(app.view, View::Browse);
        assert!(!app.quit);

        // History list and Settings -> no-op
        for target in ['H', 'S'] {
            app.tick(ch(target), now, &tx);
            let view = app.view;
            app.tick(key(KeyCode::Esc), now, &tx);
            assert_eq!(app.view, view);
            assert!(!app.quit);
        }
    }

    #[test]
    fn zoom_demotes_to_origin_pane_when_wide() {
        let (mut app, tx, now) = sized("zoom-wide", 100, 30);
        app.tick(ch('B'), now, &tx);
        press(&mut app, &tx, now, &[ch('l'), ch(' ')]);
        assert_eq!(app.view, View::Detail);
        assert_eq!(app.origin, Origin::Browse);
        app.tick(key(KeyCode::Esc), now, &tx);
        assert_eq!(app.view, View::Browse);
        assert_eq!(app.pane, Pane::Detail, "Esc undoes one step, not two");
    }

    #[test]
    fn zoom_demotes_to_list_when_narrow() {
        let (mut app, tx, now) = sized("zoom-narrow", 50, 30);
        assert_eq!(app.view, View::History);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert_eq!(app.view, View::Detail, "narrow History list opens the zoom");
        assert_eq!(app.origin, Origin::History);
        app.tick(key(KeyCode::Esc), now, &tx);
        assert_eq!(app.view, View::History);
        assert_eq!(app.pane, Pane::List);
    }

    #[test]
    fn zoom_from_discover_demotes_to_discover() {
        let (mut app, tx, now) = sized("zoom-discover", 100, 30);
        press(&mut app, &tx, now, &[ch('D'), key(KeyCode::Enter)]);
        assert_eq!(app.view, View::Detail);
        assert_eq!(app.origin, Origin::Discover);
        app.tick(key(KeyCode::Esc), now, &tx);
        assert_eq!(app.view, View::Discover);
    }

    #[test]
    fn space_is_a_symmetric_zoom_toggle() {
        let (mut app, tx, now) = sized("space-toggle", 100, 30);
        app.tick(ch('B'), now, &tx);
        press(&mut app, &tx, now, &[ch('l'), ch(' ')]);
        assert_eq!(app.view, View::Detail);
        app.tick(ch(' '), now, &tx);
        assert_eq!(app.view, View::Browse);
        assert_eq!(app.pane, Pane::Detail);
    }

    #[test]
    fn h_demotes_from_zoom_like_esc() {
        let (mut app, tx, now) = sized("h-demote", 100, 30);
        app.tick(ch('B'), now, &tx);
        press(&mut app, &tx, now, &[ch('l'), ch(' '), ch('h')]);
        assert_eq!(app.view, View::Browse);
        assert_eq!(app.pane, Pane::Detail);
    }

    #[test]
    fn browse_narrow_has_no_zoom_path() {
        let (mut app, tx, now) = sized("browse-narrow", 50, 30);
        app.tick(ch('B'), now, &tx);
        press(&mut app, &tx, now, &[key(KeyCode::Enter), ch(' ')]);
        assert_eq!(
            app.view,
            View::Browse,
            "only History opens the zoom at w<60"
        );
    }

    #[test]
    fn q_is_text_in_search_mode() {
        let (mut app, tx, now) = sized("q-text", 100, 30);
        app.tick(ch('B'), now, &tx);
        press(&mut app, &tx, now, &[ch('/'), ch('q'), ch('H')]);
        assert!(!app.quit);
        assert_eq!(app.view, View::Browse, "view letters are inert in search");
        assert_eq!(app.browse.query, "qH");
    }

    #[test]
    fn fkeys_fire_in_any_mode_and_close_prompts() {
        let (mut app, tx, now) = sized("fkeys", 100, 30);
        app.tick(ch('B'), now, &tx);
        press(&mut app, &tx, now, &[ch('/'), ch('a')]);
        app.tick(key(KeyCode::F(2)), now, &tx);
        assert_eq!(app.view, View::History);
        assert_eq!(app.mode, InputMode::Normal);
    }

    #[test]
    fn h_l_toggle_panes_per_the_focus_table() {
        for view_key in ['B', 'H'] {
            let (mut app, tx, now) = sized("h-l", 100, 30);
            app.tick(ch(view_key), now, &tx);
            app.tick(ch('h'), now, &tx);
            assert_eq!(app.pane, Pane::List, "h at leftmost is a no-op");
            app.tick(ch('l'), now, &tx);
            assert_eq!(app.pane, Pane::Detail);
            app.tick(ch('l'), now, &tx);
            assert_eq!(app.pane, Pane::Detail, "l at rightmost is a no-op");
            app.tick(ch('h'), now, &tx);
            assert_eq!(app.pane, Pane::List);
        }
    }

    #[test]
    fn narrow_terminal_clamps_pane_to_list() {
        let (mut app, tx, now) = sized("clamp", 100, 30);
        app.tick(ch('B'), now, &tx);
        app.tick(ch('l'), now, &tx);
        assert_eq!(app.pane, Pane::Detail);
        app.tick(Event::Resize(50, 30), now, &tx);
        assert_eq!(app.pane, Pane::List, "resize below 60 clamps");
        app.tick(ch('l'), now, &tx);
        assert_eq!(app.pane, Pane::List, "l is silently consumed at w<60");
    }

    #[test]
    fn enter_steps_into_the_detail_pane() {
        let (mut app, tx, now) = sized("enter-pane", 100, 30);
        app.tick(ch('B'), now, &tx);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert_eq!(app.pane, Pane::Detail);
        assert_eq!(
            app.view,
            View::Browse,
            "Enter steps into the pane, not the zoom"
        );
    }

    #[test]
    fn switching_views_resets_pane_focus() {
        let (mut app, tx, now) = sized("switch-reset", 100, 30);
        app.tick(ch('B'), now, &tx);
        app.tick(ch('l'), now, &tx);
        app.tick(ch('H'), now, &tx);
        assert_eq!(app.pane, Pane::List);
        app.tick(ch('H'), now, &tx);
        assert_eq!(app.view, View::History, "same-view switch is a no-op");
    }

    #[test]
    fn discover_slash_jumps_to_browse_search() {
        let (mut app, tx, now) = sized("discover-slash", 100, 30);
        press(&mut app, &tx, now, &[ch('D'), ch('/')]);
        assert_eq!(app.view, View::Browse);
        assert_eq!(app.mode, InputMode::Search);
    }

    #[test]
    fn discover_axis_keys() {
        let (mut app, tx, now) = sized("axis", 100, 30);
        app.tick(ch('D'), now, &tx);
        app.tick(ch(']'), now, &tx);
        assert_eq!(app.discover.axis(), DiscoverAxis::Popular);
        press(&mut app, &tx, now, &[ch('['), ch('[')]);
        assert_eq!(app.discover.axis(), DiscoverAxis::ThisSeason, "cycle wraps");
        app.tick(ch('2'), now, &tx);
        assert_eq!(app.discover.axis(), DiscoverAxis::Popular);
        // Axis keys are Discover-only.
        app.tick(ch('B'), now, &tx);
        app.tick(ch(']'), now, &tx);
        assert_eq!(app.discover.axis(), DiscoverAxis::Popular);
    }

    #[test]
    fn entering_discover_fetches_and_lands_the_feed() {
        let (mut app, tx, rx, now) =
            harness_with("feed-e2e", StubCatalog::scripted(vec![one_page(3)]));
        app.tick(Event::Resize(100, 30), now, &tx);
        app.tick(ch('D'), now, &tx);
        app.tick(Event::Tick, now, &tx);
        settle_feed(&mut app, &tx, &rx, now);
        assert_eq!(app.discover.selected_entry().unwrap().anilist_id, 1);
        // Applied rows upsert catalog_cache (04 §10).
        assert!(app.store.get_catalog(1).unwrap().is_some());
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("Show 1"));
        assert!(text.contains("all entries loaded"));
    }

    #[test]
    fn discover_feed_error_renders_the_persistent_state() {
        let (mut app, tx, rx, now) = harness_with(
            "feed-error",
            StubCatalog::scripted(vec![Err(CatalogError::Network)]),
        );
        app.tick(Event::Resize(100, 30), now, &tx);
        app.tick(ch('D'), now, &tx);
        app.tick(Event::Tick, now, &tx);
        settle_feed(&mut app, &tx, &rx, now);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("can't reach the feed"));
        // A later tick must not auto-retry a failed axis (storm guard).
        app.tick(Event::Tick, now, &tx);
        assert!(app.discover.drain(Duration::from_secs(1)));
    }

    #[test]
    fn discover_p_saves_the_selected_card_as_planning() {
        let (mut app, tx, rx, now) =
            harness_with("p-save", StubCatalog::scripted(vec![one_page(2)]));
        app.tick(Event::Resize(100, 30), now, &tx);
        app.tick(ch('D'), now, &tx);
        app.tick(Event::Tick, now, &tx);
        settle_feed(&mut app, &tx, &rx, now);
        app.tick(ch('l'), now, &tx);
        app.tick(ch('P'), now, &tx);
        let show = app.store.get_show(2).unwrap().expect("saved to library");
        assert_eq!(show.list_status, crate::domain::ListStatus::Planning);
        assert!(show.library_added_at.is_some());
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("added to watchlist"));
    }

    #[test]
    fn discover_chip_tracks_the_selected_card() {
        use crate::domain::Season;
        let page = Ok(CatalogPage {
            entries: vec![
                Enrichment {
                    season: Some(Season::Fall),
                    year: Some(2024),
                    ..feed_entry(1)
                },
                feed_entry(2),
            ],
            has_next: false,
        });
        let (mut app, tx, rx, now) = harness_with("chip-card", StubCatalog::scripted(vec![page]));
        app.tick(Event::Resize(100, 30), now, &tx);
        app.tick(ch('D'), now, &tx);
        app.tick(Event::Tick, now, &tx);
        settle_feed(&mut app, &tx, &rx, now);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("秋") && text.contains("2024"));
        // Card 2 has no season data: the chip is absent, no cour fallback.
        app.tick(ch('l'), now, &tx);
        let text = rendered(&mut app, 100, 30);
        assert!(!text.contains("2024"));
    }

    #[test]
    fn typed_search_debounces_fetches_and_renders() {
        let (mut app, tx, rx, now) = harness_with(
            "search-e2e",
            StubCatalog::search_scripted(vec![one_page(3)]),
        );
        app.tick(Event::Resize(100, 30), now, &tx);
        press(&mut app, &tx, now, &[ch('B'), ch('/'), ch('f'), ch('r')]);
        app.tick(Event::Tick, now, &tx);
        assert_eq!(app.browse.count(), 0, "inside the debounce window");
        let later = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, later, &tx);
        settle_feed(&mut app, &tx, &rx, later);
        assert_eq!(app.browse.count(), 3);
        assert!(
            app.store.get_catalog(1).unwrap().is_some(),
            "results cached"
        );
        assert_eq!(
            app.detail.shown().map(|e| e.anilist_id),
            Some(1),
            "applied results push the selection into the shared detail"
        );
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("Show 1"));
        assert!(text.contains("[catalogue · 3]"));
    }

    #[test]
    fn search_outage_toasts_persistently_and_recovers() {
        let (mut app, tx, rx, now) = harness_with(
            "search-outage",
            StubCatalog::search_scripted(vec![Err(CatalogError::Network), one_page(1)]),
        );
        app.tick(Event::Resize(100, 30), now, &tx);
        press(&mut app, &tx, now, &[ch('B'), ch('/'), ch('a')]);
        let t1 = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        assert_eq!(app.toasts.iter().count(), 1, "persistent unreachable toast");
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("can't reach AniList"), "{text}");
        // Toast outlives the transient TTL while the outage holds.
        app.tick(Event::Tick, t1 + Duration::from_secs(10), &tx);
        assert_eq!(app.toasts.iter().count(), 1);
        // Next keystroke retries; success clears the toast (DESIGN 8.5).
        app.tick(ch('b'), t1, &tx);
        let t2 = t1 + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t2, &tx);
        settle_feed(&mut app, &tx, &rx, t2);
        assert!(app.toasts.is_empty(), "first success clears the topic");
        assert_eq!(app.browse.count(), 1);
    }

    #[test]
    fn browse_nav_pushes_the_shared_detail() {
        let (mut app, tx, rx, now) = harness_with(
            "browse-push",
            StubCatalog::search_scripted(vec![one_page(4)]),
        );
        app.tick(Event::Resize(100, 30), now, &tx);
        press(&mut app, &tx, now, &[ch('B'), ch('/'), ch('x')]);
        let t1 = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        app.tick(key(KeyCode::Enter), t1, &tx);
        press(&mut app, &tx, t1, &[ch('j'), ch('j')]);
        assert_eq!(app.detail.shown().map(|e| e.anilist_id), Some(3));
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("Show 3"), "detail pane renders the selection");
    }

    #[test]
    fn discover_enter_pushes_the_card_into_the_zoom() {
        let (mut app, tx, rx, now) =
            harness_with("zoom-push", StubCatalog::scripted(vec![one_page(2)]));
        app.tick(Event::Resize(100, 30), now, &tx);
        app.tick(ch('D'), now, &tx);
        app.tick(Event::Tick, now, &tx);
        settle_feed(&mut app, &tx, &rx, now);
        press(&mut app, &tx, now, &[ch('l'), key(KeyCode::Enter)]);
        assert_eq!(app.view, View::Detail);
        assert_eq!(app.detail.shown().map(|e| e.anilist_id), Some(2));
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("Show 2"));
    }

    #[test]
    fn empty_results_clear_the_detail_pane() {
        let empty = Ok(CatalogPage {
            entries: Vec::new(),
            has_next: false,
        });
        let (mut app, tx, rx, now) = harness_with(
            "empty-clears",
            StubCatalog::search_scripted(vec![one_page(1), empty]),
        );
        app.tick(Event::Resize(100, 30), now, &tx);
        press(&mut app, &tx, now, &[ch('B'), ch('/'), ch('a')]);
        let t1 = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        assert!(app.detail.shown().is_some());
        app.tick(ch('b'), t1, &tx);
        let t2 = t1 + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t2, &tx);
        settle_feed(&mut app, &tx, &rx, t2);
        assert!(app.detail.shown().is_none(), "no stale detail (DESIGN 8.4)");
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("no results for \"ab\""));
        assert!(text.contains("try a different spelling"));
    }

    #[test]
    fn browse_p_saves_the_highlighted_result() {
        let (mut app, tx, rx, now) =
            harness_with("browse-p", StubCatalog::search_scripted(vec![one_page(2)]));
        app.tick(Event::Resize(100, 30), now, &tx);
        press(&mut app, &tx, now, &[ch('B'), ch('/'), ch('a')]);
        let t1 = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        app.tick(key(KeyCode::Enter), t1, &tx);
        app.tick(ch('P'), t1, &tx);
        let show = app.store.get_show(1).unwrap().expect("saved to library");
        assert_eq!(show.list_status, crate::domain::ListStatus::Planning);
    }

    #[test]
    fn view_switch_repushes_browse_selection_over_a_stale_card() {
        let discover_page = Ok(CatalogPage {
            entries: (100..=101).map(feed_entry).collect(),
            has_next: false,
        });
        let (mut app, tx, rx, now) = harness_with(
            "repush",
            Arc::new(StubCatalog {
                discover: Mutex::new(vec![discover_page].into()),
                search: Mutex::new(vec![one_page(2)].into()),
            }),
        );
        app.tick(Event::Resize(100, 30), now, &tx);
        press(&mut app, &tx, now, &[ch('B'), ch('/'), ch('a')]);
        let t1 = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        app.tick(key(KeyCode::Esc), t1, &tx);
        // Visit Discover, open a card in the zoom, come back to Browse.
        app.tick(ch('D'), t1, &tx);
        app.tick(Event::Tick, t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        app.tick(key(KeyCode::Enter), t1, &tx);
        assert_eq!(app.detail.shown().map(|e| e.anilist_id), Some(100));
        app.tick(key(KeyCode::Esc), t1, &tx);
        app.tick(ch('B'), t1, &tx);
        assert_eq!(
            app.detail.shown().map(|e| e.anilist_id),
            Some(1),
            "the shared surface shows Browse's own selection again"
        );
    }

    #[test]
    fn search_survives_lock_and_reopens_prefilled() {
        let (mut app, tx, now) = sized("search-lock", 100, 30);
        app.tick(ch('B'), now, &tx);
        press(
            &mut app,
            &tx,
            now,
            &[ch('/'), ch('f'), ch('r'), key(KeyCode::Enter)],
        );
        assert_eq!(app.mode, InputMode::Normal);
        assert_eq!(app.pane, Pane::List);
        assert_eq!(app.browse.query, "fr", "Enter locks, keeps the query");
        app.tick(ch('/'), now, &tx);
        assert_eq!(app.browse.query, "fr", "reopen pre-fills");
    }

    #[test]
    fn history_filter_is_a_separate_buffer() {
        let (mut app, tx, now) = sized("filter-buffer", 100, 30);
        press(&mut app, &tx, now, &[ch('/'), ch('z'), key(KeyCode::Esc)]);
        assert!(app.history.filter.is_empty());
        assert!(app.browse.query.is_empty());
        press(&mut app, &tx, now, &[ch('/'), ch('z')]);
        assert_eq!(app.history.filter, "z");
        assert!(app.browse.query.is_empty());
    }

    #[test]
    fn command_q_quits() {
        let (mut app, tx, now) = sized("cmd-q", 100, 30);
        press(&mut app, &tx, now, &[ch(':'), ch('q'), key(KeyCode::Enter)]);
        assert!(app.quit);
    }

    #[test]
    fn command_dub_toggles_translation() {
        let (mut app, tx, now) = sized("cmd-dub", 100, 30);
        assert_eq!(app.config.translation, "sub");
        press(
            &mut app,
            &tx,
            now,
            &[ch(':'), ch('d'), ch('u'), ch('b'), key(KeyCode::Enter)],
        );
        assert_eq!(app.config.translation, "dub");
        press(
            &mut app,
            &tx,
            now,
            &[ch(':'), ch('d'), ch('u'), ch('b'), key(KeyCode::Enter)],
        );
        assert_eq!(app.config.translation, "sub");
    }

    #[test]
    fn unknown_command_flashes_and_toasts() {
        let (mut app, tx, now) = sized("cmd-unknown", 100, 30);
        press(
            &mut app,
            &tx,
            now,
            &[ch(':'), ch('x'), ch('y'), key(KeyCode::Enter)],
        );
        assert_eq!(app.mode, InputMode::Normal);
        assert!(app.command_flash.is_some());
        assert_eq!(app.toasts.iter().count(), 1);
        assert!(matches!(app.bottom_bar(now), BottomBar::CommandError));
        let later = now + COMMAND_FLASH;
        app.tick(Event::Tick, later, &tx);
        assert!(app.command_flash.is_none(), "tick clears the flash");
        assert!(matches!(app.bottom_bar(later), BottomBar::Help(_)));
    }

    /// Library rows seeded through the store, then reloaded the way the app
    /// itself loads.
    fn seed_history(app: &mut App, shows: &[(i64, &str, crate::domain::ListStatus)]) {
        for (aid, title, status) in shows {
            let e = Enrichment {
                anilist_id: *aid,
                title_romaji: title.to_string(),
                total_episodes: Some(12),
                ..Enrichment::default()
            };
            app.store.add_to_library(&e, 100).unwrap();
            app.store.set_list_status(*aid, *status, 100).unwrap();
        }
        app.history.load(&app.store);
    }

    #[test]
    fn help_line_tracks_view_pane_and_empty_history() {
        let (mut app, tx, now) = sized("help-line", 100, 30);
        assert_eq!(app.help_line(), HelpLine::HistoryEmpty);
        seed_history(
            &mut app,
            &[(1, "Show 1", crate::domain::ListStatus::Watching)],
        );
        assert_eq!(app.help_line(), HelpLine::HistoryList);
        app.tick(ch('l'), now, &tx);
        assert_eq!(app.help_line(), HelpLine::HistoryDetail);
        app.tick(ch('B'), now, &tx);
        assert_eq!(app.help_line(), HelpLine::BrowseList);
        press(&mut app, &tx, now, &[ch('l'), ch(' ')]);
        assert_eq!(app.help_line(), HelpLine::Zoom);
    }

    #[test]
    fn first_run_screens_render_their_absent_states() {
        let (mut app, _tx, _now) = sized("first-run", 100, 30);
        let history = rendered(&mut app, 100, 30);
        assert!(history.contains("nothing watched yet"));
        assert!(history.contains("see what's popular"));
        assert!(history.contains("D discover"), "empty-history help line");
        let (mut app, tx, now) = sized("first-run-browse", 100, 30);
        app.tick(ch('B'), now, &tx);
        let browse = rendered(&mut app, 100, 30);
        assert!(browse.contains("search the catalogue"));
        assert!(browse.contains("find anime"));
    }

    #[test]
    fn top_bar_degrades_with_width() {
        let (mut app, _tx, _now) = sized("top-full", 100, 30);
        let full = rendered(&mut app, 100, 30);
        assert!(full.contains("[B]rowse"));
        assert!(full.contains("[H]istory"));
        let (mut app, _tx, _now) = sized("top-brief", 50, 30);
        let brief = rendered(&mut app, 50, 30);
        assert!(brief.contains("[H]"));
        assert!(!brief.contains("[H]istory"));
        let (mut app, _tx, _now) = sized("top-single", 30, 30);
        let single = rendered(&mut app, 30, 30);
        assert!(single.contains("[H]istory"), "single active label survives");
        assert!(!single.contains("[B]"));
    }

    #[test]
    fn season_chip_shows_the_current_cour_on_history() {
        let (mut app, _tx, _now) = sized("chip", 100, 30);
        let text = rendered(&mut app, 100, 30);
        // The kanji is double-width, so the buffer carries a filler cell
        // between it and the year; assert the parts, not the joined string.
        let cour = render::cour_chip(domain::current_cour(unix_now()));
        let (kanji, year) = cour.split_once(' ').unwrap();
        assert!(text.contains(kanji), "missing season kanji");
        assert!(text.contains(year), "missing season year");
    }

    #[test]
    fn search_bar_carries_the_scope_tag() {
        let (mut app, tx, now) = sized("scope-tag", 100, 30);
        app.tick(ch('/'), now, &tx);
        let history = rendered(&mut app, 100, 30);
        assert!(history.contains("[history · 0]"));
        press(&mut app, &tx, now, &[key(KeyCode::Esc), ch('B'), ch('/')]);
        let browse = rendered(&mut app, 100, 30);
        assert!(browse.contains("[catalogue · 0]"));
    }

    #[test]
    fn toasts_overlay_the_frame() {
        let (mut app, tx, now) = sized("toast-overlay", 100, 30);
        press(&mut app, &tx, now, &[ch(':'), ch('z'), key(KeyCode::Enter)]);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("unknown command"));
    }

    #[test]
    fn draw_is_total_across_views_and_sizes() {
        for view in [
            View::Browse,
            View::History,
            View::Detail,
            View::Discover,
            View::Settings,
        ] {
            for (w, h) in [
                (100, 30),
                (60, 20),
                (59, 20),
                (40, 10),
                (16, 4),
                (15, 3),
                (2, 1),
            ] {
                let (mut app, tx, now) = harness("draw-total");
                app.view = view;
                app.tick(Event::Resize(w, h), now, &tx);
                rendered(&mut app, w, h);
            }
        }
    }

    #[test]
    fn tiny_terminal_renders_the_degraded_frame() {
        let (mut app, _tx, _now) = sized("tiny", 15, 3);
        let text = rendered(&mut app, 20, 3);
        assert!(text.contains("terminal too small"));
    }

    /// Lands one Browse result set, locks the search, and steps into the pane
    /// (the second Enter engages the episode session).
    fn open_first_result(
        app: &mut App,
        tx: &EventTx,
        rx: &super::super::event::EventRx,
        now: Instant,
    ) -> Instant {
        app.tick(Event::Resize(100, 30), now, tx);
        press(app, tx, now, &[ch('B'), ch('/'), ch('a')]);
        let t1 = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t1, tx);
        settle_feed(app, tx, rx, t1);
        assert!(
            !app.detail.episodes.engaged_for(1),
            "list scroll/selection never fetches episodes (05 §10.1)"
        );
        app.tick(key(KeyCode::Enter), t1, tx); // locks the search
        app.tick(key(KeyCode::Enter), t1, tx); // enters the pane, engages
        settle_feed(app, tx, rx, t1);
        t1
    }

    #[test]
    fn detail_entry_resolves_renders_and_navigates_the_grid() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "grid-e2e",
            StubCatalog::search_scripted(vec![one_page(2)]),
            registry,
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        assert_eq!(app.detail.episodes.grid(), ["1", "2", "3"]);
        assert_eq!(app.detail.episodes.serving(), Some("megaplay"));
        assert_eq!(app.store.bindings_for(1).unwrap().len(), 1, "tier-A mint");
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("[1]") && text.contains("[3]"), "{text}");

        // j/k step episodes linearly; g/G jump ends (freeze parity).
        press(&mut app, &tx, t1, &[ch('j'), ch('j'), ch('j')]);
        assert_eq!(app.detail.episodes.cursor(), 2, "clamped at the end");
        app.tick(ch('g'), t1, &tx);
        assert_eq!(app.detail.episodes.cursor(), 0);
        app.tick(ch('G'), t1, &tx);
        assert_eq!(app.detail.episodes.cursor(), 2);

        // Back on the list, another selection: the stale grid never draws.
        app.tick(key(KeyCode::Esc), t1, &tx);
        app.tick(ch('j'), t1, &tx);
        let text = rendered(&mut app, 100, 30);
        assert!(!text.contains("[1]"), "another show must not show the grid");
    }

    #[test]
    fn episode_failure_toasts_the_class_copy_and_hops() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Err(crate::providers::ProviderError::Server { status: 503 })),
            teststub::StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "hop-e2e",
            StubCatalog::search_scripted(vec![one_page(1)]),
            registry,
        );
        open_first_result(&mut app, &tx, &rx, now);
        assert_eq!(app.detail.episodes.serving(), Some("senshi"));
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("megaplay is down"), "{text}");
        assert!(text.contains("trying senshi…"), "{text}");
    }

    #[test]
    fn v_cycles_the_pin_with_toasts_and_flip() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into()])),
            teststub::StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "pin-e2e",
            StubCatalog::search_scripted(vec![one_page(1)]),
            registry,
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        assert_eq!(app.detail.episodes.serving(), Some("megaplay"));

        app.tick(ch('v'), t1, &tx);
        let text = rendered(&mut app, 110, 32);
        assert!(text.contains("pinned to megaplay"), "{text}");
        assert_eq!(
            app.store.get_provider_pin(1).unwrap().as_deref(),
            Some("megaplay")
        );

        app.tick(ch('v'), t1, &tx);
        let text = rendered(&mut app, 110, 32);
        assert!(text.contains("trying senshi…"), "{text}");
        settle_feed(&mut app, &tx, &rx, t1);
        assert_eq!(app.detail.episodes.serving(), Some("senshi"));

        app.tick(ch('v'), t1, &tx);
        let text = rendered(&mut app, 110, 32);
        assert!(text.contains("provider pin cleared"), "{text}");
        assert_eq!(app.store.get_provider_pin(1).unwrap(), None);

        // v is a detail-surface key; on the list it must stay inert.
        app.tick(key(KeyCode::Esc), t1, &tx);
        let before = app.store.get_provider_pin(1).unwrap();
        app.tick(ch('v'), t1, &tx);
        assert_eq!(app.store.get_provider_pin(1).unwrap(), before);
    }

    /// Pins the USER-VISIBLE copy for the two pin-walk failure rows, which
    /// are distinct events (DESIGN 4.10): ran-and-missed vs could-not-run.
    #[test]
    fn pin_flip_miss_toasts_the_pin_kept_copy() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into()])),
            teststub::StubProvider::new("senshi"),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "pinkept-e2e",
            StubCatalog::search_scripted(vec![one_page(1)]),
            registry,
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        // v pins serving megaplay; v again flips to senshi, whose search
        // fails and misses.
        press(&mut app, &tx, t1, &[ch('v'), ch('v')]);
        settle_feed(&mut app, &tx, &rx, t1);
        let text = rendered(&mut app, 110, 32);
        assert!(text.contains("no match on senshi, pin kept"), "{text}");
        assert_eq!(
            app.store.get_provider_pin(1).unwrap().as_deref(),
            Some("senshi"),
            "the miss keeps the pin"
        );

        // The could-not-run sibling maps to its own distinct copy.
        app.apply_episode_feedback(
            vec![Feedback::PinUnreachable {
                provider: "senshi".into(),
            }],
            t1,
        );
        let text = rendered(&mut app, 110, 32);
        assert!(text.contains("couldn't reach senshi"), "{text}");
    }

    // ── play (chunk 6) ──────────────────────────────────────────────────

    use crate::player::Position as PlayPos;
    use crate::tui::event::PlayFailure;

    /// Grid of three landed off megaplay; the stub resolve answers
    /// Unsupported, so a fired play settles fast and mpv never spawns.
    fn play_harness(name: &str) -> (App, EventTx, super::super::event::EventRx, Instant) {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
        ]);
        harness_full(
            name,
            StubCatalog::search_scripted(vec![one_page(1)]),
            registry,
        )
    }

    /// Settle the play worker WITHOUT applying its own finish event, so a
    /// test can inject its outcome against the live token.
    fn discard_worker_finish(app: &mut App, rx: &super::super::event::EventRx) {
        assert!(app.playback.drain(Duration::from_secs(5)));
        while rx.try_recv().is_ok() {}
    }

    fn completed_pos() -> Option<PlayPos> {
        Some(PlayPos {
            secs: 1200.0,
            duration: Some(1400.0),
        })
    }

    #[test]
    fn enter_on_the_grid_fires_play_with_launching_cell_and_guard() {
        let (mut app, tx, rx, now) = play_harness("play-fire");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        assert!(!app.playback.is_playing());
        app.tick(key(KeyCode::Enter), t1, &tx);
        assert!(app.playback.is_playing());
        let token = app.playback.active_token().unwrap();
        // §4.6 launching cell: the played cell spins in place of its number.
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("[⠋]"), "{text}");
        assert!(!text.contains("[1]"), "the spinner replaces the number");
        // Double-play guard: a second Enter is a silent no-op.
        app.tick(key(KeyCode::Enter), t1, &tx);
        assert_eq!(app.playback.active_token(), Some(token));
        // First position ends the launch; the grid returns to normal cells.
        app.tick(
            Event::PlayPosition {
                anilist_id: 1,
                position: PlayPos {
                    secs: 0.0,
                    duration: None,
                },
                token,
            },
            t1,
            &tx,
        );
        let text = rendered(&mut app, 100, 30);
        assert!(!text.contains("[⠋]"), "{text}");
        assert!(text.contains("[1]"), "{text}");
    }

    #[test]
    fn zoom_enter_fires_play_too() {
        let (mut app, tx, rx, now) = play_harness("play-zoom");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(ch(' '), t1, &tx);
        assert_eq!(app.view, View::Detail);
        app.tick(key(KeyCode::Enter), t1, &tx);
        assert!(app.playback.is_playing());
    }

    #[test]
    fn play_pipeline_end_to_end_toasts_the_residual_failure() {
        // The stub resolve answers Unsupported: a dead play, never silent
        // (unlike the walk's silent Unsupported).
        let (mut app, tx, rx, now) = play_harness("play-resolve-fail");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        assert!(app.playback.drain(Duration::from_secs(5)));
        while let Ok(ev) = rx.try_recv() {
            app.tick(ev, t1, &tx);
        }
        assert!(!app.playback.is_playing(), "the failed play cleared");
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("playback failed"), "{text}");
        assert!(app.store.list_history().unwrap().is_empty(), "no writes");
    }

    #[test]
    fn completed_finish_records_advances_and_reloads_history() {
        let (mut app, tx, rx, now) = play_harness("play-complete");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        let token = app.playback.active_token().unwrap();
        discard_worker_finish(&mut app, &rx);
        assert!(app.history.is_empty());
        app.tick(
            Event::PlayFinished {
                anilist_id: 1,
                position: completed_pos(),
                failure: None,
                token,
            },
            t1,
            &tx,
        );
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("episode 1 done"), "{text}");
        assert_eq!(app.detail.episodes.watched(), 1, "high-water re-derived");
        assert_eq!(app.detail.episodes.cursor(), 1, "cursor advanced");
        assert!(!app.history.is_empty(), "history reloaded after the play");
        assert_eq!(app.store.list_history().unwrap()[0].progress, 1);
    }

    #[test]
    fn finale_finish_toasts_all_caught_up() {
        let (mut app, tx, rx, now) = play_harness("play-finale");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(ch('G'), t1, &tx);
        app.tick(key(KeyCode::Enter), t1, &tx);
        let token = app.playback.active_token().unwrap();
        discard_worker_finish(&mut app, &rx);
        app.tick(
            Event::PlayFinished {
                anilist_id: 1,
                position: completed_pos(),
                failure: None,
                token,
            },
            t1,
            &tx,
        );
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("all caught up"), "{text}");
    }

    #[test]
    fn partial_finish_records_silently_and_marks_resume() {
        let (mut app, tx, rx, now) = play_harness("play-partial");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        let token = app.playback.active_token().unwrap();
        discard_worker_finish(&mut app, &rx);
        app.tick(
            Event::PlayFinished {
                anilist_id: 1,
                position: Some(PlayPos {
                    secs: 300.0,
                    duration: Some(1400.0),
                }),
                failure: None,
                token,
            },
            t1,
            &tx,
        );
        let text = rendered(&mut app, 100, 30);
        assert!(!text.contains("done") && !text.contains("failed"), "{text}");
        assert_eq!(app.detail.episodes.resume_ix(), Some(0), "resume marked");
        assert_eq!(app.detail.episodes.cursor(), 0, "partial holds the cursor");
        assert_eq!(app.store.list_history().unwrap()[0].play_count, 1);
    }

    #[test]
    fn cross_show_finish_never_touches_the_new_detail() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "play-cross",
            StubCatalog::search_scripted(vec![one_page(2)]),
            registry,
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        let token = app.playback.active_token().unwrap();
        discard_worker_finish(&mut app, &rx);
        // Navigate away: the next selection resets the session (ROD-329).
        app.tick(key(KeyCode::Esc), t1, &tx);
        app.tick(ch('j'), t1, &tx);
        app.tick(
            Event::PlayFinished {
                anilist_id: 1,
                position: completed_pos(),
                failure: None,
                token,
            },
            t1,
            &tx,
        );
        assert!(
            !app.detail.episodes.is_for(2) || !app.detail.episodes.has_grid(),
            "show 2 never inherits show 1's play"
        );
        assert_eq!(
            app.store.list_history().unwrap()[0].progress,
            1,
            "the record still lands for show 1"
        );
    }

    /// Settle every worker family INCLUDING playback, applying events as
    /// they land so hop chains (fail → walk → land → relaunch) run to rest.
    fn settle_play(app: &mut App, tx: &EventTx, rx: &super::super::event::EventRx, now: Instant) {
        loop {
            assert!(app.detail.drain(Duration::from_secs(5)));
            assert!(app.playback.drain(Duration::from_secs(5)));
            let Ok(ev) = rx.try_recv() else { break };
            app.tick(ev, now, tx);
        }
    }

    #[test]
    fn failed_play_hops_relaunches_and_dead_ends_without_ping_pong() {
        // Both providers list episodes; every resolve fails (stub answers
        // Unsupported). The chain must be: play megaplay → fail → hop →
        // senshi grid lands → auto-relaunch → fail → dead end. One shot per
        // provider: no ping-pong back to megaplay.
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
            teststub::StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "play-hop",
            StubCatalog::search_scripted(vec![one_page(1)]),
            registry,
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        assert_eq!(app.detail.episodes.serving(), Some("megaplay"));
        app.tick(ch('j'), t1, &tx);
        app.tick(key(KeyCode::Enter), t1, &tx);
        settle_play(&mut app, &tx, &rx, t1);

        assert_eq!(
            app.detail.episodes.serving(),
            Some("senshi"),
            "the walk landed the sibling grid"
        );
        assert_eq!(
            app.detail.episodes.cursor(),
            1,
            "hop landing kept the cursor on the in-progress episode"
        );
        assert!(!app.playback.is_playing(), "the relaunch also failed");
        assert!(
            app.playback.continuation().is_none(),
            "dead end retires the continuation"
        );
        let text = rendered(&mut app, 110, 32);
        assert!(text.contains("trying senshi…"), "{text}");
        assert!(text.contains("playback failed"), "{text}");
        assert!(app.store.list_history().unwrap().is_empty(), "no writes");
    }

    #[test]
    fn player_side_failure_never_hops() {
        let (mut app, tx, rx, now) = play_harness("play-nohop");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        let token = app.playback.active_token().unwrap();
        discard_worker_finish(&mut app, &rx);
        app.tick(
            Event::PlayFinished {
                anilist_id: 1,
                position: None,
                failure: Some(PlayFailure::MpvNotFound),
                token,
            },
            t1,
            &tx,
        );
        settle_play(&mut app, &tx, &rx, t1);
        assert_eq!(
            app.detail.episodes.serving(),
            Some("megaplay"),
            "no walk fired for a player-side failure"
        );
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("mpv not found · install mpv"), "{text}");
    }

    #[test]
    fn continuation_remap_miss_toasts_and_stops() {
        // The sibling grid has different labels AND fewer episodes than the
        // played ordinal, so both remap tiers miss (03 §6.6).
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
            teststub::StubProvider::new("senshi")
                .with_key("505")
                .with_episodes(Ok(vec!["A".into(), "B".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "play-remapmiss",
            StubCatalog::search_scripted(vec![one_page(1)]),
            registry,
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(ch('G'), t1, &tx); // episode 3
        app.tick(key(KeyCode::Enter), t1, &tx);
        settle_play(&mut app, &tx, &rx, t1);

        assert_eq!(app.detail.episodes.serving(), Some("senshi"));
        assert!(!app.playback.is_playing(), "no relaunch on a remap miss");
        assert!(app.playback.continuation().is_none());
        let text = rendered(&mut app, 110, 32);
        assert!(text.contains("episode 3 not found on senshi"), "{text}");
    }

    #[test]
    fn delete_refuses_the_currently_playing_show() {
        let (mut app, tx, rx, now) = play_harness("play-delrefuse");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        let token = app.playback.active_token().unwrap();
        discard_worker_finish(&mut app, &rx);
        // A partial record puts the show in History while mpv still runs.
        app.store
            .save_progress(
                1,
                Translation::Sub,
                "1",
                30.0,
                1400.0,
                Some("megaplay"),
                100,
            )
            .unwrap();
        app.store.record_play(1, 1, false, 100).unwrap();
        app.tick(ch('H'), t1, &tx);
        press(&mut app, &tx, t1, &[ch('X'), ch('y')]);
        assert!(
            !app.store.list_history().unwrap().is_empty(),
            "the playing show survives"
        );
        assert!(app.confirm_delete.is_none(), "refusal still disarms");
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("can't delete, currently playing"), "{text}");

        // Once the play finishes, the same delete goes through.
        app.tick(
            Event::PlayFinished {
                anilist_id: 1,
                position: None,
                failure: None,
                token,
            },
            t1,
            &tx,
        );
        press(&mut app, &tx, t1, &[ch('X'), ch('y')]);
        assert!(app.store.list_history().unwrap().is_empty());
    }

    // ── Settings (chunk 7) ──────────────────────────────────────────────

    #[test]
    fn settings_renders_the_5_5_anatomy() {
        let (mut app, tx, now) = sized("settings-render", 100, 32);
        app.tick(ch('S'), now, &tx);
        let text = rendered(&mut app, 100, 32);
        for needle in [
            "Player",
            "Catalog",
            "Interface",
            "AniList Sync",
            "mpv path",
            "enter to edit",
            "megaplay (default)",
            "[████ on ████]",
            "metadata refresh",
            "automatic",
            "not connected",
            "enter to connect",
            "5s",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text}");
        }
    }

    #[test]
    fn settings_cycle_dirties_and_leaving_persists_with_toast() {
        let (mut app, tx, now) = sized("settings-persist", 100, 32);
        std::fs::create_dir_all(app.config_file.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&app.config_file);
        app.tick(ch('S'), now, &tx);
        press(&mut app, &tx, now, &[ch('j'), ch('l')]);
        assert_eq!(app.config.default_quality, "worst");
        assert!(app.settings.dirty);
        app.tick(ch('B'), now, &tx);
        assert_eq!(app.view, View::Browse, "leave persisted, then switched");
        assert!(!app.settings.dirty);
        let text = rendered(&mut app, 100, 32);
        assert!(text.contains("settings saved"), "{text}");
        let saved = Config::load(&app.config_file);
        assert_eq!(saved.default_quality, "worst");
        std::fs::remove_file(&app.config_file).ok();
    }

    #[test]
    fn settings_missing_config_dir_warns_and_skips() {
        let (mut app, tx, now) = sized("settings-nodir", 100, 32);
        app.config_file = std::path::PathBuf::from("/nonexistent-sabigoku-dir/config.toml");
        app.tick(ch('S'), now, &tx);
        press(&mut app, &tx, now, &[ch('j'), ch('l'), ch('B')]);
        let text = rendered(&mut app, 100, 32);
        assert!(text.contains("no config dir · not saved"), "{text}");
    }

    #[test]
    fn q_in_settings_persists_a_dirty_tab_then_quits() {
        let (mut app, tx, now) = sized("settings-quit", 100, 32);
        std::fs::create_dir_all(app.config_file.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&app.config_file);
        app.tick(ch('S'), now, &tx);
        press(
            &mut app,
            &tx,
            now,
            &[ch('j'), ch('j'), ch('j'), ch('l'), ch('q')],
        );
        assert!(app.quit);
        assert_eq!(Config::load(&app.config_file).resume_offset_sec, 10);
        std::fs::remove_file(&app.config_file).ok();
    }

    #[test]
    fn palette_cycle_projects_live() {
        let (mut app, tx, now) = sized("settings-palette", 100, 32);
        app.tick(ch('S'), now, &tx);
        // Down to the palette row (index 8), cycle once.
        for _ in 0..8 {
            app.tick(ch('j'), now, &tx);
        }
        app.tick(ch('l'), now, &tx);
        assert_eq!(app.config.palette, "phosphor");
        assert_eq!(app.palette.name, "phosphor", "repaints on the next frame");
    }

    #[test]
    fn translation_cycle_rekeys_an_engaged_grid() {
        let (mut app, tx, rx, now) = play_harness("settings-trans");
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        assert!(app.detail.episodes.has_grid());
        app.tick(ch('S'), t1, &tx);
        press(&mut app, &tx, t1, &[ch('j'), ch('j'), ch('l')]);
        assert_eq!(app.config.translation, "dub");
        assert!(
            !app.detail.episodes.has_grid(),
            "the sub grid must not survive a track flip (ROD-329)"
        );
        settle_feed(&mut app, &tx, &rx, t1);
    }

    #[test]
    fn settings_edit_mode_swallows_view_keys_and_esc_stays() {
        let (mut app, tx, now) = sized("settings-edit", 100, 32);
        app.tick(ch('S'), now, &tx);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert!(app.settings.editing());
        // View letters and F-keys are text / swallowed while editing.
        press(&mut app, &tx, now, &[ch('B'), key(KeyCode::F(1))]);
        assert_eq!(app.view, View::Settings);
        let text = rendered(&mut app, 100, 32);
        assert!(text.contains("type value"), "edit help line shows");
        app.tick(key(KeyCode::Enter), now, &tx);
        assert_eq!(app.config.mpv_path, "mpvB", "B was text, F1 dropped");
        // Esc outside edit mode never leaves Settings (DESIGN 7.4).
        app.tick(key(KeyCode::Esc), now, &tx);
        assert_eq!(app.view, View::Settings);
        // The letter routes again once the edit is over.
        app.tick(ch('B'), now, &tx);
        assert_eq!(app.view, View::Browse);
    }

    #[test]
    fn play_toast_copy_covers_the_matrix_rows() {
        let (mut app, _tx, now) = harness("play-copy");
        let cases: Vec<(PlayFeedback, &str)> = vec![
            (
                PlayFeedback::Retry { attempt: 2 },
                "stream didn't open · retrying 2/3",
            ),
            (
                PlayFeedback::Failed {
                    provider: "megaplay".into(),
                    failure: PlayFailure::MpvNotFound,
                },
                "mpv not found · install mpv",
            ),
            (
                PlayFeedback::Failed {
                    provider: "megaplay".into(),
                    failure: PlayFailure::MpvFailed,
                },
                "mpv exited with error",
            ),
            (
                PlayFeedback::Failed {
                    provider: "megaplay".into(),
                    failure: PlayFailure::OpenFailed,
                },
                "stream didn't open · try again",
            ),
            (
                PlayFeedback::Failed {
                    provider: "megaplay".into(),
                    failure: PlayFailure::Resolve(FetchClass::Blocked),
                },
                "megaplay blocked us",
            ),
            (
                PlayFeedback::Failed {
                    provider: "megaplay".into(),
                    failure: PlayFailure::Resolve(FetchClass::Data),
                },
                "playback failed",
            ),
            (PlayFeedback::SaveFailed, "couldn't save progress"),
        ];
        for (feedback, copy) in cases {
            app.apply_play_feedback(vec![feedback], now);
            let text = rendered(&mut app, 100, 30);
            assert!(text.contains(copy), "expected {copy:?} in {text}");
            // Fresh slate: the queue caps at 3 and evicts oldest-first.
            app.toasts = Toasts::default();
        }
    }

    #[test]
    fn zoom_metadata_stays_compact_at_every_origin() {
        let page = Ok(CatalogPage {
            entries: vec![Enrichment {
                kind: Some("TV".into()),
                total_episodes: Some(12),
                duration_minutes: Some(24),
                studios: vec!["Madhouse".into()],
                ..feed_entry(1)
            }],
            has_next: false,
        });
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "rail-e2e",
            StubCatalog::search_scripted(vec![page]),
            registry,
        );
        app.tick(Event::Resize(110, 32), now, &tx);
        press(&mut app, &tx, now, &[ch('B'), ch('/'), ch('a')]);
        let t1 = now + browse::SEARCH_DEBOUNCE;
        app.tick(Event::Tick, t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        app.tick(key(KeyCode::Enter), t1, &tx);
        app.tick(key(KeyCode::Enter), t1, &tx);
        settle_feed(&mut app, &tx, &rx, t1);
        app.tick(ch(' '), t1, &tx);
        assert_eq!(app.view, View::Detail);
        assert_eq!(app.origin, Origin::Browse);

        // Compact metadata line, no label→value rail, provider row with the
        // grid. Origin must not change any of that (the §5.3a rail is gone).
        let browse_zoom = rendered(&mut app, 110, 32);
        assert!(browse_zoom.contains("12 eps · TV"), "{browse_zoom}");
        assert!(browse_zoom.contains("▸megaplay"), "provider rides the grid");
        assert!(
            !browse_zoom.contains("Duration") && !browse_zoom.contains("Episodes"),
            "no rail labels: {browse_zoom}"
        );

        app.origin = Origin::History;
        app.dirty = true;
        let history_zoom = rendered(&mut app, 110, 32);
        assert!(history_zoom.contains("12 eps · TV"), "{history_zoom}");
        assert!(history_zoom.contains("▸megaplay"), "{history_zoom}");
        assert!(
            !history_zoom.contains("Duration") && !history_zoom.contains("Episodes"),
            "History origin no longer blooms a rail: {history_zoom}"
        );
    }

    #[test]
    fn history_renders_groups_bars_and_detail_preview() {
        use crate::domain::ListStatus;
        let (mut app, tx, now) = sized("history-groups", 100, 30);
        seed_history(
            &mut app,
            &[
                (1, "Alpha", ListStatus::Watching),
                (2, "Beta", ListStatus::Completed),
                (3, "Gamma", ListStatus::Watching),
            ],
        );
        app.tick(ch('H'), now, &tx);
        app.tick(ch('B'), now, &tx);
        app.tick(ch('H'), now, &tx);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("watching (2)"), "{text}");
        assert!(text.contains("complete (1)"), "{text}");
        assert!(text.contains("eps"), "bars carry the fraction");
        assert!(text.contains("█"), "completed rows fill the bar");
        assert_eq!(
            app.detail.shown().map(|e| e.anilist_id),
            Some(1),
            "entering History pushes the focused record into the preview"
        );
        // Cursor motion follows group order (watching first), pushing along.
        app.tick(ch('j'), now, &tx);
        assert_eq!(app.detail.shown().map(|e| e.anilist_id), Some(3));
        app.tick(ch('j'), now, &tx);
        assert_eq!(
            app.detail.shown().map(|e| e.anilist_id),
            Some(2),
            "group order, not store order"
        );
    }

    #[test]
    fn history_pane_entry_engages_and_list_focus_hides_the_grid() {
        use crate::domain::ListStatus;
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into()])),
        ]);
        let (mut app, tx, rx, now) = harness_full("history-grid", StubCatalog::inert(), registry);
        app.tick(Event::Resize(100, 30), now, &tx);
        seed_history(&mut app, &[(7, "Alpha", ListStatus::Watching)]);
        app.tick(ch('H'), now, &tx);
        app.tick(ch('B'), now, &tx);
        app.tick(ch('H'), now, &tx);
        assert!(
            !app.detail.episodes.engaged_for(7),
            "list focus never fetches (05 §10.1)"
        );
        app.tick(key(KeyCode::Enter), now, &tx);
        assert_eq!(app.pane, Pane::Detail);
        settle_feed(&mut app, &tx, &rx, now);
        assert_eq!(app.detail.episodes.grid(), ["1", "2"]);
        let text = rendered(&mut app, 100, 30);
        assert!(
            text.contains("[1]"),
            "focused pane renders the grid: {text}"
        );
        // Back on the list: the pane is a synopsis preview, never a grid
        // (DESIGN 5.4a, ROD-222).
        app.tick(key(KeyCode::Esc), now, &tx);
        let text = rendered(&mut app, 100, 30);
        assert!(!text.contains("[1]"), "unfocused pane hides the grid");
    }

    #[test]
    fn history_filter_narrows_counts_and_esc_restores() {
        use crate::domain::ListStatus;
        let (mut app, tx, now) = sized("history-filter", 100, 30);
        seed_history(
            &mut app,
            &[
                (1, "Frieren", ListStatus::Watching),
                (2, "Vinland", ListStatus::Watching),
            ],
        );
        press(&mut app, &tx, now, &[ch('/'), ch('v'), ch('i'), ch('n')]);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("[history · 1]"), "{text}");
        assert!(text.contains("Vinland") && !text.contains("Frieren"));
        app.tick(key(KeyCode::Esc), now, &tx);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("Frieren"), "esc restores the full list");
        assert!(app.history.filter.is_empty());
    }

    #[test]
    fn narrow_history_enter_opens_the_zoom_with_the_selection() {
        use crate::domain::ListStatus;
        let (mut app, tx, now) = sized("history-narrow", 50, 30);
        seed_history(&mut app, &[(9, "Alpha", ListStatus::Watching)]);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert_eq!(app.view, View::Detail);
        assert_eq!(app.origin, Origin::History);
        assert_eq!(
            app.detail.shown().map(|e| e.anilist_id),
            Some(9),
            "the zoom opens on the focused record"
        );
    }

    #[test]
    fn history_status_keys_transition_store_and_memory_with_undo() {
        use crate::domain::ListStatus;
        let (mut app, tx, now) = sized("history-status", 100, 30);
        seed_history(&mut app, &[(1, "Alpha", ListStatus::Watching)]);
        let status = |app: &App| app.store.get_show(1).unwrap().unwrap().list_status;

        app.tick(ch('c'), now, &tx);
        assert_eq!(status(&app), ListStatus::Completed);
        assert_eq!(
            app.store.get_show(1).unwrap().unwrap().progress,
            12,
            "completed ratchets to total"
        );
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("complete (1)"), "memory moved with the store");

        app.tick(ch('u'), now, &tx);
        assert_eq!(status(&app), ListStatus::Watching, "undo restores status");
        assert_eq!(
            app.store.get_show(1).unwrap().unwrap().progress,
            0,
            "undo restores the captured progress"
        );
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("undone"), "{text}");
        app.tick(ch('u'), now, &tx);
        assert_eq!(status(&app), ListStatus::Watching, "undo is single-level");

        for (key_char, expected) in [
            ('p', ListStatus::Paused),
            ('w', ListStatus::Watching),
            ('x', ListStatus::Dropped),
            ('P', ListStatus::Planning),
        ] {
            app.tick(ch(key_char), now, &tx);
            assert_eq!(status(&app), expected, "{key_char} transition");
        }
    }

    #[test]
    fn recompute_survives_a_pending_undo() {
        use crate::domain::ListStatus;
        let (mut app, tx, now) = sized("history-recompute", 100, 30);
        seed_history(&mut app, &[(1, "Alpha", ListStatus::Watching)]);
        app.tick(ch('c'), now, &tx);
        assert_eq!(app.store.get_show(1).unwrap().unwrap().progress, 12);
        // r: no fully-watched rows exist, so progress recomputes to 0.
        app.tick(ch('r'), now, &tx);
        assert_eq!(app.store.get_show(1).unwrap().unwrap().progress, 0);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("progress reset"), "{text}");
        // u after r: the recompute survives, the undo is a no-op (05 §4).
        app.tick(ch('u'), now, &tx);
        let show = app.store.get_show(1).unwrap().unwrap();
        assert_eq!(show.list_status, ListStatus::Completed);
        assert_eq!(show.progress, 0, "recompute survives");
    }

    #[test]
    fn hard_delete_confirm_freezes_fires_and_cancels() {
        use crate::domain::ListStatus;
        let (mut app, tx, now) = sized("history-delete", 100, 30);
        seed_history(
            &mut app,
            &[
                (1, "Alpha", ListStatus::Watching),
                (2, "Beta", ListStatus::Watching),
            ],
        );
        app.tick(ch('X'), now, &tx);
        assert_eq!(app.confirm_delete, Some(1));
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("delete \"Alpha\""), "{text}");
        assert!(text.contains("y confirm"), "{text}");

        // Frozen: every non-y key cancels WITHOUT acting: q does not quit,
        // j does not move the cursor, F1 does not switch views.
        app.tick(ch('q'), now, &tx);
        assert!(!app.quit, "q is swallowed while armed");
        assert!(app.confirm_delete.is_none(), "q cancels like any non-y key");
        press(&mut app, &tx, now, &[ch('X'), ch('j')]);
        assert_eq!(
            app.history.selected().unwrap().enrichment.anilist_id,
            1,
            "cursor keys never reach the frozen list"
        );
        press(&mut app, &tx, now, &[ch('X')]);
        app.tick(key(KeyCode::F(1)), now, &tx);
        assert_eq!(app.view, View::History, "view switches are swallowed");
        app.tick(ch('X'), now, &tx);
        app.tick(ch('X'), now, &tx);
        assert_eq!(app.confirm_delete, Some(1), "repeat X stays armed");
        app.tick(key(KeyCode::Esc), now, &tx);
        assert_eq!(app.confirm_delete, None, "esc cancels");
        assert!(app.store.get_show(1).unwrap().is_some(), "nothing deleted");

        // y fires the cascade; the cursor holds its ordinal (now Beta).
        press(&mut app, &tx, now, &[ch('X'), ch('y')]);
        assert!(app.store.get_show(1).unwrap().is_none(), "row deleted");
        assert_eq!(app.history.selected().unwrap().enrichment.anilist_id, 2);
        // Deleting the last show falls to the empty state.
        press(&mut app, &tx, now, &[ch('X'), ch('y')]);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("nothing watched yet"), "{text}");
    }

    #[test]
    fn ctrl_c_still_quits_while_the_confirm_is_armed() {
        use crate::domain::ListStatus;
        let (mut app, tx, now) = sized("delete-ctrlc", 100, 30);
        seed_history(&mut app, &[(1, "Alpha", ListStatus::Watching)]);
        app.tick(ch('X'), now, &tx);
        app.tick(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            now,
            &tx,
        );
        assert!(app.quit, "Ctrl-C stays the emergency exit");
    }

    fn landing_harness(
        name: &str,
        registry: Arc<ProviderRegistry>,
        seed: impl FnOnce(&Store),
    ) -> (App, EventTx, super::super::event::EventRx, Instant) {
        let store = Store::open_memory().unwrap();
        seed(&store);
        let config = Config {
            landing: "last_watched".into(),
            ..Config::default()
        };
        let (tx, rx) = super::super::event::channel();
        let app = App::new(
            &config,
            store,
            StubCatalog::inert(),
            registry,
            &test_paths(name),
            Picker::halfblocks(),
            &tx,
        );
        (app, tx, rx, Instant::now())
    }

    fn seed_played(store: &Store, aid: i64, title: &str) {
        let e = Enrichment {
            anilist_id: aid,
            title_romaji: title.to_string(),
            total_episodes: Some(12),
            mal_id: Some(500 + aid),
            ..Enrichment::default()
        };
        store.add_to_library(&e, 100).unwrap();
        store
            .record_finish(
                aid,
                crate::domain::Translation::Sub,
                "1",
                1,
                1400.0,
                1420.0,
                None,
                200,
            )
            .unwrap();
    }

    #[test]
    fn last_watched_landing_opens_the_resume_detail() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
        ]);
        let (mut app, tx, rx, now) = landing_harness("landing-open", registry, |store| {
            seed_played(store, 1, "Alpha");
        });
        assert!(app.resume_pending);
        app.tick(Event::Resize(100, 30), now, &tx);
        assert_eq!(app.view, View::History);
        assert_eq!(app.pane, Pane::Detail, "60-99+ opens in-pane, never zoom");
        assert_eq!(app.detail.shown().map(|e| e.anilist_id), Some(1));
        settle_feed(&mut app, &tx, &rx, now);
        assert_eq!(app.detail.episodes.grid().len(), 3);
        assert_eq!(
            app.detail.episodes.cursor(),
            1,
            "parked past the watched episode"
        );
        assert_eq!(app.resume_demote, None, "successful load clears the arm");
    }

    #[test]
    fn last_watched_landing_demotes_when_the_walk_exhausts() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay").with_key("505"),
        ]);
        let (mut app, tx, rx, now) = landing_harness("landing-demote", registry, |store| {
            seed_played(store, 1, "Alpha");
        });
        app.tick(Event::Resize(100, 30), now, &tx);
        assert_eq!(app.pane, Pane::Detail);
        settle_feed(&mut app, &tx, &rx, now);
        assert_eq!(app.pane, Pane::List, "exhaust demotes to the list");
        assert_eq!(app.view, View::History);
        assert_eq!(app.resume_demote, None);
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("no source found"), "{text}");
    }

    #[test]
    fn last_watched_landing_with_no_plays_stays_on_history() {
        let (mut app, tx, _rx, now) =
            landing_harness("landing-neverplayed", teststub::inert_registry(), |store| {
                let e = Enrichment {
                    anilist_id: 1,
                    title_romaji: "Alpha".into(),
                    ..Enrichment::default()
                };
                store.add_to_library(&e, 100).unwrap();
            });
        assert!(!app.resume_pending, "never-played history never arms");
        app.tick(Event::Resize(100, 30), now, &tx);
        assert_eq!(app.view, View::History);
        assert_eq!(app.pane, Pane::List);
        assert!(!app.detail.episodes.engaged_for(1), "no auto fetch");
    }

    /// The COMMON landing path: the last-watched show's listing is still in
    /// episode_cache, so engage lands synchronously with no worker event to
    /// clear the demote arm (final-gate review finding).
    #[test]
    fn cached_landing_clears_the_demote_arm_synchronously() {
        let registry = teststub::registry(vec![teststub::StubProvider::new("megaplay")]);
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (mut app, tx, rx, now) = landing_harness("landing-cached", registry, |store| {
            seed_played(store, 1, "Alpha");
            let e = Enrichment {
                anilist_id: 1,
                title_romaji: "Alpha".into(),
                ..Enrichment::default()
            };
            store.bind_provider(&e, "megaplay", "m-1", unix).unwrap();
            store
                .set_episode_cache(
                    1,
                    "megaplay",
                    Translation::Sub,
                    &["1".into(), "2".into(), "3".into()],
                    None,
                    unix,
                )
                .unwrap();
        });
        app.tick(Event::Resize(100, 30), now, &tx);
        assert_eq!(app.detail.episodes.grid().len(), 3, "cache-hit landing");
        assert_eq!(
            app.resume_demote, None,
            "a synchronous landing must clear the arm; no event will"
        );
        // Regression body: a LATER same-show walk exhaust (track flip, the
        // stub answers Network) must not demote a landed surface.
        press(&mut app, &tx, now, &[ch(':'), ch('d'), ch('u'), ch('b')]);
        app.tick(key(KeyCode::Enter), now, &tx);
        settle_feed(&mut app, &tx, &rx, now);
        assert_eq!(
            app.pane,
            Pane::Detail,
            "05 §10.6: only the auto-open's own walk demotes"
        );
        assert_eq!(app.view, View::History);
    }

    #[test]
    fn continuation_drops_on_nav_away_and_translation_flip() {
        // Both siblings list; every resolve fails, so the first play arms a
        // continuation and starts the walk toward senshi.
        let build = || {
            teststub::registry(vec![
                teststub::StubProvider::new("megaplay")
                    .with_key("505")
                    .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
                teststub::StubProvider::new("senshi")
                    .with_key("505")
                    .with_episodes(Ok(vec!["1".into(), "2".into(), "3".into()])),
            ])
        };
        // Nav-away: the next selection resets the session; the landed
        // sibling grid belongs to nobody the continuation knows.
        let (mut app, tx, rx, now) = harness_full(
            "cont-navaway",
            StubCatalog::search_scripted(vec![one_page(2)]),
            build(),
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        assert!(app.playback.drain(Duration::from_secs(5)));
        while let Ok(ev) = rx.try_recv() {
            // Apply ONLY the play failure; leave the walk's fetch in flight.
            if matches!(ev, Event::PlayFinished { .. }) {
                app.tick(ev, t1, &tx);
            }
        }
        assert!(
            app.playback.continuation().is_some(),
            "armed by the failure"
        );
        app.tick(key(KeyCode::Esc), t1, &tx);
        app.tick(ch('j'), t1, &tx);
        settle_play(&mut app, &tx, &rx, t1);
        assert!(
            app.playback.continuation().is_none(),
            "nav-away drops the continuation"
        );
        assert!(!app.playback.is_playing(), "and nothing relaunched");

        // Translation flip mid-walk: same arming, then :dub before the
        // sibling grid lands.
        let (mut app, tx, rx, now) = harness_full(
            "cont-transflip",
            StubCatalog::search_scripted(vec![one_page(1)]),
            build(),
        );
        let t1 = open_first_result(&mut app, &tx, &rx, now);
        app.tick(key(KeyCode::Enter), t1, &tx);
        assert!(app.playback.drain(Duration::from_secs(5)));
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, Event::PlayFinished { .. }) {
                app.tick(ev, t1, &tx);
            }
        }
        assert!(app.playback.continuation().is_some());
        press(&mut app, &tx, t1, &[ch(':'), ch('d'), ch('u'), ch('b')]);
        app.tick(key(KeyCode::Enter), t1, &tx);
        settle_play(&mut app, &tx, &rx, t1);
        assert!(
            app.playback.continuation().is_none(),
            "a sub continuation must not relaunch on the dub track"
        );
        assert!(!app.playback.is_playing());
    }

    #[test]
    fn ctrl_c_skips_a_dirty_settings_persist_and_fkey_leave_saves() {
        let (mut app, tx, now) = sized("settings-ctrlc", 100, 32);
        std::fs::create_dir_all(app.config_file.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&app.config_file);
        app.tick(ch('S'), now, &tx);
        press(&mut app, &tx, now, &[ch('j'), ch('l')]);
        assert!(app.settings.dirty);
        app.tick(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            now,
            &tx,
        );
        assert!(app.quit);
        assert!(
            !app.config_file.exists(),
            "the emergency exit never persists"
        );

        // The F-key leave routes through the same persist as the letters.
        let (mut app, tx, now) = sized("settings-fkey", 100, 32);
        std::fs::create_dir_all(app.config_file.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&app.config_file);
        app.tick(ch('S'), now, &tx);
        press(&mut app, &tx, now, &[ch('j'), ch('l')]);
        app.tick(key(KeyCode::F(1)), now, &tx);
        assert_eq!(app.view, View::Browse);
        assert_eq!(Config::load(&app.config_file).default_quality, "worst");
        std::fs::remove_file(&app.config_file).ok();
    }

    /// Copy-pins for the §4.10 rows that only had variant-level coverage
    /// (final-gate review): the rendered string is the contract.
    #[test]
    fn episode_toast_copy_covers_the_remaining_matrix_rows() {
        let (mut app, _tx, now) = harness("episode-copy");
        let cases: Vec<(Feedback, &str)> = vec![
            (
                Feedback::Fail {
                    provider: "megaplay".into(),
                    class: FetchClass::Http,
                },
                "megaplay returned an error",
            ),
            (
                Feedback::Fail {
                    provider: "megaplay".into(),
                    class: FetchClass::Data,
                },
                "couldn't load episodes",
            ),
            (
                Feedback::NoMatch {
                    provider: "senshi".into(),
                },
                "no match on senshi",
            ),
            (Feedback::PinPending, "still resolving, try again shortly"),
            (Feedback::PinNothing, "no source: nothing to pin"),
            (
                Feedback::PinSaveFailed { clearing: false },
                "couldn't save the provider pin",
            ),
            (
                Feedback::PinSaveFailed { clearing: true },
                "couldn't clear the provider pin",
            ),
        ];
        for (feedback, copy) in cases {
            app.apply_episode_feedback(vec![feedback], now);
            let text = rendered(&mut app, 100, 30);
            assert!(text.contains(copy), "expected {copy:?} in {text}");
            app.toasts = Toasts::default();
        }
    }

    #[test]
    fn exhausted_resolve_renders_no_source() {
        let registry = teststub::registry(vec![
            teststub::StubProvider::new("megaplay")
                .with_key("505")
                .with_episodes(Err(crate::providers::ProviderError::Network)),
        ]);
        let (mut app, tx, rx, now) = harness_full(
            "nosource-e2e",
            StubCatalog::search_scripted(vec![one_page(1)]),
            registry,
        );
        open_first_result(&mut app, &tx, &rx, now);
        assert!(app.detail.episodes.no_source());
        let text = rendered(&mut app, 100, 30);
        assert!(text.contains("network unreachable"), "{text}");
        assert!(text.contains("no source found"), "{text}");
        assert!(text.contains("no source"), "{text}");
    }
}
