//! A SPEC server-mode client as asyn ports.
//!
//! [`session`] owns the one TCP connection to SPEC: it registers the motor,
//! counter and status properties, keeps their latest values in [`State`],
//! and posts what records show. The record-facing drivers ([`axis`],
//! [`scaler`], [`port`]) only read that state and submit operations; an
//! operation is accepted or refused under the state lock, so a refusal
//! (not connected, SPEC busy, the axis already moving) is decided on the
//! same state the session updates.
//!
//! SPEC runs its command queue one command at a time. A move or a count is
//! a `start_one` / `count` send followed by an empty SV_CMD_WITH_RETURN,
//! whose reply says SPEC has run everything sent before it; that barrier is
//! what resolves a move that never starts (the motor is already there) and
//! a count SPEC refused.

mod axis;
mod interlock;
pub mod ioc;
mod log;
mod points;
mod port;
mod scaler;
mod session;

pub use log::LOG_NELM;
pub use points::SCAN_NELM;
pub use port::{INTERFACES, INTERRUPTS};
pub use scaler::SpecScaler;

use std::sync::{Mutex, MutexGuard, OnceLock};

use epics_rs::base::runtime::sync::mpsc;
use epics_rs::motor::poll_loop::PollCommand;
use epics_rs::scaler::device_support::scaler_asyn::DoneParam;

use interlock::Interlock;

/// The scaler's clock: the first channel is SPEC's timer, in seconds, and
/// the record counts it in ticks of this rate.
pub const TIMER_FREQ: f64 = 1e7;

pub(crate) struct Shared {
    state: Mutex<State>,
    requests: mpsc::UnboundedSender<Request>,
    /// One per axis, in axis order, set once the axes are installed.
    pollers: OnceLock<Vec<mpsc::Sender<PollCommand>>>,
    pub(crate) done: DoneParam,
    pub(crate) axes: Vec<String>,
    /// Counter mnemonics; the first is the timer, the second the monitor.
    pub(crate) channels: Vec<String>,
    pub(crate) interlock: Option<Interlock>,
}

pub(crate) struct State {
    pub(crate) link: Link,
    /// The latest SPEC value of each channel.
    pub(crate) counts: Vec<f64>,
    /// What the scaler record wrote for the next count.
    pub(crate) presets: Vec<u32>,
}

/// The connection. Everything that holds only while connected lives in
/// [`Live`], so a disconnect drops it whole.
pub(crate) enum Link {
    Down,
    /// Registered, waiting for the initial reads.
    Connecting(Live),
    Up(Live),
}

pub(crate) struct Live {
    /// Which connection this is; an operation submitted on an earlier one
    /// is dropped.
    pub(crate) epoch: u64,
    /// SPEC is at its prompt, as `status/ready` last said.
    pub(crate) ready: bool,
    /// A `Cmd` command is running, or its completion marker is being read;
    /// held across both so no other operation runs before the read.
    pub(crate) command: bool,
    /// Moves, counts and sets sent whose barrier has not replied.
    pub(crate) queued: usize,
    pub(crate) axes: Vec<AxisLive>,
    pub(crate) count: Count,
    /// The ^C for the interlock's axes under way now has been sent.
    pub(crate) tripped: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AxisLive {
    /// |step_size|: raw steps per user unit, the record's 1/MRES.
    pub(crate) steps: f64,
    pub(crate) sign: f64,
    /// User units.
    pub(crate) position: f64,
    pub(crate) motion: Motion,
    pub(crate) low_hit: bool,
    pub(crate) high_hit: bool,
    /// steps/s.
    pub(crate) slew: f64,
    pub(crate) base: f64,
    pub(crate) config_slew: Option<f64>,
    /// The last move went toward higher raw positions.
    pub(crate) up: bool,
}

/// A motor's motion as the session has seen it. `target`, in user units,
/// is where a motor record sent it; SPEC's own moves have none.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum Motion {
    #[default]
    Idle,
    /// Idle, the motor record's last request refused: its record shows a
    /// problem until the motor moves or a request is accepted.
    Refused,
    /// `start_one` sent; neither `move_done` = 1 nor the barrier yet.
    Starting {
        target: f64,
    },
    Moving {
        target: Option<f64>,
    },
    /// Stopped; the final position is being read.
    Settling,
}

impl Motion {
    /// Nothing under way: a motor record's request may go.
    pub(crate) fn idle(self) -> bool {
        matches!(self, Motion::Idle | Motion::Refused)
    }

    pub(crate) fn moving(self) -> bool {
        matches!(self, Motion::Starting { .. } | Motion::Moving { .. })
    }

    pub(crate) fn target(self) -> Option<f64> {
        match self {
            Motion::Starting { target } => Some(target),
            Motion::Moving { target } => target,
            Motion::Idle | Motion::Refused | Motion::Settling => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Count {
    Idle,
    /// `count` sent; neither the count event nor the barrier yet.
    Starting,
    Running,
    /// Stopped; the final values of `left` channels are being read.
    Reading {
        left: usize,
    },
}

impl Live {
    pub(crate) fn new(epoch: u64, axes: usize) -> Live {
        Live {
            epoch,
            ready: false,
            command: false,
            queued: 0,
            axes: vec![AxisLive::default(); axes],
            count: Count::Idle,
            tripped: false,
        }
    }

    /// Why SPEC cannot take a move or a count now. A busy SPEC with
    /// nothing of ours queued is running someone else's command.
    pub(crate) fn busy(&self) -> Option<&'static str> {
        if self.command {
            Some("a SPEC command is running")
        } else if !self.ready && self.queued == 0 {
            Some("SPEC is busy")
        } else {
            None
        }
    }
}

pub(crate) enum Request {
    Op {
        epoch: u64,
        op: Op,
    },
    /// A line for the log, from a driver that refused something.
    Log(String),
}

#[derive(Debug)]
pub(crate) enum Op {
    /// `user` in user units; a rate is sent first when it changed.
    Move {
        axis: usize,
        user: f64,
        slew: Option<f64>,
        base: Option<f64>,
    },
    Stop,
    SetPosition {
        axis: usize,
        user: f64,
    },
    /// `count_em value`: seconds, or monitor counts when negative.
    Count {
        value: f64,
    },
    StopCount,
    /// SPEC's reply, or the command's error, goes back on `reply`;
    /// dropping `reply` unanswered says the connection ended. With `scan`,
    /// the status port shows the scan's points as they arrive.
    Command {
        text: String,
        scan: bool,
        reply: std::sync::mpsc::SyncSender<Result<String, String>>,
    },
    Abort,
}

impl Shared {
    pub(crate) fn new(
        axes: Vec<String>,
        channels: Vec<String>,
        interlock: Option<Interlock>,
    ) -> (Shared, mpsc::UnboundedReceiver<Request>) {
        let (requests, rx) = mpsc::unbounded_channel();
        let n = channels.len();
        let shared = Shared {
            state: Mutex::new(State {
                link: Link::Down,
                counts: vec![0.0; n],
                presets: vec![0; n],
            }),
            requests,
            pollers: OnceLock::new(),
            done: DoneParam::new(1),
            axes,
            channels,
            interlock,
        };
        (shared, rx)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `accept` on the connected state under the lock; an operation it
    /// returns goes to the session, which sends it on the same connection
    /// or drops it if that connection has ended.
    pub(crate) fn submit(
        &self,
        accept: impl FnOnce(&mut Live) -> Result<Op, String>,
    ) -> Result<(), String> {
        let mut st = self.lock();
        let Link::Up(live) = &mut st.link else {
            return Err("SPEC is not connected".into());
        };
        let op = accept(live)?;
        let epoch = live.epoch;
        self.requests
            .send(Request::Op { epoch, op })
            .map_err(|_| "the SPEC session has ended".to_string())
    }

    pub(crate) fn log(&self, line: String) {
        let _ = self.requests.send(Request::Log(line));
    }

    /// Why the interlock refuses a motor record's move of `axis` to
    /// `target` (user units) now.
    pub(crate) fn refuse(&self, axes: &[AxisLive], axis: usize, target: f64) -> Option<String> {
        self.interlock
            .as_ref()?
            .refuse(&self.axes, axes, axis, target)
    }

    pub(crate) fn wake(&self, axis: usize) {
        if let Some(p) = self.pollers.get().and_then(|p| p.get(axis)) {
            let _ = p.try_send(PollCommand::Wakeup);
        }
    }

    pub(crate) fn wake_all(&self) {
        for axis in 0..self.axes.len() {
            self.wake(axis);
        }
    }
}
