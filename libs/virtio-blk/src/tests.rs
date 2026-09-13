//! Tests for the virtio-blk driver, against a device that lives in the test.
//!
//! [`fake`] is the device: the common configuration's status protocol and
//! feature negotiation, a `Vec<u8>` disk, and the device half of the split
//! queue, reached — as a real device is — only by device address, through a
//! bus that maps device pages to memory the way an IOMMU domain does. Nothing
//! it reads is the driver's bookkeeping, so a descriptor that points at the
//! wrong page reads the wrong bytes rather than the right ones by accident.
//!
//! [`round_trip`] holds the driver to a device that behaves; [`hostile`] to
//! one told to misbehave in each way the protocol leaves room for; [`model`]
//! runs random submissions and completions against a model of the disk.

mod fake;
mod hostile;
mod model;
mod round_trip;
