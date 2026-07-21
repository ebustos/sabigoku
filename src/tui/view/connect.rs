//! AniList connect modal (DESIGN 5.5a). Captured overlay drawn last on a
//! `palette.elevated` fill: title, instruction, fallback caption, URL band,
//! spinner status, a 20s paste-hint slot, then the key hints.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::tui::render::draw_centered;
use crate::tui::theme::Palette;

/// Below this the panel reads as clutter; draw one bare line instead (DESIGN 5.5a).
const MIN_COLS: u16 = 28;
const MIN_ROWS: u16 = 10;
/// The paste-hint fallback appears once the wait crosses this (DESIGN 5.5a).
const PASTE_HINT_SECS: u64 = 20;
/// Spinner escalates focus -> hot past this (§4.8 slow-path convention).
const ESCALATE_SECS: u64 = 3;

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Render inputs, owned app-side as the connect session.
pub struct ConnectView<'a> {
    pub url: &'a str,
    pub elapsed: Duration,
    pub copied: bool,
}

pub fn draw(frame: &mut Frame<'_>, area: Rect, palette: &Palette, view: &ConnectView) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // Clear the cells first: a bare Block only restyles glyphs, so Settings
    // would bleed through the fill (DESIGN 5.5a).
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(Style::new().bg(palette.elevated)), area);

    if area.width < MIN_COLS || area.height < MIN_ROWS {
        draw_centered(
            frame,
            area,
            area.height / 2,
            Line::from(Span::styled(
                "connect: esc to stop waiting",
                Style::new().fg(palette.fg2),
            )),
        );
        return;
    }

    let secs = view.elapsed.as_secs();
    let band_w = area.width.saturating_sub(6).min(72);
    let inner_w = band_w.saturating_sub(4).max(8) as usize;
    let wrapped = wrap(view.url, inner_w);
    let band_h = wrapped.len() as u16;

    // Fixed row plan (offsets from the block top); the paste slot is reserved
    // whether or not the hint shows, so the key hints never jump at 20s.
    let total = 13 + band_h;
    let top = area.height.saturating_sub(total) / 2;
    let at = |offset: u16| top + offset;

    draw_centered(
        frame,
        area,
        at(0),
        Line::from(Span::styled(
            "Connect AniList",
            Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
        )),
    );
    draw_centered(
        frame,
        area,
        at(2),
        Line::from(Span::styled(
            "approve access in your browser to continue",
            Style::new().fg(palette.fg2),
        )),
    );
    draw_centered(
        frame,
        area,
        at(4),
        Line::from(Span::styled(
            "browser didn't open? use this link:",
            Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
        )),
    );
    draw_band(frame, area, at(5), band_w, &wrapped, palette);

    let spin = SPINNER[(view.elapsed.as_millis() / 100) as usize % SPINNER.len()];
    let spin_color = if secs < ESCALATE_SECS {
        palette.focus
    } else {
        palette.hot
    };
    draw_centered(
        frame,
        area,
        at(6 + band_h),
        Line::from(vec![
            Span::styled(format!("{spin} "), Style::new().fg(spin_color)),
            Span::styled(
                format!("waiting for approval… {secs}s"),
                Style::new().fg(palette.fg2),
            ),
        ]),
    );

    if secs >= PASTE_HINT_SECS {
        draw_centered(
            frame,
            area,
            at(8 + band_h),
            Line::from(vec![
                Span::styled("no callback? run  ", Style::new().fg(palette.warn)),
                Span::styled(
                    "sabigoku login --paste",
                    Style::new().fg(palette.warn).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  in a terminal", Style::new().fg(palette.warn)),
            ]),
        );
    }

    let (copy_action, copy_color) = if view.copied {
        ("copied ✓", palette.fg)
    } else {
        ("copy link", palette.fg2)
    };
    draw_centered(
        frame,
        area,
        at(11 + band_h),
        key_hint(palette, "c", copy_action, copy_color),
    );
    draw_centered(
        frame,
        area,
        at(12 + band_h),
        // "stop waiting", not "cancel": a callback already landing still
        // completes; esc only stops the wait (ROD-448 review).
        key_hint(palette, "esc", "stop waiting", palette.fg2),
    );
}

/// The URL in its own `palette.surface` inset band (DESIGN 5.5a): a real
/// fallback action, not near-invisible dim text.
fn draw_band(frame: &mut Frame<'_>, area: Rect, y: u16, band_w: u16, lines: &[String], palette: &Palette) {
    if y >= area.height {
        return;
    }
    let x = area.x + area.width.saturating_sub(band_w) / 2;
    for (i, line) in lines.iter().enumerate() {
        let row = y + i as u16;
        if row >= area.height {
            break;
        }
        let rect = Rect::new(x, area.y + row, band_w.min(area.width), 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {line}"),
                Style::new().fg(palette.fg2),
            )))
            .style(Style::new().bg(palette.surface)),
            rect,
        );
    }
}

fn key_hint<'a>(palette: &Palette, key: &'a str, action: &'a str, action_color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            key,
            Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(action, Style::new().fg(action_color)),
    ])
}

/// Hard char-chunk wrap: a URL has no spaces to break on.
fn wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    s.chars()
        .collect::<Vec<_>>()
        .chunks(width)
        .map(|c| c.iter().collect())
        .collect()
}
