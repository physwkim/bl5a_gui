//! The outlier filter an alignment scan's points go through before
//! their statistics: a cscan's second sample counts about 1.7 times as
//! long as the rest, enough to be the maximum `CEN` and `pl_xMAX` follow.

/// The outlier filter's window: the point and two either side, so two
/// adjacent points far above the rest are outliers and three are not.
const HALF_WINDOW: usize = 2;
/// A point this many σ from its window's median is an outlier.
const OUTLIER: f64 = 3.0;
/// σ per median absolute deviation, for normal noise.
const MAD_SIGMA: f64 = 1.4826;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// σ of the values' scatter about their median, from the median absolute
/// deviation.
fn mad_sigma(v: &[f64]) -> f64 {
    let m = median(&mut v.to_vec());
    let mut dev: Vec<f64> = v.iter().map(|e| (e - m).abs()).collect();
    MAD_SIGMA * median(&mut dev)
}

/// σ of one point, from the differences of neighbours: a feature's
/// slope adds to it only where the scan crosses the feature in a few
/// points.
fn noise(y: &[f64]) -> f64 {
    let d: Vec<f64> = y.windows(2).map(|w| w[1] - w[0]).collect();
    if d.is_empty() {
        0.0
    } else {
        mad_sigma(&d) / 2f64.sqrt()
    }
}

/// Hampel filter: a point more than `OUTLIER` σ from the median of its
/// window becomes that median, and so does one that is not a number. σ
/// is the window's, but at least the counting noise √median and the
/// scan's own noise, so a quiet window does not make an ordinary
/// fluctuation an outlier. The filtered points and how many changed.
pub fn hampel(y: &[f64]) -> (Vec<f64>, usize) {
    let floor = noise(y);
    let mut out = y.to_vec();
    let mut replaced = 0;
    for (i, v) in out.iter_mut().enumerate() {
        let lo = i.saturating_sub(HALF_WINDOW);
        let hi = (i + HALF_WINDOW + 1).min(y.len());
        let w: Vec<f64> = y[lo..hi]
            .iter()
            .copied()
            .filter(|e| e.is_finite())
            .collect();
        if w.is_empty() {
            continue;
        }
        let m = median(&mut w.clone());
        let sigma = mad_sigma(&w).max(m.abs().max(1.0).sqrt()).max(floor);
        if !v.is_finite() || (*v - m).abs() > OUTLIER * sigma {
            *v = m;
            replaced += 1;
        }
    }
    (out, replaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peak::Stats;

    /// `n` points from `a` to `b`.
    fn grid(a: f64, b: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| a + (b - a) * i as f64 / (n - 1) as f64)
            .collect()
    }

    /// `f` on `x` with up to ±√y of noise, the same on every run.
    fn counts(x: &[f64], f: impl Fn(f64) -> f64) -> Vec<f64> {
        let mut s: u64 = 0x2545_f491_4f6c_dd1d;
        x.iter()
            .map(|&x| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let u = (s >> 11) as f64 / (1u64 << 53) as f64;
                let y = f(x);
                y + y.sqrt() * (2.0 * u - 1.0)
            })
            .collect()
    }

    /// 100 counts behind the sample, 1000 past it; half of the beam at
    /// `at`.
    fn edge(x: &[f64], at: f64, low: f64) -> Vec<f64> {
        counts(x, |x| {
            low + (1000.0 - low) / (1.0 + ((x - at) / 0.02).exp())
        })
    }

    fn gauss(x: &[f64], at: f64, sigma: f64, base: f64) -> Vec<f64> {
        counts(x, |x| {
            base + 1000.0 * (-((x - at) / sigma).powi(2) / 2.0).exp()
        })
    }

    #[test]
    fn long_second_sample_of_an_edge_is_rejected() {
        let x = grid(0.0, 1.0, 101);
        let mut y = edge(&x, 0.437, 100.0);
        y[1] *= 1.7;
        assert_eq!(Stats::of(&x, &y).unwrap().xpk, x[1]);
        let (f, rejected) = hampel(&y);
        assert_eq!(rejected, 1);
        assert!(f[1] < 1100.0, "{}", f[1]);
    }

    #[test]
    fn two_point_spike_and_nan_are_rejected() {
        let x = grid(0.0, 1.0, 101);
        let mut y = gauss(&x, 0.5, 0.08, 50.0);
        (y[20], y[21], y[70]) = (3000.0, 3000.0, f64::NAN);
        let (f, rejected) = hampel(&y);
        assert_eq!(rejected, 3);
        let cen = Stats::of(&x, &f).unwrap().cen;
        assert!((cen - 0.5).abs() < 0.01, "{cen}");
    }

    #[test]
    fn three_point_peak_is_kept() {
        let mut y = vec![10.0; 21];
        y[9..12].copy_from_slice(&[40.0, 100.0, 50.0]);
        assert_eq!(hampel(&y).1, 0);
    }
}
