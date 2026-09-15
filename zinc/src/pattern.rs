//! Pattern matching on tokenized patterns: `*`, `?`, `[...]`, `(a|b)`, and
//! with EXTENDED_GLOB `#`, `##` and `^`. A plain byte is literal, which is how
//! quoting reaches the matcher: quoted characters were never tokenized.
//!
//! A subset of zsh's `pattern.c`; the rest (`(#i)`, `(#b)`, `~`, `<a-b>`
//! ranges) lands with the expansion milestone.

use crate::tok::{BANG, BAR, BNULL, DASH, HAT, INANG, INBRACK, INPAR, META, OUTANG, OUTBRACK};
use crate::tok::{OUTPAR, POUND, QUEST, STAR};

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
    Group(Vec<Vec<Node>>),
    /// `x#`: zero or more of the node; `x##`: one or more.
    Repeat(Box<Node>, bool),
    /// `<a-b>`: a decimal number in the range; bounds absent are open.
    Range(Option<u64>, Option<u64>),
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub(crate) struct Pattern {
    nodes: Vec<Node>,
    negate: bool,
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
        let bytes = crate::tok::unmetafy(pat);
        let _ = bytes;
        let (alts, _) = parse_alts(pat, &mut i, extended, false);
        let nodes = if alts.len() == 1 {
            alts.into_iter().next().unwrap_or_default()
        } else {
            vec![Node::Group(alts)]
        };
        Pattern { nodes, negate }
    }

    /// True if the whole of `s` (plain bytes) matches.
    pub(crate) fn matches(&self, s: &[u8]) -> bool {
        match_nodes(&self.nodes, s) != self.negate
    }

    /// Lengths of every prefix of `s` that matches, shortest first.
    pub(crate) fn prefix_lengths(&self, s: &[u8]) -> Vec<usize> {
        (0..=s.len())
            .filter(|&n| s.get(..n).is_some_and(|p| self.matches(p)))
            .collect()
    }

    /// True if the pattern has no special characters.
    pub(crate) fn is_literal(&self) -> bool {
        !self.negate && self.nodes.iter().all(|n| matches!(n, Node::Lit(_)))
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

fn parse_alts(pat: &[u8], i: &mut usize, extended: bool, nested: bool) -> (Vec<Vec<Node>>, bool) {
    let mut alts = vec![Vec::new()];
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
            INPAR => Node::Group(parse_alts(pat, i, extended, true).0),
            BAR if nested => {
                alts.push(Vec::new());
                continue;
            }
            OUTPAR if nested => return (alts, true),
            INANG => parse_range(pat, i),
            POUND if extended => {
                let two = pat.get(*i) == Some(&POUND);
                if two {
                    *i += 1;
                }
                if let Some(last) = alts.last_mut().and_then(Vec::pop) {
                    Node::Repeat(Box::new(last), two)
                } else {
                    Node::Lit(b'#')
                }
            }
            _ => Node::Lit(crate::tok::detok(c)),
        };
        if let Some(a) = alts.last_mut() {
            a.push(node);
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
        Node::Group(alts) => {
            let mut out = Vec::new();
            for alt in alts {
                for n in 0..=s.len() {
                    if s.get(..n).is_some_and(|p| match_nodes(alt, p)) {
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
}
