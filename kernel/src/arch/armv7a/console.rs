//! Which serial port this machine's console is.
//!
//! Two of them, on the machines this architecture runs on, and that is the
//! whole reason this module exists between the facade and a driver: QEMU's
//! `virt` and most Arm development boards have a PL011, and the STM32MP157 has
//! ST's own USART at an address of its own. Both drivers are in
//! `kernel/src/arch/` beside the GIC; what is here is how the machine's
//! description says which one it has, and the `write_byte` the kernel's
//! console calls without having to know.

use core::sync::atomic::{AtomicU8, Ordering};

use ferrix_bootinfo::option_in;
use ferrix_fdt::{Fdt, Node};

use crate::arch::{pl011, stm32_usart};
use crate::early::{EarlyError, EarlyMemory};

/// The `compatible` string of the Arm primecell UART.
const PL011: &str = "arm,pl011";

/// The `compatible` string of ST's USART, which every UART on an STM32MP15
/// declares.
const STM32_USART: &str = "st,stm32h7-uart";

/// A port this kernel can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Port {
    /// The Arm primecell.
    Pl011 = 1,
    /// ST's USART.
    Stm32 = 2,
}

impl Port {
    /// Which port `node` describes, if it is one this kernel can drive.
    fn of(node: &Node<'_>) -> Option<Port> {
        if node.is_compatible(PL011) {
            return Some(Port::Pl011);
        }
        if node.is_compatible(STM32_USART) {
            return Some(Port::Stm32);
        }
        None
    }

    /// The port `console=` names, if it names one this kernel can drive.
    fn named(name: &str) -> Option<Port> {
        match name {
            "pl011" => Some(Port::Pl011),
            "stm32" => Some(Port::Stm32),
            _ => None,
        }
    }
}

/// Which port [`init`] brought up, as its discriminant, or zero until it has.
static PORT: AtomicU8 = AtomicU8::new(0);

/// The port [`init`] brought up.
fn port() -> Option<Port> {
    match PORT.load(Ordering::Relaxed) {
        1 => Some(Port::Pl011),
        2 => Some(Port::Stm32),
        _ => None,
    }
}

/// Bring up the console the device tree names, and map its registers.
///
/// `/chosen`'s `stdout-path` first, which is the machine saying which of its
/// ports the console is. Failing that — or naming one this kernel has no
/// driver for — the first port in the tree that it does have one for, skipping
/// any the tree has turned off: an STM32MP15 describes eight UARTs and a board
/// enables the one it wired to a connector.
pub(crate) fn init(tree: &Fdt<'_>, memory: &mut EarlyMemory) -> Result<(), EarlyError> {
    let (node, port) = match forced(tree) {
        Some(found) => found,
        None => {
            let named = tree
                .console()
                .and_then(|node| Port::of(&node).map(|port| (node, port)));
            named
                .or_else(|| first_enabled(tree))
                .ok_or(EarlyError::NoConsole)?
        }
    };

    let registers = node.reg().next().ok_or(EarlyError::NoConsole)?;
    match port {
        Port::Pl011 => pl011::init(memory, registers.address)?,
        Port::Stm32 => stm32_usart::init(memory, registers.address)?,
    }

    PORT.store(port as u8, Ordering::Relaxed);
    Ok(())
}

/// The port `console=` on the command line insists on, if it named one.
///
/// # Why an override exists at all
///
/// Everything [`init`] does otherwise is inference from the machine's own
/// description, and on a board being brought up for the first time that
/// description is exactly what is in question. If `stdout-path` points
/// somewhere unexpected, or a board wires its connector to a UART the tree
/// leaves disabled, the symptom is a kernel that comes up perfectly and says
/// nothing — the one failure that cannot be diagnosed from the console,
/// because it *is* the console. `console=stm32` in `/chosen/bootargs`, which
/// U-Boot sets without reflashing anything, turns that into a boot that talks.
///
/// The address still comes from the tree: this chooses between the ports the
/// machine describes, it does not invent one. A name matching no node this
/// kernel can drive falls through to the ordinary search rather than failing,
/// so a stale argument in a saved U-Boot environment cannot take the console
/// away.
fn forced<'a>(tree: &Fdt<'a>) -> Option<(Node<'a>, Port)> {
    let wanted = Port::named(option_in(tree.bootargs()?, "console")?)?;
    tree.nodes()
        .find(|node| Port::of(node) == Some(wanted) && node.reg().next().is_some())
        .map(|node| (node, wanted))
}

/// The first port in the tree this kernel can drive and the machine has not
/// turned off.
///
/// A node with no `status` is enabled: the property's absence is the
/// specification's `okay`, and most trees leave it out.
fn first_enabled<'a>(tree: &Fdt<'a>) -> Option<(Node<'a>, Port)> {
    tree.nodes().find_map(|node| {
        let off = node
            .property("status")
            .and_then(|status| status.as_str())
            .is_some_and(|status| status != "okay" && status != "ok");
        if off {
            return None;
        }
        Port::of(&node).map(|port| (node, port))
    })
}

/// Send one byte to whichever port the machine turned out to have.
///
/// Nothing at all before [`init`] has run, which is the same silence both
/// drivers keep before their window is mapped.
pub(crate) fn write_byte(byte: u8) {
    match port() {
        Some(Port::Pl011) => pl011::write_byte(byte),
        Some(Port::Stm32) => stm32_usart::write_byte(byte),
        None => {}
    }
}

/// Wait until whichever port the machine turned out to have has sent
/// everything written to it.
pub(crate) fn drain() {
    match port() {
        Some(Port::Pl011) => pl011::drain(),
        Some(Port::Stm32) => stm32_usart::drain(),
        None => {}
    }
}

/// One received byte from whichever port the machine turned out to have, if
/// one is waiting.
///
/// `None` before [`init`] has run, the receiving side of the silence
/// [`write_byte`] keeps then.
pub(crate) fn read_byte() -> Option<u8> {
    match port() {
        Some(Port::Pl011) => pl011::read_byte(),
        Some(Port::Stm32) => stm32_usart::read_byte(),
        None => None,
    }
}
