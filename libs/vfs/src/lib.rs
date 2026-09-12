//! The virtual filesystem: names, mounts, open files and descriptor tables.
//!
//! Stage 8 of `docs/ROADMAP.md`. `docs/ARCHITECTURE.md` §8 puts the VFS in the
//! kernel — an inode cache, a dentry cache with negative entries, a mount
//! table per mount namespace — and this crate is all of that which does not
//! need the machine. It knows nothing about page tables, user pointers,
//! processes or the console, which is what lets `cargo test`, Miri and a
//! fuzzer walk the same path resolution the kernel does.
//!
//! # The pieces
//!
//! * [`Inode`] — what a filesystem implements: one object per file, directory,
//!   link or device, answering in Linux's own error numbers because those are
//!   what the program on the far side of a system call will be told.
//! * [`Dentry`] — a *name*, as opposed to the thing it names. The dentry cache
//!   is what makes `..`, `getcwd`, mount points and `/proc/self/fd` possible:
//!   an inode has no idea what it is called, and a hard link means it is
//!   called several things.
//! * [`Namespace`] — a mount table with a root, and every operation that
//!   takes a path: [`Namespace::open`], [`Namespace::mkdir`],
//!   [`Namespace::rename`] and the rest. One exists in the kernel today;
//!   stage 13's mount namespaces are more of the same type rather than a
//!   rewrite of a global.
//! * [`OpenFile`] — an open file *description*: the offset and the status
//!   flags, shared by every descriptor `dup` or `fork` made from one `open`.
//! * [`fd::FdTable`] — the descriptor numbers, and the close-on-exec bit that
//!   belongs to the number rather than to the description.
//! * [`tmpfs`] — the first filesystem, over a page store the kernel supplies.
//! * [`initramfs`] — unpacking a cpio archive into a namespace.
//! * [`dirent`] — packing directory entries in `getdents64`'s layout.
//! * [`pipe`] — the buffer behind a pipe, and the rules at its edges.
//! * [`statfs`] — packing a `statfs` answer in the three layouts it has.
//!
//! # What is deliberately absent
//!
//! **Permission checks.** Everything runs as root until credentials exist, and
//! root passes every discretionary check except execute, which is the loader's
//! question rather than this crate's. The owner and mode are stored and
//! reported faithfully, so adding the check later is a function call on the
//! walk rather than a change to what is stored.
//!
//! **Blocking.** Every lock here is a spin lock and no filesystem in this
//! crate sleeps. A filesystem that does I/O — stage 11's btrfs — must not hold
//! one of these across it, which the [`Inode`] contract says and the path walk
//! respects: it never holds a dentry lock while calling into a filesystem.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod dentry;
pub mod dirent;
pub mod fd;
mod file;
pub mod initramfs;
mod namespace;
mod node;
pub mod path;
pub mod pipe;
pub mod statfs;
pub mod tmpfs;
mod walk;

#[cfg(test)]
mod tests;

pub use dentry::Dentry;
pub use ferrix_linux_abi::errno::Errno;
pub use file::{OpenFile, OpenFlags, Whence};
pub use namespace::{Context, DEFAULT_CACHE, Location, Mount, Namespace, RenameMode, Stat};
pub use node::{
    Clock, DirEntry, FIRST_CURSOR, FileSystem, FileType, Inode, Metadata, NewNode, Readiness,
    SetAttributes, StatFs, Timespec,
};

/// The result every operation here returns: a value, or the error number a
/// program would be given.
pub type Result<T> = core::result::Result<T, Errno>;
