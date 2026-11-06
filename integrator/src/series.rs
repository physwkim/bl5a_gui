//! The series stacks: HDF5 files in each item's data folder, each holding
//! its series' name, its q axis, a row of I and σ per frame, and the frame
//! table. Only this process writes them, and only files it loaded or
//! wrote itself: an append writes the whole stack beside the file and
//! renames it over, so a reader of the folder sees the stack before the
//! row or after it.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use rust_hdf5::H5File;
use serde::{Deserialize, Serialize};

use crate::engine::{Profile, UNIT};

/// What an "exposures" event says about its frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shot {
    /// The event's time, s since the epoch.
    pub time: f64,
    pub run_uid: String,
    pub seq: i64,
    pub file: String,
    pub file_number: i64,
    pub th: f64,
    pub shold: f64,
    /// icxrd, the row's divisor.
    pub mon: f64,
    pub texpo: f64,
    pub sec: f64,
}

/// A row's frame: the shot and the PONI it was integrated with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    #[serde(flatten)]
    pub shot: Shot,
    pub poni: String,
}

/// One stack: its series, the q axis it bins on and a row per frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stack {
    pub series: String,
    pub q: Vec<f64>,
    pub frames: Vec<Frame>,
    pub intensity: Vec<Vec<f32>>,
    pub sigma: Vec<Vec<f32>>,
}

/// The stacks this process loaded or wrote, by path. A series' stacks
/// sit in its item's data folder as `<name>.h5`, `<name>~2.h5`, ...: its
/// rows go to the newest while that one bins on the current q axis, and
/// otherwise start the next, so a changed axis never rebins a stack.
#[derive(Default)]
pub struct Store {
    stacks: BTreeMap<PathBuf, Stack>,
    /// The folders whose stacks were loaded.
    seen: BTreeSet<PathBuf>,
}

type Res<T> = Result<T, Box<dyn Error>>;

/// The longest file name the stacks get, bytes.
const NAME_MAX: usize = 250;

/// `series` as a file name stem: bytes other than ASCII letters, digits,
/// `_`, `-` and a `.` that does not lead become `%XX`, so distinct series
/// get distinct stems, and none holds the `~` before a stack's number.
pub fn stem(series: &str) -> String {
    let mut s = String::new();
    for b in series.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || (b == b'.' && !s.is_empty()) {
            s.push(char::from(b));
        } else {
            let _ = write!(s, "%{b:02X}");
        }
    }
    s
}

/// The `n`th stack of a series whose stem is `stem`.
fn stack_name(stem: &str, n: u32) -> String {
    if n == 1 {
        format!("{stem}.h5")
    } else {
        format!("{stem}~{n}.h5")
    }
}

/// The number of a stack file named for `stem`.
fn stack_number(file: &str, stem: &str) -> Option<u32> {
    let rest = file.strip_prefix(stem)?.strip_suffix(".h5")?;
    if rest.is_empty() {
        return Some(1);
    }
    let n = rest.strip_prefix('~')?;
    if n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok().filter(|&n| n >= 2)
}

impl Store {
    /// Loads the stacks in `dir` not loaded yet; each file that is not
    /// loaded is named with the reason, and appends never overwrite it.
    pub fn load_dir(&mut self, dir: &Path) -> Result<Vec<String>, String> {
        let shown = |e: &dyn std::fmt::Display| format!("{}: {e}", dir.display());
        let mut refused = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| shown(&e))? {
            let path = entry.map_err(|e| shown(&e))?.path();
            if path.extension().is_none_or(|x| x != "h5") || self.stacks.contains_key(&path) {
                continue;
            }
            match load(&path) {
                Ok(s) if !named_for(&path, &s.series) => {
                    refused.push(format!("{}: holds series {:?}", path.display(), s.series));
                }
                Ok(s) => {
                    self.stacks.insert(path, s);
                }
                Err(e) => refused.push(format!("{}: {e}", path.display())),
            }
        }
        self.seen.insert(dir.to_path_buf());
        Ok(refused)
    }

    /// Each stack: its path, series and number of rows.
    pub fn stacks(&self) -> Vec<(&Path, &str, usize)> {
        self.stacks
            .iter()
            .map(|(p, s)| (p.as_path(), s.series.as_str(), s.frames.len()))
            .collect()
    }

    pub fn get(&self, path: &Path) -> Option<&Stack> {
        self.stacks.get(path)
    }

    /// Appends `frame`'s row, binned on `q`, to `series`' stack in `dir`
    /// and writes the stack: its path and the row's index, or `None` when
    /// the stack has that frame (run uid, seq) already. The first row of
    /// a folder loads its stacks; the `refused` ones are named.
    pub fn append(
        &mut self,
        dir: &Path,
        series: &str,
        q: &[f64],
        frame: Frame,
        p: &Profile,
        refused: &mut Vec<String>,
    ) -> Result<Option<(PathBuf, usize)>, String> {
        let npt = q.len();
        if p.intensity.len() != npt || p.sigma.len() != npt {
            return Err(format!(
                "a row of {} bins for a {npt}-bin axis",
                p.intensity.len()
            ));
        }
        if !self.seen.contains(dir) {
            refused.extend(self.load_dir(dir)?);
        }
        let stem = stem(series);
        let newest = newest(dir, &stem)?;
        let current = newest.map(|n| dir.join(stack_name(&stem, n)));
        let (path, mut s) = match current
            .as_ref()
            .and_then(|c| Some((c, self.stacks.get(c)?)))
        {
            Some((path, s)) if s.series == series && s.q == q => {
                let key = (&frame.shot.run_uid, frame.shot.seq);
                if s.frames
                    .iter()
                    .any(|f| (&f.shot.run_uid, f.shot.seq) == key)
                {
                    return Ok(None);
                }
                (path.clone(), s.clone())
            }
            // On another axis, or a file in the way: the next stack.
            _ => {
                let name = stack_name(&stem, newest.map_or(1, |n| n + 1));
                if name.len() > NAME_MAX {
                    return Err(format!(
                        "series {series:?} makes a file name over {NAME_MAX} bytes"
                    ));
                }
                let stack = Stack {
                    series: series.to_string(),
                    q: q.to_vec(),
                    ..Stack::default()
                };
                (dir.join(name), stack)
            }
        };
        s.frames.push(frame);
        s.intensity.push(p.intensity.clone());
        s.sigma.push(p.sigma.clone());
        save(&path, &s).map_err(|e| format!("{}: {e}", path.display()))?;
        let index = s.frames.len() - 1;
        self.stacks.insert(path.clone(), s);
        Ok(Some((path, index)))
    }
}

/// The largest number of a stack file named for `stem` in `dir`.
fn newest(dir: &Path, stem: &str) -> Result<Option<u32>, String> {
    let shown = |e: &dyn std::fmt::Display| format!("{}: {e}", dir.display());
    let mut max = None;
    for entry in fs::read_dir(dir).map_err(|e| shown(&e))? {
        let name = entry.map_err(|e| shown(&e))?.file_name();
        if let Some(n) = name.to_str().and_then(|f| stack_number(f, stem)) {
            max = max.max(Some(n));
        }
    }
    Ok(max)
}

/// Whether `path` is a stack file name of `series`.
fn named_for(path: &Path, series: &str) -> bool {
    path.file_name()
        .and_then(|f| f.to_str())
        .and_then(|f| stack_number(f, &stem(series)))
        .is_some()
}

fn save(path: &Path, s: &Stack) -> Res<()> {
    let tmp = path.with_extension("h5.tmp");
    let (n, npt) = (s.frames.len(), s.q.len());
    let f = H5File::create(&tmp)?;
    f.set_attr_string("series", &s.series)?;
    f.set_attr_string("unit", UNIT)?;
    f.new_dataset::<f64>()
        .shape([npt])
        .create("q")?
        .write_raw(&s.q)?;
    f.new_dataset::<f32>()
        .shape([n, npt])
        .create("I")?
        .write_raw(&s.intensity.concat())?;
    f.new_dataset::<f32>()
        .shape([n, npt])
        .create("sigma")?
        .write_raw(&s.sigma.concat())?;
    let g = f.create_group("frames")?;
    let shots = || s.frames.iter().map(|f| &f.shot);
    let real = |get: fn(&Shot) -> f64| -> Vec<f64> { shots().map(get).collect() };
    let reals = [
        ("time", real(|s| s.time)),
        ("th", real(|s| s.th)),
        ("shold", real(|s| s.shold)),
        ("mon", real(|s| s.mon)),
        ("texpo", real(|s| s.texpo)),
        ("sec", real(|s| s.sec)),
        ("file_number", real(|s| s.file_number as f64)),
    ];
    for (k, v) in reals {
        g.new_dataset::<f64>().shape([n]).create(k)?.write_raw(&v)?;
    }
    let seq: Vec<i64> = shots().map(|s| s.seq).collect();
    g.new_dataset::<i64>()
        .shape([n])
        .create("seq")?
        .write_raw(&seq)?;
    let text = |get: fn(&Frame) -> &str| -> Vec<&str> { s.frames.iter().map(get).collect() };
    let texts = [
        ("file", text(|f| &f.shot.file)),
        ("run_uid", text(|f| &f.shot.run_uid)),
        ("poni", text(|f| &f.poni)),
    ];
    for (k, v) in texts {
        g.write_vlen_strings(k, &v)?;
    }
    f.close()?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

fn load(path: &Path) -> Res<Stack> {
    let f = H5File::open(path)?;
    let series = f.attr_string("series")?;
    let unit = f.attr_string("unit")?;
    if unit != UNIT {
        return Err(format!("binned in {unit}, not {UNIT}").into());
    }
    let q = f.dataset("q")?.read_raw::<f64>()?;
    let npt = q.len();
    if npt == 0 {
        return Err("an empty q axis".into());
    }
    let real = |k: &str| f.dataset(&format!("frames/{k}"))?.read_raw::<f64>();
    let text = |k: &str| f.dataset(&format!("frames/{k}"))?.read_vlen_strings();
    let (time, th, shold, mon) = (real("time")?, real("th")?, real("shold")?, real("mon")?);
    let (texpo, sec, file_number) = (real("texpo")?, real("sec")?, real("file_number")?);
    let seq = f.dataset("frames/seq")?.read_raw::<i64>()?;
    let (file, run_uid, poni) = (text("file")?, text("run_uid")?, text("poni")?);
    let intensity = f.dataset("I")?.read_raw::<f32>()?;
    let sigma = f.dataset("sigma")?.read_raw::<f32>()?;
    let n = time.len();
    let lengths = [
        th.len(),
        shold.len(),
        mon.len(),
        texpo.len(),
        sec.len(),
        file_number.len(),
        seq.len(),
        file.len(),
        run_uid.len(),
        poni.len(),
    ];
    if lengths.iter().any(|&l| l != n) || intensity.len() != n * npt || sigma.len() != n * npt {
        return Err(format!("the frame table and the rows disagree on {n} frames").into());
    }
    let frames = (0..n)
        .map(|r| Frame {
            shot: Shot {
                time: time[r],
                run_uid: run_uid[r].clone(),
                seq: seq[r],
                file: file[r].clone(),
                file_number: file_number[r] as i64,
                th: th[r],
                shold: shold[r],
                mon: mon[r],
                texpo: texpo[r],
                sec: sec[r],
            },
            poni: poni[r].clone(),
        })
        .collect();
    let rows = |v: Vec<f32>| v.chunks(npt).map(<[f32]>::to_vec).collect();
    Ok(Stack {
        series,
        q,
        frames,
        intensity: rows(intensity),
        sigma: rows(sigma),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bl5a-integrator-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn frame(run: &str, seq: i64) -> Frame {
        Frame {
            shot: Shot {
                time: 1.7e9 + seq as f64,
                run_uid: run.into(),
                seq,
                file: format!("/data/t_{seq:04}.mar3450"),
                file_number: seq,
                th: 0.12,
                shold: -0.2,
                mon: 1e5,
                texpo: 2.0,
                sec: 2.0,
            },
            poni: "Distance: 0.3\n".into(),
        }
    }

    fn profile(v: f32) -> Profile {
        Profile {
            intensity: vec![v; 3],
            sigma: vec![v / 10.0; 3],
        }
    }

    const Q: [f64; 3] = [0.1, 0.2, 0.3];
    const Q2: [f64; 3] = [0.1, 0.2, 0.4];

    fn append(
        st: &mut Store,
        d: &Path,
        series: &str,
        q: &[f64],
        f: Frame,
    ) -> Option<(String, usize)> {
        let mut refused = Vec::new();
        let r = st
            .append(d, series, q, f, &profile(1.0), &mut refused)
            .unwrap();
        assert!(refused.is_empty(), "{refused:?}");
        r.map(|(p, i)| (p.file_name().unwrap().to_str().unwrap().to_string(), i))
    }

    #[test]
    fn stems_are_distinct_and_plain() {
        assert_eq!(stem("t2_run-1.a"), "t2_run-1.a");
        assert_eq!(stem(".."), "%2E.");
        assert_eq!(stem("a/b"), "a%2Fb");
        assert_eq!(stem("a~2"), "a%7E2");
        let names = ["a%2Fb", "a/b", "a b", "a_b", "", "%", "é", "a~2"];
        let stems: std::collections::HashSet<_> = names.iter().map(|n| stem(n)).collect();
        assert_eq!(stems.len(), names.len());
    }

    #[test]
    fn stack_numbers() {
        assert_eq!(stack_name("s", 1), "s.h5");
        assert_eq!(stack_name("s", 12), "s~12.h5");
        assert_eq!(stack_number("s.h5", "s"), Some(1));
        assert_eq!(stack_number("s~12.h5", "s"), Some(12));
        for other in [
            "s~1.h5", "s~02.h5", "s~.h5", "s~x.h5", "st.h5", "s.h5.tmp", "t.h5",
        ] {
            assert_eq!(stack_number(other, "s"), None, "{other}");
        }
    }

    #[test]
    fn a_stack_reloads_as_written() {
        let d = dir("reload");
        let mut st = Store::default();
        assert_eq!(
            append(&mut st, &d, "film 1", &Q, frame("r1", 1)),
            Some(("film%201.h5".into(), 0))
        );
        assert_eq!(
            append(&mut st, &d, "film 1", &Q, frame("r1", 2)),
            Some(("film%201.h5".into(), 1))
        );
        assert_eq!(append(&mut st, &d, "film 1", &Q, frame("r1", 1)), None);
        assert_eq!(
            append(&mut st, &d, "other", &Q, frame("r2", 1)),
            Some(("other.h5".into(), 0))
        );
        let mut back = Store::default();
        assert!(back.load_dir(&d).unwrap().is_empty());
        let rows: Vec<_> = back
            .stacks()
            .into_iter()
            .map(|(_, s, n)| (s.to_string(), n))
            .collect();
        assert_eq!(rows, vec![("film 1".into(), 2), ("other".into(), 1)]);
        let path = d.join("film%201.h5");
        assert_eq!(back.get(&path), st.get(&path));
        assert!(!d.join("film%201.h5.tmp").exists());
        // After a restart on the same axis the rows go on in the stack.
        assert_eq!(
            append(&mut back, &d, "film 1", &Q, frame("r3", 1)),
            Some(("film%201.h5".into(), 2))
        );
        fs::remove_dir_all(&d).unwrap();
    }

    /// A changed axis starts the series' next stack and leaves the old
    /// one as it was; changing back starts another.
    #[test]
    fn another_axis_starts_the_next_stack() {
        let d = dir("axis");
        let mut st = Store::default();
        append(&mut st, &d, "s", &Q, frame("r1", 1));
        let before = fs::read(d.join("s.h5")).unwrap();
        assert_eq!(
            append(&mut st, &d, "s", &Q2, frame("r1", 2)),
            Some(("s~2.h5".into(), 0))
        );
        assert_eq!(
            append(&mut st, &d, "s", &Q2, frame("r1", 3)),
            Some(("s~2.h5".into(), 1))
        );
        assert_eq!(
            append(&mut st, &d, "s", &Q, frame("r1", 4)),
            Some(("s~3.h5".into(), 0))
        );
        assert_eq!(fs::read(d.join("s.h5")).unwrap(), before);
        assert_eq!(st.get(&d.join("s~2.h5")).unwrap().q, Q2);
        // A fresh process loads each on its own axis.
        let mut back = Store::default();
        back.load_dir(&d).unwrap();
        assert_eq!(back.stacks().len(), 3);
        assert_eq!(back.get(&d.join("s.h5")).unwrap().q, Q);
        fs::remove_dir_all(&d).unwrap();
    }

    /// A file that is not a stack of the series is never overwritten.
    #[test]
    fn a_file_in_the_way_is_left_alone() {
        let d = dir("way");
        fs::write(d.join("s.h5"), b"not hdf5").unwrap();
        let mut st = Store::default();
        let mut refused = Vec::new();
        let got = st
            .append(&d, "s", &Q, frame("r1", 1), &profile(1.0), &mut refused)
            .unwrap();
        assert_eq!(got.map(|(p, _)| p), Some(d.join("s~2.h5")));
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(fs::read(d.join("s.h5")).unwrap(), b"not hdf5");
        // A stack of another series under this one's name.
        let e = dir("way2");
        append(&mut st, &e, "x", &Q, frame("r1", 1));
        fs::rename(e.join("x.h5"), d.join("t.h5")).unwrap();
        let mut back = Store::default();
        let refused = back.load_dir(&d).unwrap();
        assert!(
            refused.iter().any(|r| r.contains("holds series \"x\"")),
            "{refused:?}"
        );
        assert!(back.get(&d.join("t.h5")).is_none());
        fs::remove_dir_all(&d).unwrap();
        fs::remove_dir_all(&e).unwrap();
    }

    #[test]
    fn stacks_of_one_series_in_two_folders_are_two() {
        let (a, b) = (dir("fa"), dir("fb"));
        let mut st = Store::default();
        assert_eq!(
            append(&mut st, &a, "s", &Q, frame("r1", 1)),
            Some(("s.h5".into(), 0))
        );
        assert_eq!(
            append(&mut st, &b, "s", &Q, frame("r2", 1)),
            Some(("s.h5".into(), 0))
        );
        assert_eq!(st.stacks().len(), 2);
        fs::remove_dir_all(&a).unwrap();
        fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn a_row_of_the_wrong_length_or_a_missing_folder_is_refused() {
        let d = dir("length");
        let mut st = Store::default();
        let mut refused = Vec::new();
        assert!(
            st.append(
                &d,
                "s",
                &[0.1, 0.2],
                frame("r1", 1),
                &profile(1.0),
                &mut refused
            )
            .is_err()
        );
        assert!(st.stacks().is_empty());
        let gone = d.join("gone");
        assert!(
            st.append(&gone, "s", &Q, frame("r1", 1), &profile(1.0), &mut refused)
                .is_err()
        );
        fs::remove_dir_all(&d).unwrap();
    }
}
