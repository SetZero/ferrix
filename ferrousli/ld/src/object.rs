//! One loaded object — the program, a library, or the loader itself — and the
//! parts of its dynamic table the loader reads.
//!
//! # No allocator
//!
//! There is no `malloc` here: the loader runs before the C library it is
//! loading, and giving it a heap of its own would be a second allocator in
//! every process. So the set of objects is a fixed array, and an object's
//! list of dependencies is a fixed array inside it. The bounds are stated
//! below and a program that exceeds one is refused by name rather than
//! silently linked against part of what it asked for.

use core::ffi::c_char;

use crate::elf::{
    DT_FINI, DT_FINI_ARRAY, DT_FINI_ARRAYSZ, DT_FLAGS, DT_FLAGS_1, DT_GNU_HASH, DT_INIT,
    DT_INIT_ARRAY, DT_INIT_ARRAYSZ, DT_JMPREL, DT_NEEDED, DT_NULL, DT_PLTREL, DT_PLTRELSZ, DT_REL,
    DT_RELA, DT_RELAENT, DT_RELASZ, DT_RELENT, DT_RELSZ, DT_RUNPATH, DT_SONAME, DT_STRSZ,
    DT_STRTAB, DT_SYMENT, DT_SYMTAB, Dyn, Sym,
};

/// How many objects one process may load: the program, the loader, and the
/// libraries of both.
///
/// Chosen to be larger than anything that has been asked of it and small
/// enough that the table is a few kilobytes of `.bss`. A desktop program
/// links against a few dozen libraries; the largest measured here is far
/// under this.
pub const MAX_OBJECTS: usize = 64;

/// How many `DT_NEEDED` entries one object may carry.
pub const MAX_NEEDED: usize = 48;

/// A table the loader reads out of an object's dynamic section.
#[derive(Debug, Clone, Copy)]
pub struct Table {
    /// Where it starts, as a run-time address; zero when the object has none.
    pub at: usize,
    /// Its size in bytes.
    pub size: usize,
    /// The size of one entry, when the table says so.
    pub stride: usize,
}

/// One object's initial TLS image, already at its run-time address.
///
/// `filesz` bytes at `image` are copied into each thread's block and the
/// remaining bytes through `memsz` start zeroed. `align` is never zero and is
/// a power of two, so a layout builder can use it without repeating ELF's
/// validation for every thread it creates.
#[derive(Debug, Clone, Copy)]
pub struct Tls {
    /// The mapped bytes that initialise each thread's block.
    pub image: usize,
    /// Bytes of `image` to copy.
    pub filesz: usize,
    /// Total bytes in every thread's block.
    pub memsz: usize,
    /// Required alignment of the block.
    pub align: usize,
    /// This image's negative offset from the initial thread pointer.
    ///
    /// [`crate::scope::Scope::layout_tls`] assigns it before relocations use
    /// `R_*_TPOFF` references.
    pub offset: isize,
}

impl Tls {
    /// Build a TLS description from one `PT_TLS` segment.
    #[must_use]
    pub const fn new(image: usize, filesz: usize, memsz: usize, align: usize) -> Option<Tls> {
        let align = if align == 0 { 1 } else { align };
        if filesz > memsz || !align.is_power_of_two() {
            return None;
        }
        Some(Tls {
            image,
            filesz,
            memsz,
            align,
            offset: 0,
        })
    }
}

impl Table {
    /// An absent table.
    pub const NONE: Table = Table {
        at: 0,
        size: 0,
        stride: 0,
    };

    /// Whether the object carries it.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.at != 0 && self.size != 0
    }
}

/// A loaded object.
#[derive(Debug, Clone, Copy)]
pub struct Object {
    /// How far it was moved from where it was linked.
    pub base: usize,
    /// Its `PT_DYNAMIC`, as a run-time address.
    pub dynamic: *const Dyn,
    /// Its string table.
    pub strtab: *const c_char,
    /// The string table's size, which bounds every name read from it.
    pub strsz: usize,
    /// Its symbol table.
    pub symtab: *const Sym,
    /// The size of one symbol, which is the class's and is checked.
    pub syment: usize,
    /// Its GNU hash table, or null. An object without one exports nothing
    /// this loader can find: the old `DT_HASH` is not read, because no linker
    /// has emitted it alone this century.
    pub gnu_hash: *const u32,
    /// Its ordinary relocations, `RELA` form.
    pub rela: Table,
    /// Its ordinary relocations, `REL` form.
    pub rel: Table,
    /// Its procedure linkage table's relocations, in whichever form
    /// `DT_PLTREL` names.
    pub jmprel: Table,
    /// Which form that is: `DT_REL` or `DT_RELA`.
    pub pltrel: usize,
    /// `DT_INIT`, the initialiser to run before the array.
    pub init: usize,
    /// `DT_INIT_ARRAY`.
    pub init_array: Table,
    /// `DT_FINI`, run after this object's finaliser array.
    pub fini: usize,
    /// `DT_FINI_ARRAY`.
    pub fini_array: Table,
    /// `PT_TLS`, if this object supplies an initial TLS image.
    pub tls: Option<Tls>,
    /// Its `DT_SONAME`, as a string table offset, or zero.
    pub soname: u32,
    /// Its `DT_RUNPATH`, as a string table offset, or zero.
    pub runpath: u32,
    /// Its `DT_NEEDED` entries, as string table offsets.
    pub needed: [u32; MAX_NEEDED],
    /// How many of them there are.
    pub needed_count: usize,
    /// Whether it asked for everything to be resolved at load. This loader
    /// does that whatever the flag says, so the flag is read only to be
    /// reported.
    pub bind_now: bool,
    /// Its `PT_GNU_RELRO`, as a run-time address and a length, or `(0, 0)`.
    ///
    /// Not read from the dynamic table -- it is a segment, not a tag -- so
    /// whoever mapped the object fills it in.
    pub relro: (usize, usize),
}

impl Object {
    /// An object with nothing in it.
    pub const EMPTY: Object = Object {
        base: 0,
        dynamic: core::ptr::null(),
        strtab: core::ptr::null(),
        strsz: 0,
        symtab: core::ptr::null(),
        syment: 0,
        gnu_hash: core::ptr::null(),
        rela: Table::NONE,
        rel: Table::NONE,
        jmprel: Table::NONE,
        pltrel: 0,
        init: 0,
        init_array: Table::NONE,
        fini: 0,
        fini_array: Table::NONE,
        tls: None,
        soname: 0,
        runpath: 0,
        needed: [0; MAX_NEEDED],
        needed_count: 0,
        bind_now: false,
        relro: (0, 0),
    };

    /// Read `dynamic`'s entries into an object placed at `base`.
    ///
    /// Every address in a dynamic table is a link-time one and is biased as it
    /// is read, so that nothing downstream has to remember to.
    ///
    /// Returns `None` when the object carries more `DT_NEEDED` entries than
    /// [`MAX_NEEDED`], which is the one thing here that cannot be represented
    /// rather than merely absent.
    ///
    /// # Safety
    ///
    /// `dynamic` must be an object's `PT_DYNAMIC`, at its run-time address,
    /// and `base` the bias it was loaded with.
    #[must_use]
    pub unsafe fn read(base: usize, dynamic: *const Dyn) -> Option<Object> {
        // The entry sizes default to this class's, because the tags that give
        // them are not always there to be read. An object whose only
        // relocations are its `JMPREL` table carries no `DT_RELA` and so no
        // `DT_RELAENT` -- there is no `DT_PLTRELAENT` to make up for it --
        // and taking the stride as zero in that case silently skips every
        // relocation the object has. That is what a program whose imported
        // function jumps into its own unrelocated procedure linkage table
        // looks like from the outside, and it is hard to read backwards.
        let mut object = Object {
            base,
            dynamic,
            rela: Table {
                stride: size_of::<crate::elf::Rela>(),
                ..Table::NONE
            },
            rel: Table {
                stride: size_of::<crate::elf::Rel>(),
                ..Table::NONE
            },
            ..Object::EMPTY
        };
        let mut at = dynamic;
        loop {
            // SAFETY: a dynamic table runs to its `DT_NULL`, and this stops
            // there.
            let entry = unsafe { at.read() };
            let value = entry.d_val;
            match entry.d_tag {
                DT_NULL => break,
                DT_NEEDED => {
                    let slot = object.needed.get_mut(object.needed_count)?;
                    *slot = value as u32;
                    object.needed_count += 1;
                }
                DT_STRTAB => object.strtab = value.wrapping_add(base) as *const c_char,
                DT_STRSZ => object.strsz = value,
                DT_SYMTAB => object.symtab = value.wrapping_add(base) as *const Sym,
                DT_SYMENT => object.syment = value,
                DT_GNU_HASH => object.gnu_hash = value.wrapping_add(base) as *const u32,
                DT_RELA => object.rela.at = value.wrapping_add(base),
                DT_RELASZ => object.rela.size = value,
                DT_RELAENT => object.rela.stride = value,
                DT_REL => object.rel.at = value.wrapping_add(base),
                DT_RELSZ => object.rel.size = value,
                DT_RELENT => object.rel.stride = value,
                DT_JMPREL => object.jmprel.at = value.wrapping_add(base),
                DT_PLTRELSZ => object.jmprel.size = value,
                DT_PLTREL => object.pltrel = value,
                DT_INIT => object.init = value.wrapping_add(base),
                DT_INIT_ARRAY => object.init_array.at = value.wrapping_add(base),
                DT_INIT_ARRAYSZ => object.init_array.size = value,
                DT_FINI => object.fini = value.wrapping_add(base),
                DT_FINI_ARRAY => object.fini_array.at = value.wrapping_add(base),
                DT_FINI_ARRAYSZ => object.fini_array.size = value,
                DT_SONAME => object.soname = value as u32,
                DT_RUNPATH => object.runpath = value as u32,
                DT_FLAGS => object.bind_now |= value & crate::elf::DF_BIND_NOW != 0,
                DT_FLAGS_1 => object.bind_now |= value & crate::elf::DF_1_NOW != 0,
                _ => {}
            }
            // SAFETY: the entry read was not the terminator, so another
            // follows.
            at = unsafe { at.add(1) };
        }
        // The `JMPREL` table's entries are the size the form it is says, and
        // `DT_PLTREL` is the only place that says which form that is.
        object.jmprel.stride = if object.pltrel == DT_RELA {
            object.rela.stride
        } else {
            object.rel.stride
        };
        Some(object)
    }

    /// A name from this object's string table, bounded by the table's size.
    ///
    /// `None` for an offset past the end, which is a malformed object rather
    /// than a name the loader should go looking for.
    #[must_use]
    pub fn name(&self, offset: u32) -> Option<*const c_char> {
        if self.strtab.is_null() || offset as usize >= self.strsz {
            return None;
        }
        // SAFETY: the offset is inside the table, whose size the object gave.
        Some(unsafe { self.strtab.add(offset as usize) })
    }

    /// Make this object's `PT_GNU_RELRO` span read-only.
    ///
    /// The span holds what the linker could arrange to be constant *after*
    /// relocation but not before: the global offset table, the tables of
    /// function pointers a constructor list is made of, a `const` pointer to
    /// another object's datum. All of those are written during loading and
    /// none afterwards, and leaving them writable leaves a program with a
    /// page full of function pointers anything can overwrite.
    ///
    /// Called once, after every relocation of this object is applied and
    /// before anything of the program runs. A failure is not reported: the
    /// object works either way, and refusing to start a program because it
    /// could not be made *more* strict would trade a working program for a
    /// hardening measure.
    pub fn protect_relro(&self, page_size: usize) {
        let (at, len) = self.relro;
        if at == 0 || len == 0 {
            return;
        }
        let mask = page_size - 1;
        let start = at & !mask;
        let end = at.wrapping_add(len).wrapping_add(mask) & !mask;
        // SAFETY: the span is inside this object's own mapping, and making it
        // read-only can fail but cannot be unsound.
        let _ = unsafe {
            crate::sys::syscall3(
                crate::sys::nr::MPROTECT,
                start,
                end.wrapping_sub(start),
                1, // PROT_READ
            )
        };
    }

    /// This object's `DT_SONAME`, the name other objects ask for it by.
    #[must_use]
    pub fn soname(&self) -> Option<*const c_char> {
        if self.soname == 0 {
            return None;
        }
        self.name(self.soname)
    }
}
