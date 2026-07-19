//! Url-keyed disk cover cache (zigoku ROD-171): disk before network, so cold
//! starts do not refetch the world. Best-effort everywhere; any failure is a
//! miss and the pipeline refetches.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use super::MAX_ENCODED_BYTES;

static TMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// hex-16 of the first 8 SHA-256 bytes of `url`; a collision costs one refetch.
fn stem(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn cover_path(dir: &Path, url: &str) -> PathBuf {
    dir.join(format!("{}.jpg", stem(url)))
}

/// Encoded body for `url`, or None on any miss. A body past the 8 MiB
/// admission bound reads as a miss, never as a truncated body.
pub fn read(dir: &Path, url: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(cover_path(dir, url)).ok()?;
    let mut body = Vec::new();
    file.take(MAX_ENCODED_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .ok()?;
    (body.len() <= MAX_ENCODED_BYTES).then_some(body)
}

/// Per-writer unique temp then atomic rename: concurrent writers of the same
/// url on a shared `.tmp` would tear the file (04 §7.3, zigoku ROD-243).
pub fn write(dir: &Path, url: &str, body: &[u8]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = cover_path(dir, url);
    let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("jpg.{}.{nonce}.tmp", std::process::id()));
    if std::fs::write(&tmp, body).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("sabigoku-covers-disk-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn roundtrip_and_miss() {
        let dir = test_dir("roundtrip");
        let url = "https://cdn.example/a.png";
        assert_eq!(read(&dir, url), None);
        write(&dir, url, b"body-bytes");
        assert_eq!(read(&dir, url).unwrap(), b"body-bytes");
        assert_eq!(read(&dir, "https://cdn.example/other.png"), None);
    }

    #[test]
    fn stem_is_stable_hex16_and_urls_do_not_collide() {
        let s = stem("https://cdn.example/a.png");
        assert_eq!(s.len(), 16);
        assert!(s.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(s, stem("https://cdn.example/a.png"));
        assert_ne!(s, stem("https://cdn.example/b.png"));
    }

    #[test]
    fn write_leaves_no_temp_droppings() {
        let dir = test_dir("no-temps");
        write(&dir, "https://cdn.example/a.png", b"x");
        write(&dir, "https://cdn.example/a.png", b"y");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert_eq!(read(&dir, "https://cdn.example/a.png").unwrap(), b"y");
    }

    #[test]
    fn oversize_file_reads_as_miss_not_truncation() {
        let dir = test_dir("oversize");
        let url = "https://cdn.example/big.png";
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cover_path(&dir, url), vec![0u8; MAX_ENCODED_BYTES + 1]).unwrap();
        assert_eq!(read(&dir, url), None);
    }
}
