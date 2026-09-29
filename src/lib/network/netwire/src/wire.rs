//! Big-endian field access that answers an out-of-range offset with `None`.
//!
//! Every module reads and writes through these, so there is one place where an
//! offset plus a width is checked for overflow and against the slice, and no
//! caller indexes a slice directly.

/// The big-endian `u16` at `at`, if two bytes are there.
pub(crate) fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    array(bytes, at).map(u16::from_be_bytes)
}

/// The big-endian `u32` at `at`, if four bytes are there.
pub(crate) fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    array(bytes, at).map(u32::from_be_bytes)
}

/// The `N` bytes at `at`, if they are there.
pub(crate) fn array<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    let end = at.checked_add(N)?;
    bytes.get(at..end)?.try_into().ok()
}

/// The byte at `at`, if it is there.
pub(crate) fn byte(bytes: &[u8], at: usize) -> Option<u8> {
    bytes.get(at).copied()
}

/// Copy `field` into `out` at `at`, if it fits.
pub(crate) fn put(out: &mut [u8], at: usize, field: &[u8]) -> Option<()> {
    let end = at.checked_add(field.len())?;
    out.get_mut(at..end)?.copy_from_slice(field);
    Some(())
}
