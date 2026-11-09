//! The plate's geometry refined on a frame of a calibrant, the way
//! Dioptas's automatic refinement does it (rsfai-calib's `auto_refine`):
//! from a start PONI, the peaks in a narrow band around each ring in
//! turn, the geometry refined on all of them after each ring.

use rsfai_calib::{AutoRefine, GeometryParams, GeometryRefinement, Param, RingSearch, auto_refine};
use rsfai_calibrant::Calibrant;
use rsfai_geometry::PoniFile;
use serde_json::{Value, json};

use crate::engine::Scan;
use crate::mar::{self, Image};

/// The peaks' visit order: one frame and request give one result.
const SEED: u64 = 1;

const PARAMS: [(&str, Param); 6] = [
    ("dist", Param::Dist),
    ("poni1", Param::Poni1),
    ("poni2", Param::Poni2),
    ("rot1", Param::Rot1),
    ("rot2", Param::Rot2),
    ("rot3", Param::Rot3),
];

pub struct Request {
    /// The mar345 frame.
    path: String,
    /// The full plate's start PONI.
    poni: String,
    /// The calibrant's d-spacings, a pyFAI `.D` file.
    calibrant: String,
    how: AutoRefine,
    /// `(y, x, ring)` picked by hand on the frame, refined on before the
    /// search.
    points: Vec<(f64, f64, usize)>,
}

impl Request {
    pub fn parse(req: &Value) -> Result<Request, String> {
        let text = |k: &str| {
            req.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("calibrate needs {k}, a text"))
        };
        let number = |k: &str| {
            req.get(k)
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("calibrate needs {k}, a number"))
        };
        let rings = req
            .get("rings")
            .and_then(Value::as_u64)
            .ok_or("calibrate needs rings, a whole number")? as usize;
        let list = |k: &str| match req.get(k) {
            None => Ok(Vec::new()),
            Some(v) => v.as_array().cloned().ok_or(format!("{k} is a list")),
        };
        let fixed = list("fixed")?
            .iter()
            .map(|v| {
                let name = v.as_str().unwrap_or_default();
                PARAMS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|&(_, p)| p)
                    .ok_or(format!(
                        "fixed {v}: one of dist, poni1, poni2, rot1, rot2, rot3"
                    ))
            })
            .collect::<Result<Vec<Param>, String>>()?;
        if PARAMS.iter().all(|(_, p)| fixed.contains(p)) {
            return Err("every parameter is fixed".into());
        }
        let points = list("points")?
            .iter()
            .map(|v| match v.as_array().map(Vec::as_slice) {
                Some([y, x, r]) => match (y.as_f64(), x.as_f64(), r.as_u64()) {
                    (Some(y), Some(x), Some(r)) => Ok((y, x, r as usize)),
                    _ => Err(format!("point {v}: not [y, x, ring]")),
                },
                _ => Err(format!("point {v}: not [y, x, ring]")),
            })
            .collect::<Result<_, _>>()?;
        Ok(Request {
            path: text("path")?,
            poni: text("poni")?,
            calibrant: text("calibrant")?,
            how: AutoRefine {
                search: RingSearch {
                    delta_tth_deg: number("delta_tth")?,
                    min_mean_factor: number("factor")?,
                    upper_limit: number("upper")?,
                },
                rings,
                fixed,
                seed: SEED,
            },
            points,
        })
    }
}

/// The frame's path, and the PONI, chi2 and peaks [`refine`] gives.
pub fn run(req: &Request) -> Result<Value, String> {
    let bytes = std::fs::read(&req.path).map_err(|e| format!("{}: {e}", req.path))?;
    let image = mar::decode(&bytes).map_err(|e| format!("{}: {e}", req.path))?;
    let mut done = refine(&image, req)?;
    done["path"] = json!(req.path);
    Ok(done)
}

/// The refined full-plate PONI, its chi2 (rad²) and the peaks
/// `[y, x, ring]` it was refined on, in the frame's pixels.
fn refine(image: &Image, req: &Request) -> Result<Value, String> {
    let poni = PoniFile::parse(&req.poni).map_err(|e| e.to_string())?;
    let wavelength = poni.wavelength.ok_or("the PONI has no Wavelength")?;
    let calibrant = Calibrant::from_dspacing_file_str(&req.calibrant);
    let mut lit = calibrant.clone();
    lit.set_wavelength(wavelength);
    let visible = lit.get_2th().len();
    if visible == 0 {
        return Err("the calibrant has no ring at this wavelength".into());
    }
    if let Some(p) = req.points.iter().find(|p| p.2 >= visible) {
        return Err(format!("point {p:?}: the calibrant has {visible} rings"));
    }
    let scan = Scan::new(&poni, image.pixels, image.pixel_um)?;
    let mask: Vec<bool> = scan.off_plate().collect();
    let mut start = GeometryParams {
        dist: poni.dist,
        poni1: poni.poni1 - scan.inset,
        poni2: poni.poni2 - scan.inset,
        rot1: poni.rot1,
        rot2: poni.rot2,
        rot3: poni.rot3,
        wavelength,
    };
    if !req.points.is_empty() {
        let mut picked = GeometryRefinement::new(
            req.points.clone(),
            calibrant.clone(),
            scan.detector.clone(),
            start,
        );
        picked.refine_with_fixed(&req.how.fixed);
        start = picked.param();
    }
    let cal = auto_refine(
        &image.data,
        Some(&mask),
        &calibrant,
        &scan.detector,
        start,
        &req.how,
    );
    if cal.points.is_empty() {
        return Err("no peak on any ring".into());
    }
    let p = cal.param;
    let refined = [
        p.dist,
        p.poni1 + scan.inset,
        p.poni2 + scan.inset,
        p.rot1,
        p.rot2,
        p.rot3,
    ];
    let points: Vec<Value> = cal
        .points
        .iter()
        .map(|&(y, x, r)| json!([y, x, r]))
        .collect();
    Ok(json!({ "poni": rewrite(&req.poni, &refined), "chi2": cal.chi2, "points": points }))
}

/// `text` with its Distance, Poni1, Poni2, Rot1, Rot2 and Rot3 lines
/// holding `values`, read the way `PoniFile::parse` reads them; the other
/// lines as they were.
fn rewrite(text: &str, values: &[f64; 6]) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        let key = trimmed
            .split_once(':')
            .filter(|_| !trimmed.starts_with('#'))
            .map(|(k, _)| k.trim());
        let at = key.and_then(|k| match k.to_ascii_lowercase().as_str() {
            "distance" | "dist" => Some(0),
            "poni1" => Some(1),
            "poni2" => Some(2),
            "rot1" => Some(3),
            "rot2" => Some(4),
            "rot3" => Some(5),
            _ => None,
        });
        match (key, at) {
            (Some(k), Some(i)) => out += &format!("{k}: {}", values[i]),
            _ => out += line,
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use rsfai_geometry::{Space, calc_pos_zyx, equation};

    const PONI: &str = include_str!("../../sim/bl5a.poni");
    /// bl5a-sim's film: cubic P, a = 3.95 Å; rings 1-6 reach the plate.
    const RINGS: [f64; 6] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    const A: f64 = 3.95;
    /// A 1150 x 300 µm scan: the whole plate, a quarter of a 2300 scan's
    /// pixels.
    const N: usize = 1150;
    const UM: u32 = 300;

    fn calibrant() -> String {
        RINGS
            .iter()
            .map(|n| format!("{}\n", A / n.sqrt()))
            .collect()
    }

    /// 2θ (rad) at each pixel centre under `poni`.
    fn tth(poni: &str) -> Vec<f64> {
        let poni = PoniFile::parse(poni).unwrap();
        let scan = Scan::new(&poni, N, UM).unwrap();
        let pix = scan.detector.pixel1;
        let centre = |i: usize| (i as f64 + 0.5) * pix;
        let (p1, p2): (Vec<f64>, Vec<f64>) =
            (0..N * N).map(|k| (centre(k / N), centre(k % N))).unzip();
        let pos = calc_pos_zyx(
            poni.dist,
            poni.poni1 - scan.inset,
            poni.poni2 - scan.inset,
            poni.rot1,
            poni.rot2,
            poni.rot3,
            &p1,
            &p2,
            None,
            3,
        );
        (0..N * N)
            .map(|k| equation(Space::TwoTheta, pos.x[k], pos.y[k], pos.z[k], 0.0))
            .collect()
    }

    /// The rings' 2θ at the PONI's wavelength.
    fn ring_tth() -> Vec<f64> {
        let lambda = PoniFile::parse(PONI).unwrap().wavelength.unwrap() * 1e10;
        RINGS
            .iter()
            .map(|n| 2.0 * (lambda / (2.0 * A / n.sqrt())).asin())
            .collect()
    }

    /// The film's rings under `PONI`, Gaussian in 2θ on a flat background.
    fn frame(tth: &[f64]) -> Image {
        let rings = ring_tth();
        let sigma = 0.05f64.to_radians();
        let data = tth
            .iter()
            .map(|&t| {
                let peaks: f64 = rings
                    .iter()
                    .map(|r| 1000.0 * (-0.5 * ((t - r) / sigma).powi(2)).exp())
                    .sum();
                (20.0 + peaks) as f32
            })
            .collect();
        Image {
            pixels: N,
            pixel_um: UM,
            data,
        }
    }

    /// `PONI` with the geometry moved off by `off` (dist, poni1, poni2,
    /// rot1, rot2, rot3).
    fn moved(off: [f64; 6]) -> String {
        let p = PoniFile::parse(PONI).unwrap();
        let at = [p.dist, p.poni1, p.poni2, p.rot1, p.rot2, p.rot3];
        rewrite(PONI, &std::array::from_fn(|k| at[k] + off[k]))
    }

    fn request(poni: &str, extra: Value) -> Request {
        let mut req = json!({
            "path": "", "poni": poni, "calibrant": calibrant(), "rings": 15,
            "delta_tth": 0.1, "factor": 1.0, "upper": 1e9, "fixed": ["rot3"],
        });
        req.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        Request::parse(&req).unwrap()
    }

    /// A fifth of a pixel's 2θ span (300 µm at 0.3 m): the refined
    /// 2θ map's measured gap is 5e-5 rad.
    const NEAR: f64 = 5e-5 * 4.0;

    /// The largest 2θ gap (rad) over the plate between the refined PONI
    /// and `PONI`: with every ring within 30° a beam centre and a tilt
    /// trade off, the 2θ they give does not.
    fn gap(done: &Value, truth: &[f64]) -> f64 {
        let ours = tth(done["poni"].as_str().unwrap());
        ours.iter()
            .zip(truth)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn a_rewritten_poni_reads_back_its_values_and_keeps_the_rest() {
        let values = [0.30123, 0.1734, 0.17185, 0.0042, -0.0031, 1e-5];
        let text = rewrite(PONI, &values);
        let p = PoniFile::parse(&text).unwrap();
        assert_eq!([p.dist, p.poni1, p.poni2, p.rot1, p.rot2, p.rot3], values);
        let q = PoniFile::parse(PONI).unwrap();
        assert_eq!((p.wavelength, p.orientation), (q.wavelength, q.orientation));
        assert_eq!(p.detector_config, q.detector_config);
        assert_eq!(text.lines().count(), PONI.lines().count());
    }

    /// From 2 mm and 1 mrad off, the search finds every ring and lands
    /// on the geometry the frame was drawn with.
    #[test]
    fn a_start_near_the_truth_refines_onto_it() {
        let tth = tth(PONI);
        let start = moved([2e-3, 0.4e-3, -0.5e-3, -1e-3, 1e-3, 0.0]);
        let done = refine(&frame(&tth), &request(&start, json!({}))).unwrap();
        let rings: std::collections::BTreeSet<u64> = done["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p[2].as_u64().unwrap())
            .collect();
        assert_eq!(rings.len(), RINGS.len(), "rings {rings:?}");
        let g = gap(&done, &tth);
        assert!(g < NEAR, "{g:e} rad");
    }

    /// 15 mm off the beam centre on both axes the search alone settles
    /// 0.07 rad off; points picked on two rings bring the start within
    /// its reach.
    #[test]
    fn picked_points_bring_a_far_start_within_reach() {
        let tth = tth(PONI);
        let image = frame(&tth);
        let start = moved([0.0, 15e-3, -15e-3, 0.0, 0.0, 0.0]);
        let alone = refine(&image, &request(&start, json!({})));
        let rings = ring_tth();
        // The pixel nearest each ring in eight sectors around the plate.
        let mut best = [[(f64::INFINITY, 0usize); 8]; 2];
        for (k, &t) in tth.iter().enumerate() {
            let (y, x) = (
                (k / N) as f64 - N as f64 / 2.0,
                (k % N) as f64 - N as f64 / 2.0,
            );
            let sector =
                ((y.atan2(x) + std::f64::consts::PI) / std::f64::consts::TAU * 8.0) as usize % 8;
            for (ring, b) in best.iter_mut().enumerate() {
                let off = (t - rings[ring]).abs();
                if off < b[sector].0 {
                    b[sector] = (off, k);
                }
            }
        }
        let points: Vec<Value> = best
            .iter()
            .enumerate()
            .flat_map(|(ring, b)| b.iter().map(move |&(_, k)| json!([k / N, k % N, ring])))
            .collect();
        let done = refine(&image, &request(&start, json!({ "points": points }))).unwrap();
        let g = gap(&done, &tth);
        assert!(g < NEAR, "{g:e} rad");
        let lost = alone.map_or(f64::INFINITY, |d| gap(&d, &tth));
        assert!(lost > 100.0 * NEAR, "found without points: {lost:e} rad");
    }

    #[test]
    fn a_request_that_cannot_refine_is_refused() {
        let all = json!({"fixed": ["dist", "poni1", "poni2", "rot1", "rot2", "rot3"]});
        let bad = |extra: Value| {
            let mut req = json!({
                "path": "", "poni": PONI, "calibrant": calibrant(), "rings": 15,
                "delta_tth": 0.1, "factor": 1.0, "upper": 1e9,
            });
            req.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            Request::parse(&req).err()
        };
        assert!(bad(all).unwrap().contains("every parameter"));
        assert!(bad(json!({"fixed": ["tilt"]})).unwrap().contains("tilt"));
        assert!(
            bad(json!({"points": [[1.0, 2.0]]}))
                .unwrap()
                .contains("[y, x, ring]")
        );
        let past = request(PONI, json!({"points": [[1.0, 2.0, 6]]}));
        let image = Image {
            pixels: N,
            pixel_um: UM,
            data: vec![0.0; N * N],
        };
        assert!(refine(&image, &past).unwrap_err().contains("6 rings"));
    }
}
