//! AniList connect modal (DESIGN 5.5a). Captured overlay drawn last on a
//! `palette.elevated` fill. 7a is the functional cut; the spinner escalation,
//! 20s paste hint, and inset URL band land in 7b.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear};

use crate::tui::render::draw_centered;
use crate::tui::theme::Palette;

/// Below this the panel reads as clutter; draw one bare line instead (DESIGN 5.5a).
const MIN_COLS: u16 = 28;
const MIN_ROWS: u16 = 10;

const SPINNER: [char; 4] = ['⠋', '⠙', '⠹', '⠸'];

/// Render inputs, owned app-side as the connect session.
pub struct ConnectView<'a> {
    pub url: &'a str,
    pub elapsed_secs: u64,
    pub copied: bool,
}

pub fn draw(frame: &mut Frame<'_>, area: Rect, palette: &Palette, view: &ConnectView) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Clear the cells first: a bare Block only restyles glyphs, so Settings
    // would bleed through the fill (DESIGN 5.5a: the obscured view must not read
    // as interactive).
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(Style::new().bg(palette.elevated)), area);

    if area.width < MIN_COLS || area.height < MIN_ROWS {
        draw_centered(
            frame,
            area,
            area.height / 2,
            Line::from(Span::styled(
                "connect: esc to cancel",
                Style::new().fg(palette.fg2),
            )),
        );
        return;
    }

    let mid = area.height / 2;
    draw_centered(
        frame,
        area,
        mid.saturating_sub(4),
        Line::from(Span::styled(
            "Connect AniList",
            Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
        )),
    );
    draw_centered(
        frame,
        area,
        mid.saturating_sub(2),
        Line::from(Span::styled(
            "approve access in your browser to continue",
            Style::new().fg(palette.fg2),
        )),
    );
    let url = truncate(view.url, area.width.saturating_sub(4) as usize);
    draw_centered(
        frame,
        area,
        mid,
        Line::from(Span::styled(url, Style::new().fg(palette.fg2))),
    );
    let spin = SPINNER[view.elapsed_secs as usize % SPINNER.len()];
    draw_centered(
        frame,
        area,
        mid + 2,
        Line::from(Span::styled(
            format!("{spin} waiting for approval… {}s", view.elapsed_secs),
            Style::new().fg(palette.focus),
        )),
    );
    let copy = if view.copied { "copied ✓" } else { "copy link" };
    draw_centered(frame, area, mid + 4, key_hint(palette, "c", copy));
    draw_centered(frame, area, mid + 5, key_hint(palette, "esc", "cancel"));
}

fn key_hint<'a>(palette: &Palette, key: &'a str, action: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            key,
            Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(action, Style::new().fg(palette.fg2)),
    ])
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}
