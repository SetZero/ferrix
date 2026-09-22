//! The persistent disk: a btrfs volume that outlives the boot.
//!
//! Every other disk a QEMU boot carries is rewritten by `xtask` before it
//! starts, because the boot check writes on it and a check that starts from
//! what another run left is checking something nobody chose. This one is
//! not. `cargo xtask run` and `run-compositor` attach `build/data.img` as the
//! fourth virtio-blk function, `vdd`, made from the blank fixture the first
//! time and kept after that (`--reset-data` starts it over); the test boots
//! do not attach it at all. If it is there, it is mounted writable at
//! `/data`.
//!
//! What reaches the disk: whatever a program `fsync`s or `sync`s, at once;
//! everything else at the next commit, which a task here makes every
//! [`COMMIT_INTERVAL`] — Linux's btrfs commits every 30 seconds by default
//! for the same reason, so that closing the machine's window loses half a
//! minute rather than everything since the boot. A power-off the kernel makes
//! itself commits first ([`sync`]).
//!
//! A mount that fails is said and not fatal: the disk holds somebody's files,
//! and a boot that refused to start over them would help nobody reach them.

use alloc::sync::Arc;

use ferrix_sync::Once;
use ferrix_vfs::initramfs::makedev;
use ferrix_vfs::{Errno, FileSystem};

use crate::block_ring::VIRTIO_BLK_MAJOR;
use crate::console::println;
use crate::{fs, sched};

/// The persistent disk: the fourth virtio-blk function, `vdd`.
const DISK_INDEX: u32 = 3;

/// Where it is mounted.
const MOUNT_POINT: &[u8] = b"/data";

/// How often everything written is committed without being asked.
const COMMIT_INTERVAL: u64 = 30_000_000_000;

/// The mounted volume, for the committer and for [`sync`].
static VOLUME: Once<Arc<dyn FileSystem>> = Once::new();

/// Mount the persistent disk at `/data` if this machine has one, and start
/// committing it every [`COMMIT_INTERVAL`]. Says what it did either way.
pub(crate) fn mount() {
    let rdev = makedev(VIRTIO_BLK_MAJOR, DISK_INDEX * 16);
    if fs::devfs::block_device(rdev).is_none() {
        return;
    }
    match mount_at(rdev) {
        Ok(volume) => {
            let _ = VOLUME.call_once(|| volume);
            if sched::spawn(
                "data commit",
                commit_forever,
                0,
                ferrix_sched::NICE_0_WEIGHT,
            )
            .is_err()
            {
                println!("  data     /data has no commit task: only sync and fsync reach the disk");
            }
            println!(
                "  data     vdd mounted writable at /data, committed every {} s",
                COMMIT_INTERVAL / 1_000_000_000
            );
        }
        Err(why) => println!("  data     vdd is not mounted: {why}"),
    }
}

/// Make `/data` and mount the volume on it.
fn mount_at(rdev: u64) -> Result<Arc<dyn FileSystem>, &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    match ns.mkdir(&ctx, None, MOUNT_POINT, 0o755) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(_) => return Err("/data could not be made"),
    }
    let at = ns
        .resolve(&ctx, None, MOUNT_POINT, true)
        .map_err(|_| "/data could not be resolved")?;
    let volume = fs::btrfs::mount_rw(rdev).map_err(|errno| match errno {
        Errno::EROFS => "the volume is one the write path will not change (read-only)",
        Errno::ENXIO => "the disk went away",
        _ => "it is not a btrfs volume this kernel can mount writable",
    })?;
    let _ = ns
        .mount(volume.clone(), &at)
        .map_err(|_| "the volume could not be mounted at /data")?;
    Ok(volume)
}

/// The committer: sleep, commit, forever.
fn commit_forever(_: usize) {
    loop {
        sched::sleep_for(COMMIT_INTERVAL);
        if sync().is_err() {
            println!("  data     the periodic commit of /data failed");
        }
    }
}

/// Commit `/data` now, if it is mounted.
///
/// # Errors
///
/// What the commit said.
pub(crate) fn sync() -> Result<(), Errno> {
    match VOLUME.get() {
        Some(volume) => volume.sync(),
        None => Ok(()),
    }
}
