//! The symbol table, for turning an address back into a name.
//!
//! Neither the loader nor the kernel needs this. It is for `xtask`, which
//! resolves the return addresses in a kernel panic's backtrace against the
//! kernel image it booted. It lives here rather than in `xtask` because it is
//! the same bounds-checked reading of the same file format.
//!
//! Only `SHT_SYMTAB` is read. A stripped image has none, and
//! [`Elf::symbols`] says so with `None` rather than an error: nothing is wrong
//! with the image, there is only nothing to look up. The same goes for a
//! malformed table, because the caller's response is the same either way —
//! print the address without a name.

use crate::{Class, EM_ARM, Elf, u16_at, u32_at, u64_at, word_at};

/// Section type of a full symbol table.
pub const SHT_SYMTAB: u32 = 2;
/// Section type of a string table.
pub const SHT_STRTAB: u32 = 3;
/// Symbol type of a symbol whose type is not given: assembly labels, and Arm
/// mapping symbols.
pub const STT_NOTYPE: u8 = 0;
/// Symbol type of a data object.
pub const STT_OBJECT: u8 = 1;
/// Symbol type of a function.
pub const STT_FUNC: u8 = 2;

/// How far past an unsized label an address may be and still be attributed to
/// it.
///
/// A label with no size — an exception vector, a trampoline — has to stand for
/// the code after it, but only for as long as a function plausibly runs.
/// Without a bound the nearest label below swallows every wild address in the
/// image, and a backtrace that has gone off the rails reads as though it
/// landed somewhere real.
pub const MAX_LABEL_SPAN: u64 = 64 * 1024;

/// Size of one ELF64 symbol table entry.
pub const SYM_SIZE: usize = 24;
/// Size of one ELF32 symbol table entry.
pub const SYM32_SIZE: usize = 16;
/// Size of one ELF64 section header.
pub const SHDR_SIZE: usize = 64;
/// Size of one ELF32 section header.
pub const SHDR32_SIZE: usize = 40;

/// One defined entry of the symbol table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Symbol<'a> {
    /// The name's bytes as the string table holds them: mangled, and not
    /// necessarily UTF-8.
    pub name: &'a [u8],
    /// The symbol's value: for a function in an executable, its address. On
    /// 32-bit Arm the Thumb bit is already cleared.
    pub value: u64,
    /// Its size in bytes, or zero if whoever wrote it did not say.
    pub size: u64,
    /// The low four bits of `st_info`: [`STT_FUNC`], [`STT_OBJECT`], and so on.
    pub kind: u8,
}

impl Symbol<'_> {
    /// Whether `address` is inside this symbol: `value <= address < value + size`.
    #[must_use]
    pub const fn contains(&self, address: u64) -> bool {
        address >= self.value && address - self.value < self.size
    }

    /// Whether this could be the code an address is in: a function, or an
    /// untyped label, but not an Arm mapping symbol such as `$x` or `$d`,
    /// which marks where code or data starts rather than naming anything.
    fn is_code(&self) -> bool {
        let named = self.name.first().is_some_and(|&first| first != b'$');
        named && (self.kind == STT_FUNC || self.kind == STT_NOTYPE)
    }
}

/// The fields of a section header the symbol table needs.
#[derive(Clone, Copy, Debug)]
struct Section {
    kind: u32,
    offset: u64,
    size: u64,
    link: u32,
    entsize: u64,
}

impl Section {
    /// Decode the section header at `base`.
    fn read(class: Class, image: &[u8], base: usize) -> Option<Section> {
        let at = |offset: usize| base.checked_add(offset);
        match class {
            Class::Elf32 => Some(Section {
                kind: u32_at(image, at(4)?)?,
                offset: u64::from(u32_at(image, at(16)?)?),
                size: u64::from(u32_at(image, at(20)?)?),
                link: u32_at(image, at(24)?)?,
                entsize: u64::from(u32_at(image, at(36)?)?),
            }),
            Class::Elf64 => Some(Section {
                kind: u32_at(image, at(4)?)?,
                offset: u64_at(image, at(24)?)?,
                size: u64_at(image, at(32)?)?,
                link: u32_at(image, at(40)?)?,
                entsize: u64_at(image, at(56)?)?,
            }),
        }
    }

    /// The section's contents, if they are inside the image.
    fn bytes<'a>(&self, image: &'a [u8]) -> Option<&'a [u8]> {
        let start = usize::try_from(self.offset).ok()?;
        let len = usize::try_from(self.size).ok()?;
        image.get(start..start.checked_add(len)?)
    }
}

impl<'a> Elf<'a> {
    /// Where the section header table is, how large its entries are, and how
    /// many there are.
    fn section_table(&self) -> Option<(u64, u64, u64)> {
        let class = self.class();
        let (shoff, shentsize, shnum, minimum) = match class {
            Class::Elf32 => (32, 46, 48, SHDR32_SIZE),
            Class::Elf64 => (40, 58, 60, SHDR_SIZE),
        };
        let table = word_at(class, self.image, shoff)?;
        let entsize = u64::from(u16_at(self.image, shentsize)?);
        let count = u64::from(u16_at(self.image, shnum)?);
        (entsize >= minimum as u64).then_some((table, entsize, count))
    }

    /// The section header at `index`, if it is inside the image.
    fn section(&self, index: u64) -> Option<Section> {
        let (table, entsize, count) = self.section_table()?;
        if index >= count {
            return None;
        }
        let base = table.checked_add(index.checked_mul(entsize)?)?;
        Section::read(self.class(), self.image, usize::try_from(base).ok()?)
    }

    /// The defined entries of the image's symbol table, or `None` if it has
    /// none or the table does not fit inside the image.
    #[must_use]
    pub fn symbols(&self) -> Option<Symbols<'a>> {
        let (_, _, count) = self.section_table()?;
        let table = (0..count)
            .filter_map(|index| self.section(index))
            .find(|section| section.kind == SHT_SYMTAB)?;
        let strings = self.section(u64::from(table.link))?;
        if strings.kind != SHT_STRTAB {
            return None;
        }
        let minimum = match self.class() {
            Class::Elf32 => SYM32_SIZE,
            Class::Elf64 => SYM_SIZE,
        };
        let entsize = usize::try_from(table.entsize).ok()?;
        if entsize < minimum {
            return None;
        }
        Some(Symbols {
            class: self.class(),
            entries: table.bytes(self.image)?,
            strings: strings.bytes(self.image)?,
            entsize,
            next: 0,
            thumb_bit: self.machine() == EM_ARM,
        })
    }

    /// The code symbol `address` is in, and how far into it `address` is.
    ///
    /// A function whose size covers the address wins. Failing one, the nearest
    /// *unsized* code symbol below the address is taken, because hand-written
    /// assembly — exception vectors, trampolines — is labelled without a size.
    /// A sized function that ends below the address is never stretched to
    /// cover it, and an unsized one reaches only [`MAX_LABEL_SPAN`]: an
    /// address in the gap is more usefully shown as a bare number than
    /// attributed to the wrong function.
    #[must_use]
    pub fn function_at(&self, address: u64) -> Option<(Symbol<'a>, u64)> {
        let mut label: Option<Symbol<'a>> = None;
        for symbol in self.symbols()? {
            if !symbol.is_code() || symbol.value > address {
                continue;
            }
            if symbol.kind == STT_FUNC && symbol.contains(address) {
                return Some((symbol, address - symbol.value));
            }
            if symbol.size == 0 && label.is_none_or(|best| symbol.value > best.value) {
                label = Some(symbol);
            }
        }
        label
            .filter(|symbol| address - symbol.value <= MAX_LABEL_SPAN)
            .map(|symbol| (symbol, address - symbol.value))
    }
}

/// The defined entries of a symbol table, in table order.
///
/// Undefined entries — the null symbol, and references to symbols another
/// object would have provided — are skipped, and so is an entry whose name
/// lies outside the string table, rather than ending the iteration.
#[derive(Clone, Debug)]
pub struct Symbols<'a> {
    class: Class,
    entries: &'a [u8],
    strings: &'a [u8],
    entsize: usize,
    next: usize,
    thumb_bit: bool,
}

impl<'a> Symbols<'a> {
    /// Decode one entry, or `None` if it is undefined or its name is not in
    /// the string table.
    fn decode(&self, entry: &[u8]) -> Option<Symbol<'a>> {
        let (name, info, section, value, size) = match self.class {
            Class::Elf32 => (
                u32_at(entry, 0)?,
                *entry.get(12)?,
                u16_at(entry, 14)?,
                u64::from(u32_at(entry, 4)?),
                u64::from(u32_at(entry, 8)?),
            ),
            Class::Elf64 => (
                u32_at(entry, 0)?,
                *entry.get(4)?,
                u16_at(entry, 6)?,
                u64_at(entry, 8)?,
                u64_at(entry, 16)?,
            ),
        };
        // SHN_UNDEF: a reference, not a definition.
        if section == 0 {
            return None;
        }
        let rest = self.strings.get(usize::try_from(name).ok()?..)?;
        let end = rest.iter().position(|&byte| byte == 0)?;
        let kind = info & 0xF;
        // A Thumb function's address has its low bit set to say so; the code
        // itself starts at the even address.
        let value = if self.thumb_bit && kind == STT_FUNC {
            value & !1
        } else {
            value
        };
        Some(Symbol {
            name: rest.get(..end)?,
            value,
            size,
            kind,
        })
    }
}

impl<'a> Iterator for Symbols<'a> {
    type Item = Symbol<'a>;

    fn next(&mut self) -> Option<Symbol<'a>> {
        loop {
            let base = self.next.checked_mul(self.entsize)?;
            let entry = self.entries.get(base..base.checked_add(self.entsize)?)?;
            self.next = self.next.checked_add(1)?;
            if let Some(symbol) = self.decode(entry) {
                return Some(symbol);
            }
        }
    }
}

#[cfg(test)]
mod tests;
