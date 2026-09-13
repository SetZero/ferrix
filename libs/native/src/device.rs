//! A device, and the interrupts and register windows it hands a driver.

use ferrix_native_abi::nr;
use ferrix_native_abi::types::IoMappingSpec;

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
        let value = Call::new(nr::BLOCK_RING_CREATE)
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
