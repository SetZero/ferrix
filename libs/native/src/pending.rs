//! Calls whose handlers are not on main.
//!
//! Each wrapper makes the real call. A kernel without the handler answers
//! `ENOSYS`, which arrives as [`Error::Unsupported`], so a program built
//! today fails cleanly on today's kernel and works unchanged on the one that
//! implements the call.
//!
//! All of them are stage 9's, owned by ferrix-4b.
//!
//! * **Process creation**, `0x1030` and `0x1031`. The numbers were copied here
//!   while `libs/native-abi`'s table left `0x1030..=0x1037` free for them;
//!   they are on the table now and re-exported from it. The argument lists
//!   are the handler's: `(job, image_vmo, name_ptr, name_len)` and
//!   `(process, bootstrap or 0)`.
//! * **`vmo_map`**, `0x1024`, whose number is on main's table and whose handler
//!   and `MAP_READ`/`MAP_WRITE` constants are not. The constants are copied here
//!   until they land in `libs/native-abi`'s `types`.

use ferrix_native_abi::nr;
use ferrix_native_abi::signals::Signals;

use crate::call::{Call, Syscall};
use crate::error::{Error, decode, decode_handle, decode_unit};
use crate::handle::{Object, OwnedHandle, object_handle, register};
use crate::job::Job;
use crate::port::Port;
use crate::vmo::Vmo;

/// `process_create`: `(job, image_vmo, name_ptr, name_len)` → process handle.
pub use ferrix_native_abi::nr::PROCESS_CREATE;

/// `process_start`: `(process, bootstrap)`, the bootstrap a channel handle.
/// It leaves the caller, becomes the new process's first handle, and its value
/// arrives in the first argument register at `_start`, zero meaning none.
pub use ferrix_native_abi::nr::PROCESS_START;

object_handle!(
    /// A process, made by [`create_process`] and not necessarily started.
    Process
);

/// `process_create`: a process in `job`, from the ELF image in `elf`, named
/// `name`. Not started.
///
/// # Errors
///
/// [`Error::Unsupported`] on a kernel without the call; the rest are not yet
/// decided.
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

    /// Queue a packet carrying `key` on `port` when the process ends.
    ///
    /// No call of its own: `object_wait_async` for `TERMINATED`. The packet is
    /// a `PACKET_SIGNAL` whose signals include `TERMINATED`, and it arrives
    /// after the process's handles and descriptors are closed, or at once if
    /// the process has already ended.
    ///
    /// # Errors
    ///
    /// As [`Object::wait_async`].
    pub fn notify_on_exit(&self, port: &Port<S>, key: u64) -> Result<(), Error> {
        self.wait_async(port, Signals::TERMINATED, key)
    }
}

/// `vmo_map`'s protection bit for a readable mapping.
///
/// Owned by stage 9 (ferrix-4b), who adds it to `libs/native-abi`'s `types`
/// with the handler; copied here until then.
pub const MAP_READ: u32 = 1;

/// `vmo_map`'s protection bit for a writable mapping, only ever with
/// [`MAP_READ`].
///
/// Owned by stage 9 (ferrix-4b), as [`MAP_READ`].
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
    /// whole pages, at `at` or wherever the kernel finds room, and return the
    /// address.
    ///
    /// The mapping is always shared, outlives the handle, and refuses Linux's
    /// `mremap` and `mprotect`. Safe at a fixed address because the handler
    /// refuses a range overlapping any mapping: it can add memory to this
    /// process, never replace memory it has.
    ///
    /// # Errors
    ///
    /// [`Error::WrongType`] for a handle that is not a VMO;
    /// [`Error::AccessDenied`] without `MAP` and `READ`, or `WRITE` for
    /// [`Protection::ReadWrite`]; [`Error::InvalidArgs`] for a length or
    /// offset that is not whole pages, a range past the VMO's end, or an
    /// address overlapping a mapping; [`Error::NoMemory`] with no address
    /// space left; [`Error::Unsupported`] until the handler lands.
    pub fn map(
        &self,
        at: Option<usize>,
        length: usize,
        protection: Protection,
        offset: u64,
    ) -> Result<usize, Error> {
        let offset = offset.to_ne_bytes();
        let value = Call::new(nr::VMO_MAP)
            .value(register(self.handle()))
            .value(at.unwrap_or(0))
            .value(length)
            .value(protection.register())
            .input(&offset)
            .make(self.syscall());
        decode(value)
    }
}
