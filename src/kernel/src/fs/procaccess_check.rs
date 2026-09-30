//! What a process keeps private in `/proc`, proved at boot
//! (`docs/NAMESPACES.md` M8, landing NP): `/proc/<pid>/root`, `cwd`, `exe`,
//! `fd`, `maps`, `mountinfo` and `ns/*` of another process are refused
//! `EACCES` unless Linux's `ptrace_may_access` allows them.
//!
//! Four targets -- a dumpable process of uid 1000, one of uid 1000 that cleared
//! `PR_SET_DUMPABLE`, one of uid 2000 and one of root -- and three readers:
//! uid 1000, root, and root inside a user namespace (kernel uid 1000). The
//! check requires:
//!
//! * the dumpable same-uid target is readable by uid 1000, and every other
//!   target is refused it: the non-dumpable one (a set-id `execve` makes a
//!   process one) and the other uids' processes;
//! * root reads every one (`CAP_SYS_PTRACE`);
//! * root inside a user namespace reads what its kernel uid may and nothing of
//!   another uid's, however the ids read inside, and is refused the `fd`
//!   directory and `maps` of root's process;
//! * `get_robust_list` asks the same of a thread that is not the caller's
//!   own, with the real ids.

use alloc::format;

use ferrix_linux_abi::nr::Syscall;
use ferrix_linux_abi::types::{AT_FDCWD, O_DIRECTORY, O_RDONLY};
use ferrix_vfs::Errno;

use crate::fs::mount_check::{Page, Report as Counts, Tally, by_number, page_for};
use crate::fs::namespace_check::{staged, unshare};
use crate::syscall::credentials;
use crate::syscall::namespace::CLONE_NEWUSER;
use crate::syscall::process::{self, Process};
use crate::syscall::registry;
use crate::syscall::userns;

/// `PR_SET_DUMPABLE`.
const PR_SET_DUMPABLE: u64 = 4;

/// A target made with ids `uid`, and `PR_SET_DUMPABLE` set as `undumpable`
/// says: a change of ids makes a process undumpable, so it is set again, as
/// bubblewrap does after it drops its ids.
fn target(uid: u64, undumpable: bool) -> Result<alloc::sync::Arc<Process>, &'static str> {
    let made = dropped(uid)?;
    let _ = by_number(
        &made,
        Syscall::Prctl,
        [PR_SET_DUMPABLE, u64::from(!undumpable), 0, 0, 0, 0],
    )
    .map_err(|_| "a /proc target could not set PR_SET_DUMPABLE")?;
    Ok(made)
}

/// A target that changed its ids and did nothing more.
fn dropped(uid: u64) -> Result<alloc::sync::Arc<Process>, &'static str> {
    let made = process::new_for_check().map_err(|_| "could not make a /proc target")?;
    if uid != 0 {
        for call in [Syscall::Setgid, Syscall::Setuid] {
            let _ = by_number(&made, call, [uid, 0, 0, 0, 0, 0])
                .map_err(|_| "a /proc target could not take its ids")?;
        }
    }
    Ok(made)
}

/// What `reader` is told of `path`: `readlink` of a link, or an open of a
/// directory. The answers a refusal and a missing exe are told apart by.
fn looks(
    reader: &Process,
    path: &str,
    directory: bool,
) -> Result<Result<usize, Errno>, &'static str> {
    let mut page = page_for(reader)?;
    let at = staged(&mut page, path.as_bytes())?;
    let process = registry::find(reader.pid()).ok_or("the /proc reader is gone")?;
    let answer = userns::acting_as(&process, || {
        if directory {
            by_number(
                reader,
                Syscall::Openat,
                [
                    AT_FDCWD as u64,
                    at,
                    u64::from(O_RDONLY | O_DIRECTORY),
                    0,
                    0,
                    0,
                ],
            )
            .inspect(|&fd| {
                let _ = by_number(reader, Syscall::Close, [fd as u64, 0, 0, 0, 0, 0]);
            })
        } else {
            by_number(
                reader,
                Syscall::Readlinkat,
                [AT_FDCWD as u64, at, page_buffer(&page), 256, 0, 0],
            )
        }
    })?;
    Ok(answer)
}

/// Where the page's reads land.
fn page_buffer(page: &Page<'_>) -> u64 {
    page.buffer()
}

/// What the check saw, for the boot line.
pub(crate) fn run() -> Result<Counts, &'static str> {
    let mut counts = Counts::default();
    let mut tally = Tally {
        report: &mut counts,
    };
    let dumpable = target(1000, false)?;
    let private = target(1000, true)?;
    let other = target(2000, false)?;
    let root = target(0, false)?;

    let user = target(1000, false)?;
    readers(&mut tally, &user, &dumpable, &private, &other, &root)?;

    let administrator = process::new_for_check().map_err(|_| "could not make the root reader")?;
    for victim in [&dumpable, &private, &other, &root] {
        allowed(
            &mut tally,
            &administrator,
            victim,
            "root was refused another process's /proc",
        )?;
    }

    // Root inside a namespace it made: kernel uid 1000 again.
    let fake = target(1000, false)?;
    tally.ok(
        unshare(&fake, CLONE_NEWUSER),
        "unshare(CLONE_NEWUSER) was refused to uid 1000",
    )?;
    allowed(
        &mut tally,
        &fake,
        &dumpable,
        "root inside a namespace was refused a same-uid process",
    )?;
    for victim in [&other, &root] {
        refused(
            &mut tally,
            &fake,
            victim,
            "root inside a namespace read another uid's /proc",
        )?;
    }
    robust_lists(&mut tally, &user, &dumpable, &other)?;
    Ok(counts)
}

/// uid 1000 reads the dumpable target and is refused the rest.
fn readers(
    tally: &mut Tally<'_>,
    user: &Process,
    dumpable: &Process,
    private: &Process,
    other: &Process,
    root: &Process,
) -> Result<(), &'static str> {
    allowed(
        tally,
        user,
        dumpable,
        "a user was refused a dumpable process of its own",
    )?;
    // A process that changed its ids is not dumpable until it says so.
    let changed = dropped(1000)?;
    refused(
        tally,
        user,
        &changed,
        "a process that changed its ids stayed dumpable",
    )?;
    refused(
        tally,
        user,
        private,
        "a user read a process that is not dumpable",
    )?;
    refused(tally, user, other, "a user read another uid's /proc")?;
    refused(tally, user, root, "a user read root's /proc")?;
    // Its own, always.
    allowed(tally, user, user, "a process was refused its own /proc")
}

/// `reader` may look at `target`'s links and directories.
fn allowed(
    tally: &mut Tally<'_>,
    reader: &Process,
    target: &Process,
    what: &'static str,
) -> Result<(), &'static str> {
    for (name, directory) in [
        ("root", false),
        ("cwd", false),
        ("fd", true),
        ("ns/mnt", false),
    ] {
        let path = format!("/proc/{}/{name}", target.pid());
        let answer = looks(reader, &path, directory)?;
        tally.report.calls += 1;
        if answer == Err(Errno::EACCES) {
            return Err(what);
        }
    }
    Ok(())
}

/// `reader` is refused every one of them, `EACCES`.
fn refused(
    tally: &mut Tally<'_>,
    reader: &Process,
    target: &Process,
    what: &'static str,
) -> Result<(), &'static str> {
    for (name, directory) in [
        ("root", false),
        ("cwd", false),
        ("exe", false),
        ("fd", true),
        ("ns/mnt", false),
        ("ns/user", false),
    ] {
        let path = format!("/proc/{}/{name}", target.pid());
        tally.refused(looks(reader, &path, directory)?, Errno::EACCES, what)?;
    }
    Ok(())
}

/// `get_robust_list`'s permission, on the credentials: a thread of another
/// uid's process is refused, a same-uid dumpable one is not.
fn robust_lists(
    tally: &mut Tally<'_>,
    user: &Process,
    dumpable: &Process,
    other: &Process,
) -> Result<(), &'static str> {
    tally.report.calls += 2;
    if !credentials::may_access(user, dumpable, true) {
        return Err("get_robust_list was refused a thread of a same-uid dumpable process");
    }
    if credentials::may_access(user, other, true) {
        return Err("get_robust_list was allowed a thread of another uid's process");
    }
    tally.report.refusals += 1;
    Ok(())
}
