//! Programs ported onto ferrousli beside busybox: built by the scripts in
//! `ferrousli/tools/ports/`, installed under `~/.local/share/ferrix/ports/ferrousli`
//! (or `$FERRIX_PORTS`), and carried by every image that carries a busybox.
//!
//! Each port installs into `x86_64/` there with the layout it has on the guest,
//! so `x86_64/bin/curl` is `/bin/curl` and `x86_64/etc/ssl/certs/…` is
//! `/etc/ssl/certs/…`.
//!
//! # Built when stale
//!
//! An image that carries the ports builds them first when they need it, as
//! `--init ferrousli` builds ferrousli's busybox, so what boots is the tree as
//! it stands. A port needs building when it has never been built, when a file
//! it installs is gone, or when anything it is built from is newer than its
//! last build: ferrousli's sources, `tools/ports/common.sh`, its own directory,
//! or a port it is built over. Each successful build leaves a `built` file in
//! the port's directory whose time is that comparison's other side. The first
//! build is minutes, the C++ runtime most of them; after that an image build
//! checks times and builds nothing. `cargo xtask ports` builds them all
//! regardless.
//!
//! # On Windows
//!
//! The scripts need gcc and the kernel's UAPI headers, so on Windows they run in
//! WSL's default distribution, on its own filesystem, under the same path they
//! use on Linux, and the files an image carries are then copied to this
//! machine's install directory. The `built` files live beside those copies.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::busybox::{INPUTS, lock_builds, newest};
use crate::paths::Arch;
use crate::{Error, Result, cargo};

/// A port: a directory of `ferrousli/tools/ports/` holding a `build.sh`, and
/// the ports it is built over.
struct Port {
    name: &'static str,
    after: &'static [&'static str],
}

/// The ports, in the order they are built. `libcxx` is the C++ runtime btop
/// links against, and installs nothing an image carries.
const PORTS: &[Port] = &[
    Port {
        name: "curl",
        after: &[],
    },
    Port {
        name: "libcxx",
        after: &[],
    },
    Port {
        name: "btop",
        after: &["libcxx"],
    },
];

/// What each port's successful build leaves in `<root>/<port>/`.
const STAMP: &str = "built";

/// A file a port installs, and where it goes in the initramfs.
pub(crate) struct Installed {
    /// The path in the archive, which is also its path under `x86_64/`.
    pub(crate) path: &'static str,
    /// Its permissions.
    pub(crate) mode: u32,
    /// The port that installs it.
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

/// The time `port` was last built, from its `built` file.
fn built_at(root: &Path, port: &str) -> Option<SystemTime> {
    std::fs::metadata(root.join(port).join(STAMP))
        .and_then(|meta| meta.modified())
        .ok()
}

/// Why `port` must be built before an image carries it, or `None` when its
/// last build is newer than everything it is built from.
fn stale(root: &Path, ferrousli: &Path, port: &Port) -> Option<String> {
    let Some(built) = built_at(root, port.name) else {
        return Some("is not built".to_owned());
    };
    let missing = FILES
        .iter()
        .filter(|file| file.port == port.name)
        .find(|file| !installed_path(root, Arch::X86_64, file).exists());
    if let Some(file) = missing {
        return Some(format!("has lost {}", file.path));
    }
    let sources = INPUTS
        .iter()
        .map(|input| ferrousli.join(input))
        .chain([
            ferrousli.join("tools/ports/common.sh"),
            ferrousli.join("tools/ports").join(port.name),
        ])
        .filter_map(|path| newest(&path))
        .max();
    if sources.is_some_and(|sources| sources > built) {
        return Some("is older than what it is built from".to_owned());
    }
    port.after
        .iter()
        .find(|after| built_at(root, after).is_none_or(|theirs| theirs > built))
        .map(|after| format!("is older than {after}, which it is built over"))
}

/// The installed files for `arch`, for an image to carry, built first where
/// they are stale. Ports are built for `x86_64` only, as ferrousli's busybox
/// is, so every other architecture carries none.
pub(crate) fn installed(arch: Arch) -> Result<Vec<File>> {
    if arch != Arch::X86_64 {
        return Ok(Vec::new());
    }
    let root = root()?;
    let ferrousli = crate::paths::workspace_root().join("ferrousli");
    if PORTS
        .iter()
        .any(|port| stale(&root, &ferrousli, port).is_some())
    {
        // Another checkout may be building into the same directory: wait for
        // it, then look again, since what it built may be current.
        let _lock = lock_builds(&root)?;
        for port in PORTS {
            if let Some(reason) = stale(&root, &ferrousli, port) {
                println!("  ports    {} {reason}; building it", port.name);
                build_port(&root, &ferrousli, port)?;
            }
        }
    }
    FILES
        .iter()
        .map(|file| {
            let path = installed_path(&root, arch, file);
            std::fs::read(&path)
                .map(|bytes| File {
                    path: file.path,
                    mode: file.mode,
                    bytes,
                })
                .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
        })
        .collect()
}

/// Run `port`'s `build.sh`, then record the build.
fn build_port(root: &Path, ferrousli: &Path, port: &Port) -> Result<()> {
    let description = format!("ferrousli/tools/ports/{}/build.sh", port.name);
    if cfg!(windows) {
        build_in_wsl(root, ferrousli, port, &description)?;
    } else {
        let mut command = Command::new("bash");
        let _ = command
            .current_dir(ferrousli)
            .arg(format!("tools/ports/{}/build.sh", port.name))
            .env("FERRIX_PORTS", root);
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
        cargo::run(command, &description)?;
    }
    let stamp = root.join(port.name).join(STAMP);
    std::fs::create_dir_all(root.join(port.name))
        .and_then(|()| std::fs::write(&stamp, b""))
        .map_err(|error| Error::new(format!("writing {}: {error}", stamp.display())))
}

/// The directory the scripts install into inside WSL: their own default.
const WSL_PORTS: &str = ".local/share/ferrix/ports/ferrousli";

/// [`build_port`] on Windows: the script in WSL, installing on WSL's own
/// filesystem, and the files an image carries copied back to `root`.
fn build_in_wsl(root: &Path, ferrousli: &Path, port: &Port, description: &str) -> Result<()> {
    crate::wsl::require_toolchain()?;
    let command = crate::wsl::bash(
        ferrousli,
        "export FERRIX_PORTS=\"$HOME/.local/share/ferrix/ports/ferrousli\"; \
         exec bash \"tools/ports/$1/build.sh\"",
        &[port.name],
    );
    cargo::run(command, description)?;
    let home = wsl_home()?;
    for file in FILES.iter().filter(|file| file.port == port.name) {
        let from = crate::wsl::path(&format!("{home}/{WSL_PORTS}/x86_64/{}", file.path))
            .ok_or_else(|| Error::new("WSL has no default distribution"))?;
        let to = installed_path(root, Arch::X86_64, file);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| Error::new(format!("creating {}: {error}", parent.display())))?;
        }
        let _ = std::fs::copy(&from, &to).map_err(|error| {
            Error::new(format!(
                "copying {} to {}: {error}",
                from.display(),
                to.display()
            ))
        })?;
    }
    Ok(())
}

/// The home directory of WSL's default user, as a Linux path.
fn wsl_home() -> Result<String> {
    let output = Command::new("wsl.exe")
        .args(["--exec", "bash", "-lc", "printf %s \"$HOME\""])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| Error::new(format!("could not run wsl.exe: {error}")))?;
    let home = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || !home.starts_with('/') {
        return Err(Error::new("could not read WSL's home directory"));
    }
    Ok(home)
}

/// `cargo xtask ports`: build every port, in order, stale or not, stopping at
/// the first that fails.
pub(crate) fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        return Err(Error::new(format!(
            "the ports are built for x86_64 only, not for {arch}"
        )));
    }
    let root = root()?;
    let ferrousli = crate::paths::workspace_root().join("ferrousli");
    let _lock = lock_builds(&root)?;
    for port in PORTS {
        build_port(&root, &ferrousli, port)?;
    }
    let names: Vec<&str> = PORTS.iter().map(|port| port.name).collect();
    println!("\nbuilt {} under {}", names.join(", "), root.display());
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
    fn every_file_and_dependency_names_a_port_built_before_it() {
        let names: Vec<&str> = PORTS.iter().map(|port| port.name).collect();
        for file in FILES {
            assert!(names.contains(&file.port), "{}", file.path);
            assert!(ferrix_cpio::is_safe_path(file.path), "{}", file.path);
        }
        for (at, port) in PORTS.iter().enumerate() {
            for after in port.after {
                let position = names.iter().position(|name| name == after);
                assert!(position.is_some_and(|position| position < at), "{after}");
            }
        }
    }

    #[test]
    fn only_x86_64_carries_ports() {
        for arch in [Arch::AArch64, Arch::Armv7a] {
            assert!(installed(arch).unwrap().is_empty(), "{arch}");
            assert!(build(arch).is_err(), "{arch}");
        }
    }

    /// Set `path`'s modification time to `seconds` ago.
    fn age(path: &Path, seconds: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - std::time::Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn a_port_is_stale_until_built_and_again_when_its_sources_or_its_base_change() {
        let dir = std::env::temp_dir().join(format!("xtask-ports-stale-{}", std::process::id()));
        let root = dir.join("ports");
        let ferrousli = dir.join("ferrousli");
        std::fs::create_dir_all(ferrousli.join("src")).unwrap();
        std::fs::create_dir_all(ferrousli.join("tools/ports/btop")).unwrap();
        let source = ferrousli.join("src/lib.rs");
        let script = ferrousli.join("tools/ports/btop/build.sh");
        std::fs::write(&source, "").unwrap();
        std::fs::write(&script, "").unwrap();
        age(&source, 600);
        age(&script, 600);
        let btop = &PORTS[2];
        assert_eq!(btop.name, "btop");

        assert_eq!(stale(&root, &ferrousli, btop).as_deref(), Some("is not built"));

        for port in ["libcxx", "btop"] {
            std::fs::create_dir_all(root.join(port)).unwrap();
            std::fs::write(root.join(port).join(STAMP), "").unwrap();
        }
        age(&root.join("libcxx").join(STAMP), 300);
        age(&root.join("btop").join(STAMP), 200);
        assert_eq!(
            stale(&root, &ferrousli, btop).as_deref(),
            Some("has lost bin/btop")
        );

        let binary = installed_path(&root, Arch::X86_64, &FILES[2]);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "").unwrap();
        assert_eq!(stale(&root, &ferrousli, btop), None);

        age(&root.join("libcxx").join(STAMP), 100);
        assert_eq!(
            stale(&root, &ferrousli, btop).as_deref(),
            Some("is older than libcxx, which it is built over")
        );

        age(&root.join("libcxx").join(STAMP), 300);
        age(&script, 50);
        assert_eq!(
            stale(&root, &ferrousli, btop).as_deref(),
            Some("is older than what it is built from")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
