//! glibc's double-underscore names for functions this library has under the
//! plain name: the locale functions GCC's C++ runtime calls by glibc's
//! internal names -- LLVM and `rustc`'s driver import them through it -- and
//! `__pthread_key_create`.
//!
//! Each takes the same arguments as the plain function and does the same
//! thing, so each is a branch to it, as [`crate::arm_names`] does for
//! ARMv7-A's time64 names, rather than a Rust wrapper that would only pass
//! its arguments on.

/// Writes one `global_asm!` of branches, one per `alias => target` pair, for
/// the architecture's branch instruction and symbol-type syntax.
macro_rules! aliases {
    ($($alias:literal => $target:literal),* $(,)?) => {
        #[cfg(all(not(test), target_arch = "x86_64"))]
        core::arch::global_asm!(
            ".pushsection .text.ferrousli_glibc_aliases,\"ax\",@progbits",
            ".p2align 4",
            $(
                concat!(".globl ", $alias),
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
                concat!(".globl ", $alias),
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
                concat!(".globl ", $alias),
                concat!(".type ", $alias, ", %function"),
                concat!($alias, ":"),
                concat!("b ", $target),
            )*
            ".popsection",
        );
    };
}

aliases! {
    "__duplocale" => "duplocale",
    "__freelocale" => "freelocale",
    "__iswctype_l" => "iswctype_l",
    "__newlocale" => "newlocale",
    "__nl_langinfo_l" => "nl_langinfo_l",
    "__pthread_key_create" => "pthread_key_create",
    "__strcoll_l" => "strcoll_l",
    "__strftime_l" => "strftime_l",
    "__strtod_l" => "strtod_l",
    "__strtof_l" => "strtof_l",
    "__strxfrm_l" => "strxfrm_l",
    "__towlower_l" => "towlower_l",
    "__towupper_l" => "towupper_l",
    "__uselocale" => "uselocale",
    "__wcscoll_l" => "wcscoll_l",
    "__wcsftime_l" => "wcsftime_l",
    "__wcsxfrm_l" => "wcsxfrm_l",
    "__wctype_l" => "wctype_l",
}
