//! spike_http: AniList catalog search over HTTP + JSON.
//! Parity: zigoku spike #1 (spike_http.zig, ROD-55).
//!
//! Proves the network spine: an HTTPS POST with a JSON body and typed parsing
//! of the response. This was the original go/no-go: if the network layer
//! didn't work, nothing else mattered.
//!
//! Run: cargo run --example spike_http -- frieren

use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "https://graphql.anilist.co";

const QUERY: &str = r#"
query ($search: String) {
  Page(perPage: 5) {
    media(search: $search, type: ANIME, sort: SEARCH_MATCH) {
      id
      title { romaji english }
      episodes
    }
  }
}"#;

// The request shape. serde derives the serializer; the field names ARE the JSON
// keys. No hand-built body, no escaping.
#[derive(Serialize)]
struct Request<'a> {
    query: &'a str,
    variables: Variables<'a>,
}

#[derive(Serialize)]
struct Variables<'a> {
    search: &'a str,
}

// The response shape mirrors the JSON. Optional fields tolerate missing keys;
// serde ignores unknown ones by default, so the server can send 50 fields we
// don't model and parsing still succeeds.
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
    id: u32,
    title: Title,
    episodes: Option<u32>,
}

#[derive(Deserialize)]
struct Title {
    romaji: Option<String>,
    english: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let search = std::env::args().nth(1).unwrap_or_else(|| "frieren".into());

    let client = reqwest::blocking::Client::new();
    let resp: Response = client
        .post(ENDPOINT)
        .json(&Request {
            query: QUERY,
            variables: Variables { search: &search },
        })
        .send()?
        .error_for_status()?
        .json()?;

    let media = &resp.data.page.media;
    println!("search: {search:?}  ->  {} result(s)", media.len());
    for m in media {
        let title = m
            .title
            .english
            .as_deref()
            .or(m.title.romaji.as_deref())
            .unwrap_or("<untitled>");
        let eps = m.episodes.map_or_else(|| "?".into(), |n| n.to_string());
        println!("  [{:>6}] {title}  ({eps} eps)", m.id);
    }
    Ok(())
}
