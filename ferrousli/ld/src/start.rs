//! `_start`, and the relocation of the loader by itself.
//!
//! # What may be touched here, and what may not
//!
//! Between `_start` and the end of [`relocate_self`] the loader's own data is
//! not yet usable: every pointer the linker stored in it holds a link-time
//! address, and the object is somewhere else. Code on this path may use local
//! variables, its arguments, and memory it reaches through a pointer it
//! computed at run time. It may not read a `static`, take the address of a
//! function through anything but a direct call, use a trait object, or index
//! a table the compiler chose to put in `.data.rel.ro`.
//!
//! Nothing in the language enforces that, so two things keep it true. The
//! code on this path is kept short enough to read in one go. And the offsets
//! it needs are computed, not stored: [`dynamic_here`] asks the assembler for
//! the run-time address of `_DYNAMIC` with one PC-relative instruction, which
//! is the one address obtainable before there is any other.
//!
//! # Why the load bias is not the aux vector's `AT_BASE`
//!
//! It could be, and `AT_BASE` is right there on the stack. But the loader has
//! to work when it is *not* what the kernel entered — `ldd` runs the loader
//! as a program, and so does a `dlopen` of it — and then `AT_BASE` is zero
//! and the program's own base is in `AT_PHDR`. The difference between the
//! run-time and link-time addresses of `_DYNAMIC` is the bias in both cases,
//! and it needs nothing but this object.

use crate::elf::{DT_NULL, DT_RELA, DT_RELAENT, DT_RELASZ, Dyn, Rela};
use crate::scope::Scope;

/// The objects whose destructors still have to run when the program exits.
///
/// The loader normally disappears from the call stack before the program
/// starts, but it remains mapped.  Keeping the completed scope here gives the
/// `rtld_fini` callback a stable record of the libraries it must finish.
static mut FINI_SCOPE: Scope = Scope::new();

/// The run-time address of this object's `_DYNAMIC`, from one PC-relative
/// instruction and nothing else.
///
/// The link-time address of the same symbol is `d_val` of no tag, so it
/// cannot be read the other way round; what gives the bias is that the
/// relocations' own `r_offset` fields are link-time addresses, and applying
/// the bias to them is the whole of the work below.
#[inline(always)]
#[cfg(target_arch = "x86_64")]
fn dynamic_here() -> *const Dyn {
    let at: *const Dyn;
    // SAFETY: `lea` computes an address and touches no memory. `_DYNAMIC` is
    // defined by the linker in every object that has a dynamic table, and an
    // object without one is not something this loader can be.
    unsafe {
        core::arch::asm!(
            "lea {}, [rip + _DYNAMIC]",
            out(reg) at,
            options(nomem, nostack, preserves_flags),
        );
    }
    at
}

/// The link-time address of `_DYNAMIC`, taken from the first slot of the
/// global offset table.
///
/// The ABI of all three architectures says `GOT[0]` holds the link-time
/// address of `_DYNAMIC`, written by the linker and never relocated. Its
/// run-time address less that is the bias.
#[inline(always)]
#[cfg(target_arch = "x86_64")]
fn dynamic_linked_at() -> usize {
    let got: *const usize;
    // SAFETY: as above; `_GLOBAL_OFFSET_TABLE_` is the linker's own symbol.
    unsafe {
        core::arch::asm!(
            "lea {}, [rip + _GLOBAL_OFFSET_TABLE_]",
            out(reg) got,
            options(nomem, nostack, preserves_flags),
        );
    }
    // SAFETY: `GOT[0]` is a word the linker wrote and the loader owns.
    unsafe { got.read() }
}

/// How far this object was moved from where it was linked.
#[inline(always)]
fn load_bias() -> usize {
    (dynamic_here() as usize).wrapping_sub(dynamic_linked_at())
}

/// Apply this object's own `RELATIVE` relocations, and return its load bias.
///
/// Only `RELATIVE`: a loader is linked with no undefined symbols, so every
/// relocation it carries is one that adds the bias to a stored address. One
/// that is not is a loader built wrongly, and is skipped rather than guessed
/// at — there is no way to report it yet, and a wrong guess writes a pointer
/// that faults later somewhere unrelated.
///
/// Idempotent it is not: applying it twice would double every address. It is
/// called once, from [`entry`].
///
/// # Safety
///
/// Called once, before anything reads this object's data.
#[inline(always)]
unsafe fn relocate_self() -> usize {
    let bias = load_bias();
    let mut entry = dynamic_here();
    let mut rela: usize = 0;
    let mut size: usize = 0;
    let mut stride: usize = size_of::<Rela>();

    // The dynamic table's addresses are link-time ones, so each is biased as
    // it is read. Walked with `read()` rather than as a slice: its length is
    // its terminator, which a slice cannot express.
    loop {
        // SAFETY: the table runs to a `DT_NULL` entry, and this stops there.
        let this = unsafe { entry.read() };
        match this.d_tag {
            DT_NULL => break,
            DT_RELA => rela = this.d_val.wrapping_add(bias),
            DT_RELASZ => size = this.d_val,
            DT_RELAENT => stride = this.d_val,
            _ => {}
        }
        // SAFETY: the entry just read was not the terminator, so another
        // follows it.
        entry = unsafe { entry.add(1) };
    }

    if rela == 0 || stride == 0 {
        return bias;
    }
    let mut at = rela;
    let end = rela.wrapping_add(size);
    while at < end {
        // SAFETY: `at` walks the table the dynamic entries described, in
        // steps of the size they gave, and stops at its end.
        let relocation = unsafe { (at as *const Rela).read() };
        if crate::arch::R_RELATIVE == crate::elf::r_type(relocation.r_info) {
            let target = relocation.r_offset.wrapping_add(bias);
            let value = bias.wrapping_add_signed(relocation.r_addend);
            // SAFETY: the target is inside this object's own writable
            // segments, which the linker guarantees of every relocation it
            // emits against it.
            unsafe { (target as *mut usize).write(value) };
        }
        at = at.wrapping_add(stride);
    }
    bias
}

/// What `_start` calls: repair the loader, then hand over.
///
/// # Safety
///
/// Called by `_start` alone, with `sp` the stack pointer the process was
/// entered with.
#[unsafe(no_mangle)]
unsafe extern "C" fn dlstart(sp: *const usize) -> ! {
    // SAFETY: the first thing this process does, and done once.
    let _bias = unsafe { relocate_self() };
    // Past here the loader's own data is usable and this is ordinary Rust.
    // SAFETY: the stack pointer is the one the kernel gave, and relocation is
    // done.
    unsafe { crate::main(sp) }
}

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".text",
    ".globl _start",
    ".type _start, @function",
    "_start:",
    // The outermost frame: a zero frame pointer ends every backtrace here.
    "    xor ebp, ebp",
    // The stack pointer is the argument, and it is where argc is. It has to
    // be read before the stack is aligned, which moves it.
    "    mov rdi, rsp",
    // The ABI wants a 16-byte aligned stack at every call and the kernel does
    // not promise one.
    "    and rsp, -16",
    // A direct call, which is PC-relative and so needs no relocation. Nothing
    // here may go through the procedure linkage table.
    "    call dlstart",
    // `dlstart` does not return.
    "    ud2",
);

/// Hand the process over to the program.
///
/// The program's `_start` expects exactly what the kernel left: the stack
/// pointer at `argc`, with `argv`, the environment and the auxiliary vector
/// above it. The loader has been using that stack, so it is restored rather
/// than continued from, and nothing of the loader's frames survives.
///
/// `rdx` holds [`dl_fini`]. The ABI says it is a function for the program to
/// register with `atexit` — glibc's `crt1.o` reads it and passes it on — so
/// dependency destructors run after the program's handlers and before its
/// own `.fini_array`.
///
/// # Safety
///
/// `entry` must be the program's entry point, with every object it needs
/// mapped and relocated, and `stack` the pointer the process was entered
/// with.
#[cfg(target_arch = "x86_64")]
pub unsafe fn enter(entry: usize, stack: &crate::auxv::Stack) -> ! {
    // SAFETY: this never returns, and what it jumps to is a program entry
    // point with the stack the kernel built beneath it.
    unsafe {
        core::arch::asm!(
            "mov rsp, {stack}",
            "lea rdx, [rip + dl_fini]",
            "jmp {entry}",
            stack = in(reg) stack.sp,
            entry = in(reg) entry,
            options(noreturn, nostack),
        )
    }
}

/// Save the completed scope for the callback the program registers at exit.
///
/// # Safety
///
/// Called once, after every object in `scope` has been mapped and relocated,
/// and before [`enter`] makes the program reachable.
pub(crate) unsafe fn save_fini_scope(scope: Scope) {
    // SAFETY: this loader has one initial thread and calls this once before it
    // transfers control to the program. `dl_fini` only reads the scope later,
    // while `exit` is ending the process.
    unsafe { (&raw mut FINI_SCOPE).write(scope) };
}

/// Run the `DT_FINI_ARRAY` and `DT_FINI` entries of every loaded dependency.
///
/// The program's own array belongs to its C runtime, which calls it after
/// this callback. Dependencies were initialised from the end of the scope to
/// the beginning, so walking them from the beginning to the end reverses that
/// order. Within one array, ELF requires last entry first.
#[unsafe(no_mangle)]
unsafe extern "C" fn dl_fini() {
    // SAFETY: `save_fini_scope` published this once before the program began;
    // the callback only reads it while the process exits.
    let scope = unsafe { &*(&raw const FINI_SCOPE) };
    let mut index = 1;
    while index < scope.len() {
        let Some(object) = scope.get(index) else {
            index += 1;
            continue;
        };
        let table = object.fini_array;
        if table.is_present() {
            let mut at = table.at.wrapping_add(table.size);
            while at > table.at {
                at = at.wrapping_sub(size_of::<usize>());
                // SAFETY: the dynamic table describes pointer-aligned entries
                // in this mapped object's `DT_FINI_ARRAY`.
                let entry = unsafe { (at as *const usize).read() };
                if entry != 0 && entry != usize::MAX {
                    // SAFETY: the loader relocated every entry before the
                    // program could start, and this is its destructor call.
                    unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(entry)() };
                }
            }
        }
        if object.fini != 0 {
            // SAFETY: `DT_FINI` is a relocated function in this object. ELF
            // calls it after the same object's finaliser array.
            unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(object.fini)() };
        }
        index += 1;
    }
}
