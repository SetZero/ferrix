//! The vDSO: the clock functions the kernel maps into every process, found
//! and called so that reading the clock is not a system call.
//!
//! Ferrix's x86-64 kernel maps a small shared object into each Linux program
//! and gives its address as `AT_SYSINFO_EHDR`, as Linux does. It exports
//! `__vdso_clock_gettime`, `__vdso_gettimeofday` and `__vdso_time` at version
//! `LINUX_2.6`, which compute the answer from the TSC and a data page the
//! kernel keeps, without entering the kernel. A browser reads the clock tens
//! of thousands of times a second: Steam's web helper on this library made
//! about 36,000 `clock_gettime` system calls a second before this, where the
//! same Chrome on glibc, which uses the vDSO, made none.
//!
//! [`lookup`] finds a symbol in the image the way musl 1.2.5's `__vdsosym`
//! (`src/internal/vdso.c`, MIT) does: through the program headers to the
//! dynamic section, then every symbol in the `DT_HASH` table's count, with
//! its version checked against the version definitions. Unlike musl, every
//! read is bounded by the image's extent as its headers give it, so a
//! malformed image is refused rather than read past.
//!
//! Each function is resolved the first time it is called, not at start-up:
//! a shared library's constructor can read the clock before
//! `__libc_start_main` runs, and the auxiliary vector is not known before
//! the loader publishes it. Two threads resolving at once compute the same
//! answer and store the same word, so no lock is needed; until the vector is
//! known nothing is stored, and the call is the system call.
//!
//! Only x86-64 has a Ferrix vDSO. AArch64 and ARMv7-A make the system call.

use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::auxv;
use crate::time::{Timespec, Timeval};

/// `AT_SYSINFO_EHDR`, from `linux/auxvec.h`: the vDSO's ELF header.
pub const AT_SYSINFO_EHDR: usize = 33;

/// The version the clock functions are defined at, in Linux's x86-64 vDSO
/// and Ferrix's.
const VERSION: &[u8] = b"LINUX_2.6";

/// The largest image believed: Linux's x86-64 vDSO is two pages and
/// Ferrix's one. A header claiming more is not read.
const MAX_IMAGE: usize = 1 << 20;

// ELF64 layouts and constants, from `elf.h`.
const EHDR_BYTES: usize = 64;
const PHDR_BYTES: usize = 56;
const SYM_BYTES: usize = 24;
const DYN_BYTES: usize = 16;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const DT_NULL: u64 = 0;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_VERSYM: u64 = 0x6fff_fff0;
const DT_VERDEF: u64 = 0x6fff_fffc;
const VER_FLG_BASE: u16 = 1;
/// `STT_NOTYPE`, `STT_OBJECT`, `STT_FUNC` and `STT_COMMON`, as musl accepts.
const OK_TYPES: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 5);
/// `STB_GLOBAL`, `STB_WEAK` and `STB_GNU_UNIQUE`, as musl accepts.
const OK_BINDS: u32 = (1 << 1) | (1 << 2) | (1 << 10);

/// Reads `N` bytes at `at`.
fn bytes<const N: usize>(image: &[u8], at: usize) -> Option<[u8; N]> {
    image.get(at..at.checked_add(N)?)?.try_into().ok()
}

fn read16(image: &[u8], at: usize) -> Option<u16> {
    bytes(image, at).map(u16::from_le_bytes)
}

fn read32(image: &[u8], at: usize) -> Option<u32> {
    bytes(image, at).map(u32::from_le_bytes)
}

fn read64(image: &[u8], at: usize) -> Option<u64> {
    bytes(image, at).map(u64::from_le_bytes)
}

/// [`read64`] as an offset or count.
fn read_usize(image: &[u8], at: usize) -> Option<usize> {
    usize::try_from(read64(image, at)?).ok()
}

/// Whether the NUL-terminated string at `at` is `name`.
fn string_is(image: &[u8], at: usize, name: &[u8]) -> Option<bool> {
    let end = at.checked_add(name.len())?;
    Some(image.get(at..end)? == name && *image.get(end)? == 0)
}

/// How many bytes from `ehdr` an image's headers say it takes: through its
/// program headers and the file contents of its segments. `None` if the
/// header is not a little-endian ELF64 one.
///
/// # Safety
///
/// `ehdr` must point at an ELF header, at least [`EHDR_BYTES`] readable,
/// as the kernel's `AT_SYSINFO_EHDR` does.
unsafe fn extent(ehdr: *const u8) -> Option<usize> {
    // SAFETY: the caller vouches for the header's bytes.
    let header = unsafe { core::slice::from_raw_parts(ehdr, EHDR_BYTES) };
    if header.get(..6)? != [0x7f, b'E', b'L', b'F', 2, 1] {
        return None;
    }
    let phoff = read_usize(header, 32)?;
    let phentsize = usize::from(read16(header, 54)?);
    let phnum = usize::from(read16(header, 56)?);
    if phentsize < PHDR_BYTES {
        return None;
    }
    let mut end = phoff
        .checked_add(phentsize.checked_mul(phnum)?)?
        .max(EHDR_BYTES);
    if end > MAX_IMAGE {
        return None;
    }
    // SAFETY: the header says its program headers end at `end`, within the
    // image the kernel mapped whole.
    let headers = unsafe { core::slice::from_raw_parts(ehdr, end) };
    for index in 0..phnum {
        let at = phoff.checked_add(phentsize.checked_mul(index)?)?;
        let kind = read32(headers, at)?;
        if kind == PT_LOAD || kind == PT_DYNAMIC {
            let offset = read_usize(headers, at.checked_add(8)?)?;
            let filesz = read_usize(headers, at.checked_add(32)?)?;
            end = end.max(offset.checked_add(filesz)?);
        }
    }
    (end <= MAX_IMAGE).then_some(end)
}

/// Where a vDSO's tables are, as offsets from its first byte.
struct Tables {
    /// An address in the image less this is an offset into it: the
    /// loadable segment's address less its file offset.
    bias: u64,
    strtab: usize,
    symtab: usize,
    hash: usize,
    /// `DT_VERSYM` and `DT_VERDEF`: checked only when the image has both,
    /// as in musl.
    versions: Option<(usize, usize)>,
}

/// The image's tables, found through its program headers as musl's
/// `__vdsosym` finds them.
fn tables(image: &[u8]) -> Option<Tables> {
    let phoff = read_usize(image, 32)?;
    let phentsize = usize::from(read16(image, 54)?);
    let phnum = usize::from(read16(image, 56)?);
    // musl keeps the last loadable segment's; an image has one.
    let mut bias = None;
    let mut dynamic = None;
    for index in 0..phnum {
        let at = phoff.checked_add(phentsize.checked_mul(index)?)?;
        match read32(image, at)? {
            PT_LOAD => {
                let offset = read64(image, at.checked_add(8)?)?;
                let vaddr = read64(image, at.checked_add(16)?)?;
                bias = Some(vaddr.wrapping_sub(offset));
            }
            PT_DYNAMIC => dynamic = Some(read_usize(image, at.checked_add(8)?)?),
            _ => {}
        }
    }
    let (bias, mut at) = (bias?, dynamic?);
    let (mut strtab, mut symtab, mut hash, mut versym, mut verdef) = (None, None, None, None, None);
    loop {
        let tag = read64(image, at)?;
        if tag == DT_NULL {
            break;
        }
        let offset = usize::try_from(read64(image, at.checked_add(8)?)?.wrapping_sub(bias)).ok();
        match tag {
            DT_STRTAB => strtab = offset,
            DT_SYMTAB => symtab = offset,
            DT_HASH => hash = offset,
            DT_VERSYM => versym = offset,
            DT_VERDEF => verdef = offset,
            _ => {}
        }
        at = at.checked_add(DYN_BYTES)?;
    }
    Some(Tables {
        bias,
        strtab: strtab?,
        symtab: symtab?,
        hash: hash?,
        versions: versym.zip(verdef),
    })
}

/// Whether version index `index` is defined as `version`: musl's `checkver`.
fn version_is(
    image: &[u8],
    tables: &Tables,
    verdef: usize,
    index: u16,
    version: &[u8],
) -> Option<bool> {
    let index = index & 0x7fff;
    let mut def = verdef;
    loop {
        if read16(image, def.checked_add(2)?)? & VER_FLG_BASE == 0
            && read16(image, def.checked_add(4)?)? & 0x7fff == index
        {
            break;
        }
        let next = usize::try_from(read32(image, def.checked_add(16)?)?).ok()?;
        if next == 0 {
            return Some(false);
        }
        def = def.checked_add(next)?;
    }
    let aux = def.checked_add(usize::try_from(read32(image, def.checked_add(12)?)?).ok()?)?;
    let name = usize::try_from(read32(image, aux)?).ok()?;
    string_is(image, tables.strtab.checked_add(name)?, version)
}

/// Where the function or object `name` at `version` is, in bytes from the
/// image's first byte, or `None` if the image does not define it or is not
/// one this can read.
pub fn lookup(image: &[u8], name: &[u8], version: &[u8]) -> Option<usize> {
    let tables = tables(image)?;
    let count = usize::try_from(read32(image, tables.hash.checked_add(4)?)?).ok()?;
    for index in 0..count {
        let sym = tables.symtab.checked_add(SYM_BYTES.checked_mul(index)?)?;
        let info = *image.get(sym.checked_add(4)?)?;
        if (1u32 << (info & 0xf)) & OK_TYPES == 0 || (1u32 << (info >> 4)) & OK_BINDS == 0 {
            continue;
        }
        if read16(image, sym.checked_add(6)?)? == 0 {
            continue;
        }
        let at = usize::try_from(read32(image, sym)?).ok()?;
        if !string_is(image, tables.strtab.checked_add(at)?, name)? {
            continue;
        }
        if let Some((versym, verdef)) = tables.versions {
            let index = read16(image, versym.checked_add(index.checked_mul(2)?)?)?;
            if !version_is(image, &tables, verdef, index, version)? {
                continue;
            }
        }
        let value = read64(image, sym.checked_add(8)?)?;
        return usize::try_from(value.wrapping_sub(tables.bias)).ok();
    }
    None
}

/// Where `name` is in the vDSO at `ehdr`, as an address.
///
/// # Safety
///
/// `ehdr` must be the vDSO's ELF header, mapped whole for the life of the
/// process, as `AT_SYSINFO_EHDR` gives it.
unsafe fn find(ehdr: usize, name: &[u8]) -> Option<usize> {
    let start = core::ptr::with_exposed_provenance::<u8>(ehdr);
    // SAFETY: the caller vouches for the header.
    let len = unsafe { extent(start) }?;
    // SAFETY: `extent` read the image's length from its own headers, and the
    // kernel maps the whole image.
    let image = unsafe { core::slice::from_raw_parts(start, len) };
    let offset = lookup(image, name, VERSION)?;
    // An entry point outside what the headers map is not believed.
    (offset < len).then(|| ehdr.wrapping_add(offset))
}

/// Not yet looked for.
const UNRESOLVED: usize = 1;
/// Looked for and not found: the system call it is.
const ABSENT: usize = 0;

/// One vDSO function: its name and, once looked for, its address or
/// [`ABSENT`].
struct Slot {
    name: &'static [u8],
    address: AtomicUsize,
}

impl Slot {
    const fn new(name: &'static [u8]) -> Self {
        Self {
            name,
            address: AtomicUsize::new(UNRESOLVED),
        }
    }

    /// The function's address, or `None` when the call should be the system
    /// call.
    fn get(&self) -> Option<usize> {
        match self.address.load(Ordering::Acquire) {
            ABSENT => None,
            UNRESOLVED => self.resolve(),
            address => Some(address),
        }
    }

    #[cold]
    fn resolve(&self) -> Option<usize> {
        // Before the vector is known -- in a shared library's constructor run
        // before the loader published it -- nothing is decided.
        let vector = auxv::known().then(|| auxv::get(AT_SYSINFO_EHDR));
        // SAFETY: `AT_SYSINFO_EHDR` is the kernel's vDSO, mapped whole for
        // the life of the process.
        unsafe { self.settle(vector) }
    }

    /// Decides the address from the vector's `AT_SYSINFO_EHDR`, `vector`:
    /// `None` when the vector is not known yet, which decides nothing.
    ///
    /// # Safety
    ///
    /// A non-zero address in `vector` must be a vDSO's ELF header, mapped
    /// whole for the life of the process.
    unsafe fn settle(&self, vector: Option<Option<usize>>) -> Option<usize> {
        let ehdr = vector?;
        // SAFETY: the caller vouches for a non-zero header.
        let found = ehdr
            .filter(|&ehdr| ehdr != 0)
            .and_then(|ehdr| unsafe { find(ehdr, self.name) });
        // Every thread that gets here finds the same answer, so a race
        // stores the same word twice.
        self.address
            .store(found.unwrap_or(ABSENT), Ordering::Release);
        found
    }
}

static CLOCK_GETTIME: Slot = Slot::new(b"__vdso_clock_gettime");
static GETTIMEOFDAY: Slot = Slot::new(b"__vdso_gettimeofday");
static TIME: Slot = Slot::new(b"__vdso_time");

/// The vDSO's `clock_gettime`.
type ClockGettime = unsafe extern "C" fn(c_int, *mut Timespec) -> c_int;
/// The vDSO's `gettimeofday`.
type Gettimeofday = unsafe extern "C" fn(*mut Timeval, *mut c_void) -> c_int;
/// The vDSO's `time`.
type Time = unsafe extern "C" fn(*mut i64) -> i64;

/// The vDSO's `clock_gettime(clock, ts)`, as zero or a negated error number,
/// or `None` when there is no vDSO function.
///
/// # Safety
///
/// `ts` must be valid for a write of a `struct timespec`.
pub unsafe fn clock_gettime(clock: c_int, ts: *mut Timespec) -> Option<c_int> {
    let address = CLOCK_GETTIME.get()?;
    // SAFETY: the address is the vDSO's `__vdso_clock_gettime`, which has
    // this signature in Linux's vDSO and Ferrix's.
    let function = unsafe { core::mem::transmute::<usize, ClockGettime>(address) };
    // SAFETY: the caller vouches for `ts`.
    Some(unsafe { function(clock, ts) })
}

/// The vDSO's `gettimeofday(tv, NULL)`, as zero or a negated error number,
/// or `None` when there is no vDSO function.
///
/// # Safety
///
/// `tv` must be valid for a write of a `struct timeval`.
pub unsafe fn gettimeofday(tv: *mut Timeval) -> Option<c_int> {
    let address = GETTIMEOFDAY.get()?;
    // SAFETY: the address is the vDSO's `__vdso_gettimeofday`, which has
    // this signature in Linux's vDSO and Ferrix's.
    let function = unsafe { core::mem::transmute::<usize, Gettimeofday>(address) };
    // SAFETY: the caller vouches for `tv`; a null time zone is not written.
    Some(unsafe { function(tv, core::ptr::null_mut()) })
}

/// The vDSO's `time(NULL)`, or `None` when there is no vDSO function.
pub fn time() -> Option<i64> {
    let address = TIME.get()?;
    // SAFETY: the address is the vDSO's `__vdso_time`, which has this
    // signature in Linux's vDSO and Ferrix's.
    let function = unsafe { core::mem::transmute::<usize, Time>(address) };
    // SAFETY: a null pointer is not written.
    Some(unsafe { function(core::ptr::null_mut()) })
}

#[cfg(test)]
mod tests;
