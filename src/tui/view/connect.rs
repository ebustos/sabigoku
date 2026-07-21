//! AniList connect modal (DESIGN 5.5a). Captured overlay drawn last as a
//! compact centered `palette.elevated` float over Settings, never a full-pane
//! fill: title, instruction, fallback caption, URL band, spinner status, a
//! reserved paste-hint slot, then the key hints.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::tui::render::draw_centered;
use crate::tui::theme::Palette;

/// Preferred float size (DESIGN 5.5a), capped to the pane with a 2-col / 1-row
/// margin. Fixed like zigoku's box so the panel size is stable across URL wrap.
const BOX_W: u16 = 68;
const BOX_H: u16 = 19;

/// The URL band is capped to this many lines (zigoku parity); `c` copies the
/// whole URL regardless, so a truncated band bounds the row plan losslessly.
const MAX_BAND_LINES: usize = 3;
/// Below this width the float reads as clutter; draw one bare line instead. The
/// height floor is the row plan itself: fall back before the box clips a hint.
const MIN_COLS: u16 = 28;
/// The paste-hint fallback appears once the wait crosses this (DESIGN 5.5a).
const PASTE_HINT_SECS: u64 = 10;
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
    // Compact centered float, capped to the pane with a 2-col / 1-row margin.
    // Never a full-pane fill: Settings stays visible around the box, and the
    // box's bg_elevated is the only overlay signal (DESIGN 5.5a, borderless).
    let bw = BOX_W.min(area.width.saturating_sub(4));
    let bh = BOX_H.min(area.height.saturating_sub(2));

    // Wrap and cap the URL band against the box width, then size the row plan.
    // band_h is capped, so `total` is bounded and the fit check below is exact.
    let band_w = bw.saturating_sub(6).min(72);
    let inner_w = band_w.saturating_sub(4).max(8) as usize;
    let wrapped: Vec<String> = wrap(view.url, inner_w)
        .into_iter()
        .take(MAX_BAND_LINES)
        .collect();
    let band_h = wrapped.len() as u16;
    let total = 13 + band_h;

    if bw < MIN_COLS || bh < total {
        // Too cramped to hold every row: a bare one-line hint on its own
        // elevated strip. Gating on `total` (not a fixed floor) is what keeps
        // draw_centered from silently clipping a key hint off the box bottom.
        let row = area.height / 2;
        let strip = Rect::new(area.x, area.y + row, area.width, 1);
        frame.render_widget(Clear, strip);
        frame.render_widget(Block::new().style(Style::new().bg(palette.elevated)), strip);
        draw_centered(
            frame,
            area,
            row,
            Line::from(Span::styled(
                "connect: esc to stop waiting",
                Style::new().fg(palette.fg2),
            )),
        );
        return;
    }
    let modal = Rect::new(
        area.x + area.width.saturating_sub(bw) / 2,
        area.y + area.height.saturating_sub(bh) / 2,
        bw,
        bh,
    );

    // Clear the cells first: a bare Block only restyles glyphs, so Settings
    // would bleed through the fill (DESIGN 5.5a).
    frame.render_widget(Clear, modal);
    frame.render_widget(Block::new().style(Style::new().bg(palette.elevated)), modal);

    let secs = view.elapsed.as_secs();

    // Fixed row plan (offsets from the modal top); the paste slot is reserved
    // whether or not the hint shows, so the key hints never jump when it appears.
    let top = modal.height.saturating_sub(total) / 2;
    let at = |offset: u16| top + offset;

    draw_centered(
        frame,
        modal,
        at(0),
        Line::from(Span::styled(
            "Connect AniList",
            Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
        )),
    );
    draw_centered(
        frame,
        modal,
        at(2),
        Line::from(Span::styled(
            "approve access in your browser to continue",
            Style::new().fg(palette.fg2),
        )),
    );
    draw_centered(
        frame,
        modal,
        at(4),
        Line::from(Span::styled(
            "browser didn't open? use this link:",
            Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
        )),
    );
    draw_band(frame, modal, at(5), band_w, &wrapped, palette);

    let spin = SPINNER[(view.elapsed.as_millis() / 100) as usize % SPINNER.len()];
    let spin_color = if secs < ESCALATE_SECS {
        palette.focus
    } else {
        palette.hot
    };
    draw_centered(
        frame,
        modal,
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
            modal,
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
        modal,
        at(11 + band_h),
        key_hint(palette, "c", copy_action, copy_color),
    );
    draw_centered(
        frame,
        modal,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    // The real authorize URL shape (client id + 128-bit hex state): the long
    // input that made the pre-cap row plan overflow small boxes.
    const URL: &str = "https://anilist.co/api/v2/oauth/authorize?client_id=46528&response_type=token&state=9f2c7a1b4e6d0f3a5c8b1d2e4f6a7b9c";

    fn render(w: u16, h: u16, elapsed: Duration) -> Vec<String> {
        let view = ConnectView {
            url: URL,
            elapsed,
            copied: false,
        };
        let pal = &crate::tui::theme::TERMINAL_GHOST;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, Rect::new(0, 0, w, h), pal, &view))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// The exit affordance always renders, and when the full float draws the
    /// bottom of the row plan draws with it. Elapsed past the paste-hint
    /// threshold so the tallest row plan is in play. This is the guard the
    /// pre-cap plan failed: draw_centered clips off-box rows without a trace.
    #[test]
    fn float_never_clips_its_key_hints() {
        for w in 32u16..=120 {
            for h in [12u16, 14, 16, 20, 30, 45] {
                let screen = render(w, h, Duration::from_secs(25)).join("\n");
                // Reachable via the modal esc hint or the cramped fallback.
                assert!(
                    screen.contains("stop waiting"),
                    "no exit affordance at {w}x{h}:\n{screen}"
                );
                // Title present means the full float drew; the status and copy
                // hint below it must have drawn too (no silent mid-plan clip).
                if screen.contains("Connect AniList") {
                    assert!(
                        screen.contains("waiting for approval"),
                        "status clipped at {w}x{h}:\n{screen}"
                    );
                    assert!(
                        screen.contains("copy link"),
                        "copy hint clipped at {w}x{h}:\n{screen}"
                    );
                }
            }
        }
    }

    /// A float, not a takeover: on a roomy pane the box floats clear of every
    /// edge, so Settings shows around it (DESIGN 5.5a, the ticket's whole point).
    #[test]
    fn float_is_contained_not_full_bleed() {
        let rows = render(100, 34, Duration::from_secs(1));
        assert!(rows.iter().any(|r| r.contains("Connect AniList")));
        assert!(rows[0].chars().all(|c| c == ' '), "bled to the top edge");
        assert!(rows[33].chars().all(|c| c == ' '), "bled to the bottom edge");
        assert!(
            rows.iter().all(|r| r.starts_with(' ') && r.ends_with(' ')),
            "bled to a side edge"
        );
    }
}
