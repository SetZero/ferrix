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
//! # One kernel handler per line, for good
//!
//! `irq` has no way to take a handler back out, so the first `Interrupt` on a
//! line registers [`on_interrupt`] and it stays registered for the life of the
//! machine. The handler finds its object through [`BOUND`]; a line whose
//! object has gone is masked and left alone.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, Ordering};

use ferrix_sync::IrqSpinLock;

use super::port::Port;
use crate::arch;
use crate::device::Vector;
use crate::irq;
use crate::sched::WaitQueue;

/// The live `Interrupt` on each line, by vector number.
static BOUND: IrqSpinLock<BTreeMap<u32, Weak<Interrupt>>, arch::Irq> =
    IrqSpinLock::new(BTreeMap::new());

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

/// A device interrupt a driver holds.
#[derive(Debug)]
pub(crate) struct Interrupt {
    /// Which line.
    vector: Vector,
    /// Fired and not yet acknowledged. Set only by the handler, cleared only
    /// by an acknowledgement.
    pending: AtomicBool,
    /// The port it is bound to, if any.
    ///
    /// The handler takes this lock before it marks the interrupt pending, and
    /// [`Interrupt::bind`] takes it to bind and to look at pending. That
    /// serialises the two: a delivery and a bind racing on two processors
    /// produce exactly one packet between them, never none and never two.
    binding: IrqSpinLock<Option<Binding>, arch::Irq>,
    /// Woken by the handler each time it goes from quiet to pending.
    waiters: WaitQueue,
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
    /// arrives between registering and unmasking finds its object in the
    /// table already, so it is not lost.
    ///
    /// # Errors
    ///
    /// [`InterruptError::Taken`], and [`InterruptError::NotMaskable`].
    pub(crate) fn new(vector: Vector) -> Result<Arc<Interrupt>, InterruptError> {
        let number = vector.number();
        let interrupt = Arc::new(Interrupt {
            vector,
            pending: AtomicBool::new(false),
            binding: IrqSpinLock::new(None),
            waiters: WaitQueue::new(),
        });

        {
            let mut bound = BOUND.lock();
            if bound
                .get(&number)
                .is_some_and(|held| held.strong_count() > 0)
            {
                // `interrupt` is dropped on the way out, and its drop leaves
                // the live holder's line alone: see `Drop`.
                return Err(InterruptError::Taken);
            }
            let _ = bound.insert(number, Arc::downgrade(&interrupt));
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
            let _ = BOUND.lock().remove(&number);
            return Err(InterruptError::Taken);
        }

        if vector.unmask().is_err() {
            let _ = BOUND.lock().remove(&number);
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
            let mut binding = self.binding.lock();
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
            self.is_pending()
        };
        if pending {
            port.queue_interrupt(key, crate::timer::now_nanos());
        }
        Ok(())
    }

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
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// The queue woken when it fires.
    pub(crate) fn waiters(&self) -> &WaitQueue {
        &self.waiters
    }

    /// The driver has serviced the device: clear pending and let the line
    /// through again.
    ///
    /// # Errors
    ///
    /// If the controller refuses to unmask, which a line it masked does not.
    pub(crate) fn acknowledge(&self) -> Result<(), &'static str> {
        self.pending.store(false, Ordering::Release);
        self.vector.unmask()
    }
}

impl Drop for Interrupt {
    /// Mask the line and let the next `Interrupt` claim it, but only if this
    /// was the object holding it.
    ///
    /// A claim refused because the line was taken drops an object that never
    /// held anything; masking there would silence the live holder's device.
    /// The table's entry tells them apart: it is this object's exactly when
    /// nothing keeps it alive any more.
    fn drop(&mut self) {
        let number = self.vector.number();
        let held_it = {
            let mut bound = BOUND.lock();
            let dead = bound
                .get(&number)
                .is_some_and(|held| held.strong_count() == 0);
            if dead {
                let _ = bound.remove(&number);
            }
            dead
        };
        if held_it {
            let _ = self.vector.mask();
        }
    }
}

/// The kernel's handler for every line an `Interrupt` has claimed.
///
/// Mask, mark, and queue a bound interrupt's packet, and nothing else: see
/// the module documentation for
/// what an interrupt handler may not do. The reference taken to the object is
/// dropped after the table's lock is released, because if it was the last one
/// the object's own drop takes that lock.
///
/// The object is found before the line is masked, because only its [`Vector`]
/// knows where masking happens: at the controller for a line, at the device's
/// table entry for an MSI-X vector, which the controller cannot reach. A line
/// nobody holds is masked at the controller; an MSI-X entry nobody holds was
/// masked by its holder's drop.
pub(crate) fn on_interrupt(number: u32) {
    let target = BOUND.lock().get(&number).and_then(Weak::upgrade);
    match target {
        Some(interrupt) => {
            let _ = interrupt.vector.mask();
            interrupt.fire();
        }
        None => {
            let _ = arch::mask_interrupt(number);
        }
    }
}
