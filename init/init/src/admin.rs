//! What `svc enable`, `disable`, `mask`, `unmask` and `set-property` change
//! on disk (§4.4, §10), as systemd's `systemctl` changes it: links and
//! drop-ins in `/etc/ferrix/units`, or in `/run/ferrix/units` for what is
//! not meant to outlive the boot. The caller reloads the units afterwards.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use ferrix_svc::source::{Layer, Source};

/// The administrator's directory.
fn admin() -> PathBuf {
    PathBuf::from(Layer::Admin.directory())
}

/// A unit name that names one file and no directory.
fn plain(name: &str) -> io::Result<&str> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a unit name"),
        ));
    }
    Ok(name)
}

/// Make `link` point at `target`, replacing a link that is there.
fn link(target: &Path, link: &Path) -> io::Result<()> {
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(link).is_ok_and(|meta| meta.file_type().is_symlink()) {
        fs::remove_file(link)?;
    }
    symlink(target, link)
}

/// `svc enable`: a link for each `WantedBy=`, `RequiredBy=` and `Alias=`
/// of the unit's `[Install]`. Returns what was made, for the client.
pub(crate) fn enable(source: &Source, name: &str) -> io::Result<Vec<String>> {
    let name = plain(name)?;
    let unit = source
        .load(name)
        .map_err(|why| io::Error::new(io::ErrorKind::NotFound, format!("{name}: {why:?}")))?;
    let Some(fragment) = unit.fragment.as_deref() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} has no file to link to"),
        ));
    };
    let target = PathBuf::from(fragment);
    let install = &unit.install;
    if install.wanted_by.is_empty() && install.required_by.is_empty() && install.alias.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} has no [Install] section to enable it by"),
        ));
    }
    let mut made = Vec::new();
    let links = install
        .wanted_by
        .iter()
        .map(|by| admin().join(format!("{}.wants", by.as_str())).join(name))
        .chain(
            install
                .required_by
                .iter()
                .map(|by| admin().join(format!("{}.requires", by.as_str())).join(name)),
        )
        .chain(
            install
                .alias
                .iter()
                .map(|alias| admin().join(alias.as_str())),
        );
    for path in links {
        link(&target, &path)?;
        made.push(format!(
            "Created symlink {} -> {}",
            path.display(),
            target.display()
        ));
    }
    Ok(made)
}

/// `svc disable`: remove every link to the unit in the administrator's
/// `.wants/` and `.requires/` directories, and its aliases there.
pub(crate) fn disable(name: &str) -> io::Result<Vec<String>> {
    let name = plain(name)?;
    let mut removed = Vec::new();
    let Ok(entries) = fs::read_dir(admin()) else {
        return Ok(removed);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file = entry.file_name().to_string_lossy().into_owned();
        let is_dependency = file.ends_with(".wants") || file.ends_with(".requires");
        if is_dependency && path.is_dir() {
            let candidate = path.join(name);
            if fs::symlink_metadata(&candidate).is_ok() {
                fs::remove_file(&candidate)?;
                removed.push(format!("Removed {}", candidate.display()));
            }
            continue;
        }
        let points_here = fs::read_link(&path)
            .ok()
            .and_then(|target| target.file_name().map(|n| n.to_string_lossy().into_owned()))
            .is_some_and(|target| target == name && file != name);
        if points_here {
            fs::remove_file(&path)?;
            removed.push(format!("Removed {}", path.display()));
        }
    }
    Ok(removed)
}

/// `svc mask`: a link to `/dev/null` under the unit's name, which hides it
/// whatever the other directories hold.
pub(crate) fn mask(name: &str) -> io::Result<Vec<String>> {
    let path = admin().join(plain(name)?);
    if fs::symlink_metadata(&path).is_ok_and(|meta| !meta.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is a file, not a link, and is left", path.display()),
        ));
    }
    link(Path::new("/dev/null"), &path)?;
    Ok(vec![format!(
        "Created symlink {} -> /dev/null",
        path.display()
    )])
}

/// `svc unmask`: remove that link.
pub(crate) fn unmask(name: &str) -> io::Result<Vec<String>> {
    let path = admin().join(plain(name)?);
    let masked = fs::read_link(&path).is_ok_and(|target| target == Path::new("/dev/null"));
    if !masked {
        return Ok(Vec::new());
    }
    fs::remove_file(&path)?;
    Ok(vec![format!("Removed {}", path.display())])
}

/// The resource keys `set-property` takes (§5.5).
const PROPERTIES: [&str; 6] = [
    "MemoryMax",
    "MemoryHigh",
    "TasksMax",
    "CPUWeight",
    "CPUQuota",
    "IOWeight",
];

/// `svc set-property`: write the assignments to a drop-in, in
/// `/etc/ferrix/units` when `persistent` and in `/run/ferrix/units`
/// otherwise, in the section the unit's kind reads them from.
pub(crate) fn set_property(
    name: &str,
    assignments: &[String],
    persistent: bool,
) -> io::Result<Vec<String>> {
    let name = plain(name)?;
    let section = match name.rsplit_once('.').map(|(_, suffix)| suffix) {
        Some("service") => "Service",
        Some("slice") => "Slice",
        Some("scope") => "Scope",
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name}: only a service, slice or scope has resource limits"),
            ));
        }
    };
    let mut text = format!("[{section}]\n");
    for assignment in assignments {
        let Some((key, value)) = assignment.split_once('=') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{assignment:?} is not Key=value"),
            ));
        };
        if !PROPERTIES.contains(&key) || value.contains('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{key} is not a property set-property takes ({})",
                    PROPERTIES.join(", ")
                ),
            ));
        }
        text.push_str(assignment);
        text.push('\n');
    }
    let layer = if persistent {
        Layer::Admin
    } else {
        Layer::Runtime
    };
    let directory = Path::new(layer.directory()).join(format!("{name}.d"));
    fs::create_dir_all(&directory)?;
    let path = directory.join("50-set-property.conf");
    // Earlier assignments stay unless this call sets the same key again:
    // the drop-in is read in order, so a later line wins.
    let mut whole = fs::read_to_string(&path).unwrap_or_default();
    if whole.is_empty() {
        whole = text;
    } else {
        whole.push_str(text.split_once('\n').map_or("", |(_, rest)| rest));
    }
    fs::write(&path, whole)?;
    Ok(vec![format!("Wrote {}", path.display())])
}
