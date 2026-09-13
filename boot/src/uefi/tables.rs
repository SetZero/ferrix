//! The system table and boot services.
//!
//! Field order here is the specification's, not ours. Every function pointer
//! is declared even where it is never called, because omitting one shifts
//! every field after it and the failure is a call to the wrong service.

use core::ffi::c_void;

use super::protocols::SimpleTextOutput;
use super::{Guid, Handle, Status, TableHeader};

/// How firmware should choose the address in `allocate_pages`.
///
/// The loader never asks for a particular address — the kernel is
/// position-independent as to *physical* placement, and the page tables built
/// below map wherever firmware chose. It does sometimes need an upper bound,
/// on a 32-bit machine with more RAM than the direct map holds.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct AllocateType(pub(crate) u32);

impl AllocateType {
    /// Anywhere firmware likes.
    pub(crate) const ANY_PAGES: AllocateType = AllocateType(0);
    /// Anywhere whose last byte is at or below the address passed in.
    pub(crate) const MAX_ADDRESS: AllocateType = AllocateType(1);
}

/// UEFI memory types.
///
/// A newtype rather than an `enum`, for the same reason as [`AllocateType`]:
/// only the values the loader actually asks for need to exist here. The
/// standard types firmware reports back are matched numerically in
/// `load::describe`, which is the one place they are read.
///
/// The `FERRIX_*` values are in the range the specification reserves for an OS
/// loader (`0x8000_0000` and up). Using them means firmware's own memory map
/// tells the kernel which allocations are ours and what each one is for — so
/// the kernel does not have to be told separately, and the two cannot disagree.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct MemoryType(pub(crate) u32);

impl MemoryType {
    /// Scratch memory the loader frees by exiting: the kernel file buffer and
    /// the memory map buffer.
    pub(crate) const LOADER_DATA: MemoryType = MemoryType(2);
    /// The kernel image.
    pub(crate) const FERRIX_KERNEL: MemoryType = MemoryType(0x8000_0000);
    /// Page tables the loader built, which are live when the kernel starts.
    pub(crate) const FERRIX_PAGE_TABLES: MemoryType = MemoryType(0x8000_0001);
    /// The stack the kernel is entered on.
    pub(crate) const FERRIX_BOOT_STACK: MemoryType = MemoryType(0x8000_0002);
    /// The boot info structure and the memory map array inside it.
    pub(crate) const FERRIX_BOOT_INFO: MemoryType = MemoryType(0x8000_0003);
    /// The initial ramdisk, when the image carries one.
    pub(crate) const FERRIX_INITRD: MemoryType = MemoryType(0x8000_0004);
    /// The loader's copy of the device tree, which the kernel keeps for good.
    pub(crate) const FERRIX_DEVICE_TREE: MemoryType = MemoryType(0x8000_0005);
}

/// One entry of the UEFI memory map.
///
/// Never index an array of these by `size_of::<MemoryDescriptor>()`: firmware
/// reports its own `descriptor_size`, which may be larger, and walking with
/// the wrong stride is a memory map that looks plausible and is wrong.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemoryDescriptor {
    pub(crate) memory_type: u32,
    pub(crate) padding: u32,
    pub(crate) physical_start: u64,
    pub(crate) virtual_start: u64,
    pub(crate) number_of_pages: u64,
    pub(crate) attribute: u64,
}

/// An entry of the system table's configuration array.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConfigurationTable {
    pub(crate) vendor_guid: Guid,
    pub(crate) vendor_table: *mut c_void,
}

/// ACPI 2.0 and later root pointer.
pub(crate) const ACPI_20_GUID: Guid = Guid::new(
    0x8868_e871,
    0xe4f1,
    0x11d3,
    [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81],
);

/// ACPI 1.0 root pointer, used only if 2.0 is absent.
pub(crate) const ACPI_10_GUID: Guid = Guid::new(
    0xeb9d_2d30,
    0x2d88,
    0x11d3,
    [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
);

/// A flattened device tree: how an ARMv7-A machine describes itself, and an
/// `AArch64` one whose firmware offers no ACPI.
pub(crate) const DEVICE_TREE_GUID: Guid = Guid::new(
    0xb1b6_21d5,
    0xf19c,
    0x41a5,
    [0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0],
);

/// Boot services: everything that stops existing at `exit_boot_services`.
#[repr(C)]
pub(crate) struct BootServices {
    pub(crate) header: TableHeader,

    // Task priority.
    raise_tpl: usize,
    restore_tpl: usize,

    // Memory.
    pub(crate) allocate_pages: unsafe extern "efiapi" fn(
        allocate_type: AllocateType,
        memory_type: MemoryType,
        pages: usize,
        memory: *mut u64,
    ) -> Status,
    pub(crate) free_pages: unsafe extern "efiapi" fn(memory: u64, pages: usize) -> Status,
    pub(crate) get_memory_map: unsafe extern "efiapi" fn(
        map_size: *mut usize,
        map: *mut MemoryDescriptor,
        map_key: *mut usize,
        descriptor_size: *mut usize,
        descriptor_version: *mut u32,
    ) -> Status,
    pub(crate) allocate_pool: unsafe extern "efiapi" fn(
        pool_type: MemoryType,
        size: usize,
        buffer: *mut *mut u8,
    ) -> Status,
    pub(crate) free_pool: unsafe extern "efiapi" fn(buffer: *mut u8) -> Status,

    // Events and timers.
    create_event: usize,
    set_timer: usize,
    wait_for_event: usize,
    signal_event: usize,
    close_event: usize,
    check_event: usize,

    // Protocol handling.
    install_protocol_interface: usize,
    reinstall_protocol_interface: usize,
    uninstall_protocol_interface: usize,
    pub(crate) handle_protocol: unsafe extern "efiapi" fn(
        handle: Handle,
        protocol: *const Guid,
        interface: *mut *mut c_void,
    ) -> Status,
    reserved: usize,
    register_protocol_notify: usize,
    locate_handle: usize,
    locate_device_path: usize,
    install_configuration_table: usize,

    // Images.
    load_image: usize,
    start_image: usize,
    exit: usize,
    unload_image: usize,
    pub(crate) exit_boot_services:
        unsafe extern "efiapi" fn(image: Handle, map_key: usize) -> Status,

    // Miscellaneous.
    get_next_monotonic_count: usize,
    pub(crate) stall: unsafe extern "efiapi" fn(microseconds: usize) -> Status,
    set_watchdog_timer: usize,

    // Driver support.
    connect_controller: usize,
    disconnect_controller: usize,

    // Opening and closing protocols.
    open_protocol: usize,
    close_protocol: usize,
    open_protocol_information: usize,

    // Library services.
    protocols_per_handle: usize,
    locate_handle_buffer: usize,
    pub(crate) locate_protocol: unsafe extern "efiapi" fn(
        protocol: *const Guid,
        registration: *mut c_void,
        interface: *mut *mut c_void,
    ) -> Status,
    install_multiple_protocol_interfaces: usize,
    uninstall_multiple_protocol_interfaces: usize,

    // CRC.
    calculate_crc32: usize,

    // Memory utilities and extended events.
    copy_mem: usize,
    set_mem: usize,
    create_event_ex: usize,
}

/// The table firmware hands the loader.
#[repr(C)]
pub(crate) struct SystemTable {
    pub(crate) header: TableHeader,
    pub(crate) firmware_vendor: *const u16,
    pub(crate) firmware_revision: u32,
    pub(crate) console_in_handle: Handle,
    pub(crate) con_in: *mut c_void,
    pub(crate) console_out_handle: Handle,
    pub(crate) con_out: *mut SimpleTextOutput,
    pub(crate) standard_error_handle: Handle,
    pub(crate) std_err: *mut SimpleTextOutput,
    pub(crate) runtime_services: *mut c_void,
    pub(crate) boot_services: *mut BootServices,
    pub(crate) number_of_table_entries: usize,
    pub(crate) configuration_table: *mut ConfigurationTable,
}
