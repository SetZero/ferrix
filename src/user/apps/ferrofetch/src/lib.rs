//! What `ferrofetch` shows, as pure functions of the text it reads.
//!
//! `ferrofetch` is Ferrix's fastfetch: the mark beside a few lines about the
//! machine, the shell and the session, printed once. The program, `main.rs`
//! beside this, is a native one: it opens the files and asks `uname`, and
//! nothing else. Everything between the bytes it reads and the bytes it
//! writes is here, in the lib target, which builds on the host and so is
//! where `cargo test` reaches it.
//!
//! Nothing here allocates, because a native program has no heap: a value
//! that outlives the buffer it was read into is copied into a [`Text`].
//!
//! # What is here
//!
//! * [`parse`] -- `/proc/<pid>/stat`'s parent, `status`'s uid, `/etc/passwd`,
//!   `/proc/uptime`, `/proc/meminfo`, `/proc/loadavg`, `/proc/cpuinfo`, a
//!   DRM connector's `modes`, `getdents64`'s records and the command line.
//! * [`render`] -- the [`Facts`] and how each is written.
//! * [`logo`] -- the mark, in text.
//!
//! [`Facts`]: render::Facts

#![no_std]

pub mod logo;
pub mod parse;
pub mod render;

#[cfg(test)]
mod tests;

use core::fmt;

/// Up to `N` bytes of UTF-8 text, owned: a name read out of a buffer that is
/// about to be reused.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Text<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Text<N> {
    /// No text.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    /// `text`, cut at the last whole character that fits.
    #[must_use]
    pub fn from(text: &str) -> Self {
        let mut out = Self::new();
        out.push(text);
        out
    }

    /// Add `text`, as much of it as fits, never splitting a character.
    pub fn push(&mut self, text: &str) {
        let room = N.saturating_sub(self.len);
        let mut take = text.len().min(room);
        while !text.is_char_boundary(take) {
            take = take.saturating_sub(1);
        }
        let (Some(to), Some(from)) = (
            self.bytes.get_mut(self.len..self.len + take),
            text.as_bytes().get(..take),
        ) else {
            return;
        };
        to.copy_from_slice(from);
        self.len += take;
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Only whole characters of a `&str` are ever pushed, so this is
        // always UTF-8; the fallback is for a bug, not for a case.
        self.bytes
            .get(..self.len)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }

    /// Whether there is no text.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Debug for Text<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<const N: usize> fmt::Display for Text<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<const N: usize> fmt::Write for Text<N> {
    /// Writes what fits, and fails if that was not all of it: a path cut
    /// short could name another file, so a caller building one must know.
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let before = self.len;
        self.push(text);
        if self.len - before == text.len() {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}
