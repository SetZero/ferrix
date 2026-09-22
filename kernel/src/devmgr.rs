//! Starting `devmgr`, and what the kernel says to it: `docs/DEVMGR.md` §2.
//!
//! After the boot checks and before `init`, the kernel reads `/sbin/devmgr`
//! and every driver the initramfs lists in `/lib/drivers/MANIFEST` out of the
//! root, puts each driver's image in an anonymous VMO, and starts `devmgr`
//! as a native process whose bootstrap channel already holds one DEVICES
//! message: a job, every device node twice, and the images by name. It then
//! waits for `devmgr`'s REPORT, which says what it started, and prints it.
//! From then on the kernel's end of the channel carries PUBLISHED to
//! `devmgr` whenever a ring on a device accepts a HELLO, and DIED from it,
//! which a task of the kernel's prints.
//!
//! The kernel reads the images; it never runs them. An image is memory
//! `process_create` loads, never a file mapping, which is what keeps a driver
//! from ever faulting on the disk it serves (`docs/DEVMGR.md` §5).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::convert::Infallible;

use ferrix_blkring::Location;
use ferrix_blkring::control::DEVICE_RIGHTS;
use ferrix_bootinfo::PAGE_SIZE;
use ferrix_devmgr_proto::{DEVICES_MAX_BYTES, Devices, Message, NAME_BYTES, SHORT_BYTES};
use ferrix_native_abi::rights::Rights;
use ferrix_native_abi::signals::Signals;
use ferrix_native_abi::types::{CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES};

use crate::object::channel::{Endpoint, ReadError};
use crate::object::job::Job;
use crate::object::{Object, Transfer};
use crate::sync::SpinLock;
use crate::syscall::{exec, process};
use crate::user::vmo::Vmo;
use crate::{device, fs, sched, timer};

/// The program.
const PROGRAM: &[u8] = b"/sbin/devmgr";
/// Where the drivers are, and the file that lists them, one name per line.
const DRIVERS: &[u8] = b"/lib/drivers";
const MANIFEST: &[u8] = b"/lib/drivers/MANIFEST";
/// What `devmgr` is listed as.
const NAME: &[u8] = b"devmgr";

/// How long `devmgr` has to start every driver and report: a driver resets
/// and negotiates with its device, and the kernel's ring task publishes on
/// HELLO; generous under TCG.
const REPORT_PATIENCE_NANOS: u64 = 20_000_000_000;

/// The rights of the device handle `devmgr` keeps, the second of each pair:
/// a driver's, and `DUPLICATE`, so a driver that died can be started again
/// with a handle of its own while `devmgr` keeps this one for the next
/// quiesce (`docs/DEVMGR.md` §4).
const KEPT_DEVICE_RIGHTS: Rights = Rights(DEVICE_RIGHTS.0 | Rights::DUPLICATE.0);

/// The kernel's end of `devmgr`'s bootstrap channel, once it is started.
static CHANNEL: SpinLock<Option<Arc<Endpoint>>> = SpinLock::new(None);

/// What `devmgr` reported, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// Device nodes handed over.
    pub(crate) devices: usize,
    /// Driver images handed over.
    pub(crate) drivers: usize,
    /// Drivers started whose disks the kernel accepted.
    pub(crate) started: u32,
    /// Devices that matched a driver and got none.
    pub(crate) failed: u32,
}

/// Start `devmgr` and wait for its REPORT. `Ok(None)` when the image carries
/// no `/sbin/devmgr`, which a boot without native programs is.
///
/// # Errors
///
/// What did not happen, as a sentence: `devmgr` could not be started, or
/// said nothing in time, or something other than REPORT.
pub(crate) fn start() -> Result<Option<Report>, &'static str> {
    let ctx = fs::namespace().context();
    let Ok(image) = fs::read_file(&ctx, None, PROGRAM) else {
        return Ok(None);
    };
    let names = manifest(&ctx);
    let mut images = Vec::new();
    for name in &names {
        let mut path = DRIVERS.to_vec();
        path.push(b'/');
        path.extend_from_slice(name_bytes(name));
        let bytes = fs::read_file(&ctx, None, &path)
            .map_err(|_| "a driver the manifest names is not in the image")?;
        images.push(vmo_of(&bytes)?);
    }
    let nodes = device::devices();
    let (kernel_end, devmgr_end) = Endpoint::pair().ok_or("no memory for devmgr's channel")?;
    // As many DEVICES messages as the handles need: the first with the job
    // and the images, the rest with devices only. ARMv7-A publishes 36
    // device nodes, and a message carries 64 handles.
    let mut images = Some(images);
    let mut index = 0;
    let mut first = true;
    while first || index < nodes.len() {
        let extra = if first { 1 + names.len() } else { 0 };
        let room = CHANNEL_MAX_HANDLES.saturating_sub(extra) / 2;
        let take = room.min(nodes.len() - index);
        let message = Devices {
            devices: u32::try_from(take).map_err(|_| "too many devices")?,
            more: u32::try_from(nodes.len() - index - take).map_err(|_| "too many devices")?,
            first,
            names: if first { &names } else { &[] },
        };
        let mut bytes = [0_u8; DEVICES_MAX_BYTES];
        let len = message
            .encode(&mut bytes)
            .ok_or("more drivers than one DEVICES message names")?;
        let mut transfers: Vec<Transfer> = Vec::with_capacity(message.handles());
        if first {
            transfers.push((Object::Job(Job::new_root()), Rights::JOB));
        }
        for node in nodes.iter().skip(index).take(take) {
            transfers.push((Object::Device(Arc::clone(node)), DEVICE_RIGHTS));
            transfers.push((Object::Device(Arc::clone(node)), KEPT_DEVICE_RIGHTS));
        }
        if let Some(images) = images.take() {
            for vmo in images {
                transfers.push((
                    Object::Vmo(vmo),
                    Rights(Rights::READ.0 | Rights::TRANSFER.0),
                ));
            }
        }
        let handles = transfers.len();
        kernel_end
            .write(bytes.get(..len).unwrap_or(&[]).to_vec(), handles, || {
                Ok::<Vec<Transfer>, Infallible>(transfers)
            })
            .map_err(|_| "DEVICES could not be written to devmgr's channel")?;
        index += take;
        first = false;
    }

    let process = exec::load_native(&image, NAME).map_err(|_| "/sbin/devmgr does not load")?;
    let bootstrap = process
        .with_handles(|table| table.insert(Object::Channel(devmgr_end), Rights::CHANNEL))
        .map_err(|_| "no room for devmgr's bootstrap handle")?;
    let claim =
        process::claim_start(&process).map_err(|_| "devmgr could not be claimed to start")?;
    *CHANNEL.lock() = Some(Arc::clone(&kernel_end));
    // The task runs for the life of the machine; nothing here joins it.
    let _task = claim
        .start(None, u64::from(bootstrap.0))
        .map_err(|_| "devmgr could not be started")?;

    let (started, failed) = report(&kernel_end)?;
    if sched::spawn("devmgr listener", listen, 0, ferrix_sched::NICE_0_WEIGHT).is_err() {
        return Err("no task to hear devmgr on");
    }
    Ok(Some(Report {
        devices: nodes.len(),
        drivers: names.len(),
        started,
        failed,
    }))
}

/// The process that last mapped each device's registers or took its interrupt,
/// as how it ends, by the device's PCI location: what a check that waits for a
/// driver's work looks at to say the driver died, and how, rather than that the
/// work never came. The record outlives the process, and the latest such
/// process replaces it: a boot check's own processes use a device before
/// devmgr's driver does.
static DRIVER_ENDINGS: SpinLock<Vec<(Location, Arc<process::Exit>)>> = SpinLock::new(Vec::new());

/// Note that `driver` mapped `node`'s registers or took its interrupt: it is
/// that device's driver now. Cheap when it is already noted, which every call
/// after its first is.
pub(crate) fn note_driver(node: &device::DeviceNode, driver: &process::Process) {
    let Some(location) = crate::block_ring::location_of(node) else {
        return;
    };
    let exit = driver.exit_record();
    let replaced = {
        let mut drivers = DRIVER_ENDINGS.lock();
        match drivers.iter_mut().find(|(at, _)| *at == location) {
            Some((_, noted)) if Arc::ptr_eq(noted, &exit) => None,
            Some((_, noted)) => Some(core::mem::replace(noted, exit)),
            None => {
                drivers.push((location, exit));
                None
            }
        }
    };
    // A record let go of here may be the last reference to how an old driver
    // ended, and is dropped with the table unlocked.
    drop(replaced);
}

/// How the driver of the device at `location` ended, if one was noted and
/// has.
pub(crate) fn driver_ending(location: Location) -> Option<(i32, Option<u32>)> {
    let exit = DRIVER_ENDINGS
        .lock()
        .iter()
        .find(|(at, _)| *at == location)
        .map(|(_, exit)| Arc::clone(exit))?;
    exit.status().map(|status| (status, exit.signal()))
}

/// Tell `devmgr` that the disk of the device at `location` is published.
/// Nothing, before `devmgr` is started or if it is gone.
pub(crate) fn published(location: Location) {
    let Some(channel) = CHANNEL.lock().clone() else {
        return;
    };
    let bytes = Message::Published {
        location: location.raw(),
    }
    .encode()
    .to_vec();
    let _ = channel.write(bytes, 0, || Ok::<Vec<Transfer>, Infallible>(Vec::new()));
}

/// The manifest's names, each NUL-padded to a driver name; an image without
/// one lists no drivers.
fn manifest(ctx: &ferrix_vfs::Context) -> Vec<[u8; NAME_BYTES]> {
    let Ok(text) = fs::read_file(ctx, None, MANIFEST) else {
        return Vec::new();
    };
    text.split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty() && line.len() <= NAME_BYTES)
        .map(|line| {
            let mut name = [0; NAME_BYTES];
            for (slot, byte) in name.iter_mut().zip(line) {
                *slot = *byte;
            }
            name
        })
        .collect()
}

/// A name without its padding.
fn name_bytes(name: &[u8; NAME_BYTES]) -> &[u8] {
    let end = name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(NAME_BYTES);
    name.get(..end).unwrap_or(&[])
}

/// `bytes` in a VMO of its own: anonymous memory the kernel filled.
fn vmo_of(bytes: &[u8]) -> Result<Arc<Vmo>, &'static str> {
    let pages = (bytes.len() as u64).div_ceil(PAGE_SIZE).max(1);
    let vmo = Vmo::new_anonymous(pages);
    for (index, chunk) in bytes.chunks(PAGE_SIZE as usize).enumerate() {
        vmo.write_page(index as u64, 0, chunk)
            .map_err(|_| "a driver's image did not fit its VMO")?;
    }
    Ok(vmo)
}

/// Wait for REPORT on `channel`.
fn report(channel: &Endpoint) -> Result<(u32, u32), &'static str> {
    let deadline = timer::now_nanos().saturating_add(REPORT_PATIENCE_NANOS);
    loop {
        match channel.read(CHANNEL_MAX_BYTES, CHANNEL_MAX_HANDLES, false) {
            Ok(message) => {
                crate::object::dispose(message.handles.into_iter().map(|(object, _)| object));
                return match Message::decode(&message.bytes) {
                    Ok(Message::Report { started, failed }) => Ok((started, failed)),
                    _ => Err("devmgr said something other than REPORT first"),
                };
            }
            Err(ReadError::Empty) => {}
            Err(_) => return Err("devmgr closed its channel before reporting"),
        }
        let ready = channel.waiters().wait_until_deadline(
            || {
                channel
                    .signals()
                    .intersects(Signals::READABLE | Signals::PEER_CLOSED)
            },
            deadline,
        );
        if !ready {
            return Err("devmgr reported nothing within the patience");
        }
    }
}

/// The kernel's task on `devmgr`'s channel after REPORT: prints each DIED
/// and RESTARTED.
fn listen(_: usize) {
    let Some(channel) = CHANNEL.lock().clone() else {
        return;
    };
    loop {
        match channel.read(SHORT_BYTES, CHANNEL_MAX_HANDLES, false) {
            Ok(message) => {
                crate::object::dispose(message.handles.into_iter().map(|(object, _)| object));
                match Message::decode(&message.bytes) {
                    Ok(Message::Died { location, status }) => crate::console::println!(
                        "  devmgr   the driver of {location:#010x} ended with status {status}; the device is quiesced"
                    ),
                    Ok(Message::Restarted { location, restarts }) => crate::console::println!(
                        "  devmgr   the driver of {location:#010x} was started again and published (restart {restarts})"
                    ),
                    _ => {}
                }
            }
            Err(ReadError::Empty) => {
                let _ = channel.waiters().wait_until_deadline(
                    || {
                        channel
                            .signals()
                            .intersects(Signals::READABLE | Signals::PEER_CLOSED)
                    },
                    u64::MAX,
                );
            }
            Err(_) => {
                crate::console::println!(
                    "  devmgr   devmgr is gone; no driver will be started again"
                );
                return;
            }
        }
    }
}
