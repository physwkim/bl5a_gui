//! The exposures: mymar1/mymar (`mar_series`) and a step of myinsitu
//! (`insitu_step`: the temperature macros, align_in.mac, then mymar_in).

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use bsrs::qs::{PlanFactory, PlanMeta, Registry};
use serde_json::json;

use super::align::{self, FILM_DET, FILM_SETTLE};
use super::args::Args;
use super::{Ops, exposure, file_name, item_md, optional, outer, param};
use crate::ctx::Step;
use crate::devices::Devices;

/// mymar's three `mar_ip7` lines.
const GEOMETRIES: [(&str, &str); 3] = [
    ("standard", "-85 435 100 330 50 1.2 -6.8"),
    ("high_q_asymm", "-85 435 110 330 80 1.2 -6.8"),
    ("low_q_symmetric", "160 435 36 345 54 1.2 -6.8"),
];
/// mymar's `do_sleep(2)` after its set-up and after each frame.
const SETTLE: f64 = 2.0;
/// mymar_in's th list and frame exposure (`0.7*Texpo`, Texpo −1e5).
const TH_LIST: [f64; 4] = [-0.05, 0.11, 0.13, 0.15];
const IN_TEXPO: f64 = -7e4;
const IN_GEOMETRY: &str = "25 440 42.4 340 81.8 1.2 -6.8";

struct Series {
    name: String,
    /// `Ti`, in seconds.
    period: f64,
    texpo: f64,
    n: usize,
    geometry: &'static str,
}

/// mymar: `n` frames, frame `i` from `i·Ti` after the first. Once set up
/// it waits only between frames, so a deferred pause lands before the next
/// frame, never after it.
async fn series(ops: &Ops<'_>, s: &Series) -> Step {
    ops.spec("mar345closeshutter").await?;
    ops.umv(&[("scell", 0.0)]).await?;
    ops.spec("counters 2 3").await?;
    ops.spec("mar345closeshutter").await?;
    ops.spec("fi 0 1 0 0").await?;
    ops.spec("ic_range 4").await?;
    ops.sleep(SETTLE).await?;
    ops.spec(&format!("mar_ip7 {}", s.geometry)).await?;
    ops.umv(&[("scell", 0.0)]).await?;
    ops.between_frames(SETTLE).await?;
    let t0 = Instant::now();
    for i in 0..s.n {
        if i > 0 {
            let elapsed = t0.elapsed().as_secs_f64();
            ops.between_frames(start_delay(i, elapsed, s.period))
                .await?;
        }
        ops.expose(&s.name, s.texpo).await?;
        ops.between_frames(SETTLE).await?;
    }
    Ok(())
}

/// Seconds until frame `i` starts, `elapsed` after the first did: at
/// `i·period`, or at the next boundary when the frames before ran over
/// it, so the frames stay on the grid.
fn start_delay(i: usize, elapsed: f64, period: f64) -> f64 {
    if period <= 0.0 {
        return 0.0;
    }
    let j = (i as f64).max((elapsed / period).ceil());
    j * period - elapsed
}

/// mymar_in: to the GIWAXS geometry, a frame per th, back to the
/// alignment position.
async fn mymar_in(ops: &Ops<'_>, name: &str, th_list: &[f64], texpo: f64) -> Step {
    if ops.position("tth").await? < 40.0 {
        ops.spec("mar345closeshutter").await?;
        ops.umv(&[("tth", 45.0), ("th", 0.0)]).await?; // uan 45 0
        ops.umv(&[
            ("tth", 81.0),
            ("th", 0.0),
            ("scell", 0.0),
            ("martran", 25.0),
            ("marsdd", 440.0),
            ("marshut", 340.0),
        ])
        .await?;
    }
    ops.spec("mar345closeshutter").await?;
    ops.umv(&[("tth", 81.0), ("th", 0.0)]).await?; // uan 81 0
    ops.spec("counters 2 3").await?;
    ops.spec("mar345closeshutter").await?;
    ops.spec("fi 1 0 0 0").await?;
    ops.spec("ic_range 5").await?;
    ops.spec("ic_range 5").await?;
    ops.spec("ic_offset 0").await?;
    ops.spec(&format!("mar_ip7 {IN_GEOMETRY}")).await?;
    ops.sleep(5.0).await?;
    ops.umv(&[("scell", 0.9)]).await?;
    for &th in th_list {
        ops.umv(&[("th", th)]).await?;
        ops.expose(name, texpo).await?;
    }
    ops.spec("ic_offset 1").await?;
    ops.spec("fi 1 0 1 1").await?;
    ops.umv(&[
        ("tth", 42.0),
        ("th", 0.0),
        ("martran", 1234.0),
        ("marsdd", 835.0),
        ("marshut", 0.0),
        ("scell", 6.0),
    ])
    .await?;
    ops.umv(&[("tth", 1.0), ("th", 0.0)]).await // uan 1 0
}

/// The temperature macros myinsitu `qdo`s: `settemp` and `set_ramp` on the
/// LakeShore 331 (ser 9), `changec`. Without it the kappa state's `settemp`
/// is standard.mac's, which only sets TEMP_SP.
const TEMPERATURE_MAC: &str = "/home/bl5a/specadm/spec6.06.11/macros/local_macros/temperature.mac";
/// changec's default tolerance, °C: the sixc state's DELTA_K. The kappa
/// state has no DELTA_K, and unset changec waits for an exact reading.
const TOLERANCE: f64 = 0.3;

enum Setpoint {
    /// `settemp T`.
    Set(f64),
    /// `changec T`: settemp, then until |reading − T| ≤ tolerance.
    Wait(f64, f64),
}

/// A myinsitu temperature step, in its order: ramp, setpoint, soak.
struct Temperature {
    /// °C/min.
    ramp: Option<f64>,
    setpoint: Option<Setpoint>,
    /// `loopscan n t`: SPEC records the temperature (att) n times.
    soak: Option<(usize, f64)>,
}

impl Temperature {
    fn parse(plan: &str, args: &Args<'_>) -> Result<Temperature, String> {
        let ramp = args.f64_opt("ramp")?;
        if ramp.is_some_and(|r| r <= 0.0) {
            return Err(format!("{plan}: ramp must be °C/min > 0"));
        }
        let wait = args.bool_or("wait", false)?;
        if !wait && args.given("tolerance") {
            return Err(format!("{plan}: tolerance is for wait"));
        }
        let setpoint = match (args.f64_opt("T")?, wait) {
            (None, false) => None,
            (None, true) => return Err(format!("{plan}: wait needs T")),
            (Some(sp), false) => Some(Setpoint::Set(sp)),
            (Some(sp), true) => match args.f64_or("tolerance", TOLERANCE)? {
                tol if tol > 0.0 => Some(Setpoint::Wait(sp, tol)),
                _ => return Err(format!("{plan}: tolerance must be °C > 0")),
            },
        };
        let soak = match args
            .given("soak")
            .then(|| args.f64_list("soak"))
            .transpose()?
            .as_deref()
        {
            None => None,
            Some(&[n, t]) if n >= 1.0 && n.fract() == 0.0 && t > 0.0 => Some((n as usize, t)),
            Some(_) => {
                return Err(format!(
                    "{plan}: soak must be (n, t): n points >= 1, t seconds > 0"
                ));
            }
        };
        Ok(Temperature {
            ramp,
            setpoint,
            soak,
        })
    }

    /// The SPEC command lines, in order.
    fn commands(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.ramp.is_some() || self.setpoint.is_some() {
            lines.push(format!("qdo {TEMPERATURE_MAC}"));
        }
        if let Some(ramp) = self.ramp {
            lines.push(format!("set_ramp {ramp}"));
        }
        match self.setpoint {
            Some(Setpoint::Set(sp)) => lines.push(format!("settemp {sp}")),
            // The kappa state has no DELTA_K for changec to read.
            Some(Setpoint::Wait(sp, tol)) => {
                lines.push(format!("{{local DELTA_K; DELTA_K={tol}; changec {sp}}}"))
            }
            None => {}
        }
        if let Some((n, t)) = self.soak {
            lines.push(format!("loopscan {n} {t}"));
        }
        lines
    }
}

fn register_series(reg: &mut Registry, devices: &Devices, root: &Path) {
    const PLAN: &str = "mar_series";
    const NAMES: &[&str] = &["name", "ti", "texpo", "n", "geometry", "md"];
    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse(PLAN, NAMES, raw)?;
        let name = file_name(PLAN, &args.str("name")?)?;
        let ti = args.f64_or("ti", 3.0)?;
        if !(ti.is_finite() && ti >= 0.0) {
            return Err(format!("{PLAN}: ti must be minutes >= 0, got {ti}"));
        }
        let texpo = exposure(PLAN, args.f64_or("texpo", 2.0)?)?;
        let n = args.count_or("n", 1)?;
        if n == 0 {
            return Err(format!("{PLAN}: n must be at least 1"));
        }
        let key = args.str_or("geometry", GEOMETRIES[0].0)?;
        let (_, geometry) = GEOMETRIES.iter().find(|(k, _)| *k == key).ok_or_else(|| {
            let known: Vec<_> = GEOMETRIES.iter().map(|(k, _)| *k).collect();
            format!("{PLAN}: geometry must be one of {known:?}, got {key:?}")
        })?;
        let s = Series {
            name,
            period: ti * 60.0,
            texpo,
            n,
            geometry,
        };
        let extra = json!({
            "detectors": ["scaler"],
            "det": FILM_DET,
            "name": s.name,
            "ti": ti,
            "texpo": texpo,
            "num_points": n,
            "geometry": key,
            "mar_ip7": geometry,
        });
        let (md, place) = item_md(PLAN, &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move {
                series(&Ops::new(&ctx, &dev, &place, FILM_DET, FILM_SETTLE), &s).await
            },
        ))
    });
    let geometries: Vec<_> = GEOMETRIES.iter().map(|(k, _)| *k).collect();
    reg.register_plan_with_meta(
        PLAN,
        factory,
        PlanMeta {
            description: Some(
                "mymar (n = 1: mymar1): n mar345 frames, one every ti minutes.".into(),
            ),
            parameters: vec![
                param("name", "str", "File name: 1 to 27 characters, no spaces."),
                optional(param("ti", "float", "Frame period, minutes."), "3.0"),
                optional(
                    param(
                        "texpo",
                        "float",
                        "Exposure: seconds, or monitor counts if negative.",
                    ),
                    "2.0",
                ),
                optional(param("n", "int", "Number of frames."), "1"),
                optional(
                    param(
                        "geometry",
                        "str",
                        &format!("mar_ip7 geometry: one of {geometries:?}."),
                    ),
                    "'standard'",
                ),
                optional(param("md", "dict", "Extra start-document metadata."), "{}"),
            ],
        },
    );
}

fn register_insitu(reg: &mut Registry, devices: &Devices, root: &Path) {
    const PLAN: &str = "insitu_step";
    const NAMES: &[&str] = &[
        "T",
        "ramp",
        "wait",
        "tolerance",
        "soak",
        "align",
        "th_list",
        "texpo",
        "name",
        "md",
    ];
    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse(PLAN, NAMES, raw)?;
        let temp = Temperature::parse(PLAN, &args)?;
        let align = args.bool_or("align", true)?;
        let th_list = if args.given("th_list") {
            args.f64_list("th_list")?
        } else {
            TH_LIST.to_vec()
        };
        let texpo = exposure(PLAN, args.f64_or("texpo", IN_TEXPO)?)?;
        let name = match (th_list.is_empty(), args.str_opt("name")?) {
            (true, _) => None,
            (false, Some(name)) => Some(file_name(PLAN, &name)?),
            (false, None) => {
                return Err(format!("{PLAN}: name is needed for the th_list frames"));
            }
        };
        let (t, wait, tolerance) = match temp.setpoint {
            None => (None, false, None),
            Some(Setpoint::Set(sp)) => (Some(sp), false, None),
            Some(Setpoint::Wait(sp, tol)) => (Some(sp), true, Some(tol)),
        };
        let extra = json!({
            "detectors": ["scaler"],
            "det": FILM_DET,
            "T": t,
            "ramp": temp.ramp,
            "wait": wait,
            "tolerance": tolerance,
            "soak": temp.soak,
            "align": align,
            "th_list": th_list,
            "texpo": texpo,
            "name": name,
        });
        let (md, place) = item_md(PLAN, &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move {
                let ops = Ops::new(&ctx, &dev, &place, FILM_DET, FILM_SETTLE);
                for line in temp.commands() {
                    ops.spec(&line).await?;
                }
                if align {
                    align::film(&ops).await?;
                }
                match name {
                    Some(name) => mymar_in(&ops, &name, &th_list, texpo).await,
                    None => Ok(()),
                }
            },
        ))
    });
    reg.register_plan_with_meta(
        PLAN,
        factory,
        PlanMeta {
            description: Some(
                "A step of myinsitu: set_ramp, settemp or changec, loopscan (temperature.mac), \
                 align_in.mac (align), then mymar_in (a frame per th_list)."
                    .into(),
            ),
            parameters: vec![
                optional(param("T", "float", "Setpoint, °C: settemp T."), "None"),
                optional(param("ramp", "float", "Ramp, °C/min: set_ramp."), "None"),
                optional(
                    param("wait", "bool", "Wait for T: changec T instead of settemp."),
                    "False",
                ),
                optional(
                    param(
                        "tolerance",
                        "float",
                        "With wait: |reading − T| to wait for, °C (changec's DELTA_K).",
                    ),
                    "0.3",
                ),
                optional(
                    param(
                        "soak",
                        "list",
                        "(n, t): loopscan n t, the temperature recorded by SPEC.",
                    ),
                    "None",
                ),
                optional(param("align", "bool", "Run align_film first."), "True"),
                optional(
                    param("th_list", "list", "th of each frame; [] for none."),
                    "[-0.05, 0.11, 0.13, 0.15]",
                ),
                optional(
                    param(
                        "texpo",
                        "float",
                        "Frame exposure: seconds, or monitor counts if negative.",
                    ),
                    "-70000.0",
                ),
                optional(
                    param("name", "str", "File name: 1 to 27 characters, no spaces."),
                    "None",
                ),
                optional(param("md", "dict", "Extra start-document metadata."), "{}"),
            ],
        },
    );
}

pub(super) fn register(reg: &mut Registry, devices: &Devices, root: &Path) {
    register_series(reg, devices, root);
    register_insitu(reg, devices, root);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temperature(kwargs: serde_json::Value) -> Result<Vec<String>, String> {
        const NAMES: &[&str] = &["T", "ramp", "wait", "tolerance", "soak"];
        let args = Args::parse("p", NAMES, &json!({ "kwargs": kwargs }))?;
        Ok(Temperature::parse("p", &args)?.commands())
    }

    #[test]
    fn temperature_steps_go_in_myinsitu_order() {
        let qdo = format!("qdo {TEMPERATURE_MAC}");
        assert_eq!(
            temperature(json!({"T": 30, "ramp": 10, "wait": true, "soak": [5, 1.5]})).unwrap(),
            [
                qdo.as_str(),
                "set_ramp 10",
                "{local DELTA_K; DELTA_K=0.3; changec 30}",
                "loopscan 5 1.5",
            ]
        );
        assert_eq!(
            temperature(json!({"T": -20.5, "wait": true, "tolerance": 1})).unwrap(),
            [qdo.as_str(), "{local DELTA_K; DELTA_K=1; changec -20.5}"]
        );
        assert_eq!(
            temperature(json!({"T": 25})).unwrap(),
            [qdo.as_str(), "settemp 25"]
        );
        // loopscan is standard.mac's: no qdo.
        assert_eq!(
            temperature(json!({"soak": [3600, 1]})).unwrap(),
            ["loopscan 3600 1"]
        );
        assert!(temperature(json!({})).unwrap().is_empty());
    }

    #[test]
    fn temperature_arguments_are_checked() {
        for bad in [
            json!({"wait": true}),
            json!({"T": 30, "tolerance": 0.5}),
            json!({"T": 30, "wait": true, "tolerance": 0}),
            json!({"ramp": 0}),
            json!({"soak": [0, 1]}),
            json!({"soak": [2.5, 1]}),
            json!({"soak": [2, 0]}),
            json!({"soak": [2]}),
            json!({"soak": [2, 1, 1]}),
        ] {
            assert!(temperature(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn frames_start_on_the_period_grid() {
        // On time: wait for the boundary.
        assert_eq!(start_delay(1, 100.0, 180.0), 80.0);
        // Ran over boundary 1: wait for boundary 2.
        assert_eq!(start_delay(1, 200.0, 180.0), 160.0);
        // Exactly on a boundary: go.
        assert_eq!(start_delay(2, 360.0, 180.0), 0.0);
        // No period: back to back.
        assert_eq!(start_delay(3, 50.0, 0.0), 0.0);
    }
}
