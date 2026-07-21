//! OAuth loopback callback server (06 §4.4). Imports: login, std net. Binds
//! 127.0.0.1:8766, IPv4 only (deliberate: a pure-`::1` host reaches it via
//! Happy Eyeballs). Serves the relay that turns the Implicit-grant
//! `location.hash` into a /callback query, checks the CSRF state, and runs
//! `complete_login`.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::Error;
use crate::login::{ConnectResult, LOOPBACK_PORT, Verifier, authorize_url, complete_login, param};

/// Per-connection read deadline (06 §4.4): a stalled socket must not wedge
/// accept. There is deliberately no overall timeout on the wait.
const READ_DEADLINE: Duration = Duration::from_secs(5);

/// The top-bar wordmark (DESIGN 3.4), above the headline on every callback page.
const WORDMARK: &str = "錆獄 sabigoku";

/// Shared `<style>` for all three callback pages (DESIGN 5.5a): theme-aware via
/// `prefers-color-scheme`, dark reusing §1.1's terminal_ghost hex. One constant
/// so a contrast fix can't drift between pages. Self-contained: no external
/// fonts/images (served off a bare loopback listener).
const PAGE_STYLE: &str = "<style>:root{--bg:#fff;--card:#f4f6f4;--hair:#d7ddd7;--fg:#0b3d1e;--muted:#5a6b5f;--accent:#0b7a3b}@media(prefers-color-scheme:dark){:root{--bg:#020d06;--card:#0b1f18;--hair:#1a4030;--fg:#39ff6a;--muted:#2a6040;--accent:#20ffdd}}*{box-sizing:border-box}body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;background:var(--bg);color:var(--fg);font-family:ui-monospace,SFMono-Regular,Menlo,monospace}.card{background:var(--card);border:1px solid var(--hair);border-radius:10px;padding:32px 40px;text-align:center;max-width:360px}.mark{color:var(--muted);font-size:13px;letter-spacing:.08em;margin-bottom:18px}.headline{font-size:18px;margin:0 0 8px}.sub{color:var(--muted);font-size:14px;margin:0}.cursor{color:var(--accent);animation:blink 1s steps(1,end) infinite}@keyframes blink{50%{opacity:0}}</style>";

/// First landing: the token is in `location.hash`, invisible to the server, so
/// this relay copies it into a /callback query. No address-bar scrub here (its
/// only job is the redirect). Byte-critical (06 §4.4): do not reformat or
/// rewrap the `location.replace` line.
fn relay_page() -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>sabigoku</title>{PAGE_STYLE}</head>\
         <body><div class=\"card\"><div class=\"mark\">{WORDMARK}</div>\
         <p class=\"headline\">finishing sign-in <span class=\"cursor\">▌</span></p></div>\
         <script>location.replace(\"/callback?\" + location.hash.substring(1));</script></body></html>"
    )
}

pub struct Loopback {
    listener: TcpListener,
    nonce: String,
    port: u16,
    cancel: Arc<AtomicBool>,
}

/// Handle to cancel a blocked [`Loopback::serve`] from another thread.
pub struct Canceler {
    port: u16,
    cancel: Arc<AtomicBool>,
}

impl Canceler {
    /// Set the flag, then dial the port once to wake a blocked `accept`
    /// (06 §4.4). `serve` sees the flag and returns `Canceled`.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
    }
}

impl Loopback {
    /// Bind the registered redirect port and mint the CSRF nonce.
    pub fn start() -> Result<Loopback, Error> {
        Self::bind_port(LOOPBACK_PORT)
    }

    fn bind_port(port: u16) -> Result<Loopback, Error> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .map_err(|e| Error::io(format!("127.0.0.1:{port}"), e))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::io("loopback addr", e))?
            .port();
        Ok(Loopback {
            listener,
            nonce: mint_nonce()?,
            port,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn authorize_url(&self) -> String {
        authorize_url(&self.nonce)
    }

    pub fn canceler(&self) -> Canceler {
        Canceler {
            port: self.port,
            cancel: Arc::clone(&self.cancel),
        }
    }

    /// Block until a callback completes login, the user cancels, or accept dies.
    /// A relay hit is not terminal: keep waiting for the /callback.
    pub fn serve<V: Verifier>(&self, verifier: &V, auth_path: &Path, now: i64) -> ConnectResult {
        loop {
            let stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(_) => return ConnectResult::NetworkError,
            };
            if self.cancel.load(Ordering::Acquire) {
                return ConnectResult::Canceled;
            }
            if let Some(result) = self.handle(stream, verifier, auth_path, now) {
                return result;
            }
        }
    }

    fn handle<V: Verifier>(
        &self,
        mut stream: TcpStream,
        verifier: &V,
        auth_path: &Path,
        now: i64,
    ) -> Option<ConnectResult> {
        let _ = stream.set_read_timeout(Some(READ_DEADLINE));
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).ok()?;
        let req = String::from_utf8_lossy(&buf[..n]);
        let target = request_target(&req)?;
        if let Some(query) = target.strip_prefix("/callback?") {
            let result = self.complete_from_query(query, verifier, auth_path, now);
            let _ = write_html(&mut stream, &result_page(&result));
            Some(result)
        } else {
            let _ = write_html(&mut stream, &relay_page());
            None
        }
    }

    fn complete_from_query<V: Verifier>(
        &self,
        query: &str,
        verifier: &V,
        auth_path: &Path,
        now: i64,
    ) -> ConnectResult {
        // CSRF: the state must match the minted nonce; never verify/persist otherwise.
        if param(query, "state").as_deref() != Some(self.nonce.as_str()) {
            return ConnectResult::BadState;
        }
        complete_login(query, verifier, auth_path, now)
    }
}

fn request_target(req: &str) -> Option<&str> {
    req.lines().next()?.split(' ').nth(1)
}

fn write_html(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes())
}

/// The success or failure page. The browser only ever shows the generic
/// outcome; the specific reason (rejected, no token, save failed) surfaces in
/// the terminal, not the tab.
fn result_page(result: &ConnectResult) -> String {
    match result {
        ConnectResult::Ok { .. } => scrubbed_page("✓ signed in to AniList", "you can close this tab"),
        _ => scrubbed_page("sign-in didn't complete", "check your terminal"),
    }
}

/// A done/fail page: the address-bar scrub (06 §4.4) is the first thing in
/// `<head>`, before the style or body parse, so the token clears as early as
/// the page can manage.
fn scrubbed_page(headline: &str, sub: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <script>history.replaceState(null,'','/')</script><title>sabigoku</title>{PAGE_STYLE}</head>\
         <body><div class=\"card\"><div class=\"mark\">{WORDMARK}</div>\
         <p class=\"headline\">{headline}</p><p class=\"sub\">{sub}</p></div></body></html>"
    )
}

/// 128-bit CSRF nonce from the OS CSPRNG. Reading `/dev/urandom` keeps this
/// std-only (sabigoku is Unix; Windows unsupported at freeze, 06 §1).
fn mint_nonce() -> Result<String, Error> {
    let mut bytes = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| Error::io("/dev/urandom", e))?;
    f.read_exact(&mut bytes)
        .map_err(|e| Error::io("/dev/urandom", e))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anilist::Viewer;
    use crate::auth::Auth;
    use crate::login::Verifier;
    use crate::providers::CatalogError;

    struct FakeV;
    impl Verifier for FakeV {
        fn verify(&self, _token: &str) -> Result<Option<Viewer>, CatalogError> {
            Ok(Some(Viewer { id: 7, name: "rod".into() }))
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("sabigoku-loopback-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    const TOKEN: &str = "abcdefghijklmnopqrstuvwxyz012345";

    fn read_response(mut stream: TcpStream) -> String {
        let mut s = String::new();
        let _ = stream.read_to_string(&mut s);
        s
    }

    #[test]
    fn relay_then_callback_completes_login() {
        let path = tmp("e2e.toml");
        let _ = std::fs::remove_file(&path);
        let lp = Loopback::bind_port(0).unwrap();
        let port = lp.port();
        // Pull the minted nonce out of the authorize URL.
        let url = lp.authorize_url();
        let nonce = param(&url, "state").unwrap();
        let verifier = FakeV;

        let result = std::thread::scope(|s| {
            let h = s.spawn(|| lp.serve(&verifier, &path, 1000));

            // First hit: any path -> relay HTML.
            let mut c1 = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c1.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let relay = read_response(c1);
            assert!(
                relay.contains("location.hash.substring(1)"),
                "relay script missing: {relay}"
            );

            // Callback carrying the token + matching state.
            let mut c2 = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c2.write_all(
                format!("GET /callback?access_token={TOKEN}&expires_in=3600&state={nonce} HTTP/1.1\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
            let done = read_response(c2);
            assert!(done.contains("signed in to AniList"));
            assert!(done.contains("history.replaceState"), "address bar not scrubbed");
            assert!(done.contains(WORDMARK), "wordmark missing");

            h.join().unwrap()
        });

        assert_eq!(result, ConnectResult::Ok { user_name: "rod".into() });
        assert_eq!(Auth::load(&path).anilist.bearer(), Some(TOKEN));
    }

    #[test]
    fn callback_with_wrong_state_is_bad_state_and_writes_nothing() {
        let path = tmp("badstate.toml");
        let _ = std::fs::remove_file(&path);
        let lp = Loopback::bind_port(0).unwrap();
        let port = lp.port();
        let verifier = FakeV;

        let result = std::thread::scope(|s| {
            let h = s.spawn(|| lp.serve(&verifier, &path, 0));
            let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
            c.write_all(
                format!("GET /callback?access_token={TOKEN}&state=forged HTTP/1.1\r\n\r\n").as_bytes(),
            )
            .unwrap();
            let _ = read_response(c);
            h.join().unwrap()
        });

        assert_eq!(result, ConnectResult::BadState);
        assert!(!path.exists(), "a forged state must never verify or persist");
    }

    #[test]
    fn relay_redirects_without_scrub_results_scrub_first() {
        let relay = relay_page();
        assert!(relay.contains("location.hash.substring(1)"), "relay redirect line");
        assert!(
            !relay.contains("history.replaceState"),
            "relay navigates away; it must not carry the scrub"
        );
        let fail = result_page(&ConnectResult::Rejected);
        assert!(fail.contains("sign-in didn't complete"));
        assert!(
            fail.starts_with(
                "<!doctype html><html><head><meta charset=\"utf-8\"><script>history.replaceState"
            ),
            "the scrub must be the first thing in <head>"
        );
    }

    #[test]
    fn cancel_wakes_a_blocked_serve() {
        let path = tmp("cancel.toml");
        let lp = Loopback::bind_port(0).unwrap();
        let canceler = lp.canceler();
        let verifier = FakeV;

        let result = std::thread::scope(|s| {
            let h = s.spawn(|| lp.serve(&verifier, &path, 0));
            canceler.cancel();
            h.join().unwrap()
        });

        assert_eq!(result, ConnectResult::Canceled);
    }
}
