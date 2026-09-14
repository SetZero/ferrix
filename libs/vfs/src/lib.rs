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
//! * [`socket`] — the buffer behind one direction of a socket: a byte stream
//!   or a run of records, with ancillary data kept at its boundaries.
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
//! **A sleeping lock.** Every lock here but one is a spin lock, and tmpfs
//! never sleeps, but an [`Inode`] implementation may block: stage 11's btrfs waits
//! on disk I/O inside `read_at`, `lookup` and `read_dir`. So the locks held
//! across a call into an inode are named exactly:
//!
//! * **None of an open file description's.** [`OpenFile`] copies the offset or
//!   the status flags out, calls the inode with neither held, and stores the
//!   result afterwards, which is why two `read`s racing on one description
//!   can get the same bytes (see `file.rs`).
//! * **No dentry lock and no mount-table or dentry-cache lock.** The path walk
//!   takes them to look at the cache and releases them before a `lookup`.
//! * **The namespace's rename lock**, across a `rename`'s two walks and the
//!   filesystem's `rename`. This is the one exception: the walks call
//!   `lookup`, so a filesystem whose lookups sleep sleeps holding it during
//!   a rename. It is therefore the one lock here that is not a spin lock, a
//!   [`ferrix_sync::SleepLock`]: the kernel lends it a wait queue through
//!   the [`ferrix_sync::Parker`] a [`Namespace`] is built with, so a walk
//!   that waits for a disk sleeps and so does whoever is queued for the
//!   lock. On the host the parker spins.
//!
//! A filesystem's own locks are its own business, except that it must not
//! hold a spin lock across I/O either.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod access;
mod dentry;
pub mod dirent;
pub mod fd;
mod file;
pub mod initramfs;
mod namespace;
mod node;
pub mod path;
pub mod pipe;
pub mod socket;
pub mod statfs;
pub mod tmpfs;
mod walk;

#[cfg(test)]
mod tests;

pub use access::Access;
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
