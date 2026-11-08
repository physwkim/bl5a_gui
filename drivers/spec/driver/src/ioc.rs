//! `SpecCreate(port, address, motorPort, motors, counters,
//! movingPollPeriod, idlePollPeriod)`: one SPEC server as the status port
//! `port`, the command port `<port>_CMD`, a motor port `motorPort` with one
//! axis per motor mnemonic, and the scaler the IOC binds to the record
//! with DTYP "Asyn Scaler".
//!
//! `SpecInterlock(port, arm, clear, out, away, guarded)`, before
//! SpecCreate of the same `port`: the guarded motors move only while
//! `arm` is at or above `clear`, and `arm` goes below it only while `out`
//! is at or above `away` (see [`crate::interlock`]).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use epics_rs::asyn::interfaces::motor::AsynMotor;
use epics_rs::asyn::manager::claim_port_driver;
use epics_rs::asyn::port::PortDriver;
use epics_rs::asyn::port_handle::PortHandle;
use epics_rs::asyn::runtime::config::RuntimeConfig;
use epics_rs::asyn::runtime::port::{PortRuntimeHandle, create_port_runtime_boxed};
use epics_rs::base::server::iocsh::registry::*;

use motor_common::iocsh::{
    arg_double_req, arg_int_opt, arg_str_req, poll_intervals, req_double, req_string,
};
use motor_common::{Controller, asyn_user, register_port};

use crate::Shared;
use crate::axis::SpecAxis;
use crate::interlock::Named;
use crate::port::{CmdPort, StatusPort};
use crate::scaler::SpecScaler;
use crate::session::Session;

/// Where `SpecCreate` leaves the scaler for the record's device support.
pub type ScalerSlot = Arc<Mutex<Option<SpecScaler>>>;

/// What the startup commands pass between them, by port.
#[derive(Default)]
struct Ports {
    /// SpecInterlock's, for SpecCreate to take.
    interlocks: HashMap<String, Named>,
    created: HashSet<String>,
}

/// Mnemonics separated by spaces or commas.
fn mnemonics(list: &str) -> Vec<String> {
    list.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// `driver` as the port `name`, its runtime kept in `runtimes`: a port's
/// actor ends with its last runtime handle.
fn start_port(
    ctx: &CommandContext,
    runtimes: &Mutex<Vec<PortRuntimeHandle>>,
    name: &str,
    driver: Box<dyn PortDriver>,
) -> Result<PortHandle, String> {
    let claim = claim_port_driver(name).map_err(|e| e.to_string())?;
    let (runtime, _) = create_port_runtime_boxed(&claim, driver, RuntimeConfig::default())
        .map_err(|e| e.to_string())?;
    let handle = runtime.port_handle().clone();
    claim
        .publish(handle.clone(), ctx.db().scan_control().interrupt_gate())
        .map_err(|e| e.to_string())?;
    runtimes.lock().unwrap().push(runtime);
    Ok(handle)
}

/// `SpecInterlock` and `SpecCreate`.
pub fn spec_commands(scaler: ScalerSlot) -> Vec<CommandDef> {
    let ports: Arc<Mutex<Ports>> = Arc::default();
    vec![
        spec_interlock_command(ports.clone()),
        spec_create_command(ports, scaler),
    ]
}

fn spec_interlock_command(ports: Arc<Mutex<Ports>>) -> CommandDef {
    CommandDef::new(
        "SpecInterlock",
        vec![
            arg_str_req("Port name"),
            arg_str_req("Arm motor"),
            arg_double_req("Arm clear position"),
            arg_str_req("Out motor"),
            arg_double_req("Out away position"),
            arg_str_req("Guarded motors"),
        ],
        "SpecInterlock(port, arm, clear, out, away, \"guarded\"), before SpecCreate(port, ...): \
         the guarded motors move only with arm >= clear, and arm goes below clear only \
         with out >= away",
        move |args: &[ArgValue], _ctx: &CommandContext| {
            let port = req_string(args, 0, "Port name")?;
            let named = Named {
                arm: req_string(args, 1, "Arm motor")?,
                clear: req_double(args, 2, "Arm clear position")?,
                out: req_string(args, 3, "Out motor")?,
                away: req_double(args, 4, "Out away position")?,
                guarded: mnemonics(&req_string(args, 5, "Guarded motors")?),
            };
            let mut ports = ports.lock().unwrap();
            if ports.created.contains(&port) {
                return Err(format!(
                    "SpecInterlock: SpecCreate(\"{port}\") has run; SpecInterlock goes before it"
                ));
            }
            if ports.interlocks.contains_key(&port) {
                return Err(format!("SpecInterlock: {port} has one already"));
            }
            ports.interlocks.insert(port, named);
            Ok(CommandOutcome::Continue)
        },
    )
}

fn spec_create_command(ports: Arc<Mutex<Ports>>, scaler: ScalerSlot) -> CommandDef {
    let runtimes: Arc<Mutex<Vec<PortRuntimeHandle>>> = Arc::default();
    CommandDef::new(
        "SpecCreate",
        vec![
            arg_str_req("Port name"),
            arg_str_req("SPEC host:port"),
            arg_str_req("Motor port name"),
            arg_str_req("Motor mnemonics"),
            arg_str_req("Counter mnemonics"),
            arg_int_opt("Moving poll period (ms)"),
            arg_int_opt("Idle poll period (ms)"),
        ],
        "SpecCreate(port, host:port, motorPort, \"motors\", \"timer monitor ...\", \
         movingPollPeriod, idlePollPeriod); the command port is <port>_CMD",
        move |args: &[ArgValue], ctx: &CommandContext| {
            let port = req_string(args, 0, "Port name")?;
            let address = req_string(args, 1, "SPEC host:port")?;
            let motor_port = req_string(args, 2, "Motor port name")?;
            let axes = mnemonics(&req_string(args, 3, "Motor mnemonics")?);
            let channels = mnemonics(&req_string(args, 4, "Counter mnemonics")?);
            let (moving_ms, idle_ms) = poll_intervals(args, 5, 6)?;
            if channels.is_empty() {
                return Err("SpecCreate: no counters; the first is the timer".into());
            }
            let interlock = {
                let mut ports = ports.lock().unwrap();
                ports.created.insert(port.clone());
                ports.interlocks.remove(&port)
            };
            let interlock = interlock
                .map(|named| named.resolve(&axes))
                .transpose()
                .map_err(|why| format!("SpecInterlock({port}): {why}"))?;
            let n = axes.len();
            let (shared, requests) = Shared::new(axes, channels, interlock);
            let shared = Arc::new(shared);

            let status = StatusPort::new(&port, shared.clone()).map_err(|e| e.to_string())?;
            let params = status.params();
            let handle = start_port(ctx, &runtimes, &port, Box::new(status))?;
            let cmd_port = format!("{port}_CMD");
            let cmd = CmdPort::new(&cmd_port, shared.clone()).map_err(|e| e.to_string())?;
            start_port(ctx, &runtimes, &cmd_port, Box::new(cmd))?;

            register_port(ctx, &motor_port, n as i32)?;
            let poller = Controller::new();
            let mut wakers = Vec::with_capacity(n);
            for index in 0..n {
                let axis: Arc<Mutex<dyn AsynMotor>> =
                    Arc::new(Mutex::new(SpecAxis::new(shared.clone(), index)));
                wakers.push(poller.install(asyn_user(&motor_port, index as i32), axis)?);
            }
            let _ = shared.pollers.set(wakers);
            poller.start(&motor_port, moving_ms, idle_ms, 2);

            *scaler.lock().unwrap() = Some(SpecScaler::new(shared.clone()));
            Session::new(shared, handle, params, address, requests)
                .spawn()
                .map_err(|e| format!("SpecCreate: {e}"))?;
            Ok(CommandOutcome::Continue)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::mnemonics;

    #[test]
    fn mnemonics_split_at_spaces_and_commas() {
        assert_eq!(
            mnemonics(" sy, shold  th,,tth "),
            ["sy", "shold", "th", "tth"]
        );
        assert!(mnemonics(" , ").is_empty());
    }
}
