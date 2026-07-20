//! Top bar and bottom bar (DESIGN 3.4, 3.5, 7.3, 7.5). Both are read-only
//! chrome driven by small view-model inputs the App assembles; chrome never
//! reaches into app or view state.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::render::display_width;
use super::theme::Palette;

/// Top-bar tab identity. The detail zoom is not a tab destination; the App
/// maps it to its `detail_origin` before building this (DESIGN 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Browse,
    History,
    Discover,
    Settings,
}

impl Tab {
    fn full(self) -> (&'static str, &'static str) {
        match self {
            Tab::Browse => ("[B]", "rowse"),
            Tab::History => ("[H]", "istory"),
            Tab::Discover => ("[D]", "iscover"),
            Tab::Settings => ("[S]", "ettings"),
        }
    }

    fn brief(self) -> &'static str {
        match self {
            Tab::Browse => "[B]",
            Tab::History => "[H]",
            Tab::Discover => "[D]",
            Tab::Settings => "[S]",
        }
    }
}

const TABS: [Tab; 4] = [Tab::Browse, Tab::History, Tab::Discover, Tab::Settings];

/// `  ░  ` between the wordmark and the tabs.
const HAIRLINE: &str = "  ░  ";
/// ` · ` between tabs.
const TAB_SEP: &str = " · ";

/// Assembled display width of a tab run (`full` labels or `brief`), separators
/// included. Tier selection measures against the real content so it survives a
/// wordmark or label change instead of trusting a hand-tuned breakpoint.
fn tabs_width(full: bool) -> usize {
    let sep = display_width(TAB_SEP) * (TABS.len() - 1);
    let labels: usize = TABS
        .iter()
        .map(|t| {
            if full {
                let (k, l) = t.full();
                display_width(k) + display_width(l)
            } else {
                display_width(t.brief())
            }
        })
        .sum();
    sep + labels
}

#[derive(Debug)]
pub struct TopBar {
    pub tab: Tab,
    /// `冬 2026`-style chip; None renders nothing (absent, not empty).
    pub season_chip: Option<String>,
    /// Pane-focus dot: focus color when lit, fg3 when dim (DESIGN 7.3).
    pub dot_lit: bool,
}

pub fn draw_top_bar(frame: &mut Frame<'_>, area: Rect, palette: &Palette, top: &TopBar) {
    if area.height == 0 {
        return;
    }
    let mut spans: Vec<Span<'_>> = vec![
        Span::raw("  "),
        Span::styled(
            "錆獄 sabigoku",
            Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
        ),
    ];
    // The ░ hairline separates the wordmark from the tabs; the narrowest tier
    // drops it so the single active label clears the fixed right-edge dot.
    let hairline = |spans: &mut Vec<Span<'_>>| {
        spans.push(Span::raw("  "));
        spans.push(Span::styled("░", Style::new().fg(palette.chrome)));
        spans.push(Span::raw("  "));
    };
    let active_key = Style::new().fg(palette.focus);
    let active_label = Style::new().fg(palette.focus).add_modifier(Modifier::BOLD);
    let sep = Style::new().fg(palette.fg3);
    // Content must end before the fixed right-edge dot (col width-2), so measure
    // each form against the columns ahead of it. Widest that fits wins.
    let avail = area.width.saturating_sub(2) as usize;
    let base_w = 2 + display_width("錆獄 sabigoku") + display_width(HAIRLINE);
    let full_w = base_w + tabs_width(true);
    let brief_w = base_w + tabs_width(false);
    if full_w <= avail {
        hairline(&mut spans);
        for (i, tab) in TABS.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", sep));
            }
            let (key, label) = tab.full();
            let inactive = Style::new().fg(palette.fg2);
            let (ks, ls) = if *tab == top.tab {
                (active_key, active_label)
            } else {
                (inactive, inactive)
            };
            spans.push(Span::styled(key, ks));
            spans.push(Span::styled(label, ls));
        }
        if let Some(chip) = &top.season_chip
            && full_w + 2 + display_width(chip) <= avail
        {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(chip.clone(), Style::new().fg(palette.fg2)));
        }
    } else if brief_w <= avail {
        hairline(&mut spans);
        for (i, tab) in TABS.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", sep));
            }
            let style = if *tab == top.tab {
                Style::new().fg(palette.focus).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(palette.fg3)
            };
            spans.push(Span::styled(tab.brief(), style));
        }
    } else {
        spans.push(Span::raw("  "));
        let (key, label) = top.tab.full();
        spans.push(Span::styled(key, active_key));
        spans.push(Span::styled(label, active_label));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    // The `·` always survives, fixed at the right edge (DESIGN 7.3).
    if area.width >= 2 {
        let dot_style = if top.dot_lit {
            Style::new().fg(palette.focus)
        } else {
            Style::new().fg(palette.fg3)
        };
        let dot = Rect::new(area.x + area.width - 2, area.y, 1, 1);
        frame.render_widget(Paragraph::new(Span::styled("·", dot_style)), dot);
    }
}

/// Help-line segment: keybind chars render fg2 + bold, words fg3 (DESIGN 7.5).
#[derive(Debug, Clone, Copy)]
enum Seg {
    K(&'static str),
    T(&'static str),
}

use Seg::{K, T};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpLine {
    BrowseList,
    BrowseDetail,
    HistoryList,
    HistoryDetail,
    HistoryEmpty,
    Zoom,
    Discover,
    Settings,
    SettingsEdit,
}

/// DESIGN 7.5 verbatim; the view keys live in the top-bar strip, never here.
fn help_segments(line: HelpLine) -> &'static [Seg] {
    match line {
        HelpLine::BrowseList => &[
            K("hjkl"),
            T(" · "),
            K("/"),
            T(" find anime · "),
            K("P"),
            T(" save · "),
            K("q"),
            T(" quit"),
        ],
        HelpLine::BrowseDetail | HelpLine::HistoryDetail => &[
            K("hjkl"),
            T(" scroll · "),
            K("h"),
            T(" back · "),
            K("enter"),
            T(" play · "),
            K("v"),
            T(" provider · "),
            K("space"),
            T(" zoom · "),
            K("q"),
            T(" quit"),
        ],
        HelpLine::HistoryList => &[
            K("jk"),
            T(" move · "),
            K("/"),
            T(" filter · "),
            K("l"),
            T("/"),
            K("enter"),
            T(" detail · "),
            K("p"),
            T("/"),
            K("x"),
            T("/"),
            K("c"),
            T("/"),
            K("w"),
            T("/"),
            K("P"),
            T(" status · "),
            K("X"),
            T(" delete · "),
            K("r"),
            T("/"),
            K("u"),
            T(" reset/undo · "),
            K("q"),
            T(" quit"),
        ],
        HelpLine::HistoryEmpty => &[
            K("D"),
            T(" discover · "),
            K("B"),
            T(" browse · "),
            K("q"),
            T(" quit"),
        ],
        HelpLine::Zoom => &[
            K("hjkl"),
            T(" scroll · "),
            K("enter"),
            T(" play · "),
            K("v"),
            T(" provider · "),
            K("space"),
            T("/"),
            K("esc"),
            T(" back"),
        ],
        HelpLine::Discover => &[
            K("hjkl"),
            T(" move · "),
            K("enter"),
            T(" open · "),
            K("P"),
            T(" save · "),
            K("["),
            T(" "),
            K("]"),
            T(" axis · "),
            K("/"),
            T(" search · "),
            K("q"),
            T(" quit"),
        ],
        HelpLine::Settings => &[
            K("hjkl"),
            T(" navigate · "),
            K("space"),
            T(" toggle · "),
            K("enter"),
            T(" edit · "),
            K("q"),
            T(" save+quit"),
        ],
        HelpLine::SettingsEdit => &[
            T("type value · "),
            K("enter"),
            T(" confirm · "),
            K("esc"),
            T(" cancel"),
        ],
    }
}

#[derive(Debug)]
pub enum BottomBar<'a> {
    Help(HelpLine),
    Search {
        query: &'a str,
        /// `catalogue` (Browse, network) vs `history` (local filter);
        /// the scope tag is what makes network-vs-local read at a glance
        /// (DESIGN 8.4).
        scope: &'static str,
        count: usize,
    },
    Command {
        input: &'a str,
    },
    /// 800ms unknown-command flash (DESIGN 3.5).
    CommandError,
    /// Armed hard-delete prompt (DESIGN 4.2, 6.5); `title` is the show under
    /// the axe.
    Confirm {
        title: &'a str,
    },
}

pub fn draw_bottom_bar(frame: &mut Frame<'_>, area: Rect, palette: &Palette, bar: &BottomBar<'_>) {
    if area.height == 0 {
        return;
    }
    match bar {
        BottomBar::Help(line) => {
            let mut spans: Vec<Span<'_>> = vec![
                Span::raw("  "),
                Span::styled(
                    "▌",
                    Style::new()
                        .fg(palette.hot)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
                Span::raw(" "),
            ];
            for seg in help_segments(*line) {
                spans.push(match seg {
                    K(s) => Span::styled(
                        *s,
                        Style::new().fg(palette.fg2).add_modifier(Modifier::BOLD),
                    ),
                    T(s) => Span::styled(*s, Style::new().fg(palette.fg3)),
                });
            }
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
        }
        BottomBar::Search {
            query,
            scope,
            count,
        } => {
            let spans = vec![
                Span::raw("  "),
                Span::styled(
                    "/",
                    Style::new().fg(palette.focus).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    query.to_string(),
                    Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled("_", Style::new().fg(palette.focus)),
            ];
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
            let tag = format!("[{scope} · {count}]");
            let w = display_width(&tag) as u16;
            if area.width > w + 2 {
                let rect = Rect::new(area.x + area.width - w - 2, area.y, w, 1);
                frame.render_widget(
                    Paragraph::new(Span::styled(tag, Style::new().fg(palette.fg2))),
                    rect,
                );
            }
        }
        BottomBar::Command { input } => {
            let spans = vec![
                Span::raw("  "),
                Span::styled(
                    ":",
                    Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    input.to_string(),
                    Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled("_", Style::new().fg(palette.focus)),
            ];
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
        }
        BottomBar::CommandError => {
            let spans = vec![
                Span::raw("  "),
                Span::styled(
                    "[!] unknown command",
                    Style::new().fg(palette.hot).add_modifier(Modifier::BOLD),
                ),
            ];
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
        }
        BottomBar::Confirm { title } => {
            // DESIGN 4.2: the title truncates against the fixed tail so the
            // y/esc hints never scroll off.
            let tail_cols = 48usize;
            let budget = (area.width as usize).saturating_sub(tail_cols).max(8);
            let muted = Style::new().fg(palette.fg2);
            let key = Style::new().fg(palette.fg2).add_modifier(Modifier::BOLD);
            let sep = Style::new().fg(palette.fg3);
            let spans = vec![
                Span::styled("[!] ", Style::new().fg(palette.hot)),
                Span::styled("delete \"", muted),
                Span::styled(
                    super::render::truncate_to_width(title, budget).into_owned(),
                    Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled("\"? episode history gone", muted),
                Span::styled(" · ", sep),
                Span::styled("y", key),
                Span::styled(" confirm", muted),
                Span::styled(" · ", sep),
                Span::styled("esc", key),
                Span::styled(" cancel", muted),
            ];
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::render::cour_chip;
    use super::{Tab, TopBar, draw_top_bar};
    use crate::domain::current_cour;

    /// The chip end to end: `domain::current_cour` owns the civil math and the
    /// December roll; this pins the kanji formatting over its boundaries.
    #[test]
    fn cour_chip_matches_anilist_seasons() {
        let cases = [
            (1_768_824_000, "冬 2026"), // 2026-01-19
            (1_774_008_000, "春 2026"), // 2026-03-20
            (1_784_887_200, "夏 2026"), // 2026-07-24
            (1_792_305_600, "秋 2026"), // 2026-10-18
            (1_797_403_600, "冬 2027"), // 2026-12-16 rolls forward
        ];
        for (secs, want) in cases {
            assert_eq!(cour_chip(current_cour(secs)), want, "at {secs}");
        }
    }

    fn top_row(width: u16, tab: Tab, chip: Option<&str>) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
        term.draw(|f| {
            draw_top_bar(
                f,
                f.area(),
                &crate::tui::theme::TERMINAL_GHOST,
                &TopBar {
                    tab,
                    season_chip: chip.map(str::to_string),
                    dot_lit: false,
                },
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..width).map(|x| buf[(x, 0)].symbol()).collect()
    }

    /// The fixed right-edge dot must never land inside a tab label: the exact
    /// regression the kanji wordmark reopened at the old fixed breakpoints.
    /// Sweep across both tier boundaries; the active label stays intact.
    #[test]
    fn top_bar_never_lets_the_dot_clobber_a_tab_label() {
        for w in 29u16..=120 {
            // Settings is the last tab, so its full label is the one the
            // right-edge dot is most likely to clip.
            let row = top_row(w, Tab::Settings, Some("夏 2026"));
            let intact = row.contains("[S]ettings") || (row.contains("[S]") && !row.contains("[S]e"));
            assert!(intact, "width {w}: active tab label corrupted: {row:?}");
        }
    }
}
