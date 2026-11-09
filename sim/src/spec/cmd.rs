//! The command queue's interpreter: the statements the server itself
//! queues (`move_em`, `count_em`, `set`, `motor_par`) and the BL5A macros
//! the bl5a-qs plans send, each following its `.mac` body. Anything else is
//! an error, so a plan that leans on an unmodelled macro fails visibly.

use std::time::Duration;

use spec_protocol::format_number;
use tokio::sync::watch;
use tokio::time::Instant;

use super::cam::Cam;
use crate::world::{MAX_TIME_PRESET, Shared, TICK};

/// How a command ended other than normally.
#[derive(Debug, PartialEq)]
pub enum Exit {
    /// A macro's `exit` or a failed builtin, with the message it printed.
    Error(String),
    /// ^C.
    Abort,
}

/// The value a `SV_CMD_WITH_RETURN` reply carries, and how the command
/// ended.
pub struct Outcome {
    pub value: String,
    pub result: Result<(), Exit>,
}

/// Runs `text` as one queued command.
pub async fn run(shared: &Shared, cam: &mut Cam, text: &str, generation: u64) -> Outcome {
    let mut run = Run {
        shared,
        cam,
        abort: shared.abort_watch(),
        generation,
        value: String::new(),
        locals: Vec::new(),
    };
    let result = run.all(text).await;
    // The command is one block: its locals end with it, however it ended.
    {
        let mut w = shared.lock();
        for (name, global) in run.locals.drain(..).rev() {
            match global {
                Some(v) => w.vars.insert(name, v),
                None => w.vars.remove(&name),
            };
        }
    }
    Outcome {
        value: run.value,
        result,
    }
}

/// Statements separated by `;`, newlines and braces outside quotes and
/// parentheses.
pub fn statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut depth = 0usize;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ';' | '\n' | '{' | '}') if depth == 0 => {
                let s = cur.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    let s = cur.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
    out
}

/// One parsed statement.
#[derive(Debug, PartialEq)]
enum Stmt {
    /// `A[mne]=value`.
    SetA(String, String),
    /// `name=value`, the value as written (quoted for a string).
    Set(String, String),
    /// `name arg ...` or `name(arg, ...)`.
    Call(String, Vec<String>),
}

fn parse(stmt: &str) -> Result<Stmt, String> {
    let syntax = || format!("Syntax error: {stmt}");
    if let Some(rest) = stmt.strip_prefix("A[") {
        let (mne, value) = rest.split_once(']').ok_or_else(syntax)?;
        let value = value.trim_start().strip_prefix('=').ok_or_else(syntax)?;
        return Ok(Stmt::SetA(mne.trim().to_string(), value.trim().to_string()));
    }
    let end = stmt
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(stmt.len());
    let (name, rest) = stmt.split_at(end);
    if name.is_empty() {
        return Err(syntax());
    }
    let rest = rest.trim();
    if let Some(value) = rest.strip_prefix('=').filter(|v| !v.starts_with('=')) {
        return Ok(Stmt::Set(name.to_string(), value.trim().to_string()));
    }
    let args = if let Some(inner) = rest.strip_prefix('(') {
        let inner = inner.strip_suffix(')').ok_or_else(syntax)?;
        split_args(inner)
    } else {
        rest.split_whitespace().map(unquote).collect()
    };
    Ok(Stmt::Call(name.to_string(), args))
}

fn split_args(inner: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    for c in inner.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, ',') => {
                args.push(unquote(cur.trim()));
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() || !args.is_empty() {
        args.push(unquote(cur.trim()));
    }
    args
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    for q in ['"', '\''] {
        if let Some(inner) = t.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.to_string();
        }
    }
    t.to_string()
}

/// `printf("%g")`: six significant digits, trailing zeros dropped.
fn g(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    let sci = format!("{x:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        trim(format!("{x:.decimals$}"))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa.to_string()), exp.abs())
    }
}

/// The amplifier range `atte` sets for attenuation `n` in the beam energy
/// band `monE`; `None` where no branch of the macro matches.
fn atte_range(mon_e: f64, n: f64) -> Option<u32> {
    let band = |bands: &[(f64, f64, u32)]| {
        bands
            .iter()
            .find(|(lo, hi, _)| (*lo..*hi).contains(&n))
            .map(|b| b.2)
    };
    if (0.0..13.0).contains(&mon_e) {
        match n {
            _ if n == 0.0 => Some(5),
            _ if n == 1.0 || n == 2.0 => Some(4),
            _ if n == 3.0 => Some(3),
            _ => band(&[(4.0, 6.0, 2), (6.0, 64.0, 1)]),
        }
    } else if (13.0..17.0).contains(&mon_e) {
        band(&[
            (0.0, 3.0, 4),
            (3.0, 7.0, 3),
            (7.0, 14.0, 2),
            (14.0, 64.0, 1),
        ])
    } else if mon_e >= 17.0 {
        band(&[
            (0.0, 2.0, 4),
            (2.0, 8.0, 3),
            (8.0, 14.0, 2),
            (14.0, 64.0, 1),
        ])
    } else {
        None
    }
}

/// `SER_PFCU0`, `SER_PFCU1`, the amplifier, mon1.
const SER_PFCU: [u32; 2] = [3, 4];
const SER_AMP: u32 = 5;
const SER_MON1: u32 = 8;
/// temperature.mac's LakeShore 331.
const SER_TC: u32 = 9;

/// The file myinsitu `qdo`s for its temperature macros. It is not in the
/// archive; the sixc state's copies of them stand in for it.
const TEMPERATURE_MAC: &str = "/home/bl5a/specadm/spec6.06.11/macros/local_macros/temperature.mac";

/// SPEC's `date()`.
fn date() -> String {
    chrono::Local::now()
        .format("%a %b %e %H:%M:%S %Y")
        .to_string()
}

struct Run<'a> {
    shared: &'a Shared,
    cam: &'a mut Cam,
    abort: watch::Receiver<u64>,
    generation: u64,
    value: String,
    /// `local` names, each with the global value it hides.
    locals: Vec<(String, Option<String>)>,
}

impl Run<'_> {
    async fn all(&mut self, text: &str) -> Result<(), Exit> {
        for stmt in statements(text) {
            self.check()?;
            self.stmt(&stmt).await?;
        }
        Ok(())
    }

    fn check(&self) -> Result<(), Exit> {
        if *self.abort.borrow() == self.generation {
            Ok(())
        } else {
            Err(Exit::Abort)
        }
    }

    /// Printed output, to every `output/tty` watcher.
    fn printf(&mut self, text: &str) {
        self.shared.lock().print(text);
    }

    fn say(&mut self, line: &str) {
        self.printf(&format!("{line}\n"));
    }

    /// `eprint msg; exit`.
    fn fail(&mut self, msg: &str) -> Exit {
        self.say(msg);
        Exit::Error(msg.to_string())
    }

    fn num(&mut self, arg: &str) -> Result<f64, Exit> {
        match spec_protocol::parse_number(arg) {
            Some(v) => Ok(v),
            None => Err(self.fail(&format!("bl5a-sim: \"{arg}\" is not a number"))),
        }
    }

    fn nums(&mut self, args: &[&str]) -> Result<Vec<f64>, Exit> {
        args.iter().map(|a| self.num(a)).collect()
    }

    fn comment(&mut self, msg: &str) {
        self.say(msg);
    }

    async fn sleep(&mut self, secs: f64) -> Result<(), Exit> {
        let d = Duration::from_secs_f64(secs.max(0.0));
        tokio::select! {
            () = tokio::time::sleep(d) => self.check(),
            _ = self.abort.changed() => Err(Exit::Abort),
        }
    }

    async fn wait_moves(&mut self) -> Result<(), Exit> {
        while self.shared.lock().any_moving() {
            self.sleep(TICK.as_secs_f64()).await?;
        }
        Ok(())
    }

    async fn wait_count(&mut self) -> Result<(), Exit> {
        while self.shared.lock().counting() {
            self.sleep(TICK.as_secs_f64()).await?;
        }
        Ok(())
    }

    fn motor(&mut self, mne: &str) -> Result<usize, Exit> {
        let i = self.shared.lock().motor(mne);
        match i {
            Some(i) => Ok(i),
            None => Err(self.fail(&format!("Invalid motor:  \"{mne}\"."))),
        }
    }

    fn ser_put(&mut self, port: u32, text: &str) {
        self.shared.lock().ser_put(port, text);
    }

    async fn stmt(&mut self, stmt: &str) -> Result<(), Exit> {
        let parsed = parse(stmt).map_err(|e| self.fail(&e))?;
        let (name, args) = match parsed {
            Stmt::SetA(mne, value) => {
                let i = self.motor(&mne)?;
                let v = self.num(&value)?;
                self.shared.lock().a[i] = v;
                return Ok(());
            }
            Stmt::Set(name, value) => {
                let quoted = unquote(&value);
                let v = if quoted != value {
                    quoted
                } else {
                    format_number(self.num(&value)?)
                };
                self.shared.lock().set_var(&name, v);
                return Ok(());
            }
            Stmt::Call(name, args) => (name, args),
        };
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match name.as_str() {
            "get_angles" | "getangles" => self.shared.lock().get_angles(),
            "sync" => {}
            "waitmove" => self.wait_moves().await?,
            "move_em" => self.move_em()?,
            "count_em" => self.count_em(&args)?,
            "uct" | "uctn" => self.uct(&args).await?,
            "sleep" | "do_sleep" => {
                let t = self.num(args.first().unwrap_or(&"0"))?;
                self.sleep(t).await?;
            }
            "set" => self.set(&args).await?,
            "motor_par" => self.motor_par(&args)?,
            "wm" | "wm2" => self.wm(&args)?,
            "fi" => self.pfcu_macro(0, &args).await?,
            "fil" => self.pfcu_macro(1, &args).await?,
            "atte" => self.atte(&args).await?,
            "atten" => self.atten(&args).await?,
            "_atten" => {
                if args.len() != 2 {
                    return Err(self.fail("Usage: _atten id[0,1] value[0-15]"));
                }
                let v = self.nums(&args)?;
                self.atten_unit(v[0], v[1]).await?;
            }
            "ic_range" => self.ic_range(args.first().copied().unwrap_or("")).await?,
            "ic_offset" => self.ic_offset(args.first().copied().unwrap_or("")).await?,
            "mon1_setup" => self.mon1_setup(&args).await?,
            "mar345openshutter" => self.shutter(true),
            "mar345closeshutter" => self.shutter(false),
            "mar345expose" => self.mar345expose(&args).await?,
            "mar345_lab6" => self.mar345_rock(&args, 1.0).await?,
            "mar345_lab6_wide" => self.mar345_rock(&args, 5.0).await?,
            "local" => self.local(&args),
            "exp_condition" => {
                let filename = self.shared.lock().vars.get("filename").cloned();
                self.exp_condition(&filename.unwrap_or_default());
            }
            "mar345readout" => self.mar345readout().await?,
            "mar345erase" => self.mar345erase().await?,
            "marstart2" => self.marstart2(&args).await?,
            "mar_ip7" => self.mar_ip(&args, 7).await?,
            "mar_ip" => self.mar_ip(&args, 4).await?,
            "mar_ip_mode" => self.mar_ip_mode(&args).await?,
            "ds_wid" => self.slit_width(&args, "hs5", 0.2).await?,
            "ss_wid" => self.slit_width(&args, "hs4", 0.4).await?,
            "all6" => self.all6(&args).await?,
            "setkappa" => self.setkappa(&args)?,
            "LF" => {
                if let Some(m) = args.first() {
                    self.motor(m)?;
                }
            }
            "autofil" => self.autofil(&args)?,
            "counters" => self.counters(&args)?,
            "qdo" => self.qdo(&args)?,
            "settemp" => self.settemp(&args).await?,
            "set_ramp" | "read_ramp" | "changec" if !self.shared.lock().temperature_mac => {
                return Err(self.fail(&format!(
                    "{name}: not defined until temperature.mac is qdo'd (bl5a-sim)"
                )));
            }
            "set_ramp" => self.set_ramp(&args).await?,
            "read_ramp" => self.read_ramp().await?,
            "changec" => self.changec(&args).await?,
            "loopscan" => self.loopscan(&args).await?,
            "cscan" => self.cscan(&args).await?,
            "clup" => self.clup(&args).await?,
            _ => {
                return Err(self.fail(&format!("{name}: not modeled by bl5a-sim")));
            }
        }
        Ok(())
    }

    fn move_em(&mut self) -> Result<(), Exit> {
        let r = self.shared.lock().move_em();
        r.map_err(|e| self.fail(&e))
    }

    fn count_em(&mut self, args: &[&str]) -> Result<(), Exit> {
        let t = match args.first() {
            Some(a) => self.num(a)?,
            None => self.shared.lock().var("COUNT"),
        };
        let r = self.shared.lock().start_count(t);
        r.map_err(|e| self.fail(&e))
    }

    /// `uct`: wait for motors, count, show the counters.
    async fn uct(&mut self, args: &[&str]) -> Result<(), Exit> {
        self.wait_moves().await?;
        self.count_em(args)?;
        self.wait_count().await?;
        let line = self.counter_line();
        self.say(&line);
        Ok(())
    }

    /// The counters of the last count, `mne=value` each.
    fn counter_line(&self) -> String {
        let w = self.shared.lock();
        w.used_counters()
            .into_iter()
            .map(|i| format!("{}={}", w.cfg.counters[i].mne, g(w.s[i])))
            .collect::<Vec<_>>()
            .join(" ")
    }

    async fn set(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args.len() != 2 {
            return Err(self.fail("Usage:  set motor new_user_value"));
        }
        let Some(i) = self.shared.lock().motor(args[0]) else {
            return Err(self.fail(&format!("set: Invalid motor: \"{}\"", args[0])));
        };
        let v = self.num(args[1])?;
        self.wait_moves().await?;
        let before = {
            let mut w = self.shared.lock();
            w.get_angles();
            w.set_position(i, v)
        };
        let after = self.shared.lock().motors[i].user();
        if before != after {
            self.comment(&format!(
                "{} reset from {} to {}",
                args[0],
                g(before),
                g(after)
            ));
        }
        Ok(())
    }

    fn motor_par(&mut self, args: &[&str]) -> Result<(), Exit> {
        let (Some(mne), Some(par)) = (args.first(), args.get(1)) else {
            return Err(self.fail("Usage: motor_par(motor, par [, value])"));
        };
        let i = self.motor(mne)?;
        let value = match args.get(2) {
            Some(v) => {
                let v = self.num(v)?;
                self.shared.lock().motor_par(i, par, v).map(|()| v)
            }
            None => self.shared.lock().motor_par_value(i, par),
        };
        match value {
            Some(v) => {
                self.value = format_number(v);
                Ok(())
            }
            None => Err(self.fail(&format!("motor_par: \"{par}\" not modeled by bl5a-sim"))),
        }
    }

    fn wm(&mut self, args: &[&str]) -> Result<(), Exit> {
        let mut text = String::new();
        let w = self.shared.lock();
        for mne in args {
            let Some(i) = w.motor(mne) else {
                drop(w);
                return Err(self.fail(&format!("Invalid motor:  \"{mne}\".")));
            };
            let m = &w.motors[i];
            text.push_str(&format!(
                "{:>10} {:10.4} ({:10.4} .. {:10.4})\n",
                mne,
                m.user(),
                m.user_of(m.low_limit),
                m.user_of(m.high_limit)
            ));
        }
        drop(w);
        self.printf(&text);
        Ok(())
    }

    /// `fi` / `fil`: four 0/1 flags, one per blade.
    async fn pfcu_macro(&mut self, unit: usize, args: &[&str]) -> Result<(), Exit> {
        let label = ["Filter", "Filter2"][unit];
        if args.is_empty() {
            self.say("Usage: fi [0/1 0/1 0/1 0/1]");
            let f = self.shared.lock().optics.pfcu[unit];
            self.say(&format!(
                "{label} {} {} {} {} is(are) working.",
                u8::from(f[0]),
                u8::from(f[1]) * 2,
                u8::from(f[2]) * 3,
                u8::from(f[3]) * 4
            ));
            return Ok(());
        }
        if args.len() != 4 {
            return Ok(());
        }
        let mut ins = [0u8; 4];
        let mut rem = [0u8; 4];
        let flags = self.nums(args)?;
        for (i, f) in flags.into_iter().enumerate() {
            let n = (i + 1) as u8;
            if f > 0.0 {
                ins[i] = n;
            } else {
                rem[i] = n;
            }
        }
        let digits = |d: [u8; 4]| d.iter().map(u8::to_string).collect::<String>();
        let i_cmd = format!("!PFCU00 I{}\r", digits(ins));
        let r_cmd = format!("!PFCU00 R{}\r", digits(rem));
        let port = SER_PFCU[unit];
        self.ser_put(port, &i_cmd);
        self.sleep(0.15).await?;
        self.ser_put(port, &r_cmd);
        self.sleep(0.15).await?;
        if unit == 0 {
            self.say(&i_cmd);
            self.say(&r_cmd);
        }
        self.say(&format!(
            "{label} {} {} {} {} are(is) working.",
            ins[0], ins[1], ins[2], ins[3]
        ));
        Ok(())
    }

    /// `atte n`: `atten n`, then the amplifier range for the beam energy
    /// band `monE`.
    async fn atte(&mut self, args: &[&str]) -> Result<(), Exit> {
        let Some(a) = args.first() else {
            let units = self.shared.lock().optics.filter_units();
            self.say(&format!("No.of filter = {units}"));
            self.printf("Usage: atte [no]");
            return Ok(());
        };
        let n = self.num(a)?;
        if n < 0.0 {
            self.printf("Usage: atte [no]");
        }
        let mon_e = self.shared.lock().var("monE");
        if let Some(range) = atte_range(mon_e, n) {
            self.atten(&[a]).await?;
            self.ic_range(&range.to_string()).await?;
        }
        Ok(())
    }

    async fn atten(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args.len() != 1 {
            return Err(self.fail("Usage: atten value[0-63]"));
        }
        let v = self.num(args[0])?;
        if !(0.0..=63.0).contains(&v) {
            return Err(self.fail("Usage: atten value[0-63]"));
        }
        let n = v as u32;
        self.atten_unit(0.0, f64::from(n % 16)).await?;
        self.atten_unit(1.0, f64::from(n / 16)).await?;
        self.shared.lock().set_var("ATTEN_VALUE", n);
        self.comment(&format!("Filter Attenuation = {n}"));
        Ok(())
    }

    /// `_atten id value`: blades of the set bits in, the rest out.
    async fn atten_unit(&mut self, id: f64, v: f64) -> Result<(), Exit> {
        if !(0.0..=15.0).contains(&v) {
            return Err(self.fail("Usage: _atten id[0,1] value[0-15]"));
        }
        let bits = v as u32;
        let (mut ic, mut rc) = (String::new(), String::new());
        for i in 1..=4 {
            if bits >> (i - 1) & 1 == 1 {
                ic.push_str(&i.to_string());
            } else {
                rc.push_str(&i.to_string());
            }
        }
        let port = match id {
            0.0 => SER_PFCU[0],
            1.0 => SER_PFCU[1],
            _ => return Ok(()),
        };
        self.ser_put(port, &format!("!PFCU00 I{ic}\r\n"));
        self.sleep(0.1).await?;
        self.ser_put(port, &format!("!PFCU00 R{rc}\r\n"));
        self.sleep(0.1).await
    }

    async fn ic_range(&mut self, arg: &str) -> Result<(), Exit> {
        self.ser_put(SER_AMP, &format!(":CONF0:CURR:RANG {arg}\n"));
        self.sleep(0.1).await?;
        self.ser_put(SER_AMP, ":CONF0:CURR:RANG?\n");
        self.sleep(0.25).await?;
        let range = self.shared.lock().optics.amp_range;
        self.say("");
        self.say(&format!("Current range {range}"));
        self.shared.lock().set_var("icrang", range);
        Ok(())
    }

    async fn ic_offset(&mut self, arg: &str) -> Result<(), Exit> {
        self.ser_put(SER_AMP, &format!(":CONF0:CURR:OFFS {arg}\n"));
        self.sleep(0.1).await?;
        self.ser_put(SER_AMP, ":CONF0:CURR:OFFS?\n");
        self.sleep(0.15).await?;
        let offset = g(self.shared.lock().optics.amp_offset);
        self.say(&offset);
        self.sleep(0.2).await?;
        self.say("");
        self.say(&format!("Current offset {offset}"));
        self.shared.lock().set_var("icoffs", arg);
        Ok(())
    }

    async fn mon1_setup(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args.len() != 5 {
            return Err(
                self.fail("Usage: mon1_setup [HV GAIN PKT UPP LOW] (bl5a-sim does not prompt)")
            );
        }
        let v = self.nums(args)?;
        let cmds = [
            format!(":SOUR0:VOLT {}\n", v[0] as i64),
            format!(":INP0:GAIN {}\n", v[1] as i64),
            format!(":SENS0:PKT {}\n", v[2] as i64),
            format!(":SENS0:SCA:UPP {:.6}\n", v[3]),
            format!(":SENS0:SCA:LOW {:.6}\n", v[4]),
        ];
        for c in &cmds {
            self.ser_put(SER_MON1, c);
            self.sleep(0.7).await?;
        }
        Ok(())
    }

    fn shutter(&mut self, open: bool) {
        let (i, r, msg) = if open {
            ("I0000", "R1234", "Shutter is removed.")
        } else {
            ("I0030", "R1204", "Shutter is working.")
        };
        self.ser_put(SER_PFCU[1], &format!("!PFCU00 {i}\r"));
        self.ser_put(SER_PFCU[1], &format!("!PFCU00 {r}\r"));
        self.say(msg);
    }

    /// Opens the shutter for 3 s counts until the monitor sees beam.
    async fn marstart2(&mut self, args: &[&str]) -> Result<(), Exit> {
        self.ic_offset("0").await?;
        let mut sl_time = 3.0;
        let icxrd = self.shared.lock().cfg.counter("icxrd");
        for _ in 0..500 {
            self.shutter(true);
            self.uct(&["3"]).await?;
            self.shutter(false);
            let s = {
                let w = self.shared.lock();
                icxrd.map_or(0.0, |c| w.s[c])
            };
            if s > 100.0 {
                break;
            }
            if let Some(a) = args.first() {
                let t = self.num(a)?;
                if t != 0.0 {
                    sl_time = t;
                }
            }
            self.sleep(sl_time).await?;
        }
        Ok(())
    }

    fn ca<T>(&mut self, r: Result<T, String>) -> Result<T, Exit> {
        r.map_err(|e| self.fail(&e))
    }

    /// `mar345expose(filename, exposure)`: names the file, exposes through
    /// the shutter in counts of at most 429 s, has mar345-ioc scan, and
    /// keeps the number of the file written in `file_no`.
    async fn mar345expose(&mut self, args: &[&str]) -> Result<(), Exit> {
        let [filename, exposure] = args else {
            return Err(self.fail("Usage:  mar345expose(\"filename\", Texpo)"));
        };
        let exposure = self.num(exposure)?;
        // _mar345fileinput
        let r = self.cam.put_text("FileName", filename).await;
        self.ca(r)?;
        self.sleep(0.1).await?;

        self.shutter(true);
        if exposure > 429.0 {
            let n = (exposure / 429.0).trunc();
            let rest = exposure - n * 429.0;
            self.say(&g(n));
            self.say(&g(rest));
            for t in 1..=n as u64 {
                self.say(&t.to_string());
                self.uct(&["429"]).await?;
            }
            self.uct(&[&format_number(rest)]).await?;
        } else {
            self.uct(&[&format_number(exposure)]).await?;
        }
        self.shutter(false);
        self.exp_condition(filename);

        // _mar345scanning
        self.mar345_run("Acquire", "Scanning", " mar345 scanning ", 1.0, 0.5)
            .await?;
        self.mar345readout().await
    }

    /// `mar345_lab6(filename, exposure)` and `mar345_lab6_wide`: th and
    /// phi from −w to w (1° and 5°) across the exposure. The sim moves them
    /// to −w, exposes still and moves them to w; the frame does not model
    /// the rock.
    async fn mar345_rock(&mut self, args: &[&str], w: f64) -> Result<(), Exit> {
        if args.len() != 2 {
            return Err(self.fail("Usage:  mar345_lab6(\"filename\", Texpo)"));
        }
        self.move_and_wait(&[("th", -w), ("phi", -w)]).await?;
        self.mar345expose(args).await?;
        self.move_and_wait(&[("th", w), ("phi", w)]).await
    }

    /// `mar345readout(filename)`: `file_no`, the number of the file
    /// mar345-ioc last wrote. The string it returns is not modeled.
    async fn mar345readout(&mut self) -> Result<(), Exit> {
        let r = self.cam.get_number("FileNumber").await;
        let next = self.ca(r)?;
        self.shared
            .lock()
            .set_var("file_no", format_number(next - 1.0));
        Ok(())
    }

    /// `local a, b`: unset until the command ends, then the globals again.
    fn local(&mut self, args: &[&str]) {
        let mut w = self.shared.lock();
        for name in args
            .iter()
            .flat_map(|a| a.split(','))
            .filter(|n| !n.is_empty())
        {
            if !self.locals.iter().any(|(n, _)| n == name) {
                self.locals.push((name.to_string(), w.vars.remove(name)));
            }
        }
    }

    /// `exp_condition`'s file line. Its exposure-condition line and the
    /// `mar345_comment` and `beam_cond1` it calls are not modeled.
    fn exp_condition(&mut self, filename: &str) {
        let file_no = {
            let mut w = self.shared.lock();
            w.get_angles();
            w.var("file_no")
        };
        // `%-0.4d`: at least four digits after the sign.
        let n = (file_no + 1.0) as i64;
        let number = format!("{}{:04}", if n < 0 { "-" } else { "" }, n.unsigned_abs());
        self.say("");
        self.say(&format!(
            "File {filename}_{number}.mar3450 will be saved at {}.",
            date()
        ));
    }

    /// `qdo file`: only temperature.mac is modeled, and it only defines
    /// the temperature macros.
    fn qdo(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args != [TEMPERATURE_MAC] {
            return Err(self.fail(&format!("qdo {}: not modeled by bl5a-sim", args.join(" "))));
        }
        self.shared.lock().temperature_mac = true;
        Ok(())
    }

    /// `settemp T`: temperature.mac's once it is `qdo`'d, `SETP 1,T` (its
    /// copy of the line in the data file is not modeled). Before that the
    /// kappa state's standard.mac one: it checks `T_LO_SP`..`T_HI_SP`, sets
    /// `TEMP_SP` and calls the empty `_settemp`, so nothing reaches the
    /// controller.
    async fn settemp(&mut self, args: &[&str]) -> Result<(), Exit> {
        if !self.shared.lock().temperature_mac {
            let [arg] = args else {
                return Err(self.fail("Usage:  settemp set_point"));
            };
            let t = self.num(arg)?;
            let (lo, hi) = {
                let w = self.shared.lock();
                (w.var("T_LO_SP"), w.var("T_HI_SP"))
            };
            if t < lo || t > hi {
                return Err(self.fail(&format!("Temp limits are {} to {}.", g(lo), g(hi))));
            }
            self.shared.lock().set_var("TEMP_SP", format_number(t));
            return Ok(());
        }
        let Some(arg) = args.first() else {
            return Err(self.fail("settemp without a set point: not modeled by bl5a-sim"));
        };
        let t = self.num(arg)?;
        self.ser_put(SER_TC, &format!("SETP 1,{arg}\r\n"));
        self.sleep(0.15).await?;
        self.say(&format!("Set T =  {} at {}.", g(t), date()));
        Ok(())
    }

    /// `set_ramp r`: ramping on at r K/min, then `read_ramp`. The line's
    /// `%s` gets `$1`, not the date, as in the macro.
    async fn set_ramp(&mut self, args: &[&str]) -> Result<(), Exit> {
        const USAGE: &str = "Usage:  set_ramp R";
        let [arg] = args else {
            self.say(USAGE);
            self.read_ramp().await?;
            return Err(Exit::Error(USAGE.into()));
        };
        let r = self.num(arg)?;
        self.ser_put(SER_TC, &format!("RAMP 1, 1, {arg}\r\n"));
        self.sleep(0.15).await?;
        self.read_ramp().await?;
        self.say(&format!(
            "Ramp Setting: Ramping ON with {arg}K/m at {}.",
            g(r)
        ));
        Ok(())
    }

    /// `read_ramp`: `RAMP? A`, then prints the reply. The 331 takes a loop
    /// number there, not an input; what it answers is not known, so the
    /// sim prints an empty reply.
    async fn read_ramp(&mut self) -> Result<(), Exit> {
        self.ser_put(SER_TC, "RAMP? A\r\n");
        self.sleep(0.2).await?;
        self.say("");
        self.sleep(0.2).await
    }

    /// `CRDG? A`: input A, °C.
    async fn crdg(&mut self) -> Result<f64, Exit> {
        self.ser_put(SER_TC, "CRDG? A\r\n");
        self.sleep(0.5).await?;
        Ok(self.shared.lock().tc.reading)
    }

    /// `changec T`: `settemp T`, then `CRDG? A` until it is within
    /// `DELTA_K` of `TEMP_SP`. The kappa state has no `DELTA_K`, and an
    /// unset one is 0: the wait then lasts until the reading is exactly T.
    async fn changec(&mut self, args: &[&str]) -> Result<(), Exit> {
        let start = Instant::now();
        self.settemp(args).await?;
        let sp = self.num(args.first().unwrap_or(&""))?;
        self.shared.lock().set_var("TEMP_SP", format_number(sp));
        self.sleep(1.0).await?;
        let mut t = self.crdg().await?;
        loop {
            let (sp, tol) = {
                let mut w = self.shared.lock();
                w.set_var("DEGK", format_number(t));
                (w.var("TEMP_SP"), w.var("DELTA_K"))
            };
            if (t - sp).abs() <= tol {
                break;
            }
            t = self.crdg().await?;
            self.shared.lock().set_var("DEGK", format_number(t));
            self.printf(&format!("\r Current Temperature: {}(K)", g(t)));
            self.sleep(1.0).await?;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let t = self.crdg().await?;
        self.comment(&format!(
            "Temperature {} C reached after {} seconds.",
            g(t),
            g(elapsed)
        ));
        Ok(())
    }

    /// `loopscan n [t [sleep]]` (`_timescan`): n counts of t s (`COUNT`
    /// if not given), `sleep` s apart. The sim prints the heading and a
    /// line a point, its time from the start and the counters as `uct`
    /// prints them; the data file and the plot are not modeled.
    async fn loopscan(&mut self, args: &[&str]) -> Result<(), Exit> {
        const USAGE: &str = "Usage:  loopscan npoints [counting_time [sleep_time]]";
        let n = match args.first() {
            Some(a) if args.len() <= 3 => self.num(a)?,
            _ => 0.0,
        };
        if n < 1.0 {
            return Err(self.fail(USAGE));
        }
        let ctime = match args.get(1) {
            Some(a) => self.num(a)?,
            None => self.shared.lock().var("COUNT"),
        };
        let stime = match args.get(2) {
            Some(a) => self.num(a)?,
            None => 0.0,
        };
        let n = n.trunc() as u64;
        self.say(&format!("loopscan {n} {} {}", g(ctime), g(stime)));
        let start = Instant::now();
        for point in 0..n {
            let time = start.elapsed().as_secs_f64();
            self.count_em(&[&format_number(ctime)])?;
            self.wait_count().await?;
            let line = self.counter_line();
            self.say(&format!("{point:>4} {time:9.4} {line}"));
            self.sleep(stime).await?;
        }
        Ok(())
    }

    /// `clup mot start end scan_time [sleep_time]`: a `cscan` from the
    /// motor's position plus start to plus end, then `umv` back.
    async fn clup(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args.len() < 4 {
            return Err(self.fail("Usage: clup mot start end scan_time [sleep_time]"));
        }
        let m = self.motor(args[0])?;
        let pos_i = {
            let mut w = self.shared.lock();
            w.get_angles();
            w.a[m]
        };
        let s = format_number(pos_i + self.num(args[1])?);
        let f = format_number(pos_i + self.num(args[2])?);
        let mut cscan = vec![args[0], &s, &f, args[3]];
        cscan.extend(args.get(4));
        self.cscan(&cscan).await?;
        self.wait_moves().await?;
        self.shared.lock().get_angles();
        self.move_and_wait(&[(args[0], pos_i)]).await
    }

    /// `cscan mot start end scan_time [sleep_time]` (`_cscan_prep`, then
    /// `_cscan` with one motor). `cscan_cleanup` ends it however it ends,
    /// as `cleanup_once` does.
    async fn cscan(&mut self, args: &[&str]) -> Result<(), Exit> {
        if !(4..=5).contains(&args.len()) {
            return Err(self.fail("Usage: cscan mot start end scan_time [sleep_time]"));
        }
        let stime = match args.get(4) {
            Some(a) => self.num(a)?,
            None => 0.0,
        };
        let ctime = self.num(args[3])?;
        let Some(m) = self.shared.lock().motor(args[0]) else {
            return Err(self.fail(&format!("cscan: Invalid motor: \"{}\"", args[0])));
        };
        let (s, f) = (self.num(args[1])?, self.num(args[2])?);
        let mne = args[0];
        let heading = format!("cscan  {mne} {} {}  {}", g(s), g(f), g(ctime));
        if ctime <= 0.0 {
            return Err(self.fail("Need to specify a positive scan time in seconds for cscan ."));
        }
        let (name, low, high, dial) = {
            let w = self.shared.lock();
            let mo = &w.motors[m];
            let dial = [mo.dial_of(s), mo.dial_of(f)];
            (
                w.cfg.motors[m].name.clone(),
                mo.low_limit,
                mo.high_limit,
                dial,
            )
        };
        let bad: Vec<String> = dial
            .iter()
            .filter_map(|&d| {
                if d < low {
                    Some(format!("{name} will hit low limit at {}.", g(low)))
                } else if d > high {
                    Some(format!("{name} will hit high limit at {}.", g(high)))
                } else {
                    None
                }
            })
            .collect();
        if !bad.is_empty() {
            for line in &bad {
                self.say(line);
            }
            return Err(Exit::Error(bad.join("\n")));
        }
        if s == f {
            return Err(self.fail(&format!(
                "Start and finish positions for {mne} are the same."
            )));
        }
        let par = |p: &str| {
            self.shared
                .lock()
                .motor_par_value(m, p)
                .expect("a parameter the sim keeps")
        };
        let speed = ((f - s) * par("step_size")).abs() / ctime;
        let config = par("config_velocity");
        if speed > config {
            return Err(self.fail(&format!(
                "Requested cscan speed of {} for {mne} exceeds configured speed of {}.",
                g(speed),
                g(config)
            )));
        }
        let saved = (par("velocity"), par("base_rate"));
        let r = self
            .cscan_sweep(m, (s, f), speed, saved.1, stime, &heading)
            .await;
        self.cscan_cleanup(m, saved);
        // scan_tail runs only after the last point; an abort leaves
        // SCAN_STATUS "running", as cleanup_once does in SPEC.
        if r.is_ok() {
            self.shared.lock().scan_tail();
        }
        r
    }

    /// `_cscan` from the move to the start to the last point.
    async fn cscan_sweep(
        &mut self,
        m: usize,
        (s, f): (f64, f64),
        speed: f64,
        base: f64,
        stime: f64,
        heading: &str,
    ) -> Result<(), Exit> {
        self.wait_moves().await?;
        let away = {
            let mut w = self.shared.lock();
            w.get_angles();
            let away = w.a[m] != s;
            w.a[m] = s;
            away
        };
        if away {
            self.say("Moving to start position ...");
        }
        self.move_em()?;
        self.wait_moves().await?;
        if away {
            self.say("");
        }
        let mne = self.shared.lock().motors[m].mne.clone();
        let velocity = self.shared.lock().motors[m].slew;
        if speed != velocity {
            self.say(&format!(
                "Setting {mne} velocity to {} (from {})",
                g(speed),
                g(velocity)
            ));
            self.shared.lock().motor_par(m, "velocity", speed);
        }
        if speed < base {
            self.say(&format!(
                "Setting {mne} base rate to {} (from {})",
                g(speed / 2.0),
                g(base)
            ));
            self.shared.lock().motor_par(m, "base_rate", speed / 2.0);
        }
        let (labels, channels) = {
            let w = self.shared.lock();
            let name = |v: f64| {
                (v >= 0.0)
                    .then(|| w.cfg.counters.get(v as usize))
                    .flatten()
                    .map(|c| (c.name.clone(), v as usize))
            };
            let det = name(w.var("DET"));
            let mon = name(w.var("MON"));
            let sec = w
                .cfg
                .counter("sec")
                .map(|i| (w.cfg.counters[i].name.clone(), i));
            let shown: Vec<(String, usize)> = [det, mon, sec].into_iter().flatten().collect();
            let labels = shown
                .iter()
                .map(|(n, _)| format!("{n:>8.8}"))
                .collect::<Vec<_>>()
                .join(" ");
            let labels = format!("  # {:>9.9} {labels}", w.cfg.motors[m].name);
            (
                labels,
                shown.into_iter().map(|(_, i)| i).collect::<Vec<_>>(),
            )
        };
        // scan_head: the heading and the column labels.
        self.say(&format!("\n{heading}\n"));
        self.say(&labels);
        self.shared.lock().set_var("NPTS", 0);
        self.shared.lock().a[m] = f;
        let r = self.shared.lock().start_count(MAX_TIME_PRESET);
        r.map_err(|e| self.fail(&e))?;
        self.move_em()?;
        let n = self.shared.lock().s.len();
        let (mut prev_s, mut prev_a, mut first, mut pno) = (vec![0.0; n], 0.0, true, 0);
        // `chk_move`: while any motor moves. Without a sleep time a sample
        // is a tick, standing in for SPEC's hardware reads.
        while self.shared.lock().any_moving() {
            self.sleep(if stime > 0.0 {
                stime
            } else {
                TICK.as_secs_f64()
            })
            .await?;
            let (now_s, now_a) = {
                let mut w = self.shared.lock();
                w.get_angles();
                (w.s.clone(), w.a[m])
            };
            let counts: Vec<f64> = now_s.iter().zip(&prev_s).map(|(a, b)| a - b).collect();
            prev_s = now_s;
            let x = (now_a + prev_a) / 2.0;
            prev_a = now_a;
            if first {
                first = false;
                continue;
            }
            let npts = self.shared.lock().var("NPTS");
            let values = channels
                .iter()
                .map(|&i| format!("{:>8}", g(counts[i])))
                .collect::<Vec<_>>()
                .join(" ");
            self.say(&format!("{npts:>3} {x:9.4} {values}"));
            let grew = self.shared.lock().scan_data(pno, m, x, &counts);
            if let Some(rows) = grew {
                self.say(&format!(
                    "Warning:  SCAN_D array is growing to {rows} rows."
                ));
            }
            self.shared.lock().set_var("NPTS", npts + 1.0);
            pno += 1;
        }
        Ok(())
    }

    /// `cscan_cleanup`: counting stops, and the motor gets back the
    /// velocity and base rate it had before the scan.
    fn cscan_cleanup(&mut self, m: usize, (velocity, base): (f64, f64)) {
        let (mne, at, v1, b1) = {
            let mut w = self.shared.lock();
            w.stop_count();
            w.get_angles();
            let mo = &w.motors[m];
            (mo.mne.clone(), w.a[m], mo.slew, mo.base)
        };
        self.say("");
        self.say(&format!("Motor {mne} at {}", g(at)));
        if velocity != v1 {
            self.say(&format!(
                "Setting {mne} velocity to {} (from {})",
                g(velocity),
                g(v1)
            ));
            self.shared.lock().motor_par(m, "velocity", velocity);
        }
        if base != b1 {
            self.say(&format!(
                "Setting {mne} base rate to {} (from {})",
                g(base),
                g(b1)
            ));
            self.shared.lock().motor_par(m, "base_rate", base);
        }
    }

    async fn mar345erase(&mut self) -> Result<(), Exit> {
        let state = self
            .mar345_run("Erase", "Erasing", " mar345 erasing ", 0.5, 1.0)
            .await?;
        if state == "Idle" {
            self.printf("\r Erase Completed !!");
        }
        Ok(())
    }

    /// Puts 1 to `field`, then polls `DetectorState_RBV` while it reads
    /// `busy`, as `_mar345scanning` and `mar345erase` do; the state it
    /// ended on.
    async fn mar345_run(
        &mut self,
        field: &'static str,
        busy: &str,
        banner: &str,
        shown: f64,
        settle: f64,
    ) -> Result<String, Exit> {
        let r = self.cam.put_number(field, 1.0).await;
        self.ca(r)?;
        self.sleep(2.0).await?;
        let r = self.cam.get_state("DetectorState_RBV").await;
        let mut state = self.ca(r)?;
        while state == busy {
            self.printf(&format!("\r{banner}"));
            self.sleep(shown).await?;
            self.printf("\r                 ");
            self.sleep(0.1).await?;
            let r = self.cam.get_state("DetectorState_RBV").await;
            state = self.ca(r)?;
            self.sleep(settle).await?;
        }
        Ok(state)
    }

    /// Moves the motors in `pairs` together and waits, as `move_em;
    /// _update(...)` does.
    async fn move_and_wait(&mut self, pairs: &[(&str, f64)]) -> Result<(), Exit> {
        let mut idx = Vec::new();
        for (mne, _) in pairs {
            idx.push(self.motor(mne)?);
        }
        {
            let mut w = self.shared.lock();
            for (i, (_, v)) in idx.iter().zip(pairs) {
                w.a[*i] = *v;
            }
        }
        self.move_em()?;
        self.wait_moves().await?;
        let names: Vec<&str> = pairs.iter().map(|(m, _)| *m).collect();
        self.wm(&names)
    }

    /// `mar_ip7` (7 axes) and `mar_ip` (the first 4), refused when tth is on
    /// the wrong side for the kappa geometry `_1`.
    async fn mar_ip(&mut self, args: &[&str], n: usize) -> Result<(), Exit> {
        const AXES: [&str; 7] = [
            "martran", "marsdd", "marz", "marshut", "marx", "ipbsx", "ipbsy",
        ];
        if args.len() != n {
            let usage = format!("Usage:  mar_ip {}", AXES[..n].join(" "));
            self.say(&usage);
            self.wm(&AXES[..n])?;
            return Err(Exit::Error(usage));
        }
        let v = self.nums(args)?;
        self.wait_moves().await?;
        let (tth, kap) = {
            let mut w = self.shared.lock();
            w.get_angles();
            let tth = w.motor("tth").map_or(0.0, |i| w.a[i]);
            (tth, w.var("_1"))
        };
        if tth < 80.0 && kap >= 0.0 {
            return Err(self.fail(" Two Theta Angle is too low for UPWARD Geometry."));
        }
        if tth > -80.0 && kap <= 0.0 {
            return Err(self.fail(" Two Theta Angle is too high for DOWNWARD Geometry."));
        }
        let mut pairs: Vec<(&str, f64)> = AXES[..n].iter().copied().zip(v).collect();
        pairs.push(("scell", 0.0));
        self.move_and_wait(&pairs).await
    }

    async fn mar_ip_mode(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args.len() != 1 {
            self.say("Usage:  mar_ip_mode [on/off] ");
            return self.mar_ip(&[], 4).await;
        }
        if args[0] == "off" {
            let marz = {
                let mut w = self.shared.lock();
                w.get_angles();
                w.motor("marz").map_or(0.0, |i| w.a[i])
            };
            let marz = format_number(marz);
            self.mar_ip(&["1280", "835", &marz, "0"], 4).await?;
        }
        Ok(())
    }

    /// `ds_wid` (hs5) / `ss_wid` (hs4): a slit opened symmetrically about 0.
    async fn slit_width(&mut self, args: &[&str], slit: &str, min: f64) -> Result<(), Exit> {
        let m = |s: &str| format!("{slit}{s}");
        let (u, d, h, r) = (m("u"), m("d"), m("h"), m("r"));
        if args.is_empty() {
            self.shared.lock().get_angles();
            self.wm(&[&u, &d, &h, &r])?;
            let w = self.shared.lock();
            let pos = |s: &str| w.motor(s).map_or(0.0, |i| w.a[i]);
            let width = |a: f64, b: f64| ((a - b) * 100.0).trunc() / 100.0;
            let msg = format!(
                "Vert wid={} Hor wid={}",
                g(width(pos(&u), pos(&d))),
                g(width(pos(&h), pos(&r)))
            );
            drop(w);
            return Err(self.fail(&msg));
        }
        let (Some(v), Some(hz)) = (args.first(), args.get(1)) else {
            return Err(self.fail("Syntax error"));
        };
        let (v, hz) = (self.num(v)?, self.num(hz)?);
        if v < min || hz < min {
            return Err(self.fail("Slit width is too narrow"));
        }
        self.move_and_wait(&[
            (&u, v / 2.0),
            (&d, -v / 2.0),
            (&h, hz / 2.0),
            (&r, -hz / 2.0),
        ])
        .await
    }

    async fn all6(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args.len() != 6 {
            return Err(self.fail("Usage:  all6 tth th chi phi nu mu"));
        }
        let v = self.nums(args)?;
        self.wait_moves().await?;
        self.shared.lock().get_angles();
        let axes = ["tth", "th", "chi", "phi", "nu", "mu"];
        let pairs: Vec<(&str, f64)> = axes.into_iter().zip(v).collect();
        self.move_and_wait(&pairs).await
    }

    fn setkappa(&mut self, args: &[&str]) -> Result<(), Exit> {
        match args {
            [t] => {
                let t = self.num(t)?;
                let mut w = self.shared.lock();
                w.set_var("_1", format_number(t));
                if w.var("g_kappa") != t {
                    w.set_var("g_kappa", format_number(t));
                }
                Ok(())
            }
            [] => Err(self.fail("setkappa: the tilt prompt is not modeled by bl5a-sim")),
            _ => Err(self.fail("Usage:  setkappa [kappa_tilt]")),
        }
    }

    fn autofil(&mut self, args: &[&str]) -> Result<(), Exit> {
        if args.len() != 1 {
            self.say("Usage:  autofil [0/1/2/3 or off/on/on2/on3] ");
            let mode = self
                .shared
                .lock()
                .vars
                .get("_autofil")
                .cloned()
                .unwrap_or_default();
            return Err(self.fail(&format!(
                "AUTOFIL_MODE ={mode}(0:off, 1:autofil, 2:autofil2, 3:autofil3)"
            )));
        }
        let (mode, msg) = match args[0] {
            "on" | "1" => (1, "Auto filter is on now"),
            "on2" | "2" => (2, "Auto filter #2 is on now"),
            "on3" | "3" => (3, "Auto filter #3 is on now"),
            "off" | "0" => (0, "Auto filter is off now"),
            _ => return Err(self.fail("Usage:  autofil [off/on/on2/on3 or 0/1/2/3] ")),
        };
        self.shared.lock().set_var("_autofil", mode);
        self.say(msg);
        Ok(())
    }

    /// `counters mon det`: the monitor-preset channel and the detector.
    fn counters(&mut self, args: &[&str]) -> Result<(), Exit> {
        let [mon, det] = args else {
            return Err(self.fail("counters: plotselect is not modeled by bl5a-sim"));
        };
        let (mon, det) = (self.num(mon)?, self.num(det)?);
        let n = self.shared.lock().cfg.counters.len() as f64;
        if mon >= n {
            self.say(&format!("Channel {mon} out of range for monitor."));
        } else {
            let mon = if mon < 0.0 { -1.0 } else { mon.trunc() };
            self.shared.lock().set_var("MON", mon);
        }
        if det < 0.0 || det >= n {
            self.say(&format!("Channel {det} out of range for detector."));
        } else {
            self.shared.lock().set_var("DET", det.trunc());
        }
        let (m, d) = {
            let w = self.shared.lock();
            let name = |v: f64| {
                (v >= 0.0)
                    .then(|| w.cfg.counters.get(v as usize))
                    .flatten()
                    .map_or("none".to_string(), |c| c.mne.clone())
            };
            (name(w.var("MON")), name(w.var("DET")))
        };
        self.say(&format!("Monitor: {m}, Detector: {d}"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Prop, Value};

    #[test]
    fn statements_split_outside_quotes_and_parens() {
        assert_eq!(
            statements("{get_angles;A[th]=0.5;move_em;}\n"),
            ["get_angles", "A[th]=0.5", "move_em"]
        );
        assert_eq!(
            statements("motor_par(th, \"slew_rate\", 4000); fi 0 1 0 0"),
            ["motor_par(th, \"slew_rate\", 4000)", "fi 0 1 0 0"]
        );
        assert_eq!(statements("x(\"a;b\")"), ["x(\"a;b\")"]);
    }

    #[test]
    fn statements_parse_into_calls() {
        assert_eq!(
            parse("A[ tth ] = 1.5").unwrap(),
            Stmt::SetA("tth".into(), "1.5".into())
        );
        assert_eq!(
            parse("motor_par(th, \"slew_rate\", 4000)").unwrap(),
            Stmt::Call(
                "motor_par".into(),
                vec!["th".into(), "slew_rate".into(), "4000".into()]
            )
        );
        assert_eq!(
            parse("mar_ip_mode off").unwrap(),
            Stmt::Call("mar_ip_mode".into(), vec!["off".into()])
        );
        assert_eq!(parse("uct()").unwrap(), Stmt::Call("uct".into(), vec![]));
        assert!(parse("(1)").is_err());
        assert_eq!(
            parse("filename=\"gui_b5\"").unwrap(),
            Stmt::Set("filename".into(), "\"gui_b5\"".into())
        );
        assert_eq!(
            parse("exposure = -7e4").unwrap(),
            Stmt::Set("exposure".into(), "-7e4".into())
        );
        assert_eq!(
            parse("local filename, exposure").unwrap(),
            Stmt::Call("local".into(), vec!["filename,".into(), "exposure".into()])
        );
        assert!(matches!(parse("x == 1").unwrap(), Stmt::Call(..)));
    }

    #[tokio::test]
    async fn locals_hide_the_globals_until_the_command_ends() {
        let (shared, _queue) = Shared::new(crate::world::tests::world());
        shared.lock().set_var("filename", "user");
        let block = "{local filename, exposure; filename=\"gui_b5\"; exposure=3; exp_condition";
        // Ended normally, and by an error.
        for (end, ok) in [("}", true), ("; uct(\"a\")}", false)] {
            let text = format!("{block}{end}");
            let out = run(&shared, &mut Cam::default(), &text, shared.abort_gen()).await;
            assert_eq!(out.result.is_ok(), ok, "{text}: {:?}", out.result);
            let w = shared.lock();
            assert_eq!(w.vars.get("filename").map(String::as_str), Some("user"));
            assert_eq!(w.vars.get("exposure"), None);
        }
    }

    async fn run_text(shared: &Shared, text: &str) -> Result<(), Exit> {
        run(shared, &mut Cam::default(), text, shared.abort_gen())
            .await
            .result
    }

    #[tokio::test(start_paused = true)]
    async fn before_temperature_mac_settemp_sets_nothing() {
        let (shared, _queue) = Shared::new(crate::world::tests::world());
        run_text(&shared, "settemp 30").await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        let w = shared.lock();
        assert_eq!(w.var("TEMP_SP"), 30.0);
        assert_eq!(w.tc.reading, 25.0);
        drop(w);
        assert_eq!(
            run_text(&shared, "settemp 120").await,
            Err(Exit::Error("Temp limits are -100 to 100.".into()))
        );
        for cmd in ["set_ramp 10", "changec 30", "read_ramp"] {
            assert!(run_text(&shared, cmd).await.is_err(), "{cmd}");
        }
        assert!(run_text(&shared, "qdo /tmp/other.mac").await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn changec_waits_for_delta_k() {
        let (shared, _queue) = Shared::new(crate::world::tests::world());
        let start = Instant::now();
        let text = format!("qdo {TEMPERATURE_MAC}; set_ramp 10; DELTA_K=0.3; changec 30");
        run_text(&shared, &text).await.unwrap();
        // 30 s of ramp, then the lag.
        assert!(start.elapsed() > Duration::from_secs(30));
        assert!((shared.lock().tc.reading - 30.0).abs() <= 0.3);
        // The kappa state has no DELTA_K: 0, and the wait outlasts any
        // reasonable one.
        shared.lock().vars.remove("DELTA_K");
        let wait = tokio::time::timeout(Duration::from_secs(120), run_text(&shared, "changec 35"));
        assert!(wait.await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn loopscan_counts_n_points() {
        let (shared, _queue) = Shared::new(crate::world::tests::world());
        let start = Instant::now();
        run_text(&shared, "loopscan 3 2").await.unwrap();
        assert!(start.elapsed() >= Duration::from_secs(6));
        let sec = shared.lock().cfg.counter("sec").unwrap();
        assert_eq!(shared.lock().s[sec], 2.0);
        for bad in ["loopscan", "loopscan 0", "loopscan 1 1 0 0"] {
            assert!(run_text(&shared, bad).await.is_err(), "{bad}");
        }
    }

    /// `SCAN_COLS` and the first `NPTS` rows of `SCAN_D`.
    fn last_scan(shared: &Shared) -> (Vec<String>, Vec<Vec<f64>>) {
        let w = shared.lock();
        let npts = w.var("NPTS") as usize;
        let Some(Value::Assoc(cols)) = w.read(&Prop::ScanCols) else {
            panic!("SCAN_COLS is an associative array");
        };
        let Some(Value::Doubles { cols: n, data, .. }) = w.read(&Prop::ScanD) else {
            panic!("SCAN_D is an array");
        };
        let rows = data
            .chunks_exact(n)
            .take(npts)
            .map(<[f64]>::to_vec)
            .collect();
        (cols.into_iter().map(|(_, c)| c).collect(), rows)
    }

    #[tokio::test(start_paused = true)]
    async fn clup_sweeps_at_the_scan_velocity_and_returns() {
        let (shared, _queue) = Shared::new(crate::world::tests::world());
        let (th, sec) = {
            let w = shared.lock();
            (w.motor("th").unwrap(), w.cfg.counter("sec").unwrap())
        };
        let (start, slew, base) = {
            let w = shared.lock();
            (w.motors[th].user(), w.motors[th].slew, w.motors[th].base)
        };
        // 2500 steps/s, under the 4000 kappa.config gives th.
        run_text(&shared, "clup th -0.5 0.5 4 0.2").await.unwrap();
        let (cols, rows) = last_scan(&shared);
        assert_eq!(cols[..4], ["th", "H", "K", "L"]);
        let sec = 4 + shared
            .lock()
            .used_counters()
            .iter()
            .position(|&i| i == sec)
            .unwrap();
        // 4 s at a sample each 0.2 s, the first dropped.
        assert!((17..=20).contains(&rows.len()), "{} points", rows.len());
        for pair in rows.windows(2) {
            assert!(pair[1][0] > pair[0][0], "th rises point by point");
        }
        for r in &rows {
            assert!((r[0] - start).abs() < 0.5);
            assert!((r[sec] - 0.2).abs() < 1e-9, "sec is the time of one sample");
        }
        let w = shared.lock();
        assert_eq!(w.motors[th].user(), start);
        assert_eq!((w.motors[th].slew, w.motors[th].base), (slew, base));
        assert!(!w.counting());
        let last = format!("[{},[", rows.len() - 1);
        assert!(
            w.vars["SCAN_PT"].starts_with(&last),
            "{}",
            w.vars["SCAN_PT"]
        );
        assert_eq!(w.vars["SCAN_STATUS"], "idle");
    }

    #[tokio::test(start_paused = true)]
    async fn an_aborted_cscan_restores_the_velocity() {
        let (shared, _queue) = Shared::new(crate::world::tests::world());
        let th = shared.lock().motor("th").unwrap();
        let slew = shared.lock().motors[th].slew;
        let abort = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert_ne!(shared.lock().motors[th].slew, slew, "scanning");
            shared.abort();
        };
        let (r, ()) = tokio::join!(run_text(&shared, "cscan th 0 1 5"), abort);
        assert_eq!(r, Err(Exit::Abort));
        let w = shared.lock();
        assert_eq!(w.motors[th].slew, slew);
        assert!(!w.counting());
        assert_eq!(w.vars["SCAN_STATUS"], "running", "no scan_tail");
    }

    #[tokio::test(start_paused = true)]
    async fn cscan_refuses_what_its_macro_refuses() {
        let (shared, _queue) = Shared::new(crate::world::tests::world());
        for bad in [
            "cscan th 0 1",
            "cscan th 0 1 0",
            "cscan th 1 1 2",
            "cscan th 0 2e4 2",
            "cscan th 0 1000 0.001",
            "cscan xx 0 1 2",
        ] {
            assert!(run_text(&shared, bad).await.is_err(), "{bad}");
        }
        assert_eq!(shared.lock().var("NPTS"), 0.0);
    }

    #[test]
    fn atte_picks_the_macro_range() {
        assert_eq!(atte_range(0.0, 0.0), Some(5));
        assert_eq!(atte_range(0.0, 2.0), Some(4));
        assert_eq!(atte_range(0.0, 2.5), None);
        assert_eq!(atte_range(0.0, 11.0), Some(1));
        assert_eq!(atte_range(15.0, 11.0), Some(2));
        assert_eq!(atte_range(20.0, 1.0), Some(4));
        assert_eq!(atte_range(0.0, 64.0), None);
    }

    #[test]
    fn g_formats_like_printf() {
        assert_eq!(g(1.0), "1");
        assert_eq!(g(-0.25), "-0.25");
        assert_eq!(g(0.1 + 0.2), "0.3");
        assert_eq!(g(1234567.0), "1.23457e+06");
        assert_eq!(g(0.00012), "0.00012");
        assert_eq!(g(123.456789), "123.457");
    }
}
