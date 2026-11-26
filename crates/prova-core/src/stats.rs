//! Statistics for experiments (ADR 0007). Small samples, skewed timing
//! data: rank-based tests and intervals, no normality assumption.
//!
//! Verified against SciPy reference vectors: `scripts/stats-reference.py`
//! writes `tests/stats_reference.json`, checked by `tests/stats.rs`.

/// Sorted copy.
pub fn sorted(v: &[f64]) -> Vec<f64> {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s
}

pub fn median(v: &[f64]) -> f64 {
    let s = sorted(v);
    let n = s.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// Median absolute deviation (unscaled).
pub fn mad(v: &[f64]) -> f64 {
    let m = median(v);
    let dev: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
    median(&dev)
}

/// Linear-interpolated quantile (NumPy's default method).
pub fn quantile(v: &[f64], q: f64) -> f64 {
    let s = sorted(v);
    if s.is_empty() {
        return f64::NAN;
    }
    let pos = q * (s.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
}

/// Indices of values outside Tukey fences (1.5 IQR). Flagged, never dropped.
pub fn tukey_outliers(v: &[f64]) -> Vec<usize> {
    let (q1, q3) = (quantile(v, 0.25), quantile(v, 0.75));
    let iqr = q3 - q1;
    let (lo, hi) = (q1 - 1.5 * iqr, q3 + 1.5 * iqr);
    v.iter()
        .enumerate()
        .filter(|(_, x)| **x < lo || **x > hi)
        .map(|(i, _)| i)
        .collect()
}

/// Null distribution of the Mann-Whitney U statistic for sizes n, m without
/// ties: counts of arrangements for u = 0..=n*m, as the coefficients of the
/// Gaussian binomial [n+m choose n]_q.
pub fn u_distribution(n: usize, m: usize) -> Vec<f64> {
    let len = n * m + 1;
    let mut c = vec![0.0; len];
    c[0] = 1.0;
    for i in 1..=n {
        // multiply by (1 - q^(m+i))
        let k = m + i;
        for t in (k..len).rev() {
            c[t] -= c[t - k];
        }
        // divide by (1 - q^i)
        for t in i..len {
            c[t] += c[t - i];
        }
    }
    c
}

/// P(U <= u) for each u under the null.
fn u_cdf(n: usize, m: usize) -> Vec<f64> {
    let d = u_distribution(n, m);
    let total: f64 = d.iter().sum();
    let mut acc = 0.0;
    d.iter()
        .map(|x| {
            acc += x;
            acc / total
        })
        .collect()
}

/// Standard normal survival function, via erfc (|error| < 1.2e-7).
pub fn normal_sf(z: f64) -> f64 {
    0.5 * erfc(z / std::f64::consts::SQRT_2)
}

fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t
        * (-z * z - 1.265_512_23
            + t * (1.000_023_68
                + t * (0.374_091_96
                    + t * (0.096_784_18
                        + t * (-0.186_288_06
                            + t * (0.278_868_07
                                + t * (-1.135_203_98
                                    + t * (1.488_515_87
                                        + t * (-0.822_152_23 + t * 0.170_872_77)))))))))
            .exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

/// Inverse standard normal CDF (Acklam; relative error < 1.2e-9).
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.38357751867269e2,
        -3.066479806614716e1,
        2.506628277459239,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e1,
        1.615858368580409e2,
        -1.556989798598866e2,
        6.680131188771972e1,
        -1.328068155288572e1,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838,
        -2.549732539343734,
        4.374664141464968,
        2.938163982698783,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-3,
        3.224671290700398e-1,
        2.445134137142996,
        3.754408661907416,
    ];
    let pl = 0.02425;
    if p < pl {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - pl {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -normal_quantile(1.0 - p)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MannWhitney {
    /// U statistic of the first sample (pairs where x > y, ties count 1/2).
    pub u: f64,
    pub p: f64,
    /// "exact" or "asymptotic".
    pub method: &'static str,
}

fn has_ties(x: &[f64], y: &[f64]) -> bool {
    let mut all: Vec<f64> = x.iter().chain(y).copied().collect();
    all.sort_by(f64::total_cmp);
    all.windows(2).any(|w| w[0] == w[1])
}

/// Two-sided Mann-Whitney U test. Exact without ties (n*m <= 10_000), exact
/// with ties up to 40 pooled values (the tie pattern fixes the null),
/// otherwise normal approximation with tie and continuity correction,
/// matching `scipy.stats.mannwhitneyu`.
/// The smallest two-sided p the exact Mann-Whitney test can give for
/// samples of sizes `n` and `m` (complete separation): 2 / C(n+m, n).
pub fn min_two_sided_p(n: usize, m: usize) -> f64 {
    let mut c = 1.0_f64;
    for i in 0..n.min(m) {
        c = c * (n + m - i) as f64 / (i + 1) as f64;
    }
    (2.0 / c).min(1.0)
}

/// Largest pooled sample for which ties are handled exactly.
const EXACT_TIES_MAX: usize = 40;

/// Exact two-sided p of the rank-sum statistic given the tie pattern: every
/// way to give `x.len()` of the pooled midranks to the first sample is
/// equally likely under the null. Counted by dynamic programming over the
/// doubled midranks (integers), so there is no sampling and no normal
/// approximation. Equals `scipy.stats.mannwhitneyu` with an exhaustive
/// `PermutationMethod`; without ties it is the usual exact test.
fn exact_p_with_ties(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len();
    let mut all: Vec<f64> = x.iter().chain(y).copied().collect();
    all.sort_by(f64::total_cmp);
    // Doubled midrank of each pooled value (1-based ranks).
    let mut ranks2: Vec<usize> = Vec::with_capacity(all.len());
    let mut i = 0;
    while i < all.len() {
        let mut j = i;
        while j + 1 < all.len() && all[j + 1] == all[i] {
            j += 1;
        }
        // Midrank = (i+1 + j+1) / 2, doubled.
        ranks2.extend(std::iter::repeat_n(i + j + 2, j - i + 1));
        i = j + 1;
    }
    let observed: usize = x
        .iter()
        .map(|v| {
            let first = all.partition_point(|a| a < v);
            ranks2[first]
        })
        .sum();
    let max_sum: usize = ranks2.iter().sum();
    // ways[k][s]: choices of k items with doubled-rank sum s.
    let mut ways = vec![vec![0.0_f64; max_sum + 1]; n + 1];
    ways[0][0] = 1.0;
    for r in &ranks2 {
        for k in (1..=n).rev() {
            for s in (*r..=max_sum).rev() {
                let w = ways[k - 1][s - r];
                ways[k][s] += w;
            }
        }
    }
    let total: f64 = ways[n].iter().sum();
    // Two-sided as SciPy does: twice the smaller tail. With ties the
    // midranks are not symmetric, so the tails differ.
    let at_most: f64 = ways[n][..=observed].iter().sum();
    let at_least: f64 = ways[n][observed..].iter().sum();
    (2.0 * at_most.min(at_least) / total).min(1.0)
}

pub fn mann_whitney(x: &[f64], y: &[f64]) -> MannWhitney {
    let (n, m) = (x.len(), y.len());
    let mut u: f64 = 0.0;
    for a in x {
        for b in y {
            u += if a > b {
                1.0
            } else if a == b {
                0.5
            } else {
                0.0
            };
        }
    }
    let nm = (n * m) as f64;
    let umax = u.max(nm - u);
    if !has_ties(x, y) && n * m <= 10_000 && n > 0 && m > 0 {
        let cdf = u_cdf(n, m);
        let k = umax as usize;
        // Rounding in the cdf can leave its tail a hair below zero.
        let sf: f64 = if k == 0 {
            1.0
        } else {
            (1.0 - cdf[k - 1]).max(0.0)
        };
        return MannWhitney {
            u,
            p: (2.0 * sf).min(1.0),
            method: "exact",
        };
    }
    if n > 0 && m > 0 && n + m <= EXACT_TIES_MAX {
        return MannWhitney {
            u,
            p: exact_p_with_ties(x, y),
            method: "exact",
        };
    }
    let total = n + m;
    let mut all: Vec<f64> = x.iter().chain(y).copied().collect();
    all.sort_by(f64::total_cmp);
    let mut tie_term = 0.0;
    let mut i = 0;
    while i < all.len() {
        let mut j = i;
        while j + 1 < all.len() && all[j + 1] == all[i] {
            j += 1;
        }
        let t = (j - i + 1) as f64;
        tie_term += t * t * t - t;
        i = j + 1;
    }
    let nt = total as f64;
    let sigma = (nm / 12.0 * ((nt + 1.0) - tie_term / (nt * (nt - 1.0)))).sqrt();
    if sigma == 0.0 {
        // Every value identical (typical for deterministic metrics such as
        // accuracy or step counts): no evidence of any difference.
        return MannWhitney {
            u,
            p: 1.0,
            method: "asymptotic",
        };
    }
    let z = (umax - nm / 2.0 - 0.5) / sigma;
    MannWhitney {
        u,
        p: (2.0 * normal_sf(z)).min(1.0),
        method: "asymptotic",
    }
}

/// Hodges-Lehmann shift estimate of `y - x` and its confidence interval,
/// computed like R's `wilcox.test(y, x, conf.int = TRUE)`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Shift {
    pub estimate: f64,
    pub lower: f64,
    pub upper: f64,
    /// Achieved confidence level of the interval.
    pub confidence: f64,
}

pub fn hodges_lehmann(x: &[f64], y: &[f64], alpha: f64) -> Shift {
    let (n, m) = (x.len(), y.len());
    let mut diffs: Vec<f64> = Vec::with_capacity(n * m);
    for b in y {
        for a in x {
            diffs.push(b - a);
        }
    }
    diffs.sort_by(f64::total_cmp);
    let nm = n * m;
    let estimate = median(&diffs);
    // qu = smallest u with P(U <= u) >= alpha/2, at least 1 (1-indexed).
    let (qu, confidence) = if n * m <= 10_000 && !has_ties(x, y) {
        let cdf = u_cdf(n, m);
        let q = cdf
            .iter()
            .position(|p| *p >= alpha / 2.0)
            .unwrap_or(0)
            .max(1);
        // [d_q, d_{nm-q+1}] (1-indexed) misses when U <= q-1 on either side.
        let conf = if q >= 2 { 1.0 - 2.0 * cdf[q - 2] } else { 1.0 };
        (q, conf)
    } else {
        let sigma = ((n * m * (n + m + 1)) as f64 / 12.0).sqrt();
        let z = normal_quantile(1.0 - alpha / 2.0);
        let q = ((nm as f64 / 2.0 - z * sigma).round() as isize).max(1) as usize;
        (q.min(nm), 1.0 - alpha)
    };
    let q = qu.min(nm);
    Shift {
        estimate,
        lower: diffs[q - 1],
        upper: diffs[nm - q],
        confidence,
    }
}

/// Holm-adjusted p-values, in the input order.
pub fn holm(p: &[f64]) -> Vec<f64> {
    let m = p.len();
    let mut idx: Vec<usize> = (0..m).collect();
    idx.sort_by(|a, b| p[*a].total_cmp(&p[*b]));
    let mut out = vec![0.0; m];
    let mut running: f64 = 0.0;
    for (rank, &i) in idx.iter().enumerate() {
        let adj = ((m - rank) as f64 * p[i]).min(1.0);
        running = running.max(adj);
        out[i] = running;
    }
    out
}

/// Small deterministic PRNG (xorshift64*), for reproducible bootstraps.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Percentile bootstrap interval for median(y)/median(x) - 1.
pub fn bootstrap_ratio(x: &[f64], y: &[f64], alpha: f64, rounds: usize, seed: u64) -> (f64, f64) {
    let mut rng = Rng::new(seed);
    let mut ratios = Vec::with_capacity(rounds);
    let mut bx = vec![0.0; x.len()];
    let mut by = vec![0.0; y.len()];
    for _ in 0..rounds {
        for v in bx.iter_mut() {
            *v = x[rng.below(x.len())];
        }
        for v in by.iter_mut() {
            *v = y[rng.below(y.len())];
        }
        ratios.push(median(&by) / median(&bx) - 1.0);
    }
    (
        quantile(&ratios, alpha / 2.0),
        quantile(&ratios, 1.0 - alpha / 2.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u_distribution_matches_hand_counts() {
        // n=2, m=2: arrangements give U = 0,1,2,2,3,4 → counts 1,1,2,1,1
        assert_eq!(u_distribution(2, 2), vec![1.0, 1.0, 2.0, 1.0, 1.0]);
        let total: f64 = u_distribution(5, 5).iter().sum();
        assert_eq!(total, 252.0); // C(10, 5)
    }

    /// RC acceptance (Go, 100 + 100 trials, no overlap): the exact tail
    /// came out as -4e-15 from rounding, printed `p=-0.0000`.
    #[test]
    fn a_p_value_is_never_negative() {
        let x: Vec<f64> = (0..100).map(|i| 1000.0 + f64::from(i)).collect();
        let y: Vec<f64> = (0..100).map(f64::from).collect();
        let p = mann_whitney(&x, &y).p;
        assert!((0.0..=1.0).contains(&p), "p = {p}");
    }

    #[test]
    fn small_samples_have_a_p_value_floor() {
        // Complete separation, 3 vs 3: the smallest possible p is 0.1.
        let r = mann_whitney(&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]);
        assert!((r.p - 0.1).abs() < 1e-12);
        let r = mann_whitney(&[1.0, 2.0, 3.0, 4.0, 5.0], &[6.0, 7.0, 8.0, 9.0, 10.0]);
        assert!((r.p - 2.0 / 252.0).abs() < 1e-12);
    }

    #[test]
    fn holm_is_monotone_and_capped() {
        let adj = holm(&[0.01, 0.04, 0.03, 0.5]);
        assert_eq!(adj, vec![0.04, 0.09, 0.09, 0.5]);
        assert!(holm(&[0.6, 0.7]).iter().all(|p| *p <= 1.0));
    }

    #[test]
    fn normal_helpers() {
        assert!((normal_sf(1.959_963_985) - 0.025).abs() < 1e-6);
        assert!((normal_quantile(0.975) - 1.959_963_985).abs() < 1e-8);
    }

    #[test]
    fn exact_test_has_a_floor_for_small_samples() {
        assert!((min_two_sided_p(5, 5) - 2.0 / 252.0).abs() < 1e-12);
        assert!((min_two_sided_p(3, 3) - 0.1).abs() < 1e-12);
        let separated = mann_whitney(&[10.0, 11.0, 12.0, 13.0, 14.0], &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!((separated.p - min_two_sided_p(5, 5)).abs() < 1e-12);
    }

    #[test]
    fn deterministic_metrics_do_not_break_the_test() {
        let same = mann_whitney(&[42.0; 5], &[42.0; 5]);
        assert_eq!(same.p, 1.0);
        let better = mann_whitney(&[10.0; 5], &[8.0; 5]);
        assert!(better.p < 0.05, "constant but different arms: {}", better.p);
        let hl = hodges_lehmann(&[10.0; 5], &[8.0; 5], 0.05);
        assert_eq!((hl.estimate, hl.lower, hl.upper), (-2.0, -2.0, -2.0));
    }

    #[test]
    fn outliers_are_flagged() {
        assert_eq!(tukey_outliers(&[10.0, 11.0, 10.5, 10.2, 50.0]), vec![4]);
    }
}
