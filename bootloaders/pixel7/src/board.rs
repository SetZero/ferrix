//! Where things are on a Pixel 7 (`panther`, Tensor G2 / gs201).
//!
//! Read from the device tree ABL passes (`/sys/firmware/fdt` on the stock
//! build, CP2A.260705.006) and `/proc/iomem`, except where a constant says it
//! is a guess.

/// The console zone of the `ramoops` region: `ramoops_mem@fd3ff000`, with no
/// dump records and no ECC, so the console zone is the first 2 MiB. Android's
/// kernel reads the previous boot's record from here as
/// `/sys/fs/pstore/console-ramoops-0`.
pub(crate) const RAMOOPS_CONSOLE: u64 = 0xfd3f_f000;

/// Size of that zone.
pub(crate) const RAMOOPS_CONSOLE_SIZE: u64 = 0x20_0000;

/// The kernel command line: continue the loader's `ramoops` record as the
/// console, since the kernel's PL011 is QEMU's and this phone has none it can
/// reach. Must name the same zone as the two constants above.
///
/// `ferrix.fbcon` draws the kernel's lines on the screen as well, since
/// until the run ends and the record can be read, the screen is all there is
/// to look at.
///
/// No `nosmp`: all eight cores start, at EL2 through TF-A's PSCI, and the
/// kernel's secondary entry drops each to EL1.
pub(crate) const CMDLINE: &str = "console=ramoops,0xfd3ff000,0x200000 ferrix.fbcon";

/// The 16550 a guest of the phone's own crosvm gets as its console, which
/// the kernel is told to use in place of `ramoops`: see [`GUEST_CMDLINE`].
pub(crate) const GUEST_UART: u64 = 0x3f8;

/// The kernel command line in a guest of crosvm, on the Pixel's own KVM.
/// There is no screen, so no `ferrix.fbcon`, and no `ramoops` region either:
/// the console is crosvm's 16550, which crosvm connects to its own output.
pub(crate) const GUEST_CMDLINE: &str = "console=uart8250,mmio,0x3f8";

/// Whether the loader runs as a guest of crosvm rather than on the phone.
static GUEST: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Record that the loader runs as a guest; see [`is_guest`].
pub(crate) fn set_guest() {
    GUEST.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// Whether the loader runs as a guest of crosvm. Decided once, first thing:
/// ABL enters the loader at EL2, and crosvm at EL1 with a device tree whose
/// `stdout-path` is its 16550.
pub(crate) fn is_guest() -> bool {
    GUEST.load(core::sync::atomic::Ordering::Relaxed)
}

/// The two watchdogs, `watchdog_cl0@10060000` and `watchdog_cl1@10070000`,
/// each with a 30 second timeout in the device tree.
pub(crate) const WATCHDOGS: [u64; 2] = [0x1006_0000, 0x1007_0000];

/// The Samsung watchdog's control register, `WTCON`; bit 5 enables it.
pub(crate) const WTCON: u64 = 0x0;

/// Its counter, `WTCNT`.
pub(crate) const WTCNT: u64 = 0x8;
