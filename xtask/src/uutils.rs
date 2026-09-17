//! uutils/coreutils built against ferrousli: the utilities that replace
//! busybox's, as one static multicall binary.
//!
//! `ferrousli/tools/uutils/build.sh` builds it, or `build-windows.sh` beside
//! it on Windows, and installs it as `x86_64/bin/coreutils` under
//! `~/.local/share/ferrix/uutils/ferrousli` (or `$FERRIX_UUTILS`).
//!
//! It is Rust, so unlike busybox it brings its own `std`, and `std` for the
//! musl target brings its own unwinder; ferrousli answers everything below
//! them. `docs/UUTILS.md` is the plan this is the second slice of, and §3a
//! there says why the target is `x86_64-unknown-linux-musl` rather than the
//! `-gnu` one ferrousli is closer to.
//!
//! What this module shares with [`crate::busybox`] is in [`crate::ferrousli`].

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::Arch;
use crate::{Error, Result, cargo, ferrousli};

/// The environment variable naming the install directory, and its default.
const VAR: &str = "FERRIX_UUTILS";
/// The default install directory under the home directory.
const HOME_SEGMENTS: &[&str] = &[".local", "share", "ferrix", "uutils", "ferrousli"];

/// Run by `bash -c` in `ferrousli/`, with `$1` the build script in
/// `tools/uutils/` and `$2` the directory to install into.
///
/// `$out` is the script's own: `$FERRIX_UUTILS`, with its default. Normally it
/// is the install directory itself, and the copy is skipped. Both lists are
/// removed first, so a list read after a failure is this run's.
const SCRIPT: &str = r#"set -uo pipefail
root=$2
out=${FERRIX_UUTILS:-$HOME/.local/share/ferrix/uutils/ferrousli}
mkdir -p "$root/x86_64/bin" || exit 1
rm -f "$out/undefined-symbols.txt" "$root/undefined-symbols.txt"
if ! bash "tools/uutils/$1"; then
    if [ -f "$out/undefined-symbols.txt" ] && ! [ "$out" -ef "$root" ]; then
        cp "$out/undefined-symbols.txt" "$root/"
    fi
    exit 1
fi
install -m 755 "$out/x86_64/coreutils" "$root/x86_64/bin/coreutils"
"#;

/// What a uutils built against ferrousli is made from, relative to
/// `ferrousli/`: the library and its entry object, and the pinned sources and
/// the build scripts.
const INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "crt",
    "include",
    "src",
    "tools/uutils",
];

/// The directory uutils is installed under.
fn root() -> Result<PathBuf> {
    ferrousli::install_root(VAR, HOME_SEGMENTS, "ferrousli's uutils")
}

/// The installed binary for `arch`, beneath `root`.
fn installed(root: &Path, arch: Arch) -> PathBuf {
    root.join(arch.name()).join("bin").join("coreutils")
}

/// Refuse every architecture the build scripts do not build for.
///
/// x86-64 only, because ferrousli is. The musl target this is built for has
/// an AArch64 and an ARMv7-A spelling, and the day ferrousli is built for
/// those this is most of what it takes to follow.
fn refuse_other_than_x86_64(arch: Arch) -> Result<()> {
    if arch == Arch::X86_64 {
        Ok(())
    } else {
        Err(Error::new(format!(
            "ferrousli's uutils is built for x86_64 only, not for {arch}"
        )))
    }
}

/// The installed uutils for `arch`, built first when it is missing or older
/// than ferrousli, so what boots is the tree as it stands.
///
/// Never another build in its place: a build that fails is the error.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the staleness rule, ready for the slice that puts uutils in \
                  the initramfs (docs/UUTILS.md S3); until then only the test \
                  below calls it"
    )
)]
pub(crate) fn program(arch: Arch) -> Result<PathBuf> {
    refuse_other_than_x86_64(arch)?;
    let root = root()?;
    let program = installed(&root, arch);
    let dir = crate::paths::workspace_root().join("ferrousli");
    if ferrousli::stale(&program, &dir, INPUTS).is_none() {
        return Ok(program);
    }
    // Another checkout may be building into the same directory: wait for it,
    // then look again, since what it installed may be current.
    let _lock = ferrousli::lock_builds(&root, "uutils")?;
    match ferrousli::stale(&program, &dir, INPUTS) {
        Some(reason) => {
            println!("  ferrousli's uutils {reason}; building it");
            build_locked(arch, &root)
        }
        None => Ok(program),
    }
}

/// `cargo xtask uutils`: run `build.sh`, or `build-windows.sh` on Windows,
/// and install the multicall binary.
pub(crate) fn build(arch: Arch) -> Result<PathBuf> {
    refuse_other_than_x86_64(arch)?;
    let root = root()?;
    let _lock = ferrousli::lock_builds(&root, "uutils")?;
    build_locked(arch, &root)
}

/// [`build`], with the build lock already held.
fn build_locked(arch: Arch, root: &Path) -> Result<PathBuf> {
    let root = root.to_path_buf();
    let program = installed(&root, arch);
    let dir = crate::paths::workspace_root().join("ferrousli");

    let (script, mut command) = if cfg!(windows) {
        let mut command = Command::new(ferrousli::git_bash()?);
        // One spelling of the directory for both the script and its caller,
        // with the separators bash expects.
        let root = root.to_string_lossy().replace('\\', "/");
        let _ = command
            .env(VAR, &root)
            .args(["-c", SCRIPT, "bash", "build-windows.sh", &root]);
        ("build-windows.sh", command)
    } else {
        let mut command = Command::new("bash");
        let _ = command.args(["-c", SCRIPT, "bash", "build.sh"]).arg(&root);
        ("build.sh", command)
    };
    ferrousli::in_ferrousli(&mut command, &dir);

    let description = format!("ferrousli/tools/uutils/{script}");
    if let Err(error) = cargo::run(command, &description) {
        let list = root.join(ferrousli::UNDEFINED);
        return Err(match std::fs::read_to_string(&list) {
            Ok(symbols) if !symbols.trim().is_empty() => {
                ferrousli::not_linked("uutils/coreutils", &symbols, &list)
            }
            _ => error,
        });
    }
    if !program.is_file() {
        return Err(Error::new(format!(
            "{script} reported success but {} does not exist",
            program.display()
        )));
    }
    println!("\nbuilt {}", program.display());
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_x86_64_has_a_ferrousli_uutils() {
        for arch in [Arch::AArch64, Arch::Armv7a] {
            assert!(program(arch).is_err(), "{arch}");
            assert!(build(arch).is_err(), "{arch}");
        }
    }

    #[test]
    fn the_installed_path_sits_beside_the_other_programs() {
        let path = installed(Path::new("root"), Arch::X86_64);
        assert_eq!(path, Path::new("root/x86_64/bin/coreutils"));
    }
}
