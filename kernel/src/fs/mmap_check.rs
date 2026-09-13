//! Stage 8's self-check of shared file mappings.
//!
//! A shared mapping of a tmpfs file maps the file's own pages, the VMO `read`
//! copies out of, so a write through the mapping is what the file's next read
//! returns and a write to the file is what the mapping shows, with no copy in
//! between to go stale. The check drives that as a program would, from a
//! process built for it: `openat` and `mmap` go in by the handlers, the
//! mapping is read and written through user memory so that every touch takes
//! the fault path, and the file is read and written through its open
//! description.
//!
//! Around that, `mmap` must refuse a file as Linux does, `msync` must answer
//! as Linux does, `/proc/<pid>/maps` must name the mapping by the file's path,
//! and a truncation must take the pages past the new end away from the
//! mapping, which a copy from the kernel then finds refused. Like the other
//! stage 8 checks it runs twice and counts frames across the second run.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{
    AT_FDCWD, MAP_ANONYMOUS, MAP_PRIVATE, MAP_SHARED, MS_ASYNC, MS_SYNC, O_CREAT, O_RDONLY, O_RDWR,
    O_TRUNC, PROT_READ, PROT_WRITE,
};
use ferrix_vfs::{Errno, OpenFlags};

use crate::fs;
use crate::mm;
use crate::syscall::check as syscall_check;
use crate::syscall::memory::{self, MmapRequest, OffsetUnit};
use crate::syscall::process::{self, Process};
use crate::syscall::{fd, uaccess};

/// The working directory, as `openat` takes it.
const CWD: u64 = AT_FDCWD as i64 as u64;

/// The file the check maps, with the terminator a program's copy carries.
const PATH: &[u8] = b"/tmp/ferrix-mmap-check\0";
/// Where [`PATH`] is staged in the check's page.
const AT_PATH: u64 = 0;
/// A directory, which has nothing to map.
const TMP: &[u8] = b"/tmp\0";
/// Where [`TMP`] is staged.
const AT_TMP: u64 = 64;

/// How many pages the shared mapping covers: the file's three, and one past
/// its end.
const MAPPED_PAGES: u64 = 4;

/// Bytes written through the mapping and looked for in the file.
const THROUGH_MAP: &[u8] = b"written through a shared mapping";
/// Bytes written to the file and looked for in the mapping.
const THROUGH_FILE: &[u8] = b"written to the file";

/// What the check measured, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// Bytes compared between the mapping and the file, both ways.
    pub(crate) bytes: u64,
    /// Pages of the mapping a truncation took away.
    pub(crate) cut: u64,
    /// Frames the second run cost. Zero, or a mapping kept a file's pages.
    pub(crate) leaked: i64,
}

/// Run it twice, measured on the second: the first grows the heap to where it
/// stays.
pub(crate) fn run() -> Result<Report, &'static str> {
    let process =
        process::new_for_check().map_err(|_| "could not make a process for the mmap check")?;
    let _warm = check_once(&process)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let before = mm::free_frames();
    let (bytes, cut) = check_once(&process)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let leaked = i64::try_from(before).unwrap_or(i64::MAX)
        - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);
    if leaked != 0 {
        crate::console::println!("  mmap     {leaked} frames across the second run");
    }
    if leaked < 0 {
        return Err(
            "the free frame count rose across the mmap check: something outside it freed frames in the window",
        );
    }
    if leaked > 0 {
        return Err("the mmap check kept frames it did not give back");
    }
    Ok(Report { bytes, cut, leaked })
}

/// A byte of the file's contents, by its offset.
fn pattern(at: u64) -> u8 {
    (at.wrapping_mul(29) ^ (at >> 12)) as u8
}

/// An `mmap` of `len` bytes of `descriptor` from its start.
fn request(descriptor: i32, len: u64, prot: u32, flags: u32) -> MmapRequest {
    MmapRequest {
        addr: 0,
        len,
        prot,
        flags,
        fd: i64::from(descriptor),
        offset: 0,
        unit: OffsetUnit::Bytes,
    }
}

/// Make `call` by its number, as a program on this architecture would.
fn by_number(process: &Process, call: Syscall, args: [u64; 6]) -> Result<usize, Errno> {
    syscall_check::call_by_number(process, call, args)
}

/// A descriptor a handler returned.
fn descriptor(got: Result<usize, Errno>, what: &'static str) -> Result<i32, &'static str> {
    got.ok().and_then(|fd| i32::try_from(fd).ok()).ok_or(what)
}

/// An address a handler returned.
fn address(got: Result<usize, Errno>, what: &'static str) -> Result<u64, &'static str> {
    got.ok().and_then(|at| u64::try_from(at).ok()).ok_or(what)
}

/// Open [`PATH`] with `flags` by number, from the staged page.
fn open(process: &Process, page: u64, flags: u32) -> Result<i32, &'static str> {
    descriptor(
        by_number(
            process,
            Syscall::Openat,
            [CWD, page + AT_PATH, u64::from(flags), 0o600, 0, 0],
        ),
        "the mmap check's file would not open",
    )
}

/// One run: stage the paths, check, and clean up whatever happened.
fn check_once(process: &Process) -> Result<(u64, u64), &'static str> {
    let page = address(
        memory::sys_mmap(
            process,
            &request(
                -1,
                PAGE_SIZE,
                PROT_READ | PROT_WRITE,
                MAP_ANONYMOUS | MAP_PRIVATE,
            ),
        ),
        "a page for the mmap check was refused",
    )?;
    let outcome = [(AT_PATH, PATH), (AT_TMP, TMP)]
        .into_iter()
        .try_for_each(|(at, bytes)| uaccess::copy_to_user(process.space(), page + at, bytes))
        .map_err(|_| "could not stage the mmap check")
        .and_then(|()| check_a_shared_file_mapping(process, page));

    for descriptor in 3..32 {
        let _ = fd::sys_close(process, descriptor);
    }
    let _ = memory::sys_munmap(process, page, PAGE_SIZE);
    let ns = fs::namespace();
    let _ = ns.unlink(
        &ns.context(),
        None,
        PATH.strip_suffix(b"\0").unwrap_or(PATH),
    );
    outcome
}

/// Steps 1 to 5, on a file of two pages and a hundred bytes.
fn check_a_shared_file_mapping(process: &Process, page: u64) -> Result<(u64, u64), &'static str> {
    let rw = open(process, page, O_RDWR | O_CREAT | O_TRUNC)?;
    let file = fd::file(process, rw).map_err(|_| "the mmap check's descriptor names nothing")?;
    let len = 2 * PAGE_SIZE + 100;
    let data: Vec<u8> = (0..len).map(pattern).collect();
    if file.write_at(0, &data) != Ok(data.len()) {
        return Err("the mmap check's file was not written whole");
    }

    check_mmap_refuses_as_linux_does(process, page, rw)?;
    let at = address(
        memory::sys_mmap(
            process,
            &request(
                rw,
                MAPPED_PAGES * PAGE_SIZE,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
            ),
        ),
        "a shared mapping of a tmpfs file was refused",
    )?;
    let outcome = check_the_mapping_is_the_file(process, &file, at, &data)
        .and_then(|bytes| check_msync_and_maps(process, at).map(|()| bytes))
        .and_then(|bytes| {
            check_a_truncation_reaches_the_mapping(process, &file, at).map(|cut| (bytes, cut))
        });
    if memory::sys_munmap(process, at, MAPPED_PAGES * PAGE_SIZE) != Ok(0) {
        return Err("a shared file mapping would not unmap");
    }
    outcome
}

/// Step 1: `mmap` of a file refuses in Linux's order and for Linux's reasons.
fn check_mmap_refuses_as_linux_does(
    process: &Process,
    page: u64,
    rw: i32,
) -> Result<(), &'static str> {
    let refused = |got: Result<usize, Errno>, errno: Errno, what| {
        if got == Err(errno) { Ok(()) } else { Err(what) }
    };
    refused(
        memory::sys_mmap(process, &request(29, PAGE_SIZE, PROT_READ, MAP_SHARED)),
        Errno::EBADF,
        "mmap of a descriptor that names nothing was not EBADF",
    )?;
    let directory = descriptor(
        by_number(
            process,
            Syscall::Openat,
            [CWD, page + AT_TMP, u64::from(O_RDONLY), 0, 0, 0],
        ),
        "/tmp would not open",
    )?;
    refused(
        memory::sys_mmap(
            process,
            &request(directory, PAGE_SIZE, PROT_READ, MAP_SHARED),
        ),
        Errno::ENODEV,
        "mmap of a directory was not ENODEV",
    )?;
    let read_only = open(process, page, O_RDONLY)?;
    refused(
        memory::sys_mmap(
            process,
            &request(read_only, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED),
        ),
        Errno::EACCES,
        "a writable shared mapping of a file opened read-only was not EACCES",
    )?;
    refused(
        memory::sys_mmap(
            process,
            &request(rw, PAGE_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE),
        ),
        Errno::ENODEV,
        "a private file mapping was not ENODEV, which it is until it can copy on write",
    )?;
    let readable = address(
        memory::sys_mmap(
            process,
            &request(read_only, PAGE_SIZE, PROT_READ, MAP_SHARED),
        ),
        "a read-only shared mapping of a file opened read-only was refused",
    )?;
    if memory::sys_munmap(process, readable, PAGE_SIZE) != Ok(0) {
        return Err("a read-only file mapping would not unmap");
    }
    Ok(())
}

/// Step 2: the mapping shows the file, a write through it reaches the file,
/// and a write to the file reaches it. Returns the bytes compared.
fn check_the_mapping_is_the_file(
    process: &Process,
    file: &ferrix_vfs::OpenFile,
    at: u64,
    data: &[u8],
) -> Result<u64, &'static str> {
    let space = process.space();
    let mut shown = vec![0_u8; data.len()];
    uaccess::copy_from_user(space, at, &mut shown)
        .map_err(|_| "a shared file mapping could not be read")?;
    if shown != data {
        return Err("a shared file mapping does not show the file's bytes");
    }
    let tail_len = usize::try_from(3 * PAGE_SIZE).unwrap_or(0) - data.len();
    let mut tail = vec![0xff_u8; tail_len];
    uaccess::copy_from_user(space, at + data.len() as u64, &mut tail)
        .map_err(|_| "the rest of a file's last page could not be read through its mapping")?;
    if tail.iter().any(|&byte| byte != 0) {
        return Err("the rest of a file's last page is not zeros in its mapping");
    }

    uaccess::copy_to_user(space, at + PAGE_SIZE + 7, THROUGH_MAP)
        .map_err(|_| "a write through a shared file mapping was refused")?;
    let mut back = vec![0_u8; THROUGH_MAP.len()];
    if file.read_at(PAGE_SIZE + 7, &mut back) != Ok(back.len()) || back != THROUGH_MAP {
        return Err("a write through a shared mapping was not what the file read back");
    }

    if file.write_at(50, THROUGH_FILE) != Ok(THROUGH_FILE.len()) {
        return Err("a write to a mapped file failed");
    }
    let mut seen = vec![0_u8; THROUGH_FILE.len()];
    uaccess::copy_from_user(space, at + 50, &mut seen)
        .map_err(|_| "a mapped file's page could not be read again")?;
    if seen != THROUGH_FILE {
        return Err("a write to a mapped file was not what its mapping showed");
    }
    Ok((data.len() + tail.len() + THROUGH_MAP.len() + THROUGH_FILE.len()) as u64)
}

/// Step 3: `msync` answers as Linux does, and `/proc/<pid>/maps` names the
/// mapping by its file.
fn check_msync_and_maps(process: &Process, at: u64) -> Result<(), &'static str> {
    let len = MAPPED_PAGES * PAGE_SIZE;
    if memory::sys_msync(process, at, len, MS_SYNC) != Ok(0)
        || memory::sys_msync(process, at, len, MS_SYNC | MS_ASYNC) != Err(Errno::EINVAL)
        || memory::sys_msync(process, at + 1, PAGE_SIZE, MS_SYNC) != Err(Errno::EINVAL)
    {
        return Err("msync did not answer a whole mapping, or refuse bad flags, as Linux does");
    }
    let gone = address(
        memory::sys_mmap(
            process,
            &request(-1, PAGE_SIZE, PROT_READ, MAP_ANONYMOUS | MAP_PRIVATE),
        ),
        "a page for msync's refusal was refused",
    )?;
    let _ = memory::sys_munmap(process, gone, PAGE_SIZE);
    if memory::sys_msync(process, gone, PAGE_SIZE, MS_SYNC) != Err(Errno::ENOMEM) {
        return Err("msync of an unmapped range was not ENOMEM");
    }

    let ns = fs::namespace();
    let maps_path = format!("/proc/{}/maps", process.pid());
    let read = OpenFlags {
        read: true,
        ..OpenFlags::default()
    };
    let maps = ns
        .open(&ns.context(), None, maps_path.as_bytes(), &read, 0)
        .map_err(|_| "the check process's maps would not open")?;
    let mut text = Vec::new();
    let mut chunk = [0_u8; 256];
    loop {
        match maps.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => text.extend_from_slice(chunk.get(..count).unwrap_or_default()),
            Err(_) => return Err("the check process's maps would not read"),
        }
    }
    let name = PATH.strip_suffix(b"\0").unwrap_or(PATH);
    let named = text
        .split(|&byte| byte == b'\n')
        .any(|line| line.ends_with(name) && line.windows(5).any(|w| w == b"rw-s "));
    if !named {
        return Err("/proc/<pid>/maps does not name a shared file mapping by its file");
    }
    Ok(())
}

/// Step 4: a truncation takes the pages past the new end away from the
/// mapping, and a grow shows zeros where they were. Returns the pages cut.
fn check_a_truncation_reaches_the_mapping(
    process: &Process,
    file: &ferrix_vfs::OpenFile,
    at: u64,
) -> Result<u64, &'static str> {
    let space = process.space();
    let mut byte = [0_u8; 1];
    file.set_len(PAGE_SIZE)
        .map_err(|_| "a mapped file would not truncate")?;
    if uaccess::copy_from_user(space, at, &mut byte).is_err() {
        return Err("a mapped file's first page was refused after a cut behind it");
    }
    let cut = (1..MAPPED_PAGES)
        .filter(|&index| uaccess::copy_from_user(space, at + index * PAGE_SIZE, &mut byte).is_err())
        .count() as u64;
    if cut != MAPPED_PAGES - 1 {
        return Err("a page past a mapped file's new end could still be read through the mapping");
    }

    file.set_len(2 * PAGE_SIZE)
        .map_err(|_| "a mapped file would not grow again")?;
    let mut regrown = vec![0xff_u8; usize::try_from(PAGE_SIZE).unwrap_or(0)];
    uaccess::copy_from_user(space, at + PAGE_SIZE, &mut regrown)
        .map_err(|_| "a regrown page could not be read through the mapping")?;
    if regrown.iter().any(|&b| b != 0) {
        return Err("a page cut and grown again showed its old bytes through the mapping");
    }
    Ok(cut)
}
