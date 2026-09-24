//! Jobs: what a set of processes is killed as.

use ferrix_native_abi::nr;
use ferrix_native_abi::rights::Requested;

use crate::call::{Call, Syscall};
use crate::error::{Error, decode_handle, decode_unit};
use crate::handle::{Object, OwnedHandle, object_handle, register, rights_register};

object_handle!(
    /// A job.
    Job
);

/// `job_for_cgroup`: the job behind the cgroupfs directory the descriptor
/// `dirfd` is open on, with `rights`, which the caller's access to that
/// directory's `cgroup.procs` bounds (`docs/CGROUPS.md` §5). What a native
/// service manager waits on for [`crate::Signals::EMPTY`] in place of
/// `cgroup.events`.
///
/// # Errors
///
/// [`Error::BadHandle`] for a descriptor not open; [`Error::WrongType`] for
/// one that is not a cgroup directory; [`Error::AccessDenied`] for rights
/// the caller's access does not allow; [`Error::BadState`] for a removed
/// cgroup; [`Error::NoHandles`].
pub fn for_cgroup<S: Syscall>(sys: S, dirfd: i32, rights: Requested) -> Result<Job<S>, Error> {
    let value = Call::new(nr::JOB_FOR_CGROUP)
        .value(dirfd as u32 as usize)
        .value(rights_register(rights))
        .make(sys);
    let handle = decode_handle(value)?;
    Ok(Job::from_owned(OwnedHandle::from_raw(sys, handle)))
}

impl<S: Syscall> Job<S> {
    /// `job_create`: a job inside this one.
    ///
    /// # Errors
    ///
    /// [`Error::AccessDenied`] without `MANAGE`; [`Error::BadState`] once
    /// this job has been killed; [`Error::NoMemory`], [`Error::NoHandles`].
    pub fn create_child(&self) -> Result<Job<S>, Error> {
        let value = Call::new(nr::JOB_CREATE)
            .value(register(self.handle()))
            .make(self.syscall());
        let handle = decode_handle(value)?;
        Ok(Job::from_owned(OwnedHandle::from_raw(
            self.syscall(),
            handle,
        )))
    }

    /// `job_kill`: end every process in this job and every job inside it.
    ///
    /// # Errors
    ///
    /// [`Error::AccessDenied`] without `MANAGE`.
    pub fn kill(&self) -> Result<(), Error> {
        decode_unit(
            Call::new(nr::JOB_KILL)
                .value(register(self.handle()))
                .make(self.syscall()),
        )
    }
}
