//! What ABL leaves the USB controller in, read and logged -- nothing written.
//!
//! ABL runs fastboot over the DWC3 on the USB-C port, in device mode, until it
//! jumps here, and the phone drops off USB the moment it does. Whether Ferrix
//! can present a USB serial port without touching a power domain depends on
//! what is still on at that moment: the HSI0 power domain the controller, its
//! PHY and HSI0's clock unit sit in; the PMU's PHY isolation control; the
//! controller itself, and whether it is still running.
//! `docs/PIXEL7-USB-HANDOVER.md` says why each matters. The stage-2 MPU in
//! front of HSI0 (`s2mpu_hsi0`) is not read: it is a security block, and the
//! device rule leaves those alone entirely.
//!
//! The DWC3's offsets are Linux's `drivers/usb/dwc3/core.h`, the blocks'
//! addresses the phone's device tree's (`panther.dts`). Every read here is a
//! plain register read with no side effect. A block that is powered down or
//! unclocked can refuse a read or hang the bus; both end the loader, and the
//! watchdog's reset keeps the record. So [`report`] reads the power domain
//! first and goes no further into HSI0 when it is off, and logs each word as
//! it is read: the last line before a hang names the block, and a refused
//! read's `FAR` names the word.

use core::ptr;

use crate::log::say;

/// The PMU's words for the USB PHY: `pmu_offset` and `pmu_offset_dp` of
/// `phy@11200000`, isolation controls for the USB and display-port halves.
const PMU_PHY: [(&str, u64); 2] = [("USB", 0x1806_3EB0), ("DP", 0x1806_3EB4)];

/// `pd-hsi0`, in the PMU: configuration, status and three more words, as
/// `display.rs` reads `pd-disp` (whose `+0x14` refused a read).
const PD_HSI0: u64 = 0x1806_2080;
/// Its status word; bit 0 is set while the domain is on (`pd-disp` and
/// `pd-dpu`, both on, read 1 there).
const PD_STATUS: u64 = 0x04;

/// The DWC3's register window, `usb@11210000` and its `dwc3` child.
const DWC3: u64 = 0x1121_0000;

/// The DWC3's global registers, by name, in the order logged: first the ones
/// that say whether the core answers and what it is.
const GLOBAL_REGISTERS: [(&str, u64); 22] = [
    ("GSNPSID", 0xC120),
    ("GHWPARAMS0", 0xC140),
    ("GHWPARAMS1", 0xC144),
    ("GHWPARAMS2", 0xC148),
    ("GHWPARAMS3", 0xC14C),
    ("GHWPARAMS4", 0xC150),
    ("GHWPARAMS6", 0xC158),
    ("GHWPARAMS7", 0xC15C),
    ("GHWPARAMS8", 0xC600),
    ("GSBUSCFG0", 0xC100),
    ("GCTL", 0xC110),
    ("GSTS", 0xC118),
    ("GUID", 0xC128),
    ("GUCTL", 0xC12C),
    ("GDBGLTSSM", 0xC164),
    ("GUSB2PHYCFG0", 0xC200),
    ("GUSB3PIPECTL0", 0xC2C0),
    ("GFLADJ", 0xC630),
    ("GEVNTADRLO0", 0xC400),
    ("GEVNTADRHI0", 0xC404),
    ("GEVNTSIZ0", 0xC408),
    ("GEVNTCOUNT0", 0xC40C),
];

/// The DWC3's device registers: whether ABL left it running, at what speed
/// and address, and with which endpoints on.
const DEVICE_REGISTERS: [(&str, u64); 5] = [
    ("DCFG", 0xC700),
    ("DCTL", 0xC704),
    ("DEVTEN", 0xC708),
    ("DSTS", 0xC70C),
    ("DALEPENA", 0xC720),
];

/// The transmit FIFOs whose sizes are logged, from `GTXFIFOSIZ(0)` on, and
/// the receive FIFO `GRXFIFOSIZ(0)`: the room ABL's endpoints were given.
const TX_FIFOS: u64 = 4;
const GTXFIFOSIZ: u64 = 0xC300;
const GRXFIFOSIZ: u64 = 0xC380;

/// The PHY's windows, from `phy@11200000`'s `reg`: link control, the combo
/// (super-speed) PHY, and the high-speed PHY. The first words of each.
const PHY_WINDOWS: [(&str, u64); 3] = [
    ("link", 0x1120_0000),
    ("combo", 0x110F_0000),
    ("hs", 0x1110_0000),
];
/// Words read from the start of each PHY window.
const PHY_WORDS: u64 = 8;

/// Read one 32-bit register.
fn read(address: u64) -> u32 {
    // SAFETY: every address here is an aligned word inside a register block
    // the device tree gives the USB controller, its PHY or the PMU, read with
    // the MMU off, which makes it a device access. None of these registers
    // has a side effect on reading.
    unsafe { ptr::read_volatile(address as *const u32) }
}

/// Read `address` and log it under `label`, on a line of its own, as soon as
/// it is read (see the module's note).
fn show(label: &str, address: u64) {
    say!("    {label} {address:#x} = {:#010x}", read(address));
}

/// Log `count` words from `base` on, labelled by their offset.
fn show_words(base: u64, count: u64) {
    for offset in (0..count * 4).step_by(4) {
        say!("    +{offset:#05x} {:#010x}", read(base + offset));
    }
}

/// Log the USB state ABL handed over, most important first.
pub(crate) fn report() {
    say!("  usb  PMU PHY isolation");
    for (name, address) in PMU_PHY {
        show(name, address);
    }

    say!("  usb  pd-hsi0");
    show_words(PD_HSI0, 5);
    if read(PD_HSI0 + PD_STATUS) & 1 == 0 {
        say!("  usb  HSI0 is powered down: nothing inside it read");
        return;
    }

    say!("  usb  DWC3 global");
    for (name, offset) in GLOBAL_REGISTERS {
        show(name, DWC3 + offset);
    }
    for fifo in 0..TX_FIFOS {
        show("GTXFIFOSIZ", DWC3 + GTXFIFOSIZ + fifo * 4);
    }
    show("GRXFIFOSIZ0", DWC3 + GRXFIFOSIZ);

    say!("  usb  DWC3 device");
    for (name, offset) in DEVICE_REGISTERS {
        show(name, DWC3 + offset);
    }

    for (name, base) in PHY_WINDOWS {
        say!("  usb  PHY {name} at {base:#x}");
        show_words(base, PHY_WORDS);
    }

    say!("  usb  done");
}
