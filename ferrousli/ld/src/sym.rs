//! Finding a symbol by name, through the GNU hash table.
//!
//! # The table
//!
//! ```text
//!   nbuckets  symoffset  bloom_size  bloom_shift
//!   bloom[bloom_size]            a Bloom filter over the defined symbols
//!   buckets[nbuckets]            first symbol index of each chain
//!   chain[]                      one word per symbol from `symoffset` up
//! ```
//!
//! A chain word holds the symbol's hash with its lowest bit replaced by a
//! flag that is set on the last entry of the chain. So a lookup compares
//! hashes ignoring bit 0, and stops when it sees the flag.
//!
//! The Bloom filter is what makes the table worth having: a name that is not
//! in an object is usually rejected by two bit tests, without touching the
//! buckets or the string table. A symbol resolved against a chain of twenty
//! libraries is tested against nineteen filters and one real lookup.
//!
//! # Why the old `DT_HASH` is not read
//!
//! Every linker in use emits `DT_GNU_HASH`, and has since 2006; `--hash-style`
//! defaults to `gnu` or `both` everywhere. An object with only `DT_HASH` is
//! one this loader reports rather than one it works around, because a silent
//! fallback would be code that nothing ever runs.

use core::ffi::c_char;

use crate::elf::{SHN_UNDEF, STB_GLOBAL, STB_WEAK, Sym, st_bind};
use crate::object::{Defined, Object};

/// The GNU hash of a symbol name: djb2 with a multiplier of 33.
#[must_use]
pub fn hash(name: *const c_char) -> u32 {
    let mut value: u32 = 5381;
    let mut at = name;
    loop {
        // SAFETY: a symbol name is a NUL-terminated string, and this stops at
        // the NUL.
        let byte = unsafe { at.read() } as u8;
        if byte == 0 {
            return value;
        }
        value = value.wrapping_mul(33).wrapping_add(u32::from(byte));
        // SAFETY: the byte read was not the terminator.
        at = unsafe { at.add(1) };
    }
}

/// Whether two NUL-terminated names are the same.
#[must_use]
fn same_name(a: *const c_char, b: *const c_char) -> bool {
    let mut a = a;
    let mut b = b;
    loop {
        // SAFETY: both are NUL-terminated, and this stops at the first NUL.
        let (x, y) = unsafe { (a.read(), b.read()) };
        if x != y {
            return false;
        }
        if x == 0 {
            return true;
        }
        // SAFETY: neither byte was the terminator.
        (a, b) = unsafe { (a.add(1), b.add(1)) };
    }
}

/// A symbol this object defines.
#[derive(Debug, Clone, Copy)]
pub struct Found {
    /// Its run-time address: the object's base plus the symbol's value.
    pub address: usize,
    /// Its value as written in the symbol table.
    ///
    /// For ordinary symbols this is relative to the defining object's base.
    /// For `STT_TLS` it is instead the offset inside that object's TLS image.
    pub value: usize,
    /// Its `st_info`, which says whether it is a function resolved by calling
    /// it.
    pub info: u8,
    /// Its size.
    pub size: usize,
    /// The defining object's TLS image offset from the thread pointer, when
    /// it has one. [`crate::scope::Scope`] supplies this after lookup.
    pub tls_offset: Option<isize>,
    /// The defining object's index in the scope, which is its TLS module
    /// number less one. [`crate::scope::Scope`] supplies this too.
    pub module: usize,
}

/// The word width of the Bloom filter: one machine word per element, as the
/// specification defines it in terms of `Elf_Addr`.
const BLOOM_BITS: u32 = usize::BITS;

/// Look `name` up in `object`, and return it only if this object *defines*
/// it.
///
/// A symbol table entry with `st_shndx == SHN_UNDEF` is the object's own
/// request for the symbol, not a definition, and answering with one would
/// resolve an import to itself.
///
/// Only global and weak symbols are visible to other objects; a local one is
/// the object's private business and never appears in the hash table anyway.
///
/// `version` is the version the reference names, if it names one. A
/// definition answers it only if it carries that version or none at all; a
/// reference naming none is answered by any definition not hidden. So an
/// object may define one name several times, as glibc defines `memcpy` at
/// `GLIBC_2.2.5` and `GLIBC_2.14`, and each binary gets the one it was
/// linked against. The chain holds every definition of the name, so the walk
/// goes on past the first one that does not answer.
#[must_use]
pub fn lookup(
    object: &Object,
    name: *const c_char,
    hash: u32,
    version: Option<*const c_char>,
) -> Option<Found> {
    if object.gnu_hash.is_null() || object.symtab.is_null() {
        return None;
    }
    let table = object.gnu_hash;
    // SAFETY: the four header words are the first of the table.
    let (nbuckets, symoffset, bloom_size, bloom_shift) = unsafe {
        (
            table.read(),
            table.add(1).read(),
            table.add(2).read(),
            table.add(3).read(),
        )
    };
    if nbuckets == 0 || bloom_size == 0 {
        return None;
    }

    // The Bloom filter: two bits derived from the hash. If either is clear
    // the object certainly does not define the name.
    // SAFETY: the filter follows the four header words.
    let bloom = unsafe { table.add(4) }.cast::<usize>();
    let word_index = (hash / BLOOM_BITS) & (bloom_size - 1);
    // SAFETY: masked to the filter's length, which is a power of two.
    let word = unsafe { bloom.add(word_index as usize).read() };
    let first = 1_usize.wrapping_shl(hash % BLOOM_BITS);
    let second = 1_usize.wrapping_shl(hash.wrapping_shr(bloom_shift) % BLOOM_BITS);
    if word & first == 0 || word & second == 0 {
        return None;
    }

    // SAFETY: the buckets follow the filter, which is `bloom_size` words.
    let buckets = unsafe { bloom.add(bloom_size as usize) }.cast::<u32>();
    // SAFETY: the bucket index is taken modulo the bucket count.
    let mut index = unsafe { buckets.add((hash % nbuckets) as usize).read() };
    if index < symoffset {
        return None;
    }
    // SAFETY: the chain array follows the buckets.
    let chain = unsafe { buckets.add(nbuckets as usize) };

    loop {
        // SAFETY: the chain has one word per symbol from `symoffset` up, and
        // the walk stops at the word whose bit 0 is set.
        let word = unsafe { chain.add((index - symoffset) as usize).read() };
        if word | 1 == hash | 1 {
            // SAFETY: `index` is a symbol table index the chain gave.
            let symbol = unsafe { object.symtab.add(index as usize).read() };
            if let Some(found) = defines(object, &symbol, name)
                && answers(object, index as usize, version)
            {
                return Some(found);
            }
        }
        if word & 1 != 0 {
            return None;
        }
        index += 1;
    }
}

/// Whether the definition at `index` answers a reference naming `version`.
fn answers(object: &Object, index: usize, version: Option<*const c_char>) -> bool {
    match object.defined_version(index) {
        Defined::Local => false,
        Defined::Unversioned => true,
        Defined::Named { name, hidden } => match version {
            Some(wanted) => same_name(name, wanted),
            None => !hidden,
        },
    }
}

/// Whether `symbol` is this object's definition of `name`.
fn defines(object: &Object, symbol: &Sym, name: *const c_char) -> Option<Found> {
    if symbol.st_shndx == SHN_UNDEF {
        return None;
    }
    let bind = st_bind(symbol.st_info);
    if bind != STB_GLOBAL && bind != STB_WEAK {
        return None;
    }
    let candidate = object.name(symbol.st_name)?;
    if !same_name(candidate, name) {
        return None;
    }
    Some(Found {
        address: (symbol.st_value as usize).wrapping_add(object.base),
        value: symbol.st_value as usize,
        info: symbol.st_info,
        size: symbol.st_size as usize,
        tls_offset: None,
        module: 0,
    })
}

/// How many dynamic symbols `object` has.
///
/// No dynamic tag says. The GNU hash table knows where the last hashed
/// symbol is: the end of the chain the highest bucket starts, marked by the
/// low bit. Every symbol not in the table -- the local and the undefined --
/// sits below `symoffset`.
#[must_use]
pub fn count(object: &Object) -> usize {
    let table = object.gnu_hash;
    if table.is_null() {
        return 0;
    }
    // SAFETY: the four header words are the first of the table.
    let nbuckets = unsafe { table.read() };
    // SAFETY: as above.
    let symoffset = unsafe { table.wrapping_add(1).read() };
    // SAFETY: as above.
    let bloom_size = unsafe { table.wrapping_add(2).read() };
    // The buckets follow the header and the Bloom filter.
    let buckets = table
        .wrapping_add(4)
        .cast::<usize>()
        .wrapping_add(bloom_size as usize)
        .cast::<u32>();
    let mut last = 0;
    for bucket in 0..nbuckets as usize {
        // SAFETY: `bucket` is below the table's bucket count.
        last = last.max(unsafe { buckets.wrapping_add(bucket).read() });
    }
    if last < symoffset {
        return symoffset as usize;
    }
    let chain = buckets.wrapping_add(nbuckets as usize);
    loop {
        let at = chain.wrapping_add((last - symoffset) as usize);
        // SAFETY: the chain has a word per symbol from `symoffset`, and ends
        // at the word whose low bit is set.
        let word = unsafe { at.read() };
        if word & 1 != 0 {
            return last as usize + 1;
        }
        last += 1;
    }
}

/// The defined symbol of `object` nearest at or below `address`: its name
/// and address. What `dladdr` names an address by.
#[must_use]
pub fn nearest(object: &Object, address: usize) -> Option<(*const c_char, usize)> {
    let mut best: Option<(*const c_char, usize)> = None;
    for index in 1..count(object) {
        let at = object.symtab.wrapping_add(index);
        // SAFETY: `index` is below the symbol count the hash table gives.
        let symbol = unsafe { at.read() };
        // Undefined symbols name nothing here, and a TLS symbol's value is
        // not an address.
        if symbol.st_shndx == SHN_UNDEF || crate::elf::st_type(symbol.st_info) == 6 {
            continue;
        }
        let at = (symbol.st_value as usize).wrapping_add(object.base);
        if at > address || best.is_some_and(|(_, best)| best >= at) {
            continue;
        }
        if let Some(name) = object.name(symbol.st_name) {
            best = Some((name, at));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values in the GNU hash specification's own worked example, and the
    /// two names every C program has. A wrong multiplier or a wrong seed
    /// gives a table no lookup finds anything in, and the failure appears as
    /// an unresolved symbol rather than as a wrong number, so the numbers are
    /// pinned here.
    #[test]
    fn hashes_match_the_published_values() {
        for (name, want) in [
            (c"", 5381_u32),
            (c"printf", 0x156b_2bb8),
            (c"exit", 0x7c96_7e3f),
            (c"syscall", 0xbac2_12a0),
            (c"main", 0x7c9a_7f6a),
        ] {
            assert_eq!(hash(name.as_ptr()), want, "the hash of {name:?}");
        }
    }

    #[test]
    fn names_compare_to_their_terminator_and_no_further() {
        assert!(same_name(c"exit".as_ptr(), c"exit".as_ptr()));
        assert!(!same_name(c"exit".as_ptr(), c"exi".as_ptr()));
        assert!(!same_name(c"exi".as_ptr(), c"exit".as_ptr()));
        assert!(!same_name(c"exit".as_ptr(), c"axit".as_ptr()));
        assert!(same_name(c"".as_ptr(), c"".as_ptr()));
    }
}
