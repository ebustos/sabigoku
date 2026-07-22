//! History/Watchlist: the default landing view (DESIGN 5.4, 8.3). ROD-439
//! chunk 5a: load, grouped rows, progress bars, local filter, the cursor
//! identity contracts (05 §2). Status mutations, delete, and the resume
//! landing are chunk 5b.
//!
//! The list is loaded synchronously from sqlite (a local read, no worker;
//! deliberate deviation from 04 §4.2's load events, recorded on the ticket).
//! Rows live here in store order; `order` is the filtered, group-ordered nav
//! list over them, and `layout()` is the one line-geometry both nav and draw
//! consume so they can never disagree (05 §2 geometry contract).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::domain::{ListStatus, Show, preferred_title};
use crate::store::Store;
use crate::tui::render::{self, draw_absent_block};
use crate::tui::theme::Palette;
use crate::tui::view::ViewEnv;

/// Cursor walk order (05 §2): group order, never store order.
const GROUP_ORDER: [ListStatus; 5] = [
    ListStatus::Watching,
    ListStatus::Planning,
    ListStatus::Paused,
    ListStatus::Completed,
    ListStatus::Dropped,
];

/// DESIGN 4.5 bar bounds.
const BAR_MIN: u16 = 16;
const BAR_MAX: u16 = 24;

#[derive(Debug, Default)]
pub struct HistoryState {
    pub filter: String,
    rows: Vec<Show>,
    /// Numeric resume episode per row (parallel to `rows`), for the `◐`
    /// marker; non-numeric labels carry no marker.
    resume: Vec<Option<u32>>,
    /// Indices into `rows`, filtered and in group order.
    order: Vec<usize>,
    cursor: usize,
    scroll: usize,
    load_failed: bool,
}

/// One display line of the grouped list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Li {
    Header { status: ListStatus, count: usize },
    Rule,
    Title { ord: usize },
    Bar { ord: usize },
    Blank,
}

impl HistoryState {
    /// Synchronous load; a failure keeps the previous rows (05 §14 intent:
    /// a reload must never wipe the list on a soft fail).
    pub fn load(&mut self, store: &Store) {
        match store.list_history() {
            Ok(rows) => {
                // Anchor from the OUTGOING rows: `order` indexes them, and
                // the incoming set may be shorter (a delete).
                let anchor = self.anchor_aid();
                self.load_failed = false;
                self.resume = rows
                    .iter()
                    .map(|s| {
                        store
                            .latest_resume(s.enrichment.anilist_id, crate::domain::Translation::Sub)
                            .ok()
                            .flatten()
                            .and_then(|(label, _)| label.parse().ok())
                    })
                    .collect();
                self.rows = rows;
                self.rebuild_with(anchor);
            }
            Err(_) => self.load_failed = true,
        }
    }

    fn anchor_aid(&self) -> Option<i64> {
        self.order
            .get(self.cursor)
            .and_then(|&ix| self.rows.get(ix))
            .map(|s| s.enrichment.anilist_id)
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn load_failed(&self) -> bool {
        self.load_failed
    }

    /// Filtered count, for the search-bar tag.
    pub fn count(&self) -> usize {
        self.order.len()
    }

    pub fn selected(&self) -> Option<&Show> {
        self.order.get(self.cursor).map(|&ix| &self.rows[ix])
    }

    pub fn show_by_aid(&self, anilist_id: i64) -> Option<&Show> {
        self.rows
            .iter()
            .find(|s| s.enrichment.anilist_id == anilist_id)
    }

    /// The resume-landing target (05 §10.6): the most-recently-watched row.
    /// The load sorts played rows first, so it is the head or nothing.
    pub fn first_played(&self) -> Option<i64> {
        self.rows
            .first()
            .filter(|s| s.last_watched_at.is_some())
            .map(|s| s.enrichment.anilist_id)
    }

    /// Move the cursor onto a show by identity; false when it is not in the
    /// current nav order.
    pub fn select_aid(&mut self, anilist_id: i64, visible: usize) -> bool {
        let Some(pos) = self
            .order
            .iter()
            .position(|&ix| self.rows[ix].enrichment.anilist_id == anilist_id)
        else {
            return false;
        };
        self.cursor = pos;
        self.scroll_into_view(visible);
        true
    }

    /// Recompute-to-0 clears the row's resume marker (05 §4); in-memory
    /// only, the next real watch re-derives it.
    /// Rebuild the nav order; the cursor follows the focused show's identity
    /// across the reorder, clamping when it fell out (05 §2 setHistory).
    fn rebuild(&mut self) {
        let anchor = self.anchor_aid();
        self.rebuild_with(anchor);
    }

    fn rebuild_with(&mut self, anchor: Option<i64>) {
        self.order.clear();
        for status in GROUP_ORDER {
            for (ix, show) in self.rows.iter().enumerate() {
                if show.list_status == status && self.matches_filter(show) {
                    self.order.push(ix);
                }
            }
        }
        self.cursor = anchor
            .and_then(|aid| {
                self.order
                    .iter()
                    .position(|&ix| self.rows[ix].enrichment.anilist_id == aid)
            })
            .unwrap_or_else(|| self.cursor.min(self.order.len().saturating_sub(1)));
    }

    /// Filter matches ANY present title form (05 §2), case-insensitive.
    fn matches_filter(&self, show: &Show) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        let needle = self.filter.to_lowercase();
        let e = &show.enrichment;
        [
            Some(e.title_romaji.as_str()),
            e.title_english.as_deref(),
            e.title_native.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|t| t.to_lowercase().contains(&needle))
    }

    pub fn on_filter_edited(&mut self) {
        self.rebuild();
        self.scroll = 0;
    }

    /// Esc clears the filter and resets the cursor (05 §2).
    pub fn on_filter_cleared(&mut self) {
        self.filter.clear();
        self.rebuild();
        self.cursor = 0;
        self.scroll = 0;
    }

    pub fn nav(&mut self, dy: i64, visible: usize) {
        if self.order.is_empty() {
            return;
        }
        let max = self.order.len() as i64 - 1;
        self.cursor = (self.cursor as i64 + dy).clamp(0, max) as usize;
        self.scroll_into_view(visible);
    }

    pub fn jump(&mut self, top: bool, visible: usize) {
        if self.order.is_empty() {
            return;
        }
        self.cursor = if top { 0 } else { self.order.len() - 1 };
        self.scroll_into_view(visible);
    }

    /// The grouped line layout (05 §2 geometry): per non-empty group a
    /// header, a rule, 2-row entries with a blank between, a closing rule,
    /// and a blank before the next group.
    fn layout(&self) -> Vec<Li> {
        let mut lines = Vec::new();
        let mut ord = 0usize;
        let groups: Vec<(ListStatus, usize)> = GROUP_ORDER
            .iter()
            .map(|&status| {
                let count = self
                    .order
                    .iter()
                    .filter(|&&ix| self.rows[ix].list_status == status)
                    .count();
                (status, count)
            })
            .filter(|(_, count)| *count > 0)
            .collect();
        for (g, &(status, count)) in groups.iter().enumerate() {
            // Packed 2-row entries (title + bar), a blank only between status
            // groups: a watchlist is a scannable list, not a card gallery.
            if g > 0 {
                lines.push(Li::Blank);
            }
            lines.push(Li::Header { status, count });
            lines.push(Li::Rule);
            for _ in 0..count {
                lines.push(Li::Title { ord });
                lines.push(Li::Bar { ord });
                ord += 1;
            }
        }
        lines
    }

    fn scroll_into_view(&mut self, visible: usize) {
        if visible == 0 {
            return;
        }
        let lines = self.layout();
        let Some(title_ix) = lines
            .iter()
            .position(|li| *li == Li::Title { ord: self.cursor })
        else {
            return;
        };
        // Keep the entry's own group header visible when hugging the top.
        if title_ix.saturating_sub(2) < self.scroll {
            self.scroll = title_ix.saturating_sub(2);
        }
        if title_ix + 2 > self.scroll + visible {
            self.scroll = title_ix + 2 - visible;
        }
    }
}

/// The list column (DESIGN 5.4): full-width single column below the split or
/// with no focused record, the left pane otherwise. `focused` is list-pane
/// focus for the §4.1/§4.5 selection rules.
pub fn draw_list(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &HistoryState,
    env: &ViewEnv,
    focused: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if state.is_empty() {
        if state.load_failed {
            render::draw_centered(
                frame,
                area,
                area.height / 2,
                Line::from(Span::styled(
                    "couldn't load your watchlist",
                    Style::new().fg(palette.warn),
                )),
            );
            return;
        }
        // An empty watchlist is a user who doesn't yet know what to watch,
        // so Discover leads and Browse recedes (DESIGN 8.3).
        draw_absent_block(
            frame,
            area,
            palette,
            "nothing watched yet",
            ("D", "see what's popular"),
            ("B", "search for a show"),
        );
        return;
    }
    let lines = state.layout();
    for (row, li) in lines
        .iter()
        .skip(state.scroll)
        .take(area.height as usize)
        .enumerate()
    {
        let y = area.y + row as u16;
        match li {
            Li::Blank => {}
            Li::Rule => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "─".repeat(area.width as usize),
                        Style::new().fg(palette.chrome),
                    )),
                    Rect::new(area.x, y, area.width, 1),
                );
            }
            Li::Header { status, count } => {
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            format!("{} ", status_glyph(*status)),
                            status_style(palette, *status),
                        ),
                        Span::styled(
                            status_label(*status).to_string(),
                            Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!(" ({count})"), Style::new().fg(palette.fg2)),
                    ])),
                    Rect::new(area.x, y, area.width, 1),
                );
            }
            Li::Title { ord } => draw_title_row(frame, area, y, palette, state, env, *ord, focused),
            Li::Bar { ord } => draw_bar_row(frame, area, y, palette, state, *ord, focused),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_title_row(
    frame: &mut Frame<'_>,
    area: Rect,
    y: u16,
    palette: &Palette,
    state: &HistoryState,
    env: &ViewEnv,
    ord: usize,
    focused: bool,
) {
    let show = &state.rows[state.order[ord]];
    let selected = ord == state.cursor;
    let row = Rect::new(area.x, y, area.width, 1);
    if selected && focused {
        frame.render_widget(Block::new().style(Style::new().bg(palette.surface)), row);
    }
    // Row 1 is title-only at every width (DESIGN 5.4a; the richer right-meta
    // is an open question).
    let title = preferred_title(
        &show.enrichment.title_romaji,
        show.enrichment.title_english.as_deref(),
        show.enrichment.title_native.as_deref(),
        env.pref,
    );
    let glyph_style = if selected && focused {
        Style::new().fg(palette.focus)
    } else if selected {
        Style::new().fg(palette.fg3)
    } else {
        Style::new().fg(palette.fg2)
    };
    let title_style = if selected && focused {
        Style::new().fg(palette.focus).add_modifier(Modifier::BOLD)
    } else if selected {
        Style::new().fg(palette.focus)
    } else {
        Style::new().fg(palette.fg)
    };
    let width = area.width.saturating_sub(4) as usize;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", status_glyph(show.list_status)), glyph_style),
            Span::styled(
                render::truncate_to_width(title, width).into_owned(),
                title_style,
            ),
        ])),
        row,
    );
}

fn draw_bar_row(
    frame: &mut Frame<'_>,
    area: Rect,
    y: u16,
    palette: &Palette,
    state: &HistoryState,
    ord: usize,
    focused: bool,
) {
    let ix = state.order[ord];
    let show = &state.rows[ix];
    let selected = ord == state.cursor;
    let row = Rect::new(area.x, y, area.width, 1);
    if selected && focused {
        frame.render_widget(Block::new().style(Style::new().bg(palette.surface)), row);
    }
    let width = bar_width(area.width);
    let (filled, resume_cell) = bar_geometry(
        show.progress,
        show.enrichment.total_episodes,
        state.resume.get(ix).copied().flatten(),
        width,
    );
    let fill_style = render::bar_fill_color(palette, show.list_status, selected, focused);
    let frac_style = render::bar_frac_color(palette, show.list_status, selected, focused);
    let mut spans = vec![Span::styled("[", Style::new().fg(palette.chrome))];
    for i in 0..width {
        let (glyph, style) = if Some(i) == resume_cell {
            ("◐", fill_style)
        } else if i < filled {
            ("█", fill_style)
        } else {
            ("░", Style::new().fg(palette.chrome))
        };
        spans.push(Span::styled(glyph, style));
    }
    spans.push(Span::styled("]", Style::new().fg(palette.chrome)));
    let total = show
        .enrichment
        .total_episodes
        .map_or("?".to_string(), |t| t.to_string());
    spans.push(Span::styled(
        format!("  {} / {} eps", show.progress, total),
        frac_style,
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), row);
}

/// DESIGN 4.5: 16 minimum, scaling to 24 with available width.
fn bar_width(avail: u16) -> u16 {
    avail.saturating_sub(30).clamp(BAR_MIN, BAR_MAX)
}

/// Filled cells + the `◐` resume cell (DESIGN 4.5, 8.1): a null total fills
/// a third of the bar as a non-zero signal and carries no resume marker.
fn bar_geometry(
    progress: u32,
    total: Option<u32>,
    resume_ep: Option<u32>,
    width: u16,
) -> (u16, Option<u16>) {
    let width_u32 = u32::from(width);
    match total {
        Some(t) if t > 0 => {
            let filled = (progress.min(t) * width_u32 / t) as u16;
            let resume = resume_ep
                .filter(|ep| *ep >= 1)
                .map(|ep| ((ep - 1).min(t - 1) * width_u32 / t) as u16);
            (filled, resume)
        }
        _ => (if progress > 0 { width / 3 } else { 0 }, None),
    }
}

fn status_glyph(status: ListStatus) -> &'static str {
    match status {
        ListStatus::Watching => "▸",
        ListStatus::Completed => "●",
        ListStatus::Planning => "○",
        ListStatus::Paused => "◐",
        ListStatus::Dropped => "·",
    }
}

fn status_label(status: ListStatus) -> &'static str {
    match status {
        ListStatus::Watching => "watching",
        ListStatus::Completed => "complete",
        ListStatus::Planning => "planning",
        ListStatus::Paused => "paused",
        ListStatus::Dropped => "dropped",
    }
}

/// DESIGN 2.4 status colours (group headers keep them; row glyphs override
/// per the selection rule).
fn status_style(palette: &Palette, status: ListStatus) -> Style {
    match status {
        ListStatus::Watching => Style::new().fg(palette.focus),
        ListStatus::Completed | ListStatus::Planning => Style::new().fg(palette.fg2),
        ListStatus::Paused => Style::new().fg(palette.focus).add_modifier(Modifier::DIM),
        ListStatus::Dropped => Style::new().fg(palette.fg3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Enrichment;

    fn show(aid: i64, title: &str, status: ListStatus, progress: u32) -> Show {
        Show {
            enrichment: Enrichment {
                anilist_id: aid,
                title_romaji: title.to_string(),
                title_english: Some(format!("{title} EN")),
                total_episodes: Some(12),
                ..Enrichment::default()
            },
            list_status: status,
            progress,
            library_added_at: Some(100),
            ..Show::default()
        }
    }

    fn state(rows: Vec<Show>) -> HistoryState {
        let resume = vec![None; rows.len()];
        let mut s = HistoryState {
            rows,
            resume,
            ..HistoryState::default()
        };
        s.rebuild();
        s
    }

    fn nav_ids(s: &HistoryState) -> Vec<i64> {
        s.order
            .iter()
            .map(|&ix| s.rows[ix].enrichment.anilist_id)
            .collect()
    }

    #[test]
    fn order_walks_groups_not_store_order() {
        // Store order interleaves statuses; nav order must group them
        // watching -> planning -> paused -> completed -> dropped (05 2).
        let s = state(vec![
            show(1, "A", ListStatus::Completed, 12),
            show(2, "B", ListStatus::Watching, 3),
            show(3, "C", ListStatus::Dropped, 1),
            show(4, "D", ListStatus::Watching, 5),
            show(5, "E", ListStatus::Planning, 0),
        ]);
        assert_eq!(nav_ids(&s), [2, 4, 5, 1, 3]);
    }

    #[test]
    fn cursor_follows_identity_across_reorder_and_clamps() {
        let mut s = state(vec![
            show(1, "A", ListStatus::Watching, 1),
            show(2, "B", ListStatus::Watching, 2),
            show(3, "C", ListStatus::Watching, 3),
        ]);
        s.cursor = 1; // B
        // B completes: it moves to the completed group's slot.
        s.rows[1].list_status = ListStatus::Completed;
        s.rebuild();
        assert_eq!(nav_ids(&s), [1, 3, 2]);
        assert_eq!(s.cursor, 2, "cursor follows B's identity");
        // B vanishes entirely: clamp to the ordinal.
        s.rows.remove(1);
        s.resume.remove(1);
        s.rebuild();
        assert_eq!(s.cursor, 1, "clamps to the last valid ordinal");
        // Out-of-range cursor clamps too.
        s.cursor = 99;
        s.rebuild();
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn filter_matches_any_title_form_and_esc_resets() {
        let mut s = state(vec![
            show(1, "Sousou no Frieren", ListStatus::Watching, 1),
            show(2, "Vinland Saga", ListStatus::Watching, 1),
        ]);
        s.rows[0].enrichment.title_native = Some("葬送のフリーレン".into());
        s.filter = "frieren en".into();
        s.on_filter_edited();
        assert_eq!(s.count(), 1, "english form matches");
        s.filter = "フリーレン".into();
        s.on_filter_edited();
        assert_eq!(s.count(), 1, "native form matches");
        s.filter = "VINLAND".into();
        s.on_filter_edited();
        assert_eq!(nav_ids(&s), [2], "case-insensitive");
        s.on_filter_cleared();
        assert!(s.filter.is_empty());
        assert_eq!(s.count(), 2);
        assert_eq!(s.cursor, 0, "esc resets the cursor");
    }

    #[test]
    fn filtered_anchor_falls_to_clamp_when_excluded() {
        let mut s = state(vec![
            show(1, "Alpha", ListStatus::Watching, 1),
            show(2, "Beta", ListStatus::Watching, 1),
        ]);
        s.cursor = 1; // Beta
        s.filter = "alpha".into();
        s.on_filter_edited();
        assert_eq!(s.count(), 1);
        assert_eq!(s.cursor, 0, "anchored show filtered out clamps");
    }

    #[test]
    fn geometry_counts_headers_rules_and_blanks() {
        let s = state(vec![
            show(1, "A", ListStatus::Watching, 1),
            show(2, "B", ListStatus::Watching, 2),
            show(3, "C", ListStatus::Completed, 12),
        ]);
        let lines = s.layout();
        // Packed entries (title + bar, no inter-item blank), one header
        // hairline per group, a blank only between groups.
        assert_eq!(
            lines,
            vec![
                Li::Header {
                    status: ListStatus::Watching,
                    count: 2
                },
                Li::Rule,
                Li::Title { ord: 0 },
                Li::Bar { ord: 0 },
                Li::Title { ord: 1 },
                Li::Bar { ord: 1 },
                Li::Blank,
                Li::Header {
                    status: ListStatus::Completed,
                    count: 1
                },
                Li::Rule,
                Li::Title { ord: 2 },
                Li::Bar { ord: 2 },
            ]
        );
    }

    #[test]
    fn scroll_keeps_the_cursor_and_its_header_visible() {
        let rows: Vec<Show> = (1..=10)
            .map(|i| show(i, &format!("S{i}"), ListStatus::Watching, 1))
            .collect();
        let mut s = state(rows);
        s.nav(9, 8);
        let lines = s.layout();
        let title_ix = lines
            .iter()
            .position(|li| *li == Li::Title { ord: s.cursor })
            .unwrap();
        assert!(
            title_ix + 2 <= s.scroll + 8,
            "cursor rows inside the window"
        );
        s.jump(true, 8);
        assert_eq!(s.scroll, 0, "top jump restores the header rows");
    }

    #[test]
    fn bar_geometry_fills_marks_and_degrades() {
        // 6/12 in a 16-wide bar: half filled.
        assert_eq!(bar_geometry(6, Some(12), None, 16), (8, None));
        // Resume at episode 7: marker on the cell after the fill.
        assert_eq!(bar_geometry(6, Some(12), Some(7), 16), (8, Some(8)));
        // Overshoot clamps; marker clamps to the last cell.
        assert_eq!(bar_geometry(20, Some(12), Some(99), 16), (16, Some(14)));
        // Null total: one-third signal when engaged, empty when not (8.1).
        assert_eq!(bar_geometry(3, None, Some(2), 16), (5, None));
        assert_eq!(bar_geometry(0, None, None, 16), (0, None));
        assert_eq!(bar_geometry(0, Some(12), Some(1), 16), (0, Some(0)));
    }

    #[test]
    fn bar_width_scales_between_bounds() {
        assert_eq!(bar_width(30), BAR_MIN);
        assert_eq!(bar_width(46), BAR_MIN);
        assert_eq!(bar_width(50), 20);
        assert_eq!(bar_width(80), BAR_MAX);
    }
}
