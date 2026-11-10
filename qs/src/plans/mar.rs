//! The exposures: mymar1/mymar (`mar_series`) and a step of myinsitu
//! (`insitu_step`: the temperature macros, an alignment sequence, then
//! mymar_in).

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use bsrs::qs::{PlanFactory, PlanMeta, Registry};
use serde_json::json;

use super::args::Args;
use super::{Ops, blocks, exposure, file_name, item_md, optional, outer, param};
use crate::ctx::Step;
use crate::devices::Devices;

/// The counter the frames' macros plot (align_in.mac's `plotselect det`).
const DET: &str = "det";
/// The exposure macro of each scan mode: a still frame, and the LaB6
/// rocks of th and phi over ±1° and ±5° during the exposure.
const MODES: [(&str, &str); 3] = [
    ("static", "mar345expose"),
    ("rocking1", "mar345_lab6"),
    ("rocking2", "mar345_lab6_wide"),
];
/// mymar's `do_sleep(2)` after its set-up and after each frame.
const SETTLE: f64 = 2.0;
/// mymar's `fi 0 1 0 0`: the PFCU0 blades the frames are taken behind.
const FILTERS: [u8; 4] = [0, 1, 0, 0];
/// mymar_in's th list and frame exposure (`0.7*Texpo`, Texpo −1e5).
const TH_LIST: [f64; 4] = [-0.05, 0.11, 0.13, 0.15];
const IN_TEXPO: f64 = -7e4;
const IN_GEOMETRY: &str = "25 440 42.4 340 81.8 1.2 -6.8";
/// mymar_in's tth with the mar in: spec-ioc's SpecInterlock line
/// (ioc/spec/st.cmd), where the macro has 81.
const IN_TTH: f64 = 83.0;
/// A `umv`'s motors and positions.
type Move = &'static [(&'static str, f64)];
/// mymar_in's moves in from tth below 40, to its geometry, and out. The
/// interlock moves the mar only with tth at IN_TTH or above and takes tth
/// below it only with the mar out, so the macro's move_alls of tth and
/// the mar are split: tth first on the way in, last on the way out.
const MAR_IN: [Move; 3] = [
    &[("tth", 45.0), ("th", 0.0)], // uan 45 0
    &[("tth", IN_TTH), ("th", 0.0), ("scell", 0.0)],
    &[("martran", 25.0), ("marsdd", 440.0), ("marshut", 340.0)],
];
const AT_IN: Move = &[("tth", IN_TTH), ("th", 0.0)]; // uan 81 0
const MAR_OUT: [Move; 3] = [
    &[("martran", 1234.0), ("marsdd", 835.0), ("marshut", 0.0)],
    &[("tth", 42.0), ("th", 0.0), ("scell", 6.0)],
    &[("tth", 1.0), ("th", 0.0)], // uan 1 0
];

struct Series {
    name: String,
    /// `Ti`, in seconds.
    period: f64,
    texpo: f64,
    n: usize,
    /// The scan mode's macro, `(name, texpo)`.
    expose: &'static str,
    /// `fi`'s four PFCU0 blades, 0 or 1 each.
    filters: [u8; 4],
}

/// mymar: `n` frames, frame `i` from `i·Ti` after the first. Once set up
/// it waits only between frames, so a deferred pause lands before the next
/// frame, never after it. The mar stays where it stands: no `mar_ip7`.
async fn series(ops: &Ops<'_>, s: &Series) -> Step {
    ops.spec("mar345closeshutter").await?;
    ops.umv(&[("scell", 0.0)]).await?;
    ops.spec("counters 2 3").await?;
    ops.spec("mar345closeshutter").await?;
    let blades: Vec<String> = s.filters.iter().map(u8::to_string).collect();
    ops.spec(&format!("fi {}", blades.join(" "))).await?;
    ops.spec("ic_range 4").await?;
    ops.sleep(SETTLE).await?;
    ops.umv(&[("scell", 0.0)]).await?;
    ops.between_frames(SETTLE).await?;
    let t0 = Instant::now();
    for i in 0..s.n {
        if i > 0 {
            let elapsed = t0.elapsed().as_secs_f64();
            ops.between_frames(start_delay(i, elapsed, s.period))
                .await?;
        }
        ops.expose(s.expose, &s.name, s.texpo).await?;
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
        for m in MAR_IN {
            ops.umv(m).await?;
        }
    }
    ops.spec("mar345closeshutter").await?;
    ops.umv(AT_IN).await?;
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
        ops.expose("mar345expose", name, texpo).await?;
    }
    ops.spec("ic_offset 1").await?;
    ops.spec("fi 1 0 1 1").await?;
    for m in MAR_OUT {
        ops.umv(m).await?;
    }
    Ok(())
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
    const NAMES: &[&str] = &["name", "ti", "texpo", "n", "mode", "filters", "md"];
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
        let key = args.str_or("mode", MODES[0].0)?;
        let (_, expose) = MODES.iter().find(|(k, _)| *k == key).ok_or_else(|| {
            let known: Vec<_> = MODES.iter().map(|(k, _)| *k).collect();
            format!("{PLAN}: mode must be one of {known:?}, got {key:?}")
        })?;
        let filters = if args.given("filters") {
            let v = args.f64_list("filters")?;
            let blades: Option<Vec<u8>> = v
                .iter()
                .map(|&x| (x == 0.0 || x == 1.0).then_some(x as u8))
                .collect();
            match blades.as_deref() {
                Some(&[a, b, c, d]) => [a, b, c, d],
                _ => {
                    return Err(format!(
                        "{PLAN}: filters must be four blades of 0 or 1, got {v:?}"
                    ));
                }
            }
        } else {
            FILTERS
        };
        let s = Series {
            name,
            period: ti * 60.0,
            texpo,
            n,
            expose,
            filters,
        };
        let extra = json!({
            "detectors": ["scaler"],
            "det": DET,
            "name": s.name,
            "ti": ti,
            "texpo": texpo,
            "num_points": n,
            "mode": key,
            "expose": expose,
            "filters": filters,
        });
        let (md, place) = item_md(PLAN, &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move { series(&Ops::new(&ctx, &dev, &place), &s).await },
        ))
    });
    let modes: Vec<_> = MODES.iter().map(|(k, _)| *k).collect();
    reg.register_plan_with_meta(
        PLAN,
        factory,
        PlanMeta {
            description: Some(
                "mymar (n = 1: mymar1), the mar left where it stands: n mar345 frames, one every ti minutes, \
                 each by the mode's macro."
                    .into(),
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
                        "mode",
                        "str",
                        &format!(
                            "Scan mode, one of {modes:?}: mar345expose, mar345_lab6 (th and phi \
                             rock over ±1°), mar345_lab6_wide (±5°)."
                        ),
                    ),
                    "'static'",
                ),
                optional(
                    param("filters", "list", "fi's four PFCU0 blades, 0 or 1 each."),
                    "[0, 1, 0, 0]",
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
        "blocks",
        "sequence",
        "th_list",
        "texpo",
        "name",
        "md",
    ];
    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse(PLAN, NAMES, raw)?;
        let temp = Temperature::parse(PLAN, &args)?;
        let given = args
            .given("blocks")
            .then(|| args.value("blocks"))
            .transpose()?;
        let align = match &given {
            Some(v) => blocks::parse(PLAN, v)?,
            None => Vec::new(),
        };
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
            "det": DET,
            "T": t,
            "ramp": temp.ramp,
            "wait": wait,
            "tolerance": tolerance,
            "soak": temp.soak,
            "sequence": args.str_opt("sequence")?,
            "blocks": given,
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
                let ops = Ops::new(&ctx, &dev, &place);
                for line in temp.commands() {
                    ops.spec(&line).await?;
                }
                if !align.is_empty() {
                    blocks::run(&ops, &align).await?;
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
                 an alignment sequence (blocks, align_in.mac's by default in the GUI), then \
                 mymar_in (a frame per th_list)."
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
                optional(
                    param(
                        "blocks",
                        "list",
                        "An alignment sequence's blocks, run first; None or [] for none.",
                    ),
                    "None",
                ),
                optional(
                    param("sequence", "str", "The sequence's name, recorded."),
                    "None",
                ),
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
    use std::collections::HashMap;

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

    /// mymar_in's moves, from the arm down and from the arm up, against
    /// spec-ioc's SpecInterlock as st.cmd sets it.
    #[test]
    fn mymar_in_moves_keep_the_interlock() {
        let line = include_str!("../../../ioc/spec/st.cmd")
            .lines()
            .find_map(|l| l.strip_prefix("SpecInterlock(")?.strip_suffix(')'))
            .unwrap();
        let a: Vec<&str> = line.split(", ").map(|a| a.trim_matches('"')).collect();
        let (arm, out) = (a[1], a[3]);
        let (clear, away): (f64, f64) = (a[2].parse().unwrap(), a[4].parse().unwrap());
        let guarded: Vec<&str> = a[5].split(' ').collect();
        let martran: f64 = IN_GEOMETRY.split(' ').next().unwrap().parse().unwrap();
        let mar_ip7 = [("martran", martran)];
        for start in [1.0, 60.0] {
            let mut at = HashMap::from([(arm, start), (out, 1234.0)]);
            let mut steps: Vec<&[(&str, f64)]> = Vec::new();
            if start < 40.0 {
                steps.extend(MAR_IN);
            }
            steps.extend([AT_IN, &mar_ip7[..]]);
            steps.extend(MAR_OUT);
            for step in steps {
                let down = step.iter().any(|&(m, v)| m == arm && v < clear);
                if step.iter().any(|(m, _)| guarded.contains(m)) {
                    assert!(at[arm] >= clear && !down, "{step:?} at {arm} {}", at[arm]);
                }
                if down {
                    assert!(at[out] >= away, "{step:?} at {out} {}", at[out]);
                }
                at.extend(step.iter().copied());
            }
            assert_eq!((at[arm], at[out]), (1.0, 1234.0), "back from {start}");
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
