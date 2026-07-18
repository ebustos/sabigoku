//! SSRF guard for provider-supplied URLs (03 §6.7): stream, embed, playlist,
//! subtitle probe, cover. Every guarded fetch MUST also refuse redirects; a
//! followed 3xx re-enters unguarded.
//!
//! Known residual (accepted, 03 §6.7): DNS rebinding. A public name resolving
//! to a private IP at connect time passes; reqwest has no
//! resolve-then-validate hook.

use std::net::{Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GuardError {
    /// Unparseable, non-http(s), userinfo, or hostless.
    #[error("bad fetch url")]
    BadUrl,

    #[error("blocked host")]
    BlockedHost,
}

/// `Url::parse` runs the WHATWG host parser, so the denylist sees the same
/// bytes the connector will: percent-encoded hosts arrive decoded
/// (`127%2e0%2e0%2e1`) and alternate IPv4 spellings (`2130706433`,
/// `0x7f.0.0.1`, `127.1`) arrive as canonical `Ipv4Addr`. Tests pin both;
/// `numeric_spelling` backstops the domain arm against parser drift.
pub fn guard_fetch_url(raw: &str) -> Result<(), GuardError> {
    let url = Url::parse(raw).map_err(|_| GuardError::BadUrl)?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(GuardError::BadUrl);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GuardError::BadUrl);
    }
    match url.host() {
        None => Err(GuardError::BadUrl),
        Some(Host::Domain(domain)) => {
            if domain == "localhost" || domain.ends_with(".localhost") {
                return Err(GuardError::BlockedHost);
            }
            if numeric_spelling(domain) {
                return Err(GuardError::BlockedHost);
            }
            Ok(())
        }
        Some(Host::Ipv4(ip)) if private_v4(ip) => Err(GuardError::BlockedHost),
        Some(Host::Ipv6(ip)) if private_v6(ip) => Err(GuardError::BlockedHost),
        Some(_) => Ok(()),
    }
}

/// IPv4 spelling that reached the `Host::Domain` arm instead of parsing as an
/// address. The `url` crate's WHATWG host parser already canonicalizes
/// `2130706433`, `0x7f000001`, `127.1` to `Host::Ipv4` upstream, so this is
/// defense-in-depth against a future parser change, not the live guard (that
/// is the `Host::Ipv4` match). The `0x` prefix arm is the one still reachable
/// today, via malformed hex like `0x7g` that stays a domain.
fn numeric_spelling(domain: &str) -> bool {
    if domain.len() >= 2 && (domain.starts_with("0x") || domain.starts_with("0X")) {
        return true;
    }
    let mut any_label = false;
    for label in domain.split('.') {
        if label.is_empty() {
            continue;
        }
        any_label = true;
        if !label.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    any_label
}

fn private_v4(ip: Ipv4Addr) -> bool {
    let b = ip.octets();
    match b[0] {
        0 | 10 | 127 => true,
        100 => (64..=127).contains(&b[1]),
        169 => b[1] == 254,
        172 => (16..=31).contains(&b[1]),
        192 => b[1] == 168,
        _ => b[0] >= 224,
    }
}

fn private_v6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() {
        return true;
    }
    let s = ip.segments();
    if (s[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    if (s[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Deliberate widen past freeze (ratified ROD-436, backport owed): `to_ipv4`
    // covers BOTH the mapped `::ffff:a.b.c.d` and the legacy IPv4-compatible
    // `::a.b.c.d` forms. zigoku checks only the mapped prefix, so
    // `::169.254.169.254` reaches cloud metadata there; here it recurses into
    // the v4 denylist. The `::/96` range (`::`..`::ff`) also lands in `0/8`,
    // which `private_v4` blocks.
    if let Some(v4) = ip.to_ipv4() {
        return private_v4(v4);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_v4_ranges() {
        for blocked in [
            [0, 1, 2, 3],
            [10, 1, 2, 3],
            [127, 0, 0, 1],
            [100, 64, 0, 1],
            [169, 254, 169, 254],
            [172, 16, 0, 1],
            [192, 168, 1, 1],
            [224, 0, 0, 1],
            [255, 255, 255, 255],
        ] {
            assert!(private_v4(blocked.into()), "{blocked:?} must be private");
        }
        for public in [
            [8, 8, 8, 8],
            [93, 184, 216, 34],
            [172, 32, 0, 1],
            [100, 128, 0, 1],
            [169, 253, 0, 1],
        ] {
            assert!(!private_v4(public.into()), "{public:?} must be public");
        }
    }

    #[test]
    fn private_v6_ranges() {
        assert!(private_v6(Ipv6Addr::UNSPECIFIED));
        assert!(private_v6(Ipv6Addr::LOCALHOST));
        assert!(private_v6("fe80::1".parse().unwrap()));
        assert!(private_v6("fd00::1".parse().unwrap()));
        assert!(private_v6("fc00::1".parse().unwrap()));
        assert!(private_v6("::ffff:127.0.0.1".parse().unwrap()));
        // IPv4-compatible `::a.b.c.d` (widen past freeze, ROD-436): the legacy
        // form, distinct from the `::ffff:` mapped form above.
        assert!(private_v6("::127.0.0.1".parse().unwrap()));
        assert!(private_v6("::169.254.169.254".parse().unwrap()));
        assert!(private_v6("::10.0.0.1".parse().unwrap()));
        assert!(private_v6("::2".parse().unwrap())); // ::/96 lands in 0/8
        assert!(!private_v6("2001:4860:4860::8888".parse().unwrap()));
        assert!(!private_v6("::ffff:8.8.8.8".parse().unwrap()));
        assert!(!private_v6("::8.8.8.8".parse().unwrap())); // compatible-form public
    }

    #[test]
    fn bad_urls_rejected() {
        for bad in [
            "not a url",
            "ftp://host/v.ts",
            "file:///etc/passwd",
            "data:text/plain,hi",
            "https://allanime.day@evil.example/x",
            "https://user:pw@evil.example/x",
            "http://",
        ] {
            assert_eq!(guard_fetch_url(bad), Err(GuardError::BadUrl), "{bad}");
        }
    }

    #[test]
    fn ssrf_vectors_blocked() {
        for blocked in [
            "http://127.0.0.1/x",
            "http://169.254.169.254/latest/meta-data/",
            "http://localhost:8080/admin",
            "http://sub.localhost/x",
            "http://[::1]/x",
            "http://[fe80::1]/x",
            "http://10.0.0.5/x",
            "http://[::169.254.169.254]/latest/meta-data/",
            "http://[::127.0.0.1]/x",
        ] {
            assert_eq!(
                guard_fetch_url(blocked),
                Err(GuardError::BlockedHost),
                "{blocked}"
            );
        }
    }

    #[test]
    fn public_hosts_allowed() {
        for ok in [
            "https://cdn.real.example/v.m3u8",
            "https://allanime.day/apivtwo/clock.json?id=x",
            "http://8.8.8.8/x",
            "https://s4.anilist.co/file/cover.jpg",
        ] {
            assert_eq!(guard_fetch_url(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn percent_encoded_host_bypass_blocked() {
        assert_eq!(
            guard_fetch_url("http://127%2e0%2e0%2e1/x"),
            Err(GuardError::BlockedHost)
        );
        assert_eq!(
            guard_fetch_url("http://%6c%6fcalhost:8080/x"),
            Err(GuardError::BlockedHost)
        );
    }

    #[test]
    fn alternate_ipv4_spellings_blocked() {
        for blocked in [
            "http://2130706433/x",
            "http://2852039166/latest",
            "http://0x7f000001/x",
            "http://0x7f.0.0.1/x",
            "http://127.1/x",
            "http://127.0.0.1./x",
            "http://[::ffff:7f00:1]/x",
        ] {
            assert_eq!(
                guard_fetch_url(blocked),
                Err(GuardError::BlockedHost),
                "{blocked}"
            );
        }
    }

    #[test]
    fn numeric_spelling_backstop() {
        assert!(numeric_spelling("0xanything"));
        assert!(numeric_spelling("123.456.789"));
        assert!(numeric_spelling("2130706433"));
        assert!(!numeric_spelling("example.com"));
        assert!(!numeric_spelling("123abc.com"));
        assert!(!numeric_spelling(""));
    }
}
