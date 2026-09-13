//! Putting the loader and the kernel onto a board's boot partition.
//!
//! # Why this copies files instead of writing an image
//!
//! `cargo xtask build` produces a FAT filesystem with no partition table,
//! which is exactly right for QEMU — firmware there is handed the whole thing
//! as a disk — and exactly wrong for this board. An STM32MP157 boots from ST's
//! own chain: the ROM loads TF-A from a partition it finds by name, TF-A loads
//! OP-TEE and U-Boot, and only then is there anything that can read a
//! filesystem. Writing our image over the card would remove all of it, and the
//! board would stop booting entirely rather than boot the wrong kernel.
//!
//! So the card keeps its vendor layout, and this copies the loader, the kernel
//! and the initramfs onto the FAT partition U-Boot already looks at — the same
//! paths the image contains, because firmware looks for them in the same places.

use std::path::{Path, PathBuf};

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result};

/// Where the loader goes, less the architecture's file name.
const BOOT_DIRECTORY: &str = "EFI/BOOT";

/// Where the kernel goes, as the loader looks for it.
const KERNEL_PATH: &str = "FERRIX/KERNEL.ELF";

/// Where the initramfs goes, beside the kernel.
const INITRD_PATH: &str = "FERRIX/INITRD.IMG";

/// Copy a freshly built loader, kernel and initramfs onto the card.
pub(crate) fn run(
    arch: Arch,
    loader: &Path,
    kernel: &Path,
    initramfs: &[u8],
    args: &Args,
) -> Result<()> {
    let target = match args.to.as_deref() {
        Some(given) => verify(Path::new(given))?,
        None => discover()?,
    };

    println!("  flashing to {}", target.display());

    let boot_name = arch.removable_boot_name();
    let loader_target = target.join(BOOT_DIRECTORY).join(boot_name);
    let kernel_target = target.join(KERNEL_PATH);

    for path in [&loader_target, &kernel_target] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| Error::new(format!("creating {}: {error}", parent.display())))?;
        }
    }

    copy(loader, &loader_target)?;
    copy(kernel, &kernel_target)?;
    // The same archive an image carries, so a board unpacks what QEMU does.
    let initramfs_target = target.join(INITRD_PATH);
    std::fs::write(&initramfs_target, initramfs)
        .map_err(|error| Error::new(format!("writing {}: {error}", initramfs_target.display())))?;
    println!("    {}", initramfs_target.display());

    // A card pulled from the slot with dirty pages still in the page cache is
    // a card with a truncated kernel on it, and the symptom is a loader that
    // rejects the image for reasons that have nothing to do with the build.
    sync();

    println!("  flashed; the card is safe to remove");
    Ok(())
}

/// Copy one file, reporting what it was.
fn copy(from: &Path, to: &Path) -> Result<()> {
    let bytes = std::fs::copy(from, to)
        .map_err(|error| Error::new(format!("writing {}: {error}", to.display())))?;
    println!("    {} ({} KiB)", to.display(), bytes / 1024);
    Ok(())
}

/// Flush the page cache, so the card can be pulled.
fn sync() {
    if let Some(sync) = paths::which("sync") {
        let _ = std::process::Command::new(sync).status();
    }
}

/// Check that `path` is somewhere it is safe to write a boot loader.
///
/// # Why this is not simply a copy
///
/// The argument is a path the caller typed, and the failure mode of getting it
/// wrong is not a failed build: it is files appearing in the wrong place on a
/// running system, possibly as root. So the destination has to be a mount
/// point — not merely a directory — and the filesystem mounted there has to be
/// a FAT, which is the only thing UEFI firmware reads and therefore the only
/// thing this could sensibly be. A typo that lands on an ordinary directory,
/// on the system root, or on the developer's home fails all three.
fn verify(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        return Err(Error::new(format!(
            "{} is not a directory.\n  \
             Point --to at the card's mounted boot partition, not at a device node: \
             flashing writes files onto a filesystem, it does not image a disk.",
            path.display()
        )));
    }

    let canonical = path
        .canonicalize()
        .map_err(|error| Error::new(format!("resolving {}: {error}", path.display())))?;

    let mounts = fat_mount_points();
    if mounts.is_empty() {
        // No /proc/mounts to read: not Linux, or something unusual. Say so
        // rather than silently dropping the check that makes this safe.
        return Err(Error::new(
            "cannot read /proc/mounts, so the destination cannot be checked.\n  \
             Copy the two files by hand; `cargo xtask build --arch armv7a` says where \
             they are.",
        ));
    }

    // Refused by name, not merely left out of discovery. `/boot/efi` is a
    // mounted FAT and passes every other check here, so without this the one
    // destination that can stop this computer booting is the one destination
    // `--to` accepts without complaint.
    if starts_with_any(&canonical, &["/boot", "/efi"]) {
        return Err(Error::new(format!(
            "{} is this machine's own EFI system partition, not a board's.\n  \
             Writing a boot loader there could stop this computer booting. If \
             you really mean it, copy the files by hand.",
            canonical.display()
        )));
    }

    if !mounts.iter().any(|mount| mount == &canonical) {
        return Err(Error::new(format!(
            "{} is not a mounted FAT filesystem.\n  \
             It must be the card's boot partition itself, mounted. Currently mounted \
             FAT filesystems:\n    {}",
            canonical.display(),
            describe(&mounts)
        )));
    }

    Ok(canonical)
}

/// The one mounted FAT filesystem, if there is exactly one.
///
/// Refuses to choose, for the same reason the serial port does: on a machine
/// with an EFI system partition mounted — which is most of them — there is
/// more than one FAT filesystem, and one of them is the one this computer
/// boots from. Writing a loader into that is a bad afternoon.
fn discover() -> Result<PathBuf> {
    let mounts: Vec<PathBuf> = fat_mount_points()
        .into_iter()
        // /boot/efi is this machine's own, and never the answer.
        .filter(|mount| !starts_with_any(mount, &["/boot", "/efi"]))
        .collect();

    match mounts.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(Error::new(
            "no removable FAT filesystem is mounted.\n  \
             Insert the card and let the desktop mount it, or mount it by hand, then \
             pass --to <path>.",
        )),
        several => Err(Error::new(format!(
            "several FAT filesystems are mounted; say which with --to:\n    {}",
            describe(several)
        ))),
    }
}

/// Every mounted FAT filesystem, from `/proc/mounts`.
fn fat_mount_points() -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let mount = fields.next()?;
            let kind = fields.next()?;
            // `vfat` is what Linux calls every FAT it mounts; `msdos` is the
            // older driver, still selectable.
            (kind == "vfat" || kind == "msdos").then(|| PathBuf::from(unescape(mount)))
        })
        .collect()
}

/// Undo the octal escaping `/proc/mounts` applies to spaces and tabs.
///
/// Removable media are mounted under a label the user chose, and labels with
/// spaces in them are ordinary. Without this, such a card never matches and
/// the error says it is not mounted when it plainly is.
fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits: String = chars.clone().take(3).collect();
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) if digits.len() == 3 => {
                out.push(byte as char);
                let _ = chars.nth(2);
            }
            _ => out.push('\\'),
        }
    }
    out
}

/// Whether `path` is at or under any of `prefixes`.
fn starts_with_any(path: &Path, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| path.starts_with(prefix))
}

/// Mount points, one per line, for an error message.
fn describe(mounts: &[PathBuf]) -> String {
    mounts
        .iter()
        .map(|mount| mount.display().to_string())
        .collect::<Vec<_>>()
        .join("\n    ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_the_octal_that_proc_mounts_uses() {
        assert_eq!(
            unescape("/media/sebastian/NO\\040NAME"),
            "/media/sebastian/NO NAME"
        );
        assert_eq!(unescape("/media/plain"), "/media/plain");
        // A trailing backslash is not an escape and must not be eaten.
        assert_eq!(unescape("/media/odd\\"), "/media/odd\\");
    }

    #[test]
    fn the_machines_own_efi_partition_is_refused_by_name_too() {
        // Discovery skipping it is not enough: `--to /boot/efi` names it
        // explicitly, and that is the request that has to be refused.
        let refused = verify(Path::new("/boot/efi"));
        if Path::new("/boot/efi").is_dir() {
            let message = refused.expect_err("must refuse this machine's ESP").message;
            assert!(
                message.contains("own EFI system partition"),
                "refused for the wrong reason: {message}"
            );
        }
    }

    #[test]
    fn the_machines_own_efi_partition_is_never_a_candidate() {
        assert!(starts_with_any(Path::new("/boot/efi"), &["/boot", "/efi"]));
        assert!(!starts_with_any(
            Path::new("/media/sebastian/bootfs"),
            &["/boot", "/efi"]
        ));
    }
}
