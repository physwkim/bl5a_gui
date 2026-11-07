//! The three queue items: `align` (an alignment sequence: coin_align's,
//! align_in.mac's or a user's), `mar_series` (mymar1, mymar) and
//! `insitu_step` (a step of myinsitu, then mymar_in). Each is one run;
//! its scans are runs of their own ([`crate::scan`]). Besides the scans
//! a plan records the SPEC commands it sent ("commands"), each alignment
//! scan's outcome ("stages"), where an alignment left the sample
//! ("final") and each frame ("exposures").

mod align;
mod args;
mod blocks;
mod mar;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bsrs::core::msg::NamedObj;
use bsrs::core::{BsrsError, Plan, RunMetadata};
use bsrs::event_model::Dtype;
use bsrs::plans::preprocessors::{finalize_wrapper, run_wrapper};
use bsrs::qs::{ParamKind, ParamSpec, Registry};
use serde_json::{Value, json};

use crate::ctx::{Ctx, Step, plan_fn};
use crate::devices::{CHANNELS, Devices, Holder, Preset, get};
use crate::place::{DATA_DIR, Place};
use crate::scan::Scan;
use crate::screen::{Pick, Screened};

/// What produced the documents.
fn versions() -> Value {
    json!({"bl5a-qs": env!("CARGO_PKG_VERSION")})
}

/// The start document: `versions`, the queue item's `md`, then the keys
/// the plan computed.
fn start_md(extra: Value, md: HashMap<String, Value>) -> HashMap<String, Value> {
    let mut map = HashMap::from([("versions".to_string(), versions())]);
    map.extend(md);
    if let Some(computed) = extra.as_object() {
        map.extend(computed.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    map
}

/// [`start_md`] as the `RunMetadata` `run_wrapper` opens the run with.
pub(crate) fn run_md(plan_name: &str, extra: Value, md: HashMap<String, Value>) -> RunMetadata {
    RunMetadata {
        scan_id: None,
        plan_name: Some(plan_name.to_string()),
        extra: start_md(extra, md),
    }
}

/// A queue item's run: its start md, `data_dir` with it, and the folder.
fn item_md(
    plan: &str,
    root: &Path,
    mut extra: Value,
    md: HashMap<String, Value>,
) -> std::result::Result<(RunMetadata, Place), String> {
    let place = Place::of(plan, root, &md)?;
    extra[DATA_DIR] = json!(place.dir);
    Ok((run_md(plan, extra, md), place))
}

fn param(name: &str, ty: &str, description: &str) -> ParamSpec {
    ParamSpec::new(name, ParamKind::PositionalOrKeyword)
        .with_type(ty)
        .with_description(description)
}

fn optional(spec: ParamSpec, default: &str) -> ParamSpec {
    spec.with_default(default)
}

/// A file name `mar345expose` takes: the macros' "less than 28
/// characters, no space", and no quote, which would end the string.
fn file_name(plan: &str, name: &str) -> std::result::Result<String, String> {
    if name.is_empty()
        || name.len() >= 28
        || name
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        return Err(format!(
            "{plan}: name must be 1 to 27 characters without spaces or quotes, got {name:?}"
        ));
    }
    Ok(name.to_string())
}

/// An exposure `mar345expose` takes: seconds, or monitor counts when
/// negative.
fn exposure(plan: &str, texpo: f64) -> std::result::Result<f64, String> {
    if !texpo.is_finite() || texpo == 0.0 {
        return Err(format!(
            "{plan}: texpo must be seconds (> 0) or monitor counts (< 0), got {texpo}"
        ));
    }
    Ok(texpo)
}

/// The plan's run around `body`, which starts once the item's folder is
/// there. However the run ends, after it closes a SPEC command still
/// running is aborted and a count still running is stopped (an abort
/// cancels the put that waits for them, not SPEC), and the shutter is
/// closed, as every plan opens with it closed.
fn outer<F, Fut>(dev: &Devices, md: RunMetadata, place: Place, body: F) -> Plan
where
    F: FnOnce(Ctx) -> Fut + Send + 'static,
    Fut: Future<Output = Step> + Send + 'static,
{
    let body = move |ctx: Ctx| async move {
        ctx.call(async move {
            place
                .make()
                .map_err(|e| BsrsError::State(format!("data folder: {e}")))
        })
        .await?;
        body(ctx).await
    };
    let (cmd, scaler) = (dev.cmd.clone(), dev.scaler.clone());
    let finalizer = plan_fn(move |ctx| async move {
        ctx.call(async move {
            cmd.abort_if_busy().await?;
            scaler.stop_if_counting().await?;
            cmd.wait_ready().await?;
            cmd.run("mar345closeshutter").await.map(drop)
        })
        .await
    });
    finalize_wrapper(run_wrapper(plan_fn(body), md), finalizer)
}

/// `mar345expose`'s counts for `texpo`: seconds in counts of at most 429
/// s, the timer preset's range; monitor counts (negative) in one.
fn presets(texpo: f64) -> Vec<Preset> {
    if texpo < 0.0 {
        return vec![Preset::Monitor(-texpo)];
    }
    let n = (texpo / 429.0).trunc();
    let rest = texpo - n * 429.0;
    let mut out = vec![Preset::Seconds(429.0); n as usize];
    // Not the macro's `uct(0)` after a whole number of 429 s counts:
    // scaler1 refuses a 0 s preset.
    if rest > 0.0 {
        out.push(Preset::Seconds(rest));
    }
    out
}

/// The macros' verbs for a plan body.
pub(crate) struct Ops<'a> {
    ctx: &'a Ctx,
    dev: &'a Devices,
    /// The item's folder: the frames' and the scans' SPEC file's.
    place: &'a Place,
    commands: Arc<Holder>,
    stages: Arc<Holder>,
    exposures: Arc<Holder>,
}

impl<'a> Ops<'a> {
    fn new(ctx: &'a Ctx, dev: &'a Devices, place: &'a Place) -> Self {
        use Dtype::{Integer, Number, String};
        let stats = [
            "before", "target", "after", "set", "cen", "com", "xmax", "fwhm", "ymax",
        ];
        let mut stages = vec![("stage", String), ("motor", String), ("pick", String)];
        stages.extend(stats.map(|k| (k, Number)));
        stages.push(("rejected", Integer));
        let mut exposures = vec![
            ("name", String),
            ("file", String),
            ("file_number", Integer),
            ("texpo", Number),
            ("th", Number),
            ("shold", Number),
        ];
        exposures.extend(CHANNELS.map(|k| (k, Number)));
        Self {
            ctx,
            dev,
            place,
            commands: Holder::new("commands", &[("cmd", String), ("reply", String)]),
            stages: Holder::new("stages", &stages),
            exposures: Holder::new("exposures", &exposures),
        }
    }

    /// One event of `holder`'s stream, named as the holder is.
    async fn event(&self, holder: &Arc<Holder>, values: Vec<(&str, Value)>) -> Step {
        holder.set(values.into_iter().map(|(k, v)| (k.to_string(), v)));
        self.ctx.create(holder.name()).await?;
        self.ctx.read(holder.clone()).await?;
        self.ctx.save().await
    }

    /// A SPEC command line, to its end; a "commands" event after.
    async fn spec(&self, text: &str) -> Step {
        let (cmd, line) = (self.dev.cmd.clone(), text.to_string());
        let reply = self.ctx.call(async move { cmd.run(&line).await }).await?;
        let reply = reply.trim_end().to_string();
        let values = vec![("cmd", json!(text)), ("reply", json!(reply))];
        self.event(&self.commands.clone(), values).await
    }

    /// `umv`: the motors together, until all are there.
    async fn umv(&self, moves: &[(&str, f64)]) -> Step {
        let moves: Vec<_> = moves.iter().map(|&(m, v)| self.dev.axis(m).to(v)).collect();
        self.ctx.mv(&moves).await
    }

    /// `set m pos`.
    async fn set(&self, m: &str, pos: f64) -> Step {
        let axis = self.dev.axis(m);
        self.ctx
            .call(async move { axis.set_position(pos).await })
            .await
    }

    async fn position(&self, m: &str) -> Step<f64> {
        self.ctx.position(self.dev.axis(m)).await
    }

    async fn sleep(&self, seconds: f64) -> Step {
        self.ctx.sleep(Duration::from_secs_f64(seconds)).await
    }

    /// `seconds` between frames, where a deferred pause takes effect: at
    /// once, within a second, or at the end. A resume goes on from there,
    /// a stop ends the plan there.
    async fn between_frames(&self, seconds: f64) -> Step {
        self.ctx
            .pausable_sleep(Duration::from_secs_f64(seconds))
            .await
    }

    /// `epics_put("BL5A:MAR345:cam1:Erase",1)`, until idle.
    async fn erase(&self) -> Step {
        let cam = self.dev.cam.clone();
        self.ctx.call(async move { cam.erase().await }).await
    }

    /// One alignment scan, then `umv m <pick>` and, given `set`, `set m
    /// set`; a "stages" event records the move and the statistics. A scan
    /// that gives no target puts the motor back where it was and ends the
    /// plan.
    async fn stage(&self, scan: Scan, pick: Pick, set: Option<f64>) -> Step {
        let axis = scan.axis.clone();
        let m = axis.name().to_string();
        let line = scan.command();
        let before = self.ctx.position(axis.clone()).await?;
        let (plan, points) = scan.plan(self.dev);
        self.ctx.run(plan).await?;
        let Screened {
            stats,
            target,
            rejected,
        } = match points.screen(pick) {
            Ok(s) => s,
            Err(why) => {
                self.umv(&[(&m, before)])
                    .await
                    .map_err(|e| format!("{line}: {why}; moving {m} back to {before}: {e}"))?;
                return Err(format!("{line}: {why}; {m} back at {before}"));
            }
        };
        self.umv(&[(&m, target)]).await?;
        if let Some(pos) = set {
            self.set(&m, pos).await?;
        }
        let after = self.ctx.position(axis).await?;
        let values = vec![
            ("stage", json!(line)),
            ("motor", json!(m)),
            ("pick", json!(pick.name())),
            ("before", json!(before)),
            ("target", json!(target)),
            ("after", json!(after)),
            // null when the stage left the motor's user offset alone.
            ("set", json!(set)),
            ("cen", json!(stats.cen)),
            ("com", json!(stats.com)),
            ("xmax", json!(stats.xpk)),
            ("fwhm", json!(stats.fwhm)),
            ("ymax", json!(stats.ymax)),
            ("rejected", json!(rejected)),
        ];
        self.event(&self.stages.clone(), values).await
    }

    /// `mar345expose(name, texpo)` in its steps: SPEC works the shutter,
    /// on its serial line, and keeps its records of the frame
    /// (`exp_condition`'s data-file comments, `mar345readout`'s
    /// `file_no`); the plan names the file in the item's folder, counts
    /// and has mar345-ioc scan. An "exposures" event with the file and
    /// the counts of the last count.
    async fn expose(&self, name: &str, texpo: f64) -> Step {
        let (cam, scaler) = (self.dev.cam.clone(), self.dev.scaler.clone());
        let (dir, file) = (self.place.dir.clone(), name.to_string());
        self.ctx
            .call(async move { cam.name_file(&dir, &file).await })
            .await?;
        self.spec("mar345openshutter").await?;
        for preset in presets(texpo) {
            let scaler = scaler.clone();
            self.ctx
                .call(async move { scaler.count(preset).await })
                .await?;
        }
        self.spec("mar345closeshutter").await?;
        // `filename` and `exposure` are the macro's arguments, local to it.
        self.spec(&format!(
            "{{local filename, exposure; filename=\"{name}\"; exposure={texpo}; exp_condition}}"
        ))
        .await?;
        let cam = self.dev.cam.clone();
        self.ctx.call(async move { cam.acquire().await }).await?;
        self.spec(&format!("mar345readout(\"{name}\")")).await?;
        let cam = self.dev.cam.clone();
        let (counts, next, file) = self
            .ctx
            .call(async move {
                let counts = scaler.counts().await?;
                let next = get(&cam.file_number).await?;
                Ok((counts, next, get(&cam.full_file_name).await?))
            })
            .await?;
        let mut values = vec![
            ("name", json!(name)),
            ("file", json!(file)),
            // FileNumber is the next file's (`file_no = FileNumber - 1`).
            ("file_number", json!(next - 1)),
            ("texpo", json!(texpo)),
            ("th", json!(self.position("th").await?)),
            ("shold", json!(self.position("shold").await?)),
        ];
        values.extend(CHANNELS.iter().zip(counts).map(|(&k, v)| (k, json!(v))));
        self.event(&self.exposures.clone(), values).await
    }
}

/// Every plan; `root` is the data root of an item that names none.
pub fn register_all(reg: &mut Registry, devices: &Devices, root: &Path) {
    align::register(reg, devices, root);
    mar::register(reg, devices, root);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_mar345expose_takes() {
        assert!(file_name("p", "in-situ_Neat_ITIC-Th_CF_240").is_ok());
        assert!(file_name("p", &"a".repeat(27)).is_ok());
        for bad in ["", "a b", "a\"b", "a'b", &"a".repeat(28)] {
            assert!(file_name("p", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn exposures_are_seconds_or_monitor_counts() {
        assert_eq!(exposure("p", 3.0), Ok(3.0));
        assert_eq!(exposure("p", -7e4), Ok(-7e4));
        for bad in [0.0, f64::NAN, f64::INFINITY] {
            assert!(exposure("p", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn exposures_count_in_429_s_pieces() {
        use Preset::{Monitor, Seconds};
        assert_eq!(presets(3.0), [Seconds(3.0)]);
        assert_eq!(presets(429.0), [Seconds(429.0)]);
        assert_eq!(presets(500.0), [Seconds(429.0), Seconds(71.0)]);
        assert_eq!(presets(858.0), [Seconds(429.0), Seconds(429.0)]);
        assert_eq!(presets(-7e4), [Monitor(7e4)]);
    }
}
