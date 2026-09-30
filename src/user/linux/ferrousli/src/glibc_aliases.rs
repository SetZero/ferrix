//! glibc's double-underscore names for functions this library has under the
//! plain name: the locale functions GCC's C++ runtime calls by glibc's
//! internal names -- LLVM and `rustc`'s driver import them through it --
//! `__pthread_key_create`, and the names the Steam client's 64-bit side
//! imports: `__strdup` and `__strtok_r` (SDL, the Steam Runtime's tools),
//! `_IO_putc` (lsof), `prlimit64` (the Runtime's tools; `rlim_t` is 64 bits
//! here on every architecture, so it is `prlimit`), and Chromium's
//! `__libc_malloc` family, which its allocator shim calls through. Python
//! imports `__sysconf` and `preadv64v2` and `pwritev64v2`; `off_t` is 64
//! bits here too, so the `64` names are the plain ones.
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
    "pwritev64" => "pwritev",
    "pwritev64v2" => "pwritev2",
}

pub(crate) use aliases;
