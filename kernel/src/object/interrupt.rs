//! Interrupts: a device's vector, delivered to the driver that holds it.
//!
//! `docs/ARCHITECTURE.md` §7 gives a driver "an `Interrupt` object per
//! vector". It is built only from a [`Vector`], which only `crate::device`
//! can make, so a driver cannot claim a line its device does not have.
//!
//! # Masked from delivery to acknowledgement
//!
//! When the line fires, the kernel's handler masks it and marks the object
//! pending. The driver sees `READABLE`, services the device, and acknowledges,
//! which clears pending and unmasks. A device that keeps its line asserted
//! therefore cannot keep a processor in the handler, and a driver that has
//! wedged costs one masked line rather than an interrupt storm.
//!
//! # What an interrupt handler may do
//!
//! Take interrupt-safe locks, and wake. The handler marks the object pending
//! and queues its port's packet under interrupt-safe locks only, lets go of
//! them, and then wakes whoever waits on the interrupt and on the port.
//! `WaitQueue::wake_all` may be called from a handler: its own lock and every
//! run queue's are taken with interrupts masked, it allocates nothing, and it
//! leaves the reschedule to the way out of the interrupt or to an IPI. What it
//! must not be called with is a lock it needs already held, which is why the
//! binding's lock is released first. A plain `SpinLock` is still never taken
//! here.
//!
//! # Holding the object is holding the line
//!
//! A claim lasts exactly as long as the [`Interrupt`], which only handles, and
//! messages carrying them, keep alive. When the last of those goes the line is
//! free by the time the close returns, because `object::dispose` drops an
//! interrupt on the spot, and a driver restarted after its predecessor died
//! can claim it straight away.
//!
//! A delivery never holds the claim. The handler takes the claimed [`Line`] --
//! the pending mark, the binding and the waiters -- and fires that, so a holder
//! that lets go while a delivery runs on another processor frees the line at
//! once, and the delivery finishes against state nobody can reach any more: at
//! most one packet on the port the old holder had bound it to. The claim used
//! to be the object itself, found through a weak reference the handler
//! upgraded, and a delivery preempted holding that reference made an immediate
//! re-claim of the line fail (`FX-0901`).
//!
//! Finding a line and masking it are one step under [`BOUND`]'s lock, and so
//! are giving a line up and masking it. Otherwise a delivery that found the old
//! holder, or the old holder's own drop, could mask the line after a new holder
//! had claimed and unmasked it, and the new driver would never hear its device
//! again.
//!
//! # One kernel handler per line, for good
//!
//! `irq` has no way to take a handler back out, so the first `Interrupt` on a
//! line registers [`on_interrupt`] and it stays registered for the life of the
//! machine. The handler finds the line through [`BOUND`]; a line nobody holds
//! is masked and left alone.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, Ordering};

use ferrix_sync::IrqSpinLock;

use super::port::Port;
use crate::arch;
use crate::device::Vector;
use crate::irq;
use crate::sched::WaitQueue;

/// Each claimed line, by vector number: there exactly as long as the
/// [`Interrupt`] that claimed it.
static BOUND: IrqSpinLock<BTreeMap<u32, Arc<Line>>, arch::Irq> = IrqSpinLock::new(BTreeMap::new());

/// The lines [`on_interrupt`] is registered on.
static REGISTERED: IrqSpinLock<BTreeSet<u32>, arch::Irq> = IrqSpinLock::new(BTreeSet::new());

/// Why an interrupt could not be claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptError {
    /// Another `Interrupt` holds the line, or the kernel uses it.
    Taken,
    /// The interrupt controller cannot mask this line.
    NotMaskable,
    /// The interrupt is already bound to a port someone still holds.
    AlreadyBound,
}

/// Where a bound interrupt's packets go.
#[derive(Debug)]
struct Binding {
    /// The port. Weak, so a binding does not keep a port nobody holds alive.
    port: Weak<Port>,
    /// The key its packets carry.
    key: u64,
}

/// What a delivery reaches on a claimed line.
///
/// Shared by the [`Interrupt`] that claimed the line and by [`BOUND`], and
/// held for a moment by a [`Delivery`]: see the module documentation.
#[derive(Debug)]
struct Line {
    /// Which line.
    vector: Vector,
    /// Fired and not yet acknowledged. Set only by a delivery, cleared only by
    /// an acknowledgement.
    pending: AtomicBool,
    /// The port it is bound to, if any.
    ///
    /// A delivery takes this lock before it marks the line pending, and
    /// [`Interrupt::bind`] takes it to bind and to look at pending. That
    /// serialises the two: a delivery and a bind racing on two processors
    /// produce exactly one packet between them, never none and never two.
    binding: IrqSpinLock<Option<Binding>, arch::Irq>,
    /// Woken by a delivery each time the line goes from quiet to pending.
    waiters: WaitQueue,
}

impl Line {
    /// Mark it pending, queue its packet if it is bound and was quiet, and
    /// wake whoever waits on it and on its port.
    ///
    /// From its interrupt handler. The marking and the packet happen under
    /// interrupt-safe locks only, and the wakes after those are released: see
    /// the module documentation.
    fn fire(&self) {
        let port = {
            let binding = self.binding.lock();
            if self.pending.swap(true, Ordering::AcqRel) {
                return;
            }
            binding.as_ref().and_then(|bound| {
                let port = bound.port.upgrade()?;
                let _ = port.queue_from_interrupt(bound.key, crate::timer::now_nanos());
                Some(port)
            })
        };
        self.waiters.wake_all();
        if let Some(port) = port {
            port.waiters().wake_all();
        }
    }

    /// Whether it has fired and not been acknowledged.
    fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

/// A device interrupt a driver holds, and with it the claim on its line.
#[derive(Debug)]
pub(crate) struct Interrupt {
    /// The line it claimed.
    line: Arc<Line>,
}

impl Interrupt {
    /// Claim `vector`.
    ///
    /// # Nothing is masked until the line is known to be free
    ///
    /// The line is claimed in [`BOUND`] first, then the kernel's handler is
    /// registered, and only then is the line unmasked. The first version
    /// masked it up front, to find out whether it could be masked, and a
    /// refused second claim, or a claim on a line the kernel itself uses,
    /// would then have masked a line somebody else owns. A delivery that
    /// arrives between registering and unmasking finds the line in the table
    /// already, so it is not lost.
    ///
    /// # Errors
    ///
    /// [`InterruptError::Taken`], and [`InterruptError::NotMaskable`].
    pub(crate) fn new(vector: Vector) -> Result<Arc<Interrupt>, InterruptError> {
        let number = vector.number();
        let interrupt = Arc::new(Interrupt {
            line: Arc::new(Line {
                vector,
                pending: AtomicBool::new(false),
                binding: IrqSpinLock::new(None),
                waiters: WaitQueue::new(),
            }),
        });

        {
            let mut bound = BOUND.lock();
            if bound.contains_key(&number) {
                // `interrupt` is dropped on the way out, and its drop leaves
                // the live holder's line alone: see `Drop`.
                return Err(InterruptError::Taken);
            }
            let _ = bound.insert(number, Arc::clone(&interrupt.line));
        }

        let registered = {
            let mut lines = REGISTERED.lock();
            if lines.contains(&number) {
                Ok(())
            } else {
                let result = irq::register(number, on_interrupt);
                if result.is_ok() {
                    let _ = lines.insert(number);
                }
                result
            }
        };
        if registered.is_err() {
            // The kernel handles this line itself. Unclaimed before the
            // object is dropped, so its drop does not mask the kernel's line.
            let _unclaimed = BOUND.lock().remove(&number);
            return Err(InterruptError::Taken);
        }

        if vector.unmask().is_err() {
            let _unclaimed = BOUND.lock().remove(&number);
            return Err(InterruptError::NotMaskable);
        }
        Ok(interrupt)
    }

    /// Deliver it to `port` as packets carrying `key`: one each time it goes
    /// from quiet to pending, so a device that fires twice before its driver
    /// acknowledges produces one packet, not a queue of them. If it is already
    /// pending when bound, its packet is queued at once.
    ///
    /// # Errors
    ///
    /// [`InterruptError::AlreadyBound`] if it is bound to a port anyone still
    /// holds.
    pub(crate) fn bind(&self, port: &Arc<Port>, key: u64) -> Result<(), InterruptError> {
        let pending = {
            let mut binding = self.line.binding.lock();
            if binding
                .as_ref()
                .is_some_and(|bound| bound.port.strong_count() > 0)
            {
                return Err(InterruptError::AlreadyBound);
            }
            *binding = Some(Binding {
                port: Arc::downgrade(port),
                key,
            });
            self.line.is_pending()
        };
        if pending {
            port.queue_interrupt(key, crate::timer::now_nanos());
        }
        Ok(())
    }

    /// Whether it has fired and not been acknowledged.
    pub(crate) fn is_pending(&self) -> bool {
        self.line.is_pending()
    }

    /// The queue woken when it fires.
    pub(crate) fn waiters(&self) -> &WaitQueue {
        &self.line.waiters
    }

    /// The driver has serviced the device: clear pending and let the line
    /// through again.
    ///
    /// # Errors
    ///
    /// If the controller refuses to unmask, which a line it masked does not.
    pub(crate) fn acknowledge(&self) -> Result<(), &'static str> {
        self.line.pending.store(false, Ordering::Release);
        self.line.vector.unmask()
    }
}

impl Drop for Interrupt {
    /// Give the line up and mask it, but only if this object held it.
    ///
    /// A claim refused because the line was taken drops an object that never
    /// held anything; masking there would silence the live holder's device.
    /// The table's entry tells them apart: it is this object's own line or it
    /// is not. Masked before the table's lock goes, so that nobody can claim
    /// the line and unmask it in between and then have it masked from under
    /// them.
    fn drop(&mut self) {
        let number = self.line.vector.number();
        let given_up = {
            let mut bound = BOUND.lock();
            if bound
                .get(&number)
                .is_some_and(|held| Arc::ptr_eq(held, &self.line))
            {
                let _ = self.line.vector.mask();
                bound.remove(&number)
            } else {
                None
            }
        };
        // The table's reference goes after its lock does.
        drop(given_up);
    }
}

/// A delivery found and not yet fired: what the handler holds between finding
/// the line and waking its waiters.
///
/// It holds the line's state, never the claim, so the holder can let go and
/// somebody else can claim the line while one is in flight. Its reference may
/// be the last to a line given up meanwhile, whose drop only frees memory.
#[derive(Debug)]
pub(crate) struct Delivery(Arc<Line>);

impl Delivery {
    /// Mark the line pending, queue its packet, and wake its waiters.
    pub(crate) fn fire(self) {
        self.0.fire();
    }
}

/// The first half of a delivery on line `number`: find the line its holder
/// claimed and mask it, in one step under [`BOUND`]'s lock.
///
/// The line is found before it is masked, because only its [`Vector`] knows
/// where masking happens: at the controller for a line, at the device's table
/// entry for an MSI-X vector, which the controller cannot reach. A line nobody
/// holds is masked at the controller and has nothing to deliver; an MSI-X
/// entry nobody holds was masked by its holder's drop.
pub(crate) fn take_delivery(number: u32) -> Option<Delivery> {
    let bound = BOUND.lock();
    match bound.get(&number) {
        Some(line) => {
            let _ = line.vector.mask();
            Some(Delivery(Arc::clone(line)))
        }
        None => {
            let _ = arch::mask_interrupt(number);
            None
        }
    }
}

/// The kernel's handler for every line an `Interrupt` has claimed.
///
/// Mask, mark, queue a bound interrupt's packet, and wake, and nothing else:
/// see the module documentation for what an interrupt handler may not do.
pub(crate) fn on_interrupt(number: u32) {
    if let Some(delivery) = take_delivery(number) {
        delivery.fire();
    }
}
