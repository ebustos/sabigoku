//! Settings: live-editable rows in four sections (DESIGN 5.5). ROD-439
//! chunk 1 renders the section skeleton; rows, edit mode, and the
//! section-boundary assertion arrive in chunk 7. The AniList Sync section
//! ships inert in M1 (ROD-448).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use crate::tui::theme::Palette;

pub const SECTIONS: [&str; 4] = ["Player", "Catalog", "Interface", "AniList Sync"];

#[derive(Debug, Default)]
pub struct SettingsState {}

pub fn draw(frame: &mut Frame<'_>, area: Rect, palette: &Palette, _state: &SettingsState) {
    let mut y = 0u16;
    for section in SECTIONS {
        if y + 1 >= area.height {
            break;
        }
        let header = Rect::new(area.x + 2, area.y + y, area.width.saturating_sub(2), 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                section,
                Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
            )),
            header,
        );
        let rule = Rect::new(area.x + 2, area.y + y + 1, area.width.saturating_sub(3), 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "─".repeat(rule.width as usize),
                Style::new().fg(palette.chrome),
            )),
            rule,
        );
        y += 3;
    }
}
