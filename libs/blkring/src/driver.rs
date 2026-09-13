//! The driver's end of the ring: it consumes submissions and produces
//! completions.
//!
//! The driver checks every submission it consumes and completes a bad one
//! `REFUSED` itself, so the glue only ever sees requests it may hand to the
//! device. It does not remember ids — that is the kernel's job — but it counts
//! the requests it holds, because that count is what lets it tell a kernel
//! that broke the outstanding bound from one that is merely slow: under the
//! bound, the completion ring always has room for everything held.

use core::fmt;

use crate::bell::{BELL_COMPLETE, Doorbell, Wait};
use crate::control::Hello;
use crate::geometry::{Device, InvalidSubmission, check_submission};
use crate::layout::{
    HeaderError, RawCompletion, RawSubmission, RingLayout, Status, Submission, header, write_header,
};
use crate::ring::{Consumer, Producer};
use crate::{Corruption, RingMemory};

/// What [`DriverSide::consume`] took off the submission ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Consumed {
    /// A request that passed every check. The driver holds it until
    /// [`DriverSide::complete`].
    Request(Submission),
    /// A submission that failed a check. Its `REFUSED` completion is already
    /// written; [`DriverSide::publish`] makes it visible.
    Refused {
        /// The id it carried.
        id: u64,
        /// The check it failed, for the driver's log.
        reason: InvalidSubmission,
    },
}

/// Why [`DriverSide::complete`] wrote nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompleteError {
    /// The ring is corrupt; reset the device and stop.
    Corrupt(Corruption),
    /// The driver holds no request. A bug in the driver's glue.
    NotHeld,
}

impl fmt::Display for CompleteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompleteError::Corrupt(corruption) => write!(formatter, "ring corrupt: {corruption}"),
            CompleteError::NotHeld => formatter.write_str("no request is held"),
        }
    }
}

/// The driver's end of one ring.
pub struct DriverSide<M> {
    memory: M,
    layout: RingLayout,
    device: Device,
    submissions: Consumer,
    completions: Producer,
    /// Requests consumed and not yet completed.
    held: u64,
    fault: Option<Corruption>,
}

impl<M> fmt::Debug for DriverSide<M> {
    /// Leaves out the memory, as [`crate::KernelSide`]'s does.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriverSide")
            .field("layout", &self.layout)
            .field("device", &self.device)
            .field("submissions", &self.submissions)
            .field("completions", &self.completions)
            .field("held", &self.held)
            .field("fault", &self.fault)
            .finish_non_exhaustive()
    }
}

impl<M: RingMemory> DriverSide<M> {
    /// Set up a ring for `device` in a ring VMO of `ring_bytes` bytes: write a
    /// fresh header for `layout`. Send [`DriverSide::hello`] next.
    ///
    /// # Errors
    ///
    /// The [`HeaderError`] if `layout` does not fit `ring_bytes`.
    pub fn new(
        mut memory: M,
        ring_bytes: u64,
        layout: RingLayout,
        device: Device,
    ) -> Result<Self, HeaderError> {
        let layout = RingLayout::new(
            layout.entries(),
            layout.sub_offset(),
            layout.comp_offset(),
            ring_bytes,
        )?;
        write_header(&mut memory, &layout);
        Ok(Self {
            memory,
            layout,
            device,
            submissions: Consumer::default(),
            completions: Producer::default(),
            held: 0,
            fault: None,
        })
    }

    /// The HELLO describing this ring's device.
    #[must_use]
    pub const fn hello(&self) -> Hello {
        Hello::for_device(&self.device)
    }

    /// The ring's layout.
    #[must_use]
    pub const fn layout(&self) -> RingLayout {
        self.layout
    }

    /// The device this ring serves.
    #[must_use]
    pub const fn device(&self) -> &Device {
        &self.device
    }

    /// Requests consumed and not yet completed.
    #[must_use]
    pub const fn held(&self) -> u64 {
        self.held
    }

    /// The corruption this side detected, if it has.
    #[must_use]
    pub const fn corruption(&self) -> Option<Corruption> {
        self.fault
    }

    /// Whether this side last asked the kernel to ring.
    #[must_use]
    pub const fn wants_bell(&self) -> bool {
        self.submissions.wants_bell()
    }

    /// Take the next submission off the ring, if one is pending.
    ///
    /// # Errors
    ///
    /// [`Corruption`], after which every call reports it: reset the device and
    /// stop.
    pub fn consume(&mut self) -> Result<Option<Consumed>, Corruption> {
        if let Some(corruption) = self.fault {
            return Err(corruption);
        }
        let pending =
            self.submissions
                .pending(&self.memory, header::SUB_TAIL, self.layout.entries());
        if self.check(pending)? == 0 {
            return Ok(None);
        }
        let at = self.layout.submission_at(self.submissions.head());
        let raw = RawSubmission::read_from(&self.memory, at);
        let consumed = match check_submission(&raw, &self.device) {
            Ok(request) => {
                self.held = self.held.saturating_add(1);
                Consumed::Request(request)
            }
            Err(reason) => {
                // Refusing it takes a completion slot while this one is, in
                // effect, held.
                let staged = self.stage(raw.id, Status::Refused, 0, self.held.saturating_add(1));
                self.check(staged)?;
                Consumed::Refused { id: raw.id, reason }
            }
        };
        self.submissions.advance(&mut self.memory, header::SUB_HEAD);
        Ok(Some(consumed))
    }

    /// Complete a held request. The kernel does not see it until
    /// [`DriverSide::publish`].
    ///
    /// # Errors
    ///
    /// [`CompleteError`] says why nothing was written.
    pub fn complete(
        &mut self,
        id: u64,
        status: Status,
        bytes_done: u64,
    ) -> Result<(), CompleteError> {
        if let Some(corruption) = self.fault {
            return Err(CompleteError::Corrupt(corruption));
        }
        if self.held == 0 {
            return Err(CompleteError::NotHeld);
        }
        let staged = self.stage(id, status, bytes_done, self.held);
        self.check(staged).map_err(CompleteError::Corrupt)?;
        self.held -= 1;
        Ok(())
    }

    /// Write a completion into the next slot, if the completion ring has room
    /// for it and for the `holding` requests its space is owed to.
    fn stage(
        &mut self,
        id: u64,
        status: Status,
        bytes_done: u64,
        holding: u64,
    ) -> Result<(), Corruption> {
        let occupied = self
            .completions
            .observe_head(self.memory.read_u32(header::COMP_HEAD))?;
        // Every completion between the head and the tail, and every request
        // held, is outstanding at the kernel, which keeps at most `entries`
        // outstanding. The head was read after the kernel published it and
        // before it forgot the ids behind it, so this cannot fire on an honest
        // kernel, however the two sides interleave.
        if u64::from(occupied).saturating_add(holding) > u64::from(self.layout.entries()) {
            return Err(Corruption::Overcommitted);
        }
        let completion = RawCompletion {
            id,
            bytes_done,
            status: status.raw(),
            reserved: 0,
        };
        let at = self.layout.completion_at(self.completions.tail());
        completion.write_to(&mut self.memory, at);
        self.completions.stage();
        Ok(())
    }

    /// Publish every completion written since the last publish, and return the
    /// doorbell to ring if the kernel asked for one.
    ///
    /// The caller queues [`Doorbell::packet`] on the kernel's completion port
    /// with `port_queue`; [`crate::bell::port_queue_rung`] counts a full port
    /// as rung.
    pub fn publish(&mut self) -> Option<Doorbell> {
        if self.fault.is_some() {
            return None;
        }
        self.completions.publish(
            &mut self.memory,
            header::COMP_TAIL,
            header::COMP_WANT_BELL,
            BELL_COMPLETE,
        )
    }

    /// Before `port_wait`: ask to be rung, then look once more.
    /// [`Wait::Pending`] means consume instead of sleeping.
    ///
    /// # Errors
    ///
    /// [`Corruption`] in the submission tail.
    pub fn prepare_to_sleep(&mut self) -> Result<Wait, Corruption> {
        if let Some(corruption) = self.fault {
            return Err(corruption);
        }
        let wait = self.submissions.prepare_to_sleep(
            &mut self.memory,
            header::SUB_WANT_BELL,
            header::SUB_TAIL,
            self.layout.entries(),
        );
        self.check(wait)
    }

    /// On waking: stop asking to be rung. Consume until nothing is pending
    /// next.
    pub fn woke(&mut self) {
        if self.fault.is_none() {
            self.submissions
                .woke(&mut self.memory, header::SUB_WANT_BELL);
        }
    }

    fn check<T>(&mut self, result: Result<T, Corruption>) -> Result<T, Corruption> {
        if let Err(corruption) = &result {
            self.fault = Some(*corruption);
        }
        result
    }
}
