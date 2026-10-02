//! `devmgr`'s isolated-interrupts mark (`docs/NVIDIA.md` §12.3, check R10):
//! a driver of a device that runs firmware of its own starts only once its
//! mark is set.

extern crate std;

use std::format;

use crate::isolation::{self, Launch, Refusal};

/// Verifies: `L.device.27`
#[test]
fn a_firmware_device_starts_only_once_marked() {
    let mut asked = false;
    assert_eq!(
        isolation::launch(true, || {
            asked = true;
            Ok::<(), ()>(())
        }),
        Launch::Start
    );
    assert!(asked, "the mark is set before the start");
    assert_eq!(
        isolation::launch(true, || Err::<(), _>("ACCESS_DENIED")),
        Launch::Refused,
        "a refused mark stops the launch"
    );
}

/// Verifies: `L.device.27`
#[test]
fn a_device_without_firmware_is_neither_marked_nor_stopped() {
    let mut asked = false;
    assert_eq!(
        isolation::launch(false, || {
            asked = true;
            Err::<(), _>("never asked")
        }),
        Launch::Start
    );
    assert!(!asked, "no mark is asked for");
}

/// Verifies: `L.device.27`
#[test]
fn the_refusal_names_the_device_and_the_mark() {
    let line = format!(
        "{}",
        Refusal {
            what: "gpu",
            location: "01:00.0",
        }
    );
    assert_eq!(
        line,
        "devmgr   gpu 01:00.0 not started: the kernel refused its isolated-interrupts mark"
    );
}
