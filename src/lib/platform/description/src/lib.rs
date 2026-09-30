//! Which of its two descriptions a machine is read by.
//!
//! A machine describes itself with ACPI tables, with a device tree, or -- on
//! AArch64 under EDK2 -- with both. Where both are there, the ACPI tables are
//! authoritative: they are what firmware keeps current, and a tree beside
//! them may be one a loader carried for another purpose. The kernel asks this
//! before it looks for devices, IOMMUs or PCI hosts, and [`choose`] is the one
//! place the answer is given.
//!
//! The decision is generic over what an opened description is, so that it can
//! be tested here without firmware: the kernel instantiates it with its own
//! `acpi::Firmware` and `ferrix_fdt::Fdt`.

#![no_std]
#![forbid(unsafe_code)]

/// How a machine describes itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Description<A, T> {
    /// Readable ACPI tables, whether or not a device tree came too.
    Acpi(A),
    /// A device tree, and no readable ACPI tables.
    Tree(T),
    /// Neither: nothing to find devices, IOMMUs or PCI hosts in.
    Neither,
}

/// The description a machine is read by: its ACPI tables if they opened,
/// else its device tree if it parses, else neither.
///
/// `tree` is only called when the ACPI tables did not open, so a machine with
/// both never has its tree parsed for this question.
pub fn choose<A, AcpiError, T, TreeError>(
    acpi: Result<A, AcpiError>,
    tree: impl FnOnce() -> Result<T, TreeError>,
) -> Description<A, T> {
    match acpi {
        Ok(tables) => Description::Acpi(tables),
        Err(_) => match tree() {
            Ok(tree) => Description::Tree(tree),
            Err(_) => Description::Neither,
        },
    }
}

#[cfg(test)]
mod tests;
