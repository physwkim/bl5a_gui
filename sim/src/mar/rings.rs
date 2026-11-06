//! The powder rings each sample diffracts into, with d-spacings that follow
//! the sample's state: the film's lattice expands with temperature and jumps
//! at a phase transition, the coin cell's cathode breathes as it cycles.

use crate::beam::Sample;

/// Thermal expansion of the film lattice, 1/K.
const FILM_ALPHA: f64 = 1.2e-5;
/// Above this temperature (°C) the film's lattice is 0.4 % larger.
const FILM_TRANSITION: f64 = 120.0;
/// One full charge–discharge cycle of the coin cell, seconds.
const CYCLE: f64 = 7200.0;
/// The aluminium current collector, fcc, Å.
const AL_A: f64 = 4.0495;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub phase: &'static str,
    pub hkl: [i32; 3],
    /// Å.
    pub d: f64,
    /// Peak height relative to the strongest ring of the sample.
    pub strength: f64,
}

/// The rings at `temperature` (°C), `elapsed` seconds into the run.
pub fn rings(sample: Sample, temperature: f64, elapsed: f64) -> Vec<Ring> {
    match sample {
        Sample::Film => film(temperature),
        Sample::Coin => coin(elapsed),
    }
}

fn cubic(a: f64, [h, k, l]: [i32; 3]) -> f64 {
    a / f64::from(h * h + k * k + l * l).sqrt()
}

fn hexagonal(a: f64, c: f64, [h, k, l]: [i32; 3]) -> f64 {
    let inv = 4.0 / 3.0 * f64::from(h * h + h * k + k * k) / (a * a) + f64::from(l * l) / (c * c);
    1.0 / inv.sqrt()
}

/// A perovskite-like cubic film.
fn film(temperature: f64) -> Vec<Ring> {
    let mut a = 3.950 * (1.0 + FILM_ALPHA * (temperature - 25.0));
    if temperature >= FILM_TRANSITION {
        a *= 1.004;
    }
    [
        ([1, 0, 0], 0.35),
        ([1, 1, 0], 1.0),
        ([1, 1, 1], 0.25),
        ([2, 0, 0], 0.45),
        ([2, 1, 0], 0.15),
        ([2, 1, 1], 0.4),
    ]
    .into_iter()
    .map(|(hkl, strength)| Ring {
        phase: "film",
        hkl,
        d: cubic(a, hkl),
        strength,
    })
    .collect()
}

/// State of charge, a triangle wave from 0 at the start of the run.
fn state_of_charge(elapsed: f64) -> f64 {
    1.0 - (2.0 * (elapsed / CYCLE).fract() - 1.0).abs()
}

/// A layered LiCoO2-like cathode (R-3m, hexagonal axes) on aluminium foil.
fn coin(elapsed: f64) -> Vec<Ring> {
    let soc = state_of_charge(elapsed);
    let (a, c) = (2.816 - 0.009 * soc, 14.05 + 0.35 * soc);
    let cathode = [
        ([0, 0, 3], 1.0),
        ([1, 0, 1], 0.35),
        ([0, 0, 6], 0.1),
        ([0, 1, 2], 0.15),
        ([1, 0, 4], 0.6),
        ([0, 1, 5], 0.15),
    ]
    .into_iter()
    .map(|(hkl, strength)| Ring {
        phase: "cathode",
        hkl,
        d: hexagonal(a, c, hkl),
        strength,
    });
    let foil = [([1, 1, 1], 0.5), ([2, 0, 0], 0.25)]
        .into_iter()
        .map(|(hkl, strength)| Ring {
            phase: "Al",
            hkl,
            d: cubic(AL_A, hkl),
            strength,
        });
    cathode.chain(foil).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(rings: &[Ring], hkl: [i32; 3]) -> f64 {
        rings.iter().find(|r| r.hkl == hkl).unwrap().d
    }

    #[test]
    fn film_expands_and_jumps_at_the_transition() {
        let cold = film(25.0);
        assert!((d(&cold, [1, 0, 0]) - 3.95).abs() < 1e-12);
        assert!((d(&cold, [1, 1, 0]) - 3.95 / 2f64.sqrt()).abs() < 1e-12);
        let below = d(&film(119.9), [1, 0, 0]);
        let above = d(&film(120.0), [1, 0, 0]);
        assert!(below > 3.95 && (above / below - 1.004).abs() < 1e-5);
    }

    #[test]
    fn coin_breathes_over_a_cycle() {
        assert_eq!(state_of_charge(0.0), 0.0);
        assert!((state_of_charge(CYCLE / 2.0) - 1.0).abs() < 1e-12);
        assert!((state_of_charge(CYCLE / 4.0) - 0.5).abs() < 1e-12);
        let (start, mid) = (coin(0.0), coin(CYCLE / 2.0));
        assert!((d(&start, [0, 0, 3]) - 14.05 / 3.0).abs() < 1e-12);
        assert!((d(&mid, [0, 0, 3]) - 14.40 / 3.0).abs() < 1e-12);
        // (101) on hexagonal axes: 1/d² = 4/(3a²) + 1/c².
        let want = 1.0 / (4.0 / (3.0 * 2.816f64.powi(2)) + 1.0 / 14.05f64.powi(2)).sqrt();
        assert!((d(&start, [1, 0, 1]) - want).abs() < 1e-12);
        assert_eq!(d(&start, [2, 0, 0]), d(&mid, [2, 0, 0]));
    }
}
