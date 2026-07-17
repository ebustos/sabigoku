//! App state, input, render, event loop, workers (04, DESIGN). `tui::workers`
//! is the ONE glue point allowed to import source, store, player, resolver, and
//! anilist together (01 §3). Render stays pure: no store writes from draw
//! (01 §5).
//!
//! The `App` here is the ROD-433 runtime shell; the real views replace its
//! body from ROD-439 without touching the loop.

pub mod clock;
pub mod event;
pub mod workers;

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use clock::{AsyncStart, TickClock};
use event::{Event, EventTx};
use workers::{CancelFlag, Drain, Generation};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DEMO_STEP: Duration = Duration::from_millis(80);

#[derive(Debug, PartialEq, Eq)]
enum Demo {
    Idle,
    Running { token: u64, percent: u8, started: AsyncStartEq },
    Done,
    Cancelled,
}

/// AsyncStart with identity-free equality so Demo stays comparable in tests.
#[derive(Debug, Clone, Copy)]
struct AsyncStartEq(AsyncStart);
impl PartialEq for AsyncStartEq {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for AsyncStartEq {}

pub struct App {
    quit: bool,
    dirty: bool,
    ticks: u64,
    demo: Demo,
    demo_gen: Generation,
    /// Replaced per spawn: a superseded worker keeps its own flag, so
    /// cancelling the current one never kills its successor.
    demo_cancel: CancelFlag,
    demo_drain: Drain,
    dropped_stale: u64,
}

impl App {
    pub fn new() -> App {
        App {
            quit: false,
            dirty: true,
            ticks: 0,
            demo: Demo::Idle,
            demo_gen: Generation::default(),
            demo_cancel: CancelFlag::default(),
            demo_drain: Drain::default(),
            dropped_stale: 0,
        }
    }

    /// Mutates; draw is pure (04 §1).
    fn tick(&mut self, event: Event, now: Instant, tx: &EventTx) {
        match event {
            Event::Key(key) => self.on_key(key, now, tx),
            Event::Resize(..) => self.dirty = true,
            Event::FocusGained | Event::FocusLost => {}
            Event::Tick => {
                self.ticks += 1;
                self.dirty = true;
            }
            Event::DemoProgress { token, percent } => {
                if !self.demo_gen.is_current(token) {
                    self.dropped_stale += 1;
                    self.dirty = true;
                    return;
                }
                if let Demo::Running { percent: p, .. } = &mut self.demo {
                    *p = percent;
                    self.dirty = true;
                }
            }
            Event::DemoDone { token } => {
                if !self.demo_gen.is_current(token) {
                    self.dropped_stale += 1;
                    self.dirty = true;
                    return;
                }
                self.demo = Demo::Done;
                self.dirty = true;
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent, now: Instant, tx: &EventTx) {
        let ctrl_c = key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            _ if ctrl_c => self.quit = true,
            KeyCode::Char('w') => self.start_demo(now, tx),
            KeyCode::Char('c') => self.cancel_demo(),
            _ => {}
        }
    }

    /// Supersede then spawn: bump first so anything the old worker already
    /// posted is stale before the new state exists (04 §6). Never join it.
    fn start_demo(&mut self, now: Instant, tx: &EventTx) {
        let token = self.demo_gen.bump();
        self.demo_cancel = CancelFlag::default();
        let cancel = self.demo_cancel.clone();
        let tx = tx.clone();
        self.demo_drain.spawn("demo", move || {
            for percent in (0..=100u8).step_by(4) {
                if cancel.is_cancelled() {
                    return;
                }
                tx.post(Event::DemoProgress { token, percent });
                std::thread::sleep(DEMO_STEP);
            }
            tx.post(Event::DemoDone { token });
        });
        self.demo = Demo::Running { token, percent: 0, started: AsyncStartEq(AsyncStart::new(now)) };
        self.dirty = true;
    }

    /// Cancel is flag + bump: the flag stops the worker, the bump invalidates
    /// results it posted before noticing (04 §6; a flag alone leaves a race).
    fn cancel_demo(&mut self) {
        if !matches!(self.demo, Demo::Running { .. }) {
            return;
        }
        self.demo_cancel.cancel();
        self.demo_gen.bump();
        self.demo = Demo::Cancelled;
        self.dirty = true;
    }

    /// Pure: state to frame, no I/O, no mutation (04 §1, 01 §5).
    fn draw(&self, frame: &mut Frame<'_>, now: Instant) {
        let demo_line = match &self.demo {
            Demo::Idle => Line::from("demo: idle".dark_gray()),
            Demo::Running { percent, started, .. } => {
                let spin = started.0.frame(now, SPINNER.len());
                let style = if started.0.is_slow(now) {
                    Style::new().fg(Color::Yellow)
                } else {
                    Style::new().fg(Color::Cyan)
                };
                Line::from(vec![
                    Span::styled(SPINNER[spin], style),
                    Span::raw(format!(" demo: {percent}%")),
                ])
            }
            Demo::Done => Line::from("demo: done".green()),
            Demo::Cancelled => Line::from("demo: cancelled".red()),
        };
        let text = vec![
            Line::from("sabigoku runtime shell (ROD-433)".bold()),
            Line::from(format!("ticks: {}", self.ticks)),
            demo_line,
            Line::from(format!("stale results dropped: {}", self.dropped_stale)),
            Line::from(""),
            Line::from("w spawn worker · c cancel · q quit".dark_gray()),
        ];
        frame.render_widget(Paragraph::new(text), frame.area());
    }
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

/// Init, loop, clean-drain teardown. The drain on quit is a deliberate
/// deviation from zigoku's production `_exit(0)` (04 §3, Rod 2026-07-17);
/// bounded timeouts below are what 04 §11 demands of that choice.
/// `ratatui::init` installs the panic hook that restores the terminal.
pub fn run() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let (tx, rx) = event::channel();
    let shutdown = CancelFlag::default();
    let input_drain = Drain::default();
    event::spawn_input_thread(&input_drain, tx.clone(), shutdown.clone());

    let mut app = App::new();
    let mut clock = TickClock::new(Instant::now());
    let result = (|| {
        while !app.quit {
            let now = Instant::now();
            if let Ok(ev) = rx.recv_timeout(clock.timeout(now)) {
                app.tick(ev, Instant::now(), &tx);
            }
            while let Ok(ev) = rx.try_recv() {
                app.tick(ev, Instant::now(), &tx);
            }
            let now = Instant::now();
            if clock.should_tick(now) {
                app.tick(Event::Tick, now, &tx);
            }
            if app.dirty {
                terminal.draw(|frame| app.draw(frame, Instant::now()))?;
                app.dirty = false;
            }
        }
        Ok(())
    })();

    shutdown.cancel();
    app.demo_cancel.cancel();
    input_drain.drain(Duration::from_millis(500));
    app.demo_drain.drain(Duration::from_secs(1));
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn harness() -> (App, EventTx, event::EventRx, Instant) {
        let (tx, rx) = event::channel();
        (App::new(), tx, rx, Instant::now())
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let (mut app, tx, _rx, now) = harness();
        app.tick(key(KeyCode::Char('q')), now, &tx);
        assert!(app.quit);
        let (mut app, tx, _rx, now) = harness();
        app.tick(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            now,
            &tx,
        );
        assert!(app.quit);
    }

    #[test]
    fn stale_demo_result_dropped() {
        let (mut app, tx, _rx, now) = harness();
        let stale = app.demo_gen.bump();
        let current = app.demo_gen.bump();
        app.demo = Demo::Running { token: current, percent: 0, started: AsyncStartEq(AsyncStart::new(now)) };
        app.tick(Event::DemoProgress { token: stale, percent: 96 }, now, &tx);
        assert_eq!(app.dropped_stale, 1);
        assert_eq!(
            app.demo,
            Demo::Running { token: current, percent: 0, started: AsyncStartEq(AsyncStart::new(now)) }
        );
        app.tick(Event::DemoDone { token: stale }, now, &tx);
        assert_eq!(app.dropped_stale, 2);
        assert!(matches!(app.demo, Demo::Running { .. }));
    }

    #[test]
    fn current_demo_result_applies() {
        let (mut app, tx, _rx, now) = harness();
        let token = app.demo_gen.bump();
        app.demo = Demo::Running { token, percent: 0, started: AsyncStartEq(AsyncStart::new(now)) };
        app.tick(Event::DemoProgress { token, percent: 40 }, now, &tx);
        assert!(matches!(app.demo, Demo::Running { percent: 40, .. }));
        app.tick(Event::DemoDone { token }, now, &tx);
        assert_eq!(app.demo, Demo::Done);
    }

    #[test]
    fn cancel_flags_and_invalidates() {
        let (mut app, tx, _rx, now) = harness();
        let token = app.demo_gen.bump();
        app.demo = Demo::Running { token, percent: 8, started: AsyncStartEq(AsyncStart::new(now)) };
        let flag = app.demo_cancel.clone();
        app.tick(key(KeyCode::Char('c')), now, &tx);
        assert!(flag.is_cancelled());
        assert!(!app.demo_gen.is_current(token));
        assert_eq!(app.demo, Demo::Cancelled);
        app.tick(Event::DemoProgress { token, percent: 12 }, now, &tx);
        assert_eq!(app.demo, Demo::Cancelled);
    }

    #[test]
    fn tick_counts_and_dirties() {
        let (mut app, tx, _rx, now) = harness();
        app.dirty = false;
        app.tick(Event::Tick, now, &tx);
        assert_eq!(app.ticks, 1);
        assert!(app.dirty);
    }

    #[test]
    fn draw_is_total_even_tiny() {
        let (app, _tx, _rx, now) = harness();
        for (w, h) in [(80, 24), (16, 4), (2, 1)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|frame| app.draw(frame, now)).unwrap();
        }
    }
}
