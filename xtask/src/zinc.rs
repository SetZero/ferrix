//! Building zinc, the zsh-compatible shell in `zinc/`, for the initramfs.
//!
//! zinc is a static Linux program built with std against the target's own
//! musl, which rust-lld links without a C toolchain (`zinc/.cargo/config.toml`
//! says how), so this works from any host, Windows included. It is its own
//! cargo workspace, so it is built from its directory into a target directory
//! of its own.

use std::path::PathBuf;

use crate::paths::{self, Arch};
use crate::{Error, Result};

/// A static, fixed-address executable against the target's own musl, which
/// rust-lld links without a C toolchain.
const RUSTFLAGS: &str =
    "-C link-self-contained=yes -C target-feature=+crt-static -C relocation-model=static";

/// The Rust target zinc is built for on `arch`, if there is one yet.
fn target(arch: Arch) -> Option<&'static str> {
    match arch.name() {
        "x86_64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" => Some("aarch64-unknown-linux-musl"),
        "armv7a" => Some("armv7-unknown-linux-musleabi"),
        _ => None,
    }
}

/// Build zinc for `arch` and return where it was written, or `None` on an
/// architecture it is not built for yet.
///
/// The path rather than the bytes, for the caller that has to hand the
/// compiler a file: `test-shell` builds zinc into the kernel, as the shell
/// the kernel starts.
pub(crate) fn built(arch: Arch) -> Result<Option<PathBuf>> {
    let Some(target) = target(arch) else {
        println!("  zinc is not built for {} yet", arch.name());
        return Ok(None);
    };
    println!("  building zinc for {target}");
    let target_dir = paths::target_dir().join("zinc");
    let program = target_dir.join(target).join("release").join("zinc");
    crate::builds::Build::cargo(
        format!("cargo build (zinc) --target {target}"),
        paths::workspace_root().join("zinc"),
    )
    .args(["build", "--release", "--target", target])
    .env("CARGO_TARGET_DIR", &target_dir)
    // The flags `zinc/.cargo/config.toml` gives each target, set here
    // because cargo merges rustflags from every config file up the tree,
    // and the root one gives `armv7-unknown-linux-musleabi` the ARM
    // loader's linker script. RUSTFLAGS replaces the configured flags.
    .env("RUSTFLAGS", RUSTFLAGS)
    .output(&program)
    .run()?;
    Ok(Some(program))
}

/// Build zinc for `arch` and return the program, or `None` on an
/// architecture it is not built for yet.
pub(crate) fn build(arch: Arch) -> Result<Option<Vec<u8>>> {
    let Some(path) = built(arch)? else {
        return Ok(None);
    };
    std::fs::read(&path)
        .map(Some)
        .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
}
