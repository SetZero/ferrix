//! Applying relocations: writing, into an object's own memory, the addresses
//! that were not known when it was linked.
//!
//! Every relocation says where to write and what to write there. The kinds
//! differ in where the value comes from:
//!
//! * `RELATIVE` — the load bias plus an addend. No symbol, no lookup, and the
//!   overwhelming majority of the relocations in a large program.
//! * `GLOB_DAT`, `JUMP_SLOT`, `ABSOLUTE` — a symbol's address, looked up
//!   across the scope. The first two are one word each; the third adds an
//!   addend.
//! * `IRELATIVE` — the resolver at bias plus addend is *called*, and what it
//!   returns is written. It picks an implementation by what the processor
//!   turned out to have.
//! * `TPOFF` — an initial-exec TLS variable's offset from the thread pointer.
//!   `DTPMOD` and `DTPOFF` — a general-dynamic one's module and offset, for
//!   `__tls_get_addr`; `TLSDESC` — a descriptor, for the same answer without
//!   a call through the PLT. [`crate::tls`] says why all of them are answered
//!   from static TLS.
//!
//! # `REL` and `RELA`
//!
//! x86-64 and AArch64 put the addend in the relocation. ARM puts it in the
//! word being relocated, so it has to be read from there first. Both are
//! handled here rather than in two copies, because everything else about the
//! two is the same.

use crate::arch;
use crate::elf::{Rel, Rela, STT_GNU_IFUNC, r_sym, r_type, st_type};
use crate::object::{Object, Table};
use crate::report::Error;
use crate::scope::Scope;

/// One relocation, in whichever form it arrived.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Where to write, as a link-time address.
    offset: usize,
    /// The symbol index and type.
    info: usize,
    /// The constant part of the value.
    addend: isize,
}

/// A function an `IRELATIVE` relocation calls to choose an implementation.
type Resolver = unsafe extern "C" fn() -> usize;

/// Apply every relocation of `object`, resolving symbols across `scope`.
///
/// The `JMPREL` table is applied with the rest and not lazily: this loader
/// binds everything at load, as `LD_BIND_NOW` asks, so there is no resolver
/// trampoline to write for each architecture and nothing is relocated after
/// the program starts.
///
/// # Errors
///
/// [`Error::UnresolvedSymbol`] for a name no object in the scope defines, and
/// [`Error::UnknownRelocation`] for a type this loader does not write. Both
/// name the object they came from, because "undefined symbol" without one is
/// the least useful message a loader gives.
///
/// # Safety
///
/// `object` must be mapped at its base with its writable segments writable,
/// and every object in `scope` must be mapped.
pub unsafe fn apply(object: &Object, scope: &Scope) -> Result<(), Error> {
    // SAFETY: the caller promises the object is mapped.
    unsafe {
        apply_table(object, scope, &object.rela, true)?;
        apply_table(object, scope, &object.rel, false)?;
        let addend_in_entry = object.pltrel == crate::elf::DT_RELA;
        apply_table(object, scope, &object.jmprel, addend_in_entry)
    }
}

/// Apply one table of relocations.
///
/// # Safety
///
/// As [`apply`].
unsafe fn apply_table(
    object: &Object,
    scope: &Scope,
    table: &Table,
    addend_in_entry: bool,
) -> Result<(), Error> {
    if !table.is_present() || table.stride == 0 {
        return Ok(());
    }
    let mut at = table.at;
    let end = table.at.wrapping_add(table.size);
    while at < end {
        // SAFETY: `at` walks the table the object described, in the steps it
        // gave, and stops at its end.
        let entry = unsafe { read_entry(at, addend_in_entry, object.base) };
        // SAFETY: the object's relocations target its own memory.
        unsafe { apply_one(object, scope, &entry)? };
        at = at.wrapping_add(table.stride);
    }
    Ok(())
}

/// Read one entry, from whichever form the table is in.
///
/// # Safety
///
/// `at` must be a relocation entry of the form `addend_in_entry` says.
unsafe fn read_entry(at: usize, addend_in_entry: bool, base: usize) -> Entry {
    if addend_in_entry {
        // SAFETY: the caller promises a `Rela` is there.
        let entry = unsafe { (at as *const Rela).read() };
        return Entry {
            offset: entry.r_offset,
            info: entry.r_info,
            addend: entry.r_addend,
        };
    }
    // SAFETY: the caller promises a `Rel` is there.
    let entry = unsafe { (at as *const Rel).read() };
    // A `REL` entry's addend is the word it is about to overwrite, so it has
    // to be read before anything is written.
    let target = entry.r_offset.wrapping_add(base) as *const usize;
    // SAFETY: the target is inside the object being relocated.
    let addend = unsafe { target.read() }.cast_signed();
    Entry {
        offset: entry.r_offset,
        info: entry.r_info,
        addend,
    }
}

/// Apply one relocation.
///
/// # Safety
///
/// As [`apply`].
unsafe fn apply_one(object: &Object, scope: &Scope, entry: &Entry) -> Result<(), Error> {
    let kind = r_type(entry.info);
    if kind == arch::R_NONE {
        return Ok(());
    }
    let target = entry.offset.wrapping_add(object.base) as *mut usize;

    let value = match kind {
        arch::R_RELATIVE => object.base.wrapping_add_signed(entry.addend),
        arch::R_IRELATIVE => {
            let at = object.base.wrapping_add_signed(entry.addend);
            // SAFETY: an `IRELATIVE` addend is the address of a resolver in
            // this object, which takes nothing and returns the address to
            // use. Calling it is the whole point of the relocation.
            unsafe { core::mem::transmute::<usize, Resolver>(at)() }
        }
        arch::R_GLOB_DAT | arch::R_JUMP_SLOT | arch::R_ABSOLUTE => {
            // SAFETY: the scope's objects are mapped.
            let found = unsafe { resolve(object, scope, entry)? };
            let Some(found) = found else {
                // A weak undefined symbol resolves to zero, which is what the
                // program tests for when it asks whether something is there.
                unsafe { target.write(0) };
                return Ok(());
            };
            // A symbol that is itself an ifunc is resolved by calling it, the
            // same way `IRELATIVE` is, wherever it was found.
            let address = if st_type(found.info) == STT_GNU_IFUNC {
                // SAFETY: an `STT_GNU_IFUNC` symbol's address is a resolver
                // of the same shape.
                unsafe { core::mem::transmute::<usize, Resolver>(found.address)() }
            } else {
                found.address
            };
            if kind == arch::R_ABSOLUTE {
                address.wrapping_add_signed(entry.addend)
            } else {
                address
            }
        }
        arch::R_TPOFF => {
            // SAFETY: the scope's objects are mapped.
            let place = unsafe { tls_place(object, scope, entry)? };
            let Some((offset, value)) = place else {
                // An undefined weak TLS reference is zero, like every other
                // weak data reference.
                // SAFETY: the target is the object's own word.
                unsafe { target.write(0) };
                return Ok(());
            };
            (offset as usize)
                .wrapping_add(value)
                .wrapping_add_signed(entry.addend)
        }
        // General-dynamic TLS: which module, and where in its block. With no
        // symbol the module is the object's own -- the local-dynamic model,
        // which asks once for its block and adds its own offsets.
        arch::R_DTPMOD => {
            if r_sym(entry.info) == 0 {
                object.index + 1
            } else {
                // SAFETY: the scope's objects are mapped.
                match unsafe { resolve(object, scope, entry)? } {
                    Some(found) => found.module + 1,
                    None => 0,
                }
            }
        }
        arch::R_DTPOFF => {
            let value = if r_sym(entry.info) == 0 {
                0
            } else {
                // SAFETY: the scope's objects are mapped.
                unsafe { resolve(object, scope, entry)? }.map_or(0, |found| found.value)
            };
            value.wrapping_add_signed(entry.addend)
        }
        // A TLS descriptor: two words, a function and its argument. Every
        // module is in static TLS here, so the function is one that returns
        // the argument, the variable's offset from the thread pointer.
        #[cfg(target_arch = "x86_64")]
        arch::R_TLSDESC => {
            // SAFETY: the scope's objects are mapped.
            let place = unsafe { tls_place(object, scope, entry)? };
            let offset = place.map_or(0, |(offset, value)| {
                (offset as usize)
                    .wrapping_add(value)
                    .wrapping_add_signed(entry.addend)
            });
            // SAFETY: a descriptor is two words in the object's own data.
            unsafe { target.wrapping_add(1).write(offset) };
            crate::tls::descriptor_function()
        }
        arch::R_COPY => {
            // SAFETY: the scope's objects are mapped.
            let Some(found) = (unsafe { resolve(object, scope, entry)? }) else {
                return Ok(());
            };
            // The program's symbol says how much room it made; the library's
            // says how much there is. They agree unless the library changed
            // size since the program was linked, and then copying the smaller
            // is what keeps the copy inside both.
            // SAFETY: the relocation's symbol indexes this object's table.
            let symbol = unsafe { object.symtab.add(r_sym(entry.info)).read() };
            let len = (symbol.st_size as usize).min(found.size);
            // SAFETY: the source is the defining object's datum, `len` bytes
            // of it, and the target the program's room for it, both mapped;
            // they are in different objects and cannot overlap.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    found.address as *const u8,
                    target.cast::<u8>(),
                    len,
                );
            }
            return Ok(());
        }
        other => return Err(Error::UnknownRelocation(other)),
    };

    // SAFETY: a relocation's target is a word inside the object it belongs
    // to, which the linker placed in a writable segment.
    unsafe { target.write(value) };
    Ok(())
}

/// Where a TLS relocation's variable is: its module's offset from the thread
/// pointer, and its offset inside that module's block. With no symbol it is
/// the relocating object's own block; `None` is a weak undefined symbol.
///
/// # Safety
///
/// As [`apply`].
unsafe fn tls_place(
    object: &Object,
    scope: &Scope,
    entry: &Entry,
) -> Result<Option<(isize, usize)>, Error> {
    const NO_TLS: Error = Error::MalformedObject("a TLS symbol has no PT_TLS");
    if r_sym(entry.info) == 0 {
        let own = object.tls.ok_or(NO_TLS)?;
        return Ok(Some((own.offset, 0)));
    }
    // SAFETY: the caller's promise.
    let Some(found) = (unsafe { resolve(object, scope, entry)? }) else {
        return Ok(None);
    };
    Ok(Some((found.tls_offset.ok_or(NO_TLS)?, found.value)))
}

/// Find the symbol a relocation names.
///
/// `Ok(None)` for a weak undefined symbol, which is allowed to be missing and
/// resolves to zero.
///
/// # Safety
///
/// As [`apply`].
unsafe fn resolve(
    object: &Object,
    scope: &Scope,
    entry: &Entry,
) -> Result<Option<crate::sym::Found>, Error> {
    let index = r_sym(entry.info);
    if index == 0 {
        return Ok(None);
    }
    // SAFETY: the index came from a relocation of this object, whose symbol
    // table it indexes.
    let symbol = unsafe { object.symtab.add(index).read() };
    let name = object.name(symbol.st_name).ok_or(Error::MalformedObject(
        "a symbol name outside the string table",
    ))?;
    let version = object.requested_version(index);
    // A `COPY` is the one relocation that must not find the program's own
    // symbol, which names the room the copy goes in.
    let first = usize::from(r_type(entry.info) == arch::R_COPY);
    // SAFETY: the scope's objects are mapped.
    if let Some(found) = unsafe { scope.lookup(name, version, first) } {
        return Ok(Some(found));
    }
    if crate::elf::st_bind(symbol.st_info) == crate::elf::STB_WEAK {
        return Ok(None);
    }
    Err(Error::UnresolvedSymbol(name))
}
