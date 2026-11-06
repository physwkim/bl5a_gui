//! The one owner of the beamline state. Every access goes through
//! [`Shared::lock`], which first advances motors and counters to now and,
//! when the guard drops, sends each subscriber the properties that changed.
//! So no reader sees a stale position and no change goes unpublished.

use std::collections::{BTreeMap, HashMap};
use std::ops::{Deref, DerefMut};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use spec_protocol::{Cmd, DataType, FLAG_DELETED, Packet, format_number};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::beam::{self, Geometry, Optics, Rng, Sample, Truth};
use crate::config::Config;
use crate::lakeshore::Lakeshore;
use crate::motor::Motor;

/// Motion and counting are integrated, and changes published, this often.
pub const TICK: Duration = Duration::from_millis(20);
/// `2^32` ticks of the 10 MHz timer (`sec` scale 1e7).
pub const MAX_TIME_PRESET: f64 = 429.496_729_6;
const DARK: f64 = 0.5;
/// `mA[0]` .. `mA[_numgeo - 1]` of the kappa geometry: a scan of one of
/// these gets H, K and L columns.
const GEOMETRY: [&str; 4] = ["tth", "th", "chi", "phi"];

/// Motors and counters the beam model reads, by config index.
#[derive(Clone, Copy, Debug)]
struct Index {
    tth: usize,
    th: usize,
    nu: usize,
    sy: usize,
    shold: usize,
    hs4: [usize; 4],
    hs5: [usize; 4],
    sec: usize,
    icxrd: usize,
    det: usize,
    ic1: usize,
}

impl Index {
    fn find(cfg: &Config) -> Result<Index, String> {
        let m = |mne: &str| cfg.motor(mne).ok_or(format!("config has no motor {mne}"));
        let c = |mne: &str| {
            cfg.counter(mne)
                .ok_or(format!("config has no counter {mne}"))
        };
        Ok(Index {
            tth: m("tth")?,
            th: m("th")?,
            nu: m("nu")?,
            sy: m("sy")?,
            shold: m("shold")?,
            hs4: [m("hs4u")?, m("hs4d")?, m("hs4h")?, m("hs4r")?],
            hs5: [m("hs5u")?, m("hs5d")?, m("hs5h")?, m("hs5r")?],
            sec: c("sec")?,
            icxrd: c("icxrd")?,
            det: c("det")?,
            ic1: c("ic1")?,
        })
    }
}

/// What the mar345 plate has collected since it was last erased.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dose {
    /// Photons onto the sample with the mar shutter open.
    pub photons: f64,
    /// Seconds the mar shutter was open.
    pub open: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Preset {
    Time(f64),
    Monitor { channel: usize, target: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MotorProp {
    /// Sent only: queues a move of this motor.
    StartOne,
    Position,
    DialPosition,
    Offset,
    StepSize,
    Sign,
    MoveDone,
    HighLimHit,
    LowLimHit,
    EmergencyStop,
    MotorFault,
    Unusable,
    HighLimit,
    LowLimit,
    SlewRate,
    BaseRate,
    Acceleration,
    Backlash,
}

impl MotorProp {
    fn parse(s: &str) -> Option<MotorProp> {
        use MotorProp::*;
        Some(match s {
            "start_one" => StartOne,
            "position" => Position,
            "dial_position" => DialPosition,
            "offset" => Offset,
            "step_size" => StepSize,
            "sign" => Sign,
            "move_done" => MoveDone,
            "high_lim_hit" => HighLimHit,
            "low_lim_hit" => LowLimHit,
            "emergency_stop" => EmergencyStop,
            "motor_fault" => MotorFault,
            "unusable" => Unusable,
            "high_limit" => HighLimit,
            "low_limit" => LowLimit,
            "slew_rate" => SlewRate,
            "base_rate" => BaseRate,
            "acceleration" => Acceleration,
            "backlash" => Backlash,
            _ => return None,
        })
    }

    /// The `motor_par` parameter this property stands for.
    pub fn par(self) -> Option<&'static str> {
        Some(match self {
            MotorProp::SlewRate => "slew_rate",
            MotorProp::BaseRate => "base_rate",
            MotorProp::Acceleration => "acceleration",
            MotorProp::Backlash => "backlash",
            _ => return None,
        })
    }
}

/// A property name, resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    Motor(usize, MotorProp),
    /// `motor/../prestart_all` and friends: sent, never read.
    MotorAll(String),
    ScalerValue(usize),
    Count,
    Ready,
    /// `status/shell`, `status/simulate`, `status/quit`: always 0 here.
    StatusZero,
    Var(String),
    /// `var/SCAN_D`, a data array.
    ScanD,
    /// `var/SCAN_COLS`, an associative array.
    ScanCols,
    Output(String),
    Error,
}

/// What a read returns and an event carries.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    /// Row by row.
    Doubles {
        rows: usize,
        cols: usize,
        data: Vec<f64>,
    },
    Assoc(Vec<(String, String)>),
}

impl Value {
    /// `p` carrying this value.
    pub fn fill(&self, mut p: Packet) -> Packet {
        match self {
            Value::Text(s) => p.with_string(s),
            Value::Assoc(pairs) => p.with_assoc(pairs.iter().map(|(k, v)| (&k[..], &v[..]))),
            Value::Doubles { rows, cols, data } => {
                p.header.data_type = Some(DataType::ArrDouble);
                p.header.rows = *rows as u32;
                p.header.cols = *cols as u32;
                p.data = data.iter().flat_map(|x| x.to_ne_bytes()).collect();
                p
            }
        }
    }
}

/// `SCAN_COLS` and `shared array SCAN_D` as `scan_data` leaves them; rows
/// past `NPTS` keep earlier scans' points, as SPEC's do.
#[derive(Default)]
pub struct ScanD {
    cols: Vec<String>,
    rows: usize,
    data: Vec<f64>,
}

impl ScanD {
    /// `scan_data_init`: the columns, and a new 4096-row array when their
    /// count changed.
    fn init(&mut self, cols: Vec<String>) {
        if self.rows == 0 || cols.len() != self.cols.len() {
            self.rows = 4096;
            self.data = vec![0.0; self.rows * cols.len()];
        }
        self.cols = cols;
    }

    /// `SCAN_D[pno] = row`, first growing by 1024 rows as
    /// `resize_scan_data` does; the row count it grew to, if it did.
    fn put(&mut self, pno: usize, row: &[f64]) -> Option<usize> {
        let grew = (pno >= self.rows).then(|| {
            self.rows += 1024;
            self.data.resize(self.rows * self.cols.len(), 0.0);
            self.rows
        });
        let n = self.cols.len();
        self.data[pno * n..(pno + 1) * n].copy_from_slice(row);
        grew
    }

    /// `SCAN_COLS`; `None` before the first scan, when SPEC has none.
    fn cols_value(&self) -> Option<Value> {
        (self.rows > 0).then(|| {
            Value::Assoc(
                self.cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (i.to_string(), c.clone()))
                    .collect(),
            )
        })
    }

    /// `SCAN_D`; `None` before the first scan, when SPEC has none.
    fn data_value(&self) -> Option<Value> {
        (self.rows > 0).then(|| Value::Doubles {
            rows: self.rows,
            cols: self.cols.len(),
            data: self.data.clone(),
        })
    }
}

impl Prop {
    /// Published after the values of the same update, so a client that sees
    /// a motor stop or a count end already has the final readings.
    fn is_flag(&self) -> bool {
        matches!(
            self,
            Prop::Motor(_, MotorProp::MoveDone) | Prop::Count | Prop::Ready
        )
    }
}

struct Watch {
    name: String,
    prop: Prop,
    sn: u32,
    last: Option<Value>,
}

struct Sub {
    tx: mpsc::UnboundedSender<Packet>,
    watches: Vec<Watch>,
}

pub struct Start {
    pub sample: Sample,
    pub seed: u64,
    /// Dial-unit software limits per motor; the rest get ±10000.
    pub limits: HashMap<String, (f64, f64)>,
    pub switches: HashMap<String, (f64, f64)>,
    /// User positions at start; offsets start at zero.
    pub positions: HashMap<String, f64>,
}

impl Start {
    /// The beamline at rest after the last session: detector out, slits and
    /// chi as align_in leaves them, tth at the direct beam.
    pub fn beamline(sample: Sample, seed: u64) -> Start {
        let positions = [
            ("chi", 89.7),
            ("martran", 1234.0),
            ("marsdd", 835.0),
            ("marz", 100.0),
            ("marshut", 0.0),
            ("marx", 50.0),
            ("ipbsx", 1.2),
            ("ipbsy", -6.8),
            ("scell", 6.0),
            ("tth", 1.0),
            ("hs4u", 0.5),
            ("hs4d", -0.5),
            ("hs4h", 3.0),
            ("hs4r", -3.0),
            ("hs5u", 0.25),
            ("hs5d", -0.25),
            ("hs5h", 0.5),
            ("hs5r", -0.5),
        ]
        .into_iter()
        .map(|(m, v)| (m.to_string(), v))
        .collect();
        Start {
            sample,
            seed,
            limits: HashMap::new(),
            switches: HashMap::new(),
            positions,
        }
    }
}

pub struct World {
    pub cfg: Config,
    pub motors: Vec<Motor>,
    /// SPEC's `A[]`, user units.
    pub a: Vec<f64>,
    /// User positions at the last `get_angles`; `move_em` starts the motors
    /// whose `A[]` differs from these.
    angles: Vec<f64>,
    /// SPEC's `S[]`.
    pub s: Vec<f64>,
    count: Option<Preset>,
    pub optics: Optics,
    pub sample: Sample,
    pub truth: Truth,
    /// The LakeShore 331 on ser 9; its input A is the sample's temperature.
    pub tc: Lakeshore,
    /// `qdo` of temperature.mac has run: `settemp` is that file's, and
    /// `set_ramp`, `read_ramp` and `changec` are defined.
    pub temperature_mac: bool,
    /// SPEC globals the modelled macros use, as SPEC prints them.
    pub vars: BTreeMap<String, String>,
    pub scan: ScanD,
    /// The command thread is running a queued command.
    pub busy: bool,
    dose: Dose,
    rng: Rng,
    epoch: Instant,
    now: Instant,
    index: Index,
    subs: HashMap<u64, Sub>,
}

impl World {
    pub fn new(cfg: Config, start: &Start) -> Result<World, String> {
        let index = Index::find(&cfg)?;
        let now = Instant::now();
        let mut motors = Vec::new();
        for m in &cfg.motors {
            let limits = start.limits.get(&m.mne).copied().unwrap_or((-1e4, 1e4));
            let user = start.positions.get(&m.mne).copied().unwrap_or(0.0);
            let mut motor = Motor::new(m, user * m.sign, limits);
            motor.switches = start.switches.get(&m.mne).copied();
            motors.push(motor);
        }
        let a: Vec<f64> = motors.iter().map(Motor::user).collect();
        let mut rng = Rng::new(start.seed);
        let truth = Truth::draw(&mut rng);
        let vars = [
            ("COUNT", "1"),
            ("MON", "2"),
            ("DET", "3"),
            ("COUNTERS", &cfg.counters.len().to_string()[..]),
            ("NPTS", "0"),
            // standard.mac's `global SCAN_PT` and `global SCAN_STATUS`.
            ("SCAN_PT", ""),
            ("SCAN_STATUS", ""),
            ("_1", "50"),
            ("g_kappa", "50"),
            ("monE", "0"),
            ("ATTEN_VALUE", "0"),
            ("_autofil", "0"),
            ("icrang", "5"),
            ("icoffs", "0"),
            // standard.mac `settemp`'s limits, as the kappa state has them.
            ("T_LO_SP", "-100"),
            ("T_HI_SP", "100"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        Ok(World {
            s: vec![0.0; cfg.counters.len()],
            angles: a.clone(),
            a,
            motors,
            cfg,
            count: None,
            optics: Optics::default(),
            sample: start.sample,
            truth,
            tc: Lakeshore::new(25.0),
            temperature_mac: false,
            vars,
            scan: ScanD::default(),
            busy: false,
            dose: Dose::default(),
            rng,
            epoch: now,
            now,
            index,
            subs: HashMap::new(),
        })
    }

    pub fn motor(&self, mne: &str) -> Option<usize> {
        self.cfg.motor(mne)
    }

    pub fn var(&self, name: &str) -> f64 {
        self.vars
            .get(name)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0.0)
    }

    pub fn set_var(&mut self, name: &str, value: impl ToString) {
        self.vars.insert(name.to_string(), value.to_string());
    }

    pub fn counting(&self) -> bool {
        self.count.is_some()
    }

    /// The channels `is_using_counter` takes: all but those configured
    /// `NONE`.
    pub fn used_counters(&self) -> Vec<usize> {
        (0..self.cfg.counters.len())
            .filter(|&i| self.cfg.counters[i].kind != "NONE")
            .collect()
    }

    /// `scan_data(pno, x)` of a scan of motor `m`, `counts` holding `S[]`;
    /// at `NPTS` 0 it first lays out `SCAN_COLS` as `scan_data_init` does.
    /// The sim keeps no orientation matrix, so H, K and L are 0. The row
    /// count `SCAN_D` grew to, if it did.
    pub fn scan_data(&mut self, pno: usize, m: usize, x: f64, counts: &[f64]) -> Option<usize> {
        let used = self.used_counters();
        let hkl = GEOMETRY.contains(&self.motors[m].mne.as_str());
        if self.var("NPTS") == 0.0 {
            let mut cols = vec![self.motors[m].mne.clone()];
            if hkl {
                cols.extend(["H", "K", "L"].map(String::from));
            }
            cols.extend(used.iter().map(|&i| self.cfg.counters[i].mne.clone()));
            self.scan_data_init(cols);
        }
        let mut row = vec![x];
        if hkl {
            row.extend([0.0; 3]);
        }
        row.extend(used.iter().map(|&i| counts[i]));
        let grew = self.scan.put(pno, &row);
        // scan_info_update(pno, "running"); `_g3` is 0 outside a mesh.
        let row: Vec<String> = row.into_iter().map(format_number).collect();
        self.set_var("SCAN_PT", format!("[{pno},[{}],0]", row.join(",")));
        grew
    }

    /// `scan_data_init`: `delete SCAN_COLS`, `SCAN_COLS = scan_cols`,
    /// then `scan_info_update(-1, "running")`. The two `SCAN_COLS` events
    /// go out as the statements run, ahead of the point's.
    fn scan_data_init(&mut self, cols: Vec<String>) {
        let old = self.scan.cols_value();
        self.scan.init(cols);
        let new = self.scan.cols_value();
        for sub in self.subs.values_mut() {
            for w in sub.watches.iter_mut().filter(|w| w.prop == Prop::ScanCols) {
                if let Some(old) = &old {
                    let mut p = old.fill(Packet::new(Cmd::Event, w.sn, &w.name));
                    p.header.flags = FLAG_DELETED;
                    let _ = sub.tx.send(p);
                }
                if let Some(new) = &new {
                    let _ = sub
                        .tx
                        .send(new.fill(Packet::new(Cmd::Event, w.sn, &w.name)));
                }
                w.last = new.clone();
            }
        }
        self.set_var("SCAN_STATUS", "running");
    }

    /// `_tail`'s `scan_info_update(NPTS - 1, "idle")`.
    pub fn scan_tail(&mut self) {
        self.set_var("SCAN_STATUS", "idle");
    }

    pub fn any_moving(&self) -> bool {
        self.motors.iter().any(Motor::moving)
    }

    fn geometry_at(&self, t: Instant) -> Geometry {
        let lab = |i: usize| self.motors[i].lab_at(t);
        let [u4, d4, h4, r4] = self.index.hs4.map(lab);
        let [u5, d5, h5, r5] = self.index.hs5.map(lab);
        Geometry {
            tth: lab(self.index.tth),
            th: lab(self.index.th),
            nu: lab(self.index.nu),
            sy: lab(self.index.sy),
            shold: lab(self.index.shold),
            hs4u: u4,
            hs4d: d4,
            hs4h: h4,
            hs4r: r4,
            hs5u: u5,
            hs5d: d5,
            hs5h: h5,
            hs5r: r5,
        }
    }

    fn ring(&self, t: Instant) -> f64 {
        beam::ring_current((t - self.epoch).as_secs_f64())
    }

    /// Counts per second on every channel at `t`; instantaneous channels
    /// (`EPICS_PV`, `EPICS_SC`) are `None`.
    fn channel_rates(&self, t: Instant) -> Vec<Option<f64>> {
        let r = beam::rates(
            &self.geometry_at(t),
            &self.optics,
            self.sample,
            &self.truth,
            self.ring(t),
        );
        self.cfg
            .counters
            .iter()
            .enumerate()
            .map(|(i, c)| match c.kind.as_str() {
                "EPICS_PV" | "EPICS_SC" => None,
                _ if i == self.index.icxrd => Some(r.icxrd),
                _ if i == self.index.det => Some(r.det),
                _ if i == self.index.ic1 => Some(r.ic1),
                "VSC16" => Some(DARK),
                _ => Some(0.0),
            })
            .collect()
    }

    /// The reading of an instantaneous channel.
    fn instant_value(&self, i: usize, t: Instant) -> f64 {
        let c = &self.cfg.counters[i];
        let raw = match c.mne.as_str() {
            "ringI" => self.ring(t),
            "att" => self.tc.reading,
            _ => 0.0,
        };
        raw / c.scale
    }

    fn advance(&mut self, now: Instant) {
        if now <= self.now {
            return;
        }
        if let Some(preset) = self.count {
            self.integrate(preset, now);
        }
        if self.optics.shutter_open() {
            let dt = (now - self.now).as_secs_f64();
            let mid = self.now + (now - self.now) / 2;
            let rate = beam::on_sample(&self.geometry_at(mid), &self.optics, self.ring(mid));
            self.dose.photons += rate * dt;
            self.dose.open += dt;
        }
        for m in &mut self.motors {
            m.advance(now);
        }
        self.tc.advance((now - self.now).as_secs_f64());
        self.now = now;
    }

    fn integrate(&mut self, preset: Preset, now: Instant) {
        let dt = (now - self.now).as_secs_f64();
        let sec = self.index.sec;
        let mut f = match preset {
            Preset::Time(t) => ((t - self.s[sec]) / dt).clamp(0.0, 1.0),
            Preset::Monitor { .. } => 1.0,
        };
        let mid = self.now + Duration::from_secs_f64(dt * f / 2.0);
        let rates = self.channel_rates(mid);
        let mut done = false;
        if let Preset::Monitor { channel, target } = preset
            && let Some(rate) = rates[channel]
        {
            let need = target - self.s[channel];
            let expected = rate * dt;
            if expected >= need {
                f = (need / expected).clamp(0.0, 1.0);
            }
            let got = if f < 1.0 {
                need
            } else {
                self.rng.poisson(expected)
            };
            self.s[channel] = (self.s[channel] + got).min(target);
            done = self.s[channel] >= target;
        }
        for (i, rate) in rates.iter().enumerate() {
            let monitor = matches!(preset, Preset::Monitor { channel, .. } if channel == i);
            if i == sec || monitor {
                continue;
            }
            match rate {
                Some(r) => self.s[i] += self.rng.poisson(r * dt * f),
                None => self.s[i] = self.instant_value(i, mid),
            }
        }
        self.s[sec] += dt * f;
        if let Preset::Time(t) = preset
            && self.s[sec] >= t - 1e-9
        {
            self.s[sec] = t;
            done = true;
        }
        if done {
            self.count = None;
        }
    }

    /// Seconds since the sim started.
    pub fn elapsed(&self) -> f64 {
        (self.now - self.epoch).as_secs_f64()
    }

    /// The plate's dose so far, which a scan or an erase clears.
    pub fn take_dose(&mut self) -> Dose {
        std::mem::take(&mut self.dose)
    }

    /// `count_em`: a positive preset is seconds, a negative one monitor
    /// counts on channel `MON`; zero counts nothing, as `count_em` does.
    pub fn start_count(&mut self, preset: f64) -> Result<(), String> {
        if self.counting() {
            return Err("Counters are already counting.".into());
        }
        let preset = if preset > 0.0 {
            if preset > MAX_TIME_PRESET {
                return Err(format!(
                    "Count time {preset} exceeds the {MAX_TIME_PRESET} s timer preset."
                ));
            }
            Preset::Time(preset)
        } else if preset < 0.0 {
            let mon = self.var("MON");
            if mon < 0.0 || mon as usize >= self.s.len() {
                return Err("No monitor channel selected for monitor preset counting.".into());
            }
            Preset::Monitor {
                channel: mon as usize,
                target: -preset,
            }
        } else {
            return Ok(());
        };
        self.s.iter_mut().for_each(|v| *v = 0.0);
        self.count = Some(preset);
        Ok(())
    }

    /// ^C: every motor decelerates to a stop and counting ends. `A[]`
    /// becomes where the motors come to rest, so no later `move_em` moves
    /// one back to an aborted target.
    pub fn halt(&mut self) {
        let now = self.now;
        for (i, m) in self.motors.iter_mut().enumerate() {
            m.stop(now);
            self.a[i] = m.user_of(m.destination());
        }
        self.angles.clone_from(&self.a);
        self.count = None;
    }

    /// `stop(2)`: counting ends where it is.
    pub fn stop_count(&mut self) {
        self.count = None;
    }

    pub fn get_angles(&mut self) {
        self.angles = self.motors.iter().map(Motor::user).collect();
        self.a.clone_from(&self.angles);
    }

    /// Starts every motor whose `A[]` differs from its `get_angles` value
    /// by a step or more; nothing moves if any of them cannot.
    pub fn move_em(&mut self) -> Result<(), String> {
        let mut starts = Vec::new();
        for (i, m) in self.motors.iter().enumerate() {
            let dial = m.dial_of(self.a[i]);
            if m.quantize(dial) == m.quantize(m.dial_of(self.angles[i])) {
                continue;
            }
            if m.moving() {
                return Err(format!("{} is already moving.", m.mne));
            }
            if dial < m.low_limit || dial > m.high_limit {
                let (side, lim) = if dial < m.low_limit {
                    ("low", m.low_limit)
                } else {
                    ("high", m.high_limit)
                };
                let user = m.user_of(lim);
                return Err(format!("{} will hit {side} limit at {}.", m.mne, user));
            }
            starts.push((i, dial));
        }
        let now = self.now;
        for (i, dial) in starts {
            self.motors[i].start(now, dial);
            self.angles[i] = self.a[i];
        }
        Ok(())
    }

    /// `set mne value`: the offset that makes the current position read
    /// `value`. Returns the position before.
    pub fn set_position(&mut self, i: usize, value: f64) -> f64 {
        let m = &mut self.motors[i];
        let before = m.user();
        m.offset = value - m.sign * m.dial();
        let after = m.user();
        self.a[i] = after;
        self.angles[i] = after;
        before
    }

    /// A `motor_par` write; `None` for a parameter this sim does not keep.
    pub fn motor_par(&mut self, i: usize, par: &str, value: f64) -> Option<()> {
        let m = &mut self.motors[i];
        match par {
            "slew_rate" | "velocity" => m.slew = value,
            "base_rate" => m.base = value,
            "acceleration" => m.accel = value,
            "backlash" => m.backlash = value,
            _ => return None,
        }
        Some(())
    }

    pub fn motor_par_value(&self, i: usize, par: &str) -> Option<f64> {
        let m = &self.motors[i];
        Some(match par {
            "slew_rate" | "velocity" => m.slew,
            "config_velocity" => m.config_slew,
            "base_rate" => m.base,
            "acceleration" => m.accel,
            "backlash" => m.backlash,
            "step_size" => m.steps,
            _ => return None,
        })
    }

    /// Bytes a macro writes to a serial port: PFCU blades on 3 and 4, the
    /// amplifier range and offset on 5, the temperature controller on 9.
    /// Everything is logged.
    pub fn ser_put(&mut self, port: u32, text: &str) {
        eprintln!("bl5a-sim: ser {port} <- {text:?}");
        match port {
            3 | 4 => {
                if let Some((insert, blades)) = pfcu_command(text) {
                    for b in blades {
                        self.optics.pfcu[port as usize - 3][b] = insert;
                    }
                }
            }
            5 => {
                let t = text.trim();
                if let Some(v) = t.strip_prefix(":CONF0:CURR:RANG ") {
                    if let Ok(n) = v.trim().parse() {
                        self.optics.amp_range = n;
                    }
                } else if let Some(v) = t.strip_prefix(":CONF0:CURR:OFFS ")
                    && let Ok(x) = v.trim().parse()
                {
                    self.optics.amp_offset = x;
                }
            }
            9 => self.tc.command(text),
            _ => {}
        }
    }

    /// Sends `text` to every `output/tty` watcher.
    pub fn print(&mut self, text: &str) {
        for sub in self.subs.values() {
            for w in &sub.watches {
                if matches!(&w.prop, Prop::Output(f) if f == "tty") {
                    let _ = sub
                        .tx
                        .send(Packet::new(Cmd::Event, w.sn, &w.name).with_string(text));
                }
            }
        }
    }

    pub fn resolve(&self, name: &str) -> Result<Prop, String> {
        let unknown = || format!("Unknown property \"{name}\"");
        let parts: Vec<&str> = name.split('/').collect();
        Ok(match parts[..] {
            ["error"] => Prop::Error,
            ["status", "ready"] => Prop::Ready,
            ["status", "shell" | "simulate" | "quit"] => Prop::StatusZero,
            ["var", "SCAN_D"] => Prop::ScanD,
            ["var", "SCAN_COLS"] => Prop::ScanCols,
            ["var", v] if !v.is_empty() => Prop::Var(v.to_string()),
            ["output", f] if !f.is_empty() => Prop::Output(f.to_string()),
            ["scaler", ".all.", "count"] => Prop::Count,
            ["scaler", mne, "value"] => {
                Prop::ScalerValue(self.cfg.counter(mne).ok_or_else(unknown)?)
            }
            ["motor", "..", op] => Prop::MotorAll(op.to_string()),
            ["motor", mne, p] => Prop::Motor(
                self.motor(mne).ok_or_else(unknown)?,
                MotorProp::parse(p).ok_or_else(unknown)?,
            ),
            _ => return Err(unknown()),
        })
    }

    /// What a `SV_CHAN_READ` returns and an event carries.
    pub fn read(&self, prop: &Prop) -> Option<Value> {
        let flag = |b: bool| u8::from(b).to_string();
        Some(Value::Text(match prop {
            Prop::Motor(i, p) => {
                let m = &self.motors[*i];
                let (lo, hi) = m.limit_hit();
                match p {
                    MotorProp::StartOne => return None,
                    MotorProp::Position => format_number(m.user()),
                    MotorProp::DialPosition => format_number(m.dial()),
                    MotorProp::Offset => format_number(m.offset),
                    MotorProp::StepSize => format_number(m.steps),
                    MotorProp::Sign => format_number(m.sign),
                    MotorProp::MoveDone => flag(m.moving()),
                    MotorProp::HighLimHit => flag(hi),
                    MotorProp::LowLimHit => flag(lo),
                    MotorProp::EmergencyStop | MotorProp::MotorFault | MotorProp::Unusable => {
                        flag(false)
                    }
                    MotorProp::HighLimit => format_number(m.high_limit),
                    MotorProp::LowLimit => format_number(m.low_limit),
                    MotorProp::SlewRate => format_number(m.slew),
                    MotorProp::BaseRate => format_number(m.base),
                    MotorProp::Acceleration => format_number(m.accel),
                    MotorProp::Backlash => format_number(m.backlash),
                }
            }
            Prop::ScalerValue(i) => format_number(self.s[*i]),
            Prop::Count => flag(self.counting()),
            Prop::Ready => flag(self.busy),
            Prop::StatusZero => flag(false),
            Prop::Var(v) => self.vars.get(v)?.clone(),
            Prop::ScanD => return self.scan.data_value(),
            Prop::ScanCols => return self.scan.cols_value(),
            Prop::MotorAll(_) | Prop::Output(_) | Prop::Error => return None,
        }))
    }

    /// The value an event carries; differs from [`World::read`] only for
    /// `status/ready`, whose event is 1 at the prompt and read is 1 busy.
    fn event_value(&self, prop: &Prop) -> Option<Value> {
        match prop {
            Prop::Ready => Some(Value::Text(u8::from(!self.busy).to_string())),
            _ => self.read(prop),
        }
    }

    pub fn connect(&mut self, id: u64, tx: mpsc::UnboundedSender<Packet>) {
        self.subs.insert(
            id,
            Sub {
                tx,
                watches: Vec::new(),
            },
        );
    }

    pub fn disconnect(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// `SV_REGISTER`: the watch and its first event; an unknown name is
    /// reported on the client's `error` watch.
    pub fn register(&mut self, id: u64, name: &str, sn: u32) {
        let prop = match self.resolve(name) {
            Ok(p) => p,
            Err(message) => {
                self.report_error(id, &message);
                return;
            }
        };
        let first = match &prop {
            Prop::Error => Some(Value::Text("No error".to_string())),
            p => self.event_value(p),
        };
        let Some(sub) = self.subs.get_mut(&id) else {
            return;
        };
        sub.watches.retain(|w| w.name != name);
        if let Some(v) = &first {
            let _ = sub.tx.send(v.fill(Packet::new(Cmd::Event, sn, name)));
        }
        let last = if prop == Prop::Error { None } else { first };
        sub.watches.push(Watch {
            name: name.to_string(),
            prop,
            sn,
            last,
        });
    }

    pub fn unregister(&mut self, id: u64, name: &str) {
        if let Some(sub) = self.subs.get_mut(&id) {
            sub.watches.retain(|w| w.name != name);
        }
    }

    pub fn report_error(&mut self, id: u64, message: &str) {
        eprintln!("bl5a-sim: client {id}: {message}");
        if let Some(sub) = self.subs.get(&id) {
            for w in sub.watches.iter().filter(|w| w.prop == Prop::Error) {
                let _ = sub
                    .tx
                    .send(Packet::new(Cmd::Event, w.sn, &w.name).with_string(message));
            }
        }
    }

    fn publish(&mut self) {
        let mut subs = std::mem::take(&mut self.subs);
        for sub in subs.values_mut() {
            for flags in [false, true] {
                for w in sub.watches.iter_mut().filter(|w| w.prop.is_flag() == flags) {
                    let Some(v) = self.event_value(&w.prop) else {
                        continue;
                    };
                    if w.last.as_ref() != Some(&v) {
                        let _ = sub.tx.send(v.fill(Packet::new(Cmd::Event, w.sn, &w.name)));
                        w.last = Some(v);
                    }
                }
            }
        }
        self.subs = subs;
    }
}

/// `!PFCU00 I0200\r`: (insert?, blade indices). `0` digits are padding.
fn pfcu_command(text: &str) -> Option<(bool, Vec<usize>)> {
    let t = text.trim_start_matches(['\\', '!']);
    let rest = t.strip_prefix("PFCU")?.get(2..)?.trim_start();
    let mut chars = rest.chars();
    let insert = match chars.next()? {
        'I' => true,
        'R' => false,
        _ => return None,
    };
    let blades = chars
        .take_while(char::is_ascii_digit)
        .filter_map(|c| match c {
            '1'..='4' => Some(c as usize - '1' as usize),
            _ => None,
        })
        .collect();
    Some((insert, blades))
}

/// A queued command and where its result goes.
pub struct Job {
    pub abort_gen: u64,
    pub text: String,
    pub reply: Option<(mpsc::UnboundedSender<Packet>, spec_protocol::Header)>,
}

pub struct Shared {
    world: Mutex<World>,
    abort: watch::Sender<u64>,
    queue: mpsc::UnboundedSender<Job>,
}

/// The world, advanced to the moment it was locked; publishes on drop.
pub struct WorldGuard<'a>(MutexGuard<'a, World>);

impl Deref for WorldGuard<'_> {
    type Target = World;
    fn deref(&self) -> &World {
        &self.0
    }
}

impl DerefMut for WorldGuard<'_> {
    fn deref_mut(&mut self) -> &mut World {
        &mut self.0
    }
}

impl Drop for WorldGuard<'_> {
    fn drop(&mut self) {
        self.0.publish();
    }
}

impl Shared {
    pub fn new(world: World) -> (Shared, mpsc::UnboundedReceiver<Job>) {
        let (queue, rx) = mpsc::unbounded_channel();
        let shared = Shared {
            world: Mutex::new(world),
            abort: watch::Sender::new(0),
            queue,
        };
        (shared, rx)
    }

    pub fn lock(&self) -> WorldGuard<'_> {
        let mut g = self.world.lock().unwrap_or_else(PoisonError::into_inner);
        g.advance(Instant::now());
        WorldGuard(g)
    }

    pub fn abort_gen(&self) -> u64 {
        *self.abort.borrow()
    }

    pub fn abort_watch(&self) -> watch::Receiver<u64> {
        self.abort.subscribe()
    }

    /// ^C: stops motion and counting and cancels the running command and
    /// every command queued before it.
    pub fn abort(&self) {
        eprintln!("bl5a-sim: ^C");
        self.lock().halt();
        self.abort.send_modify(|g| *g += 1);
    }

    /// Pushes `text` onto the command queue.
    pub fn push(
        &self,
        text: String,
        reply: Option<(mpsc::UnboundedSender<Packet>, spec_protocol::Header)>,
    ) {
        let job = Job {
            abort_gen: self.abort_gen(),
            text,
            reply,
        };
        let _ = self.queue.send(job);
    }

    pub async fn tick(&self) {
        let mut every = tokio::time::interval(TICK);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            drop(self.lock());
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn config() -> Config {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../ref/spec/kappa.config");
        Config::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    pub fn world() -> World {
        World::new(config(), &Start::beamline(Sample::Film, 1)).unwrap()
    }

    #[test]
    fn pfcu_commands_parse() {
        assert_eq!(pfcu_command("!PFCU00 I0200\r"), Some((true, vec![1])));
        assert_eq!(
            pfcu_command("\\!PFCU00 R1034\r"),
            Some((false, vec![0, 2, 3]))
        );
        assert_eq!(pfcu_command("!PFCU00 I13\r\n"), Some((true, vec![0, 2])));
        assert_eq!(pfcu_command(":CONF0"), None);
    }

    #[test]
    fn serial_writes_reach_the_optics() {
        let mut w = world();
        w.ser_put(4, "!PFCU00 I0000\r");
        w.ser_put(4, "!PFCU00 R1234\r");
        assert!(w.optics.shutter_open());
        w.ser_put(3, "!PFCU00 I1030\r");
        assert_eq!(w.optics.filter_units(), 5);
        w.ser_put(5, ":CONF0:CURR:RANG 4\n");
        w.ser_put(5, ":CONF0:CURR:OFFS 1\n");
        assert_eq!((w.optics.amp_range, w.optics.amp_offset), (4, 1.0));
    }

    #[tokio::test(start_paused = true)]
    async fn time_preset_ends_exactly() {
        let mut w = world();
        w.start_count(1.0).unwrap();
        assert!(w.start_count(1.0).is_err());
        let t0 = w.now;
        w.advance(t0 + Duration::from_millis(990));
        assert!(w.counting());
        w.advance(t0 + Duration::from_millis(1010));
        assert!(!w.counting());
        assert_eq!(w.s[w.index.sec], 1.0);
        assert!(w.start_count(MAX_TIME_PRESET + 1.0).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn monitor_preset_stops_on_the_target() {
        let mut w = world();
        w.ser_put(4, "!PFCU00 R1234\r");
        w.set_var("MON", w.index.icxrd);
        w.start_count(-1e4).unwrap();
        let mut t = w.now;
        while w.counting() {
            t += TICK;
            w.advance(t);
        }
        assert_eq!(w.s[w.index.icxrd], 1e4);
        let rate = w.channel_rates(t)[w.index.icxrd].unwrap();
        assert!((w.s[w.index.sec] - 1e4 / rate).abs() < 0.05);
        w.set_var("MON", -1);
        assert!(w.start_count(-5.0).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn move_em_moves_only_what_changed() {
        let mut w = world();
        let (tth, th) = (w.motor("tth").unwrap(), w.motor("th").unwrap());
        w.get_angles();
        w.a[tth] = 1.5;
        w.move_em().unwrap();
        assert!(w.motors[tth].moving() && !w.motors[th].moving());
        // A second client's start_one while tth is still moving.
        w.get_angles();
        w.a[th] = 0.5;
        w.move_em().unwrap();
        assert!(w.motors[th].moving());
        w.get_angles();
        w.a[tth] = 2.0;
        assert_eq!(w.move_em(), Err("tth is already moving.".into()));
        w.halt();
        w.advance(w.now + Duration::from_secs(5));
        w.get_angles();
        w.a[th] = 2e4;
        assert!(w.move_em().unwrap_err().contains("high limit"));
        assert!(!w.any_moving());
    }

    #[test]
    fn set_keeps_the_dial_and_moves_the_offset() {
        let mut w = world();
        let tth = w.motor("tth").unwrap();
        let dial = w.motors[tth].dial();
        let before = w.set_position(tth, 3.0);
        assert_eq!((before, w.motors[tth].user()), (1.0, 3.0));
        assert_eq!(w.motors[tth].dial(), dial);
        assert_eq!((w.a[tth], w.angles[tth]), (3.0, 3.0));
    }

    #[test]
    fn events_carry_values_before_flags() {
        let mut w = world();
        let (tx, mut rx) = mpsc::unbounded_channel();
        w.connect(1, tx);
        w.register(1, "status/ready", 5);
        w.register(1, "scaler/.all./count", 6);
        w.register(1, "scaler/sec/value", 7);
        w.register(1, "motor/xx/position", 8);
        w.register(1, "error", 9);
        w.register(1, "motor/yy/position", 10);
        let mut got = Vec::new();
        while let Ok(p) = rx.try_recv() {
            got.push((p.header.sn, p.text().into_owned()));
        }
        assert_eq!(
            got,
            [
                (5, "1".into()),
                (6, "0".into()),
                (7, "0".into()),
                (9, "No error".into()),
                (9, "Unknown property \"motor/yy/position\"".into())
            ]
        );
        w.s[w.index.sec] = 2.0;
        w.busy = true;
        w.count = Some(Preset::Time(2.0));
        w.publish();
        let mut got = Vec::new();
        while let Ok(p) = rx.try_recv() {
            got.push(p.header.name);
        }
        assert_eq!(
            got,
            ["scaler/sec/value", "status/ready", "scaler/.all./count"]
        );
    }

    #[test]
    fn scan_arrays_read_as_spec_sends_them() {
        let mut w = world();
        let unread = |w: &World, name| w.resolve(name).map(|p| w.read(&p));
        assert_eq!(unread(&w, "var/SCAN_D"), Ok(None), "no scan yet");
        assert_eq!(unread(&w, "var/SCAN_COLS"), Ok(None));
        let (th, sy) = (w.motor("th").unwrap(), w.motor("sy").unwrap());
        let n = w.s.len();
        let counts: Vec<f64> = (0..n).map(|i| i as f64).collect();
        w.scan_data(0, th, 0.5, &counts);
        w.set_var("NPTS", 1);
        w.scan_data(1, th, 0.75, &counts);
        let used = w.used_counters();
        let mut names = vec!["th", "H", "K", "L"];
        names.extend(used.iter().map(|&i| w.cfg.counters[i].mne.as_str()));
        let cols = w.resolve("var/SCAN_COLS").map(|p| w.read(&p)).unwrap();
        let pairs = names
            .iter()
            .enumerate()
            .map(|(i, c)| (i.to_string(), c.to_string()))
            .collect();
        assert_eq!(cols, Some(Value::Assoc(pairs)));
        let mut row: Vec<f64> = vec![0.75, 0.0, 0.0, 0.0];
        row.extend(used.iter().map(|&i| i as f64));
        let data = w.read(&Prop::ScanD).unwrap();
        let p = data.fill(Packet::new(Cmd::Reply, 1, "var/SCAN_D"));
        let h = &p.header;
        assert_eq!(
            (h.data_type, h.rows, h.cols),
            (Some(DataType::ArrDouble), 4096, names.len() as u32)
        );
        assert_eq!(p.data.len(), 4096 * names.len() * 8);
        let second: Vec<f64> = p.data[names.len() * 8..2 * names.len() * 8]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| f64::from_ne_bytes(*b))
            .collect();
        assert_eq!(second, row);
        // A motor outside the geometry has no H K L; the new column count
        // makes a new array, and a point past its rows grows it.
        w.set_var("NPTS", 0);
        w.scan_data(0, sy, 1.0, &counts);
        w.set_var("NPTS", 1);
        assert_eq!(w.scan_data(4096, sy, 2.0, &counts), Some(5120));
        let Some(Value::Doubles { rows, cols, data }) = w.read(&Prop::ScanD) else {
            panic!("SCAN_D is an array");
        };
        assert_eq!((rows, cols), (5120, 1 + used.len()));
        assert_eq!((data[0], data[4096 * cols]), (1.0, 2.0));
        assert!(w.resolve("var/SCAN_D").is_ok_and(|p| p == Prop::ScanD));
    }

    #[test]
    fn scan_events_go_out_as_spec_sends_them() {
        let mut w = world();
        let (tx, mut rx) = mpsc::unbounded_channel();
        w.connect(1, tx);
        for (sn, name) in [
            (1, "var/SCAN_COLS"),
            (2, "var/SCAN_PT"),
            (3, "var/SCAN_STATUS"),
        ] {
            w.register(1, name, sn);
        }
        let mut events = || {
            let mut got = Vec::new();
            while let Ok(p) = rx.try_recv() {
                got.push((
                    p.header.sn,
                    p.header.flags,
                    p.text().into_owned(),
                    p.assoc(),
                ));
            }
            got
        };
        let at_load: Vec<_> = events().into_iter().map(|e| (e.0, e.2)).collect();
        assert_eq!(at_load, [(2, String::new()), (3, String::new())]);
        let sy = w.motor("sy").unwrap();
        let used = w.used_counters();
        let counts: Vec<f64> = (0..w.s.len()).map(|i| i as f64 * 0.5).collect();
        let mut cols = vec![("0".to_string(), "sy".to_string())];
        cols.extend(
            used.iter()
                .enumerate()
                .map(|(k, &i)| ((k + 1).to_string(), w.cfg.counters[i].mne.clone())),
        );
        let pt = |pno: usize, x: &str| {
            let row: Vec<String> = used
                .iter()
                .map(|&i| format_number(i as f64 * 0.5))
                .collect();
            format!("[{pno},[{x},{}],0]", row.join(","))
        };

        w.scan_data(0, sy, 1.0, &counts);
        w.publish();
        let got = events();
        assert_eq!(
            (got[0].0, got[0].1, &got[0].3),
            (1, 0, &cols),
            "SCAN_COLS first"
        );
        assert_eq!((got[1].0, &got[1].2), (2, &pt(0, "1")));
        assert_eq!((got[2].0, &got[2].2[..]), (3, "running"));
        assert_eq!(got.len(), 3);
        w.set_var("NPTS", 1);
        w.scan_data(1, sy, 1.25, &counts);
        w.publish();
        let got = events();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].0, &got[0].2), (2, &pt(1, "1.25")));

        // The next scan deletes the columns and lays them out again, even
        // unchanged.
        w.set_var("NPTS", 0);
        w.scan_data(0, sy, 2.0, &counts);
        w.publish();
        let got = events();
        assert_eq!((got[0].0, got[0].1, &got[0].3), (1, FLAG_DELETED, &cols));
        assert_eq!((got[1].0, got[1].1, &got[1].3), (1, 0, &cols));
        assert_eq!((got[2].0, &got[2].2), (2, &pt(0, "2")));
        assert_eq!(got.len(), 3);
        w.scan_tail();
        w.publish();
        let got: Vec<_> = events().into_iter().map(|e| (e.0, e.2)).collect();
        assert_eq!(got, [(3, "idle".to_string())]);
    }
}
