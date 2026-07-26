//! Version comparison for the update check (06 §6.1). A plain string compare
//! gets `0.10.0` < `0.9.0` wrong; this parses the `major.minor.patch` core
//! numerically. Prerelease handling is coarse on purpose: a `-suffix` ranks
//! below the same core without one, but two prereleases are not ordered
//! against each other by identifier; the nag logic never needs it. Build
//! metadata (`+sha`) is ignored entirely.

use std::cmp::Ordering;

/// A parsed `major.minor.patch` plus prerelease presence (not identifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    prerelease: bool,
}

impl Version {
    /// Parse `[v]MAJOR.MINOR.PATCH[-prerelease][+build]`. Rejects anything
    /// without exactly three numeric core segments so a garbage remote tag
    /// cannot masquerade as a version; the caller treats `None` as "no update".
    pub fn parse(text: &str) -> Option<Version> {
        let s = text.strip_prefix(['v', 'V']).unwrap_or(text);

        // Split the core off any prerelease (`-`) or build-metadata (`+`)
        // tail. A `-` before a `+` starts the prerelease; a leading `+` is
        // build only.
        let (core, prerelease) = match (s.find('-'), s.find('+')) {
            (Some(d), p) if p.is_none_or(|p| d < p) => {
                (&s[..d], !s[d + 1..].starts_with('+') && s.len() > d + 1)
            }
            (_, Some(p)) => (&s[..p], false),
            _ => (s, false),
        };

        let mut it = core.split('.');
        let major = parse_field(it.next())?;
        let minor = parse_field(it.next())?;
        let patch = parse_field(it.next())?;
        if it.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            prerelease,
        })
    }

    /// Core numerically, then a prerelease ranks below the same released core.
    pub fn order(self, other: Version) -> Ordering {
        (self.major, self.minor, self.patch, !self.prerelease).cmp(&(
            other.major,
            other.minor,
            other.patch,
            !other.prerelease,
        ))
    }
}

fn parse_field(seg: Option<&str>) -> Option<u32> {
    let field = seg?;
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// True when `latest` is strictly newer than `current`: the one question the
/// update check asks. A parse failure on either side yields `false`; a
/// malformed remote tag stays silent instead of nagging.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (Version::parse(latest), Version::parse(current)) {
        (Some(l), Some(c)) => l.order(c) == Ordering::Greater,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_optional_v_prefix_and_plain_core() {
        let a = Version::parse("0.4.1").unwrap();
        let b = Version::parse("v0.4.1").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.order(b), Ordering::Equal);
    }

    #[test]
    fn parse_rejects_garbage_and_wrong_arity_cores() {
        for bad in [
            "", "v", "0.4", "0.4.1.2", "0.x.1", "latest", "0..1", "1.2.-3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn order_compares_fields_numerically_not_lexically() {
        let lo = Version::parse("0.9.0").unwrap();
        let hi = Version::parse("0.10.0").unwrap();
        assert_eq!(lo.order(hi), Ordering::Less);
        assert_eq!(hi.order(lo), Ordering::Greater);
    }

    #[test]
    fn order_ranks_a_prerelease_below_the_same_released_core() {
        let dev = Version::parse("0.4.1-dev").unwrap();
        let rel = Version::parse("0.4.1").unwrap();
        assert_eq!(dev.order(rel), Ordering::Less);
        assert_eq!(rel.order(dev), Ordering::Greater);
    }

    #[test]
    fn build_metadata_is_ignored_not_treated_as_prerelease() {
        let a = Version::parse("0.4.1+abc123").unwrap();
        let b = Version::parse("0.4.1").unwrap();
        assert_eq!(a.order(b), Ordering::Equal);
    }

    #[test]
    fn is_newer_the_update_check_question() {
        assert!(is_newer("0.5.0", "0.4.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(!is_newer("0.4.1", "0.4.1"));
        assert!(!is_newer("0.4.0", "0.4.1"));
        // A local dev build ahead of the last release must never nag.
        assert!(!is_newer("0.4.1", "0.5.0-dev"));
        assert!(!is_newer("garbage", "0.4.1"));
        assert!(!is_newer("", "0.4.1"));
    }
}
