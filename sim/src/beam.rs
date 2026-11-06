//! Count rates from the beamline state: source → PFCU filters and shutter →
//! sample slit (hs4) → sample → detector slit (hs5) on the tth/nu arm.
//!
//! All positions are lab frame (user without `set` offsets), mm and degrees.
//! The beam is Gaussian in both transverse directions with no divergence, so
//! every aperture is an interval and every fraction a difference of Φ.

/// Distance from the sample to the detector slit, mm.
const ARM: f64 = 1000.0;
const SIGMA_V: f64 = 0.25;
const SIGMA_H: f64 = 0.3;
const FLUX: f64 = 1e11;
/// Transmission of one filter unit (one PFCU0 blade-1 equivalent).
const UNIT_T: f64 = 0.3;
const FILM_LENGTH: f64 = 10.0;
const CRITICAL_ANGLE: f64 = 0.2;
const WINDOW_RADIUS: f64 = 1.5;
const WINDOW_T: f64 = 0.3;
const STEEL_T: f64 = 0.003;
const WINDOW_EDGE: f64 = 0.2;
const DET_DEAD_TIME: f64 = 1e-6;
const DET_DARK: f64 = 0.5;
const IC_MAX: f64 = 1e6;

/// SplitMix64 with Box–Muller normals.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in (0, 1).
    pub fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    pub fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }

    pub fn normal(&mut self) -> f64 {
        let (u, v) = (self.uniform(), self.uniform());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }

    /// Knuth's product method below a mean of 30, a rounded normal above.
    pub fn poisson(&mut self, mean: f64) -> f64 {
        if mean <= 0.0 {
            return 0.0;
        }
        if mean > 30.0 {
            return (mean + mean.sqrt() * self.normal()).round().max(0.0);
        }
        let limit = (-mean).exp();
        let mut k = 0.0;
        let mut p = self.uniform();
        while p > limit {
            k += 1.0;
            p *= self.uniform();
        }
        k
    }
}

/// Abramowitz & Stegun 7.1.26, |error| < 1.5e-7.
pub fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let y = 1.0 - poly * (-x * x).exp();
    if x < 0.0 { -y } else { y }
}

fn cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Fraction of a centred Gaussian of width `sigma` inside `[lo, hi]`.
fn inside(lo: f64, hi: f64, sigma: f64) -> f64 {
    if hi <= lo {
        0.0
    } else {
        (cdf(hi / sigma) - cdf(lo / sigma)).max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span(pub f64, pub f64);

impl Span {
    fn and(self, o: Span) -> Span {
        Span(self.0.max(o.0), self.1.min(o.1))
    }

    fn shift(self, d: f64) -> Span {
        Span(self.0 + d, self.1 + d)
    }

    fn of(self, sigma: f64) -> f64 {
        inside(self.0, self.1, sigma)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    /// Coin cell: a round window in a steel can, centred at (`sy0`, `zc`).
    Coin,
    /// Film on a thick substrate whose surface passes through the beam at
    /// shold = `z0` and is parallel to it at th = `th0`.
    Film,
}

/// The hidden alignment answers, lab frame, drawn once per run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Truth {
    pub tth0: f64,
    pub nu0: f64,
    pub th0: f64,
    pub z0: f64,
    pub sy0: f64,
    pub zc: f64,
}

impl Truth {
    pub fn draw(rng: &mut Rng) -> Truth {
        Truth {
            tth0: rng.between(-0.02, 0.02),
            nu0: rng.between(-0.05, 0.05),
            th0: rng.between(-0.3, 0.3),
            // align_in takes CEN of the edge scan from shold -3.0, which
            // lands halfway between the scan's maximum and the edge, so it
            // converges only with the surface just above -3.0. There the
            // film already cuts the beam's lower tail, and the direct-beam
            // scan before it centres up to 0.008 deg off tth0.
            z0: rng.between(-2.95, -2.8),
            sy0: rng.between(-0.5, 0.5),
            zc: rng.between(-0.5, 0.5),
        }
    }
}

/// PFCU blades (index 0 = blade 1) and the ion-chamber amplifier.
#[derive(Clone, Debug, PartialEq)]
pub struct Optics {
    pub pfcu: [[bool; 4]; 2],
    pub amp_range: i32,
    pub amp_offset: f64,
}

impl Default for Optics {
    fn default() -> Optics {
        Optics {
            pfcu: [[false; 4], [false, false, true, false]],
            amp_range: 5,
            amp_offset: 0.0,
        }
    }
}

impl Optics {
    /// PFCU1 blade 3 is the mar345 shutter.
    pub fn shutter_open(&self) -> bool {
        !self.pfcu[1][2]
    }

    /// Filter thickness in PFCU0 blade-1 units (`atte`'s number).
    pub fn filter_units(&self) -> u32 {
        let mut units = 0;
        for (i, &inserted) in self.pfcu[0].iter().enumerate() {
            units += u32::from(inserted) << i;
        }
        for (i, &inserted) in self.pfcu[1].iter().enumerate() {
            if i != 2 {
                units += u32::from(inserted) << (i + 4);
            }
        }
        units
    }

    fn transmission(&self) -> f64 {
        if self.shutter_open() {
            UNIT_T.powi(self.filter_units() as i32)
        } else {
            0.0
        }
    }
}

/// Lab positions of the motors the beam sees.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Geometry {
    pub tth: f64,
    pub th: f64,
    pub nu: f64,
    pub sy: f64,
    pub shold: f64,
    pub hs4u: f64,
    pub hs4d: f64,
    pub hs4h: f64,
    pub hs4r: f64,
    pub hs5u: f64,
    pub hs5d: f64,
    pub hs5h: f64,
    pub hs5r: f64,
}

/// Counts per second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rates {
    pub icxrd: f64,
    pub ic1: f64,
    pub det: f64,
}

fn tan_deg(x: f64) -> f64 {
    x.to_radians().tan()
}

fn fresnel(alpha: f64) -> f64 {
    if alpha <= CRITICAL_ANGLE {
        return 1.0;
    }
    let q = (alpha * alpha - CRITICAL_ANGLE * CRITICAL_ANGLE).sqrt();
    ((alpha - q) / (alpha + q)).powi(2)
}

fn ion_chamber(photons: f64, gain: f64) -> f64 {
    (photons / FLUX * gain).min(IC_MAX)
}

/// Photons per second through the filters and the mar shutter.
fn incident(optics: &Optics, ring: f64) -> f64 {
    FLUX * ring / 400.0 * optics.transmission()
}

/// Photons per second through the sample slit onto the sample.
pub fn on_sample(g: &Geometry, optics: &Optics, ring: f64) -> f64 {
    incident(optics, ring) * Span(g.hs4d, g.hs4u).of(SIGMA_V) * Span(g.hs4r, g.hs4h).of(SIGMA_H)
}

pub fn rates(g: &Geometry, optics: &Optics, sample: Sample, truth: &Truth, ring: f64) -> Rates {
    let flux = incident(optics, ring);
    let slit_v = Span(g.hs4d, g.hs4u);
    let slit_h = Span(g.hs4r, g.hs4h);
    let on_sample = on_sample(g, optics, ring);

    let det_v = Span(g.hs5d, g.hs5u).shift(ARM * tan_deg(g.tth - truth.tth0));
    let det_h = Span(g.hs5r, g.hs5h).shift(ARM * tan_deg(g.nu - truth.nu0));
    let h_after = slit_h.of(SIGMA_H);
    let h_det = slit_h.and(det_h).of(SIGMA_H);

    // (photons past the sample, photons into the detector), vertical part.
    let (past_v, det_part_v) = match sample {
        Sample::Coin => {
            let r = (g.sy - truth.sy0).hypot(g.shold - truth.zc);
            let t = STEEL_T + (WINDOW_T - STEEL_T) * cdf((WINDOW_RADIUS - r) / WINDOW_EDGE);
            (t * slit_v.of(SIGMA_V), t * slit_v.and(det_v).of(SIGMA_V))
        }
        Sample::Film => {
            let alpha = g.th - truth.th0;
            let surface = g.shold - truth.z0;
            let half = FILM_LENGTH / 2.0 * tan_deg(alpha).abs();
            let over = slit_v.and(Span(surface + half, f64::INFINITY));
            let mut past = over.of(SIGMA_V);
            let mut into = over.and(det_v).of(SIGMA_V);
            if alpha > 0.0 {
                let hit = slit_v.and(Span(surface - half, surface + half));
                let r = fresnel(alpha);
                let lands = det_v.shift(-ARM * tan_deg(2.0 * alpha));
                past += r * hit.of(SIGMA_V);
                into += r * hit.and(lands).of(SIGMA_V);
            }
            (past, into)
        }
    };

    let gain = 1e5 * 10f64.powi(5 - optics.amp_range);
    let det = flux * det_part_v * h_det;
    Rates {
        icxrd: ion_chamber(on_sample, gain) + optics.amp_offset * 1e4,
        ic1: ion_chamber(flux * past_v * h_after, 2e5),
        det: det / (1.0 + det * DET_DEAD_TIME) + DET_DARK,
    }
}

/// Ring current, mA: top-up every 120 s from 400 mA.
pub fn ring_current(elapsed: f64) -> f64 {
    400.0 - 0.4 * (elapsed / 120.0).fract()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUTH: Truth = Truth {
        tth0: 0.01,
        nu0: -0.03,
        th0: 0.2,
        z0: -2.3,
        sy0: 0.3,
        zc: -0.2,
    };

    fn film_geometry() -> Geometry {
        Geometry {
            tth: TRUTH.tth0,
            th: TRUTH.th0,
            nu: TRUTH.nu0,
            shold: -3.0,
            hs4u: 0.5,
            hs4d: -0.5,
            hs4h: 3.0,
            hs4r: -3.0,
            hs5u: 0.25,
            hs5d: -0.25,
            hs5h: 0.5,
            hs5r: -0.5,
            ..Geometry::default()
        }
    }

    fn open(units: u32) -> Optics {
        let mut o = Optics {
            pfcu: [[false; 4]; 2],
            ..Optics::default()
        };
        for i in 0..4 {
            o.pfcu[0][i] = units & (1 << i) != 0;
        }
        o
    }

    fn det(g: &Geometry) -> f64 {
        rates(g, &open(11), Sample::Film, &TRUTH, 400.0).det
    }

    #[test]
    fn erf_matches_known_values() {
        for (x, want) in [(0.0, 0.0), (0.5, 0.520_499_877_8), (1.0, 0.842_700_792_9)] {
            assert!((erf(x) - want).abs() < 2e-7);
            assert!((erf(-x) + want).abs() < 2e-7);
        }
    }

    #[test]
    fn rng_is_seeded_and_poisson_has_its_mean() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(a.next_u64(), b.next_u64());
        for mean in [3.0, 300.0] {
            let n = 20_000;
            let sum: f64 = (0..n).map(|_| a.poisson(mean)).sum();
            assert!((sum / n as f64 - mean).abs() < 0.05 * mean.sqrt());
        }
    }

    #[test]
    fn shutter_and_filters() {
        let g = film_geometry();
        let closed = Optics::default();
        assert!(!closed.shutter_open());
        let r = rates(&g, &closed, Sample::Film, &TRUTH, 400.0);
        assert_eq!((r.icxrd, r.ic1, r.det), (0.0, 0.0, DET_DARK));
        let mut o = open(0);
        o.pfcu[1] = [true, true, false, true];
        assert_eq!(o.filter_units(), 16 + 32 + 128);
        assert_eq!(open(11).filter_units(), 11);
    }

    #[test]
    fn direct_beam_peaks_at_the_true_arm_angles() {
        let mut g = film_geometry();
        let peak = det(&g);
        g.tth = TRUTH.tth0 + 0.02;
        assert!(det(&g) < 0.6 * peak);
        g.tth = TRUTH.tth0 - 0.02;
        let low = det(&g);
        g.tth = TRUTH.tth0 + 0.02;
        assert!((det(&g) - low).abs() < 1e-6 * peak);
        g.tth = TRUTH.tth0;
        g.nu = TRUTH.nu0 + 0.05;
        assert!(det(&g) < peak);
    }

    #[test]
    fn half_cut_passes_half_at_the_surface() {
        // At 15 units the dead-time loss is below the tolerance.
        let det = |g: &Geometry| rates(g, &open(15), Sample::Film, &TRUTH, 400.0).det;
        let mut g = film_geometry();
        let full = det(&g);
        g.shold = TRUTH.z0;
        assert!((det(&g) - DET_DARK - 0.5 * (full - DET_DARK)).abs() < 1e-3 * full);
        g.shold = TRUTH.z0 + 0.3;
        assert!(det(&g) < 1e-3 * full);
        g.shold = TRUTH.z0;
        let flat = det(&g);
        g.th = TRUTH.th0 - 0.5;
        assert!(det(&g) < flat);
    }

    #[test]
    fn specular_peaks_where_twice_alpha_meets_the_arm() {
        let mut g = film_geometry();
        g.shold = TRUTH.z0;
        g.tth = TRUTH.tth0 + 1.2;
        // `atte 4`, as align_in uses for the specular scan.
        let at = |th: f64| rates(&Geometry { th, ..g }, &open(4), Sample::Film, &TRUTH, 400.0).det;
        let on = at(TRUTH.th0 + 0.6);
        assert!(on > 10.0 * at(TRUTH.th0 + 0.55));
        assert!(on > 10.0 * at(TRUTH.th0 + 0.65));
    }

    #[test]
    fn coin_window_transmits_at_its_centre() {
        let g = Geometry {
            sy: TRUTH.sy0,
            shold: TRUTH.zc,
            ..film_geometry()
        };
        let ic1 = |g: &Geometry| rates(g, &open(1), Sample::Coin, &TRUTH, 400.0).ic1;
        let centre = ic1(&g);
        let steel = ic1(&Geometry {
            sy: TRUTH.sy0 + 3.0,
            ..g
        });
        assert!((centre / steel - WINDOW_T / STEEL_T).abs() < 0.01 * WINDOW_T / STEEL_T);
        let left = ic1(&Geometry {
            sy: TRUTH.sy0 - 1.0,
            ..g
        });
        let right = ic1(&Geometry {
            sy: TRUTH.sy0 + 1.0,
            ..g
        });
        assert!((left - right).abs() < 1e-9 * centre);
    }

    #[test]
    fn monitor_follows_amplifier_range_and_offset() {
        let g = film_geometry();
        let mut o = open(4);
        let r5 = rates(&g, &o, Sample::Film, &TRUTH, 400.0).icxrd;
        o.amp_range = 4;
        let r4 = rates(&g, &o, Sample::Film, &TRUTH, 400.0).icxrd;
        assert!((r4 / r5 - 10.0).abs() < 1e-9);
        o.amp_offset = 1.0;
        let off = rates(&g, &o, Sample::Film, &TRUTH, 400.0).icxrd;
        assert!((off - r4 - 1e4).abs() < 1e-6);
    }
}
