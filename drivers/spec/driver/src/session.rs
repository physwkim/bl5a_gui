//! The connection to SPEC, made again whenever it ends.
//!
//! Every value the session keeps comes from an event or a read reply, both
//! applied in the order they arrive, so a read reply is newer than every
//! event before it. A stop (`move_done` 0, `count` 0) is therefore settled
//! by reading the final position or values after it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use epics_rs::asyn::error::AsynStatus;
use epics_rs::asyn::param::ParamValue;
use epics_rs::asyn::port_handle::PortHandle;
use epics_rs::asyn::request::ParamSetValue;
use epics_rs::base::runtime::sync::mpsc;
use spec_protocol::{Cmd, DataType, Framing, Packet, ReadError, read_packet, write_packet};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;

use crate::log::Tail;
use crate::points::{self, Cols, Scan, Shown};
use crate::port::{Params, StatusPort};
use crate::{AxisLive, Count, Link, Live, Motion, Op, Request, Shared, State};

/// Between connection attempts.
const RETRY: Duration = Duration::from_secs(2);

/// The global a `Cmd` command sets to 1 as its last statement. SPEC's
/// manual promises the reply's err only for unrecoverable errors, not for
/// a macro's `exit`; a command that stops there leaves the marker 0.
const MARK: &str = "SPEC_IOC_CMD_DONE";

/// Why a command whose reply carried no error failed.
const STOPPED: &str = "stopped before its end";

/// `text` between the marker's reset and its set, on lines of their own so
/// a `#` comment in `text` ends before the set.
fn marked(text: &str) -> String {
    format!("{MARK} = 0\n{text}\n{MARK} = 1")
}

/// A command SPEC replied to without an error, with `text`: whether the
/// read of the marker after it says the command ran to its end.
fn outcome(text: &str, marker: &Packet) -> Result<(), String> {
    if !failed(marker) && marker.number() == Some(1.0) {
        return Ok(());
    }
    let text = text.trim_end();
    Err(if text.is_empty() {
        STOPPED.to_string()
    } else {
        format!("{STOPPED}: {text}")
    })
}

type Reply = std::sync::mpsc::SyncSender<Result<String, String>>;

/// A command SPEC replied to without an error, through the reads after it.
struct Closing {
    reply: Reply,
    /// SPEC's reply to the command.
    text: String,
    step: Step,
}

/// The variable a closing command reads next.
enum Step {
    /// The marker; then a scan's points, or why one did not fit, are
    /// checked against what SPEC says the scan took.
    Marker(Option<Result<Scan, String>>),
    Rows(Scan),
    Cols(Scan),
}

impl Step {
    fn var(&self) -> &'static str {
        match self {
            Step::Marker(_) => MARK,
            Step::Rows(_) => "NPTS",
            Step::Cols(_) => "SCAN_COLS",
        }
    }
}

/// The variables a scan's points come from, watched for the connection.
const SCAN_COLS: &str = "var/SCAN_COLS";
const SCAN_PT: &str = "var/SCAN_PT";

/// What an event updates.
#[derive(Clone, Copy)]
enum Watch {
    Prop(Prop),
    /// The columns of the next scan's points.
    Cols,
    /// A scan's point.
    Point,
}

/// A SPEC property the session keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prop {
    Position(usize),
    MoveDone(usize),
    LowHit(usize),
    HighHit(usize),
    Slew(usize),
    Base(usize),
    Steps(usize),
    Sign(usize),
    Value(usize),
    Count,
    Ready,
    Tty,
    Error,
}

/// What a reply answers.
enum Pending {
    Hello,
    /// `init`: one of the reads the connection waits for before it is up.
    Read {
        prop: Prop,
        init: bool,
    },
    ConfigSlew(usize),
    /// A `ScanCmd` holds its points here, so they end with its reply.
    Command {
        reply: Reply,
        scan: Option<Result<Scan, String>>,
    },
    Closing(Closing),
    Barrier(Barrier),
}

#[derive(Clone, Copy)]
enum Barrier {
    Move(usize),
    Count,
    Set(usize),
}

/// What applying packets under the state lock leaves to do after it.
#[derive(Default)]
struct Effects {
    reads: Vec<Prop>,
    posts: Vec<Post>,
    wake: Vec<usize>,
    /// A count ended: deliver the done the transition set.
    done: bool,
    /// A command replied without an error: its next read.
    closing: Option<Closing>,
    /// SPEC's own moves broke the interlock: why, for the ^C sent first.
    abort: Option<String>,
}

enum Post {
    Rates(usize, Rates),
    Live(usize, f64),
    Busy(bool),
    Log(String),
    Scan(Shown),
}

impl Effects {
    fn log(&mut self, line: String) {
        self.posts.push(Post::Log(line));
    }

    fn rates(&mut self, axis: usize, a: &AxisLive) {
        self.posts.push(Post::Rates(axis, Rates::of(a)));
    }
}

/// An axis's VELO, VBAS and VMAX, in user units per second.
#[derive(Clone, Copy)]
struct Rates {
    velo: f64,
    vbas: f64,
    /// Unknown until `config_velocity` replies.
    vmax: Option<f64>,
}

impl Rates {
    fn of(a: &AxisLive) -> Rates {
        Rates {
            velo: a.slew / a.steps,
            vbas: a.base / a.steps,
            vmax: a.config_slew.map(|v| v / a.steps),
        }
    }

    fn updates(self, p: Params, axis: i32) -> Vec<ParamSetValue> {
        let f = |r, v| ParamSetValue::new(r, axis, ParamValue::Float64(v));
        let mut u = vec![f(p.velo, self.velo), f(p.vbas, self.vbas)];
        u.extend(self.vmax.map(|v| f(p.vmax, v)));
        u
    }
}

fn failed(p: &Packet) -> bool {
    p.header.err != 0 || p.header.data_type == Some(DataType::Error)
}

pub(crate) struct Session {
    shared: Arc<Shared>,
    port: PortHandle,
    params: Params,
    address: String,
    requests: mpsc::UnboundedReceiver<Request>,
    names: HashMap<String, Watch>,
    tail: Tail,
}

/// One connection's sending side and its outstanding requests.
struct Conn {
    epoch: u64,
    wr: OwnedWriteHalf,
    sn: u32,
    pending: HashMap<u32, Pending>,
    /// Initial reads not yet replied; the connection is up at zero.
    init_left: usize,
    /// `SCAN_COLS` as its events leave it.
    scan_cols: Cols,
}

impl Conn {
    async fn send(&mut self, mut p: Packet, pending: Option<Pending>) -> Result<(), String> {
        self.sn = self.sn.wrapping_add(1).max(1);
        p.header.sn = self.sn;
        if let Some(pending) = pending {
            self.pending.insert(self.sn, pending);
        }
        write_packet(&mut self.wr, &p, Framing::CLIENT)
            .await
            .map_err(|e| format!("write {}: {e}", p.header.name))
    }

    async fn read(&mut self, name: String, prop: Prop, init: bool) -> Result<(), String> {
        if init {
            self.init_left += 1;
        }
        let p = Packet::new(Cmd::ChanRead, 0, name);
        self.send(p, Some(Pending::Read { prop, init })).await
    }

    async fn barrier(&mut self, b: Barrier) -> Result<(), String> {
        let p = Packet::new(Cmd::CmdWithReturn, 0, "").with_string("");
        self.send(p, Some(Pending::Barrier(b))).await
    }

    async fn chan_send(&mut self, name: String, value: f64) -> Result<(), String> {
        let p = Packet::new(Cmd::ChanSend, 0, name).with_number(value);
        self.send(p, None).await
    }
}

impl Session {
    pub(crate) fn new(
        shared: Arc<Shared>,
        port: PortHandle,
        params: Params,
        address: String,
        requests: mpsc::UnboundedReceiver<Request>,
    ) -> Session {
        let mut session = Session {
            shared,
            port,
            params,
            address,
            requests,
            names: HashMap::new(),
            tail: Tail::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64),
            ),
        };
        let props = (0..session.shared.axes.len())
            .flat_map(|i| {
                [
                    Prop::Position(i),
                    Prop::MoveDone(i),
                    Prop::LowHit(i),
                    Prop::HighHit(i),
                    Prop::Slew(i),
                    Prop::Base(i),
                    Prop::Steps(i),
                    Prop::Sign(i),
                ]
            })
            .chain((0..session.shared.channels.len()).map(Prop::Value))
            .chain([Prop::Count, Prop::Ready, Prop::Tty, Prop::Error]);
        session.names = props
            .map(|p| (session.name(p), Watch::Prop(p)))
            .chain([
                (SCAN_COLS.into(), Watch::Cols),
                (SCAN_PT.into(), Watch::Point),
            ])
            .collect();
        session
    }

    fn name(&self, prop: Prop) -> String {
        let motor = |i: usize, p: &str| format!("motor/{}/{p}", self.shared.axes[i]);
        match prop {
            Prop::Position(i) => motor(i, "position"),
            Prop::MoveDone(i) => motor(i, "move_done"),
            Prop::LowHit(i) => motor(i, "low_lim_hit"),
            Prop::HighHit(i) => motor(i, "high_lim_hit"),
            Prop::Slew(i) => motor(i, "slew_rate"),
            Prop::Base(i) => motor(i, "base_rate"),
            Prop::Steps(i) => motor(i, "step_size"),
            Prop::Sign(i) => motor(i, "sign"),
            Prop::Value(ch) => format!("scaler/{}/value", self.shared.channels[ch]),
            Prop::Count => "scaler/.all./count".into(),
            Prop::Ready => "status/ready".into(),
            Prop::Tty => "output/tty".into(),
            Prop::Error => "error".into(),
        }
    }

    /// Runs on a thread of its own for the life of the IOC.
    pub(crate) fn spawn(self) -> std::io::Result<std::thread::JoinHandle<()>> {
        std::thread::Builder::new()
            .name(format!("spec {}", self.address))
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a current-thread runtime builds");
                rt.block_on(self.run());
            })
    }

    async fn run(mut self) {
        self.post_link(None).await;
        let mut epoch = 0;
        let mut last_failure = None;
        loop {
            epoch += 1;
            match TcpStream::connect(&self.address).await {
                Ok(sock) => {
                    last_failure = None;
                    let _ = sock.set_nodelay(true);
                    self.log(format!("connected to SPEC at {}", self.address))
                        .await;
                    let why = self.connection(sock, epoch).await;
                    self.mark_down().await;
                    match why {
                        Some(why) => self.log(format!("SPEC connection ended: {why}")).await,
                        None => return,
                    }
                }
                Err(e) => {
                    let why = e.to_string();
                    if last_failure.as_ref() != Some(&why) {
                        self.log(format!("cannot connect to SPEC at {}: {why}", self.address))
                            .await;
                        last_failure = Some(why);
                    }
                }
            }
            let retry = tokio::time::sleep(RETRY);
            tokio::pin!(retry);
            loop {
                tokio::select! {
                    _ = &mut retry => break,
                    r = self.requests.recv() => match r {
                        None => return,
                        Some(Request::Log(line)) => self.log(line).await,
                        // Taken on a connection that has ended.
                        Some(Request::Op { .. }) => {}
                    },
                }
            }
        }
    }

    /// Why the connection ended; `None` when the IOC is going away.
    async fn connection(&mut self, sock: TcpStream, epoch: u64) -> Option<String> {
        let (mut rd, wr) = sock.into_split();
        // read_packet is not cancel-safe, so it runs where nothing cancels it.
        let (tx, mut packets) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            loop {
                let r = read_packet(&mut rd).await.map(|(p, _)| p);
                let fatal = r.as_ref().is_err_and(ReadError::is_fatal);
                if tx.send(r).is_err() || fatal {
                    break;
                }
            }
        });
        let mut conn = Conn {
            epoch,
            wr,
            sn: 0,
            pending: HashMap::new(),
            init_left: 0,
            scan_cols: Cols::default(),
        };
        let why = match self.start(&mut conn).await {
            Err(why) => Some(why),
            Ok(()) => loop {
                let step = tokio::select! {
                    p = packets.recv() => match p {
                        Some(Ok(p)) => self.packet(&mut conn, p).await,
                        Some(Err(e)) if e.is_fatal() => Err(e.to_string()),
                        Some(Err(e)) => {
                            self.log(format!("skipped a SPEC packet: {e}")).await;
                            Ok(())
                        }
                        None => Err("the reader ended".into()),
                    },
                    r = self.requests.recv() => match r {
                        None => break None,
                        Some(Request::Log(line)) => {
                            self.log(line).await;
                            Ok(())
                        }
                        Some(Request::Op { epoch, op }) if epoch == conn.epoch => {
                            self.op(&mut conn, op).await
                        }
                        Some(Request::Op { .. }) => Ok(()),
                    },
                };
                if let Err(why) = step {
                    break Some(why);
                }
            },
        };
        reader.abort();
        why
    }

    async fn start(&mut self, conn: &mut Conn) -> Result<(), String> {
        let n = self.shared.axes.len();
        self.shared.lock().link = Link::Connecting(Live::new(conn.epoch, n));
        conn.send(Packet::new(Cmd::Hello, 0, ""), Some(Pending::Hello))
            .await?;
        let mut watched: Vec<Prop> = (0..n)
            .flat_map(|i| {
                [
                    Prop::Position(i),
                    Prop::MoveDone(i),
                    Prop::LowHit(i),
                    Prop::HighHit(i),
                    Prop::Slew(i),
                    Prop::Base(i),
                ]
            })
            .collect();
        let values = (0..self.shared.channels.len()).map(Prop::Value);
        let state = [Prop::Count, Prop::Ready];
        for prop in watched
            .iter()
            .copied()
            .chain(values)
            .chain(state)
            .chain([Prop::Tty, Prop::Error])
        {
            conn.send(Packet::new(Cmd::Register, 0, self.name(prop)), None)
                .await?;
        }
        // Their first events are the state to start from: the columns, and
        // a point no command of this connection waits for.
        for name in [SCAN_COLS, SCAN_PT] {
            conn.send(Packet::new(Cmd::Register, 0, name), None).await?;
        }
        // Registering sends a first event, but SPEC does not promise one
        // for every property, so the state the connection starts from is
        // read.
        watched.extend((0..n).flat_map(|i| [Prop::Steps(i), Prop::Sign(i)]));
        for prop in watched.into_iter().chain(state) {
            conn.read(self.name(prop), prop, true).await?;
        }
        Ok(())
    }

    async fn packet(&mut self, conn: &mut Conn, p: Packet) -> Result<(), String> {
        let mut fx = Effects::default();
        match p.header.cmd {
            Cmd::Event => match self.names.get(&p.header.name) {
                Some(&Watch::Prop(prop)) => self.apply(&mut fx, prop, &p, false),
                Some(Watch::Cols) => {
                    if let Err(why) = conn.scan_cols.apply(&p) {
                        fx.log(why);
                    }
                }
                Some(Watch::Point) => point(conn, &mut fx, &p),
                None => return Ok(()),
            },
            Cmd::Reply | Cmd::HelloReply => {
                let Some(pending) = conn.pending.remove(&p.header.sn) else {
                    return Ok(());
                };
                self.reply(conn, &mut fx, pending, &p)?;
            }
            _ => return Ok(()),
        }
        self.flush(conn, fx).await?;
        if conn.init_left == 0 && matches!(self.shared.lock().link, Link::Connecting(_)) {
            self.up(conn).await?;
        }
        Ok(())
    }

    fn reply(
        &mut self,
        conn: &mut Conn,
        fx: &mut Effects,
        pending: Pending,
        p: &Packet,
    ) -> Result<(), String> {
        match pending {
            Pending::Hello => fx.log(format!("SPEC is {:?}", p.text())),
            Pending::Read { prop, init } => {
                if init {
                    conn.init_left -= 1;
                }
                if failed(p) {
                    let why = format!("reading {}: {}", self.name(prop), p.text());
                    if init {
                        return Err(why);
                    }
                    fx.log(why);
                } else {
                    self.apply(fx, prop, p, true);
                }
            }
            Pending::ConfigSlew(axis) => {
                let mut st = self.shared.lock();
                let Some(live) = live(&mut st) else {
                    return Ok(());
                };
                live.queued = live.queued.saturating_sub(1);
                match p.number().filter(|_| !failed(p)) {
                    Some(v) => {
                        let a = &mut live.axes[axis];
                        a.config_slew = Some(v);
                        fx.rates(axis, a);
                    }
                    None => fx.log(format!(
                        "{} config_velocity: {}",
                        self.shared.axes[axis],
                        p.text()
                    )),
                }
                drain(live, fx);
            }
            Pending::Command { reply, scan } => {
                let text = p.text().into_owned();
                if failed(p) {
                    self.command_ended(fx, reply, Err(text));
                } else {
                    let step = Step::Marker(scan);
                    fx.closing = Some(Closing { reply, text, step });
                }
            }
            Pending::Closing(c) => self.closing(fx, c, p),
            Pending::Barrier(b) => {
                let mut st = self.shared.lock();
                let Some(live) = live(&mut st) else {
                    return Ok(());
                };
                live.queued = live.queued.saturating_sub(1);
                let what = match b {
                    // A move or count that never started: settle it now.
                    Barrier::Move(i) => {
                        if matches!(live.axes[i].motion, Motion::Starting { .. }) {
                            fx.reads.push(Prop::MoveDone(i));
                        }
                        format!("{} move", self.shared.axes[i])
                    }
                    Barrier::Count => {
                        if live.count == Count::Starting {
                            fx.reads.push(Prop::Count);
                        }
                        "count".to_string()
                    }
                    Barrier::Set(i) => format!("{} set", self.shared.axes[i]),
                };
                if failed(p) {
                    fx.log(format!("{what}: {}", p.text()));
                }
                drain(live, fx);
            }
        }
        Ok(())
    }

    /// `p`, an event or a read reply of `prop`, into the state.
    fn apply(&self, fx: &mut Effects, prop: Prop, p: &Packet, read: bool) {
        let mut st = self.shared.lock();
        let up = matches!(st.link, Link::Up(_));
        let State { link, counts, .. } = &mut *st;
        let (Link::Connecting(live) | Link::Up(live)) = link else {
            return;
        };
        let number = p.number();
        let value =
            || number.ok_or_else(|| format!("{}: not a number: {:?}", self.name(prop), p.text()));
        let fell = match prop {
            Prop::Position(i) => number.is_some_and(|v| v < live.axes[i].position),
            _ => false,
        };
        let result = match prop {
            Prop::Position(i) => value().map(|v| {
                let a = &mut live.axes[i];
                a.position = v;
                if read && a.motion == Motion::Settling {
                    a.motion = Motion::Idle;
                }
                fx.wake.push(i);
            }),
            Prop::MoveDone(i) => value().map(|v| {
                let a = &mut live.axes[i];
                if v != 0.0 {
                    a.motion = Motion::Moving {
                        target: a.motion.target(),
                    };
                    fx.wake.push(i);
                } else if a.motion.moving() {
                    a.motion = Motion::Settling;
                    fx.reads.push(Prop::Position(i));
                }
            }),
            Prop::LowHit(i) | Prop::HighHit(i) => value().map(|v| {
                let a = &mut live.axes[i];
                match prop {
                    Prop::LowHit(_) => a.low_hit = v != 0.0,
                    _ => a.high_hit = v != 0.0,
                }
                fx.wake.push(i);
            }),
            Prop::Slew(i) | Prop::Base(i) => value().map(|v| {
                let a = &mut live.axes[i];
                match prop {
                    Prop::Slew(_) => a.slew = v,
                    _ => a.base = v,
                }
                if up {
                    fx.rates(i, a);
                }
            }),
            Prop::Steps(i) => value().map(|v| {
                live.axes[i].steps = if v == 0.0 {
                    fx.log(format!("{}: step_size is 0; using 1", self.shared.axes[i]));
                    1.0
                } else {
                    v.abs()
                };
            }),
            Prop::Sign(i) => value().map(|v| live.axes[i].sign = v),
            Prop::Value(ch) => value().map(|v| {
                counts[ch] = v;
                fx.posts.push(Post::Live(ch, v));
                if let (true, Count::Reading { left }) = (read, &mut live.count) {
                    *left = left.saturating_sub(1);
                    if *left == 0 {
                        self.count_ended(live, fx);
                    }
                }
            }),
            Prop::Count => value().map(|v| {
                if v != 0.0 {
                    live.count = Count::Running;
                } else if matches!(live.count, Count::Starting | Count::Running) {
                    let left = self.shared.channels.len();
                    live.count = Count::Reading { left };
                    fx.reads.extend((0..left).map(Prop::Value));
                    if left == 0 {
                        self.count_ended(live, fx);
                    }
                }
            }),
            // The event is 1 at the prompt; a read is 1 while busy.
            Prop::Ready => value().map(|v| {
                let ready = (v != 0.0) != read;
                if live.ready != ready {
                    live.ready = ready;
                    fx.posts.push(Post::Busy(!ready));
                }
            }),
            Prop::Tty => {
                for line in p.text().lines().map(str::trim_end) {
                    if !line.is_empty() {
                        fx.log(line.to_string());
                    }
                }
                Ok(())
            }
            Prop::Error => {
                let text = p.text();
                if !text.is_empty() && text != "No error" {
                    fx.log(format!("SPEC error: {text}"));
                }
                Ok(())
            }
        };
        if let Err(why) = result {
            fx.log(why);
        }
        if let (true, Some(lock), Prop::Position(axis) | Prop::MoveDone(axis)) =
            (up, &self.shared.interlock, prop)
            && let Some(why) =
                lock.watch(&self.shared.axes, &live.axes, &mut live.tripped, axis, fell)
        {
            fx.abort = Some(why);
        }
    }

    /// `p`, the read `c` waited for: the command's result, or the next read.
    fn closing(&self, fx: &mut Effects, c: Closing, p: &Packet) {
        let Closing { reply, text, step } = c;
        let next = match step {
            Step::Marker(scan) => outcome(&text, p).and_then(|()| match scan {
                None => Ok(None),
                Some(scan) => scan.map(|s| Some(Step::Rows(s))),
            }),
            _ if failed(p) => Err(format!("reading {}: {}", step.var(), p.text())),
            Step::Rows(scan) => match points::rows(p).and_then(|n| scan.took(n)) {
                Ok(()) if scan.rows() > 0 => Ok(Some(Step::Cols(scan))),
                r => r.map(|()| None),
            },
            Step::Cols(scan) => points::cols(p).and_then(|c| scan.named(&c)).map(|()| None),
        };
        match next {
            Ok(Some(step)) => fx.closing = Some(Closing { reply, text, step }),
            Ok(None) => self.command_ended(fx, reply, Ok(text)),
            Err(why) => self.command_ended(fx, reply, Err(why)),
        }
    }

    /// SPEC free for the next command once this one's `result` is known.
    fn command_ended(&self, fx: &mut Effects, reply: Reply, result: Result<String, String>) {
        let mut st = self.shared.lock();
        let Some(live) = live(&mut st) else {
            return;
        };
        live.command = false;
        // The port waiting on it may have gone with the IOC.
        let _ = reply.send(result);
        drain(live, fx);
    }

    /// Under the state lock, so a count started after it is not ended by it.
    fn count_ended(&self, live: &mut Live, fx: &mut Effects) {
        live.count = Count::Idle;
        self.shared.done.set(1);
        fx.done = true;
    }

    async fn up(&mut self, conn: &mut Conn) -> Result<(), String> {
        let n = self.shared.axes.len();
        let rates = {
            let mut st = self.shared.lock();
            let link = std::mem::replace(&mut st.link, Link::Down);
            let Link::Connecting(mut live) = link else {
                st.link = link;
                return Ok(());
            };
            let rates = live.axes.iter().map(Rates::of).collect();
            // The config_velocity calls are queued like a move.
            live.queued += n;
            st.link = Link::Up(live);
            rates
        };
        self.post_link(Some(rates)).await;
        self.shared.wake_all();
        for (i, mne) in self.shared.axes.iter().enumerate() {
            let args = format!("motor_par\0{mne}\0config_velocity");
            let func = Packet::new(Cmd::FuncWithReturn, 0, "").with_string(&args);
            conn.send(func, Some(Pending::ConfigSlew(i))).await?;
        }
        self.log("SPEC is up".into()).await;
        Ok(())
    }

    async fn op(&mut self, conn: &mut Conn, op: Op) -> Result<(), String> {
        let motor = |i: usize, p: &str| format!("motor/{}/{p}", self.shared.axes[i]);
        match op {
            Op::Move {
                axis,
                user,
                slew,
                base,
            } => {
                if let Some(v) = slew {
                    conn.chan_send(motor(axis, "slew_rate"), v).await?;
                }
                if let Some(v) = base {
                    conn.chan_send(motor(axis, "base_rate"), v).await?;
                }
                conn.chan_send(motor(axis, "start_one"), user).await?;
                conn.barrier(Barrier::Move(axis)).await
            }
            Op::Stop => conn.chan_send("motor/../abort_all".into(), 1.0).await,
            Op::SetPosition { axis, user } => {
                let text = format!(
                    "set {} {}",
                    self.shared.axes[axis],
                    spec_protocol::format_number(user)
                );
                let p = Packet::new(Cmd::CmdWithReturn, 0, "").with_string(&text);
                conn.send(p, Some(Pending::Barrier(Barrier::Set(axis))))
                    .await
            }
            Op::Count { value } => {
                conn.chan_send("scaler/.all./count".into(), value).await?;
                conn.barrier(Barrier::Count).await
            }
            Op::StopCount => conn.chan_send("scaler/.all./count".into(), 0.0).await,
            Op::Command { text, scan, reply } => {
                // Cleared before SPEC can send the first point.
                let scan = if scan {
                    self.show_scan(Shown::empty()).await;
                    Some(Ok(Scan::default()))
                } else {
                    None
                };
                let p = Packet::new(Cmd::CmdWithReturn, 0, "").with_string(&marked(&text));
                conn.send(p, Some(Pending::Command { reply, scan })).await
            }
            Op::Abort => conn.send(Packet::new(Cmd::Abort, 0, ""), None).await,
        }
    }

    async fn flush(&mut self, conn: &mut Conn, fx: Effects) -> Result<(), String> {
        if let Some(why) = fx.abort {
            conn.send(Packet::new(Cmd::Abort, 0, ""), None).await?;
            self.log(format!("interlock: {why}; sent ^C")).await;
        }
        if fx.done {
            self.shared.done.call_callbacks();
        }
        for axis in fx.wake {
            self.shared.wake(axis);
        }
        for post in fx.posts {
            self.post(post).await;
        }
        for prop in fx.reads {
            conn.read(self.name(prop), prop, false).await?;
        }
        if let Some(c) = fx.closing {
            let p = Packet::new(Cmd::ChanRead, 0, format!("var/{}", c.step.var()));
            conn.send(p, Some(Pending::Closing(c))).await?;
        }
        Ok(())
    }

    async fn post(&mut self, post: Post) {
        let p = self.params;
        let (addr, updates) = match post {
            Post::Rates(axis, rates) => (axis as i32, rates.updates(p, axis as i32)),
            Post::Live(ch, v) => {
                let u = vec![ParamSetValue::new(
                    p.live,
                    ch as i32,
                    ParamValue::Float64(v),
                )];
                let _ = self
                    .port
                    .set_time_stamp_params_and_notify(ch as i32, SystemTime::now(), u)
                    .await;
                return;
            }
            Post::Busy(b) => (
                0,
                vec![ParamSetValue::new(
                    p.spec_busy,
                    0,
                    ParamValue::Int32(i32::from(b)),
                )],
            ),
            Post::Log(line) => return self.log(line).await,
            Post::Scan(shown) => return self.show_scan(shown).await,
        };
        let _ = self.port.set_params_and_notify(addr, updates).await;
    }

    /// `shown` on the `Scan:` records' parameters before this returns, so
    /// a command's reply, sent after, finds every point there.
    async fn show_scan(&mut self, shown: Shown) {
        let shown = self
            .port
            .with_driver::<StatusPort, _>(move |d| d.show_scan(shown))
            .await;
        if let Err(e) = shown.and_then(|r| r) {
            self.log(format!("showing the scan: {e}")).await;
        }
    }

    /// One line of SPEC's log, posted as the whole tail.
    async fn log(&mut self, line: String) {
        epics_rs::base::stderr_println!("spec {}: {line}", self.address);
        let tail = self.tail.push(&line);
        let updates = vec![octet(self.params.log, &tail)];
        let _ = self.port.set_params_and_notify(0, updates).await;
    }

    /// The link at every address: the records of connected values in
    /// alarm while down, or out of it with the axes' `rates` once up.
    async fn post_link(&self, rates: Option<Vec<Rates>>) {
        let p = self.params;
        let status = match rates {
            Some(_) => AsynStatus::Success,
            None => AsynStatus::Disconnected,
        };
        let (axes, channels) = (self.shared.axes.len(), self.shared.channels.len());
        for addr in 0..axes.max(channels).max(1) {
            let a = addr as i32;
            let mut u = Vec::new();
            if addr < axes {
                // Values first: a record in alarm takes the value it leaves with.
                if let Some(r) = rates.as_ref() {
                    u.extend(r[addr].updates(p, a));
                }
                u.extend([p.velo, p.vbas, p.vmax].map(|r| ParamSetValue::status(r, a, status)));
            }
            if addr < channels {
                u.push(ParamSetValue::status(p.live, a, status));
            }
            if addr == 0 {
                u.push(ParamSetValue::status(p.spec_busy, 0, status));
                let up = i32::from(rates.is_some());
                u.push(ParamSetValue::new(p.connected, 0, ParamValue::Int32(up)));
            }
            let _ = self.port.set_params_and_notify(a, u).await;
        }
    }

    /// The finalizer of every connection, however it ended.
    async fn mark_down(&mut self) {
        {
            let mut st = self.shared.lock();
            st.link = Link::Down;
            // Ends the record's count; a no-op when none of ours ran.
            self.shared.done.set(1);
        }
        self.shared.done.call_callbacks();
        self.shared.wake_all();
        self.post_link(None).await;
    }
}

/// A `SCAN_PT` event: the next point of the `ScanCmd` waiting for its
/// reply. Without one it is the registration's event or another client's
/// scan; a point that does not fit ends the scan's points with why.
fn point(conn: &mut Conn, fx: &mut Effects, p: &Packet) {
    let Conn {
        pending, scan_cols, ..
    } = conn;
    let Some(scan) = pending.values_mut().find_map(|pending| match pending {
        Pending::Command {
            scan: Some(scan), ..
        } => Some(scan),
        _ => None,
    }) else {
        return;
    };
    let Ok(s) = scan else {
        return;
    };
    match points::point(&p.text()).and_then(|(pno, row)| s.add(pno, row, scan_cols)) {
        Ok(shown) => fx.posts.push(Post::Scan(shown)),
        Err(why) => {
            let why = format!("scan points: {why}");
            fx.log(why.clone());
            *scan = Err(why);
        }
    }
}

fn live(st: &mut State) -> Option<&mut Live> {
    match &mut st.link {
        Link::Connecting(live) | Link::Up(live) => Some(live),
        Link::Down => None,
    }
}

/// Once nothing of ours is queued, SPEC is back at its prompt unless
/// someone else's command runs; a read confirms it, as `status/ready`'s
/// own event may trail the reply.
fn drain(live: &mut Live, fx: &mut Effects) {
    if live.queued == 0 && !live.command && !live.ready {
        live.ready = true;
        fx.posts.push(Post::Busy(false));
        fx.reads.push(Prop::Ready);
    }
}

fn octet(reason: usize, s: &str) -> ParamSetValue {
    ParamSetValue::new(reason, 0, ParamValue::Octet(s.as_bytes().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(err: i32, text: &str) -> Packet {
        let mut p = Packet::new(Cmd::Reply, 1, "").with_string(text);
        p.header.err = err;
        p
    }

    #[test]
    fn the_marker_is_set_on_a_line_after_the_command() {
        assert_eq!(
            marked("wa # where"),
            "SPEC_IOC_CMD_DONE = 0\nwa # where\nSPEC_IOC_CMD_DONE = 1"
        );
    }

    #[test]
    fn a_reply_with_err_set_is_a_failure() {
        assert!(failed(&reply(1, "syntax error")));
        assert!(failed(&Packet::new(Cmd::Reply, 1, "").with_error("^C")));
        assert!(!failed(&reply(0, "4")));
    }

    #[test]
    fn a_set_marker_passes() {
        assert_eq!(outcome("4", &reply(0, "1")), Ok(()));
    }

    #[test]
    fn an_unset_marker_fails_with_the_reply() {
        assert_eq!(
            outcome("too low\n", &reply(0, "0")),
            Err("stopped before its end: too low".into())
        );
        assert_eq!(outcome("", &reply(0, "0")), Err(STOPPED.into()));
    }

    #[test]
    fn a_failed_marker_read_fails() {
        let p = Packet::new(Cmd::Reply, 1, "").with_error("no such variable");
        assert_eq!(outcome("4", &p), Err("stopped before its end: 4".into()));
        assert_eq!(
            outcome("4", &reply(1, "1")),
            Err("stopped before its end: 4".into())
        );
    }
}
