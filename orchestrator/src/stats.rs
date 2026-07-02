//! Robust summary statistics for replicated measurements.
//!
//! A single benchmark run is one noisy draw. We replicate each cell and reduce
//! the draws with estimators that resist outliers instead of chasing them:
//!   - the **median** for central tendency (not the mean, which a single stalled
//!     replicate drags around),
//!   - the **IQR** and **MAD** for spread,
//!   - a **Hampel (MAD) filter** to reject replicates a background transient
//!     spiked, before the final estimate is taken.
//!
//! All functions treat an empty slice as "no data" and return 0.0 rather than
//! panicking, so a cell whose replicates all failed still produces a well-formed
//! (if empty) record.

use serde::{Deserialize, Serialize};

/// Linear-interpolated quantile (`q` in [0, 1]) over a copy of `xs`. Uses the
/// same "type 7" definition as NumPy's default, so the numbers line up with the
/// Python render/analysis side. Returns 0.0 for an empty slice.
pub fn quantile(xs: &[f64], q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.len() == 1 {
        return v[0];
    }
    let pos = q.clamp(0.0, 1.0) * (v.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    v[lo] + (v[hi] - v[lo]) * frac
}

/// Median (the 0.5 quantile).
pub fn median(xs: &[f64]) -> f64 {
    quantile(xs, 0.5)
}

/// Median absolute deviation, scaled by 1.4826 so it estimates the standard
/// deviation for normally distributed data. This is the robust dispersion the
/// Hampel filter thresholds on.
pub fn mad(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let m = median(xs);
    let dev: Vec<f64> = xs.iter().map(|x| (x - m).abs()).collect();
    1.4826 * median(&dev)
}

/// Interquartile range, p75 − p25.
pub fn iqr(xs: &[f64]) -> f64 {
    quantile(xs, 0.75) - quantile(xs, 0.25)
}

/// Sample coefficient of variation (stddev / mean). Reported alongside the
/// robust spread as the familiar-units cross-check; the stability *gate* keys on
/// relative IQR, not this.
pub fn cv(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if mean == 0.0 {
        return 0.0;
    }
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    var.sqrt() / mean.abs()
}

/// Per-element keep/reject mask under the Hampel rule: element `i` is kept
/// (`true`) unless it lies more than `k` scaled-MAD from the median. When the MAD
/// is zero (identical replicates, common for integer counts) everything is kept;
/// a zero threshold would otherwise reject every point that differs at all. Fewer
/// than three points are all kept (too few to call anything an outlier).
///
/// Returning a mask (not just the values) lets the caller select the *whole*
/// replicate records that survived, so an outlier in the primary metric drops
/// that replicate's telemetry too, keeping every reported field on the same set.
pub fn hampel_mask(xs: &[f64], k: f64) -> Vec<bool> {
    if xs.len() < 3 {
        return vec![true; xs.len()];
    }
    let m = median(xs);
    let scale = mad(xs);
    if scale == 0.0 {
        return vec![true; xs.len()];
    }
    xs.iter().map(|&x| (x - m).abs() <= k * scale).collect()
}

/// Partition `xs` into (kept, dropped) by the Hampel rule. Order within each
/// bucket follows the input.
pub fn hampel_partition(xs: &[f64], k: f64) -> (Vec<f64>, Vec<f64>) {
    let mask = hampel_mask(xs, k);
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for (keep, &x) in mask.iter().zip(xs) {
        if *keep {
            kept.push(x);
        } else {
            dropped.push(x);
        }
    }
    (kept, dropped)
}

/// Robust summary of one metric across a cell's replicates, after outlier
/// rejection. Serialized into the cell record so the render step and dashboard
/// can show a central estimate with an honest spread band and a confidence flag.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stability {
    /// Replicates that survived the Hampel filter and fed the final estimate.
    pub n: usize,
    /// Measured replicates run for this cell (excludes discarded warmup rounds).
    pub replicates: usize,
    /// Replicates rejected as outliers by the Hampel filter.
    pub outliers_dropped: usize,
    /// Robust central estimate: median of the kept replicates' primary metric.
    pub median: f64,
    /// Scaled median absolute deviation of the kept replicates.
    pub mad: f64,
    /// Interquartile range of the kept replicates.
    pub iqr: f64,
    pub min: f64,
    pub max: f64,
    /// IQR / median: the dimensionless spread the stability gate keys on.
    pub rel_iqr: f64,
    /// Coefficient of variation of the kept replicates (familiar-units cross-check).
    pub cv: f64,
    /// Whether `rel_iqr` met the configured target: the cell's number is
    /// reproducible enough to trust.
    pub stable: bool,
    /// Every measured replicate's primary value, in execution order, so the
    /// dashboard can plot the raw spread. Includes points later dropped as
    /// outliers.
    pub samples: Vec<f64>,
}

impl Stability {
    /// Summarize measured primary-metric `samples` (in execution order) with the
    /// given Hampel `k`, stability `target_rel_iqr`, and `max_outlier_frac` (the
    /// fraction of replicates the filter may reject before the cell is deemed too
    /// dirty to trust).
    pub fn summarize(
        samples: &[f64],
        k: f64,
        target_rel_iqr: f64,
        max_outlier_frac: f64,
    ) -> Stability {
        let (kept, dropped) = hampel_partition(samples, k);
        let median = median(&kept);
        let iqr = iqr(&kept);
        let rel_iqr = if median != 0.0 {
            (iqr / median).abs()
        } else {
            0.0
        };
        let (min, max) = kept
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &x| {
                (lo.min(x), hi.max(x))
            });
        Stability {
            n: kept.len(),
            replicates: samples.len(),
            outliers_dropped: dropped.len(),
            median,
            mad: mad(&kept),
            iqr,
            min: if min.is_finite() { min } else { 0.0 },
            max: if max.is_finite() { max } else { 0.0 },
            rel_iqr,
            cv: cv(&kept),
            // A cell is trustworthy only if enough replicates survived, the
            // surviving spread is tight, AND the filter did not have to reject a
            // large share of the draws. That last clause is what stops a bimodal
            // cell from looking rock-solid just because the filter discarded the
            // minority mode: a tight IQR over a heavily-filtered sample is false
            // confidence. With too few points the IQR is itself meaningless, so an
            // under-replicated cell is never called stable.
            stable: kept.len() >= 3
                && rel_iqr <= target_rel_iqr
                && (!samples.is_empty()
                    && (dropped.len() as f64 / samples.len() as f64) <= max_outlier_frac),
            samples: samples.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_matches_type7() {
        let xs = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(median(&xs), 2.5);
        assert!((quantile(&xs, 0.25) - 1.75).abs() < 1e-9);
        assert!((quantile(&xs, 0.75) - 3.25).abs() < 1e-9);
    }

    #[test]
    fn hampel_rejects_lone_spike() {
        let xs = [100.0, 101.0, 99.0, 100.5, 5000.0];
        let (kept, dropped) = hampel_partition(&xs, 3.0);
        assert_eq!(dropped, vec![5000.0]);
        assert_eq!(kept.len(), 4);
    }

    #[test]
    fn identical_counts_keep_all() {
        let xs = [42.0, 42.0, 42.0, 42.0];
        let (kept, dropped) = hampel_partition(&xs, 3.0);
        assert!(dropped.is_empty());
        assert_eq!(kept.len(), 4);
    }

    #[test]
    fn summarize_flags_stable_and_unstable() {
        let tight = Stability::summarize(&[100.0, 101.0, 99.0, 100.5, 100.2], 3.0, 0.05, 0.25);
        assert!(tight.stable);
        assert_eq!(tight.outliers_dropped, 0);

        let noisy = Stability::summarize(&[100.0, 180.0, 60.0, 140.0, 90.0], 3.0, 0.05, 0.25);
        assert!(!noisy.stable);
    }

    #[test]
    fn heavy_outlier_rejection_is_not_called_stable() {
        // Bimodal: three tight lows and two highs. The filter keeps the three
        // lows (tight IQR) but drops 2/5 = 40% > 25% cap, so despite the tight
        // survivors the cell must be flagged UNSTABLE, not falsely confident.
        let s = Stability::summarize(&[6756.0, 7018.0, 6716.0, 11378.0, 15530.0], 3.0, 0.05, 0.25);
        assert_eq!(s.outliers_dropped, 2);
        assert!(!s.stable, "40% dropped should void the stable flag");
    }

    #[test]
    fn empty_is_safe() {
        let s = Stability::summarize(&[], 3.0, 0.05, 0.25);
        assert_eq!(s.n, 0);
        assert_eq!(s.median, 0.0);
        assert!(!s.stable);
    }
}
