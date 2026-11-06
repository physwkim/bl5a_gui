//! One rsfai integrator per mar345 scan mode, all from one PONI that
//! describes the full 345 mm plate: a smaller scan keeps the plate centre,
//! so its pixel origin lies (0.345 m − diameter) / 2 inside on both axes.
//! Every mode bins on one q axis, so a series can mix modes.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::f64::consts::TAU;

use rsfai::{AzimuthalIntegrator, Corrections, DetectorModel, ErrorModelKind, IntegrationOptions};
use rsfai_geometry::{PoniFile, Space, Unit, calc_pos_zyx, equation};

use crate::mar::Image;

/// The plate diameter, m.
const PLATE: f64 = 0.345;
/// The q axis ends at the plate's far edge rounded up to this step, Å⁻¹.
const Q_STEP: f64 = 0.01;
pub const UNIT: &str = "q_A^-1";

pub struct Engine {
    poni: PoniFile,
    /// The PONI file as read; every row records it.
    pub poni_text: String,
    pub npt: usize,
    /// Å⁻¹.
    pub q_max: f64,
    plates: HashMap<(usize, u32), Plate>,
}

/// One scan mode: its integrator and the pixels outside the round plate.
struct Plate {
    ai: AzimuthalIntegrator,
    mask: Vec<i8>,
}

#[derive(Debug)]
pub struct Profile {
    pub intensity: Vec<f32>,
    pub sigma: Vec<f32>,
}

impl Engine {
    pub fn new(poni_text: String, npt: usize) -> Result<Engine, String> {
        let poni = PoniFile::parse(&poni_text).map_err(|e| e.to_string())?;
        let wavelength = poni.wavelength.ok_or("the PONI has no Wavelength")?;
        if npt == 0 {
            return Err("npt is 0".into());
        }
        let q_max = (plate_q_max(&poni, wavelength) / Q_STEP).ceil() * Q_STEP;
        Ok(Engine {
            poni,
            poni_text,
            npt,
            q_max,
            plates: HashMap::new(),
        })
    }

    /// Bin centres, Å⁻¹.
    pub fn q(&self) -> Vec<f64> {
        let step = self.q_max / self.npt as f64;
        (0..self.npt).map(|k| (k as f64 + 0.5) * step).collect()
    }

    /// `image` averaged over each q ring and divided by the monitor `mon`;
    /// NaN in a bin the scan has no pixel in.
    pub fn integrate(&mut self, image: &Image, mon: f64) -> Result<Profile, String> {
        if mon.is_nan() || mon <= 0.0 {
            return Err(format!("monitor {mon}, not positive"));
        }
        let plate = match self.plates.entry((image.pixels, image.pixel_um)) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(Plate::new(&self.poni, image.pixels, image.pixel_um)?),
        };
        let opts = IntegrationOptions {
            error_model: ErrorModelKind::Poisson,
            radial_range: Some((0.0, self.q_max)),
            ..IntegrationOptions::default()
        };
        let corr = Corrections {
            mask: Some(&plate.mask),
            ..Corrections::with_normalization(mon)
        };
        let r = plate
            .ai
            .integrate1d_with(&image.data, self.npt, Unit::Q_A_INV, &opts, &corr);
        // The stored axis is q(); rsfai's centres must be the same bins.
        let step = self.q_max / self.npt as f64;
        if let Some((k, q)) = r
            .radial
            .iter()
            .enumerate()
            .find(|&(k, q)| (q - (k as f64 + 0.5) * step).abs() > 1e-3 * step)
        {
            return Err(format!(
                "rsfai bin {k} is centred at {q}, not on the q axis"
            ));
        }
        // A bin no unmasked pixel falls in (past a small scan's circle)
        // was not measured: NaN, not rsfai's 0.
        let measured = |v: Vec<f32>| -> Vec<f32> {
            v.into_iter()
                .zip(&r.count)
                .map(|(v, &c)| if c > 0.0 { v } else { f32::NAN })
                .collect()
        };
        Ok(Profile {
            intensity: measured(r.intensity),
            sigma: measured(r.sigma),
        })
    }
}

impl Plate {
    fn new(poni: &PoniFile, n: usize, um: u32) -> Result<Plate, String> {
        let pix = f64::from(um) * 1e-6;
        let diameter = n as f64 * pix;
        if diameter > PLATE + 1e-9 {
            return Err(format!("a {n} x {um} µm scan is wider than the plate"));
        }
        let inset = (PLATE - diameter) / 2.0;
        let mut detector = DetectorModel::generic(pix, pix, (n, n));
        detector.orientation = poni.orientation.unwrap_or(3);
        let mut ai = AzimuthalIntegrator::from_poni(poni, detector);
        ai.poni1 -= inset;
        ai.poni2 -= inset;
        let r = diameter / 2.0;
        let centre = |i: usize| (i as f64 + 0.5) * pix;
        let mask = (0..n * n)
            .map(|k| i8::from((centre(k / n) - r).hypot(centre(k % n) - r) > r))
            .collect();
        Ok(Plate { ai, mask })
    }
}

/// The largest q on the full plate's rim, Å⁻¹.
fn plate_q_max(poni: &PoniFile, wavelength: f64) -> f64 {
    let r = PLATE / 2.0;
    let (p1, p2): (Vec<f64>, Vec<f64>) = (0..3600)
        .map(|k| {
            let a = f64::from(k) * TAU / 3600.0;
            (r + r * a.sin(), r + r * a.cos())
        })
        .unzip();
    let pos = calc_pos_zyx(
        poni.dist,
        poni.poni1,
        poni.poni2,
        poni.rot1,
        poni.rot2,
        poni.rot3,
        &p1,
        &p2,
        None,
        poni.orientation.unwrap_or(3),
    );
    (0..p1.len())
        .map(|k| equation(Space::Q, pos.x[k], pos.y[k], pos.z[k], wavelength) * Unit::Q_A_INV.scale)
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PONI: &str = include_str!("../../sim/bl5a.poni");

    /// A 1200 x 150 µm frame with one ring at `d` Å, Gaussian in 2θ.
    fn ring(engine: &Engine, d: f64, counts: f64) -> Image {
        let (n, pix) = (1200usize, 150e-6);
        let poni = &engine.poni;
        let inset = (PLATE - n as f64 * pix) / 2.0;
        let lambda = poni.wavelength.unwrap();
        let bragg = 2.0 * (lambda * 1e10 / (2.0 * d)).asin();
        let centre = |i: usize| (i as f64 + 0.5) * pix;
        let (p1, p2): (Vec<f64>, Vec<f64>) =
            (0..n * n).map(|k| (centre(k / n), centre(k % n))).unzip();
        let pos = calc_pos_zyx(
            poni.dist,
            poni.poni1 - inset,
            poni.poni2 - inset,
            poni.rot1,
            poni.rot2,
            poni.rot3,
            &p1,
            &p2,
            None,
            3,
        );
        let sigma = 0.04f64.to_radians();
        let data = (0..n * n)
            .map(|k| {
                let tth = equation(Space::TwoTheta, pos.x[k], pos.y[k], pos.z[k], 0.0);
                (counts * (-0.5 * ((tth - bragg) / sigma).powi(2)).exp()) as f32
            })
            .collect();
        Image {
            pixels: n,
            pixel_um: 150,
            data,
        }
    }

    #[test]
    fn the_axis_reaches_the_plate_rim() {
        let e = Engine::new(PONI.into(), 2000).unwrap();
        // λ 0.7749 Å, 0.3 m to a 0.1725 m radius: 2θ ≈ 30°, q ≈ 4.2 Å⁻¹.
        assert!(e.q_max > 4.0 && e.q_max < 4.6, "q_max {}", e.q_max);
        assert_eq!(e.q().len(), 2000);
    }

    /// The peak bin of a ring at d is the bin 2π/d falls in, its height
    /// the ring's counts over the monitor and the solid angle; past the
    /// scan's circle there are no rows' values.
    #[test]
    fn a_ring_peaks_at_two_pi_over_d() {
        let mut e = Engine::new(PONI.into(), 2000).unwrap();
        // 2θ ≈ 12.7°, 68 mm from the beam: inside the 90 mm scan circle.
        let (d, counts, mon) = (3.5, 1000.0, 10.0);
        let p = e.integrate(&ring(&e, d, counts), mon).unwrap();
        let q = e.q();
        let k = (0..q.len())
            .filter(|&k| !p.intensity[k].is_nan())
            .max_by(|&a, &b| p.intensity[a].total_cmp(&p.intensity[b]))
            .unwrap();
        let step = e.q_max / e.npt as f64;
        let want = std::f64::consts::TAU / d;
        assert!(
            (q[k] - want).abs() <= step,
            "peak {} vs {want}, bin {step}",
            q[k]
        );
        let tth = 2.0 * (e.poni.wavelength.unwrap() * 1e10 / (2.0 * d)).asin();
        let top = counts / mon / tth.cos().powi(3);
        let ratio = f64::from(p.intensity[k]) / top;
        assert!(ratio > 0.9 && ratio < 1.01, "{} vs {top}", p.intensity[k]);
        // 90 mm at 0.3 m: no pixel past q ≈ 2.4 Å⁻¹.
        let last = q.iter().position(|&v| v > 2.5).unwrap();
        assert!(p.intensity[last..].iter().all(|v| v.is_nan()));
        assert!(p.sigma[last..].iter().all(|v| v.is_nan()));
        assert!(!p.intensity[k].is_nan() && !p.sigma[k].is_nan());
    }

    #[test]
    fn a_monitor_that_is_not_positive_is_refused() {
        let mut e = Engine::new(PONI.into(), 100).unwrap();
        let img = Image {
            pixels: 1200,
            pixel_um: 150,
            data: vec![1.0; 1200 * 1200],
        };
        assert!(e.integrate(&img, 0.0).is_err());
        assert!(e.integrate(&img, f64::NAN).is_err());
    }

    #[test]
    fn a_scan_wider_than_the_plate_is_refused() {
        let mut e = Engine::new(PONI.into(), 100).unwrap();
        let img = Image {
            pixels: 3000,
            pixel_um: 150,
            data: vec![0.0; 9],
        };
        assert!(e.integrate(&img, 1.0).unwrap_err().contains("wider"));
    }
}
