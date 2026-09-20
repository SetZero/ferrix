//! Pattern matching on tokenized patterns: `*`, `?`, `[...]`, `(a|b)`, and
//! with EXTENDED_GLOB `#`, `##` and `^`. A plain byte is literal, which is how
//! quoting reaches the matcher: quoted characters were never tokenized.
//!
//! A subset of zsh's `pattern.c`; the rest (`(#i)`, `(#b)`) lands with the
//! expansion milestone.

use crate::tok::{BANG, BAR, BNULL, DASH, HAT, INANG, INBRACK, INPAR, META, OUTANG, OUTBRACK};
use crate::tok::{OUTPAR, POUND, QUEST, STAR, TILDE};

/// One element of a compiled pattern.
#[derive(Debug, Clone)]
enum Node {
    Lit(u8),
    Any,
    Star,
    Class {
        neg: bool,
        items: Vec<(u32, u32)>,
        named: Vec<Vec<u8>>,
    },
    /// `(...)`: alternatives, and the number the group has under `(#b)`,
    /// counted from the opening parenthesis in the order they are written.
    Group(Vec<Branch>, usize),
    /// `x#`: zero or more of the node; `x##`: one or more.
    Repeat(Box<Node>, bool),
    /// `<a-b>`: a decimal number in the range; bounds absent are open.
    Range(Option<u64>, Option<u64>),
}

/// One alternative of a pattern: what it matches, less what it must not.
///
/// EXTENDED_GLOB's `x~y` matches whatever `x` does except what `y` does, over
/// the same whole string, and chains left to right: `a*~*b*~*c*` is `a*`
/// without the strings holding a `b` and without those holding a `c`. `|`
/// binds looser, so `a*~ab|ac` is `(a*~ab)` or `ac`.
#[derive(Debug, Clone, Default)]
struct Branch {
    nodes: Vec<Node>,
    excepts: Vec<Vec<Node>>,
}

impl Branch {
    /// True if the whole of `s` matches this alternative.
    fn matches(&self, s: &[u8]) -> bool {
        match_nodes(&self.nodes, s) && !self.excepts.iter().any(|e| match_nodes(e, s))
    }

    /// Where the nodes go now: the last `~` operand, or the alternative
    /// itself if it has none.
    fn target(&mut self) -> &mut Vec<Node> {
        match self.excepts.last_mut() {
            Some(e) => e,
            None => &mut self.nodes,
        }
    }
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub(crate) struct Pattern {
    branches: Vec<Branch>,
    negate: bool,
    /// `(#b)`: the groups are to be remembered, for `$match` and its two
    /// arrays of offsets.
    backref: bool,
    groups: usize,
}

/// Decode one UTF-8 character at `s`, falling back to one byte.
fn char_at(s: &[u8]) -> Option<(u32, usize)> {
    let &b = s.first()?;
    let len = match b {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    };
    if len > 1
        && let Some(bytes) = s.get(..len)
        && let Ok(st) = std::str::from_utf8(bytes)
        && let Some(ch) = st.chars().next()
    {
        return Some((u32::from(ch), len));
    }
    Some((u32::from(b), 1))
}

impl Pattern {
    /// Compile a tokenized, unmetafied-on-match pattern. `extended` turns on
    /// EXTENDED_GLOB's operators.
    pub(crate) fn compile(pat: &[u8], extended: bool) -> Pattern {
        let mut i = 0;
        let mut negate = false;
        if extended && pat.first() == Some(&HAT) {
            negate = true;
            i = 1;
        }
        // `(#b)` in front asks for the groups to be remembered. zsh takes
        // the flag anywhere in the pattern; here it is read where every
        // caller writes it, at the front.
        let backref = pat.get(i..i + 4) == Some(&[INPAR, POUND, b'b', OUTPAR]);
        if backref {
            i += 4;
        }
        let mut groups = 0;
        let (branches, _) = parse_alts(pat, &mut i, extended, false, &mut groups);
        Pattern {
            branches,
            negate,
            backref,
            groups,
        }
    }

    /// True if the pattern asked for its groups to be remembered.
    pub(crate) fn has_backrefs(&self) -> bool {
        self.backref
    }

    /// Where each group matched in `s`, once the whole of `s` has matched:
    /// one entry per group, in the order the groups are written, empty for a
    /// group the winning path never entered.
    ///
    /// The search takes the first path that matches the whole string, trying
    /// the longest reach of a `*` first, which is the path zsh reports.
    pub(crate) fn captures(&self, s: &[u8]) -> Option<Vec<(usize, usize)>> {
        if self.negate {
            return None;
        }
        for b in &self.branches {
            let mut caps = vec![(0, 0); self.groups];
            if b.excepts.iter().any(|e| match_nodes(e, s)) {
                continue;
            }
            if walk(&b.nodes, s, 0, s.len(), &mut caps) {
                return Some(caps);
            }
        }
        None
    }

    /// True if the whole of `s` (plain bytes) matches.
    pub(crate) fn matches(&self, s: &[u8]) -> bool {
        self.branches.iter().any(|b| b.matches(s)) != self.negate
    }

    /// Lengths of every prefix of `s` that matches, shortest first.
    pub(crate) fn prefix_lengths(&self, s: &[u8]) -> Vec<usize> {
        (0..=s.len())
            .filter(|&n| s.get(..n).is_some_and(|p| self.matches(p)))
            .collect()
    }

    /// True if the pattern has no special characters.
    pub(crate) fn is_literal(&self) -> bool {
        !self.negate
            && self.branches.len() == 1
            && self
                .branches
                .iter()
                .all(|b| b.excepts.is_empty() && b.nodes.iter().all(|n| matches!(n, Node::Lit(_))))
    }
}

/// True if `pat` contains a pattern token. `#` and `^` are operators only
/// with EXTENDED_GLOB, so `extended` says whether they count.
pub(crate) fn has_wildcards(pat: &[u8], extended: bool) -> bool {
    let mut i = 0;
    while let Some(&c) = pat.get(i) {
        if c == META || c == BNULL {
            i += 2;
            continue;
        }
        if matches!(c, STAR | QUEST | INPAR | INANG) || (extended && matches!(c, POUND | HAT)) {
            return true;
        }
        // A `[` is a pattern only with a `]` to close it: `[` alone, the
        // test command, is a word.
        if c == INBRACK && pat.get(i + 2..).is_some_and(|r| r.contains(&OUTBRACK)) {
            return true;
        }
        i += 1;
    }
    false
}

/// Match `nodes` against the whole of `s` from `at`, writing down where each
/// group landed. The first whole-string path wins, and `*` is tried at its
/// longest first, so a group takes as much as it can -- zsh's rule.
fn walk(nodes: &[Node], s: &[u8], at: usize, end: usize, caps: &mut [(usize, usize)]) -> bool {
    let Some((first, rest)) = nodes.split_first() else {
        return at == end;
    };
    let tail = s.get(at..end).unwrap_or(&[]);
    if let Node::Group(alts, index) = first {
        for alt in alts {
            for n in (0..=tail.len()).rev() {
                let Some(part) = tail.get(..n) else { continue };
                if alt.excepts.iter().any(|e| match_nodes(e, part)) {
                    continue;
                }
                let saved = caps.to_vec();
                if let Some(slot) = caps.get_mut(*index) {
                    *slot = (at, at + n);
                }
                if walk(&alt.nodes, s, at, at + n, caps) && walk(rest, s, at + n, end, caps) {
                    return true;
                }
                caps.copy_from_slice(&saved);
            }
        }
        return false;
    }
    let mut lengths = match_one(first, tail);
    lengths.reverse();
    for n in lengths {
        let saved = caps.to_vec();
        if walk(rest, s, at + n, end, caps) {
            return true;
        }
        caps.copy_from_slice(&saved);
    }
    false
}

fn parse_alts(
    pat: &[u8],
    i: &mut usize,
    extended: bool,
    nested: bool,
    groups: &mut usize,
) -> (Vec<Branch>, bool) {
    let mut alts = vec![Branch::default()];
    while let Some(&c) = pat.get(*i) {
        *i += 1;
        let node = match c {
            META => {
                let n = pat.get(*i).copied().unwrap_or(0) ^ 32;
                *i += 1;
                Node::Lit(n)
            }
            BNULL => {
                let n = pat.get(*i).copied().unwrap_or(b'\\');
                *i += 1;
                Node::Lit(n)
            }
            STAR => Node::Star,
            QUEST => Node::Any,
            INBRACK => parse_class(pat, i),
            INPAR => {
                let index = *groups;
                *groups += 1;
                Node::Group(parse_alts(pat, i, extended, true, groups).0, index)
            }
            // A `|` that reached here as a token is always alternation, at
            // the top of the pattern as much as inside a group: a literal
            // one was quoted, and quoting keeps a character out of the
            // tokens altogether.
            BAR => {
                alts.push(Branch::default());
                continue;
            }
            // `x~y`: what follows is what this alternative must not match.
            // A `~` with nothing after it is the character itself, which is
            // how zsh reads a trailing one.
            TILDE | b'~'
                if extended
                    && pat
                        .get(*i)
                        .is_some_and(|&n| n != BAR && !(nested && n == OUTPAR)) =>
            {
                if let Some(a) = alts.last_mut() {
                    a.excepts.push(Vec::new());
                }
                continue;
            }
            OUTPAR if nested => return (alts, true),
            INANG => parse_range(pat, i),
            POUND if extended => {
                let two = pat.get(*i) == Some(&POUND);
                if two {
                    *i += 1;
                }
                if let Some(last) = alts.last_mut().and_then(|a| a.target().pop()) {
                    Node::Repeat(Box::new(last), two)
                } else {
                    Node::Lit(b'#')
                }
            }
            _ => Node::Lit(crate::tok::detok(c)),
        };
        if let Some(a) = alts.last_mut() {
            a.target().push(node);
        }
    }
    (alts, false)
}

fn parse_range(pat: &[u8], i: &mut usize) -> Node {
    let start = *i;
    let mut lo = String::new();
    let mut hi = String::new();
    let mut seen_dash = false;
    while let Some(&c) = pat.get(*i) {
        *i += 1;
        match c {
            OUTANG => {
                return Node::Range(lo.parse().ok(), hi.parse().ok());
            }
            b'-' | DASH => seen_dash = true,
            d if d.is_ascii_digit() => {
                if seen_dash {
                    hi.push(char::from(d))
                } else {
                    lo.push(char::from(d))
                }
            }
            _ => break,
        }
    }
    *i = start;
    Node::Lit(b'<')
}

fn parse_class(pat: &[u8], i: &mut usize) -> Node {
    let start = *i;
    let mut neg = false;
    if matches!(
        pat.get(*i),
        Some(&BANG) | Some(&HAT) | Some(&b'!') | Some(&b'^')
    ) {
        neg = true;
        *i += 1;
    }
    let mut items = Vec::new();
    let mut named = Vec::new();
    let mut first = true;
    loop {
        let Some(&c) = pat.get(*i) else {
            // No closing bracket: a literal `[`.
            *i = start;
            return Node::Lit(b'[');
        };
        if c == OUTBRACK && !first {
            *i += 1;
            break;
        }
        first = false;
        if c == INBRACK && pat.get(*i + 1) == Some(&b':') {
            let rest = pat.get(*i + 2..).unwrap_or(&[]);
            if let Some(end) = rest.windows(2).position(|w| w == [b':', OUTBRACK]) {
                named.push(rest.get(..end).unwrap_or(&[]).to_vec());
                *i += 2 + end + 2;
                continue;
            }
        }
        let raw = if c == BNULL || c == META {
            *i += 1;
            let n = pat.get(*i).copied().unwrap_or(0);
            if c == META { n ^ 32 } else { n }
        } else {
            crate::tok::detok(c)
        };
        let (lo, len) = char_at(pat.get(*i..).unwrap_or(&[])).map_or((u32::from(raw), 1), |x| {
            if raw == crate::tok::detok(c) && c < 0x80 {
                (u32::from(raw), 1)
            } else {
                x
            }
        });
        *i += len;
        if matches!(pat.get(*i), Some(&DASH) | Some(&b'-'))
            && pat.get(*i + 1).is_some_and(|&n| n != OUTBRACK)
        {
            *i += 1;
            let (hi, hl) = char_at(pat.get(*i..).unwrap_or(&[])).unwrap_or((lo, 1));
            *i += hl;
            items.push((lo, hi));
        } else {
            items.push((lo, lo));
        }
    }
    Node::Class { neg, items, named }
}

fn class_named(name: &[u8], ch: u32) -> bool {
    let Some(c) = char::from_u32(ch) else {
        return false;
    };
    match name {
        b"alpha" => c.is_alphabetic(),
        b"digit" => c.is_ascii_digit(),
        b"alnum" => c.is_alphanumeric(),
        b"upper" => c.is_uppercase(),
        b"lower" => c.is_lowercase(),
        b"space" => c.is_whitespace(),
        b"blank" => c == ' ' || c == '\t',
        b"punct" => c.is_ascii_punctuation(),
        b"xdigit" => c.is_ascii_hexdigit(),
        b"cntrl" => c.is_control(),
        b"print" => !c.is_control(),
        b"graph" => !c.is_control() && c != ' ',
        b"IDENT" | b"WORD" => c.is_alphanumeric() || c == '_',
        _ => false,
    }
}

fn match_one(node: &Node, s: &[u8]) -> Vec<usize> {
    match node {
        Node::Lit(b) => {
            if s.first() == Some(b) {
                vec![1]
            } else {
                vec![]
            }
        }
        Node::Any => char_at(s).map(|(_, l)| vec![l]).unwrap_or_default(),
        Node::Class { neg, items, named } => {
            let Some((ch, l)) = char_at(s) else {
                return vec![];
            };
            let hit = items.iter().any(|&(lo, hi)| lo <= ch && ch <= hi)
                || named.iter().any(|n| class_named(n, ch));
            if hit != *neg { vec![l] } else { vec![] }
        }
        Node::Range(lo, hi) => {
            let digits = s.iter().take_while(|b| b.is_ascii_digit()).count();
            (1..=digits)
                .filter(|&n| {
                    std::str::from_utf8(s.get(..n).unwrap_or(&[]))
                        .ok()
                        .and_then(|t| t.parse::<u64>().ok())
                        .is_some_and(|v| lo.is_none_or(|l| v >= l) && hi.is_none_or(|h| v <= h))
                })
                .collect()
        }
        Node::Star => (0..=s.len()).collect(),
        Node::Group(alts, _) => {
            let mut out = Vec::new();
            for alt in alts {
                for n in 0..=s.len() {
                    if s.get(..n).is_some_and(|p| alt.matches(p)) {
                        out.push(n);
                    }
                }
            }
            out
        }
        Node::Repeat(inner, at_least_one) => {
            let mut reached = vec![0usize];
            let mut frontier = vec![0usize];
            while let Some(pos) = frontier.pop() {
                for l in match_one(inner, s.get(pos..).unwrap_or(&[])) {
                    let np = pos + l;
                    if l > 0 && !reached.contains(&np) {
                        reached.push(np);
                        frontier.push(np);
                    }
                }
            }
            if *at_least_one {
                reached.retain(|&p| p > 0 || match_one(inner, &[]).contains(&0));
            }
            reached
        }
    }
}

fn match_nodes(nodes: &[Node], s: &[u8]) -> bool {
    let Some((first, rest)) = nodes.split_first() else {
        return s.is_empty();
    };
    if let Node::Lit(b) = first {
        return s.first() == Some(b) && match_nodes(rest, s.get(1..).unwrap_or(&[]));
    }
    if matches!(first, Node::Star) && rest.is_empty() {
        return true;
    }
    match_one(first, s)
        .into_iter()
        .any(|n| s.get(n..).is_some_and(|tail| match_nodes(rest, tail)))
}

/// Make the glob characters of `p` into the tokens `compile` reads. A pattern
/// written inside `${...}` reaches the expansion untokenized, because the
/// lexer read it as the body of a substitution rather than as a word; a
/// backslash there protects the character after it.
pub(crate) fn tokenize(p: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(p.len());
    let mut i = 0;
    while let Some(&b) = p.get(i) {
        i += 1;
        if b == b'\\' {
            if let Some(&next) = p.get(i) {
                out.push(next);
                i += 1;
            }
            continue;
        }
        out.push(match b {
            b'*' => STAR,
            b'?' => QUEST,
            b'[' => INBRACK,
            b']' => OUTBRACK,
            b'(' => INPAR,
            b')' => OUTPAR,
            b'|' => BAR,
            b'#' => POUND,
            b'^' => HAT,
            b'~' => TILDE,
            _ => b,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokd(p: &str) -> Vec<u8> {
        tokenize(p.as_bytes())
    }

    #[test]
    fn globs_match_like_zsh() {
        let m = |p: &str, s: &str| Pattern::compile(&tokd(p), true).matches(s.as_bytes());
        assert!(m("fer*", "ferrix"));
        assert!(!m("fer*", "xferrix"));
        assert!(m("?a[b-d]", "xac"));
        assert!(m("[^a]x", "bx"));
        assert!(m("(foo|bar)baz", "barbaz"));
        assert!(m("a#b", "aaab"));
        assert!(m("a##b", "ab"));
        assert!(!m("a##b", "b"));
        assert!(m("^foo", "bar"));
        assert!(m("*.txt", "a.txt"));
    }

    /// EXTENDED_GLOB's `~`, checked against what zsh 5.9 answers.
    #[test]
    fn the_except_operator_takes_matches_away() {
        let m = |p: &str, s: &str| Pattern::compile(&tokd(p), true).matches(s.as_bytes());
        assert!(m("abc", "abc"));
        assert!(m("*~*x", "abc"));
        assert!(!m("*~abc", "abc"));
        assert!(m("*~(x|y)", "abc"));
        // `|` binds looser than `~`: this is `(a*~ab)` or `ac`.
        assert!(!m("a*~ab|ac", "ab"));
        assert!(m("a*~ab|ac", "ac"));
        assert!(m("a*~ab|ac", "ad"));
        // A chain takes each operand away in turn.
        assert!(!m("a*~*b*~*c*", "abc"));
        assert!(!m("a*~*b*~*c*", "axc"));
        assert!(m("a*~*b*~*c*", "axy"));
        // The backup and compiled files vcs_info's own glob leaves out.
        assert!(m("*~*(\\~|.zwc)", "VCS_INFO_get_data_git"));
        assert!(!m("*~*(\\~|.zwc)", "VCS_INFO_get_data_git~"));
        assert!(!m("*~*(\\~|.zwc)", "VCS_INFO_get_data_git.zwc"));
        // A `~` with nothing after it is the character itself.
        assert!(m("*~", "f~"));
        assert!(!m("*~", "foo"));
        // Without EXTENDED_GLOB it is always the character.
        let plain = |p: &str, s: &str| Pattern::compile(&tokd(p), false).matches(s.as_bytes());
        assert!(plain("*~*x", "a~bx"));
        assert!(!plain("*~*x", "abx"));
    }
}
