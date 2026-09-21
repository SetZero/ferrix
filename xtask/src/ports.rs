//! Programs ported onto ferrousli beside busybox: built by the scripts in
//! `ferrousli/tools/ports/`, installed under `~/.local/share/ferrix/ports/ferrousli`
//! (or `$FERRIX_PORTS`), and carried by every image that carries a busybox.
//!
//! Each port installs into `x86_64/` there with the layout it has on the guest,
//! so `x86_64/bin/curl` is `/bin/curl` and `x86_64/etc/ssl/certs/…` is
//! `/etc/ssl/certs/…`. An image takes whichever of [`FILES`] are installed and
//! says which are not: a port is a download and a C build of minutes, which
//! `build` and `run` do not start on their own. `cargo xtask ports` does.
//!
//! An entry is a file or a whole tree. A tree, such as git's
//! `usr/libexec/git-core`, is walked in name order so the archive is the same
//! bytes every time, and its symbolic links go in as links. A file's
//! permissions are 0755 when it starts as an ELF program or a `#!` script and
//! 0644 otherwise, rather than read from the build host, so a tree copied to a
//! Windows machine gives the same archive.
//!
//! The scripts need a Linux host with gcc and the kernel's UAPI headers, as
//! `build.sh` for busybox does. There is no Windows build of them yet.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::Arch;
use crate::{Error, Result, cargo};

/// The ports `cargo xtask ports` builds, in order, each a directory of
/// `ferrousli/tools/ports/` holding a `build.sh`. `libcxx` is the C++ runtime
/// btop links against, and installs nothing an image carries. `sshdt` is Rust
/// rather than C, built the way uutils is, and needs cargo's crates.io.
const PORTS: &[&str] = &["curl", "libcxx", "btop", "zlib", "git", "sshdt"];

/// Whether an [`Installed`] entry is one path or everything beneath it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// One file, or one symbolic link.
    File,
    /// A directory and everything in it.
    Tree,
}

/// A file or tree a port installs, and where it goes in the initramfs.
pub(crate) struct Installed {
    /// The path in the archive, which is also its path under `x86_64/`.
    pub(crate) path: &'static str,
    /// Its permissions, for a [`Kind::File`] that is not a link.
    pub(crate) mode: u32,
    /// One path or a tree.
    pub(crate) kind: Kind,
    /// The port that installs it, named when it is missing.
    port: &'static str,
}

/// Every file the ports install that an image carries.
pub(crate) const FILES: &[Installed] = &[
    Installed {
        path: "bin/curl",
        mode: 0o755,
        kind: Kind::File,
        port: "curl",
    },
    Installed {
        path: "etc/ssl/certs/ca-certificates.crt",
        mode: 0o644,
        kind: Kind::File,
        port: "curl",
    },
    Installed {
        path: "usr/libexec/ferrix/ssl_server2",
        mode: 0o755,
        kind: Kind::File,
        port: "curl",
    },
    Installed {
        path: "usr/share/ferrix/tls-test/server5.crt",
        mode: 0o644,
        kind: Kind::File,
        port: "curl",
    },
    Installed {
        path: "usr/share/ferrix/tls-test/server5.key",
        mode: 0o644,
        kind: Kind::File,
        port: "curl",
    },
    Installed {
        path: "usr/share/ferrix/tls-test/test-ca2.crt",
        mode: 0o644,
        kind: Kind::File,
        port: "curl",
    },
    Installed {
        path: "bin/btop",
        mode: 0o755,
        kind: Kind::File,
        port: "btop",
    },
    Installed {
        path: "bin/git",
        mode: 0o755,
        kind: Kind::File,
        port: "git",
    },
    Installed {
        path: "usr/bin/git",
        mode: 0o755,
        kind: Kind::File,
        port: "git",
    },
    Installed {
        path: "usr/libexec/git-core",
        mode: 0o755,
        kind: Kind::Tree,
        port: "git",
    },
    Installed {
        path: "usr/share/git-core",
        mode: 0o755,
        kind: Kind::Tree,
        port: "git",
    },
    Installed {
        path: "bin/sshdt",
        mode: 0o755,
        kind: Kind::File,
        port: "sshdt",
    },
];

/// What an installed path is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Content {
    /// A regular file's bytes.
    Bytes(Vec<u8>),
    /// A symbolic link's target.
    Link(String),
    /// A directory, which the archive makes before what is in it.
    Directory,
}

/// A path of [`FILES`] that is installed, with what is there.
#[derive(Debug)]
pub(crate) struct File {
    pub(crate) path: String,
    pub(crate) mode: u32,
    pub(crate) content: Content,
}

/// The permissions an installed regular file is given: executable when it
/// starts as an ELF program or a script.
fn mode_of(bytes: &[u8]) -> u32 {
    if bytes.starts_with(b"\x7fELF") || bytes.starts_with(b"#!") {
        0o755
    } else {
        0o644
    }
}

/// Read what is at `path` on the host, the archive path `name`, and, for a
/// directory walked as a tree, everything beneath it in name order.
fn read_entry(path: &Path, name: &str, mode: u32, walk: bool, out: &mut Vec<File>) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path)
            .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))?;
        let target = target.to_string_lossy().replace('\\', "/");
        out.push(File {
            path: name.to_owned(),
            mode: 0o777,
            content: Content::Link(target),
        });
    } else if meta.is_dir() {
        if !walk {
            return Err(Error::new(format!("{} is a directory", path.display())));
        }
        out.push(File {
            path: name.to_owned(),
            mode: 0o755,
            content: Content::Directory,
        });
        let mut children: Vec<_> = std::fs::read_dir(path)
            .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<_>>()?;
        children.sort();
        for child in children {
            read_entry(
                &path.join(&child),
                &format!("{name}/{child}"),
                mode,
                true,
                out,
            )?;
        }
    } else {
        let bytes = std::fs::read(path)
            .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))?;
        let mode = if walk { mode_of(&bytes) } else { mode };
        out.push(File {
            path: name.to_owned(),
            mode,
            content: Content::Bytes(bytes),
        });
    }
    Ok(())
}

/// The directory the ports are installed under.
fn root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("FERRIX_PORTS") {
        return Ok(PathBuf::from(dir));
    }
    std::env::home_dir()
        .map(|home| {
            [".local", "share", "ferrix", "ports", "ferrousli"]
                .iter()
                .fold(home, |dir, name| dir.join(name))
        })
        .ok_or_else(|| Error::new("no home directory to find the ports under; set FERRIX_PORTS"))
}

/// Where `file` is installed for `arch` beneath `root`.
fn installed_path(root: &Path, arch: Arch, file: &Installed) -> PathBuf {
    file.path
        .split('/')
        .fold(root.join(arch.name()), |dir, name| dir.join(name))
}

/// The installed files for `arch`, for an image to carry, and a line naming
/// the ports that are not there. Ports are built for `x86_64` only, as
/// ferrousli's busybox is, so every other architecture carries none.
pub(crate) fn installed(arch: Arch) -> Result<Vec<File>> {
    if arch != Arch::X86_64 {
        return Ok(Vec::new());
    }
    let root = root()?;
    let mut files = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    for file in FILES {
        let path = installed_path(&root, arch, file);
        if std::fs::symlink_metadata(&path).is_err() {
            if !missing.contains(&file.port) {
                missing.push(file.port);
            }
            continue;
        }
        read_entry(
            &path,
            file.path,
            file.mode,
            file.kind == Kind::Tree,
            &mut files,
        )?;
    }
    if !missing.is_empty() {
        println!(
            "  not built, so not in the image: {} (cargo xtask ports builds them)",
            missing.join(", ")
        );
    }
    Ok(files)
}

/// `cargo xtask ports`: run every port's `build.sh`, in order, stopping at the
/// first that fails.
pub(crate) fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Err(Error::new(format!(
            "the ports are built for x86_64 only, not for {arch}"
        )));
    }
    if cfg!(windows) {
        return Err(Error::new(
            "the ports' build scripts need a Linux host with gcc and the kernel's UAPI headers",
        ));
    }
    let root = root()?;
    let ferrousli = crate::paths::workspace_root().join("ferrousli");
    for port in PORTS {
        let mut command = Command::new("bash");
        let _ = command
            .current_dir(&ferrousli)
            .arg(format!("tools/ports/{port}/build.sh"))
            .env("FERRIX_PORTS", &root);
        // ferrousli gets a directory of its own inside the caller's target
        // directory, as it does for busybox.
        match std::env::var_os("CARGO_TARGET_DIR").filter(|dir| !dir.is_empty()) {
            Some(dir) => {
                let _ = command.env("CARGO_TARGET_DIR", Path::new(&dir).join("ferrousli"));
            }
            None => {
                let _ = command.env_remove("CARGO_TARGET_DIR");
            }
        }
        cargo::run(command, &format!("ferrousli/tools/ports/{port}/build.sh"))?;
    }
    println!("\nbuilt {} under {}", PORTS.join(", "), root.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_files_keep_their_guest_layout() {
        let path = installed_path(Path::new("root"), Arch::X86_64, &FILES[1]);
        assert_eq!(
            path,
            Path::new("root/x86_64/etc/ssl/certs/ca-certificates.crt")
        );
    }

    #[test]
    fn every_file_belongs_to_a_port_that_is_built() {
        for file in FILES {
            assert!(PORTS.contains(&file.port), "{}", file.path);
            assert!(!file.path.ends_with('/'), "{}", file.path);
            assert!(ferrix_cpio::is_safe_path(file.path), "{}", file.path);
        }
    }

    #[test]
    fn a_tree_is_read_in_name_order_with_its_links_and_modes() {
        let dir = std::env::temp_dir().join(format!("xtask-ports-tree-{}", std::process::id()));
        let tree = dir.join("git-core");
        std::fs::create_dir_all(tree.join("mergetools")).unwrap();
        std::fs::write(tree.join("git-sh-setup"), b"#!/bin/sh\n").unwrap();
        std::fs::write(tree.join("b-data"), b"plain").unwrap();
        std::fs::write(tree.join("mergetools").join("vimdiff"), b"# sourced\n").unwrap();
        let mut files = Vec::new();
        read_entry(&tree, "usr/libexec/git-core", 0o755, true, &mut files).unwrap();
        let names: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            names,
            [
                "usr/libexec/git-core",
                "usr/libexec/git-core/b-data",
                "usr/libexec/git-core/git-sh-setup",
                "usr/libexec/git-core/mergetools",
                "usr/libexec/git-core/mergetools/vimdiff",
            ]
        );
        assert_eq!(files[0].content, Content::Directory);
        assert_eq!(files[1].mode, 0o644);
        assert_eq!(files[2].mode, 0o755);
        assert_eq!(mode_of(b"\x7fELF\x02"), 0o755);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_x86_64_carries_ports() {
        for arch in [Arch::AArch64, Arch::Armv7a] {
            assert!(installed(arch).unwrap().is_empty(), "{arch}");
            assert!(build(arch).is_err(), "{arch}");
        }
    }
}
