//! The interface files of a cgroup that belong to cgroup itself, not to a
//! controller.
//!
//! Linux's `cgroup_base_files`, in its order, with the ones it marks
//! `CFTYPE_NOT_ON_ROOT` marked here too: the root has no type, no events, no
//! freeze and no kill, because nothing can be above it to judge them by and
//! killing it would kill the machine. A controller's files (`memory.max` and
//! the rest) are its landing's, and are listed beside it.

/// One file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File {
    /// Its name in the cgroup's directory.
    pub name: &'static str,
    /// What it is.
    pub kind: Kind,
    /// Whether the root cgroup has it.
    pub on_root: bool,
    /// Whether it can be written; `false` means read-only, mode 0444.
    pub writable: bool,
}

/// Which file a [`File`] is, for the kernel to dispatch on without comparing
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `cgroup.type`.
    Type,
    /// `cgroup.procs`.
    Procs,
    /// `cgroup.threads`.
    Threads,
    /// `cgroup.controllers`.
    Controllers,
    /// `cgroup.subtree_control`.
    SubtreeControl,
    /// `cgroup.events`.
    Events,
    /// `cgroup.max.descendants`.
    MaxDescendants,
    /// `cgroup.max.depth`.
    MaxDepth,
    /// `cgroup.stat`.
    Stat,
    /// `cgroup.freeze`.
    Freeze,
    /// `cgroup.kill`, which is write-only on Linux: mode 0200.
    Kill,
}

/// Every file, in Linux's order.
pub const FILES: &[File] = &[
    file("cgroup.type", Kind::Type, false, true),
    file("cgroup.procs", Kind::Procs, true, true),
    file("cgroup.threads", Kind::Threads, true, true),
    file("cgroup.controllers", Kind::Controllers, true, false),
    file("cgroup.subtree_control", Kind::SubtreeControl, true, true),
    file("cgroup.events", Kind::Events, false, false),
    file("cgroup.max.descendants", Kind::MaxDescendants, true, true),
    file("cgroup.max.depth", Kind::MaxDepth, true, true),
    file("cgroup.stat", Kind::Stat, true, false),
    file("cgroup.freeze", Kind::Freeze, false, true),
    file("cgroup.kill", Kind::Kill, false, true),
];

/// A table entry.
const fn file(name: &'static str, kind: Kind, on_root: bool, writable: bool) -> File {
    File {
        name,
        kind,
        on_root,
        writable,
    }
}

/// The files a cgroup has: every one, or the root's.
pub fn of(root: bool) -> impl Iterator<Item = &'static File> {
    FILES.iter().filter(move |file| file.on_root || !root)
}

/// The file called `name` in a cgroup, if it has one.
pub fn named(name: &[u8], root: bool) -> Option<&'static File> {
    of(root).find(|file| file.name.as_bytes() == name)
}

impl File {
    /// Its permission bits: Linux's 0644 for a writable file, 0444 for a
    /// read-only one, and 0200 for `cgroup.kill`, which cannot be read.
    pub fn mode(&self) -> u16 {
        match (self.kind, self.writable) {
            (Kind::Kill, _) => 0o200,
            (_, true) => 0o644,
            (_, false) => 0o444,
        }
    }
}
