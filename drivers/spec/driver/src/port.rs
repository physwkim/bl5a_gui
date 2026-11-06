//! The two asyn ports beside the motors and the scaler.
//!
//! The command port runs one SPEC command per `Cmd` write and returns from
//! the write when SPEC replies, so a put with completion ends with the
//! command and the records it links to read that command's reply. A
//! `ScanCmd` write is a `Cmd` whose scan's points the `Scan:` records show
//! as SPEC sends them. Because that write blocks its port, everything that
//! must not wait for a command is on the status port: `Cmd:Abort`, SPEC's
//! log, connection and busy state, the scan's points, and the values the
//! Live and rate records show.

use std::sync::Arc;
use std::sync::mpsc;

use epics_rs::asyn::error::{AsynError, AsynResult, AsynStatus};
use epics_rs::asyn::param::{ParamType, ParamValue};
use epics_rs::asyn::port::{InterfaceMask, PortDriver, PortDriverBase, PortFlags};
use epics_rs::asyn::user::AsynUser;

use crate::points::Shown;
use crate::{Op, Shared};

/// The status port's parameters. Abort, log, connection and scan
/// parameters are at address 0; `live` is per channel and the rates per
/// axis, at the channel's or axis's index.
#[derive(Clone, Copy)]
pub(crate) struct Params {
    pub(crate) abort: usize,
    pub(crate) log: usize,
    pub(crate) spec_busy: usize,
    pub(crate) connected: usize,
    pub(crate) live: usize,
    pub(crate) velo: usize,
    pub(crate) vbas: usize,
    pub(crate) vmax: usize,
    /// The last `ScanCmd`'s points, row by row, and its column names, one
    /// per line.
    pub(crate) scan_data: usize,
    pub(crate) scan_cols: usize,
}

/// Both ports' interrupt and interface masks.
pub const INTERRUPTS: InterfaceMask = InterfaceMask::INT32
    .or(InterfaceMask::FLOAT64)
    .or(InterfaceMask::OCTET)
    .or(InterfaceMask::FLOAT64_ARRAY);
pub const INTERFACES: InterfaceMask = INTERRUPTS.or(InterfaceMask::DRV_USER);

fn failure(message: String) -> AsynError {
    AsynError::Status {
        status: AsynStatus::Error,
        message,
    }
}

pub(crate) struct StatusPort {
    base: PortDriverBase,
    params: Params,
    shared: Arc<Shared>,
}

impl StatusPort {
    pub(crate) fn new(name: &str, shared: Arc<Shared>) -> AsynResult<StatusPort> {
        let addrs = shared.axes.len().max(shared.channels.len());
        let mut base = PortDriverBase::new(
            name,
            addrs,
            PortFlags {
                multi_device: true,
                can_block: false,
                destructible: true,
            },
            INTERFACES,
            INTERRUPTS,
        );
        let params = Params {
            abort: base.create_param("SPEC_ABORT", ParamType::Int32)?,
            log: base.create_param("SPEC_LOG", ParamType::Octet)?,
            spec_busy: base.create_param("SPEC_BUSY", ParamType::Int32)?,
            connected: base.create_param("SPEC_CONNECTED", ParamType::Int32)?,
            live: base.create_param("SPEC_LIVE", ParamType::Float64)?,
            velo: base.create_param("SPEC_VELO", ParamType::Float64)?,
            vbas: base.create_param("SPEC_VBAS", ParamType::Float64)?,
            vmax: base.create_param("SPEC_VMAX", ParamType::Float64)?,
            scan_data: base.create_param("SPEC_SCAN_DATA", ParamType::Float64Array)?,
            scan_cols: base.create_param("SPEC_SCAN_COLS", ParamType::Octet)?,
        };
        base.set_int32_param(params.abort, 0, 0)?;
        base.set_string_param(params.log, 0, "")?;
        base.set_string_param(params.scan_cols, 0, "")?;
        base.set_int32_param(params.spec_busy, 0, 0)?;
        base.set_int32_param(params.connected, 0, 0)?;
        Ok(StatusPort {
            base,
            params,
            shared,
        })
    }

    pub(crate) fn params(&self) -> Params {
        self.params
    }

    /// `shown` to the `Scan:` records: kept for a read, posted to the I/O
    /// Intr ones, the column names first.
    pub(crate) fn show_scan(&mut self, shown: Shown) -> AsynResult<()> {
        let Params {
            scan_data,
            scan_cols,
            ..
        } = self.params;
        self.base.update_time_stamp();
        if let Some(cols) = shown.cols {
            self.base.set_string_param(scan_cols, 0, &cols)?;
            self.base.call_param_callbacks(0)?;
        }
        self.base
            .params
            .set_float64_array(scan_data, 0, shown.data.clone())?;
        self.base
            .do_callbacks_array(scan_data, 0, ParamValue::Float64Array(shown.data))
    }
}

impl PortDriver for StatusPort {
    fn base(&self) -> &PortDriverBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PortDriverBase {
        &mut self.base
    }

    /// `Cmd:Abort` is SPEC's ^C: it stops the command, moves and counts.
    fn write_int32(&mut self, user: &mut AsynUser, value: i32) -> AsynResult<()> {
        if user.reason == self.params.abort && value != 0 {
            self.shared.submit(|_| Ok(Op::Abort)).map_err(failure)?;
        }
        self.base.write_int32_param(user, value)
    }

    fn read_float64_array(&mut self, user: &AsynUser, buf: &mut [f64]) -> AsynResult<usize> {
        if user.reason != self.params.scan_data {
            return Err(failure(format!(
                "no float64 array at reason {}",
                user.reason
            )));
        }
        let data = self.base.params.get_float64_array(user.reason, 0)?;
        let n = buf.len().min(data.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }
}

pub(crate) struct CmdPort {
    base: PortDriverBase,
    cmd: usize,
    scan_cmd: usize,
    reply: usize,
    status: usize,
    busy: usize,
    shared: Arc<Shared>,
}

impl CmdPort {
    pub(crate) fn new(name: &str, shared: Arc<Shared>) -> AsynResult<CmdPort> {
        let mut base = PortDriverBase::new(
            name,
            1,
            PortFlags {
                multi_device: false,
                can_block: true,
                destructible: true,
            },
            INTERFACES,
            INTERRUPTS,
        );
        let cmd = base.create_param("SPEC_CMD", ParamType::Octet)?;
        let scan_cmd = base.create_param("SPEC_SCAN_CMD", ParamType::Octet)?;
        let reply = base.create_param("SPEC_CMD_REPLY", ParamType::Octet)?;
        let status = base.create_param("SPEC_CMD_STATUS", ParamType::Int32)?;
        let busy = base.create_param("SPEC_CMD_BUSY", ParamType::Int32)?;
        let mut port = CmdPort {
            base,
            cmd,
            scan_cmd,
            reply,
            status,
            busy,
            shared,
        };
        port.base.set_string_param(cmd, 0, "")?;
        port.base.set_string_param(scan_cmd, 0, "")?;
        port.show("", false, false)?;
        Ok(port)
    }

    fn show(&mut self, reply: &str, failed: bool, busy: bool) -> AsynResult<()> {
        self.base.set_string_param(self.reply, 0, reply)?;
        self.base
            .set_int32_param(self.status, 0, i32::from(failed))?;
        self.base.set_int32_param(self.busy, 0, i32::from(busy))?;
        self.base.call_param_callbacks(0)
    }

    /// `text` run at SPEC's prompt, taken only while SPEC is idle so the
    /// reply is this command's; what it left, or why it failed.
    fn run(&mut self, text: &str, scan: bool) -> AsynResult<Result<String, String>> {
        if text.is_empty() {
            return Ok(Err("refused: an empty command".into()));
        }
        let (reply, done) = mpsc::sync_channel(1);
        let accepted = self.shared.submit(|live| {
            if let Some(why) = live.busy() {
                return Err(why.to_string());
            }
            live.command = true;
            Ok(Op::Command {
                text: text.to_string(),
                scan,
                reply,
            })
        });
        if let Err(why) = accepted {
            return Ok(Err(format!("refused: {why}")));
        }
        self.show("", false, true)?;
        // The session drops the sender when the connection ends.
        Ok(done
            .recv()
            .unwrap_or_else(|_| Err("SPEC disconnected".into())))
    }
}

impl PortDriver for CmdPort {
    fn base(&self) -> &PortDriverBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PortDriverBase {
        &mut self.base
    }

    fn write_octet(&mut self, user: &mut AsynUser, data: &[u8]) -> AsynResult<usize> {
        let scan = if user.reason == self.cmd {
            false
        } else if user.reason == self.scan_cmd {
            true
        } else {
            return self.base.write_octet(user, data);
        };
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        let text = String::from_utf8_lossy(&data[..end]).trim().to_string();
        self.base.set_string_param(user.reason, 0, &text)?;
        let result = self.run(&text, scan)?;
        let (reply, failed) = match &result {
            Ok(reply) => (reply.as_str(), false),
            Err(why) => (why.as_str(), true),
        };
        self.show(reply, failed, false)?;
        match result {
            Ok(_) => Ok(data.len()),
            Err(why) => {
                let line = format!("command {text:?}: {why}");
                self.shared.log(line.clone());
                Err(failure(line))
            }
        }
    }
}
