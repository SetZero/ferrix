//! Safe-ish wrappers over the boot services the loader uses.
//!
//! Each wrapper turns a UEFI status into a [`BootError`] carrying both what was
//! being attempted and what firmware said, because "load error" on its own at
//! this stage of boot is close to useless.

use core::ffi::c_void;
use core::fmt;
use core::ptr;

use ferrix_bootinfo::PAGE_SIZE;

use crate::uefi::protocols::{
    FILE_MODE_READ, FILE_POSITION_END, FileProtocol, GRAPHICS_OUTPUT_GUID, GraphicsOutput,
    LOADED_IMAGE_GUID, LoadedImage, SIMPLE_FILE_SYSTEM_GUID, SimpleFileSystem,
};
use crate::uefi::tables::{AllocateType, BootServices, MemoryDescriptor, MemoryType, SystemTable};
use crate::uefi::{Guid, Handle, Status};

/// Longest path the loader will open, in UCS-2 units including the terminator.
const MAX_PATH: usize = 128;

/// Something went wrong before the kernel could be started.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BootError {
    /// What was being attempted.
    pub(crate) context: &'static str,
    /// What firmware said, if firmware was involved.
    pub(crate) status: Option<Status>,
}

impl BootError {
    /// An error firmware reported.
    pub(crate) const fn firmware(context: &'static str, status: Status) -> BootError {
        BootError {
            context,
            status: Some(status),
        }
    }

    /// An error the loader decided on its own.
    pub(crate) const fn plain(context: &'static str) -> BootError {
        BootError {
            context,
            status: None,
        }
    }
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "{} (EFI_STATUS {})", self.context, status.code()),
            None => f.write_str(self.context),
        }
    }
}

/// Shorthand for the loader's fallible operations.
pub(crate) type Result<T> = core::result::Result<T, BootError>;

/// Turn a status into a result.
fn check(context: &'static str, status: Status) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        Err(BootError::firmware(context, status))
    }
}

/// A region of physical memory the loader allocated.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Allocation {
    /// Physical address, page aligned.
    pub(crate) address: u64,
    /// Length in bytes, a multiple of [`PAGE_SIZE`].
    pub(crate) len: u64,
}

/// The firmware memory map, as handed over.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemoryMap {
    buffer: *const u8,
    size: usize,
    /// Firmware's stride between descriptors, which may exceed
    /// `size_of::<MemoryDescriptor>()`.
    descriptor_size: usize,
    /// The value `exit_boot_services` has to be given.
    pub(crate) key: usize,
}

impl MemoryMap {
    /// Walk the descriptors, using firmware's stride rather than ours.
    pub(crate) fn entries(&self) -> impl Iterator<Item = MemoryDescriptor> + Clone + '_ {
        // `checked_div` rather than a guard: a zero stride would mean
        // firmware reported no descriptor size, and dividing by it is the one
        // way this loop could fault.
        let count = self.size.checked_div(self.descriptor_size).unwrap_or(0);
        (0..count).map(move |index| {
            // SAFETY: `index` is below the descriptor count computed from the
            // size firmware reported, and the buffer is alive for `'_`.
            let at = unsafe { self.buffer.add(index * self.descriptor_size) };
            // SAFETY: firmware guarantees a descriptor at every multiple of
            // `descriptor_size`; it may be larger than ours but never smaller.
            unsafe { ptr::read_unaligned(at.cast::<MemoryDescriptor>()) }
        })
    }
}

/// The loader's handle on firmware.
#[derive(Clone, Copy)]
pub(crate) struct Services {
    image: Handle,
    system_table: *mut SystemTable,
    boot: *mut BootServices,
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Services")
    }
}

impl Services {
    /// Wrap the arguments firmware passed to `efi_main`.
    ///
    /// # Safety
    ///
    /// `image` and `system_table` must be the values firmware passed, and boot
    /// services must not have been exited.
    pub(crate) unsafe fn new(image: Handle, system_table: *mut SystemTable) -> Result<Services> {
        if system_table.is_null() {
            return Err(BootError::plain("firmware passed a null system table"));
        }
        // SAFETY: the caller guarantees this is firmware's own system table.
        let boot = unsafe { (*system_table).boot_services };
        if boot.is_null() {
            return Err(BootError::plain("system table has no boot services"));
        }
        Ok(Services {
            image,
            system_table,
            boot,
        })
    }

    /// The system table, for the fields the kernel is handed directly.
    pub(crate) const fn system_table(&self) -> *mut SystemTable {
        self.system_table
    }

    /// Borrow firmware's boot services table.
    ///
    /// Every call below goes through this rather than dereferencing
    /// `self.boot` inline. That is not a style choice: a UEFI call is a
    /// dereference *and* an indirect call through a function pointer, which is
    /// two unsafe operations, and the workspace requires one claim per block.
    /// Naming the dereference once here leaves each call site with exactly one
    /// thing to justify -- that it is calling the right service with arguments
    /// firmware may write through.
    fn boot(&self) -> &BootServices {
        // SAFETY: checked non-null in `new`, and firmware guarantees the table
        // is live until `exit_boot_services`, after which nothing in this
        // module is called again.
        unsafe { &*self.boot }
    }

    /// Borrow firmware's system table.
    fn table(&self) -> &SystemTable {
        // SAFETY: checked non-null in `new`; firmware owns it and it outlives
        // the loader.
        unsafe { &*self.system_table }
    }

    // -- memory ------------------------------------------------------------

    /// Allocate `len` bytes of physical memory, rounded up to whole pages.
    ///
    /// The `kind` is what firmware's memory map will report the region as, and
    /// the loader uses its own reserved types so the kernel learns what each
    /// allocation is for from the map itself.
    pub(crate) fn allocate(
        &self,
        context: &'static str,
        len: u64,
        kind: MemoryType,
    ) -> Result<Allocation> {
        let pages = len.div_ceil(PAGE_SIZE);
        if pages == 0 {
            return Err(BootError::plain(context));
        }
        let mut address = 0u64;
        // SAFETY: `address` is a live local, and `pages` is non-zero.
        let status = unsafe {
            (self.boot().allocate_pages)(
                AllocateType::ANY_PAGES,
                kind,
                pages as usize,
                &raw mut address,
            )
        };
        check(context, status)?;

        let allocation = Allocation {
            address,
            len: pages * PAGE_SIZE,
        };
        self.zero(allocation);
        Ok(allocation)
    }

    /// Fill an allocation with zeroes.
    ///
    /// Firmware does not promise fresh pages are clean, and a page table built
    /// on top of someone else's leftovers translates to wherever they pointed.
    fn zero(&self, allocation: Allocation) {
        // SAFETY: the range was just allocated to us, is identity mapped under
        // boot services, and nothing else refers to it.
        unsafe {
            ptr::write_bytes(allocation.address as *mut u8, 0, allocation.len as usize);
        }
    }

    /// Fetch the memory map into a buffer the loader already owns.
    ///
    /// The buffer has to be allocated in advance: asking firmware for memory is
    /// itself a change to the map, so a map fetched into a fresh allocation is
    /// stale before it is read.
    pub(crate) fn memory_map(&self, buffer: Allocation) -> Result<MemoryMap> {
        let mut size = buffer.len as usize;
        let mut key = 0usize;
        let mut descriptor_size = 0usize;
        let mut version = 0u32;

        // SAFETY: every out-parameter is a live local, and the buffer is ours.
        let status = unsafe {
            (self.boot().get_memory_map)(
                &raw mut size,
                buffer.address as *mut MemoryDescriptor,
                &raw mut key,
                &raw mut descriptor_size,
                &raw mut version,
            )
        };
        check("get_memory_map", status)?;

        Ok(MemoryMap {
            buffer: buffer.address as *const u8,
            size,
            descriptor_size,
            key,
        })
    }

    /// How many bytes the memory map currently needs, with slack.
    ///
    /// The slack matters: allocating the buffer adds entries to the map, so a
    /// buffer sized to today's map is too small by the time it is used.
    pub(crate) fn memory_map_size(&self) -> Result<u64> {
        let mut size = 0usize;
        let mut key = 0usize;
        let mut descriptor_size = 0usize;
        let mut version = 0u32;

        // SAFETY: a zero size asks firmware for the size it needs, which it
        // reports by returning BUFFER_TOO_SMALL and writing `size`.
        let status = unsafe {
            (self.boot().get_memory_map)(
                &raw mut size,
                ptr::null_mut(),
                &raw mut key,
                &raw mut descriptor_size,
                &raw mut version,
            )
        };
        if status != Status::BUFFER_TOO_SMALL && status.is_error() {
            return Err(BootError::firmware("sizing the memory map", status));
        }
        Ok(size as u64 + 8 * descriptor_size as u64)
    }

    // -- files -------------------------------------------------------------

    /// Read a file from the volume the loader was itself loaded from.
    ///
    /// Returns the allocation holding it and the file's real length, which is
    /// smaller than the allocation whenever the file is not a whole number of
    /// pages.
    pub(crate) fn read_file(&self, path: &str, kind: MemoryType) -> Result<(Allocation, u64)> {
        let root = self.open_volume()?;
        let file = match self.open_file(root, path) {
            Ok(file) => file,
            Err(error) => {
                // A missing file is an answer a caller may expect -- the
                // initramfs is optional -- so the volume handle must not
                // outlive it.
                //
                // SAFETY: firmware returned `root` from `open_volume`, so it is
                // a live `FileProtocol`.
                let volume = unsafe { &*root };
                // SAFETY: `root` is open and is closed exactly once, here.
                let _ = unsafe { (volume.close)(root) };
                return Err(error);
            }
        };

        let size = self.file_size(file)?;
        let allocation = self.allocate("allocating for a file", size.max(1), kind)?;

        // SAFETY: firmware returned this handle from `open`, so it is a live
        // `FileProtocol` until it is closed below.
        let handle = unsafe { &*file };

        let mut remaining = size as usize;
        let mut at = allocation.address as *mut u8;
        while remaining > 0 {
            let mut chunk = remaining;
            // SAFETY: `at` is inside our own allocation and `chunk` is what is
            // left of the file, so firmware writes only where it may.
            let status = unsafe { (handle.read)(file, &raw mut chunk, at) };
            check("reading a file", status)?;
            if chunk == 0 {
                return Err(BootError::plain("file ended before its stated length"));
            }
            remaining -= chunk;
            // SAFETY: `chunk` bytes were just read into the allocation, so
            // advancing by it stays inside it.
            at = unsafe { at.add(chunk) };
        }

        // SAFETY: `file` is open and is closed exactly once, here.
        let _ = unsafe { (handle.close)(file) };

        // SAFETY: firmware returned `root` from `open_volume`, so it is a live
        // `FileProtocol`.
        let volume = unsafe { &*root };
        // SAFETY: `root` is open and is closed exactly once, here.
        let _ = unsafe { (volume.close)(root) };

        Ok((allocation, size))
    }

    /// Where firmware loaded this program, and how many bytes of it there are.
    ///
    /// The switch to the loader's own tables fetches its next instruction at
    /// the address it was fetching from before them, so the loader's own code
    /// has to be mapped at its own address across it. On a machine whose RAM
    /// is above the split that mapping is these pages rather than all of RAM,
    /// so the loader has to know which pages are its own.
    pub(crate) fn image_range(&self) -> Result<(u64, u64)> {
        let image: *mut LoadedImage =
            self.protocol(self.image, &LOADED_IMAGE_GUID, "loaded image")?;
        // SAFETY: firmware returned a live `LoadedImage` for our own handle.
        let loaded = unsafe { &*image };
        Ok((loaded.image_base.addr() as u64, loaded.image_size))
    }

    /// Open the root directory of the loader's own volume.
    fn open_volume(&self) -> Result<*mut FileProtocol> {
        let image: *mut LoadedImage =
            self.protocol(self.image, &LOADED_IMAGE_GUID, "loaded image")?;
        // SAFETY: firmware returned a live `LoadedImage` for our own handle.
        let loaded = unsafe { &*image };
        let device = loaded.device_handle;

        let filesystem: *mut SimpleFileSystem =
            self.protocol(device, &SIMPLE_FILE_SYSTEM_GUID, "simple file system")?;
        // SAFETY: firmware returned a live `SimpleFileSystem` for that handle.
        let volume = unsafe { &*filesystem };

        let mut root = ptr::null_mut();
        // SAFETY: `root` is a live local for firmware to write the handle into.
        let status = unsafe { (volume.open_volume)(filesystem, &raw mut root) };
        check("opening the boot volume", status)?;
        Ok(root)
    }

    /// Open one file under `root`.
    fn open_file(&self, root: *mut FileProtocol, path: &str) -> Result<*mut FileProtocol> {
        let mut wide = [0u16; MAX_PATH];
        widen(path, &mut wide)?;

        let mut file = ptr::null_mut();
        // SAFETY: `root` is an open directory handle from `open_volume`.
        let directory = unsafe { &*root };
        // SAFETY: `wide` is NUL terminated by `widen`, and `file` is a live
        // local for firmware to write the new handle into.
        let status =
            unsafe { (directory.open)(root, &raw mut file, wide.as_ptr(), FILE_MODE_READ, 0) };
        check("opening a file", status)?;
        Ok(file)
    }

    /// The length of an open file.
    ///
    /// Found by seeking to the end and asking where that was, which avoids the
    /// `FileInfo` structure and its trailing variable-length name.
    fn file_size(&self, file: *mut FileProtocol) -> Result<u64> {
        // SAFETY: firmware returned `file` from `open`, so it is live.
        let handle = unsafe { &*file };

        // SAFETY: seeking takes only the handle and a position.
        let status = unsafe { (handle.set_position)(file, FILE_POSITION_END) };
        check("seeking to the end of a file", status)?;

        let mut size = 0u64;
        // SAFETY: `size` is a live local for firmware to write into.
        let status = unsafe { (handle.get_position)(file, &raw mut size) };
        check("reading a file position", status)?;

        // SAFETY: as the first call.
        let status = unsafe { (handle.set_position)(file, 0) };
        check("rewinding a file", status)?;

        Ok(size)
    }

    // -- protocols and tables ---------------------------------------------

    /// Fetch a protocol from a handle.
    fn protocol<T>(&self, handle: Handle, guid: &Guid, context: &'static str) -> Result<*mut T> {
        let mut interface: *mut c_void = ptr::null_mut();
        // SAFETY: `handle` came from firmware, `guid` is a live local constant,
        // and `interface` is a live local.
        let status = unsafe { (self.boot().handle_protocol)(handle, guid, &raw mut interface) };
        check(context, status)?;
        if interface.is_null() {
            return Err(BootError::plain(context));
        }
        Ok(interface.cast::<T>())
    }

    /// The framebuffer firmware left configured, if any.
    ///
    /// A machine with no graphics output is not an error: the serial console is
    /// the one the boot test reads.
    pub(crate) fn framebuffer(&self) -> Option<ferrix_bootinfo::Framebuffer> {
        let mut interface: *mut c_void = ptr::null_mut();
        // SAFETY: all three arguments are live locals or constants.
        let status = unsafe {
            (self.boot().locate_protocol)(
                &GRAPHICS_OUTPUT_GUID,
                ptr::null_mut(),
                &raw mut interface,
            )
        };
        if !status.is_success() || interface.is_null() {
            return None;
        }

        let graphics = interface.cast::<GraphicsOutput>();
        // SAFETY: firmware returned a live `GraphicsOutput`.
        let output = unsafe { &*graphics };
        if output.mode.is_null() {
            return None;
        }
        // SAFETY: checked non-null just above, and firmware owns it.
        let mode = unsafe { *output.mode };
        if mode.info.is_null() {
            return None;
        }
        // SAFETY: as above, for the mode's info block.
        let info = unsafe { *mode.info };

        use crate::uefi::protocols::GraphicsPixelFormat;
        let format = match info.pixel_format {
            GraphicsPixelFormat::BLUE_GREEN_RED_RESERVED => ferrix_bootinfo::PixelFormat::Bgrx8888,
            GraphicsPixelFormat::RED_GREEN_BLUE_RESERVED => ferrix_bootinfo::PixelFormat::Rgbx8888,
            // A bit-mask or blt-only mode is a framebuffer the kernel cannot
            // treat as an array of pixels, so report it as no framebuffer.
            _ => ferrix_bootinfo::PixelFormat::Unknown,
        };

        Some(ferrix_bootinfo::Framebuffer {
            phys: mode.framebuffer_base,
            size: mode.framebuffer_size as u64,
            width: info.horizontal_resolution,
            height: info.vertical_resolution,
            stride: info.pixels_per_scan_line,
            format,
        })
    }

    /// Look up a configuration table by GUID.
    pub(crate) fn configuration_table(&self, guid: &Guid) -> Option<u64> {
        let count = self.table().number_of_table_entries;
        let entries = self.table().configuration_table;
        if entries.is_null() {
            return None;
        }

        for index in 0..count {
            // SAFETY: `index` is below the count firmware reported, so this
            // stays inside the configuration table.
            let entry = unsafe { entries.add(index) };
            // SAFETY: firmware initialised every entry it counted.
            let entry = unsafe { *entry };
            if entry.vendor_guid == *guid {
                return Some(entry.vendor_table as u64);
            }
        }
        None
    }

    // -- the last call -----------------------------------------------------

    /// Leave boot services.
    ///
    /// After this returns, nothing in this module may be called again: the
    /// function pointers are still there, and the code behind them is not.
    pub(crate) fn exit_boot_services(&self, key: usize) -> Result<()> {
        // SAFETY: `key` came from the memory map fetched immediately before.
        let status = unsafe { (self.boot().exit_boot_services)(self.image, key) };
        check("exit_boot_services", status)
    }
}

/// Convert an ASCII path to NUL-terminated UCS-2, with UEFI's separators.
fn widen(path: &str, out: &mut [u16; MAX_PATH]) -> Result<()> {
    let mut used = 0;
    for byte in path.bytes() {
        if !byte.is_ascii() {
            return Err(BootError::plain("a boot path must be ASCII"));
        }
        // UEFI paths are backslash separated. Accepting both means the rest of
        // the loader can write ordinary paths.
        let byte = if byte == b'/' { b'\\' } else { byte };
        let Some(slot) = out.get_mut(used) else {
            return Err(BootError::plain("boot path is too long"));
        };
        *slot = u16::from(byte);
        used += 1;
    }
    let Some(slot) = out.get_mut(used) else {
        return Err(BootError::plain("boot path is too long"));
    };
    *slot = 0;
    Ok(())
}
