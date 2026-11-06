//! The SPEC session config (`kappa.config`): `MOTnnn` and `CNTnnn` lines and
//! the `MOTPAR:` lines that follow a motor.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub struct MotorConfig {
    pub mne: String,
    pub name: String,
    /// `OMS`, `NONE`, `EPICS_M2`, ...
    pub controller: String,
    /// Steps per dial unit; negative for some motors.
    pub steps: f64,
    pub sign: f64,
    /// steps/s.
    pub slew: f64,
    pub base: f64,
    /// steps.
    pub backlash: f64,
    /// ms from base to slew rate.
    pub accel: f64,
    pub flags: u32,
    pub params: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CounterConfig {
    pub mne: String,
    pub name: String,
    /// `SFTWARE`, `VSC16`, `EPICS_PV`, `EPICS_SC`, `MAC_CNT`, `NONE`.
    pub kind: String,
    /// For the timer, counts per second; otherwise `S[]` = raw / scale.
    pub scale: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub motors: Vec<MotorConfig>,
    pub counters: Vec<CounterConfig>,
}

#[derive(Debug)]
pub struct ConfigError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "config line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let mut cfg = Config::default();
        for (n, line) in text.lines().enumerate() {
            let err = |message: String| ConfigError {
                line: n + 1,
                message,
            };
            if let Some(rest) = line.strip_prefix("MOTPAR:") {
                let (key, value) = rest
                    .split_once('=')
                    .ok_or_else(|| err("MOTPAR without '='".into()))?;
                let motor = cfg
                    .motors
                    .last_mut()
                    .ok_or_else(|| err("MOTPAR before any MOT line".into()))?;
                motor
                    .params
                    .push((key.trim().to_string(), value.trim().to_string()));
            } else if let Some(body) = numbered(line, "MOT") {
                cfg.motors.push(motor_line(body).map_err(err)?);
            } else if let Some(body) = numbered(line, "CNT") {
                cfg.counters.push(counter_line(body).map_err(err)?);
            }
        }
        Ok(cfg)
    }

    /// The first motor with `mne`; SPEC's `motor_num` finds the first too.
    pub fn motor(&self, mne: &str) -> Option<usize> {
        self.motors.iter().position(|m| m.mne == mne)
    }

    pub fn counter(&self, mne: &str) -> Option<usize> {
        self.counters.iter().position(|c| c.mne == mne)
    }

    /// Mnemonics configured more than once; only the first is reachable.
    pub fn duplicate_motors(&self) -> Vec<&str> {
        let mut dups = Vec::new();
        for (i, m) in self.motors.iter().enumerate() {
            if self.motor(&m.mne) != Some(i) && !dups.contains(&m.mne.as_str()) {
                dups.push(m.mne.as_str());
            }
        }
        dups
    }
}

/// The text after `PREFIXnnn =`.
fn numbered<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(prefix)?;
    let (num, body) = rest.split_once('=')?;
    let num = num.trim();
    (!num.is_empty() && num.bytes().all(|b| b.is_ascii_digit())).then_some(body)
}

fn number(field: &str, what: &str) -> Result<f64, String> {
    field
        .parse()
        .map_err(|_| format!("{what} '{field}' is not a number"))
}

fn hex(field: &str) -> Result<u32, String> {
    let digits = field.trim_start_matches("0x");
    u32::from_str_radix(digits, 16).map_err(|_| format!("flags '{field}' is not hex"))
}

/// `CTRL:unit/chan steps sign slew base backlash accel nada flags mne name...`
fn motor_line(body: &str) -> Result<MotorConfig, String> {
    let f: Vec<&str> = body.split_whitespace().collect();
    if f.len() < 11 {
        return Err(format!("MOT line has {} fields, needs 11", f.len()));
    }
    let controller = f[0].split(':').next().unwrap_or(f[0]).to_string();
    Ok(MotorConfig {
        controller,
        steps: number(f[1], "steps")?,
        sign: number(f[2], "sign")?,
        slew: number(f[3], "slew rate")?,
        base: number(f[4], "base rate")?,
        backlash: number(f[5], "backlash")?,
        accel: number(f[6], "acceleration")?,
        flags: hex(f[8])?,
        mne: f[9].to_string(),
        name: f[10..].join(" "),
        params: Vec::new(),
    })
}

/// `type unit chan scale flags mne name...`
fn counter_line(body: &str) -> Result<CounterConfig, String> {
    let f: Vec<&str> = body.split_whitespace().collect();
    if f.len() < 7 {
        return Err(format!("CNT line has {} fields, needs 7", f.len()));
    }
    Ok(CounterConfig {
        kind: f[0].to_string(),
        scale: number(f[3], "scale")?,
        mne: f[5].to_string(),
        name: f[6..].join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# comment
SDEV_3\t = SOCKET:192.168.50.93:4001 9600 cooked
MOT000 =    NONE:1/0  10000  1  4000  100  600  300    0 0x003    tthxx  Two Theta
MOTPAR:encoder_step_size = -40000
MOTPAR:step_mode = 1
MOT010 =     OMS:6/7    500 -1   200  200  -25  125    0 0x003    shold  512Z
MOT069 =    NONE:3/6   1000  1  1000  200    0  125    0 0x003   mcelly  a
MOT116 =    NONE:3/6   1000  1  1000  200    0  125    0 0x003   mcelly  b
CNT000 =  SFTWARE  0  0 10000000 0x001      sec  Seconds
CNT004 = EPICS_PV  0  0  1e-09 0x000       pd  PD
";

    #[test]
    fn motor_counter_and_motpar_lines() {
        let cfg = Config::parse(SAMPLE).unwrap();
        assert_eq!(cfg.motors.len(), 4);
        let tthxx = &cfg.motors[0];
        assert_eq!(
            (
                tthxx.mne.as_str(),
                tthxx.name.as_str(),
                tthxx.controller.as_str()
            ),
            ("tthxx", "Two Theta", "NONE")
        );
        assert_eq!(
            tthxx.params,
            [
                ("encoder_step_size".to_string(), "-40000".to_string()),
                ("step_mode".to_string(), "1".to_string())
            ]
        );
        let shold = &cfg.motors[cfg.motor("shold").unwrap()];
        assert_eq!(
            (shold.steps, shold.sign, shold.slew, shold.base),
            (500.0, -1.0, 200.0, 200.0)
        );
        assert_eq!(
            (shold.backlash, shold.accel, shold.flags),
            (-25.0, 125.0, 3)
        );
        assert_eq!(cfg.counters[0].scale, 1e7);
        assert_eq!(cfg.counters[1].mne, "pd");
        assert_eq!(cfg.counters[1].scale, 1e-9);
    }

    #[test]
    fn duplicate_mnemonic_resolves_to_the_first() {
        let cfg = Config::parse(SAMPLE).unwrap();
        assert_eq!(cfg.motor("mcelly"), Some(2));
        assert_eq!(cfg.duplicate_motors(), ["mcelly"]);
    }

    #[test]
    fn short_line_is_an_error_with_its_number() {
        let e = Config::parse("MOT000 = NONE:0/0 1 1\n").unwrap_err();
        assert_eq!(e.line, 1);
    }

    #[test]
    fn beamline_config_parses() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../ref/spec/kappa.config");
        let cfg = Config::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(cfg.motors.len(), 118);
        assert_eq!(cfg.counters.len(), 23);
        let tth = &cfg.motors[cfg.motor("tth").unwrap()];
        assert_eq!((tth.controller.as_str(), tth.steps), ("EPICS_M2", 10000.0));
        assert_eq!(cfg.duplicate_motors(), ["mcelly"]);
    }
}
