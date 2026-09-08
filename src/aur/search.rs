//! Ranking AUR search results against what the user actually typed.
//!
//! # Why rank locally
//!
//! The AUR's own search is a substring match with no ordering worth the name:
//! `by=name-desc` returns everything whose name *or description* contains the
//! term, so searching `hhd` returns the package called `hhd` alongside dozens
//! that merely mention it in prose, in no useful order.
//!
//! The endpoint is therefore treated as a candidate net, and the ordering is
//! done here. That also makes typo tolerance possible: the server cannot match
//! `decky-loder`, but a fuzzy matcher over the returned set can, provided the
//! net was cast wide enough to contain it.
//!
//! # The scoring
//!
//! Three signals, in strict precedence:
//!
//! 1. **Exactness of the name match.** An exact name always wins, then a
//!    prefix, then a fuzzy hit. Someone typing `yay` wants `yay`, not
//!    `yay-bin` and certainly not a package whose description says "like yay".
//! 2. **Where the match landed.** A name match outranks a description-only
//!    match, because names are what get installed.
//! 3. **Popularity.** Only as a tiebreak between comparable matches. It is a
//!    real signal — an AUR package with 4000 votes is likelier to be the one
//!    meant than one with 2 — but it must never promote a worse match.
//!
//! Out-of-date packages are not demoted. They are flagged in the UI instead:
//! being flagged says something about the packaging, not about whether it is
//! the package the user was looking for.

use crate::aur::rpc::SearchHit;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

/// A hit with its computed rank.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedHit {
    pub hit: SearchHit,
    /// Higher is better.
    pub score: i64,
    /// Whether the term matched the name at all, as opposed to only the
    /// description. Shown so a description-only result is not mistaken for a
    /// near-miss on the name.
    pub name_match: bool,
}

/// Score bands, kept far enough apart that no popularity bonus can lift a hit
/// out of its band into a better one.
const BAND_EXACT: i64 = 1_000_000;
const BAND_PREFIX: i64 = 500_000;
const BAND_NAME_FUZZY: i64 = 100_000;
const BAND_DESC_ONLY: i64 = 0;

/// Ceiling on the popularity contribution, so it can only ever reorder within
/// a band.
const POPULARITY_CAP: i64 = 10_000;

/// Rank `hits` against `term`, best first, dropping non-matches.
pub fn rank(term: &str, hits: Vec<SearchHit>) -> Vec<RankedHit> {
    let matcher = SkimMatcherV2::default().ignore_case();
    let needle = term.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }

    let mut ranked: Vec<RankedHit> = hits
        .into_iter()
        .filter_map(|hit| score_one(&matcher, &needle, hit))
        .collect();

    // Sort by score, then by name so equal scores are stable and the list does
    // not reshuffle between identical searches.
    ranked.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.hit.name.cmp(&b.hit.name))
    });
    ranked
}

fn score_one(matcher: &SkimMatcherV2, needle: &str, hit: SearchHit) -> Option<RankedHit> {
    let name = hit.name.to_lowercase();

    let (band, name_match) = if name == needle {
        (BAND_EXACT, true)
    } else if name.starts_with(needle) {
        (BAND_PREFIX, true)
    } else if let Some(score) = matcher.fuzzy_match(&name, needle) {
        // The matcher's own score varies with name length; keep it as a
        // within-band ordering signal only.
        (BAND_NAME_FUZZY + score.min(POPULARITY_CAP), true)
    } else if matcher
        .fuzzy_match(&hit.description.to_lowercase(), needle)
        .is_some()
    {
        (BAND_DESC_ONLY, false)
    } else {
        // Matched neither name nor description. The AUR returned it for some
        // reason, but showing it would just be noise.
        return None;
    };

    Some(RankedHit {
        score: band + popularity_bonus(hit.popularity),
        name_match,
        hit,
    })
}

/// Popularity as a bounded, monotonic bonus.
///
/// Logarithmic so the difference between 1 and 10 matters more than between
/// 1000 and 1010, and capped so it can never cross a band boundary.
fn popularity_bonus(popularity: f64) -> i64 {
    if popularity <= 0.0 {
        return 0;
    }
    let scaled = ((popularity + 1.0).ln() * 1000.0) as i64;
    scaled.clamp(0, POPULARITY_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, description: &str, popularity: f64) -> SearchHit {
        SearchHit {
            name: name.to_string(),
            version: "1.0".to_string(),
            description: description.to_string(),
            votes: 0,
            popularity,
            out_of_date: false,
        }
    }

    fn names(ranked: &[RankedHit]) -> Vec<&str> {
        ranked.iter().map(|r| r.hit.name.as_str()).collect()
    }

    #[test]
    fn an_exact_name_wins_even_against_a_far_more_popular_package() {
        let ranked = rank("yay", vec![hit("yay-bin", "", 5000.0), hit("yay", "", 0.1)]);
        assert_eq!(names(&ranked)[0], "yay");
    }

    #[test]
    fn a_prefix_beats_a_mid_string_fuzzy_match() {
        let ranked = rank(
            "hhd",
            vec![hit("adjustor-hhd", "", 0.0), hit("hhd-ui", "", 0.0)],
        );
        assert_eq!(names(&ranked)[0], "hhd-ui");
    }

    #[test]
    fn a_name_match_beats_a_description_only_match() {
        let ranked = rank(
            "decky",
            vec![
                hit("some-tool", "works well with decky loader", 900.0),
                hit("decky-loader-bin", "", 0.1),
            ],
        );
        assert_eq!(names(&ranked)[0], "decky-loader-bin");
    }

    #[test]
    fn a_typo_still_finds_the_package() {
        // The whole reason for ranking locally: the AUR's substring search
        // cannot match this, but a wide net plus fuzzy scoring can.
        let ranked = rank("deckyloader", vec![hit("decky-loader-bin", "", 0.0)]);
        assert_eq!(names(&ranked), vec!["decky-loader-bin"]);
    }

    #[test]
    fn popularity_orders_comparable_matches() {
        let ranked = rank(
            "hhd",
            vec![hit("hhd-alpha", "", 0.1), hit("hhd-beta", "", 500.0)],
        );
        assert_eq!(names(&ranked)[0], "hhd-beta");
    }

    #[test]
    fn popularity_can_never_promote_a_worse_match() {
        // The invariant the score bands exist to guarantee.
        let ranked = rank(
            "hhd",
            vec![
                hit("wildly-popular", "mentions hhd in passing", 999_999.0),
                hit("hhd", "", 0.0),
            ],
        );
        assert_eq!(names(&ranked)[0], "hhd");
    }

    #[test]
    fn results_that_match_nothing_are_dropped() {
        let ranked = rank("hhd", vec![hit("libreoffice", "office suite", 100.0)]);
        assert!(ranked.is_empty(), "{:?}", names(&ranked));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let ranked = rank("HHD", vec![hit("hhd", "", 0.0)]);
        assert_eq!(names(&ranked), vec!["hhd"]);
    }

    #[test]
    fn description_only_hits_are_marked_as_such() {
        let ranked = rank("decky", vec![hit("some-tool", "plugin for decky", 0.0)]);
        assert_eq!(ranked.len(), 1);
        assert!(!ranked[0].name_match, "must be flagged as description-only");
    }

    #[test]
    fn an_empty_term_ranks_nothing() {
        assert!(rank("", vec![hit("hhd", "", 0.0)]).is_empty());
        assert!(rank("   ", vec![hit("hhd", "", 0.0)]).is_empty());
    }

    #[test]
    fn ordering_is_stable_for_identical_scores() {
        let a = rank("hhd", vec![hit("hhd-b", "", 0.0), hit("hhd-a", "", 0.0)]);
        let b = rank("hhd", vec![hit("hhd-a", "", 0.0), hit("hhd-b", "", 0.0)]);
        assert_eq!(names(&a), names(&b));
        assert_eq!(names(&a), vec!["hhd-a", "hhd-b"]);
    }

    #[test]
    fn popularity_bonus_is_bounded_and_monotonic() {
        assert_eq!(popularity_bonus(0.0), 0);
        assert_eq!(popularity_bonus(-1.0), 0);
        assert!(popularity_bonus(10.0) > popularity_bonus(1.0));
        assert!(popularity_bonus(f64::MAX) <= POPULARITY_CAP);
    }

    #[test]
    fn an_out_of_date_package_is_not_demoted() {
        // Being flagged says something about the packaging, not about whether
        // it is the package the user meant.
        let mut stale = hit("hhd", "", 0.0);
        stale.out_of_date = true;
        let ranked = rank("hhd", vec![hit("hhd-other", "", 0.0), stale]);
        assert_eq!(names(&ranked)[0], "hhd");
    }
}
