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
    Running {
        token: u64,
        percent: u8,
        started: AsyncStartEq,
    },
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
    /// Replaced per spawn; `start_demo` cancels the outgoing flag before the
    /// swap, so no superseded worker outlives its supersession.
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
            // No keys can ever arrive again; quit clean instead of zombieing.
            Event::InputClosed => self.quit = true,
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
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            _ if ctrl_c => self.quit = true,
            KeyCode::Char('w') => self.start_demo(now, tx),
            KeyCode::Char('c') => self.cancel_demo(),
            _ => {}
        }
    }

    /// Supersede: cancel the old worker's own flag, bump so its in-queue posts
    /// go stale, THEN give the new worker a fresh flag (04 §6). Never join.
    /// Without the cancel, superseded workers pile up unkillable to their
    /// natural end and blow the teardown drain budget.
    fn start_demo(&mut self, now: Instant, tx: &EventTx) {
        self.demo_cancel.cancel();
        let token = self.demo_gen.bump();
        self.demo_cancel = CancelFlag::default();
        let cancel = self.demo_cancel.clone();
        let tx = tx.clone();
        let spawned = self.demo_drain.spawn("demo", move || {
            for percent in (0..=100u8).step_by(4) {
                if cancel.is_cancelled() {
                    return;
                }
                tx.post(Event::DemoProgress { token, percent });
                std::thread::sleep(DEMO_STEP);
            }
            tx.post(Event::DemoDone { token });
        });
        self.demo = if spawned {
            Demo::Running {
                token,
                percent: 0,
                started: AsyncStartEq(AsyncStart::new(now)),
            }
        } else {
            Demo::Idle
        };
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
            Demo::Running {
                percent, started, ..
            } => {
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

/// Cap on events per loop pass so a bursting producer can never starve the
/// tick clock, the draw, or the quit check (unbounded queue, 04 §8).
const MAX_EVENTS_PER_PASS: usize = 256;

/// Init, loop, clean-drain teardown (a ratified deviation from zigoku's
/// `_exit(0)`; the bounded timeouts are what 04 §3/§11 demand of it).
pub fn run() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    scope_panic_hook_to_main_thread();
    // External kills route through the same clean-drain quit as `q`; the OS
    // default disposition would strand the terminal raw + alt-screen.
    let sig_quit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        let _ = signal_hook::flag::register(sig, std::sync::Arc::clone(&sig_quit));
    }
    let (tx, rx) = event::channel();
    let shutdown = CancelFlag::default();
    let input_drain = Drain::default();
    if !event::spawn_input_thread(&input_drain, tx.clone(), shutdown.clone()) {
        ratatui::restore();
        return Err(std::io::Error::other("could not spawn the input thread"));
    }

    let mut app = App::new();
    let mut clock = TickClock::new(Instant::now());
    let result = (|| {
        while !app.quit && !sig_quit.load(std::sync::atomic::Ordering::Relaxed) {
            let now = Instant::now();
            if let Ok(ev) = rx.recv_timeout(clock.timeout(now)) {
                app.tick(ev, Instant::now(), &tx);
            }
            let mut budget = MAX_EVENTS_PER_PASS;
            while budget > 0 && !app.quit {
                let Ok(ev) = rx.try_recv() else { break };
                app.tick(ev, Instant::now(), &tx);
                budget -= 1;
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

/// `ratatui::init` installs a process-global restore hook, so an uncaught
/// worker panic would yank the terminal out from under the live UI thread.
/// Scope it: main panics restore; worker panics are contained by the
/// catch_unwind in `Drain::spawn` and counted there. Threads spawned outside
/// a Drain would panic without a trace, so do not spawn any.
fn scope_panic_hook_to_main_thread() {
    let main_thread = std::thread::current().id();
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main_thread {
            restore_hook(info);
        }
    }));
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
        app.demo = Demo::Running {
            token: current,
            percent: 0,
            started: AsyncStartEq(AsyncStart::new(now)),
        };
        app.tick(
            Event::DemoProgress {
                token: stale,
                percent: 96,
            },
            now,
            &tx,
        );
        assert_eq!(app.dropped_stale, 1);
        assert_eq!(
            app.demo,
            Demo::Running {
                token: current,
                percent: 0,
                started: AsyncStartEq(AsyncStart::new(now))
            }
        );
        app.tick(Event::DemoDone { token: stale }, now, &tx);
        assert_eq!(app.dropped_stale, 2);
        assert!(matches!(app.demo, Demo::Running { .. }));
    }

    #[test]
    fn current_demo_result_applies() {
        let (mut app, tx, _rx, now) = harness();
        let token = app.demo_gen.bump();
        app.demo = Demo::Running {
            token,
            percent: 0,
            started: AsyncStartEq(AsyncStart::new(now)),
        };
        app.tick(Event::DemoProgress { token, percent: 40 }, now, &tx);
        assert!(matches!(app.demo, Demo::Running { percent: 40, .. }));
        app.tick(Event::DemoDone { token }, now, &tx);
        assert_eq!(app.demo, Demo::Done);
    }

    #[test]
    fn cancel_flags_and_invalidates() {
        let (mut app, tx, _rx, now) = harness();
        let token = app.demo_gen.bump();
        app.demo = Demo::Running {
            token,
            percent: 8,
            started: AsyncStartEq(AsyncStart::new(now)),
        };
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
    fn real_worker_roundtrip_supersede_and_drain() {
        let (mut app, tx, rx, now) = harness();
        app.tick(key(KeyCode::Char('w')), now, &tx);
        assert_eq!(app.demo_drain.inflight(), 1);
        let Demo::Running {
            token: old_token, ..
        } = app.demo
        else {
            panic!("demo not running after w");
        };
        app.tick(key(KeyCode::Char('w')), Instant::now(), &tx);
        let Demo::Running {
            token: new_token, ..
        } = app.demo
        else {
            panic!("demo not running after second w");
        };
        assert_ne!(old_token, new_token);
        app.tick(
            Event::DemoProgress {
                token: old_token,
                percent: 50,
            },
            Instant::now(),
            &tx,
        );
        assert_eq!(app.dropped_stale, 1);
        assert!(matches!(app.demo, Demo::Running { percent: 0, .. }));
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.demo != Demo::Done {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = rx
                .recv_timeout(left)
                .expect("worker events before deadline");
            app.tick(ev, Instant::now(), &tx);
        }
        assert!(app.demo_drain.drain(Duration::from_secs(5)));
        assert_eq!(app.demo_drain.panics(), 0);
    }

    #[test]
    fn input_closed_quits() {
        let (mut app, tx, _rx, now) = harness();
        app.tick(Event::InputClosed, now, &tx);
        assert!(app.quit);
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
