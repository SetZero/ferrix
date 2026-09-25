//! `run-compositor --everything`: one boot with all of it -- the GPU, the
//! clipboard, the network, Chrome on the desktop and `rustc` and `cargo` in
//! the shell.
//!
//! The kernel mounts one data disk, at `/data`, and the two downloads that
//! want it are two volumes: the one `scripts/fetch-rustc-sysroot.sh` makes
//! and the one `scripts/fetch-chrome.sh` makes. So `--chrome` had to take
//! the rustc volume's place, and a desktop with Chrome had no compiler.
//!
//! Both scripts keep the tree they packed beside their image, and both
//! trees are Debian 13's: where they hold the same path it is the same
//! package's file, byte for byte (281 of them on 2026-09-26). So this makes
//! a third volume out of the two trees, linked rather than copied, and
//! makes it again whenever either image is newer than it. Two files at one
//! path that differ stop it, naming the path, rather than one quietly
//! winning.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::{Error, Result};

/// Room for what the boot writes on the volume -- Chrome's profile and
/// caches, and what a compile leaves -- the two scripts' spares together,
/// on top of what the files take.
const SPARE_MIB: u64 = 1024 + 512;

/// The volume, made or made again from the two trees when it is missing or
/// older than either of the images they were packed into.
///
/// # Errors
///
/// When either volume has not been fetched, the trees disagree about a
/// file, or `mkfs.btrfs` cannot make the image.
pub(crate) fn volume() -> Result<PathBuf> {
    let rustc_tree = crate::rustc::tree()?;
    let rustc_image = crate::rustc::volume()?;
    let chrome_image = crate::chrome::volume()?;
    let chrome_tree = chrome_image
        .parent()
        .map(|directory| directory.join("tree"))
        .filter(|tree| tree.is_dir())
        .ok_or_else(|| {
            Error::new(format!(
                "no tree beside {}: scripts/fetch-chrome.sh keeps one there",
                chrome_image.display()
            ))
        })?;
    let directory = directory()?;
    let image = directory.join("everything.img");
    let newest = [&rustc_image, &chrome_image]
        .into_iter()
        .map(|path| modified(path))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max();
    if image.is_file() && Some(modified(&image)?) >= newest {
        println!("  everything: {}", image.display());
        return Ok(image);
    }

    println!(
        "  everything: making {} from {} and {}",
        image.display(),
        rustc_tree.display(),
        chrome_tree.display()
    );
    let tree = directory.join("tree");
    if tree.exists() {
        std::fs::remove_dir_all(&tree)
            .map_err(|error| Error::new(format!("removing {}: {error}", tree.display())))?;
    }
    create_dir(&tree)?;
    let mut bytes = 0u64;
    for from in [&rustc_tree, &chrome_tree] {
        bytes += merge(from, &tree)?;
    }

    let size = bytes.div_ceil(1 << 20) + SPARE_MIB;
    let _ = std::fs::remove_file(&image);
    let file = std::fs::File::create(&image)
        .map_err(|error| Error::new(format!("creating {}: {error}", image.display())))?;
    file.set_len(size << 20)
        .map_err(|error| Error::new(format!("sizing {}: {error}", image.display())))?;
    drop(file);
    let status = Command::new("mkfs.btrfs")
        .arg("-q")
        .arg("--rootdir")
        .arg(&tree)
        .arg(&image)
        .status()
        .map_err(|error| Error::new(format!("running mkfs.btrfs: {error}")))?;
    if !status.success() {
        // A half-made image would be taken as made next time.
        let _ = std::fs::remove_file(&image);
        return Err(Error::new(format!(
            "mkfs.btrfs {}: {status}",
            image.display()
        )));
    }
    println!("  everything: {} ({size} MiB)", image.display());
    Ok(image)
}

/// `~/.local/share/ferrix/everything`, or `FERRIX_EVERYTHING_VOLUME`.
///
/// Beside the two volumes by default, which is what lets the tree be hard
/// links into theirs: one file system, and no second copy of 1.6 GiB.
fn directory() -> Result<PathBuf> {
    if let Some(directory) = std::env::var_os("FERRIX_EVERYTHING_VOLUME") {
        return Ok(PathBuf::from(directory));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| Error::new("neither HOME nor USERPROFILE is set"))?;
    Ok(PathBuf::from(home).join(".local/share/ferrix/everything"))
}

/// Put everything under `from` into `into`, and say how many bytes of files
/// that added: a file hard-linked (copied where it cannot be), a symbolic
/// link made again with its target. A path already there from the other
/// tree must be the same file or the same link.
fn merge(from: &Path, into: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    let entries = std::fs::read_dir(from)
        .map_err(|error| Error::new(format!("reading {}: {error}", from.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| Error::new(format!("reading {}: {error}", from.display())))?;
        let source = entry.path();
        let target = into.join(entry.file_name());
        let kind = std::fs::symlink_metadata(&source)
            .map_err(|error| Error::new(format!("{}: {error}", source.display())))?;
        if kind.is_symlink() {
            let link = std::fs::read_link(&source)
                .map_err(|error| Error::new(format!("{}: {error}", source.display())))?;
            if std::fs::symlink_metadata(&target).is_ok() {
                if std::fs::read_link(&target).ok().as_ref() != Some(&link) {
                    return Err(clash(&target));
                }
                continue;
            }
            std::os::unix::fs::symlink(&link, &target)
                .map_err(|error| Error::new(format!("{}: {error}", target.display())))?;
        } else if kind.is_dir() {
            match std::fs::symlink_metadata(&target) {
                Ok(there) if !there.is_dir() => return Err(clash(&target)),
                Ok(_) => {}
                Err(_) => create_dir(&target)?,
            }
            bytes += merge(&source, &target)?;
        } else {
            if let Ok(there) = std::fs::symlink_metadata(&target) {
                if !there.is_file() || !same_contents(&source, &target)? {
                    return Err(clash(&target));
                }
                continue;
            }
            if std::fs::hard_link(&source, &target).is_err() {
                let _ = std::fs::copy(&source, &target)
                    .map_err(|error| Error::new(format!("{}: {error}", target.display())))?;
            }
            // Rounded up to a block, as the file system will store it.
            bytes += kind.len().div_ceil(4096) * 4096;
        }
    }
    Ok(bytes)
}

/// Whether two files hold the same bytes.
fn same_contents(left: &Path, right: &Path) -> Result<bool> {
    let read = |path: &Path| {
        std::fs::read(path).map_err(|error| Error::new(format!("{}: {error}", path.display())))
    };
    Ok(read(left)? == read(right)?)
}

/// The error for a path the two trees disagree about.
fn clash(path: &Path) -> Error {
    Error::new(format!(
        "{}: the rustc and Chrome volumes hold different files here; fetch both again so \
         they are from the same Debian",
        path.display()
    ))
}

/// Make one directory.
fn create_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|error| Error::new(format!("creating {}: {error}", path.display())))
}

/// When a file was last written.
fn modified(path: &Path) -> Result<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| Error::new(format!("{}: {error}", path.display())))
}
