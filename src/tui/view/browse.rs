//! Browse: catalogue search over AniList (DESIGN 7.1). ROD-439 chunk 1 is the
//! first-run absent state and the search buffer; the debounced search worker,
//! result rows, and the two-pane detail arrive in chunk 3.

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::tui::render::draw_absent_block;
use crate::tui::theme::Palette;

#[derive(Debug, Default)]
pub struct BrowseState {
    pub query: String,
    /// Placeholder until results land (chunk 3); drives the search count tag.
    pub result_count: usize,
}

pub fn draw(frame: &mut Frame<'_>, area: Rect, palette: &Palette, _state: &BrowseState) {
    // First-run absent state (DESIGN 8.3): Browse names itself and teaches
    // the next action.
    draw_absent_block(
        frame,
        area,
        palette,
        "search the catalogue",
        ("/", "find anime"),
        ("P", "save"),
    );
}
