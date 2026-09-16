//! Epoll sets: the object behind `epoll_create1`'s descriptor.
//!
//! An epoll set is a list of registrations, each a descriptor number, the open
//! file it named when it was added, the events asked for and the program's
//! 64-bit cookie. What a wait reports is asked of each file's
//! [`OpenFile::poll`] at the moment of the wait, as `poll` asks: nothing here
//! is queued when a file changes. Linux keeps a ready list its files' wake-ups
//! fill instead, and the difference a program can see is in the two things
//! that list is for.
//!
//! # Edge-triggered registrations
//!
//! `EPOLLET` reports a file once when it becomes ready and again only when
//! something happens to it: new data, room made, the peer leaving. Linux
//! knows that happened because the file woke its wait queue. Here every
//! pollable file answers [`OpenFile::poll_changes`], a count of those wakes,
//! and a registration is due a report when the count moved or a readiness bit
//! appeared since the set last looked -- the second so that an object whose
//! readiness changes without a wake is not missed. A due report is kept until
//! a wait delivers it, and dropped, as on Linux, if the file is no longer
//! ready by then.
//!
//! # Registrations and closing
//!
//! A registration is keyed by the open file and the number together, as
//! Linux keys it, and holds the file weakly: closing the number leaves the
//! registration reporting while a `dup` of it keeps the file open, and the
//! registration goes when the file does. That is Linux's documented surprise,
//! kept because a program that relies on it exists.
//!
//! # Nesting
//!
//! An epoll set is itself pollable: readable when a wait on it would report
//! something. So one can be registered in another, which is how a Wayland
//! server's event loop sits inside its toolkit's. `epoll_ctl` refuses a set
//! that would contain itself, and a chain of sets deeper than Linux's
//! `EP_MAX_NESTS`, with `ELOOP`, looking down from the set being added and up
//! from the set it is added to. A set knows which sets hold it for the upward
//! look.
//!
//! # Locks
//!
//! A set's lock is never held while a file is polled: polling a nested set
//! takes that set's lock, and polling a socket takes the net core's. A scan
//! copies the registrations out, polls them unlocked, and takes the lock again
//! to record what it saw against whichever registrations are still there.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;

use ferrix_linux_abi::types::{
    EPOLLERR, EPOLLET, EPOLLHUP, EPOLLIN, EPOLLONESHOT, EPOLLOUT, EPOLLRDNORM, EPOLLWRNORM,
};
use ferrix_vfs::{Errno, Inode, Metadata, OpenFile, Readiness};

use crate::fs;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

/// How deep sets may nest: Linux's `EP_MAX_NESTS`. A chain may hold this many
/// sets below the top one.
pub(crate) const MAX_NESTS: usize = 4;

/// The name `/proc/self/fd` shows.
const NAME: &[u8] = b"anon_inode:[eventpoll]";

/// One registration.
#[derive(Debug)]
struct Item {
    /// The number it was added under.
    fd: i32,
    /// The open file's address, which with `fd` is the key. Stable for as
    /// long as `file` is held, because a weak reference keeps the allocation.
    key: usize,
    /// The file, held weakly.
    file: Weak<OpenFile>,
    /// The set it is, when it is one.
    nested: Option<Weak<Epoll>>,
    /// The events asked for, with `EPOLLERR` and `EPOLLHUP`, which are
    /// always asked for, and the flags.
    events: u32,
    /// The program's cookie.
    data: u64,
    /// Whether it may report: `EPOLLONESHOT` clears it once it has.
    armed: bool,
    /// For `EPOLLET`: a report is due.
    due: bool,
    /// For `EPOLLET`: the file's change count when the set last looked.
    seen_changes: u64,
    /// For `EPOLLET`: the readiness bits when the set last looked.
    seen_bits: u32,
}

/// What a set holds.
#[derive(Debug, Default)]
struct State {
    /// The registrations, in the order waits consider them.
    items: Vec<Item>,
    /// The sets this one is registered in, for the depth check.
    parents: Vec<Weak<Epoll>>,
    /// Bumped by every change to `items`, so a set holding this one sees a
    /// change.
    generation: u64,
}

/// One event a wait may deliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Event {
    /// The events that happened, within those asked for.
    pub(crate) events: u32,
    /// The program's cookie.
    pub(crate) data: u64,
    /// Which registration, for [`Epoll::delivered`].
    key: (usize, i32),
}

/// What `epoll_ctl` asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Interest {
    /// `epoll_event.events`.
    pub(crate) events: u32,
    /// `epoll_event.data`.
    pub(crate) data: u64,
}

/// An epoll set.
pub(crate) struct Epoll {
    state: SpinLock<State>,
    /// Itself, so a registration in another set can name it weakly.
    this: Weak<Epoll>,
    /// Woken when a registration is added, changed or removed, so a wait on
    /// the set sees a file added while it sleeps, as `ep_insert` wakes one.
    changed: Arc<WaitQueue>,
}

impl fmt::Debug for Epoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Epoll").finish_non_exhaustive()
    }
}

/// A new epoll set, as the open file `epoll_create1` installs.
///
/// # Errors
///
/// Whatever [`OpenFile::new`] refuses, which for an epoll set is nothing.
pub(crate) fn create() -> Result<Arc<OpenFile>, Errno> {
    let set = Arc::new_cyclic(|this| Epoll {
        state: SpinLock::new(State::default()),
        this: this.clone(),
        changed: Arc::new(WaitQueue::new()),
    });
    fs::anon::open(set, NAME, false)
}

/// The epoll set an open file is, if it is one.
pub(crate) fn of(file: &OpenFile) -> Option<Arc<Epoll>> {
    Arc::clone(file.io()).into_any().downcast::<Epoll>().ok()
}

/// A file's readiness as epoll bits.
const fn bits(ready: Readiness) -> u32 {
    let mut bits = 0;
    if ready.readable {
        bits |= EPOLLIN | EPOLLRDNORM;
    }
    if ready.writable {
        bits |= EPOLLOUT | EPOLLWRNORM;
    }
    if ready.hangup {
        bits |= EPOLLHUP;
    }
    if ready.error {
        bits |= EPOLLERR;
    }
    bits
}

/// What one look at a registration's file found: its bits and change count,
/// or nothing when the file is gone.
type Look = Option<(u32, u64)>;

/// Look at a file, with no lock held.
fn look(file: &Weak<OpenFile>) -> Look {
    let file = file.upgrade()?;
    let changes = file.poll_changes().unwrap_or(0);
    Some((bits(file.poll()), changes))
}

impl Epoll {
    /// `EPOLL_CTL_ADD`.
    ///
    /// # Errors
    ///
    /// `EEXIST` for a file and number already registered; `ELOOP` for a set
    /// that would contain itself or nest too deep.
    pub(crate) fn add(
        &self,
        fd: i32,
        file: &Arc<OpenFile>,
        interest: Interest,
    ) -> Result<(), Errno> {
        let nested = of(file);
        if let Some(inner) = &nested {
            self.check_nesting(inner)?;
        }
        let key = key_of(file);
        let weak = Arc::downgrade(file);
        // Looked at before the lock, as every look is.
        let seen = look(&weak).unwrap_or((0, 0));
        {
            let mut state = self.state.lock();
            if state
                .items
                .iter()
                .any(|item| (item.key, item.fd) == (key, fd))
            {
                return Err(Errno::EEXIST);
            }
            let events = interest.events | EPOLLERR | EPOLLHUP;
            state.items.push(Item {
                fd,
                key,
                file: weak,
                nested: nested.as_ref().map(Arc::downgrade),
                events,
                data: interest.data,
                armed: true,
                due: seen.0 & events != 0,
                seen_changes: seen.1,
                seen_bits: seen.0,
            });
            state.generation = state.generation.wrapping_add(1);
        }
        if let Some(inner) = nested {
            inner.state.lock().parents.push(self.this.clone());
        }
        self.changed.wake_all();
        Ok(())
    }

    /// `EPOLL_CTL_MOD`: new events and cookie, armed again, and due a report
    /// if the file is ready for them now.
    ///
    /// # Errors
    ///
    /// `ENOENT` for a file and number not registered; `EINVAL` for one
    /// registered with `EPOLLEXCLUSIVE`, which may not be modified.
    pub(crate) fn modify(
        &self,
        fd: i32,
        file: &Arc<OpenFile>,
        interest: Interest,
    ) -> Result<(), Errno> {
        self.change(fd, file, interest)?;
        self.changed.wake_all();
        Ok(())
    }

    /// [`Epoll::modify`]'s change, under the lock.
    fn change(&self, fd: i32, file: &Arc<OpenFile>, interest: Interest) -> Result<(), Errno> {
        let key = key_of(file);
        let seen = look(&Arc::downgrade(file)).unwrap_or((0, 0));
        let mut state = self.state.lock();
        let item = state
            .items
            .iter_mut()
            .find(|item| (item.key, item.fd) == (key, fd))
            .ok_or(Errno::ENOENT)?;
        if item.events & ferrix_linux_abi::types::EPOLLEXCLUSIVE != 0 {
            return Err(Errno::EINVAL);
        }
        item.events = interest.events | EPOLLERR | EPOLLHUP;
        item.data = interest.data;
        item.armed = true;
        item.due = seen.0 & item.events != 0;
        item.seen_changes = seen.1;
        item.seen_bits = seen.0;
        state.generation = state.generation.wrapping_add(1);
        Ok(())
    }

    /// `EPOLL_CTL_DEL`.
    ///
    /// # Errors
    ///
    /// `ENOENT` for a file and number not registered.
    pub(crate) fn remove(&self, fd: i32, file: &Arc<OpenFile>) -> Result<(), Errno> {
        let key = key_of(file);
        let removed = {
            let mut state = self.state.lock();
            let at = state
                .items
                .iter()
                .position(|item| (item.key, item.fd) == (key, fd))
                .ok_or(Errno::ENOENT)?;
            state.generation = state.generation.wrapping_add(1);
            state.items.remove(at)
        };
        if let Some(inner) = removed.nested.as_ref().and_then(Weak::upgrade) {
            let mut state = inner.state.lock();
            if let Some(at) = state
                .parents
                .iter()
                .position(|parent| Weak::ptr_eq(parent, &self.this))
            {
                let _ = state.parents.remove(at);
            }
        }
        self.changed.wake_all();
        Ok(())
    }

    /// How many registrations it has, for the checks.
    pub(crate) fn len(&self) -> usize {
        self.state.lock().items.len()
    }

    /// `ELOOP` if adding `inner` to this set would make a set contain itself,
    /// or a chain more than [`MAX_NESTS`] sets below its top, as
    /// `ep_loop_check` refuses.
    fn check_nesting(&self, inner: &Arc<Epoll>) -> Result<(), Errno> {
        let below = inner.depth_below(&self.this, 0).ok_or(Errno::ELOOP)?;
        let above = self.depth_above(0).ok_or(Errno::ELOOP)?;
        if below + 1 + above > MAX_NESTS {
            return Err(Errno::ELOOP);
        }
        Ok(())
    }

    /// How many sets deep the longest chain below this one goes, or `None`
    /// if `target` is among them or the chain is deeper than any allowed.
    fn depth_below(&self, target: &Weak<Epoll>, depth: usize) -> Option<usize> {
        if Weak::ptr_eq(&self.this, target) || depth > MAX_NESTS {
            return None;
        }
        let children: Vec<Arc<Epoll>> = self
            .state
            .lock()
            .items
            .iter()
            .filter_map(|item| item.nested.as_ref().and_then(Weak::upgrade))
            .collect();
        let mut deepest = 0;
        for child in children {
            deepest = deepest.max(child.depth_below(target, depth + 1)? + 1);
        }
        Some(deepest)
    }

    /// How many sets deep the longest chain above this one goes, or `None`
    /// past any allowed.
    fn depth_above(&self, depth: usize) -> Option<usize> {
        if depth > MAX_NESTS {
            return None;
        }
        let parents: Vec<Arc<Epoll>> = {
            let mut state = self.state.lock();
            state.parents.retain(|parent| parent.strong_count() > 0);
            state.parents.iter().filter_map(Weak::upgrade).collect()
        };
        let mut highest = 0;
        for parent in parents {
            highest = highest.max(parent.depth_above(depth + 1)? + 1);
        }
        Some(highest)
    }

    /// What a wait would deliver now, at most `limit` events, without
    /// delivering it: [`Epoll::delivered`] does that for the ones the program
    /// received. A registration whose file is gone is dropped here.
    pub(crate) fn ready(&self, limit: usize) -> Vec<Event> {
        let snapshot: Vec<((usize, i32), Weak<OpenFile>)> = self
            .state
            .lock()
            .items
            .iter()
            .map(|item| ((item.key, item.fd), item.file.clone()))
            .collect();
        let looks: Vec<((usize, i32), Look)> = snapshot
            .into_iter()
            .map(|(key, file)| (key, look(&file)))
            .collect();

        let mut events = Vec::new();
        let mut state = self.state.lock();
        let mut dropped = false;
        for (key, seen) in looks {
            let Some(at) = state
                .items
                .iter()
                .position(|item| (item.key, item.fd) == key)
            else {
                continue;
            };
            let Some((now, changes)) = seen else {
                let _ = state.items.remove(at);
                dropped = true;
                continue;
            };
            let Some(item) = state.items.get_mut(at) else {
                continue;
            };
            let wanted = now & item.events;
            if item.events & EPOLLET != 0 {
                let happened = changes != item.seen_changes || now & !item.seen_bits != 0;
                if happened && wanted != 0 {
                    item.due = true;
                }
                item.seen_changes = changes;
                item.seen_bits = now;
            }
            let reports = item.armed && wanted != 0 && (item.events & EPOLLET == 0 || item.due);
            if reports && events.len() < limit {
                events.push(Event {
                    events: wanted,
                    data: item.data,
                    key,
                });
            }
        }
        if dropped {
            state.generation = state.generation.wrapping_add(1);
        }
        events
    }

    /// Record that the program received `events`, the first of what
    /// [`Epoll::ready`] answered: an edge-triggered registration's report is
    /// no longer due, a one-shot one is disarmed, and a level-triggered one
    /// that is still ready moves behind the others, so the next wait with a
    /// small limit reaches them, as Linux puts it back at the end of its
    /// ready list.
    pub(crate) fn delivered(&self, events: &[Event]) {
        let mut state = self.state.lock();
        let mut behind = Vec::new();
        for event in events {
            let Some(at) = state
                .items
                .iter()
                .position(|item| (item.key, item.fd) == event.key)
            else {
                continue;
            };
            let Some(item) = state.items.get_mut(at) else {
                continue;
            };
            item.due = false;
            if item.events & EPOLLONESHOT != 0 {
                item.armed = false;
            } else if item.events & EPOLLET == 0 {
                behind.push(state.items.remove(at));
            }
        }
        state.items.extend(behind);
    }
}

/// The address a registration keys a file by.
fn key_of(file: &Arc<OpenFile>) -> usize {
    Arc::as_ptr(file).addr()
}

impl Inode for Epoll {
    fn metadata(&self) -> Metadata {
        fs::anon::metadata()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    /// Readable when a wait would report something, as `ep_eventpoll_poll`
    /// answers. Never writable: Linux's answer has only `EPOLLIN`.
    fn poll(&self) -> Readiness {
        Readiness {
            readable: !self.ready(1).is_empty(),
            writable: false,
            hangup: false,
            error: false,
        }
    }

    /// Its own queue, and every registered file's.
    fn poll_queues(&self, visit: &mut dyn FnMut(ferrix_vfs::WakeSource)) -> bool {
        visit(fs::wake::shared(&self.changed));
        let files: Vec<Arc<OpenFile>> = self
            .state
            .lock()
            .items
            .iter()
            .filter_map(|item| item.file.upgrade())
            .collect();
        let mut trusted = true;
        for file in files {
            trusted &= file.poll_queues(visit);
        }
        trusted
    }

    /// The set's own changes, and every file's in it.
    fn poll_changes(&self) -> Option<u64> {
        let (generation, files): (u64, Vec<Weak<OpenFile>>) = {
            let state = self.state.lock();
            (
                state.generation,
                state.items.iter().map(|item| item.file.clone()).collect(),
            )
        };
        let mut sum = generation;
        for file in files {
            if let Some(changes) = file.upgrade().and_then(|file| file.poll_changes()) {
                sum = sum.wrapping_add(changes);
            }
        }
        Some(sum)
    }
}
