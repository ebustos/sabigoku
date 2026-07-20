//! Per-view state + render modules (DESIGN 7, 9). Each view owns its state
//! struct, key handling, and draw; nothing here reaches into another view's
//! state. Cross-view transitions are the App's job.

pub mod browse;
pub mod detail;
pub mod discover;
pub mod history;
pub mod settings;

/// Five `active_view` values; Detail is the full-screen zoom (DESIGN 7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Browse,
    History,
    Detail,
    Discover,
    Settings,
}

/// Pane focus within a two-pane view. Deliberately a second field, not a
/// collapsed mode enum: view identity and pane focus are independent
/// dimensions (DESIGN 10, ROD-72/180).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Detail,
}

/// Where the zoom was entered from, for the Esc chain and the tab strip
/// (DESIGN 7.1, 7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Browse,
    History,
    Discover,
}

impl Origin {
    pub fn view(self) -> View {
        match self {
            Origin::Browse => View::Browse,
            Origin::History => View::History,
            Origin::Discover => View::Discover,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    Command,
}

/// Ambient render inputs every view draw shares: the resolved title
/// preference (DESIGN 8.2), the kanji-chips toggle, the current cour
/// (badges, chips), wall-clock seconds (airing countdown), and the frame
/// instant (spinner phase, slow escalation).
#[derive(Debug, Clone, Copy)]
pub struct ViewEnv {
    pub pref: crate::domain::TitleLanguage,
    pub kanji: bool,
    pub cour: crate::domain::Cour,
    pub unix_now: i64,
    pub now: std::time::Instant,
    /// Some while a play is resolving/launching: the §4.6 launching cell.
    pub play: Option<crate::tui::playback::PlayGlance>,
}
