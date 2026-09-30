//! glibc's double-underscore names for functions this library has under the
//! plain name: the locale functions GCC's C++ runtime calls by glibc's
//! internal names -- LLVM and `rustc`'s driver import them through it --
//! `__pthread_key_create`, and the names the Steam client's 64-bit side
//! imports: `__strdup` and `__strtok_r` (SDL, the Steam Runtime's tools),
//! `_IO_putc` (lsof), `prlimit64` (the Runtime's tools; `rlim_t` is 64 bits
//! here on every architecture, so it is `prlimit`), and Chromium's
//! `__libc_malloc` family, which its allocator shim calls through. Python
//! imports `__sysconf` and `preadv64v2` and `pwritev64v2`; `off_t` is 64
//! bits here too, so the `64` names are the plain ones. Berkeley DB, in the
//! Steam Runtime, imports `pthread_yield`, which glibc 2.34 retired in
//! favour of `sched_yield` and still answers as it.
//!
//! The Steam client's FFmpeg, libvorbis and libvpx import libm's `_finite`
//! names -- `__exp_finite`, `__pow_finite` and the rest -- which glibc's
//! headers before 2.31 made `-ffinite-math-only` code call. glibc keeps
//! them for such programs only, at `GLIBC_2.15`, and each is the function
//! of the plain name. They are here under their own names, and on x86-64
//! and ARMv7-A under `name@GLIBC_2.15` too (see [`versioned`]); AArch64's
//! glibc has them at its first version, which is where a name with no
//! version of its own goes.
//!
//! Each takes the same arguments as the plain function and does the same
//! thing, so each is a branch to it, as [`crate::arm_names`] does for
//! ARMv7-A's time64 names, rather than a Rust wrapper that would only pass
//! its arguments on.

/// Writes one `global_asm!` of branches, one per `alias => target` pair, for
/// the architecture's branch instruction and symbol-type syntax. `weak`
/// before the pairs makes each name weak, for a function a program may bring
/// its own copy of ([`crate::obstack`]).
macro_rules! aliases {
    (weak $($alias:literal => $target:literal),* $(,)?) => {
        aliases!(@write ".weak ", $($alias => $target),*);
    };
    ($($alias:literal => $target:literal),* $(,)?) => {
        aliases!(@write ".globl ", $($alias => $target),*);
    };
    (@write $binding:literal, $($alias:literal => $target:literal),*) => {
        #[cfg(all(not(test), target_arch = "x86_64"))]
        core::arch::global_asm!(
            ".pushsection .text.ferrousli_glibc_aliases,\"ax\",@progbits",
            ".p2align 4",
            $(
                concat!($binding, $alias),
                concat!(".type ", $alias, ", @function"),
                concat!($alias, ":"),
                concat!("jmp ", $target),
            )*
            ".popsection",
        );
        #[cfg(all(not(test), target_arch = "aarch64"))]
        core::arch::global_asm!(
            ".pushsection .text.ferrousli_glibc_aliases,\"ax\",%progbits",
            ".p2align 2",
            $(
                concat!($binding, $alias),
                concat!(".type ", $alias, ", %function"),
                concat!($alias, ":"),
                concat!("b ", $target),
            )*
            ".popsection",
        );
        #[cfg(all(not(test), target_arch = "arm"))]
        core::arch::global_asm!(
            ".pushsection .text.ferrousli_glibc_aliases,\"ax\",%progbits",
            ".p2align 2",
            ".arm",
            $(
                concat!($binding, $alias),
                concat!(".type ", $alias, ", %function"),
                concat!($alias, ":"),
                concat!("b ", $target),
            )*
            ".popsection",
        );
    };
}

aliases! {
    "_IO_putc" => "putc",
    "__duplocale" => "duplocale",
    "__freelocale" => "freelocale",
    "__iswctype_l" => "iswctype_l",
    "__libc_calloc" => "calloc",
    "__libc_free" => "free",
    "__libc_malloc" => "malloc",
    "__libc_memalign" => "memalign",
    "__libc_realloc" => "realloc",
    "__newlocale" => "newlocale",
    "__nl_langinfo_l" => "nl_langinfo_l",
    "__pthread_key_create" => "pthread_key_create",
    "__strcoll_l" => "strcoll_l",
    "__strdup" => "strdup",
    "__strftime_l" => "strftime_l",
    "__strtod_l" => "strtod_l",
    "__strtof_l" => "strtof_l",
    "__strtok_r" => "strtok_r",
    "__strxfrm_l" => "strxfrm_l",
    "__sysconf" => "sysconf",
    "__towlower_l" => "towlower_l",
    "__towupper_l" => "towupper_l",
    "__uselocale" => "uselocale",
    "__wcscoll_l" => "wcscoll_l",
    "__wcsftime_l" => "wcsftime_l",
    "__wcsxfrm_l" => "wcsxfrm_l",
    "__wctype_l" => "wctype_l",
    "preadv64" => "preadv",
    "preadv64v2" => "preadv2",
    "prlimit64" => "prlimit",
    "pthread_yield" => "sched_yield",
    "pwritev64" => "pwritev",
    "pwritev64v2" => "pwritev2",
}

// The names the 64-bit half of the Steam Runtime's scout libraries import,
// built against glibc 2.15: `_IO_getc` (GLib, libX11, libasound, zenity's
// gdk-pixbuf), `__strndup` and `__strsep_g`, the inline forms glibc's
// `<bits/string2.h>` called, `__secure_getenv` (OpenSSL, libudev), which
// glibc 2.17 renamed, the resolver's `__res_query` family and
// `__dn_expand` (GIO, OpenLDAP, Heimdal), `__poll` and `__dcgettext`, and
// `strtoq` and `strtouq`, BSD's names for `strtoll` and `strtoull` (PCRE).
// `__strtod_internal(s, end, group)` is what `strtod` expanded to; its
// `group` asks for the locale's thousands separator, which no locale here
// has, and the branch leaves it unread.
aliases! {
    "_IO_getc" => "getc",
    "__dcgettext" => "dcgettext",
    "__dn_expand" => "dn_expand",
    "__poll" => "poll",
    "__res_nquery" => "res_nquery",
    "__res_nsearch" => "res_nsearch",
    "__res_query" => "res_query",
    "__res_search" => "res_search",
    "__secure_getenv" => "secure_getenv",
    "__strndup" => "strndup",
    "__strsep_g" => "strsep",
    "__strtod_internal" => "strtod",
    "strtoq" => "strtoll",
    "strtouq" => "strtoull",
}

/// Writes one `global_asm!` of branches from glibc's names at an older
/// version, `name@VERSION`, to this library's functions, for the one
/// architecture named first. The linker reads such a name as `name` at
/// `VERSION`, not its default, as it does `build-shared.sh`'s older aliases
/// and [`crate::termios`]'s two. This is for the names no table of
/// defaults lists: those glibc keeps only for programs linked against an
/// older glibc, and those whose older version is another function.
macro_rules! versioned {
    (x86_64: $($alias:literal => $target:literal),* $(,)?) => {
        #[cfg(all(not(test), target_arch = "x86_64"))]
        core::arch::global_asm!(
            ".pushsection .text.ferrousli_glibc_versioned,\"ax\",@progbits",
            $(
                ".p2align 4",
                concat!(".globl \"", $alias, "\""),
                concat!(".type \"", $alias, "\", @function"),
                concat!("\"", $alias, "\":"),
                concat!("jmp ", $target),
            )*
            ".popsection",
        );
    };
    (aarch64: $($alias:literal => $target:literal),* $(,)?) => {
        #[cfg(all(not(test), target_arch = "aarch64"))]
        core::arch::global_asm!(
            ".pushsection .text.ferrousli_glibc_versioned,\"ax\",%progbits",
            $(
                ".p2align 2",
                concat!(".globl \"", $alias, "\""),
                concat!(".type \"", $alias, "\", %function"),
                concat!("\"", $alias, "\":"),
                concat!("b ", $target),
            )*
            ".popsection",
        );
    };
    (arm: $($alias:literal => $target:literal),* $(,)?) => {
        #[cfg(all(not(test), target_arch = "arm"))]
        core::arch::global_asm!(
            ".pushsection .text.ferrousli_glibc_versioned,\"ax\",%progbits",
            ".arm",
            $(
                ".p2align 2",
                concat!(".globl \"", $alias, "\""),
                concat!(".type \"", $alias, "\", %function"),
                concat!("\"", $alias, "\":"),
                concat!("b ", $target),
            )*
            ".popsection",
        );
    };
}

pub(crate) use versioned;

// libm's `_finite` names under their plain names, for AArch64's
// `libc.so.6`, where glibc has them at the first version, and for a static
// link. The `float` forms and the `double` ones; the `long double` forms,
// which nothing here imports, are left out with most of `long double`'s
// functions.
aliases! {
    "__acos_finite" => "acos",
    "__acosf_finite" => "acosf",
    "__acosh_finite" => "acosh",
    "__acoshf_finite" => "acoshf",
    "__asin_finite" => "asin",
    "__asinf_finite" => "asinf",
    "__atan2_finite" => "atan2",
    "__atan2f_finite" => "atan2f",
    "__atanh_finite" => "atanh",
    "__atanhf_finite" => "atanhf",
    "__cosh_finite" => "cosh",
    "__coshf_finite" => "coshf",
    "__exp10_finite" => "exp10",
    "__exp10f_finite" => "exp10f",
    "__exp2_finite" => "exp2",
    "__exp2f_finite" => "exp2f",
    "__exp_finite" => "exp",
    "__expf_finite" => "expf",
    "__fmod_finite" => "fmod",
    "__fmodf_finite" => "fmodf",
    "__gamma_r_finite" => "lgamma_r",
    "__gammaf_r_finite" => "lgammaf_r",
    "__hypot_finite" => "hypot",
    "__hypotf_finite" => "hypotf",
    "__j0_finite" => "j0",
    "__j0f_finite" => "j0f",
    "__j1_finite" => "j1",
    "__j1f_finite" => "j1f",
    "__jn_finite" => "jn",
    "__jnf_finite" => "jnf",
    "__lgamma_r_finite" => "lgamma_r",
    "__lgammaf_r_finite" => "lgammaf_r",
    "__log10_finite" => "log10",
    "__log10f_finite" => "log10f",
    "__log2_finite" => "log2",
    "__log2f_finite" => "log2f",
    "__log_finite" => "log",
    "__logf_finite" => "logf",
    "__pow_finite" => "pow",
    "__powf_finite" => "powf",
    "__remainder_finite" => "remainder",
    "__remainderf_finite" => "remainderf",
    "__scalb_finite" => "scalb",
    "__scalbf_finite" => "scalbf",
    "__sinh_finite" => "sinh",
    "__sinhf_finite" => "sinhf",
    "__sqrt_finite" => "sqrt",
    "__sqrtf_finite" => "sqrtf",
    "__y0_finite" => "y0",
    "__y0f_finite" => "y0f",
    "__y1_finite" => "y1",
    "__y1f_finite" => "y1f",
    "__yn_finite" => "yn",
    "__ynf_finite" => "ynf",
}

// The same names at `GLIBC_2.15`, the version x86-64's and ARMv7-A's glibc
// give them.
macro_rules! finite_at_2_15 {
    ($($arch:ident),*) => {
        $(
            versioned! {
                $arch:
                "__acos_finite@GLIBC_2.15" => "acos",
                "__acosf_finite@GLIBC_2.15" => "acosf",
                "__acosh_finite@GLIBC_2.15" => "acosh",
                "__acoshf_finite@GLIBC_2.15" => "acoshf",
                "__asin_finite@GLIBC_2.15" => "asin",
                "__asinf_finite@GLIBC_2.15" => "asinf",
                "__atan2_finite@GLIBC_2.15" => "atan2",
                "__atan2f_finite@GLIBC_2.15" => "atan2f",
                "__atanh_finite@GLIBC_2.15" => "atanh",
                "__atanhf_finite@GLIBC_2.15" => "atanhf",
                "__cosh_finite@GLIBC_2.15" => "cosh",
                "__coshf_finite@GLIBC_2.15" => "coshf",
                "__exp10_finite@GLIBC_2.15" => "exp10",
                "__exp10f_finite@GLIBC_2.15" => "exp10f",
                "__exp2_finite@GLIBC_2.15" => "exp2",
                "__exp2f_finite@GLIBC_2.15" => "exp2f",
                "__exp_finite@GLIBC_2.15" => "exp",
                "__expf_finite@GLIBC_2.15" => "expf",
                "__fmod_finite@GLIBC_2.15" => "fmod",
                "__fmodf_finite@GLIBC_2.15" => "fmodf",
                "__gamma_r_finite@GLIBC_2.15" => "lgamma_r",
                "__gammaf_r_finite@GLIBC_2.15" => "lgammaf_r",
                "__hypot_finite@GLIBC_2.15" => "hypot",
                "__hypotf_finite@GLIBC_2.15" => "hypotf",
                "__j0_finite@GLIBC_2.15" => "j0",
                "__j0f_finite@GLIBC_2.15" => "j0f",
                "__j1_finite@GLIBC_2.15" => "j1",
                "__j1f_finite@GLIBC_2.15" => "j1f",
                "__jn_finite@GLIBC_2.15" => "jn",
                "__jnf_finite@GLIBC_2.15" => "jnf",
                "__lgamma_r_finite@GLIBC_2.15" => "lgamma_r",
                "__lgammaf_r_finite@GLIBC_2.15" => "lgammaf_r",
                "__log10_finite@GLIBC_2.15" => "log10",
                "__log10f_finite@GLIBC_2.15" => "log10f",
                "__log2_finite@GLIBC_2.15" => "log2",
                "__log2f_finite@GLIBC_2.15" => "log2f",
                "__log_finite@GLIBC_2.15" => "log",
                "__logf_finite@GLIBC_2.15" => "logf",
                "__pow_finite@GLIBC_2.15" => "pow",
                "__powf_finite@GLIBC_2.15" => "powf",
                "__remainder_finite@GLIBC_2.15" => "remainder",
                "__remainderf_finite@GLIBC_2.15" => "remainderf",
                "__scalb_finite@GLIBC_2.15" => "scalb",
                "__scalbf_finite@GLIBC_2.15" => "scalbf",
                "__sinh_finite@GLIBC_2.15" => "sinh",
                "__sinhf_finite@GLIBC_2.15" => "sinhf",
                "__sqrt_finite@GLIBC_2.15" => "sqrt",
                "__sqrtf_finite@GLIBC_2.15" => "sqrtf",
                "__y0_finite@GLIBC_2.15" => "y0",
                "__y0f_finite@GLIBC_2.15" => "y0f",
                "__y1_finite@GLIBC_2.15" => "y1",
                "__y1f_finite@GLIBC_2.15" => "y1f",
                "__yn_finite@GLIBC_2.15" => "yn",
                "__ynf_finite@GLIBC_2.15" => "ynf",
            }
        )*
    };
}

finite_at_2_15!(x86_64, arm);

// x86-64's `memcpy` before glibc 2.14 copied overlapping bytes as `memmove`
// does, and programs came to rely on it, so glibc kept that function as
// `memcpy@GLIBC_2.2.5` when 2.14's faster one took the name. NVIDIA's Cg
// compiler, in the scout runtime, asks for it.
versioned! {
    x86_64:
    "memcpy@GLIBC_2.2.5" => "memmove",
}

pub(crate) use aliases;
