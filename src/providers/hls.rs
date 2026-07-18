//! Shared HLS master-playlist parsing + quality-cap selection (ROD-302).
//! Providers fetch the master themselves (CDN headers differ per source) and
//! feed the bytes here; cap policy and variant math live in one place.

use crate::domain::{Quality, StreamLink};

/// Master playlist entry: variant URI (verbatim, possibly relative) + vertical
/// resolution when STREAM-INF advertised one.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub url: String,
    pub resolution: Option<u32>,
}

/// Height from `RESOLUTION=WxH` on EXT-X-STREAM-INF; None if absent or
/// malformed.
fn stream_inf_height(inf_line: &str) -> Option<u32> {
    let rest = &inf_line[inf_line.find("RESOLUTION=")? + "RESOLUTION=".len()..];
    let rest = &rest[rest.find('x')? + 1..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Master playlist: each `#EXT-X-STREAM-INF:` (resolution) paired with the
/// next non-comment URI (verbatim). Network caller joins relatives against
/// the playlist URL. No STREAM-INF → media playlist, empty vec; caller treats
/// the link as one stream.
pub fn parse_master_playlist(text: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    let mut pending_res: Option<Option<u32>> = None;
    for raw in text.split('\n') {
        let line = raw.trim_matches([' ', '\t', '\r']);
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-STREAM-INF") {
            pending_res = Some(stream_inf_height(line));
        } else if line.starts_with('#') {
            continue;
        } else if let Some(resolution) = pending_res.take() {
            out.push(Variant {
                url: line.to_string(),
                resolution,
            });
        }
    }
    out
}

/// Join a possibly-relative m3u8 URI against the playlist URL. Absolute
/// http(s) passes through; `/rooted` keeps scheme+host; else relative to the
/// playlist dir. `./`/`../` stay literal: mpv normalizes; we don't resolve.
pub fn join_url(base: &str, reference: &str) -> Option<String> {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return Some(reference.to_string());
    }
    let scheme_end = base.find("://")? + 3;
    let host_end = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |i| scheme_end + i);
    if reference.starts_with('/') {
        return Some(format!("{}{}", &base[..host_end], reference));
    }
    let dir_end = match base.rfind('/') {
        Some(last_slash) if last_slash >= host_end => last_slash + 1,
        _ => host_end,
    };
    Some(format!("{}{}", &base[..dir_end], reference))
}

/// Pick by quality preference, or None if empty (ROD-152). Cap policy:
/// best / worst take the highest / lowest resolution; a rung takes the
/// highest ≤ cap, and if every variant exceeds it, the lowest available
/// (never invent a ceiling breach; always return something the source
/// offers).
pub fn select_variant(variants: &[StreamLink], quality: Quality) -> Option<&StreamLink> {
    let mut it = variants.iter();
    let mut pick = it.next()?;
    for v in it {
        if preferred(v, pick, quality) {
            pick = v;
        }
    }
    Some(pick)
}

/// Whether candidate `a` beats incumbent `b` for `quality`.
/// Landmine: a KNOWN resolution always beats unknown (None). Under a rung
/// cap, a BANDWIDTH-only STREAM-INF (no res) could be any bitrate; treating
/// it as "0p, in budget" would hand a capped user the firehose the cap
/// prevents. Unknowns are last resort, only when EVERY candidate is unknown.
fn preferred(a: &StreamLink, b: &StreamLink, quality: Quality) -> bool {
    let Some(ra) = a.resolution else { return false };
    let Some(rb) = b.resolution else { return true };
    match quality.cap() {
        None if quality == Quality::Worst => ra < rb,
        None => ra > rb,
        Some(cap) => quality_rank(ra, cap) > quality_rank(rb, cap),
    }
}

/// Cap rank for a single `>` comparison. ≤ cap: non-negative, rises with res
/// (highest-≤-cap wins). Over budget: negative, rises toward zero as res
/// shrinks (smallest over-budget wins). Any in-budget always outranks any
/// over-budget.
fn quality_rank(res: u32, cap_px: u32) -> i64 {
    if res <= cap_px {
        i64::from(res)
    } else {
        -i64::from(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_master_playlist_extracts_uris_and_resolutions() {
        let playlist = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=842x480\n\
            480/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1400000,RESOLUTION=1280x720\n\
            720/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1920x1080\n\
            1080/index.m3u8\n";
        let vs = parse_master_playlist(playlist);
        assert_eq!(vs.len(), 3);
        assert_eq!(vs[0].url, "480/index.m3u8");
        assert_eq!(vs[0].resolution, Some(480));
        assert_eq!(vs[1].resolution, Some(720));
        assert_eq!(vs[2].resolution, Some(1080));
    }

    #[test]
    fn parse_master_playlist_media_playlist_yields_empty() {
        let media = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:9.0,\nseg0.ts\n#EXTINF:9.0,\nseg1.ts\n#EXT-X-ENDLIST\n";
        assert!(parse_master_playlist(media).is_empty());
    }

    #[test]
    fn parse_master_playlist_bandwidth_only_variant_has_no_resolution() {
        let playlist = "#EXT-X-STREAM-INF:BANDWIDTH=800000\nv.m3u8\n";
        let vs = parse_master_playlist(playlist);
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].resolution, None);
    }

    fn mk(res: Option<u32>) -> StreamLink {
        StreamLink {
            url: "https://cdn.test/v.m3u8".into(),
            resolution: res,
            referer: None,
            user_agent: None,
            cloaked_segments: false,
            sub_url: None,
        }
    }

    #[test]
    fn select_variant_cap_policy_picks_the_right_rung() {
        assert!(select_variant(&[], Quality::Best).is_none());

        let full = [mk(Some(480)), mk(Some(1080)), mk(Some(720))];
        let res = |q| select_variant(&full, q).unwrap().resolution;
        assert_eq!(res(Quality::Best), Some(1080));
        assert_eq!(res(Quality::Worst), Some(480));
        assert_eq!(res(Quality::P480), Some(480));
        assert_eq!(res(Quality::P720), Some(720));
        assert_eq!(res(Quality::P1080), Some(1080));

        // Requested rung absent → highest at or below it.
        let gap = [mk(Some(480)), mk(Some(1080))];
        assert_eq!(
            select_variant(&gap, Quality::P720).unwrap().resolution,
            Some(480)
        );

        // Every variant exceeds the cap → the smallest available.
        let over = [mk(Some(720)), mk(Some(1080))];
        assert_eq!(
            select_variant(&over, Quality::P480).unwrap().resolution,
            Some(720)
        );

        // Known beats unknown in every mode (rung-cap landmine).
        let withnull = [mk(None), mk(Some(720))];
        for q in [Quality::Best, Quality::Worst, Quality::P480] {
            assert_eq!(select_variant(&withnull, q).unwrap().resolution, Some(720));
        }

        // All unknown: still return one (a stream exists; do not error out).
        let allnull = [mk(None), mk(None)];
        assert!(select_variant(&allnull, Quality::P720).is_some());
    }

    #[test]
    fn join_url_absolute_rooted_and_relative() {
        let base = "https://h.example/x/y/master.m3u8";
        assert_eq!(
            join_url(base, "https://cdn.other/v.ts").unwrap(),
            "https://cdn.other/v.ts"
        );
        assert_eq!(
            join_url(base, "/a/b.ts").unwrap(),
            "https://h.example/a/b.ts"
        );
        assert_eq!(
            join_url(base, "720/seg.ts").unwrap(),
            "https://h.example/x/y/720/seg.ts"
        );
        assert!(join_url("no-scheme", "x.ts").is_none());
    }
}
