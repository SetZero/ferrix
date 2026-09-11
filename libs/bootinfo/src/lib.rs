//! The hand-off ABI between the UEFI loader and the kernel.
//!
//! These are two separate programs, linked for different targets, that meet at
//! exactly one struct. Both compile it from this one definition, every type is
//! `repr(C)`, and the kernel refuses to start if the magic or version disagree
//! — because the failure mode of a silent layout mismatch is a triple fault
//! with nothing on the serial port to say why.
//!
//! # One layout per word width
//!
//! Where things live in the virtual address space is a [`Layout`], and there
//! are two: [`LAYOUT_64`], which x86-64 and AArch64 share constant for
//! constant, and [`LAYOUT_32`] for ARMv7-A, which cannot hold a single one of
//! them. The crate root re-exports the one matching the target's pointer width
//! under the names the rest of the tree uses, so generic code says
//! [`PHYSMAP_BASE`] and never a width. Both layouts are checked at compile time
//! on every build, and the host tests read both — which is how the 32-bit one
//! is tested by a machine that is not 32-bit.
//!
//! [`BootInfo`] itself has *one* layout on every width. Its addresses are
//! `u64` even where a pointer would be four bytes, so the structure the host
//! tests build is the structure a 32-bit kernel reads.
//!
//! # Reading it safely
//!
//! [`BootInfo`] is a raw structure full of addresses the kernel did not
//! create. Rather than sprinkle `unsafe` over every reader, the kernel
//! validates it once through [`BootInfo::validate`] and works with the
//! resulting [`BootView`], whose accessors are safe.
//!
//! ```
//! # use ferrix_bootinfo::{BootInfo, BootInfoError};
//! # fn example(raw: *const BootInfo) -> Result<(), BootInfoError> {
//! // SAFETY: in the kernel this pointer is the one argument the loader passed.
//! let view = unsafe { (*raw).validate() }?;
//! for region in view.regions() {
//!     let _ = region.kind;
//! }
//! # Ok(())
//! # }
//! ```

#![no_std]

use core::fmt;

/// Magic number identifying a [`BootInfo`], ASCII `FERRIXBI`.
///
/// Checked by the kernel before it touches anything else in the structure.
pub const BOOTINFO_MAGIC: u64 = 0x4645_5252_4958_4249;

/// Layout version of [`BootInfo`], bumped on any change to this file.
///
/// Version 2 added the physical origin of the direct map and the length of the
/// device tree, and turned the two pointers into addresses so the structure
/// has the same layout on every word width.
pub const BOOTINFO_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Virtual memory layout
// ---------------------------------------------------------------------------

/// Where everything lives in the virtual address space, for one word width.
///
/// A user half at the bottom, and three kernel regions above it: the direct map
/// of physical RAM, the dynamic mapping area, and the kernel image. The 64-bit
/// layout puts them in that order; the 32-bit one puts the vmap area first,
/// because 4 GiB is small enough that which region gets the address space left
/// over from the others is a decision rather than a detail — and it is the
/// direct map, which is what decides how much RAM a 32-bit kernel can use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Width of a virtual address.
    pub address_bits: u32,
    /// Highest user virtual address, exclusive.
    pub user_end: u64,
    /// Lowest address belonging to the kernel.
    pub kernel_half: u64,
    /// Base of the direct map of physical RAM.
    pub physmap_base: u64,
    /// End of the direct map, exclusive.
    pub physmap_end: u64,
    /// Base of the dynamic mapping area: `MMIO` windows, guard-paged kernel
    /// stacks, anything whose virtual address is chosen at runtime.
    pub vmap_base: u64,
    /// Size of the dynamic mapping area, in bytes.
    pub vmap_size: u64,
    /// Bytes at the bottom of the dynamic mapping area kept out of the
    /// allocator for the fixed windows early boot places there before there is
    /// an allocator: the console, a framebuffer, stage 3's on-demand window.
    pub vmap_reserved: u64,
    /// Where the kernel image is linked. Everything from here to the top of
    /// the address space is the image's.
    pub kernel_base: u64,
}

impl Layout {
    /// One past the last address of the dynamic mapping area.
    #[must_use]
    pub const fn vmap_end(&self) -> u64 {
        self.vmap_base + self.vmap_size
    }

    /// Bytes of physical memory the direct map can cover.
    #[must_use]
    pub const fn physmap_size(&self) -> u64 {
        self.physmap_end - self.physmap_base
    }

    /// The first rule this layout breaks, or `None` if it is consistent.
    ///
    /// A description rather than a boolean, so the test that runs this on both
    /// layouts can say which rule failed.
    #[must_use]
    pub const fn violation(&self) -> Option<&'static str> {
        let mask = PAGE_SIZE - 1;
        if (self.user_end | self.kernel_half | self.physmap_base | self.physmap_end) & mask != 0
            || (self.vmap_base | self.vmap_size | self.vmap_reserved | self.kernel_base) & mask != 0
        {
            return Some("every boundary must be page aligned");
        }
        if self.user_end > self.kernel_half {
            return Some("the user half must end before the kernel half begins");
        }
        if self.physmap_base >= self.physmap_end {
            return Some("the direct map must span a positive range");
        }
        if self.physmap_base < self.kernel_half || self.vmap_base < self.kernel_half {
            return Some("the direct map and the vmap area are the kernel's");
        }
        if self.vmap_reserved >= self.vmap_size {
            return Some("the vmap area must have room beyond its reserved windows");
        }
        if !(self.physmap_end <= self.vmap_base || self.vmap_end() <= self.physmap_base) {
            return Some("the direct map must not overlap the vmap area");
        }
        if self.physmap_end > self.kernel_base || self.vmap_end() > self.kernel_base {
            return Some("the kernel image must be above everything else");
        }
        if self.address_bits < 64 && self.kernel_base >= 1 << self.address_bits {
            return Some("the kernel image must be inside the address space");
        }
        None
    }
}

/// The layout x86-64 and AArch64 share.
///
/// The image is in the top -2 GiB, which is what the x86-64 "kernel" code
/// model addresses. AArch64 does not require that and uses it anyway, because
/// one layout across both is one set of bugs instead of two.
pub const LAYOUT_64: Layout = Layout {
    address_bits: 64,
    user_end: 0x0000_8000_0000_0000,
    kernel_half: 0xFFFF_8000_0000_0000,
    physmap_base: 0xFFFF_8000_0000_0000,
    physmap_end: 0xFFFF_FF00_0000_0000,
    vmap_base: 0xFFFF_FF00_0000_0000,
    vmap_size: 0x0000_00EF_0000_0000,
    vmap_reserved: 0x1_0000_0000,
    kernel_base: 0xFFFF_FFFF_8000_0000,
};

/// The layout of ARMv7-A: a 2/2 split of 4 GiB.
///
/// Two gibibytes of user space, because a 32-bit process wants the larger
/// half; then 512 MiB of vmap, 1.25 GiB of direct map — the ceiling on RAM
/// this kernel can use, and more than the boards it targets carry — and
/// 256 MiB for the image at the top.
pub const LAYOUT_32: Layout = Layout {
    address_bits: 32,
    user_end: 0x8000_0000,
    kernel_half: 0x8000_0000,
    vmap_base: 0x8000_0000,
    vmap_size: 0x2000_0000,
    vmap_reserved: 0x0400_0000,
    physmap_base: 0xA000_0000,
    physmap_end: 0xF000_0000,
    kernel_base: 0xF000_0000,
};

// Compile-time rather than tests on purpose: an overlap between the direct map
// and the vmap area is not something to discover from a failing test run, and
// a `const` assertion fails in the build that introduced it. Both layouts are
// checked on every build, whichever one the build uses.
const _: () = assert!(
    LAYOUT_64.violation().is_none(),
    "LAYOUT_64 is inconsistent; the layout tests name the rule"
);
const _: () = assert!(
    LAYOUT_32.violation().is_none(),
    "LAYOUT_32 is inconsistent; the layout tests name the rule"
);

/// The layout this build uses.
#[cfg(target_pointer_width = "64")]
pub const LAYOUT: Layout = LAYOUT_64;
/// The layout this build uses.
#[cfg(target_pointer_width = "32")]
pub const LAYOUT: Layout = LAYOUT_32;

/// Base of the direct map of all physical RAM.
///
/// Physical address `p` is readable at `PHYSMAP_BASE + (p - physmap_phys)`
/// once the loader's page tables are live, and stays that way for the life of
/// the kernel. See [`BootInfo::physmap_phys`].
pub const PHYSMAP_BASE: u64 = LAYOUT.physmap_base;

/// End of the direct map, exclusive.
pub const PHYSMAP_END: u64 = LAYOUT.physmap_end;

/// Base of the kernel's dynamic virtual allocation area.
pub const KERNEL_VMAP_BASE: u64 = LAYOUT.vmap_base;

/// Size of the kernel's dynamic virtual allocation area, in bytes.
pub const KERNEL_VMAP_SIZE: u64 = LAYOUT.vmap_size;

/// Bytes at the bottom of the vmap area reserved for early boot's fixed
/// windows. See [`Layout::vmap_reserved`].
pub const KERNEL_VMAP_RESERVED: u64 = LAYOUT.vmap_reserved;

/// Where the kernel image is linked. The linker script is told the same
/// number by `.cargo/config.toml`, and the loader refuses a kernel linked
/// anywhere else.
pub const KERNEL_VIRT_BASE: u64 = LAYOUT.kernel_base;

/// The lowest kernel address.
pub const KERNEL_HALF_BASE: u64 = LAYOUT.kernel_half;

/// Highest user virtual address, exclusive.
pub const USER_VIRT_END: u64 = LAYOUT.user_end;

/// The page size every architecture is configured for.
pub const PAGE_SIZE: u64 = 4096;

/// The alignment of the direct map's physical origin.
///
/// Two mebibytes, so the loader can map it with blocks rather than pages on
/// every architecture: a direct map whose first byte is halfway through a
/// block is 512 page descriptors where there should have been one.
pub const PHYSMAP_ALIGN: u64 = 2 * 1024 * 1024;

/// The stack the loader hands the kernel, in bytes. Replaced by a guard-paged
/// per-CPU stack as soon as the kernel can allocate one.
pub const BOOT_STACK_SIZE: u64 = 64 * 1024;

/// True if `addr` is in the part of the address space reserved for the kernel.
#[must_use]
pub const fn is_kernel_address(addr: u64) -> bool {
    addr >= KERNEL_HALF_BASE
}

/// True if `addr` is a valid user virtual address.
#[must_use]
pub const fn is_user_address(addr: u64) -> bool {
    addr < USER_VIRT_END
}

/// The physical origin a direct map should have, given the lowest physical
/// address of RAM: that address, rounded down to [`PHYSMAP_ALIGN`].
///
/// Zero on x86-64, where RAM starts at zero. A gibibyte on QEMU's Arm `virt`
/// machines, whose first gibibyte is flash and device registers — which a
/// direct map from zero would have covered, as cacheable memory, at the cost
/// of a quarter of a 32-bit kernel's half.
#[must_use]
pub const fn physmap_origin(lowest_ram: u64) -> u64 {
    lowest_ram & !(PHYSMAP_ALIGN - 1)
}

/// The direct-map address of `phys`, for a direct map whose physical origin
/// is `physmap_phys`.
///
/// `phys` must not be below the origin; nothing below it is mapped, and the
/// subtraction says so by overflowing rather than by wrapping into an address
/// that happens to be mapped.
#[must_use]
pub const fn direct_map_address(physmap_phys: u64, phys: u64) -> u64 {
    PHYSMAP_BASE + (phys - physmap_phys)
}

// ---------------------------------------------------------------------------
// Machine description
// ---------------------------------------------------------------------------

/// The CPU architecture the loader booted on.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    /// 64-bit x86.
    X86_64 = 0,
    /// 64-bit Arm.
    AArch64 = 1,
    /// 32-bit Arm, ARMv7-A with the Large Physical Address Extension.
    Armv7a = 2,
}

impl Arch {
    /// The `e_machine` value an ELF image for this architecture carries.
    #[must_use]
    pub const fn elf_machine(self) -> u16 {
        match self {
            Arch::X86_64 => 62,
            Arch::AArch64 => 183,
            Arch::Armv7a => 40,
        }
    }

    /// The short name used in paths, log lines and target triples.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
            Arch::Armv7a => "armv7a",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a region of physical address space is being used for.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemKind {
    /// Free for the kernel's frame allocator.
    Usable = 0,
    /// Firmware or hardware reserved; never hand out.
    Reserved = 1,
    /// ACPI tables, reclaimable once the kernel has parsed them.
    AcpiReclaim = 2,
    /// ACPI non-volatile storage, which must be preserved.
    AcpiNvs = 3,
    /// The loader's own code and data, reclaimable after early boot.
    Loader = 4,
    /// The loaded kernel image.
    Kernel = 5,
    /// The initial ramdisk.
    Initrd = 6,
    /// Page tables built by the loader; still live, so never reuse them.
    PageTables = 7,
    /// The stack the kernel is entered on.
    BootStack = 8,
    /// The boot info structure and the memory map array inside it.
    BootInfo = 12,
    /// The loader's copy of the flattened device tree, kept for the life of
    /// the system: stage 10 enumerates devices from it long after boot.
    DeviceTree = 13,
    /// A linear framebuffer.
    Framebuffer = 9,
    /// A device `MMIO` aperture.
    Mmio = 10,
    /// Memory the firmware reported as bad.
    Defective = 11,
}

impl MemKind {
    /// Regions the frame allocator may claim immediately at boot.
    #[must_use]
    pub const fn is_free_at_boot(self) -> bool {
        matches!(self, MemKind::Usable)
    }

    /// Regions that become free once early boot has finished with them.
    #[must_use]
    pub const fn is_reclaimable(self) -> bool {
        matches!(self, MemKind::Loader | MemKind::AcpiReclaim)
    }

    /// Regions that are real RAM, whether or not they are free.
    ///
    /// This is what the direct map has to cover and what "total memory"
    /// counts, so it is deliberately conservative. `AcpiReclaim` and `AcpiNvs`
    /// are included — the tables the kernel has to read live there. `Reserved`
    /// is **not**, and that is the important half: UEFI reports firmware- and
    /// hardware-reserved apertures under that type, and on QEMU's q35 one of
    /// them sits at 1 TiB. Counting it as RAM makes the direct map a thousand
    /// times larger than the machine, which is not a subtle failure but is a
    /// confusing one — it appears as the page table pool running out.
    #[must_use]
    pub const fn is_ram(self) -> bool {
        !matches!(self, MemKind::Reserved | MemKind::Mmio | MemKind::Defective)
    }
}

/// One entry of the physical memory map.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemRegion {
    /// First physical address in the region, page aligned.
    pub base: u64,
    /// Length in bytes, a multiple of [`PAGE_SIZE`].
    pub len: u64,
    /// What the region is for.
    pub kind: MemKind,
    /// Padding, so the struct has the same layout under every ABI. Zero.
    pub reserved: u32,
}

impl MemRegion {
    /// One past the last physical address in the region.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.base.wrapping_add(self.len)
    }

    /// Number of [`PAGE_SIZE`] frames the region covers.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.len / PAGE_SIZE
    }

    /// True if `addr` falls inside the region.
    #[must_use]
    pub const fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.end()
    }
}

/// Pixel layout of a linear framebuffer.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// Blue, green, red, unused — the usual UEFI graphics output format.
    Bgrx8888 = 0,
    /// Red, green, blue, unused.
    Rgbx8888 = 1,
    /// Something else, or no framebuffer.
    Unknown = 2,
}

/// A linear framebuffer the firmware left us.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Framebuffer {
    /// Physical address of the first pixel, or 0 if there is no framebuffer.
    pub phys: u64,
    /// Size of the framebuffer in bytes.
    pub size: u64,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Pixels — not bytes — from the start of one scanline to the next.
    pub stride: u32,
    /// Pixel layout.
    pub format: PixelFormat,
}

impl Framebuffer {
    /// The value used when firmware offered no graphics output.
    pub const NONE: Framebuffer = Framebuffer {
        phys: 0,
        size: 0,
        width: 0,
        height: 0,
        stride: 0,
        format: PixelFormat::Unknown,
    };

    /// True if there is a framebuffer to draw on.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.phys != 0 && self.width != 0 && self.height != 0
    }
}

// ---------------------------------------------------------------------------
// The hand-off structure
// ---------------------------------------------------------------------------

/// Everything the kernel learns from firmware, assembled by the loader.
///
/// Fields named for an address without a `_phys` suffix — `regions`,
/// `cmdline`, the boot stack — are *virtual* addresses valid in the page
/// tables the loader installed before jumping to the kernel, which is to say
/// inside the direct map at [`PHYSMAP_BASE`]. Fields named `_phys` are
/// physical. None of them is a pointer type, so the structure is the same
/// 208 bytes on every word width.
///
/// Read it through [`BootInfo::validate`] rather than field by field.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    /// Must equal [`BOOTINFO_MAGIC`].
    pub magic: u64,
    /// Must equal [`BOOTINFO_VERSION`].
    pub version: u32,
    /// The architecture the loader ran on.
    pub arch: Arch,

    /// Virtual address of `regions_len` [`MemRegion`]s, sorted by base and
    /// non-overlapping.
    pub regions: u64,
    /// Number of entries `regions` holds.
    pub regions_len: u64,

    /// Base of the direct physical map. Mirrors [`PHYSMAP_BASE`], carried
    /// explicitly so the kernel never has to assume.
    pub physmap_base: u64,
    /// The physical address that appears at `physmap_base`: the lowest RAM
    /// address, rounded down to [`PHYSMAP_ALIGN`]. Nothing below it is in the
    /// direct map.
    pub physmap_phys: u64,
    /// Bytes of physical address space the direct map covers, from
    /// `physmap_phys` up.
    pub physmap_len: u64,

    /// Physical address the kernel image was loaded at.
    pub kernel_phys: u64,
    /// Virtual address the kernel image is mapped at.
    pub kernel_virt: u64,
    /// Size of the kernel image in bytes, page rounded.
    pub kernel_len: u64,

    /// Physical address of the root page table for the kernel half: the
    /// x86-64 PML4, the AArch64 `TTBR1_EL1` table, or the ARMv7-A level-1
    /// table whose last two entries `TTBR1` translates through.
    pub root_table_phys: u64,
    /// Physical address of the identity-mapping `TTBR0` table on the two Arm
    /// architectures. Zero on x86-64, where one table covers both halves.
    pub ttbr0_phys: u64,

    /// Top of the stack the kernel is entered on, 16-byte aligned.
    pub boot_stack_top: u64,
    /// Size of that stack in bytes.
    pub boot_stack_size: u64,

    /// The framebuffer, or [`Framebuffer::NONE`].
    pub framebuffer: Framebuffer,

    /// Physical address of the initial ramdisk, or 0 if there is none.
    pub initrd_phys: u64,
    /// Length of the initial ramdisk in bytes.
    pub initrd_len: u64,

    /// Physical address of the ACPI RSDP, or 0 if firmware offered none.
    pub rsdp: u64,
    /// Physical address of the loader's copy of the flattened device tree, or
    /// 0. A copy, in memory the map reports as [`MemKind::DeviceTree`], because
    /// firmware's own tends to live somewhere the kernel would reclaim.
    pub dtb: u64,
    /// Length of that copy in bytes.
    pub dtb_len: u64,
    /// Physical address of the UEFI system table. Boot services are gone by
    /// the time the kernel sees this; runtime services are still callable.
    pub uefi_system_table: u64,

    /// Virtual address of `cmdline_len` bytes of UTF-8 kernel command line,
    /// or 0.
    pub cmdline: u64,
    /// Length of the command line in bytes.
    pub cmdline_len: u64,
}

// The claim the module documentation makes, asserted where it can fail: a
// field whose size follows the pointer width would break it, and the loader
// and the host tests would then disagree about a layout neither of them sees.
const _: () = assert!(
    size_of::<BootInfo>() == 208,
    "BootInfo must be laid out identically on every word width"
);
const _: () = assert!(
    align_of::<BootInfo>() == 8,
    "BootInfo must be eight-byte aligned on every ABI"
);
const _: () = assert!(
    size_of::<MemRegion>() == 24,
    "MemRegion must be laid out identically on every word width"
);

/// Why a [`BootInfo`] was rejected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BootInfoError {
    /// The magic number was wrong: this is not a `BootInfo` at all.
    BadMagic,
    /// The loader and the kernel were built from different versions of this
    /// file. Carries the version the loader wrote.
    VersionMismatch(u32),
    /// The memory map address was zero, or its length zero.
    NoMemoryMap,
    /// The direct map does not start where the kernel was built to expect.
    PhysmapMismatch,
    /// The direct map's physical origin is not a multiple of
    /// [`PHYSMAP_ALIGN`].
    PhysmapMisaligned,
    /// The direct map claims to cover more than its region of the address
    /// space can hold.
    PhysmapTooLarge,
    /// The command line was not valid UTF-8.
    CmdlineNotUtf8,
}

impl fmt::Display for BootInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootInfoError::BadMagic => f.write_str("boot info magic is wrong"),
            BootInfoError::VersionMismatch(found) => {
                write!(
                    f,
                    "boot info version {found}, kernel wants {BOOTINFO_VERSION}"
                )
            }
            BootInfoError::NoMemoryMap => f.write_str("boot info carries no memory map"),
            BootInfoError::PhysmapMismatch => {
                f.write_str("direct map is not where the kernel expects")
            }
            BootInfoError::PhysmapMisaligned => {
                f.write_str("direct map's physical origin is not 2 MiB aligned")
            }
            BootInfoError::PhysmapTooLarge => {
                f.write_str("direct map is larger than its region of the address space")
            }
            BootInfoError::CmdlineNotUtf8 => f.write_str("kernel command line is not UTF-8"),
        }
    }
}

impl BootInfo {
    /// Check the hand-off and produce a view whose accessors are safe.
    ///
    /// This is the only place the addresses in it are dereferenced, so it is
    /// the only place that has to be reasoned about.
    ///
    /// # Safety
    ///
    /// `self.regions` must be the address of `self.regions_len` initialised
    /// [`MemRegion`]s, and `self.cmdline` of `self.cmdline_len` initialised
    /// bytes, both mapped and immutable for as long as the returned
    /// [`BootView`] lives. The loader guarantees this by construction; nothing
    /// else should call it.
    pub unsafe fn validate(&self) -> Result<BootView<'_>, BootInfoError> {
        if self.magic != BOOTINFO_MAGIC {
            return Err(BootInfoError::BadMagic);
        }
        if self.version != BOOTINFO_VERSION {
            return Err(BootInfoError::VersionMismatch(self.version));
        }
        if self.regions == 0 || self.regions_len == 0 {
            return Err(BootInfoError::NoMemoryMap);
        }
        if self.physmap_base != PHYSMAP_BASE {
            return Err(BootInfoError::PhysmapMismatch);
        }
        if !self.physmap_phys.is_multiple_of(PHYSMAP_ALIGN) {
            return Err(BootInfoError::PhysmapMisaligned);
        }
        if self.physmap_len > PHYSMAP_END - PHYSMAP_BASE {
            return Err(BootInfoError::PhysmapTooLarge);
        }

        // The addresses were exposed by whoever wrote them — the loader, or a
        // test — and are taken back with the provenance that exposure left.
        let regions = core::ptr::with_exposed_provenance::<MemRegion>(self.regions as usize);
        // SAFETY: the caller's contract is exactly that this address and length
        // describe an initialised, immutable array.
        let regions = unsafe { core::slice::from_raw_parts(regions, self.regions_len as usize) };

        let cmdline = if self.cmdline == 0 || self.cmdline_len == 0 {
            ""
        } else {
            let bytes = core::ptr::with_exposed_provenance::<u8>(self.cmdline as usize);
            // SAFETY: as above, for the command line bytes.
            let bytes = unsafe { core::slice::from_raw_parts(bytes, self.cmdline_len as usize) };
            core::str::from_utf8(bytes).map_err(|_| BootInfoError::CmdlineNotUtf8)?
        };

        Ok(BootView {
            info: self,
            regions,
            cmdline,
        })
    }
}

/// A validated [`BootInfo`]: same data, safe to read.
#[derive(Clone, Copy, Debug)]
pub struct BootView<'a> {
    info: &'a BootInfo,
    regions: &'a [MemRegion],
    cmdline: &'a str,
}

impl<'a> BootView<'a> {
    /// The raw hand-off structure, for the fields without an accessor.
    #[must_use]
    pub const fn raw(&self) -> &'a BootInfo {
        self.info
    }

    /// The physical memory map, sorted by base address.
    #[must_use]
    pub const fn regions(&self) -> &'a [MemRegion] {
        self.regions
    }

    /// The kernel command line, possibly empty.
    #[must_use]
    pub const fn cmdline(&self) -> &'a str {
        self.cmdline
    }

    /// The architecture the loader ran on.
    #[must_use]
    pub const fn arch(&self) -> Arch {
        self.info.arch
    }

    /// Total bytes of RAM the firmware reported, free or not.
    #[must_use]
    pub fn total_ram(&self) -> u64 {
        self.regions
            .iter()
            .filter(|region| region.kind.is_ram())
            .map(|region| region.len)
            .sum()
    }

    /// Bytes the frame allocator can claim at boot.
    #[must_use]
    pub fn usable_ram(&self) -> u64 {
        self.regions
            .iter()
            .filter(|region| region.kind.is_free_at_boot())
            .map(|region| region.len)
            .sum()
    }

    /// One past the highest physical address of real RAM.
    ///
    /// The direct map has to cover at least this much.
    #[must_use]
    pub fn max_ram_address(&self) -> u64 {
        self.regions
            .iter()
            .filter(|region| region.kind.is_ram())
            .map(MemRegion::end)
            .max()
            .unwrap_or(0)
    }

    /// One past the highest physical address the direct map covers.
    ///
    /// Below [`BootView::max_ram_address`] only on a 32-bit machine with more
    /// RAM than its direct map can hold, where the difference is memory the
    /// kernel cannot reach and does not pretend to.
    #[must_use]
    pub const fn physmap_limit(&self) -> u64 {
        self.info.physmap_phys + self.info.physmap_len
    }

    /// The region containing `phys`, if the map describes one.
    #[must_use]
    pub fn region_of(&self, phys: u64) -> Option<&'a MemRegion> {
        self.regions.iter().find(|region| region.contains(phys))
    }

    /// The initial ramdisk as a physical address and length, if present.
    #[must_use]
    pub const fn initrd(&self) -> Option<(u64, u64)> {
        if self.info.initrd_phys == 0 || self.info.initrd_len == 0 {
            None
        } else {
            Some((self.info.initrd_phys, self.info.initrd_len))
        }
    }

    /// The device tree as a physical address and length, if present.
    #[must_use]
    pub const fn device_tree(&self) -> Option<(u64, u64)> {
        if self.info.dtb == 0 || self.info.dtb_len == 0 {
            None
        } else {
            Some((self.info.dtb, self.info.dtb_len))
        }
    }

    /// Look up a `key=value` option on the kernel command line.
    ///
    /// Options are whitespace separated; the first match wins.
    #[must_use]
    pub fn option(&self, key: &str) -> Option<&'a str> {
        self.cmdline.split_whitespace().find_map(|word| {
            let (name, value) = word.split_once('=')?;
            (name == key).then_some(value)
        })
    }

    /// True if the command line carries `flag` as a bare word.
    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.cmdline.split_whitespace().any(|word| word == name)
    }
}

/// The signature the loader calls.
///
/// Declared here so that a mismatch between the two programs is a type error on
/// both sides rather than a triple fault with nothing on the serial port.
pub type KernelEntry = extern "C" fn(bootinfo: *const BootInfo) -> !;

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> [MemRegion; 4] {
        [
            MemRegion {
                base: 0,
                len: 0x1000,
                kind: MemKind::Reserved,
                reserved: 0,
            },
            MemRegion {
                base: 0x1000,
                len: 0x9F000,
                kind: MemKind::Usable,
                reserved: 0,
            },
            MemRegion {
                base: 0x10_0000,
                len: 0x3F00_0000,
                kind: MemKind::Usable,
                reserved: 0,
            },
            MemRegion {
                base: 0xFEC0_0000,
                len: 0x1000,
                kind: MemKind::Mmio,
                reserved: 0,
            },
        ]
    }

    fn boot_info(map: &[MemRegion], cmdline: &str) -> BootInfo {
        BootInfo {
            magic: BOOTINFO_MAGIC,
            version: BOOTINFO_VERSION,
            arch: Arch::X86_64,
            regions: map.as_ptr().expose_provenance() as u64,
            regions_len: map.len() as u64,
            physmap_base: PHYSMAP_BASE,
            physmap_phys: 0,
            physmap_len: 0x1_0000_0000,
            kernel_phys: 0x20_0000,
            kernel_virt: KERNEL_VIRT_BASE,
            kernel_len: 0x10_0000,
            root_table_phys: 0x1000,
            ttbr0_phys: 0,
            boot_stack_top: KERNEL_VIRT_BASE,
            boot_stack_size: BOOT_STACK_SIZE,
            framebuffer: Framebuffer::NONE,
            initrd_phys: 0,
            initrd_len: 0,
            rsdp: 0,
            dtb: 0,
            dtb_len: 0,
            uefi_system_table: 0,
            cmdline: cmdline.as_ptr().expose_provenance() as u64,
            cmdline_len: cmdline.len() as u64,
        }
    }

    #[test]
    fn validates_a_well_formed_handoff() {
        let map = regions();
        let info = boot_info(&map, "console=ttyS0 quiet");
        // SAFETY: `map` and the command line outlive the view, and the
        // structure was built by `boot_info` above, so its addresses
        // describe exactly what it says they do.
        let view = unsafe { info.validate() }.unwrap();

        assert_eq!(
            view.regions().len(),
            4,
            "every region should survive validation"
        );
        assert_eq!(view.arch(), Arch::X86_64);
        assert_eq!(view.cmdline(), "console=ttyS0 quiet");
        assert_eq!(view.device_tree(), None);
    }

    #[test]
    fn rejects_a_mismatched_version() {
        let map = regions();
        let mut info = boot_info(&map, "");
        info.version = BOOTINFO_VERSION + 1;
        assert_eq!(
            // SAFETY: `map` and the command line outlive the view, and the
            // structure was built by `boot_info` above, so its addresses
            // describe exactly what it says they do.
            unsafe { info.validate() }.unwrap_err(),
            BootInfoError::VersionMismatch(BOOTINFO_VERSION + 1),
            "a loader from another build must not be trusted"
        );
    }

    #[test]
    fn rejects_bad_magic_before_anything_else() {
        let map = regions();
        let mut info = boot_info(&map, "");
        info.magic = 0;
        info.regions = 0;
        assert_eq!(
            // SAFETY: `map` and the command line outlive the view, and the
            // structure was built by `boot_info` above, so its addresses
            // describe exactly what it says they do.
            unsafe { info.validate() }.unwrap_err(),
            BootInfoError::BadMagic,
            "magic is checked first so a wild address is never followed"
        );
    }

    #[test]
    fn rejects_a_direct_map_the_kernel_cannot_use() {
        let map = regions();

        let mut info = boot_info(&map, "");
        info.physmap_phys = 0x4000_1000;
        assert_eq!(
            // SAFETY: as in the tests above.
            unsafe { info.validate() }.unwrap_err(),
            BootInfoError::PhysmapMisaligned,
            "an origin inside a block would make the direct map pages, not blocks"
        );

        let mut info = boot_info(&map, "");
        info.physmap_len = PHYSMAP_END - PHYSMAP_BASE + PHYSMAP_ALIGN;
        assert_eq!(
            // SAFETY: as in the tests above.
            unsafe { info.validate() }.unwrap_err(),
            BootInfoError::PhysmapTooLarge,
            "a direct map larger than its region would run into the next one"
        );

        let mut info = boot_info(&map, "");
        info.physmap_base = KERNEL_VMAP_BASE;
        assert_eq!(
            // SAFETY: as in the tests above.
            unsafe { info.validate() }.unwrap_err(),
            BootInfoError::PhysmapMismatch
        );
    }

    #[test]
    fn sums_only_real_ram() {
        let map = regions();
        let info = boot_info(&map, "");
        // SAFETY: `map` and the command line outlive the view, and the
        // structure was built by `boot_info` above, so its addresses
        // describe exactly what it says they do.
        let view = unsafe { info.validate() }.unwrap();

        // Neither reserved memory nor MMIO counts: a reserved aperture can be
        // anywhere in the address space, and sizing the direct map from one
        // makes it enormous.
        assert_eq!(view.total_ram(), 0x9F000 + 0x3F00_0000);
        assert_eq!(view.usable_ram(), 0x9F000 + 0x3F00_0000);
        assert_eq!(view.max_ram_address(), 0x10_0000 + 0x3F00_0000);
        assert!(
            !MemKind::Reserved.is_ram(),
            "a reserved aperture at 1 TiB must not size the direct map"
        );
        assert!(
            MemKind::DeviceTree.is_ram() && !MemKind::DeviceTree.is_reclaimable(),
            "the device tree copy is RAM the kernel keeps"
        );
    }

    #[test]
    fn finds_the_region_holding_an_address() {
        let map = regions();
        let info = boot_info(&map, "");
        // SAFETY: `map` and the command line outlive the view, and the
        // structure was built by `boot_info` above, so its addresses
        // describe exactly what it says they do.
        let view = unsafe { info.validate() }.unwrap();

        assert_eq!(
            view.region_of(0x2000).map(|r| r.kind),
            Some(MemKind::Usable)
        );
        assert_eq!(
            view.region_of(0xFEC0_0000).map(|r| r.kind),
            Some(MemKind::Mmio)
        );
        assert!(
            view.region_of(0xFFFF_FFFF_0000).is_none(),
            "gaps are not regions"
        );
    }

    #[test]
    fn parses_command_line_options() {
        let map = regions();
        let info = boot_info(&map, "console=ttyS0 root=/dev/vda1 quiet sched=rt");
        // SAFETY: `map` and the command line outlive the view, and the
        // structure was built by `boot_info` above, so its addresses
        // describe exactly what it says they do.
        let view = unsafe { info.validate() }.unwrap();

        assert_eq!(view.option("console"), Some("ttyS0"));
        assert_eq!(view.option("root"), Some("/dev/vda1"));
        assert_eq!(view.option("sched"), Some("rt"));
        assert_eq!(view.option("missing"), None);
        assert!(view.flag("quiet"));
        assert!(!view.flag("verbose"));
        assert!(
            !view.flag("console"),
            "a key=value option is not a bare flag"
        );
    }

    #[test]
    fn address_space_predicates_agree_with_the_layout() {
        assert!(is_user_address(0));
        assert!(is_user_address(USER_VIRT_END - 1));
        assert!(!is_user_address(USER_VIRT_END));
        assert!(is_kernel_address(KERNEL_HALF_BASE));
        assert!(is_kernel_address(PHYSMAP_BASE));
        assert!(is_kernel_address(KERNEL_VMAP_BASE));
        assert!(is_kernel_address(KERNEL_VIRT_BASE));
        assert!(!is_kernel_address(USER_VIRT_END - 1));
        // The layout constants themselves are checked at compile time, next
        // to where they are defined.
    }

    #[test]
    fn both_layouts_are_consistent() {
        assert_eq!(LAYOUT_64.violation(), None, "LAYOUT_64");
        assert_eq!(LAYOUT_32.violation(), None, "LAYOUT_32");
    }

    #[test]
    fn the_32_bit_layout_fits_four_gibibytes() {
        let layout = LAYOUT_32;
        assert_eq!(layout.user_end, 0x8000_0000, "a 2/2 split");
        assert_eq!(
            layout.kernel_half, layout.vmap_base,
            "the vmap area is the bottom of the kernel half"
        );
        assert_eq!(
            layout.physmap_size(),
            0x5000_0000,
            "1.25 GiB of direct map is the RAM a 32-bit kernel can reach"
        );
        assert!(layout.kernel_base < 1 << 32);
        // `TTBR1` translates the top 2 GiB with `TTBCR.T1SZ = 1`, so the whole
        // kernel half has to start exactly there.
        assert_eq!(layout.kernel_half, 1 << 31);
    }

    #[test]
    fn a_layout_that_overlaps_is_named_as_such() {
        let overlapping = Layout {
            physmap_end: LAYOUT_32.kernel_base + PAGE_SIZE,
            ..LAYOUT_32
        };
        assert_eq!(
            overlapping.violation(),
            Some("the kernel image must be above everything else")
        );

        let inverted = Layout {
            vmap_base: LAYOUT_32.physmap_base,
            ..LAYOUT_32
        };
        assert_eq!(
            inverted.violation(),
            Some("the direct map must not overlap the vmap area")
        );
    }

    #[test]
    fn the_direct_map_starts_at_the_lowest_ram_rounded_down() {
        assert_eq!(physmap_origin(0), 0, "RAM at zero, as on x86-64");
        assert_eq!(physmap_origin(0x4000_0000), 0x4000_0000);
        assert_eq!(physmap_origin(0x4012_3000), 0x4000_0000);
        assert_eq!(
            direct_map_address(0x4000_0000, 0x4000_2000),
            PHYSMAP_BASE + 0x2000
        );
        assert_eq!(direct_map_address(0, 0x2000), PHYSMAP_BASE + 0x2000);
    }

    #[test]
    fn region_arithmetic_is_in_frames() {
        let region = MemRegion {
            base: 0x1000,
            len: 0x3000,
            kind: MemKind::Usable,
            reserved: 0,
        };
        assert_eq!(region.end(), 0x4000);
        assert_eq!(region.frames(), 3);
        assert!(region.contains(0x1000));
        assert!(region.contains(0x3FFF));
        assert!(!region.contains(0x4000), "end is exclusive");
        assert!(!region.contains(0xFFF));
    }

    #[test]
    fn every_architecture_names_its_elf_machine() {
        assert_eq!(Arch::X86_64.elf_machine(), 62);
        assert_eq!(Arch::AArch64.elf_machine(), 183);
        assert_eq!(Arch::Armv7a.elf_machine(), 40);
        assert_eq!(Arch::Armv7a.name(), "armv7a");
    }
}
