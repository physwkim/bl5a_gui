//! `scan`: one scan as a queue item, an alignment scan block's fields
//! ([`super::blocks`]) with the pick optional: an `ascan`, `lup`,
//! `cscan` or `clup` that runs, draws and lands in the series' SPEC file
//! like an alignment's, then, given a pick, the motor there (and `set`)
//! as an alignment stage.

use std::path::Path;
use std::sync::Arc;

use bsrs::qs::{PlanFactory, PlanMeta, Registry};
use serde_json::json;

use super::args::Args;
use super::blocks::{self, ScanDef, Then};
use super::{Ops, item_md, optional, outer, param};
use crate::devices::Devices;

const PLAN: &str = "scan";
const NAMES: &[&str] = &[
    "type",
    "motor",
    "start",
    "finish",
    "intervals",
    "time",
    "monitor",
    "settle",
    "sleep",
    "counter",
    "pick",
    "dip",
    "set",
    "md",
];

/// A `scan` item's scan and what follows it.
fn parse(args: &Args) -> Result<(ScanDef, Option<Then>), String> {
    let mut fields = args.named();
    fields.remove("md");
    blocks::scan_item(PLAN, &fields)
}

pub(super) fn register(reg: &mut Registry, devices: &Devices, root: &Path) {
    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse(PLAN, NAMES, raw)?;
        let (scan, then) = parse(&args)?;
        let extra = json!({"detectors": ["scaler"], "det": scan.counter, "motors": [scan.motor]});
        let (md, place) = item_md(PLAN, &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move {
                blocks::scan_then(&Ops::new(&ctx, &dev, &place), &scan, then).await
            },
        ))
    });
    let opt = |name, ty, what| optional(param(name, ty, what), "None");
    reg.register_plan_with_meta(
        PLAN,
        factory,
        PlanMeta {
            description: Some(
                "One scan (ascan, lup, cscan, clup), then, given a pick, the motor to the \
                 scan's CEN, COM, xMAX or EDGE; an alignment scan block's fields, see doc/usage.md."
                    .into(),
            ),
            parameters: vec![
                param("type", "str", "ascan, lup, cscan or clup."),
                param("motor", "str", "The motor to scan."),
                param(
                    "start",
                    "float",
                    "First point (an offset for lup and clup).",
                ),
                param(
                    "finish",
                    "float",
                    "Last point (an offset for lup and clup).",
                ),
                opt(
                    "intervals",
                    "int",
                    "ascan, lup: steps; the scan takes intervals + 1 points.",
                ),
                opt(
                    "time",
                    "float",
                    "Seconds counted at each point (ascan, lup) or the whole sweep (cscan, clup).",
                ),
                opt(
                    "monitor",
                    "float",
                    "ascan, lup: monitor counts per point, for time.",
                ),
                opt("settle", "float", "ascan, lup: seconds after each move."),
                opt("sleep", "float", "cscan, clup: seconds between the counts."),
                param(
                    "counter",
                    "str",
                    "The counter the scan's statistics are of.",
                ),
                opt(
                    "pick",
                    "str",
                    "CEN, COM, xMAX or EDGE: where the motor goes after.",
                ),
                opt("dip", "bool", "The pick is of a dip (not EDGE)."),
                opt("set", "float", "`set motor <set>` there."),
                optional(param("md", "dict", "Extra start-document metadata."), "{}"),
            ],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const LUP: &str = r#"{"type": "lup", "motor": "th", "start": -0.1, "finish": 0.1,
        "intervals": 4, "time": 0.1, "settle": 0, "counter": "det"}"#;

    fn read(raw: Value) -> Result<(ScanDef, Option<Then>), String> {
        parse(&Args::parse(PLAN, NAMES, &raw)?)
    }

    fn with(extra: Value) -> Value {
        let mut kw: Value = serde_json::from_str(LUP).unwrap();
        for (k, v) in extra.as_object().unwrap() {
            kw[k] = v.clone();
        }
        json!({ "kwargs": kw })
    }

    #[test]
    fn a_scan_with_or_without_a_pick() {
        let (scan, then) = read(with(json!({"pick": "CEN", "set": 0}))).unwrap();
        assert_eq!((scan.motor, scan.counter), ("th", "det"));
        assert!(then.is_some());
        let (_, then) = read(with(json!({"dip": false}))).unwrap();
        assert_eq!(then, None);
        let clup = json!({"args": ["clup", "sy", -1, 1], "kwargs": {"time": 5, "sleep": 0.05,
            "counter": "ic1", "md": {"series": "s"}}});
        let (scan, then) = read(clup).unwrap();
        assert_eq!((scan.motor, then), ("sy", None));
    }

    #[test]
    fn refuses_what_a_scan_block_refuses_and_a_pick_s_fields_without_one() {
        let err = |extra: Value| read(with(extra)).unwrap_err();
        assert_eq!(
            err(json!({"dip": true})),
            "scan: dip is for a pick, and pick is missing"
        );
        assert_eq!(
            err(json!({"set": 0})),
            "scan: set is for a pick, and pick is missing"
        );
        assert_eq!(
            err(json!({"pick": "EDGE", "dip": true})),
            "scan: dip is for CEN, COM and xMAX, not EDGE"
        );
        assert!(err(json!({"pick": "MAX"})).starts_with("scan: pick must be one of"));
        assert!(err(json!({"counter": "sec"})).starts_with("scan: counter must be one of"));
        assert_eq!(err(json!({"sleep": 0.1})), "scan: unknown field sleep");
        assert_eq!(err(json!({"type": "cscan"})), "scan: sleep is missing");
        assert_eq!(
            err(json!({"type": "cscan", "sleep": 0.05})),
            "scan: unknown field intervals"
        );
        assert_eq!(
            err(json!({"relative": true})),
            "scan: unknown argument relative"
        );
        assert_eq!(
            read(json!({"kwargs": {"type": "lup"}})).unwrap_err(),
            "scan: motor is missing"
        );
    }
}
