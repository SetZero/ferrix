//! The ELF structures the loader reads, for this build's class alone.
//!
//! A 64-bit loader reads 64-bit objects and a 32-bit one reads 32-bit
//! objects: a dynamic linker is loaded into the process it links, so the
//! class is never in question and there is no need for the two-class reader
//! `libs/elf` is. The types are named after the C ones so that a reader with
//! the ELF specification open finds them.

/// `Elf_Phdr`, in the layout this build's class gives it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[cfg(target_pointer_width = "64")]
pub(crate) struct Phdr {
    /// `PT_*`.
    pub(crate) p_type: u32,
    /// `PF_*`.
    pub(crate) p_flags: u32,
    /// Where its contents are in the file.
    pub(crate) p_offset: u64,
    /// Where they are linked to go.
    pub(crate) p_vaddr: u64,
    /// The physical address, which no program reads.
    pub(crate) p_paddr: u64,
    /// Bytes of it in the file.
    pub(crate) p_filesz: u64,
    /// Bytes it occupies in memory; the excess is zero.
    pub(crate) p_memsz: u64,
    /// The alignment it asks for.
    pub(crate) p_align: u64,
}

/// `Elf32_Phdr`, whose fields are in a different order as well as narrower.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[cfg(target_pointer_width = "32")]
pub(crate) struct Phdr {
    /// `PT_*`.
    pub(crate) p_type: u32,
    /// Where its contents are in the file.
    pub(crate) p_offset: u32,
    /// Where they are linked to go.
    pub(crate) p_vaddr: u32,
    /// The physical address, which no program reads.
    pub(crate) p_paddr: u32,
    /// Bytes of it in the file.
    pub(crate) p_filesz: u32,
    /// Bytes it occupies in memory; the excess is zero.
    pub(crate) p_memsz: u32,
    /// `PF_*`.
    pub(crate) p_flags: u32,
    /// The alignment it asks for.
    pub(crate) p_align: u32,
}

/// One entry of `PT_DYNAMIC`: a tag and a value whose meaning the tag gives.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Dyn {
    /// `DT_*`.
    pub(crate) d_tag: usize,
    /// An address, a size or a count, by tag.
    pub(crate) d_val: usize,
}

/// `Elf_Rela`: a relocation carrying its addend.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rela {
    /// Where to write, as a link-time address.
    pub(crate) r_offset: usize,
    /// The symbol index and the type, packed.
    pub(crate) r_info: usize,
    /// The constant part of the value.
    pub(crate) r_addend: isize,
}

/// `Elf_Rel`: a relocation whose addend is already at `r_offset`. The 32-bit
/// architectures use this form for everything but `Elf32_Rela` sections.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rel {
    /// Where to write, as a link-time address.
    pub(crate) r_offset: usize,
    /// The symbol index and the type, packed.
    pub(crate) r_info: usize,
}

/// `Elf64_Sym`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[cfg(target_pointer_width = "64")]
pub(crate) struct Sym {
    /// Its name, as an offset into the string table.
    pub(crate) st_name: u32,
    /// Binding and type, packed.
    pub(crate) st_info: u8,
    /// Visibility.
    pub(crate) st_other: u8,
    /// The section it is defined in; `SHN_UNDEF` when it is imported.
    pub(crate) st_shndx: u16,
    /// Its link-time address.
    pub(crate) st_value: u64,
    /// Its size in bytes.
    pub(crate) st_size: u64,
}

/// `Elf32_Sym`, whose fields are in a different order.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[cfg(target_pointer_width = "32")]
pub(crate) struct Sym {
    /// Its name, as an offset into the string table.
    pub(crate) st_name: u32,
    /// Its link-time address.
    pub(crate) st_value: u32,
    /// Its size in bytes.
    pub(crate) st_size: u32,
    /// Binding and type, packed.
    pub(crate) st_info: u8,
    /// Visibility.
    pub(crate) st_other: u8,
    /// The section it is defined in; `SHN_UNDEF` when it is imported.
    pub(crate) st_shndx: u16,
}

/// A loadable segment.
pub(crate) const PT_LOAD: u32 = 1;
/// The dynamic table.
pub(crate) const PT_DYNAMIC: u32 = 2;
/// The path of the interpreter the program asked for: this loader's name.
pub(crate) const PT_INTERP: u32 = 3;
/// The segment holding the program headers.
pub(crate) const PT_PHDR: u32 = 6;
/// Thread-local storage: the initial image of every thread's block.
pub(crate) const PT_TLS: u32 = 7;
/// The span made read-only once relocation is done.
pub(crate) const PT_GNU_RELRO: u32 = 0x6474_e552;

/// Readable.
pub(crate) const PF_R: u32 = 4;
/// Writable.
pub(crate) const PF_W: u32 = 2;
/// Executable.
pub(crate) const PF_X: u32 = 1;

/// The end of the dynamic table.
pub(crate) const DT_NULL: usize = 0;
/// A library this object needs, as a string table offset.
pub(crate) const DT_NEEDED: usize = 1;
/// The size of the `JMPREL` table in bytes.
pub(crate) const DT_PLTRELSZ: usize = 2;
/// The string table.
pub(crate) const DT_STRTAB: usize = 5;
/// The symbol table.
pub(crate) const DT_SYMTAB: usize = 6;
/// The `RELA` relocations.
pub(crate) const DT_RELA: usize = 7;
/// Their size in bytes.
pub(crate) const DT_RELASZ: usize = 8;
/// The size of one of them.
pub(crate) const DT_RELAENT: usize = 9;
/// The string table's size, which bounds every name read from it.
pub(crate) const DT_STRSZ: usize = 10;
/// The size of one symbol table entry.
pub(crate) const DT_SYMENT: usize = 11;
/// The initialiser to run first.
pub(crate) const DT_INIT: usize = 12;
/// The legacy finaliser, run after the finaliser array.
pub(crate) const DT_FINI: usize = 13;
/// This object's own name.
pub(crate) const DT_SONAME: usize = 14;
/// The `REL` relocations.
pub(crate) const DT_REL: usize = 17;
/// Their size in bytes.
pub(crate) const DT_RELSZ: usize = 18;
/// The size of one of them.
pub(crate) const DT_RELENT: usize = 19;
/// Which of `REL` and `RELA` the `JMPREL` table is.
pub(crate) const DT_PLTREL: usize = 20;
/// The relocations for the procedure linkage table.
pub(crate) const DT_JMPREL: usize = 23;
/// The array of initialisers.
pub(crate) const DT_INIT_ARRAY: usize = 25;
/// The array of finalisers.
pub(crate) const DT_FINI_ARRAY: usize = 26;
/// The size of `DT_INIT_ARRAY` in bytes.
pub(crate) const DT_INIT_ARRAYSZ: usize = 27;
/// The size of `DT_FINI_ARRAY` in bytes.
pub(crate) const DT_FINI_ARRAYSZ: usize = 28;
/// The search path.
pub(crate) const DT_RUNPATH: usize = 29;
/// Flags; `DF_*`.
pub(crate) const DT_FLAGS: usize = 30;
/// The GNU hash table.
pub(crate) const DT_GNU_HASH: usize = 0x6fff_fef5;
/// More flags; `DF_1_*`.
pub(crate) const DT_FLAGS_1: usize = 0x6fff_fffb;
/// The symbol version table: one half-word per dynamic symbol.
pub(crate) const DT_VERSYM: usize = 0x6fff_fff0;
/// The versions this object defines.
pub(crate) const DT_VERDEF: usize = 0x6fff_fffc;
/// How many entries `DT_VERDEF` has.
pub(crate) const DT_VERDEFNUM: usize = 0x6fff_fffd;
/// The versions this object needs from others.
pub(crate) const DT_VERNEED: usize = 0x6fff_fffe;
/// How many entries `DT_VERNEED` has.
pub(crate) const DT_VERNEEDNUM: usize = 0x6fff_ffff;

/// `DT_VERSYM`: a symbol local to its object.
pub(crate) const VER_NDX_LOCAL: u16 = 0;
/// `DT_VERSYM`: a symbol with no version, visible to everyone.
pub(crate) const VER_NDX_GLOBAL: u16 = 1;
/// `DT_VERSYM`: the bit that hides a definition from references that do not
/// name its version -- `name@VERSION` rather than `name@@VERSION`.
pub(crate) const VERSYM_HIDDEN: u16 = 0x8000;

/// One version an object defines. The same in both classes: every field is
/// a fixed width, and the offsets are relative to the record.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Verdef {
    /// The structure's revision, 1.
    pub(crate) vd_version: u16,
    /// `VER_FLG_*`.
    pub(crate) vd_flags: u16,
    /// The index `DT_VERSYM` uses for this version.
    pub(crate) vd_ndx: u16,
    /// How many [`Verdaux`] records follow.
    pub(crate) vd_cnt: u16,
    /// The ELF hash of its name.
    pub(crate) vd_hash: u32,
    /// Where its first `Verdaux`, the one holding its name, is.
    pub(crate) vd_aux: u32,
    /// Where the next definition is, or zero.
    pub(crate) vd_next: u32,
}

/// A name belonging to a [`Verdef`]; the first is the version's own.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Verdaux {
    /// The name, as an offset into the string table.
    pub(crate) vda_name: u32,
    /// Where the next is, relative to this one, or zero.
    pub(crate) vda_next: u32,
}

/// One file an object needs versions from.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Verneed {
    /// The structure's revision, 1.
    pub(crate) vn_version: u16,
    /// How many [`Vernaux`] records follow.
    pub(crate) vn_cnt: u16,
    /// The file's name, as an offset into the string table.
    pub(crate) vn_file: u32,
    /// Where its first [`Vernaux`] is.
    pub(crate) vn_aux: u32,
    /// Where the next file is, or zero.
    pub(crate) vn_next: u32,
}

/// One version needed from a [`Verneed`]'s file.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Vernaux {
    /// The ELF hash of its name.
    pub(crate) vna_hash: u32,
    /// `VER_FLG_*`.
    pub(crate) vna_flags: u16,
    /// The index `DT_VERSYM` uses for it in the needing object.
    pub(crate) vna_other: u16,
    /// The name, as an offset into the string table.
    pub(crate) vna_name: u32,
    /// Where the next is, relative to this one, or zero.
    pub(crate) vna_next: u32,
}

/// `DT_FLAGS`: resolve everything at load.
pub(crate) const DF_BIND_NOW: usize = 0x08;
/// `DT_FLAGS_1`: the same, as the linker more often writes it.
pub(crate) const DF_1_NOW: usize = 0x01;

/// A symbol that is not defined here.
pub(crate) const SHN_UNDEF: u16 = 0;

/// A symbol that is only a name for an address, and binds like any other.
pub(crate) const STB_GLOBAL: u8 = 1;
/// A symbol another definition overrides.
pub(crate) const STB_WEAK: u8 = 2;
/// A symbol resolved by calling a function in the defining object.
pub(crate) const STT_GNU_IFUNC: u8 = 10;

/// The binding half of `st_info`.
#[must_use]
pub(crate) const fn st_bind(info: u8) -> u8 {
    info >> 4
}

/// The type half of `st_info`.
#[must_use]
pub(crate) const fn st_type(info: u8) -> u8 {
    info & 0xF
}

/// The symbol index half of `r_info`.
#[must_use]
#[cfg(target_pointer_width = "64")]
pub(crate) const fn r_sym(info: usize) -> usize {
    info >> 32
}

/// The symbol index half of `r_info`, which is a byte narrower here.
#[must_use]
#[cfg(target_pointer_width = "32")]
pub(crate) const fn r_sym(info: usize) -> usize {
    info >> 8
}

/// The relocation type half of `r_info`.
#[must_use]
#[cfg(target_pointer_width = "64")]
pub(crate) const fn r_type(info: usize) -> u32 {
    (info & 0xFFFF_FFFF) as u32
}

/// The relocation type half of `r_info`.
#[must_use]
#[cfg(target_pointer_width = "32")]
pub(crate) const fn r_type(info: usize) -> u32 {
    (info & 0xFF) as u32
}
