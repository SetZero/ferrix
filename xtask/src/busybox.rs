//! ferrousli's busybox: built by `ferrousli/tools/busybox/build.sh`, or on
//! Windows by `build-windows.sh` beside it, and installed as
//! `x86_64/bin/busybox.static` under `~/.local/share/ferrix/busybox/ferrousli`
//! (or `$FERRIX_BUSYBOX`), where `--init ferrousli` finds it.
//!
//! `build.sh` compiles busybox with the host's `cc` against the host's kernel
//! UAPI headers. Windows has neither, so `build-windows.sh` cross-compiles with
//! clang against Alpine's pinned UAPI headers, in Git for Windows' bash. Both
//! build the same pinned sources with the same config.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::paths::Arch;
use crate::{Error, Result, cargo};

/// The `--init` value that names ferrousli's busybox rather than a path.
pub(crate) const INIT_NAME: &str = "ferrousli";

/// The list the build scripts write when the link fails, one symbol per line.
const UNDEFINED: &str = "undefined-symbols.txt";

/// Run by `bash -c` in `ferrousli/`, with `$1` the build script in
/// `tools/busybox/` and `$2` the directory to install into.
///
/// `$out` is the scripts' own: `$FERRIX_BUSYBOX`, with their default. Normally
/// it is the install directory itself, and the copy is skipped. Both lists are
/// removed first, so a list read after a failure is this run's.
const SCRIPT: &str = r#"set -uo pipefail
root=$2
out=${FERRIX_BUSYBOX:-$HOME/.local/share/ferrix/busybox/ferrousli}
mkdir -p "$root/x86_64/bin" || exit 1
rm -f "$out/undefined-symbols.txt" "$root/undefined-symbols.txt"
if ! bash "tools/busybox/$1"; then
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

/// Refuse every architecture the build scripts do not build for.
fn refuse_other_than_x86_64(arch: Arch) -> Result<()> {
    if arch == Arch::X86_64 {
        Ok(())
    } else {
        Err(Error::new(format!(
            "ferrousli's busybox is built for x86_64 only, not for {arch}"
        )))
    }
}

/// What a busybox built against ferrousli is made from, relative to
/// `ferrousli/`: the library and its entry object, and the pinned busybox
/// sources and configuration the build scripts use.
const INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "crt",
    "include",
    "src",
    "tools/busybox",
];

/// The newest modification time of any file under `path`, a file or a
/// directory; `None` if there is nothing there.
fn newest(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_dir() {
        return meta.modified().ok();
    }
    std::fs::read_dir(path)
        .ok()?
        .flatten()
        .filter_map(|entry| newest(&entry.path()))
        .max()
}

/// Why the busybox at `program` must be built before it is used, or `None`
/// when it is newer than everything it is built from under `ferrousli`.
fn stale(program: &Path, ferrousli: &Path) -> Option<&'static str> {
    let Some(built) = std::fs::metadata(program)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|meta| meta.modified().ok())
    else {
        return Some("is not built");
    };
    let sources = INPUTS
        .iter()
        .filter_map(|input| newest(&ferrousli.join(input)))
        .max();
    sources
        .is_some_and(|sources| sources > built)
        .then_some("is older than ferrousli's sources")
}

/// `--init ferrousli`: the installed busybox for `arch`, built first when it is
/// missing or older than ferrousli, so what boots is the tree as it stands.
///
/// Never another busybox in its place: a build that fails is the error.
pub(crate) fn program(arch: Arch) -> Result<PathBuf> {
    refuse_other_than_x86_64(arch)?;
    let root = root()?;
    let program = installed(&root, arch);
    let ferrousli = crate::paths::workspace_root().join("ferrousli");
    if stale(&program, &ferrousli).is_none() {
        return Ok(program);
    }
    // Another checkout may be building into the same directory: wait for
    // it, then look again, since what it installed may be current.
    let _lock = lock_builds(&root)?;
    match stale(&program, &ferrousli) {
        Some(reason) => {
            println!("  ferrousli's busybox {reason}; building it");
            build_locked(arch, &root)
        }
        None => Ok(program),
    }
}

/// Hold the install directory's build lock until the file is dropped.
///
/// Every checkout on the machine builds into the one directory, removing and
/// unpacking the sources and headers as it goes, so two builds at once break
/// each other; this makes the second wait for the first.
fn lock_builds(root: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(root)
        .map_err(|error| Error::new(format!("creating {}: {error}", root.display())))?;
    let path = root.join("build.lock");
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| Error::new(format!("opening {}: {error}", path.display())))?;
    if file.try_lock().is_err() {
        println!(
            "  waiting for another busybox build to finish ({})",
            path.display()
        );
        file.lock()
            .map_err(|error| Error::new(format!("locking {}: {error}", path.display())))?;
    }
    Ok(file)
}

/// Git for Windows' bash for a `git` at `git`: the launcher `bin/bash.exe` in
/// the nearest ancestor that also holds `usr/bin/bash.exe`, as Git's own
/// directory does whether `git` is its `cmd/`, `bin/` or `mingw64/bin/` one.
fn git_bash_beside(git: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    git.ancestors().skip(1).find_map(|dir| {
        let launcher = dir.join("bin").join("bash.exe");
        let shell = dir.join("usr").join("bin").join("bash.exe");
        (exists(&launcher) && exists(&shell)).then_some(launcher)
    })
}

/// Git for Windows' bash, which runs `build-windows.sh`.
///
/// Not the first `bash` on `PATH`: on Windows that is often WSL's launcher in
/// `System32`. And Git's launcher rather than `usr/bin/bash.exe` itself,
/// because the launcher puts Git's POSIX tools on the shell's `PATH`.
fn git_bash() -> Result<PathBuf> {
    crate::paths::which("git")
        .and_then(|git| git_bash_beside(&git, Path::is_file))
        .or_else(|| git_bash_beside(Path::new("C:/Program Files/Git/cmd/git.exe"), Path::is_file))
        .ok_or_else(|| {
            Error::new(
                "no Git for Windows bash to run build-windows.sh in; \
                 install it with `winget install Git.Git`",
            )
        })
}

/// `cargo xtask busybox`: run `build.sh`, or `build-windows.sh` on Windows,
/// and install the binary for `--init ferrousli`.
///
/// While ferrousli still lacks functions busybox calls, the link fails and
/// this refuses, naming them.
pub(crate) fn build(arch: Arch) -> Result<PathBuf> {
    refuse_other_than_x86_64(arch)?;
    let root = root()?;
    let _lock = lock_builds(&root)?;
    build_locked(arch, &root)
}

/// [`build`], with the build lock already held.
fn build_locked(arch: Arch, root: &Path) -> Result<PathBuf> {
    let root = root.to_path_buf();
    let program = installed(&root, arch);
    let ferrousli = crate::paths::workspace_root().join("ferrousli");

    let (script, mut command) = if cfg!(windows) {
        let mut command = Command::new(git_bash()?);
        // One spelling of the directory for both the script and its caller,
        // with the separators bash expects.
        let root = root.to_string_lossy().replace('\\', "/");
        let _ = command.env("FERRIX_BUSYBOX", &root).args([
            "-c",
            SCRIPT,
            "bash",
            "build-windows.sh",
            &root,
        ]);
        ("build-windows.sh", command)
    } else {
        let mut command = Command::new("bash");
        let _ = command.args(["-c", SCRIPT, "bash", "build.sh"]).arg(&root);
        ("build.sh", command)
    };
    // Both scripts look for ferrousli's library in `ferrousli/target`.
    let _ = command
        .current_dir(&ferrousli)
        .env_remove("CARGO_TARGET_DIR");

    let description = format!("ferrousli/tools/busybox/{script}");
    if let Err(error) = cargo::run(command, &description) {
        let list = root.join(UNDEFINED);
        return Err(match std::fs::read_to_string(&list) {
            Ok(symbols) if !symbols.trim().is_empty() => not_linked(&symbols, &list),
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
    fn a_busybox_older_than_ferrousli_is_rebuilt() {
        let dir = std::env::temp_dir().join(format!("xtask-busybox-stale-{}", std::process::id()));
        let ferrousli = dir.join("ferrousli");
        std::fs::create_dir_all(ferrousli.join("src")).unwrap();
        let program = dir.join("busybox.static");
        assert_eq!(stale(&program, &ferrousli), Some("is not built"));

        let source = ferrousli.join("src").join("lib.rs");
        std::fs::write(&source, "").unwrap();
        std::fs::write(&program, "").unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(stale(&program, &ferrousli), None);

        std::fs::File::options()
            .write(true)
            .open(&program)
            .unwrap()
            .set_modified(old - std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            stale(&program, &ferrousli),
            Some("is older than ferrousli's sources")
        );
        std::fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn git_bash_is_the_launcher_in_gits_own_directory() {
        let layout = ["C:/Git/bin/bash.exe", "C:/Git/usr/bin/bash.exe"];
        let exists = |path: &Path| layout.iter().any(|known| path == Path::new(known));
        for git in [
            "C:/Git/cmd/git.exe",
            "C:/Git/bin/git.exe",
            "C:/Git/mingw64/bin/git.exe",
        ] {
            assert_eq!(
                git_bash_beside(Path::new(git), exists),
                Some(PathBuf::from("C:/Git/bin/bash.exe")),
                "{git}"
            );
        }
        assert_eq!(
            git_bash_beside(Path::new("C:/Windows/System32/git.exe"), exists),
            None
        );
    }
}
