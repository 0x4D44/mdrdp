//! Summary statistics for latency samples.
//!
//! The median carries a *nonparametric* confidence interval, computed from order
//! statistics rather than `mean ± z·σ/√n`. RTT distributions are right-skewed, so a
//! confidence interval for the mean says nothing about the median — quoting one beside a
//! p50 would be a number that looks rigorous and is wrong.

use serde::Serialize;

/// z for a two-sided 95% interval.
const Z_95: f64 = 1.959_964;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub n: usize,
    pub min: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
    pub mean: f64,
    /// Nonparametric 95% confidence interval for the median.
    pub p50_ci_low: u64,
    pub p50_ci_high: u64,
}

/// Nearest-rank percentile: the smallest value at or above which `p` of the data lies.
///
/// `sorted` must be ascending and non-empty; `p` is in `0.0..=1.0`.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    debug_assert!(!sorted.is_empty());
    let rank = (p * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

/// Distribution-free 95% CI for the median, via the normal approximation to the
/// binomial. Returns 0-based indices into the sorted sample.
fn median_ci_indices(n: usize) -> (usize, usize) {
    let nf = n as f64;
    let spread = Z_95 * nf.sqrt();
    let lo = ((nf - spread) / 2.0).floor();
    let hi = ((nf + spread) / 2.0).ceil();

    let lo = if lo < 0.0 { 0 } else { lo as usize };
    let hi = (hi as usize).min(n - 1);
    (lo.min(hi), hi)
}

/// Summarise a set of samples. Returns `None` for an empty input rather than inventing
/// statistics for no data.
pub fn summarise(samples: &[u64]) -> Option<Summary> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();

    let n = sorted.len();
    let sum: u128 = sorted.iter().map(|&v| v as u128).sum();
    let (ci_lo, ci_hi) = median_ci_indices(n);

    Some(Summary {
        n,
        min: sorted[0],
        p50: percentile(&sorted, 0.50),
        p95: percentile(&sorted, 0.95),
        p99: percentile(&sorted, 0.99),
        max: sorted[n - 1],
        mean: sum as f64 / n as f64,
        p50_ci_low: sorted[ci_lo],
        p50_ci_high: sorted[ci_hi],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1..=100 makes every expected value hand-computable.
    fn one_to_hundred() -> Vec<u64> {
        (1..=100).collect()
    }

    #[test]
    fn percentiles_match_hand_computed_values() {
        let s = summarise(&one_to_hundred()).expect("non-empty");
        assert_eq!(s.n, 100);
        assert_eq!(s.min, 1);
        assert_eq!(s.max, 100);
        // nearest-rank: ceil(0.50 * 100) = 50 -> 0-based index 49 -> value 50
        assert_eq!(s.p50, 50);
        assert_eq!(s.p95, 95);
        assert_eq!(s.p99, 99);
        assert_eq!(s.mean, 50.5);
    }

    #[test]
    fn percentile_uses_nearest_rank_not_truncation() {
        // n=100 cannot test this: 0.50*100, 0.95*100 and 0.99*100 are all exact
        // integers, so ceil and floor agree and the test passes either way.
        // Sample counts that do not divide evenly separate the two definitions.

        // n=7: 0.50*7 = 3.5 -> ceil 4 -> 0-based index 3 -> value 4.
        // A truncating implementation returns 3.
        let s = summarise(&[1, 2, 3, 4, 5, 6, 7]).expect("non-empty");
        assert_eq!(s.p50, 4, "nearest-rank p50 of 1..=7 is the 4th value");

        // n=13: 0.50*13 = 6.5 -> ceil 7 -> index 6 -> value 7 (truncation gives 6).
        //       0.95*13 = 12.35 -> ceil 13 -> index 12 -> value 13 (truncation gives 12).
        let s = summarise(&(1..=13).collect::<Vec<u64>>()).expect("non-empty");
        assert_eq!(s.p50, 7);
        assert_eq!(s.p95, 13);
    }

    #[test]
    fn median_ci_matches_hand_computed_order_statistics() {
        // n=100: spread = 1.959964 * 10 = 19.59964
        // lo = floor((100 - 19.59964)/2) = floor(40.2) = 40  -> value 41
        // hi = ceil((100 + 19.59964)/2)  = ceil(59.8)  = 60  -> value 61
        let s = summarise(&one_to_hundred()).expect("non-empty");
        assert_eq!(s.p50_ci_low, 41);
        assert_eq!(s.p50_ci_high, 61);
    }

    #[test]
    fn ci_brackets_the_median() {
        for n in [1usize, 2, 3, 10, 99, 1000] {
            let samples: Vec<u64> = (1..=n as u64).collect();
            let s = summarise(&samples).expect("non-empty");
            assert!(
                s.p50_ci_low <= s.p50 && s.p50 <= s.p50_ci_high,
                "n={n}: CI [{}, {}] must bracket p50 {}",
                s.p50_ci_low,
                s.p50_ci_high,
                s.p50
            );
        }
    }

    #[test]
    fn ci_narrows_as_sample_count_grows() {
        // Same distribution, more samples: the interval must tighten in rank terms.
        let narrow = median_ci_indices(10_000);
        let wide = median_ci_indices(100);
        let narrow_frac = (narrow.1 - narrow.0) as f64 / 10_000.0;
        let wide_frac = (wide.1 - wide.0) as f64 / 100.0;
        assert!(
            narrow_frac < wide_frac,
            "CI width fraction should shrink with n: {narrow_frac} vs {wide_frac}"
        );
    }

    #[test]
    fn unsorted_input_is_sorted_before_summarising() {
        let mut reversed = one_to_hundred();
        reversed.reverse();
        assert_eq!(summarise(&reversed), summarise(&one_to_hundred()));
    }

    #[test]
    fn empty_input_yields_no_summary() {
        assert!(summarise(&[]).is_none());
    }

    #[test]
    fn single_sample_is_its_own_everything() {
        let s = summarise(&[42]).expect("non-empty");
        assert_eq!((s.n, s.min, s.p50, s.max, s.mean), (1, 42, 42, 42, 42.0));
        assert_eq!((s.p50_ci_low, s.p50_ci_high), (42, 42));
    }
}
