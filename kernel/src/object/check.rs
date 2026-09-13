//! Stage 9's self-checks: two processes, a channel between them, and the
//! rules a capability system lives by.
//!
//! The host tests in `libs/objects` prove the handle table and the queue in
//! isolation. What they cannot prove is the join: that a handle written by one
//! process really leaves its table and arrives in another's naming the *same*
//! object, that a system call refused half-way leaves both tables as they
//! were, and that the objects a closed channel was still holding give their
//! memory back. Those are properties of the kernel's handlers over real
//! address spaces, and nothing short of running them can show them.
//!
//! Every call goes through [`native::dispatch`] with raw registers, so the
//! number decoding and the argument order are exercised as a program will
//! exercise them — not the handler functions called by name.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::sync::SpinLock;
use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::nr;
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::status;
use ferrix_native_abi::types::{CHANNEL_MAX_BYTES, PACKET_INTERRUPT, PACKET_SIGNAL, PACKET_USER};
use ferrix_vma::VmaFlags;

use crate::arch;
use crate::device::{self, DeviceNode};
use crate::mm;
use crate::object::channel::Endpoint;
use crate::object::interrupt;
use crate::object::job::{Job, KILLED_STATUS};
use crate::object::{self, Object};
use crate::sched::Task;
use crate::syscall::check::spinner;
use crate::syscall::image;
use crate::syscall::process::{self, Process};
use crate::syscall::{self as linux, Outcome, SyscallArgs, native, uaccess};
use crate::user::space::Access;
use ferrix_elf::Class;

/// Where each check process keeps its buffers.
const SCRATCH: u64 = 0x4000_0000;
/// Two pages of it.
const SCRATCH_LEN: u64 = 2 * PAGE_SIZE;
/// Bytes being sent, and bytes being written into a VMO.
const PAYLOAD: u64 = SCRATCH;
/// Bytes received.
const INBOX: u64 = SCRATCH + 0x100;
/// Handle values, in either direction.
const HANDLES: u64 = SCRATCH + 0x200;
/// `channel_create`'s two handles.
const PAIR: u64 = SCRATCH + 0x300;
/// A `ReadActual`.
const ACTUAL: u64 = SCRATCH + 0x308;
/// A VMO offset.
const OFFSET: u64 = SCRATCH + 0x310;
/// A VMO size.
const SIZE: u64 = SCRATCH + 0x318;
/// A wait's deadline.
const DEADLINE: u64 = SCRATCH + 0x320;
/// The signals a wait observed.
const OBSERVED: u64 = SCRATCH + 0x328;
/// An `IoMappingSpec`.
const SPEC: u64 = SCRATCH + 0x330;
/// A `PortPacket`.
const PACKET_AT: u64 = SCRATCH + 0x340;
/// A registration's key.
const KEY: u64 = SCRATCH + 0x360;
/// A pin's device addresses.
const PINNED_AT: u64 = SCRATCH + 0x370;

/// How long the waker sleeps before it writes.
const WAKE_AFTER_NANOS: u64 = 20_000_000;
/// How long a check waits for anything before calling it lost.
const PATIENCE_NANOS: u64 = 120_000_000_000;
/// How long the spinning programs run before a job is killed under them.
const KILL_AFTER_NANOS: u64 = 20_000_000;

/// What travels in the VMO, to show the handle that arrives names it.
const SECRET: &[u8] = b"carried by a handle";
/// What travels in the message.
const PING: &[u8] = b"ping";

/// What the checks measured, for the boot log.
#[derive(Debug)]
pub(crate) struct Report {
    /// Messages carried from one process to the other and read back intact.
    pub(crate) messages: u32,
    /// Handles that left one process's table and arrived in the other's.
    pub(crate) moved: u32,
    /// Calls refused with exactly the status they had to be refused with.
    pub(crate) refusals: u32,
    /// Frames the second run did not give back. Zero, or something leaks.
    pub(crate) leaked: i64,
    /// Waits woken by the thing they waited for, rather than their deadline.
    pub(crate) woken: u32,
    /// Packets taken from a port, user and signal alike.
    pub(crate) packets: u32,
    /// Processes ended by killing a job they were in.
    pub(crate) killed: u32,
    /// Messages two programs in user mode exchanged with each other.
    pub(crate) exchanged: u32,
}

/// Counts what happened, so the report is a measurement and not a claim.
#[derive(Debug, Default)]
struct Counter {
    /// See [`Report::messages`].
    messages: u32,
    /// See [`Report::moved`].
    moved: u32,
    /// See [`Report::refusals`].
    refusals: u32,
    /// See [`Report::woken`].
    woken: u32,
    /// See [`Report::packets`].
    packets: u32,
    /// See [`Report::killed`].
    killed: u32,
    /// See [`Report::exchanged`].
    exchanged: u32,
    /// See [`Report::mapped`].
    mapped: u32,
    /// See [`Report::interrupts`].
    interrupts: u32,
    /// See [`DeviceReport::pinned`].
    pinned: u32,
}

/// Run them. `Err` names the first thing that was not true.
pub(crate) fn run() -> Result<Report, &'static str> {
    check_the_native_range_is_not_a_linux_one()?;

    // Twice, measured on the second, for the reason `syscall::check::run`
    // gives: the heap keeps a page of each size class the first run touched.
    let _warm = check_two_processes()?;
    let before = mm::free_frames();
    let counter = check_two_processes()?;
    let leaked = i64::try_from(before).unwrap_or(i64::MAX)
        - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);
    // Checked, not only printed. The cycle check's own premise is that this
    // count is what fails if a refusal stops happening, and a count nothing
    // tested would boot green through exactly that.
    if leaked != 0 {
        return Err("the native object checks did not give every frame back");
    }

    // Outside the measured window, both: a woken waker and a killed program
    // leave kernel stacks for the scheduler to reap later, and the frame count
    // would read a stack not yet reaped as a leak.
    let mut after = Counter::default();
    check_a_wait_is_woken_by_what_it_waits_for(&mut after)?;
    check_a_job_kill_takes_down_a_process_tree(&mut after)?;
    check_a_long_chain_of_jobs_is_freed_without_recursion()?;
    check_a_port_wait_is_woken_by_a_message(&mut after)?;
    check_two_programs_talk_over_a_channel(&mut after)?;

    Ok(Report {
        messages: counter.messages,
        moved: counter.moved,
        refusals: counter.refusals + after.refusals,
        leaked,
        woken: after.woken,
        packets: counter.packets + after.packets,
        killed: after.killed,
        exchanged: after.exchanged,
    })
}

/// A native number reaches the native dispatcher and no Linux handler.
///
/// Asked of this build's own table, because a host test cannot know which one
/// the kernel was compiled against. With no process running, the answer has
/// to be `ESRCH` — the native side asking for a process — and not whatever a
/// Linux call of that number would have said.
fn check_the_native_range_is_not_a_linux_one() -> Result<(), &'static str> {
    if arch::decode_syscall(nr::CHANNEL_CREATE).is_some() {
        return Err("this build's Linux table claims a native number");
    }
    let args = SyscallArgs {
        number: nr::CHANNEL_CREATE,
        args: [0; 6],
    };
    if linux::dispatch(&args, None) != Outcome::Return(Errno::ESRCH.as_return_value()) {
        return Err("a native call with no process was not refused with ESRCH");
    }
    Ok(())
}

/// Everything, between two fresh processes, with every handle closed after.
fn check_two_processes() -> Result<Counter, &'static str> {
    let mut counter = Counter::default();
    let sender = Side::new()?;
    let receiver = Side::new()?;

    let (near, far) = connect(&sender, &receiver)?;
    let arrived =
        check_a_message_carries_a_handle_across(&sender, &receiver, near, far, &mut counter)?;
    check_rights_only_shrink(&receiver, arrived, &mut counter)?;
    check_a_refused_send_keeps_its_handles(&sender, near, &mut counter)?;
    check_a_cycle_of_channels_is_refused(&sender, &mut counter)?;
    check_an_endpoint_survives_a_bad_buffer(&sender, &mut counter)?;
    check_ports(&sender, &mut counter)?;
    check_a_full_channel_says_wait(&sender, near, &receiver, far, &mut counter)?;
    check_a_closed_peer_frees_what_was_queued(&sender, near, &receiver, far, &mut counter)?;
    if sender.call(0x1030, &[]) != Err(Errno::ENOSYS) {
        return Err("a gap in the native range was not ENOSYS");
    }

    sender.close_everything();
    receiver.close_everything();
    Ok(counter)
}

/// One of the two processes.
struct Side {
    /// The process, over its own address space.
    process: Arc<Process>,
}

impl Side {
    /// A fresh process with a scratch region mapped.
    fn new() -> Result<Side, &'static str> {
        let process = process::new_for_check().map_err(|_| "could not make a process")?;
        let _ = process
            .space()
            .map_anonymous(SCRATCH, SCRATCH_LEN, VmaFlags::READ_WRITE)
            .map_err(|_| "could not map a scratch region")?;
        Ok(Side { process })
    }

    /// Make a native call with these registers.
    fn call(&self, number: usize, args: &[u64]) -> Result<usize, Errno> {
        let mut registers = [0_u64; 6];
        for (slot, value) in registers.iter_mut().zip(args) {
            *slot = *value;
        }
        let args = SyscallArgs {
            number,
            args: registers,
        };
        native::dispatch(&args, Some(&self.process))
    }

    /// Make a call that returns a handle.
    fn handle(
        &self,
        number: usize,
        args: &[u64],
        what: &'static str,
    ) -> Result<Handle, &'static str> {
        let value = self.call(number, args).map_err(|_| what)?;
        u32::try_from(value).map(Handle).map_err(|_| what)
    }

    /// Put bytes in this process's memory.
    fn put(&self, at: u64, bytes: &[u8]) -> Result<(), &'static str> {
        uaccess::copy_to_user(self.process.space(), at, bytes)
            .map_err(|_| "could not stage user memory")
    }

    /// Read bytes back out of it.
    fn get(&self, at: u64, len: usize) -> Result<Vec<u8>, &'static str> {
        let mut out = vec![0_u8; len];
        uaccess::copy_from_user(self.process.space(), at, &mut out)
            .map_err(|_| "could not read user memory back")?;
        Ok(out)
    }

    /// A `u32` out of it.
    fn get_u32(&self, at: u64) -> Result<u32, &'static str> {
        let bytes = self.get(at, 4)?;
        <[u8; 4]>::try_from(bytes.as_slice())
            .map(u32::from_ne_bytes)
            .map_err(|_| "a short read")
    }

    /// Put handle values at [`HANDLES`].
    fn put_handles(&self, handles: &[Handle]) -> Result<(), &'static str> {
        let words: Vec<u8> = handles.iter().flat_map(|h| h.0.to_ne_bytes()).collect();
        self.put(HANDLES, &words)
    }

    /// Put a VMO offset at [`OFFSET`].
    fn put_offset(&self, offset: u64) -> Result<(), &'static str> {
        self.put(OFFSET, &offset.to_ne_bytes())
    }

    /// Make a channel inside this process, returning both ends.
    fn channel(&self) -> Result<(Handle, Handle), &'static str> {
        let _ = self
            .call(nr::CHANNEL_CREATE, &[PAIR])
            .map_err(|_| "channel_create failed")?;
        Ok((Handle(self.get_u32(PAIR)?), Handle(self.get_u32(PAIR + 4)?)))
    }

    /// Close whatever is left, as a process's exit will.
    fn close_everything(&self) {
        object::dispose(self.process.with_handles(object::HandleTable::clear));
    }
}

/// A handle as a register.
fn reg(handle: Handle) -> u64 {
    u64::from(handle.0)
}

/// A length as a register.
fn len(bytes: &[u8]) -> u64 {
    bytes.len() as u64
}

/// Require a call to be refused with exactly `wanted`.
fn refused(
    result: Result<usize, Errno>,
    wanted: Errno,
    what: &'static str,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    if result == Err(wanted) {
        counter.refusals += 1;
        Ok(())
    } else {
        Err(what)
    }
}

/// Make a channel in the sender and move one end to the receiver.
///
/// By hand, through the two tables, because that is the only way two
/// processes can share a channel before either can run — and it is exactly
/// what `process::load` followed by `process::start` will do for a real
/// child: put a handle in its table before its first instruction.
fn connect(sender: &Side, receiver: &Side) -> Result<(Handle, Handle), &'static str> {
    let _ = sender
        .call(nr::CHANNEL_CREATE, &[PAIR])
        .map_err(|_| "channel_create failed")?;
    let near = Handle(sender.get_u32(PAIR)?);
    let far = Handle(sender.get_u32(PAIR + 4)?);

    let moved = sender
        .process
        .with_handles(|table| table.take_many(&[far], Rights::NONE))
        .map_err(|_| "could not take the far end out of the sender")?;
    let placed = receiver
        .process
        .with_handles(|table| table.insert_many(moved))
        .map_err(|_| "could not place the far end in the receiver")?;
    let far_there = placed.first().copied().ok_or("no handle was placed")?;

    if sender.call(nr::HANDLE_CLOSE, &[reg(far)]) != Err(status::BAD_HANDLE) {
        return Err("the far end is still in the sender's table");
    }
    Ok((near, far_there))
}

/// Bytes and a VMO handle go from the sender to the receiver, and the handle
/// that arrives names the object that was sent. Returns that handle.
fn check_a_message_carries_a_handle_across(
    sender: &Side,
    receiver: &Side,
    near: Handle,
    far: Handle,
    counter: &mut Counter,
) -> Result<Handle, &'static str> {
    let vmo = sender.handle(nr::VMO_CREATE, &[PAGE_SIZE], "vmo_create failed")?;
    sender.put_offset(0x10)?;
    sender.put(PAYLOAD, SECRET)?;
    let _ = sender
        .call(nr::VMO_WRITE, &[reg(vmo), PAYLOAD, len(SECRET), OFFSET])
        .map_err(|_| "vmo_write failed")?;

    sender.put(PAYLOAD, PING)?;
    sender.put_handles(&[vmo])?;
    let _ = sender
        .call(
            nr::CHANNEL_WRITE,
            &[reg(near), PAYLOAD, len(PING), HANDLES, 1],
        )
        .map_err(|_| "a message carrying a handle was refused")?;
    counter.moved += 1;
    refused(
        sender.call(nr::VMO_GET_SIZE, &[reg(vmo), SIZE]),
        status::BAD_HANDLE,
        "a handle sent through a channel stayed in the sender's table",
        counter,
    )?;

    check_a_read_that_does_not_fit_takes_nothing(receiver, far, counter)?;

    let _ = receiver
        .call(nr::CHANNEL_READ, &[reg(far), INBOX, 64, HANDLES, 4, ACTUAL])
        .map_err(|_| "a read with room for the message failed")?;
    if receiver.get(INBOX, PING.len())? != PING {
        return Err("the bytes that arrived are not the bytes sent");
    }
    let arrived = Handle(receiver.get_u32(HANDLES)?);
    receiver.put_offset(0x10)?;
    let _ = receiver
        .call(nr::VMO_READ, &[reg(arrived), INBOX, len(SECRET), OFFSET])
        .map_err(|_| "the handle that arrived could not be read through")?;
    if receiver.get(INBOX, SECRET.len())? != SECRET {
        return Err("the handle that arrived names a different object");
    }
    counter.messages += 1;

    refused(
        receiver.call(nr::CHANNEL_READ, &[reg(far), INBOX, 64, HANDLES, 4, ACTUAL]),
        status::SHOULD_WAIT,
        "an empty channel did not say wait",
        counter,
    )?;
    refused(
        receiver.call(nr::VMO_READ, &[reg(far), INBOX, 1, OFFSET]),
        status::WRONG_TYPE,
        "a channel was read as a VMO",
        counter,
    )?;
    Ok(arrived)
}

/// Too little room for the bytes, then for the handle: both refused, the
/// sizes reported, and the message still there.
fn check_a_read_that_does_not_fit_takes_nothing(
    receiver: &Side,
    far: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    refused(
        receiver.call(nr::CHANNEL_READ, &[reg(far), INBOX, 3, HANDLES, 1, ACTUAL]),
        status::BUFFER_TOO_SMALL,
        "a read into three bytes took a four-byte message",
        counter,
    )?;
    if (receiver.get_u32(ACTUAL)?, receiver.get_u32(ACTUAL + 4)?) != (4, 1) {
        return Err("a read that did not fit reported the wrong sizes");
    }
    refused(
        receiver.call(nr::CHANNEL_READ, &[reg(far), INBOX, 64, HANDLES, 0, ACTUAL]),
        status::BUFFER_TOO_SMALL,
        "a read with no room for a handle took a message carrying one",
        counter,
    )
}

/// A narrower duplicate cannot do what it lacks, and cannot get it back.
fn check_rights_only_shrink(
    side: &Side,
    vmo: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let read_only = u64::from((Rights::DUPLICATE | Rights::READ).0);
    let narrow = side.handle(
        nr::HANDLE_DUPLICATE,
        &[reg(vmo), read_only],
        "duplicating with fewer rights failed",
    )?;
    refused(
        side.call(nr::VMO_WRITE, &[reg(narrow), INBOX, 1, OFFSET]),
        status::ACCESS_DENIED,
        "a handle without WRITE wrote",
        counter,
    )?;
    let _ = side
        .call(nr::VMO_READ, &[reg(narrow), INBOX, 1, OFFSET])
        .map_err(|_| "a read-only handle could not read")?;
    refused(
        side.call(
            nr::HANDLE_DUPLICATE,
            &[reg(narrow), u64::from(Rights::WRITE.0)],
        ),
        status::ACCESS_DENIED,
        "a duplicate gained a right its original lacked",
        counter,
    )?;
    refused(
        side.call(nr::HANDLE_DUPLICATE, &[reg(narrow), 0x80]),
        status::INVALID_ARGS,
        "a request for an undefined right was accepted",
        counter,
    )?;

    let replaced = side.handle(
        nr::HANDLE_REPLACE,
        &[reg(narrow), u64::from(Rights::READ.0)],
        "handle_replace failed",
    )?;
    refused(
        side.call(nr::HANDLE_CLOSE, &[reg(narrow)]),
        status::BAD_HANDLE,
        "a replaced handle still resolves",
        counter,
    )?;
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(replaced)])
        .map_err(|_| "closing a replacement failed")?;
    refused(
        side.call(nr::HANDLE_CLOSE, &[reg(replaced)]),
        status::BAD_HANDLE,
        "a handle was closed twice",
        counter,
    )
}

/// A send that fails keeps every handle it named, under the same number.
fn check_a_refused_send_keeps_its_handles(
    sender: &Side,
    near: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let kept = sender.handle(nr::VMO_CREATE, &[PAGE_SIZE], "vmo_create failed")?;
    sender.put_handles(&[kept, Handle(0xDEAD_B001)])?;
    refused(
        sender.call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 1, HANDLES, 2]),
        status::BAD_HANDLE,
        "a send naming a handle that does not exist was accepted",
        counter,
    )?;
    let _ = sender
        .call(nr::VMO_GET_SIZE, &[reg(kept), SIZE])
        .map_err(|_| "a refused send took a good handle with it")?;

    sender.put_handles(&[near])?;
    refused(
        sender.call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 1, HANDLES, 1]),
        status::INVALID_ARGS,
        "a channel end was sent through itself",
        counter,
    )?;
    refused(
        sender.call(
            nr::CHANNEL_WRITE,
            &[reg(near), PAYLOAD, CHANNEL_MAX_BYTES as u64 + 1, HANDLES, 0],
        ),
        status::TOO_BIG,
        "a message over the size limit was accepted",
        counter,
    )?;
    let _ = sender
        .call(nr::HANDLE_CLOSE, &[reg(kept)])
        .map_err(|_| "closing a kept handle failed")?;
    Ok(())
}

/// Writes to a reader that is not reading are told to wait, not queued
/// without end, and draining makes room again.
fn check_a_full_channel_says_wait(
    sender: &Side,
    near: Handle,
    receiver: &Side,
    far: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let empty = [reg(near), PAYLOAD, 0, HANDLES, 0];
    let mut queued = 0_u32;
    loop {
        match sender.call(nr::CHANNEL_WRITE, &empty) {
            Ok(_) => queued += 1,
            Err(error) if error == status::SHOULD_WAIT => break,
            Err(_) => return Err("filling a channel failed for a reason other than full"),
        }
        if queued > 4096 {
            return Err("a channel's queue has no bound");
        }
    }
    counter.refusals += 1;

    let drain = [reg(far), INBOX, 0, HANDLES, 0, ACTUAL];
    for _ in 0..queued {
        let _ = receiver
            .call(nr::CHANNEL_READ, &drain)
            .map_err(|_| "draining a full channel failed")?;
    }
    let _ = sender
        .call(nr::CHANNEL_WRITE, &empty)
        .map_err(|_| "a drained channel still refused a write")?;
    let _ = receiver
        .call(nr::CHANNEL_READ, &drain)
        .map_err(|_| "the write after draining did not arrive")?;
    Ok(())
}

/// Closing the reader ends the channel for the writer, and frees a VMO that
/// was still queued, unread, inside it. `run`'s frame count is what says the
/// VMO's page came back.
fn check_a_closed_peer_frees_what_was_queued(
    sender: &Side,
    near: Handle,
    receiver: &Side,
    far: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let held = sender.handle(nr::VMO_CREATE, &[PAGE_SIZE], "vmo_create failed")?;
    sender.put_offset(0)?;
    sender.put(PAYLOAD, b"x")?;
    let _ = sender
        .call(nr::VMO_WRITE, &[reg(held), PAYLOAD, 1, OFFSET])
        .map_err(|_| "vmo_write failed")?;
    sender.put_handles(&[held])?;
    let _ = sender
        .call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 0, HANDLES, 1])
        .map_err(|_| "queueing a VMO failed")?;

    let _ = receiver
        .call(nr::HANDLE_CLOSE, &[reg(far)])
        .map_err(|_| "closing the far end failed")?;
    refused(
        sender.call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 0, HANDLES, 0]),
        status::PEER_CLOSED,
        "a write to a closed channel was accepted",
        counter,
    )?;
    refused(
        sender.call(
            nr::CHANNEL_READ,
            &[reg(near), INBOX, 64, HANDLES, 4, ACTUAL],
        ),
        status::PEER_CLOSED,
        "an empty channel with a closed peer said wait instead of closed",
        counter,
    )?;
    let _ = sender
        .call(nr::HANDLE_CLOSE, &[reg(near)])
        .map_err(|_| "closing the near end failed")?;
    Ok(())
}

/// Two channels cannot be made to hold each other, and the send that tries
/// keeps its handle.
///
/// The leak this prevents is invisible to every other check: two endpoints
/// each queued in the other's inbox outlive every handle to both, with
/// whatever they hold. So a VMO with a committed page rides in one of the
/// queues, and if the refusal ever stops happening, `run`'s frame count is
/// what fails.
fn check_a_cycle_of_channels_is_refused(
    side: &Side,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let (first, first_far) = side.channel()?;
    let (second, second_far) = side.channel()?;

    let held = side.handle(nr::VMO_CREATE, &[PAGE_SIZE], "vmo_create failed")?;
    side.put_offset(0)?;
    side.put(PAYLOAD, b"x")?;
    let _ = side
        .call(nr::VMO_WRITE, &[reg(held), PAYLOAD, 1, OFFSET])
        .map_err(|_| "vmo_write failed")?;
    side.put_handles(&[held])?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(second), PAYLOAD, 0, HANDLES, 1])
        .map_err(|_| "queueing a VMO failed")?;

    // One edge, which is fine: `second_far`, holding the VMO, is queued in
    // `first_far`.
    side.put_handles(&[second_far])?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(first), PAYLOAD, 0, HANDLES, 1])
        .map_err(|_| "queueing one endpoint inside another was refused")?;

    // The edge back would close the loop: `first_far` into `second_far`.
    side.put_handles(&[first_far])?;
    refused(
        side.call(nr::CHANNEL_WRITE, &[reg(second), PAYLOAD, 0, HANDLES, 1]),
        status::INVALID_ARGS,
        "a send closing a cycle of two channels was accepted",
        counter,
    )?;
    // And the shortest loop: an end sent into its own inbox by its peer.
    refused(
        side.call(nr::CHANNEL_WRITE, &[reg(first), PAYLOAD, 0, HANDLES, 1]),
        status::INVALID_ARGS,
        "a channel end was sent into its own inbox",
        counter,
    )?;

    // `first_far` was kept by both refusals. Closing it frees `second_far`
    // queued in it, and the VMO queued in that.
    for end in [first, second, first_far] {
        let _ = side
            .call(nr::HANDLE_CLOSE, &[reg(end)])
            .map_err(|_| "closing a channel end after the cycle check failed")?;
    }
    Ok(())
}

/// The process and channel end the waker writes into.
static WAKER: SpinLock<Option<(Arc<Process>, Handle)>> = SpinLock::new(None);

/// A kernel thread that writes one empty message after a delay, so a wait
/// has something other than its deadline to end it.
fn write_after_a_delay(_: usize) {
    crate::sched::sleep_for(WAKE_AFTER_NANOS);
    let taken = WAKER.lock().take();
    if let Some((process, end)) = taken {
        let args = SyscallArgs {
            number: nr::CHANNEL_WRITE,
            args: [reg(end), 0, 0, 0, 0, 0],
        };
        let _ = native::dispatch(&args, Some(&process));
    }
}

/// A deadline `nanos` from now, staged at [`DEADLINE`].
fn stage_deadline(side: &Side, nanos: u64) -> Result<(), &'static str> {
    let deadline = crate::timer::now_nanos().saturating_add(nanos);
    side.put(DEADLINE, &deadline.to_ne_bytes())
}

/// A wait ends when its signal is asserted, at once if it already is, and at
/// its deadline if it never is.
///
/// The woken case is the one that matters, and the one easiest to fake: a
/// wait that only ever polled until its deadline would pass every other part
/// of this. So the deadline is two minutes, the message arrives after twenty
/// milliseconds, and the wait has to come back in between.
fn check_a_wait_is_woken_by_what_it_waits_for(counter: &mut Counter) -> Result<(), &'static str> {
    let side = Side::new()?;
    let (near, far) = side.channel()?;
    let readable = u64::from((Signals::READABLE | Signals::PEER_CLOSED).0);
    let wait = |signals: u64| {
        side.call(
            nr::OBJECT_WAIT_ONE,
            &[reg(far), signals, DEADLINE, OBSERVED],
        )
    };

    stage_deadline(&side, 0)?;
    refused(
        wait(readable),
        status::TIMED_OUT,
        "a wait on an empty channel said it was ready",
        counter,
    )?;

    *WAKER.lock() = Some((Arc::clone(&side.process), near));
    let _waker = crate::sched::spawn(
        "native waker",
        write_after_a_delay,
        0,
        ferrix_sched::NICE_0_WEIGHT,
    )
    .map_err(|_| "could not start the waker")?;
    let start = crate::timer::now_nanos();
    stage_deadline(&side, PATIENCE_NANOS)?;
    let woke = wait(readable);
    let waited = crate::timer::now_nanos().saturating_sub(start);
    *WAKER.lock() = None;
    if woke != Ok(0) {
        return Err("a wait was not woken by the message it waited for");
    }
    if waited < WAKE_AFTER_NANOS / 2 {
        return Err("a wait returned before anything had been written");
    }
    if !Signals(side.get_u32(OBSERVED)?).intersects(Signals::READABLE) {
        return Err("a woken wait did not report the channel readable");
    }
    counter.woken += 1;

    let _ = side
        .call(nr::CHANNEL_READ, &[reg(far), INBOX, 0, HANDLES, 0, ACTUAL])
        .map_err(|_| "reading the message that woke a wait failed")?;
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(near)])
        .map_err(|_| "closing a channel end failed")?;
    stage_deadline(&side, PATIENCE_NANOS)?;
    let _ = wait(u64::from(Signals::PEER_CLOSED.0))
        .map_err(|_| "a wait for a peer that had already closed did not return")?;

    let vmo = side.handle(nr::VMO_CREATE, &[PAGE_SIZE], "vmo_create failed")?;
    refused(
        side.call(
            nr::OBJECT_WAIT_ONE,
            &[reg(vmo), readable, DEADLINE, OBSERVED],
        ),
        status::ACCESS_DENIED,
        "a handle without WAIT was waited on",
        counter,
    )?;
    refused(
        wait(1 << 4),
        status::INVALID_ARGS,
        "a wait for a signal that does not exist was accepted",
        counter,
    )?;
    side.close_everything();
    Ok(())
}

/// The job a handle in `side` names.
fn job_of(side: &Side, handle: Handle) -> Result<Arc<Job>, &'static str> {
    side.process.with_handles(|table| match table.get(handle) {
        Ok((Object::Job(job), _)) => Ok(Arc::clone(job)),
        _ => Err("a handle that should name a job does not"),
    })
}

/// Load a spinning program into the job `handle` names, and start it.
fn run_in(side: &Side, handle: Handle, process: &Arc<Process>) -> Result<Arc<Task>, &'static str> {
    job_of(side, handle)?
        .adopt(process)
        .map_err(|_| "a live job refused a process")?;
    process::start(process).map_err(|_| "a program in a job could not be started")
}

/// A process ended by a job kill: it reports the kill's status, and its task
/// actually stops, for the reason `syscall::check`'s own kill check gives.
fn ended_by_the_kill(process: &Process, task: &Task) -> Result<(), &'static str> {
    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    if process.wait_for_exit(deadline) != Some(KILLED_STATUS) {
        return Err("a process in a killed job did not report the kill's status");
    }
    while !task.is_dead() {
        if crate::timer::now_nanos() >= deadline {
            return Err("a process in a killed job kept running");
        }
        crate::sched::sleep_for(1_000_000);
    }
    Ok(())
}

/// Killing a job ends every process in it and beneath it, and nothing above
/// or beside it; a killed job stays killed.
///
/// A tree of three jobs, root, middle and leaf, with a program in each that
/// is blocked in a native wait nobody will ever satisfy, and a fourth program
/// in no job at all. Killing
/// the middle job must end the middle and leaf programs and leave the root's
/// running; killing the root must end that one; the bystander must finish
/// with its own status.
/// A copy of the native program as a receiver with nobody to talk to.
///
/// It blocks in `object_wait_one` with no deadline, so only a kill ends it,
/// which is also the case `process::kill` has to get right for a waiting
/// task: the wait must notice. A program told to spin a fixed number of
/// rounds is not a substitute -- on four emulated ARMv7-A processors
/// `u32::MAX` rounds took seconds, and the root job's program finished and
/// exited before the kill it was meant to survive had even been sent.
///
/// The far end is returned and must be held until the check is done, or the
/// wait sees `PEER_CLOSED` and the program ends by itself.
fn waiting_program() -> Result<(Arc<Process>, Arc<Endpoint>), &'static str> {
    let program = native_program(b'r')?;
    let (near, far) = Endpoint::pair().ok_or("could not make a channel")?;
    let placed = program
        .with_handles(|table| table.insert(Object::Channel(near), Rights::CHANNEL))
        .map_err(|_| "no room for a bootstrap handle")?;
    if placed != BOOTSTRAP {
        return Err("a fresh process's first handle is not the one its program was built for");
    }
    Ok((program, far))
}

fn check_a_job_kill_takes_down_a_process_tree(counter: &mut Counter) -> Result<(), &'static str> {
    if arch::USER_SPIN_PROGRAM.is_empty() {
        return Ok(());
    }
    let side = Side::new()?;
    let root = side
        .process
        .with_handles(|table| table.insert(Object::Job(Job::new_root()), Rights::JOB))
        .map_err(|_| "no room for a job handle")?;
    let middle = side.handle(
        nr::JOB_CREATE,
        &[reg(root)],
        "job_create under a root failed",
    )?;
    let leaf = side.handle(
        nr::JOB_CREATE,
        &[reg(middle)],
        "job_create under a child failed",
    )?;

    let (top, _top_far) = waiting_program()?;
    let (inner, _inner_far) = waiting_program()?;
    let (deepest, _deepest_far) = waiting_program()?;
    let bystander = spinner(b'b', 1_000_000, 44)?;
    let top_task = run_in(&side, root, &top)?;
    let inner_task = run_in(&side, middle, &inner)?;
    let deepest_task = run_in(&side, leaf, &deepest)?;
    // A spinning member as well, alone on whatever processor it lands on.
    // Nothing but the kill's interrupt reaches a task that never enters the
    // kernel, so without it this program spins to the end of its rounds and
    // reports its own status, and the check fails every time rather than now
    // and then.
    let spinning = spinner(b's', u32::MAX, 6)?;
    let spinning_task = run_in(&side, middle, &spinning)?;
    let _bystander_task =
        process::start(&bystander).map_err(|_| "the bystander could not be started")?;
    crate::sched::sleep_for(KILL_AFTER_NANOS);

    // A registration on the middle job, to be fired by the kill below.
    let watching = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    side.put(KEY, &31_u64.to_ne_bytes())?;
    let _ = side
        .call(
            nr::OBJECT_WAIT_ASYNC,
            &[
                reg(middle),
                reg(watching),
                u64::from(Signals::TERMINATED.0),
                KEY,
            ],
        )
        .map_err(|_| "object_wait_async on a job failed")?;
    let _ = side
        .call(nr::JOB_KILL, &[reg(middle)])
        .map_err(|_| "job_kill failed")?;
    stage_deadline(&side, PATIENCE_NANOS)?;
    let _ = side
        .call(nr::PORT_WAIT, &[reg(watching), DEADLINE, PACKET_AT])
        .map_err(|_| "killing a job did not fire the registration watching it")?;
    let (key, kind, signals, _, _) = read_packet(&side)?;
    if key != 31 || kind != PACKET_SIGNAL || !Signals(signals).intersects(Signals::TERMINATED) {
        return Err("a killed job's signal packet did not say what fired it");
    }
    counter.packets += 1;
    ended_by_the_kill(&inner, &inner_task)?;
    ended_by_the_kill(&deepest, &deepest_task)?;
    ended_by_the_kill(&spinning, &spinning_task)?;
    if top.is_terminated() {
        return Err("killing a child job ended a process in its parent");
    }
    counter.killed += 3;

    refused(
        side.call(nr::JOB_CREATE, &[reg(middle)]),
        status::BAD_STATE,
        "a killed job made a child",
        counter,
    )?;
    let late = native_program(b'r')?;
    if job_of(&side, leaf)?.adopt(&late).is_ok() {
        return Err("a job beneath a killed one took a new process");
    }
    stage_deadline(&side, PATIENCE_NANOS)?;
    let terminated = u64::from(Signals::TERMINATED.0);
    let _ = side
        .call(
            nr::OBJECT_WAIT_ONE,
            &[reg(middle), terminated, DEADLINE, OBSERVED],
        )
        .map_err(|_| "a killed job did not say it was terminated")?;

    let _ = side
        .call(nr::JOB_KILL, &[reg(root)])
        .map_err(|_| "job_kill of the root failed")?;
    ended_by_the_kill(&top, &top_task)?;
    counter.killed += 1;

    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    if bystander.wait_for_exit(deadline) != Some(44) {
        return Err("a process in no job did not finish with its own status");
    }
    side.close_everything();
    Ok(())
}

/// Where the eight patchable bytes of [`arch::USER_NATIVE_PROGRAM`] begin.
const NATIVE_TAIL: usize = 28;

/// The handle a fresh process's first handle gets: slot 0, generation 1.
///
/// The program cannot be told its bootstrap handle after it is loaded -- its
/// text is not writable -- so it is built for this value, and the check
/// asserts the value when it places the handle. A change to how the table
/// encodes handles fails there, by name, rather than as a program talking to
/// a handle it does not hold.
const BOOTSTRAP: Handle = Handle(1);

/// Load a copy of the native program playing `role`.
fn native_program(role: u8) -> Result<Arc<Process>, &'static str> {
    let mut program = arch::USER_NATIVE_PROGRAM.to_vec();
    let tail = program
        .get_mut(NATIVE_TAIL..NATIVE_TAIL + 8)
        .ok_or("the native program is shorter than its own layout")?;
    if tail.first() != Some(&b'?') || tail.get(1) != Some(&b'\n') {
        return Err("the native program's tail is not where its layout says");
    }
    let [h0, h1, h2, h3] = BOOTSTRAP.0.to_le_bytes();
    tail.copy_from_slice(&[role, b'\n', 0, 0, h0, h1, h2, h3]);

    let class = if size_of::<usize>() == 8 {
        Class::Elf64
    } else {
        Class::Elf32
    };
    let file = image::build_with(
        class,
        arch::ARCH.elf_machine(),
        image::Shape::Good,
        &program,
    );
    process::load(
        &file,
        &[b"/native"],
        &[],
        [0x39; ferrix_ustack::RANDOM_BYTES],
    )
    .map_err(|_| "the native program could not be loaded")
}

/// What the device checks measured, for the boot log.
#[derive(Debug)]
pub(crate) struct DeviceReport {
    /// Apertures mapped into a process, and reached from a forked child.
    pub(crate) mapped: u32,
    /// Interrupts delivered to an object, waited on and acknowledged.
    pub(crate) interrupts: u32,
    /// VMO pages pinned for a device and found at their device addresses.
    pub(crate) pinned: u32,
    /// Calls refused with exactly the status they had to be refused with.
    pub(crate) refusals: u32,
}

/// The device objects: I/O mappings and interrupts, minted from the device
/// nodes stage 10 publishes.
///
/// A separate entry point from [`run`], and called later, because it needs
/// those nodes. It first ran inside [`run`], before they were published,
/// found none, and passed on every machine having checked nothing -- which
/// the boot log's `0 aperture mapped` said and nothing else did. So the
/// counts are printed, and on a machine with a whole-page aperture a count of
/// zero is a failure rather than a skip.
pub(crate) fn run_devices() -> Result<DeviceReport, &'static str> {
    let mut counter = Counter::default();
    check_a_device_gives_exactly_its_own_memory(&mut counter)?;
    check_an_interrupt_is_held_until_acknowledged(&mut counter)?;
    check_a_pin_gives_a_device_exactly_its_pages(&mut counter)?;
    let has_whole_page = device::devices().iter().any(|node| {
        node.apertures()
            .iter()
            .any(|aperture| aperture.whole_pages())
    });
    if has_whole_page && counter.mapped == 0 {
        return Err("a machine with a whole-page device aperture mapped none");
    }
    let has_vector = device::devices()
        .iter()
        .any(|node| node.vector(0).is_some());
    if has_vector && counter.interrupts == 0 {
        return Err("a machine with a device vector held no interrupt");
    }
    let has_pci = device::devices()
        .iter()
        .any(|node| matches!(node.location(), device::Location::Pci(_)));
    if has_pci && counter.pinned == 0 {
        return Err("a machine with a PCI function pinned no page for it");
    }
    Ok(DeviceReport {
        mapped: counter.mapped,
        interrupts: counter.interrupts,
        pinned: counter.pinned,
        refusals: counter.refusals,
    })
}

/// A device is given exactly the VMO pages pinned for it, at the addresses its
/// domain chose, holding what the VMO holds, and a pin is refused whatever the
/// rules refuse.
///
/// On a translated domain the pages must also be unreachable once the pin is
/// closed. On an untranslated one they stay held, which the console says.
fn check_a_pin_gives_a_device_exactly_its_pages(counter: &mut Counter) -> Result<(), &'static str> {
    let Some(node) = device::devices()
        .iter()
        .find(|node| matches!(node.location(), device::Location::Pci(_)))
        .cloned()
    else {
        return Ok(());
    };
    let side = Side::new()?;
    let handle = device_handle(&side, &node)?;
    let vmo = side.handle(
        nr::VMO_CREATE,
        &[2 * PAGE_SIZE],
        "vmo_create for a pin failed",
    )?;

    check_pin_refusals(&side, handle, vmo, counter)?;

    side.put(PAYLOAD, SECRET)?;
    side.put_offset(PAGE_SIZE)?;
    let _ = side
        .call(nr::VMO_WRITE, &[reg(vmo), PAYLOAD, len(SECRET), OFFSET])
        .map_err(|_| "vmo_write before a pin failed")?;
    let pin = side.handle(
        nr::VMO_PIN,
        &[reg(handle), reg(vmo), 0, 2 * PAGE_SIZE, 0],
        "vmo_pin of a VMO for its own device failed",
    )?;
    check_a_pin_stays_where_it_was_made(&side, pin, counter)?;
    let pages = side
        .call(nr::VMO_PIN_ADDRESSES, &[reg(pin), PINNED_AT, 2])
        .map_err(|_| "vmo_pin_addresses failed")?;
    if pages != 2 {
        return Err("a pin of two pages said it held another number");
    }
    let bytes = side.get(PINNED_AT, 16)?;
    let addresses: Vec<u64> = bytes
        .chunks_exact(8)
        .filter_map(|word| <[u8; 8]>::try_from(word).ok())
        .map(u64::from_ne_bytes)
        .collect();
    let domain = node.domain();
    let Some(second) = addresses
        .get(1)
        .and_then(|&address| domain.resolve(address))
    else {
        return Err("a pinned page's device address led nowhere");
    };
    // SAFETY: `second` is the frame holding the VMO's second page, held by the
    // pin for as long as this runs, and the direct map covers every frame; the
    // read is shorter than a page.
    let seen =
        unsafe { core::slice::from_raw_parts(mm::direct_map(second) as *const u8, SECRET.len()) };
    if seen != SECRET {
        return Err("a pinned page's device address held something other than the VMO's page");
    }
    if domain.translated() {
        refused(
            side.call(
                nr::VMO_PIN,
                &[reg(handle), reg(vmo), PAGE_SIZE, PAGE_SIZE, 0],
            ),
            status::ALREADY_BOUND,
            "a page already pinned into a translated domain was pinned again",
            counter,
        )?;
    }

    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(pin)])
        .map_err(|_| "closing a pin failed")?;
    if domain.translated()
        && addresses
            .iter()
            .any(|&address| domain.resolve(address).is_some())
    {
        return Err("a closed pin's pages were still reachable through its domain");
    }
    counter.pinned += 2;
    side.close_everything();
    Ok(())
}

/// Require a pin to stay in the process that made it: a channel write carrying
/// it and a duplicate of it are both refused, because its rights carry neither
/// `TRANSFER` nor `DUPLICATE`.
fn check_a_pin_stays_where_it_was_made(
    side: &Side,
    pin: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let (near, _far) = side.channel()?;
    side.put_handles(&[pin])?;
    refused(
        side.call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 0, HANDLES, 1]),
        status::ACCESS_DENIED,
        "a pin was sent over a channel",
        counter,
    )?;
    refused(
        side.call(nr::HANDLE_DUPLICATE, &[reg(pin), u64::from(Rights::READ.0)]),
        status::ACCESS_DENIED,
        "a pin was duplicated",
        counter,
    )
}

/// Require `vmo_pin` to refuse, with exactly the status the rules give, a
/// range off a page boundary, one past the VMO's end, an unknown option, an
/// empty range, and a writable pin through a read-only VMO handle.
fn check_pin_refusals(
    side: &Side,
    handle: Handle,
    vmo: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    for (args, what) in [
        ([1, PAGE_SIZE, 0], "a pin not on a page boundary was taken"),
        (
            [0, 3 * PAGE_SIZE, 0],
            "a pin past the end of its VMO was taken",
        ),
        ([0, PAGE_SIZE, 2], "a pin with an unknown option was taken"),
        ([0, 0, 0], "an empty pin was taken"),
    ] {
        let [offset, length, options] = args;
        refused(
            side.call(
                nr::VMO_PIN,
                &[reg(handle), reg(vmo), offset, length, options],
            ),
            status::INVALID_ARGS,
            what,
            counter,
        )?;
    }
    let reader = side.handle(
        nr::HANDLE_DUPLICATE,
        &[reg(vmo), u64::from(Rights::READ.0)],
        "handle_duplicate of a VMO failed",
    )?;
    refused(
        side.call(nr::VMO_PIN, &[reg(handle), reg(reader), 0, PAGE_SIZE, 0]),
        status::ACCESS_DENIED,
        "a writable pin was taken through a read-only VMO handle",
        counter,
    )?;
    Ok(())
}

/// Give `side` a handle to `node`, as `devmgr` will give one to a driver.
fn device_handle(side: &Side, node: &Arc<DeviceNode>) -> Result<Handle, &'static str> {
    side.process
        .with_handles(|table| table.insert(Object::Device(Arc::clone(node)), Rights::DEVICE))
        .map_err(|_| "no room for a device handle")
}

/// Stage a spec for `len` bytes at `phys` at [`SPEC`].
fn stage_spec(side: &Side, phys: u64, len: u64) -> Result<(), &'static str> {
    side.put(SPEC, &phys.to_ne_bytes())?;
    side.put(SPEC + 8, &len.to_ne_bytes())
}

/// Whether `space` translates `at` to `phys`, after faulting it in.
///
/// The page is faulted in and its translation read back, and nothing reads
/// the device memory itself: the direct map covers RAM, not registers, and a
/// read of a real device register can have side effects.
fn reaches(
    space: &crate::user::space::AddressSpace,
    at: u64,
    phys: u64,
) -> Result<bool, &'static str> {
    space
        .fault(at, Access::READ)
        .map_err(|_| "a device page could not be faulted in")?;
    Ok(mm::translate_in(space.root_table(), at) == Some(phys))
}

/// A device handle yields a mapping of its own aperture and of nothing beyond
/// it, the mapping lands on the device's physical pages, and a forked child
/// reaches the same pages rather than a copy of them.
///
/// Every machine has a whole-page aperture except where firmware found none,
/// in which case the mapping half reports nothing mapped. The refusal of a
/// sub-page aperture runs wherever one exists: the virtio-mmio transports on
/// ARMv7-A.
fn check_a_device_gives_exactly_its_own_memory(counter: &mut Counter) -> Result<(), &'static str> {
    let side = Side::new()?;

    let sub_page = device::devices().iter().find_map(|node| {
        node.apertures()
            .iter()
            .find(|aperture| !aperture.whole_pages())
            .map(|aperture| (Arc::clone(node), *aperture))
    });
    if let Some((node, aperture)) = sub_page {
        let handle = device_handle(&side, &node)?;
        stage_spec(&side, aperture.phys(), aperture.len())?;
        refused(
            side.call(nr::IO_MAPPING_CREATE, &[reg(handle), SPEC]),
            status::INVALID_ARGS,
            "an aperture smaller than a page was mapped, taking its neighbours with it",
            counter,
        )?;
    }

    let Some((node, aperture)) = device::devices().iter().find_map(|node| {
        node.apertures()
            .iter()
            .find(|aperture| aperture.whole_pages())
            .map(|aperture| (Arc::clone(node), *aperture))
    }) else {
        side.close_everything();
        return Ok(());
    };
    let handle = device_handle(&side, &node)?;

    stage_spec(&side, aperture.phys(), aperture.len() + 1)?;
    refused(
        side.call(nr::IO_MAPPING_CREATE, &[reg(handle), SPEC]),
        status::ACCESS_DENIED,
        "a range one byte past a device's aperture was granted",
        counter,
    )?;

    stage_spec(&side, aperture.phys(), aperture.len())?;
    let mapping = side.handle(
        nr::IO_MAPPING_CREATE,
        &[reg(handle), SPEC],
        "io_mapping_create of a device's own aperture failed",
    )?;
    let at = side
        .call(nr::IO_MAPPING_MAP, &[reg(mapping), 0])
        .map_err(|_| "io_mapping_map failed")? as u64;
    if !reaches(side.process.space(), at, aperture.phys())? {
        return Err("a mapped aperture does not translate to the device's own memory");
    }
    let child = side
        .process
        .space()
        .fork()
        .map_err(|_| "could not fork an address space holding a device mapping")?;
    if !reaches(&child, at, aperture.phys())? {
        return Err("a forked child does not reach the same device memory as its parent");
    }
    drop(child);

    // One driver per device: a mapping handle carries no DUPLICATE, so it
    // cannot be copied, only moved or narrowed.
    refused(
        side.call(
            nr::HANDLE_DUPLICATE,
            &[reg(mapping), u64::from(Rights::TRANSFER.0)],
        ),
        status::ACCESS_DENIED,
        "a mapping handle was duplicated",
        counter,
    )?;
    let narrow = side.handle(
        nr::HANDLE_REPLACE,
        &[reg(mapping), u64::from(Rights::TRANSFER.0)],
        "narrowing a mapping handle to TRANSFER failed",
    )?;
    refused(
        side.call(nr::IO_MAPPING_MAP, &[reg(narrow), 0]),
        status::ACCESS_DENIED,
        "a mapping handle without MAP was mapped",
        counter,
    )?;
    counter.mapped += 1;
    side.close_everything();
    Ok(())
}

/// An interrupt is claimed once, held pending from delivery until the driver
/// acknowledges it, and can be claimed again once its holder lets go.
///
/// Nothing drives a device yet, so nothing makes one interrupt. The delivery
/// here is the kernel's own handler, [`interrupt::on_interrupt`], called as the
/// controller would call it: everything after the hardware — masking, the
/// pending level a wait sees, acknowledgement unmasking — is what is being
/// checked. Only a machine whose devices have vectors runs it: every machine
/// with a PCI function whose MSI-X table can be minted from, where the vector
/// is that function's first entry and masking is the entry's own bit, and
/// ARMv7-A's virtio-mmio transports besides.
fn check_an_interrupt_is_held_until_acknowledged(
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let Some((node, vector)) = device::devices()
        .iter()
        .find_map(|node| node.vector(0).map(|vector| (Arc::clone(node), vector)))
    else {
        return Ok(());
    };
    let side = Side::new()?;
    let handle = device_handle(&side, &node)?;
    let readable = u64::from(Signals::READABLE.0);

    let first = side.handle(
        nr::INTERRUPT_CREATE,
        &[reg(handle), 0],
        "interrupt_create of a device's own vector failed",
    )?;
    refused(
        side.call(nr::INTERRUPT_CREATE, &[reg(handle), 0]),
        status::ALREADY_BOUND,
        "one interrupt line was claimed twice",
        counter,
    )?;
    refused(
        side.call(nr::INTERRUPT_CREATE, &[reg(handle), 99]),
        status::INVALID_ARGS,
        "a vector the device does not have was claimed",
        counter,
    )?;

    stage_deadline(&side, 0)?;
    refused(
        side.call(
            nr::OBJECT_WAIT_ONE,
            &[reg(first), readable, DEADLINE, OBSERVED],
        ),
        status::TIMED_OUT,
        "an interrupt that has not fired said it was pending",
        counter,
    )?;

    interrupt::on_interrupt(vector.number());
    stage_deadline(&side, PATIENCE_NANOS)?;
    let _ = side
        .call(
            nr::OBJECT_WAIT_ONE,
            &[reg(first), readable, DEADLINE, OBSERVED],
        )
        .map_err(|_| "a wait did not see an interrupt that had fired")?;
    let _ = side
        .call(nr::INTERRUPT_ACK, &[reg(first)])
        .map_err(|_| "interrupt_ack failed")?;
    stage_deadline(&side, 0)?;
    refused(
        side.call(
            nr::OBJECT_WAIT_ONE,
            &[reg(first), readable, DEADLINE, OBSERVED],
        ),
        status::TIMED_OUT,
        "an acknowledged interrupt was still pending",
        counter,
    )?;

    check_a_bound_interrupt_reaches_its_port(&side, first, vector.number(), counter)?;

    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(first)])
        .map_err(|_| "closing an interrupt failed")?;
    let again = side.handle(
        nr::INTERRUPT_CREATE,
        &[reg(handle), 0],
        "a line its holder had let go of could not be claimed again",
    )?;
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(again)])
        .map_err(|_| "closing a reclaimed interrupt failed")?;
    counter.interrupts += 1;
    side.close_everything();
    Ok(())
}

/// Wait for a task to stop, up to `deadline`.
fn task_stops(task: &Task, deadline: u64) -> Result<(), &'static str> {
    while !task.is_dead() {
        if crate::timer::now_nanos() >= deadline {
            return Err("a program that exited kept running");
        }
        crate::sched::sleep_for(1_000_000);
    }
    Ok(())
}

/// Stage 9's exit criterion: two user processes exchange messages and a
/// handle over a channel.
///
/// Everything else in this file drives the handlers from the kernel; this does
/// not. Both ends are programs running in user mode as tasks of their own,
/// and every native call goes through the real trap path of this
/// architecture: `channel_write` with a handle in the message,
/// `object_wait_one` blocking until the other side has written, `channel_read`,
/// and a VMO read through a handle that arrived from the other process. The
/// kernel only makes the channel and puts one end in each process before
/// either starts, which is what a parent does for a child.
///
/// The sender exits 0 only if the reply it reads is the secret it wrote into
/// its VMO, and the receiver could only have read that secret through the
/// handle it was sent.
fn check_two_programs_talk_over_a_channel(counter: &mut Counter) -> Result<(), &'static str> {
    if arch::USER_NATIVE_PROGRAM.is_empty() {
        return Ok(());
    }
    let sender = native_program(b's')?;
    let receiver = native_program(b'r')?;
    let (first, second) = Endpoint::pair().ok_or("could not make a channel")?;
    for (program, end) in [(&sender, first), (&receiver, second)] {
        let placed = program
            .with_handles(|table| table.insert(Object::Channel(end), Rights::CHANNEL))
            .map_err(|_| "no room for a bootstrap handle")?;
        if placed != BOOTSTRAP {
            return Err("a fresh process's first handle is not the one its program was built for");
        }
    }

    // The receiver first, so that its wait usually really blocks. If the
    // sender is scheduled first anyway, the level is already asserted when the
    // receiver looks, and returning at once is also the right answer.
    let receiver_task =
        process::start(&receiver).map_err(|_| "the receiving program could not be started")?;
    let sender_task =
        process::start(&sender).map_err(|_| "the sending program could not be started")?;

    let deadline = crate::timer::now_nanos().saturating_add(PATIENCE_NANOS);
    let received = receiver.wait_for_exit(deadline);
    let sent = sender.wait_for_exit(deadline);
    if received != Some(0) || sent != Some(0) {
        // The program exits with the number of the step that failed, so the
        // statuses are the diagnosis; a static message cannot carry them.
        crate::console::println!(
            "  exit     receiver exited with {received:?}, sender with {sent:?} \
             (steps: sender 1 vmo_create, 2 vmo_write, 3 channel_write, 4 wait, 5 channel_read, \
             6 reply; receiver 11 wait, 12 channel_read, 20 handle count, 13 vmo_read, \
             14 channel_write)"
        );
    }
    if received != Some(0) {
        return Err("the receiving program did not read the message and the handle and reply");
    }
    if sent != Some(0) {
        return Err("the sending program did not get back the secret it put in its VMO");
    }
    task_stops(&receiver_task, deadline)?;
    task_stops(&sender_task, deadline)?;
    // The message carrying the handle, and the reply.
    counter.exchanged += 2;
    Ok(())
}

/// How long a chain of jobs the recursion check builds and frees.
///
/// Recursive dropping overflows a sixteen-kibibyte kernel stack within a few
/// hundred levels. Ten thousand is far past that, and at a couple of hundred
/// bytes a job it is two megabytes of heap.
const JOB_CHAIN: usize = 10_000;

/// A chain of jobs each holding the one above it is freed without a stack
/// frame per job.
///
/// Built with one reference at a time, the way a program looping over
/// `job_create` and `handle_close` would, and dropped from the deepest end. A
/// recursive drop reaches the guard page long before the end of the chain.
fn check_a_long_chain_of_jobs_is_freed_without_recursion() -> Result<(), &'static str> {
    let root = Job::new_root();
    let mut deepest = Arc::clone(&root);
    for _ in 0..JOB_CHAIN {
        deepest = deepest
            .new_child()
            .map_err(|_| "a live job refused a child")?;
    }
    drop(root);
    drop(deepest);
    Ok(())
}

/// A message carrying an endpoint, read into a buffer that faults, is put
/// back whole, and the endpoint read afterwards is the one that was sent.
///
/// The put-back is the path that has to hold the topology lock, since it
/// re-adds an edge the cycle check walks. The race it closes cannot be staged
/// with one thread, but the path itself can: it must still deliver the right
/// object once the buffer is good.
fn check_an_endpoint_survives_a_bad_buffer(
    side: &Side,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let (near, far) = side.channel()?;
    let (carried, carried_far) = side.channel()?;
    side.put_handles(&[carried])?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 0, HANDLES, 1])
        .map_err(|_| "sending an endpoint failed")?;

    refused(
        side.call(nr::CHANNEL_READ, &[reg(far), INBOX, 0, 0x10, 1, ACTUAL]),
        status::FAULT,
        "a read into an unmapped handle buffer was not a fault",
        counter,
    )?;

    let _ = side
        .call(nr::CHANNEL_READ, &[reg(far), INBOX, 0, HANDLES, 1, ACTUAL])
        .map_err(|_| "a message put back after a bad buffer could not be read")?;
    let arrived = Handle(side.get_u32(HANDLES)?);
    side.put(PAYLOAD, b"y")?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(arrived), PAYLOAD, 1, HANDLES, 0])
        .map_err(|_| "the endpoint delivered after a put-back does not write")?;
    let _ = side
        .call(
            nr::CHANNEL_READ,
            &[reg(carried_far), INBOX, 8, HANDLES, 0, ACTUAL],
        )
        .map_err(|_| "the endpoint delivered after a put-back is not the one sent")?;

    for end in [near, far, arrived, carried_far] {
        let _ = side
            .call(nr::HANDLE_CLOSE, &[reg(end)])
            .map_err(|_| "closing a channel end failed")?;
    }
    Ok(())
}

/// Stage a user packet at [`PACKET_AT`].
fn stage_packet(side: &Side, key: u64, data: [u64; 2]) -> Result<(), &'static str> {
    let [first, second] = data;
    side.put(PACKET_AT, &key.to_ne_bytes())?;
    side.put(PACKET_AT + 16, &first.to_ne_bytes())?;
    side.put(PACKET_AT + 24, &second.to_ne_bytes())
}

/// The packet at [`PACKET_AT`]: key, kind, signals, and the two data words.
fn read_packet(side: &Side) -> Result<(u64, u32, u32, u64, u64), &'static str> {
    let bytes = side.get(PACKET_AT, 32)?;
    let word = |at: usize| {
        bytes
            .get(at..at + 8)
            .and_then(|slice| <[u8; 8]>::try_from(slice).ok())
            .map(u64::from_ne_bytes)
            .ok_or("a short packet")
    };
    let half = |at: usize| {
        bytes
            .get(at..at + 4)
            .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
            .map(u32::from_ne_bytes)
            .ok_or("a short packet")
    };
    Ok((word(0)?, half(8)?, half(12)?, word(16)?, word(24)?))
}

/// Take a packet from `port` without waiting, into [`PACKET_AT`].
fn take_now(side: &Side, port: Handle) -> Result<usize, Errno> {
    let now = crate::timer::now_nanos();
    side.put(DEADLINE, &now.to_ne_bytes())
        .map_err(|_| Errno::EFAULT)?;
    side.call(nr::PORT_WAIT, &[reg(port), DEADLINE, PACKET_AT])
}

/// A port gives back what a program queued, and a registration fires once:
/// when its signal comes true, or at once if it already is, for a message
/// arriving and for a peer closing. Refusals: a full port, an asynchronous
/// wait for `WRITABLE`, a port watched through a port, and a registration
/// through a port handle without `WRITE`.
fn check_ports(side: &Side, counter: &mut Counter) -> Result<(), &'static str> {
    let port = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    refused(
        take_now(side, port),
        status::TIMED_OUT,
        "an empty port gave a packet",
        counter,
    )?;

    stage_packet(side, 7, [0xA, 0xB])?;
    let _ = side
        .call(nr::PORT_QUEUE, &[reg(port), PACKET_AT])
        .map_err(|_| "port_queue failed")?;
    side.put(PACKET_AT, &[0; 32])?;
    let _ = take_now(side, port).map_err(|_| "a queued packet was not there to take")?;
    if read_packet(side)? != (7, PACKET_USER, 0, 0xA, 0xB) {
        return Err("a user packet came back different from how it was queued");
    }
    counter.packets += 1;

    let (near, far) = side.channel()?;
    let readable = u64::from(Signals::READABLE.0);
    side.put(KEY, &11_u64.to_ne_bytes())?;
    let _ = side
        .call(nr::OBJECT_WAIT_ASYNC, &[reg(far), reg(port), readable, KEY])
        .map_err(|_| "object_wait_async on a channel failed")?;
    refused(
        take_now(side, port),
        status::TIMED_OUT,
        "a registration fired before its signal",
        counter,
    )?;
    side.put(PAYLOAD, b"z")?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 1, HANDLES, 0])
        .map_err(|_| "a write to a watched channel failed")?;
    let _ = take_now(side, port)
        .map_err(|_| "a message did not fire the registration waiting for it")?;
    let (key, kind, signals, _, _) = read_packet(side)?;
    if key != 11 || kind != PACKET_SIGNAL || !Signals(signals).intersects(Signals::READABLE) {
        return Err("a signal packet did not say what fired it");
    }
    counter.packets += 1;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(near), PAYLOAD, 1, HANDLES, 0])
        .map_err(|_| "a second write to a watched channel failed")?;
    refused(
        take_now(side, port),
        status::TIMED_OUT,
        "a one-shot registration fired twice",
        counter,
    )?;

    // Already readable when registered: the packet is queued at once.
    side.put(KEY, &12_u64.to_ne_bytes())?;
    let _ = side
        .call(nr::OBJECT_WAIT_ASYNC, &[reg(far), reg(port), readable, KEY])
        .map_err(|_| "object_wait_async on a readable channel failed")?;
    let _ =
        take_now(side, port).map_err(|_| "a registration on a state already true did not fire")?;
    if read_packet(side)?.0 != 12 {
        return Err("the packet for an already-true state carried the wrong key");
    }
    counter.packets += 1;

    side.put(KEY, &13_u64.to_ne_bytes())?;
    let peer_closed = u64::from(Signals::PEER_CLOSED.0);
    let _ = side
        .call(
            nr::OBJECT_WAIT_ASYNC,
            &[reg(far), reg(port), peer_closed, KEY],
        )
        .map_err(|_| "object_wait_async for a closing peer failed")?;
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(near)])
        .map_err(|_| "closing a watched channel's peer failed")?;
    let _ = take_now(side, port)
        .map_err(|_| "a closing peer did not fire the registration waiting for it")?;
    let (key, _, signals, _, _) = read_packet(side)?;
    if key != 13 || !Signals(signals).intersects(Signals::PEER_CLOSED) {
        return Err("a peer-closed packet did not say so");
    }
    counter.packets += 1;

    check_port_refusals(side, port, far, counter)?;
    for end in [port, far] {
        let _ = side
            .call(nr::HANDLE_CLOSE, &[reg(end)])
            .map_err(|_| "closing a port or channel end failed")?;
    }
    Ok(())
}

/// What a port refuses.
fn check_port_refusals(
    side: &Side,
    port: Handle,
    far: Handle,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    refused(
        side.call(
            nr::OBJECT_WAIT_ASYNC,
            &[reg(far), reg(port), u64::from(Signals::WRITABLE.0), KEY],
        ),
        status::INVALID_ARGS,
        "an asynchronous wait for WRITABLE was accepted",
        counter,
    )?;
    refused(
        side.call(
            nr::OBJECT_WAIT_ASYNC,
            &[reg(port), reg(port), u64::from(Signals::READABLE.0), KEY],
        ),
        status::WRONG_TYPE,
        "a port was watched through a port",
        counter,
    )?;
    let reader = side.handle(
        nr::HANDLE_DUPLICATE,
        &[reg(port), u64::from((Rights::READ | Rights::WAIT).0)],
        "duplicating a port without WRITE failed",
    )?;
    refused(
        side.call(
            nr::OBJECT_WAIT_ASYNC,
            &[reg(far), reg(reader), u64::from(Signals::READABLE.0), KEY],
        ),
        status::ACCESS_DENIED,
        "a registration went through a port handle without WRITE",
        counter,
    )?;
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(reader)])
        .map_err(|_| "closing a narrowed port failed")?;

    stage_packet(side, 1, [0, 0])?;
    for _ in 0..object::port::PORT_CAPACITY {
        let _ = side
            .call(nr::PORT_QUEUE, &[reg(port), PACKET_AT])
            .map_err(|_| "filling a port failed before it was full")?;
    }
    refused(
        side.call(nr::PORT_QUEUE, &[reg(port), PACKET_AT]),
        status::SHOULD_WAIT,
        "a full port took another user packet",
        counter,
    )
}

/// A `port_wait` with a two-minute deadline is woken by a message a kernel
/// thread writes twenty milliseconds later, through the registration that
/// message fires: the whole chain from a channel write to a woken port.
fn check_a_port_wait_is_woken_by_a_message(counter: &mut Counter) -> Result<(), &'static str> {
    let side = Side::new()?;
    let (near, far) = side.channel()?;
    let port = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    side.put(KEY, &21_u64.to_ne_bytes())?;
    let _ = side
        .call(
            nr::OBJECT_WAIT_ASYNC,
            &[reg(far), reg(port), u64::from(Signals::READABLE.0), KEY],
        )
        .map_err(|_| "object_wait_async failed")?;

    *WAKER.lock() = Some((Arc::clone(&side.process), near));
    let _waker = crate::sched::spawn(
        "port waker",
        write_after_a_delay,
        0,
        ferrix_sched::NICE_0_WEIGHT,
    )
    .map_err(|_| "could not start the waker")?;
    let start = crate::timer::now_nanos();
    stage_deadline(&side, PATIENCE_NANOS)?;
    let woke = side.call(nr::PORT_WAIT, &[reg(port), DEADLINE, PACKET_AT]);
    let waited = crate::timer::now_nanos().saturating_sub(start);
    *WAKER.lock() = None;
    if woke != Ok(0) {
        return Err("a port wait was not woken by the message its registration watched");
    }
    if waited < WAKE_AFTER_NANOS / 2 {
        return Err("a port wait returned before anything had been written");
    }
    if read_packet(&side)?.0 != 21 {
        return Err("a woken port wait took the wrong packet");
    }
    counter.woken += 1;
    counter.packets += 1;
    side.close_everything();
    Ok(())
}

/// A bound interrupt queues one packet on its port per quiet-to-pending
/// transition: a second binding is refused, two deliveries before an
/// acknowledgement queue one packet carrying the key, the interrupt kind and
/// when it fired, and a delivery after the acknowledgement queues another.
///
/// Called with the interrupt freshly acknowledged, so it starts quiet, and
/// leaves it acknowledged.
fn check_a_bound_interrupt_reaches_its_port(
    side: &Side,
    interrupt: Handle,
    number: u32,
    counter: &mut Counter,
) -> Result<(), &'static str> {
    let port = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    side.put(KEY, &41_u64.to_ne_bytes())?;
    let _ = side
        .call(nr::INTERRUPT_BIND, &[reg(interrupt), reg(port), KEY])
        .map_err(|_| "interrupt_bind failed")?;
    refused(
        side.call(nr::INTERRUPT_BIND, &[reg(interrupt), reg(port), KEY]),
        status::ALREADY_BOUND,
        "an interrupt was bound to a second port",
        counter,
    )?;

    interrupt::on_interrupt(number);
    interrupt::on_interrupt(number);
    stage_deadline(side, PATIENCE_NANOS)?;
    let _ = side
        .call(nr::PORT_WAIT, &[reg(port), DEADLINE, PACKET_AT])
        .map_err(|_| "a bound interrupt did not reach its port")?;
    let (key, kind, _, fired_at, _) = read_packet(side)?;
    if key != 41 || kind != PACKET_INTERRUPT || fired_at == 0 {
        return Err("an interrupt packet did not carry its key, its kind and when it fired");
    }
    counter.packets += 1;
    refused(
        take_now(side, port),
        status::TIMED_OUT,
        "an interrupt delivered twice before its acknowledgement queued two packets",
        counter,
    )?;

    let _ = side
        .call(nr::INTERRUPT_ACK, &[reg(interrupt)])
        .map_err(|_| "acknowledging a bound interrupt failed")?;
    interrupt::on_interrupt(number);
    stage_deadline(side, PATIENCE_NANOS)?;
    let _ = side
        .call(nr::PORT_WAIT, &[reg(port), DEADLINE, PACKET_AT])
        .map_err(|_| "an acknowledged interrupt did not reach its port again")?;
    counter.packets += 1;
    let _ = side
        .call(nr::INTERRUPT_ACK, &[reg(interrupt)])
        .map_err(|_| "acknowledging the second delivery failed")?;
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(port)])
        .map_err(|_| "closing an interrupt's port failed")?;
    Ok(())
}
