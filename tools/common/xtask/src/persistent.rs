//! `--persistent`: a `run` or `run-compositor` whose machine keeps what is
//! done in it -- a Steam sign-in, a game it installed, a Chrome extension,
//! a cookie, a file under `/data/home` -- from one boot to the next.
//!
//! `/` keeps its files without this: the btrfs root under `build/` is
//! already the disk the last boot left (`btrfs_disk::ensure_root`). What a
//! boot throws away is `/data`, the volume Chrome, Steam and the compilers
//! are on, which QEMU attaches under `snapshot=on` so the volume the fetch
//! scripts made is never changed. Steam keeps everything there -- its client
//! in `/data/steam`, its home in `/data/home` -- and Chrome's profile is in
//! `/dev/shm`, which is memory.
//!
//! So this attaches a copy of the volume of the machine's own, under
//! `~/.local/share/ferrix/persistent/`, made from the volume the first time
//! and written through from then on, and gives Chrome a profile on it
//! ([`CHROME_PROFILE`]). The volume itself is left as it was: tests and
//! other runs keep reading what the fetch scripts made.
//!
//! The copy does not follow the volume. When the volume is made again -- a
//! new Chrome, a new yserver pin -- the run says so, and `--reset-root`
//! starts both the root and the copy over, with everything on them.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{Error, Result};

/// Chrome's profile under `--persistent`: on the kept volume, beside
/// Steam's home, where `/dev/shm`'s would be gone at the next boot.
pub(crate) const CHROME_PROFILE: &str = "/data/home/chrome";

/// The kept copy of `volume`, made from it when there is none or `reset`
/// asks, and otherwise as the last boot left it.
///
/// # Errors
///
/// When the copy cannot be made.
pub(crate) fn data_volume(volume: &Path, reset: bool) -> Result<PathBuf> {
    let name = volume
        .file_name()
        .ok_or_else(|| Error::new(format!("{} names no file", volume.display())))?;
    let directory = crate::paths::volume_directory("persistent")?;
    let copy = directory.join(name);
    let from = from_file(&copy);
    let made = modified(volume)?;
    if !reset && copy.is_file() {
        if std::fs::read_to_string(&from).ok().as_deref() != Some(&stamp(volume, made)) {
            println!(
                "  note: {} has been made again since {} was copied from it; \
                 --reset-root starts the copy over from it, losing what is on it",
                volume.display(),
                copy.display()
            );
        }
        return Ok(copy);
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| Error::new(format!("creating {}: {error}", directory.display())))?;
    println!(
        "  copying {} to {}, the /data this machine keeps",
        volume.display(),
        copy.display()
    );
    let partial = directory.join(format!("{}.{}.partial", name.display(), std::process::id()));
    copy_sparse(volume, &partial).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    std::fs::rename(&partial, &copy)
        .map_err(|error| Error::new(format!("renaming to {}: {error}", copy.display())))?;
    std::fs::write(&from, stamp(volume, made))
        .map_err(|error| Error::new(format!("writing {}: {error}", from.display())))?;
    Ok(copy)
}

/// Where the copy says which volume it was made from, and when that was
/// made: `<copy>.from`.
fn from_file(copy: &Path) -> PathBuf {
    let mut name = copy.as_os_str().to_owned();
    name.push(".from");
    PathBuf::from(name)
}

/// What [`from_file`] holds: the volume's path and its modification time.
fn stamp(volume: &Path, made: SystemTime) -> String {
    let seconds = made
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    format!("{} {seconds}\n", volume.display())
}

/// When `path` was last written.
fn modified(path: &Path) -> Result<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| Error::new(format!("{}: {error}", path.display())))
}

/// Copy `from` to `to` keeping its holes: the volumes are sparse, ten GiB
/// long with a third of it written, and a copy that filled them would take
/// the rest of the disk for nothing. `cp` does that on a Linux host; a
/// Windows host's copy is a whole one.
fn copy_sparse(from: &Path, to: &Path) -> Result<()> {
    if cfg!(windows) {
        return std::fs::copy(from, to).map(drop).map_err(|error| {
            Error::new(format!(
                "copying {} to {}: {error}",
                from.display(),
                to.display()
            ))
        });
    }
    let status = std::process::Command::new("cp")
        .args(["--sparse=always", "--reflink=auto"])
        .arg(from)
        .arg(to)
        .status()
        .map_err(|error| Error::new(format!("running cp: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "copying {} to {}: cp {status}",
            from.display(),
            to.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stamp names the volume and its time, so a volume made again --
    /// a new time -- reads as another.
    #[test]
    fn a_volume_made_again_stamps_differently() {
        let volume = Path::new("/v/everything.img");
        let then = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(200);
        assert_eq!(stamp(volume, then), "/v/everything.img 100\n");
        assert_ne!(stamp(volume, then), stamp(volume, now));
    }

    /// The copy's record sits beside it.
    #[test]
    fn the_record_is_beside_the_copy() {
        assert_eq!(
            from_file(Path::new("/p/everything.img")),
            PathBuf::from("/p/everything.img.from")
        );
    }
}
