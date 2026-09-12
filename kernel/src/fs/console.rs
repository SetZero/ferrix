//! The console, as a file.
//!
//! Stage 7 answered `read` on descriptor 0 and `write` on 1 and 2 by name,
//! because there was nothing else a descriptor could be. With a descriptor
//! table, the console has to be something a table can hold: an [`Inode`], so
//! that `dup2(1, 5)`, `fcntl(1, F_GETFL)` and `/dev/console` all reach the same
//! object by the same route as a file on tmpfs does.
//!
//! # A character device with no position
//!
//! [`Inode::is_stream`] is true, so an offset is never passed to it and
//! `lseek` on it is `ESPIPE`, as on a Linux terminal. Its identity is Linux's
//! `/dev/console`: character device 5:1. A program that asks `fstat(0)` whether
//! it is talking to a terminal sees a character device, which is what `isatty`
//! checks first -- and then an `ioctl` refused with `ENOTTY`, which is the
//! second thing it checks. See `crate::syscall::fd` for why that refusal is the
//! right answer today.
//!
//! # The line discipline, which is standing in for one
//!
//! Reading is canonical: nothing is returned until a whole line has been typed,
//! the line is echoed as it is typed, Enter ends it, Backspace edits it, and
//! Ctrl-D on an empty line is end of file.
//!
//! QEMU puts the host terminal in raw mode for `-serial stdio`, so nothing
//! echoes what is typed and Enter arrives as a carriage return. On Linux the
//! tty layer's `ECHO` and `ICRNL` fix both, between the keyboard and the
//! program. Ferrix has no tty layer yet, so those rules are here -- moved
//! unchanged from stage 7's `read`, because a person at the busybox prompt
//! depends on every byte of them -- and they move to the tty layer when stage
//! 15 brings one.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use ferrix_sync::{Once, SpinLock};
use ferrix_vfs::initramfs::makedev;
use ferrix_vfs::{
    Errno, FileSystem, FileType, Inode, Location, Metadata, Namespace, OpenFile, OpenFlags,
    Timespec,
};

use crate::arch;
use crate::console;
use crate::fs;

/// Linux's major number for `/dev/console` and the `/dev/tty*` family.
const MAJOR: u32 = 5;
/// `/dev/console`'s minor number within it.
const MINOR: u32 = 1;
/// The device number, in the encoding `stat` reports.
const RDEV: u64 = makedev(MAJOR, MINOR);

/// The block size `stat` reports: a page, as Linux does for a device node.
const BLOCK_SIZE: u32 = 4096;

/// The one console inode.
static CONSOLE: Once<Arc<dyn Inode>> = Once::new();

/// A namespace holding nothing but the console, for a console no `/dev` names.
static DETACHED: Once<Namespace> = Once::new();

/// The console device, one shared instance.
///
/// Shared rather than made per caller because it is one device: two inodes
/// would be two objects `stat` and `/proc/self/fd` could tell apart, for a
/// single serial port with a single pending line.
pub(crate) fn console_inode() -> Arc<dyn Inode> {
    Arc::clone(CONSOLE.call_once(|| Arc::new(Console)))
}

/// Open the console for reading and writing, as a new process's descriptors
/// 0, 1 and 2 are.
///
/// Through `/dev/console` when the namespace has the console there, so that a
/// descriptor reports the name a person would recognise. When it does not --
/// no devfs mounted yet, or a `/dev/console` that is some other filesystem's
/// device node rather than this device -- through a location in a namespace of
/// its own. Linux starts `init` with no descriptors at all in that case; a
/// shell whose output goes nowhere is no use to anyone at a serial port, so
/// this keeps the console.
///
/// # Errors
///
/// Whatever [`OpenFile::new`] refuses, which for this inode is nothing.
pub(crate) fn open_console() -> Result<Arc<OpenFile>, Errno> {
    let flags = OpenFlags {
        read: true,
        write: true,
        ..OpenFlags::default()
    };
    let namespace = fs::namespace();
    let named = namespace.open(&namespace.context(), None, b"/dev/console", &flags, 0);
    match named {
        Ok(file) if Arc::ptr_eq(file.inode(), &console_inode()) => Ok(file),
        _ => OpenFile::new(detached(), &flags),
    }
}

/// The console's place in a namespace nothing else is in.
fn detached() -> Location {
    DETACHED
        .call_once(|| {
            let alone = Arc::new(Alone {
                device: fs::anonymous_device(),
            });
            // No cache: there is exactly one dentry, and the mount holds it.
            Namespace::with_cache(alone, 0)
        })
        .root()
}

/// A filesystem whose root is the console, so that it has a location.
#[derive(Debug)]
struct Alone {
    /// The anonymous device `stat` reports for it.
    device: u64,
}

impl FileSystem for Alone {
    fn root(&self) -> Arc<dyn Inode> {
        console_inode()
    }

    fn name(&self) -> &'static str {
        "devtmpfs"
    }

    fn device(&self) -> u64 {
        self.device
    }
}

/// The serial console.
#[derive(Debug)]
struct Console;

impl Inode for Console {
    fn metadata(&self) -> Metadata {
        Metadata {
            // The device number doubles as the inode number: unique among
            // device nodes, and nothing a filesystem's counter will reach.
            ino: RDEV,
            kind: FileType::CharDevice,
            permissions: 0o600,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: 0,
            rdev: RDEV,
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

    fn is_stream(&self) -> bool {
        true
    }

    /// Reports the number of bytes written, which for the console is all of
    /// them. A short count is legal in the ABI and every correct caller loops
    /// on it, but there is nothing here that can be short: the console does
    /// not block and has no buffer to fill.
    fn write_at(
        &self,
        _offset: u64,
        data: &[u8],
        _append: bool,
    ) -> ferrix_vfs::Result<(usize, u64)> {
        console::write_bytes(data);
        Ok((data.len(), 0))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> ferrix_vfs::Result<usize> {
        read_canonical(buf)
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Bytes a finished line left behind because the reader asked for fewer.
///
/// One buffer for the machine, because there is one console. A program that
/// reads a line in two calls must get the second half on the second call, not
/// a fresh wait for the keyboard -- which is the whole of canonical mode's
/// contract, and the part a naive `read` gets wrong first.
static PENDING: SpinLock<Vec<u8>> = SpinLock::new(Vec::new());

/// Carriage return, which a terminal in raw mode sends for the Enter key.
const CR: u8 = b'\r';
/// Delete, which most terminals send for Backspace.
const DEL: u8 = 0x7F;
/// Backspace, which the rest send.
const BS: u8 = 0x08;
/// Ctrl-D: end of file, when it arrives on an empty line.
const EOT: u8 = 0x04;

/// One canonical read: the rest of the pending line, or a new line, into `buf`.
fn read_canonical(buf: &mut [u8]) -> ferrix_vfs::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }

    if PENDING.lock().is_empty() {
        let line = read_line();
        if line.is_empty() {
            // Ctrl-D on an empty line: end of file, which is a count of zero.
            return Ok(0);
        }
        PENDING.lock().extend_from_slice(&line);
    }

    let mut pending = PENDING.lock();
    let take = buf.len().min(pending.len());
    let chunk = pending.get(..take).ok_or(Errno::EINVAL)?;
    buf.get_mut(..take)
        .ok_or(Errno::EINVAL)?
        .copy_from_slice(chunk);
    let _ = pending.drain(..take);
    Ok(take)
}

/// How long a console read sleeps between looks for a keystroke.
///
/// Two milliseconds is shorter than anyone types, and long enough that a shell
/// waiting at its prompt costs a processor nothing measurable.
const CONSOLE_POLL_NANOS: u64 = 2_000_000;

/// Collect one line from the keyboard, echoing it, until Enter or Ctrl-D.
///
/// No lock is held here, [`PENDING`] or any other: the wait may last minutes,
/// and a lock held across it would be a lock nothing else could take. The
/// caller, `crate::syscall::file`, has already let go of the descriptor table.
///
/// On the two Arm architectures `arch::read_console_byte` answers `None` until
/// their UART drivers can read, so a read of the console waits there until the
/// program is killed, as it did before the console was a file.
fn read_line() -> Vec<u8> {
    let mut line = Vec::new();
    loop {
        let Some(byte) = arch::read_console_byte() else {
            // Nothing typed yet. A program reading the console is a task like
            // any other, so it sleeps between looks rather than spinning a
            // processor away from everything else -- and a program killed while
            // it waits stops waiting, and reads end of file.
            let killed =
                crate::syscall::process::current().is_some_and(|process| process.is_terminated());
            if killed {
                return Vec::new();
            }
            crate::sched::sleep_for(CONSOLE_POLL_NANOS);
            continue;
        };
        match byte {
            CR | b'\n' => {
                console::write_bytes(b"\n");
                line.push(b'\n');
                return line;
            }
            DEL | BS => {
                if line.pop().is_some() {
                    // Back over the character, blank it, back again.
                    console::write_bytes(b"\x08 \x08");
                }
            }
            EOT if line.is_empty() => return line,
            EOT => {}
            other => {
                console::write_bytes(&[other]);
                line.push(other);
            }
        }
    }
}
