//! Settings: live-editable rows in four sections (DESIGN 5.5). The state owns
//! the row table, cursor, text-edit buffer, and the dirty flag; key handling
//! mutates a caller-passed `&mut Config` and never reaches into App (App
//! projects palette/translation on `ConfigChanged` and persists on leave).
//! The AniList Sync section ships with `connect` inert (ROD-448).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Paragraph};

use ratatui::crossterm::event::KeyCode;

use crate::config::Config;
use crate::tui::theme::Palette;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowId {
    MpvPath,
    Quality,
    Translation,
    ResumeOffset,
    SkipMode,
    Provider,
    CoverArt,
    KanjiChips,
    Palette,
    Landing,
    TitleLanguage,
    Connect,
    Sync,
}

/// `Action` is the one Enter-fires-a-side-effect kind (the ROD-448 connect
/// row); its value column is always empty, the hint carries the affordance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Text,
    Cycle,
    Toggle,
    Action,
}

pub struct Row {
    pub id: RowId,
    pub label: &'static str,
    pub kind: RowKind,
    pub hint: &'static str,
}

const fn row(id: RowId, label: &'static str, kind: RowKind, hint: &'static str) -> Row {
    Row {
        id,
        label,
        kind,
        hint,
    }
}

/// Interactive rows only; the read-only rows (two Catalog, one AniList Sync)
/// render separately and are skipped by navigation (DESIGN 5.5).
pub const ROWS: [Row; 13] = [
    row(RowId::MpvPath, "mpv path", RowKind::Text, "enter to edit"),
    row(
        RowId::Quality,
        "default quality",
        RowKind::Cycle,
        "hjkl to cycle",
    ),
    row(
        RowId::Translation,
        "translation",
        RowKind::Cycle,
        "hjkl to cycle",
    ),
    row(
        RowId::ResumeOffset,
        "resume offset",
        RowKind::Cycle,
        "hjkl to cycle",
    ),
    row(
        RowId::SkipMode,
        "skip mode",
        RowKind::Cycle,
        "hjkl to cycle",
    ),
    row(RowId::Provider, "provider", RowKind::Cycle, "hjkl to cycle"),
    row(
        RowId::CoverArt,
        "cover art",
        RowKind::Toggle,
        "space to toggle",
    ),
    row(
        RowId::KanjiChips,
        "kanji chips",
        RowKind::Toggle,
        "space to toggle",
    ),
    row(RowId::Palette, "palette", RowKind::Cycle, "hjkl to cycle"),
    row(
        RowId::Landing,
        "landing view",
        RowKind::Cycle,
        "hjkl to cycle",
    ),
    row(
        RowId::TitleLanguage,
        "title language",
        RowKind::Cycle,
        "hjkl to cycle",
    ),
    row(
        RowId::Connect,
        "connect",
        RowKind::Action,
        "enter to connect",
    ),
    row(RowId::Sync, "sync", RowKind::Toggle, "space to toggle"),
];

// Section boundaries (DESIGN 5.5): Player 0..5, Catalog 5..6, Interface
// 6..11, AniList Sync 11..13. A row insertion that shifts a boundary must
// break the build, never silently misattribute a row to the wrong header.
const _: () = {
    assert!(ROWS.len() == 13);
    assert!(matches!(ROWS[4].id, RowId::SkipMode)); // last Player
    assert!(matches!(ROWS[5].id, RowId::Provider)); // the lone Catalog row
    assert!(matches!(ROWS[6].id, RowId::CoverArt)); // first Interface
    assert!(matches!(ROWS[10].id, RowId::TitleLanguage)); // last Interface
    assert!(matches!(ROWS[11].id, RowId::Connect)); // first AniList Sync
    assert!(matches!(ROWS[12].id, RowId::Sync)); // last AniList Sync
};

const QUALITY_PRESETS: [&str; 5] = ["worst", "480", "720", "1080", "best"];
const TRANSLATION_PRESETS: [&str; 2] = ["sub", "dub"];
const SKIP_PRESETS: [&str; 4] = ["none", "intro", "outro", "both"];
const RESUME_PRESETS: [u32; 6] = [0, 3, 5, 10, 15, 30];
const PALETTE_PRESETS: [&str; 4] = ["terminal_ghost", "phosphor", "nord", "tokyonight"];
const LANDING_PRESETS: [&str; 3] = ["history", "browse", "last_watched"];
const TITLE_LANGUAGE_PRESETS: [&str; 3] = ["romaji", "english", "native"];

/// Longest value the edit buffer accepts (paths).
const EDIT_MAX: usize = 256;

/// What a Settings keypress means to App. The state reports; App projects
/// (palette/translation) and persists (dirty rides leave/quit, never a key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Fall through to the global key chain.
    Ignored,
    Consumed,
    /// Config mutated; App re-derives its live projections.
    ConfigChanged,
    /// The connect action row fired (inert until ROD-448).
    ConnectRequested,
}

#[derive(Debug, Default)]
pub struct SettingsState {
    /// Cursor over interactive rows only.
    cursor: usize,
    editing: bool,
    edit: String,
    /// Config mutated since entry/last save; leave/quit persists only when
    /// set (DESIGN 5.5, freeze ROD-210).
    pub dirty: bool,
}

impl SettingsState {
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// Handle a key while Settings is active in normal mode. While a field
    /// is under edit every key is swallowed here (Ctrl-C is cut off in
    /// `on_key` before dispatch reaches any view).
    pub fn on_key(&mut self, key: KeyCode, config: &mut Config, providers: &[&str]) -> KeyOutcome {
        if self.editing {
            return self.on_edit_key(key, config);
        }
        let row = &ROWS[self.cursor];
        match key {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.cursor + 1 < ROWS.len() {
                    self.cursor += 1;
                }
                KeyOutcome::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                KeyOutcome::Consumed
            }
            KeyCode::Char('l') | KeyCode::Right if row.kind == RowKind::Cycle => {
                cycle(config, row.id, 1, providers);
                self.dirty = true;
                KeyOutcome::ConfigChanged
            }
            KeyCode::Char('h') | KeyCode::Left if row.kind == RowKind::Cycle => {
                cycle(config, row.id, -1, providers);
                self.dirty = true;
                KeyOutcome::ConfigChanged
            }
            KeyCode::Char(' ') if row.kind == RowKind::Toggle => {
                toggle(config, row.id);
                self.dirty = true;
                KeyOutcome::ConfigChanged
            }
            KeyCode::Enter if row.kind == RowKind::Text => {
                self.edit = config.mpv_path.clone();
                self.editing = true;
                KeyOutcome::Consumed
            }
            KeyCode::Enter if row.kind == RowKind::Action => KeyOutcome::ConnectRequested,
            // Everything else, `q` included, falls through to the global
            // chain; dirty drives the leave/quit write there.
            KeyCode::Char('h' | 'l' | ' ') | KeyCode::Left | KeyCode::Right | KeyCode::Enter => {
                KeyOutcome::Consumed
            }
            _ => KeyOutcome::Ignored,
        }
    }

    /// Edit mode (DESIGN 5.5): append-only buffer, printable ASCII, Esc
    /// discards, Enter commits, an empty commit is a no-op (never a blank
    /// mpv argv0).
    fn on_edit_key(&mut self, key: KeyCode, config: &mut Config) -> KeyOutcome {
        match key {
            KeyCode::Esc => {
                self.editing = false;
                self.edit.clear();
            }
            KeyCode::Enter => {
                self.editing = false;
                let value = std::mem::take(&mut self.edit);
                if !value.is_empty() {
                    config.mpv_path = value;
                    self.dirty = true;
                }
            }
            KeyCode::Backspace => {
                self.edit.pop();
            }
            KeyCode::Char(c) if (' '..='~').contains(&c) && self.edit.len() < EDIT_MAX => {
                self.edit.push(c);
            }
            _ => {}
        }
        KeyOutcome::Consumed
    }
}

/// Step a preset wheel; an unrecognized stored value reads as index 0 so a
/// hand-edited config still cycles sanely.
fn cycle_preset<'a>(presets: &[&'a str], current: &str, dir: i8) -> &'a str {
    let idx = presets.iter().position(|p| *p == current).unwrap_or(0);
    let n = presets.len();
    let next = if dir > 0 { idx + 1 } else { idx + n - 1 } % n;
    presets[next]
}

/// Provider wheel: unset ("") then each registry name in construction order,
/// wrapping back to unset. Unset is a real stop: it follows the registry
/// leader, an explicit pin to the same name does not (DESIGN 5.5).
fn cycle_provider(names: &[&str], current: &str, dir: i8) -> String {
    let n = names.len() + 1;
    let idx = names
        .iter()
        .position(|p| *p == current)
        .map_or(0, |i| i + 1);
    let next = if dir > 0 { idx + 1 } else { idx + n - 1 } % n;
    if next == 0 {
        String::new()
    } else {
        names[next - 1].to_string()
    }
}

fn cycle(config: &mut Config, id: RowId, dir: i8, providers: &[&str]) {
    match id {
        RowId::Quality => {
            config.default_quality =
                cycle_preset(&QUALITY_PRESETS, &config.default_quality, dir).into()
        }
        RowId::Translation => {
            config.translation = cycle_preset(&TRANSLATION_PRESETS, &config.translation, dir).into()
        }
        RowId::SkipMode => {
            config.skip_mode = cycle_preset(&SKIP_PRESETS, &config.skip_mode, dir).into()
        }
        RowId::ResumeOffset => {
            let presets = RESUME_PRESETS;
            let idx = presets
                .iter()
                .position(|p| *p == config.resume_offset_sec)
                .unwrap_or(0);
            let n = presets.len();
            let next = if dir > 0 { idx + 1 } else { idx + n - 1 } % n;
            config.resume_offset_sec = presets[next];
        }
        RowId::Palette => {
            config.palette = cycle_preset(&PALETTE_PRESETS, &config.palette, dir).into()
        }
        RowId::Landing => {
            config.landing = cycle_preset(&LANDING_PRESETS, &config.landing, dir).into()
        }
        RowId::TitleLanguage => {
            config.title_language =
                cycle_preset(&TITLE_LANGUAGE_PRESETS, &config.title_language, dir).into()
        }
        RowId::Provider if !providers.is_empty() => {
            config.preferred_provider = cycle_provider(providers, &config.preferred_provider, dir);
        }
        _ => {}
    }
}

fn toggle(config: &mut Config, id: RowId) {
    match id {
        RowId::CoverArt => config.cover_art = !config.cover_art,
        RowId::KanjiChips => config.kanji_chips = !config.kanji_chips,
        RowId::Sync => config.anilist_sync_enabled = !config.anilist_sync_enabled,
        _ => {}
    }
}

/// Display string for a row's value column (DESIGN 5.5 forms).
fn value(config: &Config, id: RowId, providers: &[&str]) -> String {
    match id {
        RowId::MpvPath => config.mpv_path.clone(),
        RowId::Quality => config.default_quality.clone(),
        RowId::Translation => config.translation.clone(),
        RowId::ResumeOffset => format!("{}s", config.resume_offset_sec),
        RowId::SkipMode => config.skip_mode.clone(),
        // Unset shows the leader tagged so blank is never confused with an
        // explicit pin to the same provider.
        RowId::Provider => {
            if !config.preferred_provider.is_empty() {
                config.preferred_provider.clone()
            } else if let Some(leader) = providers.first() {
                format!("{leader} (default)")
            } else {
                String::new()
            }
        }
        RowId::CoverArt => onoff(config.cover_art),
        RowId::KanjiChips => onoff(config.kanji_chips),
        RowId::Palette => config.palette.clone(),
        RowId::Landing => config.landing.clone(),
        RowId::TitleLanguage => config.title_language.clone(),
        RowId::Sync => onoff(config.anilist_sync_enabled),
        RowId::Connect => String::new(),
    }
}

fn onoff(v: bool) -> String {
    if v { "on" } else { "off" }.to_string()
}

// ── draw (DESIGN 5.5) ───────────────────────────────────────────────────────

/// Runtime facts the rows display; borrowed per draw.
pub struct SettingsEnv<'a> {
    pub config: &'a Config,
    pub providers: &'a [&'a str],
    /// Covers cache dir for the read-only Catalog row, `$HOME` collapsed.
    pub covers_dir: &'a str,
    /// Account row copy (DESIGN 5.5): user name, reconnect prompt, or not connected.
    pub account: &'a str,
}

const LABEL_X: u16 = 4;
const VALUE_X: u16 = 34;

enum Li {
    Header(&'static str),
    Rule,
    /// Interactive row by `ROWS` ordinal.
    Row(usize),
    /// Read-only status row: label + value, dim + italic, never focused.
    Inert(&'static str, String),
    Blank,
}

/// One line-geometry for the whole tab; sections and their row ranges are
/// pinned by the compile-time assertion above.
fn layout(env: &SettingsEnv) -> Vec<Li> {
    let mut lines = Vec::new();
    let section = |lines: &mut Vec<Li>, title| {
        lines.push(Li::Header(title));
        lines.push(Li::Rule);
    };
    section(&mut lines, "Player");
    (0..5).for_each(|i| lines.push(Li::Row(i)));
    lines.push(Li::Blank);
    section(&mut lines, "Catalog");
    // Status above controls (DESIGN 5.5): stored rows refresh via the
    // background task; a manual refresh is the `:sync` command.
    lines.push(Li::Inert("metadata refresh", "automatic".into()));
    lines.push(Li::Inert("cover art cache", env.covers_dir.to_string()));
    lines.push(Li::Row(5));
    lines.push(Li::Blank);
    section(&mut lines, "Interface");
    (6..11).for_each(|i| lines.push(Li::Row(i)));
    lines.push(Li::Blank);
    section(&mut lines, "AniList Sync");
    lines.push(Li::Inert("account", env.account.to_string()));
    (11..13).for_each(|i| lines.push(Li::Row(i)));
    lines
}

pub fn draw(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: &Palette,
    state: &SettingsState,
    env: &SettingsEnv,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = layout(env);
    let cursor_line = lines
        .iter()
        .position(|l| matches!(l, Li::Row(i) if *i == state.cursor))
        .unwrap_or(0);
    // Stateless scroll: keep the focused row on screen, top-anchored
    // otherwise.
    let visible = area.height as usize;
    let scroll = (cursor_line + 1).saturating_sub(visible);
    for (y, li) in lines.iter().skip(scroll).take(visible).enumerate() {
        let line = Rect::new(area.x, area.y + y as u16, area.width, 1);
        match li {
            Li::Blank => {}
            Li::Header(title) => draw_at(
                frame,
                line,
                LABEL_X.saturating_sub(2),
                title,
                Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
            ),
            Li::Rule => {
                let w = area.width.saturating_sub(4) as usize;
                draw_at(
                    frame,
                    line,
                    LABEL_X.saturating_sub(2),
                    &"─".repeat(w),
                    Style::new().fg(palette.chrome),
                );
            }
            Li::Inert(label, value) => {
                let style = Style::new().fg(palette.fg3).add_modifier(Modifier::ITALIC);
                draw_at(frame, line, LABEL_X, label, style);
                draw_at(frame, line, VALUE_X, value, style);
            }
            Li::Row(ix) => draw_row(frame, line, palette, state, env, *ix),
        }
    }
}

fn draw_row(
    frame: &mut Frame<'_>,
    line: Rect,
    palette: &Palette,
    state: &SettingsState,
    env: &SettingsEnv,
    ix: usize,
) {
    let row = &ROWS[ix];
    let focused = state.cursor == ix;
    let editing = focused && state.editing;
    if focused {
        let fill = if editing {
            palette.elevated
        } else {
            palette.surface
        };
        frame.render_widget(Block::new().style(Style::new().bg(fill)), line);
    }
    let bg = |s: Style| -> Style {
        if editing {
            s.bg(palette.elevated)
        } else if focused {
            s.bg(palette.surface)
        } else {
            s
        }
    };
    if focused {
        let marker_color = if editing { palette.hot } else { palette.focus };
        draw_at(frame, line, 2, "▸", bg(Style::new().fg(marker_color)));
    }
    let label_style = if focused {
        bg(Style::new().fg(palette.focus).add_modifier(Modifier::BOLD))
    } else {
        Style::new().fg(palette.fg2)
    };
    draw_at(frame, line, LABEL_X, row.label, label_style);

    if editing {
        // The append-only buffer with a trailing inverted cursor block.
        let text = format!("{}█", state.edit);
        draw_at(frame, line, VALUE_X, &text, bg(Style::new().fg(palette.fg)));
    } else if row.kind == RowKind::Toggle {
        let on = value(env.config, row.id, env.providers) == "on";
        let (text, style) = if on {
            ("[████ on ████]".to_string(), Style::new().fg(palette.focus))
        } else {
            ("[████ off ████]".to_string(), Style::new().fg(palette.fg3))
        };
        draw_at(frame, line, VALUE_X, &text, bg(style));
    } else {
        let text = value(env.config, row.id, env.providers);
        let style = if focused {
            Style::new().fg(palette.fg)
        } else {
            Style::new().fg(palette.fg2)
        };
        draw_at(frame, line, VALUE_X, &text, bg(style));
    }

    // Hint column, right-anchored (ASCII-only hints, byte len = width).
    if !editing {
        let x = line
            .width
            .saturating_sub(2)
            .saturating_sub(row.hint.len() as u16);
        if x > VALUE_X {
            draw_at(frame, line, x, row.hint, bg(Style::new().fg(palette.fg3)));
        }
    }
}

fn draw_at(frame: &mut Frame<'_>, line: Rect, x: u16, text: &str, style: Style) {
    if x >= line.width {
        return;
    }
    let cell = Rect::new(line.x + x, line.y, line.width - x, 1);
    frame.render_widget(Paragraph::new(Span::styled(text.to_string(), style)), cell);
}

/// `$HOME`-collapse for the cache-path status row (DESIGN 5.5).
pub fn tilde_path(path: &std::path::Path) -> String {
    let display = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && display.starts_with(&home) => {
            format!("~{}", &display[home.len()..])
        }
        _ => display,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDERS: [&str; 3] = ["megaplay", "senshi", "allanime"];

    fn state() -> (SettingsState, Config) {
        (SettingsState::default(), Config::default())
    }

    fn press(s: &mut SettingsState, c: &mut Config, keys: &[KeyCode]) -> KeyOutcome {
        let mut last = KeyOutcome::Ignored;
        for k in keys {
            last = s.on_key(*k, c, &PROVIDERS);
        }
        last
    }

    #[test]
    fn navigation_clamps_over_interactive_rows_only() {
        let (mut s, mut c) = state();
        press(&mut s, &mut c, &[KeyCode::Char('k')]);
        assert_eq!(s.cursor, 0, "clamped at the top");
        for _ in 0..20 {
            press(&mut s, &mut c, &[KeyCode::Char('j')]);
        }
        assert_eq!(s.cursor, ROWS.len() - 1, "clamped at the last row");
    }

    #[test]
    fn cycle_rows_step_their_wheels_both_ways() {
        let (mut s, mut c) = state();
        press(&mut s, &mut c, &[KeyCode::Char('j')]); // default quality
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Char('h')]),
            KeyOutcome::ConfigChanged
        );
        assert_eq!(c.default_quality, "1080", "best wraps back to 1080");
        press(&mut s, &mut c, &[KeyCode::Char('l')]);
        assert_eq!(c.default_quality, "best");

        press(&mut s, &mut c, &[KeyCode::Char('j')]); // translation
        press(&mut s, &mut c, &[KeyCode::Char('l')]);
        assert_eq!(c.translation, "dub");

        press(&mut s, &mut c, &[KeyCode::Char('j')]); // resume offset
        press(&mut s, &mut c, &[KeyCode::Char('l')]);
        assert_eq!(c.resume_offset_sec, 10, "5 steps to 10");
        press(&mut s, &mut c, &[KeyCode::Char('h'), KeyCode::Char('h')]);
        assert_eq!(c.resume_offset_sec, 3);
        assert!(s.dirty);
    }

    #[test]
    fn unrecognized_stored_value_cycles_from_index_zero() {
        let (mut s, mut c) = state();
        c.default_quality = "hand-edited".into();
        press(&mut s, &mut c, &[KeyCode::Char('j'), KeyCode::Char('l')]);
        assert_eq!(c.default_quality, "480", "unknown reads as index 0");
    }

    #[test]
    fn provider_wheel_walks_unset_then_names_then_unset() {
        let (mut s, mut c) = state();
        for _ in 0..5 {
            press(&mut s, &mut c, &[KeyCode::Char('j')]);
        }
        assert_eq!(ROWS[s.cursor].id, RowId::Provider);
        let mut seen = Vec::new();
        for _ in 0..4 {
            press(&mut s, &mut c, &[KeyCode::Char('l')]);
            seen.push(c.preferred_provider.clone());
        }
        assert_eq!(seen, ["megaplay", "senshi", "allanime", ""]);
        // And backwards off unset lands on the tail.
        press(&mut s, &mut c, &[KeyCode::Char('h')]);
        assert_eq!(c.preferred_provider, "allanime");
    }

    #[test]
    fn toggles_flip_with_space_and_report_change() {
        let (mut s, mut c) = state();
        for _ in 0..6 {
            press(&mut s, &mut c, &[KeyCode::Char('j')]);
        }
        assert_eq!(ROWS[s.cursor].id, RowId::CoverArt);
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Char(' ')]),
            KeyOutcome::ConfigChanged
        );
        assert!(!c.cover_art);
        // Space on a non-toggle row is consumed, never a fallthrough.
        press(&mut s, &mut c, &[KeyCode::Char('k')]);
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Char(' ')]),
            KeyOutcome::Consumed
        );
    }

    #[test]
    fn edit_mode_prefills_appends_commits_and_cancels() {
        let (mut s, mut c) = state();
        press(&mut s, &mut c, &[KeyCode::Enter]);
        assert!(s.editing());
        // Prefilled with the current value; append + backspace.
        press(
            &mut s,
            &mut c,
            &[KeyCode::Char('-'), KeyCode::Char('x'), KeyCode::Backspace],
        );
        press(&mut s, &mut c, &[KeyCode::Enter]);
        assert!(!s.editing());
        assert_eq!(c.mpv_path, "mpv-");
        assert!(s.dirty);

        // Esc discards.
        press(
            &mut s,
            &mut c,
            &[KeyCode::Enter, KeyCode::Char('z'), KeyCode::Esc],
        );
        assert_eq!(c.mpv_path, "mpv-");
        assert!(!s.editing());
    }

    #[test]
    fn empty_commit_never_blanks_the_mpv_path() {
        let (mut s, mut c) = state();
        press(&mut s, &mut c, &[KeyCode::Enter]);
        for _ in 0..10 {
            press(&mut s, &mut c, &[KeyCode::Backspace]);
        }
        press(&mut s, &mut c, &[KeyCode::Enter]);
        assert_eq!(c.mpv_path, "mpv", "empty commit is a no-op");
    }

    #[test]
    fn edit_mode_swallows_globals_and_rejects_control_chars() {
        let (mut s, mut c) = state();
        press(&mut s, &mut c, &[KeyCode::Enter]);
        // q/B/F-key letters are text while editing, never navigation.
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Char('q')]),
            KeyOutcome::Consumed
        );
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Char('\u{1b}')]),
            KeyOutcome::Consumed
        );
        press(&mut s, &mut c, &[KeyCode::Enter]);
        assert_eq!(c.mpv_path, "mpvq", "printable kept, control dropped");
    }

    #[test]
    fn q_and_view_letters_fall_through_when_not_editing() {
        let (mut s, mut c) = state();
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Char('q')]),
            KeyOutcome::Ignored
        );
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Char('B')]),
            KeyOutcome::Ignored
        );
        assert_eq!(press(&mut s, &mut c, &[KeyCode::Esc]), KeyOutcome::Ignored);
    }

    #[test]
    fn connect_row_reports_the_action() {
        let (mut s, mut c) = state();
        for _ in 0..11 {
            press(&mut s, &mut c, &[KeyCode::Char('j')]);
        }
        assert_eq!(ROWS[s.cursor].id, RowId::Connect);
        assert_eq!(
            press(&mut s, &mut c, &[KeyCode::Enter]),
            KeyOutcome::ConnectRequested
        );
        assert!(!s.dirty, "the action row never dirties the tab");
    }

    #[test]
    fn value_forms_match_the_mock() {
        let c = Config::default();
        assert_eq!(value(&c, RowId::ResumeOffset, &PROVIDERS), "5s");
        assert_eq!(
            value(&c, RowId::Provider, &PROVIDERS),
            "megaplay (default)",
            "unset shows the leader tagged"
        );
        let pinned = Config {
            preferred_provider: "senshi".into(),
            ..Config::default()
        };
        assert_eq!(value(&pinned, RowId::Provider, &PROVIDERS), "senshi");
        assert_eq!(value(&c, RowId::Connect, &PROVIDERS), "");
    }

    #[test]
    fn tilde_collapses_only_the_home_prefix() {
        let home = std::env::var("HOME").unwrap();
        let inside = std::path::PathBuf::from(&home).join(".cache/sabigoku/covers");
        assert_eq!(tilde_path(&inside), "~/.cache/sabigoku/covers");
        assert_eq!(
            tilde_path(std::path::Path::new("/srv/covers")),
            "/srv/covers"
        );
    }
}
