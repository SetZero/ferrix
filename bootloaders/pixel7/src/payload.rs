//! The kernel and initramfs this loader carries.
//!
//! ABL loads one image and nothing else, so what `boot/` reads off a FAT
//! volume is built into the loader instead; `build.rs` says from where.

/// A byte array aligned for the ELF parser, which reads headers in place.
#[repr(C, align(16))]
struct Aligned<T: ?Sized>(T);

#[cfg(payload)]
static KERNEL_BYTES: &Aligned<[u8]> = &Aligned(*include_bytes!(env!("PIXEL7_KERNEL")));
#[cfg(payload)]
static INITRD_BYTES: &Aligned<[u8]> = &Aligned(*include_bytes!(env!("PIXEL7_INITRD")));

#[cfg(not(payload))]
static KERNEL_BYTES: &Aligned<[u8]> = &Aligned([]);
#[cfg(not(payload))]
static INITRD_BYTES: &Aligned<[u8]> = &Aligned([]);

/// The kernel, as an ELF image; empty in a loader built without a payload.
pub(crate) fn kernel() -> &'static [u8] {
    &KERNEL_BYTES.0
}

/// The initramfs, a `newc` cpio archive; empty in a loader built without one.
pub(crate) fn initrd() -> &'static [u8] {
    &INITRD_BYTES.0
}
