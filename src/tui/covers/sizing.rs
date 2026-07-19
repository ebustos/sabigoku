//! Cover geometry (DESIGN 3.3 / 3.8): width tiers, adaptive heights from
//! reported cell pixels, fixed floors and caps when geometry is unreported.
//! The grid-yield caps (`cover_height_cap`, `synopsis_cap`) are NOT here;
//! DESIGN 3.3 pins them to `view/detail.rs` (ROD-439).

use ratatui_image::FontSize;

/// DESIGN 3.8 width tiers: >= 80 cols is a 20-col cover in a 22-col slot,
/// below is 14 in 16. Select from the effective column width in pane
/// contexts, never raw terminal width (DESIGN 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    pub large: bool,
    pub cover_w: u16,
    pub slot_w: u16,
}

pub fn tier(effective_w: u16) -> Tier {
    if effective_w >= 80 {
        Tier {
            large: true,
            cover_w: 20,
            slot_w: 22,
        }
    } else {
        Tier {
            large: false,
            cover_w: 14,
            slot_w: 16,
        }
    }
}

/// Card cover height (DESIGN 3.8): a ~2:3 poster fills the card width.
/// `cell` None (tmux, headless) falls back to the fixed floors 7/5; the
/// adaptive value never shrinks below them. No upper cap by design.
pub fn card_cover_h(t: &Tier, cell: Option<FontSize>) -> u16 {
    let floor = if t.large { 7 } else { 5 };
    let Some(cell) = cell else { return floor };
    poster_h(t.cover_w, cell).max(floor)
}

/// Detail cover height (DESIGN 3.3): adaptive, clamped to the 28/20 aesthetic
/// caps, which double as the fixed fallback when geometry is unreported.
pub fn detail_cover_h(t: &Tier, cell: Option<FontSize>) -> u16 {
    let cap = if t.large { 28 } else { 20 };
    let Some(cell) = cell else { return cap };
    poster_h(t.cover_w, cell).clamp(1, cap)
}

/// Rows for a 2:3 poster spanning `cover_w` columns at `cell` pixels.
fn poster_h(cover_w: u16, cell: FontSize) -> u16 {
    let w_px = cover_w as u32 * cell.width as u32;
    ((w_px * 3 / 2) / cell.height.max(1) as u32) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_tiers_split_at_80() {
        assert_eq!(
            tier(80),
            Tier {
                large: true,
                cover_w: 20,
                slot_w: 22
            }
        );
        assert_eq!(
            tier(79),
            Tier {
                large: false,
                cover_w: 14,
                slot_w: 16
            }
        );
    }

    #[test]
    fn ghostty_cells_land_the_ratified_heights() {
        // Measured for real in ROD-417: 9x20 px cells, large tier, cover_h 13.
        let cell = Some(FontSize::new(9, 20));
        assert_eq!(card_cover_h(&tier(100), cell), 13);
        assert_eq!(card_cover_h(&tier(60), cell), 9);
        assert_eq!(detail_cover_h(&tier(100), cell), 13);
    }

    #[test]
    fn unreported_geometry_falls_back_to_floors_and_caps() {
        assert_eq!(card_cover_h(&tier(100), None), 7);
        assert_eq!(card_cover_h(&tier(60), None), 5);
        assert_eq!(detail_cover_h(&tier(100), None), 28);
        assert_eq!(detail_cover_h(&tier(60), None), 20);
    }

    #[test]
    fn adaptive_never_shrinks_below_floor() {
        // Wide flat cells push the derived height under the floor.
        let cell = Some(FontSize::new(4, 40));
        assert_eq!(card_cover_h(&tier(100), cell), 7);
        assert_eq!(card_cover_h(&tier(60), cell), 5);
    }

    #[test]
    fn detail_clamps_to_cap_but_card_does_not() {
        // Square cells derive 30 rows for the large tier.
        let cell = Some(FontSize::new(10, 10));
        assert_eq!(card_cover_h(&tier(100), cell), 30);
        assert_eq!(detail_cover_h(&tier(100), cell), 28);
        assert_eq!(detail_cover_h(&tier(60), cell), 20);
    }

    #[test]
    fn degenerate_cell_height_is_survived() {
        let cell = Some(FontSize::new(9, 0));
        assert!(card_cover_h(&tier(100), cell) >= 7);
        assert!(detail_cover_h(&tier(100), cell) <= 28);
    }
}
