//! Which glibc the unit tests' comparisons with the host were recorded on.
//!
//! Some unit tests check this library against the glibc the test binary links:
//! `ctype.h`'s tables, every code point's `wctype.h` class, case and width
//! against a recorded list of differences, and `printf`'s output. glibc's
//! Unicode data changes between releases, so on another glibc those tests
//! fail without anything being wrong here.
//!
//! They call [`is_recorded`] first and return early on another glibc, after
//! one note on standard error. Every test that does not compare with the host
//! runs everywhere. A CI job on a different glibc therefore still runs them
//! all but these; one that wants these too runs on [`RECORDED_WITH`].

use core::ffi::{CStr, c_char};
use std::io::Write;
use std::sync::Once;

/// The host glibc the recorded data and the comparisons were made with. A
/// change of host means regenerating `src/wctype/glibc-differences.txt`, as
/// `tools/unicode-differences.py` says, and then changing this.
pub(crate) const RECORDED_WITH: &str = "2.43";

unsafe extern "C" {
    safe fn gnu_get_libc_version() -> *const c_char;
}

/// The host glibc's version, such as `2.43`.
pub(crate) fn host_version() -> &'static str {
    // SAFETY: glibc returns a static NUL-terminated string.
    unsafe { CStr::from_ptr(gnu_get_libc_version()) }
        .to_str()
        .unwrap_or("unknown")
}

/// Whether the host glibc is [`RECORDED_WITH`]. When it is not, says so once
/// on standard error, which `cargo test` does not capture when written
/// directly.
pub(crate) fn is_recorded() -> bool {
    let host = host_version();
    if host == RECORDED_WITH {
        return true;
    }
    static NOTE: Once = Once::new();
    NOTE.call_once(|| {
        let _ = writeln!(
            std::io::stderr(),
            "note: the host glibc is {host}, not {RECORDED_WITH}; the tests comparing \
             with it are skipped (src/host_glibc.rs)"
        );
    });
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_version_reads_as_a_version() {
        let version = host_version();
        assert!(
            version.split('.').all(|part| part.parse::<u32>().is_ok()),
            "{version}"
        );
    }
}
