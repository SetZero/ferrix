//! `devmgr`: the program that matches the devices the kernel found to the
//! drivers the initramfs carries, starts each driver in a job of its own with
//! exactly what `docs/ARCHITECTURE.md` §7 says a driver gets, and makes a
//! device safe again when its driver dies. `docs/DEVMGR.md` is the protocol.
//!
//! It drives nothing and reads no files. The kernel hands it, in DEVICES
//! messages on its bootstrap channel, a job, every device node twice and
//! every driver image as memory; `device_info` says what each device is; a
//! table here, and nowhere in the kernel, says which driver it takes.
//!
//! The exit status is the diagnosis: 0 never comes, since `devmgr` runs for
//! the life of the machine; every other number names the step that failed
//! (see [`Step`]).

#![no_std]
#![no_main]

use ferrix_blkring::control::{Block, CONTROL_RIGHTS, DEVICE_RIGHTS, Message as Ring, Start};
use ferrix_blkring::identity::DiskName;
use ferrix_devmgr_proto::{DEVICES_MAX_BYTES, DevicesView, Message, NAME_BYTES, SHORT_BYTES};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::Requested;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{CHANNEL_MAX_HANDLES, DEVICE_VIRTIO_PCI, DeviceInfo};
use ferrix_netring::control::{
    CONTROL_RIGHTS as NET_CONTROL_RIGHTS, DEVICE_RIGHTS as NET_DEVICE_RIGHTS, MAX_MESSAGE,
    Message as NetRing, Start as NetStart,
};
use ferrix_rt::native::channel::{self, Channel, ReadError};
use ferrix_rt::native::device::Device;
use ferrix_rt::native::error::Error;
use ferrix_rt::native::handle::{Deadline, Object, OwnedHandle};
use ferrix_rt::native::job::Job;
use ferrix_rt::native::pending::{self, Process};
use ferrix_rt::native::port::{self, Port};
use ferrix_rt::native::vmo::Vmo;
use ferrix_rt::{Bootstrap, Kernel};

ferrix_rt::entry!(main);

/// The most devices devmgr keeps, over every DEVICES message: ARMv7-A's
/// machine publishes 36.
const MAX_DEVICES: usize = 64;
/// The most drivers, as the protocol fixes it.
const MAX_DRIVERS: usize = ferrix_devmgr_proto::MAX_DRIVERS;

/// virtio's PCI vendor, and virtio-blk's and virtio-net's modern and
/// transitional device ids.
const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_IDS: [u16; 2] = [0x1042, 0x1001];
const VIRTIO_NET_IDS: [u16; 2] = [0x1041, 0x1000];
/// virtio-gpu has only a modern id.
const VIRTIO_GPU_IDS: [u16; 1] = [0x1050];
/// virtio-input, the same (`docs/DEVMGR.md`). QEMU's keyboard, mouse and
/// tablet are three functions of it, so one driver process starts per
/// device, as one blk driver starts per disk.
const VIRTIO_INPUT_IDS: [u16; 1] = [0x1052];

/// Which kind of ring a driver serves its device over. The two rings are
/// separate protocols with separate kernel ends, and the only thing devmgr
/// does differently between them is which one it asks the kernel to make and
/// which START it writes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `docs/BLOCK-RING.md`: a disk, named `vda` and upwards.
    Block,
    /// `docs/NET-RING.md`: a network interface, named by the kernel.
    Net,
    /// `docs/DISPLAY.md`: a card, numbered by the kernel.
    Display,
    /// `docs/INPUT.md`: an input device, numbered by the kernel.
    Input,
}

/// A driver about to be started: its kind, carrying what only that kind
/// needs. The disk name is in here rather than beside it because a function
/// that took both would take one argument too many, and because a net driver
/// with a disk name would be a thing the table could say and the protocol
/// could not carry.
#[derive(Clone, Copy)]
enum Plan {
    /// A disk, to be `vda` or whichever letter is next.
    Block(DiskName),
    /// A network interface, whose name the kernel chooses.
    Net,
    /// A card, whose number the kernel chooses.
    Display,
    /// An input device, whose number the kernel chooses.
    Input,
}

/// The table: which driver, by name in the initramfs, drives which device,
/// and over which ring.
const DRIVERS: [(u16, &[u16], &[u8], Kind); 4] = [
    (VIRTIO_VENDOR, &VIRTIO_BLK_IDS, b"blk", Kind::Block),
    (VIRTIO_VENDOR, &VIRTIO_NET_IDS, b"net", Kind::Net),
    (VIRTIO_VENDOR, &VIRTIO_GPU_IDS, b"gpu", Kind::Display),
    (VIRTIO_VENDOR, &VIRTIO_INPUT_IDS, b"input", Kind::Input),
];

/// Where devmgr gave up, as the exit status.
#[repr(i32)]
enum Step {
    /// Started with no bootstrap channel.
    NoBootstrap = 1,
    /// A DEVICES message could not be read.
    ReadDevices = 2,
    /// DEVICES did not decode, its handles did not match its counts, it
    /// named more than devmgr keeps, or no message carried the job.
    Devices = 3,
    /// No port for the drivers' ends.
    Port = 4,
    /// REPORT could not be sent.
    Report = 5,
    /// The port wait failed, which only a killed devmgr sees.
    Wait = 6,
}

/// A driver devmgr started, and what it keeps of it.
struct Started {
    /// The device's PCI address word, as START and HELLO carry it.
    location: u32,
    /// The device, for the quiesce when the driver dies.
    device: Device<Kernel>,
    /// The driver's job, killed if it never publishes.
    job: Job<Kernel>,
    /// The driver.
    process: Process<Kernel>,
    /// Whether the kernel has said its disk is published.
    published: bool,
    /// Whether it has ended.
    dead: bool,
}

/// What the kernel handed over: the job, the devices twice, the images.
struct Given {
    /// The root job, from the first message.
    job: Option<Job<Kernel>>,
    /// Every device, twice: one to give a driver, one to keep.
    devices: [Option<(Device<Kernel>, Device<Kernel>)>; MAX_DEVICES],
    /// How many of `devices` are filled.
    count: usize,
    /// The driver images, from the first message.
    images: [Option<Vmo<Kernel>>; MAX_DRIVERS],
    /// Their names, NUL-padded.
    names: [[u8; NAME_BYTES]; MAX_DRIVERS],
    /// How many of `images` and `names` are filled.
    drivers: usize,
}

fn main(bootstrap: Bootstrap) -> i32 {
    let Some(channel) = bootstrap else {
        return Step::NoBootstrap as i32;
    };
    match run(&channel) {
        Ok(()) => 0,
        Err(step) => step as i32,
    }
}

/// Everything, in `docs/DEVMGR.md`'s order: read DEVICES, start a driver per
/// match one at a time, REPORT, then serve deaths for ever.
fn run(channel: &Channel<Kernel>) -> Result<(), Step> {
    let given = receive(channel)?;
    let Some(job) = given.job else {
        return Err(Step::Devices);
    };
    let port = port::create(Kernel).map_err(|_| Step::Port)?;
    let mut started: [Option<Started>; MAX_DEVICES] = [const { None }; MAX_DEVICES];
    let mut count = 0_u32;
    let mut failed = 0_u32;
    let mut disks = 0_u32;
    for (device, keep) in given.devices.into_iter().take(given.count).flatten() {
        let Ok(info) = device.info() else {
            failed += 1;
            continue;
        };
        let Some((image, kind)) = driver_for(&info, &given.names, given.drivers, &given.images)
        else {
            // A device nobody drives: both handles close here.
            continue;
        };
        // Only a disk is named here, and only disks are counted, so a net
        // driver between two disks does not shift the second one's letter.
        let plan = match kind {
            Kind::Block => {
                let Some(name) = DiskName::for_index(disks) else {
                    failed += 1;
                    continue;
                };
                disks += 1;
                Plan::Block(name)
            }
            Kind::Net => Plan::Net,
            Kind::Display => Plan::Display,
            Kind::Input => Plan::Input,
        };
        let Some(slot) = started.get_mut(count as usize) else {
            failed += 1;
            continue;
        };
        match start(&job, device, image, &info, plan, &port, u64::from(count)) {
            Ok((job, process)) => {
                // One at a time: the kernel's PUBLISHED for this disk before
                // the next driver starts, so disks register in PCI order
                // and two drivers never race to be vda.
                let published = await_published(channel, &port, info.location, u64::from(count));
                *slot = Some(Started {
                    location: info.location,
                    device: keep,
                    job,
                    process,
                    published,
                    dead: false,
                });
                count += 1;
            }
            Err(()) => failed += 1,
        }
    }

    let mut published = 0_u32;
    for entry in started.iter_mut().flatten() {
        if entry.published {
            published += 1;
        } else {
            let _ = entry.job.kill();
            entry.dead = true;
            failed += 1;
        }
    }
    channel
        .write(
            &Message::Report {
                started: published,
                failed,
            }
            .encode(),
        )
        .map_err(|_| Step::Report)?;
    serve_deaths(channel, &port, &mut started)
}

/// Read every DEVICES message the kernel wrote: the first with the job and
/// the images, the rest with devices only, until none are still to come.
fn receive(channel: &Channel<Kernel>) -> Result<Given, Step> {
    let mut given = Given {
        job: None,
        devices: [const { None }; MAX_DEVICES],
        count: 0,
        images: [const { None }; MAX_DRIVERS],
        names: [[0; NAME_BYTES]; MAX_DRIVERS],
        drivers: 0,
    };
    loop {
        let mut bytes = [0_u8; DEVICES_MAX_BYTES];
        let mut handles = [Handle::INVALID; CHANNEL_MAX_HANDLES];
        let _ = channel
            .wait_one(Signals::READABLE | Signals::PEER_CLOSED, Deadline::Never)
            .map_err(|_| Step::ReadDevices)?;
        let received = channel
            .read(&mut bytes, &mut handles)
            .map_err(|_| Step::ReadDevices)?;
        let view = DevicesView::decode(bytes.get(..received.bytes).unwrap_or(&[]))
            .map_err(|_| Step::Devices)?;
        let devices = view.devices as usize;
        let drivers = view.drivers as usize;
        if received.handles != view.handles()
            || given.count + devices > MAX_DEVICES
            || drivers > MAX_DRIVERS
        {
            return Err(Step::Devices);
        }
        let owned = |index: usize| {
            OwnedHandle::from_raw(
                Kernel,
                handles.get(index).copied().unwrap_or(Handle::INVALID),
            )
        };
        let base = usize::from(view.first);
        if view.first {
            given.job = Some(Job::from_owned(owned(0)));
            given.drivers = drivers;
            for j in 0..drivers {
                if let (Some(image), Some(name)) = (given.images.get_mut(j), given.names.get_mut(j))
                {
                    *image = Some(Vmo::from_owned(owned(base + 2 * devices + j)));
                    *name = view.name(j).unwrap_or([0; NAME_BYTES]);
                }
            }
        }
        for i in 0..devices {
            if let Some(slot) = given.devices.get_mut(given.count + i) {
                *slot = Some((
                    Device::from_owned(owned(base + 2 * i)),
                    Device::from_owned(owned(base + 2 * i + 1)),
                ));
            }
        }
        given.count += devices;
        if view.more == 0 {
            return Ok(given);
        }
    }
}

/// The port key the kernel's channel is watched under while a driver starts:
/// above every driver's, which is its index.
const KEY_KERNEL: u64 = u64::MAX;

/// Wait for the kernel's PUBLISHED for the device at `location`, or for its
/// driver, watched on `port` under `key`, to exit first: a driver refused
/// before READY (`docs/DISPLAY.md` §2.4) or failing before it serves never
/// publishes, and the boot goes on without that device (os-02's review). A
/// native program has no clock, so a driver that neither publishes nor exits
/// holds devmgr here until the kernel's patience for REPORT runs out. A
/// channel that closes or says something else answers `false`.
fn await_published(
    channel: &Channel<Kernel>,
    port: &Port<Kernel>,
    location: u32,
    key: u64,
) -> bool {
    // Other drivers' deaths arrive here too; they are queued again after, for
    // `serve_deaths`.
    let mut deaths = [None::<u64>; MAX_DEVICES];
    let mut held = 0_usize;
    let mut exited = false;
    let published = loop {
        let mut short = [0_u8; SHORT_BYTES];
        match channel.read(&mut short, &mut []) {
            Ok(got) => match Message::decode(short.get(..got.bytes).unwrap_or(&[])) {
                Ok(Message::Published { location: at }) if at == location => break true,
                Ok(Message::Published { .. }) => continue,
                _ => break false,
            },
            // A driver that published and then died is still published: the
            // channel is read once more after its exit before giving up.
            Err(ReadError::Failed(Error::ShouldWait)) if exited => break false,
            Err(ReadError::Failed(Error::ShouldWait)) => {}
            Err(_) => break false,
        }
        if channel
            .wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_KERNEL)
            .is_err()
        {
            break false;
        }
        let Ok(packet) = port.wait(Deadline::Never) else {
            break false;
        };
        if packet.key == key {
            exited = true;
            continue;
        }
        if packet.key != KEY_KERNEL
            && let Some(slot) = deaths.get_mut(held)
        {
            *slot = Some(packet.key);
            held += 1;
        }
    };
    for died in deaths.into_iter().flatten() {
        let _ = port.queue(died, [0, 0]);
    }
    published
}

/// Deaths, for the life of the machine: quiesce the device, tell the kernel.
fn serve_deaths(
    channel: &Channel<Kernel>,
    port: &Port<Kernel>,
    started: &mut [Option<Started>; MAX_DEVICES],
) -> Result<(), Step> {
    loop {
        let packet = port.wait(Deadline::Never).map_err(|_| Step::Wait)?;
        let Some(entry) = started
            .get_mut(packet.key as usize)
            .and_then(Option::as_mut)
        else {
            continue;
        };
        if entry.dead {
            continue;
        }
        entry.dead = true;
        // A ring that has not let go yet answers TIMED_OUT and is asked
        // again; a live driver's BAD_STATE cannot happen for a dead one.
        for _ in 0..8 {
            match entry.device.quiesce() {
                Err(Error::TimedOut) => {}
                _ => break,
            }
        }
        let _ = channel.write(
            &Message::Died {
                location: entry.location,
                status: 137,
            }
            .encode(),
        );
        let _ = &entry.process;
    }
}

/// The image of the driver for `info`, by the table, if the initramfs
/// carries it.
fn driver_for<'a>(
    info: &DeviceInfo,
    names: &[[u8; NAME_BYTES]; MAX_DRIVERS],
    drivers: usize,
    images: &'a [Option<Vmo<Kernel>>; MAX_DRIVERS],
) -> Option<(&'a Vmo<Kernel>, Kind)> {
    if info.virtio != DEVICE_VIRTIO_PCI {
        return None;
    }
    let (_, _, wanted, kind) = DRIVERS
        .iter()
        .find(|(vendor, ids, _, _)| *vendor == info.vendor_id && ids.contains(&info.device_id))?;
    let image = (0..drivers)
        .find(|&j| {
            names.get(j).is_some_and(|name| {
                let end = name
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(name.len());
                name.get(..end) == Some(*wanted)
            })
        })
        .and_then(|j| images.get(j).and_then(Option::as_ref))?;
    Some((image, *kind))
}

/// Start `image` on `device`: a ring of the kind the table names, a job, a
/// process, START over its bootstrap, a watch on its end, and go.
fn start(
    job: &Job<Kernel>,
    device: Device<Kernel>,
    image: &Vmo<Kernel>,
    info: &DeviceInfo,
    plan: Plan,
    port: &Port<Kernel>,
    key: u64,
) -> Result<(Job<Kernel>, Process<Kernel>), ()> {
    match plan {
        Plan::Block(name) => start_block(job, device, image, info, name, port, key),
        Plan::Net => start_net(job, device, image, info, port, key),
        Plan::Display => start_display(job, device, image, info, port, key),
        Plan::Input => start_input(job, device, image, info, port, key),
    }
}

/// A virtio-blk driver, over a block ring, for the disk to be `name`.
fn start_block(
    job: &Job<Kernel>,
    device: Device<Kernel>,
    image: &Vmo<Kernel>,
    info: &DeviceInfo,
    name: DiskName,
    port: &Port<Kernel>,
    key: u64,
) -> Result<(Job<Kernel>, Process<Kernel>), ()> {
    let control = device.block_ring().map_err(|_| ())?;
    let block = |block: ferrix_native_abi::types::DeviceBlock| Block {
        phys: block.phys,
        offset: block.offset,
        length: block.length,
    };
    let start = Start {
        common: block(info.common),
        notify: block(info.notify),
        isr: block(info.isr),
        device: block(info.device),
        notify_off_multiplier: info.notify_off_multiplier,
        msix_table_size: info.msix_table_size,
        pci_device_id: info.device_id,
        location: info.location,
        name: *name.as_bytes(),
    };
    // Exactly the rights the driver may hold: the kernel handed the device
    // with DEVICE_RIGHTS and the control end with CONTROL_RIGHTS already, and
    // replacing to the same set is what makes that a fact rather than a hope.
    let device = device
        .into_owned()
        .replace(Requested::Exactly(DEVICE_RIGHTS))
        .map_err(|_| ())?;
    let control = control
        .into_owned()
        .replace(Requested::Exactly(CONTROL_RIGHTS))
        .map_err(|_| ())?;
    let encoded = Ring::Start(start).encode();
    launch(
        job,
        image,
        "blk",
        port,
        key,
        encoded.as_bytes(),
        [device, control],
    )
}

/// A virtio-net driver, over a net ring. The kernel names the interface, so
/// START carries no name: only which device it is, and index zero for
/// "choose one".
fn start_net(
    job: &Job<Kernel>,
    device: Device<Kernel>,
    image: &Vmo<Kernel>,
    info: &DeviceInfo,
    port: &Port<Kernel>,
    key: u64,
) -> Result<(Job<Kernel>, Process<Kernel>), ()> {
    let control = device.net_ring().map_err(|_| ())?;
    let start = NetStart {
        index: 0,
        location: info.location,
    };
    let device = device
        .into_owned()
        .replace(Requested::Exactly(NET_DEVICE_RIGHTS))
        .map_err(|_| ())?;
    let control = control
        .into_owned()
        .replace(Requested::Exactly(NET_CONTROL_RIGHTS))
        .map_err(|_| ())?;
    let mut bytes = [0_u8; MAX_MESSAGE];
    let written = NetRing::Start(start).encode(&mut bytes).map_err(|_| ())?;
    launch(
        job,
        image,
        "net",
        port,
        key,
        bytes.get(..written).unwrap_or_default(),
        [device, control],
    )
}

/// A virtio-gpu driver, over the display control channel
/// (`docs/DISPLAY.md` §2.2). START is the block driver's: where the device's
/// virtio register blocks are, which a virtio-gpu driver needs the same way;
/// its name field is unused, since the kernel numbers the card.
fn start_display(
    job: &Job<Kernel>,
    device: Device<Kernel>,
    image: &Vmo<Kernel>,
    info: &DeviceInfo,
    port: &Port<Kernel>,
    key: u64,
) -> Result<(Job<Kernel>, Process<Kernel>), ()> {
    let control = device.display_control().map_err(|_| ())?;
    let block = |block: ferrix_native_abi::types::DeviceBlock| Block {
        phys: block.phys,
        offset: block.offset,
        length: block.length,
    };
    let start = Start {
        common: block(info.common),
        notify: block(info.notify),
        isr: block(info.isr),
        device: block(info.device),
        notify_off_multiplier: info.notify_off_multiplier,
        msix_table_size: info.msix_table_size,
        pci_device_id: info.device_id,
        location: info.location,
        name: [b'g', b'p', b'u', 0, 0, 0, 0, 0],
    };
    let device = device
        .into_owned()
        .replace(Requested::Exactly(DEVICE_RIGHTS))
        .map_err(|_| ())?;
    let control = control
        .into_owned()
        .replace(Requested::Exactly(CONTROL_RIGHTS))
        .map_err(|_| ())?;
    let encoded = Ring::Start(start).encode();
    launch(
        job,
        image,
        "gpu",
        port,
        key,
        encoded.as_bytes(),
        [device, control],
    )
}

/// A virtio-input driver, over an input control channel.
///
/// The same shape as the display's: the device, the channel, and where the
/// register blocks are. The name in START is the program's, which is what a
/// driver checks its device id against.
fn start_input(
    job: &Job<Kernel>,
    device: Device<Kernel>,
    image: &Vmo<Kernel>,
    info: &DeviceInfo,
    port: &Port<Kernel>,
    key: u64,
) -> Result<(Job<Kernel>, Process<Kernel>), ()> {
    let control = device.input_control().map_err(|_| ())?;
    let block = |block: ferrix_native_abi::types::DeviceBlock| Block {
        phys: block.phys,
        offset: block.offset,
        length: block.length,
    };
    let start = Start {
        common: block(info.common),
        notify: block(info.notify),
        isr: block(info.isr),
        device: block(info.device),
        notify_off_multiplier: info.notify_off_multiplier,
        msix_table_size: info.msix_table_size,
        pci_device_id: info.device_id,
        location: info.location,
        name: [b'i', b'n', b'p', b'u', b't', 0, 0, 0],
    };
    let device = device
        .into_owned()
        .replace(Requested::Exactly(DEVICE_RIGHTS))
        .map_err(|_| ())?;
    let control = control
        .into_owned()
        .replace(Requested::Exactly(CONTROL_RIGHTS))
        .map_err(|_| ())?;
    let encoded = Ring::Start(start).encode();
    launch(
        job,
        image,
        "input",
        port,
        key,
        encoded.as_bytes(),
        [device, control],
    )
}

/// The job, the process, the bootstrap channel and the watch every driver
/// starts with: only START's bytes and handles differ between the rings.
fn launch(
    job: &Job<Kernel>,
    image: &Vmo<Kernel>,
    program: &str,
    port: &Port<Kernel>,
    key: u64,
    start: &[u8],
    handles: [OwnedHandle<Kernel>; 2],
) -> Result<(Job<Kernel>, Process<Kernel>), ()> {
    let child_job = job.create_child().map_err(|_| ())?;
    let process = pending::create_process(&child_job, image, program).map_err(|_| ())?;
    let (near, far) = channel::create(Kernel).map_err(|_| ())?;
    near.write_with(start, handles).map_err(|_| ())?;
    process.notify_on_exit(port, key).map_err(|_| ())?;
    process.start(far.into_owned()).map_err(|_| ())?;
    drop(near);
    Ok((child_job, process))
}
