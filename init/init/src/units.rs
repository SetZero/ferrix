//! The three unit directories (§4.1), read into a [`Source`].
//!
//! `libs/svc` reads no directory: it takes each entry by its path relative
//! to its directory, as a file's bytes, a mask or an alias. This is the
//! walk that hands them over, one level deep, which is as deep as unit
//! directories go: the units themselves, and in `name.d/`, `name.wants/`
//! and `name.requires/` the drop-ins and dependency links.
//!
//! A symbolic link means what systemd makes it mean. To `/dev/null` it
//! masks. To a file under the link's own name, as `svc enable` links
//! `/etc/…/x.service` to `/lib/…/x.service`, it is that file, and its bytes
//! go in. To any other name it is an alias of that name. In `.wants/` and
//! `.requires/` only the link's own name counts, so what it points at is
//! passed on as an alias and not read.

use std::fs;
use std::path::Path;

use ferrix_svc::source::{Entry, Layer, Source};

/// Every unit in the three directories, with a line for each entry that is
/// not one: a stray file, or one that cannot be read.
pub(crate) fn read(log: &mut dyn FnMut(String)) -> Source {
    let mut source = Source::new();
    for layer in Layer::ALL {
        let directory = Path::new(layer.directory());
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() && !is_link(&path) {
                read_subdirectory(&mut source, layer, &name, &path, log);
                continue;
            }
            add(&mut source, layer, &name, &path, log);
        }
    }
    source
}

/// A `name.d/`, `name.wants/` or `name.requires/` directory.
fn read_subdirectory(
    source: &mut Source,
    layer: Layer,
    name: &str,
    directory: &Path,
    log: &mut dyn FnMut(String),
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        add(source, layer, &format!("{name}/{file}"), &entry.path(), log);
    }
}

/// Add one entry, at `relative` below `layer`'s directory.
fn add(
    source: &mut Source,
    layer: Layer,
    relative: &str,
    path: &Path,
    log: &mut dyn FnMut(String),
) {
    let own_name = relative.rsplit('/').next().unwrap_or(relative);
    let is_dependency = relative.contains(".wants/") || relative.contains(".requires/");
    let entry = match fs::read_link(path) {
        Ok(target) if target == Path::new("/dev/null") => Entry::Masked,
        Ok(target) => {
            let pointed = target
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if pointed == own_name && !is_dependency {
                match fs::read(path) {
                    Ok(bytes) => Entry::File(bytes),
                    Err(error) => {
                        log(format!("{}: {error}", path.display()));
                        return;
                    }
                }
            } else {
                Entry::Alias(pointed)
            }
        }
        Err(_) if is_dependency => Entry::Alias(own_name.to_owned()),
        Err(_) => match fs::read(path) {
            Ok(bytes) => Entry::File(bytes),
            Err(error) => {
                log(format!("{}: {error}", path.display()));
                return;
            }
        },
    };
    if let Err(error) = source.add(layer, relative, entry) {
        log(format!("{}: {error}", path.display()));
    }
}

/// Whether `path` is a symbolic link.
fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}
