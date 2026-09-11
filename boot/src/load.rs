//! Placing the kernel and building the address space it will run in.

use core::ptr;

use ferrix_bootinfo::{
    KERNEL_VIRT_BASE, MemKind, MemRegion, PAGE_SIZE, PHYSMAP_ALIGN, PHYSMAP_BASE, PHYSMAP_END,
    USER_VIRT_END, direct_map_address, physmap_origin,
};
use ferrix_elf::{Elf, PF_W, PF_X, Segment};
use ferrix_paging::{MapFlags, Mapper, PhysAddr, PhysMem, VirtAddr};

use crate::arch::{self, PageEncoding};
use crate::services::{Allocation, BootError, MemoryMap, Result, Services};
use crate::uefi::tables::{MemoryDescriptor, MemoryType};

/// Frames set aside for page tables.
///
/// Two mebibytes is 512 tables, far more than the three mappings below need
/// even on a large machine. Taking it as one allocation rather than 512 keeps
/// the firmware memory map short: every separate `allocate_pages` adds a
/// descriptor to it, and the map has to be copied before the kernel starts.
const TABLE_POOL_BYTES: u64 = 2 * 1024 * 1024;

/// Physical memory as the loader sees it: identity mapped, because that is what
/// UEFI leaves in place, with a bump allocator for page table frames.
#[derive(Debug)]
pub(crate) struct LoaderMemory {
    pool: Allocation,
    used: u64,
}

impl LoaderMemory {
    /// Take a pool of frames to build page tables out of.
    pub(crate) fn new(services: &Services) -> Result<LoaderMemory> {
        let pool = services.allocate(
            "allocating the page table pool",
            TABLE_POOL_BYTES,
            MemoryType::FERRIX_PAGE_TABLES,
        )?;
        Ok(LoaderMemory { pool, used: 0 })
    }

    /// The pool, so the caller can clean it out of the caches before the MMU
    /// is turned off.
    pub(crate) const fn pool(&self) -> Allocation {
        self.pool
    }

    /// How many frames have been turned into page tables.
    pub(crate) const fn tables_used(&self) -> u64 {
        self.used / PAGE_SIZE
    }
}

// SAFETY: under boot services every physical address is identity mapped, so a
// physical address is a valid pointer. Frames come from a pool this type owns
// exclusively, are handed out at most once, are page aligned because the pool
// is, and were zeroed when the pool was allocated.
unsafe impl PhysMem for LoaderMemory {
    fn read(&self, at: PhysAddr) -> u64 {
        // SAFETY: `at` is a descriptor inside a table from our own pool, which
        // the identity map makes a valid, aligned pointer.
        unsafe { ptr::read_volatile(at.0 as *const u64) }
    }

    fn write(&mut self, at: PhysAddr, value: u64) {
        // SAFETY: as above, and nothing else holds a reference to the pool.
        unsafe { ptr::write_volatile(at.0 as *mut u64, value) };
    }

    fn allocate_table(&mut self) -> Option<PhysAddr> {
        let next = self.used.checked_add(PAGE_SIZE)?;
        if next > self.pool.len {
            return None;
        }
        let frame = PhysAddr(self.pool.address + self.used);
        self.used = next;
        Some(frame)
    }
}

/// Read the kernel off the boot volume into memory the loader owns.
///
/// Returns the allocation and the file's real length, which is smaller than
/// the allocation unless the file happens to be a whole number of pages.
pub(crate) fn read_kernel_file(services: &Services, path: &str) -> Result<(Allocation, u64)> {
    services.read_file(path, MemoryType::LOADER_DATA)
}

/// Parse and vet a kernel image before anything is mapped.
pub(crate) fn parse_kernel(bytes: &[u8]) -> Result<Elf<'_>> {
    let elf =
        Elf::parse(bytes).map_err(|_| BootError::plain("the kernel is not a valid ELF image"))?;
    if elf.class() != arch::ELF_CLASS {
        return Err(BootError::plain(
            "the kernel was built for another word width",
        ));
    }
    elf.check_machine(arch::ELF_MACHINE)
        .map_err(|_| BootError::plain("the kernel was built for another architecture"))?;
    elf.validate_segments()
        .map_err(|_| BootError::plain("the kernel has a malformed segment"))?;
    Ok(elf)
}

/// Where the kernel image ended up.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KernelImage {
    /// Physical memory holding the image.
    pub(crate) memory: Allocation,
    /// Lowest virtual address the image occupies.
    pub(crate) virt_base: u64,
    /// Virtual address of the entry point.
    pub(crate) entry: u64,
}

/// Copy the kernel where it will run.
///
/// The image goes wherever firmware offers physical memory and is mapped to the
/// address it was linked for, so nothing here depends on a physical layout.
pub(crate) fn place_kernel(services: &Services, elf: &Elf<'_>) -> Result<KernelImage> {
    let (low, high) = elf
        .load_span(PAGE_SIZE)
        .ok_or_else(|| BootError::plain("the kernel loads no segments"))?;
    if low != KERNEL_VIRT_BASE {
        return Err(BootError::plain(
            "the kernel is not linked at the address the loader maps it to",
        ));
    }

    let memory = services.allocate(
        "allocating the kernel image",
        high - low,
        MemoryType::FERRIX_KERNEL,
    )?;

    for segment in elf.loadable() {
        copy_segment(elf, &segment, memory.address, low)?;
    }

    Ok(KernelImage {
        memory,
        virt_base: low,
        entry: elf.entry(),
    })
}

/// Copy one segment into the image and zero the `.bss` tail behind it.
fn copy_segment(elf: &Elf<'_>, segment: &Segment, base: u64, virt_base: u64) -> Result<()> {
    let data = segment
        .data(elf.image())
        .map_err(|_| BootError::plain("a kernel segment is out of bounds"))?;

    let offset = segment.vaddr - virt_base;
    let destination = base + offset;

    // SAFETY: `destination` is inside the allocation, because `offset` is within
    // the load span the allocation was sized from; the file buffer and the image
    // are separate allocations, so they cannot overlap.
    unsafe { ptr::copy_nonoverlapping(data.as_ptr(), destination as *mut u8, data.len()) };

    // Everything past `p_filesz` is `.bss` and must read as zero. The allocation
    // was already zeroed, so this is belt and braces — but a loader that relies
    // on someone else having zeroed memory is a loader with an intermittent bug.
    let tail = segment.memsz - segment.filesz;
    if tail != 0 {
        // SAFETY: the tail is inside the same allocation, for the same reason.
        unsafe { ptr::write_bytes((destination + segment.filesz) as *mut u8, 0, tail as usize) };
    }
    Ok(())
}

/// The part of physical memory the direct map covers.
///
/// From the lowest RAM address, rounded down to [`PHYSMAP_ALIGN`], to the
/// highest, rounded up — and no further than the direct map's region of the
/// address space holds, which only binds on a 32-bit machine. Starting at the
/// lowest RAM rather than at zero is what lets a 32-bit kernel afford a direct
/// map at all: QEMU's Arm `virt` machines have nothing but flash and device
/// registers below 1 GiB, and mapping that as cacheable memory was never right
/// on AArch64 either.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DirectMap {
    /// The physical address at [`PHYSMAP_BASE`].
    pub(crate) origin: u64,
    /// Bytes from there that are mapped.
    pub(crate) len: u64,
}

impl DirectMap {
    /// The direct map a machine with this memory map gets.
    pub(crate) fn of(map: &MemoryMap) -> Result<DirectMap> {
        let mut low = u64::MAX;
        let mut high = 0u64;
        for descriptor in map.entries() {
            if !describe(&descriptor).kind.is_ram() {
                continue;
            }
            low = low.min(descriptor.physical_start);
            high = high.max(descriptor.physical_start + descriptor.number_of_pages * PAGE_SIZE);
        }
        if high == 0 {
            return Err(BootError::plain("firmware's memory map describes no RAM"));
        }

        let origin = physmap_origin(low);
        let end = high.next_multiple_of(PHYSMAP_ALIGN);
        Ok(DirectMap {
            origin,
            len: (end - origin).min(PHYSMAP_END - PHYSMAP_BASE),
        })
    }

    /// Where physical address `phys` appears in the direct map.
    pub(crate) const fn address(self, phys: u64) -> u64 {
        direct_map_address(self.origin, phys)
    }

    /// One past the last physical address the direct map covers.
    pub(crate) const fn end(self) -> u64 {
        self.origin + self.len
    }

    /// True if every byte of `allocation` is inside the direct map, which is
    /// the only way the kernel can reach it.
    pub(crate) const fn covers(self, allocation: Allocation) -> bool {
        allocation.address >= self.origin && allocation.address + allocation.len <= self.end()
    }
}

/// The page tables the kernel will start on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AddressSpace {
    /// Root of the kernel half: x86-64's PML4, `AArch64`'s `TTBR1_EL1` table,
    /// or the ARMv7-A root `TTBR1` translates the last two entries of.
    pub(crate) kernel_root: PhysAddr,
    /// Root of the identity map, which on x86-64 is the same table.
    pub(crate) identity_root: PhysAddr,
}

/// Build the address space described in `docs/ARCHITECTURE.md` §4.
///
/// Three mappings, and deliberately no more: an identity map so the
/// instructions after the switch still fetch, a direct map of physical
/// memory, and the kernel image at the address it was linked for. The boot
/// stack and the boot info need no mapping of their own — they are in RAM, so
/// the direct map already covers them.
pub(crate) fn build_address_space(
    memory: &mut LoaderMemory,
    elf: &Elf<'_>,
    image: &KernelImage,
    direct: DirectMap,
) -> Result<AddressSpace> {
    let kernel_root = memory
        .allocate_table()
        .ok_or_else(|| BootError::plain("no frame for the kernel root table"))?;
    let identity_root = if arch::SEPARATE_IDENTITY_TABLE {
        memory
            .allocate_table()
            .ok_or_else(|| BootError::plain("no frame for the identity root table"))?
    } else {
        kernel_root
    };

    let kernel: Mapper<PageEncoding> = Mapper::new(kernel_root);
    let identity: Mapper<PageEncoding> = Mapper::new(identity_root);

    map_identity(&identity, memory, direct)?;
    kernel
        .map_range(
            memory,
            VirtAddr(PHYSMAP_BASE),
            PhysAddr(direct.origin),
            direct.len,
            // Never executable: nothing is ever run through the direct map, and
            // it covers every byte of RAM including the kernel's own text.
            MapFlags::KERNEL_DATA,
        )
        .map_err(|_| BootError::plain("could not build the direct map"))?;

    for segment in elf.loadable() {
        map_segment(&kernel, memory, &segment, image)?;
    }

    Ok(AddressSpace {
        kernel_root,
        identity_root,
    })
}

/// Map RAM at its own address, temporarily and executably.
///
/// Only RAM: the loader's code, its stack and its tables are all in RAM, and
/// nothing else needs to be reachable at its physical address for the few
/// instructions between the switch and the jump.
fn map_identity(
    identity: &Mapper<PageEncoding>,
    memory: &mut LoaderMemory,
    direct: DirectMap,
) -> Result<()> {
    // Physical equals virtual here, so the mapping has to fit the lower half.
    // On a 32-bit machine that is 2 GiB, and a board with RAM above it is one
    // this loader cannot enter the kernel on — which it says, rather than
    // mapping its own code into the kernel's half.
    if direct.end() > USER_VIRT_END {
        return Err(BootError::plain(
            "RAM reaches the kernel's half of the address space, where the identity map cannot go",
        ));
    }

    // This mapping has to be executable, which is otherwise never true in this
    // tree: the instruction after the one that installs these tables is fetched
    // through it, and on the Arm architectures so is the whole sequence that
    // turns the MMU back on. It is transient — the kernel drops it once it is
    // running on its own stack, and `docs/ARCHITECTURE.md` §4 says so.
    let transient = MapFlags {
        read: true,
        write: true,
        execute: true,
        user: false,
        global: false,
        device: false,
    };
    identity
        .map_range(
            memory,
            VirtAddr(direct.origin),
            PhysAddr(direct.origin),
            direct.len,
            transient,
        )
        .map_err(|_| BootError::plain("could not build the identity map"))
}

/// Map one kernel segment with the permissions the linker gave it.
fn map_segment(
    kernel: &Mapper<PageEncoding>,
    memory: &mut LoaderMemory,
    segment: &Segment,
    image: &KernelImage,
) -> Result<()> {
    let flags = MapFlags {
        read: true,
        write: segment.flags & PF_W != 0,
        execute: segment.flags & PF_X != 0,
        user: false,
        global: true,
        device: false,
    };

    // The linker page-aligns segments, so rounding the base down and the length
    // up cannot make two segments overlap — and if a future linker script broke
    // that, the mapper would refuse rather than silently re-permission a page.
    let base = segment.vaddr & !(PAGE_SIZE - 1);
    let length = (segment.vaddr - base + segment.memsz).next_multiple_of(PAGE_SIZE);
    let offset = base - image.virt_base;

    kernel
        .map_range(
            memory,
            VirtAddr(base),
            PhysAddr(image.memory.address + offset),
            length,
            flags,
        )
        .map_err(|_| BootError::plain("could not map a kernel segment"))
}

/// Convert one firmware memory descriptor into a Ferrix memory region.
pub(crate) fn describe(descriptor: &MemoryDescriptor) -> MemRegion {
    // The numeric values are UEFI's, including the ones the loader assigned
    // itself out of the range the specification reserves for an OS loader.
    let kind = match descriptor.memory_type {
        7 => MemKind::Usable,
        // Boot services code and data become free the moment we exit them, and
        // on most firmware that is a large fraction of low memory.
        3 | 4 => MemKind::Usable,
        1 | 2 => MemKind::Loader,
        9 => MemKind::AcpiReclaim,
        10 => MemKind::AcpiNvs,
        11 | 12 => MemKind::Mmio,
        8 => MemKind::Defective,
        0x8000_0000 => MemKind::Kernel,
        0x8000_0001 => MemKind::PageTables,
        0x8000_0002 => MemKind::BootStack,
        0x8000_0003 => MemKind::BootInfo,
        0x8000_0004 => MemKind::Initrd,
        0x8000_0005 => MemKind::DeviceTree,
        _ => MemKind::Reserved,
    };

    MemRegion {
        base: descriptor.physical_start,
        len: descriptor.number_of_pages * PAGE_SIZE,
        kind,
        reserved: 0,
    }
}
