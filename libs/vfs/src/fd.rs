//! File descriptor tables.
//!
//! A descriptor is a small integer naming an open file description, plus one
//! bit of its own: close-on-exec. Everything else — the offset, the access
//! mode, `O_APPEND` — belongs to the description and is shared by every
//! descriptor that names it.
//!
//! Generic over what a slot holds, so the numbering rules are tested here on
//! integers and the kernel stores `Arc<OpenFile>`. The rules are small and
//! every one of them is load-bearing for a shell:
//!
//! * a new descriptor is the **lowest free number** — `close(1); open(...)`
//!   is how a program redirects its own output, and `2>&1` depends on it;
//! * `dup2` onto a live descriptor replaces it, and hands the old description
//!   back so the caller can release it outside whatever lock holds the table;
//! * `F_DUPFD` finds the lowest free number *at or above* a minimum;
//! * a table has a limit, `RLIMIT_NOFILE`, and a full table is `EMFILE`.

use alloc::vec::Vec;

use ferrix_linux_abi::errno::Errno;

use crate::Result;

/// `RLIMIT_NOFILE`'s soft default on Linux.
pub const DEFAULT_LIMIT: u32 = 1024;

/// The most `RLIMIT_NOFILE` may be raised to: Linux's default `nr_open`.
pub const MAX_LIMIT: u32 = 1 << 20;

/// One live descriptor.
#[derive(Debug, Clone)]
struct Slot<T> {
    item: T,
    cloexec: bool,
}

/// A process's descriptors.
///
/// `Clone` is `fork` without `CLONE_FILES`: the child gets its own table
/// naming the same descriptions.
#[derive(Debug, Clone)]
pub struct FdTable<T> {
    slots: Vec<Option<Slot<T>>>,
    limit: u32,
    open: usize,
}

impl<T> Default for FdTable<T> {
    fn default() -> Self {
        FdTable::new()
    }
}

impl<T> FdTable<T> {
    /// An empty table with the default limit.
    #[must_use]
    pub const fn new() -> FdTable<T> {
        FdTable {
            slots: Vec::new(),
            limit: DEFAULT_LIMIT,
            open: 0,
        }
    }

    /// The limit on descriptor numbers: every descriptor is below it.
    #[must_use]
    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Change the limit. Descriptors already at or above it stay open, as
    /// they do on Linux; only new ones are constrained.
    ///
    /// # Errors
    ///
    /// `EPERM` above [`MAX_LIMIT`].
    pub fn set_limit(&mut self, limit: u32) -> Result<()> {
        if limit > MAX_LIMIT {
            return Err(Errno::EPERM);
        }
        self.limit = limit;
        Ok(())
    }

    /// How many descriptors are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.open
    }

    /// Whether none are.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open == 0
    }

    fn slot(&self, fd: i32) -> Result<&Slot<T>> {
        let index = usize::try_from(fd).map_err(|_| Errno::EBADF)?;
        self.slots
            .get(index)
            .and_then(Option::as_ref)
            .ok_or(Errno::EBADF)
    }

    /// What `fd` names.
    ///
    /// # Errors
    ///
    /// `EBADF` for a negative or closed descriptor.
    pub fn get(&self, fd: i32) -> Result<&T> {
        self.slot(fd).map(|slot| &slot.item)
    }

    /// Whether `fd` is close-on-exec.
    ///
    /// # Errors
    ///
    /// `EBADF` for a negative or closed descriptor.
    pub fn cloexec(&self, fd: i32) -> Result<bool> {
        self.slot(fd).map(|slot| slot.cloexec)
    }

    /// `fcntl(F_SETFD)`.
    ///
    /// # Errors
    ///
    /// `EBADF` for a negative or closed descriptor.
    pub fn set_cloexec(&mut self, fd: i32, cloexec: bool) -> Result<()> {
        let index = usize::try_from(fd).map_err(|_| Errno::EBADF)?;
        let slot = self
            .slots
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or(Errno::EBADF)?;
        slot.cloexec = cloexec;
        Ok(())
    }

    /// Install `item` at the lowest free descriptor.
    ///
    /// # Errors
    ///
    /// `EMFILE` if every number below the limit is taken.
    pub fn insert(&mut self, item: T, cloexec: bool) -> Result<i32> {
        self.insert_from(0, item, cloexec)
    }

    /// Install `item` at the lowest free descriptor at or above `min`:
    /// `fcntl(F_DUPFD)`.
    ///
    /// # Errors
    ///
    /// `EINVAL` for a negative `min` or one at or above the limit, `EMFILE`
    /// if nothing is free from there.
    pub fn insert_from(&mut self, min: i32, item: T, cloexec: bool) -> Result<i32> {
        let min = usize::try_from(min).map_err(|_| Errno::EINVAL)?;
        let limit = usize::try_from(self.limit).map_err(|_| Errno::EINVAL)?;
        if min >= limit {
            return Err(Errno::EINVAL);
        }
        let free = (min..limit)
            .find(|&index| self.slots.get(index).is_none_or(Option::is_none))
            .ok_or(Errno::EMFILE)?;
        let fd = i32::try_from(free).map_err(|_| Errno::EMFILE)?;
        let _ = self.put(free, item, cloexec);
        Ok(fd)
    }

    /// Install `item` at exactly `fd`, replacing what was there: `dup2` and
    /// `dup3`. The replaced item is returned for the caller to release.
    ///
    /// # Errors
    ///
    /// `EBADF` for a negative `fd` or one at or above the limit.
    pub fn install(&mut self, fd: i32, item: T, cloexec: bool) -> Result<Option<T>> {
        let index = usize::try_from(fd).map_err(|_| Errno::EBADF)?;
        if index >= usize::try_from(self.limit).map_err(|_| Errno::EBADF)? {
            return Err(Errno::EBADF);
        }
        Ok(self.put(index, item, cloexec))
    }

    fn put(&mut self, index: usize, item: T, cloexec: bool) -> Option<T> {
        if self.slots.len() <= index {
            self.slots.resize_with(index.saturating_add(1), || None);
        }
        let slot = self.slots.get_mut(index)?;
        let old = slot.replace(Slot { item, cloexec });
        if old.is_none() {
            self.open = self.open.saturating_add(1);
        }
        old.map(|slot| slot.item)
    }

    /// `close`: remove `fd` and return what it named.
    ///
    /// # Errors
    ///
    /// `EBADF` for a negative or closed descriptor.
    pub fn remove(&mut self, fd: i32) -> Result<T> {
        let index = usize::try_from(fd).map_err(|_| Errno::EBADF)?;
        let slot = self
            .slots
            .get_mut(index)
            .and_then(Option::take)
            .ok_or(Errno::EBADF)?;
        self.open = self.open.saturating_sub(1);
        while self.slots.last().is_some_and(Option::is_none) {
            let _ = self.slots.pop();
        }
        Ok(slot.item)
    }

    /// Remove every close-on-exec descriptor, for `execve`, returning what
    /// they named.
    pub fn take_cloexec(&mut self) -> Vec<T> {
        let mut taken = Vec::new();
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|s| s.cloexec)
                && let Some(s) = slot.take()
            {
                taken.push(s.item);
            }
        }
        self.open = self.open.saturating_sub(taken.len());
        while self.slots.last().is_some_and(Option::is_none) {
            let _ = self.slots.pop();
        }
        taken
    }

    /// Every open descriptor, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = (i32, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            let fd = i32::try_from(index).ok()?;
            slot.as_ref().map(|slot| (fd, &slot.item))
        })
    }
}
