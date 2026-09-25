//! Link the Pixel 7 loader at the address the Android bootloader loads it to,
//! and embed the kernel and initramfs it starts.
//!
//! The script is passed here rather than in `.cargo/config.toml` for the reason
//! `kernel/build.rs` gives: cargo joins `rustflags` from every config file
//! between the invocation directory and the root, so a checkout inside another
//! checkout would pass `-T` twice.
//!
//! The payload is named by `FERRIX_PIXEL7_KERNEL` and `FERRIX_PIXEL7_INITRD`,
//! the stripped `KERNEL.ELF` and `INITRD.IMG` that `cargo xtask flash --stage`
//! writes. ABL loads only one image, so the kernel and its archive have to be
//! inside it. Without the variables the loader is built with nothing to load,
//! which is what linting it needs, and says so when run.

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

    println!("cargo::rustc-check-cfg=cfg(payload)");
    println!("cargo::rerun-if-env-changed=FERRIX_PIXEL7_KERNEL");
    println!("cargo::rerun-if-env-changed=FERRIX_PIXEL7_INITRD");
    let kernel = std::env::var_os("FERRIX_PIXEL7_KERNEL");
    let initrd = std::env::var_os("FERRIX_PIXEL7_INITRD");
    if let (Some(kernel), Some(initrd)) = (kernel, initrd) {
        for (name, path) in [("PIXEL7_KERNEL", &kernel), ("PIXEL7_INITRD", &initrd)] {
            let path = Path::new(path).canonicalize()?;
            println!("cargo::rerun-if-changed={}", path.display());
            println!("cargo::rustc-env={name}={}", path.display());
        }
        println!("cargo::rustc-cfg=payload");
    }
    Ok(())
}
