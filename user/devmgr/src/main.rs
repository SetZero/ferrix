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
use ferrix_devmgr_proto::{
    ANSWER_BUSY, ANSWER_DONE, ANSWER_FAILED, ANSWER_NO_DEVICE, BUS_PCI, BUS_PLATFORM,
    DEVICES_MAX_BYTES, DevicesView, Message, NAME_BYTES, SHORT_BYTES,
};
use ferrix_native_abi::handle::Handle;
use ferrix_native_abi::rights::Requested;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{
    CHANNEL_MAX_HANDLES, DEVICE_TREE_BLOCKS, DEVICE_VIRTIO_PCI, DeviceInfo, TREE_STM32_HDMI,
    TREE_STM32_USBH,
};
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

/// virtio-console's modern PCI device id and its transitional one, which is
/// the pair `docs/CLIPBOARD.md` §3.1 names.
const VIRTIO_CONSOLE_IDS: [u16; 2] = [0x1043, 0x1003];

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
    /// `docs/CLIPBOARD.md` §5 and §6: a virtio-serial port, which has no
    /// kernel subsystem at all. The driver is handed its device and a plain
    /// channel to hear START on, and everything after that is a Unix socket
    /// it binds itself.
    Port,
    /// `docs/INPUT.md` §7: a bus host, whose devices are found only as its
    /// driver enumerates the bus -- perhaps none, perhaps long after boot.
    /// Handed its device and START as a port's driver is, it asks the input
    /// core for a channel of its own per keyboard or mouse, so there is no
    /// PUBLISHED for devmgr to wait for.
    Host,
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
    /// A virtio-serial port, which publishes nowhere.
    Port,
    /// A bus host, which publishes nothing itself.
    Host,
}

/// The table: which driver, by name in the initramfs, drives which device,
/// and over which ring.
const DRIVERS: [(u16, &[u16], &[u8], Kind); 5] = [
    (VIRTIO_VENDOR, &VIRTIO_BLK_IDS, b"blk", Kind::Block),
    (VIRTIO_VENDOR, &VIRTIO_NET_IDS, b"net", Kind::Net),
    (VIRTIO_VENDOR, &VIRTIO_GPU_IDS, b"gpu", Kind::Display),
    (VIRTIO_VENDOR, &VIRTIO_INPUT_IDS, b"input", Kind::Input),
    (VIRTIO_VENDOR, &VIRTIO_CONSOLE_IDS, b"vport", Kind::Port),
];

/// The device tree bindings the kernel publishes nodes for, by the number
/// `device_info` gives each (`DEVICE_TREE_BLOCKS`): an STM32MP15 DK board's
/// HDMI output is a card, driven by `ltdc` (`docs/DISPLAY.md` §6).
///
/// Its USB host is a bus host, driven by `usbhid` (`docs/INPUT.md` §7).
const TREE_DRIVERS: [(u16, &[u8], Kind); 2] = [
    (TREE_STM32_HDMI, b"ltdc", Kind::Display),
    (TREE_STM32_USBH, b"usbhid", Kind::Host),
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

/// How many times one device's driver is started again after it dies.
///
/// A native program has no clock, so the budget is a count rather than a
/// rate: a driver that dies on every start stops being started after this
/// many, and its device stays quiesced as a device with no restart does.
const MAX_RESTARTS: u32 = 8;

/// Whether a driver of `kind` is started again when it dies
/// (`docs/DEVMGR.md` §4).
///
/// A display driver only. A disk's death under a mounted filesystem is
/// still a dead disk: what a filesystem does with a device that went and
/// came back is its own decision. The net, input and serial cores do not yet
/// wait for a dead driver's claim to go (`kernel/src/claim.rs`), so a driver
/// started again would be refused its channel.
const fn restarted(kind: Kind) -> bool {
    matches!(kind, Kind::Display)
}

/// A driver devmgr started, and what it keeps of it.
struct Started {
    /// The device's place among the DEVICES the kernel sent: what BOUND,
    /// UNBOUND, BIND and UNBIND name it by.
    index: u32,
    /// The driver's place among DEVICES' names.
    driver: u16,
    /// The device's PCI address word, as START and HELLO carry it.
    location: u32,
    /// What the device is, as `device_info` said, and which driver it takes:
    /// what starting it again needs.
    info: DeviceInfo,
    kind: Kind,
    /// How it was started, disk name and all, so a bind starts it the same
    /// way again.
    plan: Plan,
    /// Whether the last quiesce succeeded: a device that is still on is
    /// never handed to a driver again.
    quiesced: bool,
    /// A write to `unbind` asked for this driver to go, and the token its
    /// DONE carries once the death is seen and the device quiesced.
    unbinding: Option<u16>,
    /// How many times its driver has been started again.
    restarts: u32,
    /// The device, for the quiesce when the driver dies.
    device: Device<Kernel>,
    /// The driver's job, killed if it never publishes.
    job: Job<Kernel>,
    /// The driver.
    process: Process<Kernel>,
    /// Whether this driver is up: that the kernel has said it published to
    /// its subsystem, or -- for [`Kind::Port`], which has none -- simply that
    /// it started, since there is no PUBLISHED it could ever send.
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
    announce_drivers(channel, &given.names, given.drivers);
    let mut inbox = Inbox::default();
    let mut started: [Option<Started>; MAX_DEVICES] = [const { None }; MAX_DEVICES];
    let mut count = 0_u32;
    let mut failed = 0_u32;
    let mut disks = 0_u32;
    for (index, pair) in given.devices.into_iter().take(given.count).enumerate() {
        let Some((device, keep)) = pair else {
            continue;
        };
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let Ok(info) = device.info() else {
            failed += 1;
            continue;
        };
        let Some((image, kind, driver)) =
            driver_for(&info, &given.names, given.drivers, &given.images)
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
            Kind::Port => Plan::Port,
            Kind::Host => Plan::Host,
        };
        let Some(slot) = started.get_mut(count as usize) else {
            failed += 1;
            continue;
        };
        match start(&job, device, image, &info, plan, &port, u64::from(count)) {
            Ok((job, process, bootstrap)) => {
                // A bus host says when the devices plugged in at boot are
                // published, and init -- which the kernel starts at REPORT --
                // should find them: a compositor reads /dev/input once.
                if kind == Kind::Host {
                    await_settled(&bootstrap);
                }
                drop(bootstrap);
                // One at a time: the kernel's PUBLISHED for this disk before
                // the next driver starts, so disks register in PCI order
                // and two drivers never race to be vda.
                //
                // A port driver publishes to no subsystem, and a bus host's
                // devices publish when they are found, so waiting for either
                // would wait for ever and the kill below would count a
                // working driver failed. It is started and taken at its word.
                let published = if matches!(kind, Kind::Port | Kind::Host) {
                    true
                } else {
                    await_published(channel, &port, info.location, u64::from(count), &mut inbox)
                };
                if published {
                    let _ = channel.write(
                        &Message::Bound {
                            device: index,
                            driver: u32::from(driver),
                        }
                        .encode(),
                    );
                }
                *slot = Some(Started {
                    index,
                    driver,
                    location: info.location,
                    info,
                    kind,
                    plan,
                    quiesced: false,
                    unbinding: None,
                    restarts: 0,
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

    report(channel, &mut started, failed)?;
    let drivers = Drivers {
        job: &job,
        names: &given.names,
        count: given.drivers,
        images: &given.images,
    };
    serve(channel, &port, &mut started, &drivers, &mut inbox)
}

/// REPORT: every driver that published counts as started, and every one
/// that did not is ended and counts as failed, beside the `failed` that
/// never started.
fn report(
    channel: &Channel<Kernel>,
    started: &mut [Option<Started>; MAX_DEVICES],
    mut failed: u32,
) -> Result<(), Step> {
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
        .map_err(|_| Step::Report)
}

/// Tell the kernel every driver the table has an image for, and the bus of
/// the devices it drives: sysfs lists them under `/sys/bus/*/drivers`.
fn announce_drivers(
    channel: &Channel<Kernel>,
    names: &[[u8; NAME_BYTES]; MAX_DRIVERS],
    count: usize,
) {
    let pci = DRIVERS.iter().map(|(_, _, name, _)| (*name, BUS_PCI));
    let platform = TREE_DRIVERS
        .iter()
        .map(|(_, name, _)| (*name, BUS_PLATFORM));
    for (wanted, bus) in pci.chain(platform) {
        if let Some(driver) = image_index(names, count, wanted) {
            let _ = channel.write(
                &Message::Driver {
                    driver: u32::from(driver),
                    bus,
                }
                .encode(),
            );
        }
    }
}

/// The most requests held while devmgr waits for a driver to publish.
const INBOX: usize = 8;

/// BIND and UNBIND that arrived while devmgr was waiting for something else,
/// answered as soon as it is not.
#[derive(Default)]
struct Inbox {
    /// The requests, oldest first.
    held: [Option<Message>; INBOX],
}

impl Inbox {
    /// Keep `request` for later. With no room it is answered at once as
    /// failed, rather than dropped and left for the writer to time out on.
    fn keep(&mut self, channel: &Channel<Kernel>, request: Message) {
        if let Some(slot) = self.held.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(request);
        } else if let Message::Bind { token, .. } | Message::Unbind { token, .. } = request {
            done(channel, token, ANSWER_FAILED);
        }
    }

    /// The oldest request kept. `keep` fills the first free slot, so the
    /// held requests are always the front of the array, oldest first, and
    /// taking the first and rotating keeps them so.
    fn take(&mut self) -> Option<Message> {
        let request = self.held.first_mut().and_then(Option::take);
        self.held.rotate_left(1);
        request
    }
}

/// Answer the request `token` with `answer`.
fn done(channel: &Channel<Kernel>, token: u16, answer: u32) {
    let _ = channel.write(&Message::Done { token, answer }.encode());
}

/// What starting a driver again needs of what the kernel handed over.
struct Drivers<'a> {
    job: &'a Job<Kernel>,
    names: &'a [[u8; NAME_BYTES]; MAX_DRIVERS],
    count: usize,
    images: &'a [Option<Vmo<Kernel>>; MAX_DRIVERS],
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
///
/// A BIND or UNBIND that arrives meanwhile is kept in `inbox`, to be
/// answered once this driver is settled.
fn await_published(
    channel: &Channel<Kernel>,
    port: &Port<Kernel>,
    location: u32,
    key: u64,
    inbox: &mut Inbox,
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
                Ok(request @ (Message::Bind { .. } | Message::Unbind { .. })) => {
                    inbox.keep(channel, request);
                    continue;
                }
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
    // A driver that published and then died is dead all the same: its death
    // was taken here, so it is queued again for `serve_deaths`, or nothing
    // would ever quiesce the device or start the driver again.
    if published && exited {
        let _ = port.queue(key, [0, 0]);
    }
    published
}

/// For the life of the machine: deaths -- quiesce the device, tell the
/// kernel, start a driver of a kind that is [`restarted`] again -- and the
/// BIND and UNBIND a write to sysfs sends (`docs/SYSFS.md` §5).
fn serve(
    channel: &Channel<Kernel>,
    port: &Port<Kernel>,
    started: &mut [Option<Started>; MAX_DEVICES],
    drivers: &Drivers<'_>,
    inbox: &mut Inbox,
) -> Result<(), Step> {
    let _ = channel.wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_KERNEL);
    loop {
        // What arrived while a driver was being waited for comes first.
        while let Some(request) = inbox.take() {
            answer(channel, port, started, drivers, inbox, request);
        }
        let packet = port.wait(Deadline::Never).map_err(|_| Step::Wait)?;
        let key = packet.key;
        if key == KEY_KERNEL {
            take_requests(channel, inbox);
            let _ = channel.wait_async(port, Signals::READABLE | Signals::PEER_CLOSED, KEY_KERNEL);
            continue;
        }
        let Some(entry) = started.get_mut(key as usize).and_then(Option::as_mut) else {
            continue;
        };
        if entry.dead {
            continue;
        }
        entry.dead = true;
        entry.quiesced = quiesce(&entry.device);
        let _ = channel.write(
            &Message::Unbound {
                device: entry.index,
            }
            .encode(),
        );
        // An unbind asked for this death: it is answered, and nothing is
        // started again.
        if let Some(token) = entry.unbinding.take() {
            let answer = if entry.quiesced {
                ANSWER_DONE
            } else {
                ANSWER_FAILED
            };
            done(channel, token, answer);
            continue;
        }
        let _ = channel.write(
            &Message::Died {
                location: entry.location,
                status: 137,
            }
            .encode(),
        );
        // Only a device that was quiesced is handed on: until then the dead
        // driver's core may still hold it, and the new one would be refused.
        if entry.quiesced
            && restarted(entry.kind)
            && entry.restarts < MAX_RESTARTS
            && restart(channel, port, key, entry, drivers, inbox)
        {
            let _ = channel.write(
                &Message::Restarted {
                    location: entry.location,
                    restarts: entry.restarts,
                }
                .encode(),
            );
        }
    }
}

/// Everything the kernel has written, kept in `inbox` if it is a request. A
/// watch left armed by a wait for a driver fires in [`serve`] too, finding
/// nothing or what this one would have: reading is idempotent.
fn take_requests(channel: &Channel<Kernel>, inbox: &mut Inbox) {
    loop {
        let mut short = [0_u8; SHORT_BYTES];
        let Ok(got) = channel.read(&mut short, &mut []) else {
            return;
        };
        if let Ok(request @ (Message::Bind { .. } | Message::Unbind { .. })) =
            Message::decode(short.get(..got.bytes).unwrap_or(&[]))
        {
            inbox.keep(channel, request);
        }
    }
}

/// Quiesce `device`: a core that has not let go yet answers `TIMED_OUT` and
/// is asked again; a live driver's `BAD_STATE` cannot happen for a dead one.
/// Answers whether it is quiesced.
fn quiesce(device: &Device<Kernel>) -> bool {
    for _ in 0..8 {
        match device.quiesce() {
            Err(Error::TimedOut) => {}
            result => return result.is_ok(),
        }
    }
    false
}

/// Answer a BIND or UNBIND.
///
/// An UNBIND of a live driver kills its job and is answered when its death
/// comes round in [`serve`], once the device is quiesced, as Linux's
/// `unbind` returns once the driver's `remove` has. A BIND starts the driver
/// as it was started at boot and is answered once it has published. The
/// kernel checked the device and the driver exist; what only devmgr knows --
/// whether this driver drives this device, whether it is up -- is checked
/// here.
fn answer(
    channel: &Channel<Kernel>,
    port: &Port<Kernel>,
    started: &mut [Option<Started>; MAX_DEVICES],
    drivers: &Drivers<'_>,
    inbox: &mut Inbox,
    request: Message,
) {
    let (Message::Bind {
        device,
        driver,
        token,
    }
    | Message::Unbind {
        device,
        driver,
        token,
    }) = request
    else {
        return;
    };
    let found = started
        .iter_mut()
        .enumerate()
        .find(|(_, slot)| slot.as_ref().is_some_and(|entry| entry.index == device));
    let Some((key, Some(entry))) = found else {
        // devmgr never started a driver on it, so its table takes nothing
        // for it: nothing can be bound or unbound.
        done(channel, token, ANSWER_NO_DEVICE);
        return;
    };
    if entry.driver != driver {
        done(channel, token, ANSWER_NO_DEVICE);
        return;
    }
    match request {
        Message::Unbind { .. } => {
            if entry.dead || entry.unbinding.is_some() {
                done(channel, token, ANSWER_NO_DEVICE);
                return;
            }
            entry.unbinding = Some(token);
            // The death packet comes to `serve`, which answers.
            let _ = entry.job.kill();
        }
        _ => {
            if !entry.dead {
                done(channel, token, ANSWER_BUSY);
                return;
            }
            if !entry.quiesced {
                done(channel, token, ANSWER_FAILED);
                return;
            }
            let answered = if launch_again(channel, port, key as u64, entry, drivers, inbox) {
                ANSWER_DONE
            } else {
                ANSWER_FAILED
            };
            done(channel, token, answered);
        }
    }
}

/// Start `entry`'s driver again after it died, counting the restart against
/// [`MAX_RESTARTS`]. `false`, with the device left quiesced, when it could
/// not be started or did not publish.
fn restart(
    channel: &Channel<Kernel>,
    port: &Port<Kernel>,
    key: u64,
    entry: &mut Started,
    drivers: &Drivers<'_>,
    inbox: &mut Inbox,
) -> bool {
    entry.restarts += 1;
    launch_again(channel, port, key, entry, drivers, inbox)
}

/// Start `entry`'s driver again, in a job of its own, on a duplicate of the
/// device handle devmgr keeps, the way it was started at boot, and wait for
/// it to publish as at boot; tell the kernel it is bound. `false`, with the
/// device left quiesced, when it could not be started or did not publish.
fn launch_again(
    channel: &Channel<Kernel>,
    port: &Port<Kernel>,
    key: u64,
    entry: &mut Started,
    drivers: &Drivers<'_>,
    inbox: &mut Inbox,
) -> bool {
    // The dead driver's job, and anything it left running in it, goes first.
    let _ = entry.job.kill();
    let Some((image, _, _)) = driver_for(&entry.info, drivers.names, drivers.count, drivers.images)
    else {
        return false;
    };
    let Ok(device) = entry.device.duplicate(Requested::Exactly(DEVICE_RIGHTS)) else {
        return false;
    };
    let Ok((job, process, _bootstrap)) = start(
        drivers.job,
        device,
        image,
        &entry.info,
        entry.plan,
        port,
        key,
    ) else {
        return false;
    };
    entry.job = job;
    entry.process = process;
    entry.dead = false;
    entry.quiesced = false;
    let published = matches!(entry.kind, Kind::Port | Kind::Host)
        || await_published(channel, port, entry.location, key, inbox);
    if published {
        entry.published = true;
        let _ = channel.write(
            &Message::Bound {
                device: entry.index,
                driver: u32::from(entry.driver),
            }
            .encode(),
        );
        return true;
    }
    // It died before publishing, or never would: its death packet, if any,
    // was taken by the wait above, so it is ended and quiesced here.
    let _ = entry.job.kill();
    entry.dead = true;
    entry.quiesced = quiesce(&entry.device);
    false
}

/// The image of the driver for `info`, by the table, if the initramfs
/// carries it, with the driver's kind and its place among the names.
fn driver_for<'a>(
    info: &DeviceInfo,
    names: &[[u8; NAME_BYTES]; MAX_DRIVERS],
    drivers: usize,
    images: &'a [Option<Vmo<Kernel>>; MAX_DRIVERS],
) -> Option<(&'a Vmo<Kernel>, Kind, u16)> {
    let (wanted, kind) = match info.virtio {
        DEVICE_VIRTIO_PCI => DRIVERS
            .iter()
            .find(|(vendor, ids, _, _)| *vendor == info.vendor_id && ids.contains(&info.device_id))
            .map(|(_, _, wanted, kind)| (wanted, kind))?,
        DEVICE_TREE_BLOCKS => TREE_DRIVERS
            .iter()
            .find(|(binding, _, _)| *binding == info.device_id)
            .map(|(_, wanted, kind)| (wanted, kind))?,
        _ => return None,
    };
    let driver = image_index(names, drivers, wanted)?;
    let image = images.get(usize::from(driver)).and_then(Option::as_ref)?;
    Some((image, *kind, driver))
}

/// The place among the first `drivers` names of the one called `wanted`.
fn image_index(
    names: &[[u8; NAME_BYTES]; MAX_DRIVERS],
    drivers: usize,
    wanted: &[u8],
) -> Option<u16> {
    let at = (0..drivers).find(|&j| {
        names.get(j).is_some_and(|name| {
            let end = name
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(name.len());
            name.get(..end) == Some(wanted)
        })
    })?;
    u16::try_from(at).ok()
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
) -> Result<Launched, ()> {
    match plan {
        Plan::Block(name) => start_block(job, device, image, info, name, port, key),
        Plan::Net => start_net(job, device, image, info, port, key),
        Plan::Display => start_display(job, device, image, info, port, key),
        Plan::Input => start_input(job, device, image, info, port, key),
        Plan::Port => start_plain(job, device, image, info, port, key, "vport"),
        Plan::Host => start_plain(job, device, image, info, port, key, "usbhid"),
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
) -> Result<Launched, ()> {
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
) -> Result<Launched, ()> {
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
///
/// A board's HDMI output (`docs/DISPLAY.md` §6) takes the same START with
/// its two register windows where the virtio blocks would be: the LTDC's in
/// `common`, the bridge's I2C controller's in `device`.
fn start_display(
    job: &Job<Kernel>,
    device: Device<Kernel>,
    image: &Vmo<Kernel>,
    info: &DeviceInfo,
    port: &Port<Kernel>,
    key: u64,
) -> Result<Launched, ()> {
    let (program, program_name) = if info.virtio == DEVICE_TREE_BLOCKS {
        ("ltdc", &[b'l', b't', b'd', b'c', 0, 0, 0, 0])
    } else {
        ("gpu", &[b'g', b'p', b'u', 0, 0, 0, 0, 0])
    };
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
        name: *program_name,
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
        program,
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
) -> Result<Launched, ()> {
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

/// A driver given its device and nothing else, as `program`: a virtio-serial
/// port's, which serves no kernel subsystem, or a bus host's, which asks for
/// its channels itself.
///
/// Every other kind asks the device for a control channel of its subsystem's
/// kind, and the kernel learns from that what the driver is for. A port has
/// no subsystem to name (`docs/CLIPBOARD.md` §5), and a USB host has one
/// input device per keyboard or mouse it finds, which only it can count
/// (`docs/INPUT.md` §7), so `launch` makes the bootstrap channel START
/// travels on, as it does for all of them, and the driver makes the rest.
/// Nothing here waits for it: a port has no PUBLISHED it could ever send, and
/// a host's devices publish whenever they are plugged in.
fn start_plain(
    job: &Job<Kernel>,
    device: Device<Kernel>,
    image: &Vmo<Kernel>,
    info: &DeviceInfo,
    port: &Port<Kernel>,
    key: u64,
    program: &str,
) -> Result<Launched, ()> {
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
        name: start_name(program),
    };
    let device = device
        .into_owned()
        .replace(Requested::Exactly(DEVICE_RIGHTS))
        .map_err(|_| ())?;
    let encoded = Ring::Start(start).encode();
    launch(job, image, program, port, key, encoded.as_bytes(), [device])
}

/// START's name for `program`: its first eight bytes, NUL-padded.
fn start_name(program: &str) -> [u8; 8] {
    let mut name = [0_u8; 8];
    for (slot, byte) in name.iter_mut().zip(program.bytes()) {
        *slot = byte;
    }
    name
}

/// How long a bus host may take to settle its first enumeration before
/// devmgr reports without it: the DK board's hub, mouse and keyboard take a
/// second (`docs/INPUT.md` §7.3).
const SETTLE_NANOS: u64 = 5_000_000_000;

/// Wait for a bus host's driver to say its first enumeration has settled --
/// any message on its bootstrap channel -- or to die, or for
/// [`SETTLE_NANOS`]: whichever comes first. What it says is not read; a
/// driver that says nothing costs the boot the wait and no more.
fn await_settled(bootstrap: &Channel<Kernel>) {
    let Ok(now) = ferrix_rt::linux::monotonic_nanos() else {
        return;
    };
    let _ = bootstrap.wait_one(
        Signals::READABLE | Signals::PEER_CLOSED,
        Deadline::At(now.saturating_add(SETTLE_NANOS)),
    );
}

/// The job, the process, the bootstrap channel and the watch every driver
/// starts with: only START's bytes and handles differ between the rings.
fn launch<const N: usize>(
    job: &Job<Kernel>,
    image: &Vmo<Kernel>,
    program: &str,
    port: &Port<Kernel>,
    key: u64,
    start: &[u8],
    handles: [OwnedHandle<Kernel>; N],
) -> Result<Launched, ()> {
    let child_job = job.create_child().map_err(|_| ())?;
    let process = pending::create_process(&child_job, image, program).map_err(|_| ())?;
    let (near, far) = channel::create(Kernel).map_err(|_| ())?;
    near.write_with(start, handles).map_err(|_| ())?;
    process.notify_on_exit(port, key).map_err(|_| ())?;
    process.start(far.into_owned()).map_err(|_| ())?;
    Ok((child_job, process, near))
}

/// A driver as `launch` leaves it: its job, the process, and devmgr's end of
/// the channel START went down, which only a bus host's driver answers on.
type Launched = (Job<Kernel>, Process<Kernel>, Channel<Kernel>);
