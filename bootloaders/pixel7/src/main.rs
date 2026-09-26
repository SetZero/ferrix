//! Ferrix's second-stage loader for the Pixel 7 (`panther`).
//!
//! The phone's own bootloader, ABL, is signed and cannot be replaced; what an
//! unlocked one will do is load an Android boot image and jump to its kernel
//! under the Linux arm64 boot protocol. This program is that kernel. It does
//! for Ferrix what `boot/` does under UEFI -- finds memory, loads the kernel
//! and its initramfs, and hands over a `BootInfo` -- from what ABL provides
//! instead: a device tree, and a machine at EL2 with the MMU off.
//!
//! It keeps a log in the `ramoops` console record, which the Ferrix kernel
//! continues when the command line tells it to, and which survives the
//! watchdog reset that ends a run; `README.md` says how to read it.

#![no_std]
#![no_main]

mod board;
mod display;
mod entry;
mod kaslr;
mod load;
mod log;
mod memory;
mod payload;
mod seed;

use core::panic::PanicInfo;
use core::ptr;

use ferrix_bootinfo::{Framebuffer, MemKind};
use ferrix_fdt::Fdt;

use board::{CMDLINE, GUEST_CMDLINE, WATCHDOGS, WTCNT, WTCON};
use log::say;
use memory::Memory;

/// The largest device tree the loader will take. The Pixel 7's is 384 KiB.
const MAX_DEVICE_TREE: u32 = 2 * 1024 * 1024;

unsafe extern "C" {
    /// The first byte of the loader's image, from the linker script.
    static _head: u8;
    /// One past the loader's last byte, stack included.
    static _end: u8;
}

/// Called from the entry sequence at the level ABL handed over at, before
/// anything else: record what was handed over, in case what comes next hangs.
extern "C" fn early(device_tree: u64, current_el: u64, loaded_at: u64) {
    let level = (current_el >> 2) & 0b11;
    if level == 1 && is_crosvm(device_tree) {
        board::set_guest();
    }
    log::start();
    say!("ferrix-pixel7 loader {}", env!("CARGO_PKG_VERSION"));
    say!("  entered at EL{level}, loaded at {loaded_at:#x}, device tree at {device_tree:#x}");
    if level == 2 {
        // SAFETY: the level just read is EL2.
        let (hcr, sctlr) = unsafe { entry::el2_state() };
        say!("  HCR_EL2 {hcr:#x}, SCTLR_EL2 {sctlr:#x}");
    }
}

/// Called from the entry sequence at EL1: load Ferrix and start it.
extern "C" fn main(device_tree: u64) -> ! {
    let (_, frequency) = entry::counter();
    say!(
        "  now at EL{}, counter at {frequency} Hz",
        entry::current_el()
    );
    let framebuffer = if board::is_guest() {
        say!("  a guest of crosvm: no watchdogs, no screen, console on its 16550");
        None
    } else {
        report_watchdogs();
        display::report();
        display::take_over()
    };
    if let Err(why) = start(device_tree, framebuffer.unwrap_or(Framebuffer::NONE)) {
        say!("FERRIX-PANIC loader: {why}");
    }
    // In a guest no watchdog ends the run: whoever started crosvm stops it.
    entry::wait_for_watchdog()
}

/// The kernel's command line: `base`, then every `ferrix.*` word of the
/// device tree's `bootargs`, as many as `buffer` holds.
///
/// In a guest, `bootargs` is crosvm's, which `crosvm run -p` adds to. On the
/// phone it is ABL's: Android's own, and the boot image header's command
/// line, which `launcher/helper.py` fills for a run that asks for Ferrix's
/// stat service. Either way only `ferrix.*` words are taken.
fn command_line<'a>(base: &'a str, tree: &Fdt<'_>, buffer: &'a mut [u8]) -> &'a str {
    let mut used = 0;
    let words = core::iter::once(base).chain(
        tree.bootargs()
            .unwrap_or("")
            .split_whitespace()
            .filter(|word| word.starts_with("ferrix.")),
    );
    for word in words {
        let space = usize::from(used > 0);
        let Some(room) = buffer.get_mut(used..used + space + word.len()) else {
            break;
        };
        let (gap, text) = room.split_at_mut(space);
        gap.fill(b' ');
        text.copy_from_slice(word.as_bytes());
        used += space + word.len();
    }
    match buffer.get(..used).map(core::str::from_utf8) {
        Some(Ok(line)) => line,
        _ => base,
    }
}

/// Whether the device tree at `device_tree` is crosvm's: its `stdout-path`
/// an `ns16550a` at [`board::GUEST_UART`], which is where the log goes from
/// here on.
fn is_crosvm(device_tree: u64) -> bool {
    let Ok(blob) = device_tree_blob(device_tree) else {
        return false;
    };
    let Ok(tree) = Fdt::parse(blob) else {
        return false;
    };
    let Some(path) = tree.stdout_path() else {
        return false;
    };
    let path = path.split(':').next().unwrap_or(path);
    tree.find_node(path).is_some_and(|node| {
        node.is_compatible("ns16550a")
            && node
                .reg()
                .next()
                .is_some_and(|region| region.address == board::GUEST_UART)
    })
}

/// Everything between the hand-over and the kernel, as one fallible step.
fn start(device_tree: u64, framebuffer: Framebuffer) -> Result<(), &'static str> {
    let blob = device_tree_blob(device_tree)?;
    let tree = Fdt::parse(blob).map_err(|_| "ABL's device tree does not parse")?;
    let mut memory = Memory::new();
    memory.add_device_tree(&tree)?;
    let loader = loader_image();
    memory.mark(loader.0, loader.1, MemKind::Loader)?;
    // ABL's own copy of the tree is left behind once the loader has taken one
    // for the kernel.
    memory.mark(device_tree, blob.len() as u64, MemKind::Loader)?;
    // ABL's framebuffer, which the display keeps fetching: not RAM for the
    // kernel to hand out, or the screen shows whatever it put there. A guest
    // has no screen, and nothing at that address.
    if !board::is_guest() {
        memory.mark(
            display::FRAMEBUFFER,
            display::FRAMEBUFFER_BYTES,
            MemKind::Framebuffer,
        )?;
    }
    say!(
        "  device tree {} bytes, model {:?}",
        blob.len(),
        tree.model()
    );
    let seed = seed::gather(&tree, blob);
    say!(
        "  /chosen holds {} random bytes, {}",
        seed.found,
        if seed.is_any() {
            "passed on to the kernel's seed"
        } else {
            "so the kernel has no seed from firmware"
        }
    );
    let randomness = kaslr::gather(&tree);
    let mut line = [0_u8; 1024];
    let base = if board::is_guest() {
        GUEST_CMDLINE
    } else {
        CMDLINE
    };
    let cmdline = command_line(base, &tree, &mut line);
    say!("  kernel command line: {cmdline}");
    load::boot(
        &mut memory,
        load::Carried {
            device_tree: blob,
            cmdline,
            loader,
            framebuffer,
            seed,
            randomness,
        },
    )
}

/// ABL's device tree, as bytes, after checking its header.
fn device_tree_blob(device_tree: u64) -> Result<&'static [u8], &'static str> {
    if device_tree == 0 || !device_tree.is_multiple_of(8) {
        return Err("ABL passed no usable device tree pointer");
    }
    // SAFETY: the boot protocol passes an 8-byte aligned device tree in RAM.
    let magic = u32::from_be(unsafe { ptr::read_volatile(device_tree as *const u32) });
    // SAFETY: as above; the size is the header's second word.
    let size = u32::from_be(unsafe { ptr::read_volatile((device_tree + 4) as *const u32) });
    if magic != 0xd00d_feed || size > MAX_DEVICE_TREE {
        return Err("what ABL passed does not look like a device tree");
    }
    // SAFETY: the header just read says the tree is `size` bytes, bounded
    // above, and nothing writes it while the loader runs.
    Ok(unsafe { core::slice::from_raw_parts(device_tree as *const u8, size as usize) })
}

/// Where the loader's image is, and how long, stack included.
fn loader_image() -> (u64, u64) {
    let head = (&raw const _head) as u64;
    let end = (&raw const _end) as u64;
    (head, end - head)
}

/// Record whether each watchdog is running. They are left as ABL set them:
/// their reset is what ends a run and brings the phone back to Android, where
/// the log can be read.
fn report_watchdogs() {
    for base in WATCHDOGS {
        // SAFETY: aligned registers of a block the device tree lists, with no
        // side effect on reading.
        let control = unsafe { ptr::read_volatile((base + WTCON) as *const u32) };
        // SAFETY: as above.
        let count = unsafe { ptr::read_volatile((base + WTCNT) as *const u32) };
        say!("  watchdog {base:#x}: WTCON {control:#x}, WTCNT {count:#x}");
    }
}

/// Called from every vector: report the exception, and leave the reset to a
/// watchdog, whose reset keeps the report.
extern "C" fn trap(kind: u64, syndrome: u64, at: u64, address: u64) -> ! {
    say!("FERRIX-PANIC loader exception {kind}: ESR {syndrome:#x} ELR {at:#x} FAR {address:#x}");
    entry::wait_for_watchdog()
}

/// Report where the loader panicked, and leave the reset to a watchdog.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    say!("FERRIX-PANIC loader: {info}");
    entry::wait_for_watchdog()
}
