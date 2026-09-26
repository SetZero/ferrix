//! The native calls' refusals that stage 9's check does not make: a handle of
//! the wrong kind, one without the right a call needs, one named twice in a
//! message, a table with no room, a send whose cycle check would walk too far,
//! a packet whose buffer faults, a copy through a VMO a device reads past the
//! caches, and a clock asked for badly.
//!
//! Each is a sentence `docs/NATIVE-ABI.md` promises a program: which status
//! it gets, and that a refused call leaves what it was given where it was.
//! Driven through [`native::dispatch`] from one process of the check's own,
//! as stage 9's check drives the calls that succeed.

use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::errno::Errno;
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::nr;
use ferrix_native_abi::rights::{Rights, SAME_RIGHTS};
use ferrix_native_abi::status;
use ferrix_native_abi::types::CHANNEL_MAX_HANDLES;

use crate::arch;
use crate::object::check::{SCRATCH, Side, reg};
use crate::object::job::Job;
use crate::object::{HANDLE_LIMIT, Object};
use crate::syscall::image::{self, Shape};

/// Where each call's user memory is, in the side's scratch region.
const PAIR: u64 = SCRATCH + 0x100;
/// A packet going in, and one coming out.
const PACKET: u64 = SCRATCH + 0x120;
/// A deadline long past.
const DEADLINE: u64 = SCRATCH + 0x160;
/// A message's bytes, and where a read puts them.
const BYTES: u64 = SCRATCH + 0x180;
/// The handles a message carries, going out or coming in.
const HANDLES: u64 = SCRATCH + 0x200;
/// What a read reports it delivered.
const ACTUAL: u64 = SCRATCH + 0x400;
/// A user address nothing maps: the scratch region is two pages.
const UNMAPPED: u64 = SCRATCH + 0x10_0000;
/// Where a child's image is staged, past the scratch region.
const IMAGE_AT: u64 = 0x6000_0000;
/// A child's name, and where it is staged.
const CHILD_NAME: &[u8] = b"unstarted";
const NAME_AT: u64 = SCRATCH + 0x480;
/// A VMO offset for a write.
const OFFSET: u64 = SCRATCH + 0x4C0;

/// What the checks did, for the boot log.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Calls refused with the status the ABI names.
    pub(crate) refusals: u32,
    /// Handles opened to fill a table.
    pub(crate) filled: usize,
    /// Endpoints a refused send would have made its check walk.
    pub(crate) walked: usize,
}

/// Run every check.
///
/// # Errors
///
/// The first property that did not hold, as a sentence.
pub(crate) fn run() -> Result<Report, &'static str> {
    let mut report = Report::default();
    let side = Side::new()?;
    let outcome = checks(&side, &mut report);
    side.close_everything();
    outcome.map(|()| report)
}

/// The checks, on one process, which [`run`] empties afterwards whatever
/// they found.
fn checks(side: &Side, report: &mut Report) -> Result<(), &'static str> {
    the_wrong_kind_of_handle_is_refused(side, report)?;
    a_handle_without_the_right_is_refused(side, report)?;
    a_handle_named_twice_is_refused(side, report)?;
    a_packet_whose_buffer_faults_is_kept(side, report)?;
    a_copy_past_the_caches_is_refused(side, report)?;
    a_clock_asked_for_badly_is_refused(side, report)?;
    a_send_that_would_walk_too_far_is_refused(side, report)?;
    a_full_table_refuses_and_loses_nothing(side, report)?;
    a_table_that_cannot_grow_refuses_for_memory(report)?;
    a_bootstrap_without_transfer_is_refused(side, report)?;
    Ok(())
}

/// Require `result` to be exactly the refusal `wanted`.
fn refused(
    result: Result<usize, Errno>,
    wanted: Errno,
    what: &'static str,
    report: &mut Report,
) -> Result<(), &'static str> {
    if result == Err(wanted) {
        report.refusals += 1;
        Ok(())
    } else {
        Err(what)
    }
}

/// A channel in `side`: both ends.
fn channel(side: &Side) -> Result<(Handle, Handle), &'static str> {
    let _ = side
        .call(nr::CHANNEL_CREATE, &[PAIR])
        .map_err(|_| "channel_create failed")?;
    Ok((Handle(side.get_u32(PAIR)?), Handle(side.get_u32(PAIR + 4)?)))
}

/// A one-page VMO in `side`.
fn vmo(side: &Side) -> Result<Handle, &'static str> {
    side.handle(nr::VMO_CREATE, &[PAGE_SIZE], "vmo_create failed")
}

/// Stage `handles` at [`HANDLES`].
fn put_handles(side: &Side, handles: &[Handle]) -> Result<(), &'static str> {
    let words: Vec<u8> = handles.iter().flat_map(|h| h.0.to_ne_bytes()).collect();
    side.put(HANDLES, &words)
}

/// Every call that names an object of one kind refuses a handle to another
/// with `WRONG_TYPE`, whatever rights it carries: a VMO handle where a
/// channel, a process, an interrupt, a pin, an I/O mapping or a port belongs.
fn the_wrong_kind_of_handle_is_refused(
    side: &Side,
    report: &mut Report,
) -> Result<(), &'static str> {
    let memory = vmo(side)?;
    let port = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    side.put(PACKET, &[0; 32])?;
    side.put(DEADLINE, &1_u64.to_ne_bytes())?;
    let wrong = reg(memory);
    let calls: [(usize, [u64; 5], &'static str); 9] = [
        (
            nr::CHANNEL_WRITE,
            [wrong, BYTES, 0, HANDLES, 0],
            "a VMO was written as a channel",
        ),
        (
            nr::PROCESS_START,
            [wrong, 0, 0, 0, 0],
            "a VMO was started as a process",
        ),
        (
            nr::INTERRUPT_ACK,
            [wrong, 0, 0, 0, 0],
            "a VMO was acknowledged as an interrupt",
        ),
        (
            nr::INTERRUPT_BIND,
            [wrong, reg(port), PACKET, 0, 0],
            "a VMO was bound as an interrupt",
        ),
        (
            nr::VMO_PIN_ADDRESSES,
            [wrong, PACKET, 1, 0, 0],
            "a VMO was read as a pin",
        ),
        (
            nr::IO_MAPPING_MAP,
            [wrong, 0, 0, 0, 0],
            "a VMO was mapped as an I/O mapping",
        ),
        (
            nr::PORT_QUEUE,
            [wrong, PACKET, 0, 0, 0],
            "a packet was queued on a VMO",
        ),
        (
            nr::PORT_WAIT,
            [wrong, DEADLINE, PACKET, 0, 0],
            "a VMO was waited on as a port",
        ),
        (
            nr::DEVICE_CLOCK,
            [wrong, 1, 0, 0, 0],
            "a VMO was clocked as a device",
        ),
    ];
    for (number, args, what) in calls {
        refused(side.call(number, &args), status::WRONG_TYPE, what, report)?;
    }
    Ok(())
}

/// A handle that lacks the right a call needs is refused with
/// `ACCESS_DENIED`, the object untouched: a channel end duplicated with
/// `READ` alone cannot be written.
fn a_handle_without_the_right_is_refused(
    side: &Side,
    report: &mut Report,
) -> Result<(), &'static str> {
    let (end, _other) = channel(side)?;
    let read_only = side.handle(
        nr::HANDLE_DUPLICATE,
        &[reg(end), u64::from(Rights::READ.0)],
        "handle_duplicate with fewer rights failed",
    )?;
    side.put(BYTES, b"x")?;
    refused(
        side.call(nr::CHANNEL_WRITE, &[reg(read_only), BYTES, 1, HANDLES, 0]),
        status::ACCESS_DENIED,
        "a channel end without WRITE was written",
        report,
    )
}

/// A message naming one handle twice is refused with `INVALID_ARGS`, and the
/// handle stays open in the sender.
fn a_handle_named_twice_is_refused(side: &Side, report: &mut Report) -> Result<(), &'static str> {
    let (end, _other) = channel(side)?;
    let carried = vmo(side)?;
    put_handles(side, &[carried, carried])?;
    refused(
        side.call(nr::CHANNEL_WRITE, &[reg(end), BYTES, 0, HANDLES, 2]),
        status::INVALID_ARGS,
        "a message naming one handle twice was sent",
        report,
    )?;
    if side
        .call(nr::VMO_GET_SIZE, &[reg(carried), PACKET])
        .is_err()
    {
        return Err("a refused message took the handle it named twice");
    }
    Ok(())
}

/// A packet taken off a port for a buffer that faults is put back, and the
/// next wait with a good buffer gets it.
fn a_packet_whose_buffer_faults_is_kept(
    side: &Side,
    report: &mut Report,
) -> Result<(), &'static str> {
    let port = side.handle(nr::PORT_CREATE, &[], "port_create failed")?;
    let key = 0x5EED_u64;
    let mut packet = [0_u8; 32];
    if let Some(slot) = packet.get_mut(..8) {
        slot.copy_from_slice(&key.to_ne_bytes());
    }
    side.put(PACKET, &packet)?;
    side.put(DEADLINE, &1_u64.to_ne_bytes())?;
    let _ = side
        .call(nr::PORT_QUEUE, &[reg(port), PACKET])
        .map_err(|_| "port_queue failed")?;
    refused(
        side.call(nr::PORT_WAIT, &[reg(port), DEADLINE, UNMAPPED]),
        status::FAULT,
        "a packet was delivered to a buffer nothing maps",
        report,
    )?;
    side.put(PACKET, &[0; 32])?;
    let _ = side
        .call(nr::PORT_WAIT, &[reg(port), DEADLINE, PACKET])
        .map_err(|_| "a packet whose delivery faulted was lost")?;
    let got = side.get(PACKET, 8)?;
    if got.as_slice() != key.to_ne_bytes().as_slice() {
        return Err("a packet put back came out as another");
    }
    Ok(())
}

/// A VMO a device reads past the caches is not copied through the kernel's
/// cached view: `vmo_read` and `vmo_write` answer `BAD_STATE`.
fn a_copy_past_the_caches_is_refused(side: &Side, report: &mut Report) -> Result<(), &'static str> {
    let handle = vmo(side)?;
    let object = side
        .process
        .with_handles(|table| table.get(handle).map(|(object, _)| object.clone()))
        .map_err(|_| "a VMO just made is not in its table")?;
    let Object::Vmo(memory) = object else {
        return Err("vmo_create made something other than a VMO");
    };
    if !memory.make_coherent() {
        return Err("a VMO nothing maps could not be made coherent");
    }
    side.put(PACKET, &0_u64.to_ne_bytes())?;
    for number in [nr::VMO_READ, nr::VMO_WRITE] {
        refused(
            side.call(number, &[reg(handle), BYTES, 8, PACKET]),
            status::BAD_STATE,
            "a VMO a device reads past the caches was copied through them",
            report,
        )?;
    }
    Ok(())
}

/// `device_clock` refuses an option it does not know and a rate of zero
/// before it looks at the handle, and a device whose board keeps no clock for
/// it with `WRONG_TYPE`.
fn a_clock_asked_for_badly_is_refused(
    side: &Side,
    report: &mut Report,
) -> Result<(), &'static str> {
    refused(
        side.call(nr::DEVICE_CLOCK, &[0, 1, 1 << 7]),
        status::INVALID_ARGS,
        "a clock was set with an option nobody defined",
        report,
    )?;
    refused(
        side.call(nr::DEVICE_CLOCK, &[0, 0, 0]),
        status::INVALID_ARGS,
        "a clock was asked for zero hertz",
        report,
    )?;
    let Some(node) = crate::device::devices().first() else {
        return Ok(());
    };
    if crate::device::board_clock(node, 1, false).is_some() {
        // A board that keeps this device's clock: not the refusal checked here.
        return Ok(());
    }
    let device = side
        .process
        .with_handles(|table| table.insert(Object::Device(node.clone()), Rights::DEVICE))
        .map_err(|_| "no room for a device handle")?;
    refused(
        side.call(nr::DEVICE_CLOCK, &[reg(device), 1, 0]),
        status::WRONG_TYPE,
        "a clock was read from a device whose board keeps none for it",
        report,
    )
}

/// A send whose cycle check would have to walk more queued endpoints than
/// the bound is refused as `TOO_BIG`, and what it would have carried stays
/// with the sender.
///
/// One channel's queue is filled with the ends of more channels than the
/// walk visits; sending that channel's reading end anywhere then asks the
/// walk to visit all of them.
fn a_send_that_would_walk_too_far_is_refused(
    side: &Side,
    report: &mut Report,
) -> Result<(), &'static str> {
    const WALK_BOUND: usize = 1024;
    let (full, feeder) = channel(side)?;
    let mut queued = 0;
    while queued <= WALK_BOUND {
        let mut batch = Vec::new();
        while batch.len() + 2 <= CHANNEL_MAX_HANDLES && queued + batch.len() <= WALK_BOUND {
            let (first, second) = channel(side)?;
            batch.push(first);
            batch.push(second);
        }
        put_handles(side, &batch)?;
        let _ = side
            .call(
                nr::CHANNEL_WRITE,
                &[reg(feeder), BYTES, 0, HANDLES, batch.len() as u64],
            )
            .map_err(|_| "could not queue endpoints for the walk")?;
        queued += batch.len();
    }
    let (out, _elsewhere) = channel(side)?;
    put_handles(side, &[full])?;
    refused(
        side.call(nr::CHANNEL_WRITE, &[reg(out), BYTES, 0, HANDLES, 1]),
        status::TOO_BIG,
        "a send was made whose cycle check could not finish",
        report,
    )?;
    if side.call(nr::CHANNEL_READ, &[reg(full), BYTES, 0, HANDLES, 0, ACTUAL])
        == Err(status::BAD_HANDLE)
    {
        return Err("a send refused as too big took the end it carried");
    }
    report.walked = queued;
    Ok(())
}

/// A table with no room refuses a new handle with `NO_HANDLES` -- from a
/// duplicate, from a call that makes an object, and from a read that would
/// deliver one -- and the message that could not be delivered stays queued.
fn a_full_table_refuses_and_loses_nothing(
    side: &Side,
    report: &mut Report,
) -> Result<(), &'static str> {
    let (inbox, sender) = channel(side)?;
    let carried = vmo(side)?;
    put_handles(side, &[carried])?;
    let _ = side
        .call(nr::CHANNEL_WRITE, &[reg(sender), BYTES, 0, HANDLES, 1])
        .map_err(|_| "could not send a handle to read into a full table")?;

    let mut filled = 0;
    loop {
        match side.call(nr::HANDLE_DUPLICATE, &[reg(sender), u64::from(SAME_RIGHTS)]) {
            Ok(_) => filled += 1,
            Err(status::NO_HANDLES) => break,
            Err(_) => return Err("a duplicate into a table with room failed"),
        }
        if filled > HANDLE_LIMIT {
            return Err("a handle table took more handles than its limit");
        }
    }
    report.refusals += 1;
    refused(
        side.call(nr::VMO_CREATE, &[PAGE_SIZE]),
        status::NO_HANDLES,
        "an object was made into a full table",
        report,
    )?;
    refused(
        side.call(
            nr::CHANNEL_READ,
            &[
                reg(inbox),
                BYTES,
                0,
                HANDLES,
                CHANNEL_MAX_HANDLES as u64,
                ACTUAL,
            ],
        ),
        status::NO_HANDLES,
        "a handle was delivered into a full table",
        report,
    )?;

    // Room again, and the message is still there to read.
    let _ = side
        .call(nr::HANDLE_CLOSE, &[reg(sender)])
        .map_err(|_| "could not close a handle of a full table")?;
    let _ = side
        .call(
            nr::CHANNEL_READ,
            &[
                reg(inbox),
                BYTES,
                0,
                HANDLES,
                CHANNEL_MAX_HANDLES as u64,
                ACTUAL,
            ],
        )
        .map_err(|_| "a message refused for a full table was lost")?;
    report.filled = filled;
    // Room for the checks after this one.
    side.close_everything();
    Ok(())
}

/// A process whose handle table has never held a handle must grow it for its
/// first, and with no memory to grow it a call that makes an object answers
/// `NO_MEMORY` and frees the object it made, whichever of the call's
/// allocations was the one refused (finding F-23).
///
/// Each attempt is a fresh process, so the table's growth is among the few
/// allocations the call makes, and every one of them in turn is failed.
fn a_table_that_cannot_grow_refuses_for_memory(report: &mut Report) -> Result<(), &'static str> {
    let me = crate::sched::current_id().ok_or("the checking task is not running")?;
    let mut refused = 0;
    for number in [nr::VMO_CREATE, nr::CHANNEL_CREATE] {
        for period in 1..=8 {
            let fresh = Side::new()?;
            crate::fallible::inject(me, period);
            let made = fresh.call(
                number,
                &[if number == nr::VMO_CREATE {
                    PAGE_SIZE
                } else {
                    PAIR
                }],
            );
            let _ = crate::fallible::stop_injecting();
            fresh.close_everything();
            match made {
                Ok(_) => {}
                Err(status::NO_MEMORY) => refused += 1,
                Err(_) => {
                    return Err("a call without memory answered something other than NO_MEMORY");
                }
            }
        }
    }
    if refused == 0 {
        return Err("no call was refused for memory with every allocation failing in turn");
    }
    report.refusals += refused;
    Ok(())
}

/// `process_start` moves the bootstrap handle into the child only if it
/// carries `TRANSFER`: without it the start is refused with
/// `ACCESS_DENIED` before anything moves, and the handle stays the
/// caller's.
fn a_bootstrap_without_transfer_is_refused(
    side: &Side,
    report: &mut Report,
) -> Result<(), &'static str> {
    if arch::USER_ARGUMENT_PROGRAM.is_empty() {
        return Ok(());
    }
    let job = Job::new_root().map_err(|_| "no memory for a child's job")?;
    let root = side
        .process
        .with_handles(|table| table.insert(Object::Job(job), Rights::JOB))
        .map_err(|_| "no room for a child's job")?;
    let class = if size_of::<usize>() == 8 {
        ferrix_elf::Class::Elf64
    } else {
        ferrix_elf::Class::Elf32
    };
    let file = image::build_with(
        class,
        arch::ARCH.elf_machine(),
        Shape::Good,
        arch::USER_ARGUMENT_PROGRAM,
    );
    let len = file.len() as u64;
    let _ = side
        .process
        .space()
        .map_anonymous(
            IMAGE_AT,
            len.div_ceil(PAGE_SIZE) * PAGE_SIZE,
            ferrix_vma::VmaFlags::READ_WRITE,
        )
        .map_err(|_| "no room for a child's image")?;
    side.put(IMAGE_AT, &file)?;
    let image = side.handle(
        nr::VMO_CREATE,
        &[len],
        "vmo_create for a child's image failed",
    )?;
    side.put(OFFSET, &0_u64.to_ne_bytes())?;
    let _ = side
        .call(nr::VMO_WRITE, &[reg(image), IMAGE_AT, len, OFFSET])
        .map_err(|_| "a child's image could not be written")?;
    side.put(NAME_AT, CHILD_NAME)?;
    let child = side.handle(
        nr::PROCESS_CREATE,
        &[reg(root), reg(image), NAME_AT, CHILD_NAME.len() as u64],
        "process_create failed",
    )?;
    let (bootstrap, _other) = channel(side)?;
    let kept = side.handle(
        nr::HANDLE_DUPLICATE,
        &[reg(bootstrap), u64::from((Rights::READ | Rights::WRITE).0)],
        "handle_duplicate without TRANSFER failed",
    )?;
    refused(
        side.call(nr::PROCESS_START, &[reg(child), reg(kept)]),
        status::ACCESS_DENIED,
        "a child was started with a bootstrap handle that may not be transferred",
        report,
    )?;
    if side.call(nr::HANDLE_CLOSE, &[reg(kept)]).is_err() {
        return Err("a refused start took the bootstrap handle it was given");
    }
    Ok(())
}
