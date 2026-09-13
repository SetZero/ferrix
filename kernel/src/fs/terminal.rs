//! The console's terminal: its settings, and the line discipline they drive.
//!
//! # Why the settings have to be honoured
//!
//! Because answering `TCGETS` is a promise. A program that learns it is on a
//! terminal is entitled to change how the terminal behaves, and busybox's
//! `sh -i` does so at once: it turns off `ICANON` and `ECHO` and reads a byte
//! at a time, doing its own echo and line editing. Stage 8's console echoed
//! and edited in the kernel whatever it was told, so it refused `TCGETS` to
//! stop the shell from asking. Now the discipline reads the settings for every
//! byte, and with the defaults it does exactly what that console did.
//!
//! # What is honoured
//!
//! * Input mapping: `ISTRIP`, `IGNCR`, `ICRNL`, `INLCR`.
//! * `ISIG`: the interrupt, quit and suspend characters are taken out of the
//!   input, the queue is flushed unless `NOFLSH`, and
//!   `crate::syscall::tty::signal_foreground_group` is called with the signal.
//! * `ICANON`: a line at a time, with erase (`VERASE`, and Backspace too, as
//!   the console always accepted), word erase (`VWERASE` under `IEXTEN`), line
//!   kill (`VKILL`), end of file (`VEOF`) and the extra end-of-line characters.
//!   Without it, bytes are readable as they arrive, and `VMIN` and `VTIME`
//!   decide how long a read waits.
//! * `ECHO`, `ECHOE`, `ECHOK`, `ECHOKE` and `ECHONL`. Control characters
//!   other than the signal characters echo as themselves: `ECHOCTL`'s `^X`
//!   would need the erase to know how wide each character was drawn.
//! * Output: `OPOST` with `ONLCR`, the newline translation the console has
//!   always done.
//!
//! Flow control (`IXON`) and the baud rate are stored and reported but change
//! nothing: QEMU's serial port and the boards' USB bridges have neither.
//!
//! # Input arrives when somebody looks
//!
//! There is no receive interrupt, so the UART is drained by [`read`], [`poll`]
//! and `FIONREAD`, and that is when a byte is echoed. A person typing at a
//! prompt cannot tell: something is always reading or polling the console
//! while a prompt is up. It is the part of this that becomes an interrupt
//! handler when the UART drivers can take one.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use ferrix_linux_abi::types::{
    B115200, CLOCAL, CREAD, CS8, ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, ECHONL, ICANON, ICRNL,
    IEXTEN, IGNCR, INLCR, ISIG, ISTRIP, IXON, NCCS, NOFLSH, ONLCR, OPOST, SIGINT, SIGQUIT, SIGTSTP,
    TERMIOS_BYTES, VEOF, VEOL, VEOL2, VERASE, VINTR, VKILL, VMIN, VQUIT, VSUSP, VTIME, VWERASE,
};
use ferrix_sync::SpinLock;
use ferrix_vfs::{Errno, Readiness};

use crate::arch;
use crate::console;

/// Carriage return, which a terminal in raw mode sends for the Enter key.
const CR: u8 = b'\r';
/// Backspace, which some terminals send for the Backspace key instead of
/// `VERASE`'s Delete. The console has always taken both.
const BS: u8 = 0x08;

/// The longest line canonical mode collects, as Linux's `N_TTY_BUF_SIZE`
/// less the newline.
const MAX_CANON: usize = 4095;
/// The most input held for a reader. Beyond it, bytes are dropped, as Linux
/// drops them.
const INPUT_LIMIT: usize = 4096;

/// How long a console read sleeps between looks for a keystroke.
///
/// Two milliseconds is shorter than anyone types, and long enough that a shell
/// waiting at its prompt costs a processor nothing measurable.
const POLL_NANOS: u64 = 2_000_000;
/// A tenth of a second, `VTIME`'s unit.
const DECISECOND_NANOS: u64 = 100_000_000;

/// The kernel's `struct termios`: what `TCGETS` reports and `TCSETS` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Termios {
    /// Input modes.
    pub(crate) iflag: u32,
    /// Output modes.
    pub(crate) oflag: u32,
    /// Control modes.
    pub(crate) cflag: u32,
    /// Local modes.
    pub(crate) lflag: u32,
    /// The line discipline number, which is always `N_TTY`'s zero here but is
    /// kept as a program sets it.
    pub(crate) line: u8,
    /// The control characters, indexed by `VINTR` and its kin.
    pub(crate) cc: [u8; NCCS],
}

impl Termios {
    /// Linux's `tty_std_termios`, at the serial console's 115200 baud: a
    /// canonical, echoing terminal with signals, which is what the console did
    /// before it had settings.
    pub(crate) const DEFAULT: Termios = Termios {
        iflag: ICRNL | IXON,
        oflag: OPOST | ONLCR,
        cflag: B115200 | CS8 | CREAD | CLOCAL,
        lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
        line: 0,
        // `INIT_C_CC`: ^C ^\ DEL ^U ^D, VTIME 0, VMIN 1, VSWTC 0, ^Q ^S ^Z,
        // VEOL 0, ^R ^O ^W ^V, VEOL2 0, and the two spare slots.
        cc: [
            0x03, 0x1C, 0x7F, 0x15, 0x04, 0, 1, 0, 0x11, 0x13, 0x1A, 0, 0x12, 0x0F, 0x17, 0x16, 0,
            0, 0,
        ],
    };

    /// The structure as a program reads it: four little-endian flag words,
    /// `c_line`, then `c_cc`.
    pub(crate) fn to_bytes(self) -> [u8; TERMIOS_BYTES] {
        let mut bytes = [0_u8; TERMIOS_BYTES];
        let flags = [self.iflag, self.oflag, self.cflag, self.lflag];
        let tail = core::iter::once(self.line).chain(self.cc);
        let all = flags.into_iter().flat_map(u32::to_le_bytes).chain(tail);
        for (slot, byte) in bytes.iter_mut().zip(all) {
            *slot = byte;
        }
        bytes
    }

    /// The structure as a program wrote it.
    pub(crate) fn from_bytes(bytes: &[u8; TERMIOS_BYTES]) -> Termios {
        let word = |at: usize| {
            let mut four = [0_u8; 4];
            for (slot, byte) in four.iter_mut().zip(bytes.iter().skip(at)) {
                *slot = *byte;
            }
            u32::from_le_bytes(four)
        };
        let mut cc = [0_u8; NCCS];
        for (slot, byte) in cc.iter_mut().zip(bytes.iter().skip(17)) {
            *slot = *byte;
        }
        Termios {
            iflag: word(0),
            oflag: word(4),
            cflag: word(8),
            lflag: word(12),
            line: bytes.get(16).copied().unwrap_or(0),
            cc,
        }
    }

    /// Whether reads are a line at a time.
    pub(crate) const fn canonical(&self) -> bool {
        self.lflag & ICANON != 0
    }

    /// The control character at `index`.
    pub(crate) fn cc(&self, index: usize) -> u8 {
        self.cc.get(index).copied().unwrap_or(0)
    }

    /// Whether `byte` is the control character at `index`. Zero disables a
    /// character, which is Linux's `_POSIX_VDISABLE`.
    fn is(&self, index: usize, byte: u8) -> bool {
        let special = self.cc(index);
        special != 0 && special == byte
    }
}

/// `struct winsize`: rows, columns, and the two pixel sizes nobody fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Winsize {
    /// Rows.
    pub(crate) rows: u16,
    /// Columns.
    pub(crate) columns: u16,
    /// Width in pixels.
    pub(crate) x_pixels: u16,
    /// Height in pixels.
    pub(crate) y_pixels: u16,
}

impl Winsize {
    /// What a serial terminal is assumed to be until somebody says otherwise.
    pub(crate) const DEFAULT: Winsize = Winsize {
        rows: 24,
        columns: 80,
        x_pixels: 0,
        y_pixels: 0,
    };

    /// The structure as a program reads it.
    pub(crate) fn to_bytes(self) -> [u8; 8] {
        let mut bytes = [0_u8; 8];
        let fields = [self.rows, self.columns, self.x_pixels, self.y_pixels];
        for (slot, byte) in bytes
            .iter_mut()
            .zip(fields.into_iter().flat_map(u16::to_le_bytes))
        {
            *slot = byte;
        }
        bytes
    }

    /// The structure as a program wrote it.
    pub(crate) fn from_bytes(bytes: [u8; 8]) -> Winsize {
        let [r0, r1, c0, c1, x0, x1, y0, y1] = bytes;
        Winsize {
            rows: u16::from_le_bytes([r0, r1]),
            columns: u16::from_le_bytes([c0, c1]),
            x_pixels: u16::from_le_bytes([x0, x1]),
            y_pixels: u16::from_le_bytes([y0, y1]),
        }
    }
}

/// The line discipline: bytes in from the keyboard, bytes out to a reader,
/// and what to echo on the way.
///
/// Nothing in here touches a device, so the boot self-check can drive a
/// discipline of its own and read back exactly what it would have echoed.
#[derive(Debug)]
pub(crate) struct Discipline {
    /// The settings every byte is judged by.
    termios: Termios,
    /// The line being typed, in canonical mode.
    editing: Vec<u8>,
    /// Bytes a reader may have.
    ready: VecDeque<u8>,
    /// Where each finished line ends, as a count of bytes ever made ready. An
    /// end equal to the one before it, or to `taken`, is an empty line: end of
    /// file.
    ends: VecDeque<u64>,
    /// Bytes ever made ready.
    pushed: u64,
    /// Bytes ever read.
    taken: u64,
}

impl Discipline {
    /// A discipline with the default settings and nothing typed.
    pub(crate) const fn new() -> Discipline {
        Discipline {
            termios: Termios::DEFAULT,
            editing: Vec::new(),
            ready: VecDeque::new(),
            ends: VecDeque::new(),
            pushed: 0,
            taken: 0,
        }
    }

    /// The settings.
    pub(crate) const fn termios(&self) -> Termios {
        self.termios
    }

    /// Change the settings.
    ///
    /// Leaving canonical mode makes the half-typed line readable, as Linux
    /// does; entering it ends whatever raw input is waiting as a line, so that
    /// it stays readable rather than waiting for a newline that already came.
    pub(crate) fn set_termios(&mut self, termios: Termios) {
        let was = self.termios.canonical();
        self.termios = termios;
        if was && !termios.canonical() {
            let line = core::mem::take(&mut self.editing);
            self.make_ready(&line);
        } else if !was
            && termios.canonical()
            && self.pushed > self.taken
            && self.ends.back() != Some(&self.pushed)
        {
            self.ends.push_back(self.pushed);
        }
    }

    /// Discard everything typed and not yet read.
    pub(crate) fn flush_input(&mut self) {
        self.editing.clear();
        self.ready.clear();
        self.ends.clear();
        self.taken = self.pushed;
    }

    /// Take one byte from the keyboard, appending what it echoes to `echo`.
    ///
    /// Returns the signal it raises, if it is a signal character under `ISIG`.
    pub(crate) fn receive(&mut self, byte: u8, echo: &mut Vec<u8>) -> Option<u32> {
        let termios = self.termios;
        let mut byte = if termios.iflag & ISTRIP != 0 {
            byte & 0x7F
        } else {
            byte
        };
        if byte == CR {
            if termios.iflag & IGNCR != 0 {
                return None;
            }
            if termios.iflag & ICRNL != 0 {
                byte = b'\n';
            }
        } else if byte == b'\n' && termios.iflag & INLCR != 0 {
            byte = CR;
        }

        if termios.lflag & ISIG != 0 {
            let signal = [(VINTR, SIGINT), (VQUIT, SIGQUIT), (VSUSP, SIGTSTP)]
                .into_iter()
                .find(|&(index, _)| termios.is(index, byte))
                .map(|(_, signal)| signal);
            if let Some(signal) = signal {
                if termios.lflag & NOFLSH == 0 {
                    self.flush_input();
                }
                echo_signal_character(&termios, byte, echo);
                return Some(signal);
            }
        }

        if termios.canonical() {
            self.edit(byte, echo);
        } else {
            self.make_ready(&[byte]);
            if termios.lflag & ECHO != 0 {
                echo.push(byte);
            }
        }
        None
    }

    /// One byte of canonical input.
    fn edit(&mut self, byte: u8, echo: &mut Vec<u8>) {
        let termios = self.termios;
        let echoing = termios.lflag & ECHO != 0;
        let extended = termios.lflag & IEXTEN != 0;
        if termios.is(VERASE, byte) || byte == BS {
            let _ = self.erase(1, |_| true, byte, echo);
            return;
        }
        if extended && termios.is(VWERASE, byte) {
            let blank = |c: u8| c == b' ' || c == b'\t';
            let _ = self.erase(usize::MAX, blank, byte, echo);
            let _ = self.erase(usize::MAX, |c| !blank(c), byte, echo);
            return;
        }
        if termios.is(VKILL, byte) {
            let erased = self.erase(usize::MAX, |_| true, byte, echo);
            if echoing && termios.lflag & ECHOKE == 0 && erased > 0 {
                echo.push(byte);
                if termios.lflag & ECHOK != 0 {
                    echo.push(b'\n');
                }
            }
            return;
        }
        if termios.is(VEOF, byte) {
            // The line so far, without the character: on an empty line, that
            // is an end of file.
            let line = core::mem::take(&mut self.editing);
            self.finish_line(&line);
            return;
        }
        let ends_line =
            byte == b'\n' || termios.is(VEOL, byte) || (extended && termios.is(VEOL2, byte));
        if !ends_line && self.editing.len() >= MAX_CANON {
            return;
        }
        self.editing.push(byte);
        if echoing || (byte == b'\n' && termios.lflag & ECHONL != 0) {
            echo.push(byte);
        }
        if ends_line {
            let line = core::mem::take(&mut self.editing);
            self.finish_line(&line);
        }
    }

    /// Erase up to `limit` characters from the end of the line while `take`
    /// accepts them, echoing each erasure. Returns how many went.
    ///
    /// With `ECHOE` (and `ECHOKE` for a line kill) an erasure is drawn as
    /// backspace, space, backspace; without it the erase character itself is
    /// echoed, once, which is what a printing terminal wants.
    fn erase(
        &mut self,
        limit: usize,
        take: impl Fn(u8) -> bool,
        byte: u8,
        echo: &mut Vec<u8>,
    ) -> usize {
        let termios = self.termios;
        let visual =
            termios.lflag & ECHOE != 0 && (!termios.is(VKILL, byte) || termios.lflag & ECHOKE != 0);
        let mut erased = 0;
        while erased < limit && self.editing.last().is_some_and(|&c| take(c)) {
            let _ = self.editing.pop();
            erased += 1;
            if termios.lflag & ECHO != 0 && visual {
                echo.extend_from_slice(b"\x08 \x08");
            }
        }
        if erased > 0 && termios.lflag & ECHO != 0 && !visual && !termios.is(VKILL, byte) {
            echo.push(byte);
        }
        erased
    }

    /// Make `line` readable as one line.
    fn finish_line(&mut self, line: &[u8]) {
        if self.ready.len().saturating_add(line.len()) > INPUT_LIMIT {
            return;
        }
        self.make_ready(line);
        self.ends.push_back(self.pushed);
    }

    /// Make `bytes` readable, as far as there is room.
    fn make_ready(&mut self, bytes: &[u8]) {
        let room = INPUT_LIMIT.saturating_sub(self.ready.len());
        let fits = bytes.get(..room.min(bytes.len())).unwrap_or_default();
        self.ready.extend(fits);
        self.pushed = self.pushed.saturating_add(fits.len() as u64);
    }

    /// Whether a read would return without waiting for a keystroke.
    pub(crate) fn readable(&self) -> bool {
        if self.termios.canonical() {
            !self.ends.is_empty()
        } else {
            !self.ready.is_empty()
        }
    }

    /// How many bytes a reader could have now: `FIONREAD`'s answer. In
    /// canonical mode, only the finished lines.
    pub(crate) fn available(&self) -> usize {
        if self.termios.canonical() {
            self.ends.back().map_or(0, |&end| {
                usize::try_from(end - self.taken).unwrap_or(usize::MAX)
            })
        } else {
            self.ready.len()
        }
    }

    /// Read into `buf`: the rest of the first finished line in canonical mode,
    /// as much as there is otherwise. `None` if there is nothing to read;
    /// `Some(0)` for an end of file.
    pub(crate) fn take(&mut self, buf: &mut [u8]) -> Option<usize> {
        let count = if self.termios.canonical() {
            let end = *self.ends.front()?;
            let line = usize::try_from(end - self.taken).unwrap_or(usize::MAX);
            buf.len().min(line)
        } else if self.ready.is_empty() {
            return None;
        } else {
            buf.len().min(self.ready.len())
        };
        for (slot, byte) in buf.iter_mut().zip(self.ready.drain(..count)) {
            *slot = byte;
        }
        self.taken += count as u64;
        while self.ends.front().is_some_and(|&end| end <= self.taken) {
            let _ = self.ends.pop_front();
            if self.termios.canonical() {
                break;
            }
        }
        Some(count)
    }
}

/// Echo a signal character: as `^C` under `ECHOCTL`, as itself otherwise,
/// and not at all without `ECHO`.
fn echo_signal_character(termios: &Termios, byte: u8, echo: &mut Vec<u8>) {
    if termios.lflag & ECHO == 0 {
        return;
    }
    if termios.lflag & ECHOCTL != 0 {
        echo.extend_from_slice(&[b'^', byte ^ 0x40]);
    } else {
        echo.push(byte);
    }
}

impl Default for Discipline {
    fn default() -> Self {
        Discipline::new()
    }
}

/// Everything the console's terminal knows.
#[derive(Debug)]
pub(crate) struct Terminal {
    /// Its settings and its input.
    pub(crate) discipline: Discipline,
    /// Its size, as last set.
    pub(crate) winsize: Winsize,
    /// The session it is the controlling terminal of, or zero for none.
    pub(crate) session: u32,
    /// Its foreground process group, or zero for none.
    pub(crate) foreground: u32,
}

/// The one terminal: there is one console.
static TERMINAL: SpinLock<Terminal> = SpinLock::new(Terminal {
    discipline: Discipline::new(),
    winsize: Winsize::DEFAULT,
    session: 0,
    foreground: 0,
});

/// Look at or change the terminal.
///
/// `change` runs under a spin lock: it must not touch user memory, sleep, or
/// drop the last reference to anything whose drop does either.
pub(crate) fn with<R>(change: impl FnOnce(&mut Terminal) -> R) -> R {
    change(&mut TERMINAL.lock())
}

/// Take whatever has been typed into the line discipline, echo it, and raise
/// any signal it asked for.
pub(crate) fn pump() {
    let mut echo = Vec::new();
    let mut signals = Vec::new();
    let termios = with(|terminal| {
        while let Some(byte) = arch::read_console_byte() {
            if let Some(signal) = terminal.discipline.receive(byte, &mut echo) {
                signals.push(signal);
            }
        }
        terminal.discipline.termios()
    });
    if !echo.is_empty() {
        output(&termios, &echo);
    }
    for signal in signals {
        crate::syscall::tty::signal_foreground_group(signal);
    }
}

/// Write `bytes` to the console as the terminal's output settings say.
fn output(termios: &Termios, bytes: &[u8]) {
    if termios.oflag & OPOST != 0 && termios.oflag & ONLCR != 0 {
        console::write_bytes(bytes);
    } else {
        console::write_raw(bytes);
    }
}

/// A program's write to the console.
pub(crate) fn write(bytes: &[u8]) {
    let termios = with(|terminal| terminal.discipline.termios());
    output(&termios, bytes);
}

/// What `poll` reports: always writable, readable when a read would not wait.
pub(crate) fn poll() -> Readiness {
    pump();
    Readiness {
        readable: with(|terminal| terminal.discipline.readable()),
        writable: true,
        hangup: false,
        error: false,
    }
}

/// `FIONREAD`: how many bytes a read could have now.
pub(crate) fn available() -> usize {
    pump();
    with(|terminal| terminal.discipline.available())
}

/// A program's read of the console.
///
/// Canonical mode waits for a line. Otherwise `VMIN` and `VTIME` decide, as
/// on Linux: `VMIN` bytes (at most the buffer), or, with `VTIME` set, whatever
/// arrived once `VTIME` tenths of a second pass -- since the read began when
/// `VMIN` is zero, since the last byte when it is not.
///
/// No lock is held while it waits: the wait may last minutes. A program ended
/// while it waits stops waiting, and reads end of file. With `nonblock` -- the
/// description's `O_NONBLOCK` -- a read that would wait takes whatever raw
/// input there is, and is `EAGAIN` if there is none or no line is finished.
///
/// # Errors
///
/// `EAGAIN`, as above.
pub(crate) fn read(buf: &mut [u8], nonblock: bool) -> Result<usize, Errno> {
    if buf.is_empty() {
        return Ok(0);
    }
    let started = now();
    let mut seen = 0;
    let mut changed = started;
    loop {
        pump();
        let answer = with(|terminal| {
            let discipline = &mut terminal.discipline;
            let termios = discipline.termios();
            if termios.canonical() {
                return discipline.take(buf);
            }
            let min = usize::from(termios.cc(VMIN));
            let time = u64::from(termios.cc(VTIME)) * DECISECOND_NANOS;
            let available = discipline.available();
            if available != seen {
                seen = available;
                changed = now();
            }
            let since = if min == 0 { started } else { changed };
            let enough = available >= min.clamp(1, buf.len());
            let timed_out =
                time > 0 && now().saturating_sub(since) >= time && (min == 0 || available > 0);
            if enough || timed_out || (min == 0 && time == 0) || (nonblock && available > 0) {
                return Some(discipline.take(buf).unwrap_or(0));
            }
            None
        });
        if let Some(count) = answer {
            return Ok(count);
        }
        if nonblock {
            return Err(Errno::EAGAIN);
        }
        let killed =
            crate::syscall::process::current().is_some_and(|process| process.is_terminated());
        if killed {
            return Ok(0);
        }
        crate::sched::sleep_for(POLL_NANOS);
    }
}

/// The counter's reading, in nanoseconds.
fn now() -> u64 {
    crate::timer::now_nanos()
}
