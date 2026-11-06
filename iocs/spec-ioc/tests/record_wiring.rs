//! Every shipped record's DTYP names an interface the SPEC ports register.
//! spec_axis.db is left out: it includes asyn_motor.db, which binds the
//! motor port.

use std::path::PathBuf;

use ioc_conventions::wiring;
use spec_driver::{INTERFACES, INTERRUPTS};

#[test]
fn every_record_binds_an_interface_the_spec_ports_register() {
    for file in ["spec.db", "spec_live.db"] {
        let db = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("db")
            .join(file);
        wiring::assert_wired(&wiring::parse(&db), INTERFACES, INTERRUPTS);
    }
}

/// The NELM of each spec.db record that reads `param`.
fn nelms_of(param: &[u8]) -> Vec<Vec<u8>> {
    let db = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("db/spec.db");
    let nelms: Vec<Vec<u8>> = wiring::parse(&db)
        .iter()
        .filter(|d| {
            d.fields
                .iter()
                .any(|f| f.name == "INP" && f.value.as_bytes().ends_with(param))
        })
        .map(|record| {
            let nelm = record
                .fields
                .iter()
                .find(|f| f.name == "NELM")
                .expect("NELM");
            nelm.value.as_bytes().to_vec()
        })
        .collect();
    assert!(!nelms.is_empty(), "a record reads the parameter");
    nelms
}

#[test]
fn the_log_record_holds_the_drivers_tail() {
    for nelm in nelms_of(b"SPEC_LOG") {
        assert_eq!(nelm, spec_driver::LOG_NELM.to_string().as_bytes());
    }
}

#[test]
fn the_scan_data_records_hold_a_whole_scan() {
    let nelms = nelms_of(b"SPEC_SCAN_DATA");
    assert_eq!(nelms.len(), 2, "Scan:Data and Scan:Live:Data");
    for nelm in nelms {
        assert_eq!(nelm, spec_driver::SCAN_NELM.to_string().as_bytes());
    }
}
