//! Parsing a POSIX basic or extended regular expression into a syntax tree.
//!
//! The grammar and its extensions follow musl 1.2.5's parser (MIT,
//! `src/regex/regcomp.c`, from TRE), where POSIX leaves a choice:
//!
//! * In a BRE, `^` is an anchor only at the start of the expression, of a
//!   `\(` group or of a `\|` alternative, and `$` only at the end of one; a
//!   `*` there is an ordinary character. `\+`, `\?` and `\|` are GNU's
//!   repetitions and alternation. `\1` to `\9` are back-references, and `\0`
//!   is the character `0`.
//! * In an ERE, `^` and `$` are anchors everywhere, a repetition with nothing
//!   before it is `REG_BADRPT`, a `)` with no `(` is an ordinary character, and
//!   `\1` is the character `1`: POSIX gives an ERE no back-references.
//! * Empty alternatives and groups, `()` and `(a|)`, match the empty string,
//!   and repetitions may follow repetitions, as in `a**`.
//! * `{,n}` is `REG_BADBR`, as in musl, and so is a bound above `RE_DUP_MAX`
//!   (255). A `{` that is never closed is `REG_EBRACE`, POSIX's code for an
//!   unbalanced brace, where musl says `REG_BADBR`.
//!
//! Escapes are musl's: `\t`, `\n`, `\r`, `\f`, `\a` and `\e` are those control
//! characters; `\w`, `\W`, `\s`, `\S`, `\d` and `\D` are the word, space and
//! digit classes and their complements; `\b`, `\B`, `\<` and `\>` are the word
//! assertions; `\xHH` and `\x{HHHH}` name a character by its value. Any other
//! escaped character is itself. Matching is by bytes, so a `\x` value above
//! 0xff matches nothing.
//!
//! In a bracket expression, ranges and classes are musl's, with musl's
//! extension `[a-z--@]`. The C locale's collating elements are its bytes, so
//! `[.c.]` and `[=c=]` naming one byte stand for it, as POSIX has them, where
//! musl refuses all of them; a longer name is `REG_ECOLLATE`. With `REG_ICASE`
//! the list is folded before it is negated, so `[^a]` matches neither `a` nor
//! `A`, as the Austin Group resolved (bug 872). With `REG_NEWLINE`, `.` and a
//! negated list never match a newline.
//!
//! Groups and stacked repetitions nest at most [`MAX_DEPTH`] deep, beyond
//! which a pattern is `REG_ESPACE`. The parser and the compiler recurse once
//! per level, and a thread's stack may be small.

use core::ffi::c_int;

use super::{
    REG_BADBR, REG_BADRPT, REG_EBRACE, REG_EBRACK, REG_ECOLLATE, REG_ECTYPE, REG_EESCAPE,
    REG_EPAREN, REG_ERANGE, REG_ESPACE, REG_ESUBREG, REG_EXTENDED, REG_ICASE, REG_NEWLINE,
};
use crate::fnmatch::{in_class, is_class};
use crate::growable::Growable;

/// An index into [`Ast::nodes`].
pub(super) type NodeId = u32;

/// A repetition's maximum when it has none.
pub(super) const INFINITE: u32 = u32::MAX;

/// `RE_DUP_MAX`, from `include/limits.h`.
const DUP_MAX: u32 = 255;

/// The most nodes a pattern may make, beyond which it is `REG_ESPACE`.
const MAX_NODES: usize = 1 << 20;

/// How deeply groups and stacked repetitions may nest.
const MAX_DEPTH: usize = 64;

/// A zero-width assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Assertion {
    /// `^`: the start of the string, or after a newline with `REG_NEWLINE`.
    LineStart,
    /// `$`: the end of the string, or before a newline with `REG_NEWLINE`.
    LineEnd,
    /// `\b`: between a word character and a non-word one.
    WordBoundary,
    /// `\B`: not at a word boundary.
    NotWordBoundary,
    /// `\<`: before a word.
    WordStart,
    /// `\>`: after a word.
    WordEnd,
}

/// A syntax tree node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// Matches the empty string.
    Empty,
    /// Matches one byte.
    Byte(u8),
    /// Matches one byte of [`Ast::sets`]`[i]`.
    Set(u32),
    /// Matches the empty string where the assertion holds.
    Assert(Assertion),
    /// Matches what group `n` last matched.
    Backref(u32),
    /// Group `n`, capturing what `inner` matches.
    Group(u32, NodeId),
    /// The `count` nodes from [`Ast::children`]`[first]`, in sequence.
    Concat(u32, u32),
    /// Any of the `count` nodes from [`Ast::children`]`[first]`.
    Alt(u32, u32),
    /// `inner` from `min` to `max` times; `max` may be [`INFINITE`].
    Repeat(NodeId, u32, u32),
}

/// A set of bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ByteSet([u64; 4]);

impl ByteSet {
    /// The empty set.
    pub(super) const EMPTY: Self = Self([0; 4]);

    /// Whether `b` is in the set.
    pub(super) fn contains(&self, b: u8) -> bool {
        let word = self.0.get(usize::from(b >> 6)).copied().unwrap_or(0);
        word & (1 << (b & 63)) != 0
    }

    /// Adds `b`.
    fn insert(&mut self, b: u8) {
        if let Some(word) = self.0.get_mut(usize::from(b >> 6)) {
            *word |= 1 << (b & 63);
        }
    }

    /// Removes `b`.
    fn remove(&mut self, b: u8) {
        if let Some(word) = self.0.get_mut(usize::from(b >> 6)) {
            *word &= !(1 << (b & 63));
        }
    }

    /// Adds every byte from `lo` to `hi`.
    fn insert_range(&mut self, lo: u8, hi: u8) {
        for b in lo..=hi {
            self.insert(b);
        }
    }

    /// Replaces the set with its complement.
    fn complement(&mut self) {
        for word in &mut self.0 {
            *word = !*word;
        }
    }
}

/// A parsed pattern.
#[derive(Debug)]
pub(super) struct Ast {
    /// Every node; children come before their parents.
    pub(super) nodes: Growable<Kind>,
    /// The child lists of concatenations and alternations, each contiguous.
    pub(super) children: Growable<NodeId>,
    /// The byte sets of bracket expressions, `.` and case-folded letters.
    pub(super) sets: Growable<ByteSet>,
    /// The whole pattern.
    pub(super) root: NodeId,
    /// How many groups there are: `re_nsub`.
    pub(super) groups: u32,
    /// Whether any back-reference appears.
    pub(super) has_backrefs: bool,
}

impl Ast {
    /// Node `id`, or `Empty` if there is none.
    pub(super) fn kind(&self, id: NodeId) -> Kind {
        self.nodes
            .as_slice()
            .get(id as usize)
            .copied()
            .unwrap_or(Kind::Empty)
    }

    /// Child `i` of a list starting at `first`.
    pub(super) fn child(&self, first: u32, i: u32) -> NodeId {
        let index = first as usize + i as usize;
        self.children.as_slice().get(index).copied().unwrap_or(0)
    }
}

/// A result carrying a `regcomp` error code.
type Parsed<T> = Result<T, c_int>;

/// The parser's state.
struct Parser<'a> {
    /// The pattern, without its NUL.
    pat: &'a [u8],
    /// The next byte to read.
    pos: usize,
    /// `REG_EXTENDED`.
    ere: bool,
    /// `REG_ICASE`.
    icase: bool,
    /// `REG_NEWLINE`.
    newline: bool,
    /// The tree being built.
    ast: Ast,
    /// The highest back-reference seen.
    max_backref: u32,
}

/// `c` in the other case, if it is an ASCII letter.
const fn other_case(c: u8) -> u8 {
    if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c.to_ascii_uppercase()
    }
}

/// Parses `pat` under the `regcomp` flags `cflags`.
pub(super) fn parse(pat: &[u8], cflags: c_int) -> Parsed<Ast> {
    let mut p = Parser {
        pat,
        pos: 0,
        ere: cflags & REG_EXTENDED != 0,
        icase: cflags & REG_ICASE != 0,
        newline: cflags & REG_NEWLINE != 0,
        ast: Ast {
            nodes: Growable::new(),
            children: Growable::new(),
            sets: Growable::new(),
            root: 0,
            groups: 0,
            has_backrefs: false,
        },
        max_backref: 0,
    };
    let root = p.alternation(0)?;
    if p.pos < p.pat.len() {
        // Only a BRE's `\)` with no `\(` stops the top level early.
        return Err(REG_EPAREN);
    }
    if p.max_backref > p.ast.groups {
        return Err(REG_ESUBREG);
    }
    p.ast.root = root;
    Ok(p.ast)
}

impl<'a> Parser<'a> {
    /// The byte `k` past the current one, or 0 past the end.
    fn peek(&self, k: usize) -> u8 {
        self.pat.get(self.pos + k).copied().unwrap_or(0)
    }

    /// Whether at least `k + 1` bytes are left.
    fn has(&self, k: usize) -> bool {
        self.pos + k < self.pat.len()
    }

    /// Adds a node.
    fn add(&mut self, kind: Kind) -> Parsed<NodeId> {
        let id = self.ast.nodes.len();
        if id >= MAX_NODES || !self.ast.nodes.push(kind) {
            return Err(REG_ESPACE);
        }
        Ok(id as NodeId)
    }

    /// Adds a concatenation or alternation of `items`, or the single item.
    fn add_list(&mut self, items: &Growable<NodeId>, alternation: bool) -> Parsed<NodeId> {
        match items.as_slice() {
            [] => return self.add(Kind::Empty),
            [only] => return Ok(*only),
            _ => {}
        }
        let first = self.ast.children.len() as u32;
        if !self.ast.children.reserve(items.len()) {
            return Err(REG_ESPACE);
        }
        for &item in items.as_slice() {
            let _ = self.ast.children.push(item);
        }
        let count = items.len() as u32;
        self.add(if alternation {
            Kind::Alt(first, count)
        } else {
            Kind::Concat(first, count)
        })
    }

    /// Adds a set.
    fn add_set(&mut self, set: ByteSet) -> Parsed<NodeId> {
        let index = self.ast.sets.len() as u32;
        if !self.ast.sets.push(set) {
            return Err(REG_ESPACE);
        }
        self.add(Kind::Set(index))
    }

    /// Whether a group's close is next.
    fn at_close(&self) -> bool {
        if self.ere {
            self.peek(0) == b')'
        } else {
            self.peek(0) == b'\\' && self.peek(1) == b')'
        }
    }

    /// Whether an alternation bar is next.
    fn at_bar(&self) -> bool {
        if self.ere {
            self.peek(0) == b'|'
        } else {
            self.peek(0) == b'\\' && self.peek(1) == b'|'
        }
    }

    /// Alternatives separated by `|`, up to the end or, inside a group, its
    /// close.
    fn alternation(&mut self, depth: usize) -> Parsed<NodeId> {
        let mut alternatives = Growable::new();
        loop {
            let branch = self.branch(depth)?;
            if !alternatives.push(branch) {
                return Err(REG_ESPACE);
            }
            if !self.at_bar() {
                break;
            }
            self.pos += if self.ere { 1 } else { 2 };
        }
        self.add_list(&alternatives, true)
    }

    /// A sequence of repeated atoms.
    fn branch(&mut self, depth: usize) -> Parsed<NodeId> {
        let start = self.pos;
        let mut items = Growable::new();
        while self.has(0) && !self.at_bar() && !(depth > 0 && self.at_close()) {
            let at_start = self.pos == start;
            let leading_caret = !self.ere && at_start && self.peek(0) == b'^';
            let mut node = self.atom(depth, at_start)?;
            // In a BRE, a `*` right after a leading `^` is ordinary. Each
            // stacked repetition nests the tree a level deeper, so they count
            // against the depth limit as groups do.
            let mut stacked = 0;
            while !(leading_caret && self.pos == start + 1) {
                let Some((min, max)) = self.duplication()? else {
                    break;
                };
                stacked += 1;
                if depth + stacked > MAX_DEPTH {
                    return Err(REG_ESPACE);
                }
                node = self.repeat(node, min, max)?;
            }
            if !items.push(node) {
                return Err(REG_ESPACE);
            }
        }
        self.add_list(&items, false)
    }

    /// `node` repeated from `min` to `max` times.
    fn repeat(&mut self, node: NodeId, min: u32, max: u32) -> Parsed<NodeId> {
        match (min, max) {
            (_, 0) => self.add(Kind::Empty),
            (1, 1) => Ok(node),
            _ => self.add(Kind::Repeat(node, min, max)),
        }
    }

    /// One atom.
    fn atom(&mut self, depth: usize, at_start: bool) -> Parsed<NodeId> {
        let c = self.peek(0);
        match c {
            b'[' => {
                self.pos += 1;
                self.bracket()
            }
            b'.' => {
                self.pos += 1;
                let mut set = ByteSet::EMPTY;
                set.complement();
                if self.newline {
                    set.remove(b'\n');
                }
                self.add_set(set)
            }
            b'^' if self.ere || at_start => {
                self.pos += 1;
                self.add(Kind::Assert(Assertion::LineStart))
            }
            b'$' if self.ere || self.bre_dollar_is_anchor() => {
                self.pos += 1;
                self.add(Kind::Assert(Assertion::LineEnd))
            }
            b'*' | b'+' | b'?' | b'{' if self.ere => Err(REG_BADRPT),
            b'(' if self.ere => self.group(depth),
            b'\\' => self.escape(depth),
            _ => {
                self.pos += 1;
                self.literal(c)
            }
        }
    }

    /// Whether a BRE's `$` here ends the expression, a group or an
    /// alternative.
    fn bre_dollar_is_anchor(&self) -> bool {
        !self.has(1) || (self.peek(1) == b'\\' && matches!(self.peek(2), b')' | b'|'))
    }

    /// A byte that matches itself, or either case of a letter with
    /// `REG_ICASE`.
    fn literal(&mut self, c: u8) -> Parsed<NodeId> {
        if self.icase && c.is_ascii_alphabetic() {
            let mut set = ByteSet::EMPTY;
            set.insert(c);
            set.insert(other_case(c));
            return self.add_set(set);
        }
        self.add(Kind::Byte(c))
    }

    /// A group, at its `(` or `\(`.
    fn group(&mut self, depth: usize) -> Parsed<NodeId> {
        if depth >= MAX_DEPTH {
            return Err(REG_ESPACE);
        }
        let width = if self.ere { 1 } else { 2 };
        self.pos += width;
        self.ast.groups += 1;
        let index = self.ast.groups;
        let inner = self.alternation(depth + 1)?;
        if !self.at_close() {
            return Err(REG_EPAREN);
        }
        self.pos += width;
        self.add(Kind::Group(index, inner))
    }

    /// An escape, at its backslash.
    fn escape(&mut self, depth: usize) -> Parsed<NodeId> {
        if !self.has(1) {
            return Err(REG_EESCAPE);
        }
        let c = self.peek(1);
        let assertion = match c {
            b'b' => Some(Assertion::WordBoundary),
            b'B' => Some(Assertion::NotWordBoundary),
            b'<' => Some(Assertion::WordStart),
            b'>' => Some(Assertion::WordEnd),
            _ => None,
        };
        if let Some(assertion) = assertion {
            self.pos += 2;
            return self.add(Kind::Assert(assertion));
        }
        let control = match c {
            b't' => Some(b'\t'),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b'f' => Some(0x0c),
            b'a' => Some(0x07),
            b'e' => Some(0x1b),
            _ => None,
        };
        if let Some(byte) = control {
            self.pos += 2;
            return self.literal(byte);
        }
        match c {
            b'(' if !self.ere => self.group(depth),
            b')' if !self.ere => Err(REG_EPAREN),
            b'{' | b'+' | b'?' if !self.ere => Err(REG_BADRPT),
            b'1'..=b'9' if !self.ere => {
                self.pos += 2;
                let n = u32::from(c - b'0');
                self.max_backref = self.max_backref.max(n);
                self.ast.has_backrefs = true;
                self.add(Kind::Backref(n))
            }
            b'w' | b'W' | b's' | b'S' | b'd' | b'D' => {
                self.pos += 2;
                self.class_escape(c)
            }
            b'x' => {
                self.pos += 2;
                self.hex_escape()
            }
            _ => {
                self.pos += 2;
                self.literal(c)
            }
        }
    }

    /// `\w`, `\s` or `\d`, or its complement in upper case.
    fn class_escape(&mut self, c: u8) -> Parsed<NodeId> {
        let class: &[u8] = match c.to_ascii_lowercase() {
            b'w' => b"alnum",
            b's' => b"space",
            _ => b"digit",
        };
        let mut set = ByteSet::EMPTY;
        for b in 0..=u8::MAX {
            if in_class(class, b) {
                set.insert(b);
            }
        }
        if c.eq_ignore_ascii_case(&b'w') {
            set.insert(b'_');
        }
        if c.is_ascii_uppercase() {
            // musl expands these to negated lists, which never match a
            // newline with `REG_NEWLINE`.
            set.complement();
            if self.newline {
                set.remove(b'\n');
            }
        }
        self.add_set(set)
    }

    /// The digits of `\xHH` or `\x{HHHH}`, after the `x`.
    fn hex_escape(&mut self) -> Parsed<NodeId> {
        let braced = self.peek(0) == b'{';
        let digits = if braced {
            self.pos += 1;
            8
        } else {
            2
        };
        let mut value: u32 = 0;
        let mut count = 0;
        while count < digits && value < 0x11_0000 {
            let Some(digit) = char::from(self.peek(0)).to_digit(16) else {
                break;
            };
            value = value * 16 + digit;
            self.pos += 1;
            count += 1;
        }
        if braced {
            if self.peek(0) != b'}' {
                return Err(REG_EBRACE);
            }
            self.pos += 1;
        }
        match u8::try_from(value) {
            Ok(byte) => self.literal(byte),
            Err(_) => self.add_set(ByteSet::EMPTY),
        }
    }

    /// A repetition operator after an atom, if there is one.
    fn duplication(&mut self) -> Parsed<Option<(u32, u32)>> {
        let c = self.peek(0);
        if c == b'*' {
            self.pos += 1;
            return Ok(Some((0, INFINITE)));
        }
        let (op, width) = if self.ere {
            (c, 1)
        } else if c == b'\\' {
            (self.peek(1), 2)
        } else {
            return Ok(None);
        };
        let bounds = match op {
            b'+' => (1, INFINITE),
            b'?' => (0, 1),
            b'{' => {
                self.pos += width;
                return self.bounds().map(Some);
            }
            _ => return Ok(None),
        };
        self.pos += width;
        Ok(Some(bounds))
    }

    /// A decimal count, if digits are next.
    fn count(&mut self) -> Parsed<Option<u32>> {
        let mut value: Option<u32> = None;
        while self.peek(0).is_ascii_digit() {
            let digit = u32::from(self.peek(0) - b'0');
            let next = value.unwrap_or(0) * 10 + digit;
            if next > DUP_MAX {
                return Err(REG_BADBR);
            }
            value = Some(next);
            self.pos += 1;
        }
        Ok(value)
    }

    /// The bounds of `{m}`, `{m,}` or `{m,n}`, after the `{`.
    fn bounds(&mut self) -> Parsed<(u32, u32)> {
        let min = self.count()?;
        let max = if self.peek(0) == b',' {
            self.pos += 1;
            self.count()?.unwrap_or(INFINITE)
        } else {
            min.unwrap_or(0)
        };
        let close_width = if self.ere { 1 } else { 2 };
        if !self.has(close_width - 1) {
            return Err(REG_EBRACE);
        }
        let closed = if self.ere {
            self.peek(0) == b'}'
        } else {
            self.peek(0) == b'\\' && self.peek(1) == b'}'
        };
        let Some(min) = min else {
            return Err(REG_BADBR);
        };
        if !closed || max < min {
            return Err(REG_BADBR);
        }
        self.pos += close_width;
        Ok((min, max))
    }

    /// The name inside `[:name:]`, `[.name.]` or `[=name=]`, at its `[`,
    /// moving past the closing `]`.
    fn bracket_name(&mut self, delimiter: u8) -> Parsed<&'a [u8]> {
        let start = self.pos + 2;
        let mut end = start;
        loop {
            if end + 1 >= self.pat.len() {
                return Err(if delimiter == b':' {
                    REG_ECTYPE
                } else {
                    REG_EBRACK
                });
            }
            if self.pat.get(end) == Some(&delimiter) && self.pat.get(end + 1) == Some(&b']') {
                break;
            }
            end += 1;
        }
        self.pos = end + 2;
        Ok(self.pat.get(start..end).unwrap_or(&[]))
    }

    /// A bracket expression, after its `[`.
    fn bracket(&mut self) -> Parsed<NodeId> {
        let mut set = ByteSet::EMPTY;
        let negated = self.peek(0) == b'^';
        if negated {
            self.pos += 1;
        }
        let list_start = self.pos;
        loop {
            if !self.has(0) {
                return Err(REG_EBRACK);
            }
            let c = self.peek(0);
            if c == b']' && self.pos != list_start {
                self.pos += 1;
                break;
            }
            if c == b'-' && self.pos != list_start {
                if !self.has(1) {
                    return Err(REG_EBRACK);
                }
                // `-` is ordinary only first, last, or as musl's `--x` range.
                let next = self.peek(1);
                if next != b']' && (next != b'-' || self.peek(2) == b']') {
                    return Err(REG_ERANGE);
                }
            }
            self.bracket_term(&mut set)?;
        }
        if self.icase {
            for b in (b'a'..=b'z').chain(b'A'..=b'Z') {
                if set.contains(b) {
                    set.insert(other_case(b));
                }
            }
        }
        if negated {
            set.complement();
            if self.newline {
                set.remove(b'\n');
            }
        }
        self.add_set(set)
    }

    /// One term of a bracket expression: a class, an equivalence class, a
    /// character or a range.
    fn bracket_term(&mut self, set: &mut ByteSet) -> Parsed<()> {
        let c = self.peek(0);
        let next = self.peek(1);
        if c == b'[' && next == b':' {
            let name = self.bracket_name(b':')?;
            if !is_class(name) {
                return Err(REG_ECTYPE);
            }
            for b in 0..=u8::MAX {
                if in_class(name, b) {
                    set.insert(b);
                }
            }
            return Ok(());
        }
        let low = if c == b'[' && (next == b'.' || next == b'=') {
            let name = self.bracket_name(next)?;
            let [only] = *name else {
                return Err(REG_ECOLLATE);
            };
            if next == b'=' {
                set.insert(only);
                return Ok(());
            }
            only
        } else {
            self.pos += 1;
            c
        };
        if self.peek(0) != b'-' || self.peek(1) == b']' || !self.has(1) {
            set.insert(low);
            return Ok(());
        }
        self.pos += 1;
        let high = if self.peek(0) == b'[' && self.peek(1) == b'.' {
            let name = self.bracket_name(b'.')?;
            let [only] = *name else {
                return Err(REG_ECOLLATE);
            };
            only
        } else if self.peek(0) == b'[' && matches!(self.peek(1), b'=' | b':') {
            return Err(REG_ERANGE);
        } else {
            let h = self.peek(0);
            self.pos += 1;
            h
        };
        if low > high {
            return Err(REG_ERANGE);
        }
        set.insert_range(low, high);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(pat: &[u8], cflags: c_int) -> Result<Vec<Kind>, c_int> {
        parse(pat, cflags).map(|ast| ast.nodes.as_slice().to_vec())
    }

    fn only_set(pat: &[u8], cflags: c_int) -> ByteSet {
        let ast = parse(pat, cflags).unwrap_or_else(|code| panic!("{pat:?} fails with {code}"));
        assert_eq!(ast.sets.len(), 1, "{pat:?} makes one set");
        ast.sets.as_slice()[0]
    }

    #[test]
    fn errors_have_their_codes() {
        let ere = REG_EXTENDED;
        assert_eq!(kinds(b"a\\", 0).err(), Some(REG_EESCAPE));
        assert_eq!(kinds(b"[a", 0).err(), Some(REG_EBRACK));
        assert_eq!(kinds(b"[z-a]", 0).err(), Some(REG_ERANGE));
        assert_eq!(kinds(b"[[:bogus:]]", 0).err(), Some(REG_ECTYPE));
        assert_eq!(kinds(b"[[:alpha", 0).err(), Some(REG_ECTYPE));
        assert_eq!(kinds(b"[[.ab.]]", 0).err(), Some(REG_ECOLLATE));
        assert_eq!(kinds(b"\\(a", 0).err(), Some(REG_EPAREN));
        assert_eq!(kinds(b"a\\)", 0).err(), Some(REG_EPAREN));
        assert_eq!(kinds(b"(a", ere).err(), Some(REG_EPAREN));
        assert_eq!(kinds(b"a{1", ere).err(), Some(REG_EBRACE));
        assert_eq!(kinds(b"a{1x}", ere).err(), Some(REG_BADBR));
        assert_eq!(kinds(b"a{,2}", ere).err(), Some(REG_BADBR));
        assert_eq!(kinds(b"a{2,1}", ere).err(), Some(REG_BADBR));
        assert_eq!(kinds(b"a{256}", ere).err(), Some(REG_BADBR));
        assert_eq!(kinds(b"*a", ere).err(), Some(REG_BADRPT));
        assert_eq!(kinds(b"a|?", ere).err(), Some(REG_BADRPT));
        assert_eq!(kinds(b"\\{1\\}", 0).err(), Some(REG_BADRPT));
        assert_eq!(kinds(b"\\1", 0).err(), Some(REG_ESUBREG));
        assert_eq!(kinds(b"\\(a\\)\\2", 0).err(), Some(REG_ESUBREG));
        assert_eq!(kinds(b"\\x{41", 0).err(), Some(REG_EBRACE));
    }

    #[test]
    fn deep_nesting_is_out_of_space() {
        let mut deep = Vec::new();
        deep.resize(MAX_DEPTH + 1, b'(');
        deep.push(b'a');
        deep.resize(2 * (MAX_DEPTH + 1) + 1, b')');
        assert_eq!(kinds(&deep, REG_EXTENDED).err(), Some(REG_ESPACE));
        let ok = &deep[1..deep.len() - 1];
        assert!(kinds(ok, REG_EXTENDED).is_ok());
    }

    #[test]
    fn bre_stars_and_anchors_are_ordinary_where_posix_says() {
        assert_eq!(
            kinds(b"*a", 0),
            Ok(vec![Kind::Byte(b'*'), Kind::Byte(b'a'), Kind::Concat(0, 2)])
        );
        assert_eq!(
            kinds(b"a^b$c", 0),
            Ok(vec![
                Kind::Byte(b'a'),
                Kind::Byte(b'^'),
                Kind::Byte(b'b'),
                Kind::Byte(b'$'),
                Kind::Byte(b'c'),
                Kind::Concat(0, 5)
            ])
        );
        assert_eq!(
            kinds(b"^*", 0),
            Ok(vec![
                Kind::Assert(Assertion::LineStart),
                Kind::Byte(b'*'),
                Kind::Concat(0, 2)
            ])
        );
        assert_eq!(kinds(b"a)", REG_EXTENDED).map(|k| k.len()), Ok(3));
        assert_eq!(kinds(b"\\0", 0), Ok(vec![Kind::Byte(b'0')]));
        assert_eq!(kinds(b"\\1", REG_EXTENDED), Ok(vec![Kind::Byte(b'1')]));
    }

    #[test]
    fn escapes_are_musls() {
        assert_eq!(kinds(b"\\t", 0), Ok(vec![Kind::Byte(b'\t')]));
        assert_eq!(kinds(b"\\n", 0), Ok(vec![Kind::Byte(b'\n')]));
        assert_eq!(kinds(b"\\x41", 0), Ok(vec![Kind::Byte(b'A')]));
        assert_eq!(kinds(b"\\x{7e}", 0), Ok(vec![Kind::Byte(b'~')]));
        assert_eq!(kinds(b"\\q", 0), Ok(vec![Kind::Byte(b'q')]));
        assert!(only_set(b"\\d", 0).contains(b'7'));
        assert!(!only_set(b"\\D", 0).contains(b'7'));
        assert!(only_set(b"\\w", 0).contains(b'_'));
        assert!(!only_set(b"\\S", REG_NEWLINE).contains(b'\n'));
        assert!(!only_set(b"\\x{100}", 0).contains(0));
        assert_eq!(
            kinds(b"\\<", 0),
            Ok(vec![Kind::Assert(Assertion::WordStart)])
        );
    }

    #[test]
    fn brackets_take_classes_ranges_and_collating_bytes() {
        let set = only_set(b"[]a-c[:digit:][.-.][=z=]]", 0);
        for b in *b"]abc0123456789-z" {
            assert!(set.contains(b), "{}", b as char);
        }
        assert!(!set.contains(b'd'));
        let set = only_set(b"[a-z--@]", 0);
        assert!(set.contains(b'q') && set.contains(b'-') && set.contains(b'@'));
        assert!(!only_set(b"[^a]", REG_NEWLINE).contains(b'\n'));
        assert!(only_set(b"[^a]", 0).contains(b'\n'));
    }

    #[test]
    fn a_negated_folded_bracket_excludes_both_cases() {
        let set = only_set(b"[^aB]", REG_ICASE);
        for b in *b"aAbB" {
            assert!(!set.contains(b));
        }
        assert!(set.contains(b'c'));
    }
}
