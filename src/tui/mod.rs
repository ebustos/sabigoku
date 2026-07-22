//! TUI runtime: init, event loop, teardown (04). `tui::workers` is the ONE
//! glue point allowed to import source, store, player, resolver, and anilist
//! together (01 §3). Render stays pure of store writes and app-state mutation
//! (01 §5); the one draw-side mutation is ratatui-image protocol resize
//! bookkeeping, which is render-owned by that library's design.

pub mod app;
pub mod chrome;
pub mod clock;
pub mod covers;
pub mod episodes;
pub mod event;
pub mod layout;
pub mod playback;
pub mod prewarm;
pub mod render;
pub mod theme;
pub mod toast;
pub mod view;
pub mod workers;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui_image::picker::Picker;

use crate::anilist::AniList;
use crate::config::Config;
use crate::paths::Paths;
use crate::providers::allanime::AllAnime;
use crate::providers::megaplay::MegaPlay;
use crate::providers::senshi::Senshi;
use crate::providers::{CatalogProvider, ProviderRegistry, StreamProvider};
use crate::store::Store;
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
    // Bootstrap order per 01 §2: store and catalog client before the
    // terminal, so a failure prints to a normal screen. Startup does no
    // network work (04 §9.8); building the client is offline.
    let store = Store::open(&paths.db_file()).map_err(std::io::Error::other)?;
    let catalog: Arc<dyn CatalogProvider> =
        Arc::new(AniList::new().map_err(std::io::Error::other)?);
    // Construction order IS the default fallback order (03 §3.1): megaplay,
    // senshi, allanime. Building the clients is offline.
    let registry = Arc::new(ProviderRegistry::new(vec![
        Box::new(MegaPlay::new().map_err(std::io::Error::other)?) as Box<dyn StreamProvider>,
        Box::new(Senshi::new().map_err(std::io::Error::other)?),
        Box::new(AllAnime::new().map_err(std::io::Error::other)?),
    ]));
    let mut terminal = ratatui::init();
    scope_panic_hook_to_main_thread();
    // The protocol query can stall for seconds where the terminal answers
    // late or not at all (tmux); paint a minimal DESIGN 5.6 startup frame
    // first so the wait is never a black screen.
    let _ = terminal.draw(|frame| draw_startup_frame(frame, config));
    // Protocol query must run after entering the alternate screen and BEFORE
    // the input thread exists: it reads stdio itself (04 §3 query leftovers).
    // `image_protocol = "halfblocks"` skips the query outright: no stall, and
    // no late query responses for a keypress to corrupt (a key that lands
    // mid-response is eaten by the escape parser; tmux exposes this).
    let picker = match config.image_protocol.as_str() {
        "halfblocks" => Picker::halfblocks(),
        _ => Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks()),
    };
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

    let mut app = App::new(config, store, catalog, registry, paths, picker, &tx);
    if let Ok(size) = terminal.size() {
        app.tick(Event::Resize(size.width, size.height), Instant::now(), &tx);
    }
    // Launch pull-refresh (04 §3), pull only so first contact never blind-pushes.
    app.bootstrap_sync(&tx);

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
    app.browse.drain(Duration::from_secs(1));
    app.detail.drain(Duration::from_secs(1));
    app.discover.drain(Duration::from_secs(1));
    // Wake a blocked connect worker before its drain (06 §4.4).
    app.shutdown_connect();
    // Quit flush (04 §11): push what's dirty, bounded by the drain below.
    app.spawn_quit_flush(&tx);
    let sync_drain = app.sync_drain.clone();
    // The encode worker exits when the pool (inside App) drops its queue.
    let encode_drain = app.encode_drain.clone();
    drop(app);
    sync_drain.drain(Duration::from_secs(1));
    encode_drain.drain(Duration::from_secs(1));
    ratatui::restore();
    result
}

/// Minimal DESIGN 5.6 startup frame, painted before the protocol query so a
/// slow-answering terminal never shows a black hole. The real views take over
/// on the first loop draw.
fn draw_startup_frame(frame: &mut ratatui::Frame<'_>, config: &Config) {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Block;
    let palette = theme::by_name(&config.palette);
    let area = frame.area();
    frame.render_widget(Block::new().style(Style::new().bg(palette.bg)), area);
    render::draw_centered(
        frame,
        area,
        area.height / 2,
        Line::from(Span::styled(
            "SABIGOKU",
            Style::new().fg(palette.fg).add_modifier(Modifier::BOLD),
        )),
    );
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
