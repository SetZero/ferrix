//! Open file descriptions.
//!
//! What `open` returns is not a descriptor: it is a *description*, which the
//! descriptor number refers to. The difference is observable and programs
//! depend on it. Two descriptors made by `dup`, or inherited across `fork`,
//! share one description and therefore one offset — which is how a shell's
//! `(echo a; echo b) > file` writes both lines rather than the second over
//! the first. Two separate `open`s of the same file do not share one. The
//! close-on-exec flag, by contrast, belongs to the descriptor, and lives in
//! [`crate::fd::FdTable`].
//!
//! # No lock of the description is held across I/O
//!
//! The offset and the status flags are spin locks, and a filesystem that
//! reads a disk sleeps. So every operation here copies what it needs out from
//! under the lock, calls the [`Inode`] with nothing held, and takes the lock
//! again to store the result: `read` and `write` read the offset, do the I/O
//! at it, and set the offset to where that I/O ended.
//!
//! Two reads racing on one description may therefore start at the same offset
//! and get the same bytes, and the offset ends where the last one to finish
//! put it. That is what Linux did for every file before 3.14 added
//! `f_pos_lock`, and still does for a stream. The alternative is a "busy"
//! flag and a wait on the description, and this crate has nothing to wait
//! with: waiting is the kernel's (see [`crate::pipe`]). A program that shares
//! a descriptor between threads and wants ordered reads has `pread`.
//!
//! An `O_APPEND` write stays atomic without the offset lock, because the
//! filesystem decides where the end is under its own lock and returns the
//! offset just past what it wrote.

use alloc::sync::Arc;
use core::fmt;

use ferrix_linux_abi::errno::Errno;
use ferrix_sync::SpinLock;

use crate::Result;
use crate::namespace::Location;
use crate::node::{DirEntry, FIRST_CURSOR, FileType, Inode, Readiness};

/// What `open` was asked for, decoded from the architecture's `O_*` bits.
///
/// Decoded rather than raw because the bits are not the same everywhere:
/// `O_DIRECTORY` is `0o200000` on x86-64 and `0o40000` on both Arm
/// architectures. The system call layer knows which table it was built with;
/// this crate should not have to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one independent open(2) flag, and a bit set would be the \
              architecture-dependent encoding this type exists to hide"
)]
pub struct OpenFlags {
    /// Readable.
    pub read: bool,
    /// Writable.
    pub write: bool,
    /// `O_CREAT`.
    pub create: bool,
    /// `O_EXCL`.
    pub exclusive: bool,
    /// `O_TRUNC`.
    pub truncate: bool,
    /// `O_APPEND`.
    pub append: bool,
    /// `O_DIRECTORY`.
    pub directory: bool,
    /// `O_NOFOLLOW`.
    pub nofollow: bool,
    /// `O_PATH`: a handle on the name, with no I/O allowed.
    pub path: bool,
    /// `O_NONBLOCK`.
    pub nonblock: bool,
}

/// The status flags `fcntl(F_SETFL)` may change after the open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    /// `O_APPEND`.
    pub append: bool,
    /// `O_NONBLOCK`.
    pub nonblock: bool,
}

/// Where `lseek` measures from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whence {
    /// `SEEK_SET`.
    Set,
    /// `SEEK_CUR`.
    Current,
    /// `SEEK_END`.
    End,
    /// `SEEK_DATA`: the next offset holding data.
    Data,
    /// `SEEK_HOLE`: the next offset in a hole.
    Hole,
}

/// One open file description.
pub struct OpenFile {
    location: Location,
    /// What `stat` reports.
    inode: Arc<dyn Inode>,
    /// What reads and writes go to: the inode, or what its `open` returned.
    io: Arc<dyn Inode>,
    kind: FileType,
    read: bool,
    write: bool,
    path_only: bool,
    status: SpinLock<Status>,
    /// The file position, or a directory's cursor. Never held across a call
    /// into `io` or `inode`; see the module documentation.
    offset: SpinLock<u64>,
}

impl fmt::Debug for OpenFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenFile")
            .field("kind", &self.kind)
            .field("read", &self.read)
            .field("write", &self.write)
            .field("path_only", &self.path_only)
            .finish_non_exhaustive()
    }
}

impl OpenFile {
    /// Open what `location` names.
    ///
    /// The checks that need the path — `O_CREAT`, `O_EXCL`, `O_NOFOLLOW` —
    /// belong to [`crate::Namespace::open`]. This is the part that is the
    /// same however the location was reached, which is what reopening
    /// `/proc/self/fd/3` needs.
    ///
    /// # Errors
    ///
    /// `ENOENT` for a negative location, and whatever the inode's
    /// [`Inode::open`] refuses.
    pub fn new(location: Location, flags: &OpenFlags) -> Result<Arc<OpenFile>> {
        let inode = location.inode()?;
        let kind = inode.metadata().kind;
        let io = if flags.path {
            Arc::clone(&inode)
        } else {
            inode.open()?.unwrap_or_else(|| Arc::clone(&inode))
        };
        Ok(Arc::new(OpenFile {
            location,
            inode,
            io,
            kind,
            read: flags.read && !flags.path,
            write: flags.write && !flags.path,
            path_only: flags.path,
            status: SpinLock::new(Status {
                append: flags.append,
                nonblock: flags.nonblock,
            }),
            offset: SpinLock::new(0),
        }))
    }

    /// The same open, with reads and writes going to `io` instead.
    ///
    /// For an object whose I/O depends on how it was opened, which
    /// [`Inode::open`] is not told: a named pipe is a read end, a write end or
    /// both according to the access mode, and only the kernel's table of pipes
    /// knows which pipe a node on disk stands for. The opener looks at the
    /// open file [`crate::Namespace::open`] made and swaps in the end.
    /// Everything else -- the location, what `stat` reports, the access mode
    /// and the status flags -- is this open's, and the offset starts at zero
    /// as a new open's does.
    #[must_use]
    pub fn with_io(&self, io: Arc<dyn Inode>) -> Arc<OpenFile> {
        Arc::new(OpenFile {
            location: self.location.clone(),
            inode: Arc::clone(&self.inode),
            io,
            kind: self.kind,
            read: self.read,
            write: self.write,
            path_only: self.path_only,
            status: SpinLock::new(self.status()),
            offset: SpinLock::new(0),
        })
    }

    /// Where it was opened.
    #[must_use]
    pub fn location(&self) -> &Location {
        &self.location
    }

    /// The inode, as `fstat` sees it.
    #[must_use]
    pub fn inode(&self) -> &Arc<dyn Inode> {
        &self.inode
    }

    /// What reads and writes go to: the inode itself, or what its
    /// [`Inode::open`] or [`OpenFile::with_io`] put in its place. `/dev/tty`
    /// is a node of its own to `fstat` and the console to everything else,
    /// which is how the kernel tells that an `ioctl` is for the terminal.
    #[must_use]
    pub fn io(&self) -> &Arc<dyn Inode> {
        &self.io
    }

    /// What kind of object it is.
    #[must_use]
    pub fn kind(&self) -> FileType {
        self.kind
    }

    /// Opened for reading.
    #[must_use]
    pub fn readable(&self) -> bool {
        self.read
    }

    /// Opened for writing.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.write
    }

    /// Opened with `O_PATH`.
    #[must_use]
    pub fn is_path(&self) -> bool {
        self.path_only
    }

    /// Whether what it reads and writes has no position: a pipe, a terminal.
    #[must_use]
    pub fn is_stream(&self) -> bool {
        self.io.is_stream()
    }

    /// `fallocate` without `FALLOC_FL_KEEP_SIZE`: make the file at least `len`
    /// bytes long, and never shorter.
    ///
    /// # Errors
    ///
    /// As [`OpenFile::set_len`].
    pub fn grow_to(&self, len: u64) -> Result<()> {
        if self.path_only {
            return Err(Errno::EBADF);
        }
        if !self.write || self.kind != FileType::Regular {
            return Err(Errno::EINVAL);
        }
        self.inode.grow_to(len)
    }

    /// The flags `fcntl(F_GETFL)` reports beyond the access mode.
    #[must_use]
    pub fn status(&self) -> Status {
        *self.status.lock()
    }

    /// `fcntl(F_SETFL)`.
    pub fn set_status(&self, status: Status) {
        *self.status.lock() = status;
    }

    /// The current offset.
    #[must_use]
    pub fn offset(&self) -> u64 {
        *self.offset.lock()
    }

    fn set_offset(&self, offset: u64) {
        *self.offset.lock() = offset;
    }

    /// Whether the offset lock is held right now, for a test's fake inode to
    /// ask from inside a call.
    #[cfg(test)]
    pub(crate) fn offset_lock_held(&self) -> bool {
        self.offset.try_lock().is_none()
    }

    fn check_io(&self, allowed: bool) -> Result<()> {
        if self.path_only || !allowed {
            return Err(Errno::EBADF);
        }
        if self.kind == FileType::Directory {
            return Err(Errno::EISDIR);
        }
        Ok(())
    }

    /// `read`: from the current offset, advancing it.
    ///
    /// The offset is read, the read done with no lock held, and the offset set
    /// past what it got; see the module documentation for what two concurrent
    /// reads of one description see.
    ///
    /// # Errors
    ///
    /// `EBADF` if not opened for reading, `EISDIR` for a directory, and
    /// `EAGAIN` from a stream that would wait under `O_NONBLOCK`.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        self.check_io(self.read)?;
        if self.io.is_stream() {
            // No offset lock for a stream: a read may wait for another
            // program, and a second reader of the same description must not
            // wait behind it for a lock that guards nothing.
            return self.io.read_stream(buf, self.status().nonblock);
        }
        let at = self.offset();
        let count = self.io.read_at(at, buf)?;
        self.set_offset(at.saturating_add(count as u64));
        Ok(count)
    }

    /// `pread64`: at `offset`, leaving the file position alone.
    ///
    /// # Errors
    ///
    /// As [`OpenFile::read`], and `ESPIPE` for a stream.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        self.check_io(self.read)?;
        if self.io.is_stream() {
            return Err(Errno::ESPIPE);
        }
        self.io.read_at(offset, buf)
    }

    /// `write`: at the current offset, or the end under `O_APPEND`.
    ///
    /// # Errors
    ///
    /// `EBADF` if not opened for writing, and what the filesystem refuses.
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        self.check_io(self.write)?;
        if self.io.is_stream() {
            return self.io.write_stream(data, self.status().nonblock);
        }
        let append = self.status().append;
        let at = self.offset();
        // Under `O_APPEND` the filesystem ignores `at` and returns the end it
        // wrote to, found under its own lock.
        let (count, end) = self.io.write_at(at, data, append)?;
        self.set_offset(end);
        Ok(count)
    }

    /// `pwrite64`.
    ///
    /// Under `O_APPEND` this appends whatever `offset` says, which is a Linux
    /// behaviour `pwrite(2)` documents as a bug and which programs therefore
    /// rely on not being fixed.
    ///
    /// # Errors
    ///
    /// As [`OpenFile::write`], and `ESPIPE` for a stream.
    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<usize> {
        self.check_io(self.write)?;
        if self.io.is_stream() {
            return Err(Errno::ESPIPE);
        }
        let append = self.status().append;
        self.io
            .write_at(offset, data, append)
            .map(|(count, _)| count)
    }

    /// `ftruncate`.
    ///
    /// # Errors
    ///
    /// `EINVAL` unless open for writing on a regular file.
    pub fn set_len(&self, len: u64) -> Result<()> {
        if self.path_only {
            return Err(Errno::EBADF);
        }
        if !self.write || self.kind != FileType::Regular {
            return Err(Errno::EINVAL);
        }
        self.inode.set_len(len)
    }

    /// What `poll` reports for this open file: the opened object's answer,
    /// masked by what the file was opened for.
    #[must_use]
    pub fn poll(&self) -> Readiness {
        if self.path_only {
            return Readiness::default();
        }
        let ready = self.io.poll();
        Readiness {
            readable: ready.readable && self.read,
            writable: ready.writable && self.write,
            ..ready
        }
    }

    /// `lseek`.
    ///
    /// # Errors
    ///
    /// `ESPIPE` for a stream, `EINVAL` for a result before the start or
    /// `SEEK_END` on a directory, `ENXIO` for `SEEK_DATA`/`SEEK_HOLE` at or
    /// past the end.
    pub fn seek(&self, offset: i64, whence: Whence) -> Result<u64> {
        if self.path_only {
            return Err(Errno::EBADF);
        }
        if self.io.is_stream() {
            return Err(Errno::ESPIPE);
        }
        let size = self.io.metadata().size;
        let mut position = self.offset.lock();
        let result = match whence {
            Whence::Set => i128::from(offset),
            Whence::Current => i128::from(*position) + i128::from(offset),
            Whence::End if self.kind == FileType::Directory => return Err(Errno::EINVAL),
            Whence::End => i128::from(size) + i128::from(offset),
            // No filesystem here stores holes a program could find, so the
            // whole file is data and the only hole is the one at its end.
            Whence::Data | Whence::Hole => {
                let at = u64::try_from(offset).map_err(|_| Errno::ENXIO)?;
                if at >= size {
                    return Err(Errno::ENXIO);
                }
                i128::from(if whence == Whence::Data { at } else { size })
            }
        };
        let result = u64::try_from(result).map_err(|_| Errno::EINVAL)?;
        if i64::try_from(result).is_err() {
            return Err(Errno::EINVAL);
        }
        *position = result;
        Ok(result)
    }

    /// Report directory entries from the cursor, advancing past each one
    /// `emit` accepts.
    ///
    /// `.` and `..` come first, from the VFS, because only the VFS knows what
    /// a mounted directory's parent is.
    ///
    /// The cursor is copied out and stored back as each step finishes, never
    /// held across the filesystem's `read_dir` or a `metadata`, for the same
    /// reason as [`OpenFile::read`].
    ///
    /// # Errors
    ///
    /// `ENOTDIR` for anything but a directory.
    pub fn read_dir(&self, emit: &mut dyn FnMut(DirEntry<'_>) -> bool) -> Result<()> {
        if self.path_only {
            return Err(Errno::EBADF);
        }
        if self.kind != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        let mut cursor = self.offset();
        if cursor == 0 {
            let dot = DirEntry {
                ino: self.inode.metadata().ino,
                kind: FileType::Directory,
                name: b".",
                next: 1,
            };
            if !emit(dot) {
                return Ok(());
            }
            cursor = 1;
            self.set_offset(cursor);
        }
        if cursor == 1 {
            let parent = self.location.parent();
            let ino = parent
                .inode()
                .map_or_else(|_| self.inode.metadata().ino, |inode| inode.metadata().ino);
            let dotdot = DirEntry {
                ino,
                kind: FileType::Directory,
                name: b"..",
                next: FIRST_CURSOR,
            };
            if !emit(dotdot) {
                return Ok(());
            }
            cursor = FIRST_CURSOR;
            self.set_offset(cursor);
        }
        let mut reached = cursor;
        self.io.read_dir(cursor, &mut |entry| {
            if emit(entry) {
                reached = entry.next;
                true
            } else {
                false
            }
        })?;
        self.set_offset(reached);
        Ok(())
    }
}
