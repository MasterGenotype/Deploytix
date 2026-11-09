//! Package version ordering, as pacman defines it.
//!
//! A port of libalpm's `alpm_pkg_vercmp` (`lib/libalpm/version.c`), which is
//! what `vercmp(8)` and every "is there an update" decision in pacman use.
//! Shelling out to `vercmp` would cost a process per comparison and make the
//! callers untestable without pacman installed, and an approximation would
//! disagree with pacman on exactly the versions that matter: `1.0rc` sorts
//! before `1.0`, `1.5.a` after `1.5`, and an epoch outranks everything.
//!
//! A version is `[epoch:]pkgver[-pkgrel]`. Epochs and versions are always
//! compared; the release only when both sides carry one, so `1.5` and `1.5-1`
//! are equal.

use std::cmp::Ordering;

/// Compare two full package versions the way pacman does.
pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (epoch_a, ver_a, rel_a) = parse_evr(a);
    let (epoch_b, ver_b, rel_b) = parse_evr(b);

    rpmvercmp(epoch_a, epoch_b)
        .then_with(|| rpmvercmp(ver_a, ver_b))
        .then_with(|| match (rel_a, rel_b) {
            (Some(ra), Some(rb)) => rpmvercmp(ra, rb),
            _ => Ordering::Equal,
        })
}

/// Split `[epoch:]version[-release]`. A missing or empty epoch is `"0"`.
///
/// The epoch is a run of leading digits followed by `:`; anything else means
/// there is no epoch and the whole prefix is version. The release is whatever
/// follows the *last* hyphen.
fn parse_evr(evr: &str) -> (&str, &str, Option<&str>) {
    let digits = evr.bytes().take_while(u8::is_ascii_digit).count();
    let (epoch, rest) = if evr.as_bytes().get(digits) == Some(&b':') {
        let epoch = &evr[..digits];
        (
            if epoch.is_empty() { "0" } else { epoch },
            &evr[digits + 1..],
        )
    } else {
        ("0", evr)
    };
    match rest.rfind('-') {
        Some(at) => (epoch, &rest[..at], Some(&rest[at + 1..])),
        None => (epoch, rest, None),
    }
}

/// Compare one version component: alternating runs of digits and letters,
/// separated by anything else.
///
/// Numeric runs compare as numbers and beat alphabetic ones; a longer
/// separator wins; and once one side runs out, a trailing alphabetic run is
/// *older* (`1.0rc` < `1.0`) while anything else is newer (`1.5.1` > `1.5`).
fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);

    while i < a.len() && j < b.len() {
        let (sep_a, sep_b) = (i, j);
        while i < a.len() && !a[i].is_ascii_alphanumeric() {
            i += 1;
        }
        while j < b.len() && !b[j].is_ascii_alphanumeric() {
            j += 1;
        }
        if i >= a.len() || j >= b.len() {
            break;
        }
        if i - sep_a != j - sep_b {
            return (i - sep_a).cmp(&(j - sep_b));
        }

        let numeric = a[i].is_ascii_digit();
        let in_run = |c: u8| {
            if numeric {
                c.is_ascii_digit()
            } else {
                c.is_ascii_alphabetic()
            }
        };
        let end_a = i + a[i..].iter().take_while(|&&c| in_run(c)).count();
        let end_b = j + b[j..].iter().take_while(|&&c| in_run(c)).count();

        // `b` has a run of the other kind here: numbers are newer than letters.
        if end_b == j {
            return if numeric {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }

        let (mut run_a, mut run_b) = (&a[i..end_a], &b[j..end_b]);
        if numeric {
            run_a = trim_leading_zeros(run_a);
            run_b = trim_leading_zeros(run_b);
            if run_a.len() != run_b.len() {
                return run_a.len().cmp(&run_b.len());
            }
        }
        match run_a.cmp(run_b) {
            Ordering::Equal => {}
            unequal => return unequal,
        }
        i = end_a;
        j = end_b;
    }

    let a_done = i >= a.len();
    let b_done = j >= b.len();
    if a_done && b_done {
        return Ordering::Equal;
    }
    let a_alpha = !a_done && a[i].is_ascii_alphabetic();
    let b_alpha = !b_done && b[j].is_ascii_alphabetic();
    if (a_done && !b_alpha) || a_alpha {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn trim_leading_zeros(run: &[u8]) -> &[u8] {
    let zeros = run.iter().take_while(|&&c| c == b'0').count();
    &run[zeros..]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cases from pacman's own `test/util/vercmptest.sh`, as
    /// `(a, b, sign of vercmp(a, b))`. Each is also checked reversed.
    const CASES: &[(&str, &str, i8)] = &[
        // all similar length, no pkgrel
        ("1.5.0", "1.5.0", 0),
        ("1.5.1", "1.5.0", 1),
        // mixed length
        ("1.5.1", "1.5", 1),
        // with pkgrel, simple
        ("1.5.0-1", "1.5.0-1", 0),
        ("1.5.0-1", "1.5.0-2", -1),
        ("1.5.0-1", "1.5.1-1", -1),
        ("1.5.0-2", "1.5.1-1", -1),
        // with pkgrel, mixed lengths
        ("1.5-1", "1.5.1-1", -1),
        ("1.5-2", "1.5.1-1", -1),
        ("1.5-2", "1.5.1-2", -1),
        // mixed pkgrel inclusion
        ("1.5", "1.5-1", 0),
        ("1.5-1", "1.5", 0),
        ("1.1-1", "1.1", 0),
        ("1.0-1", "1.1", -1),
        ("1.1-1", "1.0", 1),
        // alphanumeric versions
        ("1.5b-1", "1.5-1", -1),
        ("1.5b", "1.5", -1),
        ("1.5b-1", "1.5", -1),
        ("1.5b", "1.5.1", -1),
        // from the manpage
        ("1.0a", "1.0alpha", -1),
        ("1.0alpha", "1.0b", -1),
        ("1.0b", "1.0beta", -1),
        ("1.0beta", "1.0rc", -1),
        ("1.0rc", "1.0", -1),
        // going crazy? alpha-dotted versions
        ("1.5.a", "1.5", 1),
        ("1.5.b", "1.5.a", 1),
        ("1.5.1", "1.5.b", 1),
        // alpha dots and dashes
        ("1.5.b-1", "1.5.b", 0),
        ("1.5-1", "1.5.b", -1),
        // same/similar content, differing separators
        ("2.0", "2_0", 0),
        ("2.0_a", "2_0.a", 0),
        ("2.0a", "2.0.a", -1),
        ("2___a", "2_a", 1),
        // epoch included version comparisons
        ("0:1.0", "0:1.0", 0),
        ("0:1.0", "0:1.1", -1),
        ("1:1.0", "0:1.0", 1),
        ("1:1.0", "0:1.1", 1),
        ("1:1.0", "2:1.1", -1),
        // epoch + sometimes present pkgrel
        ("1:1.0", "0:1.0-1", 1),
        ("1:1.0-1", "0:1.1-1", 1),
        // epoch included on one version
        ("0:1.0", "1.0", 0),
        ("0:1.0", "1.1", -1),
        ("0:1.1", "1.0", 1),
        ("1:1.0", "1.0", 1),
        ("1:1.0", "1.1", 1),
        ("1:1.1", "1.1", 1),
    ];

    fn sign(o: Ordering) -> i8 {
        match o {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }
    }

    #[test]
    fn matches_pacmans_own_test_suite() {
        for &(a, b, want) in CASES {
            assert_eq!(sign(vercmp(a, b)), want, "vercmp({a}, {b})");
            assert_eq!(sign(vercmp(b, a)), -want, "vercmp({b}, {a})");
        }
    }

    #[test]
    fn numeric_runs_compare_as_numbers_not_strings() {
        assert_eq!(vercmp("1.10-1", "1.9-1"), Ordering::Greater);
        assert_eq!(vercmp("1.010", "1.10"), Ordering::Equal);
        // Longer than any integer type, which is why it is not parsed.
        assert_eq!(
            vercmp("1.123456789012345678901234567890", "1.99"),
            Ordering::Greater
        );
    }

    #[test]
    fn a_git_snapshot_version_orders_by_revision_count() {
        // The shape makepkg's pkgver() produces for VCS packages.
        assert_eq!(vercmp("r52.abc1234-1", "r39.def5678-1"), Ordering::Greater);
    }
}
