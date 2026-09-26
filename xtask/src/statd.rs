//! Building `ferrix-statd`, the stat service in `statd/`, for the initramfs.
//!
//! Built as zinc is (`zinc.rs`): a static Linux program with std against the
//! target's own musl, from its own workspace into a target directory of its
//! own. `--statd` puts it in an image at `/sbin/ferrix-statd`, which
//! `ferrix.init=/sbin/ferrix-statd` starts as pid 1 after the boot checks.

use crate::paths::{self, Arch};
use crate::ports;
use crate::{Error, Result, zinc};

/// Where the service goes in the initramfs.
pub(crate) const PATH: &str = "sbin/ferrix-statd";

/// Build the service for `arch` and return it as a file for the initramfs,
/// or `None` on an architecture it is not built for yet.
pub(crate) fn file(arch: Arch) -> Result<Option<ports::File>> {
    let Some(target) = zinc::target(arch) else {
        println!("  ferrix-statd is not built for {} yet", arch.name());
        return Ok(None);
    };
    println!("  building ferrix-statd for {target}");
    let target_dir = paths::target_dir().join("statd");
    let program = target_dir.join(target).join("release").join("ferrix-statd");
    crate::builds::Build::cargo(
        format!("cargo build (ferrix-statd) --target {target}"),
        paths::workspace_root().join("statd"),
    )
    .args(["build", "--release", "--target", target])
    .env("CARGO_TARGET_DIR", &target_dir)
    // For zinc's reason: RUSTFLAGS replaces the flags every config file up
    // the tree would otherwise merge in.
    .env("RUSTFLAGS", zinc::RUSTFLAGS)
    .output(&program)
    .run()?;
    let bytes = std::fs::read(&program)
        .map_err(|error| Error::new(format!("reading {}: {error}", program.display())))?;
    Ok(Some(ports::File {
        path: PATH.to_owned(),
        mode: 0o755,
        content: ports::Content::Bytes(bytes),
    }))
}
