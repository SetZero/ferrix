//! Pipes, and the named pipes that are pipes found by a path.
//!
//! `libs/vfs`'s [`PipeBuffer`] is the queue and the rules at its edges, as
//! values: a read that would wait says so rather than waiting. This is the
//! half that waits. A [`Pipe`] is that buffer behind a lock with a wait queue
//! for each direction, and each end is an [`Inode`] an open file can hold.
//!
//! # Never waiting with the buffer locked
//!
//! Every operation takes the lock, asks the buffer, and lets go before it
//! decides anything: the outcome is bound to a variable first, so the guard is
//! gone before a `match` on it can sleep. A writer asleep on a full pipe with
//! the lock held would stop the very reader that was about to make room. The
//! wait looks at the buffer again, under the lock, after it has joined the
//! queue, which is the order `sched::wait` needs to lose no wake-up.
//!
//! # Ends are counted by their inodes
//!
//! An end counts itself into the buffer when it is made and out when it is
//! dropped. The open file description holds its end, and `dup` and `fork`
//! share the description, so the last reference to an end goes exactly when
//! the last descriptor that could use it closes -- which is when a reader must
//! see end of file and a writer `EPIPE`.
//!
//! # `EPIPE` and `SIGPIPE`
//!
//! Linux raises `SIGPIPE` on a write with no reader left, and most programs
//! die of it before they see the error; one that ignores or handles the signal
//! gets `EPIPE`. [`End::write_stream`] does both.
//!
//! # Named pipes
//!
//! A FIFO is a node in a filesystem, and opening it gives an end of the pipe
//! every other opener of that node shares. The node cannot hold the pipe --
//! tmpfs is `libs/vfs`, which has no wait queues -- so the pipe is found in a
//! table here, keyed by the node's device and inode number and held weakly,
//! and [`attach_fifo`] swaps an end of it into the open file `openat` made.
//!
//! That is one line in `openat` and one method on `OpenFile`. The other two
//! ways in were worse: a callback in `Namespace::open` would put the first
//! kernel hook into a crate that has none, and telling `Inode::open` the flags
//! would change a method every filesystem implements, for the one kind of node
//! that needs them.
//!
//! Opening waits as Linux's does: a reader until a writer has opened, a writer
//! until a reader has -- or `ENXIO` at once under `O_NONBLOCK` -- and an open
//! for both for nothing. What an opener waits for is a partner *having opened*
//! since it began, counted, rather than one being open when it looks, so that
//! a writer which opens, writes and closes before the reader looks has still
//! been there, and the reader reads what it wrote.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::sync::{Arc, Weak};
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use ferrix_sync::Once;

use crate::sync::SpinLock;
use ferrix_vfs::path::NAME_MAX;
use ferrix_vfs::pipe::{PIPE_CAPACITY, PIPEFS_MAGIC, PipeBuffer, ReadOutcome, WriteOutcome};
use ferrix_vfs::{
    Errno, FileSystem, FileType, Inode, Location, Metadata, OpenFile, OpenFlags, Readiness, StatFs,
    Timespec,
};

use crate::fs;
use crate::sched::WaitQueue;
use crate::syscall::process::{self, Process};

/// The deadline a pipe's wait passes: none. A pipe waits for something to
/// happen, and the process being killed is one of those things.
const FOREVER: u64 = u64::MAX;

/// The block size `stat` reports for a pipe: a page, as Linux reports.
const BLOCK_SIZE: u32 = 4096;

/// One pipe: the buffer, and who waits on it.
pub(crate) struct Pipe {
    buffer: SpinLock<PipeBuffer>,
    /// Woken when a read may no longer wait: bytes arrived, an end opened, or
    /// the last writer left.
    readable: Arc<WaitQueue>,
    /// Woken when a write may no longer wait: room was made, an end opened, or
    /// the last reader left.
    writable: Arc<WaitQueue>,
    /// Read ends ever opened. Changed only under `buffer`'s lock, so a FIFO
    /// opener can read it consistently with the count of ends open.
    readers_opened: AtomicU64,
    /// Write ends ever opened, likewise.
    writers_opened: AtomicU64,
}

impl fmt::Debug for Pipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pipe")
            .field("readers_opened", &self.readers_opened)
            .field("writers_opened", &self.writers_opened)
            .finish_non_exhaustive()
    }
}

impl Pipe {
    /// An empty pipe with no ends.
    fn new() -> Arc<Pipe> {
        Arc::new(Pipe {
            buffer: SpinLock::new(PipeBuffer::new(PIPE_CAPACITY)),
            readable: Arc::new(WaitQueue::new()),
            writable: Arc::new(WaitQueue::new()),
            readers_opened: AtomicU64::new(0),
            writers_opened: AtomicU64::new(0),
        })
    }

    /// Wake both directions: an end opened or closed, which can end a wait
    /// on either side.
    fn wake_both(&self) {
        self.readable.wake_all();
        self.writable.wake_all();
    }
}

/// Whether the process a wait is on behalf of has been killed, or has a signal
/// to take, either of which ends the wait with `EINTR`. The boot self-check
/// calls with no process, and is never killed.
fn killed(caller: Option<&Arc<Process>>) -> bool {
    caller.is_some_and(|process| process.signal_pending())
}

/// A count so far, or `errno` if nothing was done: a transfer that stopped
/// part-way reports what it managed, as `read` and `write` must.
fn partial(done: usize, errno: Errno) -> ferrix_vfs::Result<usize> {
    if done > 0 { Ok(done) } else { Err(errno) }
}

/// One end of a pipe, as the inode an open file reads or writes through.
#[derive(Debug)]
struct End {
    pipe: Arc<Pipe>,
    reads: bool,
    writes: bool,
    /// What `stat` reports through this end.
    metadata: Metadata,
}

impl End {
    /// Make an end, counting it into the pipe, and wake anyone whose open was
    /// waiting for one.
    fn open(pipe: &Arc<Pipe>, reads: bool, writes: bool, metadata: Metadata) -> Arc<End> {
        {
            let mut buffer = pipe.buffer.lock();
            if reads {
                buffer.open_reader();
                let _ = pipe.readers_opened.fetch_add(1, Ordering::Relaxed);
            }
            if writes {
                buffer.open_writer();
                let _ = pipe.writers_opened.fetch_add(1, Ordering::Relaxed);
            }
        }
        pipe.wake_both();
        Arc::new(End {
            pipe: Arc::clone(pipe),
            reads,
            writes,
            metadata,
        })
    }

    /// Bytes were taken out: a writer waiting for room may now have it.
    fn took(&self, count: usize) -> usize {
        if count > 0 {
            self.pipe.writable.wake_all();
        }
        count
    }

    /// Sleep until a read would not wait. `EINTR` if the caller is killed
    /// first, which is what ends a `cat` blocked on a pipe nobody writes to.
    fn wait_to_read(&self) -> ferrix_vfs::Result<()> {
        let caller = process::current();
        let _ = self.pipe.readable.wait_until_deadline(
            || {
                let ready = self.pipe.buffer.lock().can_read();
                ready || killed(caller.as_ref())
            },
            FOREVER,
        );
        if killed(caller.as_ref()) {
            // A restart code, not `EINTR`: a pipe read restarts under
            // `SA_RESTART`. A read that already moved bytes returns the count
            // instead (see `partial`); only one that moved none is restarted.
            return Err(Errno::ERESTARTSYS);
        }
        Ok(())
    }

    /// Sleep until a write of `len` bytes would not wait, or `EINTR`.
    fn wait_to_write(&self, len: usize) -> ferrix_vfs::Result<()> {
        let caller = process::current();
        let _ = self.pipe.writable.wait_until_deadline(
            || {
                let ready = self.pipe.buffer.lock().can_write(len);
                ready || killed(caller.as_ref())
            },
            FOREVER,
        );
        if killed(caller.as_ref()) {
            // A restart code, not `EINTR`, for the same reason a read gives one.
            return Err(Errno::ERESTARTSYS);
        }
        Ok(())
    }
}

impl Drop for End {
    fn drop(&mut self) {
        {
            let mut buffer = self.pipe.buffer.lock();
            if self.reads {
                buffer.close_reader();
            }
            if self.writes {
                buffer.close_writer();
            }
        }
        // A reader waiting on the last writer now reads end of file, and a
        // writer waiting on the last reader now gets `EPIPE`.
        self.pipe.wake_both();
    }
}

impl Inode for End {
    fn metadata(&self) -> Metadata {
        self.metadata
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll_queues(&self, visit: &mut dyn FnMut(ferrix_vfs::WakeSource)) -> bool {
        visit(fs::wake::shared(&self.pipe.readable));
        visit(fs::wake::shared(&self.pipe.writable));
        true
    }

    fn poll_changes(&self) -> Option<u64> {
        Some(
            self.pipe
                .readable
                .wakes()
                .wrapping_add(self.pipe.writable.wakes()),
        )
    }

    fn poll(&self) -> Readiness {
        let buffer = self.pipe.buffer.lock();
        let read = buffer.read_readiness();
        let write = buffer.write_readiness();
        Readiness {
            readable: self.reads && read.readable,
            writable: self.writes && write.writable,
            // An end open for both is its own partner, and never hangs up.
            hangup: self.reads && !self.writes && read.hangup,
            error: self.writes && !self.reads && write.error,
        }
    }

    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> ferrix_vfs::Result<usize> {
        loop {
            // Bound first, so the guard is gone before anything below waits.
            let outcome = self.pipe.buffer.lock().read(buf);
            match outcome {
                ReadOutcome::Read(count) => return Ok(self.took(count)),
                ReadOutcome::EndOfFile => return Ok(0),
                ReadOutcome::WouldBlock if nonblock => return Err(Errno::EAGAIN),
                ReadOutcome::WouldBlock => self.wait_to_read()?,
            }
        }
    }

    /// Queue all of `data`, waiting for room as often as it takes, unless
    /// `nonblock`: then as much as fits now, or `EAGAIN` for none. A write
    /// with no reader left is `EPIPE`, or the count queued before the reader
    /// went.
    fn write_stream(&self, data: &[u8], nonblock: bool) -> ferrix_vfs::Result<usize> {
        // An empty write succeeds without looking for a reader, as on Linux.
        if data.is_empty() {
            return Ok(0);
        }
        let mut done = 0;
        while done < data.len() {
            let rest = data.get(done..).unwrap_or_default();
            // Bound first, so the guard is gone before anything below waits.
            let outcome = self.pipe.buffer.lock().write(rest);
            let refusal = match outcome {
                WriteOutcome::Wrote(count) => {
                    done += count;
                    self.pipe.readable.wake_all();
                    continue;
                }
                WriteOutcome::Broken => {
                    // `SIGPIPE` to the writer as well, as Linux sends it; a
                    // writer that survives it still gets `EPIPE`.
                    crate::syscall::kill::send_to_current(ferrix_linux_abi::types::SIGPIPE);
                    Errno::EPIPE
                }
                WriteOutcome::WouldBlock if nonblock => Errno::EAGAIN,
                WriteOutcome::WouldBlock => match self.wait_to_write(rest.len()) {
                    Ok(()) => continue,
                    Err(errno) => errno,
                },
            };
            return partial(done, refusal);
        }
        Ok(done)
    }
}

// ---------------------------------------------------------------------------
// Anonymous pipes
// ---------------------------------------------------------------------------

/// The filesystem every anonymous pipe is on: Linux's `pipefs`, which a
/// program only ever meets as `fstat`'s device and `fstatfs`'s magic number.
#[derive(Debug)]
struct PipeFs {
    device: u64,
    root: Arc<dyn Inode>,
    /// The next pipe's inode number. Both ends of one pipe share one, as on
    /// Linux, which is how a program can tell two descriptors are one pipe.
    next_ino: AtomicU64,
}

/// The one pipefs.
static PIPEFS: Once<Arc<PipeFs>> = Once::new();

/// The one pipefs, made on first use.
fn pipefs() -> &'static Arc<PipeFs> {
    PIPEFS.call_once(|| {
        Arc::new(PipeFs {
            device: fs::anonymous_device(),
            root: Arc::new(PipeRoot),
            next_ino: AtomicU64::new(2),
        })
    })
}

impl FileSystem for PipeFs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::clone(&self.root)
    }

    fn name(&self) -> &'static str {
        "pipefs"
    }

    fn device(&self) -> u64 {
        self.device
    }

    fn statfs(&self) -> StatFs {
        StatFs {
            magic: PIPEFS_MAGIC,
            block_size: u64::from(BLOCK_SIZE),
            name_max: NAME_MAX as u64,
            ..StatFs::default()
        }
    }
}

/// pipefs's root directory, which is empty and which nothing can reach: every
/// pipe's location is detached. It exists because a filesystem has a root.
#[derive(Debug)]
struct PipeRoot;

impl Inode for PipeRoot {
    fn metadata(&self) -> Metadata {
        Metadata {
            ino: 1,
            kind: FileType::Directory,
            permissions: 0o700,
            nlink: 2,
            uid: 0,
            gid: 0,
            size: 0,
            rdev: 0,
            blocks: 0,
            block_size: BLOCK_SIZE,
            atime: Timespec::default(),
            mtime: Timespec::default(),
            ctime: Timespec::default(),
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

/// A new pipe, as the two open files `pipe2` installs: the read end, then the
/// write end, each non-blocking if asked, and owned by `owner`, the creator's
/// filesystem user and group ids.
///
/// # Errors
///
/// Whatever [`OpenFile::new`] refuses, which for a pipe end is nothing.
pub(crate) fn new_pipe(
    nonblock: bool,
    (uid, gid): (u32, u32),
) -> Result<(Arc<OpenFile>, Arc<OpenFile>), Errno> {
    let pipefs = pipefs();
    let ino = pipefs.next_ino.fetch_add(1, Ordering::Relaxed);
    let now = fs::clock().now();
    let metadata = Metadata {
        ino,
        kind: FileType::Fifo,
        permissions: 0o600,
        nlink: 1,
        uid,
        gid,
        size: 0,
        rdev: 0,
        blocks: 0,
        block_size: BLOCK_SIZE,
        atime: now,
        mtime: now,
        ctime: now,
    };
    // What `/proc/self/fd` will show, in Linux's spelling.
    let name = format!("pipe:[{ino}]");
    let pipe = Pipe::new();
    let reader = open_end(
        End::open(&pipe, true, false, metadata),
        name.as_bytes(),
        nonblock,
    )?;
    let writer = open_end(
        End::open(&pipe, false, true, metadata),
        name.as_bytes(),
        nonblock,
    )?;
    Ok((reader, writer))
}

/// An open file on `end`, at a location of its own on pipefs.
fn open_end(end: Arc<End>, name: &[u8], nonblock: bool) -> Result<Arc<OpenFile>, Errno> {
    let flags = OpenFlags {
        read: end.reads,
        write: end.writes,
        nonblock,
        ..OpenFlags::default()
    };
    let pipefs: Arc<PipeFs> = Arc::clone(pipefs());
    let parker = Arc::clone(super::namespace().parker());
    OpenFile::new(Location::detached(pipefs, end, name, parker), &flags)
}

// ---------------------------------------------------------------------------
// Named pipes
// ---------------------------------------------------------------------------

/// Every FIFO somebody has open, by the node's device and inode number.
///
/// Weak, so that a FIFO nobody has open holds no pipe, and a pipe goes away
/// with its last end as an anonymous one does.
static FIFOS: SpinLock<BTreeMap<(u64, u64), Weak<Pipe>>> = SpinLock::new(BTreeMap::new());

/// The pipe behind the FIFO `key` names, made if nobody has it open.
fn shared_pipe(key: (u64, u64)) -> Arc<Pipe> {
    let mut table = FIFOS.lock();
    if let Some(pipe) = table.get(&key).and_then(Weak::upgrade) {
        return pipe;
    }
    // Forget the pipes nobody holds any more, so the table is as large as the
    // FIFOs open now rather than every FIFO ever opened. Only weak references
    // are dropped here, so nothing is freed under the lock.
    table.retain(|_, pipe| pipe.strong_count() > 0);
    let pipe = Pipe::new();
    let _ = table.insert(key, Arc::downgrade(&pipe));
    pipe
}

/// An open file of a named pipe, made into an end of the pipe every opener of
/// that node shares. Anything else -- and a FIFO opened with `O_PATH`, which
/// is a handle on the name -- comes back as it was.
///
/// # Errors
///
/// `ENXIO` for a non-blocking open for writing only with no reader, `EINVAL`
/// for an open for neither reading nor writing, which Linux refuses on a FIFO,
/// and `EINTR` if the opener is killed while it waits for its partner.
pub(crate) fn attach_fifo(file: Arc<OpenFile>) -> Result<Arc<OpenFile>, Errno> {
    if file.kind() != FileType::Fifo || file.is_path() {
        return Ok(file);
    }
    let (reads, writes) = (file.readable(), file.writable());
    if !reads && !writes {
        return Err(Errno::EINVAL);
    }
    let nonblock = file.status().nonblock;
    let metadata = file.inode().metadata();
    let key = (file.location().mount.filesystem().device(), metadata.ino);
    let pipe = shared_pipe(key);
    let no_reader = pipe.buffer.lock().readers() == 0;
    if writes && !reads && nonblock && no_reader {
        return Err(Errno::ENXIO);
    }
    let end = End::open(&pipe, reads, writes, metadata);
    if !nonblock && reads != writes {
        // On failure `end` is dropped, which counts it out again.
        wait_for_partner(&pipe, reads)?;
    }
    Ok(file.with_io(end))
}

/// Block a FIFO opener until the other kind of end has opened: a writer for a
/// reader, a reader for a writer. See the module documentation for why this
/// counts opens rather than looking at who is open.
fn wait_for_partner(pipe: &Pipe, reader: bool) -> Result<(), Errno> {
    let (queue, opened) = if reader {
        (&pipe.readable, &pipe.writers_opened)
    } else {
        (&pipe.writable, &pipe.readers_opened)
    };
    let (partners, before) = {
        let buffer = pipe.buffer.lock();
        let open = if reader {
            buffer.writers()
        } else {
            buffer.readers()
        };
        (open, opened.load(Ordering::Relaxed))
    };
    if partners > 0 {
        return Ok(());
    }
    let caller = process::current();
    let _ = queue.wait_until_deadline(
        || opened.load(Ordering::Relaxed) != before || killed(caller.as_ref()),
        FOREVER,
    );
    if killed(caller.as_ref()) {
        // A restart code, not `EINTR`: opening a FIFO restarts under
        // `SA_RESTART`, as Linux's `fifo_open` does.
        return Err(Errno::ERESTARTSYS);
    }
    Ok(())
}
