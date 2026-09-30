//! A device's registers, mapped.

use core::ptr;

use ferrix_blkring::control::Block as StartBlock;
use ferrix_native_abi::types::IoMappingSpec;
use ferrix_rt::Kernel;
use ferrix_rt::native::device::{Device, IoMapping};

use crate::Step;

/// A page, on every architecture a driver runs on.
const PAGE: usize = 4096;

/// One of a device's register blocks, mapped into this process.
pub struct Block {
    _mapping: IoMapping<Kernel>,
    base: usize,
    len: usize,
}

impl core::fmt::Debug for Block {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Block")
            .field("base", &self.base)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl Block {
    /// Map `block` of `device`'s registers.
    ///
    /// # Errors
    ///
    /// [`Step::Registers`] if the kernel will not map it.
    pub fn map(device: &Device<Kernel>, block: &StartBlock) -> Result<Block, Step> {
        let offset = block.offset as usize;
        let len = block.length as usize;
        let end = offset.checked_add(len).ok_or(Step::Registers)?;
        let pages = end.div_ceil(PAGE).max(1) * PAGE;
        let mapping = device
            .io_mapping(IoMappingSpec {
                phys: block.phys,
                len: pages as u64,
            })
            .map_err(|_| Step::Registers)?;
        let base = mapping.map(None).map_err(|_| Step::Registers)?;
        Ok(Block {
            _mapping: mapping,
            base: base + offset,
            len,
        })
    }

    /// The block's length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the block is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read the register at `offset`.
    ///
    /// # Panics
    ///
    /// If the register is not inside the block and aligned for `T`.
    #[must_use]
    pub fn read<T: Copy>(&self, offset: u32) -> T {
        let offset = offset as usize;
        assert!(
            offset + size_of::<T>() <= self.len && offset.is_multiple_of(size_of::<T>()),
            "a register inside the block, aligned"
        );
        // SAFETY: mapped device memory the kernel gave this process, the
        // offset inside it and aligned for `T`.
        unsafe { device_read((self.base + offset) as *const T) }
    }

    /// Write the register at `offset`.
    ///
    /// # Panics
    ///
    /// If the register is not inside the block and aligned for `T`.
    pub fn write<T: Copy>(&mut self, offset: u32, value: T) {
        let offset = offset as usize;
        assert!(
            offset + size_of::<T>() <= self.len && offset.is_multiple_of(size_of::<T>()),
            "a register inside the block, aligned"
        );
        // SAFETY: as for `read`, and the mapping is writable.
        unsafe { device_write((self.base + offset) as *mut T, value) }
    }
}

/// Read a register at `at`, as one plain load.
///
/// Never inlined, so the address arrives in a register and the load is
/// `ldr` from it and nothing else. Inlined into a loop -- a config answer
/// read a byte at a time -- the compiler folds the address step into the
/// load as a post-index writeback, which a device model sees as a data
/// abort with no syndrome to emulate: KVM gives up with `ENOSYS`, and
/// crosvm stops the virtual processor (the Pixel 7's VM, 2026-09-26).
/// QEMU's emulator decodes the instruction itself, so nothing else showed
/// it.
///
/// # Safety
///
/// `at` is mapped device memory, aligned for `T`.
#[inline(never)]
unsafe fn device_read<T: Copy>(at: *const T) -> T {
    // SAFETY: as the caller promises.
    unsafe { ptr::read_volatile(at) }
}

/// Write a register at `at`, as one plain store: see [`device_read`].
///
/// # Safety
///
/// `at` is mapped, writable device memory, aligned for `T`.
#[inline(never)]
unsafe fn device_write<T: Copy>(at: *mut T, value: T) {
    // SAFETY: as the caller promises.
    unsafe { ptr::write_volatile(at, value) }
}
