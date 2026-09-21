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
pub struct Phdr {
    /// `PT_*`.
    pub p_type: u32,
    /// `PF_*`.
    pub p_flags: u32,
    /// Where its contents are in the file.
    pub p_offset: u64,
    /// Where they are linked to go.
    pub p_vaddr: u64,
    /// The physical address, which no program reads.
    pub p_paddr: u64,
    /// Bytes of it in the file.
    pub p_filesz: u64,
    /// Bytes it occupies in memory; the excess is zero.
    pub p_memsz: u64,
    /// The alignment it asks for.
    pub p_align: u64,
}

/// `Elf32_Phdr`, whose fields are in a different order as well as narrower.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[cfg(target_pointer_width = "32")]
pub struct Phdr {
    /// `PT_*`.
    pub p_type: u32,
    /// Where its contents are in the file.
    pub p_offset: u32,
    /// Where they are linked to go.
    pub p_vaddr: u32,
    /// The physical address, which no program reads.
    pub p_paddr: u32,
    /// Bytes of it in the file.
    pub p_filesz: u32,
    /// Bytes it occupies in memory; the excess is zero.
    pub p_memsz: u32,
    /// `PF_*`.
    pub p_flags: u32,
    /// The alignment it asks for.
    pub p_align: u32,
}

/// One entry of `PT_DYNAMIC`: a tag and a value whose meaning the tag gives.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Dyn {
    /// `DT_*`.
    pub d_tag: usize,
    /// An address, a size or a count, by tag.
    pub d_val: usize,
}

/// `Elf_Rela`: a relocation carrying its addend.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Rela {
    /// Where to write, as a link-time address.
    pub r_offset: usize,
    /// The symbol index and the type, packed.
    pub r_info: usize,
    /// The constant part of the value.
    pub r_addend: isize,
}

/// `Elf_Rel`: a relocation whose addend is already at `r_offset`. The 32-bit
/// architectures use this form for everything but `Elf32_Rela` sections.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Rel {
    /// Where to write, as a link-time address.
    pub r_offset: usize,
    /// The symbol index and the type, packed.
    pub r_info: usize,
}

/// `Elf64_Sym`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[cfg(target_pointer_width = "64")]
pub struct Sym {
    /// Its name, as an offset into the string table.
    pub st_name: u32,
    /// Binding and type, packed.
    pub st_info: u8,
    /// Visibility.
    pub st_other: u8,
    /// The section it is defined in; `SHN_UNDEF` when it is imported.
    pub st_shndx: u16,
    /// Its link-time address.
    pub st_value: u64,
    /// Its size in bytes.
    pub st_size: u64,
}

/// `Elf32_Sym`, whose fields are in a different order.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[cfg(target_pointer_width = "32")]
pub struct Sym {
    /// Its name, as an offset into the string table.
    pub st_name: u32,
    /// Its link-time address.
    pub st_value: u32,
    /// Its size in bytes.
    pub st_size: u32,
    /// Binding and type, packed.
    pub st_info: u8,
    /// Visibility.
    pub st_other: u8,
    /// The section it is defined in; `SHN_UNDEF` when it is imported.
    pub st_shndx: u16,
}

/// A loadable segment.
pub const PT_LOAD: u32 = 1;
/// The dynamic table.
pub const PT_DYNAMIC: u32 = 2;
/// The segment holding the program headers.
pub const PT_PHDR: u32 = 6;
/// Thread-local storage: the initial image of every thread's block.
pub const PT_TLS: u32 = 7;
/// The span made read-only once relocation is done.
pub const PT_GNU_RELRO: u32 = 0x6474_e552;

/// Readable.
pub const PF_R: u32 = 4;
/// Writable.
pub const PF_W: u32 = 2;
/// Executable.
pub const PF_X: u32 = 1;

/// The end of the dynamic table.
pub const DT_NULL: usize = 0;
/// A library this object needs, as a string table offset.
pub const DT_NEEDED: usize = 1;
/// The size of the `JMPREL` table in bytes.
pub const DT_PLTRELSZ: usize = 2;
/// The string table.
pub const DT_STRTAB: usize = 5;
/// The symbol table.
pub const DT_SYMTAB: usize = 6;
/// The `RELA` relocations.
pub const DT_RELA: usize = 7;
/// Their size in bytes.
pub const DT_RELASZ: usize = 8;
/// The size of one of them.
pub const DT_RELAENT: usize = 9;
/// The string table's size, which bounds every name read from it.
pub const DT_STRSZ: usize = 10;
/// The size of one symbol table entry.
pub const DT_SYMENT: usize = 11;
/// The initialiser to run first.
pub const DT_INIT: usize = 12;
/// The legacy finaliser, run after the finaliser array.
pub const DT_FINI: usize = 13;
/// This object's own name.
pub const DT_SONAME: usize = 14;
/// The `REL` relocations.
pub const DT_REL: usize = 17;
/// Their size in bytes.
pub const DT_RELSZ: usize = 18;
/// The size of one of them.
pub const DT_RELENT: usize = 19;
/// Which of `REL` and `RELA` the `JMPREL` table is.
pub const DT_PLTREL: usize = 20;
/// The relocations for the procedure linkage table.
pub const DT_JMPREL: usize = 23;
/// The array of initialisers.
pub const DT_INIT_ARRAY: usize = 25;
/// The array of finalisers.
pub const DT_FINI_ARRAY: usize = 26;
/// The size of `DT_INIT_ARRAY` in bytes.
pub const DT_INIT_ARRAYSZ: usize = 27;
/// The size of `DT_FINI_ARRAY` in bytes.
pub const DT_FINI_ARRAYSZ: usize = 28;
/// The search path.
pub const DT_RUNPATH: usize = 29;
/// Flags; `DF_*`.
pub const DT_FLAGS: usize = 30;
/// The GNU hash table.
pub const DT_GNU_HASH: usize = 0x6fff_fef5;
/// More flags; `DF_1_*`.
pub const DT_FLAGS_1: usize = 0x6fff_fffb;

/// `DT_FLAGS`: resolve everything at load.
pub const DF_BIND_NOW: usize = 0x08;
/// `DT_FLAGS_1`: the same, as the linker more often writes it.
pub const DF_1_NOW: usize = 0x01;

/// A symbol that is not defined here.
pub const SHN_UNDEF: u16 = 0;

/// A symbol that is only a name for an address, and binds like any other.
pub const STB_GLOBAL: u8 = 1;
/// A symbol another definition overrides.
pub const STB_WEAK: u8 = 2;
/// A symbol resolved by calling a function in the defining object.
pub const STT_GNU_IFUNC: u8 = 10;

/// The binding half of `st_info`.
#[must_use]
pub const fn st_bind(info: u8) -> u8 {
    info >> 4
}

/// The type half of `st_info`.
#[must_use]
pub const fn st_type(info: u8) -> u8 {
    info & 0xF
}

/// The symbol index half of `r_info`.
#[must_use]
#[cfg(target_pointer_width = "64")]
pub const fn r_sym(info: usize) -> usize {
    info >> 32
}

/// The symbol index half of `r_info`, which is a byte narrower here.
#[must_use]
#[cfg(target_pointer_width = "32")]
pub const fn r_sym(info: usize) -> usize {
    info >> 8
}

/// The relocation type half of `r_info`.
#[must_use]
#[cfg(target_pointer_width = "64")]
pub const fn r_type(info: usize) -> u32 {
    (info & 0xFFFF_FFFF) as u32
}

/// The relocation type half of `r_info`.
#[must_use]
#[cfg(target_pointer_width = "32")]
pub const fn r_type(info: usize) -> u32 {
    (info & 0xFF) as u32
}
