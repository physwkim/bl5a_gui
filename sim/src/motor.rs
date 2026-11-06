//! One SPEC motor: dial position moved along a base-rate / slew-rate
//! trapezoid, user = sign × dial + offset.
//!
//! Positions are dial units; `steps` (per unit) only quantizes them and
//! converts the rates, which SPEC keeps in steps/s. A move ends on a whole
//! step. Backlash is not modelled: the final approach is the plain move.

use std::time::Duration;

use tokio::time::Instant;

use crate::config::MotorConfig;

#[derive(Clone, Copy, Debug)]
struct Segment {
    start: Instant,
    dur: f64,
    x0: f64,
    /// Signed, dial units/s and /s².
    v0: f64,
    a: f64,
}

impl Segment {
    fn at(&self, t: Instant) -> (f64, f64) {
        let tau = t
            .saturating_duration_since(self.start)
            .as_secs_f64()
            .min(self.dur);
        (
            self.x0 + self.v0 * tau + 0.5 * self.a * tau * tau,
            self.v0 + self.a * tau,
        )
    }

    fn end(&self) -> Instant {
        self.start + Duration::from_secs_f64(self.dur)
    }
}

#[derive(Clone, Debug)]
struct Profile {
    segments: Vec<Segment>,
    /// Quantized dial position the profile ends on.
    target: f64,
    up: bool,
}

impl Profile {
    fn end(&self) -> Instant {
        self.segments.last().map_or(Instant::now(), Segment::end)
    }

    fn at(&self, t: Instant) -> (f64, f64) {
        match self.segments.iter().find(|s| t < s.end()) {
            Some(s) => s.at(t),
            None => (self.target, 0.0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Motor {
    pub mne: String,
    pub steps: f64,
    pub sign: f64,
    /// steps/s, steps/s, ms, steps: SPEC's `motor_par` units.
    pub slew: f64,
    /// The config file's slew rate, which `motor_par` writes leave alone.
    pub config_slew: f64,
    pub base: f64,
    pub accel: f64,
    pub backlash: f64,
    pub offset: f64,
    /// Software limits, dial.
    pub low_limit: f64,
    pub high_limit: f64,
    /// Limit-switch positions, dial; `None` has no switches.
    pub switches: Option<(f64, f64)>,
    /// Continuous dial position.
    x: f64,
    profile: Option<Profile>,
}

impl Motor {
    pub fn new(cfg: &MotorConfig, dial: f64, limits: (f64, f64)) -> Motor {
        let mut m = Motor {
            mne: cfg.mne.clone(),
            steps: cfg.steps,
            sign: cfg.sign,
            slew: cfg.slew,
            config_slew: cfg.slew,
            base: cfg.base,
            accel: cfg.accel,
            backlash: cfg.backlash,
            offset: 0.0,
            low_limit: limits.0.min(limits.1),
            high_limit: limits.0.max(limits.1),
            switches: None,
            x: 0.0,
            profile: None,
        };
        m.x = m.quantize(dial);
        m
    }

    pub fn quantize(&self, dial: f64) -> f64 {
        if self.steps == 0.0 {
            dial
        } else {
            (dial * self.steps).round() / self.steps
        }
    }

    /// Reported dial position: the whole step the motor is on.
    pub fn dial(&self) -> f64 {
        self.quantize(self.x)
    }

    pub fn user(&self) -> f64 {
        self.user_of(self.dial())
    }

    pub fn user_of(&self, dial: f64) -> f64 {
        self.sign * dial + self.offset
    }

    pub fn dial_of(&self, user: f64) -> f64 {
        (user - self.offset) * self.sign
    }

    /// The position in the beamline frame the physics uses: user units
    /// without the offset `set` adds.
    pub fn lab_at(&self, t: Instant) -> f64 {
        let x = self.profile.as_ref().map_or(self.x, |p| p.at(t).0);
        self.sign * x
    }

    pub fn moving(&self) -> bool {
        self.profile.is_some()
    }

    /// (low, high) limit switch active.
    pub fn limit_hit(&self) -> (bool, bool) {
        let d = self.dial();
        self.switches
            .map_or((false, false), |(lo, hi)| (d <= lo, d >= hi))
    }

    /// Dial units/s.
    fn rates(&self) -> (f64, f64, f64) {
        let spu = self.steps.abs().max(f64::MIN_POSITIVE);
        let vs = self.slew.abs() / spu;
        let vb = self.base.abs().min(self.slew.abs()) / spu;
        let ta = self.accel.max(0.0) / 1000.0;
        let a = if ta > 0.0 && vs > vb {
            (vs - vb) / ta
        } else {
            f64::INFINITY
        };
        (vs, vb, a)
    }

    pub fn start(&mut self, now: Instant, dial: f64) {
        let target = self.quantize(dial);
        let from = self.x;
        let d = target - from;
        if d == 0.0 {
            return;
        }
        let dir = d.signum();
        let dist = d.abs();
        let (vs, vb, a) = self.rates();
        let seg = |start: f64, dur: f64, x0: f64, v0: f64, a: f64| Segment {
            start: now + Duration::from_secs_f64(start),
            dur,
            x0,
            v0: dir * v0,
            a: dir * a,
        };
        let segments = if a.is_infinite() {
            vec![seg(0.0, dist / vs, from, vs, 0.0)]
        } else {
            let ta = (vs - vb) / a;
            let da = (vb + vs) / 2.0 * ta;
            if dist >= 2.0 * da {
                let tc = (dist - 2.0 * da) / vs;
                vec![
                    seg(0.0, ta, from, vb, a),
                    seg(ta, tc, from + dir * da, vs, 0.0),
                    seg(ta + tc, ta, target - dir * da, vs, -a),
                ]
            } else {
                let vp = (vb * vb + a * dist).sqrt();
                let tu = (vp - vb) / a;
                vec![
                    seg(0.0, tu, from, vb, a),
                    seg(tu, tu, from + dir * dist / 2.0, vp, -a),
                ]
            }
        };
        self.profile = Some(Profile {
            segments,
            target,
            up: dir > 0.0,
        });
    }

    /// Decelerates to the base rate and stops, as a controller abort does.
    pub fn stop(&mut self, now: Instant) {
        let Some(p) = &self.profile else { return };
        let (x, v) = p.at(now);
        let (_, vb, a) = self.rates();
        let speed = v.abs();
        if a.is_infinite() || speed <= vb {
            self.x = self.quantize(x);
            self.profile = None;
            return;
        }
        let dir = v.signum();
        let dur = (speed - vb) / a;
        let end = x + dir * (speed * speed - vb * vb) / (2.0 * a);
        self.profile = Some(Profile {
            segments: vec![Segment {
                start: now,
                dur,
                x0: x,
                v0: v,
                a: -dir * a,
            }],
            target: self.quantize(end),
            up: p.up,
        });
    }

    pub fn advance(&mut self, now: Instant) {
        let Some(p) = &self.profile else { return };
        let done = now >= p.end();
        let up = p.up;
        self.x = if done { p.target } else { p.at(now).0 };
        if done {
            self.profile = None;
        }
        if let Some((lo, hi)) = self.switches
            && ((up && self.x >= hi) || (!up && self.x <= lo))
        {
            self.x = if up { hi } else { lo };
            self.profile = None;
        }
    }

    /// The dial position the motor comes to rest on, unless a switch stops
    /// it first.
    pub fn destination(&self) -> f64 {
        self.profile.as_ref().map_or(self.dial(), |p| p.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(steps: f64, slew: f64, base: f64, accel: f64) -> MotorConfig {
        MotorConfig {
            mne: "m".into(),
            name: "m".into(),
            controller: "OMS".into(),
            steps,
            sign: -1.0,
            slew,
            base,
            backlash: 0.0,
            accel,
            flags: 3,
            params: Vec::new(),
        }
    }

    /// When the current move ends, if nothing stops it first.
    fn end(m: &Motor) -> Instant {
        m.profile.as_ref().unwrap().end()
    }

    fn secs(m: &Motor, t0: Instant) -> f64 {
        (end(m) - t0).as_secs_f64()
    }

    #[tokio::test(start_paused = true)]
    async fn trapezoid_takes_ramps_plus_cruise() {
        // 1000 steps/unit, slew 2000, base 200, 125 ms: ramps cover
        // (0.2 + 2) / 2 * 0.125 = 0.1375 units each.
        let t0 = Instant::now();
        let mut m = Motor::new(&cfg(1000.0, 2000.0, 200.0, 125.0), 0.0, (-1e4, 1e4));
        m.start(t0, 10.0);
        let want = 0.25 + (10.0 - 0.275) / 2.0;
        assert!((secs(&m, t0) - want).abs() < 1e-9);
        let (x, v) = m.profile.as_ref().unwrap().at(t0 + Duration::from_secs(1));
        assert!((v - 2.0).abs() < 1e-12);
        assert!((x - (0.1375 + 2.0 * 0.875)).abs() < 1e-9);
        m.advance(end(&m));
        assert_eq!((m.dial(), m.moving()), (10.0, false));
        assert_eq!(m.user(), -10.0);
    }

    #[tokio::test(start_paused = true)]
    async fn short_move_peaks_below_slew() {
        let t0 = Instant::now();
        let mut m = Motor::new(&cfg(1000.0, 2000.0, 200.0, 125.0), 0.0, (-1e4, 1e4));
        m.start(t0, -0.1);
        let a = 1.8 / 0.125;
        let vp = (0.04f64 + a * 0.1).sqrt();
        assert!((secs(&m, t0) - 2.0 * (vp - 0.2) / a).abs() < 1e-9);
    }

    #[tokio::test(start_paused = true)]
    async fn equal_rates_move_at_constant_speed() {
        let t0 = Instant::now();
        let mut m = Motor::new(&cfg(500.0, 200.0, 200.0, 125.0), 0.0, (-1e4, 1e4));
        m.start(t0, 1.0);
        assert!((secs(&m, t0) - 2.5).abs() < 1e-9);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_decelerates_onto_a_step() {
        let t0 = Instant::now();
        let mut m = Motor::new(&cfg(1000.0, 2000.0, 200.0, 125.0), 0.0, (-1e4, 1e4));
        m.start(t0, 10.0);
        let t1 = t0 + Duration::from_secs(1);
        let (x, _) = m.profile.as_ref().unwrap().at(t1);
        m.stop(t1);
        let stop = end(&m);
        assert!(((stop - t1).as_secs_f64() - 0.125).abs() < 1e-9);
        m.advance(stop);
        assert!(!m.moving());
        assert_eq!(m.dial(), m.quantize(x + 0.1375));
    }

    #[tokio::test(start_paused = true)]
    async fn switch_stops_the_motor_and_reads_as_hit() {
        let t0 = Instant::now();
        let mut m = Motor::new(&cfg(1000.0, 2000.0, 200.0, 125.0), 0.0, (-1e4, 1e4));
        m.switches = Some((-1.0, 2.0));
        m.start(t0, 10.0);
        m.advance(t0 + Duration::from_secs(3));
        assert_eq!(
            (m.dial(), m.moving(), m.limit_hit()),
            (2.0, false, (false, true))
        );
        m.start(t0, 1.0);
        m.advance(t0 + Duration::from_secs(5));
        assert_eq!((m.dial(), m.limit_hit()), (1.0, (false, false)));
    }

    #[test]
    fn half_step_does_not_move() {
        let m = Motor::new(&cfg(1000.0, 2000.0, 200.0, 125.0), 0.0, (-1e4, 1e4));
        assert_eq!(m.quantize(0.0004), 0.0);
        assert_eq!(m.quantize(0.0006), 0.001);
        assert_eq!(m.dial_of(m.user_of(3.0)), 3.0);
    }
}
