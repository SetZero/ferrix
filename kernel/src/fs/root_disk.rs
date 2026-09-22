//! The btrfs root: `/` on a disk that outlives the boot.
//!
//! The kernel cannot start on it. The disk driver is a ring-3 program on the
//! root filesystem, so the root it starts on is the tmpfs the initramfs is
//! unpacked into (`fs`'s module comment, and `docs/ARCHITECTURE.md` §7).
//! Linux's answer is `switch_root`, and this is Ferrix's: once the driver is
//! serving the disk, [`switch`] mounts the btrfs volume on it at
//! [`SYSROOT`], installs the system on it from the same archive, mounts
//! `/dev`, `/proc` and `/tmp` inside it, and from then on every process the
//! kernel makes has that volume as its `/` ([`process_context`]). The
//! kernel's own checks keep the tmpfs, where their fixtures are mounted.
//!
//! # Which disk, and when
//!
//! The root disk is the btrfs disk labelled [`LABEL`], from the fourth
//! virtio-blk function, `vdd`, on: by label rather than by position, as
//! Linux's `root=LABEL=`, so another disk beside it (`/data`,
//! [`super::data_disk`]) is never mistaken for it. `cargo xtask run` and
//! `run-compositor` attach `build/root.img`, a 1 GiB volume made from the
//! `root` fixture the first time and kept after that; the test boots do not
//! attach it, and without it — or with `ferrix.root=tmpfs` on the command
//! line — the root stays the tmpfs, as it always was.
//!
//! # The system on it
//!
//! The first boot unpacks the whole initramfs into the volume. A later boot
//! unpacks it again only if the archive is not the one the volume last got,
//! by its length and CRC-32C in [`STAMP`]: a rebuilt image's programs arrive,
//! and everything else a user put on the volume stays. A file the new archive
//! also carries is replaced by the archive's; one it no longer carries is
//! left where it was.
//!
//! # What reaches the disk
//!
//! Whatever a program `fsync`s or `sync`s, at once; everything else at the
//! next commit, which [`commit_forever`] makes every [`COMMIT_INTERVAL`] —
//! Linux's btrfs commits every 30 seconds by default for the same reason —
//! and once more when the kernel powers the machine off itself ([`sync`]).
//!
//! A root that will not mount or install is said, and the boot carries on
//! with the tmpfs: the disk holds somebody's files, and a machine that
//! refused to start over them would help nobody reach them.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use ferrix_bootinfo::BootView;
use ferrix_btrfs::crc32c::crc32c;
use ferrix_sync::Once;
use ferrix_vfs::initramfs::{self, makedev};
use ferrix_vfs::{Access, Context, Errno, FileSystem, Location, OpenFlags, SetAttributes};

use crate::block_ring::VIRTIO_BLK_MAJOR;
use crate::console::println;
use crate::fs::{devfs, procfs};
use crate::{fs, sched};

/// The label the root volume carries: `mkfs.btrfs -L ferrix-root`.
const LABEL: &[u8] = b"ferrix-root";

/// The first disk looked at, `vdd`, and one past the last, `vdh`.
const FIRST: u32 = 3;
const END: u32 = 8;

/// Where the volume is mounted in the kernel's own tree.
const SYSROOT: &[u8] = b"/sysroot";

/// The command-line option that keeps the tmpfs root.
const OPTION: &str = "ferrix.root";

/// Which archive the volume last got, relative to its root: the length and
/// the CRC-32C, in hex.
const STAMP: &[u8] = b"/.ferrix-initramfs";

/// How often everything written is committed without being asked.
const COMMIT_INTERVAL: u64 = 30_000_000_000;

/// Whether `ferrix.root=tmpfs` asked for the tmpfs root.
static TMPFS: AtomicBool = AtomicBool::new(false);

/// The volume, and where it is: set once [`switch`] has finished.
static ROOT: Once<(Arc<dyn FileSystem>, Location)> = Once::new();

/// The disk [`switch`] put `/` on.
static ROOT_RDEV: Once<u64> = Once::new();

/// The disk `/` is on, once [`switch`] has put it there.
pub(crate) fn root_rdev() -> Option<u64> {
    ROOT_RDEV.get().copied()
}

/// Whether the disk numbered `rdev` holds the root volume, by its label.
pub(crate) fn is_root(rdev: u64) -> bool {
    fs::btrfs::label(rdev).as_deref() == Some(LABEL)
}

/// Read `ferrix.root`, once, early.
pub(crate) fn init(view: &BootView<'_>) {
    match view.option(OPTION) {
        None | Some("btrfs") => {}
        Some("tmpfs") => TMPFS.store(true, Ordering::Relaxed),
        Some(other) => println!("  root     {OPTION}={other} is not understood; btrfs is tried"),
    }
}

/// The context a new process starts in: the btrfs root once [`switch`] has
/// put it in place, and the kernel's tmpfs before that or without it.
pub(crate) fn process_context() -> Context {
    match ROOT.get() {
        Some((_, root)) => Context {
            root: root.clone(),
            cwd: root.clone(),
            who: Access::root(),
        },
        None => fs::namespace().context(),
    }
}

/// Put `/` on the root disk, if this machine has one and nothing asked for
/// the tmpfs. Says what it did either way.
pub(crate) fn switch() {
    let Some(rdev) = (FIRST..END)
        .map(|index| makedev(VIRTIO_BLK_MAJOR, index * 16))
        .filter(|&rdev| devfs::block_device(rdev).is_some())
        .find(|&rdev| is_root(rdev))
    else {
        return;
    };
    if TMPFS.load(Ordering::Relaxed) {
        println!("  root     vdd is not used: {OPTION}=tmpfs keeps / in memory");
        return;
    }
    match switch_to(rdev) {
        Ok(Installed::Fresh(bytes)) => println!(
            "  root     / is btrfs on vdd; the system was installed on it ({} KiB), \
             committed every {} s",
            bytes / 1024,
            COMMIT_INTERVAL / 1_000_000_000
        ),
        Ok(Installed::Kept) => println!(
            "  root     / is btrfs on vdd, as the last boot left it; committed every {} s",
            COMMIT_INTERVAL / 1_000_000_000
        ),
        Ok(Installed::Nothing) => println!(
            "  root     / is btrfs on vdd, with no initramfs to install from; committed \
             every {} s",
            COMMIT_INTERVAL / 1_000_000_000
        ),
        Err(why) => println!("  root     / stays in memory: vdd {why}"),
    }
}

/// What the volume got from the archive.
enum Installed {
    /// The archive, unpacked: this many bytes of files.
    Fresh(u64),
    /// Nothing: it already had this archive.
    Kept,
    /// Nothing: there is no archive.
    Nothing,
}

fn switch_to(rdev: u64) -> Result<Installed, &'static str> {
    let ns = fs::namespace();
    let ctx = ns.context();
    let volume = fs::btrfs::mount_rw(rdev).map_err(|errno| match errno {
        Errno::EROFS => "is a volume the write path will not change",
        Errno::ENXIO => "went away",
        _ => "is not a btrfs volume this kernel can mount writable",
    })?;
    match ns.mkdir(&ctx, None, SYSROOT, 0o755) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(_) => return Err("has nowhere to be mounted"),
    }
    let at = ns
        .resolve(&ctx, None, SYSROOT, true)
        .map_err(|_| "has nowhere to be mounted")?;
    let _ = ns
        .mount(volume.clone(), &at)
        .map_err(|_| "could not be mounted")?;
    let root = ns
        .resolve(&ctx, None, SYSROOT, true)
        .map_err(|_| "could not be found after its mount")?;
    let inside = Context {
        root: root.clone(),
        cwd: root.clone(),
        who: Access::root(),
    };

    let installed = install(&inside)?;
    mount_kernel_filesystems(&inside)?;
    volume.sync().map_err(|_| "could not commit the system")?;
    let _ = ROOT.call_once(|| (volume, root));
    let _ = ROOT_RDEV.call_once(|| rdev);
    if sched::spawn(
        "root commit",
        commit_forever,
        0,
        ferrix_sched::NICE_0_WEIGHT,
    )
    .is_err()
    {
        println!("  root     no commit task: only sync and fsync reach the disk");
    }
    Ok(installed)
}

/// Unpack the initramfs into the volume, unless it already has this one.
fn install(inside: &Context) -> Result<Installed, &'static str> {
    let Some(archive) = fs::initramfs_archive() else {
        return Ok(Installed::Nothing);
    };
    let stamp = alloc::format!("{:x} {:08x}\n", archive.len(), crc32c(archive));
    let ns = fs::namespace();
    if read_stamp(inside).as_deref() == Some(stamp.as_bytes()) {
        return Ok(Installed::Kept);
    }
    let unpacked = initramfs::unpack(ns, inside, archive)
        .map_err(|_| "could not take the system: the initramfs did not unpack onto it")?;
    // Written last, so a boot that dies part-way through installs again.
    write_stamp(inside, stamp.as_bytes()).map_err(|_| "could not record the system")?;
    Ok(Installed::Fresh(unpacked.bytes))
}

fn read_stamp(inside: &Context) -> Option<Vec<u8>> {
    let ns = fs::namespace();
    let at = ns.resolve(inside, None, STAMP, true).ok()?;
    let inode = at.inode().ok()?;
    let mut out = alloc::vec![0u8; 64];
    let read = inode.read_at(0, &mut out).ok()?;
    out.truncate(read);
    Some(out)
}

fn write_stamp(inside: &Context, stamp: &[u8]) -> Result<(), Errno> {
    let ns = fs::namespace();
    let flags = OpenFlags {
        write: true,
        create: true,
        truncate: true,
        ..OpenFlags::default()
    };
    let file = ns.open(inside, None, STAMP, &flags, 0o644)?;
    if file.write(stamp)? != stamp.len() {
        return Err(Errno::EIO);
    }
    Ok(())
}

/// `/dev`, `/proc` and `/tmp` inside the volume, as `fs::init` makes them in
/// the tmpfs, so a process whose `/` is the volume finds them where it looks.
fn mount_kernel_filesystems(inside: &Context) -> Result<(), &'static str> {
    let ns = fs::namespace();
    let mounts: [(&[u8], Arc<dyn FileSystem>, u32); 3] = [
        (b"/dev", Arc::new(devfs::Devfs::new()), 0o755),
        (b"/proc", Arc::new(procfs::Procfs::new()), 0o555),
        (b"/tmp", fs::new_tmpfs(), 0o1777),
    ];
    for (path, filesystem, mode) in mounts {
        match ns.mkdir(inside, None, path, mode) {
            Ok(()) | Err(Errno::EEXIST) => {}
            Err(_) => return Err("has no room for /dev, /proc or /tmp"),
        }
        let at = ns
            .resolve(inside, None, path, true)
            .map_err(|_| "has no room for /dev, /proc or /tmp")?;
        let _ = ns
            .mount(filesystem, &at)
            .map_err(|_| "could not carry /dev, /proc or /tmp")?;
    }
    // Sticky and writable by everyone, as every Unix `/tmp` is.
    let tmp = ns
        .resolve(inside, None, b"/tmp", true)
        .map_err(|_| "lost its /tmp")?;
    let sticky = SetAttributes {
        permissions: Some(0o1777),
        ..SetAttributes::default()
    };
    ns.set_attributes(&tmp, &sticky)
        .map_err(|_| "could not make /tmp writable")?;
    Ok(())
}

/// The committer: sleep, commit, forever.
fn commit_forever(_: usize) {
    loop {
        sched::sleep_for(COMMIT_INTERVAL);
        if sync().is_err() {
            println!("  root     the periodic commit of / failed");
        }
        if fs::data_disk::sync().is_err() {
            println!("  data     the periodic commit of /data failed");
        }
    }
}

/// Commit the root volume now, if `/` is on one.
///
/// # Errors
///
/// What the commit said.
pub(crate) fn sync() -> Result<(), Errno> {
    match ROOT.get() {
        Some((volume, _)) => volume.sync(),
        None => Ok(()),
    }
}
