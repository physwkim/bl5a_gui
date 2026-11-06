//! The frame a scan reads out: powder rings at the PONI geometry, scaled by
//! the photons the sample saw while the shutter was open, with Poisson noise.

use std::collections::HashMap;
use std::sync::Arc;

use rsfai_geometry::{PoniFile, Space, calc_pos_zyx, equation};

use super::rings::Ring;
use crate::beam::Rng;
use crate::world::Dose;

/// The plate diameter the PONI describes, m.
const PLATE: f64 = 0.345;
/// Plate counts per photon on the sample at the top of the strongest ring.
const COUNTS_PER_PHOTON: f64 = 2e-8;
/// Mean counts of an unexposed pixel.
const DARK: f64 = 8.0;
/// Diffuse background at 2θ = 0 relative to the strongest ring, and its
/// fall-off, degrees.
const BACKGROUND: f64 = 0.02;
const BACKGROUND_FALL: f64 = 8.0;
/// Ring width (Gaussian σ in 2θ), degrees.
const SIGMA: f64 = 0.04;
/// The beamstop shadow, 2θ in degrees.
const BEAMSTOP: f64 = 0.6;
/// Step of the 2θ profile table, degrees.
const STEP: f64 = 0.002;
/// Rows of pixel coordinates transformed at a time.
const BLOCK: usize = 64;

/// A scan mode: the `COMMAND CHANGE` argument and the pixel it implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Mode {
    pub pixels: usize,
    /// µm.
    pub pixel_um: u32,
}

impl Mode {
    /// The driver's `imageSizes`: row 0 at 100 µm, row 1 at 150 µm.
    pub fn from_pixels(n: i64) -> Option<Mode> {
        let pixel_um = match n {
            1800 | 2400 | 3000 | 3450 => 100,
            1200 | 1600 | 2000 | 2300 => 150,
            _ => return None,
        };
        Some(Mode {
            pixels: n as usize,
            pixel_um,
        })
    }

    pub fn pixel(self) -> f64 {
        f64::from(self.pixel_um) * 1e-6
    }

    pub fn diameter(self) -> f64 {
        self.pixels as f64 * self.pixel()
    }

    /// How far this mode's pixel origin lies inside the full plate's, m.
    fn inset(self) -> f64 {
        (PLATE - self.diameter()) / 2.0
    }
}

pub struct Scene {
    poni: PoniFile,
    /// Å.
    pub wavelength: f64,
    /// 2θ (degrees) of every pixel per mode; NaN off the plate.
    maps: HashMap<Mode, Arc<[f32]>>,
    rng: Rng,
}

impl Scene {
    pub fn load(path: &str, seed: u64) -> Result<Scene, String> {
        let poni = PoniFile::load(path).map_err(|e| format!("{path}: {e}"))?;
        let wavelength = poni.wavelength.ok_or(format!("{path}: no Wavelength"))? * 1e10;
        Ok(Scene {
            poni,
            wavelength,
            maps: HashMap::new(),
            rng: Rng::new(seed),
        })
    }

    /// Sample-to-plate distance, mm.
    pub fn distance(&self) -> f64 {
        self.poni.dist * 1e3
    }

    /// The point of normal incidence in `mode`'s pixels (fast, slow).
    pub fn center(&self, mode: Mode) -> (f64, f64) {
        let px = |p: f64| (p - mode.inset()) / mode.pixel();
        (px(self.poni.poni2), px(self.poni.poni1))
    }

    /// Row-major (slow, fast) counts, 0 off the round plate.
    pub fn render(&mut self, mode: Mode, dose: Dose, rings: &[Ring]) -> Vec<u32> {
        let map = self.map(mode);
        let profile = Profile::new(rings, dose.photons * COUNTS_PER_PHOTON, self.wavelength);
        map.iter()
            .map(|&tth| {
                if tth.is_nan() {
                    0
                } else {
                    let counts = self.rng.poisson(DARK + profile.at(f64::from(tth)));
                    counts.min(f64::from(i32::MAX)) as u32
                }
            })
            .collect()
    }

    fn map(&mut self, mode: Mode) -> Arc<[f32]> {
        if let Some(m) = self.maps.get(&mode) {
            return m.clone();
        }
        let m: Arc<[f32]> = tth_map(&self.poni, mode).into();
        self.maps.insert(mode, m.clone());
        m
    }
}

fn tth_map(poni: &PoniFile, mode: Mode) -> Vec<f32> {
    let (n, pix) = (mode.pixels, mode.pixel());
    let centre = |i: usize| (i as f64 + 0.5) * pix;
    let radius = mode.diameter() / 2.0;
    let orientation = poni.orientation.unwrap_or(3);
    let mut map = Vec::with_capacity(n * n);
    for first in (0..n).step_by(BLOCK) {
        let rows = first..(first + BLOCK).min(n);
        let (p1, p2): (Vec<f64>, Vec<f64>) = rows
            .flat_map(|i| (0..n).map(move |j| (centre(i), centre(j))))
            .unzip();
        let pos = calc_pos_zyx(
            poni.dist,
            poni.poni1 - mode.inset(),
            poni.poni2 - mode.inset(),
            poni.rot1,
            poni.rot2,
            poni.rot3,
            &p1,
            &p2,
            None,
            orientation,
        );
        for k in 0..p1.len() {
            let on_plate = (p1[k] - radius).hypot(p2[k] - radius) <= radius;
            map.push(if on_plate {
                equation(Space::TwoTheta, pos.x[k], pos.y[k], pos.z[k], 0.0).to_degrees() as f32
            } else {
                f32::NAN
            });
        }
    }
    map
}

/// Mean exposure counts against 2θ, tabulated.
struct Profile(Vec<f64>);

impl Profile {
    fn new(rings: &[Ring], scale: f64, wavelength: f64) -> Profile {
        let peaks: Vec<(f64, f64)> = rings
            .iter()
            .filter(|r| wavelength < 2.0 * r.d)
            .map(|r| {
                (
                    2.0 * (wavelength / (2.0 * r.d)).asin().to_degrees(),
                    r.strength,
                )
            })
            .collect();
        let table = (0..(90.0 / STEP) as usize)
            .map(|i| {
                let tth = i as f64 * STEP;
                if tth < BEAMSTOP {
                    return 0.0;
                }
                let rings: f64 = peaks
                    .iter()
                    .map(|&(at, s)| s * (-0.5 * ((tth - at) / SIGMA).powi(2)).exp())
                    .sum();
                scale * (rings + BACKGROUND * (-tth / BACKGROUND_FALL).exp())
            })
            .collect();
        Profile(table)
    }

    fn at(&self, tth: f64) -> f64 {
        let i = (tth / STEP).round() as usize;
        self.0.get(i).copied().unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beam::Sample;
    use crate::mar::rings::rings;

    fn scene() -> Scene {
        Scene::load(concat!(env!("CARGO_MANIFEST_DIR"), "/bl5a.poni"), 5).unwrap()
    }

    #[test]
    fn modes_follow_the_driver_sizes() {
        let m = Mode::from_pixels(2300).unwrap();
        assert_eq!(m.pixel_um, 150);
        assert!((m.diameter() - PLATE).abs() < 1e-12);
        assert!((Mode::from_pixels(1800).unwrap().inset() - 0.0825).abs() < 1e-12);
        assert_eq!(Mode::from_pixels(3451), None);
    }

    /// Every ring lands at its Bragg angle: the counts along a radius peak
    /// within a pixel of where 2θ = 2 asin(λ/2d).
    #[test]
    fn rings_sit_at_their_bragg_angles() {
        let mut s = scene();
        let mode = Mode::from_pixels(2300).unwrap();
        let rings = rings(Sample::Film, 25.0, 0.0);
        let dose = Dose {
            photons: 5e11,
            open: 5.0,
        };
        let img = s.render(mode, dose, &rings);
        let map = s.map(mode);
        let n = mode.pixels;
        assert_eq!(img.len(), n * n);
        assert_eq!(img[0], 0, "a corner is off the plate");
        let (cx, cy) = s.center(mode);
        let centre = cy.round() as usize * n + cx.round() as usize;
        assert!(
            img[centre] < 30,
            "the beamstop shadow holds only dark counts"
        );
        // Along the row through the centre, toward +fast.
        let row = cy.round() as usize;
        let line: Vec<(f64, f64)> = (cx.round() as usize..n)
            .map(|j| (f64::from(map[row * n + j]), f64::from(img[row * n + j])))
            .filter(|(t, _)| !t.is_nan())
            .collect();
        let edge = line.last().unwrap().0;
        let mut seen = 0;
        for r in rings.iter().filter(|r| r.strength >= 0.25) {
            let want = 2.0 * (s.wavelength / (2.0 * r.d)).asin().to_degrees();
            if want > edge - 0.2 {
                continue;
            }
            seen += 1;
            let near: Vec<&(f64, f64)> = line
                .iter()
                .filter(|(t, _)| (t - want).abs() < 0.2)
                .collect();
            let peak = near.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            let pixel_tth = (mode.pixel() / s.poni.dist).to_degrees();
            assert!(
                (peak.0 - want).abs() <= pixel_tth,
                "{:?}: peak at {} want {want}",
                r.hkl,
                peak.0
            );
            let top = 5e11 * COUNTS_PER_PHOTON * r.strength;
            assert!(
                peak.1 > 0.7 * top,
                "{:?}: {} counts, want ~{top}",
                r.hkl,
                peak.1
            );
        }
        assert_eq!(seen, 5, "rings inside {edge}°");
    }

    #[test]
    fn no_photons_leave_only_dark_counts() {
        let mut s = scene();
        let mode = Mode::from_pixels(1200).unwrap();
        let img = s.render(mode, Dose::default(), &rings(Sample::Coin, 25.0, 0.0));
        let map = s.map(mode);
        let on: Vec<f64> = (0..img.len())
            .filter(|&i| !map[i].is_nan())
            .map(|i| f64::from(img[i]))
            .collect();
        assert!(
            img.iter()
                .zip(map.iter())
                .all(|(&v, t)| !t.is_nan() || v == 0)
        );
        let mean = on.iter().sum::<f64>() / on.len() as f64;
        assert!((mean - DARK).abs() < 0.05, "mean {mean}");
        // The plate is round: about π/4 of the square is on it.
        let frac = on.len() as f64 / img.len() as f64;
        assert!((frac - std::f64::consts::FRAC_PI_4).abs() < 0.01, "{frac}");
    }
}
