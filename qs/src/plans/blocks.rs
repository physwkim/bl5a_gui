//! An alignment as blocks, run in their order: a queue item carries the
//! blocks it runs, as a list of objects, each with a `kind`:
//!
//! - `scan`: an alignment scan (`type` ascan, lup, cscan or clup), then the
//!   motor to the scan's `pick` (CEN, COM, xMAX, EDGE), of its dip given
//!   `dip` true (not EDGE), and, given `set`, `set motor <set>`;
//! - `move`: `umv`, the `moves` ([motor, position] pairs) together;
//! - `spec`: a SPEC command line;
//! - `wait`: `seconds`;
//! - `erase`: the mar345's plate, as the macros erase it after aligning;
//! - `loop`: its own `blocks`, in order, `times` over.
//!
//! Any block may carry `when` ({motor, op "<" or ">", value}), run only
//! when the motor is there (a loop's, once before it starts), and a
//! `note`. The GUI keeps the macros'
//! sequences and each user's saved ones; qs keeps none.

use std::time::Duration;

use bsrs::event_model::Dtype;
use serde_json::{Map, Value, json};

use super::Ops;
use crate::ctx::Step;
use crate::devices::{AXES, CHANNELS, Holder};
use crate::scan::{Motion, Scan};
use crate::screen::{Pick, Stat};

/// The positions an alignment's "final" event records once its blocks
/// ran: coin_align's closing `wm2`.
const FINAL: [&str; 5] = ["sx", "sy", "shold", "th", "phi"];

#[derive(Debug, PartialEq)]
pub(super) struct Block {
    action: Action,
    when: Option<When>,
}

#[derive(Debug, PartialEq)]
enum Action {
    Scan(ScanBlock),
    Move(Vec<(&'static str, f64)>),
    Spec(String),
    Wait(f64),
    Erase,
    Loop { times: usize, blocks: Vec<Block> },
}

#[derive(Debug, PartialEq)]
struct ScanBlock {
    motor: &'static str,
    start: f64,
    finish: f64,
    relative: bool,
    /// Seconds per point (ascan, lup) or of the whole move (cscan, clup).
    time: f64,
    motion: Motion,
    counter: &'static str,
    pick: Pick,
    set: Option<f64>,
}

#[derive(Debug, PartialEq)]
struct When {
    motor: &'static str,
    below: bool,
    value: f64,
}

impl When {
    fn holds(&self, position: f64) -> bool {
        if self.below {
            position < self.value
        } else {
            position > self.value
        }
    }
}

impl ScanBlock {
    fn scan(&self, ops: &Ops<'_>) -> Scan {
        Scan {
            axis: ops.dev.axis(self.motor),
            start: self.start,
            finish: self.finish,
            relative: self.relative,
            ctime: self.time,
            motion: self.motion,
            det: self.counter,
            spec_file: ops.place.spec.clone(),
        }
    }
}

/// One block's fields, each taken once; what is left over is an error.
struct Fields<'a> {
    at: String,
    map: &'a Map<String, Value>,
    taken: Vec<&'static str>,
}

impl<'a> Fields<'a> {
    fn err(&self, what: impl std::fmt::Display) -> String {
        format!("{}: {what}", self.at)
    }

    /// The value of `key`, `None` when absent or null.
    fn take(&mut self, key: &'static str) -> Option<&'a Value> {
        self.taken.push(key);
        self.map.get(key).filter(|v| !v.is_null())
    }

    fn need(&mut self, key: &'static str) -> Result<&'a Value, String> {
        self.take(key)
            .ok_or_else(|| self.err(format!("{key} is missing")))
    }

    fn number(&self, key: &str, v: &Value) -> Result<f64, String> {
        v.as_f64()
            .filter(|x| x.is_finite())
            .ok_or_else(|| self.err(format!("{key} must be a number, got {v}")))
    }

    fn num(&mut self, key: &'static str) -> Result<f64, String> {
        let v = self.need(key)?;
        self.number(key, v)
    }

    fn num_opt(&mut self, key: &'static str) -> Result<Option<f64>, String> {
        self.take(key).map(|v| self.number(key, v)).transpose()
    }

    /// A number `ok` accepts, `rule` naming what it must be.
    fn num_if(
        &mut self,
        key: &'static str,
        ok: impl Fn(f64) -> bool,
        rule: &str,
    ) -> Result<f64, String> {
        let x = self.num(key)?;
        if !ok(x) {
            return Err(self.err(format!("{key} must be {rule}, got {x}")));
        }
        Ok(x)
    }

    /// A boolean, `false` when absent or null.
    fn flag(&mut self, key: &'static str) -> Result<bool, String> {
        match self.take(key) {
            None => Ok(false),
            Some(v) => v
                .as_bool()
                .ok_or_else(|| self.err(format!("{key} must be true or false, got {v}"))),
        }
    }

    fn text(&mut self, key: &'static str) -> Result<&'a str, String> {
        let v = self.need(key)?;
        v.as_str()
            .ok_or_else(|| self.err(format!("{key} must be a string, got {v}")))
    }

    fn one_of(
        &mut self,
        key: &'static str,
        names: &[&'static str],
    ) -> Result<&'static str, String> {
        let s = self.text(key)?;
        names
            .iter()
            .copied()
            .find(|n| *n == s)
            .ok_or_else(|| self.err(format!("{key} must be one of {names:?}, got {s:?}")))
    }

    fn done(self) -> Result<(), String> {
        match self.map.keys().find(|k| !self.taken.contains(&k.as_str())) {
            Some(k) => Err(self.err(format!("unknown field {k}"))),
            None => Ok(()),
        }
    }
}

/// The scan types, as the macros name them: (type, relative, fly).
const TYPES: [(&str, bool, bool); 4] = [
    ("ascan", false, false),
    ("lup", true, false),
    ("cscan", false, true),
    ("clup", true, true),
];
/// The picks by name: a statistic of a peak or a dip, or `None` for EDGE.
const PICKS: [(&str, Option<Stat>); 4] = [
    ("CEN", Some(Stat::Cen)),
    ("COM", Some(Stat::Com)),
    ("xMAX", Some(Stat::XMax)),
    ("EDGE", None),
];
const KINDS: [&str; 6] = ["scan", "move", "spec", "wait", "erase", "loop"];

fn counters() -> Vec<&'static str> {
    CHANNELS.into_iter().filter(|c| *c != "sec").collect()
}

fn scan(f: &mut Fields<'_>) -> Result<ScanBlock, String> {
    let names = TYPES.map(|t| t.0);
    let ty = f.one_of("type", &names)?;
    let &(_, relative, fly) = TYPES.iter().find(|t| t.0 == ty).expect("one of TYPES");
    let motor = f.one_of("motor", &AXES)?;
    let (start, finish) = (f.num("start")?, f.num("finish")?);
    let time = f.num_if("time", |t| t > 0.0, "seconds > 0")?;
    let motion = if fly {
        let sleep = f.num_if("sleep", |s| s >= 0.0, "seconds >= 0")?;
        Motion::Fly {
            sleep: Duration::from_secs_f64(sleep),
        }
    } else {
        let n = f.num_if(
            "intervals",
            |n| n >= 1.0 && n.fract() == 0.0,
            "a whole number >= 1",
        )?;
        let settle = f.num_if("settle", |s| s >= 0.0, "seconds >= 0")?;
        Motion::Step {
            intervals: n as usize,
            settle: Duration::from_secs_f64(settle),
        }
    };
    let counter = f.one_of("counter", &counters())?;
    let pick = f.one_of("pick", &PICKS.map(|p| p.0))?;
    let stat = PICKS.iter().find(|p| p.0 == pick).expect("one of PICKS").1;
    let pick = match (stat, f.flag("dip")?) {
        (Some(s), false) => Pick::Peak(s),
        (Some(s), true) => Pick::Dip(s),
        (None, false) => Pick::Edge,
        (None, true) => return Err(f.err("dip is for CEN, COM and xMAX, not EDGE")),
    };
    Ok(ScanBlock {
        motor,
        start,
        finish,
        relative,
        time,
        motion,
        counter,
        pick,
        set: f.num_opt("set")?,
    })
}

fn moves(f: &mut Fields<'_>) -> Result<Vec<(&'static str, f64)>, String> {
    let v = f.need("moves")?;
    let pairs = v
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| f.err(format!("moves must be [motor, position] pairs, got {v}")))?;
    let mut out: Vec<(&'static str, f64)> = Vec::new();
    for p in pairs {
        let (m, x) = match p.as_array().map(Vec::as_slice) {
            Some([m, x]) => (m.as_str(), x.as_f64().filter(|x| x.is_finite())),
            _ => (None, None),
        };
        let (Some(m), Some(x)) = (m, x) else {
            return Err(f.err(format!("moves: [motor, position], got {p}")));
        };
        let m = AXES
            .into_iter()
            .find(|a| *a == m)
            .ok_or_else(|| f.err(format!("moves: motor must be one of {AXES:?}, got {m:?}")))?;
        if out.iter().any(|(o, _)| *o == m) {
            return Err(f.err(format!("moves: {m} twice")));
        }
        out.push((m, x));
    }
    Ok(out)
}

fn when(f: &mut Fields<'_>) -> Result<Option<When>, String> {
    let Some(v) = f.take("when") else {
        return Ok(None);
    };
    let map = v
        .as_object()
        .ok_or_else(|| f.err(format!("when must be {{motor, op, value}}, got {v}")))?;
    let mut w = Fields {
        at: format!("{} when", f.at),
        map,
        taken: Vec::new(),
    };
    let motor = w.one_of("motor", &AXES)?;
    let below = w.one_of("op", &["<", ">"])? == "<";
    let value = w.num("value")?;
    w.done()?;
    Ok(Some(When {
        motor,
        below,
        value,
    }))
}

fn block(at: String, v: &Value) -> Result<Block, String> {
    let map = v
        .as_object()
        .ok_or_else(|| format!("{at}: must be an object, got {v}"))?;
    let mut f = Fields {
        at,
        map,
        taken: Vec::new(),
    };
    let kind = f.one_of("kind", &KINDS)?;
    f.at = format!("{} ({kind})", f.at);
    if let Some(note) = f.take("note")
        && !note.is_string()
    {
        return Err(f.err(format!("note must be a string, got {note}")));
    }
    let when = when(&mut f)?;
    let action = match kind {
        "scan" => Action::Scan(scan(&mut f)?),
        "move" => Action::Move(moves(&mut f)?),
        "spec" => {
            let line = f.text("command")?.trim();
            if line.is_empty() || line.contains(['\n', '\r']) {
                return Err(f.err(format!("command must be one line, got {line:?}")));
            }
            Action::Spec(line.to_string())
        }
        "wait" => Action::Wait(f.num_if("seconds", |s| s > 0.0, "seconds > 0")?),
        "erase" => Action::Erase,
        "loop" => {
            let times = f.num_if(
                "times",
                |n| n >= 1.0 && n.fract() == 0.0,
                "a whole number >= 1",
            )?;
            let v = f.need("blocks")?;
            let list = v
                .as_array()
                .filter(|a| !a.is_empty())
                .ok_or_else(|| f.err(format!("blocks must be a list of blocks, got {v}")))?;
            let blocks = list
                .iter()
                .enumerate()
                .map(|(i, v)| block(format!("{} block {}", f.at, i + 1), v))
                .collect::<Result<_, _>>()?;
            Action::Loop {
                times: times as usize,
                blocks,
            }
        }
        _ => unreachable!("one of KINDS"),
    };
    f.done()?;
    Ok(Block { action, when })
}

/// `blocks`, a list of block objects; an error names the plan, the block
/// by its place (from 1) and kind, and the field.
pub(super) fn parse(plan: &str, blocks: &Value) -> Result<Vec<Block>, String> {
    let list = blocks
        .as_array()
        .ok_or_else(|| format!("{plan}: blocks must be a list, got {blocks}"))?;
    list.iter()
        .enumerate()
        .map(|(i, v)| block(format!("{plan}: block {}", i + 1), v))
        .collect()
}

/// The blocks in order, then a "final" event of [`FINAL`]'s positions.
pub(super) async fn run(ops: &Ops<'_>, blocks: &[Block]) -> Step {
    steps(ops, blocks).await?;
    let keys = FINAL.map(|k| (k, Dtype::Number));
    let mut values = Vec::new();
    for m in FINAL {
        values.push((m, json!(ops.position(m).await?)));
    }
    ops.event(&Holder::new("final", &keys), values).await
}

async fn steps(ops: &Ops<'_>, blocks: &[Block]) -> Step {
    for b in blocks {
        if let Some(w) = &b.when
            && !w.holds(ops.position(w.motor).await?)
        {
            continue;
        }
        match &b.action {
            Action::Scan(s) => ops.stage(s.scan(ops), s.pick, s.set).await?,
            Action::Move(moves) => ops.umv(moves).await?,
            Action::Spec(line) => ops.spec(line).await?,
            Action::Wait(seconds) => ops.sleep(*seconds).await?,
            Action::Erase => ops.erase().await?,
            Action::Loop { times, blocks } => {
                for _ in 0..*times {
                    Box::pin(steps(ops, blocks)).await?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The GUI's templates: the macros' sequences, which qs must take.
    const TEMPLATES: [(&str, &str); 2] = [
        (
            "coin",
            include_str!("../../../gui/bl5a_gui/templates/coin.json"),
        ),
        (
            "film",
            include_str!("../../../gui/bl5a_gui/templates/film.json"),
        ),
    ];

    fn template(name: &str) -> Vec<Block> {
        let (_, text) = TEMPLATES.iter().find(|t| t.0 == name).unwrap();
        let file: Value = serde_json::from_str(text).unwrap();
        parse(name, &file["blocks"]).unwrap()
    }

    fn one(v: Value) -> Result<Block, String> {
        parse("p", &json!([v])).map(|mut b| b.remove(0))
    }

    #[test]
    fn templates_are_the_macros_sequences() {
        let coin = template("coin");
        assert_eq!(coin.len(), 11);
        assert_eq!(
            coin[6].action,
            Action::Scan(ScanBlock {
                motor: "sy",
                start: -2.5,
                finish: 2.5,
                relative: true,
                time: 12.5,
                motion: Motion::Fly {
                    sleep: Duration::from_millis(100)
                },
                counter: "ic1",
                pick: Pick::Peak(Stat::Com),
                set: None,
            })
        );
        let film = template("film");
        assert_eq!(film.len(), 30);
        // align_in.mac's `if (A[martran] < 1200) mar_ip_mode off`.
        assert_eq!(
            film[4],
            Block {
                action: Action::Spec("mar_ip_mode off".into()),
                when: Some(When {
                    motor: "martran",
                    below: true,
                    value: 1200.0
                }),
            }
        );
        let scans: Vec<_> = film
            .iter()
            .filter_map(|b| match &b.action {
                Action::Scan(s) => Some((s.motor, s.pick, s.set)),
                _ => None,
            })
            .collect();
        assert_eq!(
            scans,
            [
                ("tth", Pick::Peak(Stat::Cen), Some(0.0)),
                ("nu", Pick::Peak(Stat::Cen), Some(0.0)),
                ("shold", Pick::Edge, None),
                ("th", Pick::Peak(Stat::XMax), Some(0.0)),
                ("shold", Pick::Edge, None),
                ("th", Pick::Peak(Stat::Cen), None),
                ("shold", Pick::Edge, None),
                ("th", Pick::Peak(Stat::Cen), Some(0.0)),
                ("shold", Pick::Edge, None),
                ("th", Pick::Peak(Stat::XMax), Some(0.6)),
            ]
        );
        assert_eq!(film.last().unwrap().action, Action::Erase);
    }

    #[test]
    fn each_kind_reads_its_fields() {
        let b = one(
            json!({"kind": "scan", "type": "lup", "motor": "th", "start": -0.6,
            "finish": 0.6, "intervals": 30, "time": 0.5, "settle": 0.1, "counter": "det",
            "pick": "CEN", "set": 0, "note": "half-cut"}),
        )
        .unwrap();
        let Action::Scan(s) = b.action else { panic!() };
        assert!(s.relative);
        assert_eq!(
            s.motion,
            Motion::Step {
                intervals: 30,
                settle: Duration::from_millis(100)
            }
        );
        assert_eq!(s.set, Some(0.0));
        assert_eq!(
            one(json!({"kind": "move", "moves": [["tth", 1], ["th", 0]]}))
                .unwrap()
                .action,
            Action::Move(vec![("tth", 1.0), ("th", 0.0)])
        );
        assert_eq!(
            one(json!({"kind": "spec", "command": " atte 0 "}))
                .unwrap()
                .action,
            Action::Spec("atte 0".into())
        );
        assert_eq!(
            one(json!({"kind": "wait", "seconds": 3})).unwrap().action,
            Action::Wait(3.0)
        );
        let b = one(json!({"kind": "erase", "when": {"motor": "martran", "op": ">", "value": 1}}))
            .unwrap();
        assert_eq!(b.action, Action::Erase);
        let w = b.when.unwrap();
        assert!(w.holds(2.0) && !w.holds(1.0) && !w.holds(0.0));
        assert!(parse("p", &json!([])).unwrap().is_empty());
        let plain = |action| Block { action, when: None };
        assert_eq!(
            one(json!({"kind": "loop", "times": 3, "blocks": [
                {"kind": "wait", "seconds": 1},
                {"kind": "loop", "times": 2, "blocks": [{"kind": "erase"}]},
            ]}))
            .unwrap()
            .action,
            Action::Loop {
                times: 3,
                blocks: vec![
                    plain(Action::Wait(1.0)),
                    plain(Action::Loop {
                        times: 2,
                        blocks: vec![plain(Action::Erase)]
                    }),
                ]
            }
        );
    }

    #[test]
    fn errors_name_the_block_and_field() {
        let scan = json!({"kind": "scan", "type": "cscan", "motor": "sy", "start": -1,
            "finish": 1, "time": 5, "sleep": 0.1, "counter": "ic1", "pick": "COM"});
        assert!(one(scan.clone()).is_ok());
        let with = |k: &str, v: Value| {
            let mut b = scan.clone();
            b[k] = v;
            one(b).unwrap_err()
        };
        assert_eq!(
            with("motor", json!("zz")),
            format!("p: block 1 (scan): motor must be one of {AXES:?}, got \"zz\"")
        );
        assert_eq!(
            with("time", json!(0)),
            "p: block 1 (scan): time must be seconds > 0, got 0"
        );
        assert_eq!(
            with("intervals", json!(10)),
            "p: block 1 (scan): unknown field intervals"
        );
        assert!(with("counter", json!("sec")).contains("counter must be one of"));
        assert!(with("pick", json!("cen")).contains("pick must be one of"));
        assert!(with("dip", json!(1)).contains("dip must be true or false, got 1"));
        let pick = |b: Value| -> Result<Pick, String> {
            match one(b)?.action {
                Action::Scan(s) => Ok(s.pick),
                other => panic!("{other:?}"),
            }
        };
        let mut dip = scan.clone();
        dip["dip"] = json!(true);
        assert_eq!(pick(dip.clone()), Ok(Pick::Dip(Stat::Com)));
        dip["dip"] = json!(false);
        assert_eq!(pick(dip.clone()), Ok(Pick::Peak(Stat::Com)));
        dip["dip"] = json!(true);
        dip["pick"] = json!("EDGE");
        assert_eq!(
            pick(dip),
            Err("p: block 1 (scan): dip is for CEN, COM and xMAX, not EDGE".into())
        );
        assert!(with("set", json!("0")).contains("set must be a number"));
        let e = parse("p", &json!([{"kind": "erase"}, {"kind": "jump"}])).unwrap_err();
        assert!(e.starts_with("p: block 2: kind must be one of"), "{e}");
        for (bad, why) in [
            (json!({"kind": "move", "moves": []}), "moves must be"),
            (
                json!({"kind": "move", "moves": [["th", 1], ["th", 2]]}),
                "th twice",
            ),
            (
                json!({"kind": "move", "moves": [["th"]]}),
                "[motor, position]",
            ),
            (json!({"kind": "spec", "command": "a\nb"}), "one line"),
            (json!({"kind": "spec", "command": "  "}), "one line"),
            (json!({"kind": "wait", "seconds": 0}), "seconds > 0"),
            (
                json!({"kind": "erase", "when": {"motor": "th", "op": "=", "value": 1}}),
                "op must be",
            ),
            (
                json!({"kind": "erase", "when": {"motor": "th", "op": "<"}}),
                "when: value is missing",
            ),
            (json!({"kind": "erase", "note": 3}), "note must be a string"),
            (
                json!({"kind": "loop", "times": 0, "blocks": [{"kind": "erase"}]}),
                "times must be a whole number >= 1, got 0",
            ),
            (
                json!({"kind": "loop", "times": 1.5, "blocks": [{"kind": "erase"}]}),
                "a whole number >= 1",
            ),
            (
                json!({"kind": "loop", "times": 2, "blocks": []}),
                "blocks must be a list of blocks",
            ),
            (json!({"kind": "loop", "times": 2}), "blocks is missing"),
            (json!("erase"), "must be an object"),
        ] {
            let e = one(bad.clone()).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        // A block inside a loop is named by its place in each.
        let e = one(json!({"kind": "loop", "times": 2, "blocks": [
            {"kind": "erase"},
            {"kind": "loop", "times": 2, "blocks": [{"kind": "wait"}]},
        ]}))
        .unwrap_err();
        assert_eq!(
            e,
            "p: block 1 (loop) block 2 (loop) block 1 (wait): seconds is missing"
        );
        assert!(
            parse("p", &json!({"kind": "erase"}))
                .unwrap_err()
                .contains("blocks must be a list")
        );
    }
}
