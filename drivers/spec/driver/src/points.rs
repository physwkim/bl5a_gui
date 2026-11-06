//! A `ScanCmd`'s points as SPEC delivers them: one `SCAN_PT` event per
//! point, `[pno, [row...], g3]`, under the columns `SCAN_COLS` names
//! (`scan_data_init`, `scan_info_update` in SPEC's standard.mac), checked
//! as the command ends against `NPTS` and `SCAN_COLS`.

use std::collections::BTreeMap;
use std::sync::Arc;

use spec_protocol::{DataType, FLAG_DELETED, Packet};

/// `Scan:Data`'s NELM: the most values one scan carries.
pub const SCAN_NELM: usize = 65536;

/// `NPTS`: the points the scan took.
pub(crate) fn rows(p: &Packet) -> Result<usize, String> {
    match p.number() {
        Some(n) if n >= 0.0 && n.fract() == 0.0 => Ok(n as usize),
        _ => Err(format!("NPTS is {:?}, not a count", p.text())),
    }
}

/// `SCAN_COLS` read whole: its indices are the columns of a point.
pub(crate) fn cols(p: &Packet) -> Result<Vec<String>, String> {
    if p.header.data_type != Some(DataType::Assoc) {
        return Err("SCAN_COLS is not an associative array".into());
    }
    ordered(p.assoc())
}

/// The names at indices 0, 1, ... in turn.
fn ordered(named: impl IntoIterator<Item = (String, String)>) -> Result<Vec<String>, String> {
    let mut by_index: Vec<(usize, String)> = Vec::new();
    for (index, name) in named {
        let i = index
            .parse()
            .map_err(|_| format!("SCAN_COLS has the index {index:?}"))?;
        by_index.push((i, name));
    }
    by_index.sort_by_key(|(i, _)| *i);
    if by_index.iter().enumerate().any(|(n, (i, _))| n != *i) {
        return Err("SCAN_COLS does not name columns 0, 1, ... in turn".into());
    }
    Ok(by_index.into_iter().map(|(_, name)| name).collect())
}

/// `SCAN_COLS` as its events leave it, applied as SPEC's own client
/// applies them (splot's `SpecChannel.update`): an event assigns the
/// elements it carries, or with `FLAG_DELETED` deletes them; a deleted
/// event that is no associative array deletes the array.
#[derive(Debug, Default)]
pub(crate) struct Cols(BTreeMap<String, String>);

impl Cols {
    pub(crate) fn apply(&mut self, p: &Packet) -> Result<(), String> {
        let deleted = p.header.flags & FLAG_DELETED != 0;
        if p.header.data_type != Some(DataType::Assoc) {
            self.0.clear();
            return if deleted {
                Ok(())
            } else {
                Err("SCAN_COLS is not an associative array".into())
            };
        }
        for (index, name) in p.assoc() {
            if deleted {
                self.0.remove(&index);
            } else {
                self.0.insert(index, name);
            }
        }
        Ok(())
    }

    fn names(&self) -> Result<Vec<String>, String> {
        ordered(self.0.clone())
    }
}

/// A `SCAN_PT` value: the point's number and its row.
pub(crate) fn point(text: &str) -> Result<(usize, Vec<f64>), String> {
    let (pno, row, _g3): (f64, Vec<f64>, serde_json::Value) =
        serde_json::from_str(text).map_err(|e| format!("SCAN_PT {text:?}: {e}"))?;
    if pno < 0.0 || pno.fract() != 0.0 {
        return Err(format!("SCAN_PT numbers its point {pno}"));
    }
    Ok((pno as usize, row))
}

/// What the `Scan:` records show.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    /// The column names, one per line; unchanged when `None`.
    pub(crate) cols: Option<String>,
    /// Row by row.
    pub(crate) data: Arc<[f64]>,
}

impl Shown {
    /// Before a scan's first point.
    pub(crate) fn empty() -> Shown {
        Shown {
            cols: Some(String::new()),
            data: Arc::from([] as [f64; 0]),
        }
    }
}

/// The points of the running `ScanCmd`, in order from point 0.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// `SCAN_COLS` at the first point; every later row has its width.
    cols: Vec<String>,
    data: Vec<f64>,
    rows: usize,
}

impl Scan {
    /// Point `pno` with `row`, taken under the columns `named` holds at
    /// the first point; what the records show with it.
    pub(crate) fn add(&mut self, pno: usize, row: Vec<f64>, named: &Cols) -> Result<Shown, String> {
        if pno != self.rows {
            return Err(format!(
                "SCAN_PT sent point {pno} after {} points",
                self.rows
            ));
        }
        let first = self.rows == 0;
        if first {
            self.cols = named.names()?;
        }
        let width = self.cols.len();
        if row.len() != width {
            return Err(format!(
                "point {pno} has {} values for {width} columns",
                row.len()
            ));
        }
        if self.data.len() + row.len() > SCAN_NELM {
            return Err(format!(
                "point {pno} is past the {SCAN_NELM} values Scan:Data holds"
            ));
        }
        self.data.extend(row);
        self.rows += 1;
        Ok(Shown {
            cols: first.then(|| self.cols.join("\n")),
            data: Arc::from(&self.data[..]),
        })
    }

    /// `NPTS` after the scan, which counts the points it took.
    pub(crate) fn took(&self, npts: usize) -> Result<(), String> {
        if npts == self.rows {
            Ok(())
        } else {
            Err(format!(
                "SPEC took {npts} points, {} arrived as SCAN_PT",
                self.rows
            ))
        }
    }

    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    /// `SCAN_COLS` after the scan, which names the columns of its points.
    pub(crate) fn named(&self, cols: &[String]) -> Result<(), String> {
        if cols == self.cols {
            Ok(())
        } else {
            Err(format!(
                "SCAN_COLS names {cols:?}, the points came under {:?}",
                self.cols
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec_protocol::Cmd;

    fn names(n: usize) -> Cols {
        Cols((0..n).map(|i| (i.to_string(), format!("c{i}"))).collect())
    }

    fn event(pairs: &[(&str, &str)], flags: i32) -> Packet {
        let mut p = Packet::new(Cmd::Event, 0, "var/SCAN_COLS").with_assoc(pairs.iter().copied());
        p.header.flags = flags;
        p
    }

    #[test]
    fn npts_is_a_whole_count() {
        let reply = |s| Packet::new(Cmd::Reply, 1, "var/NPTS").with_string(s);
        assert_eq!(rows(&reply("12")), Ok(12));
        assert_eq!(rows(&reply("0")), Ok(0));
        assert!(rows(&reply("-1")).is_err());
        assert!(rows(&reply("1.5")).is_err());
    }

    #[test]
    fn scan_cols_are_ordered_by_index() {
        let p = Packet::new(Cmd::Event, 0, "var/SCAN_COLS").with_assoc([
            ("1", "sec"),
            ("0", "th"),
            ("2", "det"),
        ]);
        assert_eq!(cols(&p), Ok(vec!["th".into(), "sec".into(), "det".into()]));
    }

    #[test]
    fn scan_cols_with_a_gap_fail() {
        let p = Packet::new(Cmd::Event, 0, "var/SCAN_COLS").with_assoc([("0", "th"), ("2", "det")]);
        assert!(cols(&p).is_err());
        let s = Packet::new(Cmd::Event, 0, "var/SCAN_COLS").with_string("th");
        assert!(cols(&s).is_err());
    }

    #[test]
    fn scan_cols_events_assign_and_delete_elements() {
        let mut c = Cols::default();
        c.apply(&event(&[("1", "sec"), ("0", "th")], 0)).unwrap();
        c.apply(&event(&[("2", "det")], 0)).unwrap();
        assert_eq!(c.names(), Ok(vec!["th".into(), "sec".into(), "det".into()]));
        c.apply(&event(&[("2", "det")], FLAG_DELETED)).unwrap();
        assert_eq!(c.names(), Ok(vec!["th".into(), "sec".into()]));
        c.apply(&event(&[], FLAG_DELETED)).unwrap();
        assert_eq!(
            c.names(),
            Ok(vec!["th".into(), "sec".into()]),
            "deletes no element"
        );
    }

    #[test]
    fn a_deleted_scan_cols_names_none() {
        let mut c = names(2);
        let mut p = Packet::new(Cmd::Event, 0, "var/SCAN_COLS").with_string("");
        p.header.flags = FLAG_DELETED;
        c.apply(&p).unwrap();
        assert_eq!(c.names(), Ok(vec![]));
    }

    #[test]
    fn a_scan_cols_event_that_is_no_array_names_none() {
        let mut c = names(2);
        let p = Packet::new(Cmd::Event, 0, "var/SCAN_COLS").with_string("th");
        assert!(c.apply(&p).is_err());
        assert_eq!(c.names(), Ok(vec![]));
    }

    #[test]
    fn scan_pt_is_number_row_and_g3() {
        assert_eq!(
            point("[2, [0.125, 1e3, -4], 0]"),
            Ok((2, vec![0.125, 1000.0, -4.0]))
        );
        assert_eq!(point(r#"[0, [], "x"]"#), Ok((0, vec![])));
        // SPEC's 15 significant digits come back as the closest double.
        assert_eq!(point("[0, [0.100000000000000], 0]").unwrap().1, vec![0.1]);
        assert!(point("[1.5, [1], 0]").is_err());
        assert!(point("[-1, [1], 0]").is_err());
        assert!(point("[0, [1]]").is_err(), "no g3");
        assert!(point("[0, [\"a\"], 0]").is_err());
        assert!(point("0").is_err());
    }

    #[test]
    fn points_are_taken_in_order_under_the_first_cols() {
        let mut s = Scan::default();
        let gap = Cols(
            [("0", "th"), ("2", "det")]
                .map(|(i, n)| (i.into(), n.into()))
                .into(),
        );
        assert!(
            s.add(0, vec![1.0, 10.0], &gap).is_err(),
            "SCAN_COLS with a gap"
        );
        let first = s.add(0, vec![1.0, 10.0], &names(2)).unwrap();
        assert_eq!(first.cols.as_deref(), Some("c0\nc1"));
        assert_eq!(&first.data[..], &[1.0, 10.0]);
        // SCAN_COLS changing mid-scan does not relabel the points.
        let second = s.add(1, vec![2.0, 20.0], &gap).unwrap();
        assert_eq!(second.cols, None);
        assert_eq!(&second.data[..], &[1.0, 10.0, 2.0, 20.0]);
        assert_eq!(s.rows(), 2);
    }

    #[test]
    fn a_point_out_of_turn_fails() {
        let mut s = Scan::default();
        assert!(s.add(1, vec![1.0], &names(1)).is_err(), "skips point 0");
        s.add(0, vec![1.0], &names(1)).unwrap();
        assert!(s.add(0, vec![1.0], &names(1)).is_err(), "repeats point 0");
        assert!(s.add(2, vec![1.0], &names(1)).is_err(), "skips point 1");
    }

    #[test]
    fn a_row_of_another_width_fails() {
        let mut s = Scan::default();
        assert!(s.add(0, vec![1.0], &names(2)).is_err(), "first row");
        s.add(0, vec![1.0, 2.0], &names(2)).unwrap();
        assert!(s.add(1, vec![1.0], &names(1)).is_err(), "later row");
    }

    #[test]
    fn more_than_scan_nelm_fails() {
        let mut s = Scan::default();
        let half = SCAN_NELM / 2;
        for pno in 0..2 {
            s.add(pno, vec![0.0; half], &names(half)).unwrap();
        }
        assert!(s.add(2, vec![0.0; half], &names(half)).is_err());
    }

    #[test]
    fn npts_and_scan_cols_must_agree_at_the_end() {
        let mut s = Scan::default();
        assert_eq!(s.took(0), Ok(()));
        s.add(0, vec![1.0, 2.0], &names(2)).unwrap();
        assert_eq!(s.took(1), Ok(()));
        assert!(s.took(2).is_err(), "a point never arrived");
        assert_eq!(s.named(&["c0".into(), "c1".into()]), Ok(()));
        assert!(s.named(&["th".into(), "det".into()]).is_err());
    }
}
