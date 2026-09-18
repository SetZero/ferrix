//! A device, and the interrupts and register windows it hands a driver.

use ferrix_native_abi::nr;
use ferrix_native_abi::types::{DEVICE_INFO_BYTES, DeviceBlock, DeviceInfo, IoMappingSpec};

use crate::call::{Call, Syscall};
use crate::channel::Channel;
use crate::error::{Error, decode, decode_handle, decode_unit};
use crate::handle::{Object, OwnedHandle, object_handle, register};
use crate::port::Port;

object_handle!(
    /// A device node, as `devmgr` hands one to a driver.
    Device
);

object_handle!(
    /// A hardware interrupt claimed from a device.
    Interrupt
);

object_handle!(
    /// An MMIO aperture claimed from a device.
    IoMapping
);

impl<S: Syscall> Device<S> {
    /// `interrupt_create`: the device's interrupt number `index`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgs`] for an index the device does not have;
    /// [`Error::AlreadyBound`] if it is claimed; [`Error::AccessDenied`]
    /// without `MANAGE`.
    pub fn interrupt(&self, index: usize) -> Result<Interrupt<S>, Error> {
        let value = Call::new(nr::INTERRUPT_CREATE)
            .value(register(self.handle()))
            .value(index)
            .make(self.syscall());
        let handle = decode_handle(value)?;
        Ok(Interrupt::from_owned(OwnedHandle::from_raw(
            self.syscall(),
            handle,
        )))
    }

    /// `io_mapping_create`: the aperture `spec` names.
    ///
    /// # Errors
    ///
    /// [`Error::AccessDenied`] for a range outside the device's apertures or
    /// without `MANAGE`; [`Error::InvalidArgs`] for one that is not whole pages.
    pub fn io_mapping(&self, spec: IoMappingSpec) -> Result<IoMapping<S>, Error> {
        let mut bytes = [0_u8; size_of::<IoMappingSpec>()];
        for (slot, byte) in bytes.iter_mut().zip(
            spec.phys
                .to_ne_bytes()
                .into_iter()
                .chain(spec.len.to_ne_bytes()),
        ) {
            *slot = byte;
        }
        let value = Call::new(nr::IO_MAPPING_CREATE)
            .value(register(self.handle()))
            .input(&bytes)
            .make(self.syscall());
        let handle = decode_handle(value)?;
        Ok(IoMapping::from_owned(OwnedHandle::from_raw(
            self.syscall(),
            handle,
        )))
    }
}

impl<S: Syscall> Device<S> {
    /// `device_info`: the device as enumeration found it, which is what
    /// whoever starts a driver on it puts in the driver's START. Any device
    /// handle will do.
    ///
    /// # Errors
    ///
    /// [`Error::WrongType`] for a handle that is not a device.
    pub fn info(&self) -> Result<DeviceInfo, Error> {
        let mut bytes = [0_u8; DEVICE_INFO_BYTES];
        let value = Call::new(nr::DEVICE_INFO)
            .value(register(self.handle()))
            .output(&mut bytes)
            .make(self.syscall());
        decode_unit(value)?;
        Ok(device_info(&bytes))
    }

    /// `device_quiesce`: the device's driver is gone, so turn its bus
    /// mastering off and release its block ring for the next driver. Needs
    /// `MANAGE`.
    ///
    /// # Errors
    ///
    /// [`Error::BadState`] while a driver still serves the device through a
    /// ring, or if the device's configuration space could not be reached;
    /// [`Error::TimedOut`] when the driver is gone but its ring has not ended
    /// within the kernel's patience, which is worth asking again;
    /// [`Error::AccessDenied`] without `MANAGE`.
    pub fn quiesce(&self) -> Result<(), Error> {
        decode_unit(
            Call::new(nr::DEVICE_QUIESCE)
                .value(register(self.handle()))
                .make(self.syscall()),
        )
    }

    /// Ask the kernel for a block ring on this device: the driver's end of
    /// the ring's control channel, over which HELLO goes next
    /// (`docs/BLOCK-RING.md` §6). Needs `MANAGE`.
    ///
    /// # Errors
    ///
    /// [`Error::WrongType`] for a handle that is not a device,
    /// [`Error::AccessDenied`] without `MANAGE`, and the kernel's refusal
    /// for a device that already has a ring.
    pub fn block_ring(&self) -> Result<Channel<S>, Error> {
        self.ring(nr::BLOCK_RING_CREATE)
    }

    /// Ask the kernel for a net ring on this device: the driver's end of the
    /// ring's control channel, over which HELLO goes next
    /// (`docs/NET-RING.md` §7). Needs `MANAGE`.
    ///
    /// # Errors
    ///
    /// [`Error::WrongType`] for a handle that is not a device,
    /// [`Error::AccessDenied`] without `MANAGE`, and the kernel's refusal for
    /// a device that already has a ring.
    pub fn net_ring(&self) -> Result<Channel<S>, Error> {
        self.ring(nr::NET_RING_CREATE)
    }

    /// Ask the kernel for the display control channel on this device: the
    /// driver's end, over which HELLO goes next (`docs/DISPLAY.md` §2.2).
    /// Needs `MANAGE`.
    ///
    /// # Errors
    ///
    /// [`Error::WrongType`] for a handle that is not a device,
    /// [`Error::AccessDenied`] without `MANAGE`, and the kernel's refusal for
    /// a device that already has one.
    pub fn display_control(&self) -> Result<Channel<S>, Error> {
        self.ring(nr::DISPLAY_CONTROL_CREATE)
    }

    /// `render_control_create`: the *render* control channel for this
    /// device, over which `libs/renderctl`'s HELLO goes next
    /// (`docs/GPU.md` §3.3).
    ///
    /// Separate from [`Device::display_control`], because a card has two
    /// conversations and a driver may serve one of them and not the other.
    ///
    /// # Errors
    ///
    /// `ALREADY_BOUND` when the device has one, and whatever the call said.
    pub fn render_control(&self) -> Result<Channel<S>, Error> {
        self.ring(nr::RENDER_CONTROL_CREATE)
    }

    /// `input_control_create`: the input control channel for this device.
    ///
    /// # Errors
    ///
    /// `ALREADY_BOUND` when the device has one, and whatever the call said.
    pub fn input_control(&self) -> Result<Channel<S>, Error> {
        self.ring(nr::INPUT_CONTROL_CREATE)
    }

    /// The body every control channel shares: one call, one handle back.
    fn ring(&self, number: usize) -> Result<Channel<S>, Error> {
        let value = Call::new(number)
            .value(register(self.handle()))
            .make(self.syscall());
        let handle = decode_handle(value)?;
        Ok(Channel::from_owned(OwnedHandle::from_raw(
            self.syscall(),
            handle,
        )))
    }
}

impl<S: Syscall> Interrupt<S> {
    /// `interrupt_bind`: deliver this interrupt to `port` as packets carrying
    /// `key`.
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyBound`]; [`Error::AccessDenied`] without `MANAGE`, or
    /// without `WRITE` on the port.
    pub fn bind(&self, port: &Port<S>, key: u64) -> Result<(), Error> {
        let key = key.to_ne_bytes();
        let value = Call::new(nr::INTERRUPT_BIND)
            .value(register(self.handle()))
            .value(register(port.handle()))
            .input(&key)
            .make(self.syscall());
        decode_unit(value)
    }

    /// `interrupt_ack`: re-arm the interrupt after servicing it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgs`] if it is not bound; [`Error::AccessDenied`]
    /// without `MANAGE`.
    pub fn ack(&self) -> Result<(), Error> {
        decode_unit(
            Call::new(nr::INTERRUPT_ACK)
                .value(register(self.handle()))
                .make(self.syscall()),
        )
    }
}

impl<S: Syscall> IoMapping<S> {
    /// `io_mapping_map`: map the aperture at `at`, or wherever it fits, and
    /// return the address.
    ///
    /// Safe at a fixed address because the kernel refuses one that overlaps
    /// any mapping (`AddressSpace::map_device`): it can add memory to this
    /// process, never replace memory it already has.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgs`] for an address that overlaps or is not the
    /// user's; [`Error::NoMemory`]; [`Error::AccessDenied`] without `MAP`.
    pub fn map(&self, at: Option<usize>) -> Result<usize, Error> {
        let value = Call::new(nr::IO_MAPPING_MAP)
            .value(register(self.handle()))
            .value(at.unwrap_or(0))
            .make(self.syscall());
        decode(value)
    }
}

/// A `DeviceInfo` as `device_info` wrote it: field by field, in the order
/// declared, native-endian.
fn device_info(bytes: &[u8; DEVICE_INFO_BYTES]) -> DeviceInfo {
    let word = |at: usize| {
        bytes
            .get(at..at + 4)
            .and_then(|word| <[u8; 4]>::try_from(word).ok())
            .map_or(0, u32::from_ne_bytes)
    };
    let half = |at: usize| {
        bytes
            .get(at..at + 2)
            .and_then(|word| <[u8; 2]>::try_from(word).ok())
            .map_or(0, u16::from_ne_bytes)
    };
    let long = |at: usize| {
        bytes
            .get(at..at + 8)
            .and_then(|word| <[u8; 8]>::try_from(word).ok())
            .map_or(0, u64::from_ne_bytes)
    };
    let block = |at: usize| DeviceBlock {
        phys: long(at),
        offset: word(at + 8),
        length: word(at + 12),
    };
    DeviceInfo {
        common: block(0),
        notify: block(16),
        isr: block(32),
        device: block(48),
        location: word(64),
        class: word(68),
        apertures: word(72),
        vectors: word(76),
        notify_off_multiplier: word(80),
        vendor_id: half(84),
        device_id: half(86),
        msix_table_size: half(88),
        virtio: half(90),
    }
}
