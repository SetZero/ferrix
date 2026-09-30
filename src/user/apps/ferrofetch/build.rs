//! Link the program with the runtime's layout, for the kernel's targets.
//!
//! The runtime names its linker script to the programs that link it, as
//! `DEP_FERRIX_RT_LINKER_SCRIPT`. It is a dependency only on the kernel's
//! targets, so on the host -- where only the lib target is built, for its
//! tests -- there is no script and nothing to link.

// A build script talks to cargo over stdout; that is the whole interface.
#![allow(
    clippy::print_stdout,
    reason = "stdout is how a build script communicates with cargo"
)]

fn main() {
    println!("cargo::rerun-if-env-changed=DEP_FERRIX_RT_LINKER_SCRIPT");
    if let Ok(script) = std::env::var("DEP_FERRIX_RT_LINKER_SCRIPT") {
        println!("cargo::rustc-link-arg-bins=-T{script}");
        println!("cargo::rerun-if-changed={script}");
    }
}
