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
/// `nosmp` until the kernel's secondary entry can start at EL2: TF-A's PSCI
/// starts a core at the highest non-secure level, which here is EL2, and that
/// entry sequence is written for the EL1 QEMU starts one at.
pub(crate) const CMDLINE: &str = "console=ramoops,0xfd3ff000,0x200000 nosmp";

/// The two watchdogs, `watchdog_cl0@10060000` and `watchdog_cl1@10070000`,
/// each with a 30 second timeout in the device tree.
pub(crate) const WATCHDOGS: [u64; 2] = [0x1006_0000, 0x1007_0000];

/// The Samsung watchdog's control register, `WTCON`; bit 5 enables it.
pub(crate) const WTCON: u64 = 0x0;

/// Its counter, `WTCNT`.
pub(crate) const WTCNT: u64 = 0x8;
