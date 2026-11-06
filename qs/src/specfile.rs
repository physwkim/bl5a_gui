//! The scans' SPEC data files (standard.mac `newfile_head`, `_head`, the
//! scans' rows, `_scanabort`'s comment). SPEC runs as a server here with
//! no `DATAFILE`, so these are the only files the scans' points reach: a
//! run whose start carries [`SPEC_HEAD`] is one `#S` of the file it names
//! once its "primary" stream begins, and each of its events one row.
//!
//! The labels are the mnemonics the documents carry (`th`, `det`), not
//! SPEC's `motor_name`/`cnt_name`. `#O`, `#P`, `#G` and `#Q` are left
//! out: the documents do not carry SPEC's motor table or geometry.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use async_trait::async_trait;
use bsrs::engine::RunEngine;
use bsrs::event_model::{DataKey, Document, EventDescriptor, ExitStatus, RunStart, RunStop};
use bsrs::qs::EnvironmentHook;
use chrono::{DateTime, Local, TimeZone};
use serde_json::Value;

use crate::scan::{g, g_digits};

/// The start md of a run that is a scan of a file: the file (`file`),
/// SPEC's `HEADING` (`heading`) and `_ctime` (`ctime`), and whether its
/// events are timed at their points, which gives the rows `Epoch`
/// (`epoch`).
pub const SPEC_HEAD: &str = "spec_head";
/// `_head`'s `_hkl_sym1`, the columns after the motors.
const HKL: [&str; 3] = ["H", "K", "L"];
const EPOCH: &str = "Epoch";

/// A scan before its first point: what its `#S` block needs.
struct Head {
    heading: String,
    ctime: f64,
    epoch: bool,
    motors: Vec<String>,
    det: Option<String>,
    time: f64,
}

impl Head {
    fn of(start: &RunStart) -> Option<Self> {
        let head = start.extra.get(SPEC_HEAD)?;
        let motors = start
            .extra
            .get("motors")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            heading: head.get("heading")?.as_str()?.to_string(),
            ctime: head.get("ctime")?.as_f64()?,
            epoch: head.get("epoch")?.as_bool()?,
            motors,
            det: start
                .extra
                .get("det")
                .and_then(Value::as_str)
                .map(String::from),
            time: start.time,
        })
    }

    /// `_head`'s `#L` order: the motors, H K L, Epoch, the other keys,
    /// DET last. SPEC orders the others by counter number, which the
    /// documents do not carry; here they are sorted.
    fn columns(&self, keys: &HashMap<String, DataKey>) -> Vec<String> {
        let det = self.det.as_deref().filter(|d| keys.contains_key(*d));
        let mut cols: Vec<String> = self
            .motors
            .iter()
            .map(String::as_str)
            .chain(HKL)
            .filter(|k| keys.contains_key(*k) && Some(*k) != det)
            .map(String::from)
            .collect();
        let mut rest: Vec<&String> = keys
            .keys()
            .filter(|k| !cols.contains(k) && Some(k.as_str()) != det)
            .collect();
        rest.sort();
        if self.epoch {
            cols.push(EPOCH.to_string());
        }
        cols.extend(rest.into_iter().cloned());
        cols.extend(det.map(String::from));
        cols
    }
}

/// A scan whose `#S` block is written.
struct Block {
    cols: Vec<String>,
    points: u64,
}

enum Scan {
    Head(Head),
    Block(Block),
}

/// The file's writer: the documents in, the file's lines out.
pub struct SpecFile<W> {
    out: W,
    /// `#E`: the rows' Epoch counts from it.
    epoch: i64,
    /// The last `#S` number.
    last: u64,
    /// The open scans, by run.
    scans: HashMap<String, Scan>,
    /// A primary descriptor's run.
    primary: HashMap<String, String>,
}

impl SpecFile<fs::File> {
    /// `path` for appending, with `newfile_head`'s lines: `#F` first on a
    /// new or empty file; on one with scans the numbers continue from
    /// its last `#S`.
    pub fn open(path: &Path, now: f64, user: &str) -> io::Result<Self> {
        let text = match fs::read(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        let mut out = OpenOptions::new().create(true).append(true).open(path)?;
        let epoch = now.floor() as i64;
        let mut head = if text.is_empty() {
            format!("#F {}\n", path.display())
        } else {
            "\n".to_string()
        };
        head += &format!("#E {epoch}\n#D {}\n#C bl5a-qs  User = {user}\n", date(now));
        out.write_all(head.as_bytes())?;
        Ok(SpecFile::new(out, epoch, last_scan(&text)))
    }
}

impl<W: Write> SpecFile<W> {
    fn new(out: W, epoch: i64, last: u64) -> Self {
        Self {
            out,
            epoch,
            last,
            scans: HashMap::new(),
            primary: HashMap::new(),
        }
    }

    pub fn feed(&mut self, doc: &Document) -> io::Result<()> {
        match doc {
            Document::Start(s) => {
                if let Some(head) = Head::of(s) {
                    self.scans.insert(s.uid.clone(), Scan::Head(head));
                }
                Ok(())
            }
            Document::Descriptor(d) if d.name.as_deref() == Some("primary") => self.descriptor(d),
            Document::Event(e) => self.row(&e.descriptor, e.time, |k| e.data.get(k)),
            Document::EventPage(p) => {
                for (i, &t) in p.time.iter().enumerate() {
                    self.row(&p.descriptor, t, |k| p.data.get(k).and_then(|c| c.get(i)))?;
                }
                Ok(())
            }
            Document::Stop(s) => self.stop(s),
            _ => Ok(()),
        }
    }

    /// The scan's `#S` block, at its first primary descriptor.
    fn descriptor(&mut self, d: &EventDescriptor) -> io::Result<()> {
        let Some(scan) = self.scans.get_mut(&d.run_start) else {
            return Ok(());
        };
        self.primary.insert(d.uid.clone(), d.run_start.clone());
        let Scan::Head(head) = scan else {
            return Ok(());
        };
        let cols = head.columns(&d.data_keys);
        self.last += 1;
        let block = format!(
            "\n#S {}  {}\n#D {}\n#T {}  (sec)\n#N {}\n#L {}\n",
            self.last,
            head.heading,
            date(head.time),
            g(head.ctime),
            cols.len(),
            cols.join("  ")
        );
        *scan = Scan::Block(Block { cols, points: 0 });
        self.out.write_all(block.as_bytes())
    }

    fn row<'a>(
        &mut self,
        descriptor: &str,
        time: f64,
        value: impl Fn(&str) -> Option<&'a Value>,
    ) -> io::Result<()> {
        let Some(Scan::Block(block)) = self
            .primary
            .get(descriptor)
            .and_then(|run| self.scans.get_mut(run))
        else {
            return Ok(());
        };
        let line: Vec<String> = block
            .cols
            .iter()
            .map(|c| {
                if c == EPOCH {
                    format!("{:.3}", time - self.epoch as f64)
                } else {
                    let v = value(c).and_then(Value::as_f64).unwrap_or(f64::NAN);
                    g_digits(v, 8)
                }
            })
            .collect();
        block.points += 1;
        writeln!(self.out, "{}", line.join(" "))
    }

    /// `_scanabort`'s comment on a scan that did not complete.
    fn stop(&mut self, s: &RunStop) -> io::Result<()> {
        self.primary.retain(|_, run| *run != s.run_start);
        match self.scans.remove(&s.run_start) {
            Some(Scan::Block(b)) if s.exit_status != ExitStatus::Success => writeln!(
                self.out,
                "#C {}.  Scan aborted after {} points.",
                date(s.time),
                b.points
            ),
            _ => Ok(()),
        }
    }
}

/// The largest `#S` number of a SPEC file's text.
fn last_scan(text: &[u8]) -> u64 {
    text.split(|&b| b == b'\n')
        .filter_map(|line| line.strip_prefix(b"#S "))
        .filter_map(|rest| {
            let digits: Vec<u8> = rest
                .iter()
                .skip_while(|b| **b == b' ')
                .take_while(|b| b.is_ascii_digit())
                .copied()
                .collect();
            String::from_utf8(digits).ok()?.parse().ok()
        })
        .max()
        .unwrap_or(0)
}

/// SPEC's `date()`.
fn date(t: f64) -> String {
    ctime(&Local, t)
}

/// C `ctime` without its newline.
fn ctime<Tz: TimeZone>(tz: &Tz, t: f64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    DateTime::from_timestamp(t.floor() as i64, 0).map_or_else(String::new, |d| {
        d.with_timezone(tz)
            .format("%a %b %e %H:%M:%S %Y")
            .to_string()
    })
}

/// The SPEC files, each opened at its first scan of the session: a
/// scan's start goes to the file it names, every other document to each
/// open file, which takes those of its own scans.
struct Files {
    user: String,
    open: HashMap<PathBuf, SpecFile<fs::File>>,
}

impl Files {
    fn feed(&mut self, doc: &Document) -> Result<(), String> {
        if let Document::Start(s) = doc {
            let Some(path) = file_of(s) else {
                return Ok(());
            };
            let file = match self.open.entry(path) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => {
                    let file = SpecFile::open(v.key(), s.time, &self.user)
                        .map_err(|e| format!("{}: {e}", v.key().display()))?;
                    v.insert(file)
                }
            };
            return file.feed(doc).map_err(|e| e.to_string());
        }
        let errors: Vec<String> = self
            .open
            .iter_mut()
            .filter_map(|(path, file)| {
                let e = file.feed(doc).err()?;
                Some(format!("{}: {e}", path.display()))
            })
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

/// The file a scan's start names.
fn file_of(s: &RunStart) -> Option<PathBuf> {
    let file = s.extra.get(SPEC_HEAD)?.get("file")?.as_str()?;
    Some(PathBuf::from(file))
}

/// Puts every environment's documents through the [`SpecFile`]s, on a
/// thread of its own: the engine's document callbacks must be quick.
pub struct Hook(mpsc::Sender<Document>);

impl Hook {
    pub fn start() -> io::Result<Self> {
        let mut files = Files {
            user: std::env::var("USER").unwrap_or_default(),
            open: HashMap::new(),
        };
        let (tx, rx) = mpsc::channel::<Document>();
        std::thread::Builder::new()
            .name("spec-file".into())
            .spawn(move || {
                for doc in rx {
                    if let Err(e) = files.feed(&doc) {
                        tracing::error!(target: "bl5a-qs", "SPEC file: {e}");
                    }
                }
            })?;
        Ok(Self(tx))
    }
}

#[async_trait]
impl EnvironmentHook for Hook {
    async fn environment_opened(&self, re: &Arc<RunEngine>) -> bsrs::core::Result<()> {
        let tx = self.0.clone();
        re.subscribe(Arc::new(move |doc: &Document| {
            if matches!(
                doc,
                Document::Start(_)
                    | Document::Descriptor(_)
                    | Document::Event(_)
                    | Document::EventPage(_)
                    | Document::Stop(_)
            ) {
                // The thread outlives every engine; a send fails only
                // after it panicked.
                let _ = tx.send(doc.clone());
            }
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn doc(name: &str, d: Value) -> Document {
        serde_json::from_value(json!({"name": name, "doc": d})).expect(name)
    }

    fn start(uid: &str, t: f64, head: Option<Value>) -> Document {
        let mut d = json!({"uid": uid, "time": t, "motors": ["th"], "det": "det"});
        if let Some(h) = head {
            d[SPEC_HEAD] = h;
        }
        doc("start", d)
    }

    fn descriptor(uid: &str, run: &str, name: &str, keys: &[&str]) -> Document {
        let keys: serde_json::Map<String, Value> = keys
            .iter()
            .map(|k| {
                (
                    k.to_string(),
                    json!({"source": k, "dtype": "number", "shape": []}),
                )
            })
            .collect();
        doc(
            "descriptor",
            json!({"uid": uid, "run_start": run, "time": 0.0, "name": name, "data_keys": keys}),
        )
    }

    fn event(descriptor: &str, n: u64, t: f64, data: Value) -> Document {
        doc(
            "event",
            json!({"uid": format!("{descriptor}{n}"), "descriptor": descriptor, "time": t,
                   "seq_num": n, "data": data, "timestamps": {}}),
        )
    }

    fn stop(run: &str, t: f64, status: &str) -> Document {
        doc(
            "stop",
            json!({"uid": format!("{run}-stop"), "run_start": run, "time": t, "exit_status": status}),
        )
    }

    fn text(docs: &[Document], last: u64) -> String {
        let mut f = SpecFile::new(Vec::new(), 1000, last);
        for d in docs {
            f.feed(d).unwrap();
        }
        String::from_utf8(f.out).unwrap()
    }

    #[test]
    fn a_step_scan_is_an_s_block_and_a_row_per_point() {
        let head = json!({"heading": "ascan  th -1 1  1 0.3", "ctime": 0.3, "epoch": true});
        let got = text(
            &[
                start("plan", 1001.0, None),
                start("scan", 1002.0, Some(head)),
                descriptor(
                    "p",
                    "scan",
                    "primary",
                    &["th", "sec", "icxrd", "det", "ic1"],
                ),
                event(
                    "p",
                    1,
                    1003.25,
                    json!({"th": -1.0, "sec": 0.3, "icxrd": 12.0, "det": 345.0, "ic1": 1e6}),
                ),
                event(
                    "p",
                    2,
                    1004.5,
                    json!({"th": 1.0, "sec": 0.3, "icxrd": 13.0, "det": -2.8743123456, "ic1": 123456789.0}),
                ),
                stop("scan", 1005.0, "success"),
                stop("plan", 1006.0, "success"),
            ],
            6,
        );
        let want = format!(
            "\n#S 7  ascan  th -1 1  1 0.3\n#D {}\n#T 0.3  (sec)\n#N 6\n#L th  Epoch  ic1  icxrd  sec  det\n\
             -1 3.250 1000000 12 0.3 345\n\
             1 4.500 1.2345679e+08 13 0.3 -2.8743123\n",
            date(1002.0)
        );
        assert_eq!(got, want);
    }

    #[test]
    fn an_aborted_fly_scan_says_how_many_points_it_took() {
        let head = json!({"heading": "cscan  th -1 1  5", "ctime": 5.0, "epoch": false});
        let got = text(
            &[
                start("scan", 1002.0, Some(head)),
                descriptor("p", "scan", "primary", &["L", "det", "K", "th", "H", "att"]),
                event(
                    "p",
                    1,
                    1009.0,
                    json!({"th": 0.5, "H": 0.0, "K": 0.0, "L": 1.5, "att": 25.0, "det": 7.0}),
                ),
                stop("scan", 1010.0, "abort"),
            ],
            0,
        );
        let want = format!(
            "\n#S 1  cscan  th -1 1  5\n#D {}\n#T 5  (sec)\n#N 6\n#L th  H  K  L  att  det\n\
             0.5 0 0 1.5 25 7\n#C {}.  Scan aborted after 1 points.\n",
            date(1002.0),
            date(1010.0)
        );
        assert_eq!(got, want);
    }

    #[test]
    fn only_a_scan_with_points_takes_a_number() {
        let head = json!({"heading": "ascan  th -1 1  1 0.3", "ctime": 0.3, "epoch": true});
        let got = text(
            &[
                // A plan run: its streams are not scans.
                start("plan", 1001.0, None),
                descriptor("s", "plan", "stages", &["th"]),
                event("s", 1, 1002.0, json!({"th": 1.0})),
                // A scan that failed before its first point.
                start("scan", 1003.0, Some(head.clone())),
                stop("scan", 1004.0, "fail"),
                start("scan2", 1005.0, Some(head)),
                descriptor("p", "scan2", "primary", &["th", "det"]),
                stop("scan2", 1006.0, "fail"),
                stop("plan", 1007.0, "fail"),
            ],
            0,
        );
        assert!(got.starts_with("\n#S 1  ascan"), "{got}");
        assert!(got.ends_with("Scan aborted after 0 points.\n"), "{got}");
        assert_eq!(got.matches("#S").count(), 1);
    }

    #[test]
    fn a_new_file_starts_with_f_and_an_old_one_continues_its_numbers() {
        let dir = std::env::temp_dir().join(format!("bl5a-qs-specfile-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scans.spec");
        let _ = fs::remove_file(&path);

        let f = SpecFile::open(&path, 1000.5, "bl5a").unwrap();
        assert_eq!(f.last, 0);
        drop(f);
        let new = fs::read_to_string(&path).unwrap();
        assert_eq!(
            new,
            format!(
                "#F {}\n#E 1000\n#D {}\n#C bl5a-qs  User = bl5a\n",
                path.display(),
                date(1000.5)
            )
        );

        fs::write(&path, "#F x\n\n#S 7  ascan\n1 2\n\n#S 12  cscan\n#S x\n").unwrap();
        let mut f = SpecFile::open(&path, 2000.0, "bl5a").unwrap();
        assert_eq!(f.last, 12);
        let head = json!({"heading": "ascan  th 0 1  1 1", "ctime": 1.0, "epoch": true});
        f.feed(&start("scan", 2001.0, Some(head))).unwrap();
        f.feed(&descriptor("p", "scan", "primary", &["th", "det"]))
            .unwrap();
        drop(f);
        let old = fs::read_to_string(&path).unwrap();
        let want = format!(
            "#S x\n\n#E 2000\n#D {}\n#C bl5a-qs  User = bl5a\n\n\
             #S 13  ascan  th 0 1  1 1\n#D {}\n#T 1  (sec)\n#N 3\n#L th  Epoch  det\n",
            date(2000.0),
            date(2001.0)
        );
        assert!(old.ends_with(&want), "{old}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_scan_goes_to_the_file_its_start_names() {
        let dir = std::env::temp_dir().join(format!("bl5a-qs-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("a"), dir.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let (fa, fb) = (a.join("a.spec"), b.join("b.spec"));
        let head = |file: &Path| json!({"file": file, "heading": "ascan  th 0 1  1 1", "ctime": 1.0, "epoch": false});
        let mut files = Files {
            user: "u".into(),
            open: HashMap::new(),
        };
        let scan = |files: &mut Files, run: &str, t: f64, file: &Path| {
            let d = format!("{run}-p");
            for doc in [
                start(run, t, Some(head(file))),
                descriptor(&d, run, "primary", &["th", "det"]),
                event(&d, 1, t, json!({"th": t, "det": 1.0})),
                stop(run, t, "success"),
            ] {
                files.feed(&doc).unwrap();
            }
        };
        scan(&mut files, "s1", 1.0, &fa);
        scan(&mut files, "s2", 2.0, &fb);
        scan(&mut files, "s3", 3.0, &fa);
        let (ta, tb) = (
            fs::read_to_string(&fa).unwrap(),
            fs::read_to_string(&fb).unwrap(),
        );
        assert_eq!(ta.matches("#F ").count(), 1, "{ta}");
        assert_eq!(ta.matches("#E ").count(), 1, "one header a session: {ta}");
        assert!(
            ta.contains("#S 1  ascan") && ta.contains("#S 2  ascan"),
            "{ta}"
        );
        assert!(ta.contains("\n1 1\n") && ta.contains("\n3 1\n"), "{ta}");
        assert!(!ta.contains("\n2 1\n"), "s2's row is b's: {ta}");
        assert!(tb.contains("#S 1  ascan") && tb.contains("\n2 1\n"), "{tb}");
        assert!(!tb.contains("#S 2"), "{tb}");

        // A file that cannot open: the start fails, its rows go nowhere.
        let lost = dir.join("missing").join("m.spec");
        assert!(files.feed(&start("s4", 4.0, Some(head(&lost)))).is_err());
        files
            .feed(&descriptor("s4-p", "s4", "primary", &["th"]))
            .unwrap();
        files
            .feed(&event("s4-p", 1, 4.0, json!({"th": 4.0})))
            .unwrap();
        assert_eq!(fs::read_to_string(&fa).unwrap(), ta);
        // A start that names no file is no scan.
        files.feed(&start("plan", 5.0, None)).unwrap();
        assert_eq!(files.open.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn date_is_ctime() {
        assert_eq!(ctime(&Utc, 0.0), "Thu Jan  1 00:00:00 1970");
        // `date -u -d @1793920441`
        assert_eq!(ctime(&Utc, 1_793_920_441.9), "Thu Nov  5 23:14:01 2026");
    }
}
