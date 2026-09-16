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
//! The calls that act on a process beyond making and starting it, in
//! `0x1032..=0x1037`. Those numbers do not decode yet, and answer `ENOSYS`.

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
use ferrix_native_abi::types::{
    CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, DEVICE_INFO_BYTES, DeviceInfo, MAP_READ, MAP_WRITE,
    PortPacket, ReadActual,
};
use ferrix_objects::message::Message;
use ferrix_objects::reach::Reach;
use ferrix_objects::table::TableError;
use ferrix_vma::VmaFlags;

use crate::block_ring;
use crate::device::DeviceNode;
use crate::net_ring;
use crate::object::channel::{self, ChannelMessage, Endpoint, ReadError, WriteFailure};
use crate::object::interrupt::{Interrupt, InterruptError};
use crate::object::io_mapping::{IoMapping, IoMappingError};
use crate::object::job::{self, Job};
use crate::object::port::{Observer, Port};
use crate::object::{self, HandleTable, Object};
use crate::syscall::SyscallArgs;
use crate::syscall::exec;
use crate::syscall::load::LoadError;
use crate::syscall::process::{self, Process, ProcessRef};
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
        NativeCall::ProcessCreate => process_create(
            process,
            handle(a[0]),
            handle(a[1]),
            Buffer {
                at: a[2],
                count: a[3],
            },
        ),
        NativeCall::ProcessStart => process_start(process, handle(a[0]), handle(a[1])),
        NativeCall::InterruptCreate => interrupt_create(process, handle(a[0]), a[1]),
        NativeCall::InterruptAck => interrupt_ack(process, handle(a[0])),
        NativeCall::InterruptBind => interrupt_bind(process, handle(a[0]), handle(a[1]), a[2]),
        NativeCall::IoMappingCreate => io_mapping_create(process, handle(a[0]), a[1]),
        NativeCall::BlockRingCreate => block_ring_create(process, handle(a[0])),
        NativeCall::NetRingCreate => net_ring_create(process, handle(a[0])),
        NativeCall::DisplayControlCreate => display_control_create(process, handle(a[0])),
        NativeCall::DeviceInfo => device_info(process, handle(a[0]), a[1]),
        NativeCall::DeviceQuiesce => device_quiesce(process, handle(a[0])),
        NativeCall::IoMappingMap => io_mapping_map(process, handle(a[0]), a[1]),
        NativeCall::VmoPin => vmo_pin(process, handle(a[0]), handle(a[1]), a[2], a[3], a[4]),
        NativeCall::VmoPinAddresses => vmo_pin_addresses(process, handle(a[0]), a[1], a[2]),
        NativeCall::PortCreate => insert_new(process, Object::Port(Port::new()), Rights::PORT),
        NativeCall::PortQueue => port_queue(process, handle(a[0]), a[1]),
        NativeCall::PortWait => port_wait(process, handle(a[0]), a[1], a[2]),
        NativeCall::ObjectWaitAsync => {
            object_wait_async(process, handle(a[0]), handle(a[1]), a[2], a[3])
        }
        NativeCall::VmoMap => vmo_map(process, handle(a[0]), a[1], a[2], a[3], a[4]),
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
    // never waits on it.
    //
    // And under the lock nothing may fault. Resolving a fault can copy a
    // copy-on-write page and then wait for every processor to drop the
    // translation it replaced, which no lock may be held across -- and a
    // reader's buffer is still shared copy-on-write after a `fork` until it is
    // first written. So a message delivered holding the lock is copied only
    // into pages already there to be written; when a buffer is not, the
    // message goes back, the lock goes, the buffers are faulted in, and the
    // read starts again. That ends unless another thread keeps undoing the
    // fault, as `AddressSpace::with_page`'s retry does.
    let mut topology = None;
    loop {
        let message = match endpoint.read(byte_capacity, handle_capacity, topology.is_some()) {
            Ok(message) => message,
            Err(ReadError::NeedsTopology) => {
                // Faulted in once before the lock, so that the usual message
                // -- small, into buffers the program has written -- is
                // delivered on the first round. Only as far as the end of the
                // byte buffer's first page, because the capacity can be far
                // larger than the message and faulting it all in would commit
                // memory nothing asked for. Best effort: a buffer this cannot
                // fault in is answered for when the message is delivered.
                let space = process.space();
                let first_page = usize::try_from(PAGE_SIZE - bytes.at % PAGE_SIZE).unwrap_or(0);
                let _ = uaccess::fault_in_for_write(space, bytes.at, byte_capacity.min(first_page));
                let _ = uaccess::fault_in_for_write(
                    space,
                    handles.at,
                    handle_capacity.saturating_mul(size_of::<u32>()),
                );
                let _ = uaccess::fault_in_for_write(space, actual, size_of::<ReadActual>());
                topology = Some(object::TOPOLOGY.lock());
                continue;
            }
            Err(ReadError::TooSmall { bytes, handles }) => {
                // Nothing was taken, so nothing needs the lock to go back.
                drop(topology);
                report_actual(process, actual, bytes, handles)?;
                return Err(status::BUFFER_TOO_SMALL);
            }
            Err(ReadError::Empty) => return Err(status::SHOULD_WAIT),
            Err(ReadError::PeerClosed) => return Err(status::PEER_CLOSED),
        };
        let through = if topology.is_some() {
            UserCopy::Present
        } else {
            UserCopy::Faulting
        };
        let byte_count = message.bytes.len();
        let handle_count = message.handles.len();
        let at = Destination {
            bytes: bytes.at,
            handles: handles.at,
            actual,
        };
        match deliver(process, &endpoint, message, at, through) {
            Ok(()) => return Ok(0),
            Err(Undelivered::Refused(why)) => return Err(why),
            Err(Undelivered::WouldFault) => {
                let _ = FAULTED_ROUNDS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                drop(topology.take());
                let space = process.space();
                uaccess::fault_in_for_write(space, bytes.at, byte_count).map_err(fault)?;
                uaccess::fault_in_for_write(
                    space,
                    handles.at,
                    handle_count.saturating_mul(size_of::<u32>()),
                )
                .map_err(fault)?;
                uaccess::fault_in_for_write(space, actual, size_of::<ReadActual>())
                    .map_err(fault)?;
            }
        }
    }
}

/// Deliveries under the topology lock that met a buffer page they could not
/// write in place, and went round again with the lock let go: counted for the
/// boot check, which has to see that path taken rather than assume it.
static FAULTED_ROUNDS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// How many times a `channel_read` has gone round again to fault a buffer in
/// with the topology lock let go: see [`FAULTED_ROUNDS`].
pub(crate) fn faulted_rounds() -> u64 {
    FAULTED_ROUNDS.load(core::sync::atomic::Ordering::Relaxed)
}

/// Where in the reader's memory a delivery goes.
#[derive(Debug, Clone, Copy)]
struct Destination {
    /// The message's bytes.
    bytes: u64,
    /// The new handles' values.
    handles: u64,
    /// The [`ReadActual`] saying how much of each arrived.
    actual: u64,
}

/// How a delivery may reach the reader's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserCopy {
    /// Faulting pages in as it goes, holding no lock.
    Faulting,
    /// Only through pages already there to be written, holding the topology
    /// lock.
    Present,
}

/// Why a message was not delivered. It has been put back, unless another
/// thread of the reader closed one of its new handles first.
#[derive(Debug)]
enum Undelivered {
    /// With this status for the program.
    Refused(Errno),
    /// A buffer page would first have had to be faulted in, which
    /// [`UserCopy::Present`] may not do.
    WouldFault,
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
    at: Destination,
    through: UserCopy,
) -> Result<(), Undelivered> {
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
            return Err(Undelivered::Refused(status::NO_HANDLES));
        }
    };

    let copied = put_user(process, at.bytes, &data, through)
        .and_then(|()| put_user(process, at.handles, &handle_bytes(&values), through))
        .and_then(|()| {
            put_user(
                process,
                at.actual,
                &actual_bytes(data.len(), values.len()),
                through,
            )
        });
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
    Ok(())
}

/// Copy `data` to `at` in the reader's memory, as `through` allows.
fn put_user(process: &Process, at: u64, data: &[u8], through: UserCopy) -> Result<(), Undelivered> {
    match through {
        UserCopy::Faulting => uaccess::copy_to_user(process.space(), at, data)
            .map_err(|why| Undelivered::Refused(fault(why))),
        UserCopy::Present => match uaccess::copy_to_user_present(process.space(), at, data) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Undelivered::WouldFault),
            Err(why) => Err(Undelivered::Refused(fault(why))),
        },
    }
}

/// A [`ReadActual`], as the bytes a program reads.
fn actual_bytes(bytes: usize, handles: usize) -> [u8; 8] {
    let actual = ReadActual {
        bytes: u32::try_from(bytes).unwrap_or(u32::MAX),
        handles: u32::try_from(handles).unwrap_or(u32::MAX),
    };
    let [b0, b1, b2, b3] = actual.bytes.to_ne_bytes();
    let [h0, h1, h2, h3] = actual.handles.to_ne_bytes();
    [b0, b1, b2, b3, h0, h1, h2, h3]
}

/// Write a [`ReadActual`] to `at`.
fn report_actual(process: &Process, at: u64, bytes: usize, handles: usize) -> Result<(), Errno> {
    uaccess::copy_to_user(process.space(), at, &actual_bytes(bytes, handles)).map_err(fault)
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
        || object.signals().intersects(wanted) || process.caller_must_leave(),
        deadline,
    );
    let observed = object.signals();
    object::dispose([object]);

    if observed_at != 0 {
        uaccess::copy_to_user(process.space(), observed_at, &observed.0.to_ne_bytes())
            .map_err(fault)?;
    }
    if process.caller_must_leave() {
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

/// The largest ELF image `process_create` reads out of a VMO: sixteen
/// mebibytes, copied into the kernel's own memory before it is loaded.
const MAX_IMAGE_BYTES: u64 = 16 << 20;

/// `process_create`.
///
/// The image is read out of the VMO into kernel memory and loaded from there,
/// so the new process's code is ordinary memory of its own and no VMO is ever
/// mapped executable. The process is in `job` before the caller hears of it,
/// so a kill of the job reaches a process that was never started.
fn process_create(
    process: &Process,
    job: Handle,
    image: Handle,
    name: Buffer,
) -> Result<usize, Errno> {
    let job = job_in(process, job, Rights::MANAGE)?;
    let vmo = process.with_handles(|table| vmo_in(table, image, Rights::READ))?;
    let name_len = usize::try_from(name.count)
        .ok()
        .filter(|&count| count <= nr::PROCESS_NAME_MAX)
        .ok_or(status::INVALID_ARGS)?;
    let name = copy_in(process, name.at, name_len)?;
    let image = image_bytes(&vmo)?;
    let child = exec::load_native(&image, &name).map_err(load_status)?;
    drop(image);
    if job.adopt(&child).is_err() {
        // A killed job takes nothing new. What was made is ended here, in the
        // caller's task, where a kill may run.
        process::kill(&child, job::KILLED_STATUS);
        return Err(status::BAD_STATE);
    }
    insert_new(
        process,
        Object::Process(ProcessRef::created(&child)),
        Rights::PROCESS,
    )
}

/// Every byte of a VMO, for `process_create` to load.
fn image_bytes(vmo: &Vmo) -> Result<Vec<u8>, Errno> {
    let len = vmo.len_bytes();
    if len > MAX_IMAGE_BYTES {
        return Err(status::TOO_BIG);
    }
    let len = usize::try_from(len).map_err(|_| status::TOO_BIG)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| status::NO_MEMORY)?;
    bytes.resize(len, 0);
    for (index, page) in bytes.chunks_mut(PAGE_SIZE as usize).enumerate() {
        vmo.read_page(index as u64, 0, page).map_err(vmo_error)?;
    }
    Ok(bytes)
}

/// The status a failed native load travels as: a fault in the image is the
/// caller's mistake, running out of memory is not.
fn load_status(error: exec::ExecError) -> Errno {
    match error {
        exec::ExecError::Load(LoadError::Space(SpaceError::OutOfMemory) | LoadError::Copy(_))
        | exec::ExecError::Space(_)
        | exec::ExecError::Startup
        | exec::ExecError::Start(_) => status::NO_MEMORY,
        exec::ExecError::Load(_) => status::INVALID_ARGS,
    }
}

/// `process_start`.
///
/// In the order that makes a race harmless and a failure clean:
/// 1. The start is claimed first, so a second start is refused before it has
///    moved anything.
/// 2. The task is made next, without running: everything about the start that
///    can fail, failing with nothing moved.
/// 3. The bootstrap is moved under both tables, so it is in exactly one of them
///    throughout.
/// 4. Only then is the task run, with the bootstrap's value in its first
///    argument register, which cannot fail.
///
/// A process that ended before the claim is refused by the claim. One killed
/// between the claim and the move has a closed table that refuses the handle,
/// which then never leaves the caller, and the prepared task is freed on the
/// way out, after both tables' locks are let go. The `Control` held here keeps
/// a last handle closed meanwhile from killing the process under a start that
/// is about to succeed.
fn process_start(process: &Process, target: Handle, bootstrap: Handle) -> Result<usize, Errno> {
    let control = process.with_handles(|table| {
        let (object, rights) = table.get(target).map_err(table_error)?;
        let Object::Process(child) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(Rights::MANAGE) {
            return Err(status::ACCESS_DENIED);
        }
        child.control().map(Arc::clone).ok_or(status::BAD_STATE)
    })?;
    let child = control.process().ok_or(status::BAD_STATE)?;
    let claim = process::claim_start(&child).map_err(|_| status::BAD_STATE)?;
    let prepared = claim.prepare(None).map_err(|_| status::NO_MEMORY)?;
    let placed = if bootstrap == Handle::default() {
        None
    } else {
        Some(move_handle(process, &child, bootstrap)?)
    };
    let argument = placed.map_or(0, |handle| u64::from(handle.0));
    let _task = prepared.start(argument);
    control.started();
    Ok(0)
}

/// Move `handle` out of `from`'s table into `to`'s, and answer its value there.
///
/// Both tables are held for the move, `from`'s first, so the object is in
/// exactly one of them at every moment. A refusal checked before the move
/// leaves the handle where it was, under the value it had. The one refusal
/// found after it, an insert into `to` that fails although room was checked,
/// puts the object back into `from` under whatever value that insert gives,
/// or frees it if even that fails. Moving into a child whose start is claimed,
/// whose task has not run, cannot meet the same two locks taken the other way
/// round. Needs `TRANSFER`.
fn move_handle(from: &Process, to: &Process, handle: Handle) -> Result<Handle, Errno> {
    let outcome = from.with_handles(|source| {
        let (_, rights) = source.get(handle).map_err(table_error)?;
        if !rights.contains(Rights::TRANSFER) {
            return Err(status::ACCESS_DENIED);
        }
        to.with_handles(|target| {
            if target.is_closed() {
                return Err(status::BAD_STATE);
            }
            if target.room() == 0 {
                return Err(status::NO_HANDLES);
            }
            let (object, rights) = source.remove(handle).map_err(table_error)?;
            // Room was there under this same lock, so the insert takes it; a
            // refusal anyway puts the object back rather than dropping it here.
            Ok(target
                .insert(object, rights)
                .map_err(|object| source.insert(object, rights)))
        })
    })?;
    match outcome {
        Ok(placed) => Ok(placed),
        Err(Ok(_)) => Err(status::NO_HANDLES),
        Err(Err(object)) => {
            object::dispose([object]);
            Err(status::NO_HANDLES)
        }
    }
}

/// The device node a handle names, if it carries `needed`.
fn device_in(process: &Process, device: Handle, needed: Rights) -> Result<Arc<DeviceNode>, Errno> {
    let node = process.with_handles(|table| {
        let (object, rights) = table.get(device).map_err(table_error)?;
        let Object::Device(node) = object else {
            return Err(status::WRONG_TYPE);
        };
        if !rights.contains(needed) {
            return Err(status::ACCESS_DENIED);
        }
        Ok(Arc::clone(node))
    })?;
    Ok(node)
}

/// `interrupt_create`.
///
/// The vector is the device's own by index, so a driver names "my second
/// interrupt" and cannot name a line its device does not have.
fn interrupt_create(process: &Process, device: Handle, index: u64) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::MANAGE)?;
    // Taking a device's interrupt is what its driver does, and devmgr and a
    // quiesce never do: noted, so that a check waiting for the driver's work
    // can say how the driver ended.
    crate::devmgr::note_driver(&node, process);
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
    // Mapping a device's registers, likewise (see `interrupt_create`).
    crate::devmgr::note_driver(&node, process);
    let phys = read_u64(process, spec)?;
    let len = read_u64(process, spec.checked_add(8).ok_or(status::FAULT)?)?;
    let aperture = node.aperture(phys, len).ok_or(status::ACCESS_DENIED)?;
    let mapping = IoMapping::new(aperture).map_err(|why| match why {
        IoMappingError::NotWholePages => status::INVALID_ARGS,
    })?;
    insert_new(process, Object::IoMapping(mapping), Rights::IO_MAPPING)
}

/// `block_ring_create`.
///
/// The device handle needs `MANAGE`, as everything that gives a driver the
/// device does. `ALREADY_BOUND` for a device that has a ring, live or ended:
/// nothing yet says its device was reset. `INVALID_ARGS` for a device that is
/// not a PCI function, since HELLO names the disk by its PCI location.
fn block_ring_create(process: &Process, device: Handle) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::MANAGE)?;
    let driver_end = block_ring::create(&node).map_err(|why| match why {
        block_ring::CreateError::InUse => status::ALREADY_BOUND,
        block_ring::CreateError::NotPci => status::INVALID_ARGS,
        block_ring::CreateError::NoMemory => status::NO_MEMORY,
    })?;
    insert_new(
        process,
        Object::Channel(driver_end),
        block_ring::CONTROL_RIGHTS,
    )
}

/// `net_ring_create`.
///
/// The same shape as `block_ring_create` and for the same reason: a ring is
/// made for a device the caller holds with `MANAGE`, and the driver's end of
/// its control channel comes back as a handle.
fn net_ring_create(process: &Process, device: Handle) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::MANAGE)?;
    let (_, driver_end) = net_ring::create(&node).map_err(|why| match why {
        net_ring::CreateError::InUse => status::ALREADY_BOUND,
        net_ring::CreateError::NoMemory => status::NO_MEMORY,
    })?;
    insert_new(
        process,
        Object::Channel(driver_end),
        ferrix_netring::control::CONTROL_RIGHTS,
    )
}

/// `display_control_create`.
///
/// The same shape as the rings': made for a device the caller holds with
/// `MANAGE`, the driver's end of the control channel coming back as a handle.
fn display_control_create(process: &Process, device: Handle) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::MANAGE)?;
    let driver_end = crate::display::create(&node).map_err(|why| match why {
        crate::display::CreateError::InUse => status::ALREADY_BOUND,
        crate::display::CreateError::NoMemory => status::NO_MEMORY,
    })?;
    insert_new(
        process,
        Object::Channel(driver_end),
        ferrix_blkring::control::CONTROL_RIGHTS,
    )
}

/// `device_info`.
///
/// Any device handle will do: what enumeration found is not a capability, and
/// whoever holds the device at all may know what it is.
fn device_info(process: &Process, device: Handle, at: u64) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::NONE)?;
    let info = node.describe();
    uaccess::copy_to_user(process.space(), at, &info_bytes(&info)).map_err(fault)?;
    Ok(0)
}

/// A `DeviceInfo` as its user buffer holds it: field by field, in the order
/// declared, padded to `DEVICE_INFO_BYTES`.
fn info_bytes(info: &DeviceInfo) -> [u8; DEVICE_INFO_BYTES] {
    let mut bytes = [0; DEVICE_INFO_BYTES];
    let mut at = 0;
    let mut put = |source: &[u8]| {
        if let Some(slot) = bytes.get_mut(at..at + source.len()) {
            slot.copy_from_slice(source);
        }
        at += source.len();
    };
    for block in [info.common, info.notify, info.isr, info.device] {
        put(&block.phys.to_ne_bytes());
        put(&block.offset.to_ne_bytes());
        put(&block.length.to_ne_bytes());
    }
    put(&info.location.to_ne_bytes());
    put(&info.class.to_ne_bytes());
    put(&info.apertures.to_ne_bytes());
    put(&info.vectors.to_ne_bytes());
    put(&info.notify_off_multiplier.to_ne_bytes());
    put(&info.vendor_id.to_ne_bytes());
    put(&info.device_id.to_ne_bytes());
    put(&info.msix_table_size.to_ne_bytes());
    put(&info.virtio.to_ne_bytes());
    bytes
}

/// `device_quiesce`.
///
/// The driver is gone and the device must reach nothing: bus mastering off,
/// then the block ring's claim on the device released for the next driver.
/// `BAD_STATE` while a driver still serves the device through a ring, or if
/// the device's configuration space could not be reached; `TIMED_OUT` when
/// the driver is gone but its ring has not ended within the patience.
fn device_quiesce(process: &Process, device: Handle) -> Result<usize, Errno> {
    let node = device_in(process, device, Rights::MANAGE)?;
    // A dead driver's ring may not have noticed the death yet: wait for it,
    // as long as the driver's end of its channel is closed.
    block_ring::wait_until_unserved(&node, &|| process.is_terminated()).map_err(|why| {
        match why {
            // A driver still holds its end: refused for good.
            block_ring::StillServed::ByADriver => status::BAD_STATE,
            // The driver is gone but the ring has not let go in time: worth
            // asking again, and devmgr does.
            block_ring::StillServed::Waiting => status::TIMED_OUT,
        }
    })?;
    node.disable_dma().map_err(|_| status::BAD_STATE)?;
    block_ring::release_claim(&node);
    Ok(0)
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
    // The first pin is what gives the device DMA, so it is where bus
    // mastering goes on; a device whose switch cannot be reached gets no pin.
    node.enable_dma().map_err(|_| status::BAD_STATE)?;
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
        .map_err(space_status)?;
    usize::try_from(mapped).map_err(|_| status::INVALID_ARGS)
}

/// `vmo_map`.
///
/// Always shared; [`crate::user::space::AddressSpace::map_object`] says why.
/// The protection needs the rights that grant it, so a read-only handle maps
/// read-only or not at all. Every mapping reads, because none of the
/// processors Ferrix runs on can make a user page writable and not readable.
fn vmo_map(
    process: &Process,
    vmo: Handle,
    address: u64,
    length: u64,
    protection: u64,
    offset_at: u64,
) -> Result<usize, Errno> {
    let write = match u32::try_from(protection) {
        Ok(MAP_READ) => false,
        Ok(bits) if bits == MAP_READ | MAP_WRITE => true,
        _ => return Err(status::INVALID_ARGS),
    };
    let needed = if write {
        Rights::MAP | Rights::READ | Rights::WRITE
    } else {
        Rights::MAP | Rights::READ
    };
    let vmo = process.with_handles(|table| vmo_in(table, vmo, needed))?;
    let offset = read_u64(process, offset_at)?;
    let flags = VmaFlags {
        write,
        ..VmaFlags::READ
    };
    let at = (address != 0).then_some(address);
    let mapped = process
        .space()
        .map_object(at, length, vmo, offset, flags)
        .map_err(space_status)?;
    usize::try_from(mapped).map_err(|_| status::INVALID_ARGS)
}

/// The status an address space's refusal travels as.
fn space_status(why: SpaceError) -> Errno {
    match why {
        SpaceError::OutOfMemory => status::NO_MEMORY,
        SpaceError::Refused(_) => status::ACCESS_DENIED,
        SpaceError::NotUserRange(_)
        | SpaceError::BadRange
        | SpaceError::NotMapped(_)
        | SpaceError::Backing(_)
        | SpaceError::PastEnd(_) => status::INVALID_ARGS,
    }
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
/// caller is killed, or another of its process's threads replaces the program,
/// for the reason `object_wait_one` gives.
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
            .wait_until_deadline(|| !port.is_empty() || process.caller_must_leave(), deadline);
        if process.caller_must_leave() {
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
