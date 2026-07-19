//! History/Watchlist: the default landing view (DESIGN 8.3). ROD-439 chunk 1
//! is the first-run empty state and the filter buffer; the load worker, rows,
//! progress bars, and status keys arrive in chunk 5.

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::tui::render::draw_absent_block;
use crate::tui::theme::Palette;

#[derive(Debug, Default)]
pub struct HistoryState {
    pub filter: String,
    /// Placeholder until the load worker lands (chunk 5); drives the empty
    /// state and the filter count tag.
    pub row_count: usize,
}

impl HistoryState {
    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }
}

pub fn draw(frame: &mut Frame<'_>, area: Rect, palette: &Palette, state: &HistoryState) {
    if state.is_empty() {
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
    }
}
