//! A process's handle table.
//!
//! # A closed handle never names anything again
//!
//! The bug every handle or file-descriptor table has to design against: a
//! program closes handle 5, something else is opened and lands in slot 5, and
//! a stale copy of "5" held by another thread now acts on the new object.
//! With file descriptors POSIX requires exactly that reuse. With capabilities
//! it is a confused deputy — a driver's late reply written into whatever
//! channel happens to occupy the slot — so this table rules it out.
//!
//! A handle value is a slot index and a *generation*. Each slot counts how
//! many times it has been emptied, and a handle names the slot only if the
//! generations agree, so every close makes every earlier value for that slot
//! permanently stale. When a slot's generation would wrap, the slot is retired
//! and never used again. That costs at most one slot per 4095 closes of it,
//! and it turns "stale handles almost never alias" into "never", which is a
//! property a fuzzer can check rather than a probability someone has to trust.
//!
//! # All or nothing
//!
//! [`HandleTable::take_many`] and [`HandleTable::insert_many`] are what a
//! channel write and read are built on, and each either happens completely or
//! leaves the table exactly as it was. A send that moved three of five handles
//! and then failed would leave the caller unable to know which it still
//! holds.
//!
//! # Objects leave by value
//!
//! Every call that removes an object hands it back rather than dropping it.
//! Dropping the last reference to a channel endpoint wakes its peer, which
//! takes other locks; the kernel holds this table under its process's lock,
//! and must be able to let that lock go before the object dies.
//!
//! # A handle is a program's index
//!
//! Every lookup clamps the slot index with [`nospec::bounded`] before it
//! indexes, so a handle value past the table cannot steer a mispredicted
//! bounds check into memory beyond it (Spectre variant 1). A handle is the
//! one integer every native call takes from a program and uses as an array
//! index, which is why this table is where that defence has to be.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::{Requested, Rights};
use ferrix_sync::nospec;

/// Bits of a handle value that hold the generation.
const GENERATION_BITS: u32 = 12;
/// The generation part of a handle value.
const GENERATION_MASK: u32 = (1 << GENERATION_BITS) - 1;
/// The highest generation a slot may reach before it is retired.
const MAX_GENERATION: u32 = GENERATION_MASK;

/// The most slots a table can ever have.
///
/// Nineteen bits of index, not the twenty the value has room for. A native
/// call returns a new handle in the same register as an `errno`, and on a
/// 32-bit machine a value at or above 2^31 reads as negative — the top 4095
/// of them as an error. With nineteen, the largest handle is `0x7FFF_FFFF`.
pub const MAX_SLOTS: usize = 1 << (31 - GENERATION_BITS);

/// Why a table operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableError {
    /// The handle names nothing: never issued, already closed, or zero.
    BadHandle,
    /// The handle does not carry a right the operation needs, or a new
    /// handle would have had rights the original does not.
    AccessDenied,
    /// There is no room for another handle.
    Full,
    /// The same handle appears twice in one batch.
    Repeated,
    /// There was no memory for the table to grow.
    NoMemory,
}

impl From<ferrix_fallible::AllocError> for TableError {
    fn from(_: ferrix_fallible::AllocError) -> TableError {
        TableError::NoMemory
    }
}

/// One slot of the table.
#[derive(Debug)]
enum Slot<T> {
    /// Holding an object. `generation` is never zero, which is what keeps
    /// every handle value non-zero.
    Occupied {
        /// The slot's current generation.
        generation: u32,
        /// What the handle permits.
        rights: Rights,
        /// What it names.
        object: T,
    },
    /// Empty, and on the free list. The next occupant gets this generation.
    Vacant {
        /// The generation the next occupant will get.
        generation: u32,
    },
    /// Used up. Never on the free list and never occupied again.
    Retired,
}

/// What [`HandleTable::close`] hands back: every object the table held, in
/// slot order.
#[derive(Debug)]
pub struct Closed<T> {
    /// The table's slots, taken out of it.
    slots: alloc::vec::IntoIter<Slot<T>>,
}

impl<T> Iterator for Closed<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.slots.by_ref().find_map(|slot| match slot {
            Slot::Occupied { object, .. } => Some(object),
            Slot::Vacant { .. } | Slot::Retired => None,
        })
    }
}

/// A table of handles, each naming a `T` with some [`Rights`].
#[derive(Debug)]
pub struct HandleTable<T> {
    /// Indexed by the slot part of a handle value.
    slots: Vec<Slot<T>>,
    /// Vacant slots, oldest-emptied first, so reuse is spread across slots
    /// rather than hammering one slot's generation.
    ///
    /// Its capacity is kept at least the number of slots, reserved as each
    /// slot is made, so closing a handle -- which puts its slot here -- never
    /// allocates: a close has nobody to tell that memory ran out.
    free: VecDeque<u32>,
    /// How many slots are occupied.
    live: usize,
    /// The most that may be occupied at once.
    limit: usize,
    /// Set by [`HandleTable::close`]; nothing is inserted afterwards.
    closed: bool,
}

impl<T> HandleTable<T> {
    /// An empty table that will hold at most `limit` handles at once.
    ///
    /// The limit is clamped to [`MAX_SLOTS`]; it is a per-process resource
    /// limit, and the kernel chooses it.
    #[must_use]
    pub fn new(limit: usize) -> HandleTable<T> {
        HandleTable {
            slots: Vec::new(),
            free: VecDeque::new(),
            live: 0,
            limit: limit.min(MAX_SLOTS),
            closed: false,
        }
    }

    /// How many handles are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether no handle is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many more handles could be opened right now.
    ///
    /// Bounded by the limit and also by slots: a table whose every slot has
    /// been retired has no room however few handles are open.
    #[must_use]
    pub fn room(&self) -> usize {
        if self.closed {
            return 0;
        }
        let by_limit = self.limit - self.live;
        let by_slots = self.free.len() + (MAX_SLOTS - self.slots.len());
        by_limit.min(by_slots)
    }

    /// Open a handle to `object` with `rights`.
    ///
    /// # Errors
    ///
    /// The object back, if the table is full, or if it had to grow for a new
    /// slot and there was no memory. [`HandleTable::reserve`] first tells the
    /// two apart.
    pub fn insert(&mut self, object: T, rights: Rights) -> Result<Handle, T> {
        if self.room() == 0 {
            return Err(object);
        }
        self.insert_with_room(object, rights)
    }

    /// Make sure the next `count` insertions need no memory: room for that
    /// many fresh slots, beyond the vacant ones, and for every slot to be
    /// vacant at once.
    ///
    /// # Errors
    ///
    /// [`TableError::NoMemory`]; nothing has changed.
    pub fn reserve(&mut self, count: usize) -> Result<(), TableError> {
        let fresh = count.saturating_sub(self.free.len());
        ferrix_fallible::try_reserve(&mut self.slots, fresh)?;
        let slots = self.slots.len().saturating_add(fresh);
        let additional = slots.saturating_sub(self.free.len());
        ferrix_fallible::try_reserve_deque(&mut self.free, additional)?;
        Ok(())
    }

    /// Open a handle, when the caller has already established there is room.
    ///
    /// # Errors
    ///
    /// The object back, when a fresh slot was needed and there was no memory
    /// for it.
    fn insert_with_room(&mut self, object: T, rights: Rights) -> Result<Handle, T> {
        if let Some(index) = self.free.pop_front()
            && let Some(slot) = self.slots.get_mut(index as usize)
            && let Slot::Vacant { generation } = *slot
        {
            *slot = Slot::Occupied {
                generation,
                rights,
                object,
            };
            self.live += 1;
            return Ok(encode(index, generation));
        }
        // No vacant slot, so `room` promised a fresh one. The index fits: the
        // slot count is at most `MAX_SLOTS`, which is 2^20.
        if self.reserve(1).is_err() {
            return Err(object);
        }
        let index = self.slots.len() as u32;
        self.slots.push(Slot::Occupied {
            generation: 1,
            rights,
            object,
        });
        self.live += 1;
        Ok(encode(index, 1))
    }

    /// The object a handle names, and the rights it carries.
    ///
    /// # Errors
    ///
    /// [`TableError::BadHandle`].
    pub fn get(&self, handle: Handle) -> Result<(&T, Rights), TableError> {
        let (index, wanted) = decode(handle);
        let index = nospec::bounded(index, self.slots.len()).ok_or(TableError::BadHandle)?;
        match self.slots.get(index) {
            Some(Slot::Occupied {
                generation,
                rights,
                object,
            }) if *generation == wanted => Ok((object, *rights)),
            _ => Err(TableError::BadHandle),
        }
    }

    /// The object a handle names, if the handle carries every right in
    /// `needed`.
    ///
    /// # Errors
    ///
    /// [`TableError::BadHandle`], or [`TableError::AccessDenied`].
    pub fn get_with(&self, handle: Handle, needed: Rights) -> Result<&T, TableError> {
        let (object, rights) = self.get(handle)?;
        if rights.contains(needed) {
            Ok(object)
        } else {
            Err(TableError::AccessDenied)
        }
    }

    /// Close a handle, giving back what it named.
    ///
    /// # Errors
    ///
    /// [`TableError::BadHandle`].
    pub fn remove(&mut self, handle: Handle) -> Result<(T, Rights), TableError> {
        let (index, wanted) = decode(handle);
        let index = nospec::bounded(index, self.slots.len()).ok_or(TableError::BadHandle)?;
        let Some(slot) = self.slots.get_mut(index) else {
            return Err(TableError::BadHandle);
        };
        match slot {
            Slot::Occupied { generation, .. } if *generation == wanted => {}
            _ => return Err(TableError::BadHandle),
        }
        let next = if wanted < MAX_GENERATION {
            Slot::Vacant {
                generation: wanted + 1,
            }
        } else {
            Slot::Retired
        };
        let Slot::Occupied { rights, object, .. } = core::mem::replace(slot, next) else {
            return Err(TableError::BadHandle);
        };
        if matches!(slot, Slot::Vacant { .. }) {
            // The index came from a handle, so it fits in the index bits.
            // Never allocates: `free` has room for every slot.
            self.free.push_back(index as u32);
        }
        self.live -= 1;
        Ok((object, rights))
    }

    /// Open a second handle to what `handle` names, with the rights asked for.
    ///
    /// # Errors
    ///
    /// [`TableError::BadHandle`]; [`TableError::AccessDenied`] if the handle
    /// lacks [`Rights::DUPLICATE`] or the request would add a right; and
    /// [`TableError::Full`].
    pub fn duplicate(&mut self, handle: Handle, requested: Requested) -> Result<Handle, TableError>
    where
        T: Clone,
    {
        let (object, held) = self.get(handle)?;
        if !held.contains(Rights::DUPLICATE) {
            return Err(TableError::AccessDenied);
        }
        let rights = requested.resolve(held).ok_or(TableError::AccessDenied)?;
        let object = object.clone();
        self.insert(object, rights).map_err(|_| TableError::Full)
    }

    /// Swap `handle` for a new handle to the same object, with the rights
    /// asked for. The original is closed.
    ///
    /// Needs no right: it can only give something up. Changes nothing if it
    /// fails, including when the original's slot is used up and no other slot
    /// is free.
    ///
    /// # Errors
    ///
    /// [`TableError::BadHandle`]; [`TableError::AccessDenied`] if the request
    /// would add a right; [`TableError::Full`].
    pub fn replace(&mut self, handle: Handle, requested: Requested) -> Result<Handle, TableError> {
        let (_, held) = self.get(handle)?;
        let rights = requested.resolve(held).ok_or(TableError::AccessDenied)?;
        let (index, generation) = decode(handle);
        let index = nospec::bounded(index, self.slots.len()).ok_or(TableError::BadHandle)?;

        if generation < MAX_GENERATION {
            // The common case, in place: the slot's next generation is the
            // new handle, which makes the old value stale by the same rule
            // `remove` relies on.
            let Some(Slot::Occupied {
                generation: current,
                rights: slot_rights,
                ..
            }) = self.slots.get_mut(index)
            else {
                return Err(TableError::BadHandle);
            };
            *current = generation + 1;
            *slot_rights = rights;
            // The index came from a handle, so it fits in the index bits.
            return Ok(encode(index as u32, generation + 1));
        }

        // This slot retires on close, so the new handle needs another one.
        // Establish that it exists before closing anything. Closing frees one
        // unit of the limit but no slot, so a slot is the only question.
        if self.free.is_empty() && self.slots.len() >= MAX_SLOTS {
            return Err(TableError::Full);
        }
        // The new slot is reserved before the old handle is closed, so a
        // refusal changes nothing.
        self.reserve(1)?;
        let (object, _) = self.remove(handle)?;
        self.insert_with_room(object, rights)
            .map_err(|_| TableError::NoMemory)
    }

    /// Close every handle in `handles` and hand back what they named, if every
    /// one of them is open, carries `needed`, and appears once.
    ///
    /// # Errors
    ///
    /// The first problem found, with the table unchanged.
    pub fn take_many(
        &mut self,
        handles: &[Handle],
        needed: Rights,
    ) -> Result<Vec<(T, Rights)>, TableError> {
        for (i, &handle) in handles.iter().enumerate() {
            let _ = self.get_with(handle, needed)?;
            if handles.iter().take(i).any(|&earlier| earlier == handle) {
                return Err(TableError::Repeated);
            }
        }
        let mut taken = ferrix_fallible::try_with_capacity(handles.len())?;
        for &handle in handles {
            // Validated above, and removing one cannot invalidate another:
            // they are distinct, so they are in distinct slots.
            if ferrix_fallible::push_within(&mut taken, self.remove(handle)?).is_err() {
                return Err(TableError::NoMemory);
            }
        }
        Ok(taken)
    }

    /// Open a handle for each object, if there is room for all of them.
    ///
    /// # Errors
    ///
    /// The objects back, untouched, with [`TableError::Full`] if there is no
    /// room for them, or [`TableError::NoMemory`] if there was no memory for
    /// the table to grow or for the list of handles.
    #[expect(
        clippy::type_complexity,
        reason = "a refusal hands the batch back beside why; naming the pair would hide that"
    )]
    pub fn insert_many(
        &mut self,
        objects: Vec<(T, Rights)>,
    ) -> Result<Vec<Handle>, (TableError, Vec<(T, Rights)>)> {
        if objects.len() > self.room() {
            return Err((TableError::Full, objects));
        }
        if self.reserve(objects.len()).is_err() {
            return Err((TableError::NoMemory, objects));
        }
        let Ok(mut handles) = ferrix_fallible::try_with_capacity(objects.len()) else {
            return Err((TableError::NoMemory, objects));
        };
        for (object, rights) in objects {
            // Reserved above: neither the table nor the list can need memory.
            if let Ok(handle) = self.insert_with_room(object, rights) {
                let _ = ferrix_fallible::push_within(&mut handles, handle);
            }
        }
        Ok(handles)
    }

    /// Close every handle, give back every object, and refuse every insertion
    /// from now on.
    ///
    /// What a process's end does. [`HandleTable::clear`] leaves the table
    /// usable, which is wrong for one whose process has terminated: a system
    /// call still running on another processor -- between taking a message
    /// off a channel and putting its handles here, say -- would put them in a
    /// table nobody will close again, and the objects would outlive the
    /// process meant to release them, its channel peers never seeing
    /// `PEER_CLOSED`. After this, every insertion is refused with its object
    /// handed back, for the caller to dispose of.
    pub fn close(&mut self) -> Closed<T> {
        self.closed = true;
        self.live = 0;
        self.free.clear();
        // The slots move out whole: a process's end has nobody to tell that
        // memory ran out, so this allocates nothing.
        Closed {
            slots: core::mem::take(&mut self.slots).into_iter(),
        }
    }

    /// Slots, and the capacities of the slot list and the free list: what a
    /// test needs to see that nothing grew.
    #[cfg(test)]
    pub(crate) fn capacities(&self) -> (usize, usize, usize) {
        (
            self.slots.len(),
            self.slots.capacity(),
            self.free.capacity(),
        )
    }

    /// Whether [`HandleTable::close`] has been called.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Close every handle, giving back every object, and leave the table
    /// usable.
    ///
    /// For a table that lives on. A process that has ended wants
    /// [`HandleTable::close`] instead.
    pub fn clear(&mut self) -> Vec<T> {
        let open: Vec<Handle> = self.handles().map(|(handle, _)| handle).collect();
        open.into_iter()
            .filter_map(|handle| self.remove(handle).ok())
            .map(|(object, _)| object)
            .collect()
    }

    /// Every open handle and its rights, in slot order.
    pub fn handles(&self) -> impl Iterator<Item = (Handle, Rights)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| match slot {
                // The index is bounded by `MAX_SLOTS`, so it fits.
                Slot::Occupied {
                    generation, rights, ..
                } => Some((encode(index as u32, *generation), *rights)),
                _ => None,
            })
    }
}

/// A handle value from a slot and a generation.
const fn encode(index: u32, generation: u32) -> Handle {
    Handle((index << GENERATION_BITS) | generation)
}

/// A slot and a generation from a handle value.
///
/// Generation zero is never issued, so [`Handle::INVALID`] and every value
/// with a zero generation decode to a slot whose generation cannot match.
const fn decode(handle: Handle) -> (usize, u32) {
    (
        (handle.0 >> GENERATION_BITS) as usize,
        handle.0 & GENERATION_MASK,
    )
}
