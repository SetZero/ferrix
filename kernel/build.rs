//! Where the kernel's link script and its load address are named.
//!
//! # Why this is not in `.cargo/config.toml`
//!
//! It was, and it was passed twice. Cargo does not pick one configuration
//! file: it walks from the directory it was invoked in up to the filesystem
//! root and *merges* every `.cargo/config.toml` it finds, joining array values
//! like `rustflags` end to end rather than letting the nearest one win. A
//! checkout that happens to sit inside another checkout therefore links with
//! `-Tkernel/linker/kernel.ld` given twice.
//!
//! A link script given twice is evaluated twice, and the second pass is not a
//! harmless repeat. It re-runs `. = KERNEL_VIRT_BASE` with every input section
//! already consumed by the first, so it emits a second set of output sections,
//! all empty, at the base address. The empty `PROGBITS` ones are discarded;
//! `.bss` is `NOLOAD` and is kept, and it is assigned to the `data` segment —
//! whose `p_memsz` is then computed from a start above it and an end below,
//! and wraps. The kernel links without a warning and the loader rejects it:
//!
//! ```text
//! FERRIX-PANIC loader: the kernel has a malformed segment
//! ```
//!
//! which is `libs/elf`'s `validate_segments` doing its job on an image that
//! should never have been produced. `__bss_start`, `__bss_end` and
//! `__kernel_end` resolve into that phantom section too, so anything trusting
//! them would have been wrong in a quieter way.
//!
//! A build script cannot be merged with anything. It runs once per build of
//! this package and emits these flags once, whatever configuration files
//! happen to be above the checkout — which is the property that was wanted
//! from `.cargo/config.toml` and that it cannot offer. The flags that remain
//! there are the codegen ones, where being passed twice means nothing.
//!
//! The path is emitted absolute, from `CARGO_MANIFEST_DIR`, so it also stops
//! depending on which directory cargo was invoked from.

// A build script talks to cargo over stdout; that is the whole interface.
#![allow(
    clippy::print_stdout,
    reason = "stdout is how a build script communicates with cargo"
)]

use std::error::Error;
use std::path::{Path, PathBuf};

/// Where the kernel is linked, by word width.
///
/// The same number as `KERNEL_VIRT_BASE` in `libs/bootinfo` for that width,
/// and the loader refuses to start a kernel linked anywhere else. Keyed on the
/// width rather than on the architecture because that is what decides it: the
/// 64-bit pair share the top 2 GiB of a 48-bit space, and ARMv7-A takes the
/// top 256 MiB of a 32-bit one. A fourth 64-bit architecture needs no entry
/// here.
const KERNEL_VIRT_BASE_64: &str = "0xffffffff80000000";
/// As above, for a 32-bit target.
const KERNEL_VIRT_BASE_32: &str = "0xf0000000";

/// An `Err` here fails the build with the message, which is what a build
/// script that cannot work out where the kernel goes should do. Returning one
/// rather than panicking because the workspace lint table denies `panic!`,
/// `unwrap` and `exit` in this tree, build scripts included.
fn main() -> Result<(), Box<dyn Error>> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")?;
    let script = Path::new(&manifest).join("linker").join("kernel.ld");

    let width = std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH")?;
    let base = match width.as_str() {
        "64" => KERNEL_VIRT_BASE_64,
        "32" => KERNEL_VIRT_BASE_32,
        other => {
            return Err(format!("the kernel has no load address for a {other}-bit target").into());
        }
    };

    // The `--defsym` comes before the `-T`, so the symbol exists before the
    // script names it. A build that loses the definition fails to link rather
    // than producing an image at address zero.
    println!("cargo::rustc-link-arg-bins=--defsym=KERNEL_VIRT_BASE={base}");
    println!("cargo::rustc-link-arg-bins=-T{}", script.display());

    // Relink when the script changes. Without this a script edit is invisible:
    // cargo reruns a build script only when something it declares has changed,
    // and by default that is the script's own source.
    println!("cargo::rerun-if-changed={}", script.display());
    println!("cargo::rerun-if-changed=build.rs");

    // The first program to run, if one was asked for.
    //
    // Opt-in through `FERRIX_INIT`, because the obvious binary is a host's own
    // `busybox` and a build that picked it up silently would stop being
    // reproducible — `xtask build` promises the same image byte for byte, and
    // two machines do not have the same `/usr/bin`. With the variable unset the
    // kernel embeds an empty file and boots exactly as it did before.
    println!("cargo::rerun-if-env-changed=FERRIX_INIT");
    let init = match std::env::var_os("FERRIX_INIT") {
        Some(path) => {
            let path = PathBuf::from(path);
            println!("cargo::rerun-if-changed={}", path.display());
            path
        }
        None => {
            let empty = PathBuf::from(std::env::var("OUT_DIR")?).join("no-init");
            std::fs::write(&empty, b"")?;
            empty
        }
    };
    println!("cargo::rustc-env=FERRIX_INIT_IMAGE={}", init.display());

    // What the first program is told to do. Empty means an interactive shell;
    // anything else is handed to `sh -c`, which is how `cargo xtask test-shell`
    // makes stage 7's exit criterion a check rather than a transcript. Written
    // to a file rather than passed as `rustc-env`, because a script has
    // newlines and a `cargo::` directive is one line.
    println!("cargo::rerun-if-env-changed=FERRIX_INIT_SCRIPT");
    let script = std::env::var("FERRIX_INIT_SCRIPT").unwrap_or_default();
    if script.contains('\0') {
        return Err(
            "FERRIX_INIT_SCRIPT contains a NUL, which cannot survive being an argument".into(),
        );
    }
    let script_file = PathBuf::from(std::env::var("OUT_DIR")?).join("init-script");
    std::fs::write(&script_file, script)?;
    println!(
        "cargo::rustc-env=FERRIX_INIT_SCRIPT_FILE={}",
        script_file.display()
    );
    Ok(())
}
