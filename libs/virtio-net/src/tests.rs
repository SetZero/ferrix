//! Tests for the virtio-net driver, against a device that lives in the test.
//!
//! [`fake`] is the device: the common configuration's status protocol and
//! feature negotiation, two split queues, and a wire the test puts frames on,
//! reached -- as a real device is -- only by device address, through a bus
//! that maps device pages to memory the way an IOMMU domain does. Nothing it
//! reads is the driver's bookkeeping, so a descriptor that points at the
//! wrong page reads the wrong bytes rather than the right ones by accident.
//!
//! [`round_trip`] holds the driver to a device that behaves; [`hostile`] to
//! one told to misbehave in each way the protocol leaves room for.

mod fake;
mod hostile;
mod round_trip;
