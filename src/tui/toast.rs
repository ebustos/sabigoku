//! Toast queue (DESIGN 4.7, 4.10; 04 §8). Terminal outcomes only; in-progress
//! state is the spinner channel, and the two are not interchangeable.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;

use super::render::{display_width, truncate_to_width};
use super::theme::Palette;

/// Whole box, glyph prefix included (DESIGN 4.7).
pub const MAX_BOX_COLS: usize = 40;
pub const GLYPH_COLS: usize = 4;
pub const MAX_COPY_COLS: usize = MAX_BOX_COLS - GLYPH_COLS;
pub const TTL: Duration = Duration::from_millis(2500);
pub const MAX_VISIBLE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Info,
    Success,
    Error,
    Warn,
}

impl Kind {
    fn glyph(self) -> &'static str {
        match self {
            Kind::Info => "[~] ",
            Kind::Success => "[✓] ",
            Kind::Error | Kind::Warn => "[!] ",
        }
    }

    fn style(self, palette: &Palette) -> Style {
        let base = Style::new().bg(palette.elevated);
        match self {
            Kind::Info => base.fg(palette.fg2),
            Kind::Success => base.fg(palette.fg).add_modifier(Modifier::BOLD),
            Kind::Error => base.fg(palette.hot).add_modifier(Modifier::BOLD),
            Kind::Warn => base.fg(palette.warn),
        }
    }
}

#[derive(Debug)]
pub struct Toast {
    pub kind: Kind,
    pub copy: String,
    /// Persistence is reserved for ongoing conditions still true while the
    /// toast is visible; cleared by the recovery path, never by TTL.
    pub topic: Option<&'static str>,
    born: Instant,
}

/// Cap 3, oldest-first eviction of non-persistent entries; a persistent topic
/// is a singleton refreshed in place (04 §8).
#[derive(Debug, Default)]
pub struct Toasts {
    queue: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, kind: Kind, copy: &str, now: Instant) {
        self.insert(kind, copy, None, now);
    }

    pub fn push_persistent(&mut self, kind: Kind, copy: &str, topic: &'static str, now: Instant) {
        if let Some(t) = self.queue.iter_mut().find(|t| t.topic == Some(topic)) {
            t.kind = kind;
            t.copy = truncate_to_width(copy, MAX_COPY_COLS).into_owned();
            t.born = now;
            return;
        }
        self.insert(kind, copy, Some(topic), now);
    }

    fn insert(&mut self, kind: Kind, copy: &str, topic: Option<&'static str>, now: Instant) {
        if self.queue.len() >= MAX_VISIBLE {
            match self.queue.iter().position(|t| t.topic.is_none()) {
                Some(oldest) => {
                    self.queue.remove(oldest);
                }
                // Every slot is a live ongoing condition; those outrank a
                // transient, so the incoming toast is the one dropped.
                None => return,
            }
        }
        self.queue.push(Toast {
            kind,
            copy: truncate_to_width(copy, MAX_COPY_COLS).into_owned(),
            topic,
            born: now,
        });
    }

    /// Recovery path for a persistent condition (DESIGN 8.5).
    pub fn clear_topic(&mut self, topic: &'static str) -> bool {
        let before = self.queue.len();
        self.queue.retain(|t| t.topic != Some(topic));
        self.queue.len() != before
    }

    /// Expire non-persistent toasts past TTL; true when anything changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let before = self.queue.len();
        self.queue
            .retain(|t| t.topic.is_some() || now.duration_since(t.born) < TTL);
        self.queue.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.queue.iter()
    }

    /// Right-aligned, oldest at row h-2, stacking upward (DESIGN 4.7).
    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, palette: &Palette) {
        for (i, toast) in self.queue.iter().enumerate() {
            let Some(offset) = area.height.checked_sub(2 + i as u16) else {
                break;
            };
            let text = format!("{}{}", toast.kind.glyph(), toast.copy);
            let w = (display_width(&text) as u16).min(area.width);
            let rect = Rect::new(area.x + area.width - w, area.y + offset, w, 1);
            frame.render_widget(Paragraph::new(text).style(toast.kind.style(palette)), rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn copy_is_truncated_to_the_36_col_budget() {
        let mut toasts = Toasts::default();
        toasts.push(Kind::Error, &"x".repeat(50), t0());
        let copy = &toasts.iter().next().unwrap().copy;
        assert!(copy.ends_with('…'));
        assert!(display_width(copy) <= MAX_COPY_COLS);
    }

    #[test]
    fn cap_evicts_oldest_non_persistent_first() {
        let now = t0();
        let mut toasts = Toasts::default();
        toasts.push_persistent(Kind::Error, "anilist down", "anilist", now);
        toasts.push(Kind::Info, "first", now);
        toasts.push(Kind::Info, "second", now);
        toasts.push(Kind::Success, "third", now);
        let copies: Vec<_> = toasts.iter().map(|t| t.copy.as_str()).collect();
        assert_eq!(copies, ["anilist down", "second", "third"]);
    }

    #[test]
    fn all_persistent_drops_the_incoming_transient() {
        let now = t0();
        let mut toasts = Toasts::default();
        for topic in ["a", "b", "c"] {
            toasts.push_persistent(Kind::Error, topic, topic, now);
        }
        toasts.push(Kind::Info, "transient", now);
        assert_eq!(toasts.iter().count(), 3);
        assert!(toasts.iter().all(|t| t.topic.is_some()));
    }

    #[test]
    fn persistent_topic_refreshes_in_place() {
        let now = t0();
        let mut toasts = Toasts::default();
        toasts.push_persistent(Kind::Error, "down", "anilist", now);
        toasts.push_persistent(Kind::Error, "still down", "anilist", now + TTL);
        assert_eq!(toasts.iter().count(), 1);
        assert_eq!(toasts.iter().next().unwrap().copy, "still down");
    }

    #[test]
    fn ttl_expires_transients_but_never_persistent() {
        let now = t0();
        let mut toasts = Toasts::default();
        toasts.push(Kind::Info, "gone soon", now);
        toasts.push_persistent(Kind::Error, "stays", "anilist", now);
        assert!(!toasts.tick(now + TTL - Duration::from_millis(1)));
        assert!(toasts.tick(now + TTL));
        assert_eq!(toasts.iter().count(), 1);
        assert_eq!(toasts.iter().next().unwrap().copy, "stays");
    }

    #[test]
    fn clear_topic_is_the_recovery_path() {
        let now = t0();
        let mut toasts = Toasts::default();
        toasts.push_persistent(Kind::Error, "down", "anilist", now);
        assert!(toasts.clear_topic("anilist"));
        assert!(!toasts.clear_topic("anilist"));
        assert!(toasts.is_empty());
    }
}
