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
//! Mapping a VMO.
//! Their numbers decode, and answer `ENOSYS` until they exist.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::nr::{self, NativeCall};
use ferrix_native_abi::rights::{Requested, Rights};
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::status;
use ferrix_native_abi::types::{CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, PortPacket, ReadActual};
use ferrix_objects::message::Message;
use ferrix_objects::reach::Reach;
use ferrix_objects::table::TableError;

use crate::device::DeviceNode;
use crate::object::channel::{self, ChannelMessage, Endpoint, ReadError, WriteFailure};
use crate::object::interrupt::{Interrupt, InterruptError};
use crate::object::io_mapping::{IoMapping, IoMappingError};
use crate::object::job::{self, Job};
use crate::object::port::{Observer, Port};
use crate::object::{self, HandleTable, Object};
use crate::syscall::SyscallArgs;
use crate::syscall::process::Process;
use crate::syscall::uaccess::{self, UserError};
use crate::user::space::SpaceError;
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
        NativeCall::ObjectWaitOne => object_wait_one(process, handle(a[0]), a[1], a[2], a[3]),
        NativeCall::JobCreate => job_create(process, handle(a[0])),
        NativeCall::JobKill => job_kill(process, handle(a[0])),
        NativeCall::InterruptCreate => interrupt_create(process, handle(a[0]), a[1]),
        NativeCall::InterruptAck => interrupt_ack(process, handle(a[0])),
        NativeCall::InterruptBind => interrupt_bind(process, handle(a[0]), handle(a[1]), a[2]),
        NativeCall::IoMappingCreate => io_mapping_create(process, handle(a[0]), a[1]),
        NativeCall::IoMappingMap => io_mapping_map(process, handle(a[0]), a[1]),
        NativeCall::VmoPin => vmo_pin(process, handle(a[0]), handle(a[1]), a[2], a[3], a[4]),
        NativeCall::VmoPinAddresses => vmo_pin_addresses(process, handle(a[0]), a[1], a[2]),
        NativeCall::PortCreate => insert_new(process, Object::Port(Port::new()), Rights::PORT),
        NativeCall::PortQueue => port_queue(process, handle(a[0]), a[1]),
        NativeCall::PortWait => port_wait(process, handle(a[0]), a[1], a[2]),
        NativeCall::ObjectWaitAsync => {
            object_wait_async(process, handle(a[0]), handle(a[1]), a[2], a[3])
        }
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

    // The endpoints the message would carry, found first and let go of the
    // table again: the cycle check below takes the topology lock, which comes
    // before a handle table in the lock order, never inside one.
    let (writer, carried) = process.with_handles(|table| {
        let writer = channel_in(table, channel, Rights::WRITE)?;
        let carried = carried_endpoints(table, &values);
        refuse_the_writing_end(&writer, channel, &values, &carried)?;
        Ok::<_, Errno>((writer, carried))
    })?;

    // Held from the check to the push, so no other send can close a cycle in
    // between. Only a message carrying an endpoint takes it: nothing else can
    // add an edge to the graph it guards.
    let checked: Vec<*const Endpoint> = carried.iter().map(Arc::as_ptr).collect();
    let _topology = if carried.is_empty() {
        None
    } else {
        let guard = object::TOPOLOGY.lock();
        match channel::check_carry(&writer, carried) {
            Reach::Clear => {}
            Reach::Found => return Err(status::INVALID_ARGS),
            Reach::TooFar => return Err(status::TOO_BIG),
        }
        Some(guard)
    };

    process.with_handles(|table| {
        // Looked up again rather than trusting `writer`: the handle may have
        // been closed since, and a closed handle must not write.
        let endpoint = channel_in(table, channel, Rights::WRITE)?;
        // And what is taken has to be what was checked. Between the two
        // lookups another thread of this process can close a handle and be
        // issued a new one, and handle values are predictable: a value that
        // named nothing at the first lookup, and so was not checked, could
        // name the writing end itself at the second. So the writing end and
        // the endpoints the message carries are compared, and a message whose
        // handles changed underneath it is refused as try-again.
        let carried_now = carried_endpoints(table, &values);
        if !Arc::ptr_eq(&endpoint, &writer)
            || carried_now
                .iter()
                .map(Arc::as_ptr)
                .ne(checked.iter().copied())
        {
            return Err(status::SHOULD_WAIT);
        }
        refuse_the_writing_end(&endpoint, channel, &values, &carried_now)?;
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

/// Refuse a message carrying the end it is written through.
///
/// Zircon's rule, for the reason it has one: the handle a call is acting
/// through should not vanish half-way through the call. A duplicate of that
/// handle is the same end, so it is compared by object as well as by number.
/// The other end — the peer — needs no rule of its own: queued in its own
/// inbox it is the shortest cycle, and `channel::check_carry` refuses it with
/// the longer ones.
fn refuse_the_writing_end(
    writer: &Arc<Endpoint>,
    channel: Handle,
    values: &[Handle],
    carried: &[Arc<Endpoint>],
) -> Result<(), Errno> {
    if values.contains(&channel) || carried.iter().any(|other| Arc::ptr_eq(other, writer)) {
        return Err(status::INVALID_ARGS);
    }
    Ok(())
}

/// The channel endpoints among `values`.
///
/// A value that names nothing is skipped here; taking the handles refuses it
/// afterwards, with the status that says why.
fn carried_endpoints(table: &HandleTable, values: &[Handle]) -> Vec<Arc<Endpoint>> {
    values
        .iter()
        .filter_map(|&value| match table.get(value) {
            Ok((Object::Channel(endpoint), _)) => Some(Arc::clone(endpoint)),
            _ => None,
        })
        .collect()
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

    // A message carrying endpoints is taken only under the topology lock, and
    // the lock is held until it is delivered or put back: see
    // `Endpoint::read`. Everything else is read without it, so bulk traffic
    // never waits on it. At most twice round.
    let mut topology = None;
    let message = loop {
        match endpoint.read(byte_capacity, handle_capacity, topology.is_some()) {
            Ok(message) => break message,
            Err(ReadError::NeedsTopology) => topology = Some(object::TOPOLOGY.lock()),
            Err(ReadError::TooSmall { bytes, handles }) => {
                report_actual(process, actual, bytes, handles)?;
                return Err(status::BUFFER_TOO_SMALL);
            }
            Err(ReadError::Empty) => return Err(status::SHOULD_WAIT),
            Err(ReadError::PeerClosed) => return Err(status::PEER_CLOSED),
        }
    };
    let delivered = deliver(process, &endpoint, message, bytes.at, handles.at, actual);
    drop(topology);
    delivered
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
    insert_new(process, Object::Vmo(Vmo::new_anonymous(pages)), Rights::VMO)
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

/// Open a handle to a new object, or free the object if there is no room.
fn insert_new(process: &Process, object: Object, rights: Rights) -> Result<usize, Errno> {
    match process.with_handles(|table| table.insert(object, rights)) {
        Ok(handle) => Ok(returned(handle)),
        Err(object) => {
            object::dispose([object]);
            Err(status::NO_HANDLES)
        }
    }
}

/// `object_wait_one`.
///
/// Levels, not events: a signal already asserted ends the wait at once, which
/// is what lets a program look, find nothing, and wait, without losing a
/// message that arrived in between.
///
/// The wait also ends if the calling process is killed. `process::kill` wakes
/// the task, but a wait with a condition of its own would go back to sleep and
/// sleep out its deadline first; so the condition includes it, and the call
/// then answers `EINTR`, which no program sees — its task ends on the way back
/// to user mode.
fn object_wait_one(
    process: &Process,
    handle: Handle,
    signals: u64,
    deadline_at: u64,
    observed_at: u64,
) -> Result<usize, Errno> {
    let wanted = Signals::from_register(signals).ok_or(status::INVALID_ARGS)?;
    let object = process.with_handles(|table| {
        let (object, rights) = table.get(handle).map_err(table_error)?;
        if !rights.contains(Rights::WAIT) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(object.clone())
    })?;
    let deadline = if deadline_at == 0 {
        u64::MAX
    } else {
        read_u64(process, deadline_at)?
    };

    let satisfied = object.waiters().wait_until_deadline(
        || object.signals().intersects(wanted) || process.is_terminated(),
        deadline,
    );
    let observed = object.signals();
    object::dispose([object]);

    if observed_at != 0 {
        uaccess::copy_to_user(process.space(), observed_at, &observed.0.to_ne_bytes())
            .map_err(fault)?;
    }
    if process.is_terminated() {
        return Err(Errno::EINTR);
    }
    if satisfied {
        Ok(0)
    } else {
        Err(status::TIMED_OUT)
    }
}

/// The job a handle names, if it carries `needed`.
fn job_in(process: &Process, job: Handle, needed: Rights) -> Result<Arc<Job>, Errno> {
    process.with_handles(|table| {
        let (object, rights) = table.get(job).map_err(table_error)?;
        let Object::Job(job) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(needed) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(job))
    })
}

/// `job_create`.
fn job_create(process: &Process, parent: Handle) -> Result<usize, Errno> {
    let parent = job_in(process, parent, Rights::MANAGE)?;
    let child = parent.new_child().map_err(|_| status::BAD_STATE)?;
    insert_new(process, Object::Job(child), Rights::JOB)
}

/// `job_kill`.
///
/// Answers even when the caller is inside the job it kills: the kill ends the
/// caller's own process too, and its task stops on the way back to user mode
/// rather than here, with nothing held.
fn job_kill(process: &Process, job: Handle) -> Result<usize, Errno> {
    let job = job_in(process, job, Rights::MANAGE)?;
    let _ended = job.kill(job::KILLED_STATUS);
    Ok(0)
}

/// The device node a handle names, if it carries `needed`.
fn device_in(process: &Process, device: Handle, needed: Rights) -> Result<Arc<DeviceNode>, Errno> {
    process.with_handles(|table| {
        let (object, rights) = table.get(device).map_err(table_error)?;
        let Object::Device(node) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(needed) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(node))
    })
}

/// `interrupt_create`.
///
/// The vector is the device's own by index, so a driver names "my second
/// interrupt" and cannot name a line its device does not have.
fn interrupt_create(process: &Process, device: Handle, index: u64) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::MANAGE)?;
    let vector = usize::try_from(index)
        .ok()
        .and_then(|index| node.vector(index))
        .ok_or(status::INVALID_ARGS)?;
    let interrupt = Interrupt::new(vector).map_err(|why| match why {
        InterruptError::Taken | InterruptError::AlreadyBound => status::ALREADY_BOUND,
        InterruptError::NotMaskable => status::INVALID_ARGS,
    })?;
    insert_new(process, Object::Interrupt(interrupt), Rights::INTERRUPT)
}

/// `interrupt_ack`.
fn interrupt_ack(process: &Process, interrupt: Handle) -> Result<usize, Errno> {
    let interrupt = process.with_handles(|table| {
        let (object, rights) = table.get(interrupt).map_err(table_error)?;
        let Object::Interrupt(interrupt) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(Rights::MANAGE) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(interrupt))
    })?;
    interrupt.acknowledge().map_err(|_| status::INVALID_ARGS)?;
    object::dispose([Object::Interrupt(interrupt)]);
    Ok(0)
}

/// `io_mapping_create`.
///
/// `ACCESS_DENIED` for a range that is not inside one of the device's
/// apertures: that is memory this device does not have, and saying so is the
/// whole purpose of the call.
fn io_mapping_create(process: &Process, device: Handle, spec: u64) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::MANAGE)?;
    let phys = read_u64(process, spec)?;
    let len = read_u64(process, spec.checked_add(8).ok_or(status::FAULT)?)?;
    let aperture = node.aperture(phys, len).ok_or(status::ACCESS_DENIED)?;
    let mapping = IoMapping::new(aperture).map_err(|why| match why {
        IoMappingError::NotWholePages => status::INVALID_ARGS,
    })?;
    insert_new(process, Object::IoMapping(mapping), Rights::IO_MAPPING)
}

/// `vmo_pin`.
///
/// Whole pages of the VMO, held and pinned into the device's domain. The device
/// handle needs `MANAGE`, as minting its interrupts and mappings does. The VMO
/// needs `READ`, and `WRITE` unless the pin is read-only, since a device
/// writing a page is a write through the VMO.
fn vmo_pin(
    process: &Process,
    device: Handle,
    vmo: Handle,
    offset: u64,
    length: u64,
    options: u64,
) -> Result<usize, Errno> {
    let page = PAGE_SIZE;
    let known = ferrix_native_abi::types::PIN_READ_ONLY;
    if options & !known != 0
        || length == 0
        || !offset.is_multiple_of(page)
        || !length.is_multiple_of(page)
    {
        return Err(status::INVALID_ARGS);
    }
    let read_only = options & known != 0;
    let node = device_in(process, device, Rights::MANAGE)?;
    let vmo = process.with_handles(|table| {
        if !read_only {
            let _ = vmo_in(table, vmo, Rights::WRITE)?;
        }
        vmo_in(table, vmo, Rights::READ)
    })?;
    let held = vmo.hold(offset / page, length / page).map_err(vmo_error)?;
    let flags = if read_only {
        ferrix_paging::MapFlags::DMA_READ_ONLY
    } else {
        ferrix_paging::MapFlags::DMA
    };
    let pin = object::pin::Pin::new(node.domain(), held, flags).map_err(|why| {
        use crate::iommu::DomainError;
        match why {
            DomainError::Empty => status::INVALID_ARGS,
            DomainError::OutOfRange | DomainError::Tables => status::NO_MEMORY,
            DomainError::AlreadyPinned => status::ALREADY_BOUND,
            DomainError::Foreign | DomainError::Unit(_) => status::BAD_STATE,
        }
    })?;
    insert_new(process, Object::Pin(Arc::new(pin)), Rights::PIN)
}

/// `vmo_pin_addresses`. Writes up to `capacity` device addresses, and answers
/// how many pages the pin holds.
fn vmo_pin_addresses(
    process: &Process,
    pin: Handle,
    at: u64,
    capacity: u64,
) -> Result<usize, Errno> {
    let pin = process.with_handles(|table| {
        let (object, rights) = table.get(pin).map_err(table_error)?;
        let Object::Pin(pin) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(Rights::READ) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(pin))
    })?;
    let addresses = pin.addresses();
    let count =
        usize::try_from(capacity).map_or(addresses.len(), |capacity| capacity.min(addresses.len()));
    let bytes: Vec<u8> = addresses
        .iter()
        .take(count)
        .flat_map(|address| address.to_ne_bytes())
        .collect();
    let written = if bytes.is_empty() {
        Ok(())
    } else {
        uaccess::copy_to_user(process.space(), at, &bytes).map_err(fault)
    };
    let pages = addresses.len();
    object::dispose([Object::Pin(pin)]);
    written.map(|()| pages)
}

/// `io_mapping_map`. A zero address means wherever it fits.
fn io_mapping_map(process: &Process, mapping: Handle, address: u64) -> Result<usize, Errno> {
    let mapping = process.with_handles(|table| {
        let (object, rights) = table.get(mapping).map_err(table_error)?;
        let Object::IoMapping(mapping) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(Rights::MAP) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(mapping))
    })?;
    let at = (address != 0).then_some(address);
    let mapped = mapping
        .map_into(process.space(), at)
        .map_err(|why| match why {
            SpaceError::OutOfMemory => status::NO_MEMORY,
            SpaceError::Refused(_) => status::ACCESS_DENIED,
            SpaceError::NotUserRange(_)
            | SpaceError::BadRange
            | SpaceError::NotMapped(_)
            | SpaceError::Backing(_)
            | SpaceError::PastEnd(_) => status::INVALID_ARGS,
        })?;
    usize::try_from(mapped).map_err(|_| status::INVALID_ARGS)
}

/// The port a handle names, if it carries `needed`.
fn port_in(process: &Process, port: Handle, needed: Rights) -> Result<Arc<Port>, Errno> {
    process.with_handles(|table| {
        let (object, rights) = table.get(port).map_err(table_error)?;
        let Object::Port(port) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(needed) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(port))
    })
}

/// `port_queue`. The kind and signals the caller wrote are ignored: a program
/// queues user packets, and may not forge a signal packet.
fn port_queue(process: &Process, port: Handle, packet: u64) -> Result<usize, Errno> {
    let port = port_in(process, port, Rights::WRITE)?;
    let key = read_u64(process, packet)?;
    let first = read_u64(process, packet.checked_add(16).ok_or(status::FAULT)?)?;
    let second = read_u64(process, packet.checked_add(24).ok_or(status::FAULT)?)?;
    port.queue_user(key, [first, second])
        .map_err(|_| status::SHOULD_WAIT)?;
    Ok(0)
}

/// `port_wait`.
///
/// A packet taken and then not delivered, because the caller's buffer
/// faulted, is put back at the head of the queue. The wait also ends when the
/// caller is killed, for the reason `object_wait_one` gives.
fn port_wait(
    process: &Process,
    port: Handle,
    deadline_at: u64,
    packet_at: u64,
) -> Result<usize, Errno> {
    let port = port_in(process, port, Rights::READ)?;
    let deadline = if deadline_at == 0 {
        u64::MAX
    } else {
        read_u64(process, deadline_at)?
    };
    let packet = loop {
        let _ = port
            .waiters()
            .wait_until_deadline(|| !port.is_empty() || process.is_terminated(), deadline);
        if process.is_terminated() {
            return Err(Errno::EINTR);
        }
        // Another waiter on the same port may have taken the packet that
        // woke this one; that is a spurious wake-up, not a timeout.
        if let Some(packet) = port.take() {
            break packet;
        }
        if crate::timer::now_nanos() >= deadline {
            return Err(status::TIMED_OUT);
        }
    };
    if let Err(problem) = uaccess::copy_to_user(process.space(), packet_at, &packet_bytes(&packet))
    {
        port.put_back(packet);
        return Err(fault(problem));
    }
    Ok(0)
}

/// A packet as the ABI lays it out: key, kind, signals, two data words.
fn packet_bytes(packet: &PortPacket) -> [u8; 32] {
    let [first, second] = packet.data;
    let fields = packet
        .key
        .to_ne_bytes()
        .into_iter()
        .chain(packet.kind.to_ne_bytes())
        .chain(packet.signals.to_ne_bytes())
        .chain(first.to_ne_bytes())
        .chain(second.to_ne_bytes());
    let mut bytes = [0_u8; 32];
    for (slot, byte) in bytes.iter_mut().zip(fields) {
        *slot = byte;
    }
    bytes
}

/// `object_wait_async`.
///
/// Channels and jobs, whose signals change in task context under a lock the
/// registration can share. An interrupt reaches a port by being bound to
/// it, which is a call of its own; a VMO, a device node, an I/O mapping and a
/// port have no signal that changes, and are refused rather than accepted
/// into a registration that could never fire.
fn object_wait_async(
    process: &Process,
    watched: Handle,
    port: Handle,
    signals: u64,
    key_at: u64,
) -> Result<usize, Errno> {
    let wanted = Signals::from_register(signals).ok_or(status::INVALID_ARGS)?;
    if wanted == Signals::NONE || wanted.intersects(Signals::WRITABLE) {
        return Err(status::INVALID_ARGS);
    }
    let key = read_u64(process, key_at)?;
    let port = port_in(process, port, Rights::WRITE)?;
    let target = process.with_handles(|table| {
        let (object, rights) = table.get(watched).map_err(table_error)?;
        if !rights.contains(Rights::WAIT) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(object.clone())
    })?;

    let observer = Observer::new(&port, key, wanted);
    let registered = match &target {
        Object::Channel(endpoint) => Some(endpoint.observe(observer)),
        Object::Job(job) => Some(job.observe(observer)),
        Object::Process(process) => Some(process.exit().observe(observer)),
        Object::Vmo(_)
        | Object::Port(_)
        | Object::Device(_)
        | Object::Interrupt(_)
        | Object::IoMapping(_)
        | Object::Pin(_) => None,
    };
    object::dispose([target]);
    match registered {
        None => Err(status::WRONG_TYPE),
        Some(Ok(())) => Ok(0),
        Some(Err(_)) => Err(status::NO_MEMORY),
    }
}

/// The interrupt a handle names, if it carries `needed`.
fn interrupt_in(
    process: &Process,
    interrupt: Handle,
    needed: Rights,
) -> Result<Arc<Interrupt>, Errno> {
    process.with_handles(|table| {
        let (object, rights) = table.get(interrupt).map_err(table_error)?;
        let Object::Interrupt(interrupt) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(needed) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(interrupt))
    })
}

/// `interrupt_bind`.
fn interrupt_bind(
    process: &Process,
    interrupt: Handle,
    port: Handle,
    key_at: u64,
) -> Result<usize, Errno> {
    let key = read_u64(process, key_at)?;
    let port = port_in(process, port, Rights::WRITE)?;
    let interrupt = interrupt_in(process, interrupt, Rights::MANAGE)?;
    let bound = interrupt.bind(&port, key).map_err(|why| match why {
        InterruptError::AlreadyBound | InterruptError::Taken => status::ALREADY_BOUND,
        InterruptError::NotMaskable => status::INVALID_ARGS,
    });
    object::dispose([Object::Interrupt(interrupt)]);
    bound.map(|()| 0)
}
