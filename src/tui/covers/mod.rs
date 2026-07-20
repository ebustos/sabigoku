//! Cover pipeline (04 §7.3, 05 §12): url to owned pixels through decoded LRU,
//! raw LRU, disk, then guarded network, warming every layer on the way back.
//! `url` is the cache key at every layer; only the network branch resolves
//! through `cover_request`, so CDN host rotation does not bust cache (zigoku
//! ROD-267). Blocking: worker threads only (04 §1). State machines (detail
//! decision table, discover pump) and event wiring land in later ROD-438
//! chunks.

pub mod cache;
pub mod detail;
pub mod discover;
pub mod disk;
pub mod render;
pub mod sizing;

use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use image::DynamicImage;

pub use cache::CoverCaches;

use crate::fetchguard::guard_fetch_url;
use crate::providers::{CoverRequest, StreamProvider};

/// Admission bound on one encoded body (zigoku freeze), enforced at fetch and
/// disk read alike.
pub const MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;
/// Decode footprint cap per side (zigoku ROD-270): full RGBA goes to the
/// terminal with no pre-downscale, so this is the RAM rail. 2560 holds margin
/// over the largest cover seen at freeze (~1635x2247).
pub const MAX_COVER_DIMENSION: u32 = 2560;
/// Same id+url (detail) or same url (discover) failure suppress window
/// (zigoku ROD-110, 04 §8); a url change clears immediately.
pub const RETRY_COOLDOWN: Duration = Duration::from_secs(10);
/// Protocol-pool key for the single detail cover; grid slots key by url, and
/// urls are absolute so they can never collide with this.
pub const DETAIL_KEY: &str = "detail";
const FETCH_DEADLINE: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoverError {
    /// Ref unusable or blocked by the fetch guard; skip, never sanitize.
    #[error("bad or blocked cover ref")]
    BadRef,
    #[error("cover fetch failed")]
    Fetch,
    #[error("cover decode failed")]
    Decode,
}

/// `provider` resolves stored refs into absolute requests; None is the
/// AniList/absolute-url path.
pub fn load_cover_pixels(
    provider: Option<&dyn StreamProvider>,
    url: &str,
    caches: &CoverCaches,
    covers_dir: &Path,
) -> Result<DynamicImage, CoverError> {
    load_with(provider, url, caches, covers_dir, &fetch_cover_body)
}

fn load_with(
    provider: Option<&dyn StreamProvider>,
    url: &str,
    caches: &CoverCaches,
    covers_dir: &Path,
    fetch: &dyn Fn(&CoverRequest) -> Result<Vec<u8>, CoverError>,
) -> Result<DynamicImage, CoverError> {
    if let Some(img) = caches.decoded_hit(url) {
        return Ok(img);
    }
    if let Some(raw) = caches.raw_hit(url) {
        let img = decode_cover(&raw)?;
        caches.insert_decoded(url, &img);
        return Ok(img);
    }
    if let Some(body) = disk::read(covers_dir, url) {
        // Corrupt or truncated on disk: fall through and refetch.
        if let Ok(img) = decode_cover(&body) {
            caches.insert_raw(url, body);
            caches.insert_decoded(url, &img);
            return Ok(img);
        }
    }
    let req = match provider {
        Some(p) => p.cover_request(url).map_err(|_| CoverError::BadRef)?,
        None => CoverRequest {
            url: url.to_string(),
            referer: None,
            user_agent: None,
        },
    };
    guard_fetch_url(&req.url).map_err(|_| CoverError::BadRef)?;
    let body = fetch(&req)?;
    let img = decode_cover(&body)?;
    disk::write(covers_dir, url, &body);
    caches.insert_raw(url, body);
    caches.insert_decoded(url, &img);
    Ok(img)
}

/// Dimensions come from the header probe BEFORE the pixel decode, so a
/// decode bomb is rejected without allocating its pixels.
fn decode_cover(body: &[u8]) -> Result<DynamicImage, CoverError> {
    let (w, h) = image::ImageReader::new(Cursor::new(body))
        .with_guessed_format()
        .map_err(|_| CoverError::Decode)?
        .into_dimensions()
        .map_err(|_| CoverError::Decode)?;
    if w == 0 || h == 0 || w > MAX_COVER_DIMENSION || h > MAX_COVER_DIMENSION {
        return Err(CoverError::Decode);
    }
    let img = image::ImageReader::new(Cursor::new(body))
        .with_guessed_format()
        .map_err(|_| CoverError::Decode)?
        .decode()
        .map_err(|_| CoverError::Decode)?;
    Ok(DynamicImage::ImageRgba8(img.to_rgba8()))
}

fn fetch_cover_body(req: &CoverRequest) -> Result<Vec<u8>, CoverError> {
    // One process-wide client: rebuilding per fetch would redo TLS setup for
    // every cover in a pump burst. Redirects refused; a followed 3xx would
    // re-enter unguarded (03 §6.7).
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    let client = CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(FETCH_DEADLINE)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
        .ok_or(CoverError::Fetch)?;
    let mut builder = client.get(&req.url);
    if let Some(referer) = &req.referer {
        builder = builder.header("Referer", referer);
    }
    if let Some(user_agent) = &req.user_agent {
        builder = builder.header("User-Agent", user_agent);
    }
    let resp = builder.send().map_err(|_| CoverError::Fetch)?;
    if !resp.status().is_success() {
        return Err(CoverError::Fetch);
    }
    let mut body = Vec::new();
    resp.take(MAX_ENCODED_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| CoverError::Fetch)?;
    if body.len() > MAX_ENCODED_BYTES {
        return Err(CoverError::Fetch);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Enrichment, Quality, StreamLink, Translation};
    use crate::providers::{ProviderError, SearchHit, SearchOptions, StreamProvider};
    use crate::testutil::{response_with_body, serve_once};
    use std::cell::Cell;
    use std::path::PathBuf;

    const URL: &str = "https://cdn.example/cover.png";

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("sabigoku-covers-pipeline-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([120, 40, 200, 255]));
        let mut buf = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// Fetch seam that counts calls and serves `body`.
    struct CountingFetch {
        calls: Cell<u32>,
        body: Vec<u8>,
    }

    impl CountingFetch {
        fn new(body: Vec<u8>) -> CountingFetch {
            CountingFetch {
                calls: Cell::new(0),
                body,
            }
        }

        fn load(
            &self,
            provider: Option<&dyn StreamProvider>,
            url: &str,
            caches: &CoverCaches,
            dir: &Path,
        ) -> Result<DynamicImage, CoverError> {
            load_with(provider, url, caches, dir, &|_req: &CoverRequest| {
                self.calls.set(self.calls.get() + 1);
                Ok(self.body.clone())
            })
        }
    }

    #[test]
    fn decoded_hit_short_circuits() {
        let dir = test_dir("decoded-hit");
        let caches = CoverCaches::new();
        let img = decode_cover(&png_bytes(4, 6)).unwrap();
        caches.insert_decoded(URL, &img);
        let fetch = CountingFetch::new(Vec::new());
        assert_eq!(fetch.load(None, URL, &caches, &dir).unwrap(), img);
        assert_eq!(fetch.calls.get(), 0);
    }

    #[test]
    fn raw_hit_decodes_and_warms_decoded() {
        let dir = test_dir("raw-hit");
        let caches = CoverCaches::new();
        caches.insert_raw(URL, png_bytes(4, 6));
        let fetch = CountingFetch::new(Vec::new());
        let img = fetch.load(None, URL, &caches, &dir).unwrap();
        assert_eq!(fetch.calls.get(), 0);
        assert_eq!(caches.decoded_hit(URL).unwrap(), img);
    }

    #[test]
    fn disk_hit_warms_both_memory_tiers() {
        let dir = test_dir("disk-hit");
        let caches = CoverCaches::new();
        let body = png_bytes(4, 6);
        disk::write(&dir, URL, &body);
        let fetch = CountingFetch::new(Vec::new());
        let img = fetch.load(None, URL, &caches, &dir).unwrap();
        assert_eq!(fetch.calls.get(), 0);
        assert_eq!(caches.raw_hit(URL).unwrap(), body);
        assert_eq!(caches.decoded_hit(URL).unwrap(), img);
    }

    #[test]
    fn corrupt_disk_body_refetches_and_heals() {
        let dir = test_dir("corrupt-disk");
        let caches = CoverCaches::new();
        disk::write(&dir, URL, b"not an image");
        let good = png_bytes(4, 6);
        let fetch = CountingFetch::new(good.clone());
        fetch.load(None, URL, &caches, &dir).unwrap();
        assert_eq!(fetch.calls.get(), 1);
        assert_eq!(disk::read(&dir, URL).unwrap(), good, "disk must heal");
    }

    #[test]
    fn network_success_warms_all_layers_once() {
        let dir = test_dir("network");
        let caches = CoverCaches::new();
        let body = png_bytes(4, 6);
        let fetch = CountingFetch::new(body.clone());
        let img = fetch.load(None, URL, &caches, &dir).unwrap();
        assert_eq!(fetch.calls.get(), 1);
        assert_eq!(caches.raw_hit(URL).unwrap(), body);
        assert_eq!(caches.decoded_hit(URL).unwrap(), img);
        assert_eq!(disk::read(&dir, URL).unwrap(), body);
        fetch.load(None, URL, &caches, &dir).unwrap();
        assert_eq!(fetch.calls.get(), 1, "second load must be a cache hit");
    }

    #[test]
    fn blocked_or_bad_ref_never_reaches_fetch() {
        let dir = test_dir("blocked");
        let caches = CoverCaches::new();
        let fetch = CountingFetch::new(png_bytes(4, 6));
        for bad in ["http://127.0.0.1/x", "not a url", "file:///etc/passwd"] {
            assert_eq!(
                fetch.load(None, bad, &caches, &dir),
                Err(CoverError::BadRef),
                "{bad}"
            );
        }
        assert_eq!(fetch.calls.get(), 0);
    }

    #[test]
    fn decode_bomb_dimensions_rejected() {
        let dir = test_dir("bomb");
        let caches = CoverCaches::new();
        let fetch = CountingFetch::new(png_bytes(MAX_COVER_DIMENSION + 1, 1));
        assert_eq!(
            fetch.load(None, URL, &caches, &dir),
            Err(CoverError::Decode)
        );
        assert!(caches.raw_hit(URL).is_none(), "bomb must not be cached");
        assert_eq!(disk::read(&dir, URL), None);
    }

    /// Provider whose `cover_request` maps refs to an absolute CDN url.
    struct RefProvider;

    impl StreamProvider for RefProvider {
        fn name(&self) -> &'static str {
            "refprov"
        }
        fn display_name(&self) -> &'static str {
            "RefProv"
        }
        fn canonical_key(&self, _show: &Enrichment) -> Option<String> {
            None
        }
        fn search(
            &self,
            _query: &str,
            _opts: &SearchOptions,
        ) -> Result<Vec<SearchHit>, ProviderError> {
            Err(ProviderError::Unsupported)
        }
        fn episodes(
            &self,
            _provider_id: &str,
            _translation: Translation,
            _count_hint: Option<u32>,
        ) -> Result<Vec<String>, ProviderError> {
            Ok(Vec::new())
        }
        fn resolve(
            &self,
            _provider_id: &str,
            _episode: &str,
            _translation: Translation,
            _quality: Quality,
        ) -> Result<StreamLink, ProviderError> {
            Err(ProviderError::Unsupported)
        }
        fn cover_request(&self, cover_ref: &str) -> Result<CoverRequest, ProviderError> {
            if cover_ref == "bad-ref" {
                return Err(ProviderError::Unsupported);
            }
            Ok(CoverRequest {
                url: format!("https://cdn.example{cover_ref}"),
                referer: Some("https://cdn.example/".to_string()),
                user_agent: None,
            })
        }
    }

    #[test]
    fn provider_resolves_ref_only_on_the_network_branch() {
        let dir = test_dir("provider-ref");
        let caches = CoverCaches::new();
        let body = png_bytes(4, 6);
        let seen = Cell::new(String::new());
        let img = load_with(
            Some(&RefProvider),
            "/rel/cover.png",
            &caches,
            &dir,
            &|req: &CoverRequest| {
                seen.set(req.url.clone());
                assert_eq!(req.referer.as_deref(), Some("https://cdn.example/"));
                Ok(body.clone())
            },
        )
        .unwrap();
        assert_eq!(seen.take(), "https://cdn.example/rel/cover.png");
        // The stored ref stays the cache key (zigoku ROD-267).
        assert_eq!(caches.decoded_hit("/rel/cover.png").unwrap(), img);
        assert!(
            caches
                .decoded_hit("https://cdn.example/rel/cover.png")
                .is_none()
        );
    }

    #[test]
    fn provider_refusing_the_ref_is_bad_ref() {
        let dir = test_dir("provider-bad-ref");
        let caches = CoverCaches::new();
        let fetch = CountingFetch::new(png_bytes(4, 6));
        assert_eq!(
            fetch.load(Some(&RefProvider), "bad-ref", &caches, &dir),
            Err(CoverError::BadRef)
        );
        assert_eq!(fetch.calls.get(), 0);
    }

    fn cover_req(url: &str) -> CoverRequest {
        CoverRequest {
            url: url.to_string(),
            referer: None,
            user_agent: None,
        }
    }

    #[test]
    fn http_fetch_returns_ok_body() {
        let body = png_bytes(2, 3);
        let url = serve_once(response_with_body("200 OK", &body));
        assert_eq!(fetch_cover_body(&cover_req(&url)).unwrap(), body);
    }

    #[test]
    fn http_fetch_refuses_redirects_and_errors() {
        let redirect = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let url = serve_once(redirect);
        assert_eq!(fetch_cover_body(&cover_req(&url)), Err(CoverError::Fetch));
        let url = serve_once(response_with_body("404 Not Found", b""));
        assert_eq!(fetch_cover_body(&cover_req(&url)), Err(CoverError::Fetch));
    }

    #[test]
    fn http_fetch_caps_the_body_at_the_admission_bound() {
        let url = serve_once(response_with_body(
            "200 OK",
            &vec![0u8; MAX_ENCODED_BYTES + 1],
        ));
        assert_eq!(fetch_cover_body(&cover_req(&url)), Err(CoverError::Fetch));
    }
}
