//! Detail: the shared show surface, in-pane and full-screen zoom (DESIGN 3.3,
//! 4.4, 5.3). ROD-439 chunk 3 is the single-column form: header, chips,
//! score, synopsis, cover. The episode grid, meta rail, and two-column split
//! land in chunk 4.
//!
//! Ownership contract: DetailState is the ONE owner of the shared detail
//! surface. List views push a selection snapshot in; this module never reads
//! Browse/History/Discover internals (the zigoku detail.zig coupling is the
//! failure this rule exists to prevent). Cover transport (drain, settle
//! debounce, single-flight) lives here, not on App.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::domain::{Enrichment, Season, preferred_title};
use crate::tui::clock::Debounce;
use crate::tui::covers::detail::{Action, CoverState};
use crate::tui::covers::render::ProtocolPool;
use crate::tui::covers::{CoverCaches, DETAIL_KEY, sizing};
use crate::tui::event::EventTx;
use crate::tui::render::{self, SPINNER};
use crate::tui::theme::Palette;
use crate::tui::view::ViewEnv;
use crate::tui::workers::{self, Drain};

/// Continuous-scroll cover settle (04 §8); discrete navigation syncs on the
/// same tick (DESIGN 6.4).
pub const COVER_SETTLE: Duration = Duration::from_millis(150);

/// Rows reserved below the cover in the single-column layout: worst-case
/// header, a 2-line synopsis, the grid's spacer, and 2 grid rows, so the
/// episode grid always keeps >= 2 visible rows (DESIGN 3.3). The grid itself
/// lands in chunk 4; the reserve holds its ground now so nothing reflows.
const COVER_RESERVE: u16 = 12;
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

/// Cover tier from the effective column width (DESIGN 3.2): never terminal
/// width, and hard-capped at 20 cols.
fn cover_width(detail_w: u16) -> u16 {
    match detail_w {
        w if w >= 40 => 20,
        w if w >= 25 => 14,
        _ => 0,
    }
}

#[derive(Default)]
pub struct DetailState {
    shown: Option<Enrichment>,
    scroll: u16,
    cover: CoverState,
    /// Spinner epoch for the in-flight fetch (frame phase + slow escalation).
    started: Option<crate::tui::clock::AsyncStart>,
    drain: Drain,
    settle: Debounce,
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
        }
        self.shown = Some(entry.clone());
        let window = if discrete {
            Duration::ZERO
        } else {
            COVER_SETTLE
        };
        self.settle.arm(now, window);
    }

    /// Nothing selected: the pane clears, no stale detail (DESIGN 8.4).
    pub fn clear_target(&mut self, pool: &mut ProtocolPool) {
        self.shown = None;
        self.scroll = 0;
        self.cover.clear();
        pool.remove(DETAIL_KEY);
    }

    pub fn scroll_by(&mut self, delta: i64) {
        self.scroll = (self.scroll as i64 + delta).max(0) as u16;
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
                pool.set(DETAIL_KEY, img);
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

    pub fn drain(&self, timeout: Duration) -> bool {
        self.drain.drain(timeout)
    }
}

/// The persistent right-hand pane: surface-tier background marks the pane
/// boundary without a border (DESIGN 3.1).
pub fn draw_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DetailState,
    env: &ViewEnv,
    pool: &mut ProtocolPool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Block::new().style(Style::new().bg(palette.surface)), area);
    draw_content(frame, area, palette, state, env, pool);
}

/// The full-screen zoom: same single-column content on the whole canvas
/// (DESIGN 5.3; two-column split lands in chunk 4).
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
    draw_content(frame, body, palette, state, env, pool);
}

fn draw_content(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &DetailState,
    env: &ViewEnv,
    pool: &mut ProtocolPool,
) {
    let Some(entry) = state.shown() else {
        return;
    };
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

    // Header stack: title, alt rows, chips row, score line (DESIGN 4.4).
    let title = preferred_title(
        &entry.title_romaji,
        entry.title_english.as_deref(),
        entry.title_native.as_deref(),
        env.pref,
    );
    let mut lines: Vec<Line<'_>> = Vec::new();
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

    for line in &lines {
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

    draw_synopsis(frame, area, palette, entry, state.scroll, y);
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
    if state.cover.has_pixels() && pool.render(frame, cover, DETAIL_KEY) {
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
) {
    let remaining = area.height.saturating_sub(y);
    let cap = synopsis_cap(remaining);
    if cap == 0 {
        return;
    }
    let indent = 2u16;
    let width = area.width.saturating_sub(indent) as usize;
    let Some(text) = entry.description.as_deref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no synopsis yet",
                Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
            )),
            Rect::new(area.x + indent, area.y + y, area.width - indent, 1),
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
            Rect::new(
                area.x + indent,
                area.y + y + i as u16,
                area.width - indent,
                1,
            ),
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
        push(&mut spans, chip, Style::new().fg(palette.fg2));
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
        assert_eq!(cover_height_cap(32), 20);
        // 32 - blank - 20 cover - blank = 10 rows: worst header (5 lines +
        // blank) + synopsis 2 + grid spacer + 2 grid rows.
        assert!(cover_height_cap(32) <= 32 - COVER_RESERVE);
        assert_eq!(cover_height_cap(17), 0, "below 6 rows the poster drops");
        assert_eq!(synopsis_cap(5), 2);
        assert_eq!(synopsis_cap(3), 0);
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
