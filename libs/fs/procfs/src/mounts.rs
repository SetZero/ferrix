//! `/proc/mounts`.
//!
//! ```text
//! proc /proc proc rw 0 0
//! ```
//!
//! Six fields separated by single spaces, which is exactly why the first three
//! are escaped: `show_vfsmnt` passes the source, the mount point and the type
//! through `mangle`, which writes a space, tab, newline or backslash as a
//! backslash and three octal digits. `getmntent` undoes it. A mount point with
//! a space in its name printed raw would be read as two fields and every field
//! after it shifted by one.

use alloc::vec::Vec;

use crate::text::put;

/// One mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mount<'a> {
    /// What was mounted: a device path, or the filesystem's name.
    pub source: &'a [u8],
    /// Where, as a path from the reader's root.
    pub point: &'a [u8],
    /// The filesystem type.
    pub fstype: &'a [u8],
    /// The comma-separated options, printed as given.
    pub options: &'a [u8],
}

/// Append one line, newline included.
///
/// The last two fields are the `dump` and `fsck` pass numbers of `fstab`,
/// which Linux always prints as zero.
pub fn render(out: &mut Vec<u8>, mount: &Mount<'_>) {
    mangle(out, mount.source);
    out.push(b' ');
    mangle(out, mount.point);
    out.push(b' ');
    mangle(out, mount.fstype);
    out.push(b' ');
    out.extend_from_slice(mount.options);
    out.extend_from_slice(b" 0 0\n");
}

/// `mangle(m, s)`, escaping `" \t\n\\"`.
fn mangle(out: &mut Vec<u8>, field: &[u8]) {
    for &byte in field {
        if matches!(byte, b' ' | b'\t' | b'\n' | b'\\') {
            put(out, format_args!("\\{byte:03o}"));
        } else {
            out.push(byte);
        }
    }
}
