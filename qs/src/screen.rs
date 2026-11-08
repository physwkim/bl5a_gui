//! What an alignment scan must show before its motor moves. A point far
//! off its neighbours is put back to their median: a cscan's second
//! sample counts about 1.7 times as long as the rest, enough to be the
//! maximum `CEN` and `pl_xMAX` follow. Then a scan whose peak or edge
//! does not stand out of the noise, is not inside the range, or is one
//! point wide gives no target.

use crate::peak::{Stats, half};

/// The statistic an alignment scan's motor goes to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pick {
    /// `CEN` of a peak.
    Cen,
    /// `pl_COM` of a peak.
    Com,
    /// `pl_xMAX` of a peak.
    XMax,
    /// Where a half-cut scan's sample takes half the beam: the crossing
    /// halfway between the maximum and the floor under the blocked beam,
    /// on the one side the scan falls below half its maximum. The macros'
    /// `CEN` lies halfway between the crossing and the maximum, which on
    /// the unblocked plateau is wherever the noise puts it.
    Edge,
}

impl Pick {
    pub fn name(self) -> &'static str {
        match self {
            Pick::Cen => "CEN",
            Pick::Com => "pl_COM",
            Pick::XMax => "pl_xMAX",
            Pick::Edge => "EDGE",
        }
    }
}

/// A scan with a target.
#[derive(Debug)]
pub struct Screened {
    /// splot's statistics of the filtered points.
    pub stats: Stats,
    pub target: f64,
    /// How many points the filter replaced.
    pub rejected: usize,
}

/// The outlier filter's window: the point and two either side, so two
/// adjacent points far above the rest are outliers and three are not.
const HALF_WINDOW: usize = 2;
/// A point this many σ from its window's median is an outlier.
const OUTLIER: f64 = 3.0;
/// The visibility and peak tests average a scan down to this many bins,
/// so a 2000-sample fly scan is judged on means of 80 samples, not on
/// its noisiest sample.
const BINS: usize = 25;
/// A scan whose bins change by less than this fraction of their
/// maximum, or by less than `MIN_SNR` times a bin's noise, shows
/// nothing.
const MIN_CONTRAST: f64 = 0.10;
const MIN_SNR: f64 = 5.0;
/// A peak's bins fall below its largest, on each side, by more than
/// this many σ of a difference of two bins.
const MIN_FALL: f64 = 3.0;
/// A scan shows its floor when this many bins lie within `MIN_FALL` σ
/// of the lowest: a fine scan that ends on the edge's slope has its
/// minimum at an end point, above the floor.
const FLOOR_BINS: usize = 3;
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
fn hampel(y: &[f64]) -> (Vec<f64>, usize) {
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

/// The scan averaged into `BINS` bins, and the noise of one bin.
struct Bins {
    means: Vec<f64>,
    sigma: f64,
}

impl Bins {
    fn of(y: &[f64]) -> Bins {
        let b = (y.len() / BINS).max(1);
        Bins {
            means: y
                .chunks_exact(b)
                .map(|c| c.iter().sum::<f64>() / b as f64)
                .collect(),
            sigma: noise(y) / (b as f64).sqrt(),
        }
    }

    /// How much the scan changes: the range of the bins as a fraction of
    /// the largest, and in the noise of a bin.
    fn contrast(&self) -> (f64, f64) {
        let top = self.means.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let range = top - self.means.iter().copied().fold(f64::INFINITY, f64::min);
        (range / top, range / self.sigma)
    }

    /// The bins fall below the first largest on both sides, by more than
    /// `MIN_FALL` σ of a difference of two bins: a peak past an end, whose
    /// end point noise put below the one before it, does not.
    fn falls(&self) -> Result<(), String> {
        let m = &self.means;
        let top = (0..m.len()).fold(0, |t, i| if m[i] > m[t] { i } else { t });
        let least = MIN_FALL * std::f64::consts::SQRT_2 * self.sigma;
        for (side, part) in [("before", &m[..top]), ("after", &m[top + 1..])] {
            let low = part.iter().copied().fold(f64::INFINITY, f64::min);
            if m[top] - low <= least {
                return Err(format!(
                    "the scan does not fall {side} its maximum: no peak in the range"
                ));
            }
        }
        Ok(())
    }

    /// The scan levels off at its minimum: `FLOOR_BINS` bins within
    /// `MIN_FALL` σ of a difference of two bins of the lowest.
    fn settles(&self) -> bool {
        let low = self.means.iter().copied().fold(f64::INFINITY, f64::min);
        let least = MIN_FALL * std::f64::consts::SQRT_2 * self.sigma;
        self.means.iter().filter(|&&m| m - low <= least).count() >= FLOOR_BINS
    }
}

/// The edge of a half-cut scan, which falls below half its maximum on
/// one side. The floor is its minimum where it levels off there, and
/// zero, the fully blocked beam, where the scan ends on the slope.
fn edge(x: &[f64], y: &[f64], bins: &Bins) -> Result<f64, String> {
    let dir = match (half(x, y, -1), half(x, y, 1)) {
        (Some(_), None) => -1,
        (None, Some(_)) => 1,
        (None, None) => {
            return Err("the scan never falls to half its maximum: no edge in the range".into());
        }
        (Some(_), Some(_)) => {
            return Err(
                "the scan falls to half its maximum on both sides: a peak, not an edge".into(),
            );
        }
    };
    let floor = if bins.settles() {
        y.iter().copied().fold(f64::INFINITY, f64::min)
    } else {
        0.0
    };
    let above: Vec<f64> = y.iter().map(|v| v - floor).collect();
    half(x, &above, dir).ok_or_else(|| format!("the scan never falls halfway to its floor {floor}"))
}

/// The scan's statistics and `pick`'s target, or why it has none.
pub fn screen(x: &[f64], y: &[f64], pick: Pick) -> Result<Screened, String> {
    let (y, rejected) = hampel(y);
    if y.iter().any(|v| !v.is_finite()) {
        return Err("the scan has a run of points that are not numbers".into());
    }
    let stats = Stats::of(x, &y).ok_or("the scan has no points")?;
    let bins = Bins::of(&y);
    let (part, snr) = bins.contrast();
    if !(part >= MIN_CONTRAST && snr >= MIN_SNR) {
        let what = if let Pick::Edge = pick {
            "edge"
        } else {
            "peak"
        };
        return Err(format!(
            "no {what} visible: the scan changes by {:.0}% of its maximum, {snr:.1} times its noise",
            part * 100.0
        ));
    }
    if !matches!(pick, Pick::Edge) {
        inside(&y, stats.ymax)?;
        bins.falls()?;
    }
    let target = match pick {
        Pick::Cen => stats.cen,
        Pick::Com => stats.com,
        Pick::XMax => stats.xpk,
        Pick::Edge => edge(x, &y, &bins)?,
    };
    if !target.is_finite() {
        return Err(format!("{} is {target}", pick.name()));
    }
    Ok(Screened {
        stats,
        target,
        rejected,
    })
}

/// A peak inside the range: its maximum not an end point, and a
/// neighbour above half of it. With both neighbours at or below half,
/// splot's crossings are the neighbours and `CEN` the maximum, whatever
/// the peak's width.
fn inside(y: &[f64], ymax: f64) -> Result<(), String> {
    let i = y
        .iter()
        .position(|&v| v == ymax)
        .expect("the maximum is one of the points");
    let end = if i == 0 {
        "first"
    } else if i + 1 == y.len() {
        "last"
    } else if y[i - 1] <= ymax / 2.0 && y[i + 1] <= ymax / 2.0 {
        return Err("the peak is one point wide at half maximum".into());
    } else {
        return Ok(());
    };
    Err(format!(
        "the maximum is the scan's {end} point: no peak in the range"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn fails(x: &[f64], y: &[f64], pick: Pick) -> String {
        screen(x, y, pick).unwrap_err()
    }

    #[test]
    fn long_second_sample_of_an_edge_is_rejected() {
        let x = grid(0.0, 1.0, 101);
        let mut y = edge(&x, 0.437, 100.0);
        y[1] *= 1.7;
        let raw = Stats::of(&x, &y).unwrap();
        let s = screen(&x, &y, Pick::Edge).unwrap();
        assert!(raw.cen < 0.3, "the sample is the maximum: CEN {}", raw.cen);
        assert_eq!(s.rejected, 1);
        assert!((s.target - 0.437).abs() < 0.01, "{}", s.target);
    }

    #[test]
    fn two_point_spike_and_nan_are_rejected() {
        let x = grid(0.0, 1.0, 101);
        let mut y = gauss(&x, 0.5, 0.08, 50.0);
        (y[20], y[21], y[70]) = (3000.0, 3000.0, f64::NAN);
        let s = screen(&x, &y, Pick::Cen).unwrap();
        assert_eq!(s.rejected, 3);
        assert!((s.target - 0.5).abs() < 0.01, "{}", s.target);
    }

    #[test]
    fn flat_scan_shows_nothing() {
        let x = grid(0.0, 1.0, 101);
        let high = counts(&x, |_| 1000.0);
        assert!(fails(&x, &high, Pick::Cen).starts_with("no peak visible"));
        // Over 10% from bin to bin, but only by counting noise.
        let x = grid(0.0, 1.0, 401);
        let low = counts(&x, |_| 10.0);
        let (part, snr) = Bins::of(&hampel(&low).0).contrast();
        assert!(part >= MIN_CONTRAST && snr < MIN_SNR, "{part} {snr}");
        assert!(fails(&x, &low, Pick::Edge).starts_with("no edge visible"));
    }

    #[test]
    fn edge_short_of_half_has_no_target() {
        let x = grid(0.0, 1.0, 101);
        let y = edge(&x, 0.5, 750.0);
        assert!(fails(&x, &y, Pick::Edge).starts_with("the scan never falls to half"));
    }

    /// Halfway to a floor of 300 is at 650, where the edge is; half the
    /// maximum, 500, is 0.018 past it.
    #[test]
    fn edge_over_a_floor_is_halfway_to_it() {
        let x = grid(0.0, 1.0, 101);
        let falling = edge(&x, 0.437, 300.0);
        // The same edge with the beam unblocked at the far end.
        let rising: Vec<f64> = edge(&x, 1.0 - 0.437, 300.0).into_iter().rev().collect();
        for y in [falling, rising] {
            let t = screen(&x, &y, Pick::Edge).unwrap().target;
            assert!((t - 0.437).abs() < 0.005, "{t}");
        }
    }

    /// A fine scan that ends on the slope at 400: its minimum is no
    /// floor, and the edge is where it falls to half its maximum.
    #[test]
    fn edge_without_its_floor_is_at_half_the_maximum() {
        let x = grid(0.0, 1.0, 101);
        let y = counts(&x, |x| 1000.0 - 600.0 * ((x - 0.3) / 0.7).max(0.0));
        let t = screen(&x, &y, Pick::Edge).unwrap().target;
        assert!((t - (0.3 + 0.7 * 5.0 / 6.0)).abs() < 0.03, "{t}");
    }

    #[test]
    fn peak_is_no_edge() {
        let x = grid(0.0, 1.0, 101);
        let y = gauss(&x, 0.5, 0.08, 50.0);
        assert!(fails(&x, &y, Pick::Edge).ends_with("a peak, not an edge"));
    }

    #[test]
    fn peak_at_the_end_has_no_target() {
        let x = grid(0.0, 1.0, 21);
        let y: Vec<f64> = x.iter().map(|x| 100.0 + 1000.0 * x).collect();
        assert!(fails(&x, &y, Pick::XMax).starts_with("the maximum is the scan's last point"));
        let y: Vec<f64> = y.into_iter().rev().collect();
        assert!(fails(&x, &y, Pick::Com).starts_with("the maximum is the scan's first point"));
    }

    /// A peak past the last point, whose last point noise put below the
    /// one before it: the maximum is not an end point, but the scan does
    /// not fall after it.
    #[test]
    fn peak_past_the_end_has_no_target() {
        let x = grid(0.0, 1.0, 21);
        let mut y = gauss(&x, 1.05, 0.2, 50.0);
        y[20] = y[19] - 20.0;
        assert!(fails(&x, &y, Pick::Cen).starts_with("the scan does not fall after its maximum"));
        let y: Vec<f64> = y.into_iter().rev().collect();
        assert!(fails(&x, &y, Pick::Cen).starts_with("the scan does not fall before its maximum"));
        // A peak inside the range falls on both sides.
        let y = gauss(&x, 0.85, 0.05, 50.0);
        assert!(screen(&x, &y, Pick::Cen).is_ok());
    }

    #[test]
    fn peak_one_point_wide_has_no_target() {
        let x = grid(0.0, 2.0, 21);
        let mut y = vec![10.0; 21];
        y[9..12].copy_from_slice(&[40.0, 100.0, 50.0]);
        assert_eq!(hampel(&y).1, 0);
        assert!(fails(&x, &y, Pick::Cen).ends_with("one point wide at half maximum"));
        y[11] = 51.0;
        assert!(screen(&x, &y, Pick::Cen).is_ok());
    }

    /// A th scan narrower than the rocking curve: splot's `CEN` is the
    /// maximum.
    #[test]
    fn peak_wider_than_the_scan_goes_to_its_maximum() {
        let x = grid(-0.6, 0.6, 31);
        let y = counts(&x, |x| 1000.0 * (-((x - 0.1) / 1.2).powi(2) / 2.0).exp());
        let s = screen(&x, &y, Pick::Cen).unwrap();
        assert!((s.target - s.stats.xpk).abs() < 1e-12, "{}", s.target);
        assert!((s.target - 0.1).abs() < 0.2, "{}", s.target);
    }
}
