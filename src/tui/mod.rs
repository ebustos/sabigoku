//! TUI runtime: init, event loop, teardown (04). `tui::workers` is the ONE
//! glue point allowed to import source, store, player, resolver, and anilist
//! together (01 §3). Render stays pure of store writes and app-state mutation
//! (01 §5); the one draw-side mutation is ratatui-image protocol resize
//! bookkeeping, which is render-owned by that library's design.

pub mod app;
pub mod chrome;
pub mod clock;
pub mod covers;
pub mod event;
pub mod layout;
pub mod render;
pub mod theme;
pub mod toast;
pub mod view;
pub mod workers;

use std::time::{Duration, Instant};

use ratatui_image::picker::Picker;

use crate::config::Config;
use crate::paths::Paths;
use app::App;
use clock::TickClock;
use event::Event;
use workers::{CancelFlag, Drain};

/// Cap on events per loop pass so a bursting producer can never starve the
/// tick clock, the draw, or the quit check (unbounded queue, 04 §8).
const MAX_EVENTS_PER_PASS: usize = 256;

/// Init, loop, clean-drain teardown (a ratified deviation from zigoku's
/// `_exit(0)`; the bounded timeouts are what 04 §3/§11 demand of it).
pub fn run(paths: &Paths, config: &Config) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    scope_panic_hook_to_main_thread();
    // Protocol query must run after entering the alternate screen and BEFORE
    // the input thread exists: it reads stdio itself (04 §3 query leftovers).
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
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

    let mut app = App::new(config, paths.covers_dir(), picker, &tx);
    if let Ok(size) = terminal.size() {
        app.tick(Event::Resize(size.width, size.height), Instant::now(), &tx);
    }

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
                // The carry-forward law: forward this draw's resize requests
                // into the encode worker, tagged per image.
                app.pool.drain_requests();
            }
        }
        Ok(())
    })();

    shutdown.cancel();
    input_drain.drain(Duration::from_millis(500));
    app.cover_drain.drain(Duration::from_secs(1));
    app.discover_cover_drain.drain(Duration::from_secs(1));
    // The encode worker exits when the pool (inside App) drops its queue.
    let encode_drain = app.encode_drain.clone();
    drop(app);
    encode_drain.drain(Duration::from_secs(1));
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
