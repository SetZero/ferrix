//! Tells every native program where its linker script is.
//!
//! A program links with `-T`, emitted by its own build script, for the reason
//! `kernel/build.rs` gives at length: a link script in `.cargo/config.toml` is
//! merged with every configuration file above the checkout and can end up
//! passed twice. A build script's flag cannot be. But `rustc-link-arg` applies
//! only to the package that emits it, so this crate cannot pass the flag for
//! its dependents; it publishes the script's absolute path as `links`
//! metadata, which cargo hands each dependent's build script as
//! `DEP_FERRIX_RT_LINKER_SCRIPT`.

// A build script talks to cargo over stdout; that is the whole interface.
#![allow(
    clippy::print_stdout,
    reason = "stdout is how a build script communicates with cargo"
)]

use std::error::Error;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")?;
    let script = Path::new(&manifest).join("linker").join("native.ld");
    println!("cargo::metadata=LINKER_SCRIPT={}", script.display());
    println!("cargo::rerun-if-changed={}", script.display());
    println!("cargo::rerun-if-changed=build.rs");
    Ok(())
}
