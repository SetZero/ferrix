//! Ferrix's second-stage loader for the Pixel 7 (`panther`).
//!
//! The phone's own bootloader, ABL, is signed and cannot be replaced; what an
//! unlocked one will do is load an Android boot image and jump to its kernel
//! under the Linux arm64 boot protocol. This program is that kernel. Its job
//! is to do for Ferrix what `boot/` does under UEFI -- find memory, load the
//! kernel and its initramfs, and hand over a `BootInfo` -- from what ABL
//! provides instead: a device tree and a machine at EL2 with the MMU off.
//!
//! **This is the first stage of that: a probe.** It establishes the facts the
//! loader depends on and no document states -- the level ABL enters at, where
//! it loads the image, whether the watchdogs are running, and where the panel
//! scans out from -- writes them into the `ramoops` console record, paints
//! the framebuffer it believes in, and resets the phone. Android's kernel then
//! shows the record as `/sys/fs/pstore/console-ramoops-0`.

#![no_std]
#![no_main]

mod board;
mod entry;
mod log;

use core::panic::PanicInfo;
use core::ptr;

use board::{
    FRAMEBUFFER, FRAMEBUFFER_STRIDE, PANEL_HEIGHT, PANEL_WIDTH, REBOOT_REASON,
    REBOOT_TO_BOOTLOADER, WATCHDOGS, WTCNT, WTCON,
};

/// How long the probe holds before resetting. Long enough that a phone which
/// comes back sooner cannot have run it: ABL alone boots Android in about 20.
const HOLD_SECONDS: u64 = 20;

/// Called from the entry sequence at the level ABL handed over at, before
/// anything else: record what was handed over, in case what comes next hangs.
extern "C" fn early(device_tree: u64, current_el: u64, loaded_at: u64) {
    let level = (current_el >> 2) & 0b11;
    log::start();
    log::text("ferrix-pixel7 probe\n");
    log::field("device tree  ", device_tree);
    log::field("loaded at    ", loaded_at);
    log::field("entered at EL", level);
    if level == 2 {
        // SAFETY: the level just read is EL2.
        let (hcr, sctlr) = unsafe { entry::el2_state() };
        log::field("HCR_EL2      ", hcr);
        log::field("SCTLR_EL2    ", sctlr);
    }
}

/// Called from the entry sequence at EL1: probe, paint, and reset.
extern "C" fn main(device_tree: u64) -> ! {
    log::field("now at EL    ", entry::current_el());
    let (_, frequency) = entry::counter();
    log::field("counter Hz   ", frequency);
    report_device_tree(device_tree);
    report_watchdogs();
    paint(device_tree);
    log::text("holding for ");
    log::decimal(HOLD_SECONDS);
    log::text(" seconds\n");
    wait(HOLD_SECONDS);
    log::text("resetting into fastboot\n");
    // SAFETY: an aligned register of the always-on PMU. If the secure world
    // owns it, the write faults into `trap`, which resets all the same.
    unsafe {
        ptr::write_volatile(REBOOT_REASON as *mut u32, REBOOT_TO_BOOTLOADER);
    }
    entry::system_reset()
}

/// Called from every vector: report the exception and reset.
extern "C" fn trap(kind: u64, syndrome: u64, at: u64, address: u64) -> ! {
    log::field("EXCEPTION    ", kind);
    log::field("ESR_EL1      ", syndrome);
    log::field("ELR_EL1      ", at);
    log::field("FAR_EL1      ", address);
    entry::system_reset()
}

/// Read one aligned word of device or memory, volatile.
///
/// # Safety
///
/// `address` must be four-byte aligned and readable without side effects.
unsafe fn read_word(address: u64) -> u32 {
    // SAFETY: the caller's contract.
    unsafe { ptr::read_volatile(address as *const u32) }
}

/// Check the device tree ABL passed looks like one, and record its size.
fn report_device_tree(device_tree: u64) {
    if device_tree == 0 || !device_tree.is_multiple_of(8) {
        log::text("device tree pointer unusable\n");
        return;
    }
    // SAFETY: the boot protocol passes an 8-byte aligned device tree in RAM.
    let magic = u32::from_be(unsafe { read_word(device_tree) });
    // SAFETY: as above; the size is the header's second word.
    let size = u32::from_be(unsafe { read_word(device_tree + 4) });
    log::field("dtb magic    ", u64::from(magic));
    log::field("dtb size     ", u64::from(size));
}

/// Record whether each watchdog is running and how far it has counted.
fn report_watchdogs() {
    for base in WATCHDOGS {
        // SAFETY: WTCON and WTCNT are aligned registers of a block the device
        // tree lists, and reading them has no side effect.
        let control = unsafe { read_word(base + WTCON) };
        // SAFETY: as above.
        let count = unsafe { read_word(base + WTCNT) };
        log::field("watchdog     ", base);
        log::field("  WTCON      ", u64::from(control));
        log::field("  WTCNT      ", u64::from(count));
    }
}

/// Paint the panel red, green and blue from the top, if the buffer believed
/// to be the panel's does not hold the device tree.
fn paint(device_tree: u64) {
    let size = FRAMEBUFFER_STRIDE * PANEL_HEIGHT;
    if (FRAMEBUFFER..FRAMEBUFFER + size).contains(&device_tree) {
        log::text("device tree is inside the framebuffer guess; not painting\n");
        return;
    }
    for row in 0..PANEL_HEIGHT {
        // A pixel is a little-endian word, alpha red green blue from the top.
        let colour: u32 = match row * 3 / PANEL_HEIGHT {
            0 => 0xffff_0000,
            1 => 0xff00_ff00,
            _ => 0xff00_00ff,
        };
        let line = FRAMEBUFFER + row * FRAMEBUFFER_STRIDE;
        for column in 0..PANEL_WIDTH {
            // SAFETY: the address is inside the buffer the board names, which
            // is RAM, aligned to a word, and not the device tree.
            unsafe {
                ptr::write_volatile((line + column * 4) as *mut u32, colour);
            }
        }
    }
    log::field("painted      ", FRAMEBUFFER);
}

/// Spin for `seconds` on the generic timer.
fn wait(seconds: u64) {
    let (start, frequency) = entry::counter();
    let end = start.saturating_add(frequency.saturating_mul(seconds));
    while entry::counter().0 < end {}
}

/// Report where the loader panicked and reset.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log::text("PANIC");
    if let Some(location) = info.location() {
        log::text(" at ");
        log::text(location.file());
        log::text(":");
        log::decimal(u64::from(location.line()));
    }
    log::byte(b'\n');
    entry::system_reset()
}
