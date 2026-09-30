//! `seccomp(2)`, proved at boot: a filter is checked when it is installed,
//! judges each call afterwards, and is inherited.
//!
//! The check installs filters through the system call, as a program's
//! `seccomp` and `prctl` would, and judges calls through [`seccomp::enforce`],
//! the function every Linux call passes through on its way in. It requires:
//!
//! * a process that has not set `no_new_privs` and holds no capability is
//!   refused a filter, `EACCES`; one that has set it is not;
//! * a filter Linux refuses is refused `EINVAL`: empty, a byte load, a jump
//!   off its end, no final return; and a flag Ferrix cannot honour;
//! * a filter judges a call by its number: the call it names is failed with
//!   its errno, every other passes;
//! * of two filters the most restrictive answer wins, and on a tie the newer
//!   filter's data;
//! * a fork child is judged as its parent was, and a filter installed after
//!   does not reach the parent;
//! * an action that kills ends the process, strict mode allows `read` and
//!   `write` and kills at anything else;
//! * `SECCOMP_GET_ACTION_AVAIL` knows the actions Ferrix can take and refuses
//!   the two it cannot (`EOPNOTSUPP`).

use alloc::vec::Vec;

use ferrix_linux_abi::nr::Syscall;
use ferrix_seccomp::{ALLOW, ERRNO, Insn, KILL_PROCESS};
use ferrix_vfs::Errno;

use crate::fs::mount_check::{Page, Report as Counts, Tally, by_number, page_for};
use crate::syscall::process::{self, Process};
use crate::syscall::seccomp;
use crate::trap::{Abi, SyscallArgs};

/// A call number no Linux call has, which the check's filters name.
const BLOCKED: u32 = 12_345;
/// Another, which a filter kills for.
const FORBIDDEN: u32 = 12_346;

/// `LD | W | ABS`, `JMP | JEQ | K` and `RET | K`.
const LOAD: u16 = 0x20;
/// See [`LOAD`].
const JUMP_IF_EQUAL: u16 = 0x15;
/// See [`LOAD`].
const RETURN: u16 = 0x06;

/// `SECCOMP_SET_MODE_FILTER`.
const SET_MODE_FILTER: u64 = 1;
/// `SECCOMP_SET_MODE_STRICT`.
const SET_MODE_STRICT: u64 = 0;
/// `SECCOMP_GET_ACTION_AVAIL`.
const GET_ACTION_AVAIL: u64 = 2;
/// `PR_SET_NO_NEW_PRIVS`.
const PR_SET_NO_NEW_PRIVS: u64 = 38;

/// The ids the unprivileged process runs as.
const UID: u64 = 1000;

/// What the check saw, for the boot line.
pub(crate) fn run() -> Result<Counts, &'static str> {
    let mut counts = Counts::default();
    let mut tally = Tally {
        report: &mut counts,
    };
    installing(&mut tally)?;
    judging(&mut tally)?;
    killing(&mut tally)?;
    Ok(counts)
}

/// The program "if nr == `number` return `action`; return ALLOW".
fn program(number: u32, action: u32) -> Vec<Insn> {
    Vec::from([
        Insn::new(LOAD, 0, 0, 0),
        Insn::new(JUMP_IF_EQUAL, 0, 1, number),
        Insn::new(RETURN, 0, 0, action),
        Insn::new(RETURN, 0, 0, ALLOW),
    ])
}

/// Install `insns` in the page's process through `seccomp(2)`.
fn install(
    page: &mut Page<'_>,
    insns: &[Insn],
    flags: u64,
) -> Result<Result<usize, Errno>, &'static str> {
    page.reset();
    let mut bytes = Vec::new();
    for insn in insns {
        bytes.extend_from_slice(&insn.code.to_le_bytes());
        bytes.push(insn.jt);
        bytes.push(insn.jf);
        bytes.extend_from_slice(&insn.k.to_le_bytes());
    }
    let filter = page.put_bytes(&bytes)?;
    // A `struct sock_fprog`: a 16-bit length and a pointer, which is at 8 for
    // a 64-bit program and at 4 for a 32-bit one.
    let mut fprog = Vec::new();
    fprog.extend_from_slice(&(insns.len() as u16).to_le_bytes());
    if size_of::<usize>() == 8 {
        fprog.extend_from_slice(&[0; 6]);
        fprog.extend_from_slice(&filter.to_le_bytes());
    } else {
        fprog.extend_from_slice(&[0; 2]);
        fprog.extend_from_slice(&(filter as u32).to_le_bytes());
    }
    let fprog = page.put_bytes(&fprog)?;
    Ok(by_number(
        page.process,
        Syscall::Seccomp,
        [SET_MODE_FILTER, flags, fprog, 0, 0, 0],
    ))
}

/// A call numbered `number`, as a program's entry would hand it over.
fn call(number: u32) -> SyscallArgs {
    SyscallArgs {
        number: number as usize,
        args: [1, 2, 3, 4, 5, 6],
        abi: Abi::Native,
    }
}

/// Whether `enforce` lets `number` through for `process`, or what it answers.
fn judged(process: &Process, number: u32) -> Option<isize> {
    seccomp::enforce(process, Syscall::Getppid, &call(number), 0x1000).map(
        |outcome| match outcome {
            crate::trap::Outcome::Return(value) => value,
            _ => isize::MAX,
        },
    )
}

/// Require `got` to be `want`, counting a refusal when `want` is one.
fn expect(
    tally: &mut Tally<'_>,
    got: Result<usize, Errno>,
    want: Result<usize, Errno>,
    what: &'static str,
) -> Result<(), &'static str> {
    tally.report.calls += 1;
    if want.is_err() {
        tally.report.refusals += 1;
    }
    if got == want { Ok(()) } else { Err(what) }
}

/// The rules of installing.
fn installing(tally: &mut Tally<'_>) -> Result<(), &'static str> {
    let user =
        process::new_for_check().map_err(|_| "could not make the seccomp check's process")?;
    let mut page = page_for(&user)?;
    tally.ok(
        by_number(&user, Syscall::Setgid, [UID, 0, 0, 0, 0, 0]),
        "the seccomp check's process could not become gid 1000",
    )?;
    tally.ok(
        by_number(&user, Syscall::Setuid, [UID, 0, 0, 0, 0, 0]),
        "the seccomp check's process could not become uid 1000",
    )?;
    tally.refused(
        install(&mut page, &program(BLOCKED, ERRNO | 1), 0)?,
        Errno::EACCES,
        "an unprivileged process without no_new_privs was given a filter",
    )?;
    tally.ok(
        by_number(&user, Syscall::Prctl, [PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0, 0]),
        "no_new_privs could not be set",
    )?;
    // What Linux refuses of a program: empty, a byte load, a jump off the end,
    // no final return.
    let byte_load = Vec::from([Insn::new(0x30, 0, 0, 0), Insn::new(RETURN, 0, 0, ALLOW)]);
    let jump_off = Vec::from([
        Insn::new(JUMP_IF_EQUAL, 1, 0, 0),
        Insn::new(RETURN, 0, 0, ALLOW),
    ]);
    let no_return = Vec::from([Insn::new(LOAD, 0, 0, 0)]);
    for (bad, what) in [
        (Vec::new(), "an empty filter was accepted"),
        (byte_load, "a filter that loads a byte was accepted"),
        (jump_off, "a filter that jumps off its end was accepted"),
        (no_return, "a filter with no final return was accepted"),
    ] {
        tally.refused(install(&mut page, &bad, 0)?, Errno::EINVAL, what)?;
    }
    tally.refused(
        install(&mut page, &program(BLOCKED, ERRNO | 1), 1 << 3)?,
        Errno::EINVAL,
        "SECCOMP_FILTER_FLAG_NEW_LISTENER was accepted",
    )?;
    tally.refused(
        install(&mut page, &program(BLOCKED, ERRNO | 1), 1 << 40)?,
        Errno::EINVAL,
        "an unknown seccomp flag was accepted",
    )?;
    // What Ferrix can take as an action, and what it cannot.
    page.reset();
    for (action, want) in [
        (ALLOW, Ok(0)),
        (ERRNO, Ok(0)),
        (KILL_PROCESS, Ok(0)),
        (ferrix_seccomp::TRACE, Err(Errno::EOPNOTSUPP)),
        (ferrix_seccomp::USER_NOTIF, Err(Errno::EOPNOTSUPP)),
    ] {
        let at = page.put_bytes(&action.to_le_bytes())?;
        expect(
            tally,
            by_number(&user, Syscall::Seccomp, [GET_ACTION_AVAIL, 0, at, 0, 0, 0]),
            want,
            "SECCOMP_GET_ACTION_AVAIL did not know an action as Ferrix takes it",
        )?;
    }
    Ok(())
}

/// A filter judges by the call's number; two judge by precedence; a fork
/// child is judged as its parent.
fn judging(tally: &mut Tally<'_>) -> Result<(), &'static str> {
    let parent = process::new_for_check().map_err(|_| "could not make the filtered process")?;
    let mut page = page_for(&parent)?;
    // Root holds `CAP_SYS_ADMIN`, so no_new_privs is not needed.
    if judged(&parent, BLOCKED).is_some() {
        return Err("a process with no filter was judged");
    }
    tally.ok(
        install(&mut page, &program(BLOCKED, ERRNO | 1), 0)?,
        "root could not install a filter",
    )?;
    if judged(&parent, BLOCKED) != Some(Errno::EPERM.as_return_value()) {
        return Err("a filter did not fail the call it names with its errno");
    }
    if judged(&parent, 1).is_some() {
        return Err("a filter failed a call it does not name");
    }
    // A second, newer filter with the same action and other data: the tie goes
    // to the newer.
    tally.ok(
        install(&mut page, &program(BLOCKED, ERRNO | 13), 0)?,
        "a second filter could not be installed",
    )?;
    if judged(&parent, BLOCKED) != Some(Errno::EACCES.as_return_value()) {
        return Err("on a tie between two filters the older one's data stood");
    }
    // A third kills for another number; the more restrictive action wins.
    let child =
        process::fork_for_check(&parent).map_err(|_| "could not fork the filtered process")?;
    if judged(&child, BLOCKED) != Some(Errno::EACCES.as_return_value()) {
        return Err("a fork child was not judged as its parent was");
    }
    let mut child_page = page_for(&child)?;
    tally.ok(
        install(&mut child_page, &program(BLOCKED, KILL_PROCESS), 0)?,
        "a fork child could not add a filter",
    )?;
    if judged(&parent, BLOCKED) != Some(Errno::EACCES.as_return_value()) {
        return Err("a filter installed by a child reached its parent");
    }
    Ok(())
}

/// A killing action ends the process; strict mode allows `read`, `write` and
/// nothing else.
fn killing(tally: &mut Tally<'_>) -> Result<(), &'static str> {
    let doomed = process::new_for_check().map_err(|_| "could not make the doomed process")?;
    let mut page = page_for(&doomed)?;
    tally.ok(
        install(&mut page, &program(FORBIDDEN, KILL_PROCESS), 0)?,
        "a killing filter could not be installed",
    )?;
    if judged(&doomed, 1).is_some() || doomed.is_terminated() {
        return Err("a killing filter killed for a call it does not name");
    }
    let _ = judged(&doomed, FORBIDDEN);
    if !doomed.is_terminated() {
        return Err("SECCOMP_RET_KILL_PROCESS did not end the process");
    }

    let strict = process::new_for_check().map_err(|_| "could not make the strict process")?;
    tally.ok(
        by_number(&strict, Syscall::Seccomp, [SET_MODE_STRICT, 0, 0, 0, 0, 0]),
        "strict mode could not be set",
    )?;
    for allowed in [Syscall::Read, Syscall::Write] {
        if seccomp::enforce(&strict, allowed, &call(0), 0).is_some() {
            return Err("strict mode refused read or write");
        }
    }
    let _ = seccomp::enforce(&strict, Syscall::Getppid, &call(110), 0);
    if !strict.is_terminated() {
        return Err("strict mode did not end a process that made another call");
    }
    Ok(())
}
