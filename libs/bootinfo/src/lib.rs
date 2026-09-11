//! The hand-off ABI between the UEFI loader and the kernel.
//!
//! These are two separate programs, linked for different targets, that meet at
//! exactly one struct. Both compile it from this one definition, every type is
//! `repr(C)`, and the kernel refuses to start if the magic or version disagree
//! — because the failure mode of a silent layout mismatch is a triple fault
//! with nothing on the serial port to say why.
//!
//! # Reading it safely
//!
//! [`BootInfo`] is a raw structure full of pointers the kernel did not create.
//! Rather than sprinkle `unsafe` over every reader, the kernel validates it
//! once through [`BootInfo::validate`] and works with the resulting
//! [`BootView`], whose accessors are safe.
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
pub const BOOTINFO_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Virtual memory layout
// ---------------------------------------------------------------------------

/// Base of the direct map of all physical RAM.
///
/// Physical address `p` is readable at `PHYSMAP_BASE + p` once the loader's
/// page tables are live, and stays that way for the life of the kernel.
pub const PHYSMAP_BASE: u64 = 0xFFFF_8000_0000_0000;

/// End of the direct map, exclusive.
pub const PHYSMAP_END: u64 = 0xFFFF_FF00_0000_0000;

/// Base of the kernel's dynamic virtual allocation area.
///
/// Used for `MMIO` windows, guard-paged kernel stacks and large kernel
/// mappings — anything whose virtual address is chosen at runtime.
pub const KERNEL_VMAP_BASE: u64 = 0xFFFF_FF00_0000_0000;

/// Size of the kernel's dynamic virtual allocation area, in bytes.
pub const KERNEL_VMAP_SIZE: u64 = 0x0000_00EF_0000_0000;

/// Where the kernel image is linked.
///
/// The top -2 GiB, which is what the x86-64 "kernel" code model addresses.
/// AArch64 does not require it and uses it anyway, because one layout across
/// both architectures is one set of bugs instead of two.
pub const KERNEL_VIRT_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Highest user virtual address, exclusive: a 47-bit user half on both
/// architectures.
pub const USER_VIRT_END: u64 = 0x0000_8000_0000_0000;

/// The page size both architectures are configured for.
pub const PAGE_SIZE: u64 = 4096;

// The layout above has to be laid out. These are compile-time assertions
// rather than tests on purpose: an overlap between the direct map and the
// vmap area is not something to discover from a failing test run, and a
// `const` assertion fails in the build that introduced it.
const _: () = assert!(
    PHYSMAP_BASE < PHYSMAP_END,
    "the direct map must span a positive range"
);
const _: () = assert!(
    PHYSMAP_END <= KERNEL_VMAP_BASE,
    "the direct map must not overlap the vmap area"
);
const _: () = assert!(
    KERNEL_VMAP_BASE + KERNEL_VMAP_SIZE <= KERNEL_VIRT_BASE,
    "the vmap area must not overlap the kernel image"
);
const _: () = assert!(
    USER_VIRT_END < PHYSMAP_BASE,
    "the user half must end before the kernel half begins"
);

/// The stack the loader hands the kernel, in bytes. Replaced by a guard-paged
/// per-CPU stack as soon as the kernel can allocate one.
pub const BOOT_STACK_SIZE: u64 = 64 * 1024;

/// True if `addr` is in the half of the address space reserved for the kernel.
#[must_use]
pub const fn is_kernel_address(addr: u64) -> bool {
    addr >= PHYSMAP_BASE
}

/// True if `addr` is a valid user virtual address.
#[must_use]
pub const fn is_user_address(addr: u64) -> bool {
    addr < USER_VIRT_END
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
}

impl Arch {
    /// The `e_machine` value an ELF image for this architecture carries.
    #[must_use]
    pub const fn elf_machine(self) -> u16 {
        match self {
            Arch::X86_64 => 62,
            Arch::AArch64 => 183,
        }
    }

    /// The short name used in paths, log lines and target triples.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
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
/// Pointer fields are *virtual* addresses valid in the page tables the loader
/// installed before jumping to the kernel — that is, inside the direct map at
/// [`PHYSMAP_BASE`]. Address fields named `_phys` are physical.
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

    /// Pointer to `regions_len` [`MemRegion`]s, sorted by base and
    /// non-overlapping.
    pub regions: *const MemRegion,
    /// Number of entries `regions` points at.
    pub regions_len: u64,

    /// Base of the direct physical map. Mirrors [`PHYSMAP_BASE`], carried
    /// explicitly so the kernel never has to assume.
    pub physmap_base: u64,
    /// Bytes of physical address space the direct map covers.
    pub physmap_len: u64,

    /// Physical address the kernel image was loaded at.
    pub kernel_phys: u64,
    /// Virtual address the kernel image is mapped at.
    pub kernel_virt: u64,
    /// Size of the kernel image in bytes, page rounded.
    pub kernel_len: u64,

    /// Physical address of the root page table: the x86-64 PML4, or the
    /// AArch64 `TTBR1_EL1` table.
    pub root_table_phys: u64,
    /// AArch64 only: physical address of the identity-mapping `TTBR0_EL1`
    /// table. Zero on x86-64, where one table covers both halves.
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
    /// Physical address of the flattened device tree, or 0.
    pub dtb: u64,
    /// Physical address of the UEFI system table. Boot services are gone by
    /// the time the kernel sees this; runtime services are still callable.
    pub uefi_system_table: u64,

    /// Pointer to `cmdline_len` bytes of UTF-8 kernel command line.
    pub cmdline: *const u8,
    /// Length of the command line in bytes.
    pub cmdline_len: u64,
}

// SAFETY: the loader writes this structure once, before the kernel exists, and
// never touches it again; the kernel only reads it. There is no aliasing
// writer, so the raw pointers may cross the hand-off.
unsafe impl Send for BootInfo {}
// SAFETY: as above — immutable for the whole life of the system once the
// kernel is entered, so shared access from several CPUs is sound.
unsafe impl Sync for BootInfo {}

/// Why a [`BootInfo`] was rejected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BootInfoError {
    /// The magic number was wrong: this is not a `BootInfo` at all.
    BadMagic,
    /// The loader and the kernel were built from different versions of this
    /// file. Carries the version the loader wrote.
    VersionMismatch(u32),
    /// The memory map pointer was null, or its length zero.
    NoMemoryMap,
    /// The direct map does not start where the kernel was built to expect.
    PhysmapMismatch,
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
            BootInfoError::CmdlineNotUtf8 => f.write_str("kernel command line is not UTF-8"),
        }
    }
}

impl BootInfo {
    /// Check the hand-off and produce a view whose accessors are safe.
    ///
    /// This is the only place the raw pointers are dereferenced, so it is the
    /// only place that has to be reasoned about.
    ///
    /// # Safety
    ///
    /// `self.regions` must point to `self.regions_len` initialised
    /// [`MemRegion`]s, and `self.cmdline` to `self.cmdline_len` initialised
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
        if self.regions.is_null() || self.regions_len == 0 {
            return Err(BootInfoError::NoMemoryMap);
        }
        if self.physmap_base != PHYSMAP_BASE {
            return Err(BootInfoError::PhysmapMismatch);
        }

        // SAFETY: the caller's contract is exactly that this pointer and length
        // describe an initialised, immutable slice.
        let regions =
            unsafe { core::slice::from_raw_parts(self.regions, self.regions_len as usize) };

        let cmdline = if self.cmdline.is_null() || self.cmdline_len == 0 {
            ""
        } else {
            // SAFETY: as above, for the command line bytes.
            let bytes =
                unsafe { core::slice::from_raw_parts(self.cmdline, self.cmdline_len as usize) };
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
            regions: map.as_ptr(),
            regions_len: map.len() as u64,
            physmap_base: PHYSMAP_BASE,
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
            uefi_system_table: 0,
            cmdline: cmdline.as_ptr(),
            cmdline_len: cmdline.len() as u64,
        }
    }

    #[test]
    fn validates_a_well_formed_handoff() {
        let map = regions();
        let info = boot_info(&map, "console=ttyS0 quiet");
        // SAFETY: `map` and the command line outlive the view, and the
        // structure was built by `boot_info` above, so its pointers
        // describe exactly what it says they do.
        let view = unsafe { info.validate() }.unwrap();

        assert_eq!(
            view.regions().len(),
            4,
            "every region should survive validation"
        );
        assert_eq!(view.arch(), Arch::X86_64);
        assert_eq!(view.cmdline(), "console=ttyS0 quiet");
    }

    #[test]
    fn rejects_a_mismatched_version() {
        let map = regions();
        let mut info = boot_info(&map, "");
        info.version = BOOTINFO_VERSION + 1;
        assert_eq!(
            // SAFETY: `map` and the command line outlive the view, and the
            // structure was built by `boot_info` above, so its pointers
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
        info.regions = core::ptr::null();
        assert_eq!(
            // SAFETY: `map` and the command line outlive the view, and the
            // structure was built by `boot_info` above, so its pointers
            // describe exactly what it says they do.
            unsafe { info.validate() }.unwrap_err(),
            BootInfoError::BadMagic,
            "magic is checked first so a wild pointer is never followed"
        );
    }

    #[test]
    fn sums_only_real_ram() {
        let map = regions();
        let info = boot_info(&map, "");
        // SAFETY: `map` and the command line outlive the view, and the
        // structure was built by `boot_info` above, so its pointers
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
    }

    #[test]
    fn finds_the_region_holding_an_address() {
        let map = regions();
        let info = boot_info(&map, "");
        // SAFETY: `map` and the command line outlive the view, and the
        // structure was built by `boot_info` above, so its pointers
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
        // structure was built by `boot_info` above, so its pointers
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
        assert!(is_kernel_address(PHYSMAP_BASE));
        assert!(is_kernel_address(KERNEL_VIRT_BASE));
        assert!(!is_kernel_address(USER_VIRT_END));
        // The layout constants themselves are checked at compile time, next
        // to where they are defined.
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
}
