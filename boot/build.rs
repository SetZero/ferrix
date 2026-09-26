//! Where the ARMv7-A loader's link script is named.
//!
//! # Why this is not in `.cargo/config.toml`
//!
//! It was, as `-Tboot/linker/armv7a.ld`, and that path had two problems the
//! kernel's link script had before it (see `kernel/build.rs`). Cargo merges
//! every `.cargo/config.toml` from the invocation directory up to the root,
//! joining `rustflags` end to end, so a worktree inside another checkout
//! linked with the script given twice. And the path was relative to wherever
//! cargo was run, so a worktree whose tree differs from the enclosing
//! checkout's -- one on a branch from before a directory moved -- was handed
//! a script that does not exist in it, and its loader failed to link.
//!
//! A build script runs once per build of this package, and names the script
//! absolutely, from `CARGO_MANIFEST_DIR`. Only the ARMv7-A target has one: the
//! x86-64 and AArch64 loaders are PE images the UEFI targets lay out
//! themselves. The other ARMv7-A flags stay in `.cargo/config.toml`, where
//! being passed twice changes nothing.

// A build script talks to cargo over stdout; that is the whole interface.
#![allow(
    clippy::print_stdout,
    reason = "stdout is how a build script communicates with cargo"
)]

use std::error::Error;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ARCH")? != "arm" {
        return Ok(());
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR")?;
    let script = Path::new(&manifest).join("linker").join("armv7a.ld");
    println!("cargo::rustc-link-arg-bins=-T{}", script.display());
    // Relink when the script changes, which cargo would not otherwise notice.
    println!("cargo::rerun-if-changed={}", script.display());
    Ok(())
}
