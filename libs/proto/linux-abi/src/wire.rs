//! Fixed-width fields read from and written into byte buffers, answering
//! `None` for an offset past the end rather than panicking.
//!
//! [`crate::inet`] and [`crate::netlink`] read their structures through these,
//! so the offset arithmetic is checked in one place.

/// The `N` bytes at `at`, if they are all there.
pub(crate) fn array<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// Copy `field` into `out` at `at`, or nothing if it does not fit.
pub(crate) fn put(out: &mut [u8], at: usize, field: &[u8]) -> Option<()> {
    out.get_mut(at..at.checked_add(field.len())?)?
        .copy_from_slice(field);
    Some(())
}
