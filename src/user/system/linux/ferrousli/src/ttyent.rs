//! `ttyent.h`: the BSD terminal table, `/etc/ttys`, read a line at a time.
//!
//! Each line that is not blank or a `#` comment names a terminal, the
//! program to start on it, its terminal type, and then words: `on` and
//! `off`, `secure`, and `window=command`; a `#` after them starts the line's
//! comment. The getty and window fields may be in double quotes, to hold
//! spaces. Linux distributions ship no such file, so on them `setttyent`
//! fails and `getttynam` answers null, which is what util-linux's libraries
//! -- the callers here, through GLib -- expect.
//!
//! As in glibc, the entry answered lives in static storage that the next
//! call overwrites; a lock only keeps two threads from reading the file at
//! once.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_int};
use core::mem::size_of;
use core::ptr::null_mut;

use crate::lock::SpinLock;
use crate::stdio::File;
use crate::stdio::io::{fgets, rewind};
use crate::stdio::open::{fclose, fopen};
use crate::string::strcmp;

/// `_PATH_TTYS`.
const PATH_TTYS: &core::ffi::CStr = c"/etc/ttys";

/// `TTY_ON`: a getty is to be started on the terminal.
const TTY_ON: c_int = 0x01;
/// `TTY_SECURE`: root may log in on it.
const TTY_SECURE: c_int = 0x02;

/// The longest line read; a longer one is skipped whole.
const LINE: usize = 1024;

/// `struct ttyent`, as glibc's `ttyent.h` lays it out.
#[repr(C)]
#[derive(Debug)]
pub struct Ttyent {
    /// The terminal's name, such as `tty1`.
    pub ty_name: *mut c_char,
    /// The command that starts a getty on it, or null.
    pub ty_getty: *mut c_char,
    /// Its terminal type, or null.
    pub ty_type: *mut c_char,
    /// `TTY_ON` and `TTY_SECURE`.
    pub ty_status: c_int,
    /// The `window=` command, or null.
    pub ty_window: *mut c_char,
    /// The line's comment, or null.
    pub ty_comment: *mut c_char,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Ttyent>() == 48);
#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<Ttyent>() == 24);

/// Where each of an entry's strings starts in the line, the line having had
/// NULs written after each.
#[derive(Debug, Default, PartialEq, Eq)]
struct Fields {
    name: usize,
    getty: Option<usize>,
    kind: Option<usize>,
    status: c_int,
    window: Option<usize>,
    comment: Option<usize>,
}

/// Whether `byte` separates fields.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n')
}

/// The byte at `at`, or NUL past the end.
fn at(line: &[u8], at: usize) -> u8 {
    line.get(at).copied().unwrap_or(0)
}

/// The first byte from `from` that is not a separator.
fn skip_space(line: &[u8], mut from: usize) -> usize {
    while is_space(at(line, from)) {
        from += 1;
    }
    from
}

/// The field at `from`, which is not a separator: where its text starts,
/// and where the next field's search starts. A field in double quotes runs
/// to the closing quote; any other to the next separator. A NUL is written
/// after the text.
fn field(line: &mut [u8], from: usize) -> (usize, usize) {
    let quoted = at(line, from) == b'"';
    let start = if quoted { from + 1 } else { from };
    let mut end = start;
    loop {
        let byte = at(line, end);
        if byte == 0 || (quoted && byte == b'"') || (!quoted && is_space(byte)) {
            break;
        }
        end += 1;
    }
    let next = if at(line, end) == 0 { end } else { end + 1 };
    if let Some(slot) = line.get_mut(end) {
        *slot = 0;
    }
    (start, next)
}

/// The entry on `line`, a NUL-terminated line, or `None` for a blank line or
/// a comment.
fn parse(line: &mut [u8]) -> Option<Fields> {
    let start = skip_space(line, 0);
    if matches!(at(line, start), 0 | b'#') {
        return None;
    }
    let (name, mut next) = field(line, start);
    let mut fields = Fields {
        name,
        ..Fields::default()
    };
    // The getty and the type: each absent at the end of the line or where
    // the comment begins.
    let optional = |line: &mut [u8], next: &mut usize| {
        let from = skip_space(line, *next);
        if matches!(at(line, from), 0 | b'#') {
            *next = from;
            return None;
        }
        let (text, after) = field(line, from);
        *next = after;
        Some(text)
    };
    fields.getty = optional(line, &mut next);
    fields.kind = optional(line, &mut next);
    loop {
        let from = skip_space(line, next);
        match at(line, from) {
            0 => break,
            b'#' => {
                let text = skip_space(line, from + 1);
                if at(line, text) != 0 {
                    fields.comment = Some(text);
                    // The comment runs to the end; only its newline goes.
                    let mut end = text;
                    while !matches!(at(line, end), 0 | b'\n') {
                        end += 1;
                    }
                    if let Some(slot) = line.get_mut(end) {
                        *slot = 0;
                    }
                }
                break;
            }
            _ => {}
        }
        let (word, after) = field(line, from);
        next = after;
        let text = line.get(word..).unwrap_or_default();
        let text = text.get(..text.iter().position(|&b| b == 0).unwrap_or(text.len()));
        match text.unwrap_or_default() {
            b"on" => fields.status |= TTY_ON,
            b"off" => fields.status &= !TTY_ON,
            b"secure" => fields.status |= TTY_SECURE,
            word_text if word_text.starts_with(b"window=") => {
                let value = word + b"window=".len();
                // `window="a b"`: the quoted value was split at its space by
                // the unquoted word; take it again as a quoted field.
                if at(line, value) == b'"' {
                    if let Some(slot) = line.get_mut(word + word_text.len()) {
                        *slot = b' ';
                    }
                    let (text, after) = field(line, value);
                    fields.window = Some(text);
                    next = after;
                } else {
                    fields.window = Some(value);
                }
            }
            _ => {}
        }
    }
    Some(fields)
}

/// The file, the line the entry points into, and the entry.
#[derive(Debug)]
struct State {
    file: *mut File,
    line: [u8; LINE],
    entry: Ttyent,
}

/// The state and the lock that guards it.
#[derive(Debug)]
struct Table {
    lock: SpinLock,
    state: UnsafeCell<State>,
}

// SAFETY: the state is only reached through `with_state`, with the lock held.
unsafe impl Sync for Table {}

/// The one table.
static TABLE: Table = Table {
    lock: SpinLock::new(),
    state: UnsafeCell::new(State {
        file: null_mut(),
        line: [0; LINE],
        entry: Ttyent {
            ty_name: null_mut(),
            ty_getty: null_mut(),
            ty_type: null_mut(),
            ty_status: 0,
            ty_window: null_mut(),
            ty_comment: null_mut(),
        },
    }),
};

/// Runs `op` on the state with the lock held.
fn with_state<R>(op: impl FnOnce(&mut State) -> R) -> R {
    let _guard = TABLE.lock.lock();
    // SAFETY: the lock is held, so nothing else reaches the state.
    let state = unsafe { &mut *TABLE.state.get() };
    op(state)
}

/// Opens the table, or goes back to its start: 1, or 0 when it cannot be
/// opened.
fn open(state: &mut State) -> c_int {
    if state.file.is_null() {
        // SAFETY: both are NUL-terminated strings.
        state.file = unsafe { fopen(PATH_TTYS.as_ptr(), c"re".as_ptr()) };
    } else {
        // SAFETY: the stream is open.
        unsafe { rewind(state.file) };
    }
    c_int::from(!state.file.is_null())
}

/// Closes the table if it is open.
fn close(state: &mut State) {
    if !state.file.is_null() {
        // SAFETY: the stream is open, and is closed once.
        let _ = unsafe { fclose(state.file) };
        state.file = null_mut();
    }
}

/// The next entry, or null at the end of the table or when it cannot be
/// opened.
fn next(state: &mut State) -> *mut Ttyent {
    if state.file.is_null() && open(state) == 0 {
        return null_mut();
    }
    let fields = loop {
        let buffer = state.line.as_mut_ptr().cast::<c_char>();
        // SAFETY: the line holds `LINE` bytes, and the stream is open.
        if unsafe { fgets(buffer, LINE as c_int, state.file) }.is_null() {
            return null_mut();
        }
        let length = state.line.iter().position(|&b| b == 0).unwrap_or(LINE);
        let whole = state.line.get(..length).is_some_and(|l| l.ends_with(b"\n"));
        if !whole && length == LINE - 1 {
            // Too long: skip the rest of it, and the line.
            loop {
                // SAFETY: as above.
                if unsafe { fgets(buffer, LINE as c_int, state.file) }.is_null() {
                    return null_mut();
                }
                if state.line.contains(&b'\n') {
                    break;
                }
            }
            continue;
        }
        if let Some(fields) = parse(&mut state.line) {
            break fields;
        }
    };
    let base = state.line.as_mut_ptr().cast::<c_char>();
    let pointer = |offset: Option<usize>| offset.map_or(null_mut(), |at| base.wrapping_add(at));
    state.entry = Ttyent {
        ty_name: base.wrapping_add(fields.name),
        ty_getty: pointer(fields.getty),
        ty_type: pointer(fields.kind),
        ty_status: fields.status,
        ty_window: pointer(fields.window),
        ty_comment: pointer(fields.comment),
    };
    &raw mut state.entry
}

/// Opens `/etc/ttys`, or rewinds it: 1, or 0 when it cannot be opened.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn setttyent() -> c_int {
    with_state(open)
}

/// Closes `/etc/ttys`. Always 1.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn endttyent() -> c_int {
    with_state(close);
    1
}

/// The next entry of `/etc/ttys`, opening it first if need be, or null at
/// its end or when there is none.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub extern "C" fn getttyent() -> *mut Ttyent {
    with_state(next)
}

/// The entry of `/etc/ttys` for the terminal `name`, or null.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn getttynam(name: *const c_char) -> *mut Ttyent {
    with_state(|state| {
        if open(state) == 0 {
            return null_mut();
        }
        let found = loop {
            let entry = next(state);
            if entry.is_null() {
                break entry;
            }
            // SAFETY: a non-null entry is the state's own.
            let entry_name = unsafe { (*entry).ty_name };
            // SAFETY: an entry's name is a NUL-terminated string in the
            // line, and the caller passes one too.
            if unsafe { strcmp(entry_name, name) } == 0 {
                break entry;
            }
        };
        close(state);
        found
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> (Vec<u8>, Option<Fields>) {
        let mut line = text.as_bytes().to_vec();
        line.push(0);
        let fields = parse(&mut line);
        (line, fields)
    }

    fn text(line: &[u8], at: Option<usize>) -> Option<&str> {
        let rest = line.get(at?..)?;
        let end = rest.iter().position(|&b| b == 0)?;
        core::str::from_utf8(rest.get(..end)?).ok()
    }

    #[test]
    fn blank_lines_and_comments_are_no_entry() {
        assert_eq!(parsed("\n").1, None);
        assert_eq!(parsed("   \t\n").1, None);
        assert_eq!(parsed("  # console\n").1, None);
    }

    #[test]
    fn a_full_line_gives_every_field() {
        let (line, fields) = parsed(
            "ttyv0\t\"/usr/libexec/getty Pc\"\tcons25\ton  secure window=/bin/xterm # the first\n",
        );
        let fields = fields.unwrap();
        assert_eq!(text(&line, Some(fields.name)), Some("ttyv0"));
        assert_eq!(text(&line, fields.getty), Some("/usr/libexec/getty Pc"));
        assert_eq!(text(&line, fields.kind), Some("cons25"));
        assert_eq!(fields.status, TTY_ON | TTY_SECURE);
        assert_eq!(text(&line, fields.window), Some("/bin/xterm"));
        assert_eq!(text(&line, fields.comment), Some("the first"));
    }

    #[test]
    fn a_name_alone_has_no_getty_type_or_comment() {
        let (line, fields) = parsed("console\n");
        let fields = fields.unwrap();
        assert_eq!(text(&line, Some(fields.name)), Some("console"));
        assert_eq!(
            (fields.getty, fields.kind, fields.window, fields.comment),
            (None, None, None, None)
        );
        assert_eq!(fields.status, 0);
    }

    #[test]
    fn off_takes_back_on_and_a_quoted_window_keeps_its_space() {
        let (line, fields) = parsed("tty1 none dumb on off window=\"xterm -e sh\"\n");
        let fields = fields.unwrap();
        assert_eq!(fields.status, 0);
        assert_eq!(text(&line, fields.window), Some("xterm -e sh"));
    }

    #[test]
    fn a_comment_right_after_the_name_leaves_the_rest_absent() {
        let (line, fields) = parsed("tty2 # spare\n");
        let fields = fields.unwrap();
        assert_eq!(fields.getty, None);
        assert_eq!(text(&line, fields.comment), Some("spare"));
    }
}
