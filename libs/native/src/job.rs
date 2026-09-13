//! Jobs: what a set of processes is killed as.

use ferrix_native_abi::nr;

use crate::call::{Call, Syscall};
use crate::error::{Error, decode_handle, decode_unit};
use crate::handle::{Object, OwnedHandle, object_handle, register};

object_handle!(
    /// A job.
    Job
);

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
