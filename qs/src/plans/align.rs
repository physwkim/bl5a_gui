//! `align`: an alignment sequence, the blocks the item carries
//! ([`super::blocks`]). The GUI's templates are coin_align's and
//! align_in.mac's lines in their order; the `disable`/`enable` of
//! counters and `plotselect` are not sent: a scan block names its counter.

use std::path::Path;
use std::sync::Arc;

use bsrs::qs::{PlanFactory, PlanMeta, Registry};
use serde_json::json;

use super::args::Args;
use super::{Ops, blocks, item_md, optional, outer, param};
use crate::devices::Devices;

pub(super) fn register(reg: &mut Registry, devices: &Devices, root: &Path) {
    const PLAN: &str = "align";
    const NAMES: &[&str] = &["blocks", "sequence", "md"];
    let (dev, data_root) = (devices.clone(), root.to_path_buf());
    let factory: PlanFactory = Arc::new(move |_reg, raw| {
        let args = Args::parse(PLAN, NAMES, raw)?;
        let given = args.value("blocks")?;
        let blocks = blocks::parse(PLAN, &given)?;
        if blocks.is_empty() {
            return Err(format!("{PLAN}: blocks is empty"));
        }
        let extra = json!({
            "detectors": ["scaler"],
            "sequence": args.str_opt("sequence")?,
            "blocks": given,
        });
        let (md, place) = item_md(PLAN, &data_root, extra, args.md()?)?;
        let dev = dev.clone();
        Ok(outer(
            &dev.clone(),
            md,
            place.clone(),
            move |ctx| async move { blocks::run(&Ops::new(&ctx, &dev, &place), &blocks).await },
        ))
    });
    reg.register_plan_with_meta(
        PLAN,
        factory,
        PlanMeta {
            description: Some(
                "An alignment sequence: its blocks (scan, move, spec, wait, erase) in order."
                    .into(),
            ),
            parameters: vec![
                param(
                    "blocks",
                    "list",
                    "The blocks, each an object with its kind; see doc/usage.md.",
                ),
                optional(
                    param("sequence", "str", "The sequence's name, recorded."),
                    "None",
                ),
                optional(param("md", "dict", "Extra start-document metadata."), "{}"),
            ],
        },
    );
}
