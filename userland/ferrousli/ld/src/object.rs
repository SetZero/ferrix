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
    DT_STRTAB, DT_SYMENT, DT_SYMTAB, DT_VERDEF, DT_VERDEFNUM, DT_VERNEED, DT_VERNEEDNUM, DT_VERSYM,
    Dyn, Sym, VER_NDX_GLOBAL, VER_NDX_LOCAL, VERSYM_HIDDEN, Verdaux, Verdef, Vernaux, Verneed,
};

/// How many objects one process may load: the program, the loader, and the
/// libraries of both.
///
/// Chosen to be larger than anything that has been asked of it and small
/// enough that the table is a few kilobytes of `.bss`. A desktop program
/// links against a few dozen libraries; the largest measured here is far
/// under this.
pub(crate) const MAX_OBJECTS: usize = 64;

/// How many `DT_NEEDED` entries one object may carry.
pub(crate) const MAX_NEEDED: usize = 48;

/// A table the loader reads out of an object's dynamic section.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Table {
    /// Where it starts, as a run-time address; zero when the object has none.
    pub(crate) at: usize,
    /// Its size in bytes.
    pub(crate) size: usize,
    /// The size of one entry, when the table says so.
    pub(crate) stride: usize,
}

/// One object's initial TLS image, already at its run-time address.
///
/// `filesz` bytes at `image` are copied into each thread's block and the
/// remaining bytes through `memsz` start zeroed. `align` is never zero and is
/// a power of two, so a layout builder can use it without repeating ELF's
/// validation for every thread it creates.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tls {
    /// The mapped bytes that initialise each thread's block.
    pub(crate) image: usize,
    /// Bytes of `image` to copy.
    pub(crate) filesz: usize,
    /// Total bytes in every thread's block.
    pub(crate) memsz: usize,
    /// Required alignment of the block.
    pub(crate) align: usize,
    /// This image's offset from the thread pointer: negative on x86-64,
    /// positive past the control block on AArch64 and ARMv7-A.
    ///
    /// [`crate::scope::Scope::layout_tls`] assigns it before relocations use
    /// `R_*_TPOFF` references.
    pub(crate) offset: isize,
}

impl Tls {
    /// Build a TLS description from one `PT_TLS` segment.
    #[must_use]
    pub(crate) const fn new(
        image: usize,
        filesz: usize,
        memsz: usize,
        align: usize,
    ) -> Option<Tls> {
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
    pub(crate) const NONE: Table = Table {
        at: 0,
        size: 0,
        stride: 0,
    };

    /// Whether the object carries it.
    #[must_use]
    pub(crate) const fn is_present(&self) -> bool {
        self.at != 0 && self.size != 0
    }
}

/// A loaded object.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Object {
    /// How far it was moved from where it was linked.
    pub(crate) base: usize,
    /// Its string table.
    pub(crate) strtab: *const c_char,
    /// The string table's size, which bounds every name read from it.
    pub(crate) strsz: usize,
    /// Its symbol table.
    pub(crate) symtab: *const Sym,
    /// The size of one symbol, which is the class's and is checked.
    pub(crate) syment: usize,
    /// Its GNU hash table, or null. An object without one exports nothing
    /// this loader can find: the old `DT_HASH` is not read, because no linker
    /// has emitted it alone this century.
    pub(crate) gnu_hash: *const u32,
    /// Its ordinary relocations, `RELA` form.
    pub(crate) rela: Table,
    /// Its ordinary relocations, `REL` form.
    pub(crate) rel: Table,
    /// Its procedure linkage table's relocations, in whichever form
    /// `DT_PLTREL` names.
    pub(crate) jmprel: Table,
    /// Which form that is: `DT_REL` or `DT_RELA`.
    pub(crate) pltrel: usize,
    /// `DT_INIT`, the initialiser to run before the array.
    pub(crate) init: usize,
    /// `DT_INIT_ARRAY`.
    pub(crate) init_array: Table,
    /// `DT_FINI`, run after this object's finaliser array.
    pub(crate) fini: usize,
    /// `DT_FINI_ARRAY`.
    pub(crate) fini_array: Table,
    /// `PT_TLS`, if this object supplies an initial TLS image.
    pub(crate) tls: Option<Tls>,
    /// Its `DT_SONAME`, as a string table offset, or zero.
    pub(crate) soname: u32,
    /// Its `DT_RUNPATH`, as a string table offset, or zero.
    pub(crate) runpath: u32,
    /// Its `DT_NEEDED` entries, as string table offsets.
    pub(crate) needed: [u32; MAX_NEEDED],
    /// How many of them there are.
    pub(crate) needed_count: usize,
    /// Whether it asked for everything to be resolved at load. This loader
    /// does that whatever the flag says, so the flag is read only to be
    /// reported.
    pub(crate) bind_now: bool,
    /// Its `PT_GNU_RELRO`, as a run-time address and a length, or `(0, 0)`.
    ///
    /// Not read from the dynamic table -- it is a segment, not a tag -- so
    /// whoever mapped the object fills it in.
    pub(crate) relro: (usize, usize),
    /// `DT_VERSYM`: one version index per dynamic symbol, or null.
    pub(crate) versym: *const u16,
    /// `DT_VERDEF` as a run-time address, and how many records it has.
    pub(crate) verdef: (usize, usize),
    /// `DT_VERNEED` as a run-time address, and how many records it has.
    pub(crate) verneed: (usize, usize),
    /// Whether this is the loader itself: relocated already by its own
    /// start, and in the scope only so that its names are recognised and its
    /// symbols found.
    pub(crate) is_loader: bool,
    /// Where it is in the scope: its TLS module number less one. Set when it
    /// is added.
    pub(crate) index: usize,
    /// Its program headers, at their run-time address, and how many: what
    /// `dl_iterate_phdr` hands an unwinder and `dladdr` finds an address's
    /// object by. Not a dynamic tag, so whoever mapped it fills them in; zero
    /// when nobody could.
    pub(crate) phdr: usize,
    /// How many headers are at [`Self::phdr`].
    pub(crate) phnum: usize,
}

/// What a version definition is called.
#[derive(Debug, Clone, Copy)]
enum VerdefName {
    /// Its name.
    Named(*const c_char),
    /// Its name is outside the string table.
    Unnamed,
}

/// The version a definition carries, from `DT_VERSYM` and `DT_VERDEF`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Defined {
    /// Local to its object: never an answer to another object's reference.
    Local,
    /// No version at all, which satisfies any reference.
    Unversioned,
    /// A named version. `hidden` is `name@VERSION` rather than
    /// `name@@VERSION`: an old definition kept for binaries linked against it,
    /// which only a reference naming that version may reach.
    Named {
        /// The version's name.
        name: *const c_char,
        /// Whether only a reference naming it may reach it.
        hidden: bool,
    },
}

impl Object {
    /// An object with nothing in it.
    pub(crate) const EMPTY: Object = Object {
        base: 0,
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
        versym: core::ptr::null(),
        verdef: (0, 0),
        verneed: (0, 0),
        is_loader: false,
        index: 0,
        phdr: 0,
        phnum: 0,
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
    pub(crate) unsafe fn read(base: usize, dynamic: *const Dyn) -> Option<Object> {
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
                DT_VERSYM => object.versym = value.wrapping_add(base) as *const u16,
                DT_VERDEF => object.verdef.0 = value.wrapping_add(base),
                DT_VERDEFNUM => object.verdef.1 = value,
                DT_VERNEED => object.verneed.0 = value.wrapping_add(base),
                DT_VERNEEDNUM => object.verneed.1 = value,
                _ => {}
            }
            // The entry read was not the terminator, so another
            // follows.
            at = at.wrapping_add(1);
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
    pub(crate) fn name(&self, offset: u32) -> Option<*const c_char> {
        if self.strtab.is_null() || offset as usize >= self.strsz {
            return None;
        }
        // The offset is inside the table, whose size the object gave.
        Some(self.strtab.wrapping_add(offset as usize))
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
    pub(crate) fn protect_relro(&self, page_size: usize) {
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
    pub(crate) fn soname(&self) -> Option<*const c_char> {
        if self.soname == 0 {
            return None;
        }
        self.name(self.soname)
    }

    /// The version index `DT_VERSYM` gives symbol `index`, or `None` when
    /// this object carries no version table.
    fn version_index(&self, index: usize) -> Option<u16> {
        if self.versym.is_null() {
            return None;
        }
        // SAFETY: `DT_VERSYM` has one entry per dynamic symbol, and `index`
        // is one this object's own hash table or relocation gave.
        Some(unsafe { self.versym.wrapping_add(index).read() })
    }

    /// The version this object's reference to symbol `index` asks for:
    /// `printf@GLIBC_2.2.5`'s `GLIBC_2.2.5`. `None` for a reference that
    /// names no version, which any definition that is not hidden answers.
    #[must_use]
    pub(crate) fn requested_version(&self, index: usize) -> Option<*const c_char> {
        let wanted = self.version_index(index)? & !VERSYM_HIDDEN;
        if wanted == VER_NDX_LOCAL || wanted == VER_NDX_GLOBAL {
            return None;
        }
        let (mut at, mut left) = self.verneed;
        while at != 0 && left > 0 {
            // SAFETY: `DT_VERNEED` is a chain of `Verneed` records, each
            // followed by its `Vernaux` records, linked by offsets, and this
            // stops after the count `DT_VERNEEDNUM` gave.
            let need = unsafe { (at as *const Verneed).read_unaligned() };
            let mut aux_at = at.wrapping_add(need.vn_aux as usize);
            for _ in 0..need.vn_cnt {
                // SAFETY: as above, for the `Vernaux` records of one file.
                let aux = unsafe { (aux_at as *const Vernaux).read_unaligned() };
                if aux.vna_other == wanted {
                    return self.name(aux.vna_name);
                }
                if aux.vna_next == 0 {
                    break;
                }
                aux_at = aux_at.wrapping_add(aux.vna_next as usize);
            }
            if need.vn_next == 0 {
                break;
            }
            at = at.wrapping_add(need.vn_next as usize);
            left -= 1;
        }
        // A reference to a symbol this object defines itself -- libgcc_s's
        // constructor to its own `__cpu_indicator_init@GCC_4.8.0` -- carries
        // the index of one of its own definitions instead.
        match self.verdef_name(wanted)? {
            VerdefName::Named(name) => Some(name),
            VerdefName::Unnamed => None,
        }
    }

    /// The name of this object's version definition `number`, or `None`
    /// when it has none of that number.
    fn verdef_name(&self, number: u16) -> Option<VerdefName> {
        let (mut at, mut left) = self.verdef;
        while at != 0 && left > 0 {
            // SAFETY: `DT_VERDEF` is a chain of `Verdef` records, each with
            // its `Verdaux` names, linked by offsets, and this stops after
            // the count `DT_VERDEFNUM` gave.
            let def = unsafe { (at as *const Verdef).read_unaligned() };
            if def.vd_ndx == number {
                // SAFETY: the first `Verdaux` of a definition is its name.
                let aux = unsafe {
                    (at.wrapping_add(def.vd_aux as usize) as *const Verdaux).read_unaligned()
                };
                return Some(match self.name(aux.vda_name) {
                    Some(name) => VerdefName::Named(name),
                    None => VerdefName::Unnamed,
                });
            }
            if def.vd_next == 0 {
                break;
            }
            at = at.wrapping_add(def.vd_next as usize);
            left -= 1;
        }
        None
    }

    /// The version this object's definition of symbol `index` carries.
    #[must_use]
    pub(crate) fn defined_version(&self, index: usize) -> Defined {
        let Some(raw) = self.version_index(index) else {
            return Defined::Unversioned;
        };
        let hidden = raw & VERSYM_HIDDEN != 0;
        let number = raw & !VERSYM_HIDDEN;
        if number == VER_NDX_LOCAL {
            return Defined::Local;
        }
        if number == VER_NDX_GLOBAL {
            return Defined::Unversioned;
        }
        match self.verdef_name(number) {
            Some(VerdefName::Named(name)) => Defined::Named { name, hidden },
            // A version index with no definition behind it, or one with no
            // name, is a malformed object; treating the symbol as local keeps
            // it from answering anything, which is the safe way to be wrong.
            Some(VerdefName::Unnamed) | None => Defined::Local,
        }
    }
}
