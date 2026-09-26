//! What the display and render cores share about the devices they serve: a
//! device's claim by one driver's control channel, which a quiesce waits out,
//! and the numbers their nodes are published under, which a driver started
//! again gets back.
//!
//! # Why a quiesce waits for the claim
//!
//! A driver's death fires `devmgr`'s `TERMINATED` when its handles close,
//! which raises `PEER_CLOSED` on the core's end of the control channel but
//! does not wait for the core's task to see it. Until that task ends the
//! device is still claimed, and a driver `devmgr` starts again in the
//! meantime is refused its control channel as `InUse`. So `device_quiesce`
//! waits, bounded, for every claim whose driver end has closed -- as it
//! already did for a block ring (`block_ring::wait_until_unserved`) -- and
//! refuses only a claim a live driver still holds.
//!
//! # Why the lowest free number
//!
//! A program opens `/dev/dri/card0`, and `compositor_drm::cards` stops at
//! the first gap. A card whose driver died and came back must be `card0`
//! again, not `card1` after a hole, or nothing that looks for a screen finds
//! it.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_native_abi::signals::Signals;

use crate::device::DeviceNode;
use crate::object::channel::Endpoint;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::timer;

/// Why a device node cannot be quiesced.
///
/// Defined here rather than in `crate::block_ring` because the claim is the
/// core's: a quiesce asks whether anything still serves a node, and the answer
/// must not depend on which uncertified subsystem happens to be serving it.
/// `block_ring`, `render` and `display` each answer with this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StillServed {
    /// A driver holds its end of the ring's control channel: it is alive
    /// and serving, and quiescing under it is refused.
    ByADriver,
    /// The driver is gone but the ring's task has not ended within the
    /// patience, or the caller was terminated meanwhile.
    Waiting,
}

/// How long a quiesce waits for a core's task to let go of a device whose
/// driver has gone: the same patience the block ring gives.
const PATIENCE_NANOS: u64 = 5_000_000_000;

/// Devices claimed through a core's control channels: each held by the
/// core's end of the channel its driver was handed.
pub(crate) struct Claims {
    held: SpinLock<Vec<(Arc<DeviceNode>, Arc<Endpoint>)>>,
    released: WaitQueue,
}

impl Claims {
    pub(crate) const fn new() -> Self {
        Self {
            held: SpinLock::new(Vec::new()),
            released: WaitQueue::new(),
        }
    }

    /// Claim `node` for the channel whose core end is `control`; `false`
    /// when it is claimed already, or when there was no memory to record the
    /// claim -- which the caller answers as it does a device in use.
    pub(crate) fn claim(&self, node: &Arc<DeviceNode>, control: &Arc<Endpoint>) -> bool {
        let mut held = self.held.lock();
        if held.iter().any(|(claimed, _)| Arc::ptr_eq(claimed, node)) {
            return false;
        }
        crate::fallible::try_push(&mut held, (Arc::clone(node), Arc::clone(control))).is_ok()
    }

    /// Let `node` go, and wake a quiesce waiting for it.
    pub(crate) fn release(&self, node: &Arc<DeviceNode>) {
        self.held
            .lock()
            .retain(|(claimed, _)| !Arc::ptr_eq(claimed, node));
        self.released.wake_all();
    }

    /// The control channel `node` is claimed through, if it is.
    fn control_of(&self, node: &Arc<DeviceNode>) -> Option<Arc<Endpoint>> {
        self.held
            .lock()
            .iter()
            .find(|(claimed, _)| Arc::ptr_eq(claimed, node))
            .map(|(_, control)| Arc::clone(control))
    }

    /// Wait until `node` is not claimed, for a quiesce: at once when it is
    /// not, bounded when its driver's end has closed, refused when a driver
    /// still holds it. `cancelled` ends the wait early for a caller that is
    /// being terminated.
    ///
    /// # Errors
    ///
    /// [`StillServed`], as `block_ring::wait_until_unserved` answers.
    pub(crate) fn wait_until_released(
        &self,
        node: &Arc<DeviceNode>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), StillServed> {
        let deadline = timer::now_nanos().saturating_add(PATIENCE_NANOS);
        loop {
            let Some(control) = self.control_of(node) else {
                return Ok(());
            };
            if !control.signals().intersects(Signals::PEER_CLOSED) {
                return Err(StillServed::ByADriver);
            }
            let released = self
                .released
                .wait_until_deadline(|| self.control_of(node).is_none() || cancelled(), deadline);
            if !released || cancelled() {
                return Err(StillServed::Waiting);
            }
        }
    }
}

/// The numbers a core's nodes are published under, from `first` upwards,
/// each the lowest one nothing holds.
pub(crate) struct Numbers {
    first: u32,
    taken: SpinLock<Vec<u32>>,
}

impl Numbers {
    pub(crate) const fn new(first: u32) -> Self {
        Self {
            first,
            taken: SpinLock::new(Vec::new()),
        }
    }

    /// The lowest number nothing holds, now held; `None` when there was no
    /// memory to hold it.
    pub(crate) fn take(&self) -> Option<u32> {
        let mut taken = self.taken.lock();
        let mut number = self.first;
        while taken.contains(&number) {
            number = number.saturating_add(1);
        }
        crate::fallible::try_push(&mut taken, number).ok()?;
        Some(number)
    }

    /// Give `number` back, for the next node to be published under.
    pub(crate) fn give_back(&self, number: u32) {
        self.taken.lock().retain(|&held| held != number);
    }
}
