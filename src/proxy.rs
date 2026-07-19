//! Localhost HLS de-cloaking reverse proxy (ROD-443, ported from zigoku v0.4.8).
//!
//! Some CDNs (nekostream via p16-ad-sg.ibyteimg.com) prepend a decoy image
//! header to each MPEG-TS segment: 70 junk bytes (megaplay), 252 (anineko),
//! then the real TS sync at that offset. ffmpeg content-probes byte 0, sees the
//! fake magic, classifies the whole stream as an image, fatal. No mpv/ffmpeg
//! flag reaches the inner segment demuxer, so the bytes must be stripped before
//! mpv sees them.
//!
//! Shape: mpv talks plaintext HTTP to `127.0.0.1:<ephemeral>/r.ts?u=<pct
//! upstream>`. Each request fetches the upstream (TLS, referer + UA), then
//! either:
//!   - playlist (`#EXTM3U`): rewrite every URI to another `/r.ts?u=…` loopback
//!     ref so variants and segments route back through here (relatives joined
//!     via hls::join_url).
//!   - segment: strip the prefix to the first TS-sync triple, stream the rest.
//!
//! Content-sniffing avoids parsing STREAM-INF vs EXTINF; scanning for the sync
//! triple is provider-agnostic (any prefix length, clean-from-0 and non-TS pass
//! through).
//!
//! Reference note (per the v0.4.7 senshi precedent): the freeze governs the
//! architecture (03 §6.7 SSRF, §8.4 transport), but the decoy-prefix protocol
//! bytes are live-site facts, so this ports from the live tag v0.4.8, not the
//! 083abd3 freeze. Recorded in 08 §10.
//!
//! Lifecycle is one playback: `engage` starts the proxy when the link is
//! flagged and hands back a `Decloak` guard; the mpv path (ROD-437) points mpv
//! at the guard url and drops the guard on exit, tearing the proxy down. Every
//! hop is re-guarded against SSRF (03 §6.7): a hostile playlist cannot bounce
//! the proxy at a private IP. The RAII-guard seam (vs zigoku's `proxy.play`
//! wrapper) and the `Arc<Proxy>` teardown (vs zigoku's Gate refcount +
//! leak-not-free dance) are recorded in 08 §10.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::domain::StreamLink;
use crate::fetchguard::guard_fetch_url;
use crate::providers::hls::join_url;

/// TS packet size; three consecutive sync bytes at this stride mark a real stream.
const TS_PACKET: usize = 188;
/// Prefixes are tens-to-hundreds of bytes; bound the sync search so a mid-payload
/// 0x47 coincidence can never be mistaken for the stream start.
const MAX_PREFIX_SCAN: usize = 4096;
/// Response ceiling per upstream object. Playlists are tiny; TS segments a few MiB.
const MAX_BODY: u64 = 32 << 20;
/// Redirect hops per upstream fetch (the ibyteimg 302 is one; leave headroom).
const MAX_REDIRECTS: u8 = 5;
/// Wall-clock ceiling on one upstream fetch (redirect chain + body). Without it a
/// CDN that accepts then goes silent parks a handler thread forever.
const FETCH_DEADLINE: Duration = Duration::from_secs(30);
/// Per-socket idle timeout on the client-facing (mpv) side. A stalled or
/// slow-trickling local client is dropped instead of pinning its handler thread
/// (and the `Arc<Proxy>` it holds) for the port's lifetime. std has the
/// per-socket timeout zigoku's Io lacked. Safe against mpv keep-alive: a closed
/// idle connection is just reopened on the next segment.
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on one request head (line + headers). Bounds memory against a hostile
/// local client streaming an endless header; a head past this hits EOF mid-parse
/// and the connection closes.
const MAX_HEAD_BYTES: u64 = 16 * 1024;

/// Loopback request path + query prefix. The `.ts` suffix is deliberate: it sits
/// in ffmpeg's default extension allowlist, so mpv never trips the HLS extension
/// gate on our extensionless upstreams. `u` carries the fully pct-encoded
/// upstream url (dots encoded too, so `.ts` is the only extension).
const PATH_PREFIX: &str = "/r.ts?u=";

const STATUS_OK: &str = "200 OK";
const STATUS_NOT_FOUND: &str = "404 Not Found";
const STATUS_BAD_GATEWAY: &str = "502 Bad Gateway";
const CONTENT_TYPE_PLAIN: &str = "text/plain";
const CONTENT_TYPE_M3U8: &str = "application/vnd.apple.mpegurl";
const CONTENT_TYPE_TS: &str = "video/mp2t";

/// Playback-scoped de-cloaking guard. `engage` returns one per play call: either
/// a pass-through (link not flagged) or a live proxy. Dropping it stops the
/// proxy, so the mpv path holds it for exactly the process lifetime.
pub struct Decloak {
    proxy: Option<Arc<Proxy>>,
    url: String,
}

impl Decloak {
    /// The url mpv must open: the loopback master when the link is cloaked, the
    /// original stream url otherwise.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for Decloak {
    fn drop(&mut self) {
        if let Some(proxy) = &self.proxy {
            proxy.stop();
        }
    }
}

/// Start a de-cloaking proxy for `link` when it is flagged, else a transparent
/// pass-through. The returned guard owns the proxy lifetime; the caller points
/// mpv at `guard.url()` and drops the guard once mpv exits.
pub fn engage(link: &StreamLink) -> Result<Decloak, ProxyStartError> {
    if !link.decloak_segments {
        return Ok(Decloak {
            proxy: None,
            url: link.url.clone(),
        });
    }
    let proxy = Proxy::start(link)?;
    let url = build_loopback_url(proxy.port, &link.url);
    Ok(Decloak {
        proxy: Some(proxy),
        url,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyStartError {
    #[error("bind loopback: {0}")]
    Bind(#[from] io::Error),

    #[error("build http client: {0}")]
    Client(#[from] reqwest::Error),
}

struct Proxy {
    port: u16,
    listener: TcpListener,
    http: reqwest::blocking::Client,
    /// Duped from the link so handler threads never alias the caller's data.
    referer: Option<String>,
    user_agent: Option<String>,
    shutting_down: AtomicBool,
    accept_handle: Mutex<Option<JoinHandle<()>>>,
}

impl Proxy {
    /// Bind loopback:0, learn the ephemeral port, spin the accept loop. Heap
    /// (`Arc`) so handler threads share one stable address.
    fn start(link: &StreamLink) -> Result<Arc<Proxy>, ProxyStartError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        // identity + no redirect: bytes need no decompression before de-cloak,
        // and 3xx is handled by hand in fetch_upstream so every hop is guarded.
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let proxy = Arc::new(Proxy {
            port,
            listener,
            http,
            referer: link.referer.clone(),
            user_agent: link.user_agent.clone(),
            shutting_down: AtomicBool::new(false),
            accept_handle: Mutex::new(None),
        });
        let accept_arc = Arc::clone(&proxy);
        let handle = thread::spawn(move || accept_arc.accept_loop());
        *proxy.accept_handle.lock().unwrap() = Some(handle);
        Ok(proxy)
    }

    /// Stop accepting and join the accept thread. In-flight handler threads are
    /// detached; each holds an `Arc<Proxy>`, so the socket + duped strings stay
    /// alive until the last one returns, then the `Arc` drop frees everything.
    /// No drain, no leak-vs-free choice: refcounting is the whole story.
    fn stop(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.wake_accept();
        if let Some(handle) = self.accept_handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    /// One best-effort loopback dial to unblock a thread parked in `accept()`.
    /// `TcpListener::accept` has no cancel, so mirror zigoku's self-dial: set
    /// `shutting_down` first, then dial once; the loop re-checks the flag right
    /// after accept returns and exits. A refused dial means it already left.
    fn wake_accept(&self) {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", self.port)) {
            drop(stream);
        }
    }

    fn accept_loop(self: Arc<Proxy>) {
        loop {
            if self.shutting_down.load(Ordering::Acquire) {
                return;
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.shutting_down.load(Ordering::Acquire) {
                        // The wake dial (or a late real conn) landed during
                        // shutdown; drop it.
                        drop(stream);
                        return;
                    }
                    let handler = Arc::clone(&self);
                    // Detached: the Arc clone keeps Proxy alive past stop() if
                    // this outlives the drain-free teardown.
                    thread::spawn(move || handler.handle_conn(stream));
                }
                Err(_) => {
                    if self.shutting_down.load(Ordering::Acquire) {
                        return;
                    }
                    // Transient accept error while live: back off, do not
                    // hot-spin the core (fd exhaustion would feed itself).
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    fn handle_conn(&self, stream: TcpStream) {
        // Drop a stalled/slow client instead of pinning this thread (and its
        // Arc<Proxy>) for the port's lifetime. Read bounds a client that never
        // sends; write bounds one that stops reading our response body.
        let _ = stream.set_read_timeout(Some(CLIENT_IDLE_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CLIENT_IDLE_TIMEOUT));
        let Ok(read_half) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(read_half);
        let mut writer = stream;
        // Keep-alive loop: mpv reuses one connection across many segment GETs.
        loop {
            match read_request_target(&mut reader) {
                Ok(Some(target)) => match self.serve(&target, &mut writer) {
                    // A client that hangs up mid-response (mpv seeking/stopping)
                    // surfaces as a write error: a normal disconnect, drop it.
                    Ok(KeepAlive::Yes) => continue,
                    Ok(KeepAlive::No) | Err(_) => return,
                },
                // Clean EOF or a malformed request line: done with this conn.
                Ok(None) | Err(_) => return,
            }
        }
    }

    /// Serve one loopback request. Non-`/r.ts?u=` paths 404; any upstream/serve
    /// failure is a 502. Both close the connection; a success keeps it alive.
    fn serve(&self, target: &str, writer: &mut impl Write) -> io::Result<KeepAlive> {
        let Some(encoded) = target.strip_prefix(PATH_PREFIX) else {
            return close_with(writer, STATUS_NOT_FOUND);
        };
        let upstream = match percent_decode(encoded)
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| DecodeError))
        {
            Ok(url) => url,
            Err(_) => return close_with(writer, STATUS_BAD_GATEWAY),
        };
        // Bound the whole fetch (redirect chain + body). Byte-sanitise and the
        // per-hop SSRF guard both live inside fetch_upstream (redirects too).
        let fetched = match fetch_upstream(
            &self.http,
            &upstream,
            self.referer.as_deref(),
            self.user_agent.as_deref(),
        ) {
            Ok(fetched) => fetched,
            Err(_) => return close_with(writer, STATUS_BAD_GATEWAY),
        };
        respond(writer, &fetched.body, &fetched.final_url, self.port)
    }
}

/// Dispatch a fetched upstream body to the client: a playlist is rewritten to
/// loopback refs, a segment is de-cloaked. Split from `serve` so the dispatch
/// composition is unit-testable without a live upstream (the SSRF guard blocks a
/// loopback mock, so the full wire path can only be exercised piecewise).
fn respond(
    writer: &mut impl Write,
    body: &[u8],
    final_url: &str,
    port: u16,
) -> io::Result<KeepAlive> {
    if is_playlist(body) {
        // is_playlist already proved a text `#EXTM3U` head with no NUL; a body
        // that still is not utf-8 is malformed, 502 it.
        let Ok(text) = std::str::from_utf8(body) else {
            return close_with(writer, STATUS_BAD_GATEWAY);
        };
        let rewritten = rewrite_playlist(text, final_url, port);
        write_response(
            writer,
            STATUS_OK,
            CONTENT_TYPE_M3U8,
            rewritten.as_bytes(),
            true,
        )?;
    } else {
        write_response(writer, STATUS_OK, CONTENT_TYPE_TS, decloak(body), true)?;
    }
    Ok(KeepAlive::Yes)
}

/// Write an empty-body error response that closes the connection, and report it
/// as not reusable. Collapses the 404/502 exits in `serve`.
fn close_with(writer: &mut impl Write, status: &str) -> io::Result<KeepAlive> {
    write_response(writer, status, CONTENT_TYPE_PLAIN, b"", false)?;
    Ok(KeepAlive::No)
}

enum KeepAlive {
    Yes,
    No,
}

/// Read one HTTP/1.1 request off `reader`, returning its target (the second
/// request-line token). `None` on a clean EOF between requests. Headers are
/// consumed to the blank line but ignored: the client is local mpv, which only
/// ever sends bodiless GETs, so there is no body to frame.
fn read_request_target<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    // Bound the whole head: a hostile local client streaming an endless line or
    // header run hits the Take limit (read_line returns 0) instead of growing
    // memory. An oversized head yields a target that fails the path match -> 404.
    let mut head = reader.take(MAX_HEAD_BYTES);
    let mut line = String::new();
    if head.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let target = line.split(' ').nth(1).unwrap_or("").to_string();
    loop {
        let mut header = String::new();
        let n = head.read_line(&mut header)?;
        if n == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }
    Ok(Some(target))
}

fn write_response(
    writer: &mut impl Write,
    status: &str,
    content_type: &str,
    body: &[u8],
    keep_alive: bool,
) -> io::Result<()> {
    let connection = if keep_alive { "keep-alive" } else { "close" };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: {connection}\r\n\r\n",
        body.len()
    );
    writer.write_all(head.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

struct Fetched {
    body: Vec<u8>,
    /// The last hop's url; playlist relatives resolve against it, not the request.
    final_url: String,
}

#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error("bad upstream url")]
    BadUrl,
    #[error("blocked host")]
    Blocked,
    #[error("redirect without location")]
    RedirectNoLocation,
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("upstream status {0}")]
    Status(u16),
    #[error("network")]
    Network,
    #[error("body exceeds cap")]
    TooLarge,
    #[error("fetch deadline")]
    Timeout,
}

/// Fetch `start_url` with referer/UA, following redirects by hand so every hop
/// is SSRF-guarded. The whole chain (redirects + body read) is bounded by
/// `FETCH_DEADLINE` via a per-request remaining-time timeout.
fn fetch_upstream(
    http: &reqwest::blocking::Client,
    start_url: &str,
    referer: Option<&str>,
    user_agent: Option<&str>,
) -> Result<Fetched, FetchError> {
    let deadline = Instant::now() + FETCH_DEADLINE;
    let mut url = start_url.to_string();
    let mut hops: u8 = 0;
    loop {
        // Reject control bytes BEFORE the request: a decoded upstream or a
        // redirect Location carrying CR/LF/NUL would otherwise reach the
        // outbound request line. Runs on every hop, so redirects get it too.
        if !url_bytes_clean(&url) {
            return Err(FetchError::BadUrl);
        }
        guard_fetch_url(&url).map_err(|_| FetchError::Blocked)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(FetchError::Timeout)?;

        let mut builder = http
            .get(&url)
            .timeout(remaining)
            .header("Accept-Encoding", "identity");
        if let Some(referer) = referer {
            builder = builder.header("Referer", referer);
        }
        if let Some(user_agent) = user_agent {
            builder = builder.header("User-Agent", user_agent);
        }

        let response = builder.send().map_err(|_| FetchError::Network)?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::RedirectNoLocation)?;
            if hops >= MAX_REDIRECTS {
                return Err(FetchError::TooManyRedirects);
            }
            hops += 1;
            // Location may be relative; resolve against the current hop before
            // the next loop re-guards it.
            url = join_url(&url, location).ok_or(FetchError::BadUrl)?;
            continue;
        }
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }

        let mut body = Vec::new();
        response
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|_| FetchError::Network)?;
        if body.len() as u64 > MAX_BODY {
            return Err(FetchError::TooLarge);
        }
        return Ok(Fetched {
            body,
            final_url: url,
        });
    }
}

/// A leading `#EXTM3U` (past an optional BOM/whitespace) marks a playlist vs a
/// segment. A NUL in the head rejects it: real m3u8 is text, so a binary segment
/// wearing an `#EXTM3U` prefix (the cloak trick in reverse, to route a segment
/// into the rewriter and get it shredded) fails the sniff and falls to the
/// de-cloak path instead.
fn is_playlist(body: &[u8]) -> bool {
    let mut b = body;
    if b.len() >= 3 && b[0] == 0xEF && b[1] == 0xBB && b[2] == 0xBF {
        b = &b[3..];
    }
    let b = trim_start_ascii_ws(b);
    if !b.starts_with(b"#EXTM3U") {
        return false;
    }
    let head = &body[..body.len().min(1024)];
    !head.contains(&0)
}

fn trim_start_ascii_ws(mut b: &[u8]) -> &[u8] {
    while let Some((first, rest)) = b.split_first() {
        if matches!(first, b' ' | b'\t' | b'\r' | b'\n') {
            b = rest;
        } else {
            break;
        }
    }
    b
}

/// Strip any decoy prefix to the first TS-sync triple (0x47 at i, i+188, i+376).
/// Clean streams return unchanged (match at i=0); no sync in the scan window
/// passes through (fMP4/unknown, let mpv decide) rather than corrupting a stream
/// we do not understand.
fn decloak(body: &[u8]) -> &[u8] {
    let stride = TS_PACKET;
    let limit = body.len().min(MAX_PREFIX_SCAN);
    let mut i = 0;
    while i < limit && i + 2 * stride < body.len() {
        if body[i] == 0x47 && body[i + stride] == 0x47 && body[i + 2 * stride] == 0x47 {
            return &body[i..];
        }
        i += 1;
    }
    // No sync in the window. Legit for fMP4; otherwise a segment whose decoy
    // prefix outgrew MAX_PREFIX_SCAN, which silently un-fixes ROD-443. zigoku
    // warns here (distinguishing fMP4 from an outgrown prefix via a box-type
    // sniff); no log sink exists until the TUI shell ticket (ROD-439, see
    // http.rs), so that diagnostic and its fMP4 check are owed there.
    body
}

/// Printable ASCII only (0x21-0x7e): a url a real CDN serves. Rejects control
/// bytes (CR/LF/NUL), spaces, and high bytes that could split the outbound
/// request line.
fn url_bytes_clean(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| (0x21..=0x7e).contains(&c))
}

/// Rewrite every URI in a playlist to a loopback `/r.ts?u=…` ref so variants and
/// segments route back through the proxy. URI lines and `URI="…"` tag attributes
/// (KEY/MEDIA/MAP) are joined against `base_url` and re-pointed; comments and
/// blanks pass through.
fn rewrite_playlist(text: &str, base_url: &str, port: u16) -> String {
    let mut out = String::new();
    let mut first = true;
    for raw in text.split('\n') {
        if !first {
            out.push('\n');
        }
        first = false;
        let line = raw.trim_matches([' ', '\t', '\r']);
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            out.push_str(&rewrite_tag_uri(line, base_url, port));
        } else if let Some(abs) = join_url(base_url, line) {
            out.push_str(&build_loopback_url(port, &abs));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Re-point a `URI="…"` attribute inside a tag line; lines without one pass
/// through unchanged.
fn rewrite_tag_uri(line: &str, base_url: &str, port: u16) -> String {
    let key = "URI=\"";
    let Some(at) = line.find(key) else {
        return line.to_string();
    };
    let vstart = at + key.len();
    let Some(vend_rel) = line[vstart..].find('"') else {
        return line.to_string();
    };
    let vend = vstart + vend_rel;
    let Some(abs) = join_url(base_url, &line[vstart..vend]) else {
        return line.to_string();
    };
    let loopback = build_loopback_url(port, &abs);
    format!("{}{}{}", &line[..vstart], loopback, &line[vend..])
}

/// `http://127.0.0.1:<port>/r.ts?u=<pct upstream>`.
fn build_loopback_url(port: u16, upstream: &str) -> String {
    format!(
        "http://127.0.0.1:{port}{PATH_PREFIX}{}",
        percent_encode(upstream)
    )
}

/// Unreserved for our purposes EXCLUDING `.`: encoding dots keeps the loopback
/// url's only extension the synthetic `.ts` in the path, never one leaking from
/// the query.
fn is_unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'~')
}

/// Percent-encode all but RFC 3986 unreserved bytes: the whole upstream url
/// (scheme, host, path, query) survives intact as one `u` query value through
/// mpv and back.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if is_unreserved(c) {
            out.push(c as char);
        } else {
            out.push('%');
            out.push(hex_digit(c >> 4));
            out.push(hex_digit(c & 0x0f));
        }
    }
    out
}

/// A percent escape was truncated (`%`, `%A`) or non-hex (`%zz`).
#[derive(Debug, PartialEq)]
struct DecodeError;

fn percent_decode(s: &str) -> Result<Vec<u8>, DecodeError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(DecodeError);
            }
            let hi = unhex(bytes[i + 1]).ok_or(DecodeError)?;
            let lo = unhex(bytes[i + 2]).ok_or(DecodeError)?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn hex_digit(nibble: u8) -> char {
    if nibble < 10 {
        (b'0' + nibble) as char
    } else {
        (b'A' + (nibble - 10)) as char
    }
}

fn unhex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_round_trips_the_full_upstream_url() {
        let url = "https://cdn.nekostream.site/x/master.m3u8?token=ab.cd_ef&exp=1720000000";
        let enc = percent_encode(url);
        // Reserved bytes are escaped; only alnum - _ ~ stay literal. Dots too,
        // so the loopback url's sole extension is the synthetic `.ts`.
        for reserved in [':', '/', '?', '&', '.'] {
            assert!(!enc.contains(reserved), "{reserved} leaked: {enc}");
        }
        assert_eq!(percent_decode(&enc).unwrap(), url.as_bytes());
    }

    #[test]
    fn percent_decode_rejects_truncated_or_invalid_escape() {
        assert_eq!(percent_decode("abc%"), Err(DecodeError));
        assert_eq!(percent_decode("abc%2"), Err(DecodeError));
        assert_eq!(percent_decode("abc%zz"), Err(DecodeError));
        assert_eq!(percent_decode("a%20b").unwrap(), b"a b");
    }

    #[test]
    fn is_playlist_extm3u_past_bom_ws_true_ts_bytes_false() {
        assert!(is_playlist(b"#EXTM3U\n#EXT-X-VERSION:3\n"));
        assert!(is_playlist(b"\xEF\xBB\xBF#EXTM3U\n"));
        assert!(is_playlist(b"  \n#EXTM3U"));
        assert!(!is_playlist(b"\x47\x40\x00\x10"));
        assert!(!is_playlist(b""));
        assert!(!is_playlist(b"\x89PNG\r\n"));
    }

    #[test]
    fn is_playlist_rejects_binary_segment_wearing_extm3u_prefix() {
        assert!(is_playlist(b"#EXTM3U\n#EXT-X-VERSION:3\nseg0\n"));
        // A TS segment prefixed with the magic bytes but carrying NULs must NOT
        // be treated as a playlist (would be shredded through the rewriter); it
        // falls to the de-cloak path instead.
        let mut spoof = Vec::from(*b"#EXTM3U\n");
        for i in 0..504u32 {
            spoof.push(if i % 7 == 0 { 0x00 } else { 0x47 });
        }
        assert!(!is_playlist(&spoof));
    }

    #[test]
    fn decloak_strips_a_decoy_prefix_to_the_first_ts_sync_triple() {
        for prefix in [70usize, 252] {
            let mut buf = vec![0u8; prefix + 3 * TS_PACKET];
            for (i, b) in buf.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
            buf[0] = 0x89; // definitely not 0x47 at byte 0
            buf[prefix] = 0x47;
            buf[prefix + TS_PACKET] = 0x47;
            buf[prefix + 2 * TS_PACKET] = 0x47;
            let out = decloak(&buf);
            assert_eq!(out.len(), 3 * TS_PACKET);
            assert_eq!(out[0], 0x47);
        }
    }

    #[test]
    fn decloak_passes_clean_and_unrecognized_streams_through_untouched() {
        // Clean TS from byte 0 matches at i=0 → unchanged.
        let mut clean = vec![0u8; 3 * TS_PACKET];
        for (i, b) in clean.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        clean[0] = 0x47;
        clean[TS_PACKET] = 0x47;
        clean[2 * TS_PACKET] = 0x47;
        assert_eq!(decloak(&clean).len(), clean.len());
        assert_eq!(decloak(&clean)[0], 0x47);

        // No sync triple anywhere (fMP4-ish) → pass through, do not corrupt.
        let fmp4 = b"\x00\x00\x00\x18ftypmp42".repeat(8);
        assert_eq!(decloak(&fmp4), fmp4.as_slice());
    }

    #[test]
    fn decloak_passes_through_when_sync_sits_past_the_scan_window() {
        // Sync at offset 5000 (> MAX_PREFIX_SCAN): returned unchanged, never a
        // wrong-offset strip. Documents the ceiling as a known pass-through.
        let mut buf = vec![0u8; 5000 + 3 * TS_PACKET];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        buf[0] = 0x89;
        buf[5000] = 0x47;
        buf[5000 + TS_PACKET] = 0x47;
        buf[5000 + 2 * TS_PACKET] = 0x47;
        assert_eq!(decloak(&buf).len(), buf.len());
        assert_eq!(decloak(&buf)[0], 0x89);
    }

    #[test]
    fn url_bytes_clean_rejects_crlf_nul_and_nonprintables() {
        assert!(url_bytes_clean(
            "https://cdn.example/x/master.m3u8?sig=ab-cd_ef.gh"
        ));
        assert!(!url_bytes_clean(
            "https://cdn.example/x\r\nX-Injected: evil"
        ));
        assert!(!url_bytes_clean("https://cdn.example/x\ny"));
        assert!(!url_bytes_clean("https://cdn.example/x\x00y"));
        assert!(!url_bytes_clean("https://cdn.example/a b"));
        assert!(!url_bytes_clean("https://cdn.example/\x7f"));
        assert!(!url_bytes_clean(""));
    }

    #[test]
    fn build_loopback_url_encodes_upstream_into_a_decodable_ref() {
        let up = "https://cdn.example/seg/000.ts?sig=xyz";
        let lb = build_loopback_url(3210, up);
        assert!(lb.starts_with("http://127.0.0.1:3210/r.ts?u="));
        // Only the synthetic path extension; no literal dot from the upstream.
        assert_eq!(lb.matches(".ts").count(), 1);
        let enc = lb.strip_prefix("http://127.0.0.1:3210/r.ts?u=").unwrap();
        assert_eq!(percent_decode(enc).unwrap(), up.as_bytes());
    }

    #[test]
    fn rewrite_playlist_repoints_variants_segments_and_uri_tags() {
        let base = "https://cdn.nekostream.site/hls/master.m3u8";
        let master = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,URI=\"audio/en.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=842x480\n\
            480/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1920x1080\n\
            https://other.cdn/1080/index.m3u8\n";
        let out = rewrite_playlist(master, base, 45678);

        assert!(out.starts_with("#EXTM3U\n"));
        assert!(out.contains("http://127.0.0.1:45678/r.ts?u="));
        // The absolute variant is encoded (no bare https:// left on a URI line).
        assert!(!out.contains("\nhttps://other.cdn/1080"));
        // The audio rendition URI attribute was rewritten in place.
        assert!(out.contains("#EXT-X-MEDIA:TYPE=AUDIO,URI=\"http://127.0.0.1:45678/r.ts?u="));
        // Every rewritten target decodes back to a real upstream url.
        for line in out.split('\n') {
            let Some(at) = line.find("/r.ts?u=") else {
                continue;
            };
            let mut enc = &line[at + "/r.ts?u=".len()..];
            if let Some(q) = enc.find('"') {
                enc = &enc[..q];
            }
            let decoded = percent_decode(enc).unwrap();
            assert!(
                std::str::from_utf8(&decoded)
                    .unwrap()
                    .starts_with("https://")
            );
        }
    }

    #[test]
    fn fetch_upstream_refuses_a_private_ip_before_any_request() {
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        // No network: the SSRF guard rejects loopback/metadata before send.
        for blocked in [
            "http://127.0.0.1/x",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/x",
        ] {
            assert!(matches!(
                fetch_upstream(&http, blocked, None, None),
                Err(FetchError::Blocked)
            ));
        }
    }

    #[test]
    fn fetch_upstream_rejects_header_injection_bytes_before_any_request() {
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        assert!(matches!(
            fetch_upstream(&http, "https://cdn.example/x\r\nEvil: 1", None, None),
            Err(FetchError::BadUrl)
        ));
    }

    #[test]
    fn proxy_lifecycle_stop_wakes_accept_and_does_not_hang() {
        let link = StreamLink {
            url: "https://example.invalid/master.m3u8".into(),
            decloak_segments: true,
            ..Default::default()
        };
        let guard = engage(&link).unwrap();
        assert!(guard.url().starts_with("http://127.0.0.1:"));

        // Drive one request to an unknown path: exercises accept + the server
        // parse/response path with no upstream fetch (no network). A 404 closes
        // the connection. stop() (via Drop) must self-dial to unblock accept and
        // join; a regression here hangs under the test timeout.
        let host = guard
            .url()
            .strip_prefix("http://")
            .and_then(|r| r.split('/').next())
            .unwrap()
            .to_string();
        let mut client = TcpStream::connect(&host).unwrap();
        client
            .write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut buf = [0u8; 64];
        let n = client.read(&mut buf).unwrap();
        assert!(
            std::str::from_utf8(&buf[..n]).unwrap().contains("404"),
            "expected a 404 on an unknown path"
        );
        drop(client);
        drop(guard); // must return, not hang
    }

    #[test]
    fn respond_dispatches_playlist_to_rewrite_and_segment_to_decloak() {
        // Playlist: every URI re-pointed to loopback, m3u8 type, keep-alive.
        let mut out = Vec::new();
        let ka = respond(
            &mut out,
            b"#EXTM3U\n480/index.m3u8\n",
            "https://cdn.example/hls/master.m3u8",
            4444,
        )
        .unwrap();
        assert!(matches!(ka, KeepAlive::Yes));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Content-Type: application/vnd.apple.mpegurl"));
        assert!(text.contains("Connection: keep-alive"));
        assert!(text.contains("http://127.0.0.1:4444/r.ts?u="));
        assert!(!text.contains("\n480/index.m3u8")); // relative variant rewritten

        // Segment: decoy prefix stripped to the TS sync, mp2t type, exact length.
        let mut seg = vec![0u8; 70 + 3 * TS_PACKET];
        seg[0] = 0x89;
        seg[70] = 0x47;
        seg[70 + TS_PACKET] = 0x47;
        seg[70 + 2 * TS_PACKET] = 0x47;
        let mut out2 = Vec::new();
        respond(&mut out2, &seg, "https://cdn.example/seg/0.ts", 4444).unwrap();
        let head_end = out2.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let (head, sent_body) = out2.split_at(head_end);
        let head = std::str::from_utf8(head).unwrap();
        assert!(head.contains("Content-Type: video/mp2t"));
        assert!(head.contains(&format!("Content-Length: {}", 3 * TS_PACKET)));
        assert_eq!(sent_body.len(), 3 * TS_PACKET);
        assert_eq!(sent_body[0], 0x47);
    }

    #[test]
    fn read_request_target_reads_sequential_requests_on_one_connection() {
        // Two pipelined requests off one buffer prove the keep-alive read loop
        // yields both targets in order, then None at EOF.
        let raw: &[u8] = b"GET /r.ts?u=abc HTTP/1.1\r\nHost: x\r\n\r\nGET /second HTTP/1.1\r\n\r\n";
        let mut reader = BufReader::new(raw);
        assert_eq!(
            read_request_target(&mut reader).unwrap().as_deref(),
            Some("/r.ts?u=abc")
        );
        assert_eq!(
            read_request_target(&mut reader).unwrap().as_deref(),
            Some("/second")
        );
        assert_eq!(read_request_target(&mut reader).unwrap(), None);
    }

    #[test]
    fn read_request_target_cap_terminates_on_an_infinite_stream() {
        // io::repeat never EOFs and sends no newline, so without the
        // MAX_HEAD_BYTES Take cap read_line would grow its buffer without bound.
        // The cap makes the read terminate. Remove the cap and this test hangs
        // instead of passing: that hang IS the proof the bound does work (a
        // finite in-memory reader can't demonstrate unboundedness).
        let mut reader = BufReader::new(std::io::repeat(b'a'));
        // Terminates and yields a (bounded, spaceless -> empty) target.
        assert_eq!(
            read_request_target(&mut reader).unwrap().as_deref(),
            Some("")
        );
    }
}
