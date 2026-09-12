//! Fuzz the VFS with sequences of operations a program could make.
//!
//! From stage 8 every path a program passes to `open`, `mkdir`, `rename` or
//! `symlink` reaches `libs/vfs` in ring 0, and the order of those calls is the
//! program's to choose. The path walk follows symbolic links a program wrote,
//! crosses mounts, and caches both hits and misses; tmpfs takes up to four
//! inode locks for one `rename`. Neither kind of bug shows as a crash on the
//! first call — they show as a name that exists and cannot be found, or a
//! directory listing that disagrees with a lookup.
//!
//! # The properties
//!
//! * Nothing panics, whatever the sequence. With `overflow-checks` on, that
//!   includes every length and offset calculation.
//! * **A listing agrees with a lookup.** Every name `getdents64` reports in a
//!   directory resolves, without following links, to the inode number the
//!   listing gave. This is the property the dentry cache can break: a stale
//!   negative entry hides a file the filesystem has.
//! * **Records round-trip.** What the `getdents64` packer writes, a reader
//!   that knows only the layout reads back as the same entries.
//! * **A write reads back.** Bytes written at an offset read back at it.
//!
//! Paths are built from a tiny alphabet — three names, `.`, `..`, and a link
//! — so that operations collide with each other constantly rather than
//! scattering over names nothing else touches.

#![no_main]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use ferrix_vfs::dirent::{DirentWriter, records};
use ferrix_vfs::tmpfs::{HeapStorage, Tmpfs};
use ferrix_vfs::{
    Clock, Context, FileSystem, Namespace, OpenFlags, RenameMode, Timespec, Whence,
};
use libfuzzer_sys::fuzz_target;

#[derive(Debug, Default)]
struct Ticking(AtomicI64);

impl Clock for Ticking {
    fn now(&self) -> Timespec {
        Timespec {
            tv_sec: self.0.fetch_add(1, Ordering::Relaxed),
            tv_nsec: 0,
        }
    }
}

fn tmpfs(device: u64) -> Arc<dyn FileSystem> {
    Tmpfs::new(
        device,
        Arc::new(Ticking::default()),
        Arc::new(HeapStorage::new(1 << 20)),
        0o755,
    )
}

/// Hands out the fuzzer's bytes as the shapes this target needs.
struct Input<'a> {
    rest: &'a [u8],
}

impl Input<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (first, tail) = self.rest.split_first()?;
        self.rest = tail;
        Some(*first)
    }

    /// A path of up to four components from the small alphabet.
    fn path(&mut self) -> Option<Vec<u8>> {
        const PARTS: [&[u8]; 8] = [b"a", b"b", b"c", b".", b"..", b"l", b"", b"mnt"];
        let shape = self.byte()?;
        let mut path = Vec::new();
        if shape & 0x80 != 0 {
            path.push(b'/');
        }
        for _ in 0..=(shape & 0x03) {
            let part = PARTS[usize::from(self.byte()? % 8)];
            path.extend_from_slice(part);
            path.push(b'/');
        }
        if shape & 0x40 == 0 {
            let _ = path.pop();
        }
        if path.is_empty() {
            path.push(b'.');
        }
        Some(path)
    }
}

/// Every name a listing of `dir` reports resolves to the inode it reported.
fn listing_agrees(ns: &Namespace, ctx: &Context, dir: &[u8]) {
    let flags = OpenFlags {
        read: true,
        directory: true,
        ..OpenFlags::default()
    };
    let Ok(file) = ns.open(ctx, None, dir, &flags, 0) else {
        return;
    };
    let mut emitted = Vec::new();
    let mut buf = [0_u8; 256];
    loop {
        let mut writer = DirentWriter::new(&mut buf);
        let mut batch = Vec::new();
        file.read_dir(&mut |entry| {
            let fits = writer.push(entry.ino, entry.next, entry.kind.dirent_type(), entry.name);
            if fits {
                batch.push((entry.ino, entry.name.to_vec()));
            }
            fits
        })
        .expect("a directory that opened lists");
        let used = writer.used();
        if used == 0 {
            break;
        }
        let read: Vec<(u64, Vec<u8>)> = records(&buf[..used])
            .map(|record| (record.ino, record.name.to_vec()))
            .collect();
        assert_eq!(read, batch, "getdents64 records did not read back");
        emitted.extend(batch);
    }
    for (ino, name) in emitted {
        if name == b"." || name == b".." {
            continue;
        }
        let mut path = dir.to_vec();
        path.push(b'/');
        path.extend_from_slice(&name);
        let at = ns
            .resolve(ctx, None, &path, false)
            .expect("a listed name resolves");
        // A mount point is listed with the inode it covers and resolves to
        // the mounted root, on Linux as here: `readdir` reads the directory's
        // own filesystem, and a lookup crosses the mount. The first thing
        // this target found was that it had asserted otherwise.
        if Arc::ptr_eq(&at.dentry, at.mount.root()) {
            continue;
        }
        let stat = ns.stat(&at).expect("a resolved name stats");
        assert_eq!(stat.metadata.ino, ino, "a listed name resolved elsewhere");
    }
}

fuzz_target!(|data: &[u8]| {
    let ns = Namespace::with_cache(tmpfs(1), 16);
    let ctx = ns.context();
    let _ = ns.mkdir(&ctx, None, b"/mnt", 0o755);
    let mut input = Input { rest: data };
    let mut devices = 2_u64;

    for _ in 0..64 {
        let Some(op) = input.byte() else {
            break;
        };
        let Some(path) = input.path() else {
            break;
        };
        match op % 12 {
            0 => {
                let _ = ns.mkdir(&ctx, None, &path, 0o755);
            }
            1 => {
                let flags = OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    truncate: op & 0x10 != 0,
                    append: op & 0x20 != 0,
                    ..OpenFlags::default()
                };
                if let Ok(file) = ns.open(&ctx, None, &path, &flags, 0o644) {
                    let offset = i64::from(input.byte().unwrap_or(0)) * 97;
                    let len = usize::from(input.byte().unwrap_or(0));
                    let payload: Vec<u8> = (0..len).map(|i| i as u8 ^ op).collect();
                    let append = flags.append;
                    if file.seek(offset, Whence::Set).is_ok()
                        && let Ok(written) = file.write(&payload)
                        && !append
                    {
                        let mut back = vec![0_u8; written];
                        let got = file.read_at(offset as u64, &mut back).expect("reads back");
                        assert_eq!(&back[..got], &payload[..got], "a write did not read back");
                        assert_eq!(got, written, "a write read back short");
                    }
                }
            }
            2 => {
                let _ = ns.unlink(&ctx, None, &path);
            }
            3 => {
                let _ = ns.rmdir(&ctx, None, &path);
            }
            4 | 5 => {
                let Some(other) = input.path() else { break };
                let mode = if op & 0x10 == 0 {
                    RenameMode::Replace
                } else {
                    RenameMode::NoReplace
                };
                let _ = ns.rename(&ctx, (None, &path), (None, &other), mode);
            }
            6 => {
                let Some(target) = input.path() else { break };
                let _ = ns.symlink(&ctx, None, &path, &target);
            }
            7 => {
                let Some(other) = input.path() else { break };
                let _ = ns.link(&ctx, (None, &path), op & 0x10 != 0, (None, &other));
            }
            8 => {
                if let Ok(at) = ns.resolve(&ctx, None, &path, true) {
                    let _ = ns.truncate(&at, u64::from(input.byte().unwrap_or(0)) * 131);
                }
            }
            9 => {
                if let Ok(at) = ns.resolve(&ctx, None, &path, true) {
                    devices += 1;
                    let _ = ns.mount(tmpfs(devices), &at);
                }
            }
            10 => {
                if let Ok(at) = ns.resolve(&ctx, None, &path, true) {
                    let _ = ns.unmount(&at);
                }
            }
            _ => listing_agrees(&ns, &ctx, &path),
        }
    }
    listing_agrees(&ns, &ctx, b"/");
    listing_agrees(&ns, &ctx, b"/mnt");
});
