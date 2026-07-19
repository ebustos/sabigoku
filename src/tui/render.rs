//! Shared styling helpers (DESIGN 9). Pure: no app state, no stores.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::domain::{Cour, Season};

use super::theme::Palette;

/// Braille spinner frames, ~100ms per frame (DESIGN 4.8).
pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// DESIGN 2.3 season glyphs.
pub fn season_kanji(season: Season) -> &'static str {
    match season {
        Season::Winter => "冬",
        Season::Spring => "春",
        Season::Summer => "夏",
        Season::Fall => "秋",
    }
}

/// `冬 2026`-style chip for the ambient cour fallback (DESIGN 3.4).
pub fn cour_chip(cour: Cour) -> String {
    format!("{} {}", season_kanji(cour.season), cour.year)
}

/// Chip for a show/card's own season; absent unless both parts are known
/// (DESIGN 3.4: never an empty chip).
pub fn season_chip(season: Option<Season>, year: Option<u32>) -> Option<String> {
    Some(format!("{} {}", season_kanji(season?), year?))
}

/// Greedy word wrap to `width` display columns. A single word wider than the
/// line hard-breaks on grapheme boundaries so CJK prose still wraps.
pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0usize;
    for word in s.split_whitespace() {
        let w = display_width(word);
        if used > 0 && used + 1 + w > width {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if w > width {
            for g in word.graphemes(true) {
                let gw = UnicodeWidthStr::width(g);
                if used + gw > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push_str(g);
                used += gw;
            }
            continue;
        }
        if used > 0 {
            line.push(' ');
            used += 1;
        }
        line.push_str(word);
        used += w;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Compact list/card score badge (DESIGN 2.2): `[97]` / `[--]`.
pub fn score_badge(score: Option<u32>) -> String {
    match score {
        Some(s) => format!("[{s}]"),
        None => "[--]".to_string(),
    }
}

/// DESIGN 2.2 tier colours. `cap_hot` is the card rule (DESIGN 3.8): the 91+
/// tier steps down to fg so `TOP` keeps the one magenta pointer.
pub fn score_style(palette: &Palette, score: Option<u32>, cap_hot: bool) -> Style {
    match score {
        Some(s) if s >= 91 && !cap_hot => Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
        Some(s) if s >= 91 => Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
        Some(s) if s >= 76 => Style::new().fg(palette.fg),
        Some(s) if s >= 51 => Style::new().fg(palette.fg2),
        _ => Style::new().fg(palette.fg3),
    }
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
    fn wrap_breaks_on_words_and_respects_width() {
        let lines = wrap_text("the quick brown fox jumps over the lazy dog", 10);
        assert!(lines.iter().all(|l| display_width(l) <= 10));
        assert_eq!(
            lines.join(" "),
            "the quick brown fox jumps over the lazy dog"
        );
    }

    #[test]
    fn wrap_hard_breaks_oversized_words() {
        let lines = wrap_text(&"葬".repeat(12), 10);
        assert!(lines.iter().all(|l| display_width(l) <= 10));
        assert_eq!(lines.len(), 3, "24 columns of kanji over width 10");
    }

    #[test]
    fn wrap_zero_width_is_empty() {
        assert!(wrap_text("anything", 0).is_empty());
        assert!(wrap_text("", 10).is_empty());
    }

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
