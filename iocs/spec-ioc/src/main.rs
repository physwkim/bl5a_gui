//! SPEC IOC.
//!
//! Assembles: `SpecCreate` (the SPEC session, its status and command
//! ports, the motor port and the scaler) with `SpecInterlock` before it,
//! the motor and scaler record types, and the CA + PVA (QSRV) server.
//!
//! Usage:
//!   cargo run -p spec-ioc -- st.cmd

use std::sync::{Arc, Mutex};

use epics_rs::base::error::CaResult;
use epics_rs::base::server::device_support::DeviceSupport;
use epics_rs::ca::server::ioc_app::{DeviceSupportContext, IocApplication};
use epics_rs::scaler::device_support::scaler_asyn::{ASYN_SCALER_DEVICE, ScalerAsynDeviceSupport};

use spec_driver::ioc::{ScalerSlot, spec_commands};

#[epics_rs::base::epics_main]
async fn main() -> CaResult<()> {
    let args: Vec<String> = std::env::args().collect();
    let script = if args.len() > 1 && !args[1].starts_with('-') {
        args[1].clone()
    } else {
        epics_rs::base::stderr_println!("Usage: spec-ioc <st.cmd>");
        std::process::exit(1);
    };

    epics_rs::base::runtime::env::set_default_path("SPEC_IOC", env!("CARGO_MANIFEST_DIR"));
    // scaler-rs ships scaler.db; SCALER names the crate dir so st.cmd loads
    // it from $(SCALER)/db.
    let scaler_dir = std::path::Path::new(epics_rs::scaler::SCALER_DB_DIR)
        .parent()
        .expect("SCALER_DB_DIR is <crate>/db");
    epics_rs::base::runtime::env::set_default_path(
        "SCALER",
        scaler_dir.to_str().expect("cargo paths are UTF-8"),
    );

    let mut app = IocApplication::new();

    let (motor_name, motor_factory) = epics_rs::motor::motor_record_factory();
    app = app.register_record_type(motor_name, motor_factory);
    for cmd in epics_rs::motor::ioc::asyn_motor_controller_commands() {
        app = app.register_startup_command(cmd);
    }
    for cmd in epics_rs::motor::motor_util::motor_util_commands() {
        app = app.register_startup_command(cmd);
    }

    let (scaler_name, scaler_factory) = epics_rs::scaler::scaler_record_factory();
    app = app.register_record_type(scaler_name, scaler_factory);
    // scaler.db's count-rate records.
    app = app.register_record_type("transform", || {
        Box::new(epics_rs::base::server::records::transform::TransformRecord::default())
    });

    let (asyn_name, asyn_factory) = epics_rs::asyn::asyn_record::asyn_record_factory();
    app = app.register_record_type(asyn_name, asyn_factory);
    app = epics_rs::asyn::adapter::register_asyn_device_support(app);

    let scaler: ScalerSlot = Arc::new(Mutex::new(None));
    for cmd in spec_commands(scaler.clone()) {
        app = app.register_startup_command(cmd);
    }
    app = app.register_dynamic_device_support(
        &epics_rs::motor::devices::devices(),
        epics_rs::motor::devices::device_support_factory(),
    );
    // SpecCreate leaves the scaler for the record with DTYP "Asyn Scaler".
    app = app.register_dynamic_device_support(
        &[("scaler", std::slice::from_ref(&ASYN_SCALER_DEVICE))],
        move |ctx: &DeviceSupportContext| {
            if ctx.dtyp != "Asyn Scaler" {
                return None;
            }
            let driver = scaler.lock().unwrap().take()?;
            Some(Box::new(ScalerAsynDeviceSupport::new(Box::new(driver))) as Box<dyn DeviceSupport>)
        },
    );

    epics_rs::bridge::qsrv::run_ca_pva_qsrv_ioc_app(app.startup_script(&script)).await
}
