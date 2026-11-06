//! The alignments: coin_align (`align_coin`) and align_in.mac
//! (`align_film`), the macros' lines in their order. The `disable`/
//! `enable` of counters and `plotselect` are not sent: a scan names its
//! counter.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bsrs::event_model::Dtype;
use bsrs::qs::{PlanFactory, PlanMeta, Registry};
use serde_json::json;

use super::args::Args;
use super::{Ops, Pick, item_md, optional, outer, param};
use crate::ctx::Step;
use crate::devices::{Devices, Holder};

/// coin_align's counter (`plotselect ic1`).
const COIN_DET: &str = "ic1";
/// align_in.mac's (`plotselect det`), which mymar_in keeps.
pub(super) const FILM_DET: &str = "det";
/// align_in.mac's `_sleep`.
pub(super) const FILM_SETTLE: Duration = Duration::from_millis(100);
/// The positions coin_align's `wm2` shows.
const COIN_FINAL: [&str; 5] = ["sx", "sy", "shold", "th", "phi"];

/// coin_align. Its `mar_ip` without arguments prints its usage and
/// exits, so nothing after it runs; `wm2`'s positions go to a "final"
/// event here.
async fn coin(ops: &Ops<'_>) -> Step {
    ops.umv(&[("marshut", 0.0)]).await?; // shut 0
    ops.spec("mon1_setup 1050 100 300 3.0 1.0").await?;
    ops.spec("setkappa 50").await?;
    ops.spec("counters 2 5").await?;
    ops.spec("fi 1 0 0 0").await?;
    ops.spec("fil 0 0 0 0").await?;
    let sy = ops.fly("sy", -2.5, 2.5, 12.5, 0.1, true);
    ops.stage(sy, Pick::Com, None).await?;
    let shold = ops.fly("shold", -2.0, 2.0, 10.5, 0.1, true);
    ops.stage(shold, Pick::Com, None).await?;
    ops.spec("mar345closeshutter").await?;
    ops.erase().await?;
    ops.umv(&[("marshut", 340.0)]).await?; // shut 340

    let keys = COIN_FINAL.map(|k| (k, Dtype::Number));
    let mut values = Vec::new();
    for m in COIN_FINAL {
        values.push((m, json!(ops.position(m).await?)));
    }
    ops.event(&Holder::new("final", &keys), values).await
}

/// align_in.mac: the direct beam (tth, nu), the half-cut (shold, th),
/// the specular (th at 0.6), each scan's motor left at its statistic.
pub(super) async fn film(ops: &Ops<'_>) -> Step {
    ops.spec("ds_wid 0.5 1.0").await?;
    ops.spec("ss_wid 1.0 6.0").await?;
    ops.spec("LF nu").await?;
    ops.spec("autofil 0").await?;
    if ops.position("martran").await? < 1200.0 {
        ops.spec("mar_ip_mode off").await?;
    }
    ops.umv(&[("tth", 1.0), ("th", 0.0)]).await?; // uan 1 0
    ops.spec("atte 0").await?;
    ops.spec("marstart2").await?;
    ops.sleep(3.0).await?;
    ops.spec("atte 11").await?;
    ops.spec("counters 2 3").await?;
    ops.umv(&[("scell", 5.0)]).await?;
    ops.spec("ic_range 4").await?;
    ops.spec("ic_offset 1").await?;
    ops.spec("all6 0 0 89.7 0 0 0.0").await?;
    ops.umv(&[("shold", -3.0)]).await?;

    // Direct beam.
    let tth = ops.step("tth", -0.04, 0.04, 24, 0.3, false);
    ops.stage(tth, Pick::Cen, Some(0.0)).await?;
    let nu = ops.step("nu", -0.10, 0.10, 16, 0.3, false);
    ops.stage(nu, Pick::Cen, Some(0.0)).await?;

    // Half-cut.
    let stages = [
        (
            ops.fly("shold", -3.0, -1.5, 5.0, 0.05, false),
            Pick::Cen,
            None,
        ),
        (
            ops.fly("th", -1.0, 1.0, 5.0, 0.03, true),
            Pick::XMax,
            Some(0.0),
        ),
        (
            ops.fly("shold", 0.3, -0.3, 2.0, 0.05, true),
            Pick::Cen,
            None,
        ),
        (ops.fly("th", -2.0, 2.0, 10.0, 0.03, true), Pick::Cen, None),
        (ops.step("shold", -0.3, 0.3, 18, 0.3, true), Pick::Cen, None),
        (
            ops.step("th", -0.6, 0.6, 30, 0.5, true),
            Pick::Cen,
            Some(0.0),
        ),
        (
            ops.step("shold", -0.15, 0.15, 20, 0.5, true),
            Pick::Cen,
            None,
        ),
    ];
    for (scan, pick, set) in stages {
        ops.stage(scan, pick, set).await?;
    }

    // Specular.
    ops.umv(&[("tth", 1.2), ("th", 0.6)]).await?; // uan 1.2 0.6
    ops.spec("atte 4").await?;
    let th = ops.step("th", -0.2, 0.2, 120, 0.5, true);
    ops.stage(th, Pick::XMax, Some(0.6)).await?;
    ops.spec("mar345closeshutter").await?;
    ops.erase().await
}

pub(super) fn register(reg: &mut Registry, devices: &Devices, root: &Path) {
    const NAMES: &[&str] = &["md"];
    let md_param = || {
        vec![optional(
            param("md", "dict", "Extra start-document metadata."),
            "{}",
        )]
    };

    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse("align_coin", NAMES, raw)?;
        let extra = json!({"detectors": ["scaler"], "det": COIN_DET});
        let (md, place) = item_md("align_coin", &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move {
                coin(&Ops::new(&ctx, &dev, &place, COIN_DET, Duration::ZERO)).await
            },
        ))
    });
    reg.register_plan_with_meta(
        "align_coin",
        factory,
        PlanMeta {
            description: Some(
                "coin_align: sy and shold to the centre of mass of ic1 over the coin cell's window."
                    .into(),
            ),
            parameters: md_param(),
        },
    );

    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse("align_film", NAMES, raw)?;
        let extra = json!({"detectors": ["scaler"], "det": FILM_DET});
        let (md, place) = item_md("align_film", &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move { film(&Ops::new(&ctx, &dev, &place, FILM_DET, FILM_SETTLE)).await },
        ))
    });
    reg.register_plan_with_meta(
        "align_film",
        factory,
        PlanMeta {
            description: Some(
                "align_in.mac: direct beam (tth, nu), sample half-cut (shold, th), specular th at 0.6."
                    .into(),
            ),
            parameters: md_param(),
        },
    );
}
