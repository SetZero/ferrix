//! Walking a built image the way a program's `_start` walks it.
//!
//! This is not a convenience. It is the oracle the builder is checked against:
//! a test that asserts the builder wrote the bytes the builder intended to
//! write is a test that cannot fail, whereas reading the image back out with
//! nothing but a stack pointer and the ABI's own rules — one word for `argc`,
//! then pointers until a null, then pointers until a null, then pairs until
//! `AT_NULL` — checks the thing that actually matters. The fuzzer uses it for
//! the same reason.
//!
//! Stage 8 will want it too: `/proc/self/auxv` is defined as the bytes of the
//! auxiliary vector, and a reader that already agrees with the writer is
//! better than a second walk that might not.
//!
//! # Order matters
//!
//! The walk is sequential and stateful, exactly as `_start`'s is, because the
//! layout is only self-describing when read in order: nothing in the image
//! says where the environment begins except the null that ends the arguments.
//! Call [`Walk::argc`], then [`Walk::next_arg`] until it yields `None`, then
//! [`Walk::next_env`] until `None`, then [`Walk::next_aux`] until `None`.

use crate::Width;

/// Why an image could not be walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadError {
    /// A word or string lies outside the buffer.
    OutOfBounds(
        /// The address that could not be read.
        u64,
    ),
    /// A string ran to the end of the buffer without a NUL.
    Unterminated(
        /// Where the string started.
        u64,
    ),
    /// The arithmetic of the walk left the address space.
    Overflow,
}

/// A cursor over a built image.
#[derive(Debug)]
pub struct Walk<'a> {
    buf: &'a [u8],
    base: u64,
    width: Width,
    at: u64,
}

impl<'a> Walk<'a> {
    /// Start a walk at `sp`, over a buffer whose first byte is at `base`.
    #[must_use]
    pub const fn new(buf: &'a [u8], base: u64, sp: u64, width: Width) -> Self {
        Self {
            buf,
            base,
            width,
            at: sp,
        }
    }

    /// The bytes of a NUL-terminated string at a virtual address.
    ///
    /// The NUL itself is not included, which is what makes the result
    /// comparable with what the caller put in.
    ///
    /// # Errors
    ///
    /// [`ReadError::OutOfBounds`] if the address is not in the buffer, or
    /// [`ReadError::Unterminated`] if no NUL follows it.
    pub fn cstr_at(&self, addr: u64) -> Result<&'a [u8], ReadError> {
        let offset = self.offset(addr)?;
        let rest = self.buf.get(offset..).ok_or(ReadError::OutOfBounds(addr))?;
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(ReadError::Unterminated(addr))?;
        rest.get(..len).ok_or(ReadError::Unterminated(addr))
    }

    /// Read `argc`. Call once, first.
    ///
    /// # Errors
    ///
    /// See [`ReadError`].
    pub fn argc(&mut self) -> Result<u64, ReadError> {
        self.word()
    }

    /// The next argument, or `None` at the null that ends the vector.
    ///
    /// # Errors
    ///
    /// See [`ReadError`].
    pub fn next_arg(&mut self) -> Result<Option<&'a [u8]>, ReadError> {
        self.next_string()
    }

    /// The next environment entry, or `None` at the null that ends the vector.
    ///
    /// # Errors
    ///
    /// See [`ReadError`].
    pub fn next_env(&mut self) -> Result<Option<&'a [u8]>, ReadError> {
        self.next_string()
    }

    /// The next auxiliary entry, or `None` at `AT_NULL`.
    ///
    /// # Errors
    ///
    /// See [`ReadError`].
    pub fn next_aux(&mut self) -> Result<Option<(u64, u64)>, ReadError> {
        let key = self.word()?;
        let value = self.word()?;
        if key == ferrix_linux_abi::types::AT_NULL {
            return Ok(None);
        }
        Ok(Some((key, value)))
    }

    /// The address the walk has reached, for a test that wants to check it.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.at
    }

    /// One pointer-sized word, advancing the cursor.
    fn word(&mut self) -> Result<u64, ReadError> {
        let offset = self.offset(self.at)?;
        let size = usize::try_from(self.width.bytes()).map_err(|_| ReadError::Overflow)?;
        let end = offset.checked_add(size).ok_or(ReadError::Overflow)?;
        let bytes = self
            .buf
            .get(offset..end)
            .ok_or(ReadError::OutOfBounds(self.at))?;
        let value = read_le(bytes);
        self.at = self
            .at
            .checked_add(self.width.bytes())
            .ok_or(ReadError::Overflow)?;
        Ok(value)
    }

    /// A pointer word, resolved to the string it names; `None` for a null.
    fn next_string(&mut self) -> Result<Option<&'a [u8]>, ReadError> {
        let pointer = self.word()?;
        if pointer == 0 {
            return Ok(None);
        }
        self.cstr_at(pointer).map(Some)
    }

    /// A virtual address as an index into the buffer.
    fn offset(&self, addr: u64) -> Result<usize, ReadError> {
        let delta = addr
            .checked_sub(self.base)
            .ok_or(ReadError::OutOfBounds(addr))?;
        usize::try_from(delta).map_err(|_| ReadError::Overflow)
    }
}

/// A little-endian word from four or eight bytes.
///
/// Written as a fold rather than `from_le_bytes` so that one function serves
/// both widths without an unchecked array conversion.
fn read_le(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0_u64, |acc, &b| (acc << 8) | u64::from(b))
}
