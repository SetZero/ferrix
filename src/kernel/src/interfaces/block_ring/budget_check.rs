//! The driver's `port_queue` takes at most [`INLINE_BUDGET`] completions off
//! the ring in one call, and leaves the rest to the ring's task.
//!
//! The completion bell is taken in the driver's own system call
//! ([`RingDisk`]'s [`Server`]), so a driver posting completions as fast as the
//! kernel takes them could otherwise keep that one call in the kernel, under
//! the disk's lock, for as long as it liked. This check builds a disk served
//! by a ring of the kernel's own -- a few pages of an anonymous VMO, read and
//! written through the same [`Pages`] a served ring uses -- puts more
//! commands on it than the budget, lets a real [`DriverSide`] complete them
//! all, and rings the bell as the driver would. What is required: the first
//! call takes exactly the budget, declines the bell so that it is queued for
//! the ring's task, and leaves the disk marked as having work for the task;
//! the second takes the rest and keeps the bell.
//!
//! The commands are flushes, which use no data region and answer nobody: the
//! check needs the ring's completions, not callers. They are counted on
//! `/proc/ferrix-seam` as any block ring's are, submitted and completed
//! alike, and taken back off [`TAKEN_INLINE`], which the driver check reads
//! as the driver's own.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use ferrix_blkring::{
    BELL_COMPLETE, Consumed, Device, DeviceFlags, DriverSide, KernelSide, RingLayout, Slot, Status,
    Submission,
};
use ferrix_block::Limits;
use ferrix_bootinfo::PAGE_SIZE;

use super::{INLINE_BUDGET, Live, MAX_PARTS, Pages, Regions, RingDisk, TAKEN_INLINE};
use crate::object::port::{Port, Server};
use crate::user::vmo::Vmo;

/// The ring's entries: room for every command the check posts.
const ENTRIES: u32 = 128;
/// Commands posted: the budget and half as many again.
const POSTED: usize = INLINE_BUDGET + INLINE_BUDGET / 2;
const _: () = assert!(POSTED <= ENTRIES as usize, "the ring holds every command");
/// One sector.
const BLOCK: u32 = 512;

/// Run the check. `Err` names the first thing that was not true.
///
/// # Errors
///
/// What was not true.
pub(crate) fn run() -> Result<(), &'static str> {
    let layout = RingLayout::standard(ENTRIES).map_err(|_| "the budget check's layout")?;
    let ring_bytes = layout.ring_bytes();
    let ring = Vmo::new_anonymous(ring_bytes.div_ceil(PAGE_SIZE))
        .map_err(|_| "no memory for the budget check's ring")?;
    let ring_held = ring
        .hold(0, ring.len_pages())
        .map_err(|_| "the budget check's ring could not be held")?;
    let data = Vmo::new_anonymous(1).map_err(|_| "no memory for the budget check's data")?;
    let data_held = data
        .hold(0, 1)
        .map_err(|_| "the budget check's data could not be held")?;
    let device = Device::new(BLOCK, 1024, 1, DeviceFlags::default(), PAGE_SIZE)
        .map_err(|_| "the budget check's device is not one HELLO could describe")?;
    let mut driver = DriverSide::new(
        Pages::over(&ring_held, ring_bytes),
        ring_bytes,
        layout,
        device,
    )
    .map_err(|_| "the budget check's driver refused its own layout")?;
    let mut storage = Vec::new();
    storage
        .try_reserve_exact(ENTRIES as usize)
        .map_err(|_| "no memory for the budget check's slots")?;
    storage.resize(ENTRIES as usize, Slot::EMPTY);
    let side = KernelSide::attach_owned(
        Pages::over(&ring_held, ring_bytes),
        ring_bytes,
        device,
        storage.into_boxed_slice(),
    )
    .map_err(|_| "the kernel refused the budget check's ring")?;
    let limits = Limits::new(BLOCK, 1024, 1, MAX_PARTS, ENTRIES)
        .map_err(|_| "the budget check's queue limits")?;
    let fresh = Regions::over(data_held, PAGE_SIZE, &device, 1)
        .ok_or("no memory for the budget check's regions")?;
    let disk = RingDisk::new(device, limits);
    disk.serve_from(Live {
        side,
        data: fresh.data,
        free: fresh.free,
        leases: fresh.leases,
        held_back: false,
        for_task: false,
        driver_port: Port::new().map_err(|_| "no memory for a port")?,
        kernel_port: Port::new().map_err(|_| "no memory for a port")?,
        _ring_held: ring_held,
    });

    post(&disk)?;
    complete_all(&mut driver)?;

    let kept_first = disk.take_packet(BELL_COMPLETE, [0, 0]);
    let (left_first, for_task) = outstanding(&disk)?;
    let kept_second = disk.take_packet(BELL_COMPLETE, [0, 0]);
    let (left_second, _) = outstanding(&disk)?;
    // Not the driver's: the driver check counts only its own.
    let _ = TAKEN_INLINE.fetch_sub(POSTED.saturating_sub(left_second) as u64, Ordering::Relaxed);

    let first = POSTED.saturating_sub(left_first);
    let second = left_first.saturating_sub(left_second);
    if first > INLINE_BUDGET {
        return Err("the driver's port_queue took more completions in one call than its budget");
    }
    if first < INLINE_BUDGET {
        return Err(
            "the driver's port_queue took fewer completions than its budget with more posted",
        );
    }
    if kept_first || !for_task {
        return Err("the driver's port_queue kept a bell it left completions past its budget for");
    }
    if left_second != 0 || !kept_second {
        return Err("the driver's port_queue did not take what its budget had left on the ring");
    }
    crate::console::println!(
        "  budget   {POSTED} completions posted at once: {first} taken by the driver's first \
         port_queue, its bell left to the ring's task, {second} by its second"
    );
    Ok(())
}

/// Put [`POSTED`] flushes on the disk's ring and publish them.
fn post(disk: &RingDisk) -> Result<(), &'static str> {
    let mut state = disk.state.lock();
    let live = state
        .live
        .as_mut()
        .ok_or("the budget check's disk lost its ring")?;
    for id in 0..POSTED as u64 {
        live.side
            .submit(Submission::flush(id))
            .map_err(|_| "the kernel would not submit the budget check's flush")?;
        crate::fs::seam::submitted();
    }
    let _ = live.side.publish();
    Ok(())
}

/// The driver takes every command and completes each.
fn complete_all(driver: &mut DriverSide<Pages>) -> Result<(), &'static str> {
    for _ in 0..POSTED {
        match driver.consume() {
            Ok(Some(Consumed::Request(request))) => driver
                .complete(request.id, Status::Ok, 0)
                .map_err(|_| "the budget check's driver could not complete a flush")?,
            _ => return Err("the budget check's driver did not take every flush"),
        }
    }
    let _ = driver.publish();
    Ok(())
}

/// The commands still on the disk's ring, and whether the disk has work
/// left for the ring's task.
fn outstanding(disk: &RingDisk) -> Result<(usize, bool), &'static str> {
    let state = disk.state.lock();
    let live = state
        .live
        .as_ref()
        .ok_or("the budget check's disk lost its ring")?;
    Ok((live.side.outstanding(), live.for_task))
}
