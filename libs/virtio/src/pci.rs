//! The virtio 1.x PCI transport's common configuration: the status protocol,
//! feature negotiation, and bringing a queue up.
//!
//! A virtio device on PCI keeps one block of registers — the common
//! configuration, virtio 1.2 §4.1.4.3 — through which every driver does the
//! same four things in the same order: reset the device and wait for the reset
//! to finish, agree on features, describe each queue's memory, and say it is
//! ready. The order is the protocol. A driver that writes a queue's addresses
//! before `FEATURES_OK` has been accepted, or that sets `DRIVER_OK` without
//! checking the device took its features, gets a device that behaves as some
//! other driver negotiated.
//!
//! Every read is of a register the device controls, so nothing read back is
//! assumed: a status that does not clear on reset times out rather than
//! spinning forever, a `FEATURES_OK` the device dropped is a refusal, and a
//! queue size of zero is a queue that does not exist. Where the protocol says
//! to give up — a feature the driver needs and the device lacks, features the
//! device refused — the device is told, by setting `FAILED`, before the error
//! is returned.
//!
//! Where the registers are is [`CommonConfig`]'s business: the kernel maps the
//! block a PCI capability names, the tests implement a device in a struct.

use core::fmt;

/// Offset of `device_feature_select`.
pub const DEVICE_FEATURE_SELECT: u32 = 0x00;
/// Offset of `device_feature`: the 32 feature bits `device_feature_select`
/// chose.
pub const DEVICE_FEATURE: u32 = 0x04;
/// Offset of `driver_feature_select`.
pub const DRIVER_FEATURE_SELECT: u32 = 0x08;
/// Offset of `driver_feature`.
pub const DRIVER_FEATURE: u32 = 0x0C;
/// Offset of `config_msix_vector`.
pub const CONFIG_MSIX_VECTOR: u32 = 0x10;
/// Offset of `num_queues`.
pub const NUM_QUEUES: u32 = 0x12;
/// Offset of `device_status`.
pub const DEVICE_STATUS: u32 = 0x14;
/// Offset of `config_generation`.
pub const CONFIG_GENERATION: u32 = 0x15;
/// Offset of `queue_select`.
pub const QUEUE_SELECT: u32 = 0x16;
/// Offset of `queue_size`.
pub const QUEUE_SIZE: u32 = 0x18;
/// Offset of `queue_msix_vector`.
pub const QUEUE_MSIX_VECTOR: u32 = 0x1A;
/// Offset of `queue_enable`.
pub const QUEUE_ENABLE: u32 = 0x1C;
/// Offset of `queue_notify_off`.
pub const QUEUE_NOTIFY_OFF: u32 = 0x1E;
/// Offset of `queue_desc`, the descriptor table's address.
pub const QUEUE_DESC: u32 = 0x20;
/// Offset of `queue_driver`, the available ring's address.
pub const QUEUE_DRIVER: u32 = 0x28;
/// Offset of `queue_device`, the used ring's address.
pub const QUEUE_DEVICE: u32 = 0x30;
/// Bytes of the common configuration a virtio 1.x device must provide.
pub const COMMON_CONFIG_LEN: u32 = 0x38;

/// Status: the guest has noticed the device.
pub const STATUS_ACKNOWLEDGE: u8 = 1;
/// Status: the guest has a driver for it.
pub const STATUS_DRIVER: u8 = 2;
/// Status: the driver is ready.
pub const STATUS_DRIVER_OK: u8 = 4;
/// Status: the driver has written its features and the device accepted them.
pub const STATUS_FEATURES_OK: u8 = 8;
/// Status: the device has hit an error it cannot recover from without a reset.
pub const STATUS_DEVICE_NEEDS_RESET: u8 = 0x40;
/// Status: the driver has given up on the device.
pub const STATUS_FAILED: u8 = 0x80;

/// Feature: the device speaks virtio 1.x rather than the legacy interface.
pub const FEATURE_VERSION_1: u64 = 1 << 32;

/// A vector number meaning "no MSI-X vector", for a queue or the configuration.
pub const NO_VECTOR: u16 = 0xFFFF;

/// The common configuration registers of one device.
///
/// Offsets passed are always below [`COMMON_CONFIG_LEN`] with room for the
/// width, so an implementation over a block at least that long needs no bounds
/// check of its own.
pub trait CommonConfig {
    /// Read the byte at `offset`.
    fn read8(&self, offset: u32) -> u8;
    /// Read the little-endian `u16` at `offset`.
    fn read16(&self, offset: u32) -> u16;
    /// Read the little-endian `u32` at `offset`.
    fn read32(&self, offset: u32) -> u32;
    /// Write the byte at `offset`.
    fn write8(&mut self, offset: u32, value: u8);
    /// Write the `u16` at `offset`.
    fn write16(&mut self, offset: u32, value: u16);
    /// Write the `u32` at `offset`.
    fn write32(&mut self, offset: u32, value: u32);
}

/// Why bringing a device up stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransportError {
    /// `device_status` did not read zero after a reset within the polls
    /// allowed.
    ResetTimedOut,
    /// The device does not offer features the driver cannot work without.
    MissingFeatures {
        /// The bits required and not offered.
        missing: u64,
    },
    /// The device cleared `FEATURES_OK`: it does not accept the features the
    /// driver wrote.
    FeaturesRefused,
    /// The queue's size reads zero, so the device has no such queue.
    NoSuchQueue {
        /// The queue asked for.
        index: u16,
    },
    /// The size asked for is not a power of two, or is larger than the queue.
    QueueSize {
        /// The queue.
        index: u16,
        /// The size asked for.
        size: u16,
        /// The largest the device supports.
        max: u16,
    },
    /// The device set `DEVICE_NEEDS_RESET`.
    NeedsReset,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TransportError::ResetTimedOut => f.write_str("the device did not finish resetting"),
            TransportError::MissingFeatures { missing } => {
                write!(f, "the device lacks required features {missing:#x}")
            }
            TransportError::FeaturesRefused => {
                f.write_str("the device refused the features the driver wrote")
            }
            TransportError::NoSuchQueue { index } => write!(f, "the device has no queue {index}"),
            TransportError::QueueSize { index, size, max } => write!(
                f,
                "queue {index} cannot be {size} entries: a power of two up to {max} is"
            ),
            TransportError::NeedsReset => f.write_str("the device needs a reset"),
        }
    }
}

/// Reset the device and wait, for at most `polls` reads, for the reset to
/// finish.
///
/// Virtio 1.2 §4.1.4.3.2: a driver writing zero to `device_status` must not
/// continue until the register reads zero. A device may take its time; the
/// caller decides what a poll costs.
///
/// # Errors
///
/// [`TransportError::ResetTimedOut`].
pub fn reset<C: CommonConfig + ?Sized>(config: &mut C, polls: u32) -> Result<(), TransportError> {
    config.write8(DEVICE_STATUS, 0);
    for _ in 0..polls {
        if config.read8(DEVICE_STATUS) == 0 {
            return Ok(());
        }
    }
    Err(TransportError::ResetTimedOut)
}

/// All 64 feature bits the device offers.
pub fn device_features<C: CommonConfig + ?Sized>(config: &mut C) -> u64 {
    config.write32(DEVICE_FEATURE_SELECT, 0);
    let low = config.read32(DEVICE_FEATURE);
    config.write32(DEVICE_FEATURE_SELECT, 1);
    let high = config.read32(DEVICE_FEATURE);
    u64::from(high) << 32 | u64::from(low)
}

/// Set `FAILED`, keeping whatever else the status says.
fn fail<C: CommonConfig + ?Sized>(config: &mut C) {
    let status = config.read8(DEVICE_STATUS);
    config.write8(DEVICE_STATUS, status | STATUS_FAILED);
}

/// Reset the device and agree on features, stopping before any queue is set
/// up.
///
/// The driver accepts every bit of `wanted` the device offers, and requires
/// every bit of `required` plus [`FEATURE_VERSION_1`]: a device without it
/// speaks the legacy interface, whose registers are not these. Returns the
/// features agreed.
///
/// # Errors
///
/// [`TransportError::ResetTimedOut`]; [`TransportError::MissingFeatures`] and
/// [`TransportError::FeaturesRefused`], after setting `FAILED`.
pub fn negotiate<C: CommonConfig + ?Sized>(
    config: &mut C,
    wanted: u64,
    required: u64,
    polls: u32,
) -> Result<u64, TransportError> {
    reset(config, polls)?;
    config.write8(DEVICE_STATUS, STATUS_ACKNOWLEDGE);
    config.write8(DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

    let offered = device_features(config);
    let required = required | FEATURE_VERSION_1;
    let missing = required & !offered;
    if missing != 0 {
        fail(config);
        return Err(TransportError::MissingFeatures { missing });
    }
    let accepted = offered & (wanted | required);

    config.write32(DRIVER_FEATURE_SELECT, 0);
    config.write32(DRIVER_FEATURE, accepted as u32);
    config.write32(DRIVER_FEATURE_SELECT, 1);
    config.write32(DRIVER_FEATURE, (accepted >> 32) as u32);

    let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
    config.write8(DEVICE_STATUS, status);
    if config.read8(DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
        fail(config);
        return Err(TransportError::FeaturesRefused);
    }
    Ok(accepted)
}

/// The largest size queue `index` can be.
///
/// # Errors
///
/// [`TransportError::NoSuchQueue`] if the device reports zero.
pub fn queue_max_size<C: CommonConfig + ?Sized>(
    config: &mut C,
    index: u16,
) -> Result<u16, TransportError> {
    config.write16(QUEUE_SELECT, index);
    match config.read16(QUEUE_SIZE) {
        0 => Err(TransportError::NoSuchQueue { index }),
        max => Ok(max),
    }
}

/// Where a split queue's three parts are, as addresses the device uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QueueAddresses {
    /// The descriptor table.
    pub descriptors: u64,
    /// The available ring.
    pub driver: u64,
    /// The used ring.
    pub device: u64,
}

/// What the device said about a queue it enabled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ActiveQueue {
    /// The queue's notification offset, to be multiplied by the notification
    /// capability's multiplier.
    pub notify_off: u16,
    /// The MSI-X vector the device kept for the queue: the one asked for, or
    /// [`NO_VECTOR`] if it could not have it.
    pub vector: u16,
}

/// Write a 64-bit register as the two halves the transport defines it as.
fn write64<C: CommonConfig + ?Sized>(config: &mut C, offset: u32, value: u64) {
    config.write32(offset, value as u32);
    config.write32(offset + 4, (value >> 32) as u32);
}

/// Describe queue `index` to the device and enable it.
///
/// Belongs between [`negotiate`] and [`driver_ok`], which is where the
/// protocol puts it. Checks `size` before writing anything, so a refused
/// queue is left exactly as it was.
///
/// # Errors
///
/// [`TransportError::NoSuchQueue`], and [`TransportError::QueueSize`] for a
/// size that is not a power of two or is larger than the device allows.
pub fn activate_queue<C: CommonConfig + ?Sized>(
    config: &mut C,
    index: u16,
    size: u16,
    addresses: QueueAddresses,
    vector: u16,
) -> Result<ActiveQueue, TransportError> {
    let max = queue_max_size(config, index)?;
    if !size.is_power_of_two() || size > max {
        return Err(TransportError::QueueSize { index, size, max });
    }
    config.write16(QUEUE_SIZE, size);
    write64(config, QUEUE_DESC, addresses.descriptors);
    write64(config, QUEUE_DRIVER, addresses.driver);
    write64(config, QUEUE_DEVICE, addresses.device);
    config.write16(QUEUE_MSIX_VECTOR, vector);
    let kept = config.read16(QUEUE_MSIX_VECTOR);
    let notify_off = config.read16(QUEUE_NOTIFY_OFF);
    config.write16(QUEUE_ENABLE, 1);
    Ok(ActiveQueue {
        notify_off,
        vector: kept,
    })
}

/// Tell the device the driver is ready.
///
/// # Errors
///
/// [`TransportError::NeedsReset`] if the device has already given up, in
/// which case `DRIVER_OK` is not set.
pub fn driver_ok<C: CommonConfig + ?Sized>(config: &mut C) -> Result<(), TransportError> {
    let status = config.read8(DEVICE_STATUS);
    if status & STATUS_DEVICE_NEEDS_RESET != 0 {
        return Err(TransportError::NeedsReset);
    }
    config.write8(DEVICE_STATUS, status | STATUS_DRIVER_OK);
    Ok(())
}

/// Bytes into the notification block that queue notifications for a queue
/// with `notify_off` are written to.
#[must_use]
pub const fn notify_offset(notify_off: u16, multiplier: u32) -> u64 {
    notify_off as u64 * multiplier as u64
}

#[cfg(test)]
mod tests;
