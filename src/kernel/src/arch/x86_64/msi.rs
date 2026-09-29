//! Message-signalled interrupts on x86-64: a local APIC vector per message.
//!
//! An MSI on a PC is a write to the local APIC's fixed address range, carrying
//! a vector in its data. There is no controller line to program and nothing to
//! enable: the vector is taken from a range nothing else uses, and a device
//! that writes the message raises it. Interrupt remapping would sit between
//! the two; it is part of the IOMMU work and not here yet.

use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_pci::msix::local_apic_message;

use super::{apic, trap};
use crate::irq::Msi;

/// The first vector handed out. Sixty-four vectors from here stay below the
/// legacy system call gate at 0x80 and far below the kernel's own at 0xFD to
/// 0xFF.
const FIRST_VECTOR: u64 = 0x40;

/// Which of the sixty-four are allocated, one bit each.
static TAKEN: AtomicU64 = AtomicU64::new(0);

/// Take a vector and say what a device writes to raise it.
///
/// The message is aimed at the local APIC of the processor asking. Any
/// processor can service the interrupt, so which one it lands on is a
/// question of balance, not correctness. Which device writes it, `_device`,
/// makes no difference: without interrupt remapping the vector is in the
/// data.
///
/// # Errors
///
/// Every vector already taken, or a local APIC identifier too wide for a
/// message's eight-bit destination.
pub(crate) fn msi_allocate(_device: u32) -> Result<Msi, &'static str> {
    let destination =
        u8::try_from(apic::id()).map_err(|_| "this local APIC's identifier is too wide for MSI")?;
    let vector = allocate_vector().ok_or("every MSI vector is allocated")?;
    let message = local_apic_message(destination, vector as u8);
    Ok(Msi {
        number: (vector - trap::IRQ_BASE) as u32,
        address: message.address,
        data: message.data,
    })
}

/// Take a device vector from the same sixty-four a message uses, for an
/// interrupt that arrives some other way: an I/O APIC input.
pub(crate) fn allocate_vector() -> Option<u64> {
    let index = loop {
        let taken = TAKEN.load(Ordering::Relaxed);
        let free = taken.trailing_ones();
        if free >= u64::BITS {
            return None;
        }
        if TAKEN
            .compare_exchange(
                taken,
                taken | 1 << free,
                Ordering::SeqCst,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            break u64::from(free);
        }
    };
    Some(FIRST_VECTOR + index)
}
