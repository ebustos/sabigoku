//! spike_stream: the provider's AES-256-GCM stream-resolution crypto.
//! Parity: zigoku spike #4 (allanime_stream.zig, ROD-62).
//!
//! zigoku's spike POSTs a persisted GraphQL query past Cloudflare and decrypts
//! the returned `tobeparsed` blob. The network half is the same reqwest pattern
//! as spike_http against the same flaky live endpoint, so this spike keeps only
//! the part that's actually RISKY: the crypto. It runs the two golden vectors
//! zigoku pins as offline fixtures, so it's reproducible with zero network.
//!
//! Run: cargo run --example spike_stream

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::{Engine, alphabet};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const GCM_SEED: &[u8] = b"Xot36i3lK3:v1";

// tobeparsed layout: [0] prefix, [1..13] nonce, [13..] ciphertext||tag.
// Key = sha256(GCM_SEED). RustCrypto's AEAD wants the 16-byte tag appended to
// the ciphertext, which is exactly how the blob already lays it out, so there's
// no manual tag split like the Zig version needs.
fn decrypt_tobeparsed(blob: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let key = Sha256::digest(GCM_SEED);
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())?;

    // Indifferent padding: the blob is unpadded standard base64.
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let raw = engine.decode(blob)?;
    if raw.len() < 1 + 12 + 16 {
        return Err("blob too small".into());
    }

    // Types make a swapped nonce/tag a compile error, not silent garbage. GCM
    // still fails closed: a wrong key/nonce/offset returns Err, never plaintext.
    let plain = cipher
        .decrypt(Nonce::from_slice(&raw[1..13]), &raw[13..])
        .map_err(|_| "GCM authentication failed")?;
    Ok(plain)
}

// `--<hex>` provider path: hex pairs XOR 0x38 -> a clock.json API path.
fn decipher_provider_path(hex: &str) -> Result<String, Box<dyn std::error::Error>> {
    if !hex.len().is_multiple_of(2) {
        return Err("odd-length hex".into());
    }
    let bytes: Result<Vec<u8>, _> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map(|b| b ^ 0x38))
        .collect();
    Ok(String::from_utf8(bytes?)?)
}

// The decrypted payload shape, parsed with serde once the bytes are plaintext.
#[derive(Deserialize)]
struct Payload {
    episode: Episode,
}
#[derive(Deserialize)]
struct Episode {
    #[serde(rename = "sourceUrls")]
    source_urls: Vec<Source>,
}
#[derive(Deserialize)]
struct Source {
    #[serde(rename = "sourceName")]
    source_name: String,
    #[serde(rename = "sourceUrl")]
    source_url: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Golden vector 1: the same encrypted blob zigoku pins as an offline fixture.
    let blob = "AAABAgMEBQYHCAkKCw/k3QdUZIc5wIflWKnNrBJlDJDvuoUtnAhztwaZ0MPdc+7QLkxnnkAqseAyPNsmcPKDx4IlVT/nzzS1VVCzmf7KRsutWoKHB/11G9S8i9qBiKecETa/9Yrge8E1Rv/TJ35g7iREfYhMrh8s";
    let plain = decrypt_tobeparsed(blob)?;
    let json = std::str::from_utf8(&plain)?;
    println!("decrypted tobeparsed:\n  {json}\n");

    let payload: Payload = serde_json::from_str(json)?;
    for s in &payload.episode.source_urls {
        println!("  source {:?} -> {}", s.source_name, s.source_url);
    }

    // Golden vector 2: a `--hex` provider path deciphered by XOR 0x38.
    let hex = "175948514e4c4f57175b54575b5307515c056a4d0c405901685b500b486075084c09";
    let path = decipher_provider_path(hex)?;
    println!("\ndeciphered provider path:\n  {path}");

    // Assert against zigoku's pinned expectations so the spike is a real check.
    assert_eq!(
        json,
        r#"{"episode":{"sourceUrls":[{"sourceName":"Default","sourceUrl":"tools.fast4speed.rsvp/x"}]}}"#
    );
    assert_eq!(path, "/apivtwo/clock?id=Ru4xa9Pch3pXM0t1");
    println!("\nboth golden vectors match. crypto verified.");
    Ok(())
}
