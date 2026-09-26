//! The bus walk: every function reachable from a root bus.
//!
//! A bus is scanned device by device. Function 0 of a device answers or the
//! device is absent; if it answers and its header says multifunction, the
//! other seven are scanned too. A PCI-to-PCI bridge found on the way names
//! the bus behind it, and that bus is scanned in its turn.
//!
//! # What is trusted, and what is not
//!
//! Bus numbers are firmware's. EDK2 and U-Boot both number every bridge before
//! handing over, and the walk follows the numbers as found rather than
//! renumbering, because a bus number is also where a function sits in the
//! ECAM window firmware described and in the IOMMU's tables. Renumbering is a
//! change for the day Ferrix boots on firmware that does not do it.
//!
//! What is *not* trusted is that the numbers make sense. A bridge whose
//! secondary bus is not above its own, lies outside the window, or has
//! already been claimed by another bridge is not followed, and says so as a
//! [`Unfollowed`], which the walk yields and then carries on past. Each bus is
//! scanned at most once, so no arrangement of bridges makes the walk longer
//! than the window.
//!
//! # No recursion, no allocation
//!
//! Buses waiting to be scanned are a 256-bit set rather than a stack, and the
//! lowest-numbered waiting bus is taken next. Firmware numbers buses
//! depth-first, so that is also the order a recursive walk would have used.

use core::ops::RangeInclusive;

use crate::header::{BusNumbers, HeaderKind, Identity};
use crate::{Address, ConfigSpace, DEVICES_PER_BUS, FUNCTIONS_PER_DEVICE};

/// A function the walk found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Function {
    /// Where it is.
    pub address: Address,
    /// What its header says it is.
    pub identity: Identity,
}

/// Why a bridge's secondary bus was not scanned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// The secondary bus is not above the bridge's own bus. Zero, which is
    /// what an unconfigured bridge holds, is the usual case.
    NotBelow,
    /// The secondary bus is outside the window being walked.
    OutsideWindow,
    /// The subordinate bus is below the secondary bus.
    Subordinate,
    /// Another bridge already named the same secondary bus.
    AlreadyClaimed,
}

/// A bridge the walk did not follow. The walk continues without it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Unfollowed {
    /// The bridge.
    pub bridge: Address,
    /// The bus numbers it holds.
    pub numbers: BusNumbers,
    /// Why they were not followed.
    pub reason: Reason,
}

/// A set of bus numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Buses([u64; 4]);

impl Buses {
    /// Whether `bus` is in the set.
    fn contains(self, bus: u8) -> bool {
        let bus = usize::from(bus);
        self.0
            .get(bus / 64)
            .is_some_and(|word| word & (1 << (bus % 64)) != 0)
    }

    /// Put `bus` in the set.
    fn insert(&mut self, bus: u8) {
        let bus = usize::from(bus);
        if let Some(word) = self.0.get_mut(bus / 64) {
            *word |= 1 << (bus % 64);
        }
    }

    /// Take the lowest-numbered bus out of the set.
    fn pop_lowest(&mut self) -> Option<u8> {
        for (index, word) in self.0.iter_mut().enumerate() {
            if *word != 0 {
                let bit = word.trailing_zeros();
                *word &= *word - 1;
                // At most 3 * 64 + 63 = 255.
                return u8::try_from(index * 64 + bit as usize).ok();
            }
        }
        None
    }
}

/// Every function reachable from one root bus.
///
/// Yields `Ok` for each function found, in scan order, and `Err` for each
/// bridge not followed, straight after that bridge's own `Ok`.
#[derive(Debug)]
pub struct Walk<'s, C: ?Sized> {
    /// The space being read.
    space: &'s C,
    /// The segment being walked.
    segment: u16,
    /// The buses a bridge may name.
    window: RangeInclusive<u8>,
    /// Buses scanned or being scanned.
    claimed: Buses,
    /// Buses named by a bridge and not yet scanned.
    waiting: Buses,
    /// The bus being scanned, or `None` once the walk is over.
    bus: Option<u8>,
    /// The next device on it.
    device: u8,
    /// The next function of that device.
    function: u8,
    /// A bridge not followed, to yield before scanning further.
    unfollowed: Option<Unfollowed>,
}

impl<'s, C: ConfigSpace + ?Sized> Walk<'s, C> {
    /// Walk `segment` from the first bus of `window`, following bridges to
    /// buses inside it.
    pub fn new(space: &'s C, segment: u16, window: RangeInclusive<u8>) -> Self {
        let root = (!window.is_empty()).then_some(*window.start());
        let mut claimed = Buses::default();
        if let Some(root) = root {
            claimed.insert(root);
        }
        Walk {
            space,
            segment,
            window,
            claimed,
            waiting: Buses::default(),
            bus: root,
            device: 0,
            function: 0,
            unfollowed: None,
        }
    }

    /// Decide whether a bridge's secondary bus is scanned, and queue it if so.
    fn follow(&mut self, bridge: Address, bus: u8) {
        let Ok(numbers) = BusNumbers::read(self.space, bridge) else {
            return;
        };
        let reason = if numbers.secondary <= bus {
            Some(Reason::NotBelow)
        } else if !self.window.contains(&numbers.secondary) {
            Some(Reason::OutsideWindow)
        } else if numbers.subordinate < numbers.secondary {
            Some(Reason::Subordinate)
        } else if self.claimed.contains(numbers.secondary) {
            Some(Reason::AlreadyClaimed)
        } else {
            None
        };
        match reason {
            Some(reason) => {
                self.unfollowed = Some(Unfollowed {
                    bridge,
                    numbers,
                    reason,
                });
            }
            None => {
                self.claimed.insert(numbers.secondary);
                self.waiting.insert(numbers.secondary);
            }
        }
    }
}

impl<C: ConfigSpace + ?Sized> Iterator for Walk<'_, C> {
    type Item = Result<Function, Unfollowed>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(unfollowed) = self.unfollowed.take() {
            return Some(Err(unfollowed));
        }
        loop {
            let bus = self.bus?;
            if self.device >= DEVICES_PER_BUS {
                self.bus = self.waiting.pop_lowest();
                self.device = 0;
                self.function = 0;
                continue;
            }

            let address = Address::new(self.segment, bus, self.device, self.function)?;
            let identity = Identity::read(self.space, address);

            // Advance before anything can return. Function 0 decides whether
            // the rest of the device is scanned at all.
            let more_functions = identity.is_some_and(|found| found.multifunction);
            if (self.function == 0 && !more_functions) || self.function + 1 >= FUNCTIONS_PER_DEVICE
            {
                self.device += 1;
                self.function = 0;
            } else {
                self.function += 1;
            }

            let Some(identity) = identity else {
                continue;
            };
            if identity.kind == HeaderKind::Bridge {
                self.follow(address, bus);
            }
            return Some(Ok(Function { address, identity }));
        }
    }
}
