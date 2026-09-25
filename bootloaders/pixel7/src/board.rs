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

/// Where ABL is believed to leave the panel's scan-out buffer.
///
/// **A guess, still unconfirmed.** The device tree names no framebuffer; this
/// is the address the mainline device tree gives the Pixel 6 (gs101), whose
/// ABL is the same family. The screen cannot confirm it: the panel runs in DSI
/// command mode and shows only frames the display controller is triggered to
/// send, which painting memory does not do.
pub(crate) const FRAMEBUFFER: u64 = 0xfac0_0000;

/// The panel, in pixels.
pub(crate) const PANEL_WIDTH: u64 = 1080;

/// The panel, in pixels.
pub(crate) const PANEL_HEIGHT: u64 = 2400;

/// Bytes per framebuffer row, at four bytes a pixel.
pub(crate) const FRAMEBUFFER_STRIDE: u64 = PANEL_WIDTH * 4;

/// The two watchdogs, `watchdog_cl0@10060000` and `watchdog_cl1@10070000`,
/// each with a 30 second timeout in the device tree.
pub(crate) const WATCHDOGS: [u64; 2] = [0x1006_0000, 0x1007_0000];

/// The Samsung watchdog's control register, `WTCON`; bit 5 enables it.
pub(crate) const WTCON: u64 = 0x0;

/// Its counter, `WTCNT`.
pub(crate) const WTCNT: u64 = 0x8;

/// Where ABL reads why the phone was reset: `reboot-cmd-offset` 0x810 into the
/// always-on PMU block at 0x18060000, the syscon `pixel-reboot` names.
pub(crate) const REBOOT_REASON: u64 = 0x1806_0810;

/// The reason that brings the phone back up in ABL's fastboot mode rather than
/// in Android: Pixel's `REBOOT_MODE_BOOTLOADER`.
pub(crate) const REBOOT_TO_BOOTLOADER: u32 = 0xfc;
