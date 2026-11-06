//! The beamline over Channel Access: spec-ioc's SPEC motors, scaler and
//! command records, and mar345-ioc's cam1.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bsrs::backends::epics_ca::{CaEnumBackend, EpicsCaBackend};
use bsrs::core::msg::{
    DynLocation, LocatableObj, MonitorableObj, MovableObj, NamedObj, ReadableObj, StoppableObj,
    TriggerableObj,
};
use bsrs::core::status::{Status, StatusError, SubToken};
use bsrs::core::{BsrsError, ReadingValue, Result as BResult, TypedReading};
use bsrs::devices::BackendFromPv;
use bsrs::event_model::{DataKey, Dtype, SignalMetadata, make_datakey};
use bsrs::host::ca_devices::CaMotor;
use bsrs::protocols_async::{SignalBackend, Subscription};
use bsrs::qs::Registry;
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};

use crate::{Error, Result};

pub const SPEC: &str = "BL5A:SPEC:";
pub const CAM: &str = "BL5A:MAR345:cam1:";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A put that completes when the record has processed.
pub const PUT_TIMEOUT: Duration = Duration::from_secs(10);
/// A move's put completes when the motor is done.
const MOVE_TIMEOUT: Duration = Duration::from_secs(3600);
/// `set` redefines the position; the readback follows on the next poll.
const SET_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);
/// SPEC back at its prompt after a ^C.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// mar345-ioc's task takes an Acquire on its next wake-up.
const TAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// SpecCreate's motors, in its order.
pub const AXES: [&str; 11] = [
    "sy", "shold", "th", "tth", "nu", "scell", "martran", "marsdd", "marshut", "sx", "phi",
];
/// SpecCreate's counters: scaler1 S1..S4.
pub const CHANNELS: [&str; 4] = ["sec", "icxrd", "det", "ic1"];

fn connect_err(pv: &str, e: impl std::fmt::Display) -> Error {
    Error::new(format!("{pv}: {e}"))
}

/// `pv`'s backend, connected; a `String` one reads a char waveform as its
/// text up to the first NUL.
async fn backend<T>(pv: &str) -> Result<Arc<EpicsCaBackend<T>>>
where
    T: Clone + Send + Sync + 'static,
    EpicsCaBackend<T>: BackendFromPv + SignalBackend<T>,
{
    connected(EpicsCaBackend::from_pv(pv), pv).await
}

async fn connected<T>(b: EpicsCaBackend<T>, pv: &str) -> Result<Arc<EpicsCaBackend<T>>>
where
    T: Clone + Send + Sync + 'static,
    EpicsCaBackend<T>: SignalBackend<T>,
{
    b.connect(CONNECT_TIMEOUT)
        .await
        .map_err(|e| connect_err(pv, e))?;
    Ok(Arc::new(b))
}

/// A put that completes when the record has processed, bounded.
pub async fn put<T>(b: &EpicsCaBackend<T>, v: T) -> BResult<()>
where
    T: Clone + Send + Sync + 'static,
    EpicsCaBackend<T>: SignalBackend<T>,
{
    tokio::time::timeout(PUT_TIMEOUT, b.put(Some(v)))
        .await
        .map_err(|_| BsrsError::Timeout(PUT_TIMEOUT))?
}

/// A bounded get.
pub async fn get<T>(b: &EpicsCaBackend<T>) -> BResult<T>
where
    T: Clone + Send + Sync + 'static,
    EpicsCaBackend<T>: SignalBackend<T>,
{
    tokio::time::timeout(PUT_TIMEOUT, b.get_value())
        .await
        .map_err(|_| BsrsError::Timeout(PUT_TIMEOUT))?
}

/// One SPEC motor's record.
pub struct Axis {
    name: String,
    /// SPEC's motor name (`motor_name`), which its messages print.
    desc: String,
    record: String,
    motor: Arc<CaMotor>,
    val: Arc<EpicsCaBackend<f64>>,
    rbv: Arc<EpicsCaBackend<f64>>,
    dmov: Arc<EpicsCaBackend<i64>>,
    stop: Arc<EpicsCaBackend<bool>>,
    set: Arc<EpicsCaBackend<bool>>,
    hlm: Arc<EpicsCaBackend<f64>>,
    llm: Arc<EpicsCaBackend<f64>>,
    mres: f64,
}

impl Axis {
    async fn connect(name: &str) -> Result<Arc<Self>> {
        let record = format!("{SPEC}{name}");
        let f = |field: &str| format!("{record}.{field}");
        let motor = CaMotor::connect_async(name, &f("VAL"), &f("RBV"))
            .await
            .map_err(|e| connect_err(&record, e))?;
        let mres_b = backend::<f64>(&f("MRES")).await?;
        let mres = mres_b
            .get_value()
            .await
            .map_err(|e| connect_err(&f("MRES"), e))?
            .abs();
        let desc = get(&*backend::<String>(&f("DESC")).await?)
            .await
            .map_err(|e| connect_err(&f("DESC"), e))?;
        Ok(Arc::new(Self {
            name: name.to_string(),
            desc,
            motor,
            val: backend(&f("VAL")).await?,
            rbv: backend(&f("RBV")).await?,
            dmov: backend(&f("DMOV")).await?,
            stop: connected(
                EpicsCaBackend::new(f("STOP")).with_wait(|_| false),
                &f("STOP"),
            )
            .await?,
            set: backend(&f("SET")).await?,
            hlm: backend(&f("HLM")).await?,
            llm: backend(&f("LLM")).await?,
            mres,
            record,
        }))
    }

    pub fn desc(&self) -> &str {
        &self.desc
    }

    /// A move to `value`, for [`crate::ctx::Ctx::mv`].
    pub fn to(self: &Arc<Self>, value: f64) -> (Arc<dyn MovableObj>, f64) {
        (self.clone(), value)
    }

    pub async fn position(&self) -> BResult<f64> {
        get(&self.rbv).await
    }

    pub async fn moving(&self) -> BResult<bool> {
        Ok(get(&self.dmov).await? == 0)
    }

    /// STOP aborts everything SPEC runs (the driver sends ^C), so it goes
    /// out only to a motor that moves.
    pub async fn stop_if_moving(&self) -> BResult<()> {
        if self.moving().await? {
            self.stop.put(Some(true)).await?;
        }
        Ok(())
    }

    /// Until DMOV, bounded.
    pub async fn settle(&self, timeout: Duration) -> BResult<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.moving().await? {
            if tokio::time::Instant::now() >= deadline {
                return Err(BsrsError::Timeout(timeout));
            }
            tokio::time::sleep(POLL).await;
        }
        Ok(())
    }

    /// The soft limits; `None` when both are 0, the record's "no limits".
    pub async fn limits(&self) -> BResult<Option<(f64, f64)>> {
        let (lo, hi) = (get(&self.llm).await?, get(&self.hlm).await?);
        Ok((lo != 0.0 || hi != 0.0).then_some((lo, hi)))
    }

    /// SPEC `set name pos`: the record's SET mode with the offset frozen
    /// makes the driver redefine the position. SET goes back to Use
    /// whatever the write did.
    pub async fn set_position(&self, pos: f64) -> BResult<()> {
        put(&self.set, true).await?;
        let wrote = put(&self.val, pos).await;
        let used = put(&self.set, false).await;
        wrote?;
        used?;
        let deadline = tokio::time::Instant::now() + SET_TIMEOUT;
        loop {
            let rbv = self.position().await?;
            if (rbv - pos).abs() <= 1.5 * self.mres {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(BsrsError::State(format!(
                    "set {} {pos}: readback stays at {rbv}",
                    self.name
                )));
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

impl NamedObj for Axis {
    fn name(&self) -> &str {
        &self.name
    }
    fn inspect_dyn(&self) -> Value {
        json!({"name": self.name, "type": "Axis", "record": self.record})
    }
}

#[async_trait]
impl ReadableObj for Axis {
    async fn read_dyn(&self) -> BResult<HashMap<String, ReadingValue>> {
        self.motor.read_dyn().await
    }
    async fn describe_dyn(&self) -> BResult<HashMap<String, DataKey>> {
        self.motor.describe_dyn().await
    }
}

#[async_trait]
impl MovableObj for Axis {
    /// The VAL put completes at DMOV; a move that ends short of the
    /// target (a stop, a limit) is a failure, as SPEC's `umv` would leave
    /// the macro on the wrong position otherwise.
    async fn set_dyn(&self, value: f64) -> Status {
        let (status, setter) = Status::new();
        let (val, rbv, name, tol) = (
            self.val.clone(),
            self.rbv.clone(),
            self.name.clone(),
            2.0 * self.mres,
        );
        tokio::spawn(async move {
            match tokio::time::timeout(MOVE_TIMEOUT, val.put(Some(value))).await {
                // Not StatusError::Timeout: only the status's own timer
                // reports that.
                Err(_) => setter.fail(StatusError::Failed(format!(
                    "{name} move: not done in {MOVE_TIMEOUT:?}"
                ))),
                Ok(Err(e)) => setter.fail(StatusError::Failed(format!("{name} move: {e}"))),
                Ok(Ok(())) => match get(&rbv).await {
                    Ok(at) if (at - value).abs() <= tol => setter.success(),
                    Ok(at) => setter.fail(StatusError::Failed(format!(
                        "{name} stopped at {at}, not {value}"
                    ))),
                    Err(e) => setter.fail(StatusError::Failed(format!("{name} readback: {e}"))),
                },
            }
        });
        status
    }

    async fn stop_on_pause(&self, _success: bool) -> BResult<()> {
        self.stop_if_moving().await
    }
}

#[async_trait]
impl LocatableObj for Axis {
    async fn locate_dyn(&self) -> BResult<DynLocation> {
        self.motor.locate_dyn().await
    }
}

#[async_trait]
impl StoppableObj for Axis {
    async fn stop_dyn(&self, _success: bool) -> BResult<()> {
        self.stop_if_moving().await
    }
}

#[async_trait]
impl MonitorableObj for Axis {
    async fn subscribe_dyn(&self) -> BResult<Subscription> {
        self.motor.subscribe_dyn().await
    }
}

/// scaler1.
pub struct Scaler {
    /// Its count's put-callback is the only one, a stop goes without: the CA
    /// server holds a channel's second put-callback, and the circuit behind
    /// it, until the first completes, which for a count is its end.
    cnt: Arc<EpicsCaBackend<bool>>,
    tp: Arc<EpicsCaBackend<f64>>,
    g2: Arc<EpicsCaBackend<bool>>,
    pr2: Arc<EpicsCaBackend<f64>>,
    freq: f64,
    s: [Arc<EpicsCaBackend<f64>>; 4],
}

impl Scaler {
    async fn connect() -> Result<Arc<Self>> {
        let p = format!("{SPEC}scaler1");
        let freq_pv = format!("{p}.FREQ");
        let freq = backend::<f64>(&freq_pv)
            .await?
            .get_value()
            .await
            .map_err(|e| connect_err(&freq_pv, e))?;
        let s = |i: usize| format!("{p}.S{i}");
        Ok(Arc::new(Self {
            cnt: connected(
                EpicsCaBackend::new(format!("{p}.CNT")).with_wait(|start| *start),
                &format!("{p}.CNT"),
            )
            .await?,
            tp: backend(&format!("{p}.TP")).await?,
            g2: backend(&format!("{p}.G2")).await?,
            pr2: backend(&format!("{p}.PR2")).await?,
            freq,
            s: [
                backend(&s(1)).await?,
                backend(&s(2)).await?,
                backend(&s(3)).await?,
                backend(&s(4)).await?,
            ],
        }))
    }

    /// The counts in [`CHANNELS`] order, `sec` in seconds.
    pub async fn counts(&self) -> BResult<[f64; 4]> {
        let mut out = [0.0; 4];
        for (o, b) in out.iter_mut().zip(&self.s) {
            *o = get(b).await?;
        }
        out[0] /= self.freq;
        Ok(out)
    }

    /// A count to `preset`: the gates leave S1 or S2 the only preset the
    /// record loads, then CNT with completion, which ends with the count.
    /// A count SPEC refused ends at once with no ticks.
    pub async fn count(&self, preset: Preset) -> BResult<()> {
        match preset {
            Preset::Seconds(t) => {
                put(&self.g2, false).await?;
                put(&self.tp, t).await?;
            }
            Preset::Monitor(n) => {
                put(&self.pr2, n).await?;
                put(&self.g2, true).await?;
            }
        }
        self.cnt.put(Some(true)).await?;
        if get(&self.s[0]).await? == 0.0 {
            return Err(BsrsError::State(
                "scaler count refused (see BL5A:SPEC:Log)".into(),
            ));
        }
        Ok(())
    }

    pub async fn counting(&self) -> BResult<bool> {
        get(&self.cnt).await
    }

    /// Ends a count in progress; the count's own put completes with it.
    pub async fn stop_if_counting(&self) -> BResult<()> {
        if self.counting().await? {
            self.cnt.put(Some(false)).await?;
        }
        Ok(())
    }
}

/// What ends a [`Scaler::count`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Preset {
    Seconds(f64),
    /// Counts of SPEC's monitor channel, S2.
    Monitor(f64),
}

/// A step-scan point's count: [`Scaler::count`] for `seconds`, read as
/// the four channels.
pub struct Count {
    pub scaler: Arc<Scaler>,
    pub seconds: f64,
}

impl NamedObj for Count {
    fn name(&self) -> &str {
        "scaler"
    }
}

#[async_trait]
impl TriggerableObj for Count {
    async fn trigger_dyn(&self) -> Status {
        let (status, setter) = Status::new();
        let (scaler, seconds) = (self.scaler.clone(), self.seconds);
        tokio::spawn(async move {
            match scaler.count(Preset::Seconds(seconds)).await {
                Ok(()) => setter.success(),
                Err(e) => setter.fail(StatusError::Failed(e.to_string())),
            }
        });
        status
    }
}

fn number_key(source: &str) -> DataKey {
    make_datakey(
        source,
        Dtype::Number,
        vec![],
        None,
        SignalMetadata::default(),
    )
}

#[async_trait]
impl ReadableObj for Count {
    async fn read_dyn(&self) -> BResult<HashMap<String, ReadingValue>> {
        let now = now();
        let counts = self.scaler.counts().await?;
        Ok(CHANNELS
            .iter()
            .zip(counts)
            .map(|(k, v)| (k.to_string(), TypedReading::new(json!(v), now)))
            .collect())
    }
    async fn describe_dyn(&self) -> BResult<HashMap<String, DataKey>> {
        Ok(CHANNELS
            .iter()
            .map(|k| (k.to_string(), number_key(&format!("{SPEC}scaler1"))))
            .collect())
    }
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// spec-ioc's command records: a SPEC command line and its outcome.
pub struct SpecCmd {
    cmd: Arc<EpicsCaBackend<String>>,
    /// A command whose outcome brings SPEC's points of its last scan.
    scan: Arc<EpicsCaBackend<String>>,
    scan_data: Arc<EpicsCaBackend<Vec<f64>>>,
    scan_cols: Arc<EpicsCaBackend<String>>,
    /// The running scan's points as SPEC sends them, and their names.
    live_data: Arc<EpicsCaBackend<Vec<f64>>>,
    live_cols: Arc<EpicsCaBackend<String>>,
    reply: Arc<EpicsCaBackend<String>>,
    status: Arc<EpicsCaBackend<bool>>,
    busy: Arc<EpicsCaBackend<bool>>,
    abort: Arc<EpicsCaBackend<bool>>,
    /// `status/ready`: SPEC away from its prompt, whoever made it so.
    spec_busy: Arc<EpicsCaBackend<bool>>,
}

impl SpecCmd {
    async fn connect() -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            cmd: backend(&format!("{SPEC}Cmd.VAL$")).await?,
            scan: backend(&format!("{SPEC}ScanCmd.VAL$")).await?,
            scan_data: backend(&format!("{SPEC}Scan:Data")).await?,
            scan_cols: backend(&format!("{SPEC}Scan:Cols")).await?,
            live_data: backend(&format!("{SPEC}Scan:Live:Data")).await?,
            live_cols: backend(&format!("{SPEC}Scan:Live:Cols")).await?,
            reply: backend(&format!("{SPEC}Cmd:Reply")).await?,
            status: backend(&format!("{SPEC}Cmd:Status")).await?,
            busy: backend(&format!("{SPEC}Cmd:Busy")).await?,
            abort: backend(&format!("{SPEC}Cmd:Abort")).await?,
            spec_busy: backend(&format!("{SPEC}Busy")).await?,
        }))
    }

    /// Runs `text` to its end: the put completes when SPEC is done, and
    /// Reply and Status are final by then. Unbounded, as an exposure is:
    /// a plan awaits it through `Msg::WaitFor`, which an abort cancels.
    pub async fn run(&self, text: &str) -> BResult<String> {
        self.cmd.put(Some(text.to_string())).await?;
        self.outcome(text).await
    }

    /// Starts the scan `text`, its points watched from before the put.
    pub async fn start_scan(self: Arc<Self>, text: &str) -> BResult<LiveScan> {
        let (tx, seen) = watch::channel(Seen::default());
        let watching = [
            changes(&self.live_cols, tx.clone(), |s, v| s.cols.clone_from(v)).await?,
            changes(&self.live_data, tx, |s, v| s.data.clone_from(v)).await?,
        ];
        let put = self.scan.put_start(Some(text.to_string())).await?;
        let text = text.to_string();
        let done = async move {
            put.await?;
            self.outcome(&text).await?;
            Table::new(&get(&self.scan_cols).await?, get(&self.scan_data).await?)
        }
        .boxed();
        Ok(LiveScan {
            done,
            seen,
            _watching: watching,
        })
    }

    async fn outcome(&self, text: &str) -> BResult<String> {
        let reply = get(&self.reply).await?;
        if !get(&self.status).await? {
            Ok(reply)
        } else {
            Err(BsrsError::State(format!("{text}: {}", reply.trim_end())))
        }
    }

    /// ^C to a command still running.
    pub async fn abort_if_busy(&self) -> BResult<()> {
        if get(&self.busy).await? {
            put(&self.abort, true).await?;
        }
        Ok(())
    }

    /// Until SPEC is back at its prompt, which spec-ioc wants before it
    /// takes a command; a ^C or a stopped count takes a moment to get
    /// there.
    pub async fn wait_ready(&self) -> BResult<()> {
        let end = tokio::time::Instant::now() + READY_TIMEOUT;
        while get(&self.spec_busy).await? {
            if tokio::time::Instant::now() >= end {
                return Err(BsrsError::Timeout(READY_TIMEOUT));
            }
            tokio::time::sleep(POLL).await;
        }
        Ok(())
    }
}

/// SPEC's points of a scan: `SCAN_COLS`'s names and the first `NPTS` rows
/// of `SCAN_D`.
#[derive(Debug, PartialEq)]
pub struct Table {
    pub cols: Vec<String>,
    pub rows: Vec<Vec<f64>>,
}

impl Table {
    /// From Scan:Cols, a name per line, and Scan:Data, row by row.
    fn new(cols: &str, data: Vec<f64>) -> BResult<Self> {
        let cols: Vec<String> = match cols {
            "" => Vec::new(),
            c => c.split('\n').map(String::from).collect(),
        };
        if data.is_empty() {
            return Ok(Self {
                cols,
                rows: Vec::new(),
            });
        }
        if cols.is_empty() || !data.len().is_multiple_of(cols.len()) {
            return Err(BsrsError::State(format!(
                "Scan:Data has {} values, not rows of the {} Scan:Cols names",
                data.len(),
                cols.len()
            )));
        }
        let rows = data.chunks(cols.len()).map(<[f64]>::to_vec).collect();
        Ok(Self { cols, rows })
    }
}

/// Scan:Live:Cols and Scan:Live:Data as they changed after a scan's put.
#[derive(Default)]
struct Seen {
    cols: String,
    data: Vec<f64>,
}

impl Seen {
    /// The rows so far; none before the names, which spec-ioc shows
    /// ahead of the first point.
    fn table(&self) -> BResult<Table> {
        if self.cols.is_empty() {
            return Table::new("", Vec::new());
        }
        Table::new(&self.cols, self.data.clone())
    }
}

/// A scan SPEC is running.
pub struct LiveScan {
    /// The command's end: SPEC's points of the scan, which spec-ioc took
    /// before the put completed.
    done: BoxFuture<'static, BResult<Table>>,
    seen: watch::Receiver<Seen>,
    _watching: [SubToken; 2],
}

pub enum Live {
    /// The scan runs on; its points so far.
    Running(LiveScan, Table),
    Ended(Table),
}

impl LiveScan {
    /// Until the points change or the command ends.
    pub async fn next(mut self) -> BResult<Live> {
        let changed = tokio::select! {
            t = &mut self.done => return t.map(Live::Ended),
            c = self.seen.changed() => c,
        };
        changed.map_err(|_| BsrsError::State("Scan:Live monitors ended".into()))?;
        let table = self.seen.borrow_and_update().table()?;
        Ok(Live::Running(self, table))
    }
}

/// `b`'s changes applied to `seen`. A new monitor first sends the value
/// `b` holds; once that has come, every later change arrives as one.
async fn changes<T>(
    b: &EpicsCaBackend<T>,
    seen: watch::Sender<Seen>,
    apply: fn(&mut Seen, &T),
) -> BResult<SubToken>
where
    T: Clone + Send + Sync + 'static,
    EpicsCaBackend<T>: SignalBackend<T>,
{
    let (now_tx, now_rx) = oneshot::channel();
    let now = Mutex::new(Some(now_tx));
    let token = b.set_callback(Some(Box::new(move |v: &T, _, _| {
        match now.lock().unwrap().take() {
            Some(now) => {
                let _ = now.send(());
            }
            None => seen.send_modify(|s| apply(s, v)),
        }
    })));
    match tokio::time::timeout(CONNECT_TIMEOUT, now_rx).await {
        Ok(Ok(())) => Ok(token),
        Ok(Err(_)) => Err(BsrsError::State(format!(
            "{}: monitor ended",
            b.source("", true)
        ))),
        Err(_) => Err(BsrsError::Timeout(CONNECT_TIMEOUT)),
    }
}

/// mar345-ioc's cam1: what SPEC's macros do to it with `epics_put`.
pub struct MarCam {
    erase: Arc<EpicsCaBackend<bool>>,
    state: Arc<CaEnumBackend>,
    file_path: Arc<EpicsCaBackend<String>>,
    file_path_exists: Arc<EpicsCaBackend<bool>>,
    file_name: Arc<EpicsCaBackend<String>>,
    /// A bo: its put completes on the write, not with the frame.
    acquire: Arc<EpicsCaBackend<bool>>,
    /// 1 from the task taking the frame to its end, scanned or not.
    acquire_rbv: Arc<EpicsCaBackend<bool>>,
    /// Counts the frames that scanned.
    array_counter: Arc<EpicsCaBackend<i64>>,
    pub file_number: Arc<EpicsCaBackend<i64>>,
    pub full_file_name: Arc<EpicsCaBackend<String>>,
}

impl MarCam {
    async fn connect() -> Result<Arc<Self>> {
        let state_pv = format!("{CAM}DetectorState_RBV");
        let state = Arc::new(CaEnumBackend::new(&state_pv));
        state
            .connect(CONNECT_TIMEOUT)
            .await
            .map_err(|e| connect_err(&state_pv, e))?;
        Ok(Arc::new(Self {
            erase: backend(&format!("{CAM}Erase")).await?,
            state,
            file_path: backend(&format!("{CAM}FilePath")).await?,
            file_path_exists: backend(&format!("{CAM}FilePathExists_RBV")).await?,
            file_name: backend(&format!("{CAM}FileName")).await?,
            acquire: backend(&format!("{CAM}Acquire")).await?,
            acquire_rbv: backend(&format!("{CAM}Acquire_RBV")).await?,
            array_counter: backend(&format!("{CAM}ArrayCounter_RBV")).await?,
            file_number: backend(&format!("{CAM}FileNumber")).await?,
            full_file_name: backend(&format!("{CAM}FullFileName_RBV")).await?,
        }))
    }

    /// `epics_put("...cam1:Erase", 1)`, then until the detector is idle.
    pub async fn erase(&self) -> BResult<()> {
        self.erase.put(Some(true)).await?;
        loop {
            match self.state.get_value().await?.as_str() {
                "Idle" => return Ok(()),
                "Error" | "Disconnected" => {
                    return Err(BsrsError::State("mar345 erase: detector error".into()));
                }
                _ => tokio::time::sleep(POLL).await,
            }
        }
    }

    /// `_mar345fileinput`, in `dir`. ADCore takes a FilePath that does
    /// not exist and only says so in FilePathExists_RBV.
    pub async fn name_file(&self, dir: &Path, name: &str) -> BResult<()> {
        let text = dir.to_str().ok_or_else(|| {
            BsrsError::State(format!(
                "{}: not UTF-8, which FilePath takes",
                dir.display()
            ))
        })?;
        put(&self.file_path, text.to_string()).await?;
        if !get(&self.file_path_exists).await? {
            return Err(BsrsError::State(format!(
                "mar345-ioc does not see the data folder {text}"
            )));
        }
        put(&self.file_name, name.to_string()).await
    }

    /// `_mar345scanning`: Acquire, with AcquireTime 0 a readout alone,
    /// until the frame is over. A frame that did not scan is an error.
    pub async fn acquire(&self) -> BResult<()> {
        let frames = get(&self.array_counter).await?;
        put(&self.acquire, true).await?;
        // Acquire_RBV can still read the 0 from before the put; a frame
        // that came and went between two reads still counted.
        let take_by = tokio::time::Instant::now() + TAKE_TIMEOUT;
        let mut taken = false;
        loop {
            if get(&self.acquire_rbv).await? {
                taken = true;
            } else if taken || get(&self.array_counter).await? > frames {
                break;
            } else if tokio::time::Instant::now() >= take_by {
                return Err(BsrsError::State(
                    "mar345 Acquire: the detector did not take it".into(),
                ));
            }
            tokio::time::sleep(POLL).await;
        }
        if get(&self.array_counter).await? > frames {
            Ok(())
        } else {
            Err(BsrsError::State(
                "mar345 Acquire: no frame scanned (see mar345-ioc)".into(),
            ))
        }
    }
}

/// A readable whose reading a plan sets before each `Read`: the fly
/// scan's points and the plan's own event streams.
pub struct Holder {
    name: String,
    keys: Vec<(String, Dtype)>,
    values: Mutex<HashMap<String, Value>>,
}

impl Holder {
    pub fn new(name: &str, keys: &[(&str, Dtype)]) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            keys: keys.iter().map(|(k, d)| (k.to_string(), *d)).collect(),
            values: Mutex::new(HashMap::new()),
        })
    }

    pub fn set(&self, values: impl IntoIterator<Item = (String, Value)>) {
        *self.values.lock().unwrap() = values.into_iter().collect();
    }
}

impl NamedObj for Holder {
    fn name(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl ReadableObj for Holder {
    async fn read_dyn(&self) -> BResult<HashMap<String, ReadingValue>> {
        let now = now();
        let values = self.values.lock().unwrap();
        self.keys
            .iter()
            .map(|(k, _)| {
                values
                    .get(k)
                    .map(|v| (k.clone(), TypedReading::new(v.clone(), now)))
                    .ok_or_else(|| BsrsError::State(format!("{}: {k} not set", self.name)))
            })
            .collect()
    }
    async fn describe_dyn(&self) -> BResult<HashMap<String, DataKey>> {
        Ok(self
            .keys
            .iter()
            .map(|(k, d)| {
                let key = make_datakey(
                    self.name.as_str(),
                    *d,
                    vec![],
                    None,
                    SignalMetadata::default(),
                );
                (k.clone(), key)
            })
            .collect())
    }
}

/// Everything the plans use, connected.
#[derive(Clone)]
pub struct Devices {
    pub axes: HashMap<&'static str, Arc<Axis>>,
    pub scaler: Arc<Scaler>,
    pub cmd: Arc<SpecCmd>,
    pub cam: Arc<MarCam>,
}

impl Devices {
    pub async fn connect() -> Result<Self> {
        let mut axes = HashMap::new();
        for name in AXES {
            axes.insert(name, Axis::connect(name).await?);
        }
        Ok(Self {
            axes,
            scaler: Scaler::connect().await?,
            cmd: SpecCmd::connect().await?,
            cam: MarCam::connect().await?,
        })
    }

    pub fn axis(&self, name: &str) -> Arc<Axis> {
        self.axes
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} is not in AXES"))
    }

    /// The motors as positioners, for clients that list or read them.
    pub fn register(&self, reg: &mut Registry) {
        for (name, axis) in &self.axes {
            reg.register_positioner(*name, axis.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_is_rows_of_its_columns() {
        let t = Table::new("th\nsec\ndet", vec![1.0, 0.1, 5.0, 2.0, 0.1, 7.0]).unwrap();
        assert_eq!(t.cols, ["th", "sec", "det"]);
        assert_eq!(t.rows, [[1.0, 0.1, 5.0], [2.0, 0.1, 7.0]]);
    }

    #[test]
    fn a_scan_without_points_is_an_empty_table() {
        assert_eq!(Table::new("", vec![]).unwrap().rows.len(), 0);
        assert_eq!(Table::new("th\ndet", vec![]).unwrap().rows.len(), 0);
    }

    #[test]
    fn values_that_are_not_whole_rows_fail() {
        assert!(Table::new("th\ndet", vec![1.0, 2.0, 3.0]).is_err());
        assert!(Table::new("", vec![1.0]).is_err());
    }

    #[test]
    fn live_points_wait_for_their_names() {
        let seen = |cols: &str, data: &[f64]| Seen {
            cols: cols.into(),
            data: data.to_vec(),
        };
        assert_eq!(seen("", &[1.0, 2.0]).table().unwrap().rows.len(), 0);
        let t = seen("th\ndet", &[1.0, 2.0]).table().unwrap();
        assert_eq!(t.rows, [[1.0, 2.0]]);
        assert!(seen("th\ndet", &[1.0, 2.0, 3.0]).table().is_err());
    }
}
