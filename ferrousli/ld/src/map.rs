//! Opening a shared object and mapping its segments.
//!
//! The same job the kernel does for the program, done here for everything the
//! program needs. The shape is the kernel's too:
//!
//! 1. Read the file header and the program headers.
//! 2. Reserve the whole span the object occupies, in one unreadable mapping,
//!    so that nothing else takes a page in the middle of it.
//! 3. Map each `PT_LOAD` over that reservation at its own place, from the
//!    file, with the permissions it asks for.
//! 4. Zero the tail of the last page of a segment whose `p_memsz` exceeds its
//!    `p_filesz`, and map anonymous pages for whole pages past the file.
//!
//! Step 2 is what makes an object contiguous: a segment mapped on its own
//! could land anywhere, and the distance between two segments is fixed at
//! link time and cannot be changed afterwards.
//!
//! # `.bss` is not `p_memsz` minus `p_filesz` bytes of zeroes
//!
//! Part of it is: the bytes between the end of the file's contents and the
//! end of that page, which the file's last page holds whatever happened to be
//! in the file there. Those have to be written. Whole pages past that are
//! mapped anonymous and arrive zeroed, which is both faster and the only way
//! a large `.bss` does not become a large write.

use core::ffi::{c_char, c_int};

use crate::elf::{PF_R, PF_W, PF_X, PT_DYNAMIC, PT_LOAD, PT_TLS, Phdr};
use crate::object::Tls;
use crate::report::Error;
use crate::sys::{self, nr};

/// `O_RDONLY | O_CLOEXEC`.
const O_RDONLY: usize = 0;
/// From `asm-generic/fcntl.h`: close the descriptor on `execve`.
const O_CLOEXEC: usize = 0o2_000_000;

/// `PROT_*`, from `asm-generic/mman-common.h`.
const PROT_NONE: usize = 0;
/// Readable.
const PROT_READ: usize = 1;
/// Writable.
const PROT_WRITE: usize = 2;
/// Executable.
const PROT_EXEC: usize = 4;

/// `MAP_PRIVATE`.
const MAP_PRIVATE: usize = 2;
/// `MAP_FIXED`.
const MAP_FIXED: usize = 0x10;
/// `MAP_ANONYMOUS`.
const MAP_ANONYMOUS: usize = 0x20;

/// How much of a file's head is read to find its program headers.
///
/// One page holds the ELF header and the program header table of every object
/// anyone links: the table follows the header immediately and a large object
/// has a dozen entries.
const HEAD_BYTES: usize = 4096;

/// The largest `e_phnum` this loader reads, which bounds the buffer above.
const MAX_PHNUM: usize = 64;

/// A mapped object: where it went, and the two segments the loader needs to
/// find again.
#[derive(Debug, Clone, Copy)]
pub struct Mapped {
    /// Its load bias.
    pub base: usize,
    /// Its `PT_DYNAMIC`, at its run-time address.
    pub dynamic: *const crate::elf::Dyn,
    /// Its `PT_TLS`, if it has one, at its run-time address.
    pub tls: Option<Tls>,
    /// Its `PT_GNU_RELRO`, as a run-time address and a length, or `(0, 0)`.
    pub relro: (usize, usize),
}

/// Open `path` and map every loadable segment of it.
///
/// # Errors
///
/// [`Error`], naming `path`.
pub fn object(path: *const c_char, page_size: usize) -> Result<Mapped, Error> {
    let fd = open(path)?;
    let mapped = map_opened(path, fd, page_size);
    // SAFETY: `fd` is the descriptor just opened, and nothing else holds it.
    let _ = unsafe { sys::syscall2(nr::CLOSE, fd as usize, 0) };
    mapped
}

/// `open(path, O_RDONLY | O_CLOEXEC)`.
fn open(path: *const c_char) -> Result<c_int, Error> {
    // SAFETY: `path` is a NUL-terminated path, and the flags open no file for
    // writing.
    let fd = unsafe { sys::syscall3(nr::OPEN, path as usize, O_RDONLY | O_CLOEXEC, 0) };
    c_int::try_from(fd).map_err(|_| Error::CannotOpen(path, fd))
}

/// [`object`]'s middle, so that the descriptor is closed whatever happens.
fn map_opened(path: *const c_char, fd: c_int, page_size: usize) -> Result<Mapped, Error> {
    let mut head = [0_u8; HEAD_BYTES];
    let read = pread(fd, &mut head, 0);
    if read < 0 {
        return Err(Error::CannotMap(path, read));
    }

    let header = ElfHead::parse(&head).ok_or(Error::NotAnObject(path))?;
    if header.phnum > MAX_PHNUM {
        return Err(Error::NotAnObject(path));
    }
    let headers = header
        .program_headers(&head)
        .ok_or(Error::NotAnObject(path))?;

    let (low, high) = span(headers, page_size).ok_or(Error::NotAnObject(path))?;

    // One reservation over the whole object, unreadable, so that the distance
    // between its segments is the distance the linker assumed.
    let base = reserve(high - low).map_err(|errno| Error::CannotMap(path, errno))?;
    let bias = base.wrapping_sub(low);

    let mut dynamic: *const crate::elf::Dyn = core::ptr::null();
    let mut tls = None;
    let mut relro = (0, 0);
    for header in headers {
        match header.p_type {
            PT_LOAD => {
                place(header, bias, fd, page_size).map_err(|e| Error::CannotMap(path, e))?;
            }
            PT_DYNAMIC => {
                dynamic = (header.p_vaddr as usize).wrapping_add(bias) as *const crate::elf::Dyn;
            }
            PT_TLS => {
                tls = Tls::new(
                    (header.p_vaddr as usize).wrapping_add(bias),
                    header.p_filesz as usize,
                    header.p_memsz as usize,
                    header.p_align as usize,
                );
                if tls.is_none() {
                    return Err(Error::NotAnObject(path));
                }
            }
            crate::elf::PT_GNU_RELRO => {
                relro = (
                    (header.p_vaddr as usize).wrapping_add(bias),
                    header.p_memsz as usize,
                );
            }
            _ => {}
        }
    }
    if dynamic.is_null() {
        return Err(Error::NotAnObject(path));
    }
    Ok(Mapped {
        base: bias,
        dynamic,
        tls,
        relro,
    })
}

/// `pread`, which is used rather than `read` so that the file offset is never
/// state two calls have to agree about.
fn pread(fd: c_int, into: &mut [u8], at: usize) -> isize {
    // SAFETY: `into` is a live slice and its length is its length.
    unsafe {
        sys::syscall4(
            nr::PREAD64,
            fd as usize,
            into.as_mut_ptr() as usize,
            into.len(),
            at,
        )
    }
}

/// Reserve `size` bytes at an address the kernel chooses, with no access.
fn reserve(size: usize) -> Result<usize, isize> {
    // SAFETY: a mapping with no fixed address and no access takes address
    // space and nothing else.
    let at = unsafe {
        sys::syscall6(
            nr::MMAP,
            0,
            size,
            PROT_NONE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            usize::MAX, // -1: no file
            0,
        )
    };
    usize::try_from(at).map_err(|_| at)
}

/// Map one `PT_LOAD` over the reservation.
fn place(header: &Phdr, bias: usize, fd: c_int, page_size: usize) -> Result<(), isize> {
    let mask = page_size - 1;
    let vaddr = header.p_vaddr as usize;
    let offset = header.p_offset as usize;
    let filesz = header.p_filesz as usize;
    let memsz = header.p_memsz as usize;

    // A segment need not start on a page boundary, and the part of its first
    // page before it belongs to whatever precedes it in the file — which is
    // mapped with it, because a mapping is pages.
    let slop = vaddr & mask;
    let start = vaddr.wrapping_sub(slop).wrapping_add(bias);
    let file_at = offset.wrapping_sub(slop);
    let file_len = (filesz + slop + mask) & !mask;

    let prot = protection(header.p_flags);
    if file_len > 0 {
        // SAFETY: the range is inside the reservation made for this object,
        // and `MAP_FIXED` over one's own reservation replaces it.
        let at = unsafe {
            sys::syscall6(
                nr::MMAP,
                start,
                file_len,
                // Writable while the `.bss` tail is zeroed, then narrowed.
                prot | PROT_WRITE,
                MAP_PRIVATE | MAP_FIXED,
                fd as usize,
                file_at,
            )
        };
        if usize::try_from(at) != Ok(start) {
            return Err(at);
        }
    }

    if memsz > filesz {
        zero_the_tail(vaddr, filesz, bias, page_size);
        // Whole pages past the file's last are anonymous, which is both
        // faster than writing them and how a large `.bss` costs nothing until
        // it is touched.
        let mapped_to = (vaddr + filesz + slop + mask) & !mask;
        let needed_to = (vaddr + memsz + mask) & !mask;
        if needed_to > mapped_to {
            // SAFETY: still inside this object's reservation.
            let at = unsafe {
                sys::syscall6(
                    nr::MMAP,
                    mapped_to.wrapping_add(bias),
                    needed_to - mapped_to,
                    prot | PROT_WRITE,
                    MAP_PRIVATE | MAP_FIXED | MAP_ANONYMOUS,
                    usize::MAX,
                    0,
                )
            };
            if usize::try_from(at) != Ok(mapped_to.wrapping_add(bias)) {
                return Err(at);
            }
        }
    }

    // The permissions it actually asked for. A segment that is not writable
    // is made read-only now that nothing more is written into it.
    if prot & PROT_WRITE == 0 {
        let len = ((vaddr + memsz + mask) & !mask) - (vaddr & !mask);
        // SAFETY: the range is this object's own, just mapped.
        let _ = unsafe { sys::syscall3(nr::MPROTECT, start, len, prot) };
    }
    Ok(())
}

/// Zero the part of the file's last page that is `.bss` rather than file.
fn zero_the_tail(vaddr: usize, filesz: usize, bias: usize, page_size: usize) {
    let mask = page_size - 1;
    let end = vaddr.wrapping_add(filesz);
    let tail = (page_size - (end & mask)) & mask;
    if tail == 0 {
        return;
    }
    let at = end.wrapping_add(bias) as *mut u8;
    let mut written = 0;
    while written < tail {
        // SAFETY: the bytes from the end of the file's contents to the end of
        // that page are inside the mapping just made, and writable.
        unsafe { at.add(written).write(0) };
        written += 1;
    }
}

/// `PF_*` as `PROT_*`.
const fn protection(flags: u32) -> usize {
    let mut prot = PROT_NONE;
    if flags & PF_R != 0 {
        prot |= PROT_READ;
    }
    if flags & PF_W != 0 {
        prot |= PROT_WRITE;
    }
    if flags & PF_X != 0 {
        prot |= PROT_EXEC;
    }
    prot
}

/// The lowest and highest page a set of loadable segments occupies.
fn span(headers: &[Phdr], page_size: usize) -> Option<(usize, usize)> {
    let mask = page_size - 1;
    let mut low = usize::MAX;
    let mut high = 0;
    for header in headers {
        if header.p_type != PT_LOAD {
            continue;
        }
        let start = header.p_vaddr as usize;
        let end = start.checked_add(header.p_memsz as usize)?;
        low = low.min(start & !mask);
        high = high.max((end + mask) & !mask);
    }
    if low == usize::MAX {
        None
    } else {
        Some((low, high))
    }
}

/// Just enough of the ELF file header to find the program headers.
#[derive(Debug, Clone, Copy)]
struct ElfHead {
    /// `e_phoff`.
    phoff: usize,
    /// `e_phnum`.
    phnum: usize,
}

impl ElfHead {
    /// The magic every ELF file starts with.
    const MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

    /// Read it, or `None` if this is not an object of this build's class.
    fn parse(head: &[u8]) -> Option<ElfHead> {
        if head.get(..4)? != Self::MAGIC {
            return None;
        }
        // A loader is loaded into the process it links, so the class and the
        // byte order are never in question: an object of another class is one
        // this process could not run, not one to be read differently.
        let class = if cfg!(target_pointer_width = "64") {
            2
        } else {
            1
        };
        if head.get(4) != Some(&class) || head.get(5) != Some(&1) {
            return None;
        }
        let (phoff_at, phnum_at, phentsize_at) = if cfg!(target_pointer_width = "64") {
            (32, 56, 54)
        } else {
            (28, 44, 42)
        };
        let phoff = read_address(head, phoff_at)?;
        let phnum = usize::from(read_u16(head, phnum_at)?);
        if usize::from(read_u16(head, phentsize_at)?) != size_of::<Phdr>() {
            return None;
        }
        Some(ElfHead { phoff, phnum })
    }

    /// The program header table, as a slice of the bytes already read.
    fn program_headers<'a>(&self, head: &'a [u8]) -> Option<&'a [Phdr]> {
        let bytes = self.phnum.checked_mul(size_of::<Phdr>())?;
        let end = self.phoff.checked_add(bytes)?;
        let table = head.get(self.phoff..end)?;
        if !table.as_ptr().cast::<Phdr>().is_aligned() {
            return None;
        }
        // SAFETY: the range is inside the bytes read, is the right length for
        // `phnum` headers, and is aligned for one. `Phdr` is `repr(C)` and
        // every bit pattern of its fields is valid.
        Some(unsafe { core::slice::from_raw_parts(table.as_ptr().cast::<Phdr>(), self.phnum) })
    }
}

/// A `u16` at `at`, little-endian.
fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let field = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([*field.first()?, *field.get(1)?]))
}

/// An address-width field at `at`, little-endian.
fn read_address(bytes: &[u8], at: usize) -> Option<usize> {
    let width = size_of::<usize>();
    let field = bytes.get(at..at + width)?;
    let mut value: usize = 0;
    let mut index = 0;
    while index < width {
        value |= (*field.get(index)? as usize) << (index * 8);
        index += 1;
    }
    Some(value)
}
