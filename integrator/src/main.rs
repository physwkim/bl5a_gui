//! bl5a-integrator: integrates each mar345 frame an "exposures" event
//! names and appends the row to its series' stack in the run's data
//! folder (`data_dir`), and serves the stacks.
//!
//! REP, one JSON request per message, a JSON reply:
//!   {"method": "config"}                 → poni, npt, unit, q_max, root
//!   {"method": "set", "poni": text, "npt": n}
//!                                        → the config the next frames
//!                                          get, once the queued ones are
//!                                          done (PUB `config` then)
//!   {"method": "stacks"}                 → {"stacks": [{path, series, rows}, …]}
//!   {"method": "history", "path": p, "from": k (optional, default 0)}
//!                                        → q and rows k.. of the stack at p
//!   {"method": "calibrate", "path": frame, "poni": text,
//!    "calibrant": .D text, "rings": n, "delta_tth": deg, "factor": f,
//!    "upper": counts, "fixed": [names] (optional),
//!    "points": [[y, x, ring], …] (optional)}
//!                                        → {"calibrating": id}; the
//!                                          refined PONI comes as PUB
//!                                          `calibration`
//! PUB `row <json>` for each new row (its stack's path, series and
//! index), `config <json>` when a set takes effect, `error <json>` for
//! each exposure that gave no row, and `calibration <json>` for each
//! calibrate (its id and the PONI, chi2 and peaks, or its error).

mod calibrate;
mod docs;
mod engine;
mod mar;
mod series;

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use clap::Parser;
use serde_json::{Value, json};

use crate::docs::{Exposure, Runs};
use crate::engine::{Engine, UNIT};
use crate::series::{Frame, Store};

#[derive(Parser)]
struct Args {
    /// The PONI of the full 345 mm plate.
    #[arg(long)]
    poni: PathBuf,
    /// Bins from q = 0 to the plate's far rim.
    #[arg(long, default_value_t = 2000)]
    npt: usize,
    /// bl5a-qs's --data-root: the stacks in its folders are listed from
    /// the start.
    #[arg(long)]
    root: PathBuf,
    /// The bsrs daemon's document PUB.
    #[arg(long, default_value = "tcp://localhost:60725")]
    documents: String,
    #[arg(long, default_value = "tcp://*:60735")]
    rep: String,
    #[arg(long = "pub", default_value = "tcp://*:60745")]
    publish: String,
}

fn main() {
    if let Err(e) = run(Args::parse()) {
        eprintln!("bl5a-integrator: {e}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<(), String> {
    let text =
        std::fs::read_to_string(&args.poni).map_err(|e| format!("{}: {e}", args.poni.display()))?;
    let engine =
        Engine::new(text, args.npt).map_err(|e| format!("{}: {e}", args.poni.display()))?;
    // Stacks are keyed by path, and runs name their folders absolutely.
    if !args.root.is_absolute() {
        return Err(format!("--root {}: not absolute", args.root.display()));
    }
    let mut store = Store::default();
    let shown = |e: &dyn std::fmt::Display| format!("{}: {e}", args.root.display());
    for entry in std::fs::read_dir(&args.root).map_err(|e| shown(&e))? {
        let dir = entry.map_err(|e| shown(&e))?.path();
        if dir.is_dir() {
            for r in store.load_dir(&dir)? {
                eprintln!("not loaded: {r}");
            }
        }
    }
    let root = args.root.display().to_string();
    let config = Arc::new(Mutex::new(config_of(&engine, &root)));
    let store = Arc::new(Mutex::new(store));
    let ctx = zmq::Context::new();
    let socket = |kind, addr: &str, bind: bool| -> Result<zmq::Socket, String> {
        let s = ctx.socket(kind).map_err(|e| e.to_string())?;
        let done = if bind { s.bind(addr) } else { s.connect(addr) };
        done.map_err(|e| format!("{addr}: {e}"))?;
        Ok(s)
    };
    let rep = socket(zmq::REP, &args.rep, true)?;
    let publish = socket(zmq::PUB, &args.publish, true)?;
    let sub = socket(zmq::SUB, &args.documents, false)?;
    sub.set_subscribe(b"").map_err(|e| e.to_string())?;

    let (tx, rx) = mpsc::channel();
    let listened = tx.clone();
    thread::spawn(move || listen(&sub, &listened));
    let (served, shown_config) = (store.clone(), config.clone());
    thread::spawn(move || serve(&rep, &served, &shown_config, &tx, &root));
    eprintln!(
        "bl5a-integrator: {} stacks, q to {} {UNIT} in {} bins",
        store.lock().unwrap().stacks().len(),
        engine.q_max,
        engine.npt
    );
    work(&rx, engine, &store, &config, &publish);
    Err("the document listener stopped".into())
}

/// What `config` answers: the engine the next frames get.
fn config_of(engine: &Engine, root: &str) -> Value {
    json!({
        "poni": engine.poni_text, "npt": engine.npt, "unit": UNIT, "q_max": engine.q_max,
        "root": root,
    })
}

enum Job {
    Exposure(Exposure),
    Error(String),
    /// A `set`: the engine of the frames after the queued ones, and its
    /// config.
    Configure(Engine, Value),
    /// A calibrate's outcome, to publish.
    Calibrated(Value),
}

/// The document stream, as exposures and the errors of documents that
/// should have been one.
fn listen(sub: &zmq::Socket, tx: &mpsc::Sender<Job>) {
    let mut runs = Runs::default();
    loop {
        let frame = match sub.recv_bytes(0) {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.send(Job::Error(format!("document stream: {e}")));
                return;
            }
        };
        let job = match docs::parse(&frame).and_then(|(name, doc)| runs.feed(&name, &doc)) {
            Ok(Some(e)) => Job::Exposure(e),
            Ok(None) => continue,
            Err(e) => Job::Error(e),
        };
        if tx.send(job).is_err() {
            return;
        }
    }
}

/// The jobs in the order they came: the only thread that integrates,
/// swaps the engine and publishes.
fn work(
    rx: &mpsc::Receiver<Job>,
    mut engine: Engine,
    store: &Mutex<Store>,
    config: &Mutex<Value>,
    publish: &zmq::Socket,
) {
    let send = |topic: &str, body: &Value| {
        if let Err(e) = publish.send(format!("{topic} {body}").as_bytes(), 0) {
            eprintln!("pub: {e}");
        }
    };
    for job in rx {
        let (topic, body) = match job {
            Job::Exposure(e) => {
                let mut refused = Vec::new();
                let done = integrate(&mut engine, store, &e, &mut refused);
                for r in refused {
                    let body = json!({"series": e.series, "error": format!("not loaded: {r}")});
                    eprintln!("{body}");
                    send("error", &body);
                }
                match done {
                    Ok(Some((path, index))) => (
                        "row",
                        json!({"path": path, "series": e.series, "index": index}),
                    ),
                    Ok(None) => continue,
                    Err(err) => (
                        "error",
                        json!({"series": e.series, "file": e.shot.file, "error": err}),
                    ),
                }
            }
            Job::Error(err) => ("error", json!({ "error": err })),
            Job::Configure(next, shown) => {
                engine = next;
                *config.lock().unwrap() = shown.clone();
                ("config", shown)
            }
            Job::Calibrated(body) => ("calibration", body),
        };
        if topic == "error" {
            eprintln!("{body}");
        }
        send(topic, &body);
    }
}

/// `e`'s row in its series' stack: the stack's path and the row's index,
/// or `None` when the stack had it.
fn integrate(
    engine: &mut Engine,
    store: &Mutex<Store>,
    e: &Exposure,
    refused: &mut Vec<String>,
) -> Result<Option<(PathBuf, usize)>, String> {
    let bytes = std::fs::read(&e.shot.file).map_err(|err| format!("{}: {err}", e.shot.file))?;
    let image = mar::decode(&bytes).map_err(|err| format!("{}: {err}", e.shot.file))?;
    let profile = engine.integrate(&image, e.shot.mon)?;
    let frame = Frame {
        shot: e.shot.clone(),
        poni: engine.poni_text.clone(),
    };
    store
        .lock()
        .unwrap()
        .append(&e.dir, &e.series, &engine.q(), frame, &profile, refused)
}

fn serve(
    rep: &zmq::Socket,
    store: &Mutex<Store>,
    config: &Mutex<Value>,
    jobs: &mpsc::Sender<Job>,
    root: &str,
) {
    let mut calibrations = 0;
    loop {
        let reply = match rep.recv_bytes(0) {
            Ok(req) => answer(&req, store, config, jobs, root, &mut calibrations)
                .unwrap_or_else(|e| json!({ "error": e })),
            Err(e) => {
                eprintln!("rep: {e}");
                return;
            }
        };
        if let Err(e) = rep.send(reply.to_string().as_bytes(), 0) {
            eprintln!("rep: {e}");
            return;
        }
    }
}

fn answer(
    req: &[u8],
    store: &Mutex<Store>,
    config: &Mutex<Value>,
    jobs: &mpsc::Sender<Job>,
    root: &str,
    calibrations: &mut u64,
) -> Result<Value, String> {
    let req: Value = serde_json::from_slice(req).map_err(|e| format!("request: {e}"))?;
    match req.get("method").and_then(Value::as_str) {
        Some("config") => Ok(config.lock().unwrap().clone()),
        Some("set") => {
            let poni = req
                .get("poni")
                .and_then(Value::as_str)
                .ok_or("set needs the poni text")?;
            let npt = req
                .get("npt")
                .and_then(Value::as_u64)
                .ok_or("set needs npt, a whole number")?;
            let npt = usize::try_from(npt).map_err(|_| format!("npt {npt}: too large"))?;
            let engine = Engine::new(poni.to_string(), npt)?;
            let shown = config_of(&engine, root);
            jobs.send(Job::Configure(engine, shown.clone()))
                .map_err(|_| "the integrating thread stopped")?;
            Ok(shown)
        }
        Some("stacks") => {
            let store = store.lock().unwrap();
            let stacks: Vec<Value> = store
                .stacks()
                .into_iter()
                .map(|(path, series, rows)| json!({"path": path, "series": series, "rows": rows}))
                .collect();
            Ok(json!({ "stacks": stacks }))
        }
        Some("history") => {
            let path = req
                .get("path")
                .and_then(Value::as_str)
                .ok_or("history needs a stack's path")?;
            let from = match req.get("from") {
                None => 0,
                Some(v) => v.as_u64().ok_or("from is a row index")? as usize,
            };
            let store = store.lock().unwrap();
            let s = store
                .get(Path::new(path))
                .ok_or_else(|| format!("no stack {path:?}"))?;
            let from = from.min(s.frames.len());
            Ok(json!({
                "path": path, "series": s.series, "from": from, "q": s.q, "unit": UNIT,
                "frames": s.frames[from..], "intensity": s.intensity[from..], "sigma": s.sigma[from..],
            }))
        }
        Some("calibrate") => {
            let request = calibrate::Request::parse(&req)?;
            *calibrations += 1;
            let (id, jobs) = (*calibrations, jobs.clone());
            // Seconds of work on a full plate: off the REP thread, so the
            // other requests are answered meanwhile.
            thread::spawn(move || {
                let done = panic::catch_unwind(AssertUnwindSafe(|| calibrate::run(&request)))
                    .unwrap_or_else(|_| Err("the calibration panicked".into()));
                let body = match done {
                    Ok(mut v) => {
                        v["id"] = json!(id);
                        v
                    }
                    Err(e) => json!({"id": id, "error": e}),
                };
                let _ = jobs.send(Job::Calibrated(body));
            });
            Ok(json!({ "calibrating": id }))
        }
        Some(m) => Err(format!(
            "method {m:?}: one of config, set, stacks, history, calibrate"
        )),
        None => Err("a request without a method".into()),
    }
}
