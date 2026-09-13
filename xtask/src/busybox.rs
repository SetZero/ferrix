//! ferrousli's busybox: built by `ferrousli/tools/busybox/build.sh`, and
//! installed as `x86_64/bin/busybox.static` under
//! `~/.local/share/ferrix/busybox/ferrousli` (or `$FERRIX_BUSYBOX`), where
//! `--init ferrousli` finds it.
//!
//! The script needs a Linux host, because it compiles busybox with the host's
//! `cc` against the host's kernel UAPI headers. On Windows it runs in WSL's
//! default distribution, reaching this checkout through `/mnt`, and the binary
//! is copied out to the same path under the Windows home directory, so the
//! kernel's build reads it as it reads any other `--init` program.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::Arch;
use crate::{Error, Result, cargo};

/// The `--init` value that names ferrousli's busybox rather than a path.
pub(crate) const INIT_NAME: &str = "ferrousli";

/// The list `build.sh` writes when the link fails, one symbol per line.
const UNDEFINED: &str = "undefined-symbols.txt";

/// Run by `bash -c` in `ferrousli/`, with `$1` saying whether `$2`, the
/// directory to install into, is a Windows path to translate.
///
/// `$out` is `build.sh`'s own: `$FERRIX_BUSYBOX`, with the script's default.
/// On Linux it is the install directory itself, and the copy is skipped. Both
/// lists are removed first, so a list read after a failure is this run's.
const SCRIPT: &str = r#"set -uo pipefail
root=$2
if [ "$1" = windows ]; then
    root=$(wslpath -a "$root") || exit 1
fi
out=${FERRIX_BUSYBOX:-$HOME/.local/share/ferrix/busybox/ferrousli}
mkdir -p "$root/x86_64/bin" || exit 1
rm -f "$out/undefined-symbols.txt" "$root/undefined-symbols.txt"
if ! bash tools/busybox/build.sh; then
    if [ -f "$out/undefined-symbols.txt" ] && ! [ "$out" -ef "$root" ]; then
        cp "$out/undefined-symbols.txt" "$root/"
    fi
    exit 1
fi
install -m 755 "$out/x86_64/busybox" "$root/x86_64/bin/busybox.static"
"#;

/// The directory ferrousli's busybox is installed under.
fn root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("FERRIX_BUSYBOX") {
        return Ok(PathBuf::from(dir));
    }
    std::env::home_dir()
        .map(|home| {
            [".local", "share", "ferrix", "busybox", "ferrousli"]
                .iter()
                .fold(home, |dir, name| dir.join(name))
        })
        .ok_or_else(|| {
            Error::new("no home directory to find ferrousli's busybox under; set FERRIX_BUSYBOX")
        })
}

/// The installed binary for `arch`, beneath `root`.
fn installed(root: &Path, arch: Arch) -> PathBuf {
    root.join(arch.name()).join("bin").join("busybox.static")
}

/// Refuse every architecture `build.sh` does not build for.
fn refuse_other_than_x86_64(arch: Arch) -> Result<()> {
    if arch == Arch::X86_64 {
        Ok(())
    } else {
        Err(Error::new(format!(
            "ferrousli's busybox is built for x86_64 only, not for {arch}"
        )))
    }
}

/// `--init ferrousli`: the installed busybox for `arch`.
///
/// Never built from here, and never another busybox in its place: a missing
/// binary is an error naming the command that builds it.
pub(crate) fn program(arch: Arch) -> Result<PathBuf> {
    refuse_other_than_x86_64(arch)?;
    let program = installed(&root()?, arch);
    if !program.is_file() {
        return Err(Error::new(format!(
            "no ferrousli busybox at {}; build it with `cargo xtask busybox`",
            program.display()
        )));
    }
    Ok(program)
}

/// `cargo xtask busybox`: run `build.sh`, in WSL on Windows, and install the
/// binary for `--init ferrousli`.
///
/// While ferrousli still lacks functions busybox calls, the link fails and
/// this refuses, naming them.
pub(crate) fn build(arch: Arch) -> Result<PathBuf> {
    refuse_other_than_x86_64(arch)?;
    let root = root()?;
    let program = installed(&root, arch);
    let ferrousli = crate::paths::workspace_root().join("ferrousli");

    let command = if cfg!(windows) {
        let mut command = Command::new("wsl.exe");
        // A login shell, so that `~/.profile` puts rustup's cargo on PATH.
        let _ = command
            .arg("--cd")
            .arg(&ferrousli)
            .args(["--exec", "bash", "-lc", SCRIPT, "bash", "windows"])
            .arg(&root);
        command
    } else {
        let mut command = Command::new("bash");
        // `build.sh` looks for ferrousli's library in `ferrousli/target`.
        let _ = command
            .current_dir(&ferrousli)
            .env_remove("CARGO_TARGET_DIR")
            .args(["-c", SCRIPT, "bash", "native"])
            .arg(&root);
        command
    };

    if let Err(error) = cargo::run(command, "ferrousli/tools/busybox/build.sh") {
        let list = root.join(UNDEFINED);
        return Err(match std::fs::read_to_string(&list) {
            Ok(symbols) if !symbols.trim().is_empty() => not_linked(&symbols, &list),
            _ => error,
        });
    }
    if !program.is_file() {
        return Err(Error::new(format!(
            "build.sh reported success but {} does not exist",
            program.display()
        )));
    }
    println!("\nbuilt {}", program.display());
    Ok(program)
}

/// The refusal for a link that failed on `symbols`, the list at `list`.
fn not_linked(symbols: &str, list: &Path) -> Error {
    let names: Vec<&str> = symbols.lines().filter(|line| !line.is_empty()).collect();
    Error::new(format!(
        "busybox does not link against ferrousli yet: {} undefined ({}), listed in {}",
        names.len(),
        names.join(", "),
        list.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_x86_64_has_a_ferrousli_busybox() {
        for arch in [Arch::AArch64, Arch::Armv7a] {
            assert!(program(arch).is_err(), "{arch}");
            assert!(build(arch).is_err(), "{arch}");
        }
    }

    #[test]
    fn a_failed_link_names_what_is_missing() {
        let error = not_linked("crypt\nscanf\n\n", Path::new("list.txt")).to_string();
        assert!(error.contains("2 undefined (crypt, scanf)"), "{error}");
        assert!(error.contains("list.txt"), "{error}");
    }

    #[test]
    fn the_installed_path_matches_the_musl_busybox_layout() {
        let path = installed(Path::new("root"), Arch::X86_64);
        assert_eq!(path, Path::new("root/x86_64/bin/busybox.static"));
    }
}
