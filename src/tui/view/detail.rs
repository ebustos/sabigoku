//! Detail: the full-screen zoom surface and, later, the shared in-pane detail
//! (DESIGN 5.3, 7.1). ROD-439 chunk 1 is navigation only (promote, demote,
//! origin); content, episode grid, and the cover land in chunks 3-4.
//!
//! Ownership contract: DetailState is the ONE owner of the shared detail
//! surface. List views push a selection snapshot in; this module never reads
//! Browse/History/Discover internals (the zigoku detail.zig coupling is the
//! failure this rule exists to prevent).

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::tui::theme::Palette;

#[derive(Debug, Default)]
pub struct DetailState {}

pub fn draw(_frame: &mut Frame<'_>, _area: Rect, _palette: &Palette, _state: &DetailState) {
    // Blank by design until chunk 3 pushes the first selection snapshot.
}
