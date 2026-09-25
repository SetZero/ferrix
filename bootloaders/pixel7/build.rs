//! Link the Pixel 7 loader at the address the Android bootloader loads it to.
//!
//! The script is passed here rather than in `.cargo/config.toml` for the reason
//! `kernel/build.rs` gives: cargo joins `rustflags` from every config file
//! between the invocation directory and the root, so a checkout inside another
//! checkout would pass `-T` twice.

#![allow(
    clippy::print_stdout,
    reason = "stdout is how a build script communicates with cargo"
)]

use std::error::Error;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")?;
    let script = Path::new(&manifest).join("linker").join("pixel7.ld");
    println!("cargo::rustc-link-arg-bins=-T{}", script.display());
    println!("cargo::rerun-if-changed={}", script.display());
    println!("cargo::rerun-if-changed=build.rs");
    Ok(())
}
