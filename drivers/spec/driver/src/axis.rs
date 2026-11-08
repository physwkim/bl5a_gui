//! One SPEC motor as an asyn motor axis.
//!
//! Raw positions are SPEC user positions times |step_size|, so the record's
//! MRES is 1/|step_size| and, with DIR=Pos and OFF=0, its VAL is SPEC's user
//! position. Rates are steps/s on both sides.

use std::sync::Arc;

use epics_rs::asyn::error::{AsynError, AsynResult, AsynStatus};
use epics_rs::asyn::interfaces::motor::{AsynMotor, MotorStatus, PollFailure, StatusWrite};
use epics_rs::asyn::user::AsynUser;

use crate::{Link, Live, Motion, Op, Shared};

pub(crate) struct SpecAxis {
    shared: Arc<Shared>,
    index: usize,
    /// The status the last request set, posted with its DONE = 0.
    writes: Vec<StatusWrite>,
}

/// A rate the record sends differs from SPEC's own, so it must be sent.
fn changed(record: f64, spec: f64) -> Option<f64> {
    (record > 0.0 && (record - spec).abs() > 1e-9 * spec.abs().max(1.0)).then_some(record)
}

impl SpecAxis {
    pub(crate) fn new(shared: Arc<Shared>, index: usize) -> SpecAxis {
        SpecAxis {
            shared,
            index,
            writes: Vec::new(),
        }
    }

    fn mne(&self) -> &str {
        &self.shared.axes[self.index]
    }

    /// `accept` on this axis when SPEC can take a move. A refusal, by
    /// `accept` too, is logged, fails the record's request and leaves an
    /// idle axis `Refused`, so the record shows it.
    fn submit(
        &mut self,
        what: &str,
        accept: impl FnOnce(&mut Live) -> Result<Op, String>,
    ) -> AsynResult<()> {
        let index = self.index;
        let mut refused = None;
        let result = self.shared.submit(|live| {
            let decided = decide(live, index, accept);
            refused = Some(live.axes[index].motion == Motion::Refused);
            decided
        });
        // Posted with the request's DONE = 0, as a C move() sets it: the
        // record must not start a move on the last poll's problem.
        self.writes.extend(refused.map(StatusWrite::Problem));
        result.map_err(|why| {
            let line = format!("{} {what} refused: {why}", self.mne());
            self.shared.log(line.clone());
            AsynError::Status {
                status: AsynStatus::Error,
                message: line,
            }
        })
    }
}

/// What a request of axis `index` does to its motion: an accepted one
/// clears `Refused`, a refusal sets it on an idle axis.
fn decide(
    live: &mut Live,
    index: usize,
    accept: impl FnOnce(&mut Live) -> Result<Op, String>,
) -> Result<Op, String> {
    let why = match live.busy() {
        Some(why) => why.to_string(),
        None if !live.axes[index].motion.idle() => return Err("it is moving".into()),
        None => match accept(live) {
            Ok(op) => {
                let a = &mut live.axes[index];
                if a.motion == Motion::Refused {
                    a.motion = Motion::Idle;
                }
                return Ok(op);
            }
            Err(why) => why,
        },
    };
    let a = &mut live.axes[index];
    if a.motion.idle() {
        a.motion = Motion::Refused;
    }
    Err(why)
}

impl AsynMotor for SpecAxis {
    fn move_absolute(
        &mut self,
        _user: &AsynUser,
        position: f64,
        min_velocity: f64,
        velocity: f64,
        _acceleration: f64,
    ) -> AsynResult<()> {
        let (index, shared) = (self.index, self.shared.clone());
        self.submit("move", |live| {
            let target = position / live.axes[index].steps;
            if let Some(why) = shared.refuse(&live.axes, index, target) {
                return Err(why);
            }
            live.queued += 1;
            let a = &mut live.axes[index];
            let slew = changed(velocity, a.slew);
            let base = changed(min_velocity, a.base);
            a.slew = slew.unwrap_or(a.slew);
            a.base = base.unwrap_or(a.base);
            a.up = position > a.position * a.steps;
            a.motion = Motion::Starting { target };
            Ok(Op::Move {
                axis: index,
                user: target,
                slew,
                base,
            })
        })?;
        self.shared.wake(index);
        Ok(())
    }

    fn move_relative(
        &mut self,
        user: &AsynUser,
        distance: f64,
        min_velocity: f64,
        velocity: f64,
        acceleration: f64,
    ) -> AsynResult<()> {
        let from = {
            let st = self.shared.lock();
            match &st.link {
                Link::Up(live) => {
                    let a = &live.axes[self.index];
                    a.position * a.steps
                }
                _ => 0.0,
            }
        };
        self.move_absolute(user, from + distance, min_velocity, velocity, acceleration)
    }

    fn move_velocity(
        &mut self,
        _user: &AsynUser,
        _min_velocity: f64,
        _velocity: f64,
        _acceleration: f64,
    ) -> AsynResult<()> {
        Err(AsynError::Status {
            status: AsynStatus::Error,
            message: format!("{}: SPEC has no jog", self.mne()),
        })
    }

    fn home(
        &mut self,
        _user: &AsynUser,
        _min_velocity: f64,
        _velocity: f64,
        _acceleration: f64,
        _forward: bool,
    ) -> AsynResult<()> {
        Err(AsynError::Status {
            status: AsynStatus::Error,
            message: format!("{}: SPEC homes from its own macros", self.mne()),
        })
    }

    /// SPEC's only stop is `abort_all`, a ^C that also ends a count.
    fn stop(&mut self, _user: &AsynUser, _acceleration: f64) -> AsynResult<()> {
        self.shared
            .submit(|_| Ok(Op::Stop))
            .map_err(|message| AsynError::Status {
                status: AsynStatus::Error,
                message,
            })
    }

    fn set_position(&mut self, _user: &AsynUser, position: f64) -> AsynResult<()> {
        let index = self.index;
        self.submit("set", |live| {
            live.queued += 1;
            Ok(Op::SetPosition {
                axis: index,
                user: position / live.axes[index].steps,
            })
        })
    }

    fn poll(&mut self, _user: &AsynUser, status: &mut MotorStatus) -> AsynResult<()> {
        let st = self.shared.lock();
        let Link::Up(live) = &st.link else {
            status.comms_error = true;
            status.problem = false;
            status.done = true;
            status.moving = false;
            status.msta_moving = false;
            return Ok(());
        };
        let a = &live.axes[self.index];
        let moving = !a.motion.idle();
        status.set_position(a.position * a.steps);
        status.comms_error = false;
        // STATE/MAJOR on the record (C motorRecord alarm_sub).
        status.problem = a.motion == Motion::Refused;
        status.done = !moving;
        status.moving = moving;
        status.msta_moving = moving;
        status.direction = a.up;
        // SPEC's switches are dial-side; raw follows user.
        let (low, high) = if a.sign < 0.0 {
            (a.high_hit, a.low_hit)
        } else {
            (a.low_hit, a.high_hit)
        };
        status.low_limit = low;
        status.high_limit = high;
        status.powered = true;
        Ok(())
    }

    fn poll_failure(&self) -> PollFailure {
        PollFailure::COMMS_ERROR
    }

    fn take_status_writes(&mut self) -> Vec<StatusWrite> {
        std::mem::take(&mut self.writes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interlock::Named;
    use crate::{AxisLive, Request};
    use epics_rs::base::runtime::sync::mpsc::UnboundedReceiver;

    const TTH: usize = 0;
    const MARTRAN: usize = 1;

    /// Connected and ready at tth 0, martran 1280, with the BL5A interlock.
    fn spec() -> (Arc<Shared>, UnboundedReceiver<Request>) {
        let names: Vec<String> = ["tth", "martran"].map(String::from).to_vec();
        let lock = Named {
            arm: "tth".into(),
            clear: 40.0,
            out: "martran".into(),
            away: 1200.0,
            guarded: vec!["martran".into()],
        }
        .resolve(&names)
        .unwrap();
        let (shared, rx) = Shared::new(names, vec![], Some(lock));
        let mut live = Live::new(1, 2);
        live.ready = true;
        for (a, at) in live.axes.iter_mut().zip([0.0, 1280.0]) {
            *a = AxisLive {
                steps: 1.0,
                sign: 1.0,
                position: at,
                ..AxisLive::default()
            };
        }
        shared.lock().link = Link::Up(live);
        (Arc::new(shared), rx)
    }

    fn motion(shared: &Shared, axis: usize) -> Motion {
        match &shared.lock().link {
            Link::Up(live) => live.axes[axis].motion,
            _ => unreachable!(),
        }
    }

    fn polled(axis: &mut SpecAxis) -> MotorStatus {
        let mut status = MotorStatus::default();
        axis.poll(&AsynUser::new(0), &mut status).unwrap();
        status
    }

    fn move_to(axis: &mut SpecAxis, to: f64) -> AsynResult<()> {
        axis.move_absolute(&AsynUser::new(0), to, 0.0, 0.0, 0.0)
    }

    #[test]
    fn a_refused_request_is_a_problem_until_one_is_accepted() {
        let (shared, _rx) = spec();
        let mut martran = SpecAxis::new(shared.clone(), MARTRAN);
        assert!(move_to(&mut martran, 1300.0).is_err());
        assert_eq!(motion(&shared, MARTRAN), Motion::Refused);
        assert_eq!(martran.take_status_writes(), [StatusWrite::Problem(true)]);
        let s = polled(&mut martran);
        assert!(s.problem && s.done && !s.moving, "{s:?}");

        martran.set_position(&AsynUser::new(0), 1281.0).unwrap();
        assert_eq!(motion(&shared, MARTRAN), Motion::Idle);
        assert_eq!(martran.take_status_writes(), [StatusWrite::Problem(false)]);
        assert!(!polled(&mut martran).problem);

        assert!(move_to(&mut martran, 1300.0).is_err());
        let mut tth = SpecAxis::new(shared.clone(), TTH);
        move_to(&mut tth, 81.0).unwrap();
        assert_eq!(
            motion(&shared, MARTRAN),
            Motion::Refused,
            "another axis's move"
        );
        assert!(!polled(&mut tth).problem);
    }

    #[test]
    fn spec_busy_refuses_an_idle_axis_but_leaves_a_moving_one() {
        let (shared, _rx) = spec();
        let mut tth = SpecAxis::new(shared.clone(), TTH);
        let busy = |moving: Motion| {
            if let Link::Up(live) = &mut shared.lock().link {
                live.ready = false;
                live.axes[TTH].motion = moving;
            }
        };
        busy(Motion::Idle);
        assert!(move_to(&mut tth, 1.0).is_err());
        assert!(polled(&mut tth).problem);

        let spec_moves_it = Motion::Moving { target: None };
        busy(spec_moves_it);
        tth.take_status_writes();
        assert!(move_to(&mut tth, 1.0).is_err());
        assert_eq!(motion(&shared, TTH), spec_moves_it);
        assert_eq!(tth.take_status_writes(), [StatusWrite::Problem(false)]);
        let s = polled(&mut tth);
        assert!(!s.problem && s.moving, "{s:?}");
    }
}
