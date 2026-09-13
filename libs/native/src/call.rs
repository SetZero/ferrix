//! The one door every call goes through.
//!
//! A wrapper in this crate never makes a system call. It describes one — the
//! number, the six argument registers, and the pieces of its own memory the
//! pointer arguments name — as a [`Raw`], and hands that to a [`Syscall`].
//! The runtime's implementation puts the registers in place and traps; a test's
//! implementation records them and plays the kernel's part through
//! [`Raw::memory`] and [`Raw::memory_mut`], so a wrapper's number, argument
//! order and pointer layout are checked on the host, under Miri, without a
//! line of `unsafe`.
//!
//! # Why a `Raw` cannot be made outside this crate
//!
//! A real [`Syscall`] traps with whatever registers it is given, and the kernel
//! writes wherever an output pointer says. If any code could build a `Raw`, a
//! safe caller could name memory it does not own and have the kernel write
//! over it. So only this crate builds one, and only in a wrapper, where every
//! pointer argument comes from a slice the `Raw` itself borrows — shared for
//! memory the kernel reads, exclusive for memory it writes — for exactly as
//! long as the call lasts. That is the claim the runtime's one `unsafe` block
//! rests on.
//!
//! A `Raw` is neither `Clone` nor `Copy`, and [`Syscall::call`] takes it for an
//! unnamed lifetime, so an implementation cannot keep one to replay after the
//! memory it names is gone.

use core::marker::PhantomData;

/// The most pieces of memory one call names. `channel_read` names three.
pub const MAX_REGIONS: usize = 3;

/// How many argument registers a call has.
pub const ARGUMENTS: usize = 6;

/// A way to make a native call.
///
/// `Copy` because a handle keeps one to close itself with: the runtime's is a
/// zero-sized value, and a test's is a shared reference to its recorder.
pub trait Syscall: Copy {
    /// Make the call `raw` describes and return the result register as the
    /// kernel left it: a value, or `-errno` in `-4095..=-1`.
    fn call(self, raw: Raw<'_>) -> usize;
}

/// A piece of the caller's memory that a pointer argument names.
#[derive(Debug, Default)]
enum Region<'a> {
    /// Nothing.
    #[default]
    Empty,
    /// Memory the kernel reads.
    In(&'a [u8]),
    /// Memory the kernel may write.
    Out(&'a mut [u8]),
}

impl Region<'_> {
    /// The bytes, whichever way they go.
    fn bytes(&self) -> &[u8] {
        match self {
            Region::Empty => &[],
            Region::In(bytes) => bytes,
            Region::Out(bytes) => bytes,
        }
    }

    /// Where `len` bytes at `address` sit inside this region, if they do.
    fn offset_of(&self, address: usize, len: usize) -> Option<usize> {
        let bytes = self.bytes();
        let start = bytes.as_ptr().addr();
        let offset = address.checked_sub(start)?;
        (!bytes.is_empty() && offset.checked_add(len)? <= bytes.len()).then_some(offset)
    }
}

/// One native call, described and not yet made.
#[derive(Debug)]
pub struct Raw<'a> {
    /// The call number, in `0x1000..=0x1FFF`.
    number: usize,
    /// The argument registers, in order.
    args: [usize; ARGUMENTS],
    /// The memory the pointer arguments name.
    regions: [Region<'a>; MAX_REGIONS],
    /// Ties the description to the borrows it was built from.
    _memory: PhantomData<&'a mut [u8]>,
}

impl<'a> Raw<'a> {
    /// The call number.
    #[must_use]
    pub const fn number(&self) -> usize {
        self.number
    }

    /// The argument registers, in order. Those a call does not use are zero.
    #[must_use]
    pub const fn args(&self) -> [usize; ARGUMENTS] {
        self.args
    }

    /// `len` bytes at `address`, if one of the call's pieces of memory holds
    /// them all: what a kernel reading an argument pointer would find.
    #[must_use]
    pub fn memory(&self, address: usize, len: usize) -> Option<&[u8]> {
        self.regions.iter().find_map(|region| {
            let offset = region.offset_of(address, len)?;
            region.bytes().get(offset..offset.checked_add(len)?)
        })
    }

    /// `len` bytes at `address`, if one of the call's *output* pieces of
    /// memory holds them all: where a kernel may write.
    #[must_use]
    pub fn memory_mut(&mut self, address: usize, len: usize) -> Option<&mut [u8]> {
        self.regions.iter_mut().find_map(|region| {
            let offset = region.offset_of(address, len)?;
            match region {
                Region::Out(bytes) => bytes.get_mut(offset..offset.checked_add(len)?),
                Region::Empty | Region::In(_) => None,
            }
        })
    }
}

/// Builds a [`Raw`] argument by argument, so that a pointer argument and the
/// memory it names are one step and cannot disagree.
#[derive(Debug)]
pub(crate) struct Call<'a> {
    /// What is being built.
    raw: Raw<'a>,
    /// The next argument register to fill.
    next_arg: usize,
    /// The next region slot to fill.
    next_region: usize,
}

impl<'a> Call<'a> {
    /// A call to `number` with no arguments yet.
    pub(crate) fn new(number: usize) -> Call<'a> {
        Call {
            raw: Raw {
                number,
                args: [0; ARGUMENTS],
                regions: Default::default(),
                _memory: PhantomData,
            },
            next_arg: 0,
            next_region: 0,
        }
    }

    /// The next argument is `value`.
    ///
    /// A seventh argument has nowhere to go and is dropped; no call has one,
    /// and every wrapper's arity is held by its test.
    pub(crate) fn value(mut self, value: usize) -> Call<'a> {
        if let Some(slot) = self.raw.args.get_mut(self.next_arg) {
            *slot = value;
        }
        self.next_arg = self.next_arg.saturating_add(1);
        self
    }

    /// The next argument points at `bytes`, which the kernel reads. Null for
    /// an empty slice, which the kernel never dereferences.
    pub(crate) fn input(self, bytes: &'a [u8]) -> Call<'a> {
        let address = address_of(bytes);
        self.region(Region::In(bytes)).value(address)
    }

    /// The next argument points at `bytes`, which the kernel may write. Null
    /// for an empty slice.
    pub(crate) fn output(self, bytes: &'a mut [u8]) -> Call<'a> {
        let address = address_of(bytes);
        self.region(Region::Out(bytes)).value(address)
    }

    /// The next argument points at `bytes` if there are any, and is null if
    /// not: an optional input such as a deadline.
    pub(crate) fn optional_input(self, bytes: Option<&'a [u8]>) -> Call<'a> {
        match bytes {
            Some(bytes) => self.input(bytes),
            None => self.value(0),
        }
    }

    /// Record a region.
    ///
    /// A fourth has no slot and is left out, which leaves the kernel's copy
    /// unobservable to a test and so fails that test; no call names four.
    fn region(mut self, region: Region<'a>) -> Call<'a> {
        if let Some(slot) = self.raw.regions.get_mut(self.next_region) {
            *slot = region;
        }
        self.next_region = self.next_region.saturating_add(1);
        self
    }

    /// Make the call through `sys`.
    pub(crate) fn make<S: Syscall>(self, sys: S) -> usize {
        sys.call(self.raw)
    }
}

/// The address a pointer argument carries for `bytes`: null when empty.
fn address_of(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        0
    } else {
        bytes.as_ptr().addr()
    }
}
