//! The native system call handlers.
//!
//! Stage 9 of `docs/ROADMAP.md`. [`super::dispatch`] sends every number in
//! `0x1000..=0x1FFF` here before it asks any Linux table, so the two ABIs
//! never have to agree about a number. Each handler takes `&Process`, for the
//! reason [`super::process`] gives: the boot self-check drives them against
//! processes it built, long before a program can make a native call.
//!
//! # What is decided where
//!
//! Which handles are valid and what rights they carry is `libs/objects` and
//! `libs/native-abi`. What an object does is [`crate::object`]. This file does
//! what only a system call layer can: turn registers and user pointers into
//! those calls, and their failures into the status a program sees.
//!
//! # Objects are dropped outside the handle lock
//!
//! Every handler takes what it needs out of the table — an `Arc` of the
//! object, or objects by value — and lets the table's lock go before it acts
//! on them or drops them. An object's drop can free frames and drain a queue
//! of other objects, and none of that belongs under the lock another thread of
//! the same process needs to look a handle up. [`object::dispose`] is where
//! the dropping happens.
//!
//! # What is not here yet
//!
//! Ports and signal waits, jobs, interrupts, I/O mappings, and mapping a VMO.
//! Their numbers decode, and answer `ENOSYS` until they exist.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::nr::{self, NativeCall};
use ferrix_native_abi::rights::{Requested, Rights};
use ferrix_native_abi::status;
use ferrix_native_abi::types::{CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, ReadActual};
use ferrix_objects::message::Message;
use ferrix_objects::table::TableError;

use crate::object::channel::{ChannelMessage, Endpoint, ReadError, WriteFailure};
use crate::object::{self, HandleTable, Object};
use crate::syscall::SyscallArgs;
use crate::syscall::process::Process;
use crate::syscall::uaccess::{self, UserError};
use crate::user::vmo::{Vmo, VmoError};

/// The largest VMO a program may create, in pages: four gibibytes.
///
/// A reservation costs nothing until it is written, so this bounds what one
/// call can promise rather than what it costs. It is also what a 32-bit
/// caller's size register can express, so the limit is the same everywhere.
const MAX_VMO_PAGES: u64 = 1 << 20;

/// A handle's width in a user buffer.
const HANDLE_BYTES: usize = size_of::<u32>();

/// A pointer and a count, as two registers give them.
#[derive(Debug, Clone, Copy)]
struct Buffer {
    /// Where it starts in the caller's memory.
    at: u64,
    /// How many elements: bytes, or handles.
    count: u64,
}

/// Answer one native system call.
///
/// # Errors
///
/// `ENOSYS` for a gap in the range or a call not built yet; `ESRCH` with no
/// process, as the Linux half answers; otherwise the call's own status.
pub(crate) fn dispatch(args: &SyscallArgs, process: Option<&Process>) -> Result<usize, Errno> {
    let call = nr::decode(args.number).ok_or(Errno::ENOSYS)?;
    let process = process.ok_or(Errno::ESRCH)?;
    let a = args.args;
    match call {
        NativeCall::HandleClose => handle_close(process, handle(a[0])),
        NativeCall::HandleDuplicate => handle_duplicate(process, handle(a[0]), a[1]),
        NativeCall::HandleReplace => handle_replace(process, handle(a[0]), a[1]),
        NativeCall::ChannelCreate => channel_create(process, a[0]),
        NativeCall::ChannelWrite => channel_write(
            process,
            handle(a[0]),
            Buffer {
                at: a[1],
                count: a[2],
            },
            Buffer {
                at: a[3],
                count: a[4],
            },
        ),
        NativeCall::ChannelRead => channel_read(
            process,
            handle(a[0]),
            Buffer {
                at: a[1],
                count: a[2],
            },
            Buffer {
                at: a[3],
                count: a[4],
            },
            a[5],
        ),
        NativeCall::VmoCreate => vmo_create(process, a[0]),
        NativeCall::VmoRead => vmo_read(
            process,
            handle(a[0]),
            Buffer {
                at: a[1],
                count: a[2],
            },
            a[3],
        ),
        NativeCall::VmoWrite => vmo_write(
            process,
            handle(a[0]),
            Buffer {
                at: a[1],
                count: a[2],
            },
            a[3],
        ),
        NativeCall::VmoGetSize => vmo_get_size(process, handle(a[0]), a[1]),
        _ => Err(Errno::ENOSYS),
    }
}

/// A handle from a register.
fn handle(register: u64) -> Handle {
    Handle::from_register(register)
}

/// A new handle as a return value.
///
/// Positive on every architecture: `libs/objects` keeps every handle value
/// below 2^31, so a 32-bit return register never reads one as an error.
fn returned(handle: Handle) -> usize {
    handle.0 as usize
}

/// The status a table refusal travels as.
fn table_error(error: TableError) -> Errno {
    match error {
        TableError::BadHandle => status::BAD_HANDLE,
        TableError::AccessDenied => status::ACCESS_DENIED,
        TableError::Full => status::NO_HANDLES,
        TableError::Repeated => status::INVALID_ARGS,
    }
}

/// Every way a user copy fails is `EFAULT` to the program.
fn fault(_: UserError) -> Errno {
    status::FAULT
}

/// `handle_close`.
fn handle_close(process: &Process, handle: Handle) -> Result<usize, Errno> {
    let (object, _) = process
        .with_handles(|table| table.remove(handle))
        .map_err(table_error)?;
    object::dispose([object]);
    Ok(0)
}

/// `handle_duplicate`.
fn handle_duplicate(process: &Process, handle: Handle, rights: u64) -> Result<usize, Errno> {
    let requested = Requested::from_register(rights).ok_or(status::INVALID_ARGS)?;
    process
        .with_handles(|table| table.duplicate(handle, requested))
        .map(returned)
        .map_err(table_error)
}

/// `handle_replace`.
fn handle_replace(process: &Process, handle: Handle, rights: u64) -> Result<usize, Errno> {
    let requested = Requested::from_register(rights).ok_or(status::INVALID_ARGS)?;
    process
        .with_handles(|table| table.replace(handle, requested))
        .map(returned)
        .map_err(table_error)
}

/// `channel_create`.
fn channel_create(process: &Process, out: u64) -> Result<usize, Errno> {
    let (first, second) = Endpoint::pair().ok_or(status::NO_MEMORY)?;
    let ends = vec![
        (Object::Channel(first), Rights::CHANNEL),
        (Object::Channel(second), Rights::CHANNEL),
    ];
    let handles = match process.with_handles(|table| table.insert_many(ends)) {
        Ok(handles) => handles,
        Err(ends) => {
            object::dispose(ends.into_iter().map(|(object, _)| object));
            return Err(status::NO_HANDLES);
        }
    };
    if let Err(problem) = uaccess::copy_to_user(process.space(), out, &handle_bytes(&handles)) {
        // The program never learned the numbers, so nothing can close them
        // but this. Another thread may already have guessed one and closed
        // it, in which case the rest are that thread's to find.
        if let Ok(taken) = process.with_handles(|table| table.take_many(&handles, Rights::NONE)) {
            object::dispose(taken.into_iter().map(|(object, _)| object));
        }
        return Err(fault(problem));
    }
    Ok(0)
}

/// `channel_write`.
fn channel_write(
    process: &Process,
    channel: Handle,
    bytes: Buffer,
    handles: Buffer,
) -> Result<usize, Errno> {
    let byte_count = within(bytes.count, CHANNEL_MAX_BYTES)?;
    let handle_count = within(handles.count, CHANNEL_MAX_HANDLES)?;
    let data = copy_in(process, bytes.at, byte_count)?;
    let values = copy_in_handles(process, handles.at, handle_count)?;

    process.with_handles(|table| {
        let endpoint = channel_in(table, channel, Rights::WRITE)?;
        refuse_own_ends(table, &endpoint, channel, &values)?;
        endpoint
            .write(data, values.len(), || {
                table.take_many(&values, Rights::TRANSFER)
            })
            .map_err(|failure| match failure {
                WriteFailure::PeerClosed => status::PEER_CLOSED,
                WriteFailure::TooBig => status::TOO_BIG,
                WriteFailure::Full => status::SHOULD_WAIT,
                WriteFailure::Take(error) => table_error(error),
            })
    })?;
    Ok(0)
}

/// Refuse a message carrying either end of the channel it is written to.
///
/// The peer is a cycle: queued in its own inbox, it holds itself alive, and
/// no close can ever free it. The writing end itself is Zircon's rule and the
/// simpler one to reason about — the handle the call is acting through does
/// not vanish half-way through the call. Longer cycles, two endpoints each
/// queued in the other, are still possible and are recorded as debt in the
/// roadmap rather than hidden.
fn refuse_own_ends(
    table: &HandleTable,
    endpoint: &Arc<Endpoint>,
    channel: Handle,
    values: &[Handle],
) -> Result<(), Errno> {
    for &value in values {
        if value == channel {
            return Err(status::INVALID_ARGS);
        }
        if let Ok((Object::Channel(other), _)) = table.get(value)
            && (Arc::ptr_eq(other, endpoint) || endpoint.is_peer(other))
        {
            return Err(status::INVALID_ARGS);
        }
    }
    Ok(())
}

/// `channel_read`.
fn channel_read(
    process: &Process,
    channel: Handle,
    bytes: Buffer,
    handles: Buffer,
    actual: u64,
) -> Result<usize, Errno> {
    let byte_capacity = capacity(bytes.count, CHANNEL_MAX_BYTES);
    let handle_capacity = capacity(handles.count, CHANNEL_MAX_HANDLES);
    let endpoint = process.with_handles(|table| channel_in(table, channel, Rights::READ))?;

    let message = match endpoint.read(byte_capacity, handle_capacity) {
        Ok(message) => message,
        Err(ReadError::TooSmall { bytes, handles }) => {
            report_actual(process, actual, bytes, handles)?;
            return Err(status::BUFFER_TOO_SMALL);
        }
        Err(ReadError::Empty) => return Err(status::SHOULD_WAIT),
        Err(ReadError::PeerClosed) => return Err(status::PEER_CLOSED),
    };
    deliver(process, &endpoint, message, bytes.at, handles.at, actual)
}

/// Put a message's handles in the reader's table and its bytes in the
/// reader's memory, or put the message back.
///
/// Handles first, because a full table is the failure a program can do
/// something about, and finding it after the bytes were copied would mean
/// taking back a delivery the program might already be reading.
fn deliver(
    process: &Process,
    endpoint: &Endpoint,
    message: ChannelMessage,
    bytes_at: u64,
    handles_at: u64,
    actual: u64,
) -> Result<usize, Errno> {
    let Message {
        bytes: data,
        handles: transfers,
    } = message;
    let values = match process.with_handles(|table| table.insert_many(transfers)) {
        Ok(values) => values,
        Err(transfers) => {
            endpoint.unread(Message {
                bytes: data,
                handles: transfers,
            });
            return Err(status::NO_HANDLES);
        }
    };

    let copied = copy_out(process, &data, bytes_at, &values, handles_at)
        .and_then(|()| report_actual(process, actual, data.len(), values.len()));
    if let Err(problem) = copied {
        // Taken back and requeued, so a bad buffer loses nothing. If another
        // thread of this process has already closed one of the new handles,
        // the rest stay where they are: that thread has seen them.
        if let Ok(transfers) = process.with_handles(|table| table.take_many(&values, Rights::NONE))
        {
            endpoint.unread(Message {
                bytes: data,
                handles: transfers,
            });
        }
        return Err(problem);
    }
    Ok(0)
}

/// A message's bytes and handle values, into the reader's buffers.
fn copy_out(
    process: &Process,
    data: &[u8],
    bytes_at: u64,
    values: &[Handle],
    handles_at: u64,
) -> Result<(), Errno> {
    if !data.is_empty() {
        uaccess::copy_to_user(process.space(), bytes_at, data).map_err(fault)?;
    }
    if !values.is_empty() {
        uaccess::copy_to_user(process.space(), handles_at, &handle_bytes(values)).map_err(fault)?;
    }
    Ok(())
}

/// Write a [`ReadActual`] to `at`.
fn report_actual(process: &Process, at: u64, bytes: usize, handles: usize) -> Result<(), Errno> {
    let actual = ReadActual {
        bytes: u32::try_from(bytes).unwrap_or(u32::MAX),
        handles: u32::try_from(handles).unwrap_or(u32::MAX),
    };
    let [b0, b1, b2, b3] = actual.bytes.to_ne_bytes();
    let [h0, h1, h2, h3] = actual.handles.to_ne_bytes();
    uaccess::copy_to_user(process.space(), at, &[b0, b1, b2, b3, h0, h1, h2, h3]).map_err(fault)
}

/// The channel endpoint a handle names, if it carries `needed`.
///
/// The type is checked before the rights, so a VMO handle passed where a
/// channel belongs is `WRONG_TYPE` whatever rights it carries: the more
/// useful of the two answers to whoever made the mistake.
fn channel_in(
    table: &HandleTable,
    channel: Handle,
    needed: Rights,
) -> Result<Arc<Endpoint>, Errno> {
    let (object, rights) = table.get(channel).map_err(table_error)?;
    let Object::Channel(endpoint) = object else {
        return Err(status::WRONG_TYPE);
    };
    if !rights.contains(needed) {
        return Err(status::ACCESS_DENIED);
    }
    Ok(Arc::clone(endpoint))
}

/// The VMO a handle names, if it carries `needed`.
fn vmo_in(table: &HandleTable, vmo: Handle, needed: Rights) -> Result<Arc<Vmo>, Errno> {
    let (object, rights) = table.get(vmo).map_err(table_error)?;
    let Object::Vmo(vmo) = object else {
        return Err(status::WRONG_TYPE);
    };
    if !rights.contains(needed) {
        return Err(status::ACCESS_DENIED);
    }
    Ok(Arc::clone(vmo))
}

/// A count from a register, if it is at most `max`.
fn within(count: u64, max: usize) -> Result<usize, Errno> {
    usize::try_from(count)
        .ok()
        .filter(|&count| count <= max)
        .ok_or(status::TOO_BIG)
}

/// A capacity from a register, clamped to what any message can need.
///
/// Clamped rather than refused: offering more room than a message can use is
/// not a mistake, and it bounds nothing this side allocates.
fn capacity(count: u64, max: usize) -> usize {
    usize::try_from(count).map_or(max, |count| count.min(max))
}

/// `count` bytes from the caller's memory.
fn copy_in(process: &Process, at: u64, count: usize) -> Result<Vec<u8>, Errno> {
    let mut data = vec![0_u8; count];
    if count > 0 {
        uaccess::copy_from_user(process.space(), at, &mut data).map_err(fault)?;
    }
    Ok(data)
}

/// `count` handle values from the caller's memory.
fn copy_in_handles(process: &Process, at: u64, count: usize) -> Result<Vec<Handle>, Errno> {
    let bytes = copy_in(process, at, count * HANDLE_BYTES)?;
    bytes
        .chunks_exact(HANDLE_BYTES)
        .map(|word| {
            <[u8; HANDLE_BYTES]>::try_from(word).map(|word| Handle(u32::from_ne_bytes(word)))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| status::INVALID_ARGS)
}

/// Handle values as a user buffer holds them.
fn handle_bytes(handles: &[Handle]) -> Vec<u8> {
    handles
        .iter()
        .flat_map(|handle| handle.0.to_ne_bytes())
        .collect()
}

/// A 64-bit value read through a pointer argument.
fn read_u64(process: &Process, at: u64) -> Result<u64, Errno> {
    let mut word = [0_u8; 8];
    uaccess::copy_from_user(process.space(), at, &mut word).map_err(fault)?;
    Ok(u64::from_ne_bytes(word))
}

/// `vmo_create`.
fn vmo_create(process: &Process, bytes: u64) -> Result<usize, Errno> {
    let pages = bytes.div_ceil(PAGE_SIZE);
    if pages > MAX_VMO_PAGES {
        return Err(status::NO_MEMORY);
    }
    let vmo = Object::Vmo(Vmo::new_anonymous(pages));
    match process.with_handles(|table| table.insert(vmo, Rights::VMO)) {
        Ok(handle) => Ok(returned(handle)),
        Err(vmo) => {
            object::dispose([vmo]);
            Err(status::NO_HANDLES)
        }
    }
}

/// Which way a VMO copy goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Out of the VMO, into the caller's buffer.
    Read,
    /// Out of the caller's buffer, into the VMO.
    Write,
}

/// `vmo_read`.
fn vmo_read(process: &Process, vmo: Handle, buffer: Buffer, offset: u64) -> Result<usize, Errno> {
    vmo_copy(process, vmo, buffer, offset, Direction::Read)
}

/// `vmo_write`.
fn vmo_write(process: &Process, vmo: Handle, buffer: Buffer, offset: u64) -> Result<usize, Errno> {
    vmo_copy(process, vmo, buffer, offset, Direction::Write)
}

/// Copy between a VMO and the caller's memory, a page at a time.
///
/// Through a kernel page of scratch rather than frame to user page directly,
/// so the VMO's lock is never held across a user copy — which can fault a
/// page in, and take the address space's lock to do it.
fn vmo_copy(
    process: &Process,
    vmo: Handle,
    buffer: Buffer,
    offset: u64,
    direction: Direction,
) -> Result<usize, Errno> {
    let needed = match direction {
        Direction::Read => Rights::READ,
        Direction::Write => Rights::WRITE,
    };
    let vmo = process.with_handles(|table| vmo_in(table, vmo, needed))?;
    let offset = read_u64(process, offset)?;
    let end = offset
        .checked_add(buffer.count)
        .ok_or(status::INVALID_ARGS)?;
    if end > vmo.len_bytes() || buffer.at.checked_add(buffer.count).is_none() {
        return Err(status::INVALID_ARGS);
    }

    let mut scratch = vec![0_u8; PAGE_SIZE as usize];
    let mut done = 0_u64;
    while done < buffer.count {
        let at = offset + done;
        let within = (at % PAGE_SIZE) as usize;
        let chunk = (PAGE_SIZE - at % PAGE_SIZE).min(buffer.count - done) as usize;
        let slot = scratch.get_mut(..chunk).ok_or(status::INVALID_ARGS)?;
        let user = buffer.at + done;
        match direction {
            Direction::Read => {
                vmo.read_page(at / PAGE_SIZE, within, slot)
                    .map_err(vmo_error)?;
                uaccess::copy_to_user(process.space(), user, slot).map_err(fault)?;
            }
            Direction::Write => {
                uaccess::copy_from_user(process.space(), user, slot).map_err(fault)?;
                vmo.write_page(at / PAGE_SIZE, within, slot)
                    .map_err(vmo_error)?;
            }
        }
        done += chunk as u64;
    }
    Ok(0)
}

/// The status a VMO refusal travels as.
fn vmo_error(error: VmoError) -> Errno {
    match error {
        VmoError::OutOfRange { .. } => status::INVALID_ARGS,
        VmoError::OutOfMemory => status::NO_MEMORY,
    }
}

/// `vmo_get_size`.
fn vmo_get_size(process: &Process, vmo: Handle, out: u64) -> Result<usize, Errno> {
    let vmo = process.with_handles(|table| vmo_in(table, vmo, Rights::NONE))?;
    uaccess::copy_to_user(process.space(), out, &vmo.len_bytes().to_ne_bytes()).map_err(fault)?;
    Ok(0)
}
