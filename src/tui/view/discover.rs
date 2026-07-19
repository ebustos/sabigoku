//! Discover: full-canvas card grid over four AniList axes (DESIGN 3.8, 8.6;
//! 04 §7.5). This module owns its feed and cover transport: slots, drains,
//! and the pump live here, never on App (ROD-439 architecture). App routes
//! events and keys in; deps (store, pool, catalog) arrive as scoped borrows.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui_image::FontSize;

use crate::domain::{Cour, Enrichment, TitleLanguage, is_still_airing, preferred_title};
use crate::providers::{CatalogProvider, DiscoverAxis};
use crate::store::{Store, enrichment_ttl_secs};
use crate::tui::clock::AsyncStart;
use crate::tui::covers::discover::DiscoverCovers;
use crate::tui::covers::render::ProtocolPool;
use crate::tui::covers::{CoverCaches, DETAIL_KEY, sizing};
use crate::tui::event::EventTx;
use crate::tui::render::{self, SPINNER};
use crate::tui::theme::Palette;
use crate::tui::view::ViewEnv;
use crate::tui::workers::{self, Drain};

/// Freeze enum order; UI tab order matches (04 §7.5).
pub const AXES: [DiscoverAxis; 4] = [
    DiscoverAxis::Trending,
    DiscoverAxis::Popular,
    DiscoverAxis::TopRated,
    DiscoverAxis::ThisSeason,
];

/// Force-exhaustion rail (ROD-339).
pub const MAX_FEED_ROWS: usize = 300;
/// Prefetch when the cursor comes within this many card-rows of the last
/// loaded entry (DESIGN 8.6).
const PREFETCH_ROWS: usize = 2;
/// Peek band renders only when at least this tall (DESIGN 3.8).
const MIN_PEEK_ROWS: u16 = 3;

pub fn axis_label(axis: DiscoverAxis) -> &'static str {
    match axis {
        DiscoverAxis::Trending => "Trending",
        DiscoverAxis::Popular => "Popular",
        DiscoverAxis::TopRated => "Top Rated",
        DiscoverAxis::ThisSeason => "This Season",
    }
}

fn axis_idx(axis: DiscoverAxis) -> usize {
    AXES.iter().position(|a| *a == axis).unwrap_or(0)
}

/// The §3.8a vocabulary. This table is required to match DESIGN §3.8a exactly;
/// edit both together (drift is rot). Unmapped genres are silently skipped.
pub const GENRE_GLYPHS: [(&str, &str); 18] = [
    ("Action", "⚔"),
    ("Adventure", "⚑"),
    ("Comedy", "☺"),
    ("Drama", "◆"),
    ("Ecchi", "♨"),
    ("Fantasy", "⚜"),
    ("Horror", "☠"),
    ("Mahou Shoujo", "✿"),
    ("Mecha", "⚙"),
    ("Music", "♪"),
    ("Mystery", "◈"),
    ("Psychological", "◐"),
    ("Romance", "♥"),
    ("Sci-Fi", "⬡"),
    ("Slice of Life", "❖"),
    ("Sports", "◎"),
    ("Supernatural", "☽"),
    ("Thriller", "↯"),
];

/// First two mappable genres, in AniList's returned order (DESIGN 3.8).
fn genre_glyphs(genres: &[String]) -> Vec<&'static str> {
    genres
        .iter()
        .filter_map(|g| {
            GENRE_GLYPHS
                .iter()
                .find(|(name, _)| *name == g.as_str())
                .map(|(_, glyph)| *glyph)
        })
        .take(2)
        .collect()
}

/// Grid geometry shared by draw, nav, and pump so they can never disagree on
/// what is visible (zigoku ROD-243 shape). `content_h` is the §3.2 content
/// band; the axis bar and its spacer are subtracted here (DESIGN 3.8).
#[derive(Debug)]
pub struct GridGeo {
    pub tier: sizing::Tier,
    pub cover_h: u16,
    pub slot_h: u16,
    pub cols: usize,
    pub rows_visible: usize,
}

pub fn grid_geo(w: u16, content_h: u16, cell: Option<FontSize>) -> GridGeo {
    let tier = sizing::tier(w);
    let cover_h = sizing::card_cover_h(&tier, cell);
    let slot_h = cover_h + 4;
    let cols = ((w.saturating_sub(2)) / tier.slot_w).max(1) as usize;
    let grid_h = content_h.saturating_sub(2);
    let rows_visible = ((grid_h / slot_h) as usize).max(1);
    GridGeo {
        tier,
        cover_h,
        slot_h,
        cols,
        rows_visible,
    }
}

/// One axis slot (04 §7.5): independent results, cursor, scroll, and flags.
/// Rank is positional per axis; slots never share or re-sort one list.
#[derive(Debug, Default)]
struct AxisSlot {
    entries: Vec<Enrichment>,
    page: u32,
    loading: Option<AsyncStart>,
    /// Sticky until an axis key retries (DESIGN 3.8: the error state is
    /// persistent, and auto-refetch on tick would storm the API).
    failed: Option<String>,
    exhausted: bool,
    cursor: usize,
    scroll_row: usize,
}

pub struct DiscoverState {
    axis: DiscoverAxis,
    slots: [AxisSlot; 4],
    covers: DiscoverCovers,
    feed_drain: Drain,
    cover_drain: Drain,
}

impl Default for DiscoverState {
    fn default() -> Self {
        DiscoverState {
            axis: DiscoverAxis::Trending,
            slots: Default::default(),
            covers: DiscoverCovers::default(),
            feed_drain: Drain::default(),
            cover_drain: Drain::default(),
        }
    }
}

impl DiscoverState {
    pub fn axis(&self) -> DiscoverAxis {
        self.axis
    }

    fn slot(&self) -> &AxisSlot {
        &self.slots[axis_idx(self.axis)]
    }

    fn slot_mut(&mut self) -> &mut AxisSlot {
        &mut self.slots[axis_idx(self.axis)]
    }

    pub fn selected_entry(&self) -> Option<&Enrichment> {
        let slot = self.slot();
        slot.entries.get(slot.cursor)
    }

    /// `[` / `]` cycle with wraparound; switching preserves every slot's
    /// state (DESIGN 8.6) and re-arms a failed slot for retry (DESIGN 3.8).
    pub fn cycle_axis(&mut self, delta: i64) {
        let n = AXES.len() as i64;
        let idx = (axis_idx(self.axis) as i64 + delta).rem_euclid(n) as usize;
        self.axis = AXES[idx];
        self.slot_mut().failed = None;
    }

    /// `1`-`4` direct select; out-of-range keys are ignored.
    pub fn select_axis(&mut self, index: usize) {
        if let Some(axis) = AXES.get(index) {
            self.axis = *axis;
            self.slot_mut().failed = None;
        }
    }

    /// hjkl over the grid: left/right wrap within a row, up/down move
    /// card-rows and clamp (DESIGN 7.5).
    pub fn nav(&mut self, dx: i64, dy: i64, geo: &GridGeo) {
        let cols = geo.cols;
        let slot = self.slot_mut();
        let len = slot.entries.len();
        if len == 0 {
            return;
        }
        if dx != 0 {
            let row_start = (slot.cursor / cols) * cols;
            let row_len = (len - row_start).min(cols);
            let col = (slot.cursor - row_start) as i64;
            slot.cursor = row_start + (col + dx).rem_euclid(row_len as i64) as usize;
        } else if dy != 0 {
            let moved = slot.cursor as i64 + dy * cols as i64;
            slot.cursor = moved.clamp(0, (len - 1) as i64) as usize;
        }
        self.clamp_scroll(geo);
    }

    /// Keep the cursor's card-row inside the visible band.
    fn clamp_scroll(&mut self, geo: &GridGeo) {
        let slot = self.slot_mut();
        let row = slot.cursor / geo.cols;
        slot.scroll_row = slot.scroll_row.min(row);
        if row >= slot.scroll_row + geo.rows_visible {
            slot.scroll_row = row + 1 - geo.rows_visible;
        }
    }

    /// The feed decision, pure: which page (if any) the active axis wants.
    /// Empty slot at activation fetches page 1 (DESIGN 8.6); near-end cursor
    /// prefetches the next page; loading, failed, and exhausted all hold.
    pub fn wanted_fetch(&self, geo: &GridGeo) -> Option<u32> {
        let slot = self.slot();
        if slot.loading.is_some() || slot.failed.is_some() || slot.exhausted {
            return None;
        }
        if slot.entries.is_empty() {
            return Some(1);
        }
        let last_row = (slot.entries.len() - 1) / geo.cols;
        let cursor_row = slot.cursor / geo.cols;
        (last_row.saturating_sub(cursor_row) <= PREFETCH_ROWS).then_some(slot.page + 1)
    }

    /// Arm loading and spawn the worker; a failed spawn resets the arm so the
    /// spinner can never strand (no worker will ever answer it).
    pub fn fire_fetch(
        &mut self,
        page: u32,
        now: Instant,
        tx: &EventTx,
        catalog: &Arc<dyn CatalogProvider>,
    ) {
        let axis = self.axis;
        self.slot_mut().loading = Some(AsyncStart::new(now));
        let spawned = workers::spawn_discover_feed(
            &self.feed_drain,
            tx.clone(),
            Arc::clone(catalog),
            axis,
            page,
        );
        if !spawned {
            self.slot_mut().loading = None;
        }
    }

    /// File a page into its axis slot (04 §4.2, §6): an out-of-order page is
    /// discarded here, and every applied row upserts `catalog_cache` (02 L2)
    /// best-effort, so a cache write failure never drops a rendered feed.
    pub fn on_feed(
        &mut self,
        axis: DiscoverAxis,
        page: u32,
        entries: Vec<Enrichment>,
        has_next: bool,
        store: &Store,
        now_unix: i64,
    ) {
        let slot = &mut self.slots[axis_idx(axis)];
        slot.loading = None;
        if page != slot.page + 1 {
            return;
        }
        for e in &entries {
            let ttl = enrichment_ttl_secs(e.status.as_deref());
            let _ = store.upsert_catalog_cache(e, now_unix, Some(now_unix + ttl));
        }
        slot.entries.extend(entries);
        slot.page = page;
        slot.failed = None;
        slot.exhausted = !has_next || slot.entries.len() >= MAX_FEED_ROWS;
    }

    pub fn on_feed_error(&mut self, axis: DiscoverAxis, cause: String) {
        let slot = &mut self.slots[axis_idx(axis)];
        slot.loading = None;
        slot.failed = Some(cause);
    }

    /// Slot adopts by url wherever the grid moved meanwhile (04 §4.4). The
    /// buffer moves straight into the render store, no clones.
    pub fn on_cover_done(&mut self, url: &str, img: DynamicImage, pool: &mut ProtocolPool) {
        self.covers.adopt(url);
        pool.ensure(url, img);
    }

    /// Per-url cooldown; the rank placeholder stays the loading cue.
    pub fn on_cover_error(&mut self, url: &str, now: Instant) {
        self.covers.note_failure(url, now);
    }

    /// Cover pump pass (04 §7.4): visible plus one prefetch row, then sync
    /// the protocol pool with the surviving slots.
    #[allow(clippy::too_many_arguments)]
    pub fn pump(
        &mut self,
        now: Instant,
        cap: usize,
        pool: &mut ProtocolPool,
        tx: &EventTx,
        caches: &Arc<CoverCaches>,
        covers_dir: &Path,
        geo: &GridGeo,
    ) {
        let window: Vec<String> = {
            let slot = self.slot();
            if slot.entries.is_empty() {
                return;
            }
            let start = slot.scroll_row * geo.cols;
            let span = (geo.rows_visible + 1) * geo.cols;
            let end = slot.entries.len().min(start + span);
            if start >= end {
                return;
            }
            slot.entries[start..end]
                .iter()
                .filter_map(|e| e.cover_url.clone())
                .collect()
        };
        let window: Vec<&str> = window.iter().map(String::as_str).collect();
        let chosen = self
            .covers
            .pump(&window, now, cap, self.cover_drain.inflight());
        for url in chosen {
            let spawned = workers::spawn_discover_cover_fetch(
                &self.cover_drain,
                tx.clone(),
                Arc::clone(caches),
                covers_dir.to_path_buf(),
                url.clone(),
            );
            if !spawned {
                self.covers.reset_loading(&url);
            }
        }
        let covers = &self.covers;
        pool.retain(|key| key == DETAIL_KEY || covers.get(key).is_some());
    }

    /// Teardown: both worker families (04 §5.1).
    pub fn drain(&self, timeout: std::time::Duration) -> bool {
        let feed = self.feed_drain.drain(timeout);
        self.cover_drain.drain(timeout) && feed
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DiscoverState,
    pool: &mut ProtocolPool,
    env: &ViewEnv,
) {
    if area.height == 0 {
        return;
    }
    draw_axis_bar(frame, area, palette, state.axis);
    let grid = Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(2),
        ..area
    };
    let slot = state.slot();
    if slot.entries.is_empty() {
        draw_empty_grid(frame, grid, palette, slot, env.now);
        return;
    }
    let geo = grid_geo(area.width, area.height, pool.cell());
    draw_cards(frame, grid, palette, state, pool, env, &geo);
    // The load-more footer yields to the peek band (DESIGN 3.8); when there
    // is nothing to peek, the footer takes the leftover band instead.
    if !draw_peek_band(frame, grid, palette, state, pool, &geo) {
        draw_grid_tail(frame, grid, palette, state.slot(), &geo, env.now);
    }
}

/// Passive axis bar teaching its own `1`-`4` binds in place (DESIGN 3.8).
fn draw_axis_bar(frame: &mut Frame<'_>, area: Rect, palette: &Palette, active: DiscoverAxis) {
    let mut spans: Vec<Span<'_>> = vec![Span::raw("  ")];
    for (i, axis) in AXES.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(palette.fg3)));
        }
        let (key_style, label_style) = if *axis == active {
            (
                Style::new().fg(palette.focus),
                Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
            )
        } else {
            (Style::new().fg(palette.fg2), Style::new().fg(palette.fg2))
        };
        spans.push(Span::styled(format!("[{}]", i + 1), key_style));
        spans.push(Span::styled(format!(" {}", axis_label(*axis)), label_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { height: 1, ..area },
    );
}

/// The three no-results states (DESIGN 3.8): loading (slow-escalated), error
/// (persistent until an axis-key retry), and the genuine empty feed. A slot
/// nothing has fetched yet stays blank by design.
fn draw_empty_grid(
    frame: &mut Frame<'_>,
    grid: Rect,
    palette: &Palette,
    slot: &AxisSlot,
    now: Instant,
) {
    let mid = grid.height / 2;
    if let Some(started) = &slot.loading {
        let spin = SPINNER[started.frame(now, SPINNER.len())];
        let (text, style) = if started.is_slow(now) {
            (
                format!("{spin} taking a moment…"),
                Style::new().fg(palette.hot),
            )
        } else {
            (
                format!("{spin} loading feed…"),
                Style::new().fg(palette.focus),
            )
        };
        render::draw_centered(frame, grid, mid, Line::from(Span::styled(text, style)));
    } else if slot.failed.is_some() {
        render::draw_centered(
            frame,
            grid,
            mid,
            Line::from(Span::styled(
                "[!] can't reach the feed",
                Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
            )),
        );
        render::draw_centered(
            frame,
            grid,
            mid + 1,
            Line::from(Span::styled(
                "check your connection",
                Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
            )),
        );
    } else if slot.page > 0 {
        render::draw_centered(
            frame,
            grid,
            mid,
            Line::from(Span::styled(
                "no entries",
                Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
            )),
        );
    }
}

fn draw_cards(
    frame: &mut Frame<'_>,
    grid: Rect,
    palette: &Palette,
    state: &DiscoverState,
    pool: &mut ProtocolPool,
    env: &ViewEnv,
    geo: &GridGeo,
) {
    let slot = state.slot();
    for (i, entry) in slot.entries.iter().enumerate() {
        let (card_row, col) = (i / geo.cols, i % geo.cols);
        if card_row < slot.scroll_row || card_row >= slot.scroll_row + geo.rows_visible {
            continue;
        }
        // DESIGN 3.8: 2-cell left margin, one slot per card.
        let x = grid.x + 2 + (col as u16) * geo.tier.slot_w;
        let y = grid.y + ((card_row - slot.scroll_row) as u16) * geo.slot_h;
        if x + geo.tier.cover_w > grid.right() || y + geo.slot_h > grid.bottom() {
            continue;
        }
        draw_card(
            frame,
            palette,
            pool,
            entry,
            CardCtx {
                rank: i,
                selected: i == slot.cursor,
                axis: state.axis,
                pref: env.pref,
                cour: env.cour,
                x,
                y,
                cover_w: geo.tier.cover_w,
                cover_h: geo.cover_h,
            },
        );
    }
}

struct CardCtx {
    rank: usize,
    selected: bool,
    axis: DiscoverAxis,
    pref: TitleLanguage,
    cour: Cour,
    x: u16,
    y: u16,
    cover_w: u16,
    cover_h: u16,
}

fn draw_card(
    frame: &mut Frame<'_>,
    palette: &Palette,
    pool: &mut ProtocolPool,
    entry: &Enrichment,
    ctx: CardCtx,
) {
    let cover = Rect::new(ctx.x, ctx.y, ctx.cover_w, ctx.cover_h);
    let drawn = match entry.cover_url.as_deref() {
        Some(url) => pool.render(frame, cover, url),
        None => false,
    };
    if !drawn {
        // Placeholder: the only surface-elevated element in the grid.
        frame.render_widget(
            Paragraph::new(format!("\n#{}", ctx.rank + 1))
                .centered()
                .style(Style::new().fg(palette.fg3).bg(palette.surface)),
            cover,
        );
    }

    let rank_y = ctx.y + ctx.cover_h;
    if ctx.selected && ctx.x > 0 {
        // Left-gutter marker: never masks the cover cell (DESIGN 3.8).
        frame.render_widget(
            Paragraph::new(Span::styled("▸", Style::new().fg(palette.focus))),
            Rect::new(ctx.x - 1, rank_y, 1, 1),
        );
    }

    // Rank + at most one badge, left-anchored; score right-anchored.
    let mut spans = vec![Span::styled(
        format!("#{}", ctx.rank + 1),
        Style::new().fg(palette.fg),
    )];
    if ctx.rank == 0 {
        spans.push(Span::styled(
            " TOP",
            Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
        ));
    } else if is_new_this_cour(entry, ctx.axis, ctx.cour) {
        spans.push(Span::styled(
            " NEW",
            Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(ctx.x, rank_y, ctx.cover_w, 1),
    );
    let badge = render::score_badge(entry.score);
    let badge_w = badge.len() as u16;
    if badge_w <= ctx.cover_w {
        frame.render_widget(
            Paragraph::new(Span::styled(
                badge,
                render::score_style(palette, entry.score, true),
            )),
            Rect::new(ctx.x + ctx.cover_w - badge_w, rank_y, badge_w, 1),
        );
    }

    // Title row: resolved primary, clipped (DESIGN 8.2).
    let title = preferred_title(
        &entry.title_romaji,
        entry.title_english.as_deref(),
        entry.title_native.as_deref(),
        ctx.pref,
    );
    let title_style = if ctx.selected {
        Style::new().fg(palette.focus).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(palette.fg)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            render::truncate_to_width(title, ctx.cover_w as usize).into_owned(),
            title_style,
        )),
        Rect::new(ctx.x, rank_y + 1, ctx.cover_w, 1),
    );

    // Format + episode count left, genre glyphs right (DESIGN 3.8).
    let format_row = rank_y + 2;
    match render::format_label(entry.kind.as_deref()) {
        Some(label) => {
            let text = match entry.total_episodes {
                _ if label == "Movie" => label.to_string(),
                Some(eps) => format!("{label} · {eps}ep"),
                None if is_still_airing(entry.status.as_deref()) => format!("{label} · ??ep"),
                None => label.to_string(),
            };
            frame.render_widget(
                Paragraph::new(Span::styled(text, Style::new().fg(palette.fg2))),
                Rect::new(ctx.x, format_row, ctx.cover_w, 1),
            );
        }
        None => {
            frame.render_widget(
                Paragraph::new(Span::styled("—", Style::new().fg(palette.fg3))),
                Rect::new(ctx.x, format_row, ctx.cover_w.min(1), 1),
            );
        }
    }
    let glyphs = genre_glyphs(&entry.genres);
    if !glyphs.is_empty() {
        let text = glyphs.join(" ");
        let w = render::display_width(&text) as u16;
        if w < ctx.cover_w {
            frame.render_widget(
                Paragraph::new(Span::styled(text, Style::new().fg(palette.fg3))),
                Rect::new(ctx.x + ctx.cover_w - w, format_row, w, 1),
            );
        }
    }
}

/// `NEW` marks a current-cour release, suppressed on This Season where every
/// card qualifies by construction (DESIGN 3.8); exclusive with `TOP`.
fn is_new_this_cour(entry: &Enrichment, axis: DiscoverAxis, cour: Cour) -> bool {
    axis != DiscoverAxis::ThisSeason
        && entry.season == Some(cour.season)
        && entry.year == Some(cour.year)
}

/// Peek band: leftover vertical space ≥ 3 rows renders the tops of the next
/// card-row's covers, covers only (DESIGN 3.8). True when anything drew.
fn draw_peek_band(
    frame: &mut Frame<'_>,
    grid: Rect,
    palette: &Palette,
    state: &DiscoverState,
    pool: &mut ProtocolPool,
    geo: &GridGeo,
) -> bool {
    let slot = state.slot();
    let band_y = grid.y + (geo.rows_visible as u16) * geo.slot_h;
    let band_h = grid.bottom().saturating_sub(band_y);
    if band_h < MIN_PEEK_ROWS {
        return false;
    }
    let peek_row = slot.scroll_row + geo.rows_visible;
    let start = peek_row * geo.cols;
    if start >= slot.entries.len() {
        return false;
    }
    for (offset, entry) in slot.entries[start..].iter().take(geo.cols).enumerate() {
        let x = grid.x + 2 + (offset as u16) * geo.tier.slot_w;
        if x + geo.tier.cover_w > grid.right() {
            break;
        }
        let cover = Rect::new(x, band_y, geo.tier.cover_w, band_h.min(geo.cover_h));
        let drawn = match entry.cover_url.as_deref() {
            Some(url) => pool.render(frame, cover, url),
            None => false,
        };
        if !drawn {
            frame.render_widget(Block::new().style(Style::new().bg(palette.surface)), cover);
        }
    }
    true
}

/// Load-more footer, only when the peek band is absent (DESIGN 3.8).
fn draw_grid_tail(
    frame: &mut Frame<'_>,
    grid: Rect,
    palette: &Palette,
    slot: &AxisSlot,
    geo: &GridGeo,
    now: Instant,
) {
    let band_y = grid.y + (geo.rows_visible as u16) * geo.slot_h;
    if grid.bottom() <= band_y {
        return;
    }
    let last_row = (slot.entries.len().saturating_sub(1)) / geo.cols;
    let last_visible = last_row < slot.scroll_row + geo.rows_visible;
    let line = if let Some(started) = &slot.loading {
        let spin = SPINNER[started.frame(now, SPINNER.len())];
        Some(Span::styled(
            format!("{spin} loading more…"),
            Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
        ))
    } else if slot.exhausted && last_visible {
        Some(Span::styled(
            "all entries loaded",
            Style::new().fg(palette.fg3),
        ))
    } else {
        None
    };
    if let Some(span) = line {
        frame.render_widget(
            Paragraph::new(span),
            Rect::new(grid.x + 2, band_y, grid.width.saturating_sub(2), 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::tui::event;

    fn entry(id: i64) -> Enrichment {
        Enrichment {
            anilist_id: id,
            title_romaji: format!("Show {id}"),
            cover_url: Some(format!("http://127.0.0.1:9/{id}.png")),
            ..Enrichment::default()
        }
    }

    fn entries(from: i64, n: i64) -> Vec<Enrichment> {
        (from..from + n).map(entry).collect()
    }

    /// 100 cols, halfblocks: large tier, 4 cols per row.
    fn geo() -> GridGeo {
        grid_geo(100, 27, None)
    }

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    #[test]
    fn feed_pages_append_and_file_by_axis() {
        let mut d = DiscoverState::default();
        let s = store();
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 20), true, &s, 1000);
        d.on_feed(DiscoverAxis::Popular, 1, entries(100, 5), false, &s, 1000);
        assert_eq!(d.slot().entries.len(), 20);
        assert!(!d.slot().exhausted);
        d.cycle_axis(1);
        assert_eq!(d.slot().entries.len(), 5);
        assert!(d.slot().exhausted, "has_next=false exhausts");
    }

    #[test]
    fn stale_or_duplicate_pages_are_discarded() {
        let mut d = DiscoverState::default();
        let s = store();
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 20), true, &s, 1000);
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 20), true, &s, 1000);
        assert_eq!(d.slot().entries.len(), 20, "duplicate page 1 dropped");
        d.on_feed(DiscoverAxis::Trending, 3, entries(41, 20), true, &s, 1000);
        assert_eq!(d.slot().entries.len(), 20, "page 3 before 2 dropped");
        d.on_feed(DiscoverAxis::Trending, 2, entries(21, 20), true, &s, 1000);
        assert_eq!(d.slot().entries.len(), 40);
    }

    #[test]
    fn max_feed_rows_forces_exhaustion() {
        let mut d = DiscoverState::default();
        let s = store();
        for page in 1..=15 {
            let from = (page as i64 - 1) * 20 + 1;
            d.on_feed(
                DiscoverAxis::Trending,
                page,
                entries(from, 20),
                true,
                &s,
                1000,
            );
        }
        assert_eq!(d.slot().entries.len(), MAX_FEED_ROWS);
        assert!(d.slot().exhausted, "300 rows force-exhaust (ROD-339)");
        assert_eq!(d.wanted_fetch(&geo()), None);
    }

    #[test]
    fn applied_pages_upsert_catalog_cache() {
        let mut d = DiscoverState::default();
        let s = store();
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 3), true, &s, 1000);
        for id in 1..=3 {
            assert!(s.get_catalog(id).unwrap().is_some(), "row {id} cached");
        }
    }

    #[test]
    fn wanted_fetch_wants_page_one_for_an_empty_slot() {
        let d = DiscoverState::default();
        assert_eq!(d.wanted_fetch(&geo()), Some(1));
    }

    #[test]
    fn wanted_fetch_holds_while_loading_failed_or_far() {
        let mut d = DiscoverState::default();
        let s = store();
        d.slot_mut().loading = Some(AsyncStart::new(Instant::now()));
        assert_eq!(d.wanted_fetch(&geo()), None, "loading holds");
        d.slot_mut().loading = None;
        d.on_feed_error(DiscoverAxis::Trending, "offline".into());
        assert_eq!(d.wanted_fetch(&geo()), None, "failed holds until retry");
        d.select_axis(0);
        assert_eq!(d.wanted_fetch(&geo()), Some(1), "axis key re-arms");
        // 5 rows of 4: cursor at top, last row 4 rows away: no prefetch.
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 20), true, &s, 1000);
        assert_eq!(d.wanted_fetch(&geo()), None);
        // Move within 2 card-rows of the end: prefetch page 2.
        d.slot_mut().cursor = 8;
        assert_eq!(d.wanted_fetch(&geo()), Some(2));
    }

    #[test]
    fn feed_error_is_sticky_until_axis_retry() {
        let mut d = DiscoverState::default();
        d.on_feed_error(DiscoverAxis::Trending, "offline".into());
        assert!(d.slot().failed.is_some());
        assert_eq!(d.wanted_fetch(&geo()), None);
        d.cycle_axis(1);
        d.cycle_axis(-1);
        assert!(d.slot().failed.is_none(), "cycling back re-arms the slot");
    }

    #[test]
    fn nav_wraps_rows_and_clamps_columns() {
        let mut d = DiscoverState::default();
        let s = store();
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 10), false, &s, 1000);
        let g = geo();
        assert_eq!(g.cols, 4);
        d.nav(-1, 0, &g);
        assert_eq!(d.slot().cursor, 3, "h wraps within the row");
        d.nav(1, 0, &g);
        assert_eq!(d.slot().cursor, 0, "l wraps back");
        d.nav(0, 2, &g);
        assert_eq!(d.slot().cursor, 8, "j moves card-rows");
        d.nav(1, 0, &g);
        assert_eq!(d.slot().cursor, 9);
        d.nav(1, 0, &g);
        assert_eq!(d.slot().cursor, 8, "short last row wraps within itself");
        d.nav(0, 5, &g);
        assert_eq!(d.slot().cursor, 9, "j clamps to the last card");
    }

    #[test]
    fn cursor_and_scroll_survive_axis_switches() {
        let mut d = DiscoverState::default();
        let s = store();
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 40), true, &s, 1000);
        let g = geo();
        for _ in 0..6 {
            d.nav(0, 1, &g);
        }
        let (cursor, scroll) = (d.slot().cursor, d.slot().scroll_row);
        assert!(scroll > 0, "cursor motion scrolled the band");
        d.cycle_axis(1);
        assert_eq!(d.slot().cursor, 0);
        d.cycle_axis(-1);
        assert_eq!(d.slot().cursor, cursor, "slot state preserved (DESIGN 8.6)");
        assert_eq!(d.slot().scroll_row, scroll);
    }

    #[test]
    fn selected_entry_follows_the_active_slot() {
        let mut d = DiscoverState::default();
        let s = store();
        assert!(d.selected_entry().is_none());
        d.on_feed(DiscoverAxis::Trending, 1, entries(1, 4), false, &s, 1000);
        assert_eq!(d.selected_entry().unwrap().anilist_id, 1);
        d.nav(1, 0, &geo());
        assert_eq!(d.selected_entry().unwrap().anilist_id, 2);
    }

    #[test]
    fn spawn_failure_resets_the_loading_arm() {
        let mut d = DiscoverState::default();
        let (tx, rx) = event::channel();
        drop(rx);
        // A drain mid-teardown refuses spawns; simulate by draining first.
        struct NeverCatalog;
        impl CatalogProvider for NeverCatalog {
            fn search(
                &self,
                _q: &str,
                _p: u32,
            ) -> Result<crate::providers::CatalogPage, crate::providers::CatalogError> {
                Err(crate::providers::CatalogError::Network)
            }
            fn discover(
                &self,
                _a: DiscoverAxis,
                _p: u32,
            ) -> Result<crate::providers::CatalogPage, crate::providers::CatalogError> {
                Err(crate::providers::CatalogError::Network)
            }
            fn enrich(
                &self,
                _id: i64,
            ) -> Result<Option<Enrichment>, crate::providers::CatalogError> {
                Ok(None)
            }
        }
        let catalog: Arc<dyn CatalogProvider> = Arc::new(NeverCatalog);
        d.fire_fetch(1, Instant::now(), &tx, &catalog);
        assert!(d.slot().loading.is_some(), "spawn ok arms loading");
        assert!(d.drain(std::time::Duration::from_secs(5)));
    }

    #[test]
    fn genre_glyphs_map_first_two_known() {
        let genres = vec![
            "Isekai".to_string(),
            "Action".to_string(),
            "Drama".to_string(),
            "Comedy".to_string(),
        ];
        assert_eq!(genre_glyphs(&genres), ["⚔", "◆"]);
        assert!(genre_glyphs(&["Unknown".to_string()]).is_empty());
    }

    #[test]
    fn genre_glyph_table_matches_design_3_8a() {
        assert_eq!(GENRE_GLYPHS.len(), 18);
        for (name, glyph) in GENRE_GLYPHS {
            assert!(!name.is_empty());
            let c = glyph.chars().next().unwrap();
            assert!((c as u32) <= 0xFFFF, "{name} glyph must stay in the BMP");
        }
    }

    #[test]
    fn format_labels_map_and_reject_unknown() {
        assert_eq!(render::format_label(Some("TV")), Some("TV"));
        assert_eq!(render::format_label(Some("TV_SHORT")), Some("TV"));
        assert_eq!(render::format_label(Some("SPECIAL")), Some("Spec"));
        assert_eq!(render::format_label(Some("VHS")), None);
        assert_eq!(render::format_label(None), None);
    }
}
