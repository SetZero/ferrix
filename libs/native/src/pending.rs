//! Calls whose numbers are agreed and whose handlers are not on main.
//!
//! Each wrapper makes the real call. A kernel without the handler answers
//! `ENOSYS`, which arrives as [`Error::Unsupported`], so a program built
//! today fails cleanly on today's kernel and works unchanged on the one that
//! implements the call.
//!
//! **The numbers live here, not in `libs/native-abi`,** because they are not
//! on main's table: `0x1030..=0x1037` is held there for process creation and
//! `0x1025`/`0x1026` are unassigned. The owner of each call adds it to that
//! table with its handler; this module's constants then become re-exports of
//! those, and the tests below hold that the numbers agree.

use crate::call::{Call, Syscall};
use crate::device::Device;
use crate::error::{Error, decode, decode_handle, decode_unit};
use crate::handle::{Object, OwnedHandle, object_handle, register};
use crate::job::Job;
use crate::vmo::Vmo;

/// `process_create`: `(job, elf_vmo, name_ptr, name_len)` → process handle.
///
/// Owner: stage 9 (ferrix-2a), with native process creation.
pub const PROCESS_CREATE: usize = 0x1030;

/// `process_start`: `(process, bootstrap_handle)`. The bootstrap handle
/// leaves the caller, is installed in the new process as its first handle,
/// and its value is passed in the first argument register.
///
/// Owner: stage 9 (ferrix-2a), with native process creation.
pub const PROCESS_START: usize = 0x1031;

/// `vmo_pin`: pin a range of a VMO into a device's IOMMU domain, owned by a
/// `Pin` handle that unpins before it lets the frames go.
///
/// Owner: stage 10 (ferrix-8b), over `iommu::Domain` and stage 9's
/// `Vmo::hold`. The number and the meaning are agreed (`docs/ROADMAP.md`,
/// stage 10); the register layout below is this crate's proposal —
/// `(vmo, device, offset: *u64, length)` → pin handle, the offset through a
/// pointer as every VMO offset is — and it follows whatever the handler
/// decides.
pub const VMO_PIN: usize = 0x1025;

/// The pin's address query: write the pinned pages' device addresses.
///
/// Owner: stage 10 (ferrix-8b). Proposed layout: `(pin, addresses: *u64,
/// capacity)` → the number of pages, writing one device address per page up
/// to `capacity`.
pub const VMO_PIN_ADDRESSES: usize = 0x1026;

object_handle!(
    /// A process, made by [`create_process`] and not necessarily started.
    Process
);

object_handle!(
    /// Pages of a VMO pinned into a device's domain. Closing it unpins them.
    Pin
);

/// `process_create`: a process in `job`, from the ELF image in `elf`, named
/// `name`. Not started.
///
/// # Errors
///
/// [`Error::Unsupported`] on a kernel without the call; otherwise whatever
/// the handler decides.
pub fn create_process<S: Syscall>(
    job: &Job<S>,
    elf: &Vmo<S>,
    name: &str,
) -> Result<Process<S>, Error> {
    let value = Call::new(PROCESS_CREATE)
        .value(register(job.handle()))
        .value(register(elf.handle()))
        .input(name.as_bytes())
        .value(name.len())
        .make(job.syscall());
    let handle = decode_handle(value)?;
    Ok(Process::from_owned(OwnedHandle::from_raw(
        job.syscall(),
        handle,
    )))
}

impl<S: Syscall> Process<S> {
    /// `process_start`: run the process, giving it `bootstrap`.
    ///
    /// The handle leaves this process only if the call succeeds, so a failure
    /// gives it back.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] on a kernel without the call, with `bootstrap`.
    pub fn start(&self, bootstrap: OwnedHandle<S>) -> Result<(), (Error, OwnedHandle<S>)> {
        let value = Call::new(PROCESS_START)
            .value(register(self.handle()))
            .value(register(bootstrap.raw()))
            .make(self.syscall());
        match decode_unit(value) {
            Ok(()) => {
                let _ = bootstrap.into_raw();
                Ok(())
            }
            Err(error) => Err((error, bootstrap)),
        }
    }
}

impl<S: Syscall> Vmo<S> {
    /// `vmo_pin`: pin `length` bytes from `offset` into `device`'s domain.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] on a kernel without the call.
    pub fn pin(&self, device: &Device<S>, offset: u64, length: usize) -> Result<Pin<S>, Error> {
        let offset = offset.to_ne_bytes();
        let value = Call::new(VMO_PIN)
            .value(register(self.handle()))
            .value(register(device.handle()))
            .input(&offset)
            .value(length)
            .make(self.syscall());
        let handle = decode_handle(value)?;
        Ok(Pin::from_owned(OwnedHandle::from_raw(
            self.syscall(),
            handle,
        )))
    }
}

impl<S: Syscall> Pin<S> {
    /// The pinned pages' device addresses, one per page, into as much of
    /// `addresses` as one call carries ([`ADDRESS_CAPACITY`] words). Returns
    /// how many pages the pin holds, which may be more than were written.
    ///
    /// Written as bytes and read back as words, so the buffer's alignment is
    /// never assumed.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] on a kernel without the call.
    pub fn addresses(&self, addresses: &mut [u64]) -> Result<usize, Error> {
        let mut bytes = [0_u8; 8 * ADDRESS_CAPACITY];
        let capacity = addresses.len().min(ADDRESS_CAPACITY);
        let value = Call::new(VMO_PIN_ADDRESSES)
            .value(register(self.handle()))
            .output(bytes.get_mut(..capacity * 8).unwrap_or_default())
            .value(capacity)
            .make(self.syscall());
        let pages = decode(value)?;
        for (slot, word) in addresses
            .iter_mut()
            .zip(bytes.chunks_exact(8))
            .take(capacity)
        {
            if let Ok(word) = <[u8; 8]>::try_from(word) {
                *slot = u64::from_ne_bytes(word);
            }
        }
        Ok(pages)
    }
}

/// The most addresses one query returns: enough for a block ring and its data
/// pages, and a bound on the stack this wrapper uses.
pub const ADDRESS_CAPACITY: usize = 64;

/// `vmo_map`'s protection bit for a readable mapping.
///
/// Owner: stage 9 (ferrix-2a), who adds it to `libs/native-abi`'s `types`
/// with the handler; until then it is defined here.
pub const MAP_READ: u32 = 1;

/// `vmo_map`'s protection bit for a writable mapping, only ever with
/// [`MAP_READ`].
///
/// Owner: stage 9 (ferrix-2a), as [`MAP_READ`].
pub const MAP_WRITE: u32 = 2;

/// What a mapping of a VMO may do.
///
/// The only two the handler accepts, so no other can be asked for: every
/// mapping is readable, none is executable, and a writable one needs `WRITE`
/// on the handle as well as `READ` and `MAP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// `MAP_READ`.
    Read,
    /// `MAP_READ | MAP_WRITE`.
    ReadWrite,
}

impl Protection {
    /// The register value.
    #[must_use]
    pub const fn register(self) -> usize {
        match self {
            Protection::Read => MAP_READ as usize,
            Protection::ReadWrite => (MAP_READ | MAP_WRITE) as usize,
        }
    }
}

impl<S: Syscall> Vmo<S> {
    /// `vmo_map` (0x1024): map `length` bytes of the VMO from `offset`, both
    /// whole pages, wherever the kernel finds room, and return the address.
    ///
    /// The number is on main's table; the handler is not, and lands with
    /// process creation (stage 9, ferrix-2a), so today this answers
    /// [`Error::Unsupported`]. The mapping is always shared, outlives the
    /// handle, and refuses Linux's `mremap` and `mprotect`.
    ///
    /// Always at an address the kernel chooses, which cannot land on memory
    /// this process already has. The call accepts a fixed address too;
    /// offering that as a safe function waits on the handler saying it
    /// refuses an overlap, as `io_mapping_map`'s does.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgs`] for a length or offset that is not whole pages;
    /// [`Error::AccessDenied`] without `MAP` and `READ`, or `WRITE` for
    /// [`Protection::ReadWrite`]; [`Error::NoMemory`]; [`Error::Unsupported`].
    pub fn map(&self, length: usize, protection: Protection, offset: u64) -> Result<usize, Error> {
        let offset = offset.to_ne_bytes();
        let value = Call::new(ferrix_native_abi::nr::VMO_MAP)
            .value(register(self.handle()))
            .value(0)
            .value(length)
            .value(protection.register())
            .input(&offset)
            .make(self.syscall());
        decode(value)
    }
}
