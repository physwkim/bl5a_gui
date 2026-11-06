//! The daemon's document stream, reduced to what the stacks take: each
//! "exposures" event, with the data folder and the series of its run. A
//! run's folder is its start's `data_dir`; its series is the `series` its
//! queue item's metadata gave, or else the run's uid.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::Value;

use crate::series::Shot;

/// One frame of the stream, `<prefix> <name> <msgpack document>`.
pub fn parse(frame: &[u8]) -> Result<(String, Value), String> {
    let mut parts = frame.splitn(3, |&b| b == b' ');
    let (Some(_), Some(name), Some(body)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("a {}-byte frame without a name", frame.len()));
    };
    let name = String::from_utf8_lossy(name).into_owned();
    let doc = rmp_serde::from_slice(body).map_err(|e| format!("{name} document: {e}"))?;
    Ok((name, doc))
}

#[derive(Debug, PartialEq)]
pub struct Exposure {
    pub dir: PathBuf,
    pub series: String,
    pub shot: Shot,
}

/// What a run's start says about where its rows go.
struct Run {
    /// `data_dir`, if the start gave an absolute one.
    dir: Option<PathBuf>,
    series: String,
}

/// An "exposures" stream's run and where its rows go.
struct Target {
    run: String,
    dir: PathBuf,
    series: String,
}

#[derive(Default)]
pub struct Runs {
    /// Run uid → where its rows go.
    runs: HashMap<String, Run>,
    /// The "exposures" descriptors' uids → their run and its rows' place.
    exposures: HashMap<String, Target>,
    /// Every descriptor uid seen, each with its run uid.
    descriptors: HashMap<String, String>,
    /// Descriptors whose events came without them, each reported once.
    unseen: HashSet<String>,
}

impl Runs {
    /// The exposure `doc` is, if it is one. An error names a document
    /// that should have given one and could not.
    pub fn feed(&mut self, name: &str, doc: &Value) -> Result<Option<Exposure>, String> {
        let text = |k: &str| doc.get(k).and_then(Value::as_str).map(str::to_string);
        match name {
            "start" => {
                let uid = text("uid").ok_or("a start without a uid")?;
                let series = text("series")
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| uid.clone());
                let dir = text("data_dir")
                    .map(PathBuf::from)
                    .filter(|d| d.is_absolute());
                self.runs.insert(uid, Run { dir, series });
            }
            "descriptor" => {
                let (Some(uid), Some(run)) = (text("uid"), text("run_start")) else {
                    return Err("a descriptor without a uid or run_start".into());
                };
                // Seen even when refused: its events are not reported again.
                self.descriptors.insert(uid.clone(), run.clone());
                if text("name").as_deref() == Some("exposures") {
                    let target = match self.runs.get(&run) {
                        None => {
                            return Err(format!(
                                "exposures of run {run}, whose start was not seen"
                            ));
                        }
                        Some(Run { dir: None, .. }) => {
                            return Err(format!(
                                "exposures of run {run}, whose start names no absolute data_dir"
                            ));
                        }
                        Some(Run {
                            dir: Some(dir),
                            series,
                        }) => Target {
                            run: run.clone(),
                            dir: dir.clone(),
                            series: series.clone(),
                        },
                    };
                    self.exposures.insert(uid, target);
                }
            }
            "event" => {
                let descriptor = text("descriptor").ok_or("an event without a descriptor")?;
                if let Some(t) = self.exposures.get(&descriptor) {
                    let run = &t.run;
                    let shot = shot(run, doc).map_err(|e| format!("exposure of run {run}: {e}"))?;
                    return Ok(Some(Exposure {
                        dir: t.dir.clone(),
                        series: t.series.clone(),
                        shot,
                    }));
                }
                if !self.descriptors.contains_key(&descriptor)
                    && self.unseen.insert(descriptor.clone())
                {
                    return Err(format!(
                        "events of descriptor {descriptor}, which was not seen"
                    ));
                }
            }
            "stop" => {
                if let Some(run) = text("run_start") {
                    self.runs.remove(&run);
                    self.exposures.retain(|_, t| t.run != run);
                    self.descriptors.retain(|_, r| *r != run);
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

fn shot(run: &str, doc: &Value) -> Result<Shot, String> {
    let data = doc.get("data").ok_or("no data")?;
    let real = |k: &str| {
        data.get(k)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("no number {k}"))
    };
    let int = |v: Option<&Value>, k: &str| {
        v.and_then(Value::as_i64)
            .ok_or_else(|| format!("no integer {k}"))
    };
    Ok(Shot {
        time: doc.get("time").and_then(Value::as_f64).ok_or("no time")?,
        run_uid: run.to_string(),
        seq: int(doc.get("seq_num"), "seq_num")?,
        file: data
            .get("file")
            .and_then(Value::as_str)
            .ok_or("no file")?
            .to_string(),
        file_number: int(data.get("file_number"), "file_number")?,
        th: real("th")?,
        shold: real("shold")?,
        mon: real("icxrd")?,
        texpo: real("texpo")?,
        sec: real("sec")?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn start(uid: &str, md: Value) -> Value {
        let mut d = json!({"uid": uid, "time": 1.0, "plan_name": "mar_series", "data_dir": "/d/f"});
        d.as_object_mut()
            .unwrap()
            .extend(md.as_object().unwrap().clone());
        d
    }

    fn descriptor(uid: &str, run: &str, name: &str) -> Value {
        json!({"uid": uid, "run_start": run, "name": name, "data_keys": {}})
    }

    fn exposure(descriptor: &str, seq: i64) -> Value {
        json!({
            "uid": format!("e{seq}"), "descriptor": descriptor, "time": 2.0, "seq_num": seq,
            "data": {"name": "t2", "file": "/d/t2_0001.mar3450", "file_number": 1, "texpo": 2.0,
                     "th": 0.1, "shold": -0.2, "sec": 2.0, "icxrd": 5e4, "det": 1.0, "ic1": 3.0},
            "timestamps": {},
        })
    }

    fn feed(runs: &mut Runs, docs: &[(&str, Value)]) -> Vec<Result<Option<Exposure>, String>> {
        docs.iter().map(|(n, d)| runs.feed(n, d)).collect()
    }

    #[test]
    fn an_exposure_carries_its_runs_series() {
        let mut runs = Runs::default();
        let out = feed(
            &mut runs,
            &[
                ("start", start("r1", json!({"series": "film A"}))),
                ("descriptor", descriptor("dc", "r1", "commands")),
                ("descriptor", descriptor("dx", "r1", "exposures")),
                (
                    "event",
                    json!({"descriptor": "dc", "seq_num": 1, "data": {}}),
                ),
                ("event", exposure("dx", 1)),
            ],
        );
        let Ok(Some(e)) = &out[4] else {
            panic!("{out:?}")
        };
        assert_eq!(e.series, "film A");
        assert_eq!(e.dir, PathBuf::from("/d/f"));
        assert_eq!(
            (e.shot.run_uid.as_str(), e.shot.seq, e.shot.mon),
            ("r1", 1, 5e4)
        );
        assert_eq!(out[..4], [Ok(None), Ok(None), Ok(None), Ok(None)]);
    }

    /// Without a `series`, or with an empty one, the run is its own series.
    #[test]
    fn a_run_without_a_series_is_its_own() {
        for md in [json!({}), json!({"series": ""}), json!({"series": 3})] {
            let mut runs = Runs::default();
            let out = feed(
                &mut runs,
                &[
                    ("start", start("r1", md)),
                    ("descriptor", descriptor("dx", "r1", "exposures")),
                    ("event", exposure("dx", 1)),
                ],
            );
            assert!(
                matches!(&out[2], Ok(Some(e)) if e.series == "r1"),
                "{out:?}"
            );
        }
    }

    /// After its stop, a run's descriptors are forgotten; a stream joined
    /// late is reported once per descriptor.
    #[test]
    fn unseen_streams_are_reported_once() {
        let mut runs = Runs::default();
        let out = feed(
            &mut runs,
            &[
                ("start", start("r1", json!({}))),
                ("descriptor", descriptor("dx", "r1", "exposures")),
                ("stop", json!({"run_start": "r1", "exit_status": "success"})),
                ("event", exposure("dx", 1)),
                ("event", exposure("dx", 2)),
                ("descriptor", descriptor("dy", "r2", "exposures")),
            ],
        );
        assert!(out[3].as_ref().unwrap_err().contains("not seen"));
        assert_eq!(out[4], Ok(None));
        assert!(out[5].as_ref().unwrap_err().contains("start was not seen"));
    }

    /// A run's exposures need its folder: the stacks go nowhere else.
    #[test]
    fn exposures_of_a_run_without_an_absolute_data_dir_are_an_error() {
        for dir in [json!(null), json!("rel/f"), json!(3)] {
            let mut runs = Runs::default();
            let mut s = start("r1", json!({}));
            s["data_dir"] = dir.clone();
            let out = feed(
                &mut runs,
                &[
                    ("start", s),
                    ("descriptor", descriptor("dx", "r1", "exposures")),
                    ("event", exposure("dx", 1)),
                ],
            );
            assert!(
                out[1].as_ref().unwrap_err().contains("data_dir"),
                "{dir}: {out:?}"
            );
            assert_eq!(out[2], Ok(None), "reported once: {dir}");
        }
    }

    #[test]
    fn an_exposure_without_its_monitor_is_an_error() {
        let mut runs = Runs::default();
        let mut ev = exposure("dx", 1);
        ev["data"].as_object_mut().unwrap().remove("icxrd");
        let out = feed(
            &mut runs,
            &[
                ("start", start("r1", json!({}))),
                ("descriptor", descriptor("dx", "r1", "exposures")),
                ("event", ev),
            ],
        );
        assert!(out[2].as_ref().unwrap_err().contains("icxrd"), "{out:?}");
    }

    #[test]
    fn a_frame_splits_on_its_first_two_spaces() {
        let doc = json!({"uid": "a b", "n": 1});
        let mut frame = b"bsrs start ".to_vec();
        frame.extend(rmp_serde::to_vec_named(&doc).unwrap());
        assert_eq!(parse(&frame), Ok(("start".into(), doc)));
        assert!(parse(b"start").is_err());
    }
}
