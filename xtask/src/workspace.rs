//! Which workspace members the host can build, by a rule rather than a list.
//!
//! The loader, the kernel and the native programs are freestanding: a
//! `_start`, a panic handler, a linker script. `cargo test --all-targets` on
//! the host builds every binary as a test harness, which for one of them is a
//! second `panic_impl` and error E0152. So the host gates exclude them, and
//! which ones they are used to be a list in `check.rs` and another, older one
//! copied into CI. CI's went stale when `devmgr` joined the workspace, and its
//! test job went red while `cargo xtask check` stayed green.
//!
//! Now there is no list. A freestanding member is one at `boot`, at `kernel`
//! or under `user/`, read from the workspace manifest; `cargo xtask check` and
//! CI both ask this module, through the same commands. And a member anywhere
//! else whose `src/main.rs` says `#![no_main]` is refused, so a new program
//! put in the wrong place fails the gate on the machine that added it.

use std::fs;
use std::path::Path;

use crate::{Error, Result};

/// Where freestanding members live, relative to the workspace root.
const FREESTANDING_PLACES: &[&str] = &["boot", "kernel"];

/// The directory freestanding native programs live under.
const NATIVE_PLACE: &str = "user/";

/// The workspace's members, sorted by what the host can do with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Members {
    /// Package names the host builds, lints and tests.
    pub(crate) host: Vec<String>,
    /// The loader and the kernel, by package name.
    pub(crate) kernel_and_loader: Vec<String>,
    /// The native runtime and programs under `user/`, by package name.
    pub(crate) native: Vec<String>,
}

impl Members {
    /// Every member the host cannot build: the loader, the kernel and the
    /// native programs.
    pub(crate) fn freestanding(&self) -> impl Iterator<Item = &str> {
        self.kernel_and_loader
            .iter()
            .chain(&self.native)
            .map(String::as_str)
    }

    /// `--exclude <package>` for every freestanding member.
    pub(crate) fn excludes(&self) -> Vec<String> {
        self.freestanding()
            .flat_map(|package| ["--exclude".to_owned(), package.to_owned()])
            .collect()
    }
}

/// Read the workspace at `root` and sort its members.
///
/// # Errors
///
/// A manifest that cannot be read or has no `members` list, a member without
/// a package name, or a `#![no_main]` program outside the freestanding places.
pub(crate) fn members(root: &Path) -> Result<Members> {
    let manifest = read(&root.join("Cargo.toml"))?;
    let mut sorted = Members {
        host: Vec::new(),
        kernel_and_loader: Vec::new(),
        native: Vec::new(),
    };
    for member in member_paths(&manifest)? {
        let directory = root.join(&member);
        let name = package_name(&read(&directory.join("Cargo.toml"))?).ok_or_else(|| {
            Error::new(format!("workspace member `{member}` has no package name"))
        })?;
        if FREESTANDING_PLACES.contains(&member.as_str()) {
            sorted.kernel_and_loader.push(name);
        } else if member.starts_with(NATIVE_PLACE) {
            sorted.native.push(name);
        } else if is_freestanding_program(&directory) {
            return Err(Error::new(format!(
                "workspace member `{member}` is a freestanding program (its src/main.rs is \
                 #![no_main]) outside boot, kernel and user/. The host gates build every other \
                 member as a test harness, which fails for it; move it under user/"
            )));
        } else {
            sorted.host.push(name);
        }
    }
    Ok(sorted)
}

/// The strings in the manifest's `members = [ ... ]`, comments skipped.
fn member_paths(manifest: &str) -> Result<Vec<String>> {
    let start = manifest
        .find("members = [")
        .ok_or_else(|| Error::new("the workspace manifest has no `members = [` list"))?;
    let rest = manifest
        .get(start + "members = [".len()..)
        .unwrap_or_default();
    let end = rest
        .find(']')
        .ok_or_else(|| Error::new("the workspace manifest's members list is not closed"))?;
    let list = rest.get(..end).unwrap_or_default();
    Ok(list
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter_map(|item| {
            item.strip_prefix('"')
                .and_then(|item| item.strip_suffix('"'))
        })
        .map(str::to_owned)
        .collect())
}

/// The `name` of a member manifest's `[package]`.
fn package_name(manifest: &str) -> Option<String> {
    let package = manifest.split("[package]").nth(1)?;
    let section = package.split("\n[").next()?;
    section.lines().find_map(|line| {
        let value = line.trim().strip_prefix("name")?.trim().strip_prefix('=')?;
        let value = value.trim().strip_prefix('"')?.split('"').next()?;
        Some(value.to_owned())
    })
}

/// Whether the member's `src/main.rs` declares `#![no_main]`.
fn is_freestanding_program(directory: &Path) -> bool {
    fs::read_to_string(directory.join("src").join("main.rs"))
        .is_ok_and(|source| source.lines().any(|line| line.trim() == "#![no_main]"))
}

/// A file's contents, or an error naming it.
fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|error| Error::new(format!("{}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::{member_paths, members, package_name};
    use crate::paths;

    #[test]
    fn the_real_workspace_sorts_every_native_program_out_of_the_host_gates() {
        let sorted = members(&paths::workspace_root()).expect("the workspace sorts");
        for package in ["ferrix-kernel", "ferrix-boot"] {
            assert!(
                sorted.kernel_and_loader.iter().any(|name| name == package),
                "{package}"
            );
        }
        for package in ["ferrix-rt", "ferrix-devmgr", "ferrix-gpu", "ferrix-blk"] {
            assert!(
                sorted.native.iter().any(|name| name == package),
                "{package}"
            );
            assert!(!sorted.host.iter().any(|name| name == package), "{package}");
        }
        assert!(sorted.host.iter().any(|name| name == "xtask"));
        assert!(sorted.host.iter().any(|name| name == "ferrix-net"));
    }

    #[test]
    fn a_freestanding_program_outside_user_is_refused() {
        let root = std::env::temp_dir().join(format!("ferrix-workspace-{}", std::process::id()));
        let program = root.join("libs").join("stray");
        std::fs::create_dir_all(program.join("src")).expect("a scratch workspace");
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"libs/stray\"]\n",
        )
        .expect("the manifest");
        std::fs::write(program.join("Cargo.toml"), "[package]\nname = \"stray\"\n")
            .expect("the member manifest");
        std::fs::write(
            program.join("src").join("main.rs"),
            "#![no_std]\n#![no_main]\n",
        )
        .expect("the program");
        let answer = members(&root);
        let _ = std::fs::remove_dir_all(&root);
        let error = answer.expect_err("a #![no_main] program under libs/ is refused");
        assert!(error.to_string().contains("libs/stray"), "{error}");
    }

    #[test]
    fn members_are_read_past_comments_and_commas() {
        let manifest = "[workspace]\nmembers = [\n    # a comment, \"not\" a member\n    \"libs/a\",\n    \"user/b\", \"kernel\"\n]\n";
        assert_eq!(
            member_paths(manifest).expect("a list"),
            ["libs/a", "user/b", "kernel"]
        );
    }

    #[test]
    fn the_package_name_is_the_one_under_package() {
        let manifest = "[package]\nname = \"ferrix-thing\"\nversion.workspace = true\n\n[dependencies]\nname = \"not this\"\n";
        assert_eq!(package_name(manifest).as_deref(), Some("ferrix-thing"));
    }
}
