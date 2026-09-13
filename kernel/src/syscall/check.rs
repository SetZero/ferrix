//! Stage 7's self-checks: the dispatch path, on the real architecture.
//!
//! These do something the host tests in `libs/linux-abi` cannot, and it is the
//! whole reason they exist. That crate checks all three number tables against
//! each other; what it cannot check is *which one this kernel was built to
//! use*. A build that reached for the wrong table would pass every host test
//! and then answer a program's `write` with `unlink`, and nothing short of
//! running on the machine can tell the difference.
//!
//! So the checks below assert the identity of the table by its content: they
//! ask for a number that means one thing on this architecture and something
//! else, or nothing, on the other two.
//!
//! The second thing they establish is that the path is total. A trap vector
//! has nowhere to report a failure to — a program is sitting on the other end
//! of it — so `dispatch` has to end in a value for every input, including the
//! numbers no table has.

use alloc::sync::Arc;
use alloc::vec::Vec;

use ferrix_bootinfo::{KERNEL_HALF_BASE, PAGE_SIZE};
use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{
    AT_FDCWD, F_DUPFD, F_DUPFD_CLOEXEC, F_GETFD, F_GETFL, F_SETFD, F_SETFL, FD_CLOEXEC,
    MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, MREMAP_FIXED, MREMAP_MAYMOVE, O_APPEND,
    O_CLOEXEC, O_CREAT, O_EXCL, O_RDONLY, O_RDWR, O_TRUNC, PROT_READ, PROT_WRITE, SEEK_CUR,
    SEEK_END, SEEK_SET, TCGETS, TIOCGWINSZ,
};

use crate::arch;
use crate::mm;
use ferrix_elf::Class;

use crate::syscall::memory::{self, MmapRequest, OffsetUnit};
use crate::syscall::process::{self, Process};
use crate::syscall::{Outcome, SyscallArgs, dispatch, uaccess};
use crate::syscall::{exec, fd, file, image, load, signal, system};

/// What the checks measured, for the boot log.
#[derive(Debug)]
pub(crate) struct Report {
    /// Numbers put through `dispatch`, across every check.
    pub(crate) dispatched: u32,
    /// How many of them were answered rather than refused.
    pub(crate) answered: u32,
    /// The architecture's number for `getpid`, printed so the boot log says
    /// which table this build actually used rather than asserting it silently.
    pub(crate) getpid_number: usize,
    /// Pages the handler checks mapped, wrote through and gave back.
    pub(crate) pages: u64,
    /// Frames the whole check cost once everything was dropped. Zero, or a
    /// handler is leaking.
    pub(crate) leaked: i64,
    /// The status a program run in user mode exited with, if this
    /// architecture can run one yet.
    pub(crate) user_status: Option<i32>,
    /// How many times each of two programs sharing one processor was switched
    /// to. Both at least twice, or they ran one after the other.
    pub(crate) concurrent: Option<(u64, u64)>,
    /// The status a spinning program reported after being killed from outside.
    pub(crate) killed: Option<i32>,
    /// What a program that forks and waits exited with: 24 when right.
    pub(crate) forked: Option<i32>,
    /// What a program that signals itself exited with once its handler had
    /// run and returned: 77 when right.
    pub(crate) signalled: Option<i32>,
    /// What a program exited with that `execve`d a program which exists, then
    /// one that does not: 42 and 2 when right.
    pub(crate) execed: Option<(i32, i32)>,
    /// Processes the pid registry numbered, found, listed and let go.
    pub(crate) pids: u32,
    /// Futex waiters a wake or a requeue roused: 2 when right.
    pub(crate) futex_woken: usize,
}

/// Run them. `Err` names the first thing that was not true.
pub(crate) fn run() -> Result<Report, &'static str> {
    let mut counter = Counter::default();

    let getpid_number = check_the_right_table_was_compiled_in(&mut counter)?;
    check_identity_answers(&mut counter)?;
    let pids = crate::syscall::registry::check()?;
    check_an_unknown_number_is_enosys(&mut counter)?;
    check_the_whole_number_space_is_total(&mut counter)?;
    check_errors_encode_as_negative(&mut counter)?;
    check_a_call_needing_a_process_says_so(&mut counter)?;

    // Everything the handler checks allocate must come back. Measured around
    // the whole group rather than per check, so a leak anywhere in it shows.
    //
    // **Run twice, and measured on the second run.** The first run is warm-up
    // and its cost is not a leak: the kernel heap keeps the last page of each
    // size class it has used, deliberately, so that a workload oscillating
    // across a page boundary does not pay a buddy allocation per cycle. A
    // check that allocates a size nothing else does will therefore take a page
    // from the allocator and keep it, exactly once, and a single measured run
    // cannot tell that apart from a leak. The second run allocates the same
    // shapes into the pages the first left behind, so anything it fails to
    // return is real.
    //
    // This was not a hypothesis. A one-frame discrepancy appeared when the
    // loader checks landed and survived every attempt to find it in the
    // address space -- map, commit and drop in isolation was clean, and so was
    // building the image in isolation.
    let _warm = check_handlers(Output::Quiet)?;
    let before = mm::free_frames();
    let pages = check_handlers(Output::Show)?;
    let leaked = i64::try_from(before).unwrap_or(i64::MAX)
        - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);
    // Checked, not only printed: a count nothing tested would boot green
    // through the very leak it exists to show.
    if leaked != 0 {
        return Err("the handler checks did not give every frame back");
    }

    let user_status = check_a_program_runs_in_user_mode()?;
    let concurrent = check_two_programs_take_turns_on_one_processor()?;
    let killed = check_a_program_is_killed_from_outside()?;
    let forked = check_a_forked_child_is_waited_for()?;
    let signalled = check_a_handler_runs_and_returns()?;
    check_an_ended_process_closes_its_descriptors()?;
    let execed = check_execve_replaces_the_program()?;
    let futex_woken = check_futexes()?;

    Ok(Report {
        dispatched: counter.dispatched,
        answered: counter.answered,
        getpid_number,
        pages,
        leaked,
        user_status,
        concurrent,
        killed,
        forked,
        signalled,
        execed,
        pids,
        futex_woken,
    })
}

/// A call that needs an address space is refused, rather than faulting.
///
/// Today every such call takes this path, because nothing creates a process
/// yet. When stage 6's transition lands, this check keeps meaning something:
/// a kernel thread making a system call must still be told no rather than
/// dereferencing a `None`.
fn check_a_call_needing_a_process_says_so(counter: &mut Counter) -> Result<(), &'static str> {
    let Some(number) = number_for(ferrix_linux_abi::nr::Syscall::Brk) else {
        return Err("this architecture has no number for brk");
    };
    let esrch = Outcome::Return(Errno::ESRCH.as_return_value());
    if counter.call(number) != esrch {
        return Err("a call needing a process was not refused with ESRCH");
    }
    Ok(())
}

/// Counts what went through, so the report is a measurement and not a claim.
#[derive(Default)]
struct Counter {
    dispatched: u32,
    answered: u32,
}

impl Counter {
    /// Dispatch one number with no arguments, and count it.
    fn call(&mut self, number: usize) -> Outcome {
        self.call_with(number, [0; 6])
    }

    /// Dispatch one number with arguments, and count it.
    fn call_with(&mut self, number: usize, args: [u64; 6]) -> Outcome {
        let outcome = dispatch(&SyscallArgs { number, args }, None);
        self.dispatched = self.dispatched.saturating_add(1);
        if let Outcome::Return(value) = outcome
            && value >= 0
        {
            self.answered = self.answered.saturating_add(1);
        }
        outcome
    }
}

/// The number this architecture gives `getpid`, and proof it is that one.
///
/// `getpid` is the probe because all three tables have it and no two agree:
/// 39 on x86-64, 172 on AArch64, 20 on ARMv7-A. Asking the facade for its own
/// number and then requiring the *other two* numbers to mean something else is
/// what makes this a check rather than a tautology.
fn check_the_right_table_was_compiled_in(counter: &mut Counter) -> Result<usize, &'static str> {
    let candidates = [
        ferrix_linux_abi::nr::x86_64::GETPID,
        ferrix_linux_abi::nr::aarch64::GETPID,
        ferrix_linux_abi::nr::arm::GETPID,
    ];

    let mut mine = None;
    for number in candidates {
        if arch::decode_syscall(number) == Some(ferrix_linux_abi::nr::Syscall::Getpid) {
            if mine.is_some() {
                return Err("two different numbers both decode to getpid");
            }
            mine = Some(number);
        }
    }
    let Some(number) = mine else {
        return Err("no table's getpid number decodes to getpid on this build");
    };

    // And it answers, rather than merely decoding.
    match counter.call(number) {
        Outcome::Return(value) if value > 0 => Ok(number),
        _ => Err("getpid decoded but did not answer with a process identifier"),
    }
}

/// The calls that need no process state answer, and agree with each other.
fn check_identity_answers(counter: &mut Counter) -> Result<(), &'static str> {
    let uid_calls = [
        ferrix_linux_abi::nr::Syscall::Getuid,
        ferrix_linux_abi::nr::Syscall::Geteuid,
        ferrix_linux_abi::nr::Syscall::Getgid,
        ferrix_linux_abi::nr::Syscall::Getegid,
    ];
    for call in uid_calls {
        let Some(number) = number_for(call) else {
            return Err("this architecture has no number for a credential call");
        };
        if counter.call(number) != Outcome::Return(0) {
            return Err("a credential call did not report root");
        }
    }

    // `getpid` and `gettid` must agree while there is one thread per process,
    // and disagreeing later is how a threaded program discovers there is more
    // than one of it.
    let pid = number_for(ferrix_linux_abi::nr::Syscall::Getpid)
        .ok_or("this architecture has no number for getpid")?;
    let tid = number_for(ferrix_linux_abi::nr::Syscall::Gettid)
        .ok_or("this architecture has no number for gettid")?;
    if counter.call(pid) != counter.call(tid) {
        return Err("getpid and gettid disagree with one thread running");
    }
    Ok(())
}

/// A number no table carries is `ENOSYS`, not a panic and not a wrong handler.
fn check_an_unknown_number_is_enosys(counter: &mut Counter) -> Result<(), &'static str> {
    let enosys = Outcome::Return(Errno::ENOSYS.as_return_value());
    // 0xDEAD is above every table on all three architectures; the other two
    // are the edges, where an implementation that indexed rather than matched
    // would fall off.
    for number in [0xDEAD, usize::MAX, usize::MAX - 1] {
        if counter.call(number) != enosys {
            return Err("an unknown system call number was not refused with ENOSYS");
        }
    }
    Ok(())
}

/// Every number in the plausible range is answered rather than trapped.
///
/// The sweep is the point: a `match` that decoded a number into a handler
/// which then read an argument it was not given would fault here, on a kernel
/// stack, with the scheduler running — which is a much better place to find it
/// than under a user program.
fn check_the_whole_number_space_is_total(counter: &mut Counter) -> Result<(), &'static str> {
    // Deliberately non-zero and not a valid pointer: a handler that decided to
    // dereference an argument should fault rather than quietly succeed.
    let poison = [0xAAAA_AAAA_AAAA_AAA0_u64; 6];
    for number in 0..=600 {
        let Outcome::Return(_) = counter.call_with(number, poison) else {
            return Err("a system call in the ordinary range asked to enter user mode");
        };
    }
    Ok(())
}

/// A refusal lands in the range Linux reserves for one.
///
/// `include/linux/err.h` reserves `-4095..=-1`. A pointer-returning call whose
/// success value strayed into that range would be read as a failure by every C
/// library, so the boundary is worth asserting where it is decided.
fn check_errors_encode_as_negative(counter: &mut Counter) -> Result<(), &'static str> {
    let Outcome::Return(value) = counter.call(0xDEAD) else {
        return Err("an unknown number asked to enter user mode");
    };
    if !(-4095..0).contains(&value) {
        return Err("ENOSYS did not encode into the reserved error range");
    }
    if value != -38 {
        return Err("ENOSYS is 38 on every architecture Ferrix targets");
    }
    Ok(())
}

/// This architecture's number for a call, by asking the decoder rather than
/// naming a table.
///
/// A linear sweep because the tables run one way only: `libs/linux-abi` maps a
/// number to a call and deliberately offers no inverse, since an inverse would
/// be a second copy of the table to disagree with the first.
fn number_for(call: ferrix_linux_abi::nr::Syscall) -> Option<usize> {
    (0..=600).find(|&number| arch::decode_syscall(number) == Some(call))
}

// ---------------------------------------------------------------------------
// The handlers, against a real address space
//
// Everything below builds a `Process` over a real `AddressSpace` and calls the
// handlers the way `dispatch` will. That is the whole reason the handlers take
// `&Process` rather than reaching for a current one: `mmap` is exercised
// against the actual VMA tree and the actual page tables, on all three
// architectures, before a program exists that could call it.
//
// The leak check around them is not decoration. A `mmap` that forgets to
// release its object on `munmap` leaks at a rate nothing reports, and the
// machine dies of it an hour into a build.
// ---------------------------------------------------------------------------

/// Where the checks put a mapping. Well clear of where an ELF image would go,
/// and page-aligned.
const TEST_BASE: u64 = 0x2000_0000;

/// Run the handler checks, reporting how many pages ended up faulted in.
/// Whether this pass should run the checks that print.
///
/// The group runs twice and only the second is measured. Without this the boot
/// log would carry every `write` check's output twice, which reads as a bug in
/// `write` rather than as a deliberate warm-up.
///
/// Skipping them on the warm-up costs the measurement nothing: they allocate
/// exactly what the other checks do -- one mapping through `map_rw` -- and
/// `console::write_bytes` touches no heap at all, so there is no size class
/// reachable only through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    /// The warm-up run.
    Quiet,
    /// The measured run.
    Show,
}

fn check_handlers(output: Output) -> Result<u64, &'static str> {
    let process = process::new_for_check().map_err(|_| "could not make a process")?;

    check_mmap_returns_usable_memory(&process)?;
    check_mmap_rejects_what_it_should(&process)?;
    check_fixed_mapping_lands_where_asked(&process)?;
    check_copy_crosses_a_page_boundary(&process)?;
    check_a_user_pointer_into_the_kernel_is_refused(&process)?;
    check_an_unmapped_address_is_efault_not_a_kernel_fault(&process)?;
    check_a_c_string_stops_at_its_nul(&process)?;
    check_mprotect_takes_write_away(&process)?;
    check_mremap_moves_the_contents_and_shrinks_in_place(&process)?;
    check_brk_grows_and_shrinks(&process)?;
    check_set_tid_address_answers_with_a_thread_id(&process)?;
    check_uname_says_linux_to_a_script_and_ferrix_to_a_person(&process)?;
    check_poll_reports_ready_invalid_and_skipped(&process)?;
    check_select_answers_with_the_sets_that_are_ready(&process)?;
    check_the_line_discipline_follows_its_settings()?;
    check_the_console_answers_as_a_terminal(&process)?;
    check_a_signal_disposition_reads_back_as_it_was_set(&process)?;
    check_the_blocked_mask_follows_how(&process)?;
    check_kill_finds_its_targets_and_refuses_what_it_should(&process)?;
    check_an_alternate_stack_is_recorded_and_refused_when_small(&process)?;
    check_an_image_loads_where_its_headers_say(&process)?;
    check_the_loader_refuses_what_it_cannot_run(&process)?;
    check_descriptors(&process)?;
    check_what_an_applet_asks_of_the_system(&process)?;
    if output == Output::Show {
        check_write_reaches_the_console(&process)?;
        check_writev_gathers_in_order(&process)?;
    }

    // Counted rather than asserted. An earlier version of this reported a
    // constant, which says nothing about what actually ran -- a check that
    // returned early would have reported the same number.
    Ok(pages_touched(&process))
}

/// How many pages this process has a translation for.
///
/// Asked of the page tables, not of the VMA map: a region is a promise and a
/// translation is the thing that was actually paid for.
fn pages_touched(process: &Process) -> u64 {
    let at = map_rw(process, PAGE_SIZE * 4).unwrap_or(0);
    if at == 0 {
        return 0;
    }
    let root = process.space().root_table();
    let mut count = 0;
    for page in 0..4 {
        let address = at + page * PAGE_SIZE;
        // Touch it, then confirm the touch produced a translation.
        if uaccess::copy_to_user(process.space(), address, b"x").is_ok()
            && mm::translate_in(root, address).is_some()
        {
            count += 1;
        }
    }
    let _ = memory::sys_munmap(process, at, PAGE_SIZE * 4);
    count
}

/// `mmap` hands back memory the kernel can then write and read back.
fn check_mmap_returns_usable_memory(process: &Process) -> Result<(), &'static str> {
    let at = memory::sys_mmap(
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
    .map_err(|_| "mmap of one anonymous page was refused")?;
    let at = u64::try_from(at).map_err(|_| "mmap returned an impossible address")?;

    if !at.is_multiple_of(PAGE_SIZE) {
        return Err("mmap returned an unaligned address");
    }

    let written = b"stage 7 was here";
    uaccess::copy_to_user(process.space(), at, written).map_err(|_| "could not write the page")?;
    let mut read = [0_u8; 16];
    uaccess::copy_from_user(process.space(), at, &mut read)
        .map_err(|_| "could not read the page back")?;
    if &read != written {
        return Err("what came back out of the page is not what went in");
    }

    // And it goes away again, pages and all.
    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    let mut after = [0_u8; 1];
    if uaccess::copy_from_user(process.space(), at, &mut after).is_ok() {
        return Err("an unmapped page was still readable");
    }
    Ok(())
}

/// The argument checks, which is where `mmap`'s bugs live.
fn check_mmap_rejects_what_it_should(process: &Process) -> Result<(), &'static str> {
    let anon = MAP_ANONYMOUS | MAP_PRIVATE;
    let cases: [(u64, u64, u32, u32, i64, &str); 6] = [
        (0, 0, PROT_READ, anon, -1, "a zero length"),
        (
            0,
            u64::MAX,
            PROT_READ,
            anon,
            -1,
            "a length that wraps when rounded",
        ),
        (0, PAGE_SIZE, 0x40, anon, -1, "an unknown protection bit"),
        (
            0,
            PAGE_SIZE,
            PROT_READ,
            MAP_PRIVATE,
            -1,
            "a file mapping, with no VFS",
        ),
        (
            0,
            PAGE_SIZE,
            PROT_READ,
            anon,
            3,
            "an anonymous mapping with a real fd",
        ),
        (
            0,
            PAGE_SIZE,
            PROT_READ,
            MAP_ANONYMOUS | MAP_PRIVATE | MAP_SHARED,
            -1,
            "both SHARED and PRIVATE",
        ),
    ];
    for (addr, len, prot, flags, fd, what) in cases {
        let request = MmapRequest {
            addr,
            len,
            prot,
            flags,
            fd,
            offset: 0,
            unit: OffsetUnit::Bytes,
        };
        if memory::sys_mmap(process, &request).is_ok() {
            let _ = what;
            return Err("mmap accepted arguments it should have refused");
        }
    }
    Ok(())
}

/// `MAP_FIXED` puts the mapping exactly where it was told.
fn check_fixed_mapping_lands_where_asked(process: &Process) -> Result<(), &'static str> {
    let at = memory::sys_mmap(
        process,
        &MmapRequest {
            addr: TEST_BASE,
            len: PAGE_SIZE,
            prot: PROT_READ | PROT_WRITE,
            flags: MAP_ANONYMOUS | MAP_PRIVATE | MAP_FIXED,
            fd: -1,
            offset: 0,
            unit: OffsetUnit::Bytes,
        },
    )
    .map_err(|_| "a fixed mapping was refused")?;
    if u64::try_from(at).unwrap_or(0) != TEST_BASE {
        return Err("MAP_FIXED did not map where it was asked to");
    }
    let _ = memory::sys_munmap(process, TEST_BASE, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// A copy spanning two pages copies both halves, which a one-page-at-a-time
/// walk gets wrong by exactly one page if the chunking is off.
fn check_copy_crosses_a_page_boundary(process: &Process) -> Result<(), &'static str> {
    let len = PAGE_SIZE * 2;
    let at = map_rw(process, len)?;
    // Straddle the boundary: eight bytes before it and eight after.
    let straddling = at + PAGE_SIZE - 8;
    let written = *b"ABCDEFGHIJKLMNOP";
    uaccess::copy_to_user(process.space(), straddling, &written)
        .map_err(|_| "a straddling write failed")?;
    let mut read = [0_u8; 16];
    uaccess::copy_from_user(process.space(), straddling, &mut read)
        .map_err(|_| "a straddling read failed")?;
    if read != written {
        return Err("a copy across a page boundary lost or moved bytes");
    }
    let _ = memory::sys_munmap(process, at, len).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// A user pointer naming a kernel address is refused before it is followed.
///
/// The one check here that is a security property rather than a correctness
/// one. Nothing in the hardware enforces it on this tree -- no SMAP, no PAN --
/// so this bound is the only thing between a program's pointer and a read of
/// the kernel at kernel privilege.
fn check_a_user_pointer_into_the_kernel_is_refused(process: &Process) -> Result<(), &'static str> {
    let mut out = [0_u8; 8];
    if uaccess::copy_from_user(process.space(), KERNEL_HALF_BASE, &mut out).is_ok() {
        return Err("a user pointer into the kernel half was followed");
    }
    // And a length that would carry a legal start address into the kernel.
    let at = map_rw(process, PAGE_SIZE)?;
    let mut huge = [0_u8; 8];
    if uaccess::copy_from_user(process.space(), u64::MAX - 3, &mut huge).is_ok() {
        return Err("a range that wraps the address space was accepted");
    }
    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// An address in no region is `EFAULT` to the program, not a kernel fault.
fn check_an_unmapped_address_is_efault_not_a_kernel_fault(
    process: &Process,
) -> Result<(), &'static str> {
    let mut out = [0_u8; 4];
    // A user address that is legal and mapped by nothing.
    if uaccess::copy_from_user(process.space(), TEST_BASE + 0x10_0000, &mut out).is_ok() {
        return Err("reading an unmapped user address succeeded");
    }
    Ok(())
}

/// `copy_cstr_from_user` stops at the NUL, and refuses a string without one.
fn check_a_c_string_stops_at_its_nul(process: &Process) -> Result<(), &'static str> {
    let at = map_rw(process, PAGE_SIZE)?;
    uaccess::copy_to_user(process.space(), at, b"/bin/sh\0and then some")
        .map_err(|_| "could not stage a string")?;

    let mut out = Vec::new();
    uaccess::copy_cstr_from_user(process.space(), at, 64, &mut out)
        .map_err(|_| "reading a C string failed")?;
    if out.as_slice() != b"/bin/sh" {
        return Err("a C string did not stop at its terminator");
    }

    // A limit shorter than the string is EFAULT, not a truncated answer: a
    // path silently cut in half is a file operation on the wrong file.
    let mut short = Vec::new();
    if uaccess::copy_cstr_from_user(process.space(), at, 3, &mut short).is_ok() {
        return Err("an over-long C string was truncated instead of refused");
    }
    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `mprotect` to read-only stops a copy into the region, and takes the old
/// writable translation down.
///
/// **Two separate properties, and only one of them is about permissions.**
///
/// The copy is refused because [`AddressSpace::fault`] checks the region's
/// flags before it does anything else, and every `copy_to_user` goes through
/// it. That is worth asserting, but note what it does *not* prove: the kernel
/// reaches the page through the direct map, which is writable for all of RAM,
/// so no user page-table permission bit is ever consulted on this path. A
/// `mprotect` that updated the map and left the tables alone would pass a
/// check that only tried to copy.
///
/// So the second half asks the tables directly. After `mprotect` the leaf must
/// be gone, because the region's permissions changed and the translation
/// carrying the old ones is still writable in hardware until something removes
/// it. Nothing can observe that from user mode yet -- there is no user mode --
/// which is exactly why it is worth checking from here.
fn check_mprotect_takes_write_away(process: &Process) -> Result<(), &'static str> {
    let at = map_rw(process, PAGE_SIZE)?;
    uaccess::copy_to_user(process.space(), at, b"before")
        .map_err(|_| "could not write before mprotect")?;

    let root = process.space().root_table();
    if mm::translate_in(root, at).is_none() {
        return Err("the page was not mapped after being written through");
    }

    let _ = memory::sys_mprotect(process, at, PAGE_SIZE, PROT_READ)
        .map_err(|_| "mprotect to read-only was refused")?;

    if mm::translate_in(root, at).is_some() {
        return Err("mprotect left the old translation in the page tables");
    }
    if uaccess::copy_to_user(process.space(), at, b"after").is_ok() {
        return Err("a read-only region was still writable");
    }
    // Reading still works, and still sees what was there.
    let mut out = [0_u8; 6];
    uaccess::copy_from_user(process.space(), at, &mut out)
        .map_err(|_| "a read-only region was not readable")?;
    if &out != b"before" {
        return Err("mprotect lost the contents of the page");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `mremap` grows a mapping that has a neighbour in the way by moving it, and
/// the contents go with it; shrinks one where it is; moves one to a fixed
/// address; and refuses what it should.
///
/// The contents are the point. glibc's `realloc` hands a large block to
/// `mremap` and carries on using it, so a move that arrived with the right
/// length and the wrong bytes -- zeros, or the pages of another object -- is
/// a corrupted heap that no call ever reports. The string written across the
/// page boundary is there so a move that got one page right and the other
/// wrong, or both in the wrong order, is caught too.
fn check_mremap_moves_the_contents_and_shrinks_in_place(
    process: &Process,
) -> Result<(), &'static str> {
    const FIRST: &[u8] = b"the first page";
    const ACROSS: &[u8] = b"across the page boundary";
    let space = process.space();

    // Two writable pages, and a third made read-only so it is a separate
    // region in the way: growing the two cannot happen where they are.
    let at = map_rw(process, PAGE_SIZE * 3)?;
    uaccess::copy_to_user(space, at, FIRST).map_err(|_| "could not write before mremap")?;
    let across = at + PAGE_SIZE - 8;
    uaccess::copy_to_user(space, across, ACROSS).map_err(|_| "could not write before mremap")?;
    let _ = memory::sys_mprotect(process, at + PAGE_SIZE * 2, PAGE_SIZE, PROT_READ)
        .map_err(|_| "mprotect of the page in the way was refused")?;

    refuses(
        memory::sys_mremap(process, at, PAGE_SIZE * 2, PAGE_SIZE * 4, 0, 0),
        Errno::ENOMEM,
        "mremap without MREMAP_MAYMOVE grew over the mapping in its way",
    )?;
    refuses(
        memory::sys_mremap(
            process,
            at,
            PAGE_SIZE * 2,
            PAGE_SIZE * 4,
            MREMAP_FIXED,
            at + PAGE_SIZE * 8,
        ),
        Errno::EINVAL,
        "mremap accepted MREMAP_FIXED without MREMAP_MAYMOVE",
    )?;
    refuses(
        memory::sys_mremap(process, at + 1, PAGE_SIZE, PAGE_SIZE * 2, MREMAP_MAYMOVE, 0),
        Errno::EINVAL,
        "mremap accepted an unaligned old address",
    )?;

    let moved = memory::sys_mremap(process, at, PAGE_SIZE * 2, PAGE_SIZE * 4, MREMAP_MAYMOVE, 0)
        .map_err(|_| "mremap with MREMAP_MAYMOVE refused to grow a mapping")?;
    let moved = u64::try_from(moved).map_err(|_| "mremap returned an impossible address")?;
    if moved == at {
        return Err("mremap grew a mapping over its neighbour rather than moving it");
    }
    let mut first = [0_u8; FIRST.len()];
    let mut straddle = [0_u8; ACROSS.len()];
    uaccess::copy_from_user(space, moved, &mut first)
        .map_err(|_| "the moved mapping was not readable")?;
    uaccess::copy_from_user(space, moved + PAGE_SIZE - 8, &mut straddle)
        .map_err(|_| "the moved mapping was not readable across its first page")?;
    if first != FIRST || straddle != ACROSS {
        return Err("the contents of a mapping did not survive mremap moving it");
    }
    let mut byte = [0xFF_u8; 1];
    uaccess::copy_from_user(space, at, &mut byte).map_or(Ok(()), |()| {
        Err("the old address still read after mremap moved it")
    })?;
    uaccess::copy_from_user(space, moved + PAGE_SIZE * 3, &mut byte)
        .map_err(|_| "the grown part of a moved mapping was not readable")?;
    if byte != [0] {
        return Err("the grown part of a moved mapping was not zero");
    }

    answers(
        memory::sys_mremap(process, moved, PAGE_SIZE * 4, PAGE_SIZE, 0, 0),
        usize::try_from(moved).unwrap_or(0),
        "mremap did not shrink a mapping where it was",
    )?;
    uaccess::copy_from_user(space, moved, &mut first)
        .map_err(|_| "a shrunk mapping lost the page it kept")?;
    if first != FIRST {
        return Err("a shrunk mapping lost the contents of the page it kept");
    }
    uaccess::copy_from_user(space, moved + PAGE_SIZE, &mut byte).map_or(Ok(()), |()| {
        Err("the tail mremap shrank away was still readable")
    })?;

    // And to a fixed address: the old one, which the move left free.
    answers(
        memory::sys_mremap(
            process,
            moved,
            PAGE_SIZE,
            PAGE_SIZE,
            MREMAP_MAYMOVE | MREMAP_FIXED,
            at,
        ),
        usize::try_from(at).unwrap_or(0),
        "mremap with MREMAP_FIXED did not land where it was told",
    )?;
    uaccess::copy_from_user(space, at, &mut first)
        .map_err(|_| "a mapping moved to a fixed address was not readable")?;
    if first != FIRST {
        return Err("the contents did not survive mremap moving to a fixed address");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE * 3).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `brk` moves the break, reports it, and never reports an error.
fn check_brk_grows_and_shrinks(process: &Process) -> Result<(), &'static str> {
    let start = memory::sys_brk(process, 0).map_err(|_| "brk(0) failed")?;
    let start = u64::try_from(start).map_err(|_| "brk returned an impossible address")?;
    if start == 0 {
        return Err("brk(0) reported no heap at all");
    }

    let want = start + 8192;
    let grown = memory::sys_brk(process, want).map_err(|_| "growing the break failed")?;
    if u64::try_from(grown).unwrap_or(0) != want {
        return Err("brk did not grow to where it was asked");
    }

    // The new heap is usable.
    uaccess::copy_to_user(process.space(), start, b"heap")
        .map_err(|_| "the grown heap was not writable")?;

    let shrunk = memory::sys_brk(process, start).map_err(|_| "shrinking the break failed")?;
    if u64::try_from(shrunk).unwrap_or(0) != start {
        return Err("brk did not shrink back");
    }

    // A request below the start is refused *by reporting the current break*,
    // which is the convention: brk has no error channel, and a libc that got
    // a negative number back would read it as an enormous valid heap.
    let refused = memory::sys_brk(process, 1).map_err(|_| "brk(1) failed")?;
    if u64::try_from(refused).unwrap_or(0) != start {
        return Err("a refused brk did not report the unchanged break");
    }
    Ok(())
}

/// `set_tid_address` answers with a thread id, not with zero.
///
/// musl uses the return value as its process id during startup, so this is one
/// of the few calls where a plausible-looking stub is worse than an error.
fn check_set_tid_address_answers_with_a_thread_id(process: &Process) -> Result<(), &'static str> {
    let at = map_rw(process, PAGE_SIZE)?;
    let tid = process.set_clear_child_tid(at, 42);
    if tid == 0 {
        return Err("set_tid_address reported thread zero");
    }
    if process.clear_child_tid() != at {
        return Err("set_tid_address did not record the address");
    }
    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `uname` fills all six fields, NUL-terminates each within its 65 bytes, and
/// says `Linux` where a script looks and `ferrix` where a person does.
///
/// The buffer is poisoned first. The structure is fixed-width and a reader
/// stops at the first NUL, so a handler that wrote the strings and left the
/// padding alone would pass a check on an all-zero page and hand a real
/// program whatever the page held before -- which is usually zero and
/// occasionally not.
fn check_uname_says_linux_to_a_script_and_ferrix_to_a_person(
    process: &Process,
) -> Result<(), &'static str> {
    const FIELD: usize = 65;
    const SIZE: usize = FIELD * 6;

    let at = map_rw(process, PAGE_SIZE)?;
    uaccess::copy_to_user(process.space(), at, &[0xAA_u8; SIZE])
        .map_err(|_| "could not poison the utsname buffer")?;
    if system::sys_uname(process, at) != Ok(0) {
        return Err("uname was refused");
    }
    let mut out = [0_u8; SIZE];
    uaccess::copy_from_user(process.space(), at, &mut out)
        .map_err(|_| "could not read utsname back")?;

    // Every field is a string, and everything after its NUL is NUL too.
    let mut fields: [&[u8]; 6] = [&[]; 6];
    for (slot, field) in out.chunks(FIELD).zip(fields.iter_mut()) {
        let end = slot
            .iter()
            .position(|&byte| byte == 0)
            .ok_or("a utsname field is not NUL-terminated")?;
        let (text, padding) = slot
            .split_at_checked(end)
            .ok_or("a utsname field is not NUL-terminated")?;
        if text.is_empty() {
            return Err("a utsname field is empty");
        }
        if padding.iter().any(|&byte| byte != 0) {
            return Err("a utsname field is not NUL-padded to its full width");
        }
        *field = text;
    }
    let [sysname, nodename, release, _version, machine, _domainname] = fields;

    if sysname != b"Linux" {
        return Err("uname did not say Linux, which is what a configure script asks");
    }
    if nodename != b"ferrix" {
        return Err("uname did not say ferrix where a person looks");
    }
    if !release.ends_with(b"-ferrix") {
        return Err("the release does not carry the Ferrix suffix");
    }
    // The machine name is decided by the build, and a 32-bit one must not
    // claim to be a 64-bit machine: glibc's loader and every `configure`
    // script size the world by this field.
    let class = match machine {
        b"x86_64" | b"aarch64" => Class::Elf64,
        b"armv7l" => Class::Elf32,
        _ => return Err("the machine name is not one Linux uses"),
    };
    if class != class_of_this_build() {
        return Err("the machine name does not match the build's word size");
    }

    // A pointer into the kernel is EFAULT, not a write.
    if system::sys_uname(process, KERNEL_HALF_BASE) != Err(Errno::EFAULT) {
        return Err("uname into a kernel address was not EFAULT");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `rt_sigaction` hands back, as `oldact`, exactly what the previous call set
/// -- which is all a program starting up can see of signals today.
///
/// Read back through a second call rather than from the table, because the
/// layout is the thing most likely to be wrong: three native words and an
/// 8-byte mask, which is 32 bytes on 64-bit machines and 20 on ARMv7-A. A
/// handler that wrote the 64-bit layout on a 32-bit build would put the flags
/// where the program reads the restorer, and a check against the table would
/// still pass.
fn check_a_signal_disposition_reads_back_as_it_was_set(
    process: &Process,
) -> Result<(), &'static str> {
    use ferrix_linux_abi::types::{SA_RESTART, SA_RESTORER, SIGINT, SIGKILL, SIGUSR1};

    let word = size_of::<usize>();
    let size = word * 3 + 8;
    let at = map_rw(process, PAGE_SIZE)?;
    let old = at + PAGE_SIZE / 2;
    let set = signal::SIGSET_SIZE;

    // The action to install, in this architecture's own layout.
    let mut act = [0_u8; 32];
    let fields = [0x0001_2340_u64, SA_RESTORER | SA_RESTART, 0x0005_6780];
    for (slot, value) in act.chunks_mut(word).zip(fields) {
        slot.copy_from_slice(value.to_le_bytes().get(..word).ok_or("impossible word")?);
    }
    let mask = (1_u64 << (SIGUSR1 - 1)) | (1_u64 << (SIGKILL - 1));
    act.get_mut(word * 3..size)
        .ok_or("impossible sigaction size")?
        .copy_from_slice(&mask.to_le_bytes());
    let act_bytes = act.get(..size).ok_or("impossible sigaction size")?;
    uaccess::copy_to_user(process.space(), at, act_bytes)
        .map_err(|_| "could not stage a sigaction")?;

    // The first call reports the default, into a poisoned buffer.
    uaccess::copy_to_user(process.space(), old, &[0xAA_u8; 32])
        .map_err(|_| "could not poison oldact")?;
    if signal::sys_rt_sigaction(process, SIGINT, at, old, set) != Ok(0) {
        return Err("rt_sigaction refused a valid handler");
    }
    let mut back = [0_u8; 32];
    uaccess::copy_from_user(process.space(), old, &mut back)
        .map_err(|_| "could not read oldact")?;
    let (first, tail) = back
        .split_at_checked(size)
        .ok_or("impossible sigaction size")?;
    if first.iter().any(|&byte| byte != 0) {
        return Err("the first oldact was not the zeroed default");
    }
    if tail.iter().any(|&byte| byte != 0xAA) {
        return Err("rt_sigaction wrote past the end of this architecture's sigaction");
    }

    // The second reports the first, with SIGKILL taken out of the mask.
    if signal::sys_rt_sigaction(process, SIGINT, 0, old, set) != Ok(0) {
        return Err("rt_sigaction refused a query");
    }
    uaccess::copy_from_user(process.space(), old, &mut back)
        .map_err(|_| "could not read oldact")?;
    let mut expected = act;
    expected
        .get_mut(word * 3..size)
        .ok_or("impossible sigaction size")?
        .copy_from_slice(&(1_u64 << (SIGUSR1 - 1)).to_le_bytes());
    if back.get(..size) != expected.get(..size) {
        return Err("oldact was not the action set before it, field for field");
    }

    // What Linux refuses.
    if signal::sys_rt_sigaction(process, SIGINT, at, 0, 16) != Err(Errno::EINVAL) {
        return Err("a sigsetsize other than 8 was accepted");
    }
    if signal::sys_rt_sigaction(process, 0, 0, old, set) != Err(Errno::EINVAL) {
        return Err("signal 0 was accepted");
    }
    if signal::sys_rt_sigaction(process, 65, 0, old, set) != Err(Errno::EINVAL) {
        return Err("signal 65 was accepted");
    }
    if signal::sys_rt_sigaction(process, SIGKILL, at, 0, set) != Err(Errno::EINVAL) {
        return Err("a handler for SIGKILL was accepted");
    }
    if signal::sys_rt_sigaction(process, SIGKILL, 0, old, set) != Ok(0) {
        return Err("asking what SIGKILL does was refused");
    }
    if signal::sys_rt_sigaction(process, SIGINT, KERNEL_HALF_BASE, 0, set) != Err(Errno::EFAULT) {
        return Err("an action at a kernel address was not EFAULT");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `rt_sigprocmask` applies `how`, never blocks SIGKILL, and leaves `oldset`
/// untouched when it refuses.
fn check_the_blocked_mask_follows_how(process: &Process) -> Result<(), &'static str> {
    use ferrix_linux_abi::types::{SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, SIGINT, SIGKILL, SIGUSR1};

    let at = map_rw(process, PAGE_SIZE)?;
    let old = at + 8;
    let set = signal::SIGSET_SIZE;
    let usr1 = 1_u64 << (SIGUSR1 - 1);
    let int = 1_u64 << (SIGINT - 1);
    let kill = 1_u64 << (SIGKILL - 1);

    let read_old = || -> Result<u64, &'static str> {
        let mut bytes = [0_u8; 8];
        uaccess::copy_from_user(process.space(), old, &mut bytes)
            .map_err(|_| "could not read oldset")?;
        Ok(u64::from_le_bytes(bytes))
    };
    let stage = |value: u64| {
        uaccess::copy_to_user(process.space(), at, &value.to_le_bytes())
            .map_err(|_| "could not stage a set")
    };

    stage(usr1 | kill)?;
    if signal::sys_rt_sigprocmask(process, SIG_BLOCK, at, old, set) != Ok(0) {
        return Err("SIG_BLOCK was refused");
    }
    stage(int)?;
    if signal::sys_rt_sigprocmask(process, SIG_BLOCK, at, old, set) != Ok(0) || read_old()? != usr1
    {
        return Err("the mask after blocking SIGUSR1 and SIGKILL was not SIGUSR1 alone");
    }
    stage(usr1)?;
    if signal::sys_rt_sigprocmask(process, SIG_UNBLOCK, at, old, set) != Ok(0)
        || read_old()? != usr1 | int
    {
        return Err("SIG_BLOCK did not add to the mask");
    }
    stage(0)?;
    if signal::sys_rt_sigprocmask(process, SIG_SETMASK, at, old, set) != Ok(0) || read_old()? != int
    {
        return Err("SIG_UNBLOCK did not take away from the mask");
    }

    // A refused `how` writes nothing; with no set it is not even looked at.
    uaccess::copy_to_user(process.space(), old, &[0xAA_u8; 8])
        .map_err(|_| "could not poison oldset")?;
    if signal::sys_rt_sigprocmask(process, 7, at, old, set) != Err(Errno::EINVAL) {
        return Err("a nonsense how was accepted");
    }
    if read_old()? != u64::from_le_bytes([0xAA; 8]) {
        return Err("a refused sigprocmask still wrote oldset");
    }
    if signal::sys_rt_sigprocmask(process, 7, 0, old, set) != Ok(0) || read_old()? != 0 {
        return Err("a query with a nonsense how was refused, or misreported the mask");
    }
    if signal::sys_rt_sigprocmask(process, SIG_BLOCK, at, old, 4) != Err(Errno::EINVAL) {
        return Err("a sigsetsize other than 8 was accepted");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `sigaltstack` records a stack big enough, refuses one too small, and
/// reports `SS_DISABLE` when none is installed.
fn check_an_alternate_stack_is_recorded_and_refused_when_small(
    process: &Process,
) -> Result<(), &'static str> {
    use ferrix_linux_abi::types::SS_DISABLE;

    let word = size_of::<usize>();
    let size = word * 3;
    let at = map_rw(process, PAGE_SIZE)?;
    let old = at + PAGE_SIZE / 2;

    let stage = |sp: u64, flags: i32, bytes: u64| {
        let mut raw = [0_u8; 24];
        let _ = raw
            .get_mut(..word)
            .map(|slot| slot.copy_from_slice(sp.to_le_bytes().get(..word).unwrap_or(&[])));
        let _ = raw
            .get_mut(word..word + 4)
            .map(|slot| slot.copy_from_slice(&flags.to_le_bytes()));
        let _ = raw
            .get_mut(word * 2..size)
            .map(|slot| slot.copy_from_slice(bytes.to_le_bytes().get(..word).unwrap_or(&[])));
        uaccess::copy_to_user(process.space(), at, raw.get(..size).unwrap_or(&[]))
            .map_err(|_| "could not stage a stack_t")
    };
    let read_old = || -> Result<(u64, i32, u64), &'static str> {
        let mut raw = [0_u8; 24];
        let bytes = raw.get_mut(..size).ok_or("impossible stack_t size")?;
        uaccess::copy_from_user(process.space(), old, bytes).map_err(|_| "could not read old")?;
        let mut sp = [0_u8; 8];
        let mut flags = [0_u8; 4];
        let mut length = [0_u8; 8];
        sp.get_mut(..word)
            .ok_or("impossible word")?
            .copy_from_slice(bytes.get(..word).ok_or("impossible word")?);
        flags.copy_from_slice(bytes.get(word..word + 4).ok_or("impossible word")?);
        length
            .get_mut(..word)
            .ok_or("impossible word")?
            .copy_from_slice(bytes.get(word * 2..size).ok_or("impossible word")?);
        Ok((
            u64::from_le_bytes(sp),
            i32::from_le_bytes(flags),
            u64::from_le_bytes(length),
        ))
    };

    if signal::sys_sigaltstack(process, 0, old, 0) != Ok(0) || read_old()? != (0, SS_DISABLE, 0) {
        return Err("with no alternate stack, sigaltstack did not report SS_DISABLE");
    }
    stage(0x0001_0000, 0, 1024)?;
    if signal::sys_sigaltstack(process, at, 0, 0) != Err(Errno::ENOMEM) {
        return Err("an alternate stack below MINSIGSTKSZ was accepted");
    }
    stage(0x0001_0000, 5, 65536)?;
    if signal::sys_sigaltstack(process, at, 0, 0) != Err(Errno::EINVAL) {
        return Err("a nonsense ss_flags was accepted");
    }
    stage(0x0001_0000, 0, 65536)?;
    if signal::sys_sigaltstack(process, at, 0, 0) != Ok(0) {
        return Err("a valid alternate stack was refused");
    }
    if signal::sys_sigaltstack(process, 0, old, 0) != Ok(0)
        || read_old()? != (0x0001_0000, 0, 65536)
    {
        return Err("the installed alternate stack did not read back");
    }
    stage(0, SS_DISABLE, 0)?;
    if signal::sys_sigaltstack(process, at, old, 0) != Ok(0)
        || read_old()? != (0x0001_0000, 0, 65536)
    {
        return Err("disabling did not report the stack it replaced");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `poll` answers each descriptor for what it is: the console is writable, a
/// descriptor that names nothing is `POLLNVAL`, and a negative one is left
/// alone -- and with nothing ready it waits out its timeout rather than
/// returning at once.
///
/// Busybox's `while read` asks `poll` about its file before every read, and a
/// refused `poll` makes it read nothing, which is how this call was found to be
/// missing.
fn check_poll_reports_ready_invalid_and_skipped(process: &Process) -> Result<(), &'static str> {
    use crate::syscall::poll::{self, POLLIN, POLLNVAL, POLLOUT, POLLWRNORM};

    let at = map_rw(process, PAGE_SIZE)?;
    let entry = |fd: i32, events: u16| {
        let mut bytes = [0_u8; 8];
        let fields = fd.to_le_bytes().into_iter().chain(events.to_le_bytes());
        for (slot, byte) in bytes.iter_mut().zip(fields) {
            *slot = byte;
        }
        bytes
    };
    let mut array = [0_u8; 24];
    let three = entry(1, POLLOUT | POLLIN)
        .into_iter()
        .chain(entry(4000, POLLIN))
        .chain(entry(-1, POLLIN));
    for (slot, byte) in array.iter_mut().zip(three) {
        *slot = byte;
    }
    uaccess::copy_to_user(process.space(), at, &array)
        .map_err(|_| "could not stage a pollfd array")?;

    if poll::sys_poll(process, at, 3, 0) != Ok(2) {
        return Err("poll did not count exactly the console and the closed descriptor");
    }
    let mut back = [0_u8; 24];
    uaccess::copy_from_user(process.space(), at, &mut back)
        .map_err(|_| "could not read revents")?;
    let revents = |at: usize| {
        u16::from_le_bytes([
            back.get(at + 6).copied().unwrap_or(0xFF),
            back.get(at + 7).copied().unwrap_or(0xFF),
        ])
    };
    // Only what was asked for comes back, plus hang-ups and errors: the
    // console was asked about `POLLIN|POLLOUT`, so `POLLWRNORM` must not appear
    // even though the console is writable.
    if revents(0) & POLLOUT == 0 {
        return Err("poll did not report the console writable");
    }
    if revents(0) & POLLWRNORM != 0 {
        return Err("poll reported an event that was not asked for");
    }
    if revents(8) != POLLNVAL {
        return Err("poll did not answer POLLNVAL for a descriptor that names nothing");
    }
    if revents(16) != 0 {
        return Err("poll touched a negative descriptor it should have skipped");
    }

    // Nothing ready: only the skipped slot. The call must take its timeout.
    uaccess::copy_to_user(process.space(), at, &entry(-1, POLLIN))
        .map_err(|_| "could not stage a pollfd")?;
    let before = crate::timer::now_nanos();
    if poll::sys_poll(process, at, 1, 10) != Ok(0) {
        return Err("poll with nothing ready did not time out with zero");
    }
    if crate::timer::now_nanos().saturating_sub(before) < 10_000_000 {
        return Err("poll with nothing ready returned before its timeout");
    }

    if poll::sys_ppoll(
        process,
        at,
        1,
        0,
        at,
        16,
        crate::syscall::time::TimeWidth::Native,
    ) != Err(Errno::EINVAL)
    {
        return Err("ppoll accepted a signal set of the wrong size");
    }
    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// Where on its page the `select` check stages each argument.
const SELECT_WRITE: u64 = 256;
/// See [`SELECT_WRITE`].
const SELECT_EXCEPT: u64 = 512;
/// See [`SELECT_WRITE`].
const SELECT_TIME: u64 = 768;
/// See [`SELECT_WRITE`].
const SELECT_MASK: u64 = 800;
/// See [`SELECT_WRITE`].
const SELECT_PACK: u64 = 816;
/// See [`SELECT_WRITE`].
const SELECT_OLD_MASK: u64 = 840;

/// `select` and `pselect6` answer from the same readiness as `poll`, as
/// bitmaps: the console's descriptors are writable and never exceptional, a
/// set bit on a closed descriptor is `EBADF`, bits past `nfds` are neither
/// asked about nor left set, a timeout is waited out and written back, and
/// `pselect6`'s signal mask is checked for size and put back afterwards.
fn check_select_answers_with_the_sets_that_are_ready(
    process: &Process,
) -> Result<(), &'static str> {
    let page = map_rw(process, PAGE_SIZE)?;
    let outcome = select_answers(process, page).and_then(|()| pselect_answers(process, page));
    let _ = memory::sys_munmap(process, page, PAGE_SIZE);
    outcome
}

/// See [`check_select_answers_with_the_sets_that_are_ready`].
fn select_answers(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::poll;

    let stage = |offset: u64, bytes: &[u8]| {
        uaccess::copy_to_user(process.space(), page + offset, bytes)
            .map_err(|_| "could not stage a select argument")
    };
    let byte_at = |offset: u64| {
        let mut byte = [0_u8; 1];
        uaccess::copy_from_user(process.space(), page + offset, &mut byte)
            .map_err(|_| "could not read a select answer back")?;
        Ok::<u8, &'static str>(u8::from_le_bytes(byte))
    };
    let word = size_of::<usize>();
    let (write_set, except_set) = (page + SELECT_WRITE, page + SELECT_EXCEPT);

    // Descriptors 1 and 2 asked about for writing, 1 for exceptions, with a
    // zero timeout: two bits come back, and the exception set comes back
    // empty.
    stage(SELECT_WRITE, &[0b110])?;
    stage(SELECT_EXCEPT, &[0b010])?;
    stage(SELECT_TIME, &word_pair(0, 0))?;
    if poll::sys_select(process, 3, [0, write_set, except_set], page + SELECT_TIME) != Ok(2) {
        return Err("select did not count the console's two writable descriptors");
    }
    if byte_at(SELECT_WRITE)? != 0b110 || byte_at(SELECT_EXCEPT)? != 0 {
        return Err("select did not write back exactly the bits that were ready");
    }

    // A bit past `nfds` inside the last word read is not asked about, and is
    // cleared in the answer.
    stage(SELECT_WRITE, &[0b010, 0, 0b1_0000])?;
    if poll::sys_select(process, 2, [0, write_set, 0], page + SELECT_TIME) != Ok(1) {
        return Err("select looked at a descriptor past nfds");
    }
    if byte_at(SELECT_WRITE + 2)? != 0 {
        return Err("select left a bit set past nfds");
    }

    // Descriptor 40 is not open.
    stage(SELECT_WRITE, &[0b010, 0, 0, 0, 0, 0b1])?;
    if poll::sys_select(process, 41, [0, write_set, 0], page + SELECT_TIME) != Err(Errno::EBADF) {
        return Err("select did not refuse a closed descriptor with EBADF");
    }
    if poll::sys_select(process, -1, [0, 0, 0], 0) != Err(Errno::EINVAL) {
        return Err("select accepted a negative nfds");
    }
    stage(SELECT_TIME, &word_pair(0, usize::MAX))?;
    if poll::sys_select(process, 0, [0, 0, 0], page + SELECT_TIME) != Err(Errno::EINVAL) {
        return Err("select accepted a negative microsecond count");
    }

    // Nothing asked: the timeout is waited out, and the time left, none, is
    // written back.
    stage(SELECT_TIME, &word_pair(0, 10_000))?;
    let before = crate::timer::now_nanos();
    if poll::sys_select(process, 0, [0, 0, 0], page + SELECT_TIME) != Ok(0) {
        return Err("select with nothing to wait for did not time out with zero");
    }
    if crate::timer::now_nanos().saturating_sub(before) < 10_000_000 {
        return Err("select returned before its timeout");
    }
    let mut left = [0_u8; 16];
    uaccess::copy_from_user(process.space(), page + SELECT_TIME, &mut left)
        .map_err(|_| "could not read the time left back")?;
    if left.iter().take(word * 2).any(|&byte| byte != 0) {
        return Err("select did not write back that no time was left");
    }

    Ok(())
}

/// See [`check_select_answers_with_the_sets_that_are_ready`]: `pselect6`'s
/// mask.
fn pselect_answers(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::poll;
    use crate::syscall::time::TimeWidth;

    let stage = |offset: u64, bytes: &[u8]| {
        uaccess::copy_to_user(process.space(), page + offset, bytes)
            .map_err(|_| "could not stage a pselect6 argument")
    };
    let write_set = page + SELECT_WRITE;
    // `pselect6`'s mask: a set of the wrong size is refused, a null set's size
    // is not looked at, and a mask waited under is put back.
    let mask = 1_u64 << 9;
    stage(SELECT_MASK, &mask.to_le_bytes())?;
    stage(SELECT_WRITE, &[0b110])?;
    stage(SELECT_TIME, &word_pair(0, 0))?;
    let pselect = |nfds: i32, set: usize, size: usize| {
        let pack = word_pair(set, size);
        stage(SELECT_PACK, &pack)?;
        Ok::<_, &'static str>(poll::sys_pselect6(
            process,
            nfds,
            [0, write_set, 0],
            page + SELECT_TIME,
            page + SELECT_PACK,
            TimeWidth::Native,
        ))
    };
    let mask_at = usize::try_from(page + SELECT_MASK).map_err(|_| "an impossible address")?;
    if pselect(3, mask_at, 16)? != Err(Errno::EINVAL) {
        return Err("pselect6 accepted a signal set of the wrong size");
    }
    if pselect(3, 0, 16)? != Ok(2) {
        return Err("pselect6 looked at the size of a signal set it was not given");
    }
    let blocked = |process: &Process| {
        let _ = signal::sys_rt_sigprocmask(process, 0, 0, page + SELECT_OLD_MASK, 8)
            .map_err(|_| "rt_sigprocmask could not report the mask")?;
        let mut bytes = [0_u8; 8];
        uaccess::copy_from_user(process.space(), page + SELECT_OLD_MASK, &mut bytes)
            .map_err(|_| "could not read the mask back")?;
        Ok::<u64, &'static str>(u64::from_le_bytes(bytes))
    };
    let before = blocked(process)?;
    stage(SELECT_WRITE, &[0b110])?;
    if pselect(3, mask_at, 8)? != Ok(2) {
        return Err("pselect6 refused a well-formed signal mask");
    }
    if blocked(process)? != before {
        return Err("pselect6 did not put the caller's signal mask back");
    }
    Ok(())
}

/// Two native words, little-endian, in the first bytes of sixteen: a
/// `timeval`, a native `timespec`, or `pselect6`'s set-and-size pair.
fn word_pair(first: usize, second: usize) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    for (slot, byte) in bytes
        .iter_mut()
        .zip(first.to_le_bytes().into_iter().chain(second.to_le_bytes()))
    {
        *slot = byte;
    }
    bytes
}

/// The line discipline, driven directly: the default settings edit and echo a
/// line as the console always has, end of file and a line without a newline
/// both come through `VEOF`, raw mode neither waits for a line nor echoes, and
/// the interrupt character is a signal that takes the half-typed line with it.
fn check_the_line_discipline_follows_its_settings() -> Result<(), &'static str> {
    use crate::fs::terminal::{Discipline, Termios};
    use ferrix_linux_abi::types::{ECHO, ICANON, SIGINT};

    let mut discipline = Discipline::new();
    let mut echo = Vec::new();
    let mut buf = [0_u8; 8];
    let feed = |discipline: &mut Discipline, bytes: &[u8], echo: &mut Vec<u8>| {
        bytes
            .iter()
            .filter_map(|&byte| discipline.receive(byte, echo))
            .last()
    };

    if feed(&mut discipline, b"ab\x7fc\r", &mut echo).is_some() {
        return Err("an ordinary keystroke raised a signal");
    }
    if echo != b"ab\x08 \x08c\n" {
        return Err("the default settings did not echo and erase as the console always has");
    }
    if discipline.take(&mut buf) != Some(3) || buf.get(..3) != Some(b"ac\n".as_slice()) {
        return Err("a canonical line did not read back as it was edited");
    }
    let _ = feed(&mut discipline, b"x", &mut echo);
    if discipline.readable() {
        return Err("half a line was readable in canonical mode");
    }
    let _ = feed(&mut discipline, b"\x04", &mut echo);
    if discipline.take(&mut buf) != Some(1) || buf.first() != Some(&b'x') {
        return Err("VEOF did not end a line without a newline");
    }
    let _ = feed(&mut discipline, b"\x04", &mut echo);
    if discipline.take(&mut buf) != Some(0) || discipline.take(&mut buf).is_some() {
        return Err("VEOF on an empty line was not one end of file");
    }

    let raw = Termios {
        lflag: Termios::DEFAULT.lflag & !(ICANON | ECHO),
        ..Termios::DEFAULT
    };
    discipline.set_termios(raw);
    echo.clear();
    let _ = feed(&mut discipline, b"q", &mut echo);
    if !echo.is_empty() {
        return Err("a keystroke was echoed with ECHO off");
    }
    if discipline.take(&mut buf) != Some(1) || buf.first() != Some(&b'q') {
        return Err("a keystroke in raw mode was not readable at once");
    }

    discipline.set_termios(Termios::DEFAULT);
    echo.clear();
    if feed(&mut discipline, b"z\x03", &mut echo) != Some(SIGINT) {
        return Err("the interrupt character did not raise SIGINT");
    }
    if echo != b"z^C" {
        return Err("the interrupt character was not echoed as ^C");
    }
    let _ = feed(&mut discipline, b"\r", &mut echo);
    if discipline.take(&mut buf) != Some(1) {
        return Err("the interrupt character did not discard the line being typed");
    }
    Ok(())
}

/// Where on its page the terminal check stages each argument.
const TTY_TERMIOS: u64 = 0;
/// See [`TTY_TERMIOS`].
const TTY_INT: u64 = 64;
/// See [`TTY_TERMIOS`].
const TTY_PATH: u64 = 128;

/// The console answers the terminal requests an interactive shell makes:
/// settings that read back as they were set, a size, and job control for a
/// session leader -- and leaves the terminal as it found it.
fn check_the_console_answers_as_a_terminal(process: &Process) -> Result<(), &'static str> {
    use crate::fs::terminal;

    let page = map_rw(process, PAGE_SIZE)?;
    let saved = terminal::with(|terminal| {
        (
            terminal.discipline.termios(),
            terminal.winsize,
            terminal.session,
            terminal.foreground,
        )
    });
    let outcome = terminal_settings_answer(process, page)
        .and_then(|()| terminal_job_control_answers(process, page))
        .and_then(|()| terminal_answers_through_dev_tty(process, page));
    terminal::with(|terminal| {
        let (termios, winsize, session, foreground) = saved;
        terminal.discipline.set_termios(termios);
        terminal.winsize = winsize;
        terminal.session = session;
        terminal.foreground = foreground;
    });
    let _ = memory::sys_munmap(process, page, PAGE_SIZE);
    outcome
}

/// See [`check_the_console_answers_as_a_terminal`]: the same questions through
/// `/dev/tty`, a devfs node of its own that opens the console -- which is the
/// descriptor busybox's shell asks for the foreground group on, and which was
/// once refused because `fstat` names a different inode than the console.
fn terminal_answers_through_dev_tty(process: &Process, page: u64) -> Result<(), &'static str> {
    use ferrix_linux_abi::types::TIOCGPGRP;

    uaccess::copy_to_user(process.space(), page + TTY_PATH, b"/dev/tty\0")
        .map_err(|_| "could not stage /dev/tty")?;
    let tty = fd::sys_openat(process, AT_FDCWD, page + TTY_PATH, O_RDWR, 0)
        .map_err(|_| "/dev/tty did not open")?;
    let tty = i32::try_from(tty).map_err(|_| "an impossible descriptor")?;
    let outcome = answers(
        fd::sys_ioctl(process, tty, TCGETS, page + TTY_TERMIOS),
        0,
        "TCGETS through /dev/tty was refused",
    )
    .and_then(|()| {
        answers(
            fd::sys_ioctl(process, tty, TIOCGPGRP, page + TTY_INT),
            0,
            "TIOCGPGRP through /dev/tty was refused to a session leader",
        )
    });
    let _ = fd::sys_close(process, tty);
    outcome
}

/// See [`check_the_console_answers_as_a_terminal`].
fn terminal_settings_answer(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::fs::terminal::{Termios, Winsize};
    use ferrix_linux_abi::types::{
        ECHO, ICANON, ICRNL, ISIG, ONLCR, OPOST, TCFLSH, TCSETSF, TCSETSW, TCXONC, TERMIOS_BYTES,
        VERASE, VMIN,
    };

    let read_termios = || {
        let mut bytes = [0_u8; TERMIOS_BYTES];
        uaccess::copy_from_user(process.space(), page + TTY_TERMIOS, &mut bytes)
            .map_err(|_| "could not read a termios back")?;
        Ok::<_, &'static str>(Termios::from_bytes(&bytes))
    };
    answers(
        fd::sys_ioctl(process, 0, TCGETS, page + TTY_TERMIOS),
        0,
        "TCGETS on the console was refused",
    )?;
    let settings = read_termios()?;
    let local = ICANON | ECHO | ISIG;
    if settings.lflag & local != local
        || settings.iflag & ICRNL == 0
        || settings.oflag & (OPOST | ONLCR) != OPOST | ONLCR
        || settings.cc(VMIN) != 1
        || settings.cc(VERASE) != 0x7F
    {
        return Err("the console's settings were not a canonical, echoing terminal's");
    }

    // Raw mode, as `sh -i` sets it, reads back as set; then the original.
    let raw = Termios {
        lflag: settings.lflag & !(ICANON | ECHO),
        ..settings
    };
    uaccess::copy_to_user(process.space(), page + TTY_TERMIOS, &raw.to_bytes())
        .map_err(|_| "could not stage a termios")?;
    answers(
        fd::sys_ioctl(process, 0, TCSETSW, page + TTY_TERMIOS),
        0,
        "TCSETSW on the console was refused",
    )?;
    answers(
        fd::sys_ioctl(process, 1, TCGETS, page + TTY_TERMIOS),
        0,
        "TCGETS on the console was refused",
    )?;
    if read_termios()? != raw {
        return Err("TCGETS did not report what TCSETSW set");
    }
    uaccess::copy_to_user(process.space(), page + TTY_TERMIOS, &settings.to_bytes())
        .map_err(|_| "could not stage a termios")?;
    answers(
        fd::sys_ioctl(process, 0, TCSETSF, page + TTY_TERMIOS),
        0,
        "TCSETSF on the console was refused",
    )?;

    answers(
        fd::sys_ioctl(process, 1, TIOCGWINSZ, page + TTY_TERMIOS),
        0,
        "TIOCGWINSZ on the console was refused",
    )?;
    let mut size = [0_u8; 8];
    uaccess::copy_from_user(process.space(), page + TTY_TERMIOS, &mut size)
        .map_err(|_| "could not read a winsize back")?;
    if Winsize::from_bytes(size) != Winsize::DEFAULT {
        return Err("the console did not report 24 rows of 80 columns");
    }
    refuses(
        fd::sys_ioctl(process, 1, TIOCGWINSZ, 0),
        Errno::EFAULT,
        "a terminal's answer was written to address zero",
    )?;
    refuses(
        fd::sys_ioctl(process, 0, TCFLSH, 7),
        Errno::EINVAL,
        "TCFLSH accepted a queue that does not exist",
    )?;
    answers(
        fd::sys_ioctl(process, 0, TCXONC, 1),
        0,
        "TCXONC refused to restart output",
    )?;
    refuses(
        fd::sys_ioctl(process, 0, 0x5480, 0),
        Errno::ENOTTY,
        "an unknown terminal request was not ENOTTY",
    )
}

/// See [`check_the_console_answers_as_a_terminal`].
fn terminal_job_control_answers(process: &Process, page: u64) -> Result<(), &'static str> {
    use ferrix_linux_abi::types::{FIONREAD, TIOCGPGRP, TIOCGSID, TIOCNOTTY, TIOCSCTTY, TIOCSPGRP};

    let at = page + TTY_INT;
    let read_int = || {
        let mut bytes = [0_u8; 4];
        uaccess::copy_from_user(process.space(), at, &mut bytes)
            .map_err(|_| "could not read an int back")?;
        Ok::<u32, &'static str>(u32::from_le_bytes(bytes))
    };
    let stage_int = |value: i32| {
        uaccess::copy_to_user(process.space(), at, &value.to_le_bytes())
            .map_err(|_| "could not stage an int")
    };

    // The check's process leads its own session, so the free console becomes
    // its controlling terminal when it asks.
    answers(
        fd::sys_ioctl(process, 0, TIOCGPGRP, at),
        0,
        "TIOCGPGRP was refused to a session leader",
    )?;
    if read_int()? != process.pgid() {
        return Err("the console's foreground group was not its session leader's");
    }
    answers(
        fd::sys_ioctl(process, 0, TIOCGSID, at),
        0,
        "TIOCGSID was refused",
    )?;
    if read_int()? != process.sid() {
        return Err("the console did not report its session");
    }
    answers(
        fd::sys_ioctl(process, 0, TIOCSCTTY, 0),
        0,
        "TIOCSCTTY was refused for the terminal the session already has",
    )?;
    stage_int(i32::try_from(process.pgid()).map_err(|_| "an impossible group")?)?;
    answers(
        fd::sys_ioctl(process, 0, TIOCSPGRP, at),
        0,
        "TIOCSPGRP was refused the caller's own group",
    )?;
    stage_int(-1)?;
    refuses(
        fd::sys_ioctl(process, 0, TIOCSPGRP, at),
        Errno::EINVAL,
        "TIOCSPGRP accepted a negative group",
    )?;
    stage_int(0x7FFF_FFF0)?;
    refuses(
        fd::sys_ioctl(process, 0, TIOCSPGRP, at),
        Errno::ESRCH,
        "TIOCSPGRP accepted a group nobody is in",
    )?;
    answers(
        fd::sys_ioctl(process, 0, FIONREAD, at),
        0,
        "FIONREAD was refused",
    )?;
    answers(
        fd::sys_ioctl(process, 0, TIOCNOTTY, 0),
        0,
        "TIOCNOTTY was refused to the session holding the terminal",
    )
}

/// Map `len` bytes of read/write anonymous memory, wherever it fits.
fn map_rw(process: &Process, len: u64) -> Result<u64, &'static str> {
    let at = memory::sys_mmap(
        process,
        &MmapRequest {
            addr: 0,
            len,
            prot: PROT_READ | PROT_WRITE,
            flags: MAP_ANONYMOUS | MAP_PRIVATE,
            fd: -1,
            offset: 0,
            unit: OffsetUnit::Bytes,
        },
    )
    .map_err(|_| "a working mapping was refused")?;
    u64::try_from(at).map_err(|_| "mmap returned an impossible address")
}

// ---------------------------------------------------------------------------
// The ELF loader
//
// `libs/elf` parses and is fuzzed; what it cannot check is the half that
// touches memory. These build an image for whichever architecture is running,
// load it into a real address space, and then read the result back out of the
// page tables -- which is the only place the answer actually lives.
// ---------------------------------------------------------------------------

/// Load a synthetic image and check everything about where it landed.
fn check_an_image_loads_where_its_headers_say(process: &Process) -> Result<(), &'static str> {
    let class = class_of_this_build();
    let file = image::build(class, arch::ARCH.elf_machine(), image::Shape::Good);

    let loaded = load::load(process.space(), &file).map_err(|_| "a good image was refused")?;

    if loaded.entry != image::ENTRY {
        return Err("the loader reported the wrong entry point");
    }
    // AT_PHDR must land inside the text segment, at the header offset the
    // image declares. musl reads its own PT_TLS through this, so a plausible
    // but wrong answer is worse than none.
    let expected_phdr = image::BASE + class.header_size() as u64;
    if loaded.phdr != expected_phdr {
        return Err("AT_PHDR does not point at the program headers");
    }
    if loaded.phnum != 2 {
        return Err("the loader miscounted the program headers");
    }

    // The file contents of the writable segment are where the headers said.
    let mut read = [0_u8; image::DATA_FILESZ];
    uaccess::copy_from_user(process.space(), image::DATA_VADDR, &mut read)
        .map_err(|_| "the loaded data segment was not readable")?;
    if read != image::DATA_MARK {
        return Err("the data segment's contents are not at its virtual address");
    }

    // And the `.bss` tail past `p_filesz` is zero, which it gets for free from
    // a committed anonymous page -- the loader must not have copied anything
    // over it, and must not have left it unmapped either.
    let mut tail = [0xFF_u8; 32];
    uaccess::copy_from_user(
        process.space(),
        image::DATA_VADDR + image::DATA_FILESZ as u64,
        &mut tail,
    )
    .map_err(|_| "the bss tail was not mapped")?;
    if tail.iter().any(|&b| b != 0) {
        return Err("the bss tail is not zero");
    }

    check_the_segments_got_their_own_permissions(process)?;

    let end = loaded.end;
    let _ = process.space().unmap(image::BASE, end - image::BASE);
    Ok(())
}

/// The text segment is executable and not writable; the data segment is the
/// other way round.
///
/// Asked of the address space rather than of the loader, because the loader
/// reporting what it meant to do proves nothing about what it did.
fn check_the_segments_got_their_own_permissions(process: &Process) -> Result<(), &'static str> {
    // Writing into the text segment must be refused: it is read-execute.
    if uaccess::copy_to_user(process.space(), image::BASE, b"x").is_ok() {
        return Err("the text segment was left writable");
    }
    // Reading it must work.
    let mut magic = [0_u8; 4];
    uaccess::copy_from_user(process.space(), image::BASE, &mut magic)
        .map_err(|_| "the text segment was not readable")?;
    if magic != [0x7F, b'E', b'L', b'F'] {
        return Err("the text segment does not hold the image it was loaded from");
    }
    // The data segment is writable.
    uaccess::copy_to_user(process.space(), image::DATA_VADDR, b"w")
        .map_err(|_| "the data segment was not writable")?;
    Ok(())
}

/// The four images the loader must refuse, and refuse by name.
fn check_the_loader_refuses_what_it_cannot_run(process: &Process) -> Result<(), &'static str> {
    let class = class_of_this_build();
    let machine = arch::ARCH.elf_machine();

    let cases = [
        (
            image::Shape::ForeignMachine,
            "an image for another architecture",
        ),
        (
            image::Shape::PositionIndependent,
            "a position-independent image",
        ),
        (
            image::Shape::WriteExecute,
            "an image needing a write-execute page",
        ),
    ];
    for (shape, _what) in cases {
        let file = image::build(class, machine, shape);
        // Each refusal gets a space of its own: a failed load may have mapped
        // part of the image, and that is exactly why `execve` will load into a
        // fresh space and swap it in only on success.
        let scratch = process::new_for_check().map_err(|_| "could not make a process")?;
        if load::load(scratch.space(), &file).is_ok() {
            return Err("the loader accepted an image it cannot run");
        }
    }

    // And bytes that are not an ELF at all.
    let scratch = process::new_for_check().map_err(|_| "could not make a process")?;
    if load::load(scratch.space(), b"not an ELF image").is_ok() {
        return Err("the loader accepted something that is not an ELF image");
    }
    let _ = process;
    Ok(())
}

/// The ELF class this kernel's own architecture uses.
///
/// From the pointer width rather than from a `cfg`, because generic kernel
/// code naming an architecture is what the layering check forbids -- and
/// because the question really is about width.
fn class_of_this_build() -> Class {
    if size_of::<usize>() == 8 {
        Class::Elf64
    } else {
        Class::Elf32
    }
}

// ---------------------------------------------------------------------------
// `write` and `writev`
//
// The bytes really do reach the console, which is checked the only way it can
// be from in here: by writing a marker the boot test's own log will carry. If
// the line below this check's report is missing from the serial log, `write`
// did not work, whatever this file claims.
// ---------------------------------------------------------------------------

/// `write` copies from the program's memory and reports what it wrote.
fn check_write_reaches_the_console(process: &Process) -> Result<(), &'static str> {
    let at = map_rw(process, PAGE_SIZE)?;
    let message = b"  hello   from a user buffer, by way of write(2)\n";
    uaccess::copy_to_user(process.space(), at, message)
        .map_err(|_| "could not stage the message")?;

    let written = file::sys_write(process, 1, at, message.len() as u64)
        .map_err(|_| "write to fd 1 was refused")?;
    if written != message.len() {
        return Err("write reported the wrong count");
    }

    // A zero-length write is not a no-op: it still validates the descriptor.
    if file::sys_write(process, 1, at, 0) != Ok(0) {
        return Err("a zero-length write to a good descriptor was refused");
    }
    if file::sys_write(process, 7, at, 1) != Err(Errno::EBADF) {
        return Err("a descriptor that names nothing was not EBADF");
    }
    // And EBADF is decided before the buffer is looked at, so a bad descriptor
    // with a wild pointer is EBADF and not EFAULT.
    if file::sys_write(process, 7, KERNEL_HALF_BASE, 1) != Err(Errno::EBADF) {
        return Err("a bad descriptor with a kernel pointer did not report EBADF");
    }
    // A good descriptor with a pointer into the kernel is EFAULT.
    if file::sys_write(process, 1, KERNEL_HALF_BASE, 1) != Err(Errno::EFAULT) {
        return Err("writing from a kernel address was not EFAULT");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// `writev` gathers the segments in order, and checks the whole array first.
fn check_writev_gathers_in_order(process: &Process) -> Result<(), &'static str> {
    let at = map_rw(process, PAGE_SIZE * 2)?;
    // The three pieces, laid out end to end; then an iovec array pointing at
    // them in an order that is *not* their order in memory, which is what
    // proves the gather follows the array rather than the addresses.
    let pieces: [&[u8]; 3] = [b"write(2)\n", b"  gathered by ", b"  three pieces, "];
    let mut offsets = [0_u64; 3];
    let mut cursor = at;
    for (slot, piece) in offsets.iter_mut().zip(pieces) {
        uaccess::copy_to_user(process.space(), cursor, piece)
            .map_err(|_| "could not stage a segment")?;
        *slot = cursor;
        cursor += piece.len() as u64;
    }

    // The array goes in the middle of the second page, well clear of the data.
    let array = at + PAGE_SIZE;
    let word = size_of::<usize>() as u64;
    let order = [2_usize, 1, 0];
    for (index, &which) in order.iter().enumerate() {
        let entry = array + (index as u64) * word * 2;
        let at = *offsets.get(which).ok_or("bad segment index")?;
        let piece = *pieces.get(which).ok_or("bad segment index")?;
        write_word(process, entry, at)?;
        write_word(process, entry + word, piece.len() as u64)?;
    }

    let total: usize = pieces.iter().map(|p| p.len()).sum();
    let written = file::sys_writev(process, 1, array, 3).map_err(|_| "writev was refused")?;
    if written != total {
        return Err("writev reported the wrong count");
    }

    // An impossible segment count is refused rather than walked.
    if file::sys_writev(process, 1, array, 100_000) != Err(Errno::EINVAL) {
        return Err("writev accepted more segments than IOV_MAX");
    }
    if file::sys_writev(process, 1, array, 0) != Ok(0) {
        return Err("writev with no segments was not zero");
    }

    let _ = memory::sys_munmap(process, at, PAGE_SIZE * 2).map_err(|_| "munmap was refused")?;
    Ok(())
}

/// Write one pointer-sized word into the program's memory.
fn write_word(process: &Process, at: u64, value: u64) -> Result<(), &'static str> {
    let bytes = value.to_le_bytes();
    let width = size_of::<usize>();
    let slot = bytes.get(..width).ok_or("impossible pointer width")?;
    uaccess::copy_to_user(process.space(), at, slot).map_err(|_| "could not stage a word")
}

// ---------------------------------------------------------------------------
// Descriptors
//
// A file under `/tmp`, through the handlers: created, written, sought, read
// back through a second descriptor that shares its offset, changed with
// `fcntl`, truncated and closed. `libs/vfs` tests the same rules on the host;
// what only this can test is the layer between -- arguments narrowed as the
// ABI narrows them, this architecture's `open` flag bits, the table lock let
// go before the file is touched, and every description closed and every page
// of the file given back, which the frame count around `check_handlers` sees.
//
// Called directly rather than through `dispatch`, like the handler checks
// above: `dispatch` finds its process through the running task, and the boot
// task has none.
// ---------------------------------------------------------------------------

/// The file the descriptor checks make, NUL-terminated as a program passes it.
const CHECK_PATH: &[u8] = b"/tmp/descriptor-check\0";

/// Its name within `/tmp`, for the check that opens it relative to a
/// directory descriptor.
const CHECK_NAME: &[u8] = b"descriptor-check\0";

/// `/tmp` itself.
const TMP_PATH: &[u8] = b"/tmp\0";

/// What the checks write into it.
const CHECK_DATA: &[u8] = b"descriptors, stage 8";

/// Where on the scratch page each piece goes.
const AT_PATH: u64 = 0;
/// See [`AT_PATH`].
const AT_NAME: u64 = 64;
/// See [`AT_PATH`].
const AT_TMP: u64 = 128;
/// See [`AT_PATH`].
const AT_DATA: u64 = 256;
/// See [`AT_PATH`].
const AT_BACK: u64 = 512;
/// See [`AT_PATH`].
const AT_RESULT: u64 = 1024;
/// See [`AT_PATH`].
const AT_FULL: u64 = 1536;

/// The file an `openat` at the descriptor limit is refused, and must not
/// leave behind.
const FULL_PATH: &[u8] = b"/tmp/descriptor-full\0";

/// Run the descriptor checks on a page of their own, and leave nothing behind:
/// every descriptor they opened closed, the file unlinked, the page unmapped.
fn check_descriptors(process: &Process) -> Result<(), &'static str> {
    let page = map_rw(process, PAGE_SIZE)?;
    for (offset, bytes) in [
        (AT_PATH, CHECK_PATH),
        (AT_NAME, CHECK_NAME),
        (AT_TMP, TMP_PATH),
        (AT_DATA, CHECK_DATA),
        (AT_FULL, FULL_PATH),
    ] {
        uaccess::copy_to_user(process.space(), page + offset, bytes)
            .map_err(|_| "could not stage the descriptor checks")?;
    }

    let outcome = check_a_new_process_has_the_console(process)
        .and_then(|()| check_a_full_table_creates_nothing(process, page))
        .and_then(|()| check_a_file_opens_on_the_lowest_free_descriptor(process, page))
        .and_then(|()| check_a_dup_shares_the_offset(process, page))
        .and_then(|()| check_fcntl_and_dup3_follow_linux(process, page))
        .and_then(|()| check_descriptors_are_refused_by_kind(process, page));

    // Cleaned up whatever happened, so that a failure is reported as itself
    // and not also as leaked frames.
    for fd in 3..32 {
        let _ = fd::sys_close(process, fd);
    }
    let namespace = crate::fs::namespace();
    for staged in [CHECK_PATH, FULL_PATH] {
        let path = staged.strip_suffix(b"\0").unwrap_or(staged);
        let _ = namespace.unlink(&namespace.context(), None, path);
    }
    let _ = memory::sys_munmap(process, page, PAGE_SIZE);
    outcome
}

/// Require a handler to have answered `want`.
fn answers(got: Result<usize, Errno>, want: usize, what: &'static str) -> Result<(), &'static str> {
    if got == Ok(want) { Ok(()) } else { Err(what) }
}

/// Require a handler to have refused with `errno`.
fn refuses(
    got: Result<usize, Errno>,
    errno: Errno,
    what: &'static str,
) -> Result<(), &'static str> {
    if got == Err(errno) { Ok(()) } else { Err(what) }
}

/// Descriptors 0, 1 and 2 are the console, open for reading and writing --
/// which is what busybox's `printf` asks of descriptor 1 before it prints.
fn check_a_new_process_has_the_console(process: &Process) -> Result<(), &'static str> {
    for fd in 0..3 {
        answers(
            fd::sys_fcntl(process, fd, F_GETFL, 0),
            O_RDWR as usize,
            "a new process's standard descriptor did not report O_RDWR from F_GETFL",
        )?;
    }
    refuses(
        fd::sys_lseek(process, 1, 0, SEEK_CUR),
        Errno::ESPIPE,
        "the console could be sought, as if it were a file",
    )?;
    refuses(
        fd::sys_ioctl(process, 99, TCGETS, 0),
        Errno::EBADF,
        "an ioctl on a closed descriptor was not EBADF",
    )
}

/// `openat` with every descriptor taken is `EMFILE` and creates nothing: the
/// same `O_CREAT|O_EXCL` succeeds once a descriptor is free again, where a
/// file left behind by the refused call would make it `EEXIST`. Linux takes
/// the descriptor number before it touches the path, for this reason.
fn check_a_full_table_creates_nothing(process: &Process, page: u64) -> Result<(), &'static str> {
    let flags = O_RDWR | O_CREAT | O_EXCL;
    let limit = process.files().lock().limit();
    // Descriptors 0, 1 and 2 are the console, so a limit of three leaves
    // none free.
    process
        .files()
        .lock()
        .set_limit(3)
        .map_err(|_| "could not lower the descriptor limit")?;
    let refused = fd::sys_openat(process, AT_FDCWD, page + AT_FULL, flags, 0o644);
    let restored = process.files().lock().set_limit(limit);
    restored.map_err(|_| "could not restore the descriptor limit")?;
    refuses(
        refused,
        Errno::EMFILE,
        "openat with every descriptor taken was not EMFILE",
    )?;
    let opened = fd::sys_openat(process, AT_FDCWD, page + AT_FULL, flags, 0o644);
    if let Ok(fd) = opened {
        let _ = fd::sys_close(process, i32::try_from(fd).unwrap_or(-1));
    }
    let path = FULL_PATH.strip_suffix(b"\0").unwrap_or(FULL_PATH);
    let namespace = crate::fs::namespace();
    let _ = namespace.unlink(&namespace.context(), None, path);
    answers(
        opened,
        3,
        "openat refused for EMFILE had created its file anyway",
    )
}

/// `openat` with `O_CREAT` lands on descriptor 3, close-on-exec as asked, and
/// `F_SETFD` takes the flag away again.
fn check_a_file_opens_on_the_lowest_free_descriptor(
    process: &Process,
    page: u64,
) -> Result<(), &'static str> {
    let flags = O_RDWR | O_CREAT | O_TRUNC | O_CLOEXEC;
    answers(
        fd::sys_openat(process, AT_FDCWD, page + AT_PATH, flags, 0o644),
        3,
        "a file created under /tmp did not open on descriptor 3",
    )?;
    answers(
        fd::sys_fcntl(process, 3, F_GETFD, 0),
        FD_CLOEXEC as usize,
        "O_CLOEXEC did not reach the descriptor",
    )?;
    answers(
        fd::sys_fcntl(process, 3, F_SETFD, 0),
        0,
        "F_SETFD was refused",
    )?;
    answers(
        fd::sys_fcntl(process, 3, F_GETFD, 0),
        0,
        "F_SETFD did not clear close-on-exec",
    )?;
    answers(
        fd::sys_fcntl(process, 3, F_GETFL, 0),
        O_RDWR as usize,
        "a file opened O_RDWR did not report it from F_GETFL",
    )
}

/// A write, then a read back through `dup`'s descriptor, which shares the
/// offset -- and `pread64` and `pwrite64`, which leave it alone.
fn check_a_dup_shares_the_offset(process: &Process, page: u64) -> Result<(), &'static str> {
    let len = CHECK_DATA.len();
    let back = page + AT_BACK;
    answers(
        file::sys_write(process, 3, page + AT_DATA, len as u64),
        len,
        "a write to a file under /tmp was short",
    )?;
    answers(fd::sys_dup(process, 3), 4, "dup did not take descriptor 4")?;
    answers(
        fd::sys_lseek(process, 4, 0, SEEK_CUR),
        len,
        "a duplicated descriptor does not share the offset the write moved",
    )?;
    answers(
        fd::sys_lseek(process, 3, 0, SEEK_SET),
        0,
        "SEEK_SET was refused",
    )?;
    answers(
        file::sys_read(process, 4, back, 64),
        len,
        "reading back through the duplicate did not start where the seek put the shared offset",
    )?;
    let mut read = [0_u8; 64];
    let read = read.get_mut(..len).ok_or("impossible check data length")?;
    uaccess::copy_from_user(process.space(), back, read).map_err(|_| "could not read back")?;
    if read != CHECK_DATA {
        return Err("what was read back is not what was written");
    }
    answers(
        file::sys_read(process, 4, back, 64),
        0,
        "a read at the end was not end of file",
    )?;

    answers(
        file::sys_pwrite64(process, 3, page + AT_DATA, 3, 0),
        3,
        "pwrite64 was short",
    )?;
    answers(
        file::sys_pread64(process, 4, back, 5, 3),
        5,
        "pread64 did not read five bytes from the middle",
    )?;
    answers(
        fd::sys_lseek(process, 3, 0, SEEK_CUR),
        len,
        "pread64 or pwrite64 moved the offset",
    )?;
    refuses(
        file::sys_pread64(process, 3, back, 1, -1),
        Errno::EINVAL,
        "pread64 accepted a negative offset",
    )
}

/// `F_DUPFD`, `F_SETFL`, `dup3`, `ftruncate` and `_llseek`, and what each of
/// them refuses.
fn check_fcntl_and_dup3_follow_linux(process: &Process, page: u64) -> Result<(), &'static str> {
    let len = CHECK_DATA.len();
    answers(
        fd::sys_fcntl(process, 3, F_DUPFD, 10),
        10,
        "F_DUPFD did not start at 10",
    )?;
    answers(
        fd::sys_fcntl(process, 3, F_DUPFD_CLOEXEC, 10),
        11,
        "F_DUPFD_CLOEXEC did not take the next free descriptor",
    )?;
    answers(
        fd::sys_fcntl(process, 11, F_GETFD, 0),
        FD_CLOEXEC as usize,
        "F_DUPFD_CLOEXEC did not set close-on-exec",
    )?;
    refuses(
        fd::sys_fcntl(process, 3, 999, 0),
        Errno::EINVAL,
        "an unknown fcntl was not EINVAL",
    )?;
    refuses(
        fd::sys_fcntl(process, 99, 999, 0),
        Errno::EBADF,
        "an unknown fcntl on a closed descriptor was not EBADF",
    )?;
    refuses(
        fd::sys_dup3(process, 3, 3, 0),
        Errno::EINVAL,
        "dup3 onto itself was not EINVAL",
    )?;
    refuses(
        fd::sys_dup3(process, 3, 20, 1),
        Errno::EINVAL,
        "dup3 accepted an unknown flag",
    )?;
    answers(
        fd::sys_dup3(process, 3, 20, O_CLOEXEC),
        20,
        "dup3 did not install at 20",
    )?;
    answers(
        fd::sys_fcntl(process, 20, F_GETFD, 0),
        FD_CLOEXEC as usize,
        "dup3's O_CLOEXEC did not reach the descriptor",
    )?;
    answers(
        fd::sys_dup2(process, 3, 3),
        3,
        "dup2 onto itself was not a no-op",
    )?;

    // O_APPEND through F_SETFL: a write after seeking to the start still
    // lands at the end.
    answers(
        fd::sys_fcntl(process, 3, F_SETFL, u64::from(O_APPEND)),
        0,
        "F_SETFL was refused",
    )?;
    answers(
        fd::sys_fcntl(process, 4, F_GETFL, 0),
        (O_RDWR | O_APPEND) as usize,
        "O_APPEND set on one descriptor did not show on its duplicate",
    )?;
    answers(
        fd::sys_lseek(process, 3, 0, SEEK_SET),
        0,
        "SEEK_SET was refused",
    )?;
    answers(
        file::sys_write(process, 3, page + AT_DATA, 1),
        1,
        "an append was short",
    )?;
    answers(
        fd::sys_lseek(process, 3, 0, SEEK_CUR),
        len + 1,
        "a write under O_APPEND did not land at the end",
    )?;

    answers(fd::sys_ftruncate(process, 3, 4), 0, "ftruncate was refused")?;
    answers(
        fd::sys_lseek(process, 4, 0, SEEK_END),
        4,
        "the file was not four bytes long after ftruncate",
    )?;
    refuses(
        fd::sys_ftruncate(process, 3, -1),
        Errno::EINVAL,
        "ftruncate accepted a negative length",
    )?;

    let result = page + AT_RESULT;
    answers(
        fd::sys_llseek(process, 3, 0, 2, result, SEEK_SET),
        0,
        "_llseek was refused",
    )?;
    let mut offset = [0_u8; 8];
    uaccess::copy_from_user(process.space(), result, &mut offset)
        .map_err(|_| "could not read _llseek's result")?;
    if u64::from_le_bytes(offset) != 2 {
        return Err("_llseek did not write the new offset through its pointer");
    }
    Ok(())
}

/// A directory descriptor as a starting point, `O_DIRECTORY` with this
/// architecture's bit, and the refusals that depend on what a descriptor names.
fn check_descriptors_are_refused_by_kind(process: &Process, page: u64) -> Result<(), &'static str> {
    let directory = arch::OPEN_FLAGS.directory;
    refuses(
        fd::sys_openat(process, AT_FDCWD, page + AT_PATH, O_RDONLY | directory, 0),
        Errno::ENOTDIR,
        "O_DIRECTORY, in this architecture's bits, opened a regular file",
    )?;
    let tmp = fd::sys_openat(process, AT_FDCWD, page + AT_TMP, O_RDONLY | directory, 0)
        .map_err(|_| "/tmp did not open as a directory")?;
    let tmp = i32::try_from(tmp).map_err(|_| "an impossible descriptor")?;

    match fd::start_location(process, AT_FDCWD) {
        Ok(None) => {}
        _ => return Err("AT_FDCWD did not mean the working directory"),
    }
    if !matches!(fd::start_location(process, tmp), Ok(Some(_))) {
        return Err("a directory descriptor was not a starting point");
    }
    if !matches!(fd::start_location(process, 3), Err(Errno::ENOTDIR)) {
        return Err("a file descriptor was accepted as a starting point");
    }
    if !matches!(fd::start_location(process, 99), Err(Errno::EBADF)) {
        return Err("a closed descriptor was accepted as a starting point");
    }

    let relative = fd::sys_openat(process, tmp, page + AT_NAME, O_RDONLY, 0)
        .map_err(|_| "a name relative to a directory descriptor did not open")?;
    let relative = i32::try_from(relative).map_err(|_| "an impossible descriptor")?;
    refuses(
        fd::sys_ioctl(process, relative, TCGETS, page + AT_RESULT),
        Errno::ENOTTY,
        "a regular file answered TCGETS, as if it were a terminal",
    )?;
    refuses(
        fd::sys_ioctl(process, relative, TIOCGWINSZ, page + AT_RESULT),
        Errno::ENOTTY,
        "a regular file answered TIOCGWINSZ, as if it were a terminal",
    )?;
    refuses(
        file::sys_write(process, relative, page + AT_DATA, 1),
        Errno::EBADF,
        "a descriptor opened O_RDONLY was written",
    )?;
    refuses(
        file::sys_read(process, tmp, page + AT_BACK, 1),
        Errno::EISDIR,
        "a directory was read as a file",
    )?;

    answers(fd::sys_close(process, relative), 0, "close was refused")?;
    refuses(
        fd::sys_close(process, relative),
        Errno::EBADF,
        "a second close was not EBADF",
    )?;
    refuses(
        file::sys_read(process, relative, page + AT_BACK, 1),
        Errno::EBADF,
        "a closed descriptor was read",
    )
}

// ---------------------------------------------------------------------------
// Ring 3
//
// The one check here that cannot be faked. Everything above it calls handlers
// from kernel code with a `Process` in hand; this hands the processor to an
// address space the kernel built, at a privilege level where none of the
// kernel's own memory is reachable, and waits to be asked for something.
//
// If the loader mapped the wrong page, the stack image put `argc` in the wrong
// place, the trampoline mismatched its pushes, or `swapgs` went the wrong way,
// the result is not a wrong answer. It is a fault in ring 3 with no handler
// that can say anything useful -- which is why every part of this was checked
// separately first.
// ---------------------------------------------------------------------------

/// Run a program in user mode and require it to come back correctly.
fn check_a_program_runs_in_user_mode() -> Result<Option<i32>, &'static str> {
    if arch::USER_TEST_PROGRAM.is_empty() {
        // No transition on this architecture yet. Reported as absent rather
        // than skipped silently: the boot log should say which architectures
        // can do this and which cannot.
        return Ok(None);
    }

    let file = image::build_with(
        class_of_this_build(),
        arch::ARCH.elf_machine(),
        image::Shape::Good,
        arch::USER_TEST_PROGRAM,
    );

    let faults_before = crate::trap::handled_fault_count();
    let status = exec::run(
        &file,
        &[b"/hello", b"--first"],
        &[b"FERRIX=1"],
        [0x5a; ferrix_ustack::RANDOM_BYTES],
    )
    .map_err(|_| "the program could not be started")?;

    // Stage 6's exit criterion is a program that runs "with a page fault
    // serviced along the way", so require one rather than assume it.
    //
    // Today the fault is incidental, and that is exactly why it is asserted.
    // The loader maps the image writable, copies it in -- which faults every
    // page in -- and then narrows the text to executable with `protect`, which
    // takes those translations down. The program's first instruction fetch
    // re-faults the page. A natural fix to `protect`, rewriting permissions in
    // place instead of unmapping, would make that fault disappear with every
    // other check still passing, and the criterion would quietly stop being
    // exercised. This makes that visible instead.
    if crate::trap::handled_fault_count().saturating_sub(faults_before) == 0 {
        return Err("the program ran without a page fault being serviced");
    }

    if status != arch::USER_TEST_STATUS {
        return Err("the program exited with the wrong status");
    }
    Ok(Some(status))
}

// ---------------------------------------------------------------------------
// Programs as tasks
//
// Two programs at once, and one ended from outside. What the single-program
// check above cannot show: that a program is preempted in user mode, which
// needs interrupts open there; that two programs keep their own registers,
// address spaces and exit statuses while taking turns; and that a program
// which never calls `exit_group` can still be ended.
// ---------------------------------------------------------------------------

/// Loop iterations each spinning program runs before it writes and exits.
///
/// Enough to cover several scheduling slices under KVM, where the loop is
/// fastest, and a second or so under `tcg`, where it is slowest.
const SPIN_ROUNDS: u32 = 30_000_000;

/// The status a spinning program exits with when its stack pointer changed
/// across the loop, which is to say while it was preempted.
const SPIN_STACK_CHANGED: i32 = 99;

/// How long the checks wait for a program before calling it lost.
const PROGRAM_PATIENCE_NANOS: u64 = 120_000_000_000;

/// How long the killed program is left spinning first.
const KILL_AFTER_NANOS: u64 = 20_000_000;

/// How soon after its kill a spinning program's task must be gone.
///
/// Far shorter than the program would take to finish its loop on its own, so a
/// kill that did not reach it fails here rather than passing late.
const KILL_REACH_NANOS: u64 = 1_000_000_000;

/// The status a program is killed with: 128 plus `SIGKILL`, which is what a
/// shell reports for one.
const KILL_STATUS: i32 = 137;

/// Build a spinning program tagged `tag` that loops `rounds` times and exits
/// with `status`, and load it into a process of its own.
pub(crate) fn spinner(tag: u8, rounds: u32, status: u32) -> Result<Arc<Process>, &'static str> {
    let mut program = arch::USER_SPIN_PROGRAM.to_vec();
    let at = program
        .len()
        .checked_sub(10)
        .ok_or("the spinning program is shorter than its own layout")?;

    let mut tail = [0_u8; 10];
    tail[0] = tag;
    tail[1] = b'\n';
    let words = rounds.to_le_bytes().into_iter().chain(status.to_le_bytes());
    for (slot, byte) in tail.iter_mut().skip(2).zip(words) {
        *slot = byte;
    }
    let slot = program
        .get_mut(at..)
        .ok_or("the spinning program is shorter than its own layout")?;
    if slot.first() != Some(&b'?') || slot.get(1) != Some(&b'\n') {
        return Err("the spinning program's tail is not the layout it documents");
    }
    slot.copy_from_slice(&tail);

    let file = image::build_with(
        class_of_this_build(),
        arch::ARCH.elf_machine(),
        image::Shape::Good,
        &program,
    );
    // The second spinner's argument vector is longer by more than a stack
    // alignment, so the two programs' stack pointers differ. With identical
    // startup stacks, a kernel that handed one program the other's stack
    // pointer would pass the comparison each makes.
    let args: &[&[u8]] = if tag == b'2' {
        &[
            b"/spin",
            b"an argument long enough to move the stack by more than sixteen bytes",
        ]
    } else {
        &[b"/spin"]
    };
    process::load(&file, args, &[], [0x5a; ferrix_ustack::RANDOM_BYTES])
        .map_err(|_| "a spinning program could not be loaded")
}

/// Two programs pinned to one processor both finish, each with its own
/// status, and each is switched to more than once.
///
/// Two properties, because the obvious one is not enough. Each program must be
/// switched to more than once, or they ran one after the other. But that alone
/// passes with interrupts masked in user mode: each program's `write` opens
/// them inside the kernel, a tick that was pending all along is taken there,
/// and the program is switched away from and back to without ever having been
/// preempted in user mode. So interrupts must also have arrived *in user mode*
/// while the two ran. Masking them there fails this with its own message; that
/// was tried, and the switch count alone did not notice.
fn check_two_programs_take_turns_on_one_processor() -> Result<Option<(u64, u64)>, &'static str> {
    if arch::USER_SPIN_PROGRAM.is_empty() {
        return Ok(None);
    }
    let here = crate::smp::this_cpu()
        .ok_or("no processor to run two programs on")?
        .logical;

    let user_interrupts = crate::trap::user_interrupt_count();
    let first = spinner(b'1', SPIN_ROUNDS, 41)?;
    let second = spinner(b'2', SPIN_ROUNDS, 43)?;
    let first_task = process::start_on(&first, Some(here))
        .map_err(|_| "the first of two programs could not be started")?;
    let second_task = process::start_on(&second, Some(here))
        .map_err(|_| "the second of two programs could not be started")?;

    let deadline = crate::timer::now_nanos().saturating_add(PROGRAM_PATIENCE_NANOS);
    let statuses = (
        first.wait_for_exit(deadline),
        second.wait_for_exit(deadline),
    );
    if statuses.0 == Some(SPIN_STACK_CHANGED) || statuses.1 == Some(SPIN_STACK_CHANGED) {
        return Err("a program's stack pointer changed while another program ran on its processor");
    }
    if statuses.0 != Some(41) {
        return Err("the first of two programs did not exit with its own status");
    }
    if statuses.1 != Some(43) {
        return Err("the second of two programs did not exit with its own status");
    }

    let switched = (first_task.switches(), second_task.switches());
    if switched.0 < 2 || switched.1 < 2 {
        return Err(
            "two programs on one processor ran one after the other rather than taking turns",
        );
    }
    if crate::trap::user_interrupt_count() == user_interrupts {
        return Err("no interrupt arrived while two spinning programs were in user mode");
    }
    Ok(Some(switched))
}

/// A program that would spin for minutes is ended from outside: it reports
/// the status it was killed with, not its own, and its task actually stops.
fn check_a_program_is_killed_from_outside() -> Result<Option<i32>, &'static str> {
    if arch::USER_SPIN_PROGRAM.is_empty() {
        return Ok(None);
    }
    let here = crate::smp::this_cpu()
        .ok_or("no processor to run a program on")?
        .logical;
    // On another processor from this one, when there is one, so the victim
    // spins there alone. A task alone on its processor gets no timer tick, so
    // only the interrupt `kill` sends can bring it back through the kernel;
    // a victim sharing this processor would be reached by this checker's own
    // wake-ups and prove nothing about that.
    let count = crate::smp::count();
    let elsewhere = if count > 1 { (here + 1) % count } else { here };

    let victim = spinner(b'k', u32::MAX, 5)?;
    let task = process::start_on(&victim, Some(elsewhere))
        .map_err(|_| "a program to kill could not be started")?;
    crate::sched::sleep_for(KILL_AFTER_NANOS);
    if victim.is_terminated() {
        return Err(
            "a program that should still have been spinning had already ended, so nothing \
             preempted it in user mode",
        );
    }

    process::kill(&victim, KILL_STATUS);
    let deadline = crate::timer::now_nanos().saturating_add(PROGRAM_PATIENCE_NANOS);
    if victim.wait_for_exit(deadline) != Some(KILL_STATUS) {
        return Err("a killed program did not report the status it was killed with");
    }

    // The status alone would pass with a task still spinning in user mode
    // under a process that says it has ended -- and so would a generous
    // deadline, because the loop does end eventually. So the task must be gone
    // soon, not merely at some point.
    let reach = crate::timer::now_nanos().saturating_add(KILL_REACH_NANOS);
    while !task.is_dead() {
        if crate::timer::now_nanos() >= reach {
            return Err("a killed program kept running on its processor after its kill");
        }
        crate::sched::sleep_for(1_000_000);
    }
    Ok(Some(KILL_STATUS))
}

// ---------------------------------------------------------------------------
// Stage 8: the calls that take a path
// ---------------------------------------------------------------------------

pub(crate) use paths::run_paths;

/// The path calls, against the real namespace under `/tmp`.
///
/// Every call goes in by its number, decoded by this build's own table, and
/// through the table `dispatch` uses once it has found a process -- so the one
/// line that hands path calls to `syscall::path` is exercised with the
/// handlers. Names and buffers live in a real user address space, and every
/// `stat` record is decoded back out of it in this architecture's layout.
///
/// A module of its own inside this file so that its imports stay its own.
mod paths {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::mem::offset_of;

    use ferrix_bootinfo::PAGE_SIZE;
    use ferrix_linux_abi::errno::Errno;
    use ferrix_linux_abi::nr::Syscall;
    use ferrix_linux_abi::types::{
        self, AT_EMPTY_PATH, AT_FDCWD, AT_REMOVEDIR, AT_SYMLINK_NOFOLLOW, DT_REG, R_OK,
        RENAME_EXCHANGE, RENAME_NOREPLACE, S_IFLNK, S_IFREG, STATX_BASIC_STATS, Statx, UTIME_OMIT,
        W_OK, X_OK,
    };
    use ferrix_vfs::dirent::{self, Record};
    use ferrix_vfs::{FileType, Metadata, OpenFlags, Stat, Timespec};

    use super::{map_rw, number_for};
    use crate::arch;
    use crate::fs;
    use crate::mm;
    use crate::syscall::process::{self, Process};
    use crate::syscall::stat::StatLayout;
    use crate::syscall::{SyscallArgs, memory, uaccess};

    /// What the path checks measured, for the boot log.
    #[derive(Debug)]
    pub(crate) struct PathReport {
        /// Calls made on the measured run.
        pub(crate) calls: u32,
        /// Names `getdents64` reported from the directory it read in pieces.
        pub(crate) listed: usize,
        /// How many `getdents64` calls that took.
        pub(crate) listing_calls: u32,
        /// Frames the measured run cost once everything was removed. Zero, or
        /// a path call is leaking.
        pub(crate) leaked: i64,
        /// Dentries the namespace's cache held after the measured run that it
        /// did not hold before it. Reported beside `leaked` because the cache
        /// is the one thing that may legitimately keep memory across runs, and
        /// a frame it keeps is not a frame a call lost.
        pub(crate) cache_growth: i64,
    }

    /// Where the checks work. The last of them removes it again.
    const ROOT: &[u8] = b"/tmp/pathcheck";

    /// How many names the listing check makes.
    const LISTED: usize = 40;

    /// The `getdents64` buffer the listing is read with: four short entries.
    const LISTING_BUFFER: u64 = 96;

    /// What the symbolic link points at. Nothing: a dangling link is still a
    /// link, and following it must say so.
    ///
    /// Absolute, and directly in `/tmp`, for the same reason as [`LINK`].
    const TARGET: &[u8] = b"/tmp/pathcheck-nowhere";

    /// Where the link is made, and where the rename check looks for it after
    /// moving it away.
    ///
    /// In `/tmp` rather than under [`ROOT`], so that a lookup that misses
    /// leaves its negative dentry in a directory that outlives the run, where
    /// the next run finds it and reuses it. A negative entry cached under
    /// `ROOT` would keep `ROOT`'s own dentry alive after `rmdir`, and the
    /// second run's measurement would count a directory the cache kept as a
    /// leak.
    const LINK: &[u8] = b"/tmp/pathcheck-link";

    /// `AT_FDCWD`, as a register carries it.
    const CWD: u64 = AT_FDCWD as i64 as u64;

    /// Where the second string argument of a call is staged.
    const SECOND: u64 = PAGE_SIZE / 2;

    /// Run the checks twice and measure the second run, for the reason
    /// `check::run` gives: the first pays for size classes the heap keeps.
    pub(crate) fn run_paths() -> Result<PathReport, &'static str> {
        // Settle first. `check_syscalls`, just before this, starts programs
        // whose tasks exit, and the idle loop frees a finished task's stack
        // and its process's address space whenever it next runs. One freed
        // inside the measured window raises the free count, which this check
        // read as a leak, and failed intermittently for it. So give those
        // tasks time to switch away, and reap until nothing is left.
        settle();
        let _warm = check_path_calls()?;
        let cached = fs::namespace().cached();
        let before = mm::free_frames();
        let mut report = check_path_calls()?;
        report.leaked = i64::try_from(before).unwrap_or(i64::MAX)
            - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);
        report.cache_growth = i64::try_from(fs::namespace().cached()).unwrap_or(i64::MAX)
            - i64::try_from(cached).unwrap_or(i64::MAX);
        // Checked, not only printed. The dentry cache is the one thing that
        // may keep memory across runs, and it is not subtracted, because by
        // construction it does not grow here: the second run finds the
        // dentries the first left in /tmp, which is why `LINK` lives there. A
        // run that grew it has stopped being repeatable, and is told apart
        // from a call that lost a frame.
        if report.leaked < 0 {
            return Err(
                "the free frame count rose across the path calls: something outside them freed frames in the window",
            );
        }
        if report.leaked != 0 && report.cache_growth != 0 {
            return Err("the path calls kept frames, and the dentry cache grew across the run");
        }
        if report.leaked != 0 {
            return Err("the path calls did not give every frame back");
        }
        Ok(report)
    }

    /// How long a finished task is given to switch away before it is reaped.
    const SETTLE_NANOS: u64 = 20_000_000;

    /// The most settle rounds before measuring anyway: a boot with tasks still
    /// finishing after this long has a problem the count will then name.
    const SETTLE_ROUNDS: usize = 10;

    /// Wait for tasks earlier checks started to finish, and reap them, so
    /// nothing is freed inside the measured window.
    fn settle() {
        for _ in 0..SETTLE_ROUNDS {
            crate::sched::sleep_for(SETTLE_NANOS);
            if crate::sched::reap() == 0 {
                break;
            }
        }
    }

    /// One run of every check, in a process of its own.
    fn check_path_calls() -> Result<PathReport, &'static str> {
        check_every_encoder_round_trips()?;
        let process = process::new_for_check().map_err(|_| "could not make a process")?;
        let mut p = Paths::new(&process)?;
        check_names_are_made_and_read(&mut p)?;
        check_renames_replace_only_when_allowed(&mut p)?;
        check_every_stat_describes_the_same_file(&mut p)?;
        let (listed, listing_calls) = check_a_listing_in_pieces_sees_each_name_once(&mut p)?;
        check_the_working_directory_follows_chdir(&mut p)?;
        check_access_and_attributes(&mut p)?;
        check_names_are_removed(&mut p)?;
        let calls = p.calls;
        p.release()?;
        Ok(PathReport {
            calls,
            listed,
            listing_calls,
            leaked: 0,
            cache_growth: 0,
        })
    }

    /// A process, a page for the strings a call takes, and a page for what it
    /// gives back.
    struct Paths<'a> {
        process: &'a Process,
        strings: u64,
        out: u64,
        calls: u32,
    }

    impl<'a> Paths<'a> {
        fn new(process: &'a Process) -> Result<Paths<'a>, &'static str> {
            Ok(Paths {
                process,
                strings: map_rw(process, PAGE_SIZE)?,
                out: map_rw(process, PAGE_SIZE)?,
                calls: 0,
            })
        }

        fn release(self) -> Result<(), &'static str> {
            for at in [self.strings, self.out] {
                let _ = memory::sys_munmap(self.process, at, PAGE_SIZE)
                    .map_err(|_| "munmap was refused")?;
            }
            Ok(())
        }

        /// Copy `bytes` into the program at `at`.
        fn stage(&self, at: u64, bytes: &[u8]) -> Result<u64, &'static str> {
            uaccess::copy_to_user(self.process.space(), at, bytes)
                .map_err(|_| "could not stage an argument")?;
            Ok(at)
        }

        /// A path argument, NUL-terminated.
        fn path(&self, text: &[u8]) -> Result<u64, &'static str> {
            let mut string = Vec::from(text);
            string.push(0);
            self.stage(self.strings, &string)
        }

        /// A second path argument, alongside [`Paths::path`]'s.
        fn second(&self, text: &[u8]) -> Result<u64, &'static str> {
            let mut string = Vec::from(text);
            string.push(0);
            self.stage(self.strings + SECOND, &string)
        }

        /// Make `call` as a program on this architecture would.
        fn call(&mut self, call: Syscall, args: [u64; 6]) -> Result<usize, Errno> {
            self.calls = self.calls.saturating_add(1);
            let number = number_for(call).ok_or(Errno::ENOSYS)?;
            let decoded = arch::decode_syscall(number).ok_or(Errno::ENOSYS)?;
            crate::syscall::handle(decoded, &SyscallArgs { number, args }, Some(self.process))
        }

        /// Fill the start of the output page with `byte`.
        fn fill_out(&self, len: usize, byte: u8) -> Result<(), &'static str> {
            let _ = self.stage(self.out, &vec![byte; len])?;
            Ok(())
        }

        /// The start of the output page.
        fn read_out(&self, len: usize) -> Result<Vec<u8>, &'static str> {
            let mut bytes = vec![0_u8; len];
            uaccess::copy_from_user(self.process.space(), self.out, &mut bytes)
                .map_err(|_| "could not read a result back")?;
            Ok(bytes)
        }
    }

    /// The first of `calls` this architecture has a number for.
    fn first_of(calls: &[Syscall]) -> Option<Syscall> {
        calls
            .iter()
            .copied()
            .find(|&call| number_for(call).is_some())
    }

    /// `newfstatat`, or `fstatat64` where that is the only form.
    fn fstatat() -> Result<Syscall, &'static str> {
        first_of(&[Syscall::Newfstatat, Syscall::Fstatat64])
            .ok_or("no fstatat on this architecture")
    }

    /// Open `path` and install it in the process's descriptor table.
    ///
    /// Directly rather than through `openat`, which belongs to the descriptor
    /// calls: what is checked here is the calls that take the descriptor.
    fn install(process: &Process, path: &[u8], directory: bool) -> Result<u64, &'static str> {
        let ns = fs::namespace();
        let flags = OpenFlags {
            read: true,
            directory,
            ..OpenFlags::default()
        };
        let file = ns
            .open(&ns.context(), None, path, &flags, 0)
            .map_err(|_| "could not open a file for a descriptor check")?;
        let fd = process
            .files()
            .lock()
            .insert(file, false)
            .map_err(|_| "the descriptor table was full")?;
        u64::try_from(fd).map_err(|_| "a negative descriptor")
    }

    /// Take a descriptor [`install`] made out of the table again.
    fn uninstall(process: &Process, fd: u64) -> Result<(), &'static str> {
        let fd = i32::try_from(fd).map_err(|_| "an impossible descriptor")?;
        let file = process.files().lock().remove(fd);
        file.map(drop).map_err(|_| "a descriptor vanished")
    }

    // -- stat records -------------------------------------------------------

    /// The fields of a `stat` record the checks compare.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Decoded {
        dev: u64,
        ino: u64,
        mode: u32,
        nlink: u64,
        uid: u64,
        size: u64,
        mtime: u64,
    }

    /// An unsigned little-endian field of `width` bytes.
    fn le(bytes: &[u8], at: usize, width: usize) -> Option<u64> {
        let mut word = [0_u8; 8];
        word.get_mut(..width)?
            .copy_from_slice(bytes.get(at..at.checked_add(width)?)?);
        Some(u64::from_le_bytes(word))
    }

    /// Read a record back the way a C library would, at the offsets the
    /// `libs/linux-abi` structure gives -- a second reading of the layout,
    /// independent of the encoder that wrote it.
    fn decode(layout: StatLayout, b: &[u8]) -> Option<Decoded> {
        use types::aarch64::Stat as Generic;
        use types::arm::Stat64;
        use types::x86_64::Stat as Legacy;
        Some(match layout {
            StatLayout::Legacy => Decoded {
                dev: le(b, offset_of!(Legacy, st_dev), 8)?,
                ino: le(b, offset_of!(Legacy, st_ino), 8)?,
                mode: u32::try_from(le(b, offset_of!(Legacy, st_mode), 4)?).ok()?,
                nlink: le(b, offset_of!(Legacy, st_nlink), 8)?,
                uid: le(b, offset_of!(Legacy, st_uid), 4)?,
                size: le(b, offset_of!(Legacy, st_size), 8)?,
                mtime: le(b, offset_of!(Legacy, st_mtime), 8)?,
            },
            StatLayout::Generic => Decoded {
                dev: le(b, offset_of!(Generic, st_dev), 8)?,
                ino: le(b, offset_of!(Generic, st_ino), 8)?,
                mode: u32::try_from(le(b, offset_of!(Generic, st_mode), 4)?).ok()?,
                nlink: le(b, offset_of!(Generic, st_nlink), 4)?,
                uid: le(b, offset_of!(Generic, st_uid), 4)?,
                size: le(b, offset_of!(Generic, st_size), 8)?,
                mtime: le(b, offset_of!(Generic, st_mtime), 8)?,
            },
            StatLayout::Stat64 => Decoded {
                dev: le(b, offset_of!(Stat64, st_dev), 8)?,
                ino: le(b, offset_of!(Stat64, st_ino), 8)?,
                mode: u32::try_from(le(b, offset_of!(Stat64, st_mode), 4)?).ok()?,
                nlink: le(b, offset_of!(Stat64, st_nlink), 4)?,
                uid: le(b, offset_of!(Stat64, st_uid), 4)?,
                size: le(b, offset_of!(Stat64, st_size), 8)?,
                mtime: le(b, offset_of!(Stat64, st_mtime), 4)?,
            },
        })
    }

    /// Every encoder puts every field where its layout says -- all three, on
    /// every architecture, not only the one this build answers with.
    ///
    /// The size is over four gibibytes and the inode number over 32 bits, so
    /// a field written at half its width, or a layout that truncates the size,
    /// cannot pass.
    fn check_every_encoder_round_trips() -> Result<(), &'static str> {
        let time = |tv_sec| Timespec { tv_sec, tv_nsec: 5 };
        let stat = Stat {
            dev: 0x0803,
            metadata: Metadata {
                ino: 0x1_0000_0042,
                kind: FileType::Regular,
                permissions: 0o640,
                nlink: 3,
                uid: 7,
                gid: 9,
                size: 0x1_2345_6789,
                rdev: 0,
                blocks: 11,
                block_size: 4096,
                atime: time(1000),
                mtime: time(2000),
                ctime: time(3000),
            },
        };
        let want = Decoded {
            dev: 0x0803,
            ino: 0x1_0000_0042,
            mode: S_IFREG | 0o640,
            nlink: 3,
            uid: 7,
            size: 0x1_2345_6789,
            mtime: 2000,
        };
        for layout in [StatLayout::Legacy, StatLayout::Generic, StatLayout::Stat64] {
            let bytes = layout.encode(&stat);
            if bytes.len() != layout.size() {
                return Err("a stat record is not the size of its layout");
            }
            if decode(layout, &bytes) != Some(want) {
                return Err("a stat encoder put a field where its layout does not");
            }
        }
        // `stat64`'s other inode field: the low half, for old readers.
        let stat64 = StatLayout::Stat64.encode(&stat);
        if le(&stat64, offset_of!(types::arm::Stat64, __st_ino), 4) != Some(0x42) {
            return Err("stat64's truncated inode field is not the low half");
        }
        Ok(())
    }

    /// Make `call` fill the output page with a record, and decode it --
    /// checking that it wrote exactly its layout's size and not a byte more.
    fn stat_into(
        p: &mut Paths<'_>,
        call: Syscall,
        args: [u64; 6],
    ) -> Result<Decoded, &'static str> {
        const SENTINEL: u8 = 0xA5;
        let size = arch::STAT_LAYOUT.size();
        p.fill_out(size + 8, SENTINEL)?;
        if p.call(call, args) != Ok(0) {
            return Err("a stat call was refused");
        }
        let bytes = p.read_out(size + 8)?;
        if bytes.get(size..) != Some(&[SENTINEL; 8][..]) {
            return Err("a stat call wrote past the end of its layout");
        }
        if bytes.get(size - 1) == Some(&SENTINEL) {
            return Err("a stat call did not write to the end of its layout");
        }
        decode(arch::STAT_LAYOUT, &bytes).ok_or("a stat record could not be decoded")
    }

    // -- the checks ---------------------------------------------------------

    /// `mkdirat`, `mknodat` and `symlinkat` make names, and `readlinkat` reads
    /// a link back unterminated and cut silently to the buffer.
    fn check_names_are_made_and_read(p: &mut Paths<'_>) -> Result<(), &'static str> {
        let root = p.path(ROOT)?;
        if p.call(Syscall::Mkdirat, [CWD, root, 0o777, 0, 0, 0]) != Ok(0) {
            return Err("mkdirat under /tmp was refused");
        }
        if p.call(Syscall::Mkdirat, [CWD, root, 0o777, 0, 0, 0]) != Err(Errno::EEXIST) {
            return Err("mkdirat over an existing name was not EEXIST");
        }
        // The file the stat checks describe, and the one the link is renamed
        // over: a rename onto an existing name, which is what `mv` does to a
        // file it replaces.
        let regular = u64::from(S_IFREG | 0o666);
        for name in [&b"/tmp/pathcheck/file"[..], b"/tmp/pathcheck/moved"] {
            let name = p.path(name)?;
            if p.call(Syscall::Mknodat, [CWD, name, regular, 0, 0, 0]) != Ok(0) {
                return Err("mknodat of a regular file was refused");
            }
        }
        let target = p.path(TARGET)?;
        let link = p.second(LINK)?;
        if p.call(Syscall::Symlinkat, [target, CWD, link, 0, 0, 0]) != Ok(0) {
            return Err("symlinkat was refused");
        }

        let link = p.path(LINK)?;
        p.fill_out(64, 0xEE)?;
        if p.call(Syscall::Readlinkat, [CWD, link, p.out, 64, 0, 0]) != Ok(TARGET.len()) {
            return Err("readlinkat did not report the target's length");
        }
        let read = p.read_out(TARGET.len() + 1)?;
        if read.get(..TARGET.len()) != Some(TARGET) || read.get(TARGET.len()) != Some(&0xEE) {
            return Err("readlinkat did not return the target, unterminated");
        }
        if p.call(Syscall::Readlinkat, [CWD, link, p.out, 4, 0, 0]) != Ok(4) {
            return Err("readlinkat did not cut the target to the buffer");
        }
        if p.call(Syscall::Readlinkat, [CWD, link, p.out, 0, 0, 0]) != Err(Errno::EINVAL) {
            return Err("readlinkat with no room was not EINVAL");
        }
        if p.call(Syscall::Readlinkat, [CWD, root, p.out, 64, 0, 0]) != Err(Errno::EINVAL) {
            // `root` still points at the first string slot, now the link's path,
            // so restage the directory before asking.
            let root = p.path(ROOT)?;
            if p.call(Syscall::Readlinkat, [CWD, root, p.out, 64, 0, 0]) != Err(Errno::EINVAL) {
                return Err("readlinkat of a directory was not EINVAL");
            }
        }
        Ok(())
    }

    /// `renameat2` moves a name across directories and over an existing file,
    /// refuses to replace one under `RENAME_NOREPLACE`, and refuses
    /// `RENAME_EXCHANGE` outright.
    fn check_renames_replace_only_when_allowed(p: &mut Paths<'_>) -> Result<(), &'static str> {
        let from = p.path(LINK)?;
        let to = p.second(b"/tmp/pathcheck/moved")?;
        if p.call(Syscall::Renameat2, [CWD, from, CWD, to, 0, 0]) != Ok(0) {
            return Err("renameat2 over an existing file was refused");
        }
        if p.call(Syscall::Readlinkat, [CWD, from, p.out, 64, 0, 0]) != Err(Errno::ENOENT) {
            return Err("a name survived being renamed away");
        }
        let to = p.path(b"/tmp/pathcheck/moved")?;
        if p.call(Syscall::Readlinkat, [CWD, to, p.out, 64, 0, 0]) != Ok(TARGET.len()) {
            return Err("the renamed link is not where it was renamed to");
        }
        let from = p.path(b"/tmp/pathcheck/moved")?;
        let to = p.second(b"/tmp/pathcheck/file")?;
        let noreplace = u64::from(RENAME_NOREPLACE);
        if p.call(Syscall::Renameat2, [CWD, from, CWD, to, noreplace, 0]) != Err(Errno::EEXIST) {
            return Err("RENAME_NOREPLACE replaced a name");
        }
        let exchange = u64::from(RENAME_EXCHANGE);
        if p.call(Syscall::Renameat2, [CWD, from, CWD, to, exchange, 0]) != Err(Errno::EINVAL) {
            return Err("RENAME_EXCHANGE was not EINVAL");
        }
        Ok(())
    }

    /// Every form of `stat` this architecture has describes the same file the
    /// same way, and `statx` agrees with them.
    fn check_every_stat_describes_the_same_file(p: &mut Paths<'_>) -> Result<(), &'static str> {
        let at = fstatat()?;
        let file = p.path(b"/tmp/pathcheck/file")?;
        let by_path = stat_into(p, at, [CWD, file, p.out, 0, 0, 0])?;
        // 0o666 through the default umask of 0o022.
        if by_path.mode != S_IFREG | 0o644 || by_path.nlink != 1 || by_path.size != 0 {
            return Err("fstatat did not describe a new file with the umask applied");
        }

        let link = p.path(b"/tmp/pathcheck/moved")?;
        let nofollow = u64::from(AT_SYMLINK_NOFOLLOW);
        let as_link = stat_into(p, at, [CWD, link, p.out, nofollow, 0, 0])?;
        if as_link.mode != S_IFLNK | 0o777 || as_link.size != TARGET.len() as u64 {
            return Err("AT_SYMLINK_NOFOLLOW did not describe the link itself");
        }
        if p.call(at, [CWD, link, p.out, 0, 0, 0]) != Err(Errno::ENOENT) {
            return Err("following a dangling link found something");
        }
        if let Some(lstat) = first_of(&[Syscall::Lstat, Syscall::Lstat64])
            && stat_into(p, lstat, [link, p.out, 0, 0, 0, 0])? != as_link
        {
            return Err("lstat and fstatat disagree about a link");
        }

        let fd = install(p.process, b"/tmp/pathcheck/file", false)?;
        let fstat = first_of(&[Syscall::Fstat, Syscall::Fstat64]).ok_or("no fstat here")?;
        let by_fd = stat_into(p, fstat, [fd, p.out, 0, 0, 0, 0]);
        let empty = p.path(b"")?;
        let empty_path = u64::from(AT_EMPTY_PATH);
        let by_empty = stat_into(p, at, [fd, empty, p.out, empty_path, 0, 0]);
        uninstall(p.process, fd)?;
        if by_fd? != by_path || by_empty? != by_path {
            return Err("fstat, AT_EMPTY_PATH and a path disagree about one file");
        }

        let statx_size = size_of::<Statx>();
        p.fill_out(statx_size, 0xA5)?;
        let basic = u64::from(STATX_BASIC_STATS);
        let link = p.path(b"/tmp/pathcheck/moved")?;
        if p.call(Syscall::Statx, [CWD, link, nofollow, basic, p.out, 0]) != Ok(0) {
            return Err("statx was refused");
        }
        let record = p.read_out(statx_size)?;
        let field = |at, width| le(&record, at, width).unwrap_or(u64::MAX);
        if field(offset_of!(Statx, stx_mask), 4) & basic != basic
            || field(offset_of!(Statx, stx_ino), 8) != as_link.ino
            || field(offset_of!(Statx, stx_size), 8) != as_link.size
            || field(offset_of!(Statx, stx_mode), 2) != u64::from(as_link.mode)
        {
            return Err("statx disagrees with fstatat about a link");
        }
        Ok(())
    }

    /// The path of the listing check's `index`th name, `e00` to `e39`.
    fn entry(index: usize) -> Vec<u8> {
        let mut path = Vec::from(&b"/tmp/pathcheck/many/e"[..]);
        path.extend_from_slice(&[b'0' + (index / 10) as u8, b'0' + (index % 10) as u8]);
        path
    }

    /// `getdents64` over forty names with room for four at a time reports
    /// every name exactly once, and `EINVAL` when there is no room for one.
    fn check_a_listing_in_pieces_sees_each_name_once(
        p: &mut Paths<'_>,
    ) -> Result<(usize, u32), &'static str> {
        let dir = p.path(b"/tmp/pathcheck/many")?;
        if p.call(Syscall::Mkdirat, [CWD, dir, 0o755, 0, 0, 0]) != Ok(0) {
            return Err("mkdirat of the listing directory was refused");
        }
        for index in 0..LISTED {
            let name = p.path(&entry(index))?;
            if p.call(
                Syscall::Mknodat,
                [CWD, name, u64::from(S_IFREG | 0o600), 0, 0, 0],
            ) != Ok(0)
            {
                return Err("mknodat in the listing directory was refused");
            }
        }

        let fd = install(p.process, b"/tmp/pathcheck/many", true)?;
        let listing = list(p, fd);
        uninstall(p.process, fd)?;
        let (seen, dots, listed, calls) = listing?;
        if dots != 2 || seen.iter().any(|&count| count != 1) || listed != LISTED + 2 {
            return Err("getdents64 did not report every name exactly once");
        }
        if calls < 3 {
            return Err("the listing was not split across calls");
        }
        Ok((listed, calls))
    }

    /// Read the directory open on `fd` to its end.
    fn list(p: &mut Paths<'_>, fd: u64) -> Result<([u8; LISTED], usize, usize, u32), &'static str> {
        if p.call(Syscall::Getdents64, [fd, p.out, 16, 0, 0, 0]) != Err(Errno::EINVAL) {
            return Err("getdents64 with no room for an entry was not EINVAL");
        }
        let (mut seen, mut dots, mut listed, mut calls) = ([0_u8; LISTED], 0, 0, 0_u32);
        loop {
            calls += 1;
            if calls > 64 {
                return Err("getdents64 never reached the end of the directory");
            }
            let used = p
                .call(Syscall::Getdents64, [fd, p.out, LISTING_BUFFER, 0, 0, 0])
                .map_err(|_| "getdents64 was refused")?;
            if used == 0 {
                return Ok((seen, dots, listed, calls));
            }
            for record in dirent::records(&p.read_out(used)?) {
                listed += 1;
                tally(&record, &mut seen, &mut dots)?;
            }
        }
    }

    /// Count one listed name.
    fn tally(
        record: &Record<'_>,
        seen: &mut [u8; LISTED],
        dots: &mut usize,
    ) -> Result<(), &'static str> {
        let [b'e', tens, ones] = *record.name else {
            if record.name == b"." || record.name == b".." {
                *dots += 1;
                return Ok(());
            }
            return Err("getdents64 reported a name nobody made");
        };
        let index =
            usize::from(tens.wrapping_sub(b'0')) * 10 + usize::from(ones.wrapping_sub(b'0'));
        let count = seen
            .get_mut(index)
            .ok_or("getdents64 reported a name nobody made")?;
        *count = count.saturating_add(1);
        if record.kind != DT_REG {
            return Err("getdents64 reported a regular file as something else");
        }
        Ok(())
    }

    /// `getcwd` reports exactly `want`, terminated, and counts the terminator.
    fn expect_cwd(p: &mut Paths<'_>, want: &[u8]) -> Result<(), &'static str> {
        let len = p
            .call(Syscall::Getcwd, [p.out, 256, 0, 0, 0, 0])
            .map_err(|_| "getcwd was refused")?;
        let got = p.read_out(len)?;
        if len != want.len() + 1 || got.get(..want.len()) != Some(want) || got.last() != Some(&0) {
            return Err("getcwd did not report the directory chdir chose");
        }
        Ok(())
    }

    /// `chdir`, `fchdir` and a relative path agree about where the process is,
    /// and `getcwd` says `ERANGE` for a short buffer and `ENOENT` once the
    /// directory is gone.
    fn check_the_working_directory_follows_chdir(p: &mut Paths<'_>) -> Result<(), &'static str> {
        let many = p.path(b"/tmp/pathcheck/many")?;
        if p.call(Syscall::Chdir, [many, 0, 0, 0, 0, 0]) != Ok(0) {
            return Err("chdir was refused");
        }
        expect_cwd(p, b"/tmp/pathcheck/many")?;
        // One byte short: the terminator counts.
        if p.call(Syscall::Getcwd, [p.out, 19, 0, 0, 0, 0]) != Err(Errno::ERANGE) {
            return Err("getcwd into a buffer one byte short was not ERANGE");
        }

        let sub = p.path(b"sub")?;
        if p.call(Syscall::Mkdirat, [CWD, sub, 0o755, 0, 0, 0]) != Ok(0)
            || p.call(Syscall::Chdir, [sub, 0, 0, 0, 0, 0]) != Ok(0)
        {
            return Err("a relative mkdirat or chdir was refused");
        }
        expect_cwd(p, b"/tmp/pathcheck/many/sub")?;
        let gone = p.path(b"/tmp/pathcheck/many/sub")?;
        if p.call(
            Syscall::Unlinkat,
            [CWD, gone, u64::from(AT_REMOVEDIR), 0, 0, 0],
        ) != Ok(0)
        {
            return Err("rmdir of the working directory was refused");
        }
        if p.call(Syscall::Getcwd, [p.out, 256, 0, 0, 0, 0]) != Err(Errno::ENOENT) {
            return Err("getcwd in a removed directory was not ENOENT");
        }

        let fd = install(p.process, ROOT, true)?;
        let moved = p.call(Syscall::Fchdir, [fd, 0, 0, 0, 0, 0]);
        uninstall(p.process, fd)?;
        if moved != Ok(0) {
            return Err("fchdir was refused");
        }
        expect_cwd(p, ROOT)?;
        let slash = p.path(b"/")?;
        if p.call(Syscall::Chdir, [slash, 0, 0, 0, 0, 0]) != Ok(0) {
            return Err("chdir to the root was refused");
        }
        expect_cwd(p, b"/")
    }

    /// Two `timespec`s at this architecture's `long` width.
    fn timespecs(values: [i64; 4]) -> Vec<u8> {
        let width = size_of::<usize>();
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(value.to_le_bytes().get(..width).unwrap_or(&[]));
        }
        bytes
    }

    /// `faccessat` answers as root does, and `chmod`, `chown`, `utimensat`
    /// and `umask` change what they say they change.
    fn check_access_and_attributes(p: &mut Paths<'_>) -> Result<(), &'static str> {
        let file = p.path(b"/tmp/pathcheck/file")?;
        if p.call(
            Syscall::Faccessat,
            [CWD, file, u64::from(R_OK | W_OK), 0, 0, 0],
        ) != Ok(0)
        {
            return Err("root could not read and write a file");
        }
        if p.call(Syscall::Faccessat, [CWD, file, u64::from(X_OK), 0, 0, 0]) != Err(Errno::EACCES) {
            return Err("a file with no execute bit passed X_OK");
        }
        if p.call(Syscall::Fchmodat, [CWD, file, 0o755, 0, 0, 0]) != Ok(0)
            || p.call(Syscall::Faccessat2, [CWD, file, u64::from(X_OK), 0, 0, 0]) != Ok(0)
        {
            return Err("chmod did not make a file executable");
        }
        if p.call(Syscall::Fchownat, [CWD, file, 7, u64::from(u32::MAX), 0, 0]) != Ok(0) {
            return Err("fchownat was refused");
        }
        let times = p.stage(p.strings + SECOND, &timespecs([1, UTIME_OMIT, 1234, 0]))?;
        if p.call(Syscall::Utimensat, [CWD, file, times, 0, 0, 0]) != Ok(0) {
            return Err("utimensat was refused");
        }
        let after = stat_into(p, fstatat()?, [CWD, file, p.out, 0, 0, 0])?;
        if after.mode != S_IFREG | 0o755 || after.uid != 7 || after.mtime != 1234 {
            return Err("chmod, chown or utimensat did not show in stat");
        }
        if p.call(Syscall::Umask, [0o7077, 0, 0, 0, 0, 0]) != Ok(0o022)
            || p.call(Syscall::Umask, [0o022, 0, 0, 0, 0, 0]) != Ok(0o077)
        {
            return Err("umask did not swap the mask, keeping permission bits only");
        }
        Ok(())
    }

    /// `unlinkat` removes names, with and without `AT_REMOVEDIR`, and refuses
    /// the wrong kind of each; nothing the checks made is left.
    fn check_names_are_removed(p: &mut Paths<'_>) -> Result<(), &'static str> {
        let removedir = u64::from(AT_REMOVEDIR);
        let many = p.path(b"/tmp/pathcheck/many")?;
        if p.call(Syscall::Unlinkat, [CWD, many, removedir, 0, 0, 0]) != Err(Errno::ENOTEMPTY) {
            return Err("rmdir of a directory with entries was not ENOTEMPTY");
        }
        if p.call(Syscall::Unlinkat, [CWD, many, 0, 0, 0, 0]) != Err(Errno::EISDIR) {
            return Err("unlink of a directory was not EISDIR");
        }
        for index in 0..LISTED {
            let name = p.path(&entry(index))?;
            if p.call(Syscall::Unlinkat, [CWD, name, 0, 0, 0, 0]) != Ok(0) {
                return Err("unlinkat of a listed file was refused");
            }
        }
        let many = p.path(b"/tmp/pathcheck/many")?;
        if p.call(Syscall::Unlinkat, [CWD, many, removedir, 0, 0, 0]) != Ok(0) {
            return Err("rmdir of an emptied directory was refused");
        }
        let file = p.path(b"/tmp/pathcheck/file")?;
        if p.call(Syscall::Unlinkat, [CWD, file, removedir, 0, 0, 0]) != Err(Errno::ENOTDIR) {
            return Err("rmdir of a file was not ENOTDIR");
        }
        for name in [&b"/tmp/pathcheck/file"[..], b"/tmp/pathcheck/moved"] {
            let name = p.path(name)?;
            if p.call(Syscall::Unlinkat, [CWD, name, 0, 0, 0, 0]) != Ok(0) {
                return Err("unlinkat of a file was refused");
            }
        }
        let root = p.path(ROOT)?;
        if p.call(Syscall::Unlinkat, [CWD, root, removedir, 0, 0, 0]) != Ok(0) {
            return Err("rmdir of the check's own directory was refused");
        }
        if p.call(fstatat()?, [CWD, root, p.out, 0, 0, 0]) != Err(Errno::ENOENT) {
            return Err("a removed directory could still be described");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Processes making processes
// ---------------------------------------------------------------------------

/// What [`arch::USER_FORK_PROGRAM`] exits with when everything is right.
const FORK_STATUS: i32 = 24;

/// A program forks, its child exits 23, the parent waits for it and exits with
/// the child's code plus one.
///
/// One number covers the whole path: the child resumed from a copy of its
/// parent's registers with the call returning zero, it ran in a copy of the
/// parent's memory and exited, the parent's `wait4` found that child and not
/// another, and the status word put the exit code in its second byte.
fn check_a_forked_child_is_waited_for() -> Result<Option<i32>, &'static str> {
    if arch::USER_FORK_PROGRAM.is_empty() {
        return Ok(None);
    }
    let file = image::build_with(
        class_of_this_build(),
        arch::ARCH.elf_machine(),
        image::Shape::Good,
        arch::USER_FORK_PROGRAM,
    );
    let status = exec::run(&file, &[b"/fork"], &[], [0x5a; ferrix_ustack::RANDOM_BYTES])
        .map_err(|_| "a program that forks could not be started")?;
    match status {
        FORK_STATUS => Ok(Some(status)),
        99 => Err("wait4 reported a child other than the one fork made"),
        _ => Err("a program that forks and waits did not see its child exit with 23"),
    }
}

/// What [`arch::USER_SIGNAL_PROGRAM`] exits with when everything is right.
const SIGNAL_STATUS: i32 = 77;

/// A program installs a handler with a restorer, signals itself with `tgkill`
/// -- the way `abort` does -- and exits with a register its handler changed
/// through the `ucontext` of the frame it ran on.
///
/// One number covers the whole round trip: the signal was pending on the way
/// back from `tgkill`, a frame in Linux's layout was written to the program's
/// stack, the handler was entered with the signal, `siginfo` and `ucontext` in
/// the right registers and the signal blocked, it returned into its restorer,
/// and `rt_sigreturn` read the frame back -- the changed register included --
/// and put the old mask back. On ARMv7-A the program does it a second time
/// without `SA_SIGINFO`, through the other frame and `sigreturn`.
fn check_a_handler_runs_and_returns() -> Result<Option<i32>, &'static str> {
    if arch::USER_SIGNAL_PROGRAM.is_empty() {
        return Ok(None);
    }
    let file = image::build_with(
        class_of_this_build(),
        arch::ARCH.elf_machine(),
        image::Shape::Good,
        arch::USER_SIGNAL_PROGRAM,
    );
    let status = exec::run(
        &file,
        &[b"/signal"],
        &[],
        [0x5a; ferrix_ustack::RANDOM_BYTES],
    )
    .map_err(|_| "a program that handles a signal could not be started")?;
    match status {
        SIGNAL_STATUS => Ok(Some(status)),
        7 => Err(
            "a signal handler did not run, or returning from it did not restore the registers \
             its frame held",
        ),
        98 => Err("a signal handler was entered with the wrong signal, siginfo or blocked mask"),
        99 => Err("rt_sigaction, tgkill or the mask after a handler returned was wrong"),
        129..=192 => Err(
            "a program that handles a signal was killed by a signal instead: its frame, its \
             handler's entry or its return was wrong",
        ),
        _ => Err("a program that handles a signal did not exit with 77"),
    }
}

/// `kill` and its thread forms find a process by pid, refuse a signal past 64
/// and a thread that is not the process, and discard a signal the process
/// ignores by default rather than leaving it pending.
///
/// Against a process with no task, so nothing sent here is ever delivered:
/// signal zero, which only asks, and `SIGCHLD`, which is ignored.
fn check_kill_finds_its_targets_and_refuses_what_it_should(
    process: &Process,
) -> Result<(), &'static str> {
    use crate::syscall::kill;
    use ferrix_linux_abi::types::SIGCHLD;

    let pid = i32::try_from(process.pid()).map_err(|_| "a pid does not fit an int")?;
    if kill::sys_kill(process, pid, 0) != Ok(0) {
        return Err("kill with signal zero did not find the process by its pid");
    }
    if kill::sys_kill(process, i32::MAX, 0) != Err(Errno::ESRCH) {
        return Err("kill of a pid nobody has was not ESRCH");
    }
    if kill::sys_kill(process, pid, 65) != Err(Errno::EINVAL) {
        return Err("kill with signal 65 was not EINVAL");
    }
    if kill::sys_tgkill(process, pid, pid.saturating_add(1), 0) != Err(Errno::ESRCH) {
        return Err("tgkill of a thread that is not its process's was not ESRCH");
    }
    if kill::sys_tkill(process, 0, 0) != Err(Errno::EINVAL) {
        return Err("tkill of thread zero was not EINVAL");
    }
    if kill::sys_tgkill(process, pid, pid, SIGCHLD) != Ok(0)
        || process.with_signals(|signals| signals.pending()) != 0
    {
        return Err("SIGCHLD, ignored by default, was left pending instead of discarded");
    }
    Ok(())
}

/// Where [`arch::USER_EXEC_PROGRAM`] looks for its target: a symbolic link,
/// in this check, to [`EXEC_REAL`].
const EXEC_TARGET: &[u8] = b"/exec-target";

/// The file [`EXEC_TARGET`] links to, which is the one actually run.
const EXEC_REAL: &[u8] = b"/exec-target-real";

/// A program `execve`s another by path and takes on its status; with the file
/// gone, the same program gets `ENOENT` back and exits with it.
///
/// The path it asks for is a symbolic link, and the process must end up
/// recorded as the file the link names, absolute -- what `/proc/self/exe`
/// reports, and what glibc's static startup asserts is absolute. The program
/// passes the link's own name as `argv[0]`, so recording that instead fails
/// here.
fn check_execve_replaces_the_program() -> Result<Option<(i32, i32)>, &'static str> {
    use ferrix_vfs::OpenFlags;

    if arch::USER_EXEC_PROGRAM.is_empty() {
        return Ok(None);
    }
    let class = class_of_this_build();
    let machine = arch::ARCH.elf_machine();
    let target = image::build_with(class, machine, image::Shape::Good, arch::USER_TEST_PROGRAM);
    let caller = image::build_with(class, machine, image::Shape::Good, arch::USER_EXEC_PROGRAM);

    let ns = crate::fs::namespace();
    let ctx = ns.context();
    let create = OpenFlags {
        read: false,
        write: true,
        create: true,
        exclusive: false,
        truncate: true,
        append: false,
        directory: false,
        nofollow: false,
        path: false,
        nonblock: false,
    };
    let file = ns
        .open(&ctx, None, EXEC_REAL, &create, 0o755)
        .map_err(|_| "could not create the program execve is to run")?;
    if file.write(&target) != Ok(target.len()) {
        return Err("could not write the program execve is to run");
    }
    drop(file);
    ns.symlink(&ctx, None, EXEC_TARGET, EXEC_REAL)
        .map_err(|_| "could not link to the program execve is to run")?;

    // Loaded and waited for by hand rather than through `exec::run`, to keep
    // the process and read what it was recorded as after it has ended.
    let execing = exec::load(
        &caller,
        &[b"/exec-caller"],
        &[],
        [0x5a; ferrix_ustack::RANDOM_BYTES],
    )
    .map_err(|_| "a program that calls execve could not be loaded")?;
    let task =
        process::start(&execing).map_err(|_| "a program that calls execve could not be started")?;
    let found = execing
        .wait_for_exit(u64::MAX)
        .ok_or("a program that calls execve never reported how it ended")?;
    drop(task);
    let recorded = execing.exe();
    drop(execing);
    ns.unlink(&ctx, None, EXEC_TARGET)
        .map_err(|_| "could not remove the link execve ran through")?;
    ns.unlink(&ctx, None, EXEC_REAL)
        .map_err(|_| "could not remove the program execve ran")?;
    let missing = exec::run(
        &caller,
        &[b"/exec-caller"],
        &[],
        [0x5a; ferrix_ustack::RANDOM_BYTES],
    )
    .map_err(|_| "a program that calls execve could not be started a second time")?;

    if found != arch::USER_TEST_STATUS {
        return Err("a program that called execve did not end with the new program's status");
    }
    if recorded != EXEC_REAL {
        return Err(
            "execve through a symbolic link did not record the absolute path of the file it ran",
        );
    }
    if missing != Errno::ENOENT.0 as i32 {
        return Err("execve of a path that does not exist did not return ENOENT");
    }
    Ok(Some((found, missing)))
}

// ---------------------------------------------------------------------------
// Futexes
//
// Through the handler, on a word in a process the check builds, with a kernel
// task as the sleeper: a kernel task can sleep in `futex` as well as a
// program can, and needs no program assembled for three architectures to do
// it. The wake is the part worth a second task, and it is checked twice: once
// working, and once with a wake that takes its waiter off the table and
// reports it woken without rousing it. That second run must fail, and fail
// for that reason -- a check that only compared the count a wake returns
// would pass it.
// ---------------------------------------------------------------------------

use ferrix_linux_abi::types::{
    FUTEX_CLOCK_REALTIME, FUTEX_CMP_REQUEUE, FUTEX_PRIVATE_FLAG, FUTEX_WAIT, FUTEX_WAIT_BITSET,
    FUTEX_WAKE, FUTEX_WAKE_OP,
};

use crate::sched::WaitQueue;
use crate::syscall::futex;

/// What the check's futex word holds.
const FUTEX_WORD: u32 = 0x0F07_E100;

/// The timeout of a wait that should time out.
const FUTEX_SHORT_NANOS: u64 = 20_000_000;

/// How long the waiter a wake is meant for will sleep, in seconds. Far longer
/// than a working wake takes to arrive; the whole of what the negative
/// control, which never rouses it, costs a boot.
const FUTEX_LONG_SECONDS: u64 = 2;

/// How long the check waits for its waiter to start waiting, or to return.
const FUTEX_PATIENCE_NANOS: u64 = 30_000_000_000;

/// The failure a wake that never rouses its waiter produces, which the
/// negative control requires by name.
const SLEPT_THROUGH_WAKE: &str = "a futex waiter the wake counted slept on to its timeout";

/// What the waiting task waits on -- the process, the word, the timeout --
/// taken by the task when it starts.
static FUTEX_SUBJECT: ferrix_sync::SpinLock<Option<(Arc<Process>, u64, u64)>> =
    ferrix_sync::SpinLock::new(None);

/// What the waiting task's `FUTEX_WAIT` answered.
static FUTEX_ANSWER: ferrix_sync::SpinLock<Option<Result<usize, Errno>>> =
    ferrix_sync::SpinLock::new(None);

/// Woken when it has answered.
static FUTEX_ANSWERED: WaitQueue = WaitQueue::new();

/// One wake, as the check applies it to the word its waiter sleeps on.
type FutexWake = fn(&Process, u64) -> Result<usize, Errno>;

/// `futex` with six arguments and a native timespec.
fn futex_call(process: &Process, a: [u64; 6]) -> Result<usize, Errno> {
    futex::sys_futex(process, &a, crate::syscall::time::TimeWidth::Native)
}

/// Write a native `struct timespec` at `at`.
fn write_timespec(
    process: &Process,
    at: u64,
    seconds: u64,
    nanos: u64,
) -> Result<(), &'static str> {
    write_word(process, at, seconds)?;
    write_word(process, at + size_of::<usize>() as u64, nanos)
}

/// A wait on a changed word is `EAGAIN`, a timed wait nobody wakes is
/// `ETIMEDOUT` and not early, what is not implemented says so, and a waiter
/// is roused by a wake and by a requeue followed by a wake -- but not by a
/// wake that only pretends. Answers how many waiters were roused.
fn check_futexes() -> Result<usize, &'static str> {
    let process =
        process::new_for_check().map_err(|_| "could not make a process for the futex check")?;
    let page = map_rw(&process, PAGE_SIZE)?;
    let word = page;
    let timeout = page + 16;
    uaccess::copy_to_user(process.space(), word, &FUTEX_WORD.to_le_bytes())
        .map_err(|_| "could not stage the futex word")?;
    let wait = u64::from(FUTEX_WAIT | FUTEX_PRIVATE_FLAG);
    let expected = u64::from(FUTEX_WORD);

    if futex_call(&process, [word, wait, expected + 1, 0, 0, 0]) != Err(Errno::EAGAIN) {
        return Err("FUTEX_WAIT on a word that had changed did not answer EAGAIN");
    }
    if futex_call(&process, [word + 1, wait, expected, 0, 0, 0]) != Err(Errno::EINVAL) {
        return Err("FUTEX_WAIT on a misaligned word was not EINVAL");
    }

    write_timespec(&process, timeout, 0, FUTEX_SHORT_NANOS)?;
    let started = crate::timer::now_nanos();
    if futex_call(&process, [word, wait, expected, timeout, 0, 0]) != Err(Errno::ETIMEDOUT) {
        return Err("a timed FUTEX_WAIT nobody woke did not answer ETIMEDOUT");
    }
    if crate::timer::now_nanos().saturating_sub(started) < FUTEX_SHORT_NANOS {
        return Err("a timed FUTEX_WAIT came back before its timeout");
    }

    // Absolute, one nanosecond after the counter started: long past.
    write_timespec(&process, timeout, 0, 1)?;
    let bitset_wait = u64::from(FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
    if futex_call(&process, [word, bitset_wait, expected, timeout, 0, 1]) != Err(Errno::ETIMEDOUT) {
        return Err("FUTEX_WAIT_BITSET with a deadline already past did not answer ETIMEDOUT");
    }
    if futex_call(&process, [word, bitset_wait, expected, timeout, 0, 0]) != Err(Errno::EINVAL) {
        return Err("FUTEX_WAIT_BITSET with an empty bitset was not EINVAL");
    }
    let realtime_wake = u64::from(FUTEX_WAKE | FUTEX_CLOCK_REALTIME);
    if futex_call(&process, [word, realtime_wake, 1, 0, 0, 0]) != Err(Errno::ENOSYS) {
        return Err("FUTEX_WAKE accepted FUTEX_CLOCK_REALTIME");
    }
    if futex_call(
        &process,
        [word, u64::from(FUTEX_WAKE_OP), 1, 1, word + 4, 0],
    ) != Err(Errno::ENOSYS)
    {
        return Err("FUTEX_WAKE_OP, which is not implemented, did not answer ENOSYS");
    }
    if futex_call(&process, [word, u64::from(FUTEX_WAKE), 1, 0, 0, 0]) != Ok(0) {
        return Err("FUTEX_WAKE with nobody waiting did not answer zero");
    }
    let cmp_requeue = u64::from(FUTEX_CMP_REQUEUE);
    if futex_call(&process, [word, cmp_requeue, 1, 1, word + 4, expected + 1]) != Err(Errno::EAGAIN)
    {
        return Err("FUTEX_CMP_REQUEUE on a word that had changed did not answer EAGAIN");
    }

    let mut woken = wait_then_wake(&process, word, timeout, |process, word| {
        futex_call(
            process,
            [word, u64::from(FUTEX_WAKE | FUTEX_PRIVATE_FLAG), 1, 0, 0, 0],
        )
    })?;
    // Moved to the next word, then woken there: the requeue must have taken
    // it, since nothing else wakes the second word.
    woken += wait_then_wake(&process, word, timeout, |process, word| {
        let requeue = u64::from(FUTEX_CMP_REQUEUE | FUTEX_PRIVATE_FLAG);
        let moved = futex_call(
            process,
            [word, requeue, 0, 1, word + 4, u64::from(FUTEX_WORD)],
        )?;
        if moved != 1 || futex::waiters_on(process, word) != 0 {
            return Ok(0);
        }
        futex_call(
            process,
            [
                word + 4,
                u64::from(FUTEX_WAKE | FUTEX_PRIVATE_FLAG),
                1,
                0,
                0,
                0,
            ],
        )
    })?;

    match wait_then_wake(&process, word, timeout, |process, word| {
        Ok(futex::forget_waiters(process, word, 1))
    }) {
        Err(problem) if problem == SLEPT_THROUGH_WAKE => {}
        Err(_) => {
            return Err("the futex check failed a wake that roused nobody, for another reason");
        }
        Ok(_) => return Err("the futex check passed a wake that never roused its waiter"),
    }

    let _ = memory::sys_munmap(&process, page, PAGE_SIZE).map_err(|_| "munmap was refused")?;
    Ok(woken)
}

/// Start a task sleeping in `FUTEX_WAIT` on `word`, wait until it is on the
/// table, `wake` it, and require it back with zero and the wake to have
/// counted it. Answers one.
fn wait_then_wake(
    process: &Arc<Process>,
    word: u64,
    timeout: u64,
    wake: FutexWake,
) -> Result<usize, &'static str> {
    write_timespec(process, timeout, FUTEX_LONG_SECONDS, 0)?;
    *FUTEX_ANSWER.lock() = None;
    *FUTEX_SUBJECT.lock() = Some((Arc::clone(process), word, timeout));
    let waiter = crate::sched::spawn("futex-waiter", futex_waiter, 0, ferrix_sched::NICE_0_WEIGHT)?;

    let deadline = crate::timer::now_nanos().saturating_add(FUTEX_PATIENCE_NANOS);
    while futex::waiters_on(process, word) == 0 {
        if FUTEX_ANSWER.lock().is_some() {
            return Err("a futex waiter returned without ever waiting");
        }
        if crate::timer::now_nanos() >= deadline {
            return Err("a futex waiter never started waiting");
        }
        crate::sched::sleep_for(1_000_000);
    }
    let count = wake(process, word);

    let deadline = crate::timer::now_nanos().saturating_add(FUTEX_PATIENCE_NANOS);
    let _ = FUTEX_ANSWERED.wait_until_deadline(|| FUTEX_ANSWER.lock().is_some(), deadline);
    let answer = FUTEX_ANSWER.lock().take();
    drop(waiter);
    match (count, answer) {
        (_, None) => Err("a futex waiter never came back"),
        (Ok(1), Some(Ok(0))) => Ok(1),
        (Ok(1), Some(Err(error))) if error == Errno::ETIMEDOUT => Err(SLEPT_THROUGH_WAKE),
        (Ok(1), Some(_)) => Err("a woken futex waiter did not answer zero"),
        (_, Some(_)) => Err("a futex wake did not report the one waiter it had"),
    }
}

/// The waiting task: one `FUTEX_WAIT` on what [`FUTEX_SUBJECT`] names.
fn futex_waiter(_argument: usize) {
    let subject = FUTEX_SUBJECT.lock().take();
    let answer = match subject {
        Some((process, word, timeout)) => futex_call(
            &process,
            [
                word,
                u64::from(FUTEX_WAIT | FUTEX_PRIVATE_FLAG),
                u64::from(FUTEX_WORD),
                timeout,
                0,
                0,
            ],
        ),
        None => Err(Errno::ESRCH),
    };
    *FUTEX_ANSWER.lock() = Some(answer);
    FUTEX_ANSWERED.wake_all();
}

// ---------------------------------------------------------------------------
// What a busybox applet asks of the system
//
// `free`, `ulimit`, `nproc`, `renice`, `hostname`, `date -s`, `sleep`: each is
// one or two calls about the machine, its limits or its clocks, and each call
// writes a structure whose size changes with the word. So every check below
// poisons the buffer first and requires the byte after the structure to
// survive -- a handler that wrote the 64-bit layout on ARMv7-A fails here, not
// under a program that reads the wrong field and prints nonsense.
// ---------------------------------------------------------------------------

/// The byte the checks below fill a buffer with before a handler writes it.
const UNWRITTEN: u8 = 0xAA;

/// The system, limit, clock, credential and socket calls, on one page.
fn check_what_an_applet_asks_of_the_system(process: &Process) -> Result<(), &'static str> {
    let page = map_rw(process, PAGE_SIZE)?;
    let outcome = check_sysinfo_describes_the_machine(process, page)
        .and_then(|()| check_limits_read_back_and_reach_the_descriptor_table(process, page))
        .and_then(|()| check_affinity_names_the_running_processors(process, page))
        .and_then(|()| check_a_task_name_round_trips(process, page))
        .and_then(|()| check_credentials_refuse_another_user(process, page))
        .and_then(|()| check_nanosleep_takes_its_time(process, page))
        .and_then(|()| check_setting_the_clock_moves_only_realtime(process, page))
        .and_then(|()| check_a_host_name_reaches_uname(process, page))
        .and_then(|()| check_sockets_are_refused_honestly(process));
    let outcome = outcome.and_then(|()| check_time_is_the_realtime_seconds(process, page));
    let _ = memory::sys_munmap(process, page, PAGE_SIZE);
    outcome
}

/// Fill `len` bytes at `at` with [`UNWRITTEN`].
fn poison_user(process: &Process, at: u64, len: usize) -> Result<(), &'static str> {
    let bytes = [UNWRITTEN; 256];
    let span = bytes
        .get(..len)
        .ok_or("a poisoned span longer than the check allows")?;
    uaccess::copy_to_user(process.space(), at, span).map_err(|_| "could not poison a buffer")
}

/// Read `N` bytes back from `at`.
fn read_user<const N: usize>(process: &Process, at: u64) -> Result<[u8; N], &'static str> {
    let mut out = [0_u8; N];
    uaccess::copy_from_user(process.space(), at, &mut out)
        .map_err(|_| "could not read a buffer back")?;
    Ok(out)
}

/// A little-endian unsigned field of `width` bytes at `at`, or `u64::MAX` if
/// the field is not inside `bytes` -- which no check below expects to see.
fn le_at(bytes: &[u8], at: usize, width: usize) -> u64 {
    bytes.get(at..at + width).map_or(u64::MAX, |field| {
        field
            .iter()
            .rev()
            .fold(0, |value, &byte| value << 8 | u64::from(byte))
    })
}

/// Whether every byte of `bytes[from..to]` is `value`.
fn all_are(bytes: &[u8], from: usize, to: usize, value: u8) -> bool {
    bytes
        .get(from..to)
        .is_some_and(|span| span.iter().all(|&byte| byte == value))
}

/// `sysinfo` writes exactly `struct sysinfo`, its memory counts are the frame
/// allocator's in the unit it names, and every field it has nothing for is
/// zero rather than whatever the buffer held.
fn check_sysinfo_describes_the_machine(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::system::{SYSINFO_SIZE, sysinfo_at};
    const WORD: usize = size_of::<usize>();

    poison_user(process, page, 128)?;
    answers(system::sys_sysinfo(process, page), 0, "sysinfo was refused")?;
    let out: [u8; 128] = read_user(process, page)?;

    let unit = le_at(&out, sysinfo_at::MEM_UNIT, 4);
    if unit != 1 && unit != PAGE_SIZE {
        return Err("sysinfo's mem_unit is neither a byte nor a page");
    }
    let total = le_at(&out, sysinfo_at::TOTALRAM, WORD).saturating_mul(unit);
    let free = le_at(&out, sysinfo_at::FREERAM, WORD).saturating_mul(unit);
    if total != mm::managed_frames() * PAGE_SIZE {
        return Err("sysinfo's total memory is not the frame allocator's");
    }
    if free == 0 || free > total {
        return Err("sysinfo's free memory is not a part of its total");
    }
    if le_at(&out, sysinfo_at::UPTIME, WORD) == 0 {
        return Err("sysinfo reported no uptime on a machine that has been up");
    }
    if le_at(&out, sysinfo_at::PROCS, 2) == 0 {
        return Err("sysinfo counted no processes while this one is registered");
    }
    // Loads; shared, buffer and swap; the padding after procs; high memory;
    // and the tail after mem_unit. All zero, and all written.
    let zero = [
        (WORD, WORD * 4),
        (WORD * 6, WORD * 10),
        (sysinfo_at::PROCS + 2, WORD * 11),
        (WORD * 11, WORD * 13),
        (sysinfo_at::MEM_UNIT + 4, SYSINFO_SIZE),
    ];
    if !zero.iter().all(|&(from, to)| all_are(&out, from, to, 0)) {
        return Err("a sysinfo field with nothing to report was not written as zero");
    }
    if !all_are(&out, SYSINFO_SIZE, out.len(), UNWRITTEN) {
        return Err("sysinfo wrote past the end of this build's struct sysinfo");
    }
    refuses(
        system::sys_sysinfo(process, KERNEL_HALF_BASE),
        Errno::EFAULT,
        "sysinfo into a kernel address was not EFAULT",
    )
}

/// Stage a 16-byte `struct rlimit64` at `at`.
fn stage_rlimit64(process: &Process, at: u64, soft: u64, hard: u64) -> Result<(), &'static str> {
    let mut bytes = [0_u8; 16];
    let fields = soft.to_le_bytes().into_iter().chain(hard.to_le_bytes());
    for (slot, byte) in bytes.iter_mut().zip(fields) {
        *slot = byte;
    }
    uaccess::copy_to_user(process.space(), at, &bytes).map_err(|_| "could not stage an rlimit64")
}

/// `RLIMIT_NOFILE` is the descriptor table's limit in both directions, a limit
/// set through `setrlimit` reads back through `prlimit64`, and the refusals
/// are `do_prlimit`'s.
fn check_limits_read_back_and_reach_the_descriptor_table(
    process: &Process,
    page: u64,
) -> Result<(), &'static str> {
    use crate::syscall::limits::{sys_getrlimit, sys_prlimit64, sys_setrlimit};
    const WORD: usize = size_of::<usize>();
    const NOFILE: u32 = 7;
    let table_limit = || u64::from(process.files().lock().limit());
    let own = i32::try_from(process.pid()).map_err(|_| "a pid does not fit a pid_t")?;

    poison_user(process, page, WORD * 3)?;
    answers(
        sys_getrlimit(process, NOFILE, page),
        0,
        "getrlimit(RLIMIT_NOFILE) was refused",
    )?;
    let out: [u8; 24] = read_user(process, page)?;
    if le_at(&out, 0, WORD) != table_limit() || le_at(&out, WORD, WORD) < table_limit() {
        return Err("RLIMIT_NOFILE does not read as the descriptor table's limit");
    }
    if !all_are(&out, WORD * 2, WORD * 3, UNWRITTEN) {
        return Err("getrlimit wrote past this build's struct rlimit");
    }

    write_word(process, page, 64)?;
    write_word(process, page + WORD as u64, 128)?;
    answers(
        sys_setrlimit(process, NOFILE, page),
        0,
        "setrlimit(RLIMIT_NOFILE) was refused",
    )?;
    if table_limit() != 64 {
        return Err("setrlimit(RLIMIT_NOFILE) did not reach the descriptor table");
    }

    // prlimit64 by the caller's own pid: set {1024, 4096}, get back {64, 128}.
    stage_rlimit64(process, page, 1024, 4096)?;
    poison_user(process, page + 64, 24)?;
    answers(
        sys_prlimit64(process, own, NOFILE, page, page + 64),
        0,
        "prlimit64 on the caller's own pid was refused",
    )?;
    let old: [u8; 24] = read_user(process, page + 64)?;
    if le_at(&old, 0, 8) != 64 || le_at(&old, 8, 8) != 128 || !all_are(&old, 16, 24, UNWRITTEN) {
        return Err("prlimit64 did not report the limit setrlimit set, in a 16-byte rlimit64");
    }
    if table_limit() != 1024 {
        return Err("prlimit64(RLIMIT_NOFILE) did not reach the descriptor table");
    }
    check_limits_are_refused_as_linux_refuses_them(process, page)
}

/// The refusals, and the two defaults a shell's `ulimit` prints.
fn check_limits_are_refused_as_linux_refuses_them(
    process: &Process,
    page: u64,
) -> Result<(), &'static str> {
    use crate::syscall::limits::{sys_getrlimit, sys_prlimit64};
    const WORD: usize = size_of::<usize>();
    const STACK: u32 = 3;
    const CORE: u32 = 4;
    const NOFILE: u32 = 7;

    stage_rlimit64(process, page, 10, 5)?;
    refuses(
        sys_prlimit64(process, 0, NOFILE, page, 0),
        Errno::EINVAL,
        "a soft limit above its hard limit was not EINVAL",
    )?;
    stage_rlimit64(
        process,
        page,
        1024,
        u64::from(ferrix_vfs::fd::MAX_LIMIT) + 1,
    )?;
    refuses(
        sys_prlimit64(process, 0, NOFILE, page, 0),
        Errno::EPERM,
        "RLIMIT_NOFILE above nr_open was not EPERM",
    )?;
    refuses(
        sys_getrlimit(process, 16, page),
        Errno::EINVAL,
        "RLIM_NLIMITS was accepted as a resource",
    )?;
    let nobody = i32::try_from(crate::syscall::registry::PID_MAX).unwrap_or(i32::MAX);
    refuses(
        sys_prlimit64(process, nobody, NOFILE, 0, 0),
        Errno::ESRCH,
        "prlimit64 on a pid nothing has was not ESRCH",
    )?;

    // The stack's default, at the native width: 8 MiB and RLIM_INFINITY.
    answers(
        sys_getrlimit(process, STACK, page),
        0,
        "getrlimit(RLIMIT_STACK) was refused",
    )?;
    let stack: [u8; 16] = read_user(process, page)?;
    if le_at(&stack, 0, WORD) != 8 << 20 || le_at(&stack, WORD, WORD) != usize::MAX as u64 {
        return Err("RLIMIT_STACK is not 8 MiB soft and unlimited hard");
    }
    // Any other resource keeps what it was given.
    stage_rlimit64(process, page, 0, u64::MAX)?;
    answers(
        sys_prlimit64(process, 0, CORE, page, 0),
        0,
        "prlimit64(RLIMIT_CORE) was refused",
    )?;
    answers(
        sys_getrlimit(process, CORE, page),
        0,
        "getrlimit(RLIMIT_CORE) was refused",
    )?;
    if le_at(&read_user::<16>(process, page)?, 0, WORD) != 0 {
        return Err("RLIMIT_CORE did not read back the limit it was set to");
    }
    Ok(())
}

/// `sched_getaffinity` returns the bytes it wrote, writes no more, and sets
/// one bit per running processor; a mask naming none is refused.
fn check_affinity_names_the_running_processors(
    process: &Process,
    page: u64,
) -> Result<(), &'static str> {
    use crate::syscall::limits::{sys_sched_getaffinity, sys_sched_setaffinity};
    const WORD: usize = size_of::<usize>();

    poison_user(process, page, 64)?;
    let written = sys_sched_getaffinity(process, 0, 64, page)
        .map_err(|_| "sched_getaffinity with a 64-byte mask was refused")?;
    if written != crate::smp::count().div_ceil(WORD * 8) * WORD {
        return Err("sched_getaffinity did not return whole words covering every processor");
    }
    let out: [u8; 64] = read_user(process, page)?;
    if !all_are(&out, written, out.len(), UNWRITTEN) {
        return Err("sched_getaffinity wrote past the length it returned");
    }
    let bits: u32 = out
        .get(..written)
        .unwrap_or_default()
        .iter()
        .copied()
        .map(u8::count_ones)
        .sum();
    let running = crate::smp::topology().map_or(1, crate::smp::Topology::online);
    if usize::try_from(bits).ok() != Some(running) || out.first().is_none_or(|byte| byte & 1 == 0) {
        return Err("the affinity mask does not have one bit per running processor");
    }
    refuses(
        sys_sched_getaffinity(process, 0, WORD as u32 - 1, page),
        Errno::EINVAL,
        "an affinity length that is not whole words was accepted",
    )?;
    uaccess::copy_to_user(process.space(), page, &[0_u8; 8])
        .map_err(|_| "could not stage a mask")?;
    refuses(
        sys_sched_setaffinity(process, 0, 8, page),
        Errno::EINVAL,
        "an affinity mask naming no processor was accepted",
    )
}

/// `PR_SET_NAME` keeps fifteen bytes and `PR_GET_NAME` gives them back in
/// sixteen, NUL-terminated, writing nothing past them.
fn check_a_task_name_round_trips(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::attributes::sys_prctl;
    const PR_GET_DUMPABLE: i32 = 3;
    const PR_SET_NAME: i32 = 15;
    const PR_GET_NAME: i32 = 16;

    uaccess::copy_to_user(process.space(), page, b"a-name-longer-than-fifteen\0")
        .map_err(|_| "could not stage a task name")?;
    answers(
        sys_prctl(process, PR_SET_NAME, [page, 0, 0, 0]),
        0,
        "PR_SET_NAME was refused",
    )?;
    poison_user(process, page + 64, 24)?;
    answers(
        sys_prctl(process, PR_GET_NAME, [page + 64, 0, 0, 0]),
        0,
        "PR_GET_NAME was refused",
    )?;
    let out: [u8; 24] = read_user(process, page + 64)?;
    if out.get(..16) != Some(b"a-name-longer-t\0".as_slice()) || !all_are(&out, 16, 24, UNWRITTEN) {
        return Err("PR_GET_NAME did not give back fifteen bytes of the name in sixteen");
    }
    answers(
        sys_prctl(process, PR_GET_DUMPABLE, [0; 4]),
        1,
        "a new process is not dumpable",
    )?;
    refuses(
        sys_prctl(process, 0x7FFF, [0; 4]),
        Errno::EINVAL,
        "an unknown prctl option was not EINVAL",
    )
}

/// Becoming root again is accepted and becoming anyone else is `EPERM`: see
/// `credentials` for why that refusal is the honest answer.
fn check_credentials_refuse_another_user(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::credentials::dispatch as credential;
    use ferrix_linux_abi::nr::Syscall as Call;
    let unchanged = u64::from(u32::MAX);

    if credential(Call::Setuid, &[1000, 0, 0, 0, 0, 0], process) != Some(Err(Errno::EPERM)) {
        return Err("setuid to another user was not refused with EPERM");
    }
    if credential(Call::Setuid, &[0; 6], process) != Some(Ok(0)) {
        return Err("setuid(0) was refused to a process that is already root");
    }
    if credential(Call::Setuid, &[unchanged, 0, 0, 0, 0, 0], process) != Some(Err(Errno::EINVAL)) {
        return Err("setuid(-1) was not EINVAL");
    }
    if credential(
        Call::Setresgid,
        &[unchanged, 0, unchanged, 0, 0, 0],
        process,
    ) != Some(Ok(0))
    {
        return Err("setresgid to root, leaving the rest unchanged, was refused");
    }
    poison_user(process, page, 16)?;
    let ids = [page, page + 4, page + 8, 0, 0, 0];
    if credential(Call::Getresuid, &ids, process) != Some(Ok(0)) {
        return Err("getresuid was refused");
    }
    let out: [u8; 16] = read_user(process, page)?;
    if !all_are(&out, 0, 12, 0) || !all_are(&out, 12, 16, UNWRITTEN) {
        return Err("getresuid did not write three 32-bit zeros and nothing else");
    }

    // `id` asks for the count with a size of zero, then for the list.
    use crate::syscall::credentials::sys_getgroups;
    answers(
        sys_getgroups(process, 0, 0),
        1,
        "getgroups(0, NULL) did not count group 0",
    )?;
    poison_user(process, page, 8)?;
    answers(
        sys_getgroups(process, 4, page),
        1,
        "getgroups did not fill its list",
    )?;
    let groups: [u8; 8] = read_user(process, page)?;
    if !all_are(&groups, 0, 4, 0) || !all_are(&groups, 4, 8, UNWRITTEN) {
        return Err("getgroups did not write group 0 as one 32-bit gid_t");
    }
    if credential(Call::Setgroups, &[1, page + 4, 0, 0, 0, 0], process) != Some(Err(Errno::EPERM)) {
        return Err("setgroups joining a group other than root's was not EPERM");
    }
    Ok(())
}

/// `nanosleep` does not come back until its time has passed, and refuses a
/// nanosecond field of a whole second.
fn check_nanosleep_takes_its_time(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::time::sys_nanosleep;
    const NAP: u64 = 20_000_000;
    let word = size_of::<usize>() as u64;

    write_word(process, page, 0)?;
    write_word(process, page + word, NAP)?;
    let start = crate::timer::now_nanos();
    answers(sys_nanosleep(process, page, 0), 0, "nanosleep was refused")?;
    if crate::timer::now_nanos().saturating_sub(start) < NAP {
        return Err("nanosleep returned before its time had passed");
    }
    write_word(process, page + word, 1_000_000_000)?;
    refuses(
        sys_nanosleep(process, page, 0),
        Errno::EINVAL,
        "a nanosecond field of a whole second was accepted",
    )
}

/// `clock_settime(CLOCK_REALTIME)` moves what `CLOCK_REALTIME` reads and
/// leaves `CLOCK_MONOTONIC` alone, which cannot be set at all. The clock is put
/// back afterwards whatever happened, so nothing that runs later reads 2001.
fn check_setting_the_clock_moves_only_realtime(
    process: &Process,
    page: u64,
) -> Result<(), &'static str> {
    use crate::syscall::time;
    let saved = time::realtime_offset();
    let outcome = set_the_clock_and_read_it_back(process, page);
    time::restore_realtime_offset(saved);
    outcome
}

/// The body of [`check_setting_the_clock_moves_only_realtime`], which puts
/// the clock back whatever this returns.
fn set_the_clock_and_read_it_back(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::time::{self, TimeWidth};
    use ferrix_linux_abi::types::{CLOCK_MONOTONIC, CLOCK_REALTIME};
    const SEPTEMBER_2001: u64 = 1_000_000_000;
    let word = size_of::<usize>() as u64;

    write_word(process, page, SEPTEMBER_2001)?;
    write_word(process, page + word, 0)?;
    answers(
        time::sys_clock_settime(process, CLOCK_REALTIME as i32, page, TimeWidth::Native),
        0,
        "clock_settime(CLOCK_REALTIME) was refused",
    )?;
    let read = |clock: u32| -> Result<u64, &'static str> {
        answers(
            time::sys_clock_gettime(process, u64::from(clock), page + 32, TimeWidth::Native),
            0,
            "clock_gettime was refused",
        )?;
        Ok(le_at(
            &read_user::<8>(process, page + 32)?,
            0,
            size_of::<usize>(),
        ))
    };
    if !(SEPTEMBER_2001..SEPTEMBER_2001 + 60).contains(&read(CLOCK_REALTIME)?) {
        return Err("CLOCK_REALTIME did not read the time it was set to");
    }
    if read(CLOCK_MONOTONIC)? >= SEPTEMBER_2001 {
        return Err("setting CLOCK_REALTIME moved CLOCK_MONOTONIC");
    }
    refuses(
        time::sys_clock_settime(process, CLOCK_MONOTONIC as i32, page, TimeWidth::Native),
        Errno::EINVAL,
        "CLOCK_MONOTONIC could be set",
    )
}

/// `time` answers `gettimeofday`'s seconds, writes the same seconds through a
/// pointer, and is `EFAULT` through an unmapped one. The clock is set to 2001
/// first, so an answer of zero cannot pass for a clock read near the epoch,
/// and put back afterwards whatever happened. Skipped where this build's table
/// has no number for `time`, which is every table but x86-64's.
fn check_time_is_the_realtime_seconds(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::time;
    use ferrix_linux_abi::nr::{Syscall, x86_64};
    const SEPTEMBER_2001_NANOS: i64 = 1_000_000_000_000_000_000;
    if arch::decode_syscall(x86_64::TIME) != Some(Syscall::Time) {
        return Ok(());
    }
    let saved = time::realtime_offset();
    time::restore_realtime_offset(saved.saturating_add(SEPTEMBER_2001_NANOS));
    let outcome = read_the_clock_through_time(process, page);
    time::restore_realtime_offset(saved);
    outcome
}

/// The body of [`check_time_is_the_realtime_seconds`], which puts the clock
/// back whatever this returns.
fn read_the_clock_through_time(process: &Process, page: u64) -> Result<(), &'static str> {
    use crate::syscall::time;
    let seconds_at =
        |at: u64| -> Result<u64, &'static str> { Ok(le_at(&read_user::<8>(process, at)?, 0, 8)) };

    answers(
        time::sys_gettimeofday(process, page),
        0,
        "gettimeofday was refused",
    )?;
    let before = seconds_at(page)?;
    let returned = time::sys_time(process, 0).map_err(|_| "time(NULL) was refused")? as u64;
    if !(before..=before + 1).contains(&returned) {
        return Err("time(NULL) is not gettimeofday's seconds");
    }
    poison_user(process, page + 16, 8)?;
    let written = time::sys_time(process, page + 16).map_err(|_| "time(page) was refused")? as u64;
    if seconds_at(page + 16)? != written || !(returned..=returned + 1).contains(&written) {
        return Err("time did not write the seconds it returned");
    }
    refuses(
        time::sys_time(process, TEST_BASE + 0x10_0000),
        Errno::EFAULT,
        "time through an unmapped address was not EFAULT",
    )
}

/// A name `sethostname` sets is the `nodename` `uname` reports, and a name
/// longer than 64 bytes is refused. The name is forgotten afterwards, so the
/// `uname` check and a person reading `uname -a` both still see `ferrix`.
fn check_a_host_name_reaches_uname(process: &Process, page: u64) -> Result<(), &'static str> {
    uaccess::copy_to_user(process.space(), page, b"check-host")
        .map_err(|_| "could not stage a host name")?;
    let outcome = answers(
        system::sys_sethostname(process, page, 10),
        0,
        "sethostname was refused",
    )
    .and_then(|()| {
        answers(
            system::sys_uname(process, page + 512),
            0,
            "uname was refused",
        )
    })
    .and_then(|()| {
        let out: [u8; 130] = read_user(process, page + 512)?;
        if out.get(65..76) != Some(b"check-host\0".as_slice()) || !all_are(&out, 75, 130, 0) {
            return Err("uname's nodename is not the name sethostname set");
        }
        Ok(())
    })
    .and_then(|()| {
        refuses(
            system::sys_sethostname(process, page, 65),
            Errno::EINVAL,
            "a 65-byte host name was accepted",
        )
    });
    system::forget_hostname();
    outcome
}

/// `socket` is `EAFNOSUPPORT` for a family Linux has, a socket call on the
/// console is `ENOTSOCK`, and one on a closed descriptor is `EBADF`.
fn check_sockets_are_refused_honestly(process: &Process) -> Result<(), &'static str> {
    use crate::syscall::sockets;
    use ferrix_linux_abi::nr::Syscall as Call;
    const AF_INET: i32 = 2;
    const SOCK_STREAM: u32 = 1;

    refuses(
        sockets::sys_socket(AF_INET, SOCK_STREAM),
        Errno::EAFNOSUPPORT,
        "socket(AF_INET, SOCK_STREAM) was not EAFNOSUPPORT",
    )?;
    refuses(
        sockets::sys_socket(AF_INET, SOCK_STREAM | 0x100),
        Errno::EINVAL,
        "a socket type with an unknown flag was not EINVAL",
    )?;
    if sockets::dispatch(Call::Bind, &[1, 0, 0, 0, 0, 0], process) != Some(Err(Errno::ENOTSOCK)) {
        return Err("bind on the console was not ENOTSOCK");
    }
    if sockets::dispatch(Call::Listen, &[99, 0, 0, 0, 0, 0], process) != Some(Err(Errno::EBADF)) {
        return Err("listen on a closed descriptor was not EBADF");
    }
    Ok(())
}

/// A process that ends closes its descriptors then, not when it is reaped.
///
/// A never-started process holds a pipe's only write end; after its kill, the
/// read end must see the hangup that end of file is made of, while the process
/// itself is still referenced here, as an unreaped child is by its parent.
/// Without it, `ls | wc -l` in the shell hangs.
fn check_an_ended_process_closes_its_descriptors() -> Result<(), &'static str> {
    let holder = process::new_for_check().map_err(|_| "no process to hold a pipe's write end")?;
    let (reader, writer) = crate::fs::pipe::new_pipe(false).map_err(|_| "could not make a pipe")?;
    let _fd = holder
        .files()
        .lock()
        .insert(writer, false)
        .map_err(|_| "could not give a process a pipe's write end")?;
    if reader.poll().hangup {
        return Err("a pipe's read end saw a hangup while its writer was still open");
    }
    process::kill(&holder, 137);
    if !reader.poll().hangup {
        return Err("a process that ended kept its descriptors open until it was let go");
    }
    Ok(())
}
