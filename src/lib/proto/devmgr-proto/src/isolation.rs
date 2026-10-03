//! The isolated-interrupts mark, as `devmgr` sets it (`docs/NVIDIA.md`
//! §12.3, condition G5).
//!
//! A device that runs firmware of its own -- a GPU running NVIDIA's GSP is
//! the first -- can raise any interrupt vector on a machine whose interrupts
//! are not isolated, and its firmware is not the driver's to vouch for. So
//! `devmgr` marks every such device through `device_set_limit`'s
//! `DEVICE_LIMIT_ISOLATED_INTERRUPTS` before it starts the device's driver,
//! and the kernel then refuses the device vectors and pins while the
//! machine's interrupts are not isolated. A mark is set-once, and the
//! kernel's refusal is the guard; `devmgr`'s part is never to start such a
//! driver without the mark: if setting it fails, the driver is not started,
//! and the line says why.
//!
//! A pure function of what `device_set_limit` answered, tested on the host.
//! `devmgr`'s `Gpu` kind calls [`launch`] through [`crate::gpu::hand_over`],
//! first, before the isolation check and the budget (NVIDIA's N1b).

use core::fmt;

/// Whether a driver may be started, after the mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// Start it: its device runs no firmware of its own, or is marked.
    Start,
    /// Do not start it: its device runs firmware of its own and the kernel
    /// refused its mark.
    Refused,
}

/// Whether to start the driver of a device that runs firmware of its own
/// when `runs_firmware`, after `mark` -- the `device_set_limit` call that
/// sets the device's isolated-interrupts mark, made only for such a device
/// -- answered.
pub fn launch<E>(runs_firmware: bool, mark: impl FnOnce() -> Result<(), E>) -> Launch {
    if !runs_firmware {
        return Launch::Start;
    }
    match mark() {
        Ok(()) => Launch::Start,
        Err(_) => Launch::Refused,
    }
}

/// The line `devmgr` prints for a driver [`launch`] refused: `what` is the
/// kind, `location` the device's place.
#[derive(Debug, Clone, Copy)]
pub struct Refusal<'a> {
    /// The kind, as the line names it: `gpu`.
    pub what: &'a str,
    /// Where the device is: `01:00.0`.
    pub location: &'a str,
}

impl fmt::Display for Refusal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "devmgr   {} {} not started: the kernel refused its isolated-interrupts mark",
            self.what, self.location
        )
    }
}
