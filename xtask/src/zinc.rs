//! Building zinc, the zsh-compatible shell in `zinc/`, for the initramfs.
//!
//! zinc is a static Linux program built with std against the target's own
//! musl, which rust-lld links without a C toolchain (`zinc/.cargo/config.toml`
//! says how), so this works from any host, Windows included. It is its own
//! cargo workspace, so it is built from its directory into a target directory
//! of its own.

use std::process::Command;

use crate::paths::{self, Arch};
use crate::{Error, Result};

/// The Rust target zinc is built for on `arch`, if there is one yet.
fn target(arch: Arch) -> Option<&'static str> {
    match arch.name() {
        "x86_64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// Build zinc for `arch` and return the program, or `None` on an
/// architecture it is not built for yet.
pub(crate) fn build(arch: Arch) -> Result<Option<Vec<u8>>> {
    let Some(target) = target(arch) else {
        println!("  zinc is not built for {} yet", arch.name());
        return Ok(None);
    };
    println!("  building zinc for {target}");
    let target_dir = paths::target_dir().join("zinc");
    let mut command = Command::new(crate::cargo::cargo());
    let _ = command
        .current_dir(paths::workspace_root().join("zinc"))
        .args(["build", "--release", "--target", target])
        .env("CARGO_TARGET_DIR", &target_dir);
    crate::cargo::run(command, "cargo build (zinc)")?;
    let path = target_dir.join(target).join("release").join("zinc");
    std::fs::read(&path)
        .map(Some)
        .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
}
