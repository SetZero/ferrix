//! What the machine does once boot has nothing left to run.
//!
//! By default it powers off: a QEMU run then ends, and `cargo xtask test-boot`
//! and `test-shell` read that as the end of the log. A board is different. A
//! powered-off STM32MP157-DK has to be unplugged and plugged in again before it
//! does anything, and the reset button does not bring it back, so every run on
//! it used to cost a hand at the board. `ferrix.onexit=reset` on the command
//! line asks for a reset instead, which lands back at the firmware's prompt,
//! ready for the next image.
//!
//! The option is read once, early, rather than when boot ends: by then the
//! device tree may be unreachable for reasons that have nothing to do with it,
//! and a typo is better reported at the start of a log than discovered at the
//! end of one.

use core::sync::atomic::{AtomicBool, Ordering};

use ferrix_bootinfo::{BootView, option_in};

use crate::arch;
use crate::console::println;

/// The command-line option, and the one value of it that is honoured.
const OPTION: &str = "ferrix.onexit";
const RESET: &str = "reset";

/// Whether boot ends in a reset rather than a power-off.
static RESET_ON_EXIT: AtomicBool = AtomicBool::new(false);

/// Read `ferrix.onexit` from the loader's command line or, on a machine
/// described by a device tree, from `/chosen/bootargs`, where U-Boot puts
/// `bootargs`.
///
/// Any other value is reported and ignored: powering off is the safe
/// default, and a misspelt option should not turn into something nobody asked
/// for.
pub(crate) fn init(view: &BootView<'_>) {
    let tree = crate::fdt::open(view).ok();
    // Where a board keeps the boot mode `reboot(2)`'s word sets.
    if let Some(tree) = &tree {
        crate::stm32mp1::note_boot_context(tree);
    }
    let value = view.option(OPTION).or_else(|| {
        tree.as_ref()
            .and_then(|tree| option_in(tree.bootargs()?, OPTION))
    });
    match value {
        None => {}
        Some(RESET) => {
            RESET_ON_EXIT.store(true, Ordering::Relaxed);
            println!("  power    {OPTION}={RESET}: the machine resets when boot ends");
        }
        Some(other) => {
            println!(
                "  power    {OPTION}={other} is not understood; the machine powers off when boot ends"
            );
        }
    }
}

/// End boot the way [`init`] was told to.
pub(crate) fn finish() -> ! {
    // The root disk's last half-minute, which its committer has not reached
    // yet. A failure is said and does not stop the power-off.
    if crate::fs::root_disk::sync().is_err() {
        println!("  power    / could not be committed before the power-off");
    }
    if crate::fs::data_disk::sync().is_err() {
        println!("  power    /data could not be committed before the power-off");
    }
    if RESET_ON_EXIT.load(Ordering::Relaxed) {
        println!("  power    resetting, as {OPTION}={RESET} asks");
        arch::reset()
    } else {
        arch::shutdown()
    }
}
