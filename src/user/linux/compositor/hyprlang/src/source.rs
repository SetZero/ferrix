//! The paths `source = …` names, as hyprlock's and hypridle's
//! `handleSource` and `absolutePath` find them.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// `absolutePath(raw, dir)`: `~` made the home directory, a relative path
/// taken from `dir`, and the result `weakly_canonical`.
///
/// hyprlock drops the first two characters after `~` is seen, so `~/a` is
/// `$HOME/a` and, as upstream, `~user/a` is `$HOME/ser/a`.
pub(crate) fn absolute(raw: &str, dir: &Path, home: &str) -> PathBuf {
    let path = if raw.starts_with('~') {
        Path::new(home).join(raw.get(2..).unwrap_or_default())
    } else {
        PathBuf::from(raw)
    };
    let joined = if path.is_relative() {
        dir.join(path)
    } else {
        path
    };
    weakly_canonical(&joined)
}

/// `std::filesystem::weakly_canonical`: the longest part of `path` that
/// exists with its links resolved, the rest appended, `.` and `..` gone.
pub(crate) fn weakly_canonical(path: &Path) -> PathBuf {
    let normal = lexically_normal(path);
    if let Ok(canonical) = fs::canonicalize(&normal) {
        return canonical;
    }
    let mut head = normal.clone();
    let mut tail = Vec::new();
    while let Some(name) = head.file_name().map(ToOwned::to_owned) {
        let _ = head.pop();
        tail.push(name);
        if head.as_os_str().is_empty() {
            break;
        }
        if let Ok(mut canonical) = fs::canonicalize(&head) {
            for name in tail.iter().rev() {
                canonical.push(name);
            }
            return canonical;
        }
    }
    normal
}

/// `path` with `.` dropped and `..` taking back the name before it.
fn lexically_normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    let _ = out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `glob(pattern)` with `*` and `?` in its last component: the matching
/// names in that directory, sorted, a leading `.` matched only by a
/// pattern that has one. A pattern with neither is itself if it exists.
/// `None` is `GLOB_NOMATCH`.
pub(crate) fn glob(pattern: &Path) -> Option<Vec<PathBuf>> {
    let wildcard = pattern
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.contains(['*', '?']));
    let Some(name) = wildcard else {
        return fs::symlink_metadata(pattern)
            .ok()
            .map(|_| vec![pattern.to_path_buf()]);
    };
    let dir = pattern.parent().unwrap_or_else(|| Path::new(""));
    let listed = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let mut found: Vec<PathBuf> = fs::read_dir(listed)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let entry_name = entry.file_name();
            let entry_name = entry_name.to_str()?;
            let hidden = entry_name.starts_with('.') && !name.starts_with('.');
            (!hidden && matches(name, entry_name)).then(|| dir.join(entry_name))
        })
        .collect();
    found.sort();
    (!found.is_empty()).then_some(found)
}

/// Whether `name` matches `pattern`, where `*` is any run of characters
/// and `?` any one.
pub(crate) fn matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut at_pattern, mut at_name) = (0, 0);
    // The last `*` seen, and where in `name` it has taken up to.
    let mut star: Option<(usize, usize)> = None;
    while at_name < name.len() {
        match pattern.get(at_pattern) {
            Some('*') => {
                star = Some((at_pattern, at_name));
                at_pattern += 1;
            }
            Some(&want) if want == '?' || Some(&want) == name.get(at_name) => {
                at_pattern += 1;
                at_name += 1;
            }
            _ => {
                let Some((star_at, taken)) = star else {
                    return false;
                };
                at_pattern = star_at + 1;
                at_name = taken + 1;
                star = Some((star_at, taken + 1));
            }
        }
    }
    pattern
        .get(at_pattern..)
        .is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}
