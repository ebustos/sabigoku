//! Discover: full-canvas card grid over four AniList axes (DESIGN 3.8).
//! ROD-439 chunk 1 is the passive axis bar and axis selection; the per-axis
//! feed slots, card grid, and cover pump arrive in chunk 2.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::tui::theme::Palette;

/// Freeze enum order; UI tab order matches (04 §7.5).
pub const AXES: [&str; 4] = ["Trending", "Popular", "Top Rated", "This Season"];

#[derive(Debug, Default)]
pub struct DiscoverState {
    pub axis: usize,
}

impl DiscoverState {
    /// `[` / `]` cycle with wraparound (DESIGN 7.5).
    pub fn cycle_axis(&mut self, delta: i64) {
        let n = AXES.len() as i64;
        self.axis = (self.axis as i64 + delta).rem_euclid(n) as usize;
    }

    /// `1`-`4` direct select; out-of-range keys are ignored.
    pub fn select_axis(&mut self, index: usize) {
        if index < AXES.len() {
            self.axis = index;
        }
    }
}

pub fn draw(frame: &mut Frame<'_>, area: Rect, palette: &Palette, state: &DiscoverState) {
    if area.height == 0 {
        return;
    }
    // Axis bar: passive, teaches its own 1-4 binds in place (DESIGN 3.8).
    let mut spans: Vec<Span<'_>> = vec![Span::raw("  ")];
    for (i, name) in AXES.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(palette.fg3)));
        }
        let active = i == state.axis;
        let (key_style, label_style) = if active {
            (
                Style::new().fg(palette.focus),
                Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
            )
        } else {
            (Style::new().fg(palette.fg2), Style::new().fg(palette.fg2))
        };
        spans.push(Span::styled(format!("[{}]", i + 1), key_style));
        spans.push(Span::styled(format!(" {name}"), label_style));
    }
    let bar = Rect { height: 1, ..area };
    frame.render_widget(Paragraph::new(Line::from(spans)), bar);
    // The card grid fills the rest from chunk 2; blank by design until then.
}
