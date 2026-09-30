//! Link flags for the loader: the symbols it exports.
//!
//! The loader is an executable, and an executable's symbols are not in its
//! dynamic symbol table unless the link asks. A few must be: the C library
//! finds `__ferrousli_loader` through its GOT, which the loader resolves by
//! looking itself up like any other object in the scope. Named one by one
//! rather than with `--export-dynamic`, which would put every Rust symbol in
//! the table for any program to bind to by accident.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // x86-64's loader links through `cc`, which takes linker flags after
    // `-Wl,`; the Arm ones through rust-lld itself (`../.cargo/config.toml`).
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let prefix = if arch == "x86_64" { "-Wl," } else { "" };
    // `__tls_get_addr` is the loader's in glibc too (`ld-linux`'s, at
    // `GLIBC_2.3`), and a library built with `-fPIC` that reads a TLS variable
    // imports it.
    for symbol in [
        "__ferrousli_loader",
        "__tls_get_addr",
        "dlopen",
        "dlsym",
        "dlclose",
        "dlerror",
        "dladdr",
        "dl_iterate_phdr",
        "dladdr1",
        "dlinfo",
    ] {
        println!("cargo::rustc-link-arg-bins={prefix}--export-dynamic-symbol={symbol}");
    }
    // The loader finds symbols through `DT_GNU_HASH` alone (`src/sym.rs`), so
    // its own table has to be one.
    println!("cargo::rustc-link-arg-bins={prefix}--hash-style=gnu");
}
