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

use crate::config::Config;
use crate::domain::{self, TitleLanguage};
use crate::providers::{CatalogProvider, DiscoverAxis};
use crate::store::Store;

use super::chrome::{self, BottomBar, HelpLine, Tab, TopBar};
use super::covers::CoverCaches;
use super::covers::render::ProtocolPool;
use super::event::{Event, EventTx};
use super::layout;
use super::render;
use super::theme::{self, Palette};
use super::toast::{Kind, Toasts};
use super::view::browse::{self, BrowseState};
use super::view::detail::{self, DetailState};
use super::view::discover::{self, DiscoverState};
use super::view::history::{self, HistoryState};
use super::view::settings::{self, SettingsState};
use super::view::{InputMode, Origin, Pane, View, ViewEnv};
use super::workers::Drain;

/// Unknown-command bottom-bar flash (DESIGN 3.5).
const COMMAND_FLASH: Duration = Duration::from_millis(800);

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
    toasts: Toasts,
    palette: &'static Palette,
    config: Config,
    browse: BrowseState,
    history: HistoryState,
    pub(super) discover: DiscoverState,
    settings: SettingsState,
    detail: DetailState,
    store: Store,
    catalog: Arc<dyn CatalogProvider>,
    caches: Arc<CoverCaches>,
    covers_dir: PathBuf,
    pub(super) pool: ProtocolPool,
    pub(super) cover_drain: Drain,
    pub(super) encode_drain: Drain,
}

impl App {
    /// State only; workers start in `run` (bootstrap order is its job).
    pub fn new(
        config: &Config,
        store: Store,
        catalog: Arc<dyn CatalogProvider>,
        covers_dir: PathBuf,
        picker: Picker,
        tx: &EventTx,
    ) -> App {
        let encode_drain = Drain::default();
        let pool = ProtocolPool::new(picker, tx.clone(), &encode_drain);
        App {
            quit: false,
            dirty: true,
            term: (0, 0),
            view: landing_view(&config.landing),
            pane: Pane::List,
            origin: Origin::Browse,
            mode: InputMode::Normal,
            command: String::new(),
            command_flash: None,
            toasts: Toasts::default(),
            palette: theme::by_name(&config.palette),
            config: config.clone(),
            browse: BrowseState::default(),
            history: HistoryState::default(),
            discover: DiscoverState::default(),
            settings: SettingsState::default(),
            detail: DetailState::default(),
            store,
            catalog,
            caches: Arc::new(CoverCaches::new()),
            covers_dir,
            pool,
            cover_drain: Drain::default(),
            encode_drain,
        }
    }

    /// Mutates; draw is pure (04 §1). Dispatch only.
    pub(super) fn tick(&mut self, event: Event, now: Instant, tx: &EventTx) {
        match event {
            Event::Key(key) => self.on_key(key, now),
            Event::Resize(w, h) => self.on_resize(w, h),
            Event::FocusGained | Event::FocusLost => {}
            // No keys can ever arrive again; quit clean instead of zombieing.
            Event::InputClosed => self.quit = true,
            Event::Tick => self.on_tick(now, tx),
            // Detail cover results rewire into DetailState in chunk 3;
            // nothing spawns that worker yet.
            Event::CoverDone { .. } | Event::CoverError { .. } => {}
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
        }
    }

    /// Key dispatch. Ctrl-C hard-quits from every mode; F-keys are global
    /// aliases that fire in any mode (DESIGN 7.2).
    fn on_key(&mut self, key: KeyEvent, now: Instant) {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            self.quit = true;
            return;
        }
        if let KeyCode::F(n @ 1..=4) = key.code {
            self.on_fkey(n);
            return;
        }
        match self.mode {
            InputMode::Normal => self.on_normal_key(key, now),
            InputMode::Search => self.on_search_key(key),
            InputMode::Command => self.on_command_key(key, now),
        }
        self.dirty = true;
    }

    fn on_normal_key(&mut self, key: KeyEvent, now: Instant) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('B') => self.switch_view(View::Browse),
            KeyCode::Char('H') => self.switch_view(View::History),
            KeyCode::Char('D') => self.switch_view(View::Discover),
            KeyCode::Char('S') => self.switch_view(View::Settings),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char(':') => self.open_command(),
            KeyCode::Esc => self.on_escape(),
            KeyCode::Char(' ') => self.on_space(),
            KeyCode::Enter => self.on_enter(),
            KeyCode::Char('h') => self.on_h(),
            KeyCode::Char('l') => self.on_l(),
            KeyCode::Char('j') => self.on_j(),
            KeyCode::Char('k') => self.on_k(),
            KeyCode::Char('P') => self.on_plan(now),
            KeyCode::Char(']') => self.on_axis_cycle(1),
            KeyCode::Char('[') => self.on_axis_cycle(-1),
            KeyCode::Char(c @ '1'..='4') => self.on_axis_select(c),
            _ => {}
        }
    }

    /// F1-F4 close any prompt: they are navigation, and unlike the letters
    /// they cannot be typed into a search (DESIGN 7.2).
    fn on_fkey(&mut self, n: u8) {
        self.mode = InputMode::Normal;
        let target = match n {
            1 => View::Browse,
            2 => View::History,
            3 => View::Discover,
            _ => View::Settings,
        };
        self.switch_view(target);
        self.dirty = true;
    }

    /// Direct go-to, never a toggle; same-view is a no-op (DESIGN 7.2).
    fn switch_view(&mut self, target: View) {
        if self.view == target {
            return;
        }
        self.view = target;
        self.pane = Pane::List;
    }

    fn open_search(&mut self) {
        match self.view {
            View::Browse | View::History => self.mode = InputMode::Search,
            // `/` jumps to Browse; Discover has no in-view filter (DESIGN 7.5).
            View::Discover => self.jump_to_browse_search(),
            View::Detail | View::Settings => {}
        }
    }

    fn jump_to_browse_search(&mut self) {
        self.switch_view(View::Browse);
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
    fn on_space(&mut self) {
        match self.view {
            View::Detail => self.demote(),
            View::Browse | View::History if self.pane == Pane::Detail => self.promote(),
            View::History if !self.two_pane() => self.promote(),
            _ => {}
        }
    }

    /// Enter drills toward wherever the grid is visible, then plays
    /// (DESIGN 10, ROD-170/259). Play itself lands in chunk 6.
    fn on_enter(&mut self) {
        match self.view {
            View::Discover => self.promote(),
            View::Browse | View::History if self.pane == Pane::Detail => {}
            View::History if !self.two_pane() => self.promote(),
            View::Browse | View::History if self.two_pane() => self.pane = Pane::Detail,
            _ => {}
        }
    }

    fn on_h(&mut self) {
        match self.view {
            View::Browse | View::History if self.pane == Pane::Detail => self.pane = Pane::List,
            View::Detail => self.demote(),
            View::Discover => self.nav_discover(-1, 0),
            _ => {}
        }
    }

    fn on_l(&mut self) {
        match self.view {
            View::Browse | View::History if self.pane == Pane::List && self.two_pane() => {
                self.pane = Pane::Detail
            }
            View::Discover => self.nav_discover(1, 0),
            _ => {}
        }
    }

    fn on_j(&mut self) {
        if self.view == View::Discover {
            self.nav_discover(0, 1);
        }
    }

    fn on_k(&mut self) {
        if self.view == View::Discover {
            self.nav_discover(0, -1);
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

    /// `P` "plan it" (DESIGN 6.1): Discover saves the selected card to the
    /// watchlist; the show row's default list status is planning. Browse's
    /// variant joins in chunk 3, History's transition in chunk 5.
    fn on_plan(&mut self, now: Instant) {
        if self.view != View::Discover {
            return;
        }
        let Some(entry) = self.discover.selected_entry() else {
            return;
        };
        match self.store.add_to_library(entry, unix_now()) {
            Ok(()) => self.toasts.push(Kind::Success, "added to watchlist", now),
            Err(_) => self
                .toasts
                .push(Kind::Error, "couldn't add to watchlist", now),
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

    fn on_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.close_search(),
            KeyCode::Enter => self.lock_search(),
            KeyCode::Backspace => self.on_search_backspace(),
            KeyCode::Char(c) => self.on_search_char(c),
            _ => {}
        }
    }

    /// Esc clears the query and restores the full list (DESIGN 6.2).
    fn close_search(&mut self) {
        self.search_buffer().clear();
        self.mode = InputMode::Normal;
    }

    /// Enter locks the result set and moves focus to the list; the query
    /// survives for the next `/` (DESIGN 6.2).
    fn lock_search(&mut self) {
        self.mode = InputMode::Normal;
        self.pane = Pane::List;
    }

    fn on_search_char(&mut self, c: char) {
        self.search_buffer().push(c);
    }

    fn on_search_backspace(&mut self) {
        self.search_buffer().pop();
    }

    /// Browse owns the catalogue query, History its local filter (DESIGN 8.4).
    fn search_buffer(&mut self) -> &mut String {
        match self.view {
            View::History => &mut self.history.filter,
            _ => &mut self.browse.query,
        }
    }

    fn on_command_key(&mut self, key: KeyEvent, now: Instant) {
        match key.code {
            KeyCode::Esc => self.mode = InputMode::Normal,
            KeyCode::Enter => self.run_command(now),
            KeyCode::Backspace => {
                self.command.pop();
            }
            KeyCode::Char(c) => self.command.push(c),
            _ => {}
        }
    }

    /// DESIGN 6.3. `sync` and `cache clear` are recognized but inert until
    /// their subsystems land (chunks 2-5); see the ROD-439 handoff notes.
    fn run_command(&mut self, now: Instant) {
        let command = std::mem::take(&mut self.command);
        self.mode = InputMode::Normal;
        match command.trim() {
            "q" => self.quit = true,
            "dub" => self.toggle_translation(),
            "sync" | "cache clear" => {}
            _ => self.on_unknown_command(now),
        }
    }

    fn toggle_translation(&mut self) {
        self.config.translation = if self.config.translation == "dub" {
            "sub".to_string()
        } else {
            "dub".to_string()
        };
    }

    /// Both feedback channels are specced: the 800ms bar flash (DESIGN 3.5)
    /// and the error toast (DESIGN 6.3).
    fn on_unknown_command(&mut self, now: Instant) {
        self.command_flash = Some(now + COMMAND_FLASH);
        self.toasts.push(Kind::Error, "unknown command", now);
    }

    fn on_resize(&mut self, w: u16, h: u16) {
        self.term = (w, h);
        if !self.two_pane() {
            // Below the split there is only one column (DESIGN 7.3).
            self.pane = Pane::List;
        }
        self.dirty = true;
    }

    /// ~100ms cadence (04 §8): toast TTL, the command flash, and the Discover
    /// feed + cover pump ride it.
    fn on_tick(&mut self, now: Instant, tx: &EventTx) {
        self.toasts.tick(now);
        if self.command_flash.is_some_and(|until| now >= until) {
            self.command_flash = None;
        }
        if self.view == View::Discover {
            self.tick_discover(now, tx);
        }
        self.dirty = true;
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

    /// Encode worker wake: apply routed responses on the UI thread.
    fn on_encode_ready(&mut self) {
        if self.pool.apply_responses() {
            self.dirty = true;
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
            View::Browse => browse::draw(frame, rows.content, self.palette, &self.browse),
            View::History => history::draw(frame, rows.content, self.palette, &self.history),
            View::Detail => detail::draw(frame, rows.content, self.palette, &self.detail),
            View::Discover => self.draw_discover(frame, rows.content, now),
            View::Settings => settings::draw(frame, rows.content, self.palette, &self.settings),
        }
        chrome::draw_bottom_bar(frame, rows.bottom, self.palette, &self.bottom_bar(now));
        self.toasts.draw(frame, area, self.palette);
    }

    fn draw_discover(&mut self, frame: &mut Frame<'_>, area: ratatui::layout::Rect, now: Instant) {
        let env = ViewEnv {
            pref: TitleLanguage::parse(&self.config.title_language),
            cour: domain::current_cour(unix_now()),
            now,
        };
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
        // Browse/History fall back to the current cour; Discover tracks the
        // selected card with no fallback; Detail tracks its show (chunk 3);
        // Settings shows no chip (DESIGN 3.4, 7.3).
        let season_chip = match self.view {
            View::Browse | View::History => {
                Some(render::cour_chip(domain::current_cour(unix_now())))
            }
            View::Discover => self
                .discover
                .selected_entry()
                .and_then(|e| render::season_chip(e.season, e.year)),
            _ => None,
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
        if self.command_flash.is_some_and(|until| now < until) {
            return BottomBar::CommandError;
        }
        match self.mode {
            InputMode::Search => match self.view {
                View::History => BottomBar::Search {
                    query: &self.history.filter,
                    scope: "history",
                    count: self.history.row_count,
                },
                _ => BottomBar::Search {
                    query: &self.browse.query,
                    scope: "catalogue",
                    count: self.browse.result_count,
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
            View::Settings => HelpLine::Settings,
        }
    }
}

/// Landing seeds from config; History is the default and the fallback for any
/// unrecognized value. `last_watched` demotes to History until the resume arm
/// lands (chunk 5); that demotion is also its specced no-resume fallback
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
    struct StubCatalog(Mutex<VecDeque<Result<CatalogPage, CatalogError>>>);

    impl StubCatalog {
        fn scripted(pages: Vec<Result<CatalogPage, CatalogError>>) -> Arc<dyn CatalogProvider> {
            Arc::new(StubCatalog(Mutex::new(pages.into())))
        }

        fn inert() -> Arc<dyn CatalogProvider> {
            Self::scripted(Vec::new())
        }
    }

    impl CatalogProvider for StubCatalog {
        fn search(&self, _q: &str, _p: u32) -> Result<CatalogPage, CatalogError> {
            Err(CatalogError::Network)
        }
        fn discover(&self, _a: DiscoverAxis, _p: u32) -> Result<CatalogPage, CatalogError> {
            self.0
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

    fn harness_with(
        name: &str,
        catalog: Arc<dyn CatalogProvider>,
    ) -> (App, EventTx, super::super::event::EventRx, Instant) {
        let covers_dir = std::env::temp_dir().join("sabigoku-app-tests").join(name);
        let (tx, rx) = super::super::event::channel();
        let app = App::new(
            &Config::default(),
            Store::open_memory().unwrap(),
            catalog,
            covers_dir,
            Picker::halfblocks(),
            &tx,
        );
        (app, tx, rx, Instant::now())
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

    /// Settle the feed worker, then apply everything it posted.
    fn settle_feed(app: &mut App, tx: &EventTx, rx: &super::super::event::EventRx, now: Instant) {
        assert!(app.discover.drain(Duration::from_secs(5)));
        while let Ok(ev) = rx.try_recv() {
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

    #[test]
    fn help_line_tracks_view_pane_and_empty_history() {
        let (mut app, tx, now) = sized("help-line", 100, 30);
        assert_eq!(app.help_line(), HelpLine::HistoryEmpty);
        app.history.row_count = 3;
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
}
