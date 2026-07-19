//! spike_cover: cover-art pipeline (ratatui-image, Kitty graphics + halfblocks).
//! ROD-417. No zigoku parity spike: zigoku got Kitty graphics natively from
//! libvaxis; sabigoku bets on the `ratatui-image` crate, and this is the bet's
//! validation. DESIGN.md 3.3 / 3.8 spec the sizing rules; 11.2 holds the
//! ratified support matrix (Kitty path: ghostty/kitty/wezterm; everything else
//! degrades to halfblocks; must survive tmux).
//!
//! What it proves, per the ROD-417 acceptance criteria:
//!   1. Kitty-protocol render with Resize::Crop into fixed cell blocks
//!   2. cell-pixel geometry query, and the adaptive cover height derived from it
//!      (fixed floors 7/5 card, caps 28/20 detail, when geometry is unreported)
//!   3. halfblock fallback (auto-detected, or forced with --halfblocks / `p`)
//!   4. tmux survival: no crash, no escape garbage (verify via --probe in tmux)
//!   5. decode+encode on worker threads (ThreadProtocol); render never blocks
//!
//! Run:  cargo run --bin spike_cover                    # interactive
//!       cargo run --bin spike_cover -- --halfblocks    # force fallback path
//!       cargo run --bin spike_cover -- --probe 3       # auto-quit, print report
//! Keys: q quit · arrows/hjkl select · enter/d detail overlay · p cycle protocol
//!       r hard redraw

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui_image::errors::Errors;
use ratatui_image::picker::{Capability, Picker, ProtocolType};
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{FontSize, Resize, StatefulImage};
use serde::Deserialize;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://graphql.anilist.co";
const QUERY: &str = "query{Page(perPage:9){media(type:ANIME,sort:TRENDING_DESC){id title{romaji} coverImage{large}}}}";

#[derive(Deserialize)]
struct Response {
    data: Data,
}
#[derive(Deserialize)]
struct Data {
    #[serde(rename = "Page")]
    page: Page,
}
#[derive(Deserialize)]
struct Page {
    media: Vec<Media>,
}
#[derive(Deserialize)]
struct Media {
    title: Title,
    #[serde(rename = "coverImage")]
    cover: CoverImage,
}
#[derive(Deserialize)]
struct Title {
    romaji: String,
}
#[derive(Deserialize)]
struct CoverImage {
    large: String,
}

enum AppEvent {
    Key(KeyCode),
    /// idx, decoded image, download+decode wall time
    Cover(usize, image::DynamicImage, Duration),
    CoverFailed(usize, String),
    /// card index + resize+encode result from the worker, with the encode wall time
    Encoded(usize, Box<Result<ResizeResponse, Errors>>, Duration),
    Tick,
}

// ThreadProtocol ids count per-instance, so responses off one shared channel
// cannot be trial-routed across cards (colliding ids would install the wrong
// poster). Each card gets a private request channel; requests are drained
// after every draw into the worker queue tagged with the card index.
struct Card {
    title: String,
    proto: Option<ThreadProtocol>,
    /// kept so `p` can rebuild the protocol under a different picker
    img: Option<image::DynamicImage>,
    error: Option<String>,
    req_tx: mpsc::Sender<ResizeRequest>,
    req_rx: mpsc::Receiver<ResizeRequest>,
}

/// DESIGN 3.8 width tiers: >= 80 cols -> 20-col cover in a 22-col slot,
/// below -> 14-col cover in a 16-col slot.
struct Tier {
    large: bool,
    cover_w: u16,
    slot_w: u16,
}

fn tier(term_w: u16) -> Tier {
    if term_w >= 80 {
        Tier {
            large: true,
            cover_w: 20,
            slot_w: 22,
        }
    } else {
        Tier {
            large: false,
            cover_w: 14,
            slot_w: 16,
        }
    }
}

/// DESIGN 3.8 adaptive cover height: a ~2:3 poster should fill the card width.
/// Geometry unreported -> fixed floors (7 large / 5 small); the adaptive value
/// never shrinks below them.
fn card_cover_h(t: &Tier, font: FontSize, geometry: bool) -> (u16, bool) {
    let floor = if t.large { 7 } else { 5 };
    if !geometry {
        return (floor, false);
    }
    let w_px = t.cover_w as u32 * font.width as u32;
    let h = (w_px * 3 / 2) / font.height.max(1) as u32;
    ((h as u16).max(floor), true)
}

/// DESIGN 3.3 detail caps: 28 rows large tier / 20 small; also the fixed
/// fallback when geometry is unreported.
fn detail_cover_h(t: &Tier, font: FontSize, geometry: bool) -> u16 {
    let cap = if t.large { 28 } else { 20 };
    if !geometry {
        return cap;
    }
    let w_px = t.cover_w as u32 * font.width as u32;
    let h = ((w_px * 3 / 2) / font.height.max(1) as u32) as u16;
    h.clamp(1, cap)
}

struct Stats {
    frames: u32,
    last_frame: Duration,
    worst_frame: Duration,
    encodes: u32,
    last_encode: Duration,
    encode_total: Duration,
    encode_errors: u32,
    decode_total: Duration,
    decodes: u32,
}

struct App {
    cards: Vec<Card>,
    picker: Picker,
    geometry: bool,
    selected: usize,
    scroll_row: usize,
    detail: bool,
    stats: Stats,
    tx_worker: mpsc::Sender<(usize, ResizeRequest)>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let force_halfblocks = args.iter().any(|a| a == "--halfblocks");
    let probe_secs: Option<u64> = args
        .iter()
        .position(|a| a == "--probe")
        .map(|i| args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(3));

    let media = match fetch_trending() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("AniList fetch failed: {e}");
            std::process::exit(1);
        }
    };

    let mut terminal = ratatui::init();

    // Query must run after entering the alternate screen but before reading
    // terminal events (it reads stdio itself).
    let mut picker = match Picker::from_query_stdio() {
        Ok(p) => p,
        Err(_) => Picker::halfblocks(),
    };
    if force_halfblocks {
        picker.set_protocol_type(ProtocolType::Halfblocks);
    }
    let geometry = picker
        .capabilities()
        .iter()
        .any(|c| matches!(c, Capability::CellSize(Some(_))));

    let (tx_worker, rx_worker) = mpsc::channel::<(usize, ResizeRequest)>();
    let (tx_main, rx_main) = mpsc::channel::<AppEvent>();

    // Encode worker: resize+encode off the render thread (AC 5).
    let tx = tx_main.clone();
    thread::spawn(move || {
        while let Ok((i, req)) = rx_worker.recv() {
            let t0 = Instant::now();
            let res = req.resize_encode();
            if tx
                .send(AppEvent::Encoded(i, Box::new(res), t0.elapsed()))
                .is_err()
            {
                break;
            }
        }
    });

    // One fetch thread per cover; decoded images pop into the grid as they land.
    for (i, m) in media.iter().enumerate() {
        let url = m.cover.large.clone();
        let tx = tx_main.clone();
        thread::spawn(move || {
            let t0 = Instant::now();
            let ev = match fetch_and_decode(&url) {
                Ok(img) => AppEvent::Cover(i, img, t0.elapsed()),
                Err(e) => AppEvent::CoverFailed(i, e),
            };
            let _ = tx.send(ev);
        });
    }

    // Input thread; polls so the probe timer can tick even with no input.
    let tx = tx_main.clone();
    thread::spawn(move || {
        loop {
            let ev = match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => AppEvent::Key(k.code),
                    Ok(_) => AppEvent::Tick,
                    Err(_) => break,
                },
                Ok(false) => AppEvent::Tick,
                Err(_) => break,
            };
            if tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut app = App {
        cards: media
            .into_iter()
            .map(|m| {
                let (req_tx, req_rx) = mpsc::channel();
                Card {
                    title: m.title.romaji,
                    proto: None,
                    img: None,
                    error: None,
                    req_tx,
                    req_rx,
                }
            })
            .collect(),
        picker,
        geometry,
        selected: 0,
        scroll_row: 0,
        detail: false,
        stats: Stats {
            frames: 0,
            last_frame: Duration::ZERO,
            worst_frame: Duration::ZERO,
            encodes: 0,
            last_encode: Duration::ZERO,
            encode_total: Duration::ZERO,
            encode_errors: 0,
            decode_total: Duration::ZERO,
            decodes: 0,
        },
        tx_worker,
    };

    let deadline = probe_secs.map(|s| Instant::now() + Duration::from_secs(s));
    loop {
        let t0 = Instant::now();
        terminal.draw(|f| ui(f, &mut app)).expect("draw");
        app.stats.last_frame = t0.elapsed();
        app.stats.worst_frame = app.stats.worst_frame.max(app.stats.last_frame);
        app.stats.frames += 1;

        // Forward the resize requests this draw produced, tagged by card.
        for (i, c) in app.cards.iter().enumerate() {
            while let Ok(req) = c.req_rx.try_recv() {
                let _ = app.tx_worker.send((i, req));
            }
        }

        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        match rx_main.recv_timeout(Duration::from_millis(200)) {
            Ok(AppEvent::Key(code)) => {
                if !handle_key(&mut app, code, &mut terminal) {
                    break;
                }
            }
            Ok(AppEvent::Cover(i, img, took)) => {
                app.stats.decodes += 1;
                app.stats.decode_total += took;
                app.cards[i].proto = Some(ThreadProtocol::new(
                    app.cards[i].req_tx.clone(),
                    Some(app.picker.new_resize_protocol(img.clone())),
                ));
                app.cards[i].img = Some(img);
            }
            Ok(AppEvent::CoverFailed(i, e)) => app.cards[i].error = Some(e),
            Ok(AppEvent::Encoded(i, res, took)) => match *res {
                Ok(done) => {
                    app.stats.encodes += 1;
                    app.stats.last_encode = took;
                    app.stats.encode_total += took;
                    if let Some(p) = app.cards[i].proto.as_mut() {
                        // false only if the card was rebuilt mid-encode; drop it
                        let _ = p.update_resized_protocol(done);
                    }
                }
                Err(_) => app.stats.encode_errors += 1,
            },
            Ok(AppEvent::Tick) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    ratatui::restore();
    report(&app);
    std::process::exit(if app.stats.encode_errors == 0 { 0 } else { 1 });
}

fn handle_key(app: &mut App, code: KeyCode, terminal: &mut ratatui::DefaultTerminal) -> bool {
    match code {
        KeyCode::Char('q') => return false,
        KeyCode::Esc => {
            if app.detail {
                app.detail = false;
            } else {
                return false;
            }
        }
        KeyCode::Enter | KeyCode::Char('d') => app.detail = !app.detail,
        KeyCode::Left | KeyCode::Char('h') => app.selected = app.selected.saturating_sub(1),
        KeyCode::Right | KeyCode::Char('l') => {
            app.selected = (app.selected + 1).min(app.cards.len().saturating_sub(1));
        }
        KeyCode::Up | KeyCode::Char('k') => app.scroll_row = app.scroll_row.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_row += 1,
        KeyCode::Char('p') => {
            // Cycle protocol and rebuild every loaded image under the new one.
            // Halfblocks and the queried protocol are the two states of interest;
            // skip the ones the terminal never claimed.
            let next = match app.picker.protocol_type() {
                ProtocolType::Halfblocks => ProtocolType::Kitty,
                _ => ProtocolType::Halfblocks,
            };
            app.picker.set_protocol_type(next);
            for c in app.cards.iter_mut() {
                if let Some(img) = &c.img {
                    c.proto = Some(ThreadProtocol::new(
                        c.req_tx.clone(),
                        Some(app.picker.new_resize_protocol(img.clone())),
                    ));
                }
            }
            let _ = terminal.clear();
        }
        KeyCode::Char('r') => {
            let _ = terminal.clear();
        }
        _ => {}
    }
    true
}

fn ui(f: &mut Frame<'_>, app: &mut App) {
    let area = f.area();
    let t = tier(area.width);
    let (cover_h, adaptive) = card_cover_h(&t, app.picker.font_size(), app.geometry);
    let slot_h = cover_h + 4;

    // Status header (2 rows) stands in for top bar + axis bar chrome.
    let font = app.picker.font_size();
    let head = format!(
        "spike_cover ROD-417 · proto={:?} · cell={}x{}px ({}) · tier={} cover={}x{} ({}) · slot={}x{} · tmux={}",
        app.picker.protocol_type(),
        font.width,
        font.height,
        if app.geometry {
            "reported"
        } else {
            "unreported"
        },
        if t.large { "large" } else { "small" },
        t.cover_w,
        cover_h,
        if adaptive { "adaptive" } else { "fixed-floor" },
        t.slot_w,
        slot_h,
        std::env::var("TMUX").is_ok(),
    );
    let mean_enc = app
        .stats
        .encode_total
        .checked_div(app.stats.encodes.max(1))
        .unwrap_or_default();
    let stat = format!(
        "frame last={:?} worst={:?} · encode last={:?} mean={:?} n={} err={} · [q]uit [arrows]select [enter]detail [p]roto [r]edraw",
        app.stats.last_frame,
        app.stats.worst_frame,
        app.stats.last_encode,
        mean_enc,
        app.stats.encodes,
        app.stats.encode_errors,
    );
    f.render_widget(
        Paragraph::new(head).style(Style::new().fg(Color::Cyan)),
        rect_row(area, 0),
    );
    f.render_widget(Paragraph::new(stat).dim(), rect_row(area, 1));

    // Card grid, DESIGN 3.8 geometry: 2-cell left margin, cols = (w-2)/slot_w.
    let grid_top = 3u16;
    let cols = ((area.width.saturating_sub(2)) / t.slot_w).max(1) as usize;
    let grid_h = area.height.saturating_sub(grid_top);
    let vis_rows = (grid_h / slot_h).max(1) as usize;

    let sel_row = app.selected / cols;
    app.scroll_row = app
        .scroll_row
        .min(sel_row)
        .max(sel_row.saturating_sub(vis_rows - 1));

    for (i, card) in app.cards.iter_mut().enumerate() {
        let (row, col) = (i / cols, i % cols);
        if row < app.scroll_row || row >= app.scroll_row + vis_rows {
            continue;
        }
        let x = 2 + col as u16 * t.slot_w;
        let y = grid_top + (row - app.scroll_row) as u16 * slot_h;
        if x + t.cover_w > area.width || y + slot_h > area.height {
            continue;
        }
        let cover = Rect::new(x, y, t.cover_w, cover_h);
        draw_cover(f, cover, card);
        let sel = i == app.selected;
        let title = Paragraph::new(card.title.as_str()).style(if sel {
            Style::new().fg(Color::Magenta).bold()
        } else {
            Style::new().dim()
        });
        f.render_widget(title, Rect::new(x, y + cover_h + 1, t.cover_w, 1));
    }

    // Detail overlay: DESIGN 3.3 block, hard cap 20 cols wide, 28/20-row cap.
    if app.detail {
        let dh = detail_cover_h(&t, font, app.geometry);
        let dw = t.cover_w.min(20);
        let w = dw + 4;
        let h = dh + 4;
        let ox = area.width.saturating_sub(w) / 2;
        let oy = area.height.saturating_sub(h) / 2;
        let overlay = Rect::new(ox, oy, w.min(area.width), h.min(area.height));
        f.render_widget(Clear, overlay);
        let card = &mut app.cards[app.selected];
        let block = Block::bordered().title(format!(" {} · {}x{} ", card.title, dw, dh));
        let inner = block.inner(overlay);
        f.render_widget(block, overlay);
        let cover = Rect::new(
            inner.x + 1,
            inner.y + 1,
            dw.min(inner.width),
            dh.min(inner.height),
        );
        draw_cover(f, cover, card);
    }
}

fn draw_cover(f: &mut Frame<'_>, cover: Rect, card: &mut Card) {
    match (card.proto.as_mut(), card.error.as_ref()) {
        (Some(p), _) => {
            f.render_stateful_widget(
                StatefulImage::default().resize(Resize::Crop(None)),
                cover,
                p,
            );
        }
        (None, Some(e)) => {
            let msg: &str = e;
            f.render_widget(Paragraph::new(msg).fg(Color::Red), cover);
        }
        (None, None) => {
            f.render_widget(
                Paragraph::new("…loading")
                    .dim()
                    .block(Block::new().style(Style::new().bg(Color::Rgb(30, 30, 40)))),
                cover,
            );
        }
    }
}

fn rect_row(area: Rect, y: u16) -> Rect {
    Rect::new(
        area.x,
        area.y + y,
        area.width,
        1.min(area.height.saturating_sub(y)),
    )
    .intersection(area)
}

fn report(app: &App) {
    let font = app.picker.font_size();
    let mean_enc = app
        .stats
        .encode_total
        .checked_div(app.stats.encodes.max(1))
        .unwrap_or_default();
    let mean_dec = app
        .stats
        .decode_total
        .checked_div(app.stats.decodes.max(1))
        .unwrap_or_default();
    println!("spike_cover report (ROD-417)");
    println!("  protocol        {:?}", app.picker.protocol_type());
    println!("  capabilities    {:?}", app.picker.capabilities());
    println!(
        "  cell px         {}x{} ({})",
        font.width,
        font.height,
        if app.geometry {
            "reported"
        } else {
            "unreported -> fixed floors"
        }
    );
    println!("  tmux            {}", std::env::var("TMUX").is_ok());
    println!(
        "  covers loaded   {}/{} (mean download+decode {:?})",
        app.stats.decodes,
        app.cards.len(),
        mean_dec
    );
    println!(
        "  frames          {} (last {:?}, worst {:?})",
        app.stats.frames, app.stats.last_frame, app.stats.worst_frame
    );
    println!(
        "  encodes         {} (last {:?}, mean {:?}, errors {})",
        app.stats.encodes, app.stats.last_encode, mean_enc, app.stats.encode_errors
    );
    for c in &app.cards {
        if let Some(e) = &c.error {
            println!("  cover error     {}: {}", c.title, e);
        }
    }
}

fn fetch_trending() -> Result<Vec<Media>, String> {
    let client = reqwest::blocking::Client::new();
    let resp: Response = client
        .post(ENDPOINT)
        .json(&serde_json::json!({ "query": QUERY }))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    Ok(resp.data.page.media)
}

fn fetch_and_decode(url: &str) -> Result<image::DynamicImage, String> {
    let bytes = reqwest::blocking::get(url)
        .map_err(|e| e.to_string())?
        .bytes()
        .map_err(|e| e.to_string())?;
    image::load_from_memory(&bytes).map_err(|e| e.to_string())
}
