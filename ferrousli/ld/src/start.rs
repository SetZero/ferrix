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
//! is the one address obtainable before there is any other. On AArch64 and
//! ARMv7-A the one address asked for is the ELF header's, `__ehdr_start`,
//! and `_DYNAMIC`'s is read from the program headers behind it: that needs
//! nothing of the linker's GOT conventions, which differ between the three.
//!
//! # Why the load bias is not the aux vector's `AT_BASE`
//!
//! It could be, and `AT_BASE` is right there on the stack. But the loader has
//! to work when it is *not* what the kernel entered — `ldd` runs the loader
//! as a program, and so does a `dlopen` of it — and then `AT_BASE` is zero
//! and the program's own base is in `AT_PHDR`. The difference between the
//! run-time and link-time addresses of `_DYNAMIC` is the bias in both cases,
//! and it needs nothing but this object.

use crate::elf::{
    DT_NULL, DT_REL, DT_RELA, DT_RELAENT, DT_RELASZ, DT_RELENT, DT_RELSZ, Dyn, Rel, Rela,
};
use crate::scope::Scope;

/// The completed scope, kept for the calls the program makes back into the
/// loader after it starts.
///
/// The loader disappears from the call stack before the program starts, but
/// it remains mapped. The `rtld_fini` callback finds the libraries it must
/// finish here, and [`crate::interface`] the TLS images a new thread needs.
static mut SCOPE: Scope = Scope::new();

/// The completed scope. Empty until [`save_scope`] runs, which is before the
/// program can call anything that reads it.
pub(crate) fn scope() -> &'static Scope {
    let scope = &raw const SCOPE;
    // SAFETY: written once by `save_scope` before the program starts, and
    // only read afterwards.
    unsafe { &*scope }
}

/// The completed scope, to write: `dlopen`'s, with [`crate::dl`]'s lock held.
pub(crate) fn scope_mut() -> *mut Scope {
    &raw mut SCOPE
}

/// The loader as an object in its own scope: its dynamic table, at the bias
/// it was loaded with.
pub(crate) fn own_object() -> Option<crate::object::Object> {
    let base = load_bias();
    // SAFETY: `dynamic_here` is this object's own `_DYNAMIC`, at its run-time
    // address, and `base` how far it moved.
    let mut object = unsafe { crate::object::Object::read(base, dynamic_here()) }?;
    // The loader is linked at zero with its ELF header at the start of its
    // first segment, so the header is at the bias, and the program headers
    // at `e_phoff` past it: where `dl_iterate_phdr` reports them.
    let (phoff, phnum) = header_table(base);
    object.phdr = base.wrapping_add(phoff);
    object.phnum = phnum;
    Some(object)
}

/// Where the program headers are, as an offset from the ELF header at
/// `ehdr`, and how many there are.
#[inline(always)]
fn header_table(ehdr: usize) -> (usize, usize) {
    #[cfg(target_pointer_width = "64")]
    let (phoff_at, phnum_at) = (32, 56);
    #[cfg(target_pointer_width = "32")]
    let (phoff_at, phnum_at) = (28, 44);
    // SAFETY: the ELF header is mapped, readable, at `ehdr`: this object's
    // first segment holds it.
    let phoff = unsafe { (ehdr.wrapping_add(phoff_at) as *const usize).read_unaligned() };
    // SAFETY: as above.
    let phnum = unsafe { (ehdr.wrapping_add(phnum_at) as *const u16).read_unaligned() };
    (phoff, usize::from(phnum))
}

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
#[cfg(target_arch = "x86_64")]
fn load_bias() -> usize {
    (dynamic_here() as usize).wrapping_sub(dynamic_linked_at())
}

/// The run-time address of this object's ELF header, `__ehdr_start`, from
/// one PC-relative computation and nothing else.
#[inline(always)]
#[cfg(target_arch = "aarch64")]
fn ehdr_here() -> usize {
    let at: usize;
    // SAFETY: `adrp` and `add` compute an address and touch no memory. The
    // linker defines `__ehdr_start` in every object whose first segment
    // holds the ELF header, which the loader's does.
    unsafe {
        core::arch::asm!(
            "adrp {0}, __ehdr_start",
            "add {0}, {0}, :lo12:__ehdr_start",
            out(reg) at,
            options(pure, nomem, nostack, preserves_flags),
        );
    }
    at
}

/// The run-time address of this object's ELF header, `__ehdr_start`: its
/// distance from the `add` below, which the assembler stores beside it, plus
/// the `pc` that `add` reads, eight bytes past itself in ARM state.
#[inline(always)]
#[cfg(target_arch = "arm")]
fn ehdr_here() -> usize {
    let at: usize;
    // SAFETY: reads one word of this function's own code and computes an
    // address from it. `__ehdr_start` is defined as on AArch64.
    unsafe {
        core::arch::asm!(
            "ldr {0}, 1f",
            "2:",
            "add {0}, pc, {0}",
            "b 3f",
            "1:",
            ".word __ehdr_start - (2b + 8)",
            "3:",
            out(reg) at,
            options(pure, readonly, nostack, preserves_flags),
        );
    }
    at
}

/// The first program header of `kind` in the object whose ELF header is at
/// `ehdr`, if it has one.
#[inline(always)]
#[cfg(not(target_arch = "x86_64"))]
fn header_of(ehdr: usize, kind: u32) -> Option<crate::elf::Phdr> {
    let (phoff, phnum) = header_table(ehdr);
    let mut index = 0;
    while index < phnum {
        let at = ehdr
            .wrapping_add(phoff)
            .wrapping_add(index * size_of::<crate::elf::Phdr>());
        // SAFETY: the program headers are mapped with the ELF header, and
        // `index` is below their count.
        let header = unsafe { (at as *const crate::elf::Phdr).read_unaligned() };
        if header.p_type == kind {
            return Some(header);
        }
        index += 1;
    }
    None
}

/// How far this object was moved from where it was linked: where its ELF
/// header is, less where the segment holding it was linked.
#[inline(always)]
#[cfg(not(target_arch = "x86_64"))]
fn load_bias() -> usize {
    let ehdr = ehdr_here();
    // The segment that holds the header is the one mapped from offset zero,
    // the first `PT_LOAD`; with none, the object was linked at zero.
    let linked_at = header_of(ehdr, crate::elf::PT_LOAD)
        .filter(|header| header.p_offset == 0)
        .map_or(0, |header| header.p_vaddr as usize);
    ehdr.wrapping_sub(linked_at)
}

/// The run-time address of this object's `_DYNAMIC`: its `PT_DYNAMIC`'s
/// link-time address, moved by the bias.
#[inline(always)]
#[cfg(not(target_arch = "x86_64"))]
fn dynamic_here() -> *const Dyn {
    let ehdr = ehdr_here();
    let at = header_of(ehdr, crate::elf::PT_DYNAMIC).map_or(0, |header| header.p_vaddr as usize);
    load_bias().wrapping_add(at) as *const Dyn
}

/// Apply this object's own `RELATIVE` relocations, and return its load bias.
/// x86-64 and AArch64 link them as `RELA`, with the addend in the entry, and
/// ARMv7-A as `REL`, with it in the word relocated.
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
    let mut rela_size: usize = 0;
    let mut rela_stride: usize = size_of::<Rela>();
    let mut rel: usize = 0;
    let mut rel_size: usize = 0;
    let mut rel_stride: usize = size_of::<Rel>();

    // The dynamic table's addresses are link-time ones, so each is biased as
    // it is read. Walked with `read()` rather than as a slice: its length is
    // its terminator, which a slice cannot express. An `if` chain rather
    // than a `match`, which the compiler may turn into a jump table in
    // `.data.rel.ro` -- a table nothing has relocated yet.
    loop {
        // SAFETY: the table runs to a `DT_NULL` entry, and this stops there.
        let this = unsafe { entry.read() };
        let (tag, value) = (this.d_tag, this.d_val);
        if tag == DT_NULL {
            break;
        } else if tag == DT_RELA {
            rela = value.wrapping_add(bias);
        } else if tag == DT_RELASZ {
            rela_size = value;
        } else if tag == DT_RELAENT {
            rela_stride = value;
        } else if tag == DT_REL {
            rel = value.wrapping_add(bias);
        } else if tag == DT_RELSZ {
            rel_size = value;
        } else if tag == DT_RELENT {
            rel_stride = value;
        }
        // The entry just read was not the terminator, so another
        // follows it.
        entry = entry.wrapping_add(1);
    }

    if rela != 0 && rela_stride != 0 {
        let mut at = rela;
        let end = rela.wrapping_add(rela_size);
        while at < end {
            // SAFETY: `at` walks the table the dynamic entries described, in
            // steps of the size they gave, and stops at its end.
            let relocation = unsafe { (at as *const Rela).read() };
            if crate::arch::R_RELATIVE == crate::elf::r_type(relocation.r_info) {
                let target = relocation.r_offset.wrapping_add(bias);
                let value = bias.wrapping_add_signed(relocation.r_addend);
                // SAFETY: the target is inside this object's own writable
                // segments, which the linker guarantees of every relocation
                // it emits against it.
                unsafe { (target as *mut usize).write(value) };
            }
            at = at.wrapping_add(rela_stride);
        }
    }
    if rel != 0 && rel_stride != 0 {
        let mut at = rel;
        let end = rel.wrapping_add(rel_size);
        while at < end {
            // SAFETY: as above, for the `REL` table.
            let relocation = unsafe { (at as *const Rel).read() };
            if crate::arch::R_RELATIVE == crate::elf::r_type(relocation.r_info) {
                let target = relocation.r_offset.wrapping_add(bias) as *mut usize;
                // SAFETY: as above; the addend is the word being relocated.
                let addend = unsafe { target.read() };
                // SAFETY: as above.
                unsafe { target.write(addend.wrapping_add(bias)) };
            }
            at = at.wrapping_add(rel_stride);
        }
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

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".text",
    ".globl _start",
    ".type _start, %function",
    "_start:",
    // The outermost frame: a zero frame pointer and link register end every
    // backtrace here.
    "    mov x29, #0",
    "    mov x30, #0",
    // The stack pointer is the argument, and it is where argc is. The kernel
    // leaves it 16-byte aligned, which AArch64 requires of every access
    // through it.
    "    mov x0, sp",
    // A direct branch, which is PC-relative and so needs no relocation.
    "    bl dlstart",
    "    udf #0",
);

#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".text",
    ".arm",
    ".globl _start",
    ".type _start, %function",
    "_start:",
    // The outermost frame, as above.
    "    mov fp, #0",
    "    mov lr, #0",
    // The stack pointer is the argument. AAPCS wants it 8-byte aligned at a
    // call, and it is aligned to 16 here as on the others.
    "    mov r0, sp",
    "    bic r1, r0, #15",
    "    mov sp, r1",
    "    bl dlstart",
    "    udf #0",
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
pub(crate) unsafe fn enter(entry: usize, stack: &crate::auxv::Stack) -> ! {
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

/// [`enter`] on AArch64: the exit callback goes in `x0`, as the ABI says.
///
/// # Safety
///
/// As on x86-64.
#[cfg(target_arch = "aarch64")]
pub(crate) unsafe fn enter(entry: usize, stack: &crate::auxv::Stack) -> ! {
    let fini = dl_fini as unsafe extern "C" fn() as usize;
    // SAFETY: as on x86-64.
    unsafe {
        core::arch::asm!(
            "mov sp, {stack}",
            "mov x29, #0",
            "mov x30, #0",
            "br {entry}",
            stack = in(reg) stack.sp,
            entry = in(reg) entry,
            in("x0") fini,
            options(noreturn, nostack),
        )
    }
}

/// [`enter`] on ARMv7-A: the exit callback goes in `r0`, as the ABI says.
///
/// # Safety
///
/// As on x86-64.
#[cfg(target_arch = "arm")]
pub(crate) unsafe fn enter(entry: usize, stack: &crate::auxv::Stack) -> ! {
    let fini = dl_fini as unsafe extern "C" fn() as usize;
    // SAFETY: as on x86-64.
    unsafe {
        core::arch::asm!(
            "mov sp, {stack}",
            "mov lr, #0",
            "bx {entry}",
            stack = in(reg) stack.sp,
            entry = in(reg) entry,
            in("r0") fini,
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
pub(crate) unsafe fn save_scope(scope: Scope) {
    let at = &raw mut SCOPE;
    // SAFETY: this loader has one initial thread and calls this once before it
    // transfers control to the program. Everything else only reads it later.
    unsafe { at.write(scope) };
}

/// Run the `DT_FINI_ARRAY` and `DT_FINI` entries of every initialised
/// object, in the reverse of the order they were initialised
/// ([`crate::dl::initialised`]).
///
/// That puts a `dlopen`ed library, initialised last, first; then the program,
/// when the loader initialised it at `libferrousli.so`'s request (see
/// [`crate::interface`]) -- a C library linked into the program runs its
/// own; then the libraries loaded at start-up, which were initialised
/// dependencies first. Within one array, ELF requires last entry first.
///
/// Each object is copied out under [`crate::dl`]'s lock and finished without
/// it, since a destructor may call `dlsym` as readily as a constructor.
#[unsafe(no_mangle)]
unsafe extern "C" fn dl_fini() {
    for index in crate::dl::initialised().rev() {
        let object = crate::dl::with_scope(|scope| scope.get(index).copied());
        let Some(object) = object.filter(|object| !object.is_loader) else {
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
                    // program could start, so each is a destructor.
                    let destructor =
                        unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(entry) };
                    // SAFETY: calling it is what `DT_FINI_ARRAY` is for.
                    unsafe { destructor() };
                }
            }
        }
        if object.fini != 0 {
            // SAFETY: `DT_FINI` is a relocated function in this object.
            let fini =
                unsafe { core::mem::transmute::<usize, unsafe extern "C" fn()>(object.fini) };
            // SAFETY: ELF calls it after the same object's finaliser array.
            unsafe { fini() };
        }
    }
}
