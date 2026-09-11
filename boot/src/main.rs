//! The Ferrix UEFI loader.
//!
//! Firmware calls [`efi_main`] with a stack set up and the MMU on — in 64-bit
//! mode on the 64-bit pair, in SVC mode on ARMv7-A — which is why this project
//! has no bootstrap assembly on any architecture. From there the job is:
//!
//! 1. read the kernel off the volume the loader came from,
//! 2. copy it to the address it was linked for,
//! 3. build the address space `docs/ARCHITECTURE.md` §4 describes,
//! 4. take firmware's memory map and leave boot services,
//! 5. install the new tables and jump.
//!
//! Only step 5 is assembly, and only because the return address of a Rust
//! function call would be in the address space we just replaced.

#![no_std]
#![no_main]

mod arch;
mod console;
mod load;
mod services;
mod uefi;

use core::convert::Infallible;
use core::panic::PanicInfo;
use core::ptr;

use ferrix_bootinfo::{
    BOOT_STACK_SIZE, BOOTINFO_MAGIC, BOOTINFO_VERSION, BootInfo, Framebuffer, MemRegion, PAGE_SIZE,
    PHYSMAP_BASE,
};

use console::println;
use load::{AddressSpace, DirectMap, KernelImage, LoaderMemory};
use services::{Allocation, BootError, MemoryMap, Result, Services};
use uefi::tables::{ACPI_10_GUID, ACPI_20_GUID, DEVICE_TREE_GUID, MemoryType, SystemTable};
use uefi::{Handle, Status};

/// Where the kernel lives on the EFI system partition.
const KERNEL_PATH: &str = "/FERRIX/KERNEL.ELF";

/// Bytes set aside for the boot info structure and the memory map behind it.
/// At 24 bytes a region this holds around 2700 of them; firmware typically
/// reports fewer than a hundred.
const BOOT_INFO_BYTES: u64 = 64 * 1024;

/// Offset of the memory region array within that allocation.
const REGIONS_OFFSET: u64 = PAGE_SIZE;

/// What every flattened device tree begins with, big-endian.
const FDT_MAGIC: u32 = 0xD00D_FEED;

/// The largest device tree the loader will copy. QEMU's is a few kilobytes and
/// a board's a few tens; the bound is there so that a corrupt size field
/// cannot make the loader try to allocate the machine.
const MAX_DEVICE_TREE: u64 = 2 * 1024 * 1024;

/// Firmware's entry point.
///
/// The name is what the UEFI targets look for; it is not called from Rust.
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(image: Handle, system_table: *mut SystemTable) -> Status {
    if !system_table.is_null() {
        // SAFETY: firmware passed this table and guarantees it is live for the
        // whole of boot services.
        let table = unsafe { &*system_table };
        // SAFETY: `con_out` is firmware's own text output protocol, and
        // `console::shutdown` is called before boot services end.
        unsafe { console::init(table.con_out) };
    }

    println!(
        "Ferrix loader {} ({})",
        env!("CARGO_PKG_VERSION"),
        arch::ARCH.name()
    );

    match boot(image, system_table) {
        // `boot` returns `Infallible` on success, so this arm cannot be reached
        // and the compiler knows it.
        Ok(never) => match never {},
        Err(error) => {
            println!("FERRIX-PANIC loader: {error}");
            Status::LOAD_ERROR
        }
    }
}

/// Everything between firmware and the kernel.
fn boot(image: Handle, system_table: *mut SystemTable) -> Result<Infallible> {
    // SAFETY: these are the arguments firmware passed to `efi_main`, and boot
    // services have not been exited.
    let services = unsafe { Services::new(image, system_table)? };
    arch::prepare_cpu().map_err(BootError::plain)?;

    let kernel = stage_kernel(&services)?;
    let mut memory = LoaderMemory::new(&services)?;

    let stack = services.allocate(
        "allocating the boot stack",
        BOOT_STACK_SIZE,
        MemoryType::FERRIX_BOOT_STACK,
    )?;
    let info_area = services.allocate(
        "allocating the boot info",
        BOOT_INFO_BYTES,
        MemoryType::FERRIX_BOOT_INFO,
    )?;
    let device_tree = copy_device_tree(&services)?;
    let map_buffer = services.allocate(
        "allocating the memory map buffer",
        services.memory_map_size()?,
        MemoryType::LOADER_DATA,
    )?;

    // A first look at the memory map, only to learn where RAM is. The map
    // fetched here is stale the moment anything else is allocated, which is
    // why the one handed to the kernel is fetched again below.
    let direct = DirectMap::of(&services.memory_map(map_buffer)?)?;
    println!(
        "  direct map of {:#x}..{:#x}, kernel at {:#x}",
        direct.origin,
        direct.end(),
        kernel.image.memory.address
    );

    // Everything the kernel is handed has to be reachable through the direct
    // map. That is not a given on a 32-bit machine with more RAM than the
    // direct map holds, because firmware allocates from the top down.
    let handed_over = [kernel.image.memory, memory.pool(), stack, info_area];
    let copied = device_tree.map(|(copy, _)| copy);
    if !handed_over
        .into_iter()
        .chain(copied)
        .all(|allocation| direct.covers(allocation))
    {
        return Err(BootError::plain(
            "firmware placed a loader allocation above the direct map",
        ));
    }

    let space = load::build_address_space(&mut memory, &kernel.elf()?, &kernel.image, direct)?;
    println!(
        "  {} page tables, roots {:#x}/{:#x}",
        memory.tables_used(),
        space.kernel_root.0,
        space.identity_root.0
    );

    write_boot_info(
        &services,
        &kernel.image,
        &space,
        stack,
        info_area,
        direct,
        device_tree,
    );

    // Past this line firmware is gone: no allocation, no console, no protocols.
    console::shutdown();
    let map = leave_firmware(&services, map_buffer)?;
    let regions = record_memory_map(&map, info_area);
    finish_boot_info(info_area, regions, direct);

    // The Arm architectures turn the MMU off in the middle of the switch, so
    // anything still dirty in a cache would vanish. No-op on x86-64.
    arch::clean_dcache(memory.pool().address, memory.pool().len);
    arch::clean_dcache(kernel.image.memory.address, kernel.image.memory.len);
    arch::clean_dcache(info_area.address, info_area.len);
    arch::clean_dcache(stack.address, stack.len);
    if let Some((copy, _)) = device_tree {
        arch::clean_dcache(copy.address, copy.len);
    }

    // SAFETY: boot services are gone, `prepare_cpu` ran above, and the tables
    // in `space` identity map the loader's own code, which is what makes the
    // instruction after the switch fetchable.
    unsafe {
        arch::enter_kernel(arch::Handoff {
            root_table: space.kernel_root.0,
            identity_table: space.identity_root.0,
            entry: kernel.image.entry,
            // Both the stack and the boot info are in RAM, so the direct map
            // already covers them and neither needs a mapping of its own.
            stack_top: direct.address(stack.address + stack.len),
            boot_info: direct.address(info_area.address),
        })
    }
}

/// Copy firmware's device tree into memory the kernel keeps.
///
/// Firmware's own copy lives wherever firmware put it — U-Boot uses
/// `EfiACPIReclaimMemory`, which the kernel hands to the frame allocator once
/// interrupt bring-up has read it — and stage 10 enumerates devices from the
/// tree long after that. So the loader takes a copy, in memory the map reports
/// as `DeviceTree`, which nothing reclaims.
///
/// `None` on a machine that offers no tree, which is the ordinary case on
/// x86-64 and on AArch64 under ACPI.
fn copy_device_tree(services: &Services) -> Result<Option<(Allocation, u64)>> {
    let Some(tree) = services.configuration_table(&DEVICE_TREE_GUID) else {
        return Ok(None);
    };

    // SAFETY: firmware published a device tree at this address, identity
    // mapped under boot services, and every tree begins with a forty-byte
    // header whose first eight bytes are read here.
    let header = unsafe { core::slice::from_raw_parts(tree as *const u8, 8) };
    let word = |at: usize| {
        header
            .get(at..at + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map_or(0, u32::from_be_bytes)
    };
    if word(0) != FDT_MAGIC {
        return Err(BootError::plain(
            "firmware's device tree does not begin with the device tree magic",
        ));
    }
    let len = u64::from(word(4));
    if !(40..=MAX_DEVICE_TREE).contains(&len) {
        return Err(BootError::plain(
            "firmware's device tree claims an impossible size",
        ));
    }

    let copy = services.allocate(
        "copying the device tree",
        len,
        MemoryType::FERRIX_DEVICE_TREE,
    )?;
    // SAFETY: firmware's tree is `len` bytes long by its own header, the copy
    // is a fresh allocation of at least that many, and the two cannot overlap.
    unsafe { ptr::copy_nonoverlapping(tree as *const u8, copy.address as *mut u8, len as usize) };
    println!("  device tree copied, {len} bytes");
    Ok(Some((copy, len)))
}

/// The kernel file and the image copied out of it.
///
/// The file allocation is kept because the parsed [`ferrix_elf::Elf`] borrows
/// it, and the address space is built from the same parse that placed it.
#[derive(Clone, Copy, Debug)]
struct StagedKernel {
    file: Allocation,
    file_len: u64,
    image: KernelImage,
}

impl StagedKernel {
    /// Re-borrow the file bytes as a parsed image.
    fn elf(&self) -> Result<ferrix_elf::Elf<'_>> {
        // SAFETY: `file` is our own allocation, firmware filled `file_len` of
        // its bytes, and it is identity mapped and not freed until we exit.
        let bytes = unsafe {
            core::slice::from_raw_parts(self.file.address as *const u8, self.file_len as usize)
        };
        load::parse_kernel(bytes)
    }
}

/// Read the kernel and copy it to where it will run.
fn stage_kernel(services: &Services) -> Result<StagedKernel> {
    let (file, file_len) = load::read_kernel_file(services, KERNEL_PATH)?;
    let staged = StagedKernel {
        file,
        file_len,
        image: KernelImage {
            memory: Allocation { address: 0, len: 0 },
            virt_base: 0,
            entry: 0,
        },
    };
    let image = load::place_kernel(services, &staged.elf()?)?;
    Ok(StagedKernel { image, ..staged })
}

/// Fill in everything about the boot info that firmware can still be asked.
fn write_boot_info(
    services: &Services,
    kernel: &KernelImage,
    space: &AddressSpace,
    stack: Allocation,
    info_area: Allocation,
    direct: DirectMap,
    device_tree: Option<(Allocation, u64)>,
) {
    let info = BootInfo {
        magic: BOOTINFO_MAGIC,
        version: BOOTINFO_VERSION,
        arch: arch::ARCH,
        // Filled in by `finish_boot_info` once the final map has been taken.
        regions: 0,
        regions_len: 0,
        physmap_base: PHYSMAP_BASE,
        physmap_phys: direct.origin,
        physmap_len: direct.len,
        kernel_phys: kernel.memory.address,
        kernel_virt: kernel.virt_base,
        kernel_len: kernel.memory.len,
        root_table_phys: space.kernel_root.0,
        ttbr0_phys: if arch::SEPARATE_IDENTITY_TABLE {
            space.identity_root.0
        } else {
            0
        },
        boot_stack_top: direct.address(stack.address + stack.len),
        boot_stack_size: stack.len,
        framebuffer: services.framebuffer().unwrap_or(Framebuffer::NONE),
        initrd_phys: 0,
        initrd_len: 0,
        rsdp: services
            .configuration_table(&ACPI_20_GUID)
            .or_else(|| services.configuration_table(&ACPI_10_GUID))
            .unwrap_or(0),
        dtb: device_tree.map_or(0, |(copy, _)| copy.address),
        dtb_len: device_tree.map_or(0, |(_, len)| len),
        uefi_system_table: services.system_table() as u64,
        cmdline: 0,
        cmdline_len: 0,
    };

    // SAFETY: `info_area` is our own allocation of BOOT_INFO_BYTES, identity
    // mapped, and larger than one BootInfo.
    unsafe { ptr::write_volatile(info_area.address as *mut BootInfo, info) };
}

/// Take the final memory map and leave boot services.
///
/// The specification allows `exit_boot_services` to fail if the map changed
/// between fetching it and the call, and says to fetch it again and retry once.
fn leave_firmware(services: &Services, buffer: Allocation) -> Result<MemoryMap> {
    let map = services.memory_map(buffer)?;
    if services.exit_boot_services(map.key).is_ok() {
        return Ok(map);
    }

    let map = services.memory_map(buffer)?;
    services.exit_boot_services(map.key)?;
    Ok(map)
}

/// Copy the firmware memory map into the boot info, sorted by address.
///
/// Runs after `exit_boot_services`, so it touches nothing but memory the loader
/// allocated for itself.
fn record_memory_map(map: &MemoryMap, info_area: Allocation) -> u64 {
    let array = (info_area.address + REGIONS_OFFSET) as *mut MemRegion;
    let capacity = (info_area.len - REGIONS_OFFSET) / size_of::<MemRegion>() as u64;

    // SAFETY: the array sits inside the loader's own allocation, `capacity` was
    // computed from that allocation's length, and `Services::allocate` zeroed
    // it. An all-zero `MemRegion` is a valid one -- base 0, length 0, kind
    // `Usable` -- so the slice is initialised even before anything is written.
    let slots = unsafe { core::slice::from_raw_parts_mut(array, capacity as usize) };

    let mut count = 0usize;
    for (slot, descriptor) in slots.iter_mut().zip(map.entries()) {
        *slot = load::describe(&descriptor);
        count += 1;
    }

    // Firmware usually reports the map in address order but is not required
    // to, and the kernel's frame allocator walks it assuming it is. Sorting a
    // slice needs no allocator, which is just as well: there is none, and
    // firmware is gone.
    let Some(written) = slots.get_mut(..count) else {
        return 0;
    };
    written.sort_unstable_by_key(|region| region.base);

    count as u64
}

/// Point the boot info at the memory map now that it exists.
fn finish_boot_info(info_area: Allocation, regions: u64, direct: DirectMap) {
    let info = info_area.address as *mut BootInfo;
    // SAFETY: `write_boot_info` put a BootInfo here, and nothing else refers to
    // it.
    let mut value = unsafe { ptr::read_volatile(info) };
    // The kernel reads this through the direct map, so the pointer it is given
    // has to be the virtual one.
    value.regions = direct.address(info_area.address + REGIONS_OFFSET);
    value.regions_len = regions;
    // SAFETY: as above.
    unsafe { ptr::write_volatile(info, value) };
}

/// Where a loader panic ends up.
///
/// There is nothing to unwind to and, after `exit_boot_services`, nothing left
/// to print with — so this says what it can and stops the machine rather than
/// letting it wander.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!("FERRIX-PANIC loader: {info}");
    loop {
        core::hint::spin_loop();
    }
}
