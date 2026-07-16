//! spike_concurrency: N workers fetch concurrently, results come back on a channel.
//! Parity: zigoku spike #3 (concurrency.zig, ROD-58).
//!
//! This is the pattern the TUI rides on: offload blocking work to worker threads
//! that post results back to one thread through a channel. zigoku had to
//! hand-build a generic `Channel(T)` on `std.Io.Mutex`/`Condition`, thread `io`
//! through every lock, and pick a thread-safe allocator per scope. Rust's std
//! ships the channel, and the borrow checker proves there's no data race for
//! free: the `move` closure transfers ownership, so a worker cannot touch memory
//! another thread owns.
//!
//! Run: cargo run --bin spike_concurrency -- <ignored, queries are fixed>

use serde::Deserialize;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

const ENDPOINT: &str = "https://graphql.anilist.co";
const QUERY: &str =
    "query($s:String){Page(perPage:1){media(search:$s,type:ANIME,sort:SEARCH_MATCH){title{romaji}}}}";

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
}
#[derive(Deserialize)]
struct Title {
    romaji: Option<String>,
}

// What a worker hands back. Carries its own errors as data, not panics: a failed
// fetch reports cleanly instead of taking down the thread.
struct Msg {
    idx: usize,
    query: &'static str,
    result: Result<String, String>,
    ms: u128,
}

fn fetch_top(client: &reqwest::blocking::Client, search: &str) -> Result<String, String> {
    let body = serde_json::json!({ "query": QUERY, "variables": { "s": search } });
    let resp: Response = client
        .post(ENDPOINT)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    resp.data
        .page
        .media
        .into_iter()
        .next()
        .and_then(|m| m.title.romaji)
        .ok_or_else(|| "no results".to_string())
}

fn main() {
    let queries = ["frieren", "bocchi the rock", "steins gate", "vinland saga", "chainsaw man"];
    let (tx, rx) = mpsc::channel();

    for (idx, &query) in queries.iter().enumerate() {
        let tx = tx.clone();
        thread::spawn(move || {
            // Each worker owns its own client; a reqwest client is a cheap handle.
            let client = reqwest::blocking::Client::new();
            let start = Instant::now();
            let result = fetch_top(&client, query);
            // Ignore send errors: if the receiver is gone, we're shutting down.
            let _ = tx.send(Msg { idx, query, result, ms: start.elapsed().as_millis() });
        });
    }
    // Drop the original sender so the `for msg in rx` loop ends once every worker
    // (each holding a clone) has finished. Forget this and the loop hangs forever.
    drop(tx);

    println!("spawned {} workers; results in COMPLETION order:\n", queries.len());
    for msg in rx {
        match msg.result {
            Ok(title) => println!("  #{} {:<16} -> {title} ({} ms)", msg.idx, msg.query, msg.ms),
            Err(e) => println!("  #{} {:<16} -> ERR: {e} ({} ms)", msg.idx, msg.query, msg.ms),
        }
    }
}
