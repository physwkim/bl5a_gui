//! `step_scan`: one `ascan` (or `lup` when relative) of any motor as a
//! queue item, so a scan typed at the GUI runs, draws and lands in the
//! series' SPEC file like an alignment's. Step scans only: they count
//! through scaler1 here, while a `cscan`/`clup` runs SPEC's own macros.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bsrs::qs::{PlanFactory, PlanMeta, Registry};
use serde_json::json;

use super::args::Args;
use super::{Ops, item_md, optional, outer, param};
use crate::devices::{AXES, CHANNELS, Devices};

const PLAN: &str = "step_scan";
const NAMES: &[&str] = &[
    "motor",
    "start",
    "finish",
    "intervals",
    "time",
    "relative",
    "det",
    "md",
];
/// SPEC's `DET` when the item names none: align_in.mac's `plotselect det`.
const DEFAULT_DET: &str = "det";

/// A `step_scan` item's arguments, checked.
#[derive(Debug, PartialEq)]
struct StepScan {
    motor: &'static str,
    start: f64,
    finish: f64,
    intervals: usize,
    time: f64,
    relative: bool,
    det: &'static str,
}

impl StepScan {
    fn parse(args: &Args) -> Result<Self, String> {
        let motor = args.str("motor")?;
        let motor = AXES.into_iter().find(|m| *m == motor).ok_or_else(|| {
            format!(
                "{PLAN}: motor must be one of {}, got {motor:?}",
                AXES.join(" ")
            )
        })?;
        let det = args.str_or("det", DEFAULT_DET)?;
        let det = CHANNELS.into_iter().find(|c| *c == det).ok_or_else(|| {
            format!(
                "{PLAN}: det must be one of {}, got {det:?}",
                CHANNELS.join(" ")
            )
        })?;
        let finite = |name: &str| -> Result<f64, String> {
            let v = args
                .f64_opt(name)?
                .ok_or_else(|| format!("{PLAN}: missing argument {name}"))?;
            if !v.is_finite() {
                return Err(format!("{PLAN}: {name} must be finite, got {v}"));
            }
            Ok(v)
        };
        let (start, finish) = (finite("start")?, finite("finish")?);
        let intervals = args.count_or("intervals", 0)?;
        if intervals == 0 {
            return Err(format!("{PLAN}: intervals must be at least 1"));
        }
        let time = finite("time")?;
        if time <= 0.0 {
            return Err(format!(
                "{PLAN}: time must be seconds per point (> 0), got {time}"
            ));
        }
        Ok(Self {
            motor,
            start,
            finish,
            intervals,
            time,
            relative: args.bool_or("relative", false)?,
            det,
        })
    }
}

pub(super) fn register(reg: &mut Registry, devices: &Devices, root: &Path) {
    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse(PLAN, NAMES, raw)?;
        let s = StepScan::parse(&args)?;
        let extra = json!({"detectors": ["scaler"], "det": s.det, "motors": [s.motor]});
        let (md, place) = item_md(PLAN, &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move {
                let ops = Ops::new(&ctx, &dev, &place, s.det, Duration::ZERO);
                let scan = ops.step(s.motor, s.start, s.finish, s.intervals, s.time, s.relative);
                let (plan, _points) = scan.plan(&dev);
                ctx.run(plan).await
            },
        ))
    });
    reg.register_plan_with_meta(
        PLAN,
        factory,
        PlanMeta {
            description: Some(
                "ascan motor start finish intervals time (lup when relative), counting det.".into(),
            ),
            parameters: vec![
                param("motor", "str", "The motor to scan."),
                param("start", "float", "First point (an offset when relative)."),
                param("finish", "float", "Last point (an offset when relative)."),
                param(
                    "intervals",
                    "int",
                    "Steps; the scan takes intervals + 1 points.",
                ),
                param("time", "float", "Seconds counted at each point."),
                optional(
                    param(
                        "relative",
                        "bool",
                        "lup: offsets from where the motor is, and back there after.",
                    ),
                    "False",
                ),
                optional(
                    param("det", "str", "The counter the scan's statistics are of."),
                    "'det'",
                ),
                optional(param("md", "dict", "Extra start-document metadata."), "{}"),
            ],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: serde_json::Value) -> Result<StepScan, String> {
        StepScan::parse(&Args::parse(PLAN, NAMES, &raw)?)
    }

    #[test]
    fn a_lup_and_an_ascan() {
        let s = parse(
            json!({"kwargs": {"motor": "th", "start": -0.1, "finish": 0.1,
            "intervals": 4, "time": 0.1, "relative": true}}),
        )
        .unwrap();
        assert_eq!(
            s,
            StepScan {
                motor: "th",
                start: -0.1,
                finish: 0.1,
                intervals: 4,
                time: 0.1,
                relative: true,
                det: "det",
            }
        );
        let s =
            parse(json!({"args": ["shold", 1, -1, 10, 0.5], "kwargs": {"det": "ic1"}})).unwrap();
        assert_eq!(
            (s.motor, s.relative, s.det, s.intervals),
            ("shold", false, "ic1", 10)
        );
    }

    #[test]
    fn refuses_what_the_scan_cannot_take() {
        let base = json!({"motor": "th", "start": 0, "finish": 1, "intervals": 2, "time": 1});
        let with = |k: &str, v: serde_json::Value| {
            let mut kw = base.clone();
            kw[k] = v;
            parse(json!({ "kwargs": kw })).unwrap_err()
        };
        assert!(with("motor", json!("chi")).contains("motor must be one of"));
        assert!(with("det", json!("pd")).contains("det must be one of"));
        assert!(with("intervals", json!(0)).contains("at least 1"));
        assert!(with("intervals", json!(2.5)).contains("non-negative integer"));
        assert!(with("time", json!(0)).contains("time must be"));
        assert!(with("time", json!(-1)).contains("time must be"));
        assert!(with("start", json!("x")).contains("must be a number"));
        let mut kw = base.as_object().unwrap().clone();
        kw.remove("finish");
        assert_eq!(
            parse(json!({ "kwargs": kw })).unwrap_err(),
            "step_scan: missing argument finish"
        );
    }
}
