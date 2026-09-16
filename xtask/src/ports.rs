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
//! The scripts need a Linux host with gcc and the kernel's UAPI headers, as
//! `build.sh` for busybox does. There is no Windows build of them yet.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths::Arch;
use crate::{Error, Result, cargo};

/// The ports `cargo xtask ports` builds, in order, each a directory of
/// `ferrousli/tools/ports/` holding a `build.sh`. `libcxx` is the C++ runtime
/// btop links against, and installs nothing an image carries.
const PORTS: &[&str] = &["curl", "libcxx", "btop"];

/// A file a port installs, and where it goes in the initramfs.
pub(crate) struct Installed {
    /// The path in the archive, which is also its path under `x86_64/`.
    pub(crate) path: &'static str,
    /// Its permissions.
    pub(crate) mode: u32,
    /// The port that installs it, named when it is missing.
    port: &'static str,
}

/// Every file the ports install that an image carries.
pub(crate) const FILES: &[Installed] = &[
    Installed {
        path: "bin/curl",
        mode: 0o755,
        port: "curl",
    },
    Installed {
        path: "etc/ssl/certs/ca-certificates.crt",
        mode: 0o644,
        port: "curl",
    },
    Installed {
        path: "usr/libexec/ferrix/ssl_server2",
        mode: 0o755,
        port: "curl",
    },
    Installed {
        path: "usr/share/ferrix/tls-test/server5.crt",
        mode: 0o644,
        port: "curl",
    },
    Installed {
        path: "usr/share/ferrix/tls-test/server5.key",
        mode: 0o644,
        port: "curl",
    },
    Installed {
        path: "usr/share/ferrix/tls-test/test-ca2.crt",
        mode: 0o644,
        port: "curl",
    },
    Installed {
        path: "bin/btop",
        mode: 0o755,
        port: "btop",
    },
];

/// A file of [`FILES`] that is installed, with its bytes.
pub(crate) struct File {
    pub(crate) path: &'static str,
    pub(crate) mode: u32,
    pub(crate) bytes: Vec<u8>,
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
        match std::fs::read(&path) {
            Ok(bytes) => files.push(File {
                path: file.path,
                mode: file.mode,
                bytes,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !missing.contains(&file.port) {
                    missing.push(file.port);
                }
            }
            Err(error) => {
                return Err(Error::new(format!("reading {}: {error}", path.display())));
            }
        }
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
            assert!(ferrix_cpio::is_safe_path(file.path), "{}", file.path);
        }
    }

    #[test]
    fn only_x86_64_carries_ports() {
        for arch in [Arch::AArch64, Arch::Armv7a] {
            assert!(installed(arch).unwrap().is_empty(), "{arch}");
            assert!(build(arch).is_err(), "{arch}");
        }
    }
}
