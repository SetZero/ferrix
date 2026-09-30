//! START: what devmgr hands a driver on its bootstrap channel.

use ferrix_blkring::control::{Message as StartMessage, START_BYTES, Start};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::signals::Signals;
use ferrix_rt::Kernel;
use ferrix_rt::native::channel::Channel;
use ferrix_rt::native::device::Device;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::port::Port;

use crate::Step;

/// What START gave: where the device is, the device itself, and the
/// driver's end of its interface core's control channel.
pub struct Started {
    /// The device's location and register blocks.
    pub start: Start,
    /// The device, with `MANAGE`.
    pub device: Device<Kernel>,
    /// The driver's end of the control channel.
    pub control: Channel<Kernel>,
}

impl core::fmt::Debug for Started {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Started")
            .field("location", &self.start.location)
            .field("pci_device_id", &self.start.pci_device_id)
            .finish_non_exhaustive()
    }
}

/// A device a driver can be started on. The type is the match: `bind` refuses
/// a START for any other device with [`Step::Identity`].
pub trait Bind: Sized {
    /// Make the device from START's `device` and `start`, with its interrupt
    /// delivered to `port` under `key`.
    ///
    /// # Errors
    ///
    /// [`Step::Identity`] for another device, [`Step::Registers`] or
    /// [`Step::Events`] when its registers or interrupt cannot be had.
    fn bind(
        device: Device<Kernel>,
        start: &Start,
        port: &Port<Kernel>,
        key: u64,
    ) -> Result<Self, Step>;
}

/// Wait for START on `boot` and take it apart.
///
/// # Errors
///
/// [`Step::Start`] if the first message is not a START with its two handles.
pub fn read(boot: &Channel<Kernel>) -> Result<Started, Step> {
    let _ = boot
        .wait_one(Signals::READABLE, Deadline::Never)
        .map_err(|_| Step::Start)?;
    let mut bytes = [0_u8; START_BYTES];
    let mut handles = [Handle::INVALID; 2];
    let received = boot
        .read(&mut bytes, &mut handles)
        .map_err(|_| Step::Start)?;
    if received.handles != 2 {
        return Err(Step::Start);
    }
    let device = Device::from_owned(OwnedHandle::from_raw(Kernel, handles[0]));
    let control = Channel::from_owned(OwnedHandle::from_raw(Kernel, handles[1]));
    match StartMessage::decode(bytes.get(..received.bytes).unwrap_or_default()) {
        Ok(StartMessage::Start(start)) => Ok(Started {
            start,
            device,
            control,
        }),
        _ => Err(Step::Start),
    }
}
