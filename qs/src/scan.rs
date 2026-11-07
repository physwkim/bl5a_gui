//! SPEC's scans (kappa_bl5a_macros.mac `_ascan`, `_cscan`), each its own
//! run "scan" beside the plan's: `ascan`/`lup` run here, a count at each
//! step; `cscan`/`clup` run in SPEC, whose timing they need, and their
//! points go out as SPEC takes them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bsrs::core::msg::NamedObj;
use bsrs::core::{BsrsError, Msg, Plan, ReadingValue, Result as BResult};
use bsrs::event_model::Dtype;
use bsrs::plans::preprocessors::{finalize_wrapper, msg_mutator, run_wrapper};
use serde_json::json;

use crate::ctx::{Ctx, Step, plan_fn};
use crate::devices::{Axis, Count, Devices, Holder, Live, Scaler, SpecCmd, Table};
use crate::plans::run_md;
use crate::screen::{Pick, Screened, screen};
use crate::specfile::SPEC_HEAD;

/// The run key of a scan's documents.
pub const SCAN_RUN: &str = "scan";
/// An interrupted scan's motor, to DMOV.
const SETTLE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    /// `intervals` steps; `settle` is the macros' `_sleep` before each
    /// count.
    Step { intervals: usize, settle: Duration },
    /// SPEC's own cscan; `sleep` is its `_stime` between samples.
    Fly { sleep: Duration },
}

pub struct Scan {
    pub axis: Arc<Axis>,
    pub start: f64,
    pub finish: f64,
    /// `start` and `finish` are offsets from where the motor is.
    pub relative: bool,
    /// Seconds per point (step) or of the whole move (fly).
    pub ctime: f64,
    pub motion: Motion,
    /// The counter the statistics are of, SPEC's `DET`.
    pub det: &'static str,
    /// The SPEC file the scan is a `#S` of.
    pub spec_file: PathBuf,
}

/// A scan's (x, DET) points: counts per point of a step scan, per sample
/// of a fly scan, as SPEC takes them.
#[derive(Clone, Default)]
pub struct Points(Arc<Mutex<Vec<(f64, f64)>>>);

impl Points {
    fn push(&self, x: f64, y: f64) {
        self.0.lock().unwrap().push((x, y));
    }

    /// The points' statistics and `pick`'s target, or why they give none.
    pub fn screen(&self, pick: Pick) -> Result<Screened, String> {
        let (x, y): (Vec<f64>, Vec<f64>) = self.0.lock().unwrap().iter().copied().unzip();
        screen(&x, &y, pick)
    }
}

impl Scan {
    pub fn name(&self) -> &'static str {
        match (self.motion, self.relative) {
            (Motion::Step { .. }, false) => "ascan",
            (Motion::Step { .. }, true) => "lup",
            (Motion::Fly { .. }, false) => "cscan",
            (Motion::Fly { .. }, true) => "clup",
        }
    }

    /// The scan as the macros write it.
    pub fn command(&self) -> String {
        let (m, s, f, t) = (
            self.axis.name(),
            g(self.start),
            g(self.finish),
            g(self.ctime),
        );
        match self.motion {
            Motion::Step { intervals, .. } => {
                format!("{} {m} {s} {f} {intervals} {t}", self.name())
            }
            Motion::Fly { sleep } => {
                let st = g(sleep.as_secs_f64());
                format!("{} {m} {s} {f} {t} {st}", self.name())
            }
        }
    }

    /// SPEC's `HEADING` of the scan, its `#S` title: `_ascan`'s and
    /// `_cscan_prep`'s give the ends absolute, and `lup` runs as `ascan`,
    /// `clup` as `cscan`.
    fn heading(&self, s: f64, f: f64) -> String {
        let (m, s, f, t) = (self.axis.name(), g(s), g(f), g(self.ctime));
        match self.motion {
            Motion::Step { intervals, .. } => format!("ascan  {m} {s} {f}  {intervals} {t}"),
            Motion::Fly { .. } => format!("cscan  {m} {s} {f}  {t}"),
        }
    }

    /// The scan's run and the points it records. `lup` returns the motor
    /// to where it started however the scan ends, `clup` after a scan
    /// that completed (SPEC's `umv $1 pos_i` follows its cscan), the
    /// absolute scans never.
    pub fn plan(self, dev: &Devices) -> (Plan, Points) {
        let points = Points::default();
        let recorded = points.clone();
        let (scaler, cmd) = (dev.scaler.clone(), dev.cmd.clone());
        let plan = plan_fn(move |ctx| async move {
            let scan = Arc::new(self);
            let p0 = ctx.position(scan.axis.clone()).await?;
            let (s, f) = if scan.relative {
                (p0 + scan.start, p0 + scan.finish)
            } else {
                (scan.start, scan.finish)
            };
            let md = scan.md(s, f);
            let (body, finalizer) = match scan.motion {
                Motion::Step { intervals, settle } => {
                    let body = {
                        let (scan, scaler) = (scan.clone(), scaler.clone());
                        plan_fn(move |ctx| async move {
                            scan.step(&ctx, &scaler, (s, f), intervals, settle, &recorded)
                                .await
                        })
                    };
                    let finalizer = plan_fn(move |ctx| async move {
                        clean_up(&ctx, &scan.axis, &scaler).await?;
                        if scan.relative {
                            ctx.mv(&[scan.axis.to(p0)]).await?;
                        }
                        Ok(())
                    });
                    (body, finalizer)
                }
                // An interrupted cscan is SPEC's to clean up (`cleanup_once`)
                // once it has its ^C.
                Motion::Fly { .. } => {
                    let body = {
                        let cmd = cmd.clone();
                        plan_fn(move |ctx| async move { scan.fly(&ctx, cmd, &recorded).await })
                    };
                    let finalizer = plan_fn(move |ctx| async move {
                        ctx.call(async move {
                            cmd.abort_if_busy().await?;
                            cmd.wait_ready().await
                        })
                        .await
                    });
                    (body, finalizer)
                }
            };
            let run = finalize_wrapper(run_wrapper(body, md), finalizer);
            ctx.run(msg_mutator(run, in_scan_run)).await
        });
        (plan, points)
    }

    fn md(&self, s: f64, f: f64) -> bsrs::core::RunMetadata {
        let m = self.axis.name();
        let mut args = json!({
            "motor": m,
            "start": self.start,
            "finish": self.finish,
            "time": self.ctime,
        });
        let mut extra = json!({
            "motors": [m],
            "detectors": ["scaler"],
            "det": self.det,
            "start": s,
            "finish": f,
            "hints": {"dimensions": [[[m], "primary"]]},
        });
        // A fly scan's events are timed when SPEC's points arrive, not
        // when SPEC took them: no Epoch column.
        extra[SPEC_HEAD] = json!({
            "file": self.spec_file,
            "heading": self.heading(s, f),
            "ctime": self.ctime,
            "epoch": matches!(self.motion, Motion::Step { .. }),
        });
        match self.motion {
            Motion::Step { intervals, settle } => {
                args["intervals"] = json!(intervals);
                args["settle"] = json!(settle.as_secs_f64());
                extra["num_points"] = json!(intervals + 1);
            }
            Motion::Fly { sleep } => args["sleep"] = json!(sleep.as_secs_f64()),
        }
        extra["plan_args"] = args;
        run_md(self.name(), extra, HashMap::new())
    }

    /// `_ascan`'s point: move, read the motor, `_sleep`, count.
    async fn step(
        &self,
        ctx: &Ctx,
        scaler: &Arc<Scaler>,
        (s, f): (f64, f64),
        intervals: usize,
        settle: Duration,
        points: &Points,
    ) -> Step {
        let axis = &self.axis;
        let limits = check_limits(axis.clone(), s, f);
        ctx.call(limits).await?;
        let d = (f - s) / intervals as f64;
        let count = Arc::new(Count {
            scaler: scaler.clone(),
            seconds: self.ctime,
        });
        for i in 0..=intervals {
            ctx.mv(&[axis.to(s + i as f64 * d)]).await?;
            ctx.create("primary").await?;
            let x = number(&ctx.read(axis.clone()).await?, axis.name())?;
            if !settle.is_zero() {
                ctx.sleep(settle).await?;
            }
            ctx.trigger(count.clone(), "count").await?;
            ctx.wait("count").await?;
            let y = number(&ctx.read(count.clone()).await?, self.det)?;
            ctx.save().await?;
            points.push(x, y);
        }
        Ok(())
    }

    /// SPEC's `cscan`/`clup`: an event per point, with SPEC's columns, as
    /// SPEC takes it; at the end SPEC's points of the scan bring those
    /// not yet sent. A scan that fails or is aborted keeps the events
    /// sent.
    async fn fly(&self, ctx: &Ctx, cmd: Arc<SpecCmd>, points: &Points) -> Step {
        let line = self.command();
        let mut live = {
            let line = line.clone();
            ctx.call(async move { cmd.start_scan(&line).await }).await?
        };
        let mut sent = Sent::default();
        loop {
            let (next, table) = match ctx.call(live.next()).await? {
                Live::Running(next, table) => (Some(next), table),
                Live::Ended(table) => (None, table),
            };
            self.send(ctx, &line, &mut sent, &table, points).await?;
            match next {
                Some(next) => live = next,
                None => return Ok(()),
            }
        }
    }

    /// An event per point of `table` past those sent.
    async fn send(
        &self,
        ctx: &Ctx,
        line: &str,
        sent: &mut Sent,
        table: &Table,
        points: &Points,
    ) -> Step {
        let rows = sent.past(table).map_err(|e| format!("{line}: {e}"))?;
        if rows.is_empty() {
            return Ok(());
        }
        let (holder, det) = match &sent.under {
            Some((_, holder, det)) => (holder.clone(), *det),
            None => {
                let (holder, det) = self.holder(line, &table.cols)?;
                sent.under = Some((table.cols.clone(), holder.clone(), det));
                (holder, det)
            }
        };
        for row in rows {
            holder.set(table.cols.iter().cloned().zip(row.iter().map(|v| json!(v))));
            ctx.create("primary").await?;
            ctx.read(holder.clone()).await?;
            ctx.save().await?;
            points.push(row[0], row[det]);
            sent.rows.push(row.clone());
        }
        Ok(())
    }

    /// The events' device for SPEC's columns `cols`, and the index of
    /// `DET` among them.
    fn holder(&self, line: &str, cols: &[String]) -> Step<(Arc<Holder>, usize)> {
        let m = self.axis.name();
        if cols[0] != m {
            return Err(format!("{line}: SPEC's first column is {}", cols[0]));
        }
        let det = cols
            .iter()
            .position(|c| c == self.det)
            .ok_or_else(|| format!("{line}: SPEC's points have no {} column", self.det))?;
        let keys: Vec<(&str, Dtype)> = cols.iter().map(|c| (c.as_str(), Dtype::Number)).collect();
        Ok((Holder::new(self.name(), &keys), det))
    }
}

/// A fly scan's points sent as events, with which every later view of
/// the scan's points must begin.
#[derive(Default)]
struct Sent {
    /// From the first point: SPEC's columns, the events' device for them
    /// and `DET`'s index among them.
    under: Option<(Vec<String>, Arc<Holder>, usize)>,
    rows: Vec<Vec<f64>>,
}

impl Sent {
    /// `table`'s rows past those sent.
    fn past<'t>(&self, table: &'t Table) -> Step<&'t [Vec<f64>]> {
        let Some((cols, ..)) = &self.under else {
            return Ok(&table.rows);
        };
        let n = self.rows.len();
        if table.cols != *cols || table.rows.get(..n) != Some(&self.rows[..]) {
            return Err(format!(
                "SPEC's points of the scan do not begin with the {n} sent as they came"
            ));
        }
        Ok(&table.rows[n..])
    }
}

/// The bundler verbs go to the scan's run, the rest to the engine as is.
fn in_scan_run(m: Msg) -> Msg {
    match m {
        Msg::OpenRun(_)
        | Msg::CloseRun { .. }
        | Msg::Create { .. }
        | Msg::Save
        | Msg::Drop
        | Msg::Read(_) => Msg::in_run(SCAN_RUN, m),
        m => m,
    }
}

fn number(data: &HashMap<String, ReadingValue>, key: &str) -> Step<f64> {
    data.get(key)
        .and_then(|r| r.value.as_f64())
        .ok_or_else(|| format!("reading has no number {key}"))
}

/// `_chk_lim` of both ends.
async fn check_limits(axis: Arc<Axis>, s: f64, f: f64) -> BResult<()> {
    let Some((lo, hi)) = axis.limits().await? else {
        return Ok(());
    };
    for p in [s, f] {
        let hit = if p < lo {
            Some(("low", lo))
        } else if p > hi {
            Some(("high", hi))
        } else {
            None
        };
        if let Some((side, lim)) = hit {
            return Err(BsrsError::Other(format!(
                "{} will hit {side} limit at {}.",
                axis.desc(),
                g(lim)
            )));
        }
    }
    Ok(())
}

/// An interrupted step scan's stop, whatever the scan's end: the count
/// and the motor stopped. Both steps run; the first failure fails the
/// plan after.
async fn clean_up(ctx: &Ctx, axis: &Arc<Axis>, scaler: &Arc<Scaler>) -> Step {
    let (axis, scaler) = (axis.clone(), scaler.clone());
    let failed = ctx
        .call(async move {
            let mut first = None;
            keep_first(&mut first, scaler.stop_if_counting().await);
            let stopped = match axis.stop_if_moving().await {
                Ok(()) => axis.settle(SETTLE).await,
                e => e,
            };
            keep_first(&mut first, stopped);
            Ok(first.map(|e: BsrsError| format!("{} clean-up: {e}", axis.name())))
        })
        .await?;
    match failed {
        Some(e) => {
            tracing::warn!("{e}");
            Err(e)
        }
        None => Ok(()),
    }
}

fn keep_first(first: &mut Option<BsrsError>, r: BResult<()>) {
    if let Err(e) = r {
        first.get_or_insert(e);
    }
}

/// C `%g`.
pub fn g(v: f64) -> String {
    g_digits(v, 6)
}

/// C `%.<digits>g`: `digits` significant digits, the exponent form below
/// 1e-4 and from 10^digits, no trailing zeros.
pub fn g_digits(v: f64, digits: usize) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v == 0.0 || v.is_infinite() {
        return format!("{v}");
    }
    let p = digits.max(1);
    let e = format!("{v:.prec$e}", prec = p - 1);
    let (mantissa, exp) = e.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("{:e}'s exponent is an integer");
    if exp < -4 || exp >= p as i32 {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exp.abs())
    } else {
        let decimals = (p as i32 - 1 - exp) as usize;
        trim(&format!("{v:.decimals$}")).to_string()
    }
}

fn trim(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_matches_c() {
        // printf("%g") of each.
        for (v, c) in [
            (0.0, "0"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (0.000123456789, "0.000123457"),
            (123456.0, "123456"),
            (999999.4, "999999"),
            (999999.5, "1e+06"),
            (1234567.0, "1.23457e+06"),
            (4000.0, "4000"),
            (1.0 / 3.0, "0.333333"),
            (-1.5e-7, "-1.5e-07"),
        ] {
            assert_eq!(g(v), c, "{v}");
        }
    }

    #[test]
    fn later_views_of_a_fly_scan_begin_with_the_points_sent() {
        let table = |cols: &[&str], rows: &[[f64; 2]]| Table {
            cols: cols.iter().map(|c| c.to_string()).collect(),
            rows: rows.iter().map(|r| r.to_vec()).collect(),
        };
        let mut sent = Sent::default();
        let first = table(&["th", "det"], &[[1.0, 10.0]]);
        assert_eq!(sent.past(&first).unwrap(), &first.rows[..]);
        sent.under = Some((first.cols.clone(), Holder::new("cscan", &[]), 1));
        sent.rows = first.rows.clone();
        let more = table(&["th", "det"], &[[1.0, 10.0], [2.0, 20.0]]);
        assert_eq!(sent.past(&more).unwrap(), &more.rows[1..]);
        assert_eq!(sent.past(&first).unwrap().len(), 0);
        assert!(sent.past(&table(&["th", "det"], &[])).is_err(), "fewer");
        let other = table(&["th", "det"], &[[1.0, 11.0], [2.0, 20.0]]);
        assert!(sent.past(&other).is_err(), "another first point");
        let renamed = table(&["th", "ic1"], &[[1.0, 10.0], [2.0, 20.0]]);
        assert!(sent.past(&renamed).is_err(), "other columns");
    }

    #[test]
    fn g8_matches_c() {
        // printf("%.8g") of each.
        for (v, c) in [
            (-2.874_312_345_6, "-2.8743123"),
            (12345678.0, "12345678"),
            (123456789.0, "1.2345679e+08"),
            (0.000_012_345_678_9, "1.2345679e-05"),
            (0.1, "0.1"),
            (f64::NAN, "nan"),
        ] {
            assert_eq!(g_digits(v, 8), c, "{v}");
        }
    }
}
