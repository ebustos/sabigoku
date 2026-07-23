//! 128-bit hex nonces from the OS CSPRNG. Reading `/dev/urandom` keeps this
//! std-only (sabigoku is Unix; Windows unsupported at freeze, 06 §1).

use std::io::{self, Read};

/// 32 lowercase hex chars (128 bits).
pub fn mint() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom")?;
    f.read_exact(&mut bytes)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    Ok(out)
}
