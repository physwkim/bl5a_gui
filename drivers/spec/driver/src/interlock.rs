//! A collision interlock between an arm and the motors that must not move
//! while it is down (at BL5A, tth and the mar345's motors).
//!
//! MUST: a guarded motor moves only while the arm stands at or above
//! `clear` and is not bound below it; the arm goes below `clear` only
//! while `out` stands at or above `away` and no guarded motor moves.
//!
//! The motor records' moves are refused under the state lock when they
//! would break it ([`Interlock::refuse`]). SPEC's own moves (a macro, a
//! command) cannot be refused, so the session sends SPEC a ^C, which stops
//! every motor, once they have broken it ([`Interlock::broken`]). Positions
//! are SPEC user positions, as the macros compare them.

use crate::AxisLive;

/// `SpecInterlock`'s arguments, by mnemonic, until SpecCreate names the
/// axes.
#[derive(Debug)]
pub(crate) struct Named {
    pub(crate) arm: String,
    pub(crate) clear: f64,
    pub(crate) out: String,
    pub(crate) away: f64,
    pub(crate) guarded: Vec<String>,
}

impl Named {
    /// The interlock on SpecCreate's motor `names`.
    pub(crate) fn resolve(&self, names: &[String]) -> Result<Interlock, String> {
        let index = |mne: &str| {
            names
                .iter()
                .position(|n| n == mne)
                .ok_or_else(|| format!("{mne} is not one of SpecCreate's motors"))
        };
        let guarded = self
            .guarded
            .iter()
            .map(|m| index(m))
            .collect::<Result<Vec<_>, _>>()?;
        let arm = index(&self.arm)?;
        if guarded.is_empty() {
            return Err("no guarded motors".into());
        }
        if guarded.contains(&arm) {
            return Err(format!("{} is the arm and guarded", self.arm));
        }
        Ok(Interlock {
            arm,
            clear: self.clear,
            out: index(&self.out)?,
            away: self.away,
            guarded,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Interlock {
    arm: usize,
    clear: f64,
    out: usize,
    away: f64,
    guarded: Vec<usize>,
}

impl Interlock {
    /// Why a move of `axis` to `target` (user units) would break the
    /// interlock now.
    pub(crate) fn refuse(
        &self,
        names: &[String],
        axes: &[AxisLive],
        axis: usize,
        target: f64,
    ) -> Option<String> {
        let (arm, mne, clear) = (&axes[self.arm], &names[self.arm], self.clear);
        if self.guarded.contains(&axis) {
            if arm.position < clear {
                return Some(format!("{mne} {} is below {clear}", arm.position));
            }
            if arm.motion.moving() {
                return match arm.motion.target() {
                    Some(t) if t >= clear => None,
                    Some(t) => Some(format!("{mne} is moving to {t}, below {clear}")),
                    None => Some(format!("{mne} is moving on SPEC's own command")),
                };
            }
        }
        if axis == self.arm && target < clear {
            let out = &axes[self.out];
            if out.position < self.away {
                return Some(format!(
                    "{} {} is below {}",
                    names[self.out], out.position, self.away
                ));
            }
            if let Some(&m) = self.guarded.iter().find(|&&m| axes[m].motion.moving()) {
                return Some(format!("{} is moving", names[m]));
            }
        }
        None
    }

    /// What breaks the interlock in `axes` as SPEC reports them, after an
    /// event of `axis` that took its position down when `fell`. Only motion
    /// breaks it: a stopped arm below `clear` with `out` in is left for a
    /// move up.
    pub(crate) fn broken(
        &self,
        names: &[String],
        axes: &[AxisLive],
        axis: usize,
        fell: bool,
    ) -> Option<String> {
        let (arm, mne, clear) = (&axes[self.arm], &names[self.arm], self.clear);
        if arm.position >= clear {
            return None;
        }
        if let Some(&m) = self.guarded.iter().find(|&&m| axes[m].motion.moving()) {
            return Some(format!(
                "{} moves while {mne} {} is below {clear}",
                names[m], arm.position
            ));
        }
        let out = &axes[self.out];
        if axis == self.arm && fell && arm.motion.moving() && out.position < self.away {
            return Some(format!(
                "{mne} {} goes below {clear} while {} {} is below {}",
                arm.position, names[self.out], out.position, self.away
            ));
        }
        None
    }

    /// After SPEC's event of `axis`: why to send SPEC a ^C, once while its
    /// axes are under way; `sent` says it was, until they all stop.
    pub(crate) fn watch(
        &self,
        names: &[String],
        axes: &[AxisLive],
        sent: &mut bool,
        axis: usize,
        fell: bool,
    ) -> Option<String> {
        if !self.involves(axis) {
            return None;
        }
        if !self.moving(axes) {
            *sent = false;
            return None;
        }
        if *sent {
            return None;
        }
        let why = self.broken(names, axes, axis, fell)?;
        *sent = true;
        Some(why)
    }

    /// The axes it reads.
    fn involves(&self, axis: usize) -> bool {
        axis == self.arm || axis == self.out || self.guarded.contains(&axis)
    }

    /// Any of the axes it reads under way.
    fn moving(&self, axes: &[AxisLive]) -> bool {
        (0..axes.len()).any(|i| self.involves(i) && axes[i].motion.moving())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Motion;

    const NAMES: [&str; 4] = ["tth", "martran", "marsdd", "th"];
    const TTH: usize = 0;
    const MARTRAN: usize = 1;
    const MARSDD: usize = 2;
    const TH: usize = 3;

    fn names() -> Vec<String> {
        NAMES.map(String::from).to_vec()
    }

    fn named(arm: &str, guarded: &[&str]) -> Named {
        Named {
            arm: arm.into(),
            clear: 40.0,
            out: "martran".into(),
            away: 1200.0,
            guarded: guarded.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn lock() -> Interlock {
        named("tth", &["martran", "marsdd"])
            .resolve(&names())
            .unwrap()
    }

    /// Every axis still, at `tth` and `martran`.
    fn at(tth: f64, martran: f64) -> Vec<AxisLive> {
        let mut axes = vec![AxisLive::default(); NAMES.len()];
        axes[TTH].position = tth;
        axes[MARTRAN].position = martran;
        axes
    }

    #[test]
    fn its_motors_are_named_in_specs_list() {
        let resolve = |arm, guarded: &[&str]| named(arm, guarded).resolve(&names()).map(|_| ());
        assert_eq!(
            resolve("tth", &["marz"]),
            Err("marz is not one of SpecCreate's motors".into())
        );
        assert_eq!(
            resolve("2theta", &["martran"]),
            Err("2theta is not one of SpecCreate's motors".into())
        );
        assert_eq!(resolve("tth", &[]), Err("no guarded motors".into()));
        assert_eq!(
            resolve("tth", &["tth"]),
            Err("tth is the arm and guarded".into())
        );
        let l = lock();
        assert_eq!(
            (l.arm, l.out, l.guarded),
            (TTH, MARTRAN, vec![MARTRAN, MARSDD])
        );
    }

    #[test]
    fn a_guarded_motor_moves_only_with_the_arm_up() {
        let (l, n) = (lock(), names());
        assert_eq!(l.refuse(&n, &at(40.0, 25.0), MARSDD, 440.0), None);
        assert_eq!(
            l.refuse(&n, &at(39.99, 1234.0), MARTRAN, 25.0),
            Some("tth 39.99 is below 40".into())
        );
        // th is not guarded.
        assert_eq!(l.refuse(&n, &at(1.0, 25.0), TH, 3.0), None);
    }

    #[test]
    fn a_guarded_motor_moves_with_the_arm_bound_up_only() {
        let (l, n) = (lock(), names());
        let mut axes = at(45.0, 25.0);
        // martran asked for while tth, already up, moves further up.
        axes[TTH].motion = Motion::Moving { target: Some(81.0) };
        assert_eq!(l.refuse(&n, &axes, MARTRAN, 1234.0), None);
        axes[TTH].motion = Motion::Starting { target: 39.0 };
        assert_eq!(
            l.refuse(&n, &axes, MARTRAN, 1234.0),
            Some("tth is moving to 39, below 40".into())
        );
        axes[TTH].motion = Motion::Moving { target: None };
        assert_eq!(
            l.refuse(&n, &axes, MARTRAN, 1234.0),
            Some("tth is moving on SPEC's own command".into())
        );
        // Stopped, its last position being read.
        axes[TTH].motion = Motion::Settling;
        assert_eq!(l.refuse(&n, &axes, MARTRAN, 1234.0), None);
    }

    #[test]
    fn the_arm_goes_down_only_with_the_mar_out_and_still() {
        let (l, n) = (lock(), names());
        assert_eq!(l.refuse(&n, &at(42.0, 1200.0), TTH, 1.0), None);
        assert_eq!(
            l.refuse(&n, &at(42.0, 1199.0), TTH, 39.0),
            Some("martran 1199 is below 1200".into())
        );
        // Up, or down to clear, is always allowed.
        assert_eq!(l.refuse(&n, &at(1.0, 25.0), TTH, 45.0), None);
        assert_eq!(l.refuse(&n, &at(81.0, 25.0), TTH, 40.0), None);
        let mut axes = at(42.0, 1234.0);
        axes[MARSDD].motion = Motion::Moving { target: None };
        assert_eq!(
            l.refuse(&n, &axes, TTH, 1.0),
            Some("marsdd is moving".into())
        );
    }

    #[test]
    fn a_guarded_motor_moving_with_the_arm_down_breaks_it() {
        let (l, n) = (lock(), names());
        let mut axes = at(30.0, 1234.0);
        assert_eq!(l.broken(&n, &axes, MARSDD, false), None);
        axes[MARSDD].motion = Motion::Moving { target: None };
        assert_eq!(
            l.broken(&n, &axes, MARSDD, false),
            Some("marsdd moves while tth 30 is below 40".into())
        );
        axes[TTH].position = 40.0;
        assert_eq!(l.broken(&n, &axes, TTH, true), None);
    }

    #[test]
    fn the_arm_going_down_with_the_mar_in_breaks_it() {
        let (l, n) = (lock(), names());
        let mut axes = at(39.5, 25.0);
        axes[TTH].motion = Motion::Moving { target: None };
        assert_eq!(
            l.broken(&n, &axes, TTH, true),
            Some("tth 39.5 goes below 40 while martran 25 is below 1200".into())
        );
        // Going up out of it is the way out.
        assert_eq!(l.broken(&n, &axes, TTH, false), None);
        // The final position read after a stop.
        axes[TTH].motion = Motion::Settling;
        assert_eq!(l.broken(&n, &axes, TTH, true), None);
        let mut out = at(39.5, 1200.0);
        out[TTH].motion = Motion::Moving { target: None };
        assert_eq!(l.broken(&n, &out, TTH, true), None);
    }

    #[test]
    fn moving_reads_only_its_axes() {
        let l = lock();
        let mut axes = at(1.0, 1234.0);
        axes[TH].motion = Motion::Moving { target: None };
        assert!(!l.moving(&axes));
        axes[MARSDD].motion = Motion::Starting { target: 440.0 };
        assert!(l.moving(&axes));
    }

    #[test]
    fn one_abort_while_its_axes_are_under_way() {
        let (l, n) = (lock(), names());
        let mut sent = false;
        let mut axes = at(30.0, 1234.0);
        axes[MARSDD].motion = Motion::Moving { target: None };
        assert_eq!(
            l.watch(&n, &axes, &mut sent, MARSDD, false),
            Some("marsdd moves while tth 30 is below 40".into())
        );
        // Its next events, until it stops.
        assert_eq!(l.watch(&n, &axes, &mut sent, MARSDD, false), None);
        assert_eq!(l.watch(&n, &axes, &mut sent, TH, false), None);
        assert!(sent);
        axes[MARSDD].motion = Motion::Settling;
        assert_eq!(l.watch(&n, &axes, &mut sent, MARSDD, false), None);
        assert!(!sent);
        // The next move is the next episode.
        axes[MARTRAN].motion = Motion::Moving { target: None };
        assert_eq!(
            l.watch(&n, &axes, &mut sent, MARTRAN, false),
            Some("martran moves while tth 30 is below 40".into())
        );
    }

    #[test]
    fn no_abort_for_an_axis_it_does_not_read() {
        let (l, n) = (lock(), names());
        let mut sent = false;
        let mut axes = at(30.0, 1234.0);
        axes[MARSDD].motion = Motion::Moving { target: None };
        assert_eq!(l.watch(&n, &axes, &mut sent, TH, false), None);
        assert!(!sent);
    }
}
