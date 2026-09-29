//! Kernel memory held for a program, charged to its job.
//!
//! The job quotas (`kernel/src/object/quota.rs`, certification finding F-35)
//! charge a job for the frames of its programs' memory, their native objects
//! and their tasks. What they left out is the kernel heap a program drives
//! through the Linux personality and the libraries under it -- a file's
//! inode and name in tmpfs, a pipe's buffer, a region of an address space, a
//! descriptor in flight -- which is finding F-37. This crate is how that heap
//! is charged: a [`Charge`] is made where the allocation is, to the job of
//! the task that caused it, lives inside what it pays for, and takes the
//! charge back when it is dropped. Linux does the same with
//! `GFP_KERNEL_ACCOUNT` and an object's `obj_cgroup`, and folds it into
//! `memory.max` as the kernel does here.
//!
//! # Why a crate, and a hook
//!
//! The allocations are in libraries -- `ferrix-vfs`, `ferrix-vma` -- as much
//! as in the kernel, and a library cannot name the kernel's quota table. So
//! the token is here, and what it calls is an [`Account`] the kernel
//! [`install`]s once at boot. Until one is, and in a host test that installs
//! none, a charge is to nobody and always succeeds, which is what a program
//! in the job tree's root pays too: the kernel's account answers
//! [`NOBODY`] for it.
//!
//! # What a charge is worth
//!
//! Bytes of heap. [`footprint`] says what one allocation of a size really
//! takes from the kernel heap -- its size class, or the power-of-two run of
//! pages a large one is served from -- so a site that charges
//! [`Charge::boxed`], [`Charge::arc`] or [`Charge::bytes`] charges what the
//! heap gave, not what was asked. A site that charges several allocations
//! at once sums them; it may leave out a small fixed part, and says so.
//!
//! # A charge outlives its job
//!
//! A charge names its job by a slot index, and the kernel keeps a slot as
//! long as a charge holds it ([`Account::hold`]). So an object that outlives
//! the job that made it -- a file passed to another job over a socket, an
//! inode in a shared tmpfs -- stays charged to the job that made it until it
//! goes, as a page does, and as Linux's `obj_cgroup` keeps it.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "testing"))]
extern crate std;

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::sync::atomic::AtomicUsize;

use ferrix_heap::{CLASS_SIZES, Heap, PAGE_SIZE, Request};
use ferrix_sync::Once;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

/// No job: the tree's root, a kernel thread, or no account installed.
pub const NOBODY: u32 = u32::MAX;

/// A charge a limit refused. The caller answers `ENOMEM`, as running out
/// of memory is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused;

/// What the kernel does with a charge. Every method is atomics only: a
/// charge is made under spin locks and dropped under them.
pub trait Account: Sync {
    /// The job the running task is charged to, or [`NOBODY`].
    fn current(&self) -> u32;

    /// Charge `bytes` to `owner` and every job above it, if every limit on
    /// the way allows it. Never called for [`NOBODY`].
    ///
    /// # Errors
    ///
    /// [`Refused`], with nothing charged anywhere.
    fn charge(&self, owner: u32, bytes: u64) -> Result<(), Refused>;

    /// Take back `bytes` from `owner` and every job above it. Never called
    /// for [`NOBODY`].
    fn uncharge(&self, owner: u32, bytes: u64);

    /// Keep `owner`'s slot while a charge names it.
    fn hold(&self, owner: u32);

    /// Let go of what [`Account::hold`] kept.
    fn release(&self, owner: u32);
}

/// The account, once the kernel has installed one.
static ACCOUNT: Once<&'static dyn Account> = Once::new();

/// Install the account. The first call wins; the kernel makes it once, at
/// boot, before the first program runs.
pub fn install(account: &'static dyn Account) {
    let _ = ACCOUNT.call_once(|| account);
}

/// The account, if there is one.
fn account() -> Option<&'static dyn Account> {
    ACCOUNT.get().copied()
}

/// The job a charge made now would go to: the running task's, or
/// [`NOBODY`]. For an object that charges later growth to whoever made it,
/// such as a queue a peer's writes fill.
#[must_use]
pub fn current() -> u32 {
    account().map_or(NOBODY, Account::current)
}

/// What an allocation of `size` bytes aligned to `align` takes from the
/// kernel heap: its size class, or the pages a large one is served from.
#[must_use]
pub fn footprint(size: usize, align: usize) -> usize {
    let request = Request::new(size, align);
    match Heap::class_of(request) {
        Some(class) => CLASS_SIZES.get(class).copied().unwrap_or(size),
        None => Heap::order_of(request)
            .ok()
            .and_then(|order| PAGE_SIZE.checked_shl(u32::from(order)))
            .unwrap_or(size),
    }
}

/// What a `Box<T>` takes from the heap.
#[must_use]
pub fn boxed_footprint<T>() -> usize {
    let layout = Layout::new::<T>();
    footprint(layout.size(), layout.align())
}

/// What an `Arc<T>` takes from the heap: the value and its two counts.
#[must_use]
pub fn arc_footprint<T>() -> usize {
    let layout = Layout::new::<[AtomicUsize; 2]>()
        .extend(Layout::new::<T>())
        .map_or(Layout::new::<T>(), |(layout, _)| layout.pad_to_align());
    footprint(layout.size(), layout.align())
}

/// What a buffer of `count` `T`s takes from the heap: a `Vec`'s or a
/// `VecDeque`'s of that capacity. Nothing for an empty one, which
/// allocates nothing.
#[must_use]
pub fn buffer_footprint<T>(count: usize) -> usize {
    let layout = Layout::new::<T>();
    match layout.size().checked_mul(count) {
        Some(0) => 0,
        Some(size) => footprint(size, layout.align()),
        None => usize::MAX,
    }
}

/// Bytes of kernel heap charged to a job, taken back as this is dropped.
///
/// Made where the allocation is and kept inside what was allocated, so that
/// whatever path frees the object frees the charge with it.
#[derive(Debug, PartialEq, Eq)]
pub struct Charge {
    /// The job charged, or [`NOBODY`].
    owner: u32,
    /// Bytes charged to it.
    bytes: u64,
}

impl Charge {
    /// Nothing, charged to nobody.
    #[must_use]
    pub const fn none() -> Charge {
        Charge {
            owner: NOBODY,
            bytes: 0,
        }
    }

    /// Charge `bytes` to the running task's job.
    ///
    /// # Errors
    ///
    /// [`Refused`] when a limit on the job or one above it would be passed.
    pub fn bytes(bytes: usize) -> Result<Charge, Refused> {
        Charge::to(current(), bytes)
    }

    /// Charge `bytes` to `owner`, which a caller had from [`current`] or
    /// from another charge's [`Charge::owner`].
    ///
    /// # Errors
    ///
    /// [`Refused`].
    pub fn to(owner: u32, bytes: usize) -> Result<Charge, Refused> {
        let bytes = u64::try_from(bytes).map_err(|_| Refused)?;
        if owner != NOBODY
            && let Some(account) = account()
        {
            // Nothing to refuse in nothing: a job over a limit lowered under
            // its use still gets a charge a buffer can grow later.
            if bytes != 0 {
                account.charge(owner, bytes)?;
            }
            account.hold(owner);
        }
        Ok(Charge { owner, bytes })
    }

    /// Charge the running task's job for a `Box<T>`.
    ///
    /// # Errors
    ///
    /// [`Refused`].
    pub fn boxed<T>() -> Result<Charge, Refused> {
        Charge::bytes(boxed_footprint::<T>())
    }

    /// Charge the running task's job for an `Arc<T>`.
    ///
    /// # Errors
    ///
    /// [`Refused`].
    pub fn arc<T>() -> Result<Charge, Refused> {
        Charge::bytes(arc_footprint::<T>())
    }

    /// The job charged, or [`NOBODY`].
    #[must_use]
    pub fn owner(&self) -> u32 {
        self.owner
    }

    /// Bytes charged. For a charge to [`NOBODY`], what would have been.
    #[must_use]
    pub fn charged(&self) -> u64 {
        self.bytes
    }

    /// Charge the same job `more` bytes: a buffer that grew.
    ///
    /// # Errors
    ///
    /// [`Refused`], with the charge as it was.
    pub fn grow(&mut self, more: usize) -> Result<(), Refused> {
        let more = u64::try_from(more).map_err(|_| Refused)?;
        let total = self.bytes.checked_add(more).ok_or(Refused)?;
        if more != 0
            && self.owner != NOBODY
            && let Some(account) = account()
        {
            account.charge(self.owner, more)?;
        }
        self.bytes = total;
        Ok(())
    }

    /// Take `less` bytes back from the job, at most what is charged: a
    /// buffer that shrank.
    pub fn shrink(&mut self, less: usize) {
        let less = u64::try_from(less).unwrap_or(u64::MAX).min(self.bytes);
        if less == 0 {
            return;
        }
        if self.owner != NOBODY
            && let Some(account) = account()
        {
            account.uncharge(self.owner, less);
        }
        self.bytes -= less;
    }

    /// Make the charge exactly `bytes`: grow it or shrink it.
    ///
    /// # Errors
    ///
    /// [`Refused`] for a growth, with the charge as it was.
    pub fn resize(&mut self, bytes: usize) -> Result<(), Refused> {
        let now = usize::try_from(self.bytes).unwrap_or(usize::MAX);
        if bytes > now {
            self.grow(bytes - now)
        } else {
            self.shrink(now - bytes);
            Ok(())
        }
    }
}

/// Make room in `vec` for `additional` more, with `charge` -- the vector's
/// own, which is its room's [`buffer_footprint`] -- grown first to what the
/// room will be: twice what it was, or what is needed if more. A vector
/// never gives room back, so neither does its charge until it goes.
///
/// # Errors
///
/// [`Refused`] when the charge or the allocation is, with the vector and
/// the charge as they were.
pub fn reserve<T>(vec: &mut Vec<T>, additional: usize, charge: &mut Charge) -> Result<(), Refused> {
    let (len, room) = (vec.len(), vec.capacity());
    let target = grown(len, room, additional)?;
    if target == room {
        return Ok(());
    }
    charge.resize(buffer_footprint::<T>(target))?;
    if vec.try_reserve_exact(target - len).is_err() {
        let _ = charge.resize(buffer_footprint::<T>(room));
        return Err(Refused);
    }
    // The allocator may round the room up; the charge follows it if it can,
    // and otherwise stays at what was asked, which is what was needed.
    let _ = charge.resize(buffer_footprint::<T>(vec.capacity()));
    Ok(())
}

/// [`reserve`] for a `VecDeque`.
///
/// # Errors
///
/// [`Refused`], with the queue and the charge as they were.
pub fn reserve_deque<T>(
    deque: &mut VecDeque<T>,
    additional: usize,
    charge: &mut Charge,
) -> Result<(), Refused> {
    let (len, room) = (deque.len(), deque.capacity());
    let target = grown(len, room, additional)?;
    if target == room {
        return Ok(());
    }
    charge.resize(buffer_footprint::<T>(target))?;
    if deque.try_reserve_exact(target - len).is_err() {
        let _ = charge.resize(buffer_footprint::<T>(room));
        return Err(Refused);
    }
    let _ = charge.resize(buffer_footprint::<T>(deque.capacity()));
    Ok(())
}

/// The room a buffer of `len` in `room` grows to for `additional` more:
/// `room` if they fit, else twice `room` or what is needed, at least four.
fn grown(len: usize, room: usize, additional: usize) -> Result<usize, Refused> {
    let needed = len.checked_add(additional).ok_or(Refused)?;
    if needed <= room {
        return Ok(room);
    }
    Ok(needed.max(room.saturating_mul(2)).max(4))
}

impl Default for Charge {
    fn default() -> Charge {
        Charge::none()
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        if self.owner != NOBODY
            && let Some(account) = account()
        {
            if self.bytes != 0 {
                account.uncharge(self.owner, self.bytes);
            }
            account.release(self.owner);
        }
    }
}
