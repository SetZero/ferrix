//! Stage 8's self-check of `memfd_create` and its seals.
//!
//! A memfd is a tmpfs file nothing names, so it reads, writes, truncates and
//! maps as one does. What is new is the seals, and what they refuse is checked
//! in the order a Wayland compositor meets them: a file made to allow sealing
//! takes a shrink seal and refuses a truncation downwards; a grow seal refuses
//! extending it by truncation or by a write. A write seal is refused with
//! `EBUSY` while any shared mapping may write the file -- a `fork` child's
//! copy of one included -- and once it stands, writes, shared writable
//! mappings and `mprotect` to writable are refused, while a private mapping
//! still maps and keeps its writes to itself. `F_SEAL_SEAL` then refuses every
//! further seal.
//!
//! Beside the seals: an unknown flag and an overlong name are `EINVAL`, a file
//! made without `MFD_ALLOW_SEALING` carries `F_SEAL_SEAL`, and a read-only
//! shared mapping of a file opened read-only may not be made writable, the gap
//! the may-write accounting closed. Like the other stage 8 checks it runs
//! twice and counts frames across the second run, in a frame window.

use alloc::vec;

use ferrix_bootinfo::PAGE_SIZE;
use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{
    AT_FDCWD, F_ADD_SEALS, F_GET_SEALS, F_SEAL_FUTURE_WRITE, F_SEAL_GROW, F_SEAL_SEAL,
    F_SEAL_SHRINK, F_SEAL_WRITE, MAP_ANONYMOUS, MAP_PRIVATE, MAP_SHARED, MFD_ALLOW_SEALING,
    MFD_CLOEXEC, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, PROT_READ, PROT_WRITE,
};
use ferrix_vfs::Errno;

use crate::fs;
use crate::mm;
use crate::syscall::check as syscall_check;
use crate::syscall::memory::{self, MmapRequest, OffsetUnit};
use crate::syscall::process::{self, Process};
use crate::syscall::{fd, uaccess};

/// A memfd's name, staged in the check's page.
const NAME: &[u8] = b"ferrix-seals\0";
/// Where [`NAME`] is staged.
const AT_NAME: u64 = 0;
/// A name one byte past the longest `memfd_create` takes, staged after it.
const AT_LONG: u64 = 64;
/// The longest name `memfd_create` takes.
const NAME_MAX_LEN: usize = 249;
/// A regular file for the read-only mapping check, staged in the page.
const PATH: &[u8] = b"/tmp/ferrix-memfd-check\0";
/// Where [`PATH`] is staged.
const AT_PATH: u64 = 512;
/// A seal bit no kernel defines.
const UNKNOWN_SEAL: u32 = 0x100;
/// `MFD_HUGETLB`, which has no hugetlbfs behind it here.
const MFD_HUGETLB: u32 = 0x0004;

/// What the check measured, for the boot log.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Report {
    /// Seals added and then enforced.
    pub(crate) seals: u32,
    /// Calls refused as Linux refuses them.
    pub(crate) refusals: u32,
    /// Frames the second run cost.
    pub(crate) leaked: i64,
}

/// Run it twice, measured on the second.
pub(crate) fn run() -> Result<Report, &'static str> {
    let process =
        process::new_for_check().map_err(|_| "could not make a process for the memfd check")?;
    let _warm = check_once(&process)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let window = mm::FrameWindow::open();
    let (seals, refusals) = check_once(&process)?;
    crate::sched::wait_until_reaper_quiet(crate::sched::REAPER_PATIENCE_NANOS)?;
    let leaked = window.kept();
    if leaked != 0 {
        window.report("memfd");
        crate::console::println!("  memfd    {leaked} frames across the second run");
        return Err("the memfd check did not give back every frame it took");
    }
    Ok(Report {
        seals,
        refusals,
        leaked,
    })
}

/// Make `call` by its number, as a program would.
fn by_number(process: &Process, call: Syscall, args: [u64; 6]) -> Result<usize, Errno> {
    syscall_check::call_by_number(process, call, args)
}

/// A descriptor a call returned, as a register.
fn descriptor(got: Result<usize, Errno>, what: &'static str) -> Result<u64, &'static str> {
    got.ok().map(|fd| fd as u64).ok_or(what)
}

/// Require `got` to be `Err(wanted)`, counting the refusal.
fn refused(
    got: Result<usize, Errno>,
    wanted: Errno,
    what: &'static str,
    refusals: &mut u32,
) -> Result<(), &'static str> {
    if got != Err(wanted) {
        return Err(what);
    }
    *refusals += 1;
    Ok(())
}

/// `fcntl(fd, F_ADD_SEALS, seals)`.
fn add_seals(process: &Process, fd: u64, seals: u32) -> Result<usize, Errno> {
    by_number(
        process,
        Syscall::Fcntl,
        [fd, u64::from(F_ADD_SEALS), u64::from(seals), 0, 0, 0],
    )
}

/// `fcntl(fd, F_GET_SEALS)`.
fn get_seals(process: &Process, fd: u64) -> Result<usize, Errno> {
    by_number(
        process,
        Syscall::Fcntl,
        [fd, u64::from(F_GET_SEALS), 0, 0, 0, 0],
    )
}

/// An `mmap` of `len` bytes of `fd` from its start.
fn map(process: &Process, fd: u64, len: u64, prot: u32, flags: u32) -> Result<usize, Errno> {
    memory::sys_mmap(
        process,
        &MmapRequest {
            addr: 0,
            len,
            prot,
            flags,
            fd: fd as i64,
            offset: 0,
            unit: OffsetUnit::Bytes,
        },
    )
}

/// One run: stage the names, check, and close and unlink whatever it made.
fn check_once(process: &Process) -> Result<(u32, u32), &'static str> {
    let page = memory::sys_mmap(
        process,
        &MmapRequest {
            addr: 0,
            len: PAGE_SIZE,
            prot: PROT_READ | PROT_WRITE,
            flags: MAP_ANONYMOUS | MAP_PRIVATE,
            fd: -1,
            offset: 0,
            unit: OffsetUnit::Bytes,
        },
    )
    .map(|at| at as u64)
    .map_err(|_| "a page for the memfd check was refused")?;
    let long = vec![b'n'; NAME_MAX_LEN + 1];
    let outcome = uaccess::copy_to_user(process.space(), page + AT_NAME, NAME)
        .and_then(|()| uaccess::copy_to_user(process.space(), page + AT_LONG, &long))
        .and_then(|()| {
            uaccess::copy_to_user(process.space(), page + AT_LONG + long.len() as u64, &[0])
        })
        .and_then(|()| uaccess::copy_to_user(process.space(), page + AT_PATH, PATH))
        .map_err(|_| "could not stage the memfd check")
        .and_then(|()| check_memfd(process, page));

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

/// The whole check on one staged page. Returns the seals enforced and the
/// refusals counted.
fn check_memfd(process: &Process, page: u64) -> Result<(u32, u32), &'static str> {
    let mut refusals = 0;
    let create = |name: u64, flags: u32| {
        by_number(
            process,
            Syscall::MemfdCreate,
            [page + name, u64::from(flags), 0, 0, 0, 0],
        )
    };
    refused(
        create(AT_NAME, MFD_HUGETLB),
        Errno::EINVAL,
        "memfd_create with MFD_HUGETLB was not EINVAL",
        &mut refusals,
    )?;
    refused(
        create(AT_LONG, 0),
        Errno::EINVAL,
        "memfd_create with a 250-byte name was not EINVAL",
        &mut refusals,
    )?;

    let plain = descriptor(
        create(AT_NAME, MFD_CLOEXEC),
        "memfd_create refused a plain memfd",
    )?;
    if get_seals(process, plain) != Ok(F_SEAL_SEAL as usize) {
        return Err("a memfd made without MFD_ALLOW_SEALING does not carry F_SEAL_SEAL");
    }
    refused(
        add_seals(process, plain, F_SEAL_SHRINK),
        Errno::EPERM,
        "a seal on a memfd that does not allow sealing was not EPERM",
        &mut refusals,
    )?;

    let sealed = descriptor(
        create(AT_NAME, MFD_ALLOW_SEALING),
        "memfd_create refused a sealable memfd",
    )?;
    if get_seals(process, sealed) != Ok(0) {
        return Err("a memfd made with MFD_ALLOW_SEALING starts with seals");
    }
    let seals = check_size_seals(process, sealed, &mut refusals)?
        + check_the_write_seal(process, sealed, &mut refusals)?;
    refused(
        add_seals(process, sealed, UNKNOWN_SEAL),
        Errno::EINVAL,
        "an unknown seal was not EINVAL",
        &mut refusals,
    )?;
    if add_seals(process, sealed, F_SEAL_SEAL) != Ok(0) {
        return Err("F_SEAL_SEAL was refused");
    }
    refused(
        add_seals(process, sealed, F_SEAL_FUTURE_WRITE),
        Errno::EPERM,
        "a seal after F_SEAL_SEAL was not EPERM",
        &mut refusals,
    )?;
    check_a_read_only_mapping_stays_read_only(process, page, &mut refusals)?;
    Ok((seals + 1, refusals))
}

/// The shrink and grow seals, on a memfd of three pages.
fn check_size_seals(
    process: &Process,
    sealed: u64,
    refusals: &mut u32,
) -> Result<u32, &'static str> {
    let truncate = |len: u64| by_number(process, Syscall::Ftruncate, [sealed, len, 0, 0, 0, 0]);
    if truncate(3 * PAGE_SIZE) != Ok(0) {
        return Err("a sealable memfd would not grow");
    }
    let file =
        fd::file(process, fd::arg(sealed)).map_err(|_| "a memfd's descriptor names nothing")?;
    if file.write_at(0, b"sealed contents") != Ok(15) {
        return Err("a sealable memfd could not be written");
    }

    if add_seals(process, sealed, F_SEAL_SHRINK) != Ok(0) {
        return Err("F_SEAL_SHRINK was refused");
    }
    refused(
        truncate(PAGE_SIZE),
        Errno::EPERM,
        "a truncation downwards past F_SEAL_SHRINK was not EPERM",
        refusals,
    )?;
    if truncate(4 * PAGE_SIZE) != Ok(0) {
        return Err("a truncation upwards was refused by a shrink seal");
    }

    if add_seals(process, sealed, F_SEAL_GROW) != Ok(0) {
        return Err("F_SEAL_GROW was refused");
    }
    refused(
        truncate(5 * PAGE_SIZE),
        Errno::EPERM,
        "a truncation upwards past F_SEAL_GROW was not EPERM",
        refusals,
    )?;
    if file.write_at(4 * PAGE_SIZE - 2, b"past") != Err(Errno::EPERM) {
        return Err("a write extending a file past F_SEAL_GROW was not EPERM");
    }
    *refusals += 1;
    if file.write_at(PAGE_SIZE, b"inside") != Ok(6) {
        return Err("a write inside a grow-sealed memfd was refused");
    }
    Ok(2)
}

/// The write seal: refused while a shared mapping may write the file, a fork
/// child's copy included, and enforced once it stands.
fn check_the_write_seal(
    process: &Process,
    sealed: u64,
    refusals: &mut u32,
) -> Result<u32, &'static str> {
    let len = PAGE_SIZE;
    let at = map(process, sealed, len, PROT_READ | PROT_WRITE, MAP_SHARED)
        .map_err(|_| "a shared writable mapping of a memfd was refused")? as u64;
    refused(
        add_seals(process, sealed, F_SEAL_WRITE),
        Errno::EBUSY,
        "F_SEAL_WRITE with a writable shared mapping alive was not EBUSY",
        refusals,
    )?;
    if get_seals(process, sealed).map(|seals| seals as u32 & F_SEAL_WRITE) != Ok(0) {
        return Err("a refused F_SEAL_WRITE was left standing");
    }

    // A fork child's copy of the mapping counts on its own.
    let child = process
        .space()
        .fork()
        .map_err(|_| "the memfd check's space would not fork")?;
    if memory::sys_munmap(process, at, len) != Ok(0) {
        return Err("a memfd mapping would not unmap");
    }
    refused(
        add_seals(process, sealed, F_SEAL_WRITE),
        Errno::EBUSY,
        "F_SEAL_WRITE with only a fork child's writable mapping alive was not EBUSY",
        refusals,
    )?;
    drop(child);
    if add_seals(process, sealed, F_SEAL_WRITE) != Ok(0) {
        return Err("F_SEAL_WRITE was refused once no shared mapping could write the file");
    }

    let file =
        fd::file(process, fd::arg(sealed)).map_err(|_| "a memfd's descriptor names nothing")?;
    if file.write_at(0, b"x") != Err(Errno::EPERM) {
        return Err("a write past F_SEAL_WRITE was not EPERM");
    }
    *refusals += 1;
    refused(
        map(process, sealed, len, PROT_READ | PROT_WRITE, MAP_SHARED),
        Errno::EPERM,
        "a shared writable mapping of a write-sealed memfd was not EPERM",
        refusals,
    )?;
    let shown = map(process, sealed, len, PROT_READ, MAP_SHARED)
        .map_err(|_| "a shared read-only mapping of a write-sealed memfd was refused")?
        as u64;
    let outcome = refused(
        memory::sys_mprotect(process, shown, len, PROT_READ | PROT_WRITE),
        Errno::EACCES,
        "mprotect to writable of a write-sealed memfd's shared mapping was not EACCES",
        refusals,
    );
    let _ = memory::sys_munmap(process, shown, len);
    outcome?;

    let private = map(process, sealed, len, PROT_READ | PROT_WRITE, MAP_PRIVATE)
        .map_err(|_| "a private mapping of a write-sealed memfd was refused")?
        as u64;
    let written = uaccess::copy_to_user(process.space(), private, b"private");
    let mut kept = [0_u8; 7];
    let read = file.read_at(0, &mut kept);
    let _ = memory::sys_munmap(process, private, len);
    written.map_err(|_| "a write into a private mapping of a write-sealed memfd was refused")?;
    if read != Ok(kept.len()) || &kept == b"private" {
        return Err("a private mapping's write reached a write-sealed memfd");
    }
    Ok(1)
}

/// The gap the may-write accounting closed: a shared read-only mapping of a
/// file opened read-only may not be made writable.
fn check_a_read_only_mapping_stays_read_only(
    process: &Process,
    page: u64,
    refusals: &mut u32,
) -> Result<(), &'static str> {
    let cwd = AT_FDCWD as i64 as u64;
    let open = |flags: u32| {
        by_number(
            process,
            Syscall::Openat,
            [cwd, page + AT_PATH, u64::from(flags), 0o600, 0, 0],
        )
    };
    let writer = descriptor(
        open(O_RDWR | O_CREAT | O_TRUNC),
        "the memfd check's file would not open",
    )?;
    let file = fd::file(process, fd::arg(writer)).map_err(|_| "the check's file names nothing")?;
    if file.write_at(0, b"read only") != Ok(9) {
        return Err("the memfd check's file could not be written");
    }
    let reader = descriptor(
        open(O_RDONLY),
        "the memfd check's file would not open read-only",
    )?;
    let at = map(process, reader, PAGE_SIZE, PROT_READ, MAP_SHARED)
        .map_err(|_| "a shared read-only mapping of a file opened read-only was refused")?
        as u64;
    let outcome = refused(
        memory::sys_mprotect(process, at, PAGE_SIZE, PROT_READ | PROT_WRITE),
        Errno::EACCES,
        "mprotect made a shared mapping of a file opened read-only writable",
        refusals,
    );
    let _ = memory::sys_munmap(process, at, PAGE_SIZE);
    outcome
}
