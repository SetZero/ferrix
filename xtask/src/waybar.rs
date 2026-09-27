//! waybar on the desktop: the files its boot carries.
//!
//! The boot runs `/bin/waybar` against a config of the tree's own
//! ([`BOOT_CONFIG`]: the user's bar, module for module, with each script
//! replaced by one that prints a fixed answer) and the user's own
//! stylesheet -- `~/.config/waybar/style.css` and the `icons/` beside it,
//! read from this machine when the image is built and never committed. A
//! machine without that file takes [`FALLBACK_STYLE`], and says so. The
//! fonts are the ones the stylesheet names, resolved here as
//! `run-compositor` resolves them (`crate::dotfiles`).
//!
//! Everything goes under [`HOME_DIR`] in the image, and into a directory on
//! this machine as well, where the `x86_64` build of the same program draws
//! the picture the guest's screen must show (`waybar --render`).

use std::path::{Path, PathBuf};

use crate::ports::{Content, File};
use crate::{Error, Result, paths};

/// The boot's config, in the tree.
const BOOT_CONFIG: &str = "userland/compositor/waybar/data/boot/config.jsonc";

/// The stylesheet a machine without the user's takes, in the tree.
const FALLBACK_STYLE: &str = "userland/compositor/waybar/data/boot/style.css";

/// Where the boot's files go in the image: under the desktop's `HOME`, `/`.
pub(crate) const HOME_DIR: &str = ".config/waybar-boot";

/// The output the config asks for, and the size of the screen.
pub(crate) const OUTPUT: &str = "Virtual-1";
/// The screen's size.
pub(crate) const SIZE: (u32, u32) = (1024, 768);

/// The colour hyprix clears to (`misc:background_color`'s default), which
/// the bar's translucent ground is laid over.
pub(crate) const GROUND: &str = "111111";

/// The boot's files: its config, the stylesheet with the icons beside it,
/// and whether the stylesheet is the user's.
///
/// # Errors
///
/// A file of the tree's, or of the user's that is there, that cannot be read.
pub(crate) fn files() -> Result<(Vec<File>, bool)> {
    let root = paths::workspace_root();
    let read = |path: &Path| {
        std::fs::read(path).map_err(|error| Error::new(format!("{}: {error}", path.display())))
    };
    let mut out = vec![File {
        path: format!("{HOME_DIR}/config.jsonc"),
        mode: 0o644,
        content: Content::Bytes(read(&root.join(BOOT_CONFIG))?),
    }];
    let users = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config/waybar"))
        .filter(|dir| dir.join("style.css").is_file());
    match &users {
        Some(dir) => {
            out.push(File {
                path: format!("{HOME_DIR}/style.css"),
                mode: 0o644,
                content: Content::Bytes(read(&dir.join("style.css"))?),
            });
            let icons = dir.join("icons");
            if icons.is_dir() {
                let mut names: Vec<_> = std::fs::read_dir(&icons)
                    .map_err(|error| Error::new(format!("{}: {error}", icons.display())))?
                    .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                    .collect();
                names.sort();
                for name in names {
                    let path = icons.join(&name);
                    if path.is_file() {
                        out.push(File {
                            path: format!("{HOME_DIR}/icons/{}", name.to_string_lossy()),
                            mode: 0o644,
                            content: Content::Bytes(read(&path)?),
                        });
                    }
                }
            }
        }
        None => out.push(File {
            path: format!("{HOME_DIR}/style.css"),
            mode: 0o644,
            content: Content::Bytes(read(&root.join(FALLBACK_STYLE))?),
        }),
    }
    Ok((out, users.is_some()))
}

/// The font families the carried stylesheet names.
#[must_use]
pub(crate) fn families(files: &[File]) -> Vec<String> {
    files
        .iter()
        .filter(|file| file.path.ends_with("/style.css"))
        .filter_map(|file| match &file.content {
            Content::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .flat_map(|text| crate::dotfiles::families_named("style.css", &text))
        .collect()
}

/// Write `files` under `dir` on this machine, each at its path less
/// `strip`, for the host's render to read.
///
/// # Errors
///
/// The directory or a file that cannot be written.
pub(crate) fn write_here(dir: &Path, files: &[File], strip: &str) -> Result<()> {
    for file in files {
        let Content::Bytes(bytes) = &file.content else {
            continue;
        };
        let relative = file.path.strip_prefix(strip).unwrap_or(&file.path);
        let target = dir.join(relative.trim_start_matches('/'));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| Error::new(format!("{}: {error}", parent.display())))?;
        }
        std::fs::write(&target, bytes)
            .map_err(|error| Error::new(format!("{}: {error}", target.display())))?;
    }
    Ok(())
}
