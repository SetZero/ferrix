//! Finding the Generic Interrupt Controller on `AArch64`.
//!
//! The driver is `crate::arch::gicv2`, shared with ARMv7-A: the register
//! interface is the same on both machines, and what differs is how the kernel
//! learns where the registers are. Here that is the MADT, and this module is
//! the MADT walk and nothing else.
//!
//! GICv3 replaces the CPU interface with system registers and adds a
//! redistributor per core. It is not here yet: `init` reports the version it
//! found and refuses one it cannot drive, rather than writing GICv2 layouts
//! into GICv3 registers and producing a machine that takes no interrupts for
//! reasons nothing explains.

use ferrix_acpi::{Acpi, MadtEntry};

use crate::acpi::DirectMap;
use crate::arch::gicv2;

/// Where the two register blocks are, according to firmware.
struct Layout {
    /// Physical address of the distributor.
    distributor: u64,
    /// Physical address of the CPU interface, which every core shares.
    cpu_interface: u64,
    /// The architecture version firmware reports, or zero for "probe it".
    version: u8,
}

/// Read the distributor and CPU interface addresses out of the MADT.
fn layout(acpi: &Acpi<'_, DirectMap>) -> Result<Layout, &'static str> {
    let madt = acpi.madt().map_err(|_| "the machine has no MADT")?;

    let mut distributor = 0;
    let mut cpu_interface = 0;
    let mut version = 0;
    let mut banked = true;

    for entry in madt.entries() {
        match entry {
            MadtEntry::Gicd(gicd) => {
                distributor = gicd.physical_base_address;
                version = gicd.gic_version;
            }
            // A GICv2 CPU interface is banked: every core reaches its own
            // through the same address, which is why one mapping serves them
            // all. Firmware lists it once per core, and every copy is
            // required to agree.
            MadtEntry::Gicc(gicc) if gicc.is_enabled() => {
                if cpu_interface == 0 {
                    cpu_interface = gicc.physical_base_address;
                } else if gicc.physical_base_address != cpu_interface {
                    banked = false;
                }
            }
            _ => {}
        }
    }

    if distributor == 0 {
        return Err("the MADT describes no GIC distributor");
    }
    if !banked {
        return Err("the cores' GIC CPU interfaces are at different addresses, not banked");
    }
    Ok(Layout {
        distributor,
        cpu_interface,
        version,
    })
}

/// Find the controller in the MADT and bring it up, returning its version.
///
/// # Safety
///
/// `gicv2::init`'s contract: once, on the boot CPU, after the vector table is
/// installed and while interrupts are masked.
pub(crate) unsafe fn init(acpi: &Acpi<'_, DirectMap>) -> Result<u8, &'static str> {
    let layout = layout(acpi)?;

    // Version 0 means firmware declined to say and the OS should probe. On a
    // machine that also gave a CPU interface address, that is a GICv2 layout:
    // GICv3 has no CPU interface to give an address for.
    let version = match layout.version {
        0 if layout.cpu_interface != 0 => 2,
        other => other,
    };
    if version != 2 {
        return Err("this GIC is not a GICv2, and GICv3 support is not written yet");
    }
    if layout.cpu_interface == 0 {
        return Err("the MADT describes a GICv2 with no CPU interface");
    }

    // SAFETY: the caller's contract is `gicv2::init`'s, and these are the two
    // register blocks firmware describes, checked above to be a GICv2's.
    unsafe { gicv2::init(layout.distributor, layout.cpu_interface)? };
    Ok(version)
}
