//! App state, input, render, event loop, workers (04, DESIGN). `tui::workers`
//! is the ONE glue point allowed to import source, store, player, resolver, and
//! anilist together (01 §3). Render stays pure of store writes and app-state
//! mutation (01 §5); the one draw-side mutation is ratatui-image protocol
//! resize bookkeeping, which is render-owned by that library's design.
//!
//! The `App` body is the ROD-438 cover demo: a Discover-style grid plus detail
//! overlay driven by the real pump, caches, and encode routing. The real views
//! replace it from ROD-439 without touching the loop.

pub mod clock;
pub mod covers;
pub mod event;
pub mod workers;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::DynamicImage;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui_image::picker::Picker;

use crate::config::Config;
use crate::paths::Paths;
use clock::TickClock;
use covers::CoverCaches;
use covers::detail::{Action, CoverState};
use covers::discover::DiscoverCovers;
use covers::render::ProtocolPool;
use covers::sizing;
use event::{DemoCard, Event, EventTx};
use workers::{CancelFlag, Drain};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Pool key for the single detail cover; grid slots key by url, and urls are
/// absolute so they can never collide with this.
const DETAIL_KEY: &str = "detail";

#[derive(Debug, PartialEq, Eq)]
enum Feed {
    Loading,
    Ready,
    Failed(String),
}

/// Grid geometry shared by draw and pump so the two can never disagree on
/// what is visible (zigoku ROD-243 shape).
struct GridGeo {
    tier: sizing::Tier,
    cover_h: u16,
    slot_h: u16,
    grid_top: u16,
    cols: usize,
    rows_visible: usize,
}

fn grid_geo(w: u16, h: u16, cell: Option<ratatui_image::FontSize>) -> GridGeo {
    let tier = sizing::tier(w);
    let cover_h = sizing::card_cover_h(&tier, cell);
    // DESIGN 3.8 card anatomy: three meta rows plus a gap row.
    let slot_h = cover_h + 4;
    // Demo chrome: two header rows + spacer; the DESIGN 3.4/3.8 bars are 439.
    let grid_top = 3u16;
    let cols = ((w.saturating_sub(2)) / tier.slot_w).max(1) as usize;
    let grid_h = h.saturating_sub(grid_top).saturating_sub(1);
    let rows_visible = ((grid_h / slot_h) as usize).max(1);
    GridGeo {
        tier,
        cover_h,
        slot_h,
        grid_top,
        cols,
        rows_visible,
    }
}

pub struct App {
    quit: bool,
    dirty: bool,
    ticks: u64,
    term: (u16, u16),
    cover_art: bool,
    /// Startup snapshot. Freeze law is live cap re-read each pump (ROD-240);
    /// safe only while nothing mutates config at runtime. The Settings view
    /// (ROD-439) must replace this with a live read or the law breaks.
    cover_cap: usize,
    cards: Vec<DemoCard>,
    feed: Feed,
    selected: usize,
    scroll_row: usize,
    detail_open: bool,
    covers: DiscoverCovers,
    detail_cover: CoverState,
    caches: Arc<CoverCaches>,
    covers_dir: PathBuf,
    pool: ProtocolPool,
    feed_drain: Drain,
    cover_drain: Drain,
    discover_cover_drain: Drain,
    encode_drain: Drain,
}

impl App {
    /// State only; workers start in `run` (bootstrap order is its job).
    pub fn new(config: &Config, covers_dir: PathBuf, picker: Picker, tx: &EventTx) -> App {
        let encode_drain = Drain::default();
        let pool = ProtocolPool::new(picker, tx.clone(), &encode_drain);
        App {
            quit: false,
            dirty: true,
            ticks: 0,
            term: (0, 0),
            cover_art: config.cover_art,
            cover_cap: config.effective_cover_concurrency() as usize,
            cards: Vec::new(),
            feed: Feed::Loading,
            selected: 0,
            scroll_row: 0,
            detail_open: false,
            covers: DiscoverCovers::default(),
            detail_cover: CoverState::default(),
            caches: Arc::new(CoverCaches::new()),
            covers_dir,
            pool,
            feed_drain: Drain::default(),
            cover_drain: Drain::default(),
            discover_cover_drain: Drain::default(),
            encode_drain,
        }
    }

    /// Geometry for the last known terminal size.
    fn geo(&self) -> GridGeo {
        grid_geo(self.term.0, self.term.1, self.pool.cell())
    }

    /// Mutates; draw is pure (04 §1). DISPATCH ONLY: every non-trivial arm
    /// goes through a named handler, an arm may stay inline only as a single
    /// assignment. The zigoku god-file grew one inline arm at a time; the
    /// same rule governs `on_key`.
    fn tick(&mut self, event: Event, now: Instant, tx: &EventTx) {
        match event {
            Event::Key(key) => self.on_key(key, now, tx),
            Event::Resize(w, h) => self.on_resize(w, h),
            Event::FocusGained | Event::FocusLost => {}
            // No keys can ever arrive again; quit clean instead of zombieing.
            Event::InputClosed => self.quit = true,
            Event::Tick => self.on_tick(now, tx),
            Event::CoverDone { for_id, img } => self.on_cover_done(for_id, img),
            Event::CoverError { for_id } => self.on_cover_error(for_id, now),
            Event::DiscoverCoverDone { url, img } => self.on_discover_cover_done(&url, img),
            Event::DiscoverCoverError { url } => self.on_discover_cover_error(&url, now),
            Event::CoverEncodeReady => self.on_encode_ready(),
            Event::DemoFeedLoaded { cards } => self.on_feed_loaded(cards),
            Event::DemoFeedFailed { cause } => self.on_feed_failed(cause),
        }
    }

    /// Key dispatch; same rule as `tick`, bindings act through named handlers.
    fn on_key(&mut self, key: KeyEvent, now: Instant, tx: &EventTx) {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            _ if ctrl_c => self.quit = true,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.escape(),
            KeyCode::Enter | KeyCode::Char('d') => self.toggle_detail(now, tx),
            KeyCode::Left | KeyCode::Char('h') => self.move_selection(-1, now, tx),
            KeyCode::Right | KeyCode::Char('l') => self.move_selection(1, now, tx),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-(self.geo().cols as i64), now, tx)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(self.geo().cols as i64, now, tx)
            }
            _ => {}
        }
    }

    fn on_resize(&mut self, w: u16, h: u16) {
        self.term = (w, h);
        self.clamp_scroll();
        self.dirty = true;
    }

    /// ~100ms cadence (04 §8): spinner frame, the cover pump, and the detail
    /// retry ride it (the single-flight gate defers superseded fetches to
    /// the next tick instead of joining, 04 §6).
    fn on_tick(&mut self, now: Instant, tx: &EventTx) {
        self.ticks += 1;
        self.pump_covers(now, tx);
        if self.detail_open {
            self.sync_detail_cover(now, tx);
        }
        self.dirty = true;
    }

    /// Detail art landed. Dual keep-check (zigoku ROD-156 #2): the state id
    /// match alone is blind to a selection that moved while the single-flight
    /// gate deferred `begin_fetch`, so the live selection re-validates before
    /// install; a miss clears and the tick retry refetches the live target.
    /// `on_cover_error` needs no twin: it only records a cooldown keyed by
    /// the id that actually failed, never touching the render store.
    fn on_cover_done(&mut self, for_id: i64, img: DynamicImage) {
        if self.detail_cover.on_done(for_id) {
            let live = self.cards.get(self.selected).map(|c| c.anilist_id);
            if live == Some(for_id) {
                self.pool.set(DETAIL_KEY, img);
            } else {
                self.detail_cover.clear();
            }
        }
        self.dirty = true;
    }

    /// Detail fetch failed; starts the 05 §12 cooldown.
    fn on_cover_error(&mut self, for_id: i64, now: Instant) {
        self.detail_cover.on_error(for_id, now);
        self.dirty = true;
    }

    /// Slot adopts by url wherever the grid moved meanwhile (04 §4.4). The
    /// buffer moves straight into the render store, no clones.
    fn on_discover_cover_done(&mut self, url: &str, img: DynamicImage) {
        self.covers.adopt(url);
        self.pool.ensure(url, img);
        self.dirty = true;
    }

    /// Per-url cooldown; the rank placeholder stays the loading cue.
    fn on_discover_cover_error(&mut self, url: &str, now: Instant) {
        self.covers.note_failure(url, now);
        self.dirty = true;
    }

    /// Encode worker wake: apply routed responses on the UI thread.
    fn on_encode_ready(&mut self) {
        if self.pool.apply_responses() {
            self.dirty = true;
        }
    }

    fn on_feed_loaded(&mut self, cards: Vec<DemoCard>) {
        self.cards = cards;
        self.feed = Feed::Ready;
        self.selected = 0;
        self.scroll_row = 0;
        self.dirty = true;
    }

    fn on_feed_failed(&mut self, cause: String) {
        self.feed = Feed::Failed(cause);
        self.dirty = true;
    }

    /// Esc chain (DESIGN 6.x shape): innermost surface first, quit last.
    fn escape(&mut self) {
        if self.detail_open {
            self.detail_open = false;
            self.dirty = true;
        } else {
            self.quit = true;
        }
    }

    fn toggle_detail(&mut self, now: Instant, tx: &EventTx) {
        self.detail_open = !self.detail_open;
        if self.detail_open {
            self.sync_detail_cover(now, tx);
        }
        self.dirty = true;
    }

    /// Cursor step with edge clamp; re-syncs the detail cover when open.
    fn move_selection(&mut self, delta: i64, now: Instant, tx: &EventTx) {
        if self.cards.is_empty() {
            return;
        }
        let last = (self.cards.len() - 1) as i64;
        self.selected = (self.selected as i64 + delta).clamp(0, last) as usize;
        self.clamp_scroll();
        if self.detail_open {
            self.sync_detail_cover(now, tx);
        }
        self.dirty = true;
    }

    /// Keep the selected row inside the visible band.
    fn clamp_scroll(&mut self) {
        let geo = self.geo();
        let sel_row = self.selected / geo.cols;
        self.scroll_row = self.scroll_row.min(sel_row);
        if sel_row >= self.scroll_row + geo.rows_visible {
            self.scroll_row = sel_row + 1 - geo.rows_visible;
        }
    }

    /// Detail cover reconcile (05 §12): decide, then fetch or clear. The
    /// discrete-nav path syncs immediately; the 150ms settle debounce belongs
    /// to Browse/History scroll (439).
    fn sync_detail_cover(&mut self, now: Instant, tx: &EventTx) {
        if !self.cover_art {
            return;
        }
        let target = self.cards.get(self.selected);
        let target_id = target.map(|c| c.anilist_id);
        let target_url = target.and_then(|c| c.cover_url.as_deref());
        match self.detail_cover.decide(target_id, target_url, now) {
            Action::None | Action::Suppress | Action::UpToDate => {}
            Action::Clear => {
                self.detail_cover.clear();
                self.pool.remove(DETAIL_KEY);
            }
            Action::Fetch => {
                // Single-flight: one detail fetch at a time, matching the
                // zigoku one-thread semantic without its UI-blocking join
                // (04 §6 forbids joins on the hot path). A selection storm
                // defers to the tick retry instead of spawning per keystroke.
                if self.cover_drain.inflight() > 0 {
                    return;
                }
                let (id, url) = (target_id.unwrap(), target_url.unwrap().to_string());
                self.detail_cover.begin_fetch(id, &url);
                let spawned = workers::spawn_cover_fetch(
                    &self.cover_drain,
                    tx.clone(),
                    Arc::clone(&self.caches),
                    self.covers_dir.clone(),
                    id,
                    url,
                );
                if !spawned {
                    // No worker will ever answer; a stranded spinner is worse.
                    self.detail_cover.clear();
                }
            }
        }
    }

    /// Discover pump pass (04 §7.4): visible plus one prefetch row, then sync
    /// the protocol pool with the surviving slots.
    fn pump_covers(&mut self, now: Instant, tx: &EventTx) {
        if !self.cover_art || self.cards.is_empty() {
            return;
        }
        let geo = self.geo();
        let start = self.scroll_row * geo.cols;
        let span = (geo.rows_visible + 1) * geo.cols;
        let end = self.cards.len().min(start + span);
        if start >= end {
            return;
        }
        let window: Vec<&str> = self.cards[start..end]
            .iter()
            .filter_map(|c| c.cover_url.as_deref())
            .collect();
        let chosen = self.covers.pump(
            &window,
            now,
            self.cover_cap,
            self.discover_cover_drain.inflight(),
        );
        for url in chosen {
            let spawned = workers::spawn_discover_cover_fetch(
                &self.discover_cover_drain,
                tx.clone(),
                Arc::clone(&self.caches),
                self.covers_dir.clone(),
                url.clone(),
            );
            if !spawned {
                self.covers.reset_loading(&url);
            }
        }
        let covers = &self.covers;
        self.pool
            .retain(|key| key == DETAIL_KEY || covers.get(key).is_some());
    }

    /// Pure of app state and stores (04 §1, 01 §5); `&mut` is for the
    /// protocol pool's render-owned resize bookkeeping only.
    fn draw(&mut self, frame: &mut Frame<'_>, now: Instant) {
        let area = frame.area();
        let geo = grid_geo(area.width, area.height, self.pool.cell());
        let cell = self.pool.cell();
        let head = format!(
            "sabigoku cover demo (ROD-438) · proto={:?} · cell={} · tier={} cover={}x{} · slots={} · encode_err={}",
            self.pool.protocol_type(),
            cell.map_or("unreported".to_string(), |c| format!(
                "{}x{}px",
                c.width, c.height
            )),
            if geo.tier.large { "large" } else { "small" },
            geo.tier.cover_w,
            geo.cover_h,
            self.covers.len(),
            self.pool.encode_errors(),
        );
        frame.render_widget(
            Paragraph::new(head).style(Style::new().fg(Color::Cyan)),
            row(area, 0),
        );
        frame.render_widget(
            Paragraph::new("hjkl/arrows move · enter detail · esc/q quit").dim(),
            row(area, 1),
        );

        match &self.feed {
            Feed::Loading => {
                let spin = SPINNER[(self.ticks as usize) % SPINNER.len()];
                frame.render_widget(
                    Paragraph::new(format!("{spin} loading feed…")).fg(Color::Cyan),
                    row(area, geo.grid_top + 1),
                );
                return;
            }
            Feed::Failed(cause) => {
                frame.render_widget(
                    Paragraph::new(format!("[!] can't reach the feed: {cause}"))
                        .fg(Color::Red)
                        .bold(),
                    row(area, geo.grid_top + 1),
                );
                return;
            }
            Feed::Ready if self.cards.is_empty() => {
                frame.render_widget(
                    Paragraph::new("no entries").dim().italic(),
                    row(area, geo.grid_top + 1),
                );
                return;
            }
            Feed::Ready => {}
        }

        for (i, card) in self.cards.iter().enumerate() {
            let (card_row, col) = (i / geo.cols, i % geo.cols);
            if card_row < self.scroll_row || card_row >= self.scroll_row + geo.rows_visible {
                continue;
            }
            // DESIGN 3.8: 2-cell left margin, one slot per card.
            let x = 2 + (col as u16) * geo.tier.slot_w;
            let y = geo.grid_top + ((card_row - self.scroll_row) as u16) * geo.slot_h;
            if x + geo.tier.cover_w > area.width || y + geo.slot_h > area.height {
                continue;
            }
            let cover = Rect::new(x, y, geo.tier.cover_w, geo.cover_h);
            let drawn = match card.cover_url.as_deref() {
                Some(url) => self.pool.render(frame, cover, url),
                None => false,
            };
            if !drawn {
                // Placeholder per DESIGN 3.8: surface fill, centered rank.
                frame.render_widget(
                    Paragraph::new(format!("\n#{}", i + 1))
                        .centered()
                        .dim()
                        .block(Block::new().style(Style::new().bg(Color::Rgb(30, 30, 40)))),
                    cover,
                );
            }
            let selected = i == self.selected;
            if selected && x > 0 {
                frame.render_widget(
                    Paragraph::new("▸").fg(Color::Magenta),
                    Rect::new(x - 1, y + geo.cover_h, 1, 1),
                );
            }
            frame.render_widget(
                Paragraph::new(format!("#{}", i + 1)),
                Rect::new(x, y + geo.cover_h, geo.tier.cover_w, 1),
            );
            let title = Paragraph::new(card.title.as_str()).style(if selected {
                Style::new().fg(Color::Magenta).bold()
            } else {
                Style::new().dim()
            });
            frame.render_widget(
                title,
                Rect::new(x, y + geo.cover_h + 1, geo.tier.cover_w, 1),
            );
        }

        if self.detail_open {
            self.draw_detail(frame, area, &geo, now);
        }
    }

    /// Centered zoom overlay: DESIGN 3.3 block, hard 20-col cap, 28/20 rows.
    fn draw_detail(&mut self, frame: &mut Frame<'_>, area: Rect, geo: &GridGeo, _now: Instant) {
        let Some(card) = self.cards.get(self.selected) else {
            return;
        };
        let cover_w = geo.tier.cover_w.min(20);
        let cover_h = sizing::detail_cover_h(&geo.tier, self.pool.cell());
        let w = (cover_w + 4).min(area.width);
        let h = (cover_h + 4).min(area.height);
        let overlay = Rect::new(
            area.width.saturating_sub(w) / 2,
            area.height.saturating_sub(h) / 2,
            w,
            h,
        );
        frame.render_widget(Clear, overlay);
        let block = Block::bordered().title(format!(" {} ", card.title));
        let inner = block.inner(overlay);
        frame.render_widget(block, overlay);
        let cover = Rect::new(
            inner.x + 1,
            inner.y + 1,
            cover_w.min(inner.width),
            cover_h.min(inner.height),
        );
        if self.detail_cover.has_pixels() && self.pool.render(frame, cover, DETAIL_KEY) {
            return;
        }
        let msg = if self.detail_cover.is_loading() {
            format!(
                "{} loading…",
                SPINNER[(self.ticks as usize) % SPINNER.len()]
            )
        } else if card.cover_url.is_none() {
            "no art".to_string()
        } else {
            "cover unavailable".to_string()
        };
        frame.render_widget(Paragraph::new(msg).dim().centered(), cover);
    }
}

/// Cap on events per loop pass so a bursting producer can never starve the
/// tick clock, the draw, or the quit check (unbounded queue, 04 §8).
const MAX_EVENTS_PER_PASS: usize = 256;

/// Init, loop, clean-drain teardown (a ratified deviation from zigoku's
/// `_exit(0)`; the bounded timeouts are what 04 §3/§11 demand of it).
pub fn run(paths: &Paths, config: &Config) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    scope_panic_hook_to_main_thread();
    // Protocol query must run after entering the alternate screen and BEFORE
    // the input thread exists: it reads stdio itself (04 §3 query leftovers).
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    // External kills route through the same clean-drain quit as `q`; the OS
    // default disposition would strand the terminal raw + alt-screen.
    let sig_quit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        let _ = signal_hook::flag::register(sig, std::sync::Arc::clone(&sig_quit));
    }
    let (tx, rx) = event::channel();
    let shutdown = CancelFlag::default();
    let input_drain = Drain::default();
    if !event::spawn_input_thread(&input_drain, tx.clone(), shutdown.clone()) {
        ratatui::restore();
        return Err(std::io::Error::other("could not spawn the input thread"));
    }

    let mut app = App::new(config, paths.covers_dir(), picker, &tx);
    if let Ok(size) = terminal.size() {
        app.tick(Event::Resize(size.width, size.height), Instant::now(), &tx);
    }
    if !workers::spawn_demo_feed(&app.feed_drain, tx.clone()) {
        tx.post(Event::DemoFeedFailed {
            cause: "could not spawn the feed worker".to_string(),
        });
    }

    let mut clock = TickClock::new(Instant::now());
    let result = (|| {
        while !app.quit && !sig_quit.load(std::sync::atomic::Ordering::Relaxed) {
            let now = Instant::now();
            if let Ok(ev) = rx.recv_timeout(clock.timeout(now)) {
                app.tick(ev, Instant::now(), &tx);
            }
            let mut budget = MAX_EVENTS_PER_PASS;
            while budget > 0 && !app.quit {
                let Ok(ev) = rx.try_recv() else { break };
                app.tick(ev, Instant::now(), &tx);
                budget -= 1;
            }
            let now = Instant::now();
            if clock.should_tick(now) {
                app.tick(Event::Tick, now, &tx);
            }
            if app.dirty {
                terminal.draw(|frame| app.draw(frame, Instant::now()))?;
                app.dirty = false;
                // The carry-forward law: forward this draw's resize requests
                // into the encode worker, tagged per image.
                app.pool.drain_requests();
            }
        }
        Ok(())
    })();

    shutdown.cancel();
    input_drain.drain(Duration::from_millis(500));
    app.feed_drain.drain(Duration::from_secs(1));
    app.cover_drain.drain(Duration::from_secs(1));
    app.discover_cover_drain.drain(Duration::from_secs(1));
    // The encode worker exits when the pool (inside App) drops its queue.
    let encode_drain = app.encode_drain.clone();
    drop(app);
    encode_drain.drain(Duration::from_secs(1));
    ratatui::restore();
    result
}

/// `ratatui::init` installs a process-global restore hook, so an uncaught
/// worker panic would yank the terminal out from under the live UI thread.
/// Scope it: main panics restore; worker panics are contained by the
/// catch_unwind in `Drain::spawn` and counted there. Threads spawned outside
/// a Drain would panic without a trace, so do not spawn any.
fn scope_panic_hook_to_main_thread() {
    let main_thread = std::thread::current().id();
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main_thread {
            restore_hook(info);
        }
    }));
}

/// One-row rect at `y`, clipped to `area`.
fn row(area: Rect, y: u16) -> Rect {
    Rect::new(
        area.x,
        area.y + y,
        area.width,
        1.min(area.height.saturating_sub(y)),
    )
    .intersection(area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use covers::discover::SlotStatus;
    use image::DynamicImage;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn img() -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::new(2, 3))
    }

    /// Blocked-by-guard url: workers fail fast and hermetically (no network).
    fn blocked_url(i: usize) -> String {
        format!("http://127.0.0.1:9/{i}.png")
    }

    fn cards(n: usize) -> Vec<DemoCard> {
        (0..n)
            .map(|i| DemoCard {
                anilist_id: (i + 1) as i64,
                title: format!("Show {}", i + 1),
                cover_url: Some(blocked_url(i)),
            })
            .collect()
    }

    fn harness(name: &str) -> (App, EventTx, event::EventRx, Instant) {
        let covers_dir = std::env::temp_dir().join("sabigoku-shell-tests").join(name);
        let _ = std::fs::remove_dir_all(&covers_dir);
        let (tx, rx) = event::channel();
        let app = App::new(&Config::default(), covers_dir, Picker::halfblocks(), &tx);
        (app, tx, rx, Instant::now())
    }

    /// 100x30 halfblocks: large tier, cover_h 7, slot_h 11, 4 cols, 2 rows.
    fn sized(name: &str, n_cards: usize) -> (App, EventTx, event::EventRx, Instant) {
        let (mut app, tx, rx, now) = harness(name);
        app.tick(Event::Resize(100, 30), now, &tx);
        app.tick(
            Event::DemoFeedLoaded {
                cards: cards(n_cards),
            },
            now,
            &tx,
        );
        (app, tx, rx, now)
    }

    #[test]
    fn q_esc_and_ctrl_c_quit() {
        let (mut app, tx, _rx, now) = harness("quit-q");
        app.tick(key(KeyCode::Char('q')), now, &tx);
        assert!(app.quit);
        let (mut app, tx, _rx, now) = harness("quit-esc");
        app.tick(key(KeyCode::Esc), now, &tx);
        assert!(app.quit);
        let (mut app, tx, _rx, now) = harness("quit-ctrl-c");
        app.tick(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            now,
            &tx,
        );
        assert!(app.quit);
    }

    #[test]
    fn esc_closes_detail_before_quitting() {
        let (mut app, tx, _rx, now) = sized("esc-detail", 4);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert!(app.detail_open);
        app.tick(key(KeyCode::Esc), now, &tx);
        assert!(!app.detail_open);
        assert!(!app.quit);
        app.tick(key(KeyCode::Esc), now, &tx);
        assert!(app.quit);
    }

    #[test]
    fn input_closed_quits() {
        let (mut app, tx, _rx, now) = harness("input-closed");
        app.tick(Event::InputClosed, now, &tx);
        assert!(app.quit);
    }

    #[test]
    fn feed_events_populate_or_fail() {
        let (mut app, tx, _rx, now) = harness("feed");
        app.tick(Event::DemoFeedLoaded { cards: cards(3) }, now, &tx);
        assert_eq!(app.feed, Feed::Ready);
        assert_eq!(app.cards.len(), 3);
        let (mut app, tx, _rx, now) = harness("feed-fail");
        app.tick(
            Event::DemoFeedFailed {
                cause: "offline".to_string(),
            },
            now,
            &tx,
        );
        assert_eq!(app.feed, Feed::Failed("offline".to_string()));
    }

    #[test]
    fn tick_pumps_at_most_cap_covers() {
        let (mut app, tx, _rx, now) = sized("pump-cap", 8);
        app.tick(Event::Tick, now, &tx);
        // Default cap 4: exactly 4 of the 8 visible slots go loading; slot
        // state only changes when worker events are ticked, so this is
        // deterministic even though the workers already raced to fail.
        let loading = (0..8)
            .filter(|i| {
                app.covers
                    .get(&blocked_url(*i))
                    .is_some_and(|s| s.status() == SlotStatus::Loading)
            })
            .count();
        assert_eq!(loading, 4);
        assert!(app.discover_cover_drain.drain(Duration::from_secs(5)));
    }

    #[test]
    fn blocked_cover_roundtrip_lands_error_cooldown() {
        let (mut app, tx, rx, now) = sized("cover-roundtrip", 2);
        app.tick(Event::Tick, now, &tx);
        assert!(app.discover_cover_drain.drain(Duration::from_secs(5)));
        let mut errors = 0;
        while let Ok(ev) = rx.try_recv() {
            let is_err = matches!(ev, Event::DiscoverCoverError { .. });
            app.tick(ev, now, &tx);
            if is_err {
                errors += 1;
            }
        }
        assert_eq!(errors, 2, "guard-blocked urls must fail fast");
        for i in 0..2 {
            assert_eq!(
                app.covers.get(&blocked_url(i)).unwrap().status(),
                SlotStatus::Failed
            );
        }
        // Cooling: the next pump spawns nothing.
        app.tick(Event::Tick, now, &tx);
        assert_eq!(app.discover_cover_drain.inflight(), 0);
    }

    #[test]
    fn discover_cover_done_adopts_and_builds_a_protocol() {
        let (mut app, tx, _rx, now) = sized("adopt", 2);
        let url = blocked_url(0);
        app.tick(
            Event::DiscoverCoverDone {
                url: url.clone(),
                img: img(),
            },
            now,
            &tx,
        );
        assert_eq!(app.covers.get(&url).unwrap().status(), SlotStatus::Ready);
        assert!(app.pool.contains(&url));
    }

    #[test]
    fn stale_detail_cover_done_is_discarded() {
        let (mut app, tx, _rx, now) = sized("stale-detail", 4);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert!(app.detail_cover.is_loading());
        assert_eq!(app.detail_cover.for_id(), Some(1));
        app.tick(
            Event::CoverDone {
                for_id: 99,
                img: img(),
            },
            now,
            &tx,
        );
        assert!(!app.detail_cover.has_pixels());
        assert!(!app.pool.contains(DETAIL_KEY));
        app.tick(
            Event::CoverDone {
                for_id: 1,
                img: img(),
            },
            now,
            &tx,
        );
        assert!(app.detail_cover.has_pixels());
        assert!(app.pool.contains(DETAIL_KEY));
        assert!(app.cover_drain.drain(Duration::from_secs(5)));
    }

    #[test]
    fn detail_fetch_is_single_flight_with_tick_retry() {
        let (mut app, tx, _rx, now) = sized("single-flight", 6);
        // Hold the family busy: an artificial in-flight worker.
        let held = app.cover_drain.begin();
        app.tick(key(KeyCode::Enter), now, &tx);
        assert!(!app.detail_cover.is_loading(), "gate must defer, not spawn");
        // A selection storm while blocked must not spawn per keystroke.
        for _ in 0..10 {
            app.tick(key(KeyCode::Char('l')), now, &tx);
        }
        assert_eq!(app.cover_drain.inflight(), 1, "only the held guard");
        drop(held);
        // The tick retry fetches for the CURRENT selection, not the storm's.
        app.tick(Event::Tick, now, &tx);
        assert!(app.detail_cover.is_loading());
        assert_eq!(app.detail_cover.for_id(), Some(6));
        assert!(app.cover_drain.drain(Duration::from_secs(5)));
    }

    #[test]
    fn gated_stale_result_never_installs_for_a_moved_selection() {
        let (mut app, tx, _rx, now) = sized("gated-stale", 4);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert_eq!(app.detail_cover.for_id(), Some(1));
        // Gate every re-sync while the card-1 fetch is nominally in flight.
        let held = app.cover_drain.begin();
        app.tick(key(KeyCode::Char('l')), now, &tx);
        app.tick(key(KeyCode::Char('l')), now, &tx);
        assert_eq!(app.detail_cover.for_id(), Some(1), "gated: id stays pinned");
        // The stale fetch lands: state accepts, live check must refuse it.
        app.tick(
            Event::CoverDone {
                for_id: 1,
                img: img(),
            },
            now,
            &tx,
        );
        assert!(!app.pool.contains(DETAIL_KEY), "stale art must not install");
        assert!(!app.detail_cover.has_pixels());
        // Self-heal: the tick retry fetches for the live selection. Settle
        // the real worker from the opening Enter first or the gate re-fires.
        drop(held);
        assert!(app.cover_drain.drain(Duration::from_secs(5)));
        app.tick(Event::Tick, now, &tx);
        assert!(app.detail_cover.is_loading());
        assert_eq!(app.detail_cover.for_id(), Some(3));
        assert!(app.cover_drain.drain(Duration::from_secs(5)));
    }

    #[test]
    fn selection_moves_clamp_and_survive_empty_grid() {
        let (mut app, tx, _rx, now) = harness("empty-moves");
        for code in [KeyCode::Char('j'), KeyCode::Char('l'), KeyCode::Char('k')] {
            app.tick(key(code), now, &tx);
        }
        assert_eq!(app.selected, 0);
        let (mut app, tx, _rx, now) = sized("clamp-moves", 6);
        app.tick(key(KeyCode::Char('h')), now, &tx);
        assert_eq!(app.selected, 0, "left edge clamps");
        for _ in 0..20 {
            app.tick(key(KeyCode::Char('j')), now, &tx);
        }
        assert_eq!(app.selected, 5, "bottom clamps to last card");
    }

    #[test]
    fn cover_art_off_never_spawns_cover_workers() {
        let covers_dir = std::env::temp_dir()
            .join("sabigoku-shell-tests")
            .join("art-off");
        let (tx, _rx) = event::channel();
        let config = Config {
            cover_art: false,
            ..Config::default()
        };
        let mut app = App::new(&config, covers_dir, Picker::halfblocks(), &tx);
        let now = Instant::now();
        app.tick(Event::Resize(100, 30), now, &tx);
        app.tick(Event::DemoFeedLoaded { cards: cards(4) }, now, &tx);
        app.tick(Event::Tick, now, &tx);
        app.tick(key(KeyCode::Enter), now, &tx);
        assert_eq!(app.discover_cover_drain.inflight(), 0);
        assert_eq!(app.cover_drain.inflight(), 0);
        assert!(!app.detail_cover.is_loading());
    }

    #[test]
    fn draw_is_total_even_tiny_and_mid_load() {
        let (mut app, tx, _rx, now) = harness("draw-tiny");
        for (w, h) in [(100, 30), (16, 4), (2, 1)] {
            app.tick(Event::Resize(w, h), now, &tx);
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|frame| app.draw(frame, now)).unwrap();
        }
        app.tick(Event::DemoFeedLoaded { cards: cards(9) }, now, &tx);
        app.tick(key(KeyCode::Enter), now, &tx);
        for (w, h) in [(100, 30), (40, 12), (16, 4)] {
            app.tick(Event::Resize(w, h), now, &tx);
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|frame| app.draw(frame, now)).unwrap();
        }
        assert!(app.cover_drain.drain(Duration::from_secs(5)));
    }
}
