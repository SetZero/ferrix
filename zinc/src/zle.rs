//! The line editor: reading a command line from the terminal with editing,
//! history and completion, as zsh's ZLE does without `compinit`.
//!
//! Tab follows zsh's defaults (`AUTO_LIST`, `LIST_AMBIGUOUS`, `AUTO_MENU`,
//! `LIST_TYPES`): a unique match is inserted with a space, or a slash for a
//! directory; an ambiguous one inserts the longest common prefix; a Tab that
//! can insert nothing lists the matches; and the Tab after a listing starts
//! cycling through them. The first word of a command completes command
//! names, `$` completes parameters, `~` user names, and everything else file
//! names.

use crate::exec::write_fd;
use crate::shell::Shell;
use crate::tok;
use crate::wcwidth::wcwidth;

/// The terminal settings the editor changes, restored when it returns.
struct RawMode {
    saved: libc::termios,
}

impl std::fmt::Debug for RawMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RawMode")
    }
}

impl RawMode {
    /// Put fd 0 into character-at-a-time mode without echo, or `None` if it
    /// is not a terminal that allows it.
    fn enter() -> Option<RawMode> {
        // SAFETY: an all-zero termios is a valid value for tcgetattr to fill.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: saved is a valid out-pointer.
        if unsafe { libc::tcgetattr(0, &raw mut saved) } != 0 {
            return None;
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::INLCR | libc::IGNCR);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: raw is a valid termios.
        if unsafe { libc::tcsetattr(0, libc::TCSADRAIN, &raw const raw) } != 0 {
            return None;
        }
        Some(RawMode { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: saved came from tcgetattr.
        let _ok = unsafe { libc::tcsetattr(0, libc::TCSADRAIN, &raw const self.saved) };
    }
}

/// What the previous Tab did, for the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CompState {
    None,
    /// The matches were listed for the word starting at this offset.
    Listed(usize),
    /// Cycling: the word starts at `start`, `index` is shown, and `end` is
    /// where the inserted text ends.
    Menu {
        start: usize,
        end: usize,
        index: usize,
        matches: Vec<Match>,
    },
}

/// One completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Match {
    /// The text as typed (unquoted).
    word: Vec<u8>,
    /// `/` for a directory: appended instead of a space.
    dir: bool,
    /// The character `LIST_TYPES` shows after it in a listing.
    kind: Option<u8>,
    /// What a listing shows, when not the word itself.
    display: Option<Vec<u8>>,
}

/// The line editor's state that outlives one line: the history.
#[derive(Debug, Default)]
pub(crate) struct Editor {
    history: Vec<Vec<u8>>,
}

impl Editor {
    /// Remember an event for Up and Down.
    pub(crate) fn add_history(&mut self, text: &[u8]) {
        let t = text.strip_suffix(b"\n").unwrap_or(text);
        if t.is_empty() || t.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        if self.history.last().map(Vec::as_slice) == Some(t) {
            return;
        }
        self.history.push(t.to_vec());
    }

    /// Read a line with editing after printing `prompt`; the line ends with
    /// a newline. `None` at end of input (Ctrl-D on an empty line). When fd 0
    /// is not a terminal the line is read as is.
    pub(crate) fn read_line(&mut self, sh: &Shell, prompt: &[u8]) -> Option<Vec<u8>> {
        let Some(raw) = RawMode::enter() else {
            let _ok = write_fd(2, prompt);
            return plain_read_line();
        };
        let mut line = Line::new(prompt);
        line.print_prompt_lines();
        line.refresh();
        let mut hist_pos = self.history.len();
        let mut saved_edit: Vec<u8> = Vec::new();
        let mut comp = CompState::None;
        let result = loop {
            let Some(key) = read_key() else {
                break None;
            };
            let was_tab = key == Key::Tab;
            match key {
                Key::Tab => {
                    comp = complete(sh, &mut line, std::mem::replace(&mut comp, CompState::None))
                }
                Key::Enter => {
                    line.cursor = line.buf.len();
                    line.refresh();
                    let _ok = write_fd(2, b"\r\n");
                    let mut out = line.buf.clone();
                    out.push(b'\n');
                    break Some(out);
                }
                Key::Char(c) => line.insert(&[c]),
                Key::Backspace => line.backspace(),
                Key::Delete => line.delete(),
                Key::CtrlD => {
                    if line.buf.is_empty() {
                        break None;
                    }
                    line.delete();
                }
                Key::Left => line.left(),
                Key::Right => line.right(),
                Key::Home => {
                    line.cursor = 0;
                    line.refresh();
                }
                Key::End => {
                    line.cursor = line.buf.len();
                    line.refresh();
                }
                Key::WordLeft => line.word_left(),
                Key::WordRight => line.word_right(),
                Key::KillLine => {
                    line.buf.truncate(line.cursor);
                    line.refresh();
                }
                Key::KillWhole => {
                    line.buf.drain(..line.cursor);
                    line.cursor = 0;
                    line.refresh();
                }
                Key::KillWord => line.kill_word(),
                Key::Clear => {
                    let _ok = write_fd(2, b"\x1b[H\x1b[2J");
                    line.row = 0;
                    line.refresh();
                }
                Key::Interrupt => {
                    line.cursor = line.buf.len();
                    line.refresh();
                    let _ok = write_fd(2, b"\r\n");
                    line.buf.clear();
                    line.cursor = 0;
                    line.row = 0;
                    hist_pos = self.history.len();
                    line.refresh();
                }
                Key::Up => self.history_up(&mut line, &mut hist_pos, &mut saved_edit),
                Key::Down => self.history_down(&mut line, &mut hist_pos, &mut saved_edit),
                Key::Ignore => {}
            }
            if !was_tab {
                comp = CompState::None;
            }
        };
        drop(raw);
        result
    }
}

impl Editor {
    /// Up: the previous event, saving the line being edited first.
    fn history_up(&self, line: &mut Line, pos: &mut usize, saved: &mut Vec<u8>) {
        if *pos == 0 {
            return;
        }
        if *pos == self.history.len() {
            *saved = line.buf.clone();
        }
        *pos -= 1;
        if let Some(h) = self.history.get(*pos) {
            line.buf = h.clone();
            line.cursor = line.buf.len();
            line.refresh();
        }
    }

    /// Down: the next event, or the saved edit after the last.
    fn history_down(&self, line: &mut Line, pos: &mut usize, saved: &mut Vec<u8>) {
        if *pos >= self.history.len() {
            return;
        }
        *pos += 1;
        line.buf = match self.history.get(*pos) {
            Some(h) => h.clone(),
            None => std::mem::take(saved),
        };
        line.cursor = line.buf.len();
        line.refresh();
    }
}

/// Read a line from fd 0 without editing.
fn plain_read_line() -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        // SAFETY: b is one writable byte.
        let n = unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) };
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if n <= 0 {
            return if line.is_empty() { None } else { Some(line) };
        }
        let [c] = b;
        line.push(c);
        if c == b'\n' {
            return Some(line);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Char(u8),
    Tab,
    Enter,
    Backspace,
    Delete,
    CtrlD,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    WordLeft,
    WordRight,
    KillLine,
    KillWhole,
    KillWord,
    Clear,
    Interrupt,
    Ignore,
}

fn read_byte() -> Option<u8> {
    let mut b = [0u8; 1];
    loop {
        // SAFETY: b is one writable byte.
        let n = unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) };
        if n == 1 {
            let [c] = b;
            return Some(c);
        }
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return None;
    }
}

fn read_key() -> Option<Key> {
    let c = read_byte()?;
    Some(match c {
        b'\t' => Key::Tab,
        b'\r' | b'\n' => Key::Enter,
        0x7f | 0x08 => Key::Backspace,
        0x01 => Key::Home,
        0x02 => Key::Left,
        0x03 => Key::Interrupt,
        0x04 => Key::CtrlD,
        0x05 => Key::End,
        0x06 => Key::Right,
        0x0b => Key::KillLine,
        0x0c => Key::Clear,
        0x0e => Key::Down,
        0x10 => Key::Up,
        0x15 => Key::KillWhole,
        0x17 => Key::KillWord,
        0x1b => read_escape()?,
        c if c < 0x20 => Key::Ignore,
        c => Key::Char(c),
    })
}

/// The rest of an escape sequence.
fn read_escape() -> Option<Key> {
    let c = read_byte()?;
    Some(match c {
        b'[' | b'O' => {
            let mut params = Vec::new();
            loop {
                let d = read_byte()?;
                if d.is_ascii_digit() || d == b';' {
                    params.push(d);
                    continue;
                }
                break match (d, params.as_slice()) {
                    (b'A', _) => Key::Up,
                    (b'B', _) => Key::Down,
                    (b'C', p) if p.ends_with(b";5") => Key::WordRight,
                    (b'D', p) if p.ends_with(b";5") => Key::WordLeft,
                    (b'C', _) => Key::Right,
                    (b'D', _) => Key::Left,
                    (b'H', _) => Key::Home,
                    (b'F', _) => Key::End,
                    (b'~', b"1" | b"7") => Key::Home,
                    (b'~', b"4" | b"8") => Key::End,
                    (b'~', b"3") => Key::Delete,
                    _ => Key::Ignore,
                };
            }
        }
        b'b' | b'B' => Key::WordLeft,
        b'f' | b'F' => Key::WordRight,
        0x7f | 0x08 => Key::KillWord,
        _ => Key::Ignore,
    })
}

/// The line being edited and where it is on the screen.
#[derive(Debug)]
struct Line {
    prompt: Vec<u8>,
    prompt_width: usize,
    buf: Vec<u8>,
    cursor: usize,
    /// The screen row of the cursor, counted from the prompt's first row.
    row: usize,
}

/// The terminal's width.
fn columns() -> usize {
    // SAFETY: an all-zero winsize is valid for ioctl to fill.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: ws is a valid out-pointer for TIOCGWINSZ.
    let r = unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &raw mut ws) };
    if r == 0 && ws.ws_col > 0 {
        return usize::from(ws.ws_col);
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80)
}

/// How many columns `s` takes: escape sequences take none, and a UTF-8
/// character what `wcwidth` says, so the cursor lands right after a wide
/// one such as a prompt's `⚡`. A byte that starts no valid character takes
/// one.
pub(crate) fn display_width(s: &[u8]) -> usize {
    let mut w = 0;
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        if c == 0x1b {
            i += 1;
            if s.get(i) == Some(&b'[') {
                i += 1;
                while s.get(i).is_some_and(|&d| !(0x40..=0x7e).contains(&d)) {
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        if c >= 0x80 && (c & 0xc0) != 0x80 {
            let len = match c {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            let ch = s
                .get(i..i + len)
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(|t| t.chars().next());
            if let Some(ch) = ch {
                w += usize::try_from(wcwidth(u32::from(ch))).unwrap_or(0);
                i += len;
                continue;
            }
            w += 1;
        } else if (0x20..0x7f).contains(&c) {
            w += 1;
        }
        i += 1;
    }
    w
}

impl Line {
    fn new(prompt: &[u8]) -> Line {
        let prompt = tok::unmetafy(prompt);
        let last_line = prompt.rsplit(|&c| c == b'\n').next().unwrap_or(&[]);
        let prompt_width = display_width(last_line);
        Line {
            prompt,
            prompt_width,
            buf: Vec::new(),
            cursor: 0,
            row: 0,
        }
    }

    /// Redraw the prompt and the line and put the cursor where it belongs.
    fn refresh(&mut self) {
        let cols = columns().max(1);
        let mut out = Vec::new();
        if self.row > 0 {
            out.extend_from_slice(format!("\x1b[{}A", self.row).as_bytes());
        }
        out.push(b'\r');
        let last_line_start = self
            .prompt
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(0, |p| p + 1);
        out.extend_from_slice(self.prompt.get(last_line_start..).unwrap_or(&[]));
        out.extend_from_slice(&self.buf);
        let total = self.prompt_width + display_width(&self.buf);
        let mut end_row = total / cols;
        if total > 0 && total.is_multiple_of(cols) {
            // The terminal holds the cursor at the margin; make the wrap real.
            out.extend_from_slice(b" \r");
        } else if total.is_multiple_of(cols) {
            end_row = 0;
        }
        out.extend_from_slice(b"\x1b[J");
        let before = self.prompt_width + display_width(self.buf.get(..self.cursor).unwrap_or(&[]));
        let row = before / cols;
        let col = before % cols;
        if end_row > row {
            out.extend_from_slice(format!("\x1b[{}A", end_row - row).as_bytes());
        }
        out.push(b'\r');
        if col > 0 {
            out.extend_from_slice(format!("\x1b[{col}C").as_bytes());
        }
        self.row = row;
        let _ok = write_fd(2, &out);
    }

    /// Print the prompt in full the first time.
    fn print_prompt_lines(&self) {
        let last_line_start = self
            .prompt
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(0, |p| p + 1);
        let head = self.prompt.get(..last_line_start).unwrap_or(&[]);
        if !head.is_empty() {
            let mut h = Vec::new();
            for &c in head {
                if c == b'\n' {
                    h.extend_from_slice(b"\r\n");
                } else {
                    h.push(c);
                }
            }
            let _ok = write_fd(2, &h);
        }
    }

    fn insert(&mut self, text: &[u8]) {
        let tail = self.buf.split_off(self.cursor);
        self.buf.extend_from_slice(text);
        self.cursor = self.buf.len();
        self.buf.extend(tail);
        self.refresh();
    }

    fn char_before(&self, pos: usize) -> usize {
        let mut p = pos.saturating_sub(1);
        while p > 0 && self.buf.get(p).is_some_and(|&c| (c & 0xc0) == 0x80) {
            p -= 1;
        }
        p
    }

    fn char_after(&self, pos: usize) -> usize {
        let mut p = (pos + 1).min(self.buf.len());
        while p < self.buf.len() && self.buf.get(p).is_some_and(|&c| (c & 0xc0) == 0x80) {
            p += 1;
        }
        p
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            let p = self.char_before(self.cursor);
            self.buf.drain(p..self.cursor);
            self.cursor = p;
            self.refresh();
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.buf.len() {
            let p = self.char_after(self.cursor);
            self.buf.drain(self.cursor..p);
            self.refresh();
        }
    }

    fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.char_before(self.cursor);
            self.refresh();
        }
    }

    fn right(&mut self) {
        if self.cursor < self.buf.len() {
            self.cursor = self.char_after(self.cursor);
            self.refresh();
        }
    }

    fn word_left(&mut self) {
        let mut p = self.cursor;
        while p > 0 && !self.buf.get(p - 1).is_some_and(u8::is_ascii_alphanumeric) {
            p -= 1;
        }
        while p > 0 && self.buf.get(p - 1).is_some_and(u8::is_ascii_alphanumeric) {
            p -= 1;
        }
        self.cursor = p;
        self.refresh();
    }

    fn word_right(&mut self) {
        let mut p = self.cursor;
        let n = self.buf.len();
        while p < n && !self.buf.get(p).is_some_and(u8::is_ascii_alphanumeric) {
            p += 1;
        }
        while p < n && self.buf.get(p).is_some_and(u8::is_ascii_alphanumeric) {
            p += 1;
        }
        self.cursor = p;
        self.refresh();
    }

    fn kill_word(&mut self) {
        let mut p = self.cursor;
        while p > 0 && self.buf.get(p - 1).is_some_and(u8::is_ascii_whitespace) {
            p -= 1;
        }
        while p > 0 && !self.buf.get(p - 1).is_some_and(u8::is_ascii_whitespace) {
            p -= 1;
        }
        self.buf.drain(p..self.cursor);
        self.cursor = p;
        self.refresh();
    }

    /// Move below the line, for output that must not overwrite it.
    fn below(&mut self) {
        let cols = columns().max(1);
        let total = self.prompt_width + display_width(&self.buf);
        let end_row = total / cols;
        let mut out = Vec::new();
        if end_row > self.row {
            out.extend_from_slice(format!("\x1b[{}B", end_row - self.row).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        let _ok = write_fd(2, &out);
        self.row = 0;
    }
}

/// The word the cursor is in, as the completer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Word {
    /// Offset of the word's first byte in the line.
    start: usize,
    /// The word up to the cursor with its quoting removed.
    text: Vec<u8>,
    /// The quote the word is open in, if any.
    quote: Option<u8>,
    /// The word is in command position.
    command: bool,
}

/// Words after which the next one is a command again.
const COMMAND_PREFIXES: &[&[u8]] = &[
    b"if",
    b"then",
    b"else",
    b"elif",
    b"do",
    b"while",
    b"until",
    b"time",
    b"!",
    b"{",
    b"nocorrect",
    b"noglob",
    b"exec",
    b"command",
    b"builtin",
    b"-",
];

/// Split the line up to `cursor` into shell words enough to find the word
/// being completed.
pub(crate) fn current_word(line: &[u8], cursor: usize) -> Word {
    let text = line.get(..cursor).unwrap_or(line);
    let mut start = 0usize;
    let mut word: Vec<u8> = Vec::new();
    let mut in_word = false;
    let mut quote: Option<u8> = None;
    let mut command = true;
    let mut i = 0usize;
    while let Some(&c) = text.get(i) {
        match quote {
            Some(b'\'') => {
                if c == b'\'' {
                    quote = None;
                } else {
                    word.push(c);
                }
                i += 1;
                continue;
            }
            Some(q) => {
                if c == b'\\'
                    && text
                        .get(i + 1)
                        .is_some_and(|&n| matches!(n, b'"' | b'\\' | b'$' | b'`'))
                {
                    if let Some(&n) = text.get(i + 1) {
                        word.push(n);
                    }
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                } else {
                    word.push(c);
                }
                i += 1;
                continue;
            }
            None => {}
        }
        match c {
            b' ' | b'\t' | b'\n' => {
                if in_word {
                    command = COMMAND_PREFIXES.contains(&word.as_slice())
                        || (command && is_assignment(&word));
                    in_word = false;
                    word.clear();
                }
            }
            b';' | b'|' | b'&' | b'(' | b')' | b'`' => {
                in_word = false;
                word.clear();
                command = c != b')';
            }
            b'<' | b'>' => {
                in_word = false;
                word.clear();
                command = false;
            }
            _ => {
                if !in_word {
                    in_word = true;
                    start = i;
                    word.clear();
                }
                match c {
                    b'\\' => {
                        if let Some(&n) = text.get(i + 1) {
                            word.push(n);
                            i += 1;
                        }
                    }
                    b'\'' | b'"' => quote = Some(c),
                    _ => word.push(c),
                }
            }
        }
        i += 1;
    }
    if !in_word {
        start = cursor;
        word.clear();
    }
    Word {
        start,
        text: word,
        quote,
        command,
    }
}

fn is_assignment(w: &[u8]) -> bool {
    match w.iter().position(|&c| c == b'=') {
        Some(p) if p > 0 => w
            .get(..p)
            .is_some_and(|n| n.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_')),
        _ => false,
    }
}

/// The longest prefix every match shares.
pub(crate) fn common_prefix(matches: &[Match]) -> Vec<u8> {
    let Some(first) = matches.first() else {
        return Vec::new();
    };
    let mut n = first.word.len();
    for m in matches.iter().skip(1) {
        n = n.min(
            first
                .word
                .iter()
                .zip(m.word.iter())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    // Do not cut a UTF-8 character in half.
    while n > 0 && first.word.get(n).is_some_and(|&c| (c & 0xc0) == 0x80) {
        n -= 1;
    }
    first.word.get(..n).unwrap_or(&[]).to_vec()
}

/// Quote `s` for insertion into a word open in `quote`.
fn quote_for(s: &[u8], quote: Option<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    for &c in s {
        match quote {
            Some(b'\'') => {
                if c == b'\'' {
                    out.extend_from_slice(b"'\\''");
                } else {
                    out.push(c);
                }
            }
            Some(_) => {
                if matches!(c, b'"' | b'\\' | b'$' | b'`') {
                    out.push(b'\\');
                }
                out.push(c);
            }
            None => {
                if b" \t\n\\'\"`$&|;<>()[]{}*?#~=%!^".contains(&c) {
                    out.push(b'\\');
                }
                out.push(c);
            }
        }
    }
    out
}

/// The home directory of `user`, from the password file.
fn user_home(user: &[u8]) -> Option<Vec<u8>> {
    let passwd = std::fs::read("/etc/passwd").ok()?;
    for line in passwd.split(|&c| c == b'\n') {
        let fields: Vec<&[u8]> = line.split(|&c| c == b':').collect();
        if fields.first() == Some(&user) {
            return fields.get(5).map(|h| h.to_vec());
        }
    }
    None
}

fn user_names() -> Vec<Vec<u8>> {
    std::fs::read("/etc/passwd")
        .map(|p| {
            p.split(|&c| c == b'\n')
                .filter_map(|l| l.split(|&c| c == b':').next())
                .filter(|n| !n.is_empty())
                .map(<[u8]>::to_vec)
                .collect()
        })
        .unwrap_or_default()
}

/// Expand a leading `~` or `~user` for looking up files.
fn expand_tilde(sh: &Shell, dir: &[u8]) -> Vec<u8> {
    let Some(rest) = dir.strip_prefix(b"~") else {
        return dir.to_vec();
    };
    let slash = rest.iter().position(|&c| c == b'/').unwrap_or(rest.len());
    let user = rest.get(..slash).unwrap_or(&[]);
    let home = if user.is_empty() {
        sh.get(b"HOME").map(|v| tok::unmetafy(&v.joined()))
    } else {
        user_home(user)
    };
    match home {
        Some(mut h) => {
            h.extend_from_slice(rest.get(slash..).unwrap_or(&[]));
            h
        }
        None => dir.to_vec(),
    }
}

fn file_kind(path: &[u8]) -> (bool, Option<u8>) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let p = std::ffi::OsStr::from_bytes(path);
    let Ok(lmeta) = std::fs::symlink_metadata(p) else {
        return (false, None);
    };
    let meta = std::fs::metadata(p).ok();
    let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
    let kind = if lmeta.file_type().is_symlink() {
        Some(b'@')
    } else if lmeta.is_dir() {
        Some(b'/')
    } else {
        use std::os::unix::fs::FileTypeExt;
        let ft = lmeta.file_type();
        if ft.is_fifo() {
            Some(b'|')
        } else if ft.is_socket() {
            Some(b'=')
        } else if ft.is_block_device() || ft.is_char_device() {
            Some(b'#')
        } else if lmeta.permissions().mode() & 0o111 != 0 {
            Some(b'*')
        } else {
            None
        }
    };
    (is_dir, kind)
}

/// File names completing `prefix`; with `commands`, only directories and
/// executables.
fn file_matches(sh: &Shell, prefix: &[u8], commands: bool) -> Vec<Match> {
    use std::os::unix::ffi::OsStrExt;
    let (dir_part, name_part) = match prefix.iter().rposition(|&c| c == b'/') {
        Some(p) => (
            prefix.get(..=p).unwrap_or(&[]),
            prefix.get(p + 1..).unwrap_or(&[]),
        ),
        None => (&b""[..], prefix),
    };
    let lookup = if dir_part.is_empty() {
        b"./".to_vec()
    } else {
        expand_tilde(sh, dir_part)
    };
    let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::from_bytes(&lookup)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.as_bytes();
        if !name.starts_with(name_part) {
            continue;
        }
        if name.first() == Some(&b'.') && !name_part.starts_with(b".") && !sh.opt("globdots") {
            continue;
        }
        let mut full = lookup.clone();
        full.extend_from_slice(name);
        let (dir, kind) = file_kind(&full);
        if commands && !dir && kind != Some(b'*') && kind != Some(b'@') {
            continue;
        }
        let mut word = dir_part.to_vec();
        word.extend_from_slice(name);
        out.push(Match {
            word,
            dir,
            kind,
            display: None,
        });
    }
    out
}

/// Reserved words zsh completes in command position.
const RESERVED: &[&[u8]] = &[
    b"!",
    b"[[",
    b"case",
    b"coproc",
    b"do",
    b"done",
    b"elif",
    b"else",
    b"esac",
    b"fi",
    b"for",
    b"foreach",
    b"function",
    b"if",
    b"nocorrect",
    b"repeat",
    b"select",
    b"then",
    b"time",
    b"until",
    b"while",
];

/// Command names starting with `prefix`.
fn command_matches(sh: &Shell, prefix: &[u8]) -> Vec<Match> {
    use std::os::unix::ffi::OsStrExt;
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut add = |n: &[u8]| {
        if n.starts_with(prefix) {
            names.push(tok::unmetafy(n));
        }
    };
    for n in sh.aliases.keys() {
        add(n);
    }
    for n in sh.functions.keys().chain(sh.autoloads.iter()) {
        add(n);
    }
    for n in crate::builtins::names() {
        add(n);
    }
    for n in RESERVED {
        add(n);
    }
    let path = sh
        .get(b"PATH")
        .map_or_else(|| b"/bin:/usr/bin".to_vec(), |v| tok::unmetafy(&v.joined()));
    for dir in path.split(|&c| c == b':') {
        let d = if dir.is_empty() { &b"."[..] } else { dir };
        let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::from_bytes(d)) else {
            continue;
        };
        for ent in rd.flatten() {
            let name = ent.file_name();
            let name = name.as_bytes();
            if !name.starts_with(prefix) {
                continue;
            }
            let mut full = d.to_vec();
            full.push(b'/');
            full.extend_from_slice(name);
            if crate::exec::is_executable(&full) {
                names.push(name.to_vec());
            }
        }
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|word| Match {
            word,
            dir: false,
            kind: None,
            display: None,
        })
        .collect()
}

/// Parameter names completing `$prefix` or `${prefix`.
fn parameter_matches(sh: &Shell, lead: &[u8], prefix: &[u8]) -> Vec<Match> {
    let mut names: Vec<Vec<u8>> = sh
        .vars
        .keys()
        .filter(|n| n.starts_with(prefix))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|n| {
            let mut word = lead.to_vec();
            word.extend(tok::unmetafy(&n));
            if lead.ends_with(b"{") {
                word.push(b'}');
            }
            Match {
                word,
                dir: false,
                kind: None,
                display: Some(tok::unmetafy(&n)),
            }
        })
        .collect()
}

/// The matches for `word`.
pub(crate) fn matches_for(sh: &Shell, word: &Word) -> Vec<Match> {
    let text = &word.text;
    if word.quote != Some(b'\'')
        && let Some(dollar) = text.iter().rposition(|&c| c == b'$')
    {
        let after = text.get(dollar + 1..).unwrap_or(&[]);
        let (brace, name) = match after.strip_prefix(b"{") {
            Some(n) => (true, n),
            None => (false, after),
        };
        if name.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_') {
            let mut lead = text.get(..=dollar).unwrap_or(&[]).to_vec();
            if brace {
                lead.push(b'{');
            }
            return parameter_matches(sh, &lead, name);
        }
    }
    if let Some(user) = text.strip_prefix(b"~")
        && !user.contains(&b'/')
    {
        let mut names: Vec<Vec<u8>> = user_names()
            .into_iter()
            .filter(|n| n.starts_with(user))
            .collect();
        names.sort();
        names.dedup();
        return names
            .into_iter()
            .map(|n| {
                let mut w = b"~".to_vec();
                w.extend(n);
                Match {
                    word: w,
                    dir: true,
                    kind: Some(b'/'),
                    display: None,
                }
            })
            .collect();
    }
    if word.command && !text.contains(&b'/') {
        let mut m = command_matches(sh, text);
        if sh.opt("autocd") {
            m.extend(file_matches(sh, text, true).into_iter().filter(|x| x.dir));
        }
        return m;
    }
    let mut m = file_matches(sh, text, word.command);
    m.sort_by(|a, b| a.word.cmp(&b.word));
    m
}

/// Lay `matches` out in columns the way zsh lists them: sorted down the
/// columns, each as wide as the longest entry plus two.
pub(crate) fn list_columns(matches: &[Match], cols: usize) -> Vec<u8> {
    let entries: Vec<Vec<u8>> = matches
        .iter()
        .map(|m| {
            let shown = if let Some(d) = &m.display {
                d.clone()
            } else {
                match m.word.iter().rposition(|&c| c == b'/') {
                    Some(p) if p + 1 < m.word.len() => m.word.get(p + 1..).unwrap_or(&[]).to_vec(),
                    _ => m.word.clone(),
                }
            };
            let mut e = shown;
            if let Some(k) = m.kind {
                e.push(k);
            }
            e
        })
        .collect();
    let width = entries.iter().map(|e| display_width(e)).max().unwrap_or(0) + 2;
    let ncols = (cols.saturating_sub(1) / width.max(1)).max(1);
    let nrows = entries.len().div_ceil(ncols);
    let mut out = Vec::new();
    for r in 0..nrows {
        let mut line = Vec::new();
        for c in 0..ncols {
            let Some(e) = entries.get(c * nrows + r) else {
                continue;
            };
            line.extend_from_slice(e);
            if (c + 1) * nrows + r < entries.len() {
                line.extend(std::iter::repeat_n(b' ', width - display_width(e)));
            }
        }
        out.extend_from_slice(&line);
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Handle one Tab.
fn complete(sh: &Shell, line: &mut Line, state: CompState) -> CompState {
    if let CompState::Menu {
        start,
        end,
        index,
        matches,
    } = state
    {
        let next = (index + 1) % matches.len().max(1);
        let Some(m) = matches.get(next) else {
            return CompState::None;
        };
        let word = current_word(line.buf.get(..start).unwrap_or(&[]), start);
        let insert = quote_for(&m.word, word.quote);
        let tail = line.buf.split_off(end);
        line.buf.truncate(start);
        line.buf.extend_from_slice(&insert);
        let new_end = line.buf.len();
        line.buf.extend(tail);
        line.cursor = new_end;
        line.refresh();
        return CompState::Menu {
            start,
            end: new_end,
            index: next,
            matches,
        };
    }
    let word = current_word(&line.buf, line.cursor);
    let matches = matches_for(sh, &word);
    if matches.is_empty() {
        let _ok = write_fd(2, b"\x07");
        return CompState::None;
    }
    let prefix_quoted_len = line.cursor - word.start;
    if let [only] = matches.as_slice() {
        let mut insert = if word.quote.is_some() {
            let mut q = vec![word.quote.unwrap_or(b'"')];
            q.extend(quote_for(&only.word, word.quote));
            q
        } else {
            quote_for(&only.word, None)
        };
        if only.dir {
            insert.push(b'/');
            if insert.ends_with(b"//") {
                let _slash = insert.pop();
            }
        } else {
            if let Some(q) = word.quote {
                insert.push(q);
            }
            insert.push(b' ');
        }
        replace_word(line, word.start, prefix_quoted_len, &insert);
        return CompState::None;
    }
    let common = common_prefix(&matches);
    if common.len() > word.text.len() {
        let mut insert = Vec::new();
        if let Some(q) = word.quote {
            insert.push(q);
        }
        insert.extend(quote_for(&common, word.quote));
        replace_word(line, word.start, prefix_quoted_len, &insert);
        return CompState::None;
    }
    if state == CompState::Listed(word.start) {
        let first = matches
            .first()
            .map(|m| quote_for(&m.word, word.quote))
            .unwrap_or_default();
        let mut insert = Vec::new();
        if let Some(q) = word.quote {
            insert.push(q);
        }
        insert.extend(first);
        replace_word(line, word.start, prefix_quoted_len, &insert);
        return CompState::Menu {
            start: word.start,
            end: line.cursor,
            index: 0,
            matches,
        };
    }
    let saved_cursor = line.cursor;
    line.cursor = line.buf.len();
    line.below();
    let _ok = write_fd(2, &list_columns(&matches, columns()));
    line.print_prompt_lines();
    line.cursor = saved_cursor;
    line.refresh();
    CompState::Listed(word.start)
}

/// Replace `len` bytes of the line at `start` with `text`, leaving the
/// cursor after it.
fn replace_word(line: &mut Line, start: usize, len: usize, text: &[u8]) {
    let tail = line.buf.split_off((start + len).min(line.buf.len()));
    line.buf.truncate(start);
    line.buf.extend_from_slice(text);
    line.cursor = line.buf.len();
    line.buf.extend(tail);
    line.refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(w: &str) -> Match {
        Match {
            word: w.as_bytes().to_vec(),
            dir: false,
            kind: None,
            display: None,
        }
    }

    #[test]
    fn the_first_word_is_in_command_position() {
        let w = current_word(b"gi", 2);
        assert!(w.command);
        assert_eq!(w.text, b"gi");
        assert_eq!(w.start, 0);
    }

    #[test]
    fn an_argument_is_not_in_command_position() {
        let w = current_word(b"ls src/ma", 9);
        assert!(!w.command);
        assert_eq!(w.text, b"src/ma");
        assert_eq!(w.start, 3);
    }

    #[test]
    fn a_separator_starts_a_new_command() {
        assert!(current_word(b"cd /tmp && ec", 13).command);
        assert!(current_word(b"ls | gr", 7).command);
        assert!(current_word(b"FOO=1 ma", 8).command);
        assert!(current_word(b"sudo -", 6).text == b"-");
    }

    #[test]
    fn quotes_and_backslashes_are_removed_from_the_prefix() {
        let w = current_word(b"cat My\\ Fi", 10);
        assert_eq!(w.text, b"My Fi");
        let w = current_word(b"cat 'My Fi", 10);
        assert_eq!(w.text, b"My Fi");
        assert_eq!(w.quote, Some(b'\''));
    }

    #[test]
    fn an_empty_word_at_a_space_starts_at_the_cursor() {
        let w = current_word(b"ls ", 3);
        assert_eq!(w.start, 3);
        assert!(w.text.is_empty());
        assert!(!w.command);
    }

    #[test]
    fn the_common_prefix_stops_where_matches_differ() {
        assert_eq!(common_prefix(&[m("foobar"), m("foobaz"), m("foo")]), b"foo");
        assert_eq!(common_prefix(&[m("abc")]), b"abc");
    }

    #[test]
    fn inserted_names_are_backslash_quoted() {
        assert_eq!(quote_for(b"a b$c", None), b"a\\ b\\$c");
        assert_eq!(quote_for(b"it's", Some(b'\'')), b"it'\\''s");
    }

    #[test]
    fn listings_run_down_the_columns() {
        let ms: Vec<Match> = ["a", "b", "c", "d", "e"].iter().map(|s| m(s)).collect();
        let out = list_columns(&ms, 10);
        assert_eq!(out, b"a  c  e\r\nb  d\r\n");
    }

    #[test]
    fn escape_sequences_take_no_columns() {
        assert_eq!(display_width(b"\x1b[1;32mok\x1b[0m> "), 4);
    }

    #[test]
    fn wide_characters_take_two_columns() {
        // agnoster's status segment: a narrow ✗, a wide ⚡ and a narrow
        // powerline arrow.
        assert_eq!(display_width("✗ ⚡ root\u{e0b0} ".as_bytes()), 11);
        assert_eq!(display_width("日本".as_bytes()), 4);
        assert_eq!(display_width("e\u{301}".as_bytes()), 1);
        assert_eq!(display_width(b"a\xffb"), 3);
    }
}
