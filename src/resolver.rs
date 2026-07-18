//! Pure tier-B/C matchers (03 §4.2, ROD-328/342): given the AniList canonical
//! and one provider's search results, pick the candidate to bind, or nothing.
//! Workers do the network search; this scores offline. Imports domain +
//! anilist only (01 §5): the fuzzy rule is shared with AniList reverse match.
//!
//! A wrong bind is a silently persisted watchlist row, so every guard here
//! prefers "no match" over guessing.

use crate::anilist::title_score;
use crate::domain::Enrichment;
use crate::providers::SearchHit;

/// Min best score to bind (mirrors AniList best-match).
const BEST_FLOOR: i32 = 1200;
/// Min lead over the runner-up; near-ties refuse rather than guess.
const MATCH_MARGIN: i32 = 250;

/// Tier-B exact-id match (ROD-342). Tried before `best_provider_match`; no
/// title floor (romaji vs English catalog often fails every fuzzy floor).
///
/// Metadata that contradicts the id (eps/year) is treated as a mis-stamp and
/// skipped. Among survivors, corroborated (eps or year agrees) beats bare
/// regardless of list order; bare still binds when nothing is corroborated.
/// First hit within a class. No-op when the candidates carry no ids.
pub fn best_id_match(canonical: &Enrichment, candidates: &[SearchHit]) -> Option<usize> {
    let mut bare = None;
    for (i, cand) in candidates.iter().enumerate() {
        let id_agrees = cand.anilist_id == Some(canonical.anilist_id)
            || (canonical.mal_id.is_some() && cand.mal_id == canonical.mal_id);
        if !id_agrees {
            continue;
        }
        if id_match_contradicted(canonical, cand) {
            continue;
        }
        if id_match_corroborated(canonical, cand) {
            return Some(i);
        }
        if bare.is_none() {
            bare = Some(i);
        }
    }
    bare
}

/// Strong-evidence veto on an id agreement: episode gap > 3 when the total is
/// authoritative, or year gap > 1. Missing metadata never contradicts.
fn id_match_contradicted(canonical: &Enrichment, cand: &SearchHit) -> bool {
    let known_eps = canonical.total_episodes.unwrap_or(0);
    let cand_eps = candidate_episodes(cand);
    if known_eps > 0
        && cand_eps > 0
        && total_is_authoritative(canonical.status.as_deref())
        && known_eps.abs_diff(cand_eps) > 3
    {
        return true;
    }
    matches!((canonical.year, cand.year), (Some(ky), Some(cy)) if ky.abs_diff(cy) > 1)
}

/// Positive agreement beyond the id: eps within veto tolerance, or year
/// within 1.
fn id_match_corroborated(canonical: &Enrichment, cand: &SearchHit) -> bool {
    let known_eps = canonical.total_episodes.unwrap_or(0);
    let cand_eps = candidate_episodes(cand);
    if known_eps > 0 && cand_eps > 0 && known_eps.abs_diff(cand_eps) <= 3 {
        return true;
    }
    matches!((canonical.year, cand.year), (Some(ky), Some(cy)) if ky.abs_diff(cy) <= 1)
}

/// Best provider candidate for the canonical, or `None` below floor / inside
/// margin.
pub fn best_provider_match(canonical: &Enrichment, candidates: &[SearchHit]) -> Option<usize> {
    let mut best_idx = None;
    let mut best_score = i32::MIN;
    let mut second_score = i32::MIN;

    for (i, cand) in candidates.iter().enumerate() {
        let score = candidate_score(canonical, cand);
        if score > best_score {
            second_score = best_score;
            best_score = score;
            best_idx = Some(i);
        } else if score > second_score {
            second_score = score;
        }
    }

    let idx = best_idx?;
    if best_score < BEST_FLOOR {
        return None;
    }
    if second_score >= 0 && best_score - second_score < MATCH_MARGIN {
        return None;
    }
    Some(idx)
}

/// Score one provider candidate against the known canonical. Title is the
/// floor; episode count and year earn the margin.
fn candidate_score(canonical: &Enrichment, cand: &SearchHit) -> i32 {
    let mut score = i32::MIN / 4;

    // Best title agreement over known × candidate titles.
    let known = [
        Some(canonical.title_romaji.as_str()),
        canonical.title_english.as_deref(),
        canonical.title_native.as_deref(),
    ];
    let cand_titles = [
        Some(cand.title.as_str()),
        cand.title_english.as_deref(),
        cand.title_native.as_deref(),
    ];
    for k in known.into_iter().flatten() {
        if k.is_empty() {
            continue;
        }
        for c in cand_titles {
            score = score.max(title_score(k, c));
        }
    }
    if score < 0 {
        return score; // no title overlap → reject before tie-breakers
    }

    let known_eps = canonical.total_episodes.unwrap_or(0);
    let cand_eps = candidate_episodes(cand);
    if known_eps > 0 && cand_eps > 0 {
        let diff = known_eps.abs_diff(cand_eps);
        if diff == 0 {
            score += 180;
        } else if diff <= 1 {
            score += 120;
        } else if diff <= 3 {
            score += 60;
        } else if total_is_authoritative(canonical.status.as_deref()) {
            // Authoritative total off by >3 either direction = different work.
            // Hard reject so a lone exact-title hit cannot bind movie↔series.
            // RELEASING spared (partial listing is legitimate).
            return -4000;
        } else {
            score -= 120;
        }
    }

    if let (Some(ky), Some(cy)) = (canonical.year, cand.year) {
        let diff = ky.abs_diff(cy);
        if diff == 0 {
            score += 120;
        } else if diff == 1 {
            score += 40;
        } else {
            score -= 160;
        }
    }

    score
}

/// Catalog total if present, else max of the per-track counts; 0 skips the
/// eps signal.
fn candidate_episodes(cand: &SearchHit) -> u32 {
    cand.total_episodes
        .unwrap_or_else(|| cand.eps_sub.max(cand.eps_dub))
}

/// Whether `total_episodes` can drive the episode veto. True for settled
/// status AND for null/unknown (a doubtful total should reject a wild gap,
/// not mint a wrong bind). False only for RELEASING/HIATUS. Deliberately NOT
/// `is_still_airing`: that defaults null to still-airing (spare); a mis-bind
/// guard wants null to reject.
fn total_is_authoritative(status: Option<&str>) -> bool {
    let Some(s) = status else { return true };
    !s.eq_ignore_ascii_case("RELEASING") && !s.eq_ignore_ascii_case("HIATUS")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(name: &str) -> Enrichment {
        Enrichment {
            anilist_id: 1,
            title_romaji: name.to_string(),
            ..Enrichment::default()
        }
    }

    fn hit(provider_id: &str, title: &str) -> SearchHit {
        SearchHit {
            provider_id: provider_id.to_string(),
            title: title.to_string(),
            ..SearchHit::default()
        }
    }

    #[test]
    fn id_match_binds_on_mal_agreement_regardless_of_title() {
        // Id match needs no title agreement (English catalog vs romaji fails
        // every fuzzy floor).
        let canon = Enrichment {
            anilist_id: 154587,
            mal_id: Some(52991),
            title_romaji: "Sousou no Frieren".into(),
            ..Enrichment::default()
        };
        let candidates = [
            SearchHit {
                mal_id: Some(58305),
                ..hit("1443", "Frieren: Beyond Journey's End Season 2")
            },
            SearchHit {
                mal_id: Some(52991),
                ..hit("2454", "Frieren: Beyond Journey's End")
            },
        ];
        let idx = best_id_match(&canon, &candidates).unwrap();
        assert_eq!(candidates[idx].provider_id, "2454");
    }

    #[test]
    fn id_match_binds_on_anilist_id_when_the_provider_embeds_one() {
        let canon = Enrichment {
            anilist_id: 999,
            title_romaji: "X".into(),
            ..Enrichment::default()
        };
        let candidates = [
            SearchHit {
                anilist_id: Some(998),
                ..hit("a", "Y")
            },
            SearchHit {
                anilist_id: Some(999),
                ..hit("b", "Z")
            },
        ];
        let idx = best_id_match(&canon, &candidates).unwrap();
        assert_eq!(candidates[idx].provider_id, "b");
    }

    #[test]
    fn id_match_skips_candidate_whose_metadata_contradicts_the_id() {
        // ROD-342: same mal_id but metadata screaming different work must not
        // bind.
        let canon = Enrichment {
            anilist_id: 16498,
            mal_id: Some(16498),
            title_romaji: "Attack on Titan".into(),
            total_episodes: Some(25),
            year: Some(2013),
            status: Some("FINISHED".into()),
            ..Enrichment::default()
        };
        let decoy = SearchHit {
            mal_id: Some(16498),
            total_episodes: Some(1),
            year: Some(1998),
            ..hit("666", "Some Random 1998 Cooking OVA")
        };
        assert_eq!(best_id_match(&canon, std::slice::from_ref(&decoy)), None);

        let with_real = [
            decoy,
            SearchHit {
                mal_id: Some(16498),
                total_episodes: Some(25),
                year: Some(2013),
                ..hit("42", "Shingeki no Kyojin")
            },
        ];
        let idx = best_id_match(&canon, &with_real).unwrap();
        assert_eq!(with_real[idx].provider_id, "42");
    }

    #[test]
    fn id_match_prefers_corroborated_survivor_over_earlier_bare_one() {
        // A sparse hostile stamp is uncontradictable; corroboration must
        // outrank list order.
        let canon = Enrichment {
            anilist_id: 5114,
            mal_id: Some(5114),
            title_romaji: "Fullmetal Alchemist: Brotherhood".into(),
            total_episodes: Some(64),
            year: Some(2009),
            status: Some("FINISHED".into()),
            ..Enrichment::default()
        };
        let sparse = SearchHit {
            mal_id: Some(5114),
            ..hit("sparse-decoy", "Totally Different Show")
        };
        let candidates = [
            sparse.clone(),
            SearchHit {
                mal_id: Some(5114),
                total_episodes: Some(64),
                year: Some(2009),
                ..hit("real", "Fullmetal Alchemist: Brotherhood")
            },
        ];
        let idx = best_id_match(&canon, &candidates).unwrap();
        assert_eq!(candidates[idx].provider_id, "real");

        // Sparse alone still binds: absence of corroboration is not a veto.
        assert!(best_id_match(&canon, std::slice::from_ref(&sparse)).is_some());
    }

    #[test]
    fn id_match_veto_spares_bare_metadata_and_releasing_partials() {
        let canon = Enrichment {
            anilist_id: 1,
            mal_id: Some(52991),
            title_romaji: "X".into(),
            total_episodes: Some(28),
            year: Some(2023),
            status: Some("FINISHED".into()),
            ..Enrichment::default()
        };
        let bare = SearchHit {
            mal_id: Some(52991),
            ..hit("2454", "Y")
        };
        assert!(best_id_match(&canon, std::slice::from_ref(&bare)).is_some());

        // RELEASING partial count must not trip the eps veto.
        let airing = Enrichment {
            anilist_id: 1,
            mal_id: Some(59978),
            title_romaji: "X".into(),
            total_episodes: Some(28),
            year: Some(2026),
            status: Some("RELEASING".into()),
            ..Enrichment::default()
        };
        let partial = SearchHit {
            mal_id: Some(59978),
            total_episodes: Some(4),
            year: Some(2026),
            ..hit("1443", "Y")
        };
        assert!(best_id_match(&airing, std::slice::from_ref(&partial)).is_some());

        let wrong_year = SearchHit {
            mal_id: Some(59978),
            year: Some(1998),
            ..hit("9", "Y")
        };
        assert_eq!(
            best_id_match(&airing, std::slice::from_ref(&wrong_year)),
            None
        );
    }

    #[test]
    fn id_match_is_a_noop_without_id_agreement() {
        // Tier-C senshi shape: canonical has no mal_id, candidates embed no
        // anilist_id.
        let no_mal = Enrichment {
            anilist_id: 999,
            title_romaji: "X".into(),
            ..Enrichment::default()
        };
        let mal_only = SearchHit {
            mal_id: Some(52991),
            ..hit("52991", "X")
        };
        assert_eq!(
            best_id_match(&no_mal, std::slice::from_ref(&mal_only)),
            None
        );

        let full = Enrichment {
            anilist_id: 999,
            mal_id: Some(52991),
            title_romaji: "X".into(),
            ..Enrichment::default()
        };
        assert_eq!(best_id_match(&full, &[hit("a", "X")]), None);
        assert_eq!(best_id_match(&full, &[]), None);
    }

    #[test]
    fn provider_match_binds_exact_title_with_eps_and_year_agreement() {
        let canon = Enrichment {
            anilist_id: 154587,
            title_romaji: "Sousou no Frieren".into(),
            total_episodes: Some(28),
            year: Some(2023),
            ..Enrichment::default()
        };
        let candidates = [
            SearchHit {
                total_episodes: Some(12),
                year: Some(2019),
                ..hit("999", "Unrelated Show")
            },
            SearchHit {
                total_episodes: Some(28),
                year: Some(2023),
                ..hit("52991", "Sousou no Frieren")
            },
        ];
        let idx = best_provider_match(&canon, &candidates).unwrap();
        assert_eq!(candidates[idx].provider_id, "52991");
    }

    #[test]
    fn provider_match_rejects_when_no_candidate_clears_the_title_floor() {
        let canon = canonical("Sousou no Frieren");
        let candidates = [hit("a", "Naruto"), hit("b", "Bleach")];
        assert_eq!(best_provider_match(&canon, &candidates), None);
    }

    #[test]
    fn provider_match_rejects_an_ambiguous_near_tie() {
        // Identical scores → margin 0 → refuse rather than guess.
        let canon = Enrichment {
            year: Some(2023),
            ..canonical("Frieren")
        };
        let candidates = [
            SearchHit {
                year: Some(2023),
                ..hit("x", "Frieren")
            },
            SearchHit {
                year: Some(2023),
                ..hit("y", "Frieren")
            },
        ];
        assert_eq!(best_provider_match(&canon, &candidates), None);
    }

    #[test]
    fn provider_match_reconciles_season_n_title_forms() {
        let canon = Enrichment {
            total_episodes: Some(25),
            ..canonical("Re:Zero 2nd Season")
        };
        let candidates = [
            SearchHit {
                total_episodes: Some(25),
                ..hit("hit", "Re:Zero Season 2")
            },
            SearchHit {
                total_episodes: Some(12),
                ..hit("miss", "Completely Different")
            },
        ];
        let idx = best_provider_match(&canon, &candidates).unwrap();
        assert_eq!(candidates[idx].provider_id, "hit");
    }

    #[test]
    fn provider_match_rejects_lone_same_title_different_work_by_eps_veto() {
        // Exact title + no rival would clear floor/margin without the eps
        // veto (movie vs series).
        let canon = Enrichment {
            total_episodes: Some(25),
            status: Some("FINISHED".into()),
            ..canonical("Given")
        };
        let movie = SearchHit {
            total_episodes: Some(1),
            ..hit("movie", "Given")
        };
        assert_eq!(
            best_provider_match(&canon, std::slice::from_ref(&movie)),
            None
        );
    }

    #[test]
    fn provider_match_picks_the_series_when_its_movie_is_also_listed() {
        let canon = Enrichment {
            total_episodes: Some(11),
            status: Some("FINISHED".into()),
            ..canonical("Given")
        };
        let candidates = [
            SearchHit {
                total_episodes: Some(1),
                ..hit("movie", "Given")
            },
            SearchHit {
                total_episodes: Some(11),
                ..hit("series", "Given")
            },
        ];
        let idx = best_provider_match(&canon, &candidates).unwrap();
        assert_eq!(candidates[idx].provider_id, "series");
    }

    #[test]
    fn provider_match_spares_still_airing_canonical_with_fewer_listed_eps() {
        let canon = Enrichment {
            total_episodes: Some(1100),
            status: Some("RELEASING".into()),
            ..canonical("One Piece")
        };
        let candidates = [SearchHit {
            total_episodes: Some(1050),
            ..hit("op", "One Piece")
        }];
        let idx = best_provider_match(&canon, &candidates).unwrap();
        assert_eq!(candidates[idx].provider_id, "op");
    }

    #[test]
    fn provider_match_veto_is_symmetric_and_covers_null_status() {
        // Settled 12-ep must not bind a lone same-title 500-ep runner.
        let canon = Enrichment {
            total_episodes: Some(12),
            status: Some("FINISHED".into()),
            ..canonical("X")
        };
        let long_runner = SearchHit {
            total_episodes: Some(500),
            ..hit("long-runner", "X")
        };
        assert_eq!(
            best_provider_match(&canon, std::slice::from_ref(&long_runner)),
            None
        );

        // null status must not spare (that reopened the movie mis-bind).
        let unclassified = Enrichment {
            total_episodes: Some(25),
            status: None,
            ..canonical("Given")
        };
        let movie = SearchHit {
            total_episodes: Some(1),
            ..hit("movie", "Given")
        };
        assert_eq!(
            best_provider_match(&unclassified, std::slice::from_ref(&movie)),
            None
        );
    }

    #[test]
    fn provider_match_survives_garbage_huge_episode_count() {
        let canon = Enrichment {
            total_episodes: Some(100),
            status: Some("FINISHED".into()),
            ..canonical("X")
        };
        let garbage = SearchHit {
            total_episodes: Some(u32::MAX),
            ..hit("garbage", "X")
        };
        assert_eq!(
            best_provider_match(&canon, std::slice::from_ref(&garbage)),
            None
        );
    }

    #[test]
    fn provider_match_empty_page_is_none() {
        assert_eq!(best_provider_match(&canonical("Anything"), &[]), None);
    }

    #[test]
    fn candidate_episodes_prefers_total_falls_back_to_larger_track() {
        let total = SearchHit {
            total_episodes: Some(24),
            eps_sub: 12,
            ..SearchHit::default()
        };
        assert_eq!(candidate_episodes(&total), 24);

        let tracks = SearchHit {
            eps_sub: 12,
            eps_dub: 6,
            ..SearchHit::default()
        };
        assert_eq!(candidate_episodes(&tracks), 12);
        assert_eq!(candidate_episodes(&SearchHit::default()), 0);
    }
}
