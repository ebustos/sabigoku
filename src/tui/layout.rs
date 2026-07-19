//! Frame and pane geometry (DESIGN 3.2, 9.2). `pane_split` is the single
//! source of truth for the two-pane split; do not re-derive its numbers
//! elsewhere. Plain integer arithmetic, never a constraint solver: a solver's
//! rounding could drift from the DESIGN 3.2 formulas.

use ratatui::layout::Rect;

/// Two-pane threshold; below it Browse/History collapse to list-only and
/// `active_pane` clamps to List (DESIGN 3.2, 7.3).
pub const PANE_SPLIT_MIN: u16 = 60;

/// Degraded-frame floor (04 §8): below this the app renders a message frame,
/// never bails.
pub const MIN_W: u16 = 16;
pub const MIN_H: u16 = 4;

pub fn is_too_small(w: u16, h: u16) -> bool {
    w < MIN_W || h < MIN_H
}

#[derive(Debug, PartialEq, Eq)]
pub struct PaneSplit {
    pub list_w: u16,
    pub detail_x: u16,
    pub detail_w: u16,
}

/// DESIGN 3.2: list 38% of terminal width, min 30 cols; 2-cell left margin,
/// 2-cell gap, 1-cell right margin.
pub fn pane_split(w: u16) -> PaneSplit {
    let list_w = (w * 38 / 100).max(30);
    let detail_x = 2 + list_w + 2;
    PaneSplit {
        list_w,
        detail_x,
        detail_w: w.saturating_sub(detail_x).saturating_sub(1),
    }
}

/// The shared chrome rows: top bar, content band, bottom bar (DESIGN 9.2).
#[derive(Debug, PartialEq, Eq)]
pub struct FrameRows {
    pub top: Rect,
    pub content: Rect,
    pub bottom: Rect,
}

/// Content is everything between the top bar + spacer and the bottom bar.
pub fn frame_rows(area: Rect) -> FrameRows {
    let top = Rect {
        height: 1.min(area.height),
        ..area
    };
    let bottom_y = area.y + area.height.saturating_sub(1);
    let bottom = Rect {
        y: bottom_y,
        height: 1.min(area.height),
        ..area
    };
    let content_y = area.y + 2;
    let content_h = area.height.saturating_sub(3);
    let content = Rect {
        y: content_y.min(bottom_y),
        height: content_h,
        ..area
    };
    FrameRows {
        top,
        content,
        bottom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DESIGN 3.2 sample-width table.
    #[test]
    fn pane_split_matches_the_design_table() {
        for (w, list_w, detail_w) in [(80, 30, 45), (100, 38, 57), (120, 45, 70), (160, 60, 95)] {
            let split = pane_split(w);
            assert_eq!(split.list_w, list_w, "list at {w}");
            assert_eq!(split.detail_x, 2 + list_w + 2, "x at {w}");
            assert_eq!(split.detail_w, detail_w, "detail at {w}");
        }
    }

    #[test]
    fn pane_split_clamps_the_list_minimum() {
        assert_eq!(pane_split(60).list_w, 30);
        assert_eq!(pane_split(0).detail_w, 0);
    }

    #[test]
    fn frame_rows_carve_top_content_bottom() {
        let rows = frame_rows(Rect::new(0, 0, 80, 24));
        assert_eq!(rows.top, Rect::new(0, 0, 80, 1));
        assert_eq!(rows.content, Rect::new(0, 2, 80, 21));
        assert_eq!(rows.bottom, Rect::new(0, 23, 80, 1));
    }

    #[test]
    fn frame_rows_survive_tiny_areas() {
        for h in 0..4 {
            let rows = frame_rows(Rect::new(0, 0, 10, h));
            assert!(rows.content.height <= h);
            assert!(rows.bottom.y <= h);
        }
    }

    #[test]
    fn too_small_thresholds() {
        assert!(is_too_small(15, 10));
        assert!(is_too_small(80, 3));
        assert!(!is_too_small(16, 4));
    }
}
