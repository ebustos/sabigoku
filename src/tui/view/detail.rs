//! Detail: the shared show surface, in-pane and full-screen zoom (DESIGN 3.3,
//! 4.4, 4.6, 5.3). The meta rail and two-column split land with ROD-439
//! chunk 4b.
//!
//! Ownership contract: DetailState is the ONE owner of the shared detail
//! surface. List views push a selection snapshot in; this module never reads
//! Browse/History/Discover internals (the zigoku detail.zig coupling is the
//! failure this rule exists to prevent). Cover transport (drain, settle
//! debounce, single-flight) lives here, and the episode session rides along
//! as its own subsystem (`tui::episodes`); neither lives on App.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::domain::{self, Enrichment, Season, preferred_title};
use crate::providers::CatalogProvider;
use crate::store::{ProviderAvailability, Store};
use crate::tui::clock::Debounce;
use crate::tui::covers::detail::{Action, CoverState};
use crate::tui::covers::render::ProtocolPool;
use crate::tui::covers::{CoverCaches, DETAIL_KEY, sizing};
use crate::tui::episodes::EpisodeSession;
use crate::tui::event::EventTx;
use crate::tui::render::{self, SPINNER};
use crate::tui::theme::Palette;
use crate::tui::view::ViewEnv;
use crate::tui::workers::{self, Drain};

/// Continuous-scroll cover settle (04 §8); discrete navigation syncs on the
/// same tick (DESIGN 6.4).
pub const COVER_SETTLE: Duration = Duration::from_millis(150);

/// Rows reserved below the cover in the single-column layout: worst-case
/// header + the compact meta line, a 2-line synopsis, the grid's spacer, the
/// provider caption that heads the grid (§5.3a), and 2 grid rows, so the
/// episode grid always keeps >= 2 visible rows (DESIGN 3.3).
const COVER_RESERVE: u16 = 14;
/// Below this a squashed poster is dropped outright, never a sliver.
const MIN_COVER_ROWS: u16 = 6;
/// Grid share of the synopsis budget: spacer + 2 rows.
const GRID_RESERVE: u16 = 3;

/// Single-column cover bound (DESIGN 3.3); the two-column zoom and the
/// History preview stack are exempt (their cover shares no column with the
/// grid) and pass `None`.
pub fn cover_height_cap(pane_h: u16) -> u16 {
    let cap = pane_h.saturating_sub(COVER_RESERVE);
    if cap < MIN_COVER_ROWS { 0 } else { cap }
}

/// Synopsis lines allowed inside `remaining` rows, leaving the grid its
/// spacer and 2 rows (DESIGN 3.3).
pub fn synopsis_cap(remaining: u16) -> u16 {
    remaining.saturating_sub(GRID_RESERVE)
}

/// Focused-grid row budget below the header (DESIGN 3.3). Layout below `y`:
/// synopsis, a 1-row spacer, the provider caption (§5.3a: it heads the grid),
/// then the grid. The synopsis yields so the grid keeps >= 2 rows; the
/// provider caption draws only when a grid row still follows it, never
/// orphaned above an empty region. Returns `(synopsis_rows, show_provider)`.
fn body_budget(
    remaining: u16,
    syn_natural: u16,
    grid_rows: u16,
    has_provider: bool,
) -> (u16, bool) {
    let prov = u16::from(has_provider);
    let syn_rows = syn_natural
        .min(remaining.saturating_sub(1 + prov + grid_rows).max(2))
        .min(remaining.saturating_sub(1 + prov + 2));
    let show_provider = has_provider && syn_rows + 2 < remaining;
    (syn_rows, show_provider)
}

/// Cover tier from the effective column width (DESIGN 3.2): never terminal
/// width, and hard-capped at 20 cols.
fn cover_width(detail_w: u16) -> u16 {
    match detail_w {
        w if w >= 40 => 20,
        w if w >= 25 => 14,
        _ => 0,
    }
}

/// Episode grid cell width (DESIGN 4.6): `[NN] ` / `[NNN]`.
const CELL_W: u16 = 5;

/// Two-internal-column gate (DESIGN 3.2), keyed to the pane's OWN width. The
/// split needs the surface to allow it (`split_ok`); Browse's in-pane detail
/// keeps the single stack at any width. It gates the cover/content column
/// split only: the metadata is one compact line at every width (§5.3a).
pub const DETAIL_TWO_COL_MIN: u16 = 100;

/// Gap between the cover column and the content column in the split layout.
const SPLIT_GAP: u16 = 4;

#[derive(Default)]
pub struct DetailState {
    shown: Option<Enrichment>,
    scroll: u16,
    cover: CoverState,
    /// Spinner epoch for the in-flight fetch (frame phase + slow escalation).
    started: Option<crate::tui::clock::AsyncStart>,
    drain: Drain,
    settle: Debounce,
    /// Refresh-on-view family (04 §5.1). Its own drain: the cover drain's
    /// single-flight gate counts inflight and must not see enrich fetches.
    enrich_drain: Drain,
    enrich_settle: Debounce,
    /// One staleness decision per shown id; a failed fetch retries only when
    /// the selection changes shows (set_target re-arms on a new id).
    enrich_checked: Option<i64>,
    pub(crate) episodes: EpisodeSession,
}

impl DetailState {
    pub fn shown(&self) -> Option<&Enrichment> {
        self.shown.as_ref()
    }

    /// Selection snapshot push. Continuous scroll debounces the cover by the
    /// settle window; discrete navigation (pane/view switch, promote) syncs
    /// on the next reconcile pass (DESIGN 6.4).
    pub fn set_target(&mut self, entry: &Enrichment, discrete: bool, now: Instant) {
        if self.shown.as_ref().map(|e| e.anilist_id) != Some(entry.anilist_id) {
            self.scroll = 0;
            // A new show voids the grid immediately (ROD-329: a stale grid
            // must never let play launch the wrong show).
            self.episodes.reset();
            self.enrich_checked = None;
        }
        self.shown = Some(entry.clone());
        let window = if discrete {
            Duration::ZERO
        } else {
            COVER_SETTLE
        };
        self.settle.arm(now, window);
        self.enrich_settle.arm(now, window);
    }

    /// Nothing selected: the pane clears, no stale detail (DESIGN 8.4).
    pub fn clear_target(&mut self, pool: &mut ProtocolPool) {
        self.shown = None;
        self.scroll = 0;
        self.cover.clear();
        self.episodes.reset();
        pool.remove(DETAIL_KEY);
    }

    pub fn scroll_by(&mut self, delta: i64) {
        self.scroll = (self.scroll as i64 + delta).max(0) as u16;
    }

    /// j/k on a detail surface: a landed grid owns the vertical axis (freeze
    /// parity: the cursor steps episodes linearly), else the synopsis scrolls.
    pub fn on_vertical(&mut self, delta: i64) {
        if self.grid_active() {
            self.episodes.cursor_by(delta);
        } else {
            self.scroll_by(delta);
        }
    }

    /// g/G on a detail surface; true when the grid consumed the jump.
    pub fn jump(&mut self, top: bool) -> bool {
        if self.grid_active() {
            self.episodes.cursor_end(top);
            return true;
        }
        false
    }

    fn grid_active(&self) -> bool {
        self.shown
            .as_ref()
            .is_some_and(|e| self.episodes.is_for(e.anilist_id) && self.episodes.has_grid())
    }

    /// Refresh-on-view (04 §10): freshness is judged locally, the network is
    /// spent only on miss/stale. Rides the tick beat next to the cover
    /// reconcile but independent of `cover_art`: metadata heals with covers
    /// off. Single-flight; a spawn refusal retries on the next tick.
    pub fn maybe_enrich(
        &mut self,
        now: Instant,
        tx: &EventTx,
        catalog: &Arc<dyn CatalogProvider>,
        store: &Store,
        now_unix: i64,
    ) {
        if self.enrich_settle.is_armed() && !self.enrich_settle.fire(now) {
            return;
        }
        let Some(id) = self.shown.as_ref().map(|e| e.anilist_id) else {
            return;
        };
        if self.enrich_checked == Some(id) || self.enrich_drain.inflight() > 0 {
            return;
        }
        match store.enrichment_stale(id, now_unix) {
            Ok(true) => {
                if workers::spawn_enrich(&self.enrich_drain, tx.clone(), Arc::clone(catalog), id) {
                    self.enrich_checked = Some(id);
                }
            }
            // A store read error counts as fresh: no basis to spend network.
            Ok(false) | Err(_) => self.enrich_checked = Some(id),
        }
    }

    /// A healed answer replaces the shown snapshot in place (scroll and grid
    /// survive: same show, fresher fields). A stale id is dropped; the store
    /// write already landed in app.
    pub fn on_enrichment(&mut self, for_id: i64, e: &Enrichment) {
        if self.shown.as_ref().map(|s| s.anilist_id) == Some(for_id) {
            self.shown = Some(e.clone());
        }
    }

    /// Cover reconcile (05 §12), called each tick while a detail surface is
    /// visible. The settle window holds during continuous scroll; otherwise
    /// this is the tick retry that un-strands the single-flight gate.
    pub fn maybe_sync(
        &mut self,
        now: Instant,
        cover_art: bool,
        tx: &EventTx,
        caches: &Arc<CoverCaches>,
        covers_dir: &Path,
        pool: &mut ProtocolPool,
    ) {
        if !cover_art {
            return;
        }
        if self.settle.is_armed() && !self.settle.fire(now) {
            return;
        }
        let target_id = self.shown.as_ref().map(|e| e.anilist_id);
        let target_url = self.shown.as_ref().and_then(|e| e.cover_url.as_deref());
        match self.cover.decide(target_id, target_url, now) {
            Action::None | Action::Suppress | Action::UpToDate => {}
            Action::Clear => {
                self.cover.clear();
                pool.remove(DETAIL_KEY);
            }
            Action::Fetch => {
                // Single-flight: one detail fetch at a time; a selection
                // storm defers to the tick retry instead of spawning per
                // keystroke (04 §6 forbids joins on the hot path).
                if self.drain.inflight() > 0 {
                    return;
                }
                let (id, url) = (target_id.unwrap(), target_url.unwrap().to_string());
                self.cover.begin_fetch(id, &url);
                self.started = Some(crate::tui::clock::AsyncStart::new(now));
                let spawned = workers::spawn_cover_fetch(
                    &self.drain,
                    tx.clone(),
                    Arc::clone(caches),
                    covers_dir.to_path_buf(),
                    id,
                    url,
                );
                if !spawned {
                    // No worker will ever answer; a stranded spinner is worse.
                    self.cover.clear();
                    self.started = None;
                }
            }
        }
    }

    /// Dual keep-check (zigoku ROD-156 #2): the state id match alone is blind
    /// to a selection that moved while the single-flight gate deferred, so
    /// the live selection re-validates before install; a miss clears and the
    /// tick retry refetches the live target.
    pub fn on_cover_done(
        &mut self,
        for_id: i64,
        img: image::DynamicImage,
        pool: &mut ProtocolPool,
    ) {
        self.started = None;
        if self.cover.on_done(for_id) {
            if self.shown.as_ref().map(|e| e.anilist_id) == Some(for_id) {
                pool.set_cover(DETAIL_KEY, img);
            } else {
                self.cover.clear();
            }
        }
    }

    /// Cooldown keyed by the id that actually failed (05 §12); no twin
    /// keep-check needed, the render store is untouched.
    pub fn on_cover_error(&mut self, for_id: i64, now: Instant) {
        self.started = None;
        self.cover.on_error(for_id, now);
    }

    /// Drains every worker family; attempts each even when an earlier one
    /// times out so teardown reclaims whatever it can.
    pub fn drain(&self, timeout: Duration) -> bool {
        let covers = self.drain.drain(timeout);
        let enrich = self.enrich_drain.drain(timeout);
        let episodes = self.episodes.drain(timeout);
        covers && enrich && episodes
    }
}

/// The persistent right-hand pane: surface-tier background marks the pane
/// boundary without a border (DESIGN 3.1). `focused` lights the grid cursor;
/// `split_ok` gates the two-column cover|content split (Browse keeps the
/// single stack, History splits past the width gate).
#[allow(clippy::too_many_arguments)]
pub fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DetailState,
    env: &ViewEnv,
    pool: &mut ProtocolPool,
    focused: bool,
    split_ok: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Block::new().style(Style::new().bg(palette.bg)), area);
    draw_content(frame, area, palette, state, env, pool, focused, split_ok);
}

/// The full-screen zoom: the split layout at `DETAIL_TWO_COL_MIN`, single
/// stack below (DESIGN 5.3).
pub fn draw_zoom(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DetailState,
    env: &ViewEnv,
    pool: &mut ProtocolPool,
) {
    let body = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(3),
        ..area
    };
    draw_content(frame, body, palette, state, env, pool, true, true);
}

#[allow(clippy::too_many_arguments)]
fn draw_content(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DetailState,
    env: &ViewEnv,
    pool: &mut ProtocolPool,
    focused: bool,
    split_ok: bool,
) {
    let Some(entry) = state.shown() else {
        return;
    };
    if split_ok && area.width >= DETAIL_TWO_COL_MIN {
        draw_split(frame, area, palette, state, entry, env, pool, focused);
        return;
    }
    let width = area.width as usize;
    let mut y = 1u16;

    // Cover block: flush to the column origin, 1 blank above and below
    // (DESIGN 3.3); the single-column cap keeps the grid's rows safe.
    let cover_w = cover_width(area.width);
    if cover_w > 0 {
        let tier = sizing::Tier {
            large: cover_w == 20,
            cover_w,
            // Unused by height derivation; the detail block has no slot.
            slot_w: cover_w,
        };
        let natural = sizing::detail_cover_h(&tier, pool.cell());
        let cap = cover_height_cap(area.height);
        let cover_h = natural.min(cap);
        if cover_h >= MIN_COVER_ROWS {
            let cover = Rect::new(area.x, area.y + y, cover_w, cover_h);
            draw_cover_block(frame, cover, palette, state, entry, env.now, pool);
            y += cover_h + 1;
        }
    }

    let header = header_lines(entry, palette, env, width);
    for line in &header {
        if y >= area.height {
            break;
        }
        frame.render_widget(
            Paragraph::new(line.clone()),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
        y += 1;
    }

    // Section rule between the score line and the compact meta line (DESIGN
    // 3.6). The provider row rides with the episode grid, not the show info.
    y = draw_hairline(frame, area, palette, y);
    let fields = detail_meta_fields(entry);
    let meta = [meta_line(&fields, palette)];
    for line in &meta {
        if y >= area.height {
            break;
        }
        frame.render_widget(
            Paragraph::new(line.clone()),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
        y += 1;
    }
    y += 1;

    draw_body(frame, area, palette, state, entry, env, y, focused);
}

/// Header stack: title, alt rows, chips row, score line (DESIGN 4.4).
fn header_lines(
    entry: &Enrichment,
    palette: &Palette,
    env: &ViewEnv,
    width: usize,
) -> Vec<Line<'static>> {
    let title = preferred_title(
        &entry.title_romaji,
        entry.title_english.as_deref(),
        entry.title_native.as_deref(),
        env.pref,
    );
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        render::truncate_to_width(title, width).into_owned(),
        Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
    )));
    for (alt, native) in alt_rows(entry, title) {
        let mut style = Style::new().fg(palette.fg2);
        if native {
            style = style.add_modifier(Modifier::ITALIC);
        }
        lines.push(Line::from(Span::styled(
            render::truncate_to_width(alt, width).into_owned(),
            style,
        )));
    }
    if let Some(chips) = chips_line(entry, palette, env) {
        lines.push(chips);
    }
    lines.push(score_line(entry, palette));
    lines
}

/// The two-internal-column form (DESIGN 5.3): cover in the left column,
/// header + meta + synopsis + grid in the right. The left column width feeds
/// the §3.2 cover tier; the cover shares no column with the grid, so the
/// single-column height cap does not apply here.
#[allow(clippy::too_many_arguments)]
fn draw_split(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DetailState,
    entry: &Enrichment,
    env: &ViewEnv,
    pool: &mut ProtocolPool,
    focused: bool,
) {
    let left_w = (area.width * 38 / 100).max(20);
    let cover_w = cover_width(left_w);
    let mut content_x = area.x;
    if cover_w > 0 {
        let tier = sizing::Tier {
            large: cover_w == 20,
            cover_w,
            slot_w: cover_w,
        };
        let cover_h = sizing::detail_cover_h(&tier, pool.cell()).min(area.height.saturating_sub(2));
        if cover_h >= MIN_COVER_ROWS {
            let cover = Rect::new(area.x, area.y + 1, cover_w, cover_h);
            draw_cover_block(frame, cover, palette, state, entry, env.now, pool);
        }
        content_x = area.x + cover_w + SPLIT_GAP;
    }
    let content_w = (area.x + area.width).saturating_sub(content_x);
    if content_w == 0 {
        return;
    }
    let content = Rect::new(content_x, area.y, content_w, area.height);
    let mut y = 1u16;
    for line in header_lines(entry, palette, env, content_w as usize) {
        if y >= content.height {
            return;
        }
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(content.x, content.y + y, content.width, 1),
        );
        y += 1;
    }

    let fields = detail_meta_fields(entry);
    y = draw_hairline(frame, content, palette, y);
    // One compact meta line at every width; the provider row rides with the
    // episode grid, not the show info.
    if y < content.height {
        frame.render_widget(
            Paragraph::new(meta_line(&fields, palette)),
            Rect::new(content.x + 1, content.y + y, content.width - 1, 1),
        );
        y += 1;
    }
    y = draw_hairline(frame, content, palette, y);

    draw_body(frame, content, palette, state, entry, env, y, focused);
}

/// `─` section rule (DESIGN 3.6), chrome tier.
fn draw_hairline(frame: &mut Frame<'_>, area: Rect, palette: &Palette, y: u16) -> u16 {
    if y >= area.height {
        return y;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(area.width as usize),
            Style::new().fg(palette.chrome),
        )),
        Rect::new(area.x, area.y + y, area.width, 1),
    );
    y + 1
}

/// Split the rows under the header between synopsis and grid. With no session
/// engaged the grid reserve still holds its ground so nothing reflows on
/// landing (DESIGN 3.3). With one engaged: the synopsis keeps its natural
/// height when everything fits, else it floors at 2 and yields; the grid
/// scrolls past whatever it still cannot get.
#[allow(clippy::too_many_arguments)]
fn draw_body(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DetailState,
    entry: &Enrichment,
    env: &ViewEnv,
    y: u16,
    focused: bool,
) {
    let remaining = area.height.saturating_sub(y);
    let session = &state.episodes;
    // The grid region renders only while its surface has focus (DESIGN 5.4a,
    // ROD-222): an unfocused pane is a synopsis preview, never a grid.
    if !focused || !session.engaged_for(entry.anilist_id) {
        let cap = synopsis_cap(remaining);
        draw_synopsis(frame, area, palette, entry, state.scroll, y, cap);
        return;
    }
    let syn_natural = entry.description.as_deref().map_or(1, |t| {
        render::wrap_text(t, area.width as usize).len() as u16
    });
    let cols = grid_cols(area.width);
    let grid_rows = (session.grid().len().div_ceil(cols).max(1)) as u16;
    let provider = provider_line(session, palette);
    let (syn_rows, show_provider) =
        body_budget(remaining, syn_natural, grid_rows, provider.is_some());
    draw_synopsis(frame, area, palette, entry, state.scroll, y, syn_rows);
    let mut grid_y = y + syn_rows + 1;
    if show_provider && let Some(pline) = provider {
        frame.render_widget(
            Paragraph::new(pline),
            Rect::new(area.x, area.y + grid_y, area.width, 1),
        );
        grid_y += 1;
    }
    let grid_h = area.height.saturating_sub(grid_y);
    if grid_h == 0 {
        return;
    }
    let grid_area = Rect::new(area.x, area.y + grid_y, area.width, grid_h);
    draw_grid(frame, grid_area, palette, session, entry, env, focused);
}

fn grid_cols(width: u16) -> usize {
    (width / CELL_W).max(1) as usize
}

/// The episode grid region (DESIGN 4.6): spinner while fetching, `no source`
/// when every road is exhausted, blank before any fetch, else the cell rows
/// scrolled to keep the cursor visible.
fn draw_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    session: &EpisodeSession,
    entry: &Enrichment,
    env: &ViewEnv,
    focused: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if let Some(started) = session.loading() {
        let spin = SPINNER[started.frame(env.now, SPINNER.len())];
        let color = if started.is_slow(env.now) {
            palette.hot
        } else {
            palette.focus
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("{spin} loading episodes…"),
                Style::new().fg(color),
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    }
    let grid = session.grid();
    if grid.is_empty() {
        let label = if session.no_source() {
            "no source"
        } else {
            // Engaged and genuinely answered zero; centered + dim like the
            // other non-actionable absences (DESIGN 4.6).
            "no episodes"
        };
        if session.no_source() || session.serving().is_some() {
            render::draw_centered(
                frame,
                area,
                area.height / 2,
                Line::from(Span::styled(
                    label,
                    Style::new().fg(palette.fg3).add_modifier(Modifier::ITALIC),
                )),
            );
        }
        return;
    }
    let cols = grid_cols(area.width);
    let total_rows = grid.len().div_ceil(cols);
    let visible = area.height as usize;
    let cursor_row = session.cursor() / cols;
    let scroll = cursor_row.saturating_sub(visible.saturating_sub(1));
    let aired = aired_count(entry);
    for (r, row) in (scroll..total_rows).take(visible).enumerate() {
        for c in 0..cols {
            let ix = row * cols + c;
            if ix >= grid.len() {
                break;
            }
            let (text, style) = grid_cell(palette, &grid[ix], ix, session, aired, focused, env);
            let x = area.x + c as u16 * CELL_W;
            let w = (area.x + area.width).saturating_sub(x).min(CELL_W);
            frame.render_widget(
                Paragraph::new(Span::styled(text, style)),
                Rect::new(x, area.y + r as u16, w, 1),
            );
        }
    }
}

/// One cell's text + style, by the DESIGN 4.6 precedence: launching >
/// resume > cursor > watched > unaired > unwatched. The launching cell
/// outranks everything: it sits at the user's attention locus and tracks the
/// playing episode, not the cursor.
#[allow(clippy::too_many_arguments)]
fn grid_cell(
    palette: &Palette,
    label: &str,
    ix: usize,
    session: &EpisodeSession,
    aired: Option<u32>,
    focused: bool,
    env: &ViewEnv,
) -> (String, Style) {
    let launching = env
        .play
        .filter(|g| session.is_for(g.anilist_id) && g.cell == ix);
    if let Some(glance) = launching {
        let color = if glance.started.is_slow(env.now) {
            palette.hot
        } else {
            palette.focus
        };
        let spin = SPINNER[glance.started.frame(env.now, SPINNER.len())];
        return (
            format!("[{spin}]"),
            Style::new()
                .bg(palette.surface)
                .fg(color)
                .add_modifier(Modifier::BOLD),
        );
    }
    let resume = session.resume_ix() == Some(ix);
    let style = if resume {
        Style::new()
            .bg(palette.surface)
            .fg(palette.hot)
            .add_modifier(Modifier::BOLD)
    } else if focused && session.cursor() == ix {
        Style::new()
            .bg(palette.surface)
            .fg(palette.focus)
            .add_modifier(Modifier::BOLD)
    } else if (ix as u32) < session.watched() {
        Style::new().fg(palette.fg3).add_modifier(Modifier::DIM)
    } else if aired.is_some_and(|a| ix as u32 >= a) {
        Style::new().fg(palette.fg3).add_modifier(Modifier::ITALIC)
    } else {
        Style::new().fg(palette.fg2)
    };
    (cell_text(label, resume), style)
}

/// `[NN]` in a 5-col slot; the resume arrow only fits labels up to 2 columns
/// (DESIGN 5.3: on wider labels the colour carries resume alone). Labels are
/// provider-controlled and this is one edge where they meet the terminal:
/// hostile codepoints (escapes, bidi, zero-width) are stripped here via the
/// shared ROD-435 filter (ROD-441 review note; Cf widening ROD-439 review).
fn cell_text(label: &str, resume: bool) -> String {
    let clean = domain::strip_controls(label.to_string());
    let width = render::display_width(&clean);
    let core = if width > 3 {
        render::truncate_to_width(&clean, 3)
    } else {
        std::borrow::Cow::Borrowed(clean.as_str())
    };
    if resume && width <= 2 {
        format!("[▸{core}]")
    } else {
        format!("[{core}]")
    }
}

/// Cells past the aired count render in the airing register (DESIGN 4.6);
/// None means everything listed is out. Also clamps the play path's finale
/// judgment in app.rs, so the two never disagree on what is playable.
pub(crate) fn aired_count(entry: &Enrichment) -> Option<u32> {
    if !domain::is_still_airing(entry.status.as_deref()) {
        return None;
    }
    entry.next_airing_episode.map(|n| n.saturating_sub(1))
}

/// One §5.3a metadata field. Show metadata only: the provider row is derived
/// straight from the session by `provider_line`, never routed through here.
struct MetaField {
    value: String,
    unit: Option<&'static str>,
    dim: bool,
}

/// The ordered six-field list (§5.3a), highest priority first. Episodes is
/// the floor and never omitted; every other field omits outright when its
/// value is null.
fn detail_meta_fields(entry: &Enrichment) -> Vec<MetaField> {
    let mut out = Vec::new();
    let (value, dim) = match entry.total_episodes {
        Some(n) => (n.to_string(), false),
        None => ("?".to_string(), true),
    };
    out.push(MetaField {
        value,
        unit: Some("eps"),
        dim,
    });
    let plain = |value: String| MetaField {
        value,
        unit: None,
        dim: false,
    };
    if let Some(label) = render::format_label(entry.kind.as_deref()) {
        out.push(plain(label.to_string()));
    }
    if let Some(src) = entry.source_material.as_deref() {
        out.push(plain(title_case(src)));
    }
    // A 0-minute runtime is a missing value, not a fact (§5.3a).
    if let Some(minutes) = entry.duration_minutes.filter(|m| *m > 0) {
        out.push(plain(format!("{minutes} min")));
    }
    if let Some(studios) = studios_value(&entry.studios) {
        out.push(plain(studios));
    }
    // Rank rides the compact line last (§5.3a), so it is the first field the
    // line sheds when width tightens.
    if let Some(rank) = rank_value(entry) {
        out.push(plain(rank));
    }
    out
}

/// `LIGHT_NOVEL` -> `Light novel` (§5.3a).
fn title_case(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase().replace('_', " ");
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => lower,
    }
}

/// Collapse-format capped at 2 named studios (§5.3a): `A`, `A, B`, `A, B +N`.
fn studios_value(studios: &[String]) -> Option<String> {
    match studios {
        [] => None,
        [a] => Some(a.clone()),
        [a, b] => Some(format!("{a}, {b}")),
        [a, b, rest @ ..] => Some(format!("{a}, {b} +{}", rest.len())),
    }
}

/// `#{rank} rated {year}` from the pre-selected scalar columns (§5.3a); the
/// season name never renders, the header chip already carries it.
fn rank_value(entry: &Enrichment) -> Option<String> {
    let rank = entry.rank?;
    let word = match entry.rank_type.as_deref()? {
        "RATED" => "rated",
        "POPULAR" => "popular",
        _ => return None,
    };
    Some(match entry.rank_year {
        Some(year) => format!("#{rank} {word} {year}"),
        None => format!("#{rank} {word}"),
    })
}

/// One token on the §5.3a provider row. `marker` carries availability as
/// shape; `serving`/`pinned` are independent and each drive their own fg boost
/// in `provider_line`, so a pin-flip miss renders as two elevated tokens.
struct ProviderToken {
    marker: char,
    name: String,
    serving: bool,
    pinned: bool,
}

/// Provider tokens in fixed registry order (§5.3a): `▸` serving, `+` bound,
/// `-` fresh negative, `?` unchecked; shape carries the state, not color.
/// Dim only when nothing is known about any provider. A pin naming a provider
/// outside the registry lands on no token and renders nowhere (§5.3a).
fn provider_value(session: &EpisodeSession) -> Option<(Vec<ProviderToken>, bool)> {
    let avail = session.avail();
    if avail.is_empty() {
        return None;
    }
    let serving = session.serving();
    let pin = session.pin();
    let mut informative = false;
    let tokens: Vec<ProviderToken> = avail
        .iter()
        .map(|(name, availability)| {
            let is_serving = serving == Some(name.as_str());
            let marker = if is_serving {
                informative = true;
                '▸'
            } else {
                match availability {
                    ProviderAvailability::Bound => {
                        informative = true;
                        '+'
                    }
                    ProviderAvailability::Absent => {
                        informative = true;
                        '-'
                    }
                    ProviderAvailability::Unchecked => '?',
                }
            };
            ProviderToken {
                marker,
                name: name.clone(),
                serving: is_serving,
                pinned: pin == Some(name.as_str()),
            }
        })
        .collect();
    Some((tokens, !informative))
}

/// The compact `·`-joined form (§5.3a): non-rail fields only, unit suffixes,
/// separators only between emitted fields (no orphan `·`).
fn meta_line(fields: &[MetaField], palette: &Palette) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for f in fields {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", Style::new().fg(palette.fg3)));
        }
        let style = if f.dim {
            Style::new().fg(palette.fg3)
        } else {
            Style::new().fg(palette.fg2)
        };
        let text = match f.unit {
            Some(unit) => format!("{} {unit}", f.value),
            None => f.value.clone(),
        };
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// The dedicated provider row under the compact line (§5.3a). Pin and serving
/// are per-token boosts on the luminance ladder, never a text segment: serving
/// takes `fg`, pinned takes `fg` + bold, every other token stays `fg2`.
///
/// The pin boost is gated on a confirmed marker (`▸` or `+`): a pin on an
/// unchecked or absent provider gets no lift, so `?` can never visually
/// outrank a `+`. That gate is also what keeps the nothing-known row honest,
/// since every token there is `?` and none can qualify.
fn provider_line(session: &EpisodeSession, palette: &Palette) -> Option<Line<'static>> {
    let (tokens, dim) = provider_value(session)?;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for token in tokens {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        let style = if dim {
            Style::new().fg(palette.fg3)
        } else if token.pinned && matches!(token.marker, '▸' | '+') {
            Style::new().fg(palette.fg).add_modifier(Modifier::BOLD)
        } else if token.serving {
            Style::new().fg(palette.fg)
        } else {
            Style::new().fg(palette.fg2)
        };
        spans.push(Span::styled(
            format!("{}{}", token.marker, token.name),
            style,
        ));
    }
    // Cycle-provider affordance (the `v` key), keybind-hint styling (§7.5).
    // Hint bold and state bold are separate registers and coexist here (§1.3).
    spans.push(Span::styled(" · [", Style::new().fg(palette.fg3)));
    spans.push(Span::styled(
        "v",
        Style::new().fg(palette.fg2).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled("]", Style::new().fg(palette.fg3)));
    Some(Line::from(spans))
}

/// Cover cell states (DESIGN 8.1): image, fetch spinner, or the `no art yet`
/// absent state when there is nothing to fetch. The two must never conflate.
fn draw_cover_block(
    frame: &mut Frame<'_>,
    cover: Rect,
    palette: &Palette,
    state: &DetailState,
    entry: &Enrichment,
    now: Instant,
    pool: &mut ProtocolPool,
) {
    if state.cover.has_pixels() && pool.render_cover(frame, cover, DETAIL_KEY) {
        return;
    }
    frame.render_widget(Block::new().style(Style::new().bg(palette.surface)), cover);
    let mid = cover.height / 2;
    if let Some(started) = state.started.filter(|_| state.cover.is_loading()) {
        let spin = SPINNER[started.frame(now, SPINNER.len())];
        let color = if started.is_slow(now) {
            palette.hot
        } else {
            palette.focus
        };
        render::draw_centered(
            frame,
            cover,
            mid,
            Line::from(Span::styled(spin.to_string(), Style::new().fg(color))),
        );
    } else if entry.cover_url.is_none() {
        render::draw_centered(
            frame,
            cover,
            mid,
            Line::from(Span::styled(
                "no art yet",
                Style::new().fg(palette.fg3).add_modifier(Modifier::ITALIC),
            )),
        );
    }
}

fn draw_synopsis(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    entry: &Enrichment,
    scroll: u16,
    y: u16,
    cap: u16,
) {
    if cap == 0 {
        return;
    }
    let width = area.width as usize;
    let Some(text) = entry.description.as_deref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no synopsis yet",
                Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
            )),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
        return;
    };
    let lines = render::wrap_text(text, width);
    let scroll = (scroll as usize).min(lines.len().saturating_sub(1));
    let truncated = lines.len() - scroll > cap as usize;
    for (i, line) in lines.iter().skip(scroll).take(cap as usize).enumerate() {
        let is_last_visible = i as u16 == cap - 1;
        let mut spans = vec![Span::styled(line.clone(), Style::new().fg(palette.fg2))];
        if truncated && is_last_visible {
            spans.push(Span::styled(
                "…",
                Style::new().fg(palette.fg3).add_modifier(Modifier::ITALIC),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, area.y + y + i as u16, area.width, 1),
        );
    }
}

/// Alt-title rows (DESIGN 4.4, 8.2): the two forms that are not the resolved
/// primary, in romaji, english, native order, skipped when null or byte-equal
/// to the primary. Native is italic whenever it is an alt, never as primary.
fn alt_rows<'a>(entry: &'a Enrichment, primary: &str) -> Vec<(&'a str, bool)> {
    let forms: [(Option<&str>, bool); 3] = [
        (Some(entry.title_romaji.as_str()), false),
        (entry.title_english.as_deref(), false),
        (entry.title_native.as_deref(), true),
    ];
    forms
        .into_iter()
        .filter_map(|(form, native)| form.map(|f| (f, native)))
        .filter(|(f, _)| !f.is_empty() && *f != primary)
        .collect()
}

/// Kanji chip vocabulary (DESIGN 2.3); the English fallback renders when the
/// kanji_chips toggle is off.
fn status_chip(status: &str, kanji: bool) -> Option<(&'static str, ChipTone)> {
    let (kanji_label, english, tone) = match status.to_ascii_uppercase().as_str() {
        "RELEASING" => ("放映中", "AIRING", ChipTone::Hot),
        "FINISHED" => ("完結", "DONE", ChipTone::Muted),
        "NOT_YET_RELEASED" => ("放映前", "SOON", ChipTone::Focus),
        "HIATUS" => ("休止中", "HIATUS", ChipTone::Warn),
        "CANCELLED" => ("中止", "DROPPED", ChipTone::Dim),
        _ => return None,
    };
    Some((if kanji { kanji_label } else { english }, tone))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChipTone {
    Hot,
    Muted,
    Focus,
    Warn,
    Dim,
}

impl ChipTone {
    fn style(self, palette: &Palette) -> Style {
        match self {
            ChipTone::Hot => Style::new().fg(palette.hot),
            ChipTone::Muted => Style::new().fg(palette.fg2),
            ChipTone::Focus => Style::new().fg(palette.focus),
            ChipTone::Warn => Style::new().fg(palette.warn),
            ChipTone::Dim => Style::new().fg(palette.fg3),
        }
    }
}

/// The chips row (DESIGN 4.4): status, season+year, airing countdown, non-JP
/// origin marker; each omitted when absent, the row skipped when all are.
fn chips_line<'a>(entry: &Enrichment, palette: &Palette, env: &ViewEnv) -> Option<Line<'a>> {
    let mut spans: Vec<Span<'_>> = Vec::new();
    let push = |spans: &mut Vec<Span<'a>>, text: String, style: Style| {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(text, style));
    };
    if let Some((label, tone)) = entry
        .status
        .as_deref()
        .and_then(|s| status_chip(s, env.kanji))
    {
        push(&mut spans, label.to_string(), tone.style(palette));
    }
    if let Some(chip) = season_chip_text(entry.season, entry.year, env.kanji) {
        push(&mut spans, chip, Style::new().fg(palette.focus));
    }
    if let Some(countdown) = countdown_label(
        entry.next_airing_at,
        entry.next_airing_episode,
        env.unix_now,
    ) {
        push(&mut spans, countdown, Style::new().fg(palette.hot));
    }
    if let Some(country) = entry.country.as_deref().filter(|c| *c != "JP") {
        push(
            &mut spans,
            country.to_string(),
            Style::new().fg(palette.fg3),
        );
    }
    if spans.is_empty() {
        None
    } else {
        Some(Line::from(spans))
    }
}

fn season_chip_text(season: Option<Season>, year: Option<u32>, kanji: bool) -> Option<String> {
    if kanji {
        return render::season_chip(season, year);
    }
    let name = match season? {
        Season::Winter => "Winter",
        Season::Spring => "Spring",
        Season::Summer => "Summer",
        Season::Fall => "Autumn",
    };
    Some(format!("{name} {}", year?))
}

/// `Ep14 · 3d` in the airing register (DESIGN 4.4): one coarsest unit, never
/// combined; a lapsed countdown is omitted, never negative. Recomputed from
/// the persisted absolute `airingAt`, so it survives long sessions.
fn countdown_label(airing_at: Option<i64>, episode: Option<u32>, now: i64) -> Option<String> {
    let at = airing_at?;
    let ep = episode?;
    let delta = at - now;
    if delta <= 0 {
        return None;
    }
    let unit = if delta >= 86_400 {
        format!("{}d", delta / 86_400)
    } else if delta >= 3_600 {
        format!("{}h", delta / 3_600)
    } else {
        format!("{}m", (delta / 60).max(1))
    };
    Some(format!("Ep{ep} · {unit}"))
}

/// `✦ [93/100] · Adventure · Drama` (DESIGN 4.3); `[--/100]` for null.
fn score_line<'a>(entry: &Enrichment, palette: &Palette) -> Line<'a> {
    let mut spans: Vec<Span<'_>> = Vec::new();
    let style = render::score_style(palette, entry.score, false);
    match entry.score {
        Some(s) if s >= 91 => spans.push(Span::styled(format!("✦ [{s}/100]"), style)),
        Some(s) => spans.push(Span::styled(format!("[{s}/100]"), style)),
        None => spans.push(Span::styled("[--/100]".to_string(), style)),
    }
    for genre in &entry.genres {
        spans.push(Span::styled(
            " · ".to_string(),
            Style::new().fg(palette.fg3),
        ));
        spans.push(Span::styled(genre.clone(), Style::new().fg(palette.fg2)));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TitleLanguage;
    use crate::tui::event;
    use ratatui_image::picker::Picker;

    /// The focused-grid budget: the grid keeps >= 2 rows when the pane can
    /// hold them, and the provider caption is never orphaned above an empty
    /// grid at tight heights (ROD-458 review).
    #[test]
    fn body_budget_keeps_grid_rows_and_never_orphans_provider() {
        // Roomy: full synopsis, caption shown, grid keeps its rows.
        let (syn, prov) = body_budget(30, 8, 4, true);
        assert!(prov);
        let grid_h = 30 - (syn + 1 + 1);
        assert!(grid_h >= 2, "grid keeps >= 2 rows, got {grid_h}");
        // Tight: caption yields rather than sit above an empty grid.
        for remaining in 0u16..=8 {
            let (syn, show) = body_budget(remaining, 8, 4, true);
            let grid_y = syn + 1 + u16::from(show);
            let grid_h = remaining.saturating_sub(grid_y);
            assert!(
                !show || grid_h >= 1,
                "remaining {remaining}: caption orphaned (grid_h {grid_h})"
            );
        }
        // No provider row: never claims to show one.
        assert!(!body_budget(30, 8, 4, false).1);
    }

    fn entry(id: i64) -> Enrichment {
        Enrichment {
            anilist_id: id,
            title_romaji: format!("Show {id}"),
            cover_url: Some(format!("http://127.0.0.1:9/{id}.png")),
            ..Enrichment::default()
        }
    }

    fn full_entry() -> Enrichment {
        Enrichment {
            anilist_id: 1,
            title_romaji: "Sousou no Frieren".into(),
            title_english: Some("Frieren: Beyond Journey's End".into()),
            title_native: Some("葬送のフリーレン".into()),
            ..entry(1)
        }
    }

    fn pool() -> (ProtocolPool, event::EventRx) {
        let (tx, rx) = event::channel();
        let drain = Drain::default();
        (ProtocolPool::new(Picker::halfblocks(), tx, &drain), rx)
    }

    fn img() -> image::DynamicImage {
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 3))
    }

    fn deps() -> (
        EventTx,
        event::EventRx,
        Arc<CoverCaches>,
        std::path::PathBuf,
    ) {
        let (tx, rx) = event::channel();
        let dir = std::env::temp_dir().join("sabigoku-detail-tests");
        (tx, rx, Arc::new(CoverCaches::new()), dir)
    }

    /// The DESIGN 3.3 worst case: 35-row terminal, pane height 32, no pixel
    /// geometry. The 28-row aesthetic cap must lose to the reserve so the
    /// grid keeps its 2 rows.
    #[test]
    fn cover_cap_protects_the_grid_at_the_worst_case() {
        assert_eq!(cover_height_cap(32), 18);
        // 32 - blank - 18 cover - blank = 12 rows: worst header (7 lines
        // incl. meta + provider row) + synopsis 2 + grid spacer + 2 grid rows.
        assert!(cover_height_cap(32) <= 32 - COVER_RESERVE);
        assert_eq!(cover_height_cap(19), 0, "below 6 rows the poster drops");
        assert_eq!(synopsis_cap(5), 2);
        assert_eq!(synopsis_cap(3), 0);
    }

    #[test]
    fn grid_cell_text_shapes_and_strips_control_bytes() {
        assert_eq!(cell_text("7", false), "[7]");
        assert_eq!(cell_text("24", false), "[24]");
        assert_eq!(cell_text("128", false), "[128]");
        assert_eq!(cell_text("7", true), "[▸7]");
        assert_eq!(cell_text("24", true), "[▸24]");
        assert_eq!(cell_text("128", true), "[128]", "3-wide drops the arrow");
        assert_eq!(cell_text("13.5", false), "[13…]", "over-wide truncates");
        // Provider-controlled label with an escape sequence: stripped at the
        // render edge, never written into the buffer.
        assert_eq!(cell_text("\x1b]0;x\x077", false), "[]0…]");
        assert!(!cell_text("\x1b[31m1", false).contains('\x1b'));
        // The Cf class (bidi overrides, zero-width) is stripped too; it is
        // not `is_control()` and needs the ROD-435 filter (final-gate
        // review reproduced the gap).
        assert_eq!(cell_text("\u{202E}1", false), "[1]");
        assert_eq!(cell_text("\u{200B}2\u{FEFF}", false), "[2]");
    }

    #[test]
    fn aired_count_only_marks_releasing_shows() {
        let mut e = entry(1);
        e.status = Some("RELEASING".into());
        e.next_airing_episode = Some(15);
        assert_eq!(aired_count(&e), Some(14));
        e.status = Some("FINISHED".into());
        assert_eq!(aired_count(&e), None);
        e.status = Some("RELEASING".into());
        e.next_airing_episode = None;
        assert_eq!(aired_count(&e), None);
    }

    #[test]
    fn grid_cols_floor_at_one() {
        assert_eq!(grid_cols(60), 12);
        assert_eq!(grid_cols(25), 5);
        assert_eq!(grid_cols(3), 1);
    }

    fn rich_entry() -> Enrichment {
        Enrichment {
            total_episodes: Some(28),
            kind: Some("TV".into()),
            source_material: Some("LIGHT_NOVEL".into()),
            duration_minutes: Some(24),
            studios: vec!["Madhouse".into()],
            rank: Some(12),
            rank_type: Some("RATED".into()),
            rank_year: Some(2023),
            ..entry(1)
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn meta_fields_walk_the_priority_order() {
        let fields = detail_meta_fields(&rich_entry());
        let values: Vec<&str> = fields.iter().map(|f| f.value.as_str()).collect();
        assert_eq!(
            values,
            [
                "28",
                "TV",
                "Light novel",
                "24 min",
                "Madhouse",
                "#12 rated 2023"
            ],
            "Episodes, Format, Source, Duration, Studios, Rank"
        );
        assert!(
            fields.iter().all(|f| !f.value.contains("megaplay")),
            "the provider row is session-derived and never enters the field list"
        );
    }

    #[test]
    fn meta_fields_floor() {
        // Bare entry: only the dim Episodes floor.
        let fields = detail_meta_fields(&entry(1));
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].value, "?");
        assert!(fields[0].dim);
    }

    #[test]
    fn provider_tokens_carry_shape_and_boost_flags() {
        use crate::store::ProviderAvailability::{Absent, Bound, Unchecked};
        let session = EpisodeSession::seeded(
            1,
            Some("megaplay"),
            Some("senshi"),
            vec![
                ("megaplay".into(), Bound),
                ("senshi".into(), Absent),
                ("allanime".into(), Unchecked),
            ],
            vec!["1".into()],
        );
        let (tokens, dim) = provider_value(&session).unwrap();
        // Serving outranks bound; absence and unchecked keep their shapes.
        let shapes: Vec<String> = tokens
            .iter()
            .map(|t| format!("{}{}", t.marker, t.name))
            .collect();
        assert_eq!(shapes, ["▸megaplay", "-senshi", "?allanime"]);
        assert!(!dim);
        assert!(tokens[0].serving && !tokens[0].pinned);
        assert!(
            tokens[1].pinned && !tokens[1].serving,
            "pin rides its own token"
        );

        // Nothing known anywhere: tokens still emit, row dims.
        let bare = EpisodeSession::seeded(
            1,
            None,
            None,
            vec![("megaplay".into(), Unchecked)],
            vec!["1".into()],
        );
        let (tokens, dim) = provider_value(&bare).unwrap();
        assert_eq!(tokens[0].marker, '?');
        assert!(dim, "all-unchecked dims the row");
        assert!(!tokens[0].pinned, "unpinned");
    }

    #[test]
    fn meta_line_joins_without_orphan_separators() {
        let palette = &crate::tui::theme::TERMINAL_GHOST;
        let sparse = line_text(&meta_line(&detail_meta_fields(&entry(1)), palette));
        assert_eq!(sparse, "? eps", "single field carries no separator");
        let full = line_text(&meta_line(&detail_meta_fields(&rich_entry()), palette));
        assert_eq!(
            full, "28 eps · TV · Light novel · 24 min · Madhouse · #12 rated 2023",
            "Rank rides the compact line, last"
        );
        assert!(
            !full.contains("megaplay"),
            "Provider/Pinned stay off the line (they ride the grid)"
        );
    }

    /// The style of the span rendering `text`, for the fg-ladder asserts.
    fn token_style(line: &Line<'_>, text: &str) -> Style {
        line.spans
            .iter()
            .find(|s| s.content == text)
            .unwrap_or_else(|| panic!("no span {text:?} in {:?}", line_text(line)))
            .style
    }

    fn is_bold(style: Style) -> bool {
        style.add_modifier.contains(Modifier::BOLD)
    }

    #[test]
    fn provider_row_encodes_pin_and_serving_on_the_ladder() {
        use crate::store::ProviderAvailability::Bound;
        let palette = &crate::tui::theme::TERMINAL_GHOST;
        // Pin/serving divergence: two elevated tokens, no pin text anywhere.
        let split = EpisodeSession::seeded(
            1,
            Some("megaplay"),
            Some("senshi"),
            vec![("megaplay".into(), Bound), ("senshi".into(), Bound)],
            vec!["1".into()],
        );
        let line = provider_line(&split, palette).unwrap();
        assert_eq!(line_text(&line), "▸megaplay +senshi · [v]");
        let serving = token_style(&line, "▸megaplay");
        assert_eq!(serving.fg, Some(palette.fg));
        assert!(!is_bold(serving), "serving lifts fg only");
        let pinned = token_style(&line, "+senshi");
        assert_eq!(pinned.fg, Some(palette.fg));
        assert!(is_bold(pinned), "the pin is the bold token");

        // Serving and pinned on one token: one boosted span, still bold.
        let together = EpisodeSession::seeded(
            1,
            Some("megaplay"),
            Some("megaplay"),
            vec![("megaplay".into(), Bound), ("senshi".into(), Bound)],
            vec!["1".into()],
        );
        let line = provider_line(&together, palette).unwrap();
        assert_eq!(line_text(&line), "▸megaplay +senshi · [v]");
        assert!(is_bold(token_style(&line, "▸megaplay")));
        assert_eq!(token_style(&line, "+senshi").fg, Some(palette.fg2));

        // Unpinned: the serving token lifts, nothing goes bold but the hint.
        let unpinned = EpisodeSession::seeded(
            1,
            Some("megaplay"),
            None,
            vec![("megaplay".into(), Bound)],
            vec!["1".into()],
        );
        let line = provider_line(&unpinned, palette).unwrap();
        assert_eq!(
            line_text(&line),
            "▸megaplay · [v]",
            "cycle hint always trails"
        );
        assert!(!is_bold(token_style(&line, "▸megaplay")));
        assert!(
            is_bold(token_style(&line, "v")),
            "hint bold is its own register"
        );

        // No engaged session: the whole row is skipped.
        assert!(provider_line(&EpisodeSession::default(), palette).is_none());
    }

    #[test]
    fn pin_boost_needs_a_confirmed_marker() {
        use crate::store::ProviderAvailability::{Absent, Bound, Unchecked};
        let palette = &crate::tui::theme::TERMINAL_GHOST;
        // A pin on a still-unchecked provider must never outrank the bound one.
        let unchecked = EpisodeSession::seeded(
            1,
            None,
            Some("senshi"),
            vec![("megaplay".into(), Bound), ("senshi".into(), Unchecked)],
            vec!["1".into()],
        );
        let line = provider_line(&unchecked, palette).unwrap();
        let pin = token_style(&line, "?senshi");
        assert!(!is_bold(pin), "? never lifts");
        assert_eq!(pin.fg, Some(palette.fg2));
        assert_eq!(token_style(&line, "+megaplay").fg, Some(palette.fg2));

        // Same gate on a provider confirmed absent.
        let absent = EpisodeSession::seeded(
            1,
            None,
            Some("senshi"),
            vec![("senshi".into(), Absent)],
            vec!["1".into()],
        );
        assert!(!is_bold(token_style(
            &provider_line(&absent, palette).unwrap(),
            "-senshi"
        )));

        // Nothing known: the row dims whole and the gate leaves it flat.
        let bare = EpisodeSession::seeded(
            1,
            None,
            Some("megaplay"),
            vec![("megaplay".into(), Unchecked)],
            vec!["1".into()],
        );
        let line = provider_line(&bare, palette).unwrap();
        let token = token_style(&line, "?megaplay");
        assert_eq!(token.fg, Some(palette.fg3));
        assert!(!is_bold(token), "the dim row admits no boost");

        // A pin outside the registry renders nowhere at all.
        let retired = EpisodeSession::seeded(
            1,
            Some("megaplay"),
            Some("gone"),
            vec![("megaplay".into(), Bound)],
            vec!["1".into()],
        );
        let line = provider_line(&retired, palette).unwrap();
        assert_eq!(line_text(&line), "▸megaplay · [v]");
        assert!(!is_bold(token_style(&line, "▸megaplay")));
    }

    #[test]
    fn meta_value_formatters() {
        assert_eq!(title_case("LIGHT_NOVEL"), "Light novel");
        assert_eq!(title_case("ORIGINAL"), "Original");
        assert_eq!(studios_value(&[]), None);
        assert_eq!(studios_value(&["A".into()]).unwrap(), "A");
        assert_eq!(studios_value(&["A".into(), "B".into()]).unwrap(), "A, B");
        assert_eq!(
            studios_value(&["A".into(), "B".into(), "C".into(), "D".into()]).unwrap(),
            "A, B +2"
        );
        let mut e = rich_entry();
        e.rank_year = None;
        assert_eq!(rank_value(&e).unwrap(), "#12 rated");
        e.rank_type = Some("POPULAR".into());
        assert_eq!(rank_value(&e).unwrap(), "#12 popular");
        e.rank_type = Some("WEIRD".into());
        assert_eq!(rank_value(&e), None, "unknown ranking type omits");
        e.rank_type = None;
        assert_eq!(rank_value(&e), None);
    }

    #[test]
    fn cover_width_tiers_follow_the_column() {
        assert_eq!(cover_width(45), 20);
        assert_eq!(cover_width(40), 20);
        assert_eq!(cover_width(39), 14);
        assert_eq!(cover_width(25), 14);
        assert_eq!(cover_width(24), 0);
    }

    #[test]
    fn countdown_collapses_to_one_coarsest_unit() {
        assert_eq!(
            countdown_label(Some(1000 + 3 * 86_400), Some(14), 1000),
            Some("Ep14 · 3d".into())
        );
        assert_eq!(
            countdown_label(Some(1000 + 7_200), Some(2), 1000),
            Some("Ep2 · 2h".into())
        );
        assert_eq!(
            countdown_label(Some(1000 + 90), Some(2), 1000),
            Some("Ep2 · 1m".into())
        );
        assert_eq!(
            countdown_label(Some(999), Some(2), 1000),
            None,
            "lapsed omits"
        );
        assert_eq!(countdown_label(None, Some(2), 1000), None);
    }

    #[test]
    fn alt_rows_exclude_the_resolved_primary_and_dedup() {
        let e = full_entry();
        let romaji_primary = preferred_title(
            &e.title_romaji,
            e.title_english.as_deref(),
            e.title_native.as_deref(),
            TitleLanguage::Romaji,
        );
        let alts = alt_rows(&e, romaji_primary);
        assert_eq!(alts.len(), 2);
        assert_eq!(alts[0].0, "Frieren: Beyond Journey's End");
        assert!(!alts[0].1);
        assert_eq!(alts[1].0, "葬送のフリーレン");
        assert!(alts[1].1, "native is italic as an alt");

        let native_primary = preferred_title(
            &e.title_romaji,
            e.title_english.as_deref(),
            e.title_native.as_deref(),
            TitleLanguage::Native,
        );
        let alts = alt_rows(&e, native_primary);
        assert!(
            alts.iter().all(|(_, native)| !native),
            "primary never dups into alts"
        );

        // english pref with a null english form resolves to romaji; the
        // fallback target must not duplicate into its own alt row.
        let mut sparse = full_entry();
        sparse.title_english = None;
        let resolved = preferred_title(
            &sparse.title_romaji,
            None,
            sparse.title_native.as_deref(),
            TitleLanguage::English,
        );
        assert_eq!(resolved, "Sousou no Frieren");
        let alts = alt_rows(&sparse, resolved);
        assert_eq!(alts.len(), 1);
        assert!(alts[0].1);
    }

    #[test]
    fn status_chips_map_and_fall_back_to_english() {
        assert_eq!(status_chip("RELEASING", true).unwrap().0, "放映中");
        assert_eq!(status_chip("releasing", false).unwrap().0, "AIRING");
        assert_eq!(status_chip("FINISHED", false).unwrap().0, "DONE");
        assert!(status_chip("SOMETHING_NEW", true).is_none());
    }

    #[test]
    fn set_target_resets_scroll_only_on_a_new_show() {
        let mut d = DetailState::default();
        let now = Instant::now();
        d.set_target(&entry(1), true, now);
        d.scroll_by(3);
        d.set_target(&entry(1), false, now);
        assert_eq!(d.scroll, 3, "same show keeps the scroll");
        d.set_target(&entry(2), true, now);
        assert_eq!(d.scroll, 0, "new show resets");
    }

    struct CountingCatalog(std::sync::Mutex<Vec<i64>>);

    impl CatalogProvider for CountingCatalog {
        fn search(
            &self,
            _q: &str,
            _p: u32,
        ) -> Result<crate::providers::CatalogPage, crate::providers::CatalogError> {
            Err(crate::providers::CatalogError::Network)
        }
        fn discover(
            &self,
            _a: crate::providers::DiscoverAxis,
            _p: u32,
        ) -> Result<crate::providers::CatalogPage, crate::providers::CatalogError> {
            Err(crate::providers::CatalogError::Network)
        }
        fn enrich(&self, id: i64) -> Result<Option<Enrichment>, crate::providers::CatalogError> {
            self.0.lock().unwrap().push(id);
            Ok(None)
        }
    }

    #[test]
    fn continuous_scroll_settles_before_enriching_then_decides_once() {
        let mut d = DetailState::default();
        let (tx, _rx, _caches, _dir) = deps();
        let store = Store::open_memory().unwrap();
        let counting = Arc::new(CountingCatalog(std::sync::Mutex::new(Vec::new())));
        let catalog: Arc<dyn CatalogProvider> = counting.clone();
        let t0 = Instant::now();
        d.set_target(&entry(1), false, t0);
        d.maybe_enrich(t0, &tx, &catalog, &store, 1_000);
        assert!(counting.0.lock().unwrap().is_empty(), "settle window holds");
        d.maybe_enrich(t0 + COVER_SETTLE, &tx, &catalog, &store, 1_000);
        assert!(d.drain(Duration::from_secs(5)));
        assert_eq!(
            *counting.0.lock().unwrap(),
            vec![1],
            "a store miss fires after settle"
        );
        d.maybe_enrich(t0 + COVER_SETTLE * 2, &tx, &catalog, &store, 1_000);
        assert!(d.drain(Duration::from_secs(5)));
        assert_eq!(
            counting.0.lock().unwrap().len(),
            1,
            "one decision per shown id"
        );

        // A fresh cache row spends nothing.
        store
            .upsert_catalog_cache(&entry(2), 1_000, Some(2_000))
            .unwrap();
        d.set_target(&entry(2), true, t0);
        d.maybe_enrich(t0, &tx, &catalog, &store, 1_000);
        assert_eq!(counting.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn continuous_scroll_settles_before_fetching() {
        let mut d = DetailState::default();
        let (tx, _rx, caches, dir) = deps();
        let (mut p, _prx) = pool();
        let t0 = Instant::now();
        d.set_target(&entry(1), false, t0);
        d.maybe_sync(t0, true, &tx, &caches, &dir, &mut p);
        assert!(
            !d.cover.is_loading(),
            "inside the settle window nothing fires"
        );
        d.maybe_sync(t0 + COVER_SETTLE, true, &tx, &caches, &dir, &mut p);
        assert!(d.cover.is_loading(), "settle expiry fires the fetch");
        assert!(d.drain(Duration::from_secs(5)));
    }

    #[test]
    fn discrete_nav_syncs_immediately() {
        let mut d = DetailState::default();
        let (tx, _rx, caches, dir) = deps();
        let (mut p, _prx) = pool();
        let t0 = Instant::now();
        d.set_target(&entry(1), true, t0);
        d.maybe_sync(t0, true, &tx, &caches, &dir, &mut p);
        assert!(d.cover.is_loading());
        assert!(d.drain(Duration::from_secs(5)));
    }

    #[test]
    fn cover_art_off_never_fetches() {
        let mut d = DetailState::default();
        let (tx, _rx, caches, dir) = deps();
        let (mut p, _prx) = pool();
        let t0 = Instant::now();
        d.set_target(&entry(1), true, t0);
        d.maybe_sync(t0, false, &tx, &caches, &dir, &mut p);
        assert!(!d.cover.is_loading());
        assert_eq!(d.drain.inflight(), 0);
    }

    /// Ports the ROD-438 App-level contract: a result for a selection that
    /// moved must never install; the state clears for the tick retry.
    #[test]
    fn stale_cover_never_installs_for_a_moved_selection() {
        let mut d = DetailState::default();
        let (tx, _rx, caches, dir) = deps();
        let (mut p, _prx) = pool();
        let t0 = Instant::now();
        d.set_target(&entry(1), true, t0);
        d.maybe_sync(t0, true, &tx, &caches, &dir, &mut p);
        assert!(d.cover.is_loading());
        // Selection moves while the fetch is in flight.
        d.set_target(&entry(2), true, t0);
        d.on_cover_done(1, img(), &mut p);
        assert!(!p.contains(DETAIL_KEY), "stale art must not install");
        assert!(!d.cover.has_pixels());
        assert!(d.drain(Duration::from_secs(5)));
    }

    #[test]
    fn matching_cover_installs() {
        let mut d = DetailState::default();
        let (tx, _rx, caches, dir) = deps();
        let (mut p, _prx) = pool();
        let t0 = Instant::now();
        d.set_target(&entry(1), true, t0);
        d.maybe_sync(t0, true, &tx, &caches, &dir, &mut p);
        d.on_cover_done(1, img(), &mut p);
        assert!(d.cover.has_pixels());
        assert!(p.contains(DETAIL_KEY));
        assert!(d.drain(Duration::from_secs(5)));
    }

    #[test]
    fn single_flight_defers_a_selection_storm() {
        let mut d = DetailState::default();
        let (tx, _rx, caches, dir) = deps();
        let (mut p, _prx) = pool();
        let t0 = Instant::now();
        let held = d.drain.begin();
        for id in 1..=5 {
            d.set_target(&entry(id), true, t0);
            d.maybe_sync(t0, true, &tx, &caches, &dir, &mut p);
        }
        assert_eq!(d.drain.inflight(), 1, "only the held guard");
        assert!(!d.cover.is_loading(), "gate defers, never spawns per key");
        drop(held);
        // The tick retry fetches for the CURRENT selection, not the storm's.
        d.maybe_sync(
            t0 + Duration::from_secs(1),
            true,
            &tx,
            &caches,
            &dir,
            &mut p,
        );
        assert!(d.cover.is_loading());
        assert_eq!(d.cover.for_id(), Some(5));
        assert!(d.drain(Duration::from_secs(5)));
    }

    #[test]
    fn clear_target_wipes_pane_and_pool() {
        let mut d = DetailState::default();
        let (tx, _rx, caches, dir) = deps();
        let (mut p, _prx) = pool();
        let t0 = Instant::now();
        d.set_target(&entry(1), true, t0);
        d.maybe_sync(t0, true, &tx, &caches, &dir, &mut p);
        d.on_cover_done(1, img(), &mut p);
        d.clear_target(&mut p);
        assert!(d.shown().is_none());
        assert!(!p.contains(DETAIL_KEY));
        assert!(d.drain(Duration::from_secs(5)));
    }
}
