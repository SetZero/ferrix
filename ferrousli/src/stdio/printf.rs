//! The `printf` family: the format grammar, the arguments, and where the
//! output goes.
//!
//! # The grammar
//!
//! A directive is `%`, an optional position `n$`, flags from `-+ #0'`, a
//! width (digits, `*` or `*m$`), a precision (`.` then digits, `*` or `*m$`),
//! a length (`hh h l ll j z t L q`), and a conversion from
//! `d i o u x X f F e E g G a A c s p n m %`. `'` groups nothing in the C
//! locale, and `%lc` and `%ls` write the characters below 0x80 and fail with
//! `EILSEQ` on any other, as the C locale's single-byte encoding requires.
//!
//! Where C leaves the choice open, glibc's behaviour is kept, since programs
//! built against glibc are the destination:
//!
//! * `%p` of null is `(nil)`, and of anything else `%#lx`, with `+` and space
//!   honoured;
//! * `%s` of null is `(null)`, or nothing if the precision is below 6;
//! * `%%` ignores any flags and width;
//! * `L` and `q` with an integer conversion mean `long long`;
//! * a position that no directive uses is read as an `int`.
//!
//! And musl's, where glibc's is to print text C calls undefined: an unknown
//! conversion, a format ending inside a directive, or a mix of positional and
//! sequential arguments fails with `EINVAL`. Any output or `%n` count past
//! `INT_MAX` fails with `EOVERFLOW` before it is written. Positions run from 1
//! to [`MAX_POSITION`].
//!
//! # Positional arguments
//!
//! A `va_list` can only be read in order, so a format whose first argument is
//! positional is scanned first, as musl does: the type of every position is
//! recorded, all of them are read in order into an array, and the directives
//! are then formatted from the array. A format that starts sequentially is
//! formatted as it is read.
//!
//! # Sinks
//!
//! Output goes to a [`Sink`]: a string for `snprintf`, a stream for `fprintf`,
//! a descriptor for `dprintf`. The stream and descriptor sinks gather output
//! in a 256-byte chunk, so that an unbuffered stream such as standard error
//! takes a `printf` in one write when it fits.
//!
//! `%m` prints `strerror` of `errno` as it was when the call began.

use core::ffi::{c_char, c_int, c_long, c_ulong};
use core::ptr::{null_mut, with_exposed_provenance_mut};
use core::slice;
use core::sync::atomic::Ordering;

use super::file::{self, File, Inner};
use super::float::{self, Float};
use super::sys;
use crate::va::{self, LongDouble, VaList, VaListArg};
use crate::{errno, malloc, strerror, string};

/// `INT_MAX`, the most a `printf` may count.
pub const INT_MAX: usize = c_int::MAX as usize;

/// The `-` flag: pad on the right.
pub const LEFT: u8 = 1;
/// The `+` flag: sign positive numbers.
pub const PLUS: u8 = 2;
/// The space flag: a space before positive numbers.
pub const SPACE: u8 = 4;
/// The `#` flag: the alternative form.
pub const ALT: u8 = 8;
/// The `0` flag: pad numbers with zeros.
pub const ZERO: u8 = 16;

/// The highest argument position a format may name.
pub const MAX_POSITION: usize = 64;
/// Slots for positions, which start at 1.
const SLOTS: usize = MAX_POSITION + 1;

/// Where formatted output goes.
pub trait Sink {
    /// Takes `bytes`.
    fn write(&mut self, bytes: &[u8]);

    /// Takes `count` copies of `byte`.
    fn pad(&mut self, byte: u8, count: usize) {
        let chunk = [byte; 64];
        let mut left = count;
        while left != 0 && !self.failed() {
            let n = left.min(chunk.len());
            self.write(chunk.get(..n).unwrap_or_default());
            left -= n;
        }
    }

    /// Whether output has failed, so that formatting should stop.
    fn failed(&self) -> bool {
        false
    }

    /// Whether this sink takes wide characters, for the `wprintf` family: its
    /// `%c` and `%s` then count and write characters with [`Sink::write_wide`],
    /// and everything else still comes as ASCII bytes through [`Sink::write`].
    fn wide(&self) -> bool {
        false
    }

    /// Takes wide characters. Only a [`Sink::wide`] sink is given any.
    fn write_wide(&mut self, wcs: &[i32]) {
        let _ = wcs;
    }
}

/// Output into a string of known room, counting nothing past it.
#[derive(Debug)]
pub struct StringSink {
    /// The string.
    dst: *mut u8,
    /// How many bytes may be written.
    room: usize,
    /// How many have been.
    used: usize,
}

impl StringSink {
    /// A sink writing at most `room` bytes at `dst`.
    ///
    /// # Safety
    ///
    /// `dst` must be valid for writes of `room` bytes while the sink is used.
    pub unsafe fn new(dst: *mut u8, room: usize) -> Self {
        Self { dst, room, used: 0 }
    }

    /// How many bytes were written.
    pub fn used(&self) -> usize {
        self.used
    }
}

impl Sink for StringSink {
    fn write(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.room - self.used);
        if n != 0 {
            // SAFETY: the sink's creator vouched for `room` bytes, and
            // `used + n` is within them.
            let _ = unsafe {
                string::memcpy(
                    self.dst.wrapping_add(self.used).cast(),
                    bytes.as_ptr().cast(),
                    n,
                )
            };
            self.used += n;
        }
    }

    fn pad(&mut self, byte: u8, count: usize) {
        let n = count.min(self.room - self.used);
        if n != 0 {
            // SAFETY: as in `write`.
            let _ = unsafe {
                string::memset(
                    self.dst.wrapping_add(self.used).cast(),
                    c_int::from(byte),
                    n,
                )
            };
            self.used += n;
        }
    }
}

/// The size of the chunk the stream and descriptor sinks gather.
const CHUNK: usize = 256;

/// Output into a stream whose lock is held.
#[derive(Debug)]
pub struct FileSink<'a> {
    /// The stream's state.
    file: &'a mut Inner,
    /// Output not yet given to the stream.
    chunk: [u8; CHUNK],
    /// How much of `chunk` is output.
    len: usize,
}

impl<'a> FileSink<'a> {
    /// A sink writing to `file`.
    pub fn new(file: &'a mut Inner) -> Self {
        Self {
            file,
            chunk: [0; CHUNK],
            len: 0,
        }
    }

    /// Gives the gathered output to the stream.
    pub fn flush(&mut self) {
        let len = self.len;
        self.len = 0;
        if len != 0 {
            // SAFETY: the first `len` bytes of the chunk are output.
            let _ = unsafe { self.file.write(self.chunk.as_ptr(), len) };
        }
    }
}

impl Sink for FileSink<'_> {
    fn write(&mut self, bytes: &[u8]) {
        if bytes.len() > CHUNK - self.len {
            self.flush();
            if bytes.len() >= CHUNK {
                // SAFETY: `bytes` is a live slice.
                let _ = unsafe { self.file.write(bytes.as_ptr(), bytes.len()) };
                return;
            }
        }
        if let Some(slot) = self.chunk.get_mut(self.len..self.len + bytes.len()) {
            slot.copy_from_slice(bytes);
            self.len += bytes.len();
        }
    }

    fn failed(&self) -> bool {
        self.file.error
    }
}

/// Output to a descriptor.
#[derive(Debug)]
struct FdSink {
    /// The descriptor.
    fd: c_int,
    /// Output not yet written.
    chunk: [u8; CHUNK],
    /// How much of `chunk` is output.
    len: usize,
    /// Whether a write failed, with `errno` set.
    failed: bool,
}

impl FdSink {
    /// Writes all of `bytes`, unless a write already failed.
    fn write_through(&mut self, bytes: &[u8]) {
        let mut done = 0;
        while done < bytes.len() && !self.failed {
            let rest = bytes.get(done..).unwrap_or_default();
            // SAFETY: `rest` is a live slice.
            match unsafe { sys::write(self.fd, rest.as_ptr(), rest.len()) } {
                Ok(0) => {
                    errno::set(errno::EIO);
                    self.failed = true;
                }
                Ok(n) => done += n,
                Err(errno::EINTR) => {}
                Err(error) => {
                    errno::set(error);
                    self.failed = true;
                }
            }
        }
    }

    /// Writes the gathered output.
    fn flush(&mut self) {
        let chunk = self.chunk;
        let len = self.len;
        self.len = 0;
        self.write_through(chunk.get(..len).unwrap_or_default());
    }
}

impl Sink for FdSink {
    fn write(&mut self, bytes: &[u8]) {
        if bytes.len() > CHUNK - self.len {
            self.flush();
            if bytes.len() >= CHUNK {
                self.write_through(bytes);
                return;
            }
        }
        if let Some(slot) = self.chunk.get_mut(self.len..self.len + bytes.len()) {
            slot.copy_from_slice(bytes);
            self.len += bytes.len();
        }
    }

    fn failed(&self) -> bool {
        self.failed
    }
}

/// An argument, read by its class.
#[derive(Debug, Clone, Copy)]
enum Arg {
    /// An integer or pointer, in a whole word.
    Word(u64),
    /// A `double`.
    Double(f64),
    /// A `long double`.
    Long(LongDouble),
}

impl Arg {
    /// The word, or zero for a floating-point argument.
    fn word(self) -> u64 {
        match self {
            Self::Word(word) => word,
            Self::Double(_) | Self::Long(_) => 0,
        }
    }
}

/// How an argument is passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// No argument.
    Unused,
    /// In an integer register or stack word: an `int`, a `long` or a
    /// pointer.
    Word,
    /// A 64-bit integer: the same as [`Class::Word`] on a 64-bit target, and
    /// two registers or an aligned pair of stack words on ARMv7-A.
    Wide,
    /// In a vector register or stack word.
    Double,
    /// On the stack, in 16 bytes.
    Long,
}

/// A length modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    /// `hh`.
    Char,
    /// `h`.
    Short,
    /// None.
    Int,
    /// `l`, `z` and `t`: a `long`, a `size_t` and a `ptrdiff_t`, which are
    /// the same width on every target here.
    Long,
    /// `ll`, `j` and `q`: 64 bits on every target.
    Wide,
    /// `L`.
    LongDouble,
}

/// A width or precision as a format gives it.
#[derive(Debug, Clone, Copy)]
enum Count {
    /// Not given.
    Absent,
    /// Digits.
    Fixed(usize),
    /// `*`: the next argument.
    Next,
    /// `*m$`: argument `m`.
    Position(usize),
}

/// A parsed directive.
#[derive(Debug, Clone, Copy)]
struct Directive {
    /// The `n$` position of its argument.
    position: Option<usize>,
    /// The flags.
    flags: u8,
    /// The width.
    width: Count,
    /// The precision.
    precision: Count,
    /// The length modifier.
    length: Length,
    /// The conversion character.
    conversion: u8,
    /// The byte after the directive.
    end: *const u8,
}

/// A directive's flags, width and precision once `*` arguments are read.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// The flags, `LEFT` and `ZERO` never both.
    pub flags: u8,
    /// The width.
    pub width: usize,
    /// The precision, if given and not negative.
    pub precision: Option<usize>,
}

/// Fails with `EOVERFLOW` if `len` more bytes would take the count past
/// `INT_MAX`.
pub fn reserve(count: usize, len: usize) -> Result<(), c_int> {
    if len > INT_MAX.saturating_sub(count) {
        return Err(errno::EOVERFLOW);
    }
    Ok(())
}

/// Starts a field of `body` bytes: checks the count, and pads on the left
/// unless the field is left-justified. Returns the field's width.
pub fn begin(sink: &mut dyn Sink, spec: &Spec, body: usize, count: usize) -> Result<usize, c_int> {
    let total = body.max(spec.width);
    reserve(count, total)?;
    if spec.flags & LEFT == 0 {
        sink.pad(b' ', total - body);
    }
    Ok(total)
}

/// Ends a field started with [`begin`], padding on the right if it is
/// left-justified.
pub fn end(sink: &mut dyn Sink, spec: &Spec, body: usize, total: usize) {
    if spec.flags & LEFT != 0 {
        sink.pad(b' ', total - body);
    }
}

/// The byte at `p`.
///
/// # Safety
///
/// `p` must be readable.
unsafe fn byte_at(p: *const u8) -> u8 {
    // SAFETY: the caller vouches for the byte.
    unsafe { p.read() }
}

/// Reads decimal digits at `*p`, advancing past them. `None` if there are
/// none; `EOVERFLOW` above `INT_MAX`.
///
/// # Safety
///
/// `*p` must be inside a NUL-terminated string.
unsafe fn number(p: &mut *const u8) -> Result<Option<usize>, c_int> {
    let mut value = None;
    loop {
        // SAFETY: no NUL came before `*p`.
        let c = unsafe { byte_at(*p) };
        if !c.is_ascii_digit() {
            return Ok(value);
        }
        let next = value
            .unwrap_or(0_usize)
            .checked_mul(10)
            .and_then(|v| v.checked_add(usize::from(c - b'0')))
            .filter(|v| *v <= INT_MAX)
            .ok_or(errno::EOVERFLOW)?;
        value = Some(next);
        *p = p.wrapping_add(1);
    }
}

/// Reads a position `n$` at `*p`, advancing past it, or leaves `*p` alone.
///
/// # Safety
///
/// `*p` must be inside a NUL-terminated string.
unsafe fn position(p: &mut *const u8) -> Result<Option<usize>, c_int> {
    // SAFETY: the caller vouches for `*p`.
    if !matches!(unsafe { byte_at(*p) }, b'1'..=b'9') {
        return Ok(None);
    }
    let mut q = *p;
    // SAFETY: as above.
    let Ok(Some(n)) = (unsafe { number(&mut q) }) else {
        return Ok(None);
    };
    // SAFETY: `number` stopped at a byte that is not a digit, inside the
    // string.
    if unsafe { byte_at(q) } != b'$' {
        return Ok(None);
    }
    if n > MAX_POSITION {
        return Err(errno::EINVAL);
    }
    *p = q.wrapping_add(1);
    Ok(Some(n))
}

/// Reads a width or precision at `*p`.
///
/// # Safety
///
/// `*p` must be inside a NUL-terminated string.
unsafe fn count(p: &mut *const u8) -> Result<Count, c_int> {
    // SAFETY: the caller vouches for `*p`.
    if unsafe { byte_at(*p) } == b'*' {
        *p = p.wrapping_add(1);
        // SAFETY: the `*` was not the NUL.
        return Ok(match unsafe { position(p) }? {
            Some(n) => Count::Position(n),
            None => Count::Next,
        });
    }
    // SAFETY: as above.
    Ok(unsafe { number(p) }?.map_or(Count::Absent, Count::Fixed))
}

/// Parses the directive after a `%` at `start - 1`.
///
/// # Safety
///
/// `start` must be inside a NUL-terminated string.
unsafe fn parse(start: *const u8) -> Result<Directive, c_int> {
    let mut p = start;
    // SAFETY: each read below is at a byte not past the NUL, since every
    // advance steps over a byte that was checked not to be the NUL.
    let position = unsafe { position(&mut p) }?;
    let mut flags = 0;
    loop {
        // SAFETY: as above.
        let flag = match unsafe { byte_at(p) } {
            b'-' => LEFT,
            b'+' => PLUS,
            b' ' => SPACE,
            b'#' => ALT,
            b'0' => ZERO,
            b'\'' => 0,
            _ => break,
        };
        flags |= flag;
        p = p.wrapping_add(1);
    }
    // SAFETY: as above.
    let width = unsafe { count(&mut p) }?;
    // SAFETY: as above.
    let precision = if unsafe { byte_at(p) } == b'.' {
        p = p.wrapping_add(1);
        // SAFETY: as above.
        match unsafe { count(&mut p) }? {
            Count::Absent => Count::Fixed(0),
            given => given,
        }
    } else {
        Count::Absent
    };
    // SAFETY: as above.
    let first = unsafe { byte_at(p) };
    // SAFETY: as above; the second byte is read only after a first that is
    // not the NUL.
    let second = || unsafe { byte_at(p.wrapping_add(1)) };
    let (length, skip) = match first {
        b'h' if second() == b'h' => (Length::Char, 2),
        b'h' => (Length::Short, 1),
        b'l' if second() == b'l' => (Length::Wide, 2),
        b'l' => (Length::Long, 1),
        b'z' | b't' => (Length::Long, 1),
        b'j' | b'q' => (Length::Wide, 1),
        b'L' => (Length::LongDouble, 1),
        _ => (Length::Int, 0),
    };
    p = p.wrapping_add(skip);
    // SAFETY: as above.
    let conversion = unsafe { byte_at(p) };
    if !b"diouxXfFeEgGaAcspnm%".contains(&conversion) {
        return Err(errno::EINVAL);
    }
    Ok(Directive {
        position,
        flags,
        width,
        precision,
        length,
        conversion,
        end: p.wrapping_add(1),
    })
}

/// How a directive's own argument is passed.
fn class_of(directive: &Directive) -> Class {
    match directive.conversion {
        b'%' | b'm' => Class::Unused,
        b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
            if directive.length == Length::LongDouble {
                Class::Long
            } else {
                Class::Double
            }
        }
        b'd' | b'i' | b'o' | b'u' | b'x' | b'X'
            if matches!(directive.length, Length::Wide | Length::LongDouble) =>
        {
            Class::Wide
        }
        _ => Class::Word,
    }
}

/// The next `%` or the NUL at or after `p`.
///
/// # Safety
///
/// `p` must be inside a NUL-terminated string.
unsafe fn next_directive(p: *const u8) -> *const u8 {
    // SAFETY: the caller vouches for the string.
    unsafe { string::strchrnul(p.cast(), c_int::from(b'%')) }
        .cast_const()
        .cast()
}

/// Whether the first directive that takes an argument takes it by position.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated string.
unsafe fn starts_positional(fmt: *const u8) -> bool {
    let mut p = fmt;
    loop {
        // SAFETY: `p` is inside the format.
        p = unsafe { next_directive(p) };
        // SAFETY: as above.
        if unsafe { byte_at(p) } == 0 {
            return false;
        }
        // SAFETY: the `%` is not the NUL.
        let Ok(directive) = (unsafe { parse(p.wrapping_add(1)) }) else {
            return false;
        };
        let by_position = |count| matches!(count, Count::Position(_));
        if directive.position.is_some()
            || by_position(directive.width)
            || by_position(directive.precision)
        {
            return true;
        }
        let next = |count| matches!(count, Count::Next);
        if next(directive.width)
            || next(directive.precision)
            || class_of(&directive) != Class::Unused
        {
            return false;
        }
        p = directive.end;
    }
}

/// Records every position's class, and returns the highest position used.
/// `EINVAL` if a directive takes an argument in order.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated string.
unsafe fn collect(fmt: *const u8, classes: &mut [Class; SLOTS]) -> Result<usize, c_int> {
    let mut highest = 0;
    let mut mark = |n: usize, class: Class| {
        if let Some(slot) = classes.get_mut(n) {
            *slot = class;
        }
        highest = highest.max(n);
    };
    let mut p = fmt;
    loop {
        // SAFETY: `p` is inside the format.
        p = unsafe { next_directive(p) };
        // SAFETY: as above.
        if unsafe { byte_at(p) } == 0 {
            return Ok(highest);
        }
        // SAFETY: the `%` is not the NUL.
        let directive = unsafe { parse(p.wrapping_add(1)) }?;
        for count in [directive.width, directive.precision] {
            match count {
                Count::Position(n) => mark(n, Class::Word),
                Count::Next => return Err(errno::EINVAL),
                Count::Absent | Count::Fixed(_) => {}
            }
        }
        let class = class_of(&directive);
        if class != Class::Unused {
            let Some(n) = directive.position else {
                return Err(errno::EINVAL);
            };
            mark(n, class);
        }
        p = directive.end;
    }
}

/// Reads the next argument of `class`.
///
/// # Safety
///
/// The next argument must be of that class.
unsafe fn read_arg(list: &mut VaList<'_>, class: Class) -> Arg {
    match class {
        // SAFETY: the caller vouches for the class.
        Class::Unused | Class::Word => Arg::Word(unsafe { list.next_word() }),
        // SAFETY: as above.
        Class::Wide => Arg::Word(unsafe { list.next_wide() }),
        // SAFETY: as above.
        Class::Double => Arg::Double(unsafe { list.next_double() }),
        // SAFETY: as above.
        Class::Long => Arg::Long(unsafe { list.next_long_double() }),
    }
}

/// Where the arguments come from.
enum Source<'s, 'l> {
    /// Read in order from the list.
    Sequential(&'s mut VaList<'l>),
    /// Read by position from what was collected.
    Positional(&'s [Arg; SLOTS]),
}

impl Source<'_, '_> {
    /// The argument at `position`, or the next one. `EINVAL` if the format
    /// mixes the two.
    ///
    /// # Safety
    ///
    /// Reading in order, the next argument must be of `class`.
    unsafe fn take(&mut self, position: Option<usize>, class: Class) -> Result<Arg, c_int> {
        match (self, position) {
            // SAFETY: the caller vouches for the class.
            (Source::Sequential(list), None) => Ok(unsafe { read_arg(list, class) }),
            (Source::Positional(args), Some(n)) => args.get(n).copied().ok_or(errno::EINVAL),
            _ => Err(errno::EINVAL),
        }
    }
}

/// A `*` argument's value.
fn star(arg: Arg) -> i32 {
    arg.word() as i32
}

/// Formats `fmt` into `sink` with the arguments from `source`. Returns the
/// count, or the error number; zero if the sink failed and set it.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated string, and the arguments must match it.
unsafe fn run(
    sink: &mut dyn Sink,
    fmt: *const u8,
    source: &mut Source<'_, '_>,
    saved_errno: c_int,
) -> Result<usize, c_int> {
    let mut count = 0_usize;
    let mut p = fmt;
    loop {
        // SAFETY: `p` is inside the format.
        let next = unsafe { next_directive(p) };
        let len = next.addr() - p.addr();
        if len != 0 {
            reserve(count, len)?;
            // SAFETY: the `len` bytes before `next` are the format's.
            sink.write(unsafe { slice::from_raw_parts(p, len) });
            count += len;
        }
        // SAFETY: `next` is inside the format.
        if unsafe { byte_at(next) } == 0 {
            return if sink.failed() { Err(0) } else { Ok(count) };
        }
        if sink.failed() {
            return Err(0);
        }
        // SAFETY: the `%` is not the NUL.
        let directive = unsafe { parse(next.wrapping_add(1)) }?;
        p = directive.end;

        let mut flags = directive.flags;
        let width = match directive.width {
            Count::Absent => 0,
            Count::Fixed(width) => width,
            Count::Next | Count::Position(_) => {
                let at = match directive.width {
                    Count::Position(n) => Some(n),
                    _ => None,
                };
                // SAFETY: the format says an `int` is there.
                let value = star(unsafe { source.take(at, Class::Word) }?);
                if value < 0 {
                    flags |= LEFT;
                }
                value.unsigned_abs() as usize
            }
        };
        if width > INT_MAX {
            return Err(errno::EOVERFLOW);
        }
        let precision = match directive.precision {
            Count::Absent => None,
            Count::Fixed(precision) => Some(precision),
            Count::Next | Count::Position(_) => {
                let at = match directive.precision {
                    Count::Position(n) => Some(n),
                    _ => None,
                };
                // SAFETY: the format says an `int` is there.
                let value = star(unsafe { source.take(at, Class::Word) }?);
                usize::try_from(value).ok()
            }
        };
        if flags & LEFT != 0 {
            flags &= !ZERO;
        }
        let class = class_of(&directive);
        let arg = if class == Class::Unused {
            Arg::Word(0)
        } else {
            // SAFETY: the format says an argument of this class is there.
            unsafe { source.take(directive.position, class) }?
        };
        let spec = Spec {
            flags,
            width,
            precision,
        };
        count += if sink.wide() && matches!(directive.conversion, b'c' | b's') {
            // SAFETY: the argument is what the directive names.
            unsafe { wide_text(sink, &directive, &spec, arg, count) }?
        } else {
            // SAFETY: the argument is what the directive names.
            unsafe { convert(sink, &directive, &spec, arg, count, saved_errno) }?
        };
    }
}

/// Writes `wcs` to a wide sink in chunks as a field of `len` characters.
fn wide_field(
    sink: &mut dyn Sink,
    spec: &Spec,
    len: usize,
    count: usize,
    mut next: impl FnMut() -> i32,
) -> Result<usize, c_int> {
    let total = begin(sink, spec, len, count)?;
    let mut chunk = [0_i32; 64];
    let mut done = 0;
    while done < len {
        let take = (len - done).min(chunk.len());
        for slot in chunk.iter_mut().take(take) {
            *slot = next();
        }
        sink.write_wide(chunk.get(..take).unwrap_or_default());
        done += take;
    }
    end(sink, spec, len, total);
    Ok(total)
}

/// `%c`, `%lc`, `%s` and `%ls` for the `wprintf` family, as musl 1.2.5's
/// `stdio/vfwprintf.c` (MIT) writes them: widths and precisions count wide
/// characters; `%c` is a byte made wide with `btowc`; `%s` is a multibyte
/// string decoded with `mbrtowc`, one that is no string failing with
/// `EILSEQ`; `%lc` and `%ls` are written as they are.
///
/// # Safety
///
/// The argument must be what the directive names; a pointer must be valid for
/// its conversion.
unsafe fn wide_text(
    sink: &mut dyn Sink,
    directive: &Directive,
    spec: &Spec,
    arg: Arg,
    count: usize,
) -> Result<usize, c_int> {
    let word = arg.word();
    let long = directive.length == Length::Long;
    if directive.conversion == b'c' {
        let wc = if long {
            word as i32
        } else {
            let wc = crate::multibyte::btowc(word as u8 as c_int);
            if wc == crate::multibyte::WEOF {
                return Err(errno::EILSEQ);
            }
            wc as i32
        };
        let mut once = Some(wc);
        return wide_field(sink, spec, 1, count, || once.take().unwrap_or(0));
    }
    if word == 0 {
        return text(sink, spec, null_text(spec), count);
    }
    let limit = spec.precision.unwrap_or(usize::MAX);
    if long {
        let s = with_exposed_provenance_mut::<i32>(word as usize);
        let mut len = 0;
        // SAFETY: the caller vouches for the wide string; no NUL came before.
        while len < limit && unsafe { s.wrapping_add(len).read_unaligned() } != 0 {
            len += 1;
        }
        let mut at = 0;
        return wide_field(sink, spec, len, count, || {
            // SAFETY: these characters were read above.
            let wc = unsafe { s.wrapping_add(at).read_unaligned() };
            at += 1;
            wc
        });
    }
    // A multibyte string: count its characters up to the precision, then
    // decode them again as they are written.
    let s = with_exposed_provenance_mut::<u8>(word as usize).cast_const();
    let decode = |at: usize| -> Result<(i32, usize), c_int> {
        let mut wc = 0_i32;
        let mut state = crate::multibyte::MbState::new();
        // SAFETY: the caller vouches for the string, and `at` is at or before
        // its NUL; at most four bytes are read, and never past the NUL.
        let used = unsafe {
            crate::multibyte::mbrtowc(&raw mut wc, s.wrapping_add(at).cast(), 4, &raw mut state)
        };
        if used >= usize::MAX - 1 {
            return Err(errno::EILSEQ);
        }
        Ok((wc, used))
    };
    let mut len = 0;
    let mut at = 0;
    while len < limit {
        let (wc, used) = decode(at)?;
        if wc == 0 {
            break;
        }
        at += used;
        len += 1;
    }
    let mut at = 0;
    wide_field(sink, spec, len, count, || {
        let (wc, used) = decode(at).unwrap_or((0, 1));
        at += used;
        wc
    })
}

/// The value of a signed integer argument of `length`.
fn signed(word: u64, length: Length) -> i64 {
    match length {
        Length::Char => i64::from(word as i8),
        Length::Short => i64::from(word as i16),
        Length::Int => i64::from(word as i32),
        // Sign-extended from a `long`'s width, 32 bits on ARMv7-A.
        Length::Long => word as c_long as i64,
        Length::Wide | Length::LongDouble => word as i64,
    }
}

/// The value of an unsigned integer argument of `length`.
fn unsigned(word: u64, length: Length) -> u64 {
    match length {
        Length::Char => u64::from(word as u8),
        Length::Short => u64::from(word as u16),
        Length::Int => u64::from(word as u32),
        Length::Long => word as c_ulong as u64,
        Length::Wide | Length::LongDouble => word,
    }
}

/// The sign a number takes.
fn sign(negative: bool, flags: u8) -> &'static [u8] {
    if negative {
        b"-"
    } else if flags & PLUS != 0 {
        b"+"
    } else if flags & SPACE != 0 {
        b" "
    } else {
        b""
    }
}

/// Formats one integer.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is a separate part of the conversion"
)]
fn integer(
    sink: &mut dyn Sink,
    spec: &Spec,
    value: u64,
    base: u64,
    upper: bool,
    prefix: &[u8],
    octal: bool,
    count: usize,
) -> Result<usize, c_int> {
    let mut buf = [0_u8; 24];
    let mut start = buf.len();
    if !(value == 0 && spec.precision == Some(0)) {
        let mut v = value;
        loop {
            start -= 1;
            let d = (v % base) as u8;
            let c = match d {
                0..=9 => b'0' + d,
                _ if upper => b'A' + d - 10,
                _ => b'a' + d - 10,
            };
            if let Some(slot) = buf.get_mut(start) {
                *slot = c;
            }
            v /= base;
            if v == 0 {
                break;
            }
        }
    }
    let digits = buf.get(start..).unwrap_or_default();
    let mut zeros = spec.precision.map_or(0, |p| p.saturating_sub(digits.len()));
    if octal && zeros == 0 && digits.first() != Some(&b'0') {
        zeros = 1;
    }
    let mut body = prefix.len() + digits.len() + zeros;
    if spec.flags & ZERO != 0 && spec.precision.is_none() && spec.width > body {
        zeros += spec.width - body;
        body = spec.width;
    }
    let total = begin(sink, spec, body, count)?;
    sink.write(prefix);
    sink.pad(b'0', zeros);
    sink.write(digits);
    end(sink, spec, body, total);
    Ok(total)
}

/// Formats `bytes` as a field.
fn text(sink: &mut dyn Sink, spec: &Spec, bytes: &[u8], count: usize) -> Result<usize, c_int> {
    let total = begin(sink, spec, bytes.len(), count)?;
    sink.write(bytes);
    end(sink, spec, bytes.len(), total);
    Ok(total)
}

/// What `%s` prints for a null pointer: glibc prints nothing when the
/// precision cannot hold all of `(null)`.
fn null_text(spec: &Spec) -> &'static [u8] {
    if spec.precision.is_none_or(|p| p >= 6) {
        b"(null)"
    } else {
        b""
    }
}

/// Formats the string at `s`, up to the precision.
///
/// # Safety
///
/// `s` must be null, or a string that is NUL-terminated or at least as long
/// as the precision.
unsafe fn c_string(
    sink: &mut dyn Sink,
    spec: &Spec,
    s: *const u8,
    count: usize,
) -> Result<usize, c_int> {
    if s.is_null() {
        return text(sink, spec, null_text(spec), count);
    }
    let len = match spec.precision {
        // SAFETY: the caller vouches for the string.
        Some(limit) => unsafe { string::strnlen(s.cast(), limit) },
        // SAFETY: as above.
        None => unsafe { string::strlen(s.cast()) },
    };
    // SAFETY: `len` bytes at `s` are readable.
    text(sink, spec, unsafe { slice::from_raw_parts(s, len) }, count)
}

/// Formats the wide string at `s` in the C locale, where each character must
/// be below 0x80.
///
/// # Safety
///
/// `s` must be null, or a wide string that is NUL-terminated or at least as
/// long as the precision.
unsafe fn wide_string(
    sink: &mut dyn Sink,
    spec: &Spec,
    s: *const i32,
    count: usize,
) -> Result<usize, c_int> {
    if s.is_null() {
        return text(sink, spec, null_text(spec), count);
    }
    let limit = spec.precision.unwrap_or(usize::MAX);
    let mut len = 0;
    while len < limit {
        // SAFETY: no NUL came before, and the caller vouches for the string.
        let wc = unsafe { s.wrapping_add(len).read_unaligned() };
        if wc == 0 {
            break;
        }
        if !(0..0x80).contains(&wc) {
            return Err(errno::EILSEQ);
        }
        len += 1;
    }
    let total = begin(sink, spec, len, count)?;
    let mut chunk = [0_u8; 64];
    let mut done = 0;
    while done < len {
        let take = (len - done).min(chunk.len());
        for (i, slot) in chunk.iter_mut().take(take).enumerate() {
            // SAFETY: these characters were read above.
            *slot = unsafe { s.wrapping_add(done + i).read_unaligned() } as u8;
        }
        sink.write(chunk.get(..take).unwrap_or_default());
        done += take;
    }
    end(sink, spec, len, total);
    Ok(total)
}

/// Formats one directive's argument.
///
/// # Safety
///
/// The argument must be what the directive names; a pointer must be valid for
/// its conversion.
unsafe fn convert(
    sink: &mut dyn Sink,
    directive: &Directive,
    spec: &Spec,
    arg: Arg,
    count: usize,
    saved_errno: c_int,
) -> Result<usize, c_int> {
    let word = arg.word();
    let length = directive.length;
    match directive.conversion {
        b'%' => {
            reserve(count, 1)?;
            sink.write(b"%");
            Ok(1)
        }
        b'd' | b'i' => {
            let value = signed(word, length);
            let prefix = sign(value < 0, spec.flags);
            integer(
                sink,
                spec,
                value.unsigned_abs(),
                10,
                false,
                prefix,
                false,
                count,
            )
        }
        b'u' => integer(
            sink,
            spec,
            unsigned(word, length),
            10,
            false,
            b"",
            false,
            count,
        ),
        b'o' => {
            let alt = spec.flags & ALT != 0;
            integer(
                sink,
                spec,
                unsigned(word, length),
                8,
                false,
                b"",
                alt,
                count,
            )
        }
        conversion @ (b'x' | b'X') => {
            let value = unsigned(word, length);
            let upper = conversion == b'X';
            let prefix: &[u8] = match (spec.flags & ALT != 0 && value != 0, upper) {
                (false, _) => b"",
                (true, false) => b"0x",
                (true, true) => b"0X",
            };
            integer(sink, spec, value, 16, upper, prefix, false, count)
        }
        b'p' => {
            if word == 0 {
                return text(sink, spec, b"(nil)", count);
            }
            let prefix: &[u8] = if spec.flags & PLUS != 0 {
                b"+0x"
            } else if spec.flags & SPACE != 0 {
                b" 0x"
            } else {
                b"0x"
            };
            let digits = Spec {
                precision: None,
                ..*spec
            };
            integer(sink, &digits, word, 16, false, prefix, false, count)
        }
        b'c' => {
            let byte = if length == Length::Long {
                let wc = word as u32;
                if wc >= 0x80 {
                    return Err(errno::EILSEQ);
                }
                wc as u8
            } else {
                word as u8
            };
            text(sink, spec, &[byte], count)
        }
        b's' => {
            if length == Length::Long {
                let s = with_exposed_provenance_mut::<i32>(word as usize);
                // SAFETY: the caller vouches for the wide string.
                unsafe { wide_string(sink, spec, s, count) }
            } else {
                let s = with_exposed_provenance_mut::<u8>(word as usize);
                // SAFETY: the caller vouches for the string.
                unsafe { c_string(sink, spec, s, count) }
            }
        }
        b'm' => {
            let s = strerror::strerror(saved_errno).cast::<u8>();
            // SAFETY: `strerror` returns a NUL-terminated string.
            unsafe { c_string(sink, spec, s, count) }
        }
        b'n' => {
            let address = word as usize;
            // In each arm, the caller vouches that the pointer is valid for
            // the type the length names, and the count is truncated to it as
            // C does.
            match (address, length) {
                (0, _) => {}
                (_, Length::Char) => {
                    // SAFETY: as above.
                    unsafe {
                        with_exposed_provenance_mut::<i8>(address).write_unaligned(count as i8)
                    };
                }
                (_, Length::Short) => {
                    // SAFETY: as above.
                    unsafe {
                        with_exposed_provenance_mut::<i16>(address).write_unaligned(count as i16)
                    };
                }
                (_, Length::Int) => {
                    // SAFETY: as above.
                    unsafe {
                        with_exposed_provenance_mut::<i32>(address).write_unaligned(count as i32)
                    };
                }
                (_, Length::Long) => {
                    // SAFETY: as above.
                    unsafe {
                        with_exposed_provenance_mut::<c_long>(address)
                            .write_unaligned(count as c_long)
                    };
                }
                (_, Length::Wide | Length::LongDouble) => {
                    // SAFETY: as above.
                    unsafe {
                        with_exposed_provenance_mut::<i64>(address).write_unaligned(count as i64)
                    };
                }
            }
            Ok(0)
        }
        conversion => {
            let value = match arg {
                Arg::Double(value) => Float::Double(value),
                Arg::Long(value) => Float::Long(value),
                Arg::Word(_) => Float::Double(0.0),
            };
            float::format(sink, spec, conversion, value, count)
        }
    }
}

/// Formats with every argument read first, for a positional format.
///
/// # Safety
///
/// As [`format`].
#[inline(never)]
unsafe fn format_positional(
    sink: &mut dyn Sink,
    fmt: *const u8,
    list: &mut VaList<'_>,
    saved_errno: c_int,
) -> Result<usize, c_int> {
    let mut classes = [Class::Unused; SLOTS];
    // SAFETY: the caller passes a NUL-terminated format.
    let highest = unsafe { collect(fmt, &mut classes) }?;
    let mut args = [Arg::Word(0); SLOTS];
    for (slot, class) in args.iter_mut().zip(classes).take(highest + 1).skip(1) {
        // SAFETY: the caller vouches that the arguments match the format,
        // which gives each position's class; a position no directive names
        // is read as an `int`, as glibc does.
        *slot = unsafe { read_arg(list, class) };
    }
    // SAFETY: as above.
    unsafe { run(sink, fmt, &mut Source::Positional(&args), saved_errno) }
}

/// Formats `fmt` into `sink` with the arguments in `ap`. Returns the count,
/// or the error number to set, zero if it is already set.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated string, and `ap` a `va_list` whose
/// arguments match it.
pub unsafe fn format(
    sink: &mut dyn Sink,
    fmt: *const c_char,
    ap: VaListArg,
) -> Result<usize, c_int> {
    // SAFETY: the pointer is this thread's `errno`.
    let saved_errno = unsafe { errno::__errno_location().read() };
    let fmt = fmt.cast::<u8>();
    // SAFETY: the caller passes a live list.
    let mut list = unsafe { VaList::from_raw(ap) };
    // SAFETY: the caller passes a NUL-terminated format.
    if unsafe { starts_positional(fmt) } {
        // SAFETY: as above.
        unsafe { format_positional(sink, fmt, &mut list, saved_errno) }
    } else {
        // SAFETY: as above.
        unsafe { run(sink, fmt, &mut Source::Sequential(&mut list), saved_errno) }
    }
}

/// C's return for a formatting result: the count, or -1 with `errno` set.
fn finish(result: Result<usize, c_int>) -> c_int {
    match result {
        Ok(count) => count as c_int,
        Err(error) => {
            if error != 0 {
                errno::set(error);
            }
            -1
        }
    }
}

/// `vfprintf` on a stream whose lock is held. A stream already in error is
/// written anyway, and stays in error.
///
/// # Safety
///
/// As `vfprintf`.
pub unsafe fn vfprintf_locked(inner: &mut Inner, fmt: *const c_char, ap: VaListArg) -> c_int {
    let old_error = inner.error;
    inner.error = false;
    let result = {
        let mut sink = FileSink::new(inner);
        // SAFETY: the caller vouches for the format and arguments.
        let result = unsafe { format(&mut sink, fmt, ap) };
        sink.flush();
        result
    };
    let failed = inner.error;
    inner.error |= old_error;
    match result {
        Ok(_) if failed => -1,
        result => finish(result),
    }
}

/// Formats to `stream`.
///
/// # Safety
///
/// `stream` must be a live stream, `fmt` a NUL-terminated string, and `ap` a
/// `va_list` whose arguments match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vfprintf(stream: *mut File, fmt: *const c_char, ap: VaListArg) -> c_int {
    let op = |inner: &mut Inner| {
        // SAFETY: the caller passes a live stream, a format and its arguments.
        unsafe { vfprintf_locked(inner, fmt, ap) }
    };
    // SAFETY: the caller passes a live stream.
    unsafe { file::locked(stream, op) }
}

/// Formats to standard output.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated string, and `ap` a `va_list` whose arguments
/// match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vprintf(fmt: *const c_char, ap: VaListArg) -> c_int {
    // SAFETY: standard output is a static stream, and the caller vouches for
    // the rest.
    unsafe { vfprintf(file::stdout.load(Ordering::Relaxed), fmt, ap) }
}

/// Formats into the `n` bytes at `s`, always NUL-terminated if `n` is not
/// zero, and returns the length the whole output would have had.
///
/// # Safety
///
/// `s` must be valid for writes of `n` bytes, `fmt` a NUL-terminated string,
/// and `ap` a `va_list` whose arguments match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vsnprintf(
    s: *mut c_char,
    n: usize,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller vouches for `n` bytes, of which `n - 1` are for text.
    let mut sink = unsafe { StringSink::new(s.cast(), n.saturating_sub(1)) };
    // SAFETY: the caller vouches for the format and arguments.
    let result = unsafe { format(&mut sink, fmt, ap) };
    if n != 0 {
        // SAFETY: `used` is at most `n - 1`.
        unsafe { s.wrapping_add(sink.used()).write(0) };
    }
    finish(result)
}

/// Formats into `s`, which must be large enough.
///
/// # Safety
///
/// `s` must have room for the output and its NUL, `fmt` must be a
/// NUL-terminated string, and `ap` a `va_list` whose arguments match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vsprintf(s: *mut c_char, fmt: *const c_char, ap: VaListArg) -> c_int {
    // SAFETY: the caller vouches for room.
    unsafe { vsnprintf(s, usize::MAX, fmt, ap) }
}

/// Formats to the descriptor `fd`.
///
/// # Safety
///
/// `fmt` must be a NUL-terminated string, and `ap` a `va_list` whose arguments
/// match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vdprintf(fd: c_int, fmt: *const c_char, ap: VaListArg) -> c_int {
    let mut sink = FdSink {
        fd,
        chunk: [0; CHUNK],
        len: 0,
        failed: false,
    };
    // SAFETY: the caller vouches for the format and arguments.
    let result = unsafe { format(&mut sink, fmt, ap) };
    sink.flush();
    match result {
        Ok(_) if sink.failed => -1,
        result => finish(result),
    }
}

/// Formats into a string from `malloc`, stored in `*strp`, which the caller
/// frees. On failure returns -1 and leaves `*strp` alone.
///
/// # Safety
///
/// `strp` must be valid for writes, `fmt` a NUL-terminated string, and `ap` a
/// `va_list` whose arguments match it.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn vasprintf(
    strp: *mut *mut c_char,
    fmt: *const c_char,
    ap: VaListArg,
) -> c_int {
    // SAFETY: the caller passes a live list; this is `va_copy`.
    let mut copy = unsafe { va::copy(ap) };
    // SAFETY: a sink with no room writes nothing.
    let mut counter = unsafe { StringSink::new(null_mut(), 0) };
    // SAFETY: the caller vouches for the format, and the copy holds the
    // arguments.
    let len = match unsafe { format(&mut counter, fmt, va::as_arg(&mut copy)) } {
        Ok(len) => len,
        Err(error) => return finish(Err(error)),
    };
    let buf = malloc::malloc(len + 1).cast::<u8>();
    if buf.is_null() {
        return -1;
    }
    // SAFETY: `buf` has room for `len` bytes and a NUL.
    let mut sink = unsafe { StringSink::new(buf, len) };
    // SAFETY: the caller vouches for the format and arguments.
    let result = unsafe { format(&mut sink, fmt, ap) };
    // SAFETY: `used` is at most `len`.
    unsafe { buf.wrapping_add(sink.used()).write(0) };
    if result.is_err() {
        // SAFETY: `buf` came from `malloc` and is not handed out.
        unsafe { malloc::free(buf.cast()) };
        return finish(result);
    }
    // SAFETY: the caller passes a valid pointer.
    unsafe { strp.write(buf.cast()) };
    finish(result)
}

va::variadic!(printf, 1, vprintf);
va::variadic!(fprintf, 2, vfprintf);
va::variadic!(dprintf, 2, vdprintf);
va::variadic!(sprintf, 2, vsprintf);
va::variadic!(snprintf, 3, vsnprintf);
va::variadic!(asprintf, 2, vasprintf);
