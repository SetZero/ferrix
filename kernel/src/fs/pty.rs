//! Pseudoterminals: `/dev/ptmx`, `/dev/pts/<n>`, and the pair between them.
//!
//! A pseudoterminal is a terminal with a program at each end instead of a
//! screen and a keyboard. The program that opens `/dev/ptmx` -- a terminal
//! emulator, `script`, a session over the network -- holds the *master*; the
//! program it starts opens `/dev/pts/<n>`, the *slave*, and cannot tell it
//! from a serial console. That is what a terminal emulator on Ferrix needs,
//! and `docs/ROADMAP.md` stage 18's exit asks for one.
//!
//! # What goes which way
//!
//! * The master writes what the person typed. Those bytes go through the
//!   line discipline -- the same [`Discipline`] the console's terminal uses,
//!   so `ICANON`, `ECHO`, `ISIG` and the rest behave exactly as they do
//!   there -- and the slave reads what it makes ready. The echo goes back to
//!   the master, because on a pseudoterminal the *terminal* is the program
//!   holding the master, and it is the one that has to draw it.
//! * The slave writes the program's output. `OPOST` and `ONLCR` are applied,
//!   as the console's writes are, and the master reads the result.
//!
//! # What a pair is, and when it goes
//!
//! Opening `/dev/ptmx` makes a pair and gives back the master. Its number is
//! `TIOCGPTN`'s answer, and `/dev/pts/<number>` is its slave for as long as
//! the master is open. Closing the master takes the pair away: the slave's
//! reads then give end of file and its writes `EIO`, which is what Linux
//! does and what a shell reads as "the terminal has gone". Closing every
//! slave leaves the master readable with nothing to read, as Linux's does
//! until it is closed too.
//!
//! `TIOCSPTLCK` is answered, and its lock honoured: a slave may not be opened
//! while the master has not unlocked it. `openpty` and `posix_openpt` both
//! unlock before they open the slave.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

use ferrix_linux_abi::errno::Errno;
use ferrix_linux_abi::types::{ONLCR, OPOST, SIGWINCH};
use ferrix_vfs::initramfs::makedev;
use ferrix_vfs::{FileType, Inode, Metadata, Readiness, Result as VfsResult, Timespec};

use crate::fs::terminal::{Discipline, Termios, Winsize};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::syscall::process;

/// The major number Linux gives the pseudoterminal masters' multiplexer,
/// which is `/dev/ptmx` at 5:2 (`Documentation/admin-guide/devices.txt`).
pub(crate) const PTMX_MAJOR: u32 = 5;
/// `/dev/ptmx`'s minor.
pub(crate) const PTMX_MINOR: u32 = 2;

/// The major number a Unix 98 slave has: `/dev/pts/<n>` is 136:n for the
/// first 256, which is the range this kernel hands out.
pub(crate) const SLAVE_MAJOR: u32 = 136;

/// The most pairs at once, which is the range of `SLAVE_MAJOR`'s minors.
const MAX_PAIRS: u32 = 256;

/// The most bytes a slave's output may hold before a write waits.
///
/// Linux's pseudoterminal buffer is 8 KiB by default; this is the same, and
/// the reason is the same: a program writing to a terminal nobody is reading
/// has to stop somewhere.
const OUTPUT_LIMIT: usize = 8192;

/// A slave's inode number is this plus its number.
///
/// A range of its own, above the event nodes' `1 << 41`: two nodes of one
/// file system may not share a number.
pub(crate) const SLAVE_INO_BASE: u64 = 1 << 42;

/// `/dev/pts`'s inode number.
pub(crate) const PTS_INO: u64 = 1 << 38;

/// One pair.
pub(crate) struct Pty {
    /// Its number: `TIOCGPTN`'s answer and the slave's name.
    pub(crate) number: u32,
    state: SpinLock<State>,
    /// Woken when either side has something to read, or an end has gone.
    changed: Arc<WaitQueue>,
}

/// What the two ends share.
#[derive(Debug)]
struct State {
    /// What the master wrote, on its way to the slave.
    discipline: Discipline,
    /// What the slave wrote, on its way to the master.
    output: VecDeque<u8>,
    /// The size the master says the terminal is.
    winsize: Winsize,
    /// The foreground process group, as `TIOCSPGRP` set it.
    foreground: u32,
    /// The session the slave is the controlling terminal of.
    session: u32,
    /// Whether the master is still open.
    master: bool,
    /// How many slaves are open.
    slaves: usize,
    /// `TIOCSPTLCK`: a slave may not be opened while this is set, which is
    /// how it starts.
    locked: bool,
}

impl core::fmt::Debug for Pty {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Pty")
            .field("number", &self.number)
            .finish_non_exhaustive()
    }
}

/// Every pair that exists, by number.
static PAIRS: SpinLock<BTreeMap<u32, Arc<Pty>>> = SpinLock::new(BTreeMap::new());

/// How many pairs have ever been made, for the boot report.
static MADE: AtomicU32 = AtomicU32::new(0);

/// How many pairs have ever been made.
pub(crate) fn made() -> u32 {
    MADE.load(Ordering::Relaxed)
}

/// The numbers of the pairs that exist, in order.
pub(crate) fn numbers() -> Vec<u32> {
    PAIRS.lock().keys().copied().collect()
}

/// The pair `number` names, if it is there.
pub(crate) fn pair(number: u32) -> Option<Arc<Pty>> {
    PAIRS.lock().get(&number).map(Arc::clone)
}

/// Make a pair and give back its master, as opening `/dev/ptmx` does.
///
/// # Errors
///
/// `ENOSPC` when every number is taken, which is what Linux answers when its
/// pseudoterminal limit is reached.
pub(crate) fn open_master() -> VfsResult<Arc<MasterFile>> {
    let mut pairs = PAIRS.lock();
    let number = (0..MAX_PAIRS)
        .find(|number| !pairs.contains_key(number))
        .ok_or(Errno::ENOSPC)?;
    let pty = Arc::new(Pty {
        number,
        state: SpinLock::new(State {
            discipline: Discipline::new(),
            output: VecDeque::new(),
            winsize: Winsize::DEFAULT,
            foreground: 0,
            session: 0,
            master: true,
            slaves: 0,
            locked: true,
        }),
        changed: Arc::new(WaitQueue::new()),
    });
    let _ = pairs.insert(number, Arc::clone(&pty));
    drop(pairs);
    let _ = MADE.fetch_add(1, Ordering::Relaxed);
    Ok(Arc::new(MasterFile { pty }))
}

/// Open the slave of pair `number`, as opening `/dev/pts/<number>` does.
///
/// # Errors
///
/// `ENXIO` for a pair that is not there or whose master has gone, and `EIO`
/// for one `TIOCSPTLCK` has not unlocked, which is what Linux answers.
pub(crate) fn open_slave(number: u32) -> VfsResult<Arc<SlaveFile>> {
    let pty = pair(number).ok_or(Errno::ENXIO)?;
    {
        let mut state = pty.state.lock();
        if !state.master {
            return Err(Errno::ENXIO);
        }
        if state.locked {
            return Err(Errno::EIO);
        }
        state.slaves = state.slaves.saturating_add(1);
    }
    Ok(Arc::new(SlaveFile { pty }))
}

impl Pty {
    /// The settings.
    pub(crate) fn termios(&self) -> Termios {
        self.state.lock().discipline.termios()
    }

    /// Change the settings.
    pub(crate) fn set_termios(&self, termios: Termios, flush: bool) {
        let mut state = self.state.lock();
        if flush {
            state.discipline.flush_input();
        }
        state.discipline.set_termios(termios);
    }

    /// The size the terminal says it is.
    pub(crate) fn winsize(&self) -> Winsize {
        self.state.lock().winsize
    }

    /// Say the terminal is another size, and tell the foreground group.
    ///
    /// Linux raises `SIGWINCH` on the foreground process group when the size
    /// changes, and a program that draws in a window -- a shell's line
    /// editor, a pager -- redraws when it arrives.
    pub(crate) fn set_winsize(&self, winsize: Winsize) {
        let changed = {
            let mut state = self.state.lock();
            let was = state.winsize;
            state.winsize = winsize;
            was != winsize
        };
        if changed {
            self.signal_foreground(SIGWINCH);
        }
    }

    /// The foreground process group.
    pub(crate) fn foreground(&self) -> u32 {
        self.state.lock().foreground
    }

    /// Set it, as `tcsetpgrp` does.
    pub(crate) fn set_foreground(&self, group: u32) {
        self.state.lock().foreground = group;
    }

    /// The session the slave is the controlling terminal of.
    pub(crate) fn session(&self) -> u32 {
        self.state.lock().session
    }

    /// Make it the controlling terminal of `session`, as `TIOCSCTTY` does.
    pub(crate) fn set_session(&self, session: u32, foreground: u32) {
        let mut state = self.state.lock();
        state.session = session;
        state.foreground = foreground;
    }

    /// How many bytes the slave could read without waiting.
    pub(crate) fn slave_available(&self) -> usize {
        self.state.lock().discipline.available()
    }

    /// How many bytes the master could read without waiting.
    pub(crate) fn master_available(&self) -> usize {
        self.state.lock().output.len()
    }

    /// Throw away what has been typed and not read.
    pub(crate) fn flush_input(&self) {
        self.state.lock().discipline.flush_input();
    }

    /// Throw away what the slave wrote and the master has not read.
    pub(crate) fn flush_output(&self) {
        self.state.lock().output.clear();
    }

    /// Send `signal` to every process in the foreground group.
    fn signal_foreground(&self, signal: u32) {
        let group = self.foreground();
        if group == 0 {
            return;
        }
        for target in crate::syscall::registry::live() {
            if target.pgid() == group {
                crate::syscall::kill::send(&target, signal, crate::syscall::signal::Origin::Kernel);
            }
        }
    }

    /// Put what the master wrote through the line discipline, and give back
    /// the signal it asks for, if any.
    fn typed(&self, bytes: &[u8]) -> Option<u32> {
        let mut signal = None;
        {
            let mut state = self.state.lock();
            let mut echo = Vec::new();
            for byte in bytes {
                if let Some(raised) = state.discipline.receive(*byte, &mut echo) {
                    signal = Some(raised);
                }
            }
            // The echo goes to the master: on a pseudoterminal the terminal
            // is the program at that end, and drawing what was typed is its
            // work.
            for byte in echo {
                if state.output.len() < OUTPUT_LIMIT {
                    state.output.push_back(byte);
                }
            }
        }
        self.changed.wake_all();
        signal
    }

    /// Take what the slave may read, or nothing if there is none.
    fn read_typed(&self, buf: &mut [u8]) -> Option<usize> {
        let taken = self.state.lock().discipline.take(buf);
        if taken.is_some() {
            self.changed.wake_all();
        }
        taken
    }

    /// Put what the slave wrote where the master can read it, applying
    /// `OPOST` as the console's writes do.
    fn wrote(&self, bytes: &[u8]) -> usize {
        let mut written = 0;
        {
            let mut state = self.state.lock();
            let post = state.discipline.termios().oflag;
            let newlines = post & OPOST != 0 && post & ONLCR != 0;
            for byte in bytes {
                if state.output.len() >= OUTPUT_LIMIT {
                    break;
                }
                if newlines && *byte == b'\n' {
                    state.output.push_back(b'\r');
                }
                state.output.push_back(*byte);
                written += 1;
            }
        }
        self.changed.wake_all();
        written
    }

    /// Take what the master may read.
    fn read_written(&self, buf: &mut [u8]) -> usize {
        let mut state = self.state.lock();
        let mut taken = 0;
        while taken < buf.len() {
            let Some(byte) = state.output.pop_front() else {
                break;
            };
            if let Some(slot) = buf.get_mut(taken) {
                *slot = byte;
            }
            taken += 1;
        }
        taken
    }

    /// Whether the master is still open.
    fn master_open(&self) -> bool {
        self.state.lock().master
    }

    /// Whether any slave is open.
    fn slave_open(&self) -> bool {
        self.state.lock().slaves > 0
    }

    /// Wait until `ready` or the caller is signalled.
    fn wait_for(&self, ready: impl Fn() -> bool) -> Result<(), Errno> {
        let caller = process::current();
        let killed = || {
            caller
                .as_ref()
                .is_some_and(|process| process.signal_pending())
        };
        while !ready() && !killed() {
            let _ = self
                .changed
                .wait_until_deadline(|| ready() || killed(), u64::MAX);
        }
        if ready() {
            Ok(())
        } else {
            Err(Errno::ERESTARTSYS)
        }
    }
}

/// The master end: what a terminal emulator holds.
#[derive(Debug)]
pub(crate) struct MasterFile {
    pub(crate) pty: Arc<Pty>,
}

/// The slave end: what the program on the terminal holds.
#[derive(Debug)]
pub(crate) struct SlaveFile {
    pub(crate) pty: Arc<Pty>,
}

impl Drop for MasterFile {
    /// Closing the master takes the pair away, as Linux's `pty_close` does:
    /// the slave's reads end and its writes fail, and the number is free for
    /// the next `/dev/ptmx`.
    fn drop(&mut self) {
        {
            let mut state = self.pty.state.lock();
            state.master = false;
            state.output.clear();
        }
        let _ = PAIRS.lock().remove(&self.pty.number);
        self.pty.changed.wake_all();
    }
}

impl Drop for SlaveFile {
    fn drop(&mut self) {
        {
            let mut state = self.pty.state.lock();
            state.slaves = state.slaves.saturating_sub(1);
        }
        self.pty.changed.wake_all();
    }
}

impl Inode for MasterFile {
    fn metadata(&self) -> Metadata {
        Metadata {
            ino: PTS_INO,
            kind: FileType::CharDevice,
            permissions: 0o666,
            nlink: 1,
            rdev: makedev(PTMX_MAJOR, PTMX_MINOR),
            block_size: 4096,
            ..blank()
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        self.read_stream(buf, false)
    }

    /// What the slave wrote. A pair whose every slave has closed reads
    /// nothing and is not at end of file: Linux keeps the master readable,
    /// because a slave may be opened again.
    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let ready = || self.pty.master_available() > 0;
        if !ready() {
            if nonblock {
                return Err(Errno::EAGAIN);
            }
            self.pty.wait_for(ready)?;
        }
        Ok(self.pty.read_written(buf))
    }

    fn write_at(&self, offset: u64, buf: &[u8], _append: bool) -> VfsResult<(usize, u64)> {
        self.write_stream(buf, false)
            .map(|written| (written, offset))
    }

    /// What the person typed, through the line discipline.
    fn write_stream(&self, buf: &[u8], _nonblock: bool) -> VfsResult<usize> {
        if let Some(signal) = self.pty.typed(buf) {
            self.pty.signal_foreground(signal);
        }
        Ok(buf.len())
    }

    fn poll(&self) -> Readiness {
        Readiness {
            readable: self.pty.master_available() > 0,
            writable: true,
            hangup: false,
            error: false,
        }
    }

    fn poll_queues(&self, visit: &mut dyn FnMut(ferrix_vfs::WakeSource)) -> bool {
        visit(crate::fs::wake::shared(&self.pty.changed));
        true
    }

    fn poll_changes(&self) -> Option<u64> {
        Some(self.pty.changed.wakes())
    }
}

impl Inode for SlaveFile {
    fn metadata(&self) -> Metadata {
        slave_metadata(self.pty.number)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        self.read_stream(buf, false)
    }

    /// What the master typed, once the discipline has a line or a byte to
    /// give. A master that has closed is end of file, which is what a shell
    /// reads as its terminal going away.
    fn read_stream(&self, buf: &mut [u8], nonblock: bool) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(taken) = self.pty.read_typed(buf) {
                return Ok(taken);
            }
            if !self.pty.master_open() {
                return Ok(0);
            }
            if nonblock {
                return Err(Errno::EAGAIN);
            }
            self.pty
                .wait_for(|| self.pty.slave_available() > 0 || !self.pty.master_open())?;
        }
    }

    fn write_at(&self, offset: u64, buf: &[u8], _append: bool) -> VfsResult<(usize, u64)> {
        self.write_stream(buf, false)
            .map(|written| (written, offset))
    }

    /// The program's output, on its way to the master.
    fn write_stream(&self, buf: &[u8], _nonblock: bool) -> VfsResult<usize> {
        if !self.pty.master_open() {
            return Err(Errno::EIO);
        }
        Ok(self.pty.wrote(buf))
    }

    fn poll(&self) -> Readiness {
        let gone = !self.pty.master_open();
        Readiness {
            readable: self.pty.slave_available() > 0 || gone,
            writable: !gone,
            hangup: gone,
            error: false,
        }
    }

    fn poll_queues(&self, visit: &mut dyn FnMut(ferrix_vfs::WakeSource)) -> bool {
        visit(crate::fs::wake::shared(&self.pty.changed));
        true
    }

    fn poll_changes(&self) -> Option<u64> {
        Some(self.pty.changed.wakes())
    }
}

/// A slave node's metadata.
pub(crate) fn slave_metadata(number: u32) -> Metadata {
    Metadata {
        ino: SLAVE_INO_BASE + u64::from(number),
        kind: FileType::CharDevice,
        // What Linux's devpts gives a slave: the owner's to read and write,
        // and the `tty` group's to write. Ferrix has no groups, so it is the
        // mode without them.
        permissions: 0o620,
        nlink: 1,
        rdev: makedev(SLAVE_MAJOR, number),
        block_size: 4096,
        ..blank()
    }
}

/// The fields every node here shares.
fn blank() -> Metadata {
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    Metadata {
        ino: 0,
        kind: FileType::CharDevice,
        permissions: 0o666,
        nlink: 1,
        uid: 0,
        gid: 0,
        size: 0,
        rdev: 0,
        blocks: 0,
        block_size: 4096,
        atime: zero,
        mtime: zero,
        ctime: zero,
    }
}

/// The open master `io` is, if it is one.
pub(crate) fn master_of(io: &Arc<dyn Inode>) -> Option<Arc<MasterFile>> {
    Arc::clone(io).into_any().downcast::<MasterFile>().ok()
}

/// The open slave `io` is, if it is one.
pub(crate) fn slave_of(io: &Arc<dyn Inode>) -> Option<Arc<SlaveFile>> {
    Arc::clone(io).into_any().downcast::<SlaveFile>().ok()
}

/// Unlock a pair, as `TIOCSPTLCK` does with a zero.
pub(crate) fn set_locked(pty: &Pty, locked: bool) {
    pty.state.lock().locked = locked;
}

/// Whether a pair is locked.
pub(crate) fn locked(pty: &Pty) -> bool {
    pty.state.lock().locked
}
