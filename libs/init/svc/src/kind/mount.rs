//! `.mount`: a mount point, which init mounts and unmounts.
//!
//! As in systemd, a mount unit is named after the path it mounts, escaped
//! (`/sys/fs/cgroup` is `sys-fs-cgroup.mount`), and a `Where=` that does not
//! match the name refuses the unit: the name is how every other unit, and
//! the mount's own implied dependencies, find it.

use alloc::string::String;
use core::time::Duration;

use super::{Config, UnitError};
use crate::Warnings;
use crate::ini::Section;
use crate::keys::{self, Setter};
use crate::name::{UnitName, UnitType, unescape};
use crate::value::Span;

/// A mount's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// `What=`: the device, or the file system's source.
    pub what: String,
    /// `Where=`: the mount point, absolute.
    pub r#where: String,
    /// `Type=`: the file system; `None` lets the kernel tell.
    pub fs_type: Option<String>,
    /// `Options=`: the options, comma-separated, as `mount -o` takes them.
    pub options: String,
    /// `TimeoutSec=`.
    pub timeout: Span,
    /// `LazyUnmount=`.
    pub lazy_unmount: bool,
    /// `ForceUnmount=`.
    pub force_unmount: bool,
}

impl Default for Mount {
    fn default() -> Self {
        Self {
            what: String::new(),
            r#where: String::new(),
            fs_type: None,
            options: String::new(),
            timeout: Span::Finite(Duration::from_secs(90)),
            lazy_unmount: false,
            force_unmount: false,
        }
    }
}

/// The `[Mount]` keys.
const MOUNT_KEYS: [(&str, Setter<Mount>); 7] = [
    ("What", |m, a, _| m.what = a.value.clone()),
    ("Where", |m, a, w| {
        if let Some(path) = keys::path(a, w) {
            m.r#where = path.unwrap_or_default();
        }
    }),
    ("Type", |m, a, _| m.fs_type = keys::string(a)),
    ("Options", |m, a, _| m.options = a.value.clone()),
    ("TimeoutSec", |m, a, w| {
        if let Some(span) = keys::span(a, w) {
            m.timeout = super::service::zero_is_infinity(span);
        }
    }),
    ("LazyUnmount", |m, a, w| {
        m.lazy_unmount = keys::boolean(a, w).unwrap_or(m.lazy_unmount);
    }),
    ("ForceUnmount", |m, a, w| {
        m.force_unmount = keys::boolean(a, w).unwrap_or(m.force_unmount);
    }),
];

/// The mount kind.
pub(super) struct MountKind;

/// The `[Mount]` section, parsed and checked against the unit's name.
pub(super) fn parse(
    name: &UnitName,
    section: &Section,
    warnings: &mut Warnings,
) -> Result<Config, UnitError> {
    let mut mount = Mount::default();
    keys::apply(section, &MOUNT_KEYS, &mut mount, warnings);
    if mount.r#where.is_empty() {
        mount.r#where = if name.stem() == "-" {
            String::from("/")
        } else {
            let mut path = String::from("/");
            path.push_str(&unescape(name.stem()));
            path
        };
    }
    if UnitName::for_path(&mount.r#where, UnitType::Mount).as_ref() != Ok(name) {
        return Err(UnitError::new(
            "Where= setting doesn't match unit name. Refusing.",
        ));
    }
    if mount.what.is_empty() {
        return Err(UnitError::new("What= setting is missing. Refusing."));
    }
    Ok(Config::Mount(mount))
}
