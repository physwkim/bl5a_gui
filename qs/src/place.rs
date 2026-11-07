//! Where a queue item's data goes: one folder per item under a data root,
//! `<root>/<folder>/`, holding its mar345 frames, `<folder>.spec` (its
//! scans) and the integrator's stacks. The folder is the item's
//! `md.series`, else its plan's name; the root is its `md.data_root`,
//! else bl5a-qs's `--data-root`. The item carries the root so that what
//! is queued shows where its data goes; qs keeps no mode of its own.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The item md key that overrides `--data-root`.
pub const DATA_ROOT: &str = "data_root";
/// The start-document key of the item's folder, which the integrator
/// files its stacks in.
pub const DATA_DIR: &str = "data_dir";
/// An item folder's longest path, bytes: the C ADmar345 sends
/// `COMMAND SCAN <folder>/<name>_<number>.mar3450` from a 256-byte
/// buffer, with a name of up to 27 bytes and a number of up to 5 digits.
const DIR_MAX: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub dir: PathBuf,
    /// The SPEC file of the item's scans.
    pub spec: PathBuf,
}

impl Place {
    /// The item's folder from its md, under `root` unless md names one.
    pub fn of(plan: &str, root: &Path, md: &HashMap<String, Value>) -> Result<Self, String> {
        let root = match md.get(DATA_ROOT) {
            None => root.to_path_buf(),
            Some(Value::String(s)) if Path::new(s).is_absolute() => PathBuf::from(s),
            Some(v) => {
                return Err(format!(
                    "{plan}: md.{DATA_ROOT} must be an absolute path, got {v}"
                ));
            }
        };
        let folder = match md.get("series") {
            None => plan.to_string(),
            Some(Value::String(s)) => folder(s).map_err(|e| format!("{plan}: md.series {e}"))?,
            Some(v) => return Err(format!("{plan}: md.series must be a string, got {v}")),
        };
        let dir = root.join(&folder);
        frame_dir(&dir).map_err(|e| format!("{plan}: data folder {e}"))?;
        Ok(Self {
            spec: dir.join(format!("{folder}.spec")),
            dir,
        })
    }

    /// The folder, made if it is not there; its root must be.
    pub fn make(&self) -> io::Result<()> {
        match std::fs::create_dir(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && self.dir.is_dir() => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(io::Error::new(
                e.kind(),
                format!(
                    "{}: the data root {} does not exist",
                    self.dir.display(),
                    self.dir.parent().unwrap_or(Path::new("")).display()
                ),
            )),
            Err(e) => Err(io::Error::new(
                e.kind(),
                format!("{}: {e}", self.dir.display()),
            )),
        }
    }
}

/// A series as one path component; `frame_dir` bounds the rest.
fn folder(series: &str) -> Result<String, String> {
    if series.is_empty() || series == "." || series == ".." || series.contains('/') {
        return Err(format!(
            "must not be empty, \".\" or \"..\", nor hold '/', to name a folder: got {series:?}"
        ));
    }
    Ok(series.to_string())
}

/// `dir` as the folder of mar345 frames: mar345dtb reads it as one word
/// of its `COMMAND SCAN <file>` line, so no space or control character
/// (a space split the line in bl5a-sim's mar345dtb), and `DIR_MAX`.
pub fn frame_dir(dir: &Path) -> Result<(), String> {
    let text = dir
        .to_str()
        .ok_or_else(|| format!("{}: not UTF-8", dir.display()))?;
    if text.len() > DIR_MAX || text.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{text:?}: must be at most {DIR_MAX} bytes without spaces or control \
             characters, as mar345dtb takes it in one word"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn md(v: Value) -> HashMap<String, Value> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn the_folder_is_the_series_else_the_plan() {
        let root = Path::new("/data");
        let p = Place::of("mar_series", root, &md(json!({"series": "cell_7"}))).unwrap();
        assert_eq!(p.dir, Path::new("/data/cell_7"));
        assert_eq!(p.spec, Path::new("/data/cell_7/cell_7.spec"));
        let p = Place::of("align", root, &md(json!({}))).unwrap();
        assert_eq!(p.dir, Path::new("/data/align"));
        assert_eq!(p.spec, Path::new("/data/align/align.spec"));
    }

    #[test]
    fn the_item_root_wins() {
        let p = Place::of(
            "p",
            Path::new("/data"),
            &md(json!({"data_root": "/mnt/u1", "series": "s"})),
        )
        .unwrap();
        assert_eq!(p.dir, Path::new("/mnt/u1/s"));
        for bad in [
            json!("rel/dir"),
            json!(""),
            json!("/mnt/u 1"),
            json!("/mnt/u\t1"),
            json!(format!("/{}", "x".repeat(198))),
            json!(3),
            json!(null),
        ] {
            let m = md(json!({ "data_root": bad }));
            assert!(Place::of("p", Path::new("/data"), &m).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_series_names_one_folder() {
        for bad in [
            json!(""),
            json!("."),
            json!(".."),
            json!("a/b"),
            json!("a\0b"),
            json!("cell 7"),
            json!("cell\t7"),
            json!("cell\u{3000}7"),
            json!("x".repeat(198)),
            json!("é".repeat(99)),
            json!(7),
            json!(null),
        ] {
            let m = md(json!({ "series": bad }));
            assert!(Place::of("p", Path::new("/d"), &m).is_err(), "{bad}");
        }
        // "/d/" and the series: DIR_MAX bytes, not characters.
        for good in ["x".repeat(197), "é".repeat(98)] {
            let m = md(json!({ "series": good }));
            assert!(Place::of("p", Path::new("/d"), &m).is_ok(), "{good}");
        }
    }

    #[test]
    fn make_needs_the_root() {
        let tmp = std::env::temp_dir().join(format!("bl5a-qs-place-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir(&tmp).unwrap();
        let p = Place::of("p", &tmp, &md(json!({"series": "s"}))).unwrap();
        p.make().unwrap();
        p.make().unwrap();
        assert!(p.dir.is_dir());
        let orphan = Place::of("p", &tmp.join("missing"), &md(json!({}))).unwrap();
        let e = orphan.make().unwrap_err();
        assert!(e.to_string().contains("does not exist"), "{e}");
        std::fs::write(tmp.join("f"), b"").unwrap();
        let file = Place::of("p", &tmp, &md(json!({"series": "f"}))).unwrap();
        assert!(file.make().is_err(), "a file in the way");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
