//! Host tests: the VFS against the rules a shell and a C library assume.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI64, Ordering};

use crate::dirent::{DirentWriter, records};
use crate::fd::FdTable;
use crate::initramfs::{self, makedev};
use crate::path::split_last;
use crate::tmpfs::{HeapStorage, Tmpfs};
use crate::{
    Clock, Context, Errno, FileSystem, FileType, Namespace, OpenFile, OpenFlags, RenameMode,
    Timespec, Whence,
};

/// A clock that advances a second every time it is read.
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
        Arc::new(HeapStorage::new(1 << 30)),
        0o755,
    )
}

fn fresh() -> (Namespace, Context) {
    let ns = Namespace::new(tmpfs(1));
    let ctx = ns.context();
    (ns, ctx)
}

const RW_CREATE: OpenFlags = OpenFlags {
    read: true,
    write: true,
    create: true,
    exclusive: false,
    truncate: false,
    append: false,
    directory: false,
    nofollow: false,
    path: false,
    nonblock: false,
};

const READ: OpenFlags = OpenFlags {
    read: true,
    write: false,
    create: false,
    exclusive: false,
    truncate: false,
    append: false,
    directory: false,
    nofollow: false,
    path: false,
    nonblock: false,
};

fn write_file(ns: &Namespace, ctx: &Context, path: &str, data: &[u8]) {
    let file = ns
        .open(ctx, None, path.as_bytes(), &RW_CREATE, 0o644)
        .unwrap();
    assert_eq!(file.write(data).unwrap(), data.len());
}

fn read_file(ns: &Namespace, ctx: &Context, path: &str) -> Result<Vec<u8>, Errno> {
    let file = ns.open(ctx, None, path.as_bytes(), &READ, 0)?;
    let mut out = Vec::new();
    let mut buf = [0_u8; 7];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n]);
    }
}

fn names(file: &OpenFile) -> Vec<String> {
    let mut all = Vec::new();
    file.read_dir(&mut |entry| {
        all.push(String::from_utf8_lossy(entry.name).into_owned());
        true
    })
    .unwrap();
    all
}

fn kind(ns: &Namespace, ctx: &Context, path: &str, follow: bool) -> Result<FileType, Errno> {
    let at = ns.resolve(ctx, None, path.as_bytes(), follow)?;
    Ok(ns.stat(&at)?.metadata.kind)
}

// -- Paths -------------------------------------------------------------------

#[test]
fn split_last_matches_dirname_and_basename() {
    assert_eq!(split_last(b"a/b/c"), (&b"a/b"[..], &b"c"[..]));
    assert_eq!(split_last(b"a/b/"), (&b"a"[..], &b"b"[..]));
    assert_eq!(split_last(b"/x"), (&b"/"[..], &b"x"[..]));
    assert_eq!(split_last(b"x"), (&b""[..], &b"x"[..]));
}

#[test]
fn an_empty_path_is_enoent_and_root_is_a_directory() {
    let (ns, ctx) = fresh();
    assert_eq!(
        ns.resolve(&ctx, None, b"", true).unwrap_err(),
        Errno::ENOENT
    );
    assert_eq!(kind(&ns, &ctx, "/", true), Ok(FileType::Directory));
    assert_eq!(kind(&ns, &ctx, "///", true), Ok(FileType::Directory));
}

#[test]
fn a_name_longer_than_name_max_is_refused() {
    let (ns, ctx) = fresh();
    let long = vec![b'a'; 256];
    assert_eq!(
        ns.mkdir(&ctx, None, &long, 0o755).unwrap_err(),
        Errno::ENAMETOOLONG
    );
    assert!(ns.mkdir(&ctx, None, &long[..255], 0o755).is_ok());
}

// -- Files -------------------------------------------------------------------

#[test]
fn a_file_reads_back_what_was_written_and_stat_agrees() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/hello", b"hello, world\n");
    assert_eq!(read_file(&ns, &ctx, "/hello").unwrap(), b"hello, world\n");
    let at = ns.resolve(&ctx, None, b"/hello", true).unwrap();
    let stat = ns.stat(&at).unwrap();
    assert_eq!(stat.metadata.size, 13);
    assert_eq!(stat.metadata.mode(), 0o100_644);
    assert_eq!(stat.dev, 1);
}

#[test]
fn a_file_spanning_pages_reads_back_whole() {
    let (ns, ctx) = fresh();
    let data: Vec<u8> = (0..10_000_u32).map(|i| (i % 251) as u8).collect();
    write_file(&ns, &ctx, "/big", &data);
    assert_eq!(read_file(&ns, &ctx, "/big").unwrap(), data);
}

#[test]
fn exclusive_create_refuses_an_existing_name_even_a_dangling_link() {
    let (ns, ctx) = fresh();
    ns.symlink(&ctx, None, b"/dangling", b"/nowhere").unwrap();
    let flags = OpenFlags {
        exclusive: true,
        ..RW_CREATE
    };
    assert_eq!(
        ns.open(&ctx, None, b"/dangling", &flags, 0o644)
            .unwrap_err(),
        Errno::EEXIST
    );
    // Without O_EXCL the link is followed and its target created.
    let _ = ns
        .open(&ctx, None, b"/dangling", &RW_CREATE, 0o644)
        .unwrap();
    assert_eq!(kind(&ns, &ctx, "/nowhere", false), Ok(FileType::Regular));
}

#[test]
fn truncate_on_open_empties_and_append_writes_at_the_end() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/f", b"0123456789");
    let append = OpenFlags {
        append: true,
        ..RW_CREATE
    };
    let file = ns.open(&ctx, None, b"/f", &append, 0).unwrap();
    let _ = file.seek(0, Whence::Set).unwrap();
    let _ = file.write(b"AB").unwrap();
    assert_eq!(read_file(&ns, &ctx, "/f").unwrap(), b"0123456789AB");

    let trunc = OpenFlags {
        truncate: true,
        ..RW_CREATE
    };
    let _ = ns.open(&ctx, None, b"/f", &trunc, 0).unwrap();
    assert_eq!(read_file(&ns, &ctx, "/f").unwrap(), b"");
}

#[test]
fn shrinking_then_growing_reads_zeros_not_old_bytes() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/f", &[0xAA; 5000]);
    let at = ns.resolve(&ctx, None, b"/f", true).unwrap();
    ns.truncate(&at, 10).unwrap();
    ns.truncate(&at, 5000).unwrap();
    let back = read_file(&ns, &ctx, "/f").unwrap();
    assert_eq!(&back[..10], &[0xAA; 10]);
    assert!(back[10..].iter().all(|&b| b == 0));
}

#[test]
fn dup_shares_an_offset_and_separate_opens_do_not() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/f", b"abcdef");
    let one = ns.open(&ctx, None, b"/f", &READ, 0).unwrap();
    let dup = Arc::clone(&one);
    let other = ns.open(&ctx, None, b"/f", &READ, 0).unwrap();
    let mut buf = [0_u8; 3];
    let _ = one.read(&mut buf).unwrap();
    let _ = dup.read(&mut buf).unwrap();
    assert_eq!(&buf, b"def");
    let _ = other.read(&mut buf).unwrap();
    assert_eq!(&buf, b"abc");
}

#[test]
fn seek_measures_from_each_origin_and_refuses_before_the_start() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/f", b"0123456789");
    let file = ns.open(&ctx, None, b"/f", &READ, 0).unwrap();
    assert_eq!(file.seek(3, Whence::Set), Ok(3));
    assert_eq!(file.seek(2, Whence::Current), Ok(5));
    assert_eq!(file.seek(-1, Whence::End), Ok(9));
    assert_eq!(file.seek(-100, Whence::Current), Err(Errno::EINVAL));
    assert_eq!(file.seek(4, Whence::Data), Ok(4));
    assert_eq!(file.seek(4, Whence::Hole), Ok(10));
    assert_eq!(file.seek(10, Whence::Data), Err(Errno::ENXIO));
}

#[test]
fn an_unlinked_file_stays_readable_through_an_open_description() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/gone", b"still here");
    let file = ns.open(&ctx, None, b"/gone", &READ, 0).unwrap();
    ns.unlink(&ctx, None, b"/gone").unwrap();
    assert_eq!(read_file(&ns, &ctx, "/gone").unwrap_err(), Errno::ENOENT);
    let mut buf = [0_u8; 10];
    assert_eq!(file.read(&mut buf), Ok(10));
    assert_eq!(&buf, b"still here");
    assert_eq!(file.inode().metadata().nlink, 0);
}

#[test]
fn an_unlinked_or_replaced_file_is_freed_once_nothing_holds_it() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/doomed", b"x");
    write_file(&ns, &ctx, "/replaced", b"y");
    write_file(&ns, &ctx, "/replacement", b"z");
    let inode_of = |path: &str| {
        let file = ns.open(&ctx, None, path.as_bytes(), &READ, 0).unwrap();
        Arc::downgrade(file.inode())
    };
    let doomed = inode_of("/doomed");
    let replaced = inode_of("/replaced");
    for _ in 0..3 {
        let _ = read_file(&ns, &ctx, "/doomed");
        let _ = read_file(&ns, &ctx, "/replaced");
    }
    ns.unlink(&ctx, None, b"/doomed").unwrap();
    ns.rename(
        &ctx,
        (None, b"/replacement"),
        (None, b"/replaced"),
        RenameMode::Replace,
    )
    .unwrap();
    assert!(
        doomed.upgrade().is_none(),
        "the cache kept an unlinked file alive"
    );
    assert!(
        replaced.upgrade().is_none(),
        "the cache kept a replaced file alive"
    );
}

#[test]
fn a_removed_directory_is_freed_even_after_misses_inside_it() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    let directory = {
        let at = ns.resolve(&ctx, None, b"/d", true).unwrap();
        Arc::downgrade(&at.inode().unwrap())
    };
    for name in ["/d/missing", "/d/also-missing", "/d/missing/deeper"] {
        let _ = ns.resolve(&ctx, None, name.as_bytes(), true);
    }
    ns.rmdir(&ctx, None, b"/d").unwrap();
    assert!(
        directory.upgrade().is_none(),
        "cached misses inside a removed directory kept it alive"
    );
}

#[test]
fn a_rename_to_a_new_name_does_not_leave_its_miss_holding_the_directory() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    write_file(&ns, &ctx, "/d/a", b"x");
    let directory = {
        let at = ns.resolve(&ctx, None, b"/d", true).unwrap();
        Arc::downgrade(&at.inode().unwrap())
    };
    ns.rename(
        &ctx,
        (None, b"/d/a"),
        (None, b"/d/b"),
        RenameMode::NoReplace,
    )
    .unwrap();
    ns.unlink(&ctx, None, b"/d/b").unwrap();
    ns.rmdir(&ctx, None, b"/d").unwrap();
    assert!(
        directory.upgrade().is_none(),
        "the rename's destination miss kept the directory alive"
    );
}

#[test]
fn reading_a_directory_is_eisdir_and_writing_one_cannot_be_opened() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    let dir = ns.open(&ctx, None, b"/d", &READ, 0).unwrap();
    assert_eq!(dir.read(&mut [0; 4]), Err(Errno::EISDIR));
    let write = OpenFlags {
        write: true,
        ..OpenFlags::default()
    };
    assert_eq!(
        ns.open(&ctx, None, b"/d", &write, 0).unwrap_err(),
        Errno::EISDIR
    );
}

// -- Directories -------------------------------------------------------------

#[test]
fn a_missing_parent_is_enoent_and_a_file_on_the_way_is_enotdir() {
    let (ns, ctx) = fresh();
    assert_eq!(
        ns.mkdir(&ctx, None, b"/a/b", 0o755).unwrap_err(),
        Errno::ENOENT
    );
    write_file(&ns, &ctx, "/file", b"");
    assert_eq!(
        ns.mkdir(&ctx, None, b"/file/b", 0o755).unwrap_err(),
        Errno::ENOTDIR
    );
    assert_eq!(
        ns.resolve(&ctx, None, b"/file/", true).unwrap_err(),
        Errno::ENOTDIR
    );
}

#[test]
fn directory_link_counts_follow_their_subdirectories() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/d/one", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/d/two", 0o755).unwrap();
    write_file(&ns, &ctx, "/d/file", b"");
    let nlink = |path: &str| {
        let at = ns.resolve(&ctx, None, path.as_bytes(), true).unwrap();
        ns.stat(&at).unwrap().metadata.nlink
    };
    assert_eq!(nlink("/d"), 4);
    ns.rmdir(&ctx, None, b"/d/one").unwrap();
    assert_eq!(nlink("/d"), 3);
}

#[test]
fn rmdir_refuses_dot_dotdot_nonempty_and_files() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    write_file(&ns, &ctx, "/d/f", b"");
    assert_eq!(ns.rmdir(&ctx, None, b"/d").unwrap_err(), Errno::ENOTEMPTY);
    assert_eq!(ns.rmdir(&ctx, None, b"/d/.").unwrap_err(), Errno::EINVAL);
    assert_eq!(
        ns.rmdir(&ctx, None, b"/d/..").unwrap_err(),
        Errno::ENOTEMPTY
    );
    assert_eq!(ns.rmdir(&ctx, None, b"/d/f").unwrap_err(), Errno::ENOTDIR);
    assert_eq!(ns.unlink(&ctx, None, b"/d").unwrap_err(), Errno::EISDIR);
    assert_eq!(ns.rmdir(&ctx, None, b"/").unwrap_err(), Errno::EBUSY);
}

#[test]
fn readdir_lists_dot_dotdot_and_every_name_once() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    for name in ["c", "a", "b"] {
        write_file(&ns, &ctx, &alloc::format!("/d/{name}"), b"");
    }
    let dir = ns.open(&ctx, None, b"/d", &READ, 0).unwrap();
    assert_eq!(names(&dir), [".", "..", "c", "a", "b"]);
    // Read to the end: a second pass reports nothing until rewound.
    assert!(names(&dir).is_empty());
    let _ = dir.seek(0, Whence::Set).unwrap();
    assert_eq!(names(&dir).len(), 5);
}

#[test]
fn a_directory_being_emptied_while_read_loses_no_entries() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    for i in 0..50 {
        write_file(&ns, &ctx, &alloc::format!("/d/{i}"), b"");
    }
    let dir = ns.open(&ctx, None, b"/d", &READ, 0).unwrap();
    // `rm -rf`: read a few, delete them, read a few more.
    let mut removed = 0;
    loop {
        let mut batch = Vec::new();
        dir.read_dir(&mut |entry| {
            if batch.len() == 7 {
                return false;
            }
            batch.push(entry.name.to_vec());
            true
        })
        .unwrap();
        if batch.is_empty() {
            break;
        }
        for name in batch {
            if name != b"." && name != b".." {
                let mut path = b"/d/".to_vec();
                path.extend_from_slice(&name);
                ns.unlink(&ctx, None, &path).unwrap();
                removed += 1;
            }
        }
    }
    assert_eq!(removed, 50);
    ns.rmdir(&ctx, None, b"/d").unwrap();
}

#[test]
fn getdents_packing_resumes_where_a_full_buffer_stopped() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    let expected: Vec<Vec<u8>> = (0..40)
        .map(|i| alloc::format!("entry-{i:03}").into_bytes())
        .collect();
    for name in &expected {
        write_file(
            &ns,
            &ctx,
            core::str::from_utf8(&[b"/d/", &name[..]].concat()).unwrap(),
            b"",
        );
    }
    let dir = ns.open(&ctx, None, b"/d", &READ, 0).unwrap();
    let mut seen = Vec::new();
    loop {
        let mut buf = [0_u8; 100];
        let mut writer = DirentWriter::new(&mut buf);
        dir.read_dir(&mut |entry| {
            writer.push(entry.ino, entry.next, entry.kind.dirent_type(), entry.name)
        })
        .unwrap();
        let used = writer.used();
        if used == 0 {
            break;
        }
        for record in records(&buf[..used]) {
            seen.push(record.name.to_vec());
        }
    }
    assert_eq!(&seen[..2], &[b".".to_vec(), b"..".to_vec()]);
    assert_eq!(&seen[2..], &expected[..]);
}

#[test]
fn a_dirent_record_is_padded_to_eight_with_the_name_at_nineteen() {
    let mut buf = [0xFF_u8; 64];
    let mut writer = DirentWriter::new(&mut buf);
    assert!(writer.push(7, 3, 8, b"hello"));
    assert_eq!(writer.used(), 32);
    assert_eq!(&buf[19..24], b"hello");
    assert_eq!(buf[24], 0);
    assert_eq!(u16::from_le_bytes([buf[16], buf[17]]), 32);
    let mut tiny = [0_u8; 20];
    assert!(!DirentWriter::new(&mut tiny).push(1, 1, 8, b"x"));
}

// -- Symbolic links ----------------------------------------------------------

#[test]
fn relative_and_absolute_links_resolve_and_lstat_does_not_follow() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/usr", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/usr/bin", 0o755).unwrap();
    write_file(&ns, &ctx, "/usr/bin/busybox", b"#!");
    ns.symlink(&ctx, None, b"/bin", b"usr/bin").unwrap();
    ns.symlink(&ctx, None, b"/usr/bin/sh", b"/bin/busybox")
        .unwrap();
    assert_eq!(read_file(&ns, &ctx, "/bin/sh").unwrap(), b"#!");
    assert_eq!(kind(&ns, &ctx, "/bin", false), Ok(FileType::Symlink));
    assert_eq!(kind(&ns, &ctx, "/bin/", false), Ok(FileType::Directory));
    assert_eq!(
        ns.read_link(&ctx, None, b"/bin/sh").unwrap(),
        b"/bin/busybox"
    );
    assert_eq!(
        ns.read_link(&ctx, None, b"/usr").unwrap_err(),
        Errno::EINVAL
    );
}

#[test]
fn a_link_cycle_is_eloop_and_nofollow_refuses_a_link() {
    let (ns, ctx) = fresh();
    ns.symlink(&ctx, None, b"/a", b"b").unwrap();
    ns.symlink(&ctx, None, b"/b", b"a").unwrap();
    assert_eq!(read_file(&ns, &ctx, "/a").unwrap_err(), Errno::ELOOP);
    write_file(&ns, &ctx, "/real", b"");
    ns.symlink(&ctx, None, b"/link", b"real").unwrap();
    let nofollow = OpenFlags {
        nofollow: true,
        ..READ
    };
    assert_eq!(
        ns.open(&ctx, None, b"/link", &nofollow, 0).unwrap_err(),
        Errno::ELOOP
    );
}

#[test]
fn dotdot_through_a_link_is_lexical_to_the_target_not_the_link() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/x", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/x/y", 0o755).unwrap();
    write_file(&ns, &ctx, "/x/marker", b"x");
    ns.symlink(&ctx, None, b"/l", b"/x/y").unwrap();
    // `l/..` is `/x`, the target's parent, as on Linux.
    assert_eq!(read_file(&ns, &ctx, "/l/../marker").unwrap(), b"x");
}

// -- Hard links --------------------------------------------------------------

#[test]
fn hard_links_share_contents_and_count_names() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/one", b"shared");
    ns.link(&ctx, (None, b"/one"), false, (None, b"/two"))
        .unwrap();
    let at = ns.resolve(&ctx, None, b"/two", true).unwrap();
    assert_eq!(ns.stat(&at).unwrap().metadata.nlink, 2);
    ns.unlink(&ctx, None, b"/one").unwrap();
    assert_eq!(read_file(&ns, &ctx, "/two").unwrap(), b"shared");
    assert_eq!(ns.stat(&at).unwrap().metadata.nlink, 1);
    ns.mkdir(&ctx, None, b"/dir", 0o755).unwrap();
    assert_eq!(
        ns.link(&ctx, (None, b"/dir"), false, (None, b"/dir2"))
            .unwrap_err(),
        Errno::EPERM
    );
}

// -- Rename ------------------------------------------------------------------

#[test]
fn rename_replaces_a_file_and_moves_a_directory_with_its_contents() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/a", b"new");
    write_file(&ns, &ctx, "/b", b"old");
    ns.rename(&ctx, (None, b"/a"), (None, b"/b"), RenameMode::Replace)
        .unwrap();
    assert_eq!(read_file(&ns, &ctx, "/b").unwrap(), b"new");
    assert_eq!(read_file(&ns, &ctx, "/a").unwrap_err(), Errno::ENOENT);

    ns.mkdir(&ctx, None, b"/src", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/dst", 0o755).unwrap();
    write_file(&ns, &ctx, "/src/inner", b"moved");
    ns.rename(
        &ctx,
        (None, b"/src"),
        (None, b"/dst/sub"),
        RenameMode::Replace,
    )
    .unwrap();
    assert_eq!(read_file(&ns, &ctx, "/dst/sub/inner").unwrap(), b"moved");
    assert_eq!(kind(&ns, &ctx, "/src", true), Err(Errno::ENOENT));
}

#[test]
fn rename_refuses_the_cases_rename2_lists() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/d", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/d/sub", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/full", 0o755).unwrap();
    write_file(&ns, &ctx, "/full/f", b"");
    write_file(&ns, &ctx, "/file", b"");
    let r = |old: &[u8], new: &[u8], mode| ns.rename(&ctx, (None, old), (None, new), mode);
    assert_eq!(
        r(b"/d", b"/d/sub/inside", RenameMode::Replace),
        Err(Errno::EINVAL)
    );
    assert_eq!(
        r(b"/d", b"/full", RenameMode::Replace),
        Err(Errno::ENOTEMPTY)
    );
    assert_eq!(r(b"/file", b"/d", RenameMode::Replace), Err(Errno::EISDIR));
    assert_eq!(
        r(b"/d/sub", b"/file", RenameMode::Replace),
        Err(Errno::ENOTDIR)
    );
    assert_eq!(
        r(b"/file", b"/full/f", RenameMode::NoReplace),
        Err(Errno::EEXIST)
    );
    assert_eq!(
        r(b"/d/sub", b"/d", RenameMode::Replace),
        Err(Errno::ENOTEMPTY)
    );
    assert_eq!(
        r(b"/missing", b"/x", RenameMode::Replace),
        Err(Errno::ENOENT)
    );
}

#[test]
fn a_working_directory_follows_its_directory_through_a_rename() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/a", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/a/b", 0o755).unwrap();
    let cwd = ns.resolve(&ctx, None, b"/a/b", true).unwrap();
    assert_eq!(ns.path_of(&cwd, &ctx.root), b"/a/b");
    ns.rename(
        &ctx,
        (None, b"/a"),
        (None, b"/renamed"),
        RenameMode::Replace,
    )
    .unwrap();
    assert_eq!(ns.path_of(&cwd, &ctx.root), b"/renamed/b");

    let here = Context {
        root: ctx.root.clone(),
        cwd,
    };
    write_file(&ns, &here, "relative", b"r");
    assert_eq!(read_file(&ns, &ctx, "/renamed/b/relative").unwrap(), b"r");
    assert_eq!(read_file(&ns, &here, "../b/relative").unwrap(), b"r");
}

// -- The dentry cache --------------------------------------------------------

#[test]
fn a_cached_miss_does_not_hide_a_later_create() {
    let (ns, ctx) = fresh();
    for _ in 0..3 {
        assert_eq!(read_file(&ns, &ctx, "/later").unwrap_err(), Errno::ENOENT);
    }
    write_file(&ns, &ctx, "/later", b"here");
    assert_eq!(read_file(&ns, &ctx, "/later").unwrap(), b"here");
    ns.unlink(&ctx, None, b"/later").unwrap();
    assert_eq!(read_file(&ns, &ctx, "/later").unwrap_err(), Errno::ENOENT);
}

#[test]
fn the_cache_is_bounded() {
    let ns = Namespace::with_cache(tmpfs(1), 8);
    let ctx = ns.context();
    for i in 0..100 {
        let _ = ns.resolve(&ctx, None, alloc::format!("/miss-{i}").as_bytes(), true);
    }
    assert!(ns.cached() <= 8);
}

// -- Mounts ------------------------------------------------------------------

#[test]
fn a_mount_covers_a_directory_and_dotdot_climbs_out_of_it() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/mnt", 0o755).unwrap();
    write_file(&ns, &ctx, "/mnt/under", b"hidden");
    write_file(&ns, &ctx, "/top", b"top");
    let mnt = ns.resolve(&ctx, None, b"/mnt", true).unwrap();
    let mount = ns.mount(tmpfs(2), &mnt).unwrap();

    assert_eq!(
        read_file(&ns, &ctx, "/mnt/under").unwrap_err(),
        Errno::ENOENT
    );
    write_file(&ns, &ctx, "/mnt/over", b"over");
    let over = ns.resolve(&ctx, None, b"/mnt/over", true).unwrap();
    assert_eq!(ns.stat(&over).unwrap().dev, 2);
    assert_eq!(read_file(&ns, &ctx, "/mnt/../top").unwrap(), b"top");
    assert_eq!(ns.path_of(&over, &ctx.root), b"/mnt/over");

    assert_eq!(ns.rmdir(&ctx, None, b"/mnt").unwrap_err(), Errno::EBUSY);
    assert_eq!(
        ns.rename(
            &ctx,
            (None, b"/mnt/over"),
            (None, b"/moved"),
            RenameMode::Replace
        )
        .unwrap_err(),
        Errno::EXDEV
    );
    assert_eq!(
        ns.link(&ctx, (None, b"/top"), false, (None, b"/mnt/link"))
            .unwrap_err(),
        Errno::EXDEV
    );

    let root = ns.resolve(&ctx, None, b"/mnt", true).unwrap();
    assert!(Arc::ptr_eq(&root.mount, &mount));
    ns.unmount(&root).unwrap();
    assert_eq!(read_file(&ns, &ctx, "/mnt/under").unwrap(), b"hidden");
    assert_eq!(
        read_file(&ns, &ctx, "/mnt/over").unwrap_err(),
        Errno::ENOENT
    );
}

#[test]
fn unmount_refuses_a_mount_with_mounts_inside_it() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/a", 0o755).unwrap();
    let a = ns.resolve(&ctx, None, b"/a", true).unwrap();
    let _ = ns.mount(tmpfs(2), &a).unwrap();
    ns.mkdir(&ctx, None, b"/a/b", 0o755).unwrap();
    let b = ns.resolve(&ctx, None, b"/a/b", true).unwrap();
    let _ = ns.mount(tmpfs(3), &b).unwrap();
    let a_root = ns.resolve(&ctx, None, b"/a", true).unwrap();
    assert_eq!(ns.unmount(&a_root).unwrap_err(), Errno::EBUSY);
    assert_eq!(ns.unmount(&ctx.root).unwrap_err(), Errno::EINVAL);
    let b_root = ns.resolve(&ctx, None, b"/a/b", true).unwrap();
    ns.unmount(&b_root).unwrap();
    ns.unmount(&a_root).unwrap();
}

#[test]
fn dotdot_never_climbs_above_a_context_root() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/jail", 0o755).unwrap();
    write_file(&ns, &ctx, "/secret", b"s");
    write_file(&ns, &ctx, "/jail/secret", b"j");
    let jail = ns.resolve(&ctx, None, b"/jail", true).unwrap();
    let inside = Context {
        root: jail.clone(),
        cwd: jail,
    };
    assert_eq!(read_file(&ns, &inside, "/../../secret").unwrap(), b"j");
}

// -- Descriptor tables -------------------------------------------------------

#[test]
fn descriptors_are_the_lowest_free_number() {
    let mut table = FdTable::new();
    assert_eq!(table.insert('a', false), Ok(0));
    assert_eq!(table.insert('b', false), Ok(1));
    assert_eq!(table.insert('c', false), Ok(2));
    assert_eq!(table.remove(1), Ok('b'));
    assert_eq!(table.insert('d', false), Ok(1));
    assert_eq!(table.insert_from(5, 'e', false), Ok(5));
    assert_eq!(table.insert('f', false), Ok(3));
    assert_eq!(table.len(), 5);
}

#[test]
fn dup2_replaces_and_hands_back_the_old_description() {
    let mut table = FdTable::new();
    let _ = table.insert("stdout", false).unwrap();
    let _ = table.insert("file", false).unwrap();
    assert_eq!(table.install(0, "file", false), Ok(Some("stdout")));
    assert_eq!(table.install(7, "x", true), Ok(None));
    assert_eq!(table.get(7), Ok(&"x"));
    assert_eq!(table.install(-1, "x", false), Err(Errno::EBADF));
    assert_eq!(table.install(1024, "x", false), Err(Errno::EBADF));
    assert_eq!(table.get(3), Err(Errno::EBADF));
    assert_eq!(table.get(-1), Err(Errno::EBADF));
}

#[test]
fn a_full_table_is_emfile_and_exec_closes_cloexec() {
    let mut table = FdTable::new();
    table.set_limit(4).unwrap();
    for i in 0..4 {
        let _ = table.insert(i, i % 2 == 1).unwrap();
    }
    assert_eq!(table.insert(9, false), Err(Errno::EMFILE));
    assert_eq!(table.insert_from(4, 9, false), Err(Errno::EINVAL));
    let mut closed = table.take_cloexec();
    closed.sort_unstable();
    assert_eq!(closed, [1, 3]);
    assert_eq!(table.iter().map(|(fd, _)| fd).collect::<Vec<_>>(), [0, 2]);
    assert_eq!(table.set_limit(2_000_000), Err(Errno::EPERM));
}

// -- initramfs ---------------------------------------------------------------

/// Build a newc archive.
struct Newc(Vec<u8>);

impl Newc {
    fn new() -> Newc {
        Newc(Vec::new())
    }

    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
    }

    fn entry(
        &mut self,
        name: &str,
        mode: u32,
        ino: u32,
        nlink: u32,
        rdev: (u32, u32),
        data: &[u8],
    ) {
        let fields = [
            ino,
            mode,
            0,
            0,
            nlink,
            1_700_000_000,
            u32::try_from(data.len()).unwrap(),
            0,
            0,
            rdev.0,
            rdev.1,
            u32::try_from(name.len() + 1).unwrap(),
            0,
        ];
        self.0.extend_from_slice(b"070701");
        for field in fields {
            self.0
                .extend_from_slice(alloc::format!("{field:08X}").as_bytes());
        }
        self.0.extend_from_slice(name.as_bytes());
        self.0.push(0);
        self.pad();
        self.0.extend_from_slice(data);
        self.pad();
    }

    fn finish(mut self) -> Vec<u8> {
        self.entry("TRAILER!!!", 0, 0, 1, (0, 0), b"");
        self.0
    }
}

#[test]
fn an_archive_unpacks_with_links_nodes_and_unsafe_names_skipped() {
    let mut archive = Newc::new();
    archive.entry(".", 0o040_700, 1, 2, (0, 0), b"");
    archive.entry("bin", 0o040_755, 2, 2, (0, 0), b"");
    archive.entry("bin/busybox", 0o100_755, 3, 1, (0, 0), b"\x7fELF");
    archive.entry("bin/sh", 0o120_777, 4, 1, (0, 0), b"busybox");
    archive.entry("etc", 0o040_755, 5, 2, (0, 0), b"");
    archive.entry("etc/a", 0o100_644, 6, 2, (0, 0), b"");
    archive.entry("etc/b", 0o100_644, 6, 2, (0, 0), b"linked");
    archive.entry("dev", 0o040_755, 7, 2, (0, 0), b"");
    archive.entry("dev/console", 0o020_600, 8, 1, (5, 1), b"");
    archive.entry("../escape", 0o100_644, 9, 1, (0, 0), b"no");
    archive.entry("./etc/", 0o040_750, 5, 2, (0, 0), b"");
    let archive = archive.finish();

    let (ns, ctx) = fresh();
    let made = initramfs::unpack(&ns, &ctx, &archive).unwrap();
    assert_eq!(made.directories, 3);
    assert_eq!(made.files, 2);
    assert_eq!(made.symlinks, 1);
    assert_eq!(made.hard_links, 1);
    assert_eq!(made.nodes, 1);
    assert_eq!(made.skipped, 1);

    assert_eq!(read_file(&ns, &ctx, "/bin/sh").unwrap(), b"\x7fELF");
    assert_eq!(read_file(&ns, &ctx, "/etc/a").unwrap(), b"linked");
    let console = ns.resolve(&ctx, None, b"/dev/console", true).unwrap();
    let meta = ns.stat(&console).unwrap().metadata;
    assert_eq!(meta.kind, FileType::CharDevice);
    assert_eq!(meta.rdev, makedev(5, 1));
    assert_eq!(meta.rdev, 0x501);

    let etc = ns.resolve(&ctx, None, b"/etc", true).unwrap();
    let meta = ns.stat(&etc).unwrap().metadata;
    assert_eq!(meta.permissions, 0o750);
    assert_eq!(meta.mtime.tv_sec, 1_700_000_000);
    assert_eq!(ns.stat(&ctx.root).unwrap().metadata.permissions, 0o700);
}

#[test]
fn a_truncated_archive_is_an_error_not_a_partial_success() {
    let mut archive = Newc::new();
    archive.entry("file", 0o100_644, 1, 1, (0, 0), b"0123456789");
    let archive = archive.finish();
    let (ns, ctx) = fresh();
    assert!(matches!(
        initramfs::unpack(&ns, &ctx, &archive[..archive.len() - 130]),
        Err(initramfs::UnpackError::Archive(_))
    ));
}

// -- Pipes, poll and statfs --------------------------------------------------

use crate::pipe::{PIPE_BUF, PIPE_CAPACITY, PipeBuffer, ReadOutcome, WriteOutcome};

fn open_pipe(capacity: usize) -> PipeBuffer {
    let mut pipe = PipeBuffer::new(capacity);
    pipe.open_reader();
    pipe.open_writer();
    pipe
}

#[test]
fn a_drained_pipe_is_end_of_file_only_once_no_writer_is_left() {
    let mut pipe = open_pipe(PIPE_CAPACITY);
    let mut buf = [0_u8; 8];
    assert_eq!(pipe.read(&mut buf), ReadOutcome::WouldBlock);
    assert!(!pipe.read_readiness().readable);
    assert_eq!(pipe.write(b"last"), WriteOutcome::Wrote(4));
    pipe.close_writer();
    assert_eq!(pipe.read(&mut buf), ReadOutcome::Read(4));
    assert_eq!(&buf[..4], b"last");
    assert_eq!(pipe.read(&mut buf), ReadOutcome::EndOfFile);
    let ready = pipe.read_readiness();
    assert!(
        ready.readable && ready.hangup,
        "end of file must wake a poller"
    );
}

#[test]
fn a_write_with_no_reader_is_broken_and_polls_as_an_error() {
    let mut pipe = open_pipe(PIPE_CAPACITY);
    pipe.close_reader();
    assert_eq!(pipe.write(b"x"), WriteOutcome::Broken);
    let ready = pipe.write_readiness();
    assert!(ready.error && ready.writable);
}

#[test]
fn a_small_write_is_never_split_and_a_large_one_takes_what_fits() {
    let mut pipe = open_pipe(2 * PIPE_BUF);
    let big = vec![7_u8; PIPE_BUF + PIPE_BUF / 2];
    assert_eq!(pipe.write(&big), WriteOutcome::Wrote(big.len()));
    let small = vec![1_u8; PIPE_BUF];
    assert_eq!(
        pipe.write(&small),
        WriteOutcome::WouldBlock,
        "a write of PIPE_BUF bytes must not be split"
    );
    assert!(!pipe.write_readiness().writable);
    assert_eq!(pipe.write(&big), WriteOutcome::Wrote(PIPE_BUF / 2));
    assert_eq!(pipe.write(&big), WriteOutcome::WouldBlock);
    let mut out = vec![0_u8; 3 * PIPE_BUF];
    assert_eq!(pipe.read(&mut out), ReadOutcome::Read(2 * PIPE_BUF));
    assert!(pipe.write_readiness().writable);
}

#[test]
fn a_pipe_delivers_bytes_in_order_against_a_model() {
    let mut pipe = open_pipe(PIPE_CAPACITY);
    let mut model = alloc::collections::VecDeque::new();
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for _ in 0..4000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let len = usize::try_from((state >> 8) % 9000).unwrap();
        if state & 1 == 0 {
            let data: Vec<u8> = (0..len).map(|i| (state as usize + i) as u8).collect();
            match pipe.write(&data) {
                WriteOutcome::Wrote(n) => {
                    if data.len() <= PIPE_BUF {
                        assert_eq!(n, data.len(), "a small write was split");
                    }
                    model.extend(&data[..n]);
                }
                WriteOutcome::WouldBlock => {}
                WriteOutcome::Broken => panic!("a reader is open"),
            }
        } else {
            let mut buf = vec![0_u8; len];
            match pipe.read(&mut buf) {
                ReadOutcome::Read(n) => {
                    let expected: Vec<u8> = model.drain(..n).collect();
                    assert_eq!(&buf[..n], &expected[..], "bytes came out of order");
                }
                ReadOutcome::WouldBlock => assert!(model.is_empty()),
                ReadOutcome::EndOfFile => panic!("a writer is open"),
            }
        }
        assert_eq!(pipe.len(), model.len());
        assert!(pipe.len() <= PIPE_CAPACITY);
    }
}

#[test]
fn tmpfs_says_what_it_is_and_an_open_file_polls_for_its_access_mode() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/f", b"x");
    let stat = ns.statfs(&ctx.root);
    assert_eq!(stat.magic, crate::tmpfs::TMPFS_MAGIC);
    assert_eq!(stat.name_max, 255);
    assert_eq!(stat.files, 2, "the root and one file");
    let file = ns.open(&ctx, None, b"/f", &READ, 0).unwrap();
    let ready = file.poll();
    assert!(
        ready.readable && !ready.writable,
        "masked by the access mode"
    );
}

/// A directory whose names change without the VFS being told, as `/proc`'s
/// do when a process starts or exits.
#[derive(Debug, Default)]
struct Volatile {
    names: ferrix_sync::SpinLock<Vec<Vec<u8>>>,
}

impl Volatile {
    fn meta(ino: u64, kind: FileType) -> crate::Metadata {
        crate::Metadata {
            ino,
            kind,
            permissions: 0o555,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: 0,
            rdev: 0,
            blocks: 0,
            block_size: 4096,
            atime: Timespec::default(),
            mtime: Timespec::default(),
            ctime: Timespec::default(),
        }
    }
}

#[derive(Debug)]
struct VolatileFile;

impl crate::Inode for VolatileFile {
    fn metadata(&self) -> crate::Metadata {
        Volatile::meta(2, FileType::Regular)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }
}

impl crate::Inode for Volatile {
    fn metadata(&self) -> crate::Metadata {
        Volatile::meta(1, FileType::Directory)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn caches_lookups(&self) -> bool {
        false
    }

    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn crate::Inode>, Errno> {
        if self.names.lock().iter().any(|held| held == name) {
            Ok(Arc::new(VolatileFile))
        } else {
            Err(Errno::ENOENT)
        }
    }
}

#[derive(Debug)]
struct VolatileFs(Arc<Volatile>);

impl FileSystem for VolatileFs {
    fn root(&self) -> Arc<dyn crate::Inode> {
        Arc::clone(&self.0) as Arc<dyn crate::Inode>
    }

    fn name(&self) -> &'static str {
        "volatile"
    }

    fn device(&self) -> u64 {
        9
    }
}

#[test]
fn a_directory_that_does_not_cache_lookups_is_asked_every_time() {
    let (ns, ctx) = fresh();
    ns.mkdir(&ctx, None, b"/proc", 0o755).unwrap();
    let dir = Arc::new(Volatile::default());
    let at = ns.resolve(&ctx, None, b"/proc", true).unwrap();
    let _ = ns
        .mount(Arc::new(VolatileFs(Arc::clone(&dir))), &at)
        .unwrap();

    // A miss first, which a caching directory would remember.
    assert_eq!(
        ns.resolve(&ctx, None, b"/proc/42", true).err(),
        Some(Errno::ENOENT),
        "nothing is called 42 yet"
    );
    dir.names.lock().push(b"42".to_vec());
    let found = ns.resolve(&ctx, None, b"/proc/42", true).unwrap();
    assert_eq!(
        ns.path_of(&found, &ctx.root),
        b"/proc/42".to_vec(),
        "an uncached dentry still knows where it is"
    );

    // Then a hit, which must not outlive the name either.
    dir.names.lock().clear();
    assert_eq!(
        ns.resolve(&ctx, None, b"/proc/42", true).err(),
        Some(Errno::ENOENT),
        "a name that went away is gone at once"
    );
}

// -- Streams, detached locations and statfs layouts ------------------------

use core::sync::atomic::AtomicBool;

use crate::file::Status;
use crate::pipe::PIPEFS_MAGIC;
use crate::statfs::StatfsLayout;
use crate::{Location, StatFs};

fn stream_metadata() -> crate::Metadata {
    crate::Metadata {
        ino: 7,
        kind: FileType::Fifo,
        permissions: 0o600,
        nlink: 1,
        uid: 0,
        gid: 0,
        size: 0,
        rdev: 0,
        blocks: 0,
        block_size: 4096,
        atime: Timespec::default(),
        mtime: Timespec::default(),
        ctime: Timespec::default(),
    }
}

/// A stream that remembers whether its last call was told not to wait, and
/// refuses a read that was, as a pipe with nothing in it does.
#[derive(Debug, Default)]
struct Recorder {
    nonblock: AtomicBool,
}

impl crate::Inode for Recorder {
    fn metadata(&self) -> crate::Metadata {
        stream_metadata()
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }
    fn is_stream(&self) -> bool {
        true
    }
    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> Result<usize, Errno> {
        self.nonblock.store(nonblock, Ordering::Relaxed);
        if nonblock {
            return Err(Errno::EAGAIN);
        }
        buf.fill(b'r');
        Ok(buf.len())
    }
    fn write_stream(&self, data: &[u8], nonblock: bool) -> Result<usize, Errno> {
        self.nonblock.store(nonblock, Ordering::Relaxed);
        Ok(data.len())
    }
}

/// A stream that implements only the positioned calls, as the console does.
#[derive(Debug)]
struct Positioned;

impl crate::Inode for Positioned {
    fn metadata(&self) -> crate::Metadata {
        stream_metadata()
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }
    fn is_stream(&self) -> bool {
        true
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if offset != 0 {
            return Err(Errno::EINVAL);
        }
        buf.fill(b'p');
        Ok(buf.len())
    }
    fn write_at(&self, _offset: u64, data: &[u8], _append: bool) -> Result<(usize, u64), Errno> {
        Ok((data.len(), 0))
    }
}

#[derive(Debug)]
struct Pipes;

impl FileSystem for Pipes {
    fn root(&self) -> Arc<dyn crate::Inode> {
        Arc::new(Positioned)
    }
    fn name(&self) -> &'static str {
        "pipefs"
    }
    fn device(&self) -> u64 {
        99
    }
    fn statfs(&self) -> StatFs {
        StatFs {
            magic: PIPEFS_MAGIC,
            ..StatFs::default()
        }
    }
}

const READ_WRITE: OpenFlags = OpenFlags {
    write: true,
    ..READ
};

#[test]
fn a_stream_is_told_whether_its_open_file_may_wait() {
    let recorder = Arc::new(Recorder::default());
    let at = Location::detached(Arc::new(Pipes), recorder.clone(), b"pipe:[7]");
    let file = OpenFile::new(at, &READ_WRITE).unwrap();
    let mut buf = [0_u8; 4];
    assert_eq!(file.read(&mut buf), Ok(4));
    assert!(!recorder.nonblock.load(Ordering::Relaxed));

    file.set_status(Status {
        append: false,
        nonblock: true,
    });
    assert_eq!(
        file.read(&mut buf),
        Err(Errno::EAGAIN),
        "F_SETFL's O_NONBLOCK reaches the next read"
    );
    assert_eq!(file.write(b"xy"), Ok(2));
    assert!(recorder.nonblock.load(Ordering::Relaxed));
    assert_eq!(file.read_at(0, &mut buf), Err(Errno::ESPIPE));
    assert_eq!(file.seek(0, Whence::Current), Err(Errno::ESPIPE));

    // A stream that knows nothing of the flag keeps working through the
    // defaults, which is what leaves the console unchanged.
    let at = Location::detached(Arc::new(Pipes), Arc::new(Positioned), b"console");
    let console = OpenFile::new(at, &READ_WRITE).unwrap();
    console.set_status(Status {
        append: false,
        nonblock: true,
    });
    assert_eq!(console.read(&mut buf), Ok(4));
    assert_eq!(&buf, b"pppp");
    assert_eq!(console.write(b"out"), Ok(3));
}

#[test]
fn a_detached_location_opens_and_names_itself_without_a_tree() {
    let (ns, ctx) = fresh();
    let at = Location::detached(Arc::new(Pipes), Arc::new(Recorder::default()), b"pipe:[7]");
    assert!(at.is_detached() && !ctx.root.is_detached());
    assert_eq!(ns.path_of(&at, &ctx.root), b"pipe:[7]");
    assert_eq!(
        ns.stat(&at).unwrap().dev,
        99,
        "the device is its filesystem's"
    );
    assert_eq!(ns.statfs(&at).magic, PIPEFS_MAGIC);
    assert!(at.parent().same(&at), "`..` from nowhere stays there");
    assert_eq!(ns.unmount(&at), Err(Errno::EINVAL), "it is not mounted");
    let file = OpenFile::new(at, &READ).unwrap();
    assert_eq!(file.kind(), FileType::Fifo);
    assert_eq!(ns.path_of(file.location(), &ctx.root), b"pipe:[7]");
}

#[test]
fn with_io_sends_reads_elsewhere_and_keeps_what_stat_reports() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/f", b"abc");
    let file = ns.open(&ctx, None, b"/f", &READ, 0).unwrap();
    let mut buf = [0_u8; 3];
    assert_eq!(file.read(&mut buf), Ok(3));

    let recorder = Arc::new(Recorder::default());
    let swapped = file.with_io(recorder);
    assert!(Arc::ptr_eq(swapped.inode(), file.inode()));
    assert!(swapped.location().same(file.location()));
    assert!(swapped.readable() && !swapped.writable());
    assert_eq!(swapped.offset(), 0, "a new open starts at the beginning");
    assert_eq!(swapped.read(&mut buf), Ok(3));
    assert_eq!(&buf, b"rrr", "the read went to the new object");
    assert_eq!(
        swapped.write(b"no"),
        Err(Errno::EBADF),
        "the access mode came too"
    );
}

#[test]
fn a_blocked_pipe_end_waits_for_exactly_what_lets_it_proceed() {
    let mut pipe = open_pipe(2 * PIPE_BUF);
    assert!(!pipe.can_read());
    assert_eq!(
        pipe.write(&vec![0_u8; 2 * PIPE_BUF]),
        WriteOutcome::Wrote(2 * PIPE_BUF)
    );
    assert!(pipe.can_read());
    assert!(!pipe.can_write(1) && !pipe.can_write(PIPE_BUF + 1));
    assert!(pipe.can_write(0), "an empty write never waits");

    let mut one = [0_u8; 1];
    assert_eq!(pipe.read(&mut one), ReadOutcome::Read(1));
    assert!(pipe.can_write(1) && !pipe.can_write(2));
    assert_eq!(
        pipe.write(&[1, 2]),
        WriteOutcome::WouldBlock,
        "can_write agrees with write for a small write"
    );
    assert!(pipe.can_write(PIPE_BUF + 1), "a large write takes any room");
    pipe.close_reader();
    assert!(
        pipe.can_write(PIPE_BUF),
        "and wakes to find the pipe broken"
    );

    let mut empty = open_pipe(PIPE_CAPACITY);
    empty.close_writer();
    assert!(empty.can_read(), "and a reader wakes to end of file");
}

fn le32(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()))
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[test]
fn statfs_packs_each_layout_at_the_headers_offsets() {
    let stat = StatFs {
        magic: crate::tmpfs::TMPFS_MAGIC,
        block_size: 4096,
        blocks: 1000,
        blocks_free: 600,
        blocks_available: 500,
        files: 12,
        files_free: 3,
        name_max: 255,
    };
    let wide = StatfsLayout::Wide.encode(&stat).unwrap();
    assert_eq!(wide.len(), StatfsLayout::Wide.size());
    let fields = [0, 8, 16, 24, 32, 40, 48, 64, 72, 80].map(|at| le64(&wide, at));
    assert_eq!(
        fields,
        [0x0102_1994, 4096, 1000, 600, 500, 12, 3, 255, 4096, 0x20]
    );
    assert!(wide[56..64].iter().chain(&wide[88..]).all(|&b| b == 0));

    let narrow = StatfsLayout::Narrow.encode(&stat).unwrap();
    assert_eq!(narrow.len(), 64);
    let fields = [0, 4, 8, 12, 16, 20, 24, 36, 40, 44].map(|at| le32(&narrow, at));
    assert_eq!(
        fields,
        [0x0102_1994, 4096, 1000, 600, 500, 12, 3, 255, 4096, 0x20]
    );

    let packed = StatfsLayout::Packed64.encode(&stat).unwrap();
    assert_eq!(packed.len(), 84);
    assert_eq!([le32(&packed, 0), le32(&packed, 4)], [0x0102_1994, 4096]);
    let counts = [8, 16, 24, 32, 40].map(|at| le64(&packed, at));
    assert_eq!(counts, [1000, 600, 500, 12, 3]);
    let tail = [56, 60, 64].map(|at| le32(&packed, at));
    assert_eq!(tail, [255, 4096, 0x20]);
    assert!(packed[48..56].iter().chain(&packed[68..]).all(|&b| b == 0));

    let big = StatFs {
        blocks: 1 << 32,
        files: u64::MAX,
        ..stat
    };
    assert_eq!(StatfsLayout::Narrow.encode(&big), Err(Errno::EOVERFLOW));
    assert_eq!(
        le64(&StatfsLayout::Packed64.encode(&big).unwrap(), 8),
        1 << 32
    );
    let unlimited = StatFs {
        files: u64::MAX,
        ..stat
    };
    let narrow = StatfsLayout::Narrow.encode(&unlimited).unwrap();
    assert_eq!(le32(&narrow, 20), 0xFFFF_FFFF, "-1 passes at either width");
    assert_eq!(StatfsLayout::native(8), StatfsLayout::Wide);
    assert_eq!(StatfsLayout::native(4), StatfsLayout::Narrow);
}

#[test]
fn growing_a_file_never_shrinks_it_and_needs_it_open_for_writing() {
    let (ns, ctx) = fresh();
    write_file(&ns, &ctx, "/f", b"0123456789");
    let file = ns.open(&ctx, None, b"/f", &RW_CREATE, 0o644).unwrap();
    file.grow_to(4).unwrap();
    assert_eq!(
        file.inode().metadata().size,
        10,
        "growing to less is no change"
    );
    file.grow_to(5000).unwrap();
    let grown = read_file(&ns, &ctx, "/f").unwrap();
    assert_eq!(grown.len(), 5000);
    assert_eq!(&grown[..12], b"0123456789\0\0", "what it uncovers is zeros");
    let reader = ns.open(&ctx, None, b"/f", &READ, 0).unwrap();
    assert_eq!(reader.grow_to(9000), Err(Errno::EINVAL));
    let dir = ns.open(&ctx, None, b"/", &READ, 0).unwrap();
    assert_eq!(dir.grow_to(1), Err(Errno::EINVAL));
}

// -- Races, made to happen on one thread -------------------------------------

/// When a [`Meddling`] directory runs its hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moment {
    /// After the filesystem has answered a lookup, before the VFS records it.
    Lookup,
    /// After the VFS decided a name is missing, before the filesystem creates it.
    Create,
}

/// Something another thread could have done, run at a chosen moment.
type Hook = alloc::boxed::Box<dyn FnOnce() + Send>;

/// At most one armed hook, shared by every inode of a meddling filesystem.
#[derive(Default)]
struct Hooks {
    armed: ferrix_sync::SpinLock<Option<(Moment, Vec<u8>, Hook)>>,
}

impl core::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

impl Hooks {
    fn arm(&self, moment: Moment, name: &[u8], hook: impl FnOnce() + Send + 'static) {
        *self.armed.lock() = Some((moment, name.to_vec(), alloc::boxed::Box::new(hook)));
    }

    /// Run the hook if it is armed for this moment and name, once, with the
    /// lock released so that the hook may use the filesystem.
    fn fire(&self, moment: Moment, name: &[u8]) {
        let hook = {
            let mut armed = self.armed.lock();
            match armed.take() {
                Some((at, armed_name, hook)) if at == moment && armed_name == name => Some(hook),
                other => {
                    *armed = other;
                    None
                }
            }
        };
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// A tmpfs inode that lets a test interleave another operation with a lookup
/// or a create, which is how a second thread gets in on a real machine.
#[derive(Debug)]
struct Meddling {
    inner: Arc<dyn crate::Inode>,
    hooks: Arc<Hooks>,
}

impl Meddling {
    fn wrap(&self, inner: Arc<dyn crate::Inode>) -> Arc<dyn crate::Inode> {
        Arc::new(Meddling {
            inner,
            hooks: Arc::clone(&self.hooks),
        })
    }
}

impl crate::Inode for Meddling {
    fn metadata(&self) -> crate::Metadata {
        self.inner.metadata()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8], append: bool) -> Result<(usize, u64), Errno> {
        self.inner.write_at(offset, data, append)
    }

    fn lookup(&self, name: &[u8]) -> Result<Arc<dyn crate::Inode>, Errno> {
        let found = self.inner.lookup(name);
        self.hooks.fire(Moment::Lookup, name);
        found.map(|inner| self.wrap(inner))
    }

    fn create(
        &self,
        name: &[u8],
        node: crate::NewNode<'_>,
        permissions: u32,
    ) -> Result<Arc<dyn crate::Inode>, Errno> {
        self.hooks.fire(Moment::Create, name);
        self.inner
            .create(name, node, permissions)
            .map(|inner| self.wrap(inner))
    }

    fn unlink(&self, name: &[u8]) -> Result<(), Errno> {
        self.inner.unlink(name)
    }

    fn rmdir(&self, name: &[u8]) -> Result<(), Errno> {
        self.inner.rmdir(name)
    }

    fn rename(
        &self,
        old: &[u8],
        new_parent: &Arc<dyn crate::Inode>,
        new: &[u8],
        replace: bool,
    ) -> Result<(), Errno> {
        let new_parent = Arc::clone(new_parent)
            .into_any()
            .downcast::<Meddling>()
            .map_err(|_| Errno::EXDEV)?;
        self.inner.rename(old, &new_parent.inner, new, replace)
    }

    fn read_dir(
        &self,
        cursor: u64,
        emit: &mut dyn FnMut(crate::DirEntry<'_>) -> bool,
    ) -> Result<(), Errno> {
        self.inner.read_dir(cursor, emit)
    }
}

#[derive(Debug)]
struct MeddlingFs(Arc<Meddling>);

impl FileSystem for MeddlingFs {
    fn root(&self) -> Arc<dyn crate::Inode> {
        Arc::clone(&self.0) as Arc<dyn crate::Inode>
    }

    fn name(&self) -> &'static str {
        "meddling"
    }

    fn device(&self) -> u64 {
        1
    }
}

/// A namespace over a meddling tmpfs keeping `cache` unused dentries, and the
/// hooks that meddle with it.
fn meddling(cache: usize) -> (Arc<Namespace>, Arc<Hooks>) {
    let hooks = Arc::new(Hooks::default());
    let root = Arc::new(Meddling {
        inner: tmpfs(1).root(),
        hooks: Arc::clone(&hooks),
    });
    let ns = Namespace::with_cache(Arc::new(MeddlingFs(root)), cache);
    (Arc::new(ns), hooks)
}

const DIRECTORY: OpenFlags = OpenFlags {
    directory: true,
    ..READ
};

#[test]
fn a_lookup_that_loses_a_race_does_not_let_a_directory_move_inside_itself() {
    // No cache, so that every walk to /A/X asks the filesystem again.
    let (ns, hooks) = meddling(0);
    let ctx = ns.context();
    ns.mkdir(&ctx, None, b"/A", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/A/X", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/B", 0o755).unwrap();

    // While /A/X is being looked up, a name in /A changes, as it would if
    // another thread created a file there.
    let other = Arc::clone(&ns);
    hooks.arm(Moment::Lookup, b"X", move || {
        let ctx = other.context();
        other
            .mknod(&ctx, None, b"/A/junk", crate::NewNode::Fifo, 0o644)
            .unwrap();
    });
    let x = ns.open(&ctx, None, b"/A/X", &DIRECTORY, 0).unwrap();
    let again = ns.resolve(&ctx, None, b"/A/X", true).unwrap();
    assert!(
        Arc::ptr_eq(&x.location().dentry, &again.dentry),
        "one directory has two dentries"
    );
    drop(again);

    ns.rename(
        &ctx,
        (None, b"/A/X"),
        (None, b"/B/X"),
        RenameMode::Replace,
    )
    .unwrap();
    assert_eq!(ns.path_of(x.location(), &ctx.root), b"/B/X");
    assert_eq!(
        ns.rename(
            &ctx,
            (None, b"/B"),
            (Some(x.location()), b"sub"),
            RenameMode::Replace
        ),
        Err(Errno::EINVAL),
        "/B was moved into its own child"
    );
    assert_eq!(kind(&ns, &ctx, "/B/X", true), Ok(FileType::Directory));
}

#[test]
#[cfg_attr(
    miri,
    ignore = "threads under Miri take minutes, and the test above is deterministic"
)]
fn a_directory_has_one_dentry_while_its_parent_churns() {
    extern crate std;

    let ns = Arc::new(Namespace::with_cache(tmpfs(1), 0));
    let ctx = ns.context();
    ns.mkdir(&ctx, None, b"/A", 0o755).unwrap();
    ns.mkdir(&ctx, None, b"/A/X", 0o755).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let churn = {
        let ns = Arc::clone(&ns);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let ctx = ns.context();
            while !stop.load(Ordering::Relaxed) {
                let _ = ns.mknod(&ctx, None, b"/A/junk", crate::NewNode::Fifo, 0o644);
                let _ = ns.unlink(&ctx, None, b"/A/junk");
            }
        })
    };
    let started = std::time::Instant::now();
    let mut split = 0_u32;
    while started.elapsed() < core::time::Duration::from_millis(300) {
        let x = ns.open(&ctx, None, b"/A/X", &DIRECTORY, 0).unwrap();
        let again = ns.resolve(&ctx, None, b"/A/X", true).unwrap();
        if !Arc::ptr_eq(&x.location().dentry, &again.dentry) {
            split += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    churn.join().unwrap();
    assert_eq!(split, 0, "a walk handed out a second dentry for /A/X");
}

#[test]
fn tmpfs_refuses_to_move_a_directory_into_itself_whatever_the_vfs_checked() {
    let fs = tmpfs(1);
    let root = fs.root();
    let a = root.create(b"a", crate::NewNode::Directory, 0o755).unwrap();
    let b = a.create(b"b", crate::NewNode::Directory, 0o755).unwrap();
    assert_eq!(root.rename(b"a", &b, b"inside", true), Err(Errno::EINVAL));
    assert_eq!(root.rename(b"a", &a, b"itself", true), Err(Errno::EINVAL));
    assert!(root.lookup(b"a").is_ok(), "a refused move moved nothing");

    // A move that is not into itself still works, and afterwards the moved
    // directory's new ancestors are the ones checked.
    let c = root.create(b"c", crate::NewNode::Directory, 0o755).unwrap();
    root.rename(b"c", &b, b"c", true).unwrap();
    assert_eq!(b.rename(b"c", &c, b"x", true), Err(Errno::EINVAL));
    assert_eq!(root.rename(b"a", &c, b"a", true), Err(Errno::EINVAL));
}

#[test]
fn open_create_without_excl_opens_a_file_created_under_it() {
    let (ns, hooks) = meddling(crate::DEFAULT_CACHE);
    let ctx = ns.context();
    let append = OpenFlags {
        append: true,
        ..RW_CREATE
    };

    // Two `echo >> log` on a new file: the other one creates it between this
    // one's walk finding nothing and its create.
    let other = Arc::clone(&ns);
    hooks.arm(Moment::Create, b"log", move || {
        let ctx = other.context();
        let file = other.open(&ctx, None, b"/log", &append, 0o644).unwrap();
        assert_eq!(file.write(b"first\n"), Ok(6));
    });
    let file = ns
        .open(&ctx, None, b"/log", &append, 0o644)
        .expect("O_CREAT without O_EXCL is not EEXIST");
    assert_eq!(file.write(b"second\n"), Ok(7));
    assert_eq!(read_file(&ns, &ctx, "/log").unwrap(), b"first\nsecond\n");

    // With O_EXCL the loser is told, as it must be.
    let other = Arc::clone(&ns);
    hooks.arm(Moment::Create, b"lock", move || {
        let ctx = other.context();
        let _ = other.open(&ctx, None, b"/lock", &RW_CREATE, 0o644).unwrap();
    });
    let exclusive = OpenFlags {
        exclusive: true,
        ..RW_CREATE
    };
    assert_eq!(
        ns.open(&ctx, None, b"/lock", &exclusive, 0o644).unwrap_err(),
        Errno::EEXIST
    );
}
