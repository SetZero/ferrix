//! What ABL leaves the display in, read and logged -- nothing written.
//!
//! ABL drives the panel (its "bootloader is unlocked" warning is on screen
//! before this loader runs), so the display controller, the DSI link and the
//! panel are up at hand-off. Whether Ferrix can reuse that depends on numbers
//! only the phone has: whether the controller is still running, in command
//! mode and on which trigger; which window is on and which DPP fetches it;
//! where that DPP reads its pixels from, in what format and at what size; and
//! whether the display's `SysMMU` is translating those reads.
//!
//! Register offsets are from Google's gs201 display driver
//! (`kernel/google-modules/display`, `samsung/cal_9845/regs-decon.h` and
//! `regs-dpp.h`, which `cal_9855` -- gs201 -- builds on); the block addresses
//! are the phone's device tree's.
//!
//! The first probe, which read all of [`report`]'s blocks, reset the phone at
//! once (`watchdog,apc,early`) and lost the `ramoops` record with it, so
//! nothing says which read did it. Since then the loader leaves its reset to a
//! watchdog, which keeps the record, and [`show`] logs each word as it is
//! read. Both power domains read as on, and DECON0 answers.

use core::ptr;

use crate::log::say;

/// DECON0 `main`: global control, trigger and blender registers.
const DECON_MAIN: u64 = 0x1C24_0000;
/// DECON0 `win`: each window's position and colour map, 0x1000 apart.
const DECON_WIN: u64 = 0x1C25_0000;
/// DECON0 `wincon`: each window's enable and channel, 0x1000 apart.
const DECON_WINCON: u64 = 0x1C27_0000;
/// The DPPs' DMA blocks, 0x1000 apart.
const DPP_DMA: u64 = 0x1C0B_0000;
/// The display's first `SysMMU`, `samsung,sysmmu-v8`, "DPU L0/L1".
const SYSMMU_DPU: u64 = 0x1C10_0000;

/// The power domains the display sits in: `pd-disp`, and `pd-dpu` inside it,
/// both in the power management unit.
const POWER_DOMAINS: [(&str, u64); 2] = [("pd-disp", 0x1806_2280), ("pd-dpu", 0x1806_2200)];

/// Windows and DPPs DECON0 has.
const WINDOWS: u64 = 6;

/// `DECON_MAIN` registers, by name, in the order logged.
const MAIN_REGISTERS: [(&str, u64); 12] = [
    ("VERSION", 0x000),
    ("FRAME_COUNT", 0x004),
    ("GLOBAL_CON", 0x020),
    ("TRIG_CON", 0x030),
    ("HW_TE_CNT", 0x038),
    ("SHD_REG_UP_REQ", 0x050),
    ("DATA_PATH_CON_0", 0x200),
    ("SRAM_EN_OF_PRI_0", 0x210),
    ("BLD_BG_IMG_SIZE_PRI", 0x220),
    ("BLD_BG_IMG_COLOR_0", 0x224),
    ("BLD_BG_IMG_COLOR_1", 0x228),
    ("OF_SIZE_0", 0x290),
];

/// Read one 32-bit register.
fn read(address: u64) -> u32 {
    // SAFETY: every address here is inside a register block the device tree
    // gives the display, read with the MMU off, which makes it a device
    // access. A read has no side effect in any of these blocks' registers.
    unsafe { ptr::read_volatile(address as *const u32) }
}

/// Read `address` and log it under `label`, on a line of its own.
///
/// One word to a line, each logged as soon as it is read: a read the block
/// refuses is a synchronous external abort, which ends the loader, and this
/// way everything read before it is in the log and the abort's `FAR` names
/// the one that was refused.
fn show(label: &str, address: u64) {
    say!("    {label} {address:#x} = {:#010x}", read(address));
}

/// Log the display state ABL handed over, most important first.
pub(crate) fn report() {
    // `pd-disp` answered its first five words and refused `+0x14`.
    for (name, base) in POWER_DOMAINS {
        say!("  display  {name}");
        for offset in (0..0x14).step_by(4) {
            show("word", base + offset);
        }
    }

    say!("  display  DECON0 main");
    for (name, offset) in MAIN_REGISTERS {
        show(name, DECON_MAIN + offset);
    }

    // ABL's log says it shows its first window on window 5.
    for window in (0..WINDOWS).rev() {
        say!("  display  window {window}");
        let win = DECON_WIN + window * 0x1000;
        show("CON", DECON_WINCON + window * 0x1000);
        show("FUNC", win + 0x04);
        show("START", win + 0x0C);
        show("END", win + 0x10);
        show("MAP0", win + 0x14);
        show("MAP1", win + 0x18);
    }

    for dpp in 0..WINDOWS {
        say!("  display  DPP{dpp} DMA");
        let dma = DPP_DMA + dpp * 0x1000;
        show("ENABLE", dma);
        show("IN_CTRL_0", dma + 0x08);
        show("SRC_SIZE", dma + 0x10);
        show("IMG_SIZE", dma + 0x18);
        show("BASEADDR_Y8", dma + 0x40);
        show("SRC_STRIDE_0", dma + 0x50);
        show("SRC_STRIDE_1", dma + 0x54);
    }

    // Its first three words read as zero; `+0xC` is refused.
    say!("  display  SysMMU");
    for offset in [0x0, 0x4, 0x8] {
        show("word", SYSMMU_DPU + offset);
    }
    say!("  display  done");
}

/// ABL's framebuffer, which DPP0 fetches for window 5: its physical address
/// (the display's `SysMMU` reads as off), and its size in pixels.
pub(crate) const FRAMEBUFFER: u64 = 0xFAC0_0000;
/// Its width and height.
const WIDTH: u64 = 1080;
const HEIGHT: u64 = 2400;
/// Its bytes: `BGRA8888`, format 0, four bytes a pixel, rows packed.
pub(crate) const FRAMEBUFFER_BYTES: u64 = WIDTH * HEIGHT * 4;

/// `DECON_MAIN`'s trigger control.
const TRIG_CON: u64 = DECON_MAIN + 0x030;
/// `TRIG_CON`: the TE pin triggers frames.
const HW_TRIG_EN: u32 = 1 << 0;
/// `TRIG_CON`: and DECON ignores it.
const HW_TRIG_MASK_DECON: u32 = 1 << 4;
/// `GLOBAL_CON`: command mode.
const GLOBAL_CON_COMMAND_MODE: u32 = 1 << 8;

/// First light: fill ABL's framebuffer -- the top two thirds white, the rest
/// black, so which way up it is shows -- and unmask the TE trigger ABL
/// masked, so DECON sends it on the next TE.
///
/// It writes that RAM and `TRIG_CON`, both gone at the next reset, and
/// nothing else: no command reaches the panel, whose link ABL set up. Only if
/// the hardware is still as ABL left it -- DPP0 fetching 1080 x 2400 from
/// [`FRAMEBUFFER`] for DECON in command mode -- and otherwise says why not.
pub(crate) fn first_light() {
    let base = read(DPP_DMA + 0x40);
    let size = read(DPP_DMA + 0x18);
    let global = read(DECON_MAIN + 0x020);
    if u64::from(base) != FRAMEBUFFER
        || size != 0x0960_0438
        || global & GLOBAL_CON_COMMAND_MODE == 0
    {
        say!(
            "  display  not as ABL left it (base {base:#x}, size {size:#x}, GLOBAL_CON {global:#x}); nothing written"
        );
        return;
    }

    crate::entry::invalidate_dcache(FRAMEBUFFER, FRAMEBUFFER_BYTES);
    for row in 0..HEIGHT {
        let value = if row < HEIGHT * 2 / 3 { u32::MAX } else { 0 };
        let line = FRAMEBUFFER + row * WIDTH * 4;
        for column in 0..WIDTH {
            // SAFETY: inside ABL's framebuffer, RAM that no reserved region
            // covers and nothing else in the loader uses; the caches are off,
            // so the write reaches RAM, where DPP0 reads it.
            unsafe { ptr::write_volatile((line + column * 4) as *mut u32, value) };
        }
    }

    let before = read(TRIG_CON);
    let frames = read(DECON_MAIN + 0x004);
    // SAFETY: DECON's trigger control, written as Google's driver unmasks a
    // TE trigger: enable set, DECON's mask cleared, the rest kept.
    unsafe {
        ptr::write_volatile(
            TRIG_CON as *mut u32,
            (before | HW_TRIG_EN) & !HW_TRIG_MASK_DECON,
        );
    }
    let (start, hz) = crate::entry::counter();
    while crate::entry::counter().0.wrapping_sub(start) < hz / 5 {}
    say!(
        "  display  framebuffer filled; TRIG_CON {before:#x} -> {:#x}; frames {frames} -> {} in 200 ms",
        read(TRIG_CON),
        read(DECON_MAIN + 0x004)
    );
}
