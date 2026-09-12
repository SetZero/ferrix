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

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::nr;
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::status;
use ferrix_native_abi::types::CHANNEL_MAX_BYTES;
use ferrix_sync::SpinLock;
use ferrix_vma::VmaFlags;

use crate::arch;
use crate::mm;
use crate::object::channel::Endpoint;
use crate::object::job::{Job, KILLED_STATUS};
use crate::object::{self, Object};
use crate::sched::Task;
use crate::syscall::check::spinner;
use crate::syscall::image;
use crate::syscall::process::{self, Process};
use crate::syscall::{self as linux, Outcome, SyscallArgs, native, uaccess};
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
    /// Processes ended by killing a job they were in.
    pub(crate) killed: u32,
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
    /// See [`Report::killed`].
    killed: u32,
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

    // Outside the measured window, both: a woken waker and a killed program
    // leave kernel stacks for the scheduler to reap later, and the frame count
    // would read a stack not yet reaped as a leak.
    let mut after = Counter::default();
    check_a_wait_is_woken_by_what_it_waits_for(&mut after)?;
    check_a_job_kill_takes_down_a_process_tree(&mut after)?;

    Ok(Report {
        messages: counter.messages,
        moved: counter.moved,
        refusals: counter.refusals + after.refusals,
        leaked,
        woken: after.woken,
        killed: after.killed,
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
    if linux::dispatch(&args) != Outcome::Return(Errno::ESRCH.as_return_value()) {
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
    let _bystander_task =
        process::start(&bystander).map_err(|_| "the bystander could not be started")?;
    crate::sched::sleep_for(KILL_AFTER_NANOS);

    let _ = side
        .call(nr::JOB_KILL, &[reg(middle)])
        .map_err(|_| "job_kill failed")?;
    ended_by_the_kill(&inner, &inner_task)?;
    ended_by_the_kill(&deepest, &deepest_task)?;
    if top.is_terminated() {
        return Err("killing a child job ended a process in its parent");
    }
    counter.killed += 2;

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
