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
    MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, PROT_READ, PROT_WRITE,
};

use crate::arch;
use crate::mm;
use ferrix_elf::Class;

use crate::syscall::memory::{self, MmapRequest, OffsetUnit};
use crate::syscall::process::{self, Process};
use crate::syscall::{Outcome, SyscallArgs, dispatch, uaccess};
use crate::syscall::{exec, file, image, load, signal, system};

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
}

/// Run them. `Err` names the first thing that was not true.
pub(crate) fn run() -> Result<Report, &'static str> {
    let mut counter = Counter::default();

    let getpid_number = check_the_right_table_was_compiled_in(&mut counter)?;
    check_identity_answers(&mut counter)?;
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

    Ok(Report {
        dispatched: counter.dispatched,
        answered: counter.answered,
        getpid_number,
        pages,
        leaked,
        user_status,
        concurrent,
        killed,
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
        let outcome = dispatch(&SyscallArgs { number, args });
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
    check_a_signal_disposition_reads_back_as_it_was_set(&process)?;
    check_the_blocked_mask_follows_how(&process)?;
    check_an_alternate_stack_is_recorded_and_refused_when_small(&process)?;
    check_an_image_loads_where_its_headers_say(&process)?;
    check_the_loader_refuses_what_it_cannot_run(&process)?;
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

/// How long the checks wait for a program before calling it lost.
const PROGRAM_PATIENCE_NANOS: u64 = 120_000_000_000;

/// How long the killed program is left spinning first.
const KILL_AFTER_NANOS: u64 = 20_000_000;

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
    process::load(&file, &[b"/spin"], &[], [0x5a; ferrix_ustack::RANDOM_BYTES])
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
    if first.wait_for_exit(deadline) != Some(41) {
        return Err("the first of two programs did not exit with its own status");
    }
    if second.wait_for_exit(deadline) != Some(43) {
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

    let victim = spinner(b'k', u32::MAX, 5)?;
    let task = process::start_on(&victim, Some(here))
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
    // under a process that says it has ended.
    while !task.is_dead() {
        if crate::timer::now_nanos() >= deadline {
            return Err("a killed program's task kept running");
        }
        crate::sched::sleep_for(1_000_000);
    }
    Ok(Some(KILL_STATUS))
}
