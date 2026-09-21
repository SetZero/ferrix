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
    println!("cargo::rustc-link-arg-bins=-Wl,--export-dynamic-symbol=__ferrousli_loader");
    // The loader finds symbols through `DT_GNU_HASH` alone (`src/sym.rs`), so
    // its own table has to be one.
    println!("cargo::rustc-link-arg-bins=-Wl,--hash-style=gnu");
}
