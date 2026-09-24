//! Programs ported onto ferrousli beside busybox: built by the scripts in
//! `ferrousli/tools/ports/`, installed under `~/.local/share/ferrix/ports/ferrousli`
//! (or `$FERRIX_PORTS`), and carried by every image that carries a busybox.
//!
//! Each port installs into `<arch>/` there with the layout it has on the
//! guest, so `x86_64/bin/curl` is `/bin/curl` and `x86_64/etc/ssl/certs/…` is
//! `/etc/ssl/certs/…`. An image takes whichever of [`FILES`] are installed and
//! says which are not: a port is a download and a C build of minutes, which
//! `build` and `run` do not start on their own. `cargo xtask ports` does.
//!
//! x86-64 builds every port. AArch64 and ARMv7-A build the C ones git needs,
//! [`ARM_PORTS`], cross-compiled with the target's gcc: btop and its C++
//! runtime need the target's g++, and sshdt's build names its Rust target.
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

use crate::paths::Arch;
use crate::{Error, Result};

/// The ports `cargo xtask ports` builds, in order, each a directory of
/// `ferrousli/tools/ports/` holding a `build.sh`. `libcxx` is the C++ runtime
/// btop links against, and installs nothing an image carries. `sshdt` is Rust
/// rather than C, built the way uutils is, and needs cargo's crates.io.
/// `foot` is the Wayland terminal `docs/CHROME.md` starts from, built with
/// every library it links and the one font it draws with.
const PORTS: &[&str] = &["curl", "libcxx", "btop", "zlib", "git", "sshdt", "foot"];

/// The ports AArch64 and ARMv7-A build, in order: git and what it links.
const ARM_PORTS: &[&str] = &["curl", "zlib", "git"];

/// The ports `arch` builds and carries.
fn ports_for(arch: Arch) -> &'static [&'static str] {
    if arch == Arch::X86_64 {
        PORTS
    } else {
        ARM_PORTS
    }
}

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
    Installed {
        path: "bin/foot",
        mode: 0o755,
        kind: Kind::File,
        port: "foot",
    },
    Installed {
        path: "bin/footclient",
        mode: 0o755,
        kind: Kind::File,
        port: "foot",
    },
    Installed {
        path: "etc/fonts/fonts.conf",
        mode: 0o644,
        kind: Kind::File,
        port: "foot",
    },
    Installed {
        path: "usr/share/fonts/dejavu",
        mode: 0o755,
        kind: Kind::Tree,
        port: "foot",
    },
];

/// What an installed path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    /// A regular file's bytes.
    Bytes(Vec<u8>),
    /// A symbolic link's target.
    Link(String),
    /// A directory, which the archive makes before what is in it.
    Directory,
}

/// A path of [`FILES`] that is installed, with what is there.
#[derive(Debug, Clone)]
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
/// the ports that are not there, of those `arch` builds.
pub(crate) fn installed(arch: Arch) -> Result<Vec<File>> {
    installed_where(arch, |port| ports_for(arch).contains(&port))
}

/// The installed files of one port for `arch`, for a boot that wants that
/// program and not the megabytes of the others, and the same line when it is
/// not there.
pub(crate) fn installed_port(arch: Arch, port: &str) -> Result<Vec<File>> {
    installed_where(arch, |wanted| {
        wanted == port && ports_for(arch).contains(&port)
    })
}

/// The installed files of the ports `wanted` names.
fn installed_where(arch: Arch, wanted: impl Fn(&str) -> bool) -> Result<Vec<File>> {
    let root = root()?;
    let mut files = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    for file in FILES.iter().filter(|file| wanted(file.port)) {
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

/// `cargo xtask ports`: run the `build.sh` of every port `arch` builds, in
/// order, stopping at the first that fails.
pub(crate) fn build(arch: Arch) -> Result<()> {
    if cfg!(windows) {
        return Err(Error::new(
            "the ports' build scripts need a Linux host with gcc and the kernel's UAPI headers",
        ));
    }
    let root = root()?;
    let ferrousli = crate::paths::workspace_root().join("ferrousli");
    let ports = ports_for(arch);
    // One build of them all, in order, since each later port reads what an
    // earlier one installed: a build `FERRIX_BUILDS` may record or replay,
    // reading the sources the scripts would otherwise download from
    // `root/src`, and making the tree images carry, `root/<arch>`.
    let script = format!(
        "set -e\nfor port in {}; do bash \"tools/ports/$port/build.sh\" --arch {}; done",
        ports.join(" "),
        arch.name()
    );
    let mut build = crate::builds::Build::bash(
        format!("ferrousli/tools/ports ({}) for {arch}", ports.join(", ")),
        &ferrousli,
    )
    .args(["-c", &script])
    .env("FERRIX_PORTS", &root)
    .reads_dir(root.join("src"))
    .output(root.join(arch.name()));
    // ferrousli gets a directory of its own inside the caller's target
    // directory, as it does for busybox.
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR").filter(|dir| !dir.is_empty()) {
        build = build.env("CARGO_TARGET_DIR", Path::new(&dir).join("ferrousli"));
    }
    build.run()?;
    println!(
        "\nbuilt {} for {arch} under {}",
        ports.join(", "),
        root.display()
    );
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
    fn arm_builds_git_and_what_it_links_and_nothing_else() {
        for arch in [Arch::AArch64, Arch::Armv7a] {
            let ports = ports_for(arch);
            assert!(ports.contains(&"git"), "{arch}");
            // git links zlib and libcurl, which must be built first.
            let at = |port| ports.iter().position(|p| *p == port).unwrap();
            assert!(at("zlib") < at("git") && at("curl") < at("git"), "{arch}");
            for port in ["btop", "libcxx", "sshdt", "foot"] {
                assert!(!ports.contains(&port), "{arch} {port}");
            }
            assert!(ports.iter().all(|port| PORTS.contains(port)), "{arch}");
        }
        assert_eq!(ports_for(Arch::X86_64), PORTS);
    }
}
