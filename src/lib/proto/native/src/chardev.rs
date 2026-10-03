//! The chardev core's driver calls (`docs/NVIDIA.md` §4.4): answering the
//! requests the kernel forwards on a control channel made with
//! [`crate::device::Device::chardev_control`], and reaching the memory and
//! descriptors of the program waiting in one.
//!
//! Each names the control channel and a request id from a REQUEST
//! (`ferrix_chardevctl::message`), and works only while that request is
//! outstanding: once answered or abandoned, the id names nothing.

use ferrix_native_abi::nr;

use crate::call::{Call, Syscall};
use crate::channel::Channel;
use crate::error::{Error, decode, decode_unit};
use crate::handle::{Object, register};

/// `chardev_reply`: answer `request` with `status` (zero or a negative
/// errno) and `value` (an ioctl's return).
///
/// # Errors
///
/// [`Error::BadState`] for a request not outstanding on this control,
/// [`Error::InvalidArgs`] for a status that is neither, and
/// [`Error::WrongType`] for a channel that is not a chardev control.
pub fn reply<S: Syscall>(
    control: &Channel<S>,
    request: u64,
    status: i32,
    value: i64,
) -> Result<(), Error> {
    let value = Call::new(nr::CHARDEV_REPLY)
        .value(register(control.handle()))
        .value(request as usize)
        .value(status as isize as usize)
        .value(value as usize)
        .make(control.syscall());
    decode_unit(value)
}

/// `chardev_copy_in`: fill `buffer` from the waiting program's memory at
/// `client`.
///
/// # Errors
///
/// As [`reply`], and [`Error::Fault`] for memory the program cannot read.
pub fn copy_in<S: Syscall>(
    control: &Channel<S>,
    request: u64,
    client: u64,
    buffer: &mut [u8],
) -> Result<(), Error> {
    let length = buffer.len();
    let value = Call::new(nr::CHARDEV_COPY_IN)
        .value(register(control.handle()))
        .value(request as usize)
        .value(client as usize)
        .output(buffer)
        .value(length)
        .make(control.syscall());
    decode_unit(value)
}

/// `chardev_copy_out`: write `buffer` into the waiting program's memory at
/// `client`.
///
/// # Errors
///
/// As [`copy_in`].
pub fn copy_out<S: Syscall>(
    control: &Channel<S>,
    request: u64,
    client: u64,
    buffer: &[u8],
) -> Result<(), Error> {
    let length = buffer.len();
    let value = Call::new(nr::CHARDEV_COPY_OUT)
        .value(register(control.handle()))
        .value(request as usize)
        .value(client as usize)
        .input(buffer)
        .value(length)
        .make(control.syscall());
    decode_unit(value)
}

/// `chardev_file`: the identity, on this control, of the file the waiting
/// program's descriptor `fd` names.
///
/// # Errors
///
/// As [`reply`], and [`Error::BadHandle`] for a descriptor that is not one
/// of this control's files.
pub fn file<S: Syscall>(control: &Channel<S>, request: u64, fd: i32) -> Result<u64, Error> {
    let value = Call::new(nr::CHARDEV_FILE)
        .value(register(control.handle()))
        .value(request as usize)
        .value(fd as isize as usize)
        .make(control.syscall());
    decode(value).map(|file| file as u64)
}
