//! `ferrix-install`: put the live system on a disk of its own.
//!
//! The minimum `docs/INSTALLER.md` §11 calls the MVP. The target disk is
//! wiped and given a GUID partition table with two partitions:
//!
//! 1. an EFI system partition holding a byte copy of the live disk's FAT
//!    volume -- the loader, the kernel and the initramfs the machine booted;
//! 2. a root partition holding the empty btrfs volume labelled `ferrix-root`
//!    that `run` boots on (`src/lib/fs/btrfs/testdata/root.img.packed`, 1 GiB).
//!
//! The kernel does the rest at the installed disk's first boot, as it does
//! for every `run`: it finds `ferrix-root`, now on a partition, and unpacks
//! the initramfs onto it.
//!
//! ```text
//! ferrix-install [--yes] [--from /dev/vdX] /dev/vdY
//! ```
//!
//! Without `--from` the live disk is the one whose first sector is the FAT32
//! boot sector `xtask` writes (OEM name `FERRIX`). Without `--yes` it asks
//! before it erases anything.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::process::ExitCode;

use ferrix_partition::{ESP, Guid, LINUX_FILESYSTEM, Partition, usable, write_table};

/// The empty root volume, as `xtask` packs it: records of a little-endian
/// `u64` offset and the 4 KiB block there; zeros elsewhere.
const ROOT_PACKED: &str = "/usr/share/ferrix/root.img.packed";
/// The root volume's size.
const ROOT_BYTES: u64 = 1 << 30;
/// A packed block.
const BLOCK: usize = 4096;
/// Bytes in a sector: virtio's unit.
const SECTOR: u64 = 512;
/// Partitions start on MiB boundaries, as every tool puts them.
const ALIGN: u64 = 2048;
/// What is wiped at each end of the disk, so no old table or superblock
/// survives outside the new partitions.
const WIPE: u64 = 1 << 20;
/// Bytes copied at a time.
const CHUNK: usize = 1 << 20;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("ferrix-install: {why}");
            ExitCode::FAILURE
        }
    }
}

/// What the command line asked.
struct Asked {
    yes: bool,
    from: Option<String>,
    target: String,
}

fn parse() -> Result<Asked, String> {
    let mut yes = false;
    let mut from = None;
    let mut target = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            "--from" => from = Some(args.next().ok_or("--from needs a disk")?),
            "--help" | "-h" => {
                return Err("usage: ferrix-install [--yes] [--from /dev/vdX] /dev/vdY".into());
            }
            _ if target.is_none() => target = Some(arg),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    let target = target.ok_or("name the disk to install on, as /dev/vdY")?;
    Ok(Asked { yes, from, target })
}

fn run() -> Result<(), String> {
    let asked = parse()?;
    let live = match asked.from {
        Some(from) => from,
        None => find_live()?,
    };
    if live == asked.target {
        return Err(format!("{live} is the disk this system booted from"));
    }
    if mounted(&asked.target)? {
        return Err(format!("{} has something mounted from it", asked.target));
    }
    let mut source = File::open(&live).map_err(|e| format!("{live}: {e}"))?;
    let esp_bytes = fat_volume_bytes(&mut source).map_err(|e| format!("{live}: {e}"))?;
    let mut disk = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&asked.target)
        .map_err(|e| format!("{}: {e}", asked.target))?;
    let size = disk
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("{}: {e}", asked.target))?;
    let sectors = size / SECTOR;
    let (_, last_usable) = usable(sectors, SECTOR as u32).map_err(|e| e.to_string())?;
    let esp_first = ALIGN;
    let esp_last = esp_first + esp_bytes.div_ceil(SECTOR).next_multiple_of(ALIGN) - 1;
    let root_first = esp_last + 1;
    let root_last = (last_usable + 1) / ALIGN * ALIGN - 1;
    if root_last < root_first || (root_last - root_first + 1) * SECTOR < ROOT_BYTES {
        return Err(format!(
            "{} is {} MiB; it needs {} MiB",
            asked.target,
            size >> 20,
            (root_first * SECTOR + ROOT_BYTES + WIPE) >> 20
        ));
    }
    println!(
        "ferrix-install: {} ({} MiB) will be erased: EFI system partition of {} MiB from {live}, \
         root of {} MiB",
        asked.target,
        size >> 20,
        (esp_last - esp_first + 1) * SECTOR >> 20,
        (root_last - root_first + 1) * SECTOR >> 20
    );
    if !asked.yes && !confirm()? {
        return Err("nothing was written".into());
    }

    let partitions = [
        Partition::new(
            ESP,
            random_guid()?,
            esp_first,
            esp_last,
            "EFI system partition",
        ),
        Partition::new(
            LINUX_FILESYSTEM,
            random_guid()?,
            root_first,
            root_last,
            "ferrix-root",
        ),
    ];
    let table = write_table(sectors, SECTOR as u32, random_guid()?, &partitions)
        .map_err(|e| e.to_string())?;

    step("wiping the old table", || {
        let zeros = vec![0_u8; WIPE as usize];
        write_at(&mut disk, 0, &zeros)?;
        write_at(&mut disk, size - WIPE, &zeros)
    })?;
    step("copying the EFI system partition", || {
        copy_esp(&mut source, &mut disk, esp_bytes, esp_first)
    })?;
    step("writing the root volume", || {
        write_root(&mut disk, root_first * SECTOR)
    })?;
    step("writing the partition table", || {
        table
            .iter()
            .try_for_each(|(lba, bytes)| write_at(&mut disk, lba * SECTOR, bytes))
    })?;
    step("flushing", || disk.sync_all())?;
    println!(
        "ferrix-install: done. Remove the live disk and start the machine from {}.",
        asked.target
    );
    Ok(())
}

/// Run a step, saying what it is and what went wrong.
fn step(what: &str, work: impl FnOnce() -> io::Result<()>) -> Result<(), String> {
    println!("ferrix-install: {what}");
    work().map_err(|e| format!("{what}: {e}"))
}

/// The live disk: a `vd` disk whose first sector is `xtask`'s boot sector.
fn find_live() -> Result<String, String> {
    for letter in b'a'..=b'z' {
        let path = format!("/dev/vd{}", letter as char);
        let Ok(mut disk) = File::open(&path) else {
            continue;
        };
        let mut sector = [0_u8; 512];
        if disk.read_exact(&mut sector).is_ok()
            && &sector[3..11] == b"FERRIX  "
            && &sector[82..90] == b"FAT32   "
        {
            return Ok(path);
        }
    }
    Err("no live disk found; name it with --from".into())
}

/// Whether anything is mounted from `disk` or one of its partitions.
fn mounted(disk: &str) -> Result<bool, String> {
    let mounts =
        std::fs::read_to_string("/proc/mounts").map_err(|e| format!("/proc/mounts: {e}"))?;
    Ok(mounts.lines().any(|line| {
        line.split_whitespace().next().is_some_and(|source| {
            source
                .strip_prefix(disk)
                .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit()))
        })
    }))
}

/// The FAT volume's size, from its boot sector.
fn fat_volume_bytes(disk: &mut File) -> io::Result<u64> {
    let mut sector = [0_u8; 512];
    disk.seek(SeekFrom::Start(0))?;
    disk.read_exact(&mut sector)?;
    let bytes_per_sector = u64::from(u16::from_le_bytes([sector[11], sector[12]]));
    let small = u64::from(u16::from_le_bytes([sector[19], sector[20]]));
    let large = u64::from(u32::from_le_bytes([
        sector[32], sector[33], sector[34], sector[35],
    ]));
    let count = if small != 0 { small } else { large };
    if bytes_per_sector != SECTOR || count == 0 {
        return Err(io::Error::other("not a FAT volume of 512-byte sectors"));
    }
    Ok(count * bytes_per_sector)
}

/// Copy the live FAT volume to the ESP, telling its boot sectors where it
/// now starts (the BPB's hidden sectors).
fn copy_esp(source: &mut File, disk: &mut File, bytes: u64, first: u64) -> io::Result<()> {
    let mut buffer = vec![0_u8; CHUNK];
    let mut done = 0;
    source.seek(SeekFrom::Start(0))?;
    while done < bytes {
        let len = usize::try_from((bytes - done).min(CHUNK as u64)).map_err(io::Error::other)?;
        let piece = &mut buffer[..len];
        source.read_exact(piece)?;
        if done == 0 {
            let hidden = u32::try_from(first)
                .map_err(io::Error::other)?
                .to_le_bytes();
            piece[28..32].copy_from_slice(&hidden);
            let backup = usize::from(u16::from_le_bytes([piece[50], piece[51]])) * 512;
            if backup != 0 && backup + 32 <= piece.len() {
                piece[backup + 28..backup + 32].copy_from_slice(&hidden);
            }
        }
        write_at(disk, first * SECTOR + done, piece)?;
        done += len as u64;
    }
    Ok(())
}

/// Write the packed root volume at `at`, zeroing its first and last MiB
/// first so nothing old is taken for part of it.
fn write_root(disk: &mut File, at: u64) -> io::Result<()> {
    let packed = std::fs::read(ROOT_PACKED)?;
    let zeros = vec![0_u8; WIPE as usize];
    write_at(disk, at, &zeros)?;
    write_at(disk, at + ROOT_BYTES - WIPE, &zeros)?;
    for record in packed.chunks_exact(8 + BLOCK) {
        let (offset, block) = record.split_at(8);
        let offset = u64::from_le_bytes(offset.try_into().map_err(io::Error::other)?);
        if offset + BLOCK as u64 > ROOT_BYTES {
            return Err(io::Error::other(
                "the packed root volume is larger than it says",
            ));
        }
        write_at(disk, at + offset, block)?;
    }
    Ok(())
}

fn write_at(disk: &mut File, at: u64, bytes: &[u8]) -> io::Result<()> {
    disk.seek(SeekFrom::Start(at))?;
    disk.write_all(bytes)
}

fn random_guid() -> Result<Guid, String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(Guid::random(bytes))
}

fn confirm() -> Result<bool, String> {
    print!("Erase it and install Ferrix? Type yes: ");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim() == "yes")
}
