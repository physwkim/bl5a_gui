//! SPEC's counters as a scaler record's device.
//!
//! The first channel is SPEC's timer: its seconds are counted in ticks of
//! [`TIMER_FREQ`], so PR1 is the time preset. The second is SPEC's monitor
//! counter (`MON`): a PR2 preset counts to monitor counts instead, sent as
//! a negative `count_em` argument as SPEC's `ct` does.

use std::sync::Arc;

use epics_rs::base::error::CaResult;
use epics_rs::scaler::device_support::scaler_asyn::{DoneInterrupt, ScalerDriver};
use epics_rs::scaler::records::scaler::MAX_SCALER_CHANNELS;

use crate::{Count, Link, Op, Shared, TIMER_FREQ};

pub struct SpecScaler {
    shared: Arc<Shared>,
}

impl SpecScaler {
    pub(crate) fn new(shared: Arc<Shared>) -> SpecScaler {
        SpecScaler { shared }
    }

    /// The `count_em` argument the presets ask for.
    fn preset(presets: &[u32]) -> Result<f64, String> {
        let monitor = presets.get(1).copied().unwrap_or(0);
        if monitor > 0 {
            return Ok(-f64::from(monitor));
        }
        match presets.first().copied().unwrap_or(0) {
            0 => Err("no preset".into()),
            // The record saturates PR1 = TP x FREQ here.
            u32::MAX => Err(format!(
                "a time preset over {} s; count in parts",
                spec_protocol::format_number((f64::from(u32::MAX) / TIMER_FREQ).floor())
            )),
            ticks => Ok(f64::from(ticks) / TIMER_FREQ),
        }
    }

    fn start(&self) -> Result<(), String> {
        let presets = self.shared.lock().presets.clone();
        let value = Self::preset(&presets)?;
        self.shared.submit(|live| {
            if let Some(why) = live.busy() {
                return Err(why.to_string());
            }
            if live.count != Count::Idle {
                return Err("SPEC is counting".into());
            }
            live.count = Count::Starting;
            live.queued += 1;
            // Under the state lock, so the session's done = 1 comes after.
            self.shared.done.set(0);
            Ok(Op::Count { value })
        })
    }
}

impl ScalerDriver for SpecScaler {
    fn reset(&mut self) -> CaResult<()> {
        let mut st = self.shared.lock();
        st.presets.iter_mut().for_each(|p| *p = 0);
        st.counts.iter_mut().for_each(|c| *c = 0.0);
        Ok(())
    }

    fn read(&mut self, counts: &mut [u32; MAX_SCALER_CHANNELS]) -> CaResult<()> {
        let st = self.shared.lock();
        for (i, (out, v)) in counts.iter_mut().zip(&st.counts).enumerate() {
            let v = if i == 0 { v * TIMER_FREQ } else { *v };
            *out = v.round().clamp(0.0, f64::from(u32::MAX)) as u32;
        }
        Ok(())
    }

    fn write_preset(&mut self, channel: usize, preset: u32) -> CaResult<u32> {
        if let Some(p) = self.shared.lock().presets.get_mut(channel) {
            *p = preset;
        }
        Ok(preset)
    }

    fn arm(&mut self, start: bool) -> CaResult<()> {
        if !start {
            let counting = matches!(&self.shared.lock().link,
                Link::Up(live) if live.count != Count::Idle);
            if counting {
                let _ = self.shared.submit(|_| Ok(Op::StopCount));
            }
            return Ok(());
        }
        let done = &self.shared.done;
        match self.start() {
            Ok(()) => done.call_callbacks(),
            Err(why) => {
                self.shared.log(format!("count refused: {why}"));
                // A done pulse ends the record's count; done is already 1.
                done.set(0);
                done.set(1);
                done.call_callbacks();
            }
        }
        Ok(())
    }

    fn actual_frequency(&self) -> Option<f64> {
        Some(TIMER_FREQ)
    }

    fn register_done_interrupt(&mut self, user: DoneInterrupt) {
        self.shared.done.register(user);
    }

    fn num_channels(&self) -> usize {
        self.shared.channels.len()
    }
}
