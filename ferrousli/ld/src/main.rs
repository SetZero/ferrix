//! `ld-ferrousli`: the dynamic loader.
//!
//! The kernel loads a dynamically linked program and this loader together,
//! places each at a base of its own, and enters *this* one, with the
//! auxiliary vector saying where both went — `AT_BASE` here, `AT_PHDR`,
//! `AT_PHNUM` and `AT_ENTRY` for the program. Everything between that and the
//! program's first instruction is this program's work: find what the program
//! needs, map it, resolve what it imports, run its initialisers, and jump to
//! `AT_ENTRY`.
//!
//! # The first thing it must do is repair itself
//!
//! This loader is a position-independent object placed at an address it did
//! not choose, and nothing has relocated it. Every pointer stored in its own
//! data — a `static` holding a reference, a table of function pointers, a
//! string constant's address — still holds the address the linker assumed.
//! Reading one before the relocations are applied reads whatever is at that
//! address, which is nothing.
//!
//! So [`start::relocate_self`] runs first, and everything it and its callers
//! touch before it returns has to be a local variable or something reached
//! through a pointer computed at run time. That rule is what
//! `start::CHECKED_BY` describes and what the test beside it enforces: a
//! `static` read from that path is a bug that shows up as a fault at a
//! address near zero, long after the line that caused it.
//!
//! After it returns, this is an ordinary Rust program.
//!
//! # No lazy binding
//!
//! Everything is resolved when it is loaded, as `LD_BIND_NOW` asks. There is
//! no resolver trampoline, which is a piece of assembly per architecture that
//! this loader does not have to carry, and no relocation happens after the
//! program starts running except through `dlopen`.

#![no_std]
#![no_main]
// ELF's fields are 64 bits wide in a 64-bit object and 32 in a 32-bit one,
// and C's `long` with them, so a cast or `from` between one of them and
// `usize` or a fixed-width type is needed on some targets and the identity on
// the others, where these two lints would reject it. The library allows them
// for the same reason.
#![allow(
    clippy::unnecessary_cast,
    clippy::useless_conversion,
    reason = "ELF's and C's field widths differ between the targets"
)]

mod arch;
mod auxv;
mod dl;
mod elf;
mod interface;
mod map;
mod mem;
mod object;
mod reloc;
mod report;
mod scope;
mod start;
mod sym;
mod tls;

// The library's own system call module, used as it is rather than written
// again. The loader cannot *call* the C library -- it is the half of it that
// runs before there is one -- but the numbers must still come from
// `tools/gen-abi.py`'s table, which `ferrousli/CONVENTIONS.md` requires of
// every call site here. A path dependency would not do it: `ferrousli` is a
// `staticlib`, which no Rust crate can link against.
#[path = "../../src/syscall.rs"]
#[allow(
    unreachable_pub,
    dead_code,
    reason = "the library's module, used whole: its items are `pub` for the \
              library and most of its call numbers are for calls the loader \
              does not make"
)]
mod sys;

use core::ffi::{c_char, c_int};

/// What the loader exits with when it cannot do its job.
///
/// A loader has nowhere to report to: there is no C library to set `errno` in
/// and no program yet to return to. glibc's writes a line to standard error
/// and exits 127, which is what a shell reports for "command not found", and
/// this does the same.
const FAILED: c_int = 127;

/// What the loader says when it is run with no program to load.
///
/// A `static` holding a reference, so its bytes are a pointer the linker
/// wrote as a link-time address with an `R_*_RELATIVE` relocation beside it.
/// Read before [`start`]'s self-relocation it points nowhere, which is why
/// this line appearing at all is the proof that relocation happened and not
/// merely that the loader ran.
static USAGE: &str = "ld-ferrousli: usage: ld-ferrousli <program> [arguments]\n";

/// Where the loader goes once it has repaired itself.
///
/// # Safety
///
/// Called by [`start`] alone, once, with `sp` the stack pointer the kernel
/// entered the process with and the loader's own relocations already applied.
unsafe extern "C" fn main(sp: *const usize) -> ! {
    // SAFETY: `sp` is the entry stack pointer, which is this layout.
    let stack = unsafe { auxv::Stack::read(sp) };

    // `AT_BASE` is where the kernel put the loader, and it is zero when there
    // was no loader to place -- which is the case when this *is* the program
    // the kernel was asked to run. glibc's loader makes the same test, and
    // answers it the same way: run directly, it is a tool rather than an
    // interpreter, and it loads the program named on its command line.
    match stack.get(auxv::AT_BASE) {
        // SAFETY: `stack` is this process's entry stack, read once, and the
        // loader is relocated.
        Some(0) | None => unsafe { run_as_a_command(&stack) },
        // SAFETY: as above.
        Some(_) => unsafe { interpret(&stack) },
    }
}

/// The loader as somebody's interpreter: the kernel has already placed the
/// program, and `AT_PHDR` describes it.
///
/// # Safety
///
/// `stack` must describe this process's entry stack.
unsafe fn interpret(stack: &auxv::Stack) -> ! {
    // SAFETY: the stack is this process's.
    match unsafe { link(stack) } {
        // SAFETY: `link` returns the program's entry with every object it
        // needs mapped and relocated, which is what makes entering it sound.
        Ok(entry) => unsafe { start::enter(entry, stack) },
        Err(error) => fail(error),
    }
}

/// Link the program the kernel placed, and answer where to start it.
///
/// # Safety
///
/// `stack` must describe this process's entry stack, whose `AT_PHDR` names a
/// mapped program.
unsafe fn link(stack: &auxv::Stack) -> Result<usize, report::Error> {
    let page_size = stack.get(auxv::AT_PAGESZ).unwrap_or(4096);
    let entry = stack
        .get(auxv::AT_ENTRY)
        .ok_or(report::Error::MalformedObject("no AT_ENTRY on the stack"))?;

    // SAFETY: `AT_PHDR` and `AT_PHNUM` describe the program's own headers,
    // which the kernel mapped with it.
    let (program, interpreter) = unsafe { program_object(stack)? };

    let mut scope = scope::Scope::new();
    if let Some(path) = interpreter {
        scope.set_interpreter(path);
    }
    // `LD_LIBRARY_PATH`, unless the program runs with privileges it was not
    // started with. `AT_SECURE` is the kernel saying so, and a set-user-id
    // program that honoured the variable would load a library of the
    // caller's choosing as its owner -- which is the oldest hole there is.
    let secure = stack.get(auxv::AT_SECURE).unwrap_or(0) != 0;
    if !secure {
        // SAFETY: `envp` is this process's environment.
        if let Some(path) = unsafe { environment(stack, b"LD_LIBRARY_PATH") } {
            scope.set_library_path(path);
        }
    }
    // The program goes in first, and so is searched first: a symbol it
    // defines wins over any library's.
    scope.push(c"".as_ptr(), program)?;
    scope.load_dependencies(page_size)?;
    // Last, so that a program and its libraries are searched before it: what
    // the loader defines is the C library's to use, not to override it.
    scope.push_loader(start::own_object().ok_or(report::Error::TooManyObjects)?)?;
    scope.layout_tls()?;
    // Before any constructor, which may already ask for a TLS variable.
    // SAFETY: the only thread, before the program runs, after the layout.
    unsafe { tls::publish_offsets(&scope) };

    // SAFETY: every object in the scope is mapped.
    unsafe { relocate(&scope, 0, page_size)? };

    tls::install(&scope)?;
    // SAFETY: the only thread, before the program runs.
    unsafe { interface::publish(scope.tls_size(), scope.tls_align(), stack.auxv) };
    // `dlopen` runs its libraries' initialisers with these too.
    let args = (
        stack.argc as c_int,
        stack.argv.cast_mut().cast::<*mut c_char>(),
        stack.envp.cast_mut().cast::<*mut c_char>(),
    );
    // SAFETY: the only thread, before the program runs.
    unsafe { dl::set_arguments(args, page_size) };
    // Saved before any initialiser runs, since one may call `dlsym` or
    // `dlopen`, which find the scope there.
    // SAFETY: all objects are mapped and relocated, and this is the sole path
    // that reaches the program's entry point.
    unsafe { start::save_scope(scope) };
    // SAFETY: every object is relocated and its initial TLS blocks are in
    // place, so an initialiser may call anything. The program's own are its
    // C library's to ask for (`interface`), so they start at 1.
    unsafe { run_initialisers(1, args) };
    Ok(entry)
}

/// The program's own object, read from the headers the kernel described.
///
/// The program's load bias is not on the stack: `AT_PHDR` is where its
/// headers ended up, and `PT_PHDR` is where they were linked to go, so the
/// difference between them is the bias. An `ET_EXEC` has no `PT_PHDR` and no
/// bias, and the zero this leaves is right for it.
///
/// # Safety
///
/// `stack` must carry `AT_PHDR` and `AT_PHNUM` for a mapped program.
unsafe fn program_object(
    stack: &auxv::Stack,
) -> Result<(object::Object, Option<*const c_char>), report::Error> {
    let at = stack
        .get(auxv::AT_PHDR)
        .ok_or(report::Error::MalformedObject("no AT_PHDR on the stack"))?;
    let count = stack
        .get(auxv::AT_PHNUM)
        .ok_or(report::Error::MalformedObject("no AT_PHNUM on the stack"))?;
    let headers = at as *const elf::Phdr;

    let mut bias = 0_usize;
    let mut dynamic = 0_usize;
    let mut relro = (0_usize, 0_usize);
    let mut tls = None;
    let mut interpreter = 0_usize;
    for index in 0..count {
        // SAFETY: `AT_PHNUM` headers are mapped at `AT_PHDR`.
        let header = unsafe { headers.wrapping_add(index).read() };
        match header.p_type {
            elf::PT_PHDR => bias = at.wrapping_sub(header.p_vaddr as usize),
            elf::PT_INTERP => interpreter = header.p_vaddr as usize,
            elf::PT_DYNAMIC => dynamic = header.p_vaddr as usize,
            elf::PT_TLS => tls = Some(header),
            elf::PT_GNU_RELRO => relro = (header.p_vaddr as usize, header.p_memsz as usize),
            _ => {}
        }
    }
    if dynamic == 0 {
        return Err(report::Error::MalformedObject(
            "the program has no PT_DYNAMIC and needs no loader",
        ));
    }
    // SAFETY: the program's `PT_DYNAMIC`, at its run-time address.
    let mut object =
        unsafe { object::Object::read(bias, dynamic.wrapping_add(bias) as *const elf::Dyn) }
            .ok_or(report::Error::TooManyObjects)?;
    if relro.1 != 0 {
        object.relro = (relro.0.wrapping_add(bias), relro.1);
    }
    object.phdr = at;
    object.phnum = count;
    if let Some(header) = tls {
        let tls = object::Tls::new(
            (header.p_vaddr as usize).wrapping_add(bias),
            header.p_filesz as usize,
            header.p_memsz as usize,
            header.p_align as usize,
        )
        .ok_or(report::Error::MalformedObject("an invalid PT_TLS segment"))?;
        object.tls = Some(tls);
    }
    // The kernel read the same bytes to find this loader, so they are a
    // NUL-terminated path in a mapped segment.
    let interpreter = (interpreter != 0).then(|| interpreter.wrapping_add(bias) as *const c_char);
    Ok((object, interpreter))
}

/// Relocate every object from `first` on, from the last loaded to the first.
///
/// A library is relocated before whatever needed it. Nothing here requires
/// that order -- everything is bound at load, so no object calls another
/// before all of them are done -- but an initialiser does, and this is the
/// order the initialisers run in reverse of.
///
/// # Errors
///
/// As [`reloc::apply`].
///
/// # Safety
///
/// Every object in `scope` must be mapped, and those from `first` on not yet
/// relocated.
pub(crate) unsafe fn relocate(
    scope: &scope::Scope,
    first: usize,
    page_size: usize,
) -> Result<(), report::Error> {
    let mut index = scope.len();
    while index > first {
        index -= 1;
        let object = scope
            .get(index)
            .ok_or(report::Error::MalformedObject("an object left the scope"))?;
        if object.is_loader {
            continue;
        }
        // SAFETY: the caller's promise.
        unsafe { reloc::apply(object, scope)? };
        // Its relocations are written, so what they wrote can be made
        // read-only. Done per object as each finishes rather than in a sweep
        // at the end, so that nothing is ever both relocated and writable for
        // longer than it has to be.
        object.protect_relro(page_size);
    }
    Ok(())
}

/// A process's `argc`, `argv` and environment, as initialisers are given them.
pub(crate) type Arguments = (c_int, *mut *mut c_char, *mut *mut c_char);

/// Run every library's initialisers, dependencies first.
///
/// The order is the reverse of the order they were loaded, which is what puts
/// a library's initialiser before that of whatever needed it. A constructor
/// that calls into a library it depends on is the reason this order is not
/// arbitrary.
///
/// The program's own are not run here but by its C library, which is not
/// started yet and which they may use: see [`interface`]. So `first` is 1
/// at start-up.
///
/// # Safety
///
/// Every object in the saved scope must be mapped and fully relocated.
pub(crate) unsafe fn run_initialisers(first: usize, args: Arguments) {
    let count = dl::with_scope(scope::Scope::len);
    // SAFETY: the caller's promise.
    unsafe { run_range(first, count, args) };
}

/// Run the initialisers of the objects in `first..end`, last first. Each is
/// copied out of the scope under [`dl`]'s lock and run without it, so that a
/// constructor may call `dlopen` or `dlsym`.
///
/// # Safety
///
/// As [`run_initialisers`].
pub(crate) unsafe fn run_range(first: usize, end: usize, args: Arguments) {
    let mut index = end;
    while index > first {
        index -= 1;
        let object = dl::with_scope(|scope| scope.get(index).copied());
        let Some(object) = object.filter(|object| !object.is_loader) else {
            continue;
        };
        // SAFETY: the object is mapped and relocated, and the arguments are
        // the process's own.
        unsafe { run_object_init(&object, args.0, args.1, args.2) };
    }
}

/// Run one object's `DT_INIT` and then its `DT_INIT_ARRAY`.
///
/// Each is called with `argc`, `argv` and the environment, as glibc calls
/// them and as a constructor compiled for Linux may read; one that takes
/// nothing is called the same way, since the extra arguments sit in
/// registers it does not read.
///
/// # Safety
///
/// `object` must be mapped and fully relocated, and the arguments the
/// process's own.
pub(crate) unsafe fn run_object_init(
    object: &object::Object,
    argc: c_int,
    argv: *mut *mut c_char,
    envp: *mut *mut c_char,
) {
    type Init = unsafe extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char);
    // Before its constructors run, so that `dl_fini` finishes it even if one
    // of them exits the process.
    dl::record_initialised(object.index);
    if object.init != 0 {
        // SAFETY: `DT_INIT` is a function in the object.
        let init = unsafe { core::mem::transmute::<usize, Init>(object.init) };
        // SAFETY: called once, with the program's arguments, as ELF says.
        unsafe { init(argc, argv, envp) };
    }
    if !object.init_array.is_present() {
        return;
    }
    let mut at = object.init_array.at;
    let end = at.wrapping_add(object.init_array.size);
    while at < end {
        // SAFETY: `DT_INIT_ARRAY` is an array of function pointers, each
        // relocated already because the whole object was.
        let entry = unsafe { (at as *const usize).read() };
        if entry != 0 && entry != usize::MAX {
            // SAFETY: each entry is a constructor.
            let constructor = unsafe { core::mem::transmute::<usize, Init>(entry) };
            // SAFETY: as `DT_INIT` is.
            unsafe { constructor(argc, argv, envp) };
        }
        at = at.wrapping_add(size_of::<usize>());
    }
}

/// The value of an environment variable, or `None`.
///
/// Walked rather than copied into a table, for the reason
/// [`auxv::Stack::get`] gives: the loader reads a couple of variables, once.
///
/// # Safety
///
/// `stack` must describe this process's entry stack.
unsafe fn environment(stack: &auxv::Stack, name: &[u8]) -> Option<*const c_char> {
    let mut at = stack.envp;
    loop {
        // SAFETY: the environment is null-terminated, and this stops there.
        let entry = unsafe { at.read() };
        if entry.is_null() {
            return None;
        }
        let mut index = 0;
        let matched = loop {
            let Some(wanted) = name.get(index) else {
                // The name ran out: this is the variable if what follows is
                // the `=`, and a longer name that starts the same if not.
                // SAFETY: `entry` is NUL-terminated and `index` is inside it.
                break unsafe { entry.wrapping_add(index).read() } as u8 == b'=';
            };
            // SAFETY: as above.
            let byte = unsafe { entry.wrapping_add(index).read() } as u8;
            if byte != *wanted {
                break false;
            }
            index += 1;
        };
        if matched {
            // The `=` is at `index`, so the value follows it.
            return Some(entry.wrapping_add(index + 1));
        }
        // The entry read was not the terminator.
        at = at.wrapping_add(1);
    }
}

/// Report an error the way a loader has to, and stop.
fn fail(error: report::Error) -> ! {
    write_all(2, b"ld-ferrousli: ");
    write_all(2, error.text().as_bytes());
    if let Some(name) = error.subject() {
        write_all(2, b": ");
        write_cstr(2, name);
    }
    if let Some(number) = error.number() {
        write_all(2, b": ");
        write_number(2, number);
    }
    write_all(2, b"\n");
    exit(FAILED)
}

/// Write a NUL-terminated string, without measuring it first.
fn write_cstr(fd: c_int, name: *const c_char) {
    let mut at = name;
    loop {
        // SAFETY: the string is NUL-terminated, and this stops at the NUL.
        let byte = unsafe { at.read() } as u8;
        if byte == 0 {
            return;
        }
        write_all(fd, &[byte]);
        // The byte read was not the terminator.
        at = at.wrapping_add(1);
    }
}

/// Write a number in decimal, which is the only formatting the loader has.
fn write_number(fd: c_int, value: isize) {
    let mut digits = [0_u8; 24];
    let negative = value < 0;
    let mut left = value.unsigned_abs();
    let mut at = digits.len();
    loop {
        at = at.saturating_sub(1);
        if let Some(slot) = digits.get_mut(at) {
            *slot = b'0' + (left % 10) as u8;
        }
        left /= 10;
        if left == 0 || at == 0 {
            break;
        }
    }
    if negative {
        write_all(fd, b"-");
    }
    if let Some(text) = digits.get(at..) {
        write_all(fd, text);
    }
}

/// The loader run as a program of its own, with the program to load named as
/// its first argument.
///
/// # Safety
///
/// `stack` must describe this process's entry stack.
unsafe fn run_as_a_command(stack: &auxv::Stack) -> ! {
    if stack.argc < 2 {
        write_all(2, USAGE.as_bytes());
        exit(FAILED);
    }
    exit(FAILED)
}

/// Writes every byte, or gives up. The loader's only output.
fn write_all(fd: c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: `bytes` is a live slice, and its length is its length.
        let written = unsafe {
            sys::syscall3(
                sys::nr::WRITE,
                fd as usize,
                bytes.as_ptr() as usize,
                bytes.len(),
            )
        };
        let Ok(written) = usize::try_from(written) else {
            return;
        };
        let Some(rest) = bytes.get(written..) else {
            return;
        };
        if written == 0 {
            return;
        }
        bytes = rest;
    }
}

/// Ends the process.
fn exit(status: c_int) -> ! {
    sys::exit_group(status)
}

/// A panic has nowhere to go here: there is no C library beneath and no
/// program above. It traps, as the library's does.
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    sys::trap()
}
