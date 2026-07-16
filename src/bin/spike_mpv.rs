//! spike_mpv: the capstone. Search, resolve a real stream, hand it to mpv.
//! Parity: zigoku spike #5 (mpv_play.zig, ROD-57).
//!
//! This is the whole chain end to end against the live provider: search for a
//! show, fetch its episode, decrypt the `tobeparsed` blob (the same crypto
//! spike_stream proved), pick a playable URL (direct fast4speed, or follow a
//! `--<hex>` provider through clock.json), and spawn mpv on it. The headless
//! probe decodes one real frame and exits 0, which is the actual proof: the
//! resolved stream plays.
//!
//! Run:  cargo run --bin spike_mpv -- frieren                                # window
//!       cargo run --bin spike_mpv -- frieren --frames=1 --vo=null --no-audio # probe

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::{Engine, alphabet};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::process::{Command, ExitCode};

const API: &str = "https://api.allanime.day/api";
const SITE: &str = "https://allanime.day";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/86.0.4240.198 Safari/537.36";
const REFERER_API: &str = "https://allmanga.to/"; // search + clock GET
const REFERER_VIDEO: &str = "https://youtu-chan.com/"; // get_video
const GCM_SEED: &[u8] = b"Xot36i3lK3:v1";

const HASH_SEARCH: &str = "a24c500a1b765c68ae1d8dd85174931f661c71369c89b92b88b75a725afc471c";
const HASH_VIDEO: &str = "d405d0edd690624b66baba3068e0edc3ac90f1597d898a1ec8db4e5c43c00fec";

// anipy's trusted sourceName allow-list (case-insensitive; API sends "S-mp4").
const ALLOWED_SOURCES: &[&str] = &["Yt-mp4", "S-Mp4", "Uv-mp4", "Ak", "Default"];

type Err = Box<dyn std::error::Error>;

struct StreamLink {
    url: String,
    referer: String,
}

// ── search ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchResp {
    data: Option<SData>,
}
#[derive(Deserialize)]
struct SData {
    shows: Shows,
}
#[derive(Deserialize)]
struct Shows {
    edges: Vec<Edge>,
}
#[derive(Deserialize)]
struct Edge {
    #[serde(rename = "_id")]
    id: String,
    name: Option<String>,
    #[serde(rename = "availableEpisodes")]
    available: Option<Avail>,
}
#[derive(Deserialize)]
struct Avail {
    sub: Option<u32>,
}

fn search(client: &reqwest::blocking::Client, query: &str) -> Result<Edge, Err> {
    let ext = format!(r#"{{"persistedQuery":{{"version":1,"sha256Hash":"{HASH_SEARCH}"}}}}"#);
    let body = serde_json::json!({
        "variables": {
            "search": { "query": query },
            "limit": 40, "page": 1,
            "translationType": "sub", "countryOrigin": "ALL"
        },
        "extensions": ext,
    })
    .to_string();

    let resp: SearchResp = client
        .post(API)
        .header("Referer", REFERER_API)
        .header("Content-Type", "application/json")
        .body(body)
        .send()?
        .error_for_status()?
        .json()?;

    let edges = resp.data.ok_or("search: data null (hash rotated?)")?.shows.edges;
    // Pick the best sub match: a name-contains bonus, then most episodes.
    edges
        .into_iter()
        .filter(|e| e.available.as_ref().and_then(|a| a.sub).unwrap_or(0) > 0)
        .max_by_key(|e| {
            let name = e.name.as_deref().unwrap_or("").to_lowercase();
            let bonus = if name.contains(&query.to_lowercase()) { 1_000_000 } else { 0 };
            bonus + e.available.as_ref().and_then(|a| a.sub).unwrap_or(0)
        })
        .ok_or_else(|| "search: no show with sub episodes".into())
}

// ── get_video + decrypt ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VideoResp {
    data: Option<VData>,
}
#[derive(Deserialize)]
struct VData {
    tobeparsed: Option<String>,
}
#[derive(Deserialize)]
struct Dec {
    episode: DecEp,
}
#[derive(Deserialize)]
struct DecEp {
    #[serde(rename = "sourceUrls")]
    source_urls: Vec<Src>,
}
#[derive(Deserialize)]
struct Src {
    #[serde(rename = "sourceName")]
    source_name: Option<String>,
    #[serde(rename = "sourceUrl")]
    source_url: Option<String>,
}

fn get_tobeparsed(
    client: &reqwest::blocking::Client,
    show_id: &str,
    episode: &str,
) -> Result<String, Err> {
    // Double escape on purpose: inner variables is a JSON *string* of JSON.
    let inner = serde_json::json!({
        "showId": show_id, "translationType": "sub", "episodeString": episode
    })
    .to_string();
    let ext = format!(r#"{{"persistedQuery":{{"version":1,"sha256Hash":"{HASH_VIDEO}"}}}}"#);
    let body = serde_json::json!({ "variables": inner, "extensions": ext }).to_string();

    let resp: VideoResp = client
        .post(API)
        .header("Referer", REFERER_VIDEO)
        .header("Content-Type", "application/json")
        .body(body)
        .send()?
        .error_for_status()?
        .json()?;

    resp.data
        .and_then(|d| d.tobeparsed)
        .ok_or_else(|| "get_video: no tobeparsed (hash rotated or unencrypted)".into())
}

// tobeparsed layout: [0] prefix, [1..13] nonce, [13..] ciphertext||tag.
fn decrypt_tobeparsed(blob: &str) -> Result<Vec<u8>, Err> {
    let key = Sha256::digest(GCM_SEED);
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())?;
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let raw = engine.decode(blob)?;
    if raw.len() < 1 + 12 + 16 {
        return Err("blob too small".into());
    }
    Ok(cipher
        .decrypt(Nonce::from_slice(&raw[1..13]), &raw[13..])
        .map_err(|_| "GCM authentication failed")?)
}

// ── long-tail follow ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ClkResp {
    #[serde(default)]
    links: Vec<ClkLink>,
}
#[derive(Deserialize)]
struct ClkLink {
    link: Option<String>,
    headers: Option<ClkHdr>,
}
#[derive(Deserialize)]
struct ClkHdr {
    #[serde(rename = "Referer")]
    referer: Option<String>,
}

// `--<hex>` provider path: hex pairs XOR 0x38 -> a clock API path.
fn decipher_provider_path(hex: &str) -> Result<String, Err> {
    if hex.len() % 2 != 0 {
        return Err("odd-length hex".into());
    }
    let bytes: Result<Vec<u8>, _> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map(|b| b ^ 0x38))
        .collect();
    Ok(String::from_utf8(bytes?)?)
}

// Insert ".json" right after the "clock" segment.
fn clock_json(path: &str) -> String {
    match path.find("clock") {
        Some(at) => {
            let cut = at + "clock".len();
            format!("{}.json{}", &path[..cut], &path[cut..])
        }
        None => path.to_string(),
    }
}

// Safe for mpv argv: printable ASCII only. Catches CR/LF and >=0x80 that a
// "< 0x20" denylist would miss. Also rejects a leading "--" mpv reads as a flag.
fn clean_arg(s: &str) -> bool {
    s.bytes().all(|c| (0x21..=0x7e).contains(&c))
}

fn consider(url: &str, referer: &str) -> Option<StreamLink> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    if !clean_arg(url) {
        return None;
    }
    Some(StreamLink { url: url.to_string(), referer: referer.to_string() })
}

fn safe_referer(r: Option<&str>) -> String {
    match r {
        Some(v) if clean_arg(v) => v.to_string(),
        _ => SITE.to_string(),
    }
}

fn follow_provider(
    client: &reqwest::blocking::Client,
    hex_path: &str,
) -> Result<Option<StreamLink>, Err> {
    let path = clock_json(&decipher_provider_path(hex_path)?);
    // Path must start with "/" or SITE becomes userinfo ("@evil/x" SSRF).
    if !path.starts_with('/') {
        return Err("bad provider path".into());
    }
    let resp: ClkResp = client
        .get(format!("{SITE}{path}"))
        .header("Referer", REFERER_API)
        .send()?
        .error_for_status()?
        .json()?;

    // mpv follows m3u8 masters itself, so any playable http(s) link is enough.
    for l in resp.links {
        let Some(link) = l.link else { continue };
        let referer = safe_referer(l.headers.and_then(|h| h.referer).as_deref());
        if let Some(sl) = consider(&link, &referer) {
            return Ok(Some(sl));
        }
    }
    Ok(None)
}

fn resolve(
    client: &reqwest::blocking::Client,
    show_id: &str,
    episode: &str,
) -> Result<StreamLink, Err> {
    let plain = decrypt_tobeparsed(&get_tobeparsed(client, show_id, episode)?)?;
    let dec: Dec = serde_json::from_slice(&plain)?;

    let allowed = |name: &Option<String>| {
        name.as_deref()
            .map(|n| ALLOWED_SOURCES.iter().any(|a| a.eq_ignore_ascii_case(n)))
            .unwrap_or(false)
    };

    // Fast path: a direct fast4speed URL.
    for s in &dec.episode.source_urls {
        if !allowed(&s.source_name) {
            continue;
        }
        if let Some(url) = &s.source_url {
            if url.contains("tools.fast4speed.rsvp") {
                if let Some(sl) = consider(url, SITE) {
                    return Ok(sl);
                }
            }
        }
    }

    // Long tail: follow a "--<hex>" provider to clock.json.
    for s in &dec.episode.source_urls {
        if !allowed(&s.source_name) {
            continue;
        }
        let Some(url) = &s.source_url else { continue };
        let Some(hex) = url.strip_prefix("--") else { continue };
        match follow_provider(client, hex) {
            Ok(Some(sl)) => return Ok(sl),
            Ok(None) => {}
            Err(e) => eprintln!("  provider {url}: {e}"),
        }
    }
    Err("no playable stream found".into())
}

fn main() -> ExitCode {
    let all: Vec<String> = std::env::args().skip(1).collect();
    // First non-flag arg is the search query; the rest pass through to mpv.
    let (query, passthrough) = match all.split_first() {
        Some((first, rest)) if !first.starts_with('-') => (first.clone(), rest.to_vec()),
        _ => ("frieren".to_string(), all),
    };

    let client = match reqwest::blocking::Client::builder().user_agent(UA).build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("client: {e}");
            return ExitCode::FAILURE;
        }
    };

    let show = match search(&client, &query) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("search failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "show: {:?}  id={}  ({} sub eps)",
        show.name.as_deref().unwrap_or("?"),
        show.id,
        show.available.and_then(|a| a.sub).unwrap_or(0),
    );

    let link = match resolve(&client, &show.id, "1") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("resolve failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("resolved: {}\n  referer: {}", link.url, link.referer);

    let mut cmd = Command::new("mpv");
    cmd.arg(format!("--referrer={}", link.referer)).arg(&link.url).args(&passthrough);
    println!("spawning: mpv --referrer={} {} {}", link.referer, link.url, passthrough.join(" "));

    match cmd.status() {
        Ok(s) if s.success() => {
            println!("mpv exited 0: the resolved stream played");
            ExitCode::SUCCESS
        }
        Ok(s) => {
            println!("mpv exited: {s}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("failed to spawn mpv: {e}");
            ExitCode::FAILURE
        }
    }
}
