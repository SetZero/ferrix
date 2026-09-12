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
    MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, O_APPEND, O_CLOEXEC, O_CREAT, O_EXCL, O_RDONLY,
    O_RDWR, O_TRUNC, PROT_READ, PROT_WRITE, SEEK_CUR, SEEK_END, SEEK_SET, TCGETS,
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
    /// What a program exited with that `execve`d a program which exists, then
    /// one that does not: 42 and 2 when right.
    pub(crate) execed: Option<(i32, i32)>,
    /// Processes the pid registry numbered, found, listed and let go.
    pub(crate) pids: u32,
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

    let user_status = check_a_program_runs_in_user_mode()?;
    let concurrent = check_two_programs_take_turns_on_one_processor()?;
    let killed = check_a_program_is_killed_from_outside()?;
    let forked = check_a_forked_child_is_waited_for()?;
    let execed = check_execve_replaces_the_program()?;

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
        execed,
        pids,
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
    check_brk_grows_and_shrinks(&process)?;
    check_set_tid_address_answers_with_a_thread_id(&process)?;
    check_uname_says_linux_to_a_script_and_ferrix_to_a_person(&process)?;
    check_poll_reports_ready_invalid_and_skipped(&process)?;
    check_a_signal_disposition_reads_back_as_it_was_set(&process)?;
    check_the_blocked_mask_follows_how(&process)?;
    check_an_alternate_stack_is_recorded_and_refused_when_small(&process)?;
    check_an_image_loads_where_its_headers_say(&process)?;
    check_the_loader_refuses_what_it_cannot_run(&process)?;
    check_descriptors(&process)?;
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

    if signal::sys_sigaltstack(process, 0, old) != Ok(0) || read_old()? != (0, SS_DISABLE, 0) {
        return Err("with no alternate stack, sigaltstack did not report SS_DISABLE");
    }
    stage(0x0001_0000, 0, 1024)?;
    if signal::sys_sigaltstack(process, at, 0) != Err(Errno::ENOMEM) {
        return Err("an alternate stack below MINSIGSTKSZ was accepted");
    }
    stage(0x0001_0000, 5, 65536)?;
    if signal::sys_sigaltstack(process, at, 0) != Err(Errno::EINVAL) {
        return Err("a nonsense ss_flags was accepted");
    }
    stage(0x0001_0000, 0, 65536)?;
    if signal::sys_sigaltstack(process, at, 0) != Ok(0) {
        return Err("a valid alternate stack was refused");
    }
    if signal::sys_sigaltstack(process, 0, old) != Ok(0) || read_old()? != (0x0001_0000, 0, 65536) {
        return Err("the installed alternate stack did not read back");
    }
    stage(0, SS_DISABLE, 0)?;
    if signal::sys_sigaltstack(process, at, old) != Ok(0) || read_old()? != (0x0001_0000, 0, 65536)
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
        fd::sys_ioctl(process, 1, TCGETS, 0),
        Errno::ENOTTY,
        "an ioctl on the console was not ENOTTY",
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
        let _warm = check_path_calls()?;
        let cached = fs::namespace().cached();
        let before = mm::free_frames();
        let mut report = check_path_calls()?;
        report.leaked = i64::try_from(before).unwrap_or(i64::MAX)
            - i64::try_from(mm::free_frames()).unwrap_or(i64::MAX);
        report.cache_growth = i64::try_from(fs::namespace().cached()).unwrap_or(i64::MAX)
            - i64::try_from(cached).unwrap_or(i64::MAX);
        Ok(report)
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

/// Where [`arch::USER_EXEC_PROGRAM`] looks for its target.
const EXEC_TARGET: &[u8] = b"/exec-target";

/// A program `execve`s another by path and takes on its status; with the file
/// gone, the same program gets `ENOENT` back and exits with it.
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
        .open(&ctx, None, EXEC_TARGET, &create, 0o755)
        .map_err(|_| "could not create the program execve is to run")?;
    if file.write(&target) != Ok(target.len()) {
        return Err("could not write the program execve is to run");
    }
    drop(file);

    let found = exec::run(
        &caller,
        &[b"/exec-caller"],
        &[],
        [0x5a; ferrix_ustack::RANDOM_BYTES],
    )
    .map_err(|_| "a program that calls execve could not be started")?;
    ns.unlink(&ctx, None, EXEC_TARGET)
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
    if missing != Errno::ENOENT.0 as i32 {
        return Err("execve of a path that does not exist did not return ENOENT");
    }
    Ok(Some((found, missing)))
}
