//! One SPEC motor as an asyn motor axis.
//!
//! Raw positions are SPEC user positions times |step_size|, so the record's
//! MRES is 1/|step_size| and, with DIR=Pos and OFF=0, its VAL is SPEC's user
//! position. Rates are steps/s on both sides.

use std::sync::Arc;

use epics_rs::asyn::error::{AsynError, AsynResult, AsynStatus};
use epics_rs::asyn::interfaces::motor::{AsynMotor, MotorStatus, PollFailure};
use epics_rs::asyn::user::AsynUser;

use crate::{Link, Live, Motion, Op, Shared};

pub(crate) struct SpecAxis {
    shared: Arc<Shared>,
    index: usize,
}

/// A rate the record sends differs from SPEC's own, so it must be sent.
fn changed(record: f64, spec: f64) -> Option<f64> {
    (record > 0.0 && (record - spec).abs() > 1e-9 * spec.abs().max(1.0)).then_some(record)
}

impl SpecAxis {
    pub(crate) fn new(shared: Arc<Shared>, index: usize) -> SpecAxis {
        SpecAxis { shared, index }
    }

    fn mne(&self) -> &str {
        &self.shared.axes[self.index]
    }

    /// `accept` on this axis when SPEC can take a move; a refusal is logged
    /// and fails the record's request.
    fn submit(&self, what: &str, accept: impl FnOnce(&mut Live) -> Op) -> AsynResult<()> {
        let index = self.index;
        let result = self.shared.submit(|live| {
            if let Some(why) = live.busy() {
                return Err(why.to_string());
            }
            if live.axes[index].motion != Motion::Idle {
                return Err("it is moving".into());
            }
            Ok(accept(live))
        });
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

impl AsynMotor for SpecAxis {
    fn move_absolute(
        &mut self,
        _user: &AsynUser,
        position: f64,
        min_velocity: f64,
        velocity: f64,
        _acceleration: f64,
    ) -> AsynResult<()> {
        let index = self.index;
        self.submit("move", |live| {
            live.queued += 1;
            let a = &mut live.axes[index];
            let slew = changed(velocity, a.slew);
            let base = changed(min_velocity, a.base);
            a.slew = slew.unwrap_or(a.slew);
            a.base = base.unwrap_or(a.base);
            a.up = position > a.position * a.steps;
            a.motion = Motion::Starting;
            Op::Move {
                axis: index,
                user: position / a.steps,
                slew,
                base,
            }
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
            Op::SetPosition {
                axis: index,
                user: position / live.axes[index].steps,
            }
        })
    }

    fn poll(&mut self, _user: &AsynUser, status: &mut MotorStatus) -> AsynResult<()> {
        let st = self.shared.lock();
        let Link::Up(live) = &st.link else {
            status.comms_error = true;
            status.done = true;
            status.moving = false;
            status.msta_moving = false;
            return Ok(());
        };
        let a = &live.axes[self.index];
        let moving = a.motion != Motion::Idle;
        status.set_position(a.position * a.steps);
        status.comms_error = false;
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
}
