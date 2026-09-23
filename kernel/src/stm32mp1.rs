//! The kernel's part in an STM32MP15 DK board's display: clocks, pins and a
//! reset line, then a device node a ring-3 driver is started on.
//!
//! The DK boards drive HDMI from the chip's LTDC through a Silicon Image
//! `SiI9022` bridge on an I2C bus (`docs/DISPLAY.md` §6). The driver,
//! `user/ltdc`, programs both controllers; what it cannot do is what every
//! peripheral on the chip shares -- the RCC's clock gates and the GPIO banks'
//! pin multiplexing -- and a driver that could write those could stop the
//! memory controller's clock or take the console's pins. So the kernel does
//! exactly these things, once, while it publishes device nodes, and nothing
//! more:
//!
//! * turns on the LTDC's clock and the bridge's I2C controller's, and puts
//!   that controller's kernel clock on the 64 MHz HSI so the driver has one
//!   timing to program;
//! * checks that the LTDC's pixel clock -- PLL4's Q output, which firmware
//!   set up and which clocks other things too -- is the 74.25 MHz that
//!   CEA-861's 720p60 needs, and leaves the display alone if it is not;
//! * muxes both controllers' pins as their `pinctrl-0` says;
//! * pulses the bridge's reset line.
//!
//! What the driver then gets is a node with two apertures, the LTDC's
//! registers and the I2C controller's, and the LTDC's interrupt.

use alloc::vec::Vec;
use core::fmt;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_fdt::{Fdt, GicInterrupt, Node};

use crate::mmio::Mmio;
use crate::{timer, vmap};

/// The LTDC's `compatible`.
pub(crate) const LTDC_COMPATIBLE: &str = "st,stm32-ltdc";
/// The HDMI bridge's.
const BRIDGE_COMPATIBLE: &str = "sil,sii9022";
/// The RCC's.
const RCC_COMPATIBLE: &str = "st,stm32mp1-rcc";
/// The STM32MP157's pin controller's, whose banks this module knows.
const PINCTRL_COMPATIBLE: &str = "st,stm32mp157-pinctrl";

/// The first GPIO bank's registers; bank `n` (A = 0 .. K = 10) is `n` pages
/// on (RM0436's memory map, and every `gpio@` node of the pin controller).
const GPIO_BASE: u64 = 0x5000_2000;
/// Banks A to K. Bank Z is elsewhere and secure; nothing here uses it.
const GPIO_BANKS: u32 = 11;

// The RCC's registers, from Linux's `drivers/clk/stm32/clk-stm32mp1.c`.
const RCC_HSICFGR: u64 = 0x18;
const RCC_APB4ENSETR: u64 = 0x200;
const RCC_OCRDYR: u64 = 0x808;
const RCC_RCK4SELR: u64 = 0x824;
const RCC_PLL4CR: u64 = 0x894;
const RCC_PLL4CFGR1: u64 = 0x898;
const RCC_PLL4CFGR2: u64 = 0x89C;
const RCC_PLL4FRACR: u64 = 0x8A0;
const RCC_I2C12CKSELR: u64 = 0x8C0;
const RCC_APB1ENSETR: u64 = 0xA00;
const RCC_AHB4ENSETR: u64 = 0xA28;
/// `APB4ENSETR`: the LTDC.
const LTDC_ENABLE: u32 = 1 << 0;
/// `PLL4CR`: on, locked, Q output enabled.
const PLL_ON: u32 = 1 << 0;
const PLL_READY: u32 = 1 << 1;
const PLL_Q_ENABLE: u32 = 1 << 5;
/// `PLL4FRACR`: the fraction is in use.
const FRAC_ENABLE: u32 = 1 << 16;
/// `OCRDYR`: the HSI is running.
const HSI_READY: u32 = 1 << 0;
/// `I2C12CKSELR`'s value for the HSI.
const I2C12_FROM_HSI: u32 = 2;

/// The HSI's undivided rate.
const HSI_HZ: u64 = 64_000_000;
/// The CSI's.
const CSI_HZ: u64 = 4_000_000;
/// The pixel clock 720p60 needs, and how far off it may be: HDMI sinks take
/// half a percent, which is CEA-861's own tolerance.
const PIXEL_HZ: u64 = 74_250_000;
const PIXEL_TOLERANCE_HZ: u64 = PIXEL_HZ / 200;

/// The I2C controllers whose clocks are on the I2C12 mux, by address, with
/// their `APB1ENSETR` bit.
const I2C12: [(u64, u32); 2] = [(0x4001_2000, 1 << 21), (0x4001_3000, 1 << 22)];

/// What the kernel prepared, for the device node.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Prepared {
    /// The LTDC's registers, the whole page RM0436 gives it.
    pub(crate) ltdc: (u64, u64),
    /// The I2C controller's, likewise.
    pub(crate) i2c: (u64, u64),
    /// The LTDC's interrupt.
    pub(crate) interrupt: GicInterrupt,
    /// The bridge's address on the bus.
    pub(crate) bridge: u32,
    /// The pixel clock found.
    pub(crate) pixel_hz: u64,
    /// Pins muxed.
    pub(crate) pins: usize,
}

impl fmt::Display for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LTDC at {:#x}, HDMI bridge at {:#x} on I2C {:#x}, pixel clock {}.{:03} MHz, {} pins muxed",
            self.ltdc.0,
            self.bridge,
            self.i2c.0,
            self.pixel_hz / 1_000_000,
            (self.pixel_hz / 1000) % 1000,
            self.pins
        )
    }
}

/// Find the board's display, and prepare it for a driver.
///
/// `Ok(None)` on a machine with no enabled LTDC wired to a `SiI9022`, which is
/// every machine but a DK board. `Err` names what stopped the kernel from
/// handing the display over; nothing it did before that is undone, and
/// nothing it did is harmful to leave: clocks on and pins muxed.
pub(crate) fn prepare(tree: &Fdt<'_>) -> Result<Option<Prepared>, &'static str> {
    let Some(ltdc) = tree
        .compatible_nodes(LTDC_COMPATIBLE)
        .find(Node::is_enabled)
    else {
        return Ok(None);
    };
    let Some((i2c, bridge)) = bridge_and_bus(tree) else {
        return Ok(None);
    };
    let ltdc_reg = ltdc.reg().next().ok_or("the LTDC has no registers")?;
    let i2c_reg = i2c
        .reg()
        .next()
        .ok_or("the I2C controller has no registers")?;
    let interrupt = tree
        .gic_interrupt_of(&ltdc, 0)
        .ok_or("the LTDC's interrupt does not reach the GIC")?;
    let bridge_address = bridge
        .reg()
        .next()
        .map(|region| region.address)
        .and_then(|address| u32::try_from(address).ok())
        .ok_or("the bridge has no address")?;
    let (_, i2c_enable) = I2C12
        .iter()
        .find(|(base, _)| *base == i2c_reg.address)
        .copied()
        .ok_or("the bridge is on an I2C controller whose clock this kernel does not know")?;
    // The bank addresses below are this pin controller's.
    if tree.compatible_nodes(PINCTRL_COMPATIBLE).next().is_none() {
        return Err("no STM32MP157 pin controller");
    }

    let rcc_node = tree
        .compatible_nodes(RCC_COMPATIBLE)
        .next()
        .ok_or("no RCC")?;
    let rcc_reg = rcc_node.reg().next().ok_or("the RCC has no registers")?;
    let rcc = Window::map(rcc_reg.address, PAGE_SIZE)?;

    let pixel_hz = pll4_q(tree, rcc.mmio)?;
    if pixel_hz.abs_diff(PIXEL_HZ) > PIXEL_TOLERANCE_HZ {
        return Err("PLL4's Q output is not the 74.25 MHz 720p60 needs");
    }
    if rcc.mmio.read32(RCC_HSICFGR) & 0x3 != 0 || rcc.mmio.read32(RCC_OCRDYR) & HSI_READY == 0 {
        return Err("the HSI is not running at 64 MHz");
    }

    // Clocks: the controllers, then every bank a pin is in. The set
    // registers take ones and leave the other bits alone.
    rcc.mmio.write32(RCC_APB4ENSETR, LTDC_ENABLE);
    rcc.mmio.write32(RCC_APB1ENSETR, i2c_enable);
    let selected = rcc.mmio.read32(RCC_I2C12CKSELR);
    rcc.mmio
        .write32(RCC_I2C12CKSELR, (selected & !0x7) | I2C12_FROM_HSI);

    let mut pins = Vec::new();
    collect_pins(tree, &ltdc, &mut pins)?;
    collect_pins(tree, &i2c, &mut pins)?;
    let reset = reset_line(tree, &bridge)?;
    let mut banks = pins.iter().fold(0_u32, |mask, pin| mask | (1 << pin.bank));
    if let Some((bank, _, _)) = reset {
        banks |= 1 << bank;
    }
    rcc.mmio.write32(RCC_AHB4ENSETR, banks);
    drop(rcc);

    for pin in &pins {
        pin.apply()?;
    }
    if let Some((bank, line, active_low)) = reset {
        pulse_reset(bank, line, active_low)?;
    }

    Ok(Some(Prepared {
        ltdc: (ltdc_reg.address, PAGE_SIZE),
        i2c: (i2c_reg.address, PAGE_SIZE),
        interrupt,
        bridge: bridge_address,
        pixel_hz,
        pins: pins.len(),
    }))
}

/// The enabled `SiI9022` and the I2C controller it is a child of.
fn bridge_and_bus<'a>(tree: &Fdt<'a>) -> Option<(Node<'a>, Node<'a>)> {
    // The nodes on the path to the one being looked at, by depth.
    let mut path: Vec<Node<'a>> = Vec::new();
    for node in tree.nodes() {
        path.truncate(node.depth);
        if node.is_compatible(BRIDGE_COMPATIBLE) && node.is_enabled() {
            let parent = path.last().copied()?;
            return parent.is_enabled().then_some((parent, node));
        }
        path.push(node);
    }
    None
}

/// The rate of PLL4's Q output, from the RCC's registers and the device
/// tree's HSE frequency: `ref / (M + 1) * (N + 1 + frac / 8192) / (Q + 1)`,
/// Linux's `pll_recalc_rate`.
fn pll4_q(tree: &Fdt<'_>, rcc: Mmio) -> Result<u64, &'static str> {
    let control = rcc.read32(RCC_PLL4CR);
    if control & (PLL_ON | PLL_READY | PLL_Q_ENABLE) != PLL_ON | PLL_READY | PLL_Q_ENABLE {
        return Err("PLL4's Q output is off");
    }
    let reference = match rcc.read32(RCC_RCK4SELR) & 0x3 {
        0 => HSI_HZ >> (rcc.read32(RCC_HSICFGR) & 0x3),
        1 => hse_hz(tree).ok_or("PLL4 runs from the HSE, whose rate the tree does not give")?,
        2 => CSI_HZ,
        _ => return Err("PLL4's reference is not a clock"),
    };
    let config = rcc.read32(RCC_PLL4CFGR1);
    let m = u64::from((config >> 16) & 0x3F) + 1;
    let n = u64::from(config & 0x1FF) + 1;
    let fraction = rcc.read32(RCC_PLL4FRACR);
    let frac = if fraction & FRAC_ENABLE != 0 {
        u64::from((fraction >> 3) & 0x1FFF)
    } else {
        0
    };
    let q = u64::from((rcc.read32(RCC_PLL4CFGR2) >> 8) & 0x7F) + 1;
    let vco = reference * n / m + reference * frac / (m * 8192);
    Ok(vco / q)
}

/// The HSE's rate: the `clock-frequency` of the fixed clock named `clk-hse`.
fn hse_hz(tree: &Fdt<'_>) -> Option<u64> {
    tree.nodes()
        .find(|node| node.name == "clk-hse")?
        .property("clock-frequency")?
        .as_u32()
        .map(u64::from)
}

/// One pin, as a `pinmux` cell and its group's properties say.
#[derive(Clone, Copy, Debug)]
struct Pin {
    bank: u32,
    line: u32,
    /// `MODER`: 0 input, 1 output, 2 alternate function, 3 analog.
    mode: u32,
    /// The alternate function, for mode 2.
    function: u32,
    open_drain: bool,
    /// `PUPDR`: 0 none, 1 up, 2 down.
    pull: u32,
    speed: u32,
}

impl Pin {
    /// Write the pin's bank registers, alternate function before mode as
    /// Linux does, so it never drives the wrong function.
    fn apply(&self) -> Result<(), &'static str> {
        let bank = Window::map(GPIO_BASE + u64::from(self.bank) * PAGE_SIZE, PAGE_SIZE)?;
        let r = bank.mmio;
        let two = self.line * 2;
        let update = |offset: u64, shift: u32, width: u32, value: u32| {
            let mask = ((1 << width) - 1) << shift;
            r.write32(
                offset,
                (r.read32(offset) & !mask) | ((value << shift) & mask),
            );
        };
        let (afr, shift) = if self.line < 8 {
            (GPIO_AFRL, self.line * 4)
        } else {
            (GPIO_AFRH, (self.line - 8) * 4)
        };
        update(afr, shift, 4, self.function);
        update(GPIO_OTYPER, self.line, 1, u32::from(self.open_drain));
        update(GPIO_OSPEEDR, two, 2, self.speed);
        update(GPIO_PUPDR, two, 2, self.pull);
        update(GPIO_MODER, two, 2, self.mode);
        Ok(())
    }
}

// A GPIO bank's registers.
const GPIO_MODER: u64 = 0x00;
const GPIO_OTYPER: u64 = 0x04;
const GPIO_OSPEEDR: u64 = 0x08;
const GPIO_PUPDR: u64 = 0x0C;
const GPIO_BSRR: u64 = 0x18;
const GPIO_AFRL: u64 = 0x20;
const GPIO_AFRH: u64 = 0x24;

/// Every pin `node`'s `pinctrl-0` groups name.
fn collect_pins(tree: &Fdt<'_>, node: &Node<'_>, out: &mut Vec<Pin>) -> Result<(), &'static str> {
    let groups = node
        .property("pinctrl-0")
        .ok_or("a display controller has no pinctrl-0")?;
    for phandle in groups.cells() {
        let mut found = false;
        for pins in children(tree, phandle) {
            found = true;
            pins_of(&pins, out)?;
        }
        if !found {
            return Err("a pinctrl-0 group has no pins");
        }
    }
    Ok(())
}

/// The children of the node whose phandle is `phandle`.
fn children<'a>(tree: &Fdt<'a>, phandle: u32) -> Vec<Node<'a>> {
    let mut found = Vec::new();
    let mut parent = None;
    for node in tree.nodes() {
        match parent {
            None if node.phandle() == Some(phandle) => parent = Some(node.depth),
            None => {}
            Some(depth) if node.depth <= depth => break,
            Some(depth) if node.depth == depth + 1 => found.push(node),
            Some(_) => {}
        }
    }
    found
}

/// The pins of one `pins` subnode: its `pinmux` cells, each
/// `STM32_PINMUX(port, line, mode)` -- `(port * 16 + line) << 8 | mode`, with
/// mode 0 GPIO, 1 to 16 alternate functions 0 to 15, 17 analog -- and the
/// group's bias, drive and slew rate.
fn pins_of(pins: &Node<'_>, out: &mut Vec<Pin>) -> Result<(), &'static str> {
    let cells = pins
        .property("pinmux")
        .ok_or("a pin group has no pinmux")?
        .cells();
    let pull = if pins.property("bias-pull-up").is_some() {
        1
    } else if pins.property("bias-pull-down").is_some() {
        2
    } else {
        0
    };
    let open_drain = pins.property("drive-open-drain").is_some();
    let speed = pins
        .property("slew-rate")
        .and_then(|property| property.as_u32())
        .unwrap_or(0)
        .min(3);
    for cell in cells {
        let number = cell >> 8;
        let (bank, line) = (number / 16, number % 16);
        if bank >= GPIO_BANKS {
            return Err("a pin is in a bank this kernel does not map");
        }
        let (mode, function) = match cell & 0xFF {
            0 => (0, 0),
            alternate @ 1..=16 => (2, alternate - 1),
            17 => (3, 0),
            _ => return Err("a pinmux cell names no mode"),
        };
        out.push(Pin {
            bank,
            line,
            mode,
            function,
            open_drain,
            pull,
            speed,
        });
    }
    Ok(())
}

/// The bridge's `reset-gpios`: bank, line and whether it is active low.
fn reset_line(tree: &Fdt<'_>, bridge: &Node<'_>) -> Result<Option<(u32, u32, bool)>, &'static str> {
    let Some(property) = bridge.property("reset-gpios") else {
        return Ok(None);
    };
    let mut cells = property.cells();
    let (Some(phandle), Some(line), Some(flags)) = (cells.next(), cells.next(), cells.next())
    else {
        return Err("the bridge's reset-gpios is not one GPIO");
    };
    let bank_name = tree
        .node_by_phandle(phandle)
        .and_then(|node| node.property("st,bank-name")?.as_str())
        .ok_or("the bridge's reset GPIO is not a bank's")?;
    let bank = match bank_name.as_bytes() {
        [b'G', b'P', b'I', b'O', letter @ b'A'..=b'K'] => u32::from(letter - b'A'),
        _ => return Err("the bridge's reset GPIO is in a bank this kernel does not map"),
    };
    if line > 15 {
        return Err("the bridge's reset GPIO names no line");
    }
    Ok(Some((bank, line, flags & 1 != 0)))
}

/// Assert the reset line for a millisecond and let it go: the `SiI9022`'s
/// datasheet asks for 100 µs, and Linux gives it 150.
fn pulse_reset(bank: u32, line: u32, active_low: bool) -> Result<(), &'static str> {
    let window = Window::map(GPIO_BASE + u64::from(bank) * PAGE_SIZE, PAGE_SIZE)?;
    let r = window.mmio;
    let high = 1_u32 << line;
    let low = 1_u32 << (line + 16);
    let (asserted, released) = if active_low { (low, high) } else { (high, low) };
    // The level first, then the pin an output: no glitch the other way.
    r.write32(GPIO_BSRR, asserted);
    let two = line * 2;
    let otype = r.read32(GPIO_OTYPER) & !(1 << line);
    r.write32(GPIO_OTYPER, otype);
    let mode = r.read32(GPIO_MODER) & !(0x3 << two);
    r.write32(GPIO_MODER, mode | (1 << two));
    spin(1_000_000);
    r.write32(GPIO_BSRR, released);
    Ok(())
}

/// Wait `nanos` on the clock.
fn spin(nanos: u64) {
    let until = timer::now_nanos().saturating_add(nanos);
    while timer::now_nanos() < until {
        core::hint::spin_loop();
    }
}

/// A page of device registers mapped for as long as this lives.
struct Window {
    at: u64,
    mmio: Mmio,
}

impl Window {
    fn map(phys: u64, len: u64) -> Result<Window, &'static str> {
        let at = vmap::map_device(phys, len).map_err(|_| "device registers could not be mapped")?;
        Ok(Window {
            at,
            mmio: Mmio::at(at),
        })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = vmap::unmap_device(self.at);
    }
}
