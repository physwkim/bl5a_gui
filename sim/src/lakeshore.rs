//! The LakeShore 331 on ser 9 as temperature.mac drives it: loop 1's
//! setpoint (`SETP 1,T`) and ramp (`RAMP 1,on,rate`), read back on input
//! A (`CRDG? A`), all in °C. The sample follows the setpoint the loop is
//! controlling to with a first-order lag, heating and cooling alike; the
//! heater range and PID are not modeled.

/// The sample's lag behind the controlled setpoint, s: short enough for
/// the sim's runs, not a measured value.
const LAG: f64 = 10.0;

pub struct Lakeshore {
    /// `SETP 1`: where the setpoint is going.
    target: f64,
    /// The setpoint loop 1 controls to: `target`, or on its way there at
    /// `ramp`.
    setpoint: f64,
    /// `RAMP 1,1,rate`: K/min; `None` while ramping is off.
    ramp: Option<f64>,
    /// Input A, °C.
    pub reading: f64,
}

impl Lakeshore {
    /// Settled at `t`, ramping off.
    pub fn new(t: f64) -> Lakeshore {
        Lakeshore {
            target: t,
            setpoint: t,
            ramp: None,
            reading: t,
        }
    }

    /// A command line on ser 9. Queries change nothing, and loops other
    /// than 1 are not modeled.
    pub fn command(&mut self, text: &str) {
        let t = text.trim();
        let args = |rest: &str| -> Option<Vec<f64>> {
            rest.split(',').map(|a| a.trim().parse().ok()).collect()
        };
        if let Some(rest) = t.strip_prefix("SETP ")
            && let Some(&[l, v]) = args(rest).as_deref()
            && l == 1.0
        {
            self.target = v;
            if self.ramp.is_none() {
                self.setpoint = v;
            }
        } else if let Some(rest) = t.strip_prefix("RAMP ")
            && let Some(&[l, on, rate]) = args(rest).as_deref()
            && l == 1.0
        {
            self.ramp = (on != 0.0).then_some(rate);
            if self.ramp.is_none() {
                self.setpoint = self.target;
            }
        }
    }

    /// `dt` seconds on.
    pub fn advance(&mut self, dt: f64) {
        self.setpoint = match self.ramp {
            Some(rate) => {
                let step = rate / 60.0 * dt;
                self.setpoint + (self.target - self.setpoint).clamp(-step, step)
            }
            None => self.target,
        };
        self.reading += (self.setpoint - self.reading) * (1.0 - (-dt / LAG).exp());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(tc: &mut Lakeshore, seconds: f64) {
        for _ in 0..(seconds * 50.0) as usize {
            tc.advance(0.02);
        }
    }

    #[test]
    fn without_a_ramp_the_sample_lags_the_setpoint() {
        let mut tc = Lakeshore::new(25.0);
        tc.command("SETP 1,35\r\n");
        run(&mut tc, LAG);
        assert!((tc.reading - (35.0 - 10.0 / std::f64::consts::E)).abs() < 0.01);
        run(&mut tc, 10.0 * LAG);
        assert!((tc.reading - 35.0).abs() < 1e-3);
    }

    #[test]
    fn a_ramp_moves_the_setpoint_at_its_rate() {
        let mut tc = Lakeshore::new(25.0);
        tc.command("RAMP 1, 1, 10\r\n");
        tc.command("SETP 1,30\r\n");
        run(&mut tc, 15.0);
        assert!((tc.setpoint - 27.5).abs() < 1e-9);
        run(&mut tc, 15.0);
        assert_eq!(tc.setpoint, 30.0);
        // Cooling ramps the same way.
        tc.command("SETP 1,025\r\n");
        run(&mut tc, 6.0);
        assert!((tc.setpoint - 29.0).abs() < 1e-9);
        // Ramping off: the setpoint goes to the target at once.
        tc.command("RAMP 1, 0, 10\r\n");
        assert_eq!(tc.setpoint, 25.0);
    }

    #[test]
    fn queries_and_other_loops_change_nothing() {
        let mut tc = Lakeshore::new(25.0);
        for q in [
            "RAMP? A\r\n",
            "CRDG? A\r\n",
            "SETP? 1\r\n",
            "SETP 2,80\r\n",
            "RAMP 2,1,5\r\n",
        ] {
            tc.command(q);
        }
        assert_eq!((tc.target, tc.setpoint, tc.ramp), (25.0, 25.0, None));
    }
}
