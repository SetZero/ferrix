//! A secret a person typed, held so that it leaves as little behind as a
//! program can arrange (`docs/AUTH.md` §3.8).

use crate::MAX_SECRET;

/// A password, a one-time code or any other answer to a secret PROMPT.
///
/// A fixed buffer, so growing it never leaves a copy behind in memory it
/// gave back, as a `Vec` would. Never `Clone` or `Copy`, so it is not
/// duplicated by accident, and `Debug` shows only that it is one. Zeroed
/// when dropped, with the writes kept from being optimised away by passing
/// the buffer through `black_box` after them.
pub struct Secret {
    bytes: [u8; MAX_SECRET],
    len: usize,
}

impl Secret {
    /// An empty secret.
    #[must_use]
    pub const fn new() -> Secret {
        Secret {
            bytes: [0; MAX_SECRET],
            len: 0,
        }
    }

    /// A secret holding `bytes`, or `None` if they are longer than
    /// [`MAX_SECRET`]. The caller zeroes where they came from.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Secret> {
        let mut secret = Secret::new();
        secret.bytes.get_mut(..bytes.len())?.copy_from_slice(bytes);
        secret.len = bytes.len();
        Some(secret)
    }

    /// Add one byte, as a person types it; false, and nothing added, when
    /// it is full.
    pub fn push(&mut self, byte: u8) -> bool {
        match self.bytes.get_mut(self.len) {
            Some(slot) => {
                *slot = byte;
                self.len += 1;
                true
            }
            None => false,
        }
    }

    /// Take the last byte off, as a backspace does.
    pub fn pop(&mut self) {
        if let Some(last) = self.len.checked_sub(1) {
            if let Some(slot) = self.bytes.get_mut(last) {
                *slot = 0;
            }
            self.len = last;
        }
    }

    /// Forget every byte.
    pub fn clear(&mut self) {
        self.bytes.fill(0);
        self.len = 0;
        let _ = core::hint::black_box(&self.bytes);
    }

    /// The bytes, to hash or to send.
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or(&[])
    }

    /// How many bytes it holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it holds none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for Secret {
    fn default() -> Secret {
        Secret::new()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.clear();
    }
}

impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(..)")
    }
}
