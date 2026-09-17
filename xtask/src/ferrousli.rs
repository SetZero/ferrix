//! What the programs built against ferrousli share: where they are installed,
//! when they are stale, and how their build scripts are run.
//!
//! Two of them now — busybox in [`crate::busybox`] and uutils/coreutils in
//! [`crate::uutils`] — and they are built the same way: a script under
//! `ferrousli/tools/` downloads pinned sources, builds `libferrousli.a` and
//! `crt1.o`, links a static x86-64 program against those and nothing from the
//! host's C library, and installs it under a directory outside the repository.
//! A link that fails writes the symbols ferrousli has not got yet beside the
//! build and exits 1.
//!
//! busybox is on its way out (`docs/UUTILS.md`); this module is what stays
//! when it goes.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{Error, Result};

/// The list a failed link leaves beside the build, one symbol per line.
pub(crate) const UNDEFINED: &str = "undefined-symbols.txt";

/// The directory a program is installed under: `$var` when it is set, and
/// otherwise `segments` under the home directory.
pub(crate) fn install_root(var: &str, segments: &[&str], what: &str) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(var) {
        return Ok(PathBuf::from(dir));
    }
    std::env::home_dir()
        .map(|home| segments.iter().fold(home, |dir, name| dir.join(name)))
        .ok_or_else(|| Error::new(format!("no home directory to find {what} under; set {var}")))
}

/// The newest modification time of any file under `path`, a file or a
/// directory; `None` if there is nothing there.
pub(crate) fn newest(path: &Path) -> Option<SystemTime> {
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

/// Why the program at `program` must be built before it is used, or `None`
/// when it is newer than every `input` under `ferrousli`.
pub(crate) fn stale(program: &Path, ferrousli: &Path, inputs: &[&str]) -> Option<&'static str> {
    let Some(built) = std::fs::metadata(program)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|meta| meta.modified().ok())
    else {
        return Some("is not built");
    };
    let sources = inputs
        .iter()
        .filter_map(|input| newest(&ferrousli.join(input)))
        .max();
    sources
        .is_some_and(|sources| sources > built)
        .then_some("is older than ferrousli's sources")
}

/// Hold the install directory's build lock until the file is dropped.
///
/// Every checkout on the machine builds into the one directory, removing and
/// unpacking the sources as it goes, so two builds at once break each other;
/// this makes the second wait for the first. `what` names the build in the
/// line printed while waiting.
pub(crate) fn lock_builds(root: &Path, what: &str) -> Result<std::fs::File> {
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
            "  waiting for another {what} build to finish ({})",
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

/// Git for Windows' bash, which runs the `build-windows.sh` scripts.
///
/// Not the first `bash` on `PATH`: on Windows that is often WSL's launcher in
/// `System32`. And Git's launcher rather than `usr/bin/bash.exe` itself,
/// because the launcher puts Git's POSIX tools on the shell's `PATH`.
pub(crate) fn git_bash() -> Result<PathBuf> {
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

/// The target directory ferrousli is built in for a caller whose
/// `CARGO_TARGET_DIR` is `caller`: `ferrousli` inside it, spelled with the
/// forward slashes the build scripts' bash expects, or `None` when the caller
/// has none.
///
/// A caller's target directory is shared by every workspace it builds, so
/// ferrousli gets a directory of its own inside it rather than mixing its
/// crates with the caller's.
pub(crate) fn target_dir(caller: Option<std::ffi::OsString>) -> Option<String> {
    let caller = caller.filter(|dir| !dir.is_empty())?;
    let dir = Path::new(&caller).join("ferrousli");
    Some(dir.to_string_lossy().replace('\\', "/"))
}

/// Point a build script's command at `ferrousli/`, with the target directory
/// [`target_dir`] gives.
pub(crate) fn in_ferrousli(command: &mut std::process::Command, ferrousli: &Path) {
    let _ = command.current_dir(ferrousli);
    match target_dir(std::env::var_os("CARGO_TARGET_DIR")) {
        Some(dir) => {
            let _ = command.env("CARGO_TARGET_DIR", dir);
        }
        None => {
            let _ = command.env_remove("CARGO_TARGET_DIR");
        }
    }
}

/// The refusal for a link that failed on `symbols`, the list at `list`, for
/// the program called `program`.
pub(crate) fn not_linked(program: &str, symbols: &str, list: &Path) -> Error {
    let names: Vec<&str> = symbols.lines().filter(|line| !line.is_empty()).collect();
    Error::new(format!(
        "{program} does not link against ferrousli yet: {} undefined ({}), listed in {}",
        names.len(),
        names.join(", "),
        list.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ferrousli_builds_inside_the_callers_target_dir() {
        assert_eq!(target_dir(None), None);
        assert_eq!(target_dir(Some("".into())), None);
        assert_eq!(
            target_dir(Some("/home/u/.local/share/ferrix/target-os-05".into())).as_deref(),
            Some("/home/u/.local/share/ferrix/target-os-05/ferrousli")
        );
    }

    #[test]
    fn a_program_older_than_ferrousli_is_rebuilt() {
        let inputs = ["src"];
        let dir =
            std::env::temp_dir().join(format!("xtask-ferrousli-stale-{}", std::process::id()));
        let ferrousli = dir.join("ferrousli");
        std::fs::create_dir_all(ferrousli.join("src")).unwrap();
        let program = dir.join("program");
        assert_eq!(stale(&program, &ferrousli, &inputs), Some("is not built"));

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
        assert_eq!(stale(&program, &ferrousli, &inputs), None);

        std::fs::File::options()
            .write(true)
            .open(&program)
            .unwrap()
            .set_modified(old - std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            stale(&program, &ferrousli, &inputs),
            Some("is older than ferrousli's sources")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_link_names_what_is_missing() {
        let error = not_linked("busybox", "crypt\nscanf\n\n", Path::new("list.txt")).to_string();
        assert!(error.contains("2 undefined (crypt, scanf)"), "{error}");
        assert!(error.contains("list.txt"), "{error}");
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
