//! Tests for the common configuration protocol, against a device that
//! behaves as virtio 1.2 §4.1.4.3 says.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

/// One queue's registers.
#[derive(Clone, Copy, Debug, Default)]
struct Queue {
    max: u16,
    size: u16,
    descriptors: u64,
    driver: u64,
    device: u64,
    vector: u16,
    enabled: bool,
    notify_off: u16,
}

/// A device's common configuration.
#[derive(Debug)]
struct Device {
    offered: u64,
    accepted: u64,
    device_select: u32,
    driver_select: u32,
    status: u8,
    /// Whether the device keeps `FEATURES_OK` when the driver sets it.
    takes_features: bool,
    /// Whether the device can give a queue an MSI-X vector.
    has_vectors: bool,
    queue_select: u16,
    queues: Vec<Queue>,
    /// Every value written to `device_status`, in order.
    status_writes: Vec<u8>,
}

impl Device {
    fn new(offered: u64) -> Self {
        Device {
            offered,
            accepted: 0,
            device_select: 0,
            driver_select: 0,
            status: 0x0F,
            takes_features: true,
            has_vectors: true,
            queue_select: 0,
            queues: vec![
                Queue {
                    max: 256,
                    notify_off: 0,
                    vector: NO_VECTOR,
                    ..Queue::default()
                },
                Queue {
                    max: 64,
                    notify_off: 1,
                    vector: NO_VECTOR,
                    ..Queue::default()
                },
            ],
            status_writes: Vec::new(),
        }
    }

    fn queue(&mut self) -> Option<&mut Queue> {
        self.queues.get_mut(usize::from(self.queue_select))
    }

    fn set64(slot: &mut u64, offset: u32, base: u32, value: u32) {
        if offset == base {
            *slot = (*slot & !0xFFFF_FFFF) | u64::from(value);
        } else {
            *slot = (*slot & 0xFFFF_FFFF) | u64::from(value) << 32;
        }
    }
}

impl CommonConfig for Device {
    fn read8(&self, offset: u32) -> u8 {
        match offset {
            DEVICE_STATUS => self.status,
            _ => 0,
        }
    }

    fn read16(&self, offset: u32) -> u16 {
        let queue = self.queues.get(usize::from(self.queue_select));
        match offset {
            NUM_QUEUES => self.queues.len() as u16,
            QUEUE_SIZE => queue.map_or(0, |q| q.max),
            QUEUE_MSIX_VECTOR => queue.map_or(NO_VECTOR, |q| q.vector),
            QUEUE_NOTIFY_OFF => queue.map_or(0, |q| q.notify_off),
            _ => 0,
        }
    }

    fn read32(&self, offset: u32) -> u32 {
        match (offset, self.device_select) {
            (DEVICE_FEATURE, 0) => self.offered as u32,
            (DEVICE_FEATURE, 1) => (self.offered >> 32) as u32,
            _ => 0,
        }
    }

    fn write8(&mut self, offset: u32, value: u8) {
        if offset != DEVICE_STATUS {
            return;
        }
        self.status_writes.push(value);
        if value == 0 {
            self.accepted = 0;
            for queue in &mut self.queues {
                *queue = Queue {
                    max: queue.max,
                    notify_off: queue.notify_off,
                    vector: NO_VECTOR,
                    ..Queue::default()
                };
            }
            self.status = 0;
            return;
        }
        let mut value = value;
        if value & STATUS_FEATURES_OK != 0 && !self.takes_features {
            value &= !STATUS_FEATURES_OK;
        }
        self.status = value;
    }

    fn write16(&mut self, offset: u32, value: u16) {
        let has_vectors = self.has_vectors;
        match offset {
            QUEUE_SELECT => self.queue_select = value,
            QUEUE_SIZE => {
                if let Some(q) = self.queue() {
                    q.size = value;
                }
            }
            QUEUE_MSIX_VECTOR => {
                if let Some(q) = self.queue() {
                    q.vector = if has_vectors { value } else { NO_VECTOR };
                }
            }
            QUEUE_ENABLE => {
                if let Some(q) = self.queue() {
                    q.enabled = value == 1;
                }
            }
            _ => {}
        }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        match offset {
            DEVICE_FEATURE_SELECT => self.device_select = value,
            DRIVER_FEATURE_SELECT => self.driver_select = value,
            DRIVER_FEATURE => {
                let shift = 32 * u64::from(self.driver_select.min(1));
                self.accepted =
                    (self.accepted & !(0xFFFF_FFFF << shift)) | u64::from(value) << shift;
            }
            QUEUE_DESC | 0x24 => {
                if let Some(q) = self.queue() {
                    Device::set64(&mut q.descriptors, offset, QUEUE_DESC, value);
                }
            }
            QUEUE_DRIVER | 0x2C => {
                if let Some(q) = self.queue() {
                    Device::set64(&mut q.driver, offset, QUEUE_DRIVER, value);
                }
            }
            QUEUE_DEVICE | 0x34 => {
                if let Some(q) = self.queue() {
                    Device::set64(&mut q.device, offset, QUEUE_DEVICE, value);
                }
            }
            _ => {}
        }
    }
}

/// A device whose status goes on reading nonzero for `delay` polls after a
/// reset is written.
#[derive(Debug)]
struct Resetting {
    delay: u32,
    remaining: core::cell::Cell<u32>,
}

impl CommonConfig for Resetting {
    fn read8(&self, offset: u32) -> u8 {
        if offset != DEVICE_STATUS {
            return 0;
        }
        match self.remaining.get() {
            0 => 0,
            left => {
                self.remaining.set(left - 1);
                STATUS_DRIVER_OK
            }
        }
    }
    fn read16(&self, _: u32) -> u16 {
        0
    }
    fn read32(&self, _: u32) -> u32 {
        0
    }
    fn write8(&mut self, offset: u32, value: u8) {
        if offset == DEVICE_STATUS && value == 0 {
            self.remaining.set(self.delay);
        }
    }
    fn write16(&mut self, _: u32, _: u16) {}
    fn write32(&mut self, _: u32, _: u32) {}
}

#[test]
fn negotiation_walks_the_status_protocol_in_order() {
    let mut device = Device::new(FEATURE_VERSION_1 | 0b101);
    let agreed = negotiate(&mut device, 0b001, 0, 10).unwrap();
    assert_eq!(
        agreed,
        FEATURE_VERSION_1 | 0b001,
        "wanted and offered, plus VERSION_1"
    );
    assert_eq!(device.accepted, agreed, "both halves written");
    assert_eq!(
        device.status_writes,
        vec![
            0,
            STATUS_ACKNOWLEDGE,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK
        ],
        "reset, acknowledge, driver, features"
    );
}

#[test]
fn a_feature_the_device_does_not_offer_is_never_accepted() {
    let mut device = Device::new(FEATURE_VERSION_1);
    assert_eq!(
        negotiate(&mut device, 1 << 5, 0, 10),
        Ok(FEATURE_VERSION_1),
        "bit 5 was wanted, not offered"
    );
}

#[test]
fn a_legacy_device_or_a_missing_required_feature_fails_the_device() {
    let mut device = Device::new(0b1);
    assert_eq!(
        negotiate(&mut device, 0, 0, 10),
        Err(TransportError::MissingFeatures {
            missing: FEATURE_VERSION_1
        }),
        "no VERSION_1"
    );
    assert_ne!(device.status & STATUS_FAILED, 0, "FAILED set");

    let mut device = Device::new(FEATURE_VERSION_1);
    assert_eq!(
        negotiate(&mut device, 0, 1 << 3, 10),
        Err(TransportError::MissingFeatures { missing: 1 << 3 }),
        "required bit 3"
    );
    assert_eq!(device.accepted, 0, "nothing written");
}

#[test]
fn a_device_that_drops_features_ok_has_refused() {
    let mut device = Device::new(FEATURE_VERSION_1);
    device.takes_features = false;
    assert_eq!(
        negotiate(&mut device, 0, 0, 10),
        Err(TransportError::FeaturesRefused),
        "refused"
    );
    assert_ne!(device.status & STATUS_FAILED, 0, "FAILED set");
}

#[test]
fn a_reset_is_waited_for_and_given_up_on() {
    let slow = |delay| Resetting {
        delay,
        remaining: core::cell::Cell::new(0),
    };
    assert_eq!(reset(&mut slow(3), 4), Ok(()), "clears on the fourth read");
    assert_eq!(
        reset(&mut slow(3), 3),
        Err(TransportError::ResetTimedOut),
        "three reads are not enough"
    );
    assert_eq!(
        reset(&mut slow(u32::MAX), 50),
        Err(TransportError::ResetTimedOut),
        "a status that never clears does not hang"
    );
    assert_eq!(
        reset(&mut Device::new(FEATURE_VERSION_1), 1),
        Ok(()),
        "an immediate reset"
    );
}

#[test]
fn a_queue_is_described_with_both_halves_of_every_address() {
    let mut device = Device::new(FEATURE_VERSION_1);
    let _ = negotiate(&mut device, 0, 0, 10).unwrap();
    let addresses = QueueAddresses {
        descriptors: 0x1_2345_6000,
        driver: 0x1_2345_6100,
        device: 0x1_2345_6200,
    };
    let active = activate_queue(&mut device, 1, 16, addresses, 3).unwrap();
    assert_eq!(
        active,
        ActiveQueue {
            notify_off: 1,
            vector: 3
        },
        "kept its vector"
    );
    let q = device.queues[1];
    assert_eq!(
        (q.size, q.descriptors, q.driver, q.device, q.enabled),
        (16, 0x1_2345_6000, 0x1_2345_6100, 0x1_2345_6200, true),
        "every register"
    );
    assert!(!device.queues[0].enabled, "queue 0 untouched");
}

#[test]
fn a_vector_the_device_cannot_keep_is_reported_as_none() {
    let mut device = Device::new(FEATURE_VERSION_1);
    device.has_vectors = false;
    let addresses = QueueAddresses {
        descriptors: 0x1000,
        driver: 0x2000,
        device: 0x3000,
    };
    let active = activate_queue(&mut device, 0, 8, addresses, 0).unwrap();
    assert_eq!(active.vector, NO_VECTOR, "refused");
}

#[test]
fn a_queue_that_does_not_exist_or_does_not_fit_is_left_alone() {
    let mut device = Device::new(FEATURE_VERSION_1);
    let addresses = QueueAddresses {
        descriptors: 0x1000,
        driver: 0x2000,
        device: 0x3000,
    };
    assert_eq!(
        activate_queue(&mut device, 7, 8, addresses, NO_VECTOR),
        Err(TransportError::NoSuchQueue { index: 7 }),
        "queue 7"
    );
    for size in [0, 12, 512] {
        assert_eq!(
            activate_queue(&mut device, 1, size, addresses, NO_VECTOR),
            Err(TransportError::QueueSize {
                index: 1,
                size,
                max: 64
            }),
            "size {size}"
        );
    }
    let q = device.queues[1];
    assert_eq!(
        (q.size, q.descriptors, q.enabled),
        (0, 0, false),
        "nothing written"
    );
    assert_eq!(queue_max_size(&mut device, 0), Ok(256), "queue 0's maximum");
}

#[test]
fn driver_ok_is_refused_to_a_device_that_needs_a_reset() {
    let mut device = Device::new(FEATURE_VERSION_1);
    let _ = negotiate(&mut device, 0, 0, 10).unwrap();
    assert_eq!(driver_ok(&mut device), Ok(()), "ready");
    assert_ne!(device.status & STATUS_DRIVER_OK, 0, "DRIVER_OK set");

    device.status |= STATUS_DEVICE_NEEDS_RESET;
    let before = device.status_writes.len();
    assert_eq!(
        driver_ok(&mut device),
        Err(TransportError::NeedsReset),
        "needs reset"
    );
    assert_eq!(device.status_writes.len(), before, "nothing written");
}

#[test]
fn a_notification_offset_cannot_overflow() {
    assert_eq!(notify_offset(3, 4), 12, "ordinary");
    assert_eq!(
        notify_offset(u16::MAX, u32::MAX),
        u64::from(u16::MAX) * u64::from(u32::MAX),
        "the largest"
    );
}

#[test]
fn every_error_describes_itself() {
    let errors = [
        TransportError::ResetTimedOut,
        TransportError::MissingFeatures { missing: 1 },
        TransportError::FeaturesRefused,
        TransportError::NoSuchQueue { index: 1 },
        TransportError::QueueSize {
            index: 0,
            size: 3,
            max: 8,
        },
        TransportError::NeedsReset,
    ];
    for error in errors {
        assert!(!std::format!("{error}").is_empty(), "{error:?}");
    }
}
