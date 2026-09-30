//! The virtio bus: a virtio device over PCI, as every virtio device logic
//! crate drives it.
//!
//! [`Device`] is the transport those crates are written against -- common
//! configuration, device configuration, the doorbell and the interrupt -- so
//! a virtio driver only brings its logic crate up on one. It also watches the
//! device status register, and makes a [`Stopped`] only after it saw the
//! device reset: status written zero, then read back zero.

use core::cell::Cell;
use core::marker::PhantomData;

use ferrix_blkring::control::Start;
use ferrix_rt::Kernel;
use ferrix_rt::native::device::{Device as DeviceHandle, Interrupt};
use ferrix_rt::native::port::Port;
use ferrix_virtio::DeviceConfig;
use ferrix_virtio::input::ConfigSelect;
use ferrix_virtio::pci::{CommonConfig, DEVICE_STATUS, NO_VECTOR};

use crate::dma::Dma;
use crate::mmio;
use crate::start::Bind;
use crate::{Step, Stopped};

/// The ISR status bit a queue interrupt sets.
const ISR_QUEUE: u8 = 1;

/// A kind of virtio device, as a type: its PCI device id lives here, once.
pub trait Kind {
    /// The modern (1.0) PCI device id, `0x1040` plus the virtio device id.
    const PCI_ID: u16;

    /// Whether START's `pci_id` is this kind: the modern id, unless a kind
    /// also takes its transitional one.
    #[must_use]
    fn matches(pci_id: u16) -> bool {
        pci_id == Self::PCI_ID
    }
}

/// virtio-input.
#[derive(Debug)]
pub enum Input {}

impl Kind for Input {
    const PCI_ID: u16 = 0x1052;
}

/// virtio-blk: the modern id, or the transitional one QEMU gives a disk on a
/// legacy-capable bus.
#[derive(Debug)]
pub enum Block {}

impl Kind for Block {
    const PCI_ID: u16 = 0x1042;

    fn matches(pci_id: u16) -> bool {
        pci_id == Self::PCI_ID || pci_id == 0x1001
    }
}

/// Where the device's reset stands, as its status register showed it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reset {
    /// Running, or never reset.
    Running,
    /// Zero was written to the status; the device has not read back zero.
    Asked,
    /// The device read back zero after it was asked: it is reset.
    Done,
}

/// A virtio device of kind `K`, over PCI.
pub struct Device<K: Kind> {
    handle: DeviceHandle<Kernel>,
    common: mmio::Block,
    notify: mmio::Block,
    isr: mmio::Block,
    device: mmio::Block,
    notify_off_multiplier: u32,
    msix: bool,
    interrupt: Interrupt<Kernel>,
    reset: Cell<Reset>,
    kind: PhantomData<K>,
}

impl<K: Kind> core::fmt::Debug for Device<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Device")
            .field("pci_id", &K::PCI_ID)
            .field("msix", &self.msix)
            .field("reset", &self.reset.get())
            .finish_non_exhaustive()
    }
}

impl<K: Kind> Bind for Device<K> {
    fn bind(
        handle: DeviceHandle<Kernel>,
        start: &Start,
        port: &Port<Kernel>,
        key: u64,
    ) -> Result<Self, Step> {
        if !K::matches(start.pci_device_id) {
            return Err(Step::Identity);
        }
        let interrupt = handle.interrupt(0).map_err(|_| Step::Registers)?;
        let common = mmio::Block::map(&handle, &start.common)?;
        let notify = mmio::Block::map(&handle, &start.notify)?;
        let isr = mmio::Block::map(&handle, &start.isr)?;
        let device = mmio::Block::map(&handle, &start.device)?;
        interrupt.bind(port, key).map_err(|_| Step::Events)?;
        Ok(Device {
            handle,
            common,
            notify,
            isr,
            device,
            notify_off_multiplier: start.notify_off_multiplier,
            msix: start.msix_table_size > 0,
            interrupt,
            reset: Cell::new(Reset::Running),
            kind: PhantomData,
        })
    }
}

impl<K: Kind> Device<K> {
    /// `pages` pages of memory this device reads and writes.
    ///
    /// # Errors
    ///
    /// [`Step::Memory`] if they cannot be made, pinned or mapped.
    pub fn dma(&self, pages: usize) -> Result<Dma, Step> {
        Dma::new(&self.handle, pages)
    }

    /// Proof of the reset, if this transport saw one finish: status written
    /// zero and read back zero, with nothing written to it since.
    #[must_use]
    pub fn stopped(&self) -> Option<Stopped> {
        (self.reset.get() == Reset::Done).then_some(Stopped(()))
    }

    /// Ring the doorbell for `queue`, whose `queue_notify_off` is
    /// `notify_off`.
    pub fn notify(&mut self, queue: u16, notify_off: u16) {
        let at = u32::from(notify_off).saturating_mul(self.notify_off_multiplier);
        self.notify.write(at, queue);
    }

    /// The MSI-X table entry a queue should interrupt through, or
    /// [`NO_VECTOR`] for a line interrupt.
    #[must_use]
    pub fn queue_vector(&self) -> u16 {
        if self.msix { 0 } else { NO_VECTOR }
    }

    /// Acknowledge the interrupt and say why it came: the ISR status byte for
    /// a line interrupt, the queue bit for MSI-X.
    pub fn acknowledge_interrupt(&mut self) -> u8 {
        if self.msix {
            let _ = self.interrupt.ack();
            ISR_QUEUE
        } else {
            let status: u8 = self.isr.read(0);
            let _ = self.interrupt.ack();
            status
        }
    }
}

impl<K: Kind> CommonConfig for Device<K> {
    fn read8(&self, offset: u32) -> u8 {
        let value: u8 = self.common.read(offset);
        if offset == DEVICE_STATUS && value == 0 && self.reset.get() == Reset::Asked {
            self.reset.set(Reset::Done);
        }
        value
    }
    fn read16(&self, offset: u32) -> u16 {
        self.common.read(offset)
    }
    fn read32(&self, offset: u32) -> u32 {
        self.common.read(offset)
    }
    fn write8(&mut self, offset: u32, value: u8) {
        if offset == DEVICE_STATUS {
            self.reset.set(if value == 0 {
                Reset::Asked
            } else {
                Reset::Running
            });
        }
        self.common.write(offset, value);
    }
    fn write16(&mut self, offset: u32, value: u16) {
        self.common.write(offset, value);
    }
    fn write32(&mut self, offset: u32, value: u32) {
        self.common.write(offset, value);
    }
}

impl<K: Kind> DeviceConfig for Device<K> {
    fn config_len(&self) -> u32 {
        self.device.len() as u32
    }
    fn config_read8(&self, offset: u32) -> u8 {
        self.device.read(offset)
    }
    fn config_read16(&self, offset: u32) -> u16 {
        self.device.read(offset)
    }
    fn config_read32(&self, offset: u32) -> u32 {
        self.device.read(offset)
    }
}

impl<K: Kind> ConfigSelect for Device<K> {
    fn config_write8(&mut self, offset: u32, value: u8) {
        self.device.write(offset, value);
    }
}

#[cfg(feature = "input")]
impl ferrix_virtio_input::Transport for Device<Input> {
    fn notify(&mut self, queue: u16, notify_off: u16) {
        Device::notify(self, queue, notify_off);
    }
    fn queue_vector(&self) -> u16 {
        Device::queue_vector(self)
    }
    fn acknowledge_interrupt(&mut self) -> u8 {
        Device::acknowledge_interrupt(self)
    }
}

#[cfg(feature = "block")]
impl ferrix_virtio_blk::Transport for Device<Block> {
    fn notify(&mut self, queue: u16, notify_off: u16) {
        Device::notify(self, queue, notify_off);
    }
    fn queue_vector(&self) -> u16 {
        Device::queue_vector(self)
    }
    fn acknowledge_interrupt(&mut self) -> u8 {
        Device::acknowledge_interrupt(self)
    }
}
