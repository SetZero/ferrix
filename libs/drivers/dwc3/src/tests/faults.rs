//! What goes wrong: a core that is not there or never finishes, memory it
//! cannot use, and a driver that forgets a clean or an invalidate.

use ferrix_usb_device::acm::DATA_IN;

use super::model::{DWC31, Memory, Model, Regs, Shared, Time};
use super::rig::{Rig, parts};
use crate::layout::{AREA_BYTES, ENDPOINT_BYTES, endpoint_buffer};
use crate::regs::{DCTL, DSTS, GSTS};
use crate::{Controller, Error, MILLISECOND, Parts};

#[test]
fn not_a_dwc3() {
    let model = Model::new();
    model.borrow_mut().gsnpsid = 0x4F54_300A;
    let error = Controller::start(parts(&model))
        .map(|_| ())
        .map_err(|(error, _)| error);
    assert_eq!(error, Err(Error::NotDwc3(0x4F54_300A)), "refused");
    assert_eq!(model.borrow().writes, 0, "untouched");
}

fn start(model: &Shared) -> Result<(), Error> {
    Controller::start(parts(model))
        .map(|_| ())
        .map_err(|(error, _)| error)
}

#[test]
fn refuses_a_controller_in_host_mode_then_takes_it_in_device_mode() {
    let model = Model::new();
    model.borrow_mut().gsts = 0x7E80_0021;
    assert_eq!(
        start(&model),
        Err(Error::Refused {
            register: GSTS,
            value: 0x7E80_0021
        }),
        "GSTS says host"
    );
    assert_eq!(model.borrow().writes, 0, "nothing written");
    model.borrow_mut().gsts = 0x7E80_0020;
    assert_eq!(start(&model), Ok(()), "device mode, as ABL leaves it");
    assert_eq!(model.borrow().gsts & 0x20, 0, "CSR_TIMEOUT cleared");
}

#[test]
fn refuses_a_running_controller_then_takes_it_stopped() {
    let model = Model::new();
    model.borrow_mut().dctl = 0x80F0_0000;
    assert_eq!(
        start(&model),
        Err(Error::Refused {
            register: DCTL,
            value: 0x80F0_0000
        }),
        "run bit set: someone else is driving it"
    );
    assert_eq!(model.borrow().writes, 0, "nothing written");
    model.borrow_mut().dctl = 0x00F0_0000;
    assert_eq!(start(&model), Ok(()), "stopped");
}

#[test]
fn refuses_a_controller_still_halting_then_takes_it_halted() {
    let model = Model::new();
    model.borrow_mut().halting = true;
    let refused = start(&model);
    assert!(
        matches!(refused, Err(Error::Refused { register: DSTS, value }) if value & (1 << 22) == 0),
        "DEVCTRLHLT clear: {refused:?}"
    );
    assert_eq!(model.borrow().writes, 0, "nothing written");
    model.borrow_mut().halting = false;
    assert_eq!(start(&model), Ok(()), "halted");
}

#[test]
fn an_area_too_small() {
    let model = Model::new();
    let parts = Parts {
        registers: Regs(model.clone()),
        memory: Memory(model.clone(), AREA_BYTES - 1),
        clock: Time(model),
    };
    let error = Controller::start(parts)
        .map(|_| ())
        .map_err(|(error, _)| error);
    assert_eq!(error, Err(Error::Memory), "refused");
}

#[test]
fn a_reset_that_never_ends() {
    let model = Model::new();
    model.borrow_mut().wedged_reset = true;
    let error = Controller::start(parts(&model))
        .map(|_| ())
        .map_err(|(error, _)| error);
    assert_eq!(error, Err(Error::Reset), "gave up");
    let now = model.borrow().now;
    assert!(
        (500 * MILLISECOND..600 * MILLISECOND).contains(&now),
        "after half a second: {now}"
    );
}

#[test]
fn a_command_that_never_ends() {
    let model = Model::new();
    model.borrow_mut().wedged_command = true;
    let error = Controller::start(parts(&model))
        .map(|_| ())
        .map_err(|(error, _)| error);
    assert_eq!(
        error,
        Err(Error::Command {
            physical: 0,
            command: 9,
            status: None
        }),
        "DEPSTARTCFG timed out"
    );
    assert!(model.borrow().gsnpsid & DWC31 == DWC31, "a DWC_usb31");
}

#[test]
fn a_missing_clean_is_caught() {
    let model = Model::new();
    model.borrow_mut().skip_clean = true;
    let _ = Controller::start(parts(&model))
        .map_err(|(error, _)| error)
        .expect("starts");
    let violations = model.borrow().violations.clone();
    assert!(
        violations
            .iter()
            .any(|text| text.contains("a TRB") && text.contains("not cleaned")),
        "the SETUP TRB handed over dirty: {violations:?}"
    );
    assert!(
        violations.iter().any(|text| text.contains("event buffer")),
        "the event buffer too: {violations:?}"
    );
}

#[test]
fn a_missing_clean_of_in_data_is_caught() {
    let mut rig = Rig::new();
    let _ = rig.enumerate(true);
    rig.assert_clean();
    // Bulk IN is the third endpoint: slot 2's page.
    let page = endpoint_buffer(2);
    rig.model.borrow_mut().skip_clean_in = Some(page..page + ENDPOINT_BYTES);
    assert_eq!(rig.controller.write(DATA_IN, b"dirty"), Ok(5), "taken");
    let violations = rig.violations();
    assert!(
        violations.iter().any(|text| text.starts_with("IN data")),
        "the bytes handed over dirty: {violations:?}"
    );
}

#[test]
fn a_missing_invalidate_is_caught() {
    let mut rig = Rig::new();
    rig.model.borrow_mut().skip_invalidate = true;
    rig.attach(true);
    let violations = rig.violations();
    assert!(
        violations
            .iter()
            .any(|text| text.contains("without invalidating")),
        "an event read stale: {violations:?}"
    );
}

#[test]
fn stop_halts_after_taking_the_events() {
    let mut rig = Rig::new();
    let _ = rig.enumerate(true);
    assert_eq!(
        rig.controller.write(DATA_IN, b"x"),
        Ok(1),
        "a transfer started"
    );
    // Events the controller wrote and nobody took: it will not halt with
    // them counted.
    rig.model.borrow_mut().detach();
    assert!(rig.model.borrow().interrupt(), "events waiting");
    assert_eq!(rig.controller.stop(), Ok(()), "halted");
    assert!(rig.model.borrow().is_halted(), "DEVCTRLHLT");
    rig.assert_clean();
}
