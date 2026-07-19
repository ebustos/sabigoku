//! Byte-bounded LRU cover caches (04 §7.3): raw encoded bodies and decoded
//! pixels, url-keyed, get promotes. zigoku's ROD-243 clone-under-lock rule is
//! structural here: values move in and independent clones move out within one
//! `Mutex` section, so no caller ever holds a cache-owned buffer.

use std::collections::HashMap;
use std::sync::Mutex;

use image::DynamicImage;

/// Freeze caps (zigoku `workers.zig`); tune in M1 if measured.
pub const RAW_CACHE_BYTES: usize = 32 * 1024 * 1024;
pub const DECODED_CACHE_BYTES: usize = 48 * 1024 * 1024;

pub trait ByteSized {
    fn byte_len(&self) -> usize;
}

impl ByteSized for Vec<u8> {
    fn byte_len(&self) -> usize {
        self.len()
    }
}

impl ByteSized for DynamicImage {
    fn byte_len(&self) -> usize {
        self.as_bytes().len()
    }
}

/// LRU bounded by payload bytes, not entry count. An entry larger than the
/// whole cap is declined outright; the caller keeps its copy.
#[derive(Debug)]
pub struct ByteLru<V> {
    cap: usize,
    total: usize,
    stamp: u64,
    entries: HashMap<String, Entry<V>>,
}

#[derive(Debug)]
struct Entry<V> {
    stamp: u64,
    value: V,
}

impl<V: ByteSized> ByteLru<V> {
    pub fn new(cap: usize) -> ByteLru<V> {
        ByteLru {
            cap,
            total: 0,
            stamp: 0,
            entries: HashMap::new(),
        }
    }

    /// A hit promotes: get is a recency writer.
    pub fn get(&mut self, key: &str) -> Option<&V> {
        self.stamp += 1;
        let stamp = self.stamp;
        let entry = self.entries.get_mut(key)?;
        entry.stamp = stamp;
        Some(&entry.value)
    }

    /// Evicts oldest entries until the value fits; false means declined.
    pub fn insert(&mut self, key: &str, value: V) -> bool {
        let bytes = value.byte_len();
        if bytes > self.cap {
            return false;
        }
        if let Some(old) = self.entries.remove(key) {
            self.total -= old.value.byte_len();
        }
        while self.total + bytes > self.cap {
            if !self.evict_oldest() {
                break;
            }
        }
        self.stamp += 1;
        self.total += bytes;
        self.entries.insert(
            key.to_string(),
            Entry {
                stamp: self.stamp,
                value,
            },
        );
        true
    }

    fn evict_oldest(&mut self) -> bool {
        // O(n) scan; the byte caps keep n in the dozens.
        let Some(key) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.stamp)
            .map(|(k, _)| k.clone())
        else {
            return false;
        };
        if let Some(old) = self.entries.remove(&key) {
            self.total -= old.value.byte_len();
        }
        true
    }

    pub fn total_bytes(&self) -> usize {
        self.total
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The two in-memory tiers behind one lock (04 §7.3).
#[derive(Debug)]
pub struct CoverCaches {
    inner: Mutex<Tiers>,
}

#[derive(Debug)]
struct Tiers {
    raw: ByteLru<Vec<u8>>,
    decoded: ByteLru<DynamicImage>,
}

impl CoverCaches {
    pub fn new() -> CoverCaches {
        CoverCaches::with_caps(RAW_CACHE_BYTES, DECODED_CACHE_BYTES)
    }

    pub fn with_caps(raw: usize, decoded: usize) -> CoverCaches {
        CoverCaches {
            inner: Mutex::new(Tiers {
                raw: ByteLru::new(raw),
                decoded: ByteLru::new(decoded),
            }),
        }
    }

    pub fn decoded_hit(&self, url: &str) -> Option<DynamicImage> {
        self.inner.lock().unwrap().decoded.get(url).cloned()
    }

    pub fn raw_hit(&self, url: &str) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().raw.get(url).cloned()
    }

    pub fn insert_raw(&self, url: &str, body: Vec<u8>) {
        self.inner.lock().unwrap().raw.insert(url, body);
    }

    pub fn insert_decoded(&self, url: &str, img: &DynamicImage) {
        self.inner.lock().unwrap().decoded.insert(url, img.clone());
    }
}

impl Default for CoverCaches {
    fn default() -> Self {
        CoverCaches::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(n: usize, fill: u8) -> Vec<u8> {
        vec![fill; n]
    }

    #[test]
    fn get_promotes_so_eviction_takes_the_cold_entry() {
        let mut lru = ByteLru::new(10);
        assert!(lru.insert("a", body(4, 1)));
        assert!(lru.insert("b", body(4, 2)));
        assert!(lru.get("a").is_some());
        assert!(lru.insert("c", body(4, 3)));
        assert!(lru.get("b").is_none(), "cold entry must go first");
        assert!(lru.get("a").is_some());
        assert!(lru.get("c").is_some());
        assert_eq!(lru.total_bytes(), 8);
    }

    #[test]
    fn oversize_is_declined_and_nothing_changes() {
        let mut lru = ByteLru::new(10);
        assert!(lru.insert("a", body(4, 1)));
        assert!(!lru.insert("big", body(11, 9)));
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.total_bytes(), 4);
        assert!(lru.get("a").is_some());
    }

    #[test]
    fn replacing_a_key_settles_the_byte_accounting() {
        let mut lru = ByteLru::new(10);
        assert!(lru.insert("a", body(4, 1)));
        assert!(lru.insert("a", body(6, 2)));
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.total_bytes(), 6);
        assert_eq!(lru.get("a").unwrap(), &body(6, 2));
    }

    #[test]
    fn eviction_frees_until_it_fits() {
        let mut lru = ByteLru::new(10);
        assert!(lru.insert("a", body(4, 1)));
        assert!(lru.insert("b", body(4, 2)));
        assert!(lru.insert("c", body(9, 3)));
        assert_eq!(lru.len(), 1);
        assert!(lru.get("c").is_some());
        assert_eq!(lru.total_bytes(), 9);
    }

    #[test]
    fn cover_caches_hand_out_independent_copies() {
        let caches = CoverCaches::with_caps(64, 64);
        caches.insert_raw("u", body(4, 7));
        let copy = caches.raw_hit("u").unwrap();
        assert_eq!(copy, body(4, 7));
        assert!(caches.raw_hit("miss").is_none());
        let img = DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2));
        caches.insert_decoded("u", &img);
        assert_eq!(caches.decoded_hit("u").unwrap(), img);
        assert!(caches.decoded_hit("miss").is_none());
    }

    #[test]
    fn decoded_lru_accounts_pixel_bytes() {
        // 2x2 RGBA is 16 bytes; a 15-byte cap declines it.
        let img = DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2));
        let mut lru: ByteLru<DynamicImage> = ByteLru::new(15);
        assert!(!lru.insert("u", img.clone()));
        let mut lru: ByteLru<DynamicImage> = ByteLru::new(16);
        assert!(lru.insert("u", img));
        assert_eq!(lru.total_bytes(), 16);
    }
}
