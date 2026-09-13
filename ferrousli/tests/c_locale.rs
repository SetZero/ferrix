//! The C programs that exercise `locale.h` and `langinfo.h`, and the
//! multibyte and wide character conversions.
//!
//! Each is built with `-fno-builtin`, so the compiler cannot answer a call on
//! constant arguments itself, and every check reaches the library. Several are
//! adapted from libc-test, and say so.
//!
//! The harness is in `tests/common`.

mod common;

use common::{Case, check};

/// Keeps the compiler from folding calls to the functions under test.
const NO_BUILTIN: &[&str] = &["-fno-builtin"];

/// A program in `tests/c/locale`, built with [`NO_BUILTIN`].
const fn locale(name: &'static str) -> Case {
    Case {
        cflags: NO_BUILTIN,
        ..Case::named(name)
    }
}

/// Runs `locale/environment` in `env`, where `setlocale(LC_ALL, "")` must
/// return `name` and `MB_CUR_MAX` must then be `max`.
fn environment(
    env: &'static [(&'static str, &'static str)],
    name: &'static str,
    max: &'static str,
) {
    check(&Case {
        args: Box::leak(Box::new([name, max])),
        env,
        ..locale("locale/environment")
    });
}

#[test]
fn setlocale_names_composites_and_langinfo() {
    check(&locale("locale/setlocale"));
}

#[test]
fn an_empty_name_reads_lc_all_then_the_categorys_variable_then_lang() {
    environment(&[], "C.UTF-8;C;C;C;C;C", "4");
    environment(&[("LANG", "C")], "C", "1");
    environment(&[("LANG", "en_US.UTF-8")], "en_US.UTF-8", "4");
    environment(
        &[("LANG", "C"), ("LC_CTYPE", "C.UTF-8")],
        "C.UTF-8;C;C;C;C;C",
        "4",
    );
    environment(
        &[
            ("LC_ALL", "POSIX"),
            ("LC_CTYPE", "en_US.UTF-8"),
            ("LANG", "de_DE.UTF-8"),
        ],
        "C",
        "1",
    );
    environment(&[("LC_ALL", ""), ("LANG", "POSIX")], "C", "1");
    environment(
        &[("LANG", "de_DE.UTF-8"), ("LC_MESSAGES", "C")],
        "de_DE.UTF-8;de_DE.UTF-8;de_DE.UTF-8;de_DE.UTF-8;de_DE.UTF-8;C",
        "4",
    );
    environment(&[("LANG", "../x")], "C.UTF-8;C;C;C;C;C", "4");
    environment(&[("LANG", "C"), ("LC_NUMERIC", "C.UTF-8")], "C", "1");
}

#[test]
fn newlocale_duplocale_freelocale_and_uselocale() {
    check(&locale("locale/newlocale"));
}

#[test]
fn multibyte_conversions_in_c_and_utf8() {
    check(&locale("locale/multibyte"));
}

#[test]
fn char16_and_char32_conversions() {
    check(&locale("locale/uchar"));
}

#[test]
fn libc_test_mbc() {
    check(&locale("locale/mbc"));
}
