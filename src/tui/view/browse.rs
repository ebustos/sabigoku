//! Browse: catalogue search over AniList (DESIGN 6.2, 8.4; 04 §4.2, §8).
//! Owns its search transport: debounce, drain, and staleness live here.
//! App routes keys and events in; deps arrive as scoped borrows.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::{Enrichment, preferred_title};
use crate::providers::CatalogProvider;
use crate::store::{Store, enrichment_ttl_secs};
use crate::tui::clock::{AsyncStart, Debounce};
use crate::tui::event::EventTx;
use crate::tui::render::{self, draw_absent_block};
use crate::tui::theme::Palette;
use crate::tui::view::ViewEnv;
use crate::tui::workers::{self, Drain};

/// Armed on the edited keystroke; fires when `now >= deadline` (04 §8).
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// Episode-count meta field earns space only on a wide pane; title > score >
/// eps (DESIGN 4.1).
const EPS_FIELD_MIN_W: u16 = 40;
/// Fixed meta-column widths (freeze parity): score `[100]`/`[--]`, eps
/// `1000 ep` worst case, with a gap between title, eps, and score. The marker
/// glyph plus its trailing space is two cells.
const SCORE_W: u16 = 5;
const EPS_W: u16 = 7;
const META_GAP: u16 = 2;
const MARKER_W: u16 = 2;

#[derive(Default)]
pub struct BrowseState {
    pub query: String,
    results: Vec<Enrichment>,
    cursor: usize,
    scroll: usize,
    /// Pages applied for `answered`; 0 until the first answer lands.
    page: u32,
    /// AniList's explicit hasNextPage for the applied results (port
    /// adaptation: replaces zigoku's short-page heuristic, see anilist.rs).
    has_next: bool,
    /// Spinner + slow escalation while a fetch is in flight; cleared when the
    /// answer for the live buffer lands (a stale answer keeps it spinning).
    started: Option<AsyncStart>,
    debounce: Debounce,
    drain: Drain,
    /// The query the visible results answered, for the 8.4 empty state.
    answered: Option<String>,
}

impl BrowseState {
    pub fn selected(&self) -> Option<&Enrichment> {
        self.results.get(self.cursor)
    }

    pub fn count(&self) -> usize {
        self.results.len()
    }

    /// Every buffer edit re-arms the debounce (DESIGN 6.2).
    pub fn on_query_edited(&mut self, now: Instant) {
        self.debounce.arm(now, SEARCH_DEBOUNCE);
    }

    /// Tick hook: fire the debounced fetch. An empty buffer fetches nothing
    /// (the existing results stay; no flash to empty, DESIGN 6.4).
    pub fn maybe_fire(&mut self, now: Instant, tx: &EventTx, catalog: &Arc<dyn CatalogProvider>) {
        if !self.debounce.fire(now) || self.query.is_empty() {
            return;
        }
        self.started = Some(AsyncStart::new(now));
        let spawned = workers::spawn_search(
            &self.drain,
            tx.clone(),
            Arc::clone(catalog),
            self.query.clone(),
            1,
        );
        if !spawned {
            self.started = None;
        }
    }

    /// `j`/Down at the last result fires the next page (05 §16, ROD-156
    /// parity). Gated on hasNextPage, the in-flight guard, and the results
    /// actually answering the live buffer.
    pub fn maybe_load_more(
        &mut self,
        now: Instant,
        tx: &EventTx,
        catalog: &Arc<dyn CatalogProvider>,
    ) {
        let at_end = self.cursor + 1 == self.results.len();
        let answers_buffer = self.answered.as_deref() == Some(self.query.as_str());
        if self.results.is_empty()
            || !at_end
            || !self.has_next
            || self.started.is_some()
            || !answers_buffer
        {
            return;
        }
        self.started = Some(AsyncStart::new(now));
        let spawned = workers::spawn_search(
            &self.drain,
            tx.clone(),
            Arc::clone(catalog),
            self.query.clone(),
            self.page + 1,
        );
        if !spawned {
            self.started = None;
        }
    }

    /// Apply a search answer; stale if the buffer moved on (04 §6). Page 1
    /// replaces and resets the cursor; a later page appends and keeps it
    /// (05 §16), with out-of-order pages dropped. Applied rows upsert
    /// catalog_cache best-effort (04 §10). True when applied (the recovery
    /// signal that clears the persistent AniList toast).
    pub fn on_done(
        &mut self,
        query: &str,
        page: u32,
        results: Vec<Enrichment>,
        has_next: bool,
        store: &Store,
        now_unix: i64,
    ) -> bool {
        if query != self.query {
            return false;
        }
        self.started = None;
        if page > 1 && (self.answered.as_deref() != Some(query) || page != self.page + 1) {
            return false;
        }
        for e in &results {
            let ttl = enrichment_ttl_secs(e.status.as_deref());
            let _ = store.upsert_catalog_cache(e, now_unix, Some(now_unix + ttl));
        }
        if page == 1 {
            self.results = results;
            self.cursor = 0;
            self.scroll = 0;
            self.answered = Some(query.to_string());
        } else {
            self.results.extend(results);
        }
        self.page = page;
        self.has_next = has_next;
        true
    }

    /// A failed fetch keeps the current results (DESIGN 8.5: cached results
    /// stay visible during an outage). True when it answered the live buffer.
    pub fn on_failed(&mut self, query: &str) -> bool {
        if query != self.query {
            return false;
        }
        self.started = None;
        true
    }

    /// j/k with clamp; g/G jump (DESIGN 6.1). `visible` is the list height.
    pub fn nav(&mut self, dy: i64, visible: usize) {
        if self.results.is_empty() {
            return;
        }
        let last = (self.results.len() - 1) as i64;
        self.cursor = (self.cursor as i64 + dy).clamp(0, last) as usize;
        self.clamp_scroll(visible);
    }

    pub fn jump(&mut self, top: bool, visible: usize) {
        if self.results.is_empty() {
            return;
        }
        self.cursor = if top { 0 } else { self.results.len() - 1 };
        self.clamp_scroll(visible);
    }

    fn clamp_scroll(&mut self, visible: usize) {
        let visible = visible.max(1);
        self.scroll = self.scroll.min(self.cursor);
        if self.cursor >= self.scroll + visible {
            self.scroll = self.cursor + 1 - visible;
        }
    }

    pub fn drain(&self, timeout: Duration) -> bool {
        self.drain.drain(timeout)
    }
}

/// The list column (DESIGN 4.1). The detail pane is drawn by the detail
/// module; App composes the two.
pub fn draw_list(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &BrowseState,
    env: &ViewEnv,
    list_focused: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    if state.results.is_empty() {
        draw_empty(frame, area, palette, state);
        return;
    }
    let visible = area.height as usize;
    for (i, entry) in state.results.iter().enumerate().skip(state.scroll) {
        if i >= state.scroll + visible {
            break;
        }
        let y = area.y + (i - state.scroll) as u16;
        let row = Rect::new(area.x, y, area.width, 1);
        draw_row(
            frame,
            row,
            palette,
            entry,
            env,
            i == state.cursor,
            list_focused,
        );
    }
    draw_footer(frame, area, palette, state, env.now);
}

/// Load-more footer on the row under the tail; a scrolled-full pane has no
/// spare row and draws none (freeze parity).
fn draw_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &BrowseState,
    now: Instant,
) {
    if !state.has_next {
        return;
    }
    let drawn = state.results.len() - state.scroll;
    if drawn >= area.height as usize {
        return;
    }
    let (text, style) = match &state.started {
        Some(s) => (
            format!(
                "{} loading…",
                render::SPINNER[s.frame(now, render::SPINNER.len())]
            ),
            Style::new().fg(palette.focus),
        ),
        None => ("╌ more ╌".to_string(), Style::new().fg(palette.fg3)),
    };
    render::draw_centered(
        frame,
        area,
        drawn as u16,
        Line::from(Span::styled(text, style)),
    );
}

/// One list row: `[glyph] [title…] [eps] [score]`, selection per the
/// focus-aware table (DESIGN 4.1).
fn draw_row(
    frame: &mut Frame<'_>,
    row: Rect,
    palette: &Palette,
    entry: &Enrichment,
    env: &ViewEnv,
    selected: bool,
    list_focused: bool,
) {
    if selected && list_focused {
        frame.render_widget(
            ratatui::widgets::Block::new().style(Style::new().bg(palette.surface)),
            row,
        );
    }
    let title_style = match (selected, list_focused) {
        (true, true) => Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
        (true, false) => Style::new().fg(palette.focus),
        _ => Style::new().fg(palette.fg),
    };
    let marker_style = if selected && list_focused {
        Style::new().fg(palette.focus)
    } else {
        Style::new().fg(palette.focus).add_modifier(Modifier::DIM)
    };

    // Fixed meta columns against the pane's right edge (DESIGN 4.3): score
    // always, episode count to its left on a wide pane. Fixed edges keep the
    // title's truncation point steady so the columns never jitter row-to-row.
    let show_eps = row.width >= EPS_FIELD_MIN_W;
    let score_x = row.width.saturating_sub(SCORE_W);
    let (eps_x, title_right) = if show_eps {
        let eps_x = score_x.saturating_sub(META_GAP + EPS_W);
        (Some(eps_x), eps_x.saturating_sub(META_GAP))
    } else {
        (None, score_x.saturating_sub(META_GAP))
    };

    // Marker + title first, clipped to the fixed meta edge; the meta columns
    // then render on top of their reserved right zone. The marker sits flush
    // at the pane origin so the list aligns with the rest of the chrome.
    let marker = if selected { "▸ " } else { "  " };
    let title = preferred_title(
        &entry.title_romaji,
        entry.title_english.as_deref(),
        entry.title_native.as_deref(),
        env.pref,
    );
    let title_budget = title_right.saturating_sub(MARKER_W) as usize;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(marker, marker_style),
            Span::styled(
                render::truncate_to_width(title, title_budget).into_owned(),
                title_style,
            ),
        ])),
        row,
    );

    // `{n} ep` when the count is known, else `[--]`; right-aligned in the
    // fixed slot (fg3), only on a wide pane. Clamp to the slot so an
    // out-of-range count can never bleed into the score column.
    if let Some(eps_x) = eps_x {
        let field = match entry.total_episodes {
            Some(t) => format!("{t} ep"),
            None => "[--]".to_string(),
        };
        let w = (field.len() as u16).min(EPS_W);
        let x = row.x + eps_x + EPS_W.saturating_sub(w);
        frame.render_widget(
            Paragraph::new(Span::styled(field, Style::new().fg(palette.fg3))),
            Rect::new(x, row.y, w, 1),
        );
    }

    let badge = render::score_badge(entry.score);
    let bw = badge.len() as u16;
    let x = row.x + score_x + SCORE_W.saturating_sub(bw);
    frame.render_widget(
        Paragraph::new(Span::styled(
            badge,
            render::score_style(palette, entry.score, false),
        )),
        Rect::new(x, row.y, bw, 1),
    );
}

/// First-run vs zero-results (DESIGN 8.3, 8.4): a query that answered empty
/// names itself; before any answer, Browse teaches the next action.
fn draw_empty(frame: &mut Frame<'_>, area: Rect, palette: &Palette, state: &BrowseState) {
    if let Some(answered) = &state.answered
        && !state.query.is_empty()
    {
        let mid = area.height / 2;
        render::draw_centered(
            frame,
            area,
            mid,
            Line::from(Span::styled(
                format!("no results for \"{answered}\""),
                Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
            )),
        );
        render::draw_centered(
            frame,
            area,
            mid + 1,
            Line::from(Span::styled(
                "try a different spelling",
                Style::new().fg(palette.fg3).add_modifier(Modifier::ITALIC),
            )),
        );
        return;
    }
    draw_absent_block(
        frame,
        area,
        palette,
        "search the catalogue",
        ("/", "find anime"),
        ("P", "save"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{CatalogError, CatalogPage};
    use crate::tui::event;

    fn entry(id: i64) -> Enrichment {
        Enrichment {
            anilist_id: id,
            title_romaji: format!("Show {id}"),
            ..Enrichment::default()
        }
    }

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    struct NoCatalog;
    impl CatalogProvider for NoCatalog {
        fn search(&self, _q: &str, _p: u32) -> Result<CatalogPage, CatalogError> {
            Err(CatalogError::Network)
        }
        fn discover(
            &self,
            _a: crate::providers::DiscoverAxis,
            _p: u32,
        ) -> Result<CatalogPage, CatalogError> {
            Err(CatalogError::Network)
        }
        fn enrich(&self, _id: i64) -> Result<Option<Enrichment>, CatalogError> {
            Ok(None)
        }
    }

    /// The fixed meta columns never overlap the title, at any width from the
    /// list-pane floor up: widest badge + widest eps field against a title
    /// long enough to want the whole row (ROD-458 review, F5/F6).
    #[test]
    fn row_meta_columns_never_overlap_the_title() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut e = entry(1);
        e.title_romaji = "A Very Long Title That Would Happily Run Into The Meta Zone".into();
        e.score = Some(100); // widest badge, "[100]"
        e.total_episodes = Some(1000); // widest eps, "1000 ep"
        let env = ViewEnv {
            pref: crate::domain::TitleLanguage::Romaji,
            kanji: false,
            cour: crate::domain::current_cour(0),
            unix_now: 0,
            now: Instant::now(),
            play: None,
        };
        let pal = &crate::tui::theme::TERMINAL_GHOST;
        for w in 30u16..=80 {
            let mut term = Terminal::new(TestBackend::new(w, 1)).unwrap();
            term.draw(|f| draw_row(f, Rect::new(0, 0, w, 1), pal, &e, &env, true, true))
                .unwrap();
            let buf = term.backend().buffer();
            let row: String = (0..w).map(|x| buf[(x, 0)].symbol()).collect();
            assert!(row.contains("[100]"), "width {w}: score clipped: {row:?}");
            if w >= EPS_FIELD_MIN_W {
                assert!(row.contains("1000 ep"), "width {w}: eps clipped: {row:?}");
            }
        }
    }

    #[test]
    fn debounce_gates_the_fetch() {
        let mut b = BrowseState::default();
        let (tx, _rx) = event::channel();
        let catalog: Arc<dyn CatalogProvider> = Arc::new(NoCatalog);
        let t0 = Instant::now();
        b.query.push('f');
        b.on_query_edited(t0);
        b.maybe_fire(
            t0 + SEARCH_DEBOUNCE - Duration::from_millis(1),
            &tx,
            &catalog,
        );
        assert!(b.started.is_none(), "before the deadline nothing fires");
        b.maybe_fire(t0 + SEARCH_DEBOUNCE, &tx, &catalog);
        assert!(b.started.is_some(), "deadline fires the fetch");
        assert!(b.drain(Duration::from_secs(5)));
    }

    #[test]
    fn empty_query_never_fetches() {
        let mut b = BrowseState::default();
        let (tx, _rx) = event::channel();
        let catalog: Arc<dyn CatalogProvider> = Arc::new(NoCatalog);
        let t0 = Instant::now();
        b.on_query_edited(t0);
        b.maybe_fire(t0 + SEARCH_DEBOUNCE, &tx, &catalog);
        assert!(b.started.is_none());
        assert_eq!(b.drain.inflight(), 0);
    }

    #[test]
    fn stale_results_are_dropped_and_keep_the_spinner() {
        let mut b = BrowseState::default();
        let s = store();
        b.query = "frieren".into();
        b.started = Some(AsyncStart::new(Instant::now()));
        assert!(
            !b.on_done("frier", 1, vec![entry(1)], false, &s, 1000),
            "stale query"
        );
        assert!(b.results.is_empty());
        assert!(b.started.is_some(), "a newer fetch is still owed");
        assert!(b.on_done("frieren", 1, vec![entry(2)], false, &s, 1000));
        assert_eq!(b.count(), 1);
        assert!(b.started.is_none());
    }

    #[test]
    fn applied_results_upsert_catalog_cache() {
        let mut b = BrowseState::default();
        let s = store();
        b.query = "x".into();
        assert!(b.on_done("x", 1, vec![entry(7)], false, &s, 1000));
        assert!(s.get_catalog(7).unwrap().is_some());
    }

    #[test]
    fn failure_keeps_current_results() {
        let mut b = BrowseState::default();
        let s = store();
        b.query = "a".into();
        b.on_done("a", 1, vec![entry(1), entry(2)], false, &s, 1000);
        b.query = "ab".into();
        assert!(b.on_failed("ab"));
        assert_eq!(b.count(), 2, "outage keeps cached results (DESIGN 8.5)");
        assert!(!b.on_failed("zzz"), "stale failure is not an answer");
    }

    /// Search calls recorded by page, answering Network so nothing applies.
    #[derive(Default)]
    struct PageProbe {
        pages: std::sync::Mutex<Vec<u32>>,
    }
    impl CatalogProvider for PageProbe {
        fn search(&self, _q: &str, p: u32) -> Result<CatalogPage, CatalogError> {
            self.pages.lock().unwrap().push(p);
            Err(CatalogError::Network)
        }
        fn discover(
            &self,
            _a: crate::providers::DiscoverAxis,
            _p: u32,
        ) -> Result<CatalogPage, CatalogError> {
            Err(CatalogError::Network)
        }
        fn enrich(&self, _id: i64) -> Result<Option<Enrichment>, CatalogError> {
            Ok(None)
        }
    }

    fn answered_page_one(b: &mut BrowseState, s: &Store, has_next: bool) {
        b.query = "a".into();
        b.on_done("a", 1, (1..=3).map(entry).collect(), has_next, s, 1000);
    }

    /// Down at the last result fires page 2 (05 §16, ROD-156 parity); the
    /// exhausted twin is the mutation check on the hasNextPage gate.
    #[test]
    fn load_more_fires_page_two_at_the_last_result() {
        let mut b = BrowseState::default();
        let s = store();
        let (tx, _rx) = event::channel();
        let probe = Arc::new(PageProbe::default());
        let catalog: Arc<dyn CatalogProvider> = Arc::clone(&probe) as _;
        answered_page_one(&mut b, &s, true);
        b.nav(1, 5);
        b.maybe_load_more(Instant::now(), &tx, &catalog);
        assert!(b.started.is_none(), "mid-list never fires");
        b.jump(false, 5);
        b.maybe_load_more(Instant::now(), &tx, &catalog);
        assert!(b.started.is_some(), "last result fires");
        b.maybe_load_more(Instant::now(), &tx, &catalog);
        assert!(b.drain(Duration::from_secs(5)));
        assert_eq!(
            *probe.pages.lock().unwrap(),
            vec![2],
            "in-flight never dups"
        );
    }

    #[test]
    fn load_more_holds_when_exhausted_or_buffer_moved_on() {
        let mut b = BrowseState::default();
        let s = store();
        let (tx, _rx) = event::channel();
        let probe = Arc::new(PageProbe::default());
        let catalog: Arc<dyn CatalogProvider> = Arc::clone(&probe) as _;
        answered_page_one(&mut b, &s, false);
        b.jump(false, 5);
        b.maybe_load_more(Instant::now(), &tx, &catalog);
        assert!(b.started.is_none(), "exhausted holds");
        b.has_next = true;
        b.query = "ab".into();
        b.maybe_load_more(Instant::now(), &tx, &catalog);
        assert!(b.started.is_none(), "an edited buffer holds");
        assert!(probe.pages.lock().unwrap().is_empty());
    }

    #[test]
    fn page_two_appends_and_keeps_the_cursor() {
        let mut b = BrowseState::default();
        let s = store();
        answered_page_one(&mut b, &s, true);
        b.jump(false, 5);
        assert!(b.on_done("a", 2, vec![entry(4), entry(5)], false, &s, 1000));
        assert_eq!(b.count(), 5, "page 2 appends");
        assert_eq!(b.cursor, 2, "append keeps the cursor");
        assert!(!b.has_next, "exhaustion adopts the page's hasNextPage");
        assert!(
            !b.on_done("a", 2, vec![entry(6)], true, &s, 1000),
            "duplicate page dropped"
        );
        assert!(
            !b.on_done("a", 4, vec![entry(7)], true, &s, 1000),
            "out-of-order page dropped"
        );
        assert_eq!(b.count(), 5);
    }

    #[test]
    fn nav_clamps_and_jumps() {
        let mut b = BrowseState::default();
        let s = store();
        b.query = "a".into();
        b.on_done("a", 1, (1..=10).map(entry).collect(), false, &s, 1000);
        b.nav(-1, 5);
        assert_eq!(b.cursor, 0);
        b.nav(3, 5);
        assert_eq!(b.cursor, 3);
        b.jump(false, 5);
        assert_eq!(b.cursor, 9);
        assert_eq!(b.scroll, 5, "scroll follows the jump");
        b.jump(true, 5);
        assert_eq!(b.cursor, 0);
        assert_eq!(b.scroll, 0);
    }
}
