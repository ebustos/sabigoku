//! Shared styling helpers (DESIGN 9). Pure: no app state, no stores.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::theme::Palette;

/// Braille spinner frames, ~100ms per frame (DESIGN 4.8).
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate to `max_cols` display columns on a grapheme boundary with a
/// trailing `…` (DESIGN 4.7). Width-aware, not chars: CJK counts double.
pub fn truncate_to_width(s: &str, max_cols: usize) -> Cow<'_, str> {
    if display_width(s) <= max_cols {
        return Cow::Borrowed(s);
    }
    let budget = max_cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for g in s.graphemes(true) {
        let w = UnicodeWidthStr::width(g);
        if used + w > budget {
            break;
        }
        used += w;
        out.push_str(g);
    }
    out.push('…');
    Cow::Owned(out)
}

/// Draw `line` horizontally centered on row `y` of `area`; clipped, never
/// panicking on tiny frames.
pub fn draw_centered(frame: &mut Frame<'_>, area: Rect, y: u16, line: Line<'_>) {
    if y >= area.height || area.width == 0 {
        return;
    }
    let w = line.width() as u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let rect = Rect::new(x, area.y + y, w.min(area.width), 1);
    frame.render_widget(ratatui::widgets::Paragraph::new(line), rect);
}

/// First-run absent block (DESIGN 8.3): actionable headline one tier brighter
/// than persistent absences, key glyphs focus + bold, secondary hint receded.
/// Headline at mid-2, primary hint at mid, secondary at mid+2.
pub fn draw_absent_block(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    headline: &str,
    primary: (&str, &str),
    secondary: (&str, &str),
) {
    let mid = area.height / 2;
    let key_style = Style::new().fg(palette.focus).add_modifier(Modifier::BOLD);
    draw_centered(
        frame,
        area,
        mid.saturating_sub(2),
        Line::from(Span::styled(
            headline.to_string(),
            Style::new().fg(palette.fg2).add_modifier(Modifier::ITALIC),
        )),
    );
    let hint = |key: &str, label: &str, fg| {
        Line::from(vec![
            Span::styled(key.to_string(), key_style),
            Span::styled(format!("  {label}"), Style::new().fg(fg)),
        ])
    };
    draw_centered(frame, area, mid, hint(primary.0, primary.1, palette.fg2));
    draw_centered(
        frame,
        area,
        mid + 2,
        hint(secondary.0, secondary.1, palette.fg3),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_passes_short_strings_through() {
        assert_eq!(truncate_to_width("short", 36), "short");
        assert!(matches!(truncate_to_width("short", 36), Cow::Borrowed(_)));
    }

    #[test]
    fn truncate_is_width_aware_for_cjk() {
        // 20 kanji = 40 columns; budget 36 keeps 17 kanji (34 cols) + `…`.
        let wide = "葬".repeat(20);
        let cut = truncate_to_width(&wide, 36);
        assert!(cut.ends_with('…'));
        assert!(display_width(&cut) <= 36);
        assert_eq!(cut.graphemes(true).count(), 18);
    }

    #[test]
    fn truncate_exact_fit_is_untouched() {
        let s = "a".repeat(36);
        assert_eq!(truncate_to_width(&s, 36), s.as_str());
    }
}
