//! The initial process stack image a Linux program starts on.
//!
//! A static musl binary's `_start` does not receive arguments in registers. It
//! reads them off the stack, from a layout the System V ABI fixes and the
//! kernel is obliged to build: `argc`, the `argv` pointers, a null, the `envp`
//! pointers, a null, and then the auxiliary vector — pairs of (key, value)
//! terminated by `AT_NULL` — with the strings those pointers refer to sitting
//! above the whole arrangement.
//!
//! # Why this is a crate and not twenty lines in `execve`
//!
//! Because it is the single most unforgiving byte-level structure in the
//! system call ABI, and every way of getting it wrong produces the same
//! symptom: a program that dies before its first instruction with nothing to
//! say about why. A pointer off by one word makes `argv[0]` the environment. A
//! stack pointer that is 8-byte rather than 16-byte aligned runs correctly
//! until the first SSE or NEON spill. A missing `AT_RANDOM` makes musl read
//! sixteen bytes of stack garbage for its stack-guard cookie, which works, and
//! then a `fork` child disagrees with its parent about the canary.
//!
//! None of that is debuggable from the far side. All of it is a pure function
//! of bytes, so it belongs here, where `cargo test`, Miri and a fuzzer can
//! reach it before the kernel ever calls it.
//!
//! # What the caller owns, and what this crate owns
//!
//! The split is not arbitrary. This crate fills in every auxiliary entry whose
//! value is *an address inside the stack it is building* — `AT_RANDOM`,
//! `AT_EXECFN`, `AT_PLATFORM` — because nothing else can know those addresses
//! until the layout is decided. Everything else is the caller's: `AT_PHDR`,
//! `AT_ENTRY` and `AT_BASE` come from the ELF, `AT_PAGESZ` and `AT_HWCAP` from
//! the machine, `AT_UID` and friends from the credentials. Passing one of the
//! three reserved keys is refused rather than merged, because two entries with
//! the same key is a thing the ABI does not define and the C library resolves
//! by whichever it happens to see last.
//!
//! # Both widths
//!
//! ARMv7-A is a 32-bit target, so a pointer on the stack is four bytes and an
//! auxiliary value that does not fit in four bytes is an error rather than a
//! truncation. [`Width`] carries that, as a value rather than a `cfg`: this
//! crate is built for the host, and a host test that could only check its own
//! pointer width would not be checking the interesting case.
//!
//! # Example
//!
//! ```
//! use ferrix_ustack::{Spec, Width, build};
//! use ferrix_linux_abi::types::{AT_ENTRY, AT_PAGESZ};
//!
//! let mut page = [0_u8; 4096];
//! let spec = Spec {
//!     args: &[b"/bin/sh", b"-c", b"exit 0"],
//!     env: &[b"PATH=/bin"],
//!     auxv: &[(AT_PAGESZ, 4096), (AT_ENTRY, 0x40_1000)],
//!     random: [0x5a; 16],
//!     exec_fn: b"/bin/sh",
//!     platform: Some(b"aarch64"),
//!     width: Width::Bits64,
//! };
//!
//! // The page is mapped so that its last byte is the last byte of the stack.
//! let image = build(&spec, 0x7fff_f000 + 4096, &mut page).expect("it fits");
//! assert_eq!(image.sp % 16, 0, "the ABI requires a 16-byte aligned entry");
//! assert!(image.arg_start < image.arg_end);
//! ```

#![no_std]
#![forbid(unsafe_code)]

use ferrix_linux_abi::types::{AT_EXECFN, AT_NULL, AT_PLATFORM, AT_RANDOM};

/// The number of random bytes `AT_RANDOM` points at.
///
/// Fixed by the ABI at sixteen. musl takes the first `sizeof(uintptr_t)` of
/// them for its stack-guard cookie and the rest seed the pointer mangling, so
/// this is not a size the kernel gets to choose.
pub const RANDOM_BYTES: usize = 16;

/// [`RANDOM_BYTES`] as an address-sized value, so the layout arithmetic needs
/// no conversion that could fail.
const RANDOM_BYTES_U64: u64 = 16;

/// The alignment the stack pointer must have when the program's first
/// instruction runs.
///
/// Sixteen on every architecture Ferrix targets. ARM's own EABI would settle
/// for eight, but Linux's `STACK_ROUND` rounds to sixteen on all of them and a
/// program entitled to assume what Linux does is a program that assumes this.
pub const STACK_ALIGN: u64 = 16;

/// The longest single argument or environment string, Linux's `MAX_ARG_STRLEN`.
///
/// 32 pages. A longer one is `E2BIG`, which is worth enforcing here rather than
/// discovering as a stack that did not fit.
pub const MAX_ARG_STRLEN: usize = 32 * 4096;

/// The width of a user pointer on the architecture the image is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// 32-bit, which for Ferrix means ARMv7-A.
    Bits32,
    /// 64-bit: x86-64 and AArch64.
    Bits64,
}

impl Width {
    /// The size of one stack word in bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Bits32 => 4,
            Self::Bits64 => 8,
        }
    }
}

/// What the program should find on its stack.
///
/// Strings are given without their terminating NUL; this crate adds it. They
/// are byte slices rather than `str` because a Linux argument is bytes: a path
/// that is not valid UTF-8 is a path a program is still entitled to be handed.
#[derive(Debug, Clone, Copy)]
pub struct Spec<'a> {
    /// The argument vector. `args[0]` is conventionally the program name, but
    /// nothing here enforces that, because `execve` does not either.
    pub args: &'a [&'a [u8]],
    /// The environment, each entry conventionally `KEY=value`.
    pub env: &'a [&'a [u8]],
    /// Auxiliary vector entries, as (key, value).
    ///
    /// Must not contain [`AT_NULL`], which terminates the vector, nor any of
    /// the three keys this crate fills in itself.
    pub auxv: &'a [(u64, u64)],
    /// The bytes `AT_RANDOM` will point at.
    pub random: [u8; RANDOM_BYTES],
    /// The string `AT_EXECFN` will point at: the path `execve` was given.
    pub exec_fn: &'a [u8],
    /// The string `AT_PLATFORM` will point at, if the architecture has one.
    pub platform: Option<&'a [u8]>,
    /// The pointer width of the program being started.
    pub width: Width,
}

/// Where everything landed.
///
/// The four string bounds are not decoration: `/proc/self/cmdline` and
/// `/proc/self/environ` are defined as the bytes between them, so stage 8 needs
/// the numbers this pass already computed rather than a second walk that could
/// disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    /// The stack pointer the program must start with. Always [`STACK_ALIGN`]
    /// aligned.
    pub sp: u64,
    /// The first byte of the first argument string.
    pub arg_start: u64,
    /// One past the NUL of the last argument string.
    pub arg_end: u64,
    /// The first byte of the first environment string.
    pub env_start: u64,
    /// One past the NUL of the last environment string.
    pub env_end: u64,
}

/// Why an image could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StackError {
    /// The buffer is smaller than the image needs. `E2BIG`.
    TooSmall,
    /// A single string exceeds [`MAX_ARG_STRLEN`]. `E2BIG`.
    StringTooLong,
    /// A string contains a NUL byte, so it cannot be written as a C string.
    /// `EINVAL`.
    ///
    /// Not a pedantic check. Everything on this stack is NUL-terminated, so a
    /// string with a NUL inside it is silently *truncated at that byte* when
    /// the program reads it back: the caller asks for one argument and the
    /// program receives a shorter, different one. `execve` cannot produce such
    /// a string -- it copies NUL-terminated strings out of user memory, so the
    /// string ends at the first NUL by construction -- which means one arriving
    /// here is a bug above this crate, and passing it on would hide it.
    EmbeddedNul,
    /// The caller passed an auxiliary key this crate fills in itself, or
    /// [`AT_NULL`]. `EINVAL`.
    ReservedAuxKey(
        /// The offending key.
        u64,
    ),
    /// A value does not fit in a pointer of the requested [`Width`]. `EINVAL`.
    ValueTooWide(
        /// The offending value.
        u64,
    ),
    /// The stack top is not [`STACK_ALIGN`] aligned. `EINVAL`.
    UnalignedTop,
    /// The arithmetic of laying the image out left the address space. `EINVAL`.
    Overflow,
}

/// Keys this crate supplies, and therefore refuses from the caller.
const RESERVED: [u64; 4] = [AT_NULL, AT_RANDOM, AT_EXECFN, AT_PLATFORM];

/// Build the image into `buf`, which is the memory mapped directly below
/// `stack_top`.
///
/// `buf` is taken to occupy `stack_top - buf.len() .. stack_top`; that is the
/// whole of the contract, and it is what lets this run on the host against a
/// plain array while the kernel passes it a mapped page. The image is written
/// into the tail of the buffer and everything below the returned
/// [`Image::sp`] is left untouched.
///
/// # Errors
///
/// See [`StackError`]. Nothing here panics, allocates or indexes unchecked.
///
/// On error the buffer may hold part of an image: the strings are placed
/// before the layout of the vectors is known, so "it did not fit" is
/// discovered after some bytes have been written. That is deliberate rather
/// than overlooked — the caller is building a fresh stack mapping and throws
/// the whole thing away — but it does mean a failed `build` must not be
/// retried into the same buffer and the result trusted.
pub fn build(spec: &Spec<'_>, stack_top: u64, buf: &mut [u8]) -> Result<Image, StackError> {
    if !stack_top.is_multiple_of(STACK_ALIGN) {
        return Err(StackError::UnalignedTop);
    }
    for &(key, _) in spec.auxv {
        if RESERVED.contains(&key) {
            return Err(StackError::ReservedAuxKey(key));
        }
    }
    let len = u64::try_from(buf.len()).map_err(|_| StackError::Overflow)?;
    let base = stack_top.checked_sub(len).ok_or(StackError::Overflow)?;
    let mut writer = Writer { buf, base };

    let placed = place_strings(spec, stack_top, &mut writer)?;
    let sp = vector_base(spec, placed.bottom)?;
    if sp < base {
        return Err(StackError::TooSmall);
    }
    write_vectors(spec, sp, &placed, &mut writer)?;

    Ok(Image {
        sp,
        arg_start: placed.arg_start,
        arg_end: placed.arg_end,
        env_start: placed.env_start,
        env_end: placed.env_end,
    })
}

/// The addresses the string pass decided on.
#[derive(Debug, Clone, Copy)]
struct Placed {
    arg_start: u64,
    arg_end: u64,
    env_start: u64,
    env_end: u64,
    random: u64,
    exec_fn: u64,
    platform: Option<u64>,
    /// The lowest address the strings occupy; the vectors go below it.
    bottom: u64,
}

/// Check a string may be written as a C string, and say how long it is with
/// its terminator.
///
/// Every string that reaches the image goes through here, which is the point:
/// the argument vector, the environment, `AT_EXECFN` and `AT_PLATFORM` are all
/// read back by the same NUL scan, so they all have the same two limits.
fn checked_len(s: &[u8]) -> Result<u64, StackError> {
    if s.len() > MAX_ARG_STRLEN {
        return Err(StackError::StringTooLong);
    }
    if s.contains(&0) {
        return Err(StackError::EmbeddedNul);
    }
    u64::try_from(s.len())
        .map_err(|_| StackError::Overflow)?
        .checked_add(1)
        .ok_or(StackError::Overflow)
}

/// Total bytes a list of strings occupies, NUL terminators included.
fn blob_len(strings: &[&[u8]]) -> Result<u64, StackError> {
    let mut total = 0_u64;
    for s in strings {
        let with_nul = checked_len(s)?;
        total = total.checked_add(with_nul).ok_or(StackError::Overflow)?;
    }
    Ok(total)
}

/// Lay the strings out downward from the top of the stack, and write them.
///
/// Order matters in exactly one respect: the argument strings must be one
/// contiguous run and the environment strings another directly above it,
/// because that is what `/proc/self/cmdline` is later defined as reading.
fn place_strings(
    spec: &Spec<'_>,
    stack_top: u64,
    writer: &mut Writer<'_>,
) -> Result<Placed, StackError> {
    let mut p = stack_top;

    p = reserve_string(p, spec.exec_fn)?;
    let exec_fn = p;
    writer.put_cstr(exec_fn, spec.exec_fn)?;

    let platform = match spec.platform {
        Some(text) => {
            p = reserve_string(p, text)?;
            writer.put_cstr(p, text)?;
            Some(p)
        }
        None => None,
    };

    p = reserve(p, RANDOM_BYTES_U64)?;
    let random = p;
    writer.put(random, &spec.random)?;

    let env_end = p;
    let env_start = p
        .checked_sub(blob_len(spec.env)?)
        .ok_or(StackError::Overflow)?;
    writer.put_blob(env_start, spec.env)?;

    let arg_end = env_start;
    let arg_start = arg_end
        .checked_sub(blob_len(spec.args)?)
        .ok_or(StackError::Overflow)?;
    writer.put_blob(arg_start, spec.args)?;

    Ok(Placed {
        arg_start,
        arg_end,
        env_start,
        env_end,
        random,
        exec_fn,
        platform,
        bottom: arg_start,
    })
}

/// Move an address down by enough room for a validated C string.
fn reserve_string(p: u64, s: &[u8]) -> Result<u64, StackError> {
    let len = checked_len(s)?;
    p.checked_sub(len).ok_or(StackError::Overflow)
}

/// Move an address down by a fixed number of bytes.
fn reserve(p: u64, len: u64) -> Result<u64, StackError> {
    p.checked_sub(len).ok_or(StackError::Overflow)
}

/// Where the stack pointer must sit for the vectors to end below the strings.
///
/// The rounding down to [`STACK_ALIGN`] is the whole reason this is computed
/// rather than accumulated: the vectors are written *upward* from the aligned
/// stack pointer, so the alignment has to be decided before the first word is
/// placed. Rounding afterwards would move every pointer already written.
fn vector_base(spec: &Spec<'_>, strings_bottom: u64) -> Result<u64, StackError> {
    let auxv_entries = u64::try_from(spec.auxv.len())
        .map_err(|_| StackError::Overflow)?
        .checked_add(supplied_aux_count(spec))
        .ok_or(StackError::Overflow)?;
    let argc = u64::try_from(spec.args.len()).map_err(|_| StackError::Overflow)?;
    let envc = u64::try_from(spec.env.len()).map_err(|_| StackError::Overflow)?;

    // argc, argv[] and its null, envp[] and its null, then two words per
    // auxiliary entry including the AT_NULL that ends it.
    let words = [1, argc, 1, envc, 1]
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
        .and_then(|a| auxv_entries.checked_add(1)?.checked_mul(2)?.checked_add(a))
        .ok_or(StackError::Overflow)?;

    let bytes = words
        .checked_mul(spec.width.bytes())
        .ok_or(StackError::Overflow)?;
    let unaligned = strings_bottom
        .checked_sub(bytes)
        .ok_or(StackError::Overflow)?;
    Ok(unaligned & !(STACK_ALIGN - 1))
}

/// How many auxiliary entries this crate adds on top of the caller's.
fn supplied_aux_count(spec: &Spec<'_>) -> u64 {
    // AT_RANDOM and AT_EXECFN always; AT_PLATFORM only if there is one.
    if spec.platform.is_some() { 3 } else { 2 }
}

/// Write `argc`, the two pointer vectors and the auxiliary vector, upward.
fn write_vectors(
    spec: &Spec<'_>,
    sp: u64,
    placed: &Placed,
    writer: &mut Writer<'_>,
) -> Result<(), StackError> {
    let width = spec.width;
    let mut p = Cursor { at: sp, width };

    let argc = u64::try_from(spec.args.len()).map_err(|_| StackError::Overflow)?;
    p.word(writer, argc)?;
    p.pointers(writer, placed.arg_start, spec.args)?;
    p.word(writer, 0)?;
    p.pointers(writer, placed.env_start, spec.env)?;
    p.word(writer, 0)?;

    for &(key, value) in spec.auxv {
        p.word(writer, key)?;
        p.word(writer, value)?;
    }
    p.word(writer, AT_RANDOM)?;
    p.word(writer, placed.random)?;
    p.word(writer, AT_EXECFN)?;
    p.word(writer, placed.exec_fn)?;
    if let Some(addr) = placed.platform {
        p.word(writer, AT_PLATFORM)?;
        p.word(writer, addr)?;
    }
    p.word(writer, AT_NULL)?;
    p.word(writer, 0)?;
    Ok(())
}

/// An address walking upward, one stack word at a time.
#[derive(Debug)]
struct Cursor {
    at: u64,
    width: Width,
}

impl Cursor {
    /// Write one word and advance.
    fn word(&mut self, writer: &mut Writer<'_>, value: u64) -> Result<(), StackError> {
        writer.put_word(self.at, value, self.width)?;
        self.at = self
            .at
            .checked_add(self.width.bytes())
            .ok_or(StackError::Overflow)?;
        Ok(())
    }

    /// Write one pointer per string, each to where that string was placed.
    ///
    /// The addresses are re-derived from the blob's start rather than recorded
    /// during the string pass, so that the two passes cannot drift apart: if
    /// they disagreed, every pointer after the disagreement would be wrong and
    /// the program would see a shifted environment.
    fn pointers(
        &mut self,
        writer: &mut Writer<'_>,
        blob_start: u64,
        strings: &[&[u8]],
    ) -> Result<(), StackError> {
        let mut at = blob_start;
        for s in strings {
            self.word(writer, at)?;
            let step = u64::try_from(s.len())
                .map_err(|_| StackError::Overflow)?
                .checked_add(1)
                .ok_or(StackError::Overflow)?;
            at = at.checked_add(step).ok_or(StackError::Overflow)?;
        }
        Ok(())
    }
}

/// A buffer that knows the virtual address its first byte has.
#[derive(Debug)]
struct Writer<'b> {
    buf: &'b mut [u8],
    base: u64,
}

impl Writer<'_> {
    /// Copy bytes to a virtual address inside the buffer.
    fn put(&mut self, addr: u64, bytes: &[u8]) -> Result<(), StackError> {
        let offset = addr.checked_sub(self.base).ok_or(StackError::TooSmall)?;
        let offset = usize::try_from(offset).map_err(|_| StackError::Overflow)?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or(StackError::Overflow)?;
        self.buf
            .get_mut(offset..end)
            .ok_or(StackError::TooSmall)?
            .copy_from_slice(bytes);
        Ok(())
    }

    /// Copy a string and its terminating NUL.
    fn put_cstr(&mut self, addr: u64, bytes: &[u8]) -> Result<(), StackError> {
        self.put(addr, bytes)?;
        let nul = addr
            .checked_add(u64::try_from(bytes.len()).map_err(|_| StackError::Overflow)?)
            .ok_or(StackError::Overflow)?;
        self.put(nul, &[0])
    }

    /// Copy a run of strings upward from `start`, each NUL-terminated.
    fn put_blob(&mut self, start: u64, strings: &[&[u8]]) -> Result<(), StackError> {
        let mut at = start;
        for s in strings {
            self.put_cstr(at, s)?;
            let step = u64::try_from(s.len())
                .map_err(|_| StackError::Overflow)?
                .checked_add(1)
                .ok_or(StackError::Overflow)?;
            at = at.checked_add(step).ok_or(StackError::Overflow)?;
        }
        Ok(())
    }

    /// Write one stack word, refusing a value the width cannot hold.
    ///
    /// Little-endian on all three targets, so there is no byte order to carry.
    /// The refusal matters on ARMv7-A: a truncated `AT_PHDR` is a pointer into
    /// the wrong page rather than an error, and musl would follow it.
    fn put_word(&mut self, addr: u64, value: u64, width: Width) -> Result<(), StackError> {
        match width {
            Width::Bits32 => {
                let narrow = u32::try_from(value).map_err(|_| StackError::ValueTooWide(value))?;
                self.put(addr, &narrow.to_le_bytes())
            }
            Width::Bits64 => self.put(addr, &value.to_le_bytes()),
        }
    }
}

pub mod read;

#[cfg(test)]
mod tests;
