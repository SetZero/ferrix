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
//! `ferrix.onexit=panic` asks for what Linux does when init exits: a panic,
//! here the one the catalog calls `FX-1501`. The disks are committed first,
//! which Linux does not do, because nothing is gained by losing them
//! (`docs/INIT.md` §8.3).
//!
//! The option is read once, early, rather than when boot ends: by then the
//! device tree may be unreachable for reasons that have nothing to do with it,
//! and a typo is better reported at the start of a log than discovered at the
//! end of one.

use core::sync::atomic::{AtomicU8, Ordering};

use ferrix_bootinfo::{BootView, option_in};

use crate::arch;
use crate::console::println;
use crate::panic::{catalog, fatal};

/// The command-line option, and the values of it that are honoured.
const OPTION: &str = "ferrix.onexit";
const RESET: &str = "reset";
const PANIC: &str = "panic";

/// What boot ends in: one of the three below.
static ON_EXIT: AtomicU8 = AtomicU8::new(POWER_OFF);
const POWER_OFF: u8 = 0;
const RESET_ON_EXIT: u8 = 1;
const PANIC_ON_EXIT: u8 = 2;

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
            ON_EXIT.store(RESET_ON_EXIT, Ordering::Relaxed);
            println!("  power    {OPTION}={RESET}: the machine resets when boot ends");
        }
        Some(PANIC) => {
            ON_EXIT.store(PANIC_ON_EXIT, Ordering::Relaxed);
            println!("  power    {OPTION}={PANIC}: the kernel panics when init exits");
        }
        Some(other) => {
            println!(
                "  power    {OPTION}={other} is not understood; the machine powers off when boot ends"
            );
        }
    }
}

/// Commit `/` and `/data`: the root disk's last half-minute, which its
/// committer has not reached yet, and the data disk, which has none.
///
/// Before the machine stops, whoever stops it: [`finish`] when init has
/// exited, and `reboot(2)` when a program asks, so that a program calling it
/// without a `sync` of its own loses no transaction either. A failure is said
/// and does not stop the machine stopping.
pub(crate) fn sync_disks() {
    if crate::fs::root_disk::sync().is_err() {
        println!("  power    / could not be committed before the power-off");
    }
    if crate::fs::data_disk::sync().is_err() {
        println!("  power    /data could not be committed before the power-off");
    }
}

/// End boot the way [`init`] was told to.
pub(crate) fn finish() -> ! {
    sync_disks();
    match ON_EXIT.load(Ordering::Relaxed) {
        RESET_ON_EXIT => {
            println!("  power    resetting, as {OPTION}={RESET} asks");
            arch::reset()
        }
        PANIC_ON_EXIT => fatal!(
            catalog::INIT_EXITED,
            "init exited, and {OPTION}={PANIC} asks for a panic"
        ),
        _ => arch::shutdown(),
    }
}
