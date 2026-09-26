//! Parameter expansion: `$name` and `${...}` with zsh's flags, subscripts,
//! operators and modifiers (a subset of zsh's `paramsubst`).

use crate::expand::{expand_pattern, expand_single};
use crate::pattern::Pattern;
use crate::shell::{Shell, Value};
use crate::tok::{self, INBRACE, INPAR, OUTBRACE, OUTPAR, QSTRING, STRING};

/// The result of one parameter expansion.
#[derive(Debug, Clone)]
pub(crate) struct Expansion {
    pub(crate) value: Value,
    /// Array elements become separate fields.
    pub(crate) splat: bool,
    /// RC_EXPAND_PARAM: every element takes a copy of the word around it,
    /// which a `^` before the name asks for and the option makes the rule.
    pub(crate) rc: bool,
    /// `${=spec}` split these fields at `$IFS`, so an empty one among them
    /// is a word of its own rather than an expansion that came to nothing.
    pub(crate) split: bool,
}

/// `$name`.
pub(crate) fn expand_simple(sh: &mut Shell, name: &[u8], dq: bool) -> Result<Expansion, String> {
    let value = sh.get(name).unwrap_or(Value::Scalar(Vec::new()));
    Ok(Expansion {
        value,
        splat: !dq || name == b"@",
        rc: sh.opt("rcexpandparam"),
        split: false,
    })
}

#[derive(Debug, Default)]
struct Flags {
    splat: bool,
    join: Option<Vec<u8>>,
    split: Option<Vec<u8>>,
    lines: bool,
    join_lines: bool,
    words: bool,
    upper: bool,
    lower: bool,
    capital: bool,
    keys: bool,
    values: bool,
    sort: bool,
    rsort: bool,
    numeric: bool,
    unique: bool,
    /// `(M)` reverses `${array:#pattern}`: retain matching elements.
    matching: bool,
    /// `${=spec}`: split the result into fields at `$IFS`, whether or not
    /// the expansion is quoted and whatever `SH_WORD_SPLIT` says.
    force_split: bool,
    quote: u8,
    unquote: bool,
    indirect: bool,
    /// Set by `^`, cleared by `^^`; unset leaves the option to decide.
    rc: Option<bool>,
}

fn parse_flags(s: &[u8], f: &mut Flags) {
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        i += 1;
        let c = tok::detok(c);
        let mut arg = || {
            let d = s.get(i).copied().unwrap_or(b':');
            let d = tok::detok(d);
            let start = i + 1;
            let end = s
                .get(start..)
                .and_then(|r| r.iter().position(|&x| tok::detok(x) == d))
                .map_or(s.len(), |p| start + p);
            i = end + 1;
            let raw: Vec<u8> = s
                .get(start..end)
                .unwrap_or(&[])
                .iter()
                .map(|&x| tok::detok(x))
                .collect();
            tok::metafy(&crate::expand::dollar_quote(&raw))
        };
        match c {
            b'@' => f.splat = true,
            b'j' => f.join = Some(arg()),
            b's' => f.split = Some(arg()),
            b'f' => f.lines = true,
            b'F' => f.join_lines = true,
            b'z' => f.words = true,
            b'U' => f.upper = true,
            b'L' => f.lower = true,
            b'C' => f.capital = true,
            b'k' => f.keys = true,
            b'v' => f.values = true,
            b'o' => f.sort = true,
            b'O' => f.rsort = true,
            b'n' => f.numeric = true,
            b'u' => f.unique = true,
            b'M' => f.matching = true,
            b'q' => f.quote += 1,
            b'Q' => f.unquote = true,
            b'P' => f.indirect = true,
            b'l' | b'r' => {
                let _pad = arg();
            }
            _ => {}
        }
    }
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || (c >= 0x80 && !tok::is_tok(c))
}

fn char_len(s: &[u8]) -> usize {
    std::str::from_utf8(s).map_or(s.len(), |t| t.chars().count())
}

/// `(#b)`: write down where each group of `pat` matched `s`, in the three
/// arrays zsh leaves behind -- `$match` with the text, `$mbegin` and `$mend`
/// with the offsets, counted from one and both ends inclusive.
///
/// The offsets are in bytes. zsh counts characters, so a pattern reaching
/// past a multibyte character reports a smaller number there; nothing in
/// oh-my-zsh does arithmetic on them.
pub(crate) fn set_backrefs(sh: &mut Shell, pat: &Pattern, value: &[u8]) {
    let plain = tok::unmetafy(value);
    let Some(caps) = pat.captures(&plain) else {
        return;
    };
    let mut text = Vec::new();
    let mut begin = Vec::new();
    let mut end = Vec::new();
    for (from, to) in caps {
        text.push(tok::metafy(plain.get(from..to).unwrap_or(&[])));
        begin.push((from + 1).to_string().into_bytes());
        end.push(to.to_string().into_bytes());
    }
    sh.set_value(b"match", Value::Array(text));
    sh.set_value(b"mbegin", Value::Array(begin));
    sh.set_value(b"mend", Value::Array(end));
}

/// `${...}`, given the text between the braces.
#[expect(
    clippy::too_many_lines,
    reason = "zsh's paramsubst is one long procedure too"
)]
pub(crate) fn expand_brace(sh: &mut Shell, inner: &[u8], dq: bool) -> Result<Expansion, String> {
    let mut f = Flags::default();
    let mut i = 0;
    // Inside double quotes the lexer leaves `(` and `)` as they are, so the
    // flags open with either, as they do in zsh's `paramsubst`.
    if inner.first().is_some_and(|&c| tok::detok(c) == b'(') {
        let end = inner
            .iter()
            .position(|&c| tok::detok(c) == b')')
            .unwrap_or(inner.len());
        parse_flags(inner.get(1..end).unwrap_or(&[]), &mut f);
        i = end + 1;
    }
    let (mut length, mut isset) = (false, false);
    loop {
        let c = inner.get(i).map(|&c| tok::detok(c));
        let next = inner.get(i + 1).copied();
        match c {
            Some(b'#')
                if next.is_some()
                    && next != Some(OUTBRACE)
                    && !matches!(
                        next.map(tok::detok),
                        Some(b':' | b'-' | b'=' | b'+' | b'?' | b'#' | b'%' | b'/')
                    ) =>
            {
                length = true;
                i += 1;
            }
            Some(b'+')
                if next.is_some_and(|n| {
                    is_ident_byte(n) || n == STRING || n == QSTRING || tok::detok(n) == b'@'
                }) =>
            {
                isset = true;
                i += 1;
            }
            // `^` asks for RC_EXPAND_PARAM and `^^` refuses it.
            Some(b'^') if next.is_some() => {
                let off = next.map(tok::detok) == Some(b'^');
                f.rc = Some(!off);
                i += if off { 2 } else { 1 };
            }
            Some(b'=') if next.is_some() => {
                f.force_split = true;
                i += 1;
            }
            Some(b'~') if next.is_some() => i += 1,
            _ => break,
        }
    }
    // The name, or a nested expansion.
    let mut name: Vec<u8> = Vec::new();
    let mut value: Option<Value> = None;
    match inner.get(i).copied() {
        Some(STRING | QSTRING) if inner.get(i + 1) == Some(&INBRACE) => {
            let end = close_of(inner, i + 2, INBRACE, OUTBRACE);
            let nested = inner.get(i + 2..end).unwrap_or(&[]).to_vec();
            value = Some(expand_brace(sh, &nested, dq)?.value);
            i = end + 1;
        }
        Some(STRING | QSTRING) if inner.get(i + 1) == Some(&INPAR) => {
            let end = close_of(inner, i + 2, INPAR, OUTPAR);
            let cmd = tok::remove_nulls(inner.get(i + 2..end).unwrap_or(&[]));
            let mut out = tok::metafy(&crate::exec::capture(sh, &cmd));
            while out.last() == Some(&b'\n') {
                let _nl = out.pop();
            }
            value = Some(Value::Scalar(out));
            i = end + 1;
        }
        Some(tok::DNULL) => {
            // `${"$(cmd)":-x}`: a quoted nested word.
            let end = inner
                .get(i + 1..)
                .and_then(|r| r.iter().position(|&b| b == tok::DNULL))
                .map_or(inner.len(), |p| i + 1 + p);
            let w = inner
                .get(i..=end.min(inner.len().saturating_sub(1)))
                .unwrap_or(&[])
                .to_vec();
            value = Some(Value::Scalar(expand_single(sh, &w)?));
            i = end + 1;
        }
        Some(c) if c.is_ascii_digit() => {
            while inner.get(i).is_some_and(u8::is_ascii_digit) {
                name.push(inner.get(i).copied().unwrap_or(b'0'));
                i += 1;
            }
        }
        Some(c) if is_ident_byte(c) => {
            while inner.get(i).is_some_and(|&c| is_ident_byte(c)) {
                name.push(inner.get(i).copied().unwrap_or(b'_'));
                i += 1;
            }
        }
        Some(c) if b"@*#?$!-0".contains(&tok::detok(c)) => {
            name.push(tok::detok(c));
            i += 1;
        }
        _ => {}
    }
    let mut splat = f.splat || !dq || name == b"@";
    // Subscript. The brackets are tokenized or plain, depending on where the
    // word they came from was quoted.
    let subscripted = inner.get(i).is_some_and(|&c| tok::detok(c) == b'[');
    if value.is_none() && subscripted && !f.indirect && !name.is_empty() {
        // A named parameter's element is read where the parameter is kept,
        // rather than out of a copy of all of it.
        let end = close_sub(inner, i + 1);
        let sub = inner.get(i + 1..end).unwrap_or(&[]).to_vec();
        i = end + 1;
        let (v, whole) = subscript_named(sh, &name, &sub)?;
        if whole {
            splat = !dq || matches!(sub.first().map(|&c| tok::detok(c)), Some(b'@'));
        }
        value = v;
    } else {
        if value.is_none() {
            let mut v = if name.is_empty() { None } else { sh.get(&name) };
            if f.indirect
                && let Some(target) = v.take()
            {
                v = sh.get(&target.joined());
            }
            let set = v.is_some();
            value = v;
            if !set && isset {
                return Ok(Expansion {
                    value: Value::Scalar(b"0".to_vec()),
                    splat: false,
                    rc: false,
                    split: false,
                });
            }
        }
        if subscripted {
            let end = close_sub(inner, i + 1);
            let sub = inner.get(i + 1..end).unwrap_or(&[]).to_vec();
            i = end + 1;
            let (v, whole) = subscript(sh, value.take(), &sub)?;
            if whole {
                splat = !dq || matches!(sub.first().map(|&c| tok::detok(c)), Some(b'@'));
            }
            value = v;
        }
    }
    if isset {
        return Ok(Expansion {
            value: Value::Scalar(if value.is_some() { b"1" } else { b"0" }.to_vec()),
            splat: false,
            rc: false,
            split: false,
        });
    }
    let is_set = value.is_some();
    let mut v = value.unwrap_or(Value::Scalar(Vec::new()));
    if f.keys || f.values {
        if let Value::Assoc(pairs) = &v {
            let mut out = Vec::new();
            for (k, val) in pairs {
                if f.keys {
                    out.push(k.clone());
                }
                if f.values {
                    out.push(val.clone());
                }
            }
            v = Value::Array(out);
        }
    } else if let Value::Assoc(pairs) = &v {
        v = Value::Array(pairs.iter().map(|(_, x)| x.clone()).collect());
    }
    // Operators.
    let rest = inner.get(i..).unwrap_or(&[]).to_vec();
    v = apply_op(sh, v, is_set, &name, &rest, f.matching)?;
    if length {
        let n = match &v {
            Value::Array(a) => a.len(),
            other => char_len(&other.joined()),
        };
        return Ok(Expansion {
            value: Value::Scalar(n.to_string().into_bytes()),
            splat: false,
            rc: false,
            split: false,
        });
    }
    v = map_elems(v, |s| {
        if f.upper {
            s.to_ascii_uppercase()
        } else if f.lower {
            s.to_ascii_lowercase()
        } else if f.capital {
            capitalize(&s)
        } else {
            s
        }
    });
    if let Some(sep) = &f.join {
        v = Value::Scalar(to_vec(v).join(sep.as_slice()));
    } else if f.join_lines {
        v = Value::Scalar(to_vec(v).join(&b'\n'));
    }
    if let Some(sep) = &f.split {
        let s = v.joined();
        v = Value::Array(split_on(&s, sep));
        splat = splat || !dq;
    } else if f.lines {
        v = Value::Array(split_on(&v.joined(), b"\n"));
    } else if f.words {
        v = Value::Array(
            v.joined()
                .split(|c| c.is_ascii_whitespace())
                .filter(|w| !w.is_empty())
                .map(<[u8]>::to_vec)
                .collect(),
        );
    }
    // `${=spec}`, after everything that decides what the words are: each of
    // them is split at IFS, and the result is more than one word even inside
    // quotes, which is the whole point of asking.
    if f.force_split {
        let ifs = crate::expand::ifs(sh);
        let mut fields = Vec::new();
        for word in to_vec(v) {
            fields.extend(crate::expand::split_ifs(&word, &ifs));
        }
        v = Value::Array(fields);
        splat = true;
    }
    if f.sort || f.rsort || f.unique {
        let mut a = to_vec(v);
        if f.unique {
            let mut seen = std::collections::HashSet::new();
            a.retain(|x| seen.insert(x.clone()));
        }
        if f.sort || f.rsort {
            if f.numeric {
                a.sort_by_key(|x| {
                    std::str::from_utf8(x)
                        .ok()
                        .and_then(|t| t.parse::<i64>().ok())
                        .unwrap_or(0)
                });
            } else {
                a.sort();
            }
            if f.rsort {
                a.reverse();
            }
        }
        v = Value::Array(a);
    }
    if f.quote > 0 {
        v = map_elems(v, |s| quote(&s, f.quote));
    }
    if f.unquote {
        v = map_elems(v, |s| unquote(&s));
    }
    Ok(Expansion {
        value: v,
        splat,
        rc: f.rc.unwrap_or_else(|| sh.opt("rcexpandparam")),
        split: f.force_split,
    })
}

/// The `]` closing a subscript that opened just before `i`, counting nested
/// brackets. Either bracket may be tokenized or plain, so both count.
fn close_sub(s: &[u8], mut i: usize) -> usize {
    let mut depth = 1;
    while let Some(&c) = s.get(i) {
        match tok::detok(c) {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

fn close_of(s: &[u8], mut i: usize, open: u8, close: u8) -> usize {
    let mut depth = 1;
    while let Some(&c) = s.get(i) {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    s.len()
}

fn to_vec(v: Value) -> Vec<Vec<u8>> {
    match v {
        Value::Array(a) => a,
        Value::Scalar(s) => vec![s],
        Value::Assoc(p) => p.into_iter().map(|(_, x)| x).collect(),
    }
}

fn map_elems(v: Value, mut f: impl FnMut(Vec<u8>) -> Vec<u8>) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.into_iter().map(f).collect()),
        Value::Scalar(s) => Value::Scalar(f(s)),
        other => other,
    }
}

fn split_on(s: &[u8], sep: &[u8]) -> Vec<Vec<u8>> {
    if sep.is_empty() {
        return s.iter().map(|&c| vec![c]).collect();
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + sep.len() <= s.len() {
        if s.get(i..i + sep.len()) == Some(sep) {
            out.push(s.get(start..i).unwrap_or(&[]).to_vec());
            i += sep.len();
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(s.get(start..).unwrap_or(&[]).to_vec());
    out.retain(|x| !x.is_empty());
    out
}

fn capitalize(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut start = true;
    for &c in s {
        if c.is_ascii_alphanumeric() {
            out.push(if start {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            });
            start = false;
        } else {
            out.push(c);
            start = true;
        }
    }
    out
}

/// Quote a word for reuse as shell input, only when it needs it.
pub(crate) fn quote_word(s: &[u8]) -> Vec<u8> {
    if !s.is_empty()
        && s.iter()
            .all(|&c| c.is_ascii_alphanumeric() || b"_-./=:,+@%".contains(&c))
    {
        return s.to_vec();
    }
    quote(s, 2)
}

fn quote(s: &[u8], level: u8) -> Vec<u8> {
    if level >= 2 {
        let mut out = vec![b'\''];
        for &c in s {
            if c == b'\'' {
                out.extend_from_slice(b"'\\''");
            } else {
                out.push(c);
            }
        }
        out.push(b'\'');
        return out;
    }
    let mut out = Vec::new();
    for &c in s {
        if b" \t\n\\'\"$`*?[]{}()<>|&;~#^!=".contains(&c) {
            out.push(b'\\');
        }
        out.push(c);
    }
    out
}

fn unquote(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut q = 0u8;
    while let Some(&c) = s.get(i) {
        i += 1;
        match c {
            b'\'' | b'"' if q == 0 => q = c,
            b'\'' | b'"' if q == c => q = 0,
            b'\\' if q != b'\'' => {
                if let Some(&n) = s.get(i) {
                    out.push(n);
                    i += 1;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A subscript with its words expanded: what it picks out of a value, apart
/// from the value it will pick it out of.
///
/// Evaluating a subscript may run anything -- a command substitution, an
/// assignment in arithmetic -- and so needs the shell to itself, while
/// applying one only reads. Keeping the two apart is what lets an element be
/// read where its parameter is kept ([`Shell::stored`]) instead of out of a
/// copy of the whole array or hash, which is what every `$_comps[$name]`
/// compinit asks cost until they were.
enum Sub {
    /// `(r)`, `(i)`, `(k)` and the rest: the flags, the pattern, and, for a
    /// hash's `(k)`, the key to compare with exactly.
    Flags {
        flags: Vec<u8>,
        pat: Pattern,
        literal: Vec<u8>,
    },
    /// A hash's key.
    Key(Vec<u8>),
    /// An array's or a string's index, or the range `lo,hi`.
    Index(i64, Option<i64>),
}

/// Whether a subscript is `[@]` or `[*]`, the whole array.
fn is_whole(sub: &[u8]) -> bool {
    matches!(sub, [c] if matches!(tok::detok(*c), b'@' | b'*'))
}

/// Evaluate a subscript for a value that is a hash (`assoc`) or is not.
fn eval_subscript(sh: &mut Shell, sub: &[u8], assoc: bool) -> Result<Sub, String> {
    let plain: Vec<u8> = sub.iter().map(|&c| tok::detok(c)).collect();
    // Subscript flags: (r) (R) (i) (I) (k) (K).
    if plain.first() == Some(&b'(')
        && let Some(close) = plain.iter().position(|&c| c == b')')
    {
        let flags = plain.get(1..close).unwrap_or(&[]).to_vec();
        let pat_word = sub.get(close + 1..).unwrap_or(&[]).to_vec();
        let pat = Pattern::compile(
            &expand_pattern(sh, &crate::pattern::tokenize(&pat_word))?,
            sh.opt("extendedglob"),
        );
        let literal = if assoc && flags.contains(&b'k') {
            expand_single(sh, &pat_word)?
        } else {
            Vec::new()
        };
        return Ok(Sub::Flags {
            flags,
            pat,
            literal,
        });
    }
    if assoc {
        return Ok(Sub::Key(expand_single(sh, sub)?));
    }
    let text = expand_single(sh, sub)?;
    let (a, b) = match text.iter().position(|&c| c == b',') {
        Some(p) => (
            text.get(..p).unwrap_or(&[]).to_vec(),
            Some(text.get(p + 1..).unwrap_or(&[]).to_vec()),
        ),
        None => (text, None),
    };
    let lo = crate::arith::eval(sh, &a)?;
    let hi = match b {
        Some(b) => Some(crate::arith::eval(sh, &b)?),
        None => None,
    };
    Ok(Sub::Index(lo, hi))
}

/// Apply an evaluated subscript to `v`. Returns what it picked and whether
/// that is the whole array; only a flag that answers with every match is.
fn apply_subscript(v: &Value, s: &Sub) -> (Option<Value>, bool) {
    match s {
        Sub::Flags {
            flags,
            pat,
            literal,
        } => {
            // A hash reads these flags differently from an array, and zsh's
            // own `colors` depends on it: `i`/`I` match the *keys* and give
            // keys back, `r`/`R` match the values and give values, `k`/`K`
            // look a key up. The capital of each pair answers with every
            // match rather than the first, so `${color[(I)fg-*]}` is every
            // colour name there is.
            if let Value::Assoc(pairs) = v {
                let keys = flags.contains(&b'i') || flags.contains(&b'I');
                let exact = flags.contains(&b'k');
                let by_key = keys || exact || flags.contains(&b'K');
                let every = flags.contains(&b'I') || flags.contains(&b'R') || flags.contains(&b'K');
                let mut found: Vec<Vec<u8>> = Vec::new();
                for (key, value) in pairs {
                    let subject = if by_key { key } else { value };
                    let hit = if exact {
                        subject == literal
                    } else {
                        pat.matches(&tok::unmetafy(subject))
                    };
                    if hit {
                        found.push(if keys { key.clone() } else { value.clone() });
                        if !every {
                            break;
                        }
                    }
                }
                return if every {
                    (Some(Value::Array(found)), true)
                } else {
                    (
                        Some(Value::Scalar(found.into_iter().next().unwrap_or_default())),
                        false,
                    )
                };
            }
            // An array's keys are its indices, counted from one; a string is
            // an array of the one element.
            let elems: &[Vec<u8>] = match v {
                Value::Array(a) => a,
                Value::Scalar(s) => std::slice::from_ref(s),
                Value::Assoc(_) => &[],
            };
            let reverse = flags.contains(&b'R') || flags.contains(&b'I');
            let want_index = flags.contains(&b'i') || flags.contains(&b'I');
            let by_key = flags.contains(&b'k') || flags.contains(&b'K');
            let index = |n: usize| (n + 1).to_string().into_bytes();
            let hit = |&(n, x): &(usize, &Vec<u8>)| {
                if by_key {
                    pat.matches(&tok::unmetafy(&index(n)))
                } else {
                    pat.matches(&tok::unmetafy(x))
                }
            };
            let found = if reverse {
                elems.iter().enumerate().rev().find(hit)
            } else {
                elems.iter().enumerate().find(hit)
            };
            let text = match found {
                Some((n, x)) => {
                    if want_index {
                        index(n)
                    } else {
                        x.clone()
                    }
                }
                None if want_index => {
                    if reverse {
                        b"0".to_vec()
                    } else {
                        (elems.len() + 1).to_string().into_bytes()
                    }
                }
                None => Vec::new(),
            };
            (Some(Value::Scalar(text)), false)
        }
        Sub::Key(key) => match v {
            Value::Assoc(pairs) => (
                pairs
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, x)| Value::Scalar(x.clone())),
                false,
            ),
            // A key is only ever evaluated for a hash.
            _ => (None, false),
        },
        Sub::Index(lo, hi) => {
            let chars: Vec<Vec<u8>>;
            let elems: &[Vec<u8>] = match v {
                Value::Array(x) => x,
                other => {
                    let s = other.joined();
                    chars = match std::str::from_utf8(&s) {
                        Ok(t) => t.chars().map(|c| c.to_string().into_bytes()).collect(),
                        Err(_) => s.iter().map(|&c| vec![c]).collect(),
                    };
                    &chars
                }
            };
            let n = i64::try_from(elems.len()).unwrap_or(i64::MAX);
            let norm = |x: i64| if x < 0 { n + x + 1 } else { x };
            let (lo, hi) = (norm(*lo), hi.map(norm));
            let pick = |from: i64, to: i64| -> Vec<Vec<u8>> {
                let from = from.max(1);
                (from..=to.min(n))
                    .filter_map(|k| {
                        usize::try_from(k - 1)
                            .ok()
                            .and_then(|k| elems.get(k))
                            .cloned()
                    })
                    .collect()
            };
            let is_array = matches!(v, Value::Array(_));
            (
                Some(match hi {
                    Some(hi) => {
                        let part = pick(lo, hi);
                        if is_array {
                            Value::Array(part)
                        } else {
                            Value::Scalar(part.concat())
                        }
                    }
                    None => Value::Scalar(pick(lo, lo).concat()),
                }),
                false,
            )
        }
    }
}

/// Apply a subscript to a value already in hand. Returns the new value and
/// whether it selected the whole array (`[@]`, `[*]`).
fn subscript(
    sh: &mut Shell,
    v: Option<Value>,
    sub: &[u8],
) -> Result<(Option<Value>, bool), String> {
    if is_whole(sub) {
        return Ok((v, true));
    }
    let Some(v) = v else { return Ok((None, false)) };
    let s = eval_subscript(sh, sub, matches!(v, Value::Assoc(_)))?;
    Ok(apply_subscript(&v, &s))
}

/// `${name[sub]}`: [`subscript`] for the parameter `name`, reading the
/// element where the parameter is kept rather than out of a copy of it, and
/// one element of `$commands` or `$functions` without listing the rest.
fn subscript_named(
    sh: &mut Shell,
    name: &[u8],
    sub: &[u8],
) -> Result<(Option<Value>, bool), String> {
    if is_whole(sub) {
        return Ok((sh.get(name), true));
    }
    if Shell::is_special(name) {
        if matches!(name, b"commands" | b"functions" | b"aliases" | b"galiases") {
            let s = eval_subscript(sh, sub, true)?;
            if let Sub::Key(key) = &s
                && let Some(found) = sh.special_element(name, key)
            {
                return Ok((found.map(Value::Scalar), false));
            }
            return Ok(sh
                .get(name)
                .map_or((None, false), |v| apply_subscript(&v, &s)));
        }
        let v = sh.get(name);
        return subscript(sh, v, sub);
    }
    let assoc = match sh.stored(name) {
        Some(v) => matches!(v, Value::Assoc(_)),
        None => return Ok((None, false)),
    };
    let s = eval_subscript(sh, sub, assoc)?;
    Ok(sh
        .stored(name)
        .map_or((None, false), |v| apply_subscript(v, &s)))
}

#[expect(clippy::too_many_lines, reason = "one arm per operator")]
fn apply_op(
    sh: &mut Shell,
    v: Value,
    is_set: bool,
    name: &[u8],
    rest: &[u8],
    matching: bool,
) -> Result<Value, String> {
    let Some(&first) = rest.first() else {
        return Ok(v);
    };
    let c0 = tok::detok(first);
    let c1 = rest.get(1).map(|&c| tok::detok(c));
    let empty = match &v {
        Value::Array(a) => a.is_empty(),
        other => other.joined().is_empty(),
    };
    // For an array, `${name:#pattern}` removes matching elements rather than
    // removing text from each element. `(M)` keeps the matches instead. This
    // is how compaudit partitions its candidate directory list.
    if c0 == b':' && c1 == Some(b'#') {
        let pat_word = rest.get(2..).unwrap_or(&[]).to_vec();
        let pat = Pattern::compile(
            &expand_pattern(sh, &crate::pattern::tokenize(&pat_word))?,
            sh.opt("extendedglob"),
        );
        let hit = |s: &[u8]| pat.matches(&tok::unmetafy(s));
        // `(#b)` remembers where the groups landed. vcs_info names its
        // backends this way: `: ${file:#(#b)VCS_INFO_get_data_(*)}`, whose
        // whole purpose is the `$match` it leaves behind.
        if pat.has_backrefs()
            && let Some(first) = match &v {
                Value::Array(a) => a.iter().find(|s| hit(s)).cloned(),
                Value::Assoc(a) => a.iter().map(|(_, x)| x).find(|s| hit(s)).cloned(),
                Value::Scalar(s) => Some(s.clone()).filter(|s| hit(s)),
            }
        {
            set_backrefs(sh, &pat, &first);
        }
        return Ok(match v {
            Value::Array(a) => Value::Array(a.into_iter().filter(|s| hit(s) == matching).collect()),
            Value::Assoc(a) => Value::Assoc(
                a.into_iter()
                    .filter(|(_, value)| hit(value) == matching)
                    .collect(),
            ),
            Value::Scalar(s) => {
                if hit(&s) == matching {
                    Value::Scalar(s)
                } else {
                    Value::Scalar(Vec::new())
                }
            }
        });
    }
    // `${a:|b}` and `${a:*b}`: the elements of `a` that are absent from, or
    // present in, the array named by `b`.
    if c0 == b':' && matches!(c1, Some(b'|' | b'*')) {
        let keep = c1 == Some(b'*');
        let other = expand_single(sh, rest.get(2..).unwrap_or(&[]))?;
        let theirs = match sh.get(&other) {
            Some(Value::Array(a)) => a,
            Some(one) => vec![one.joined()],
            None => Vec::new(),
        };
        let mine = to_vec(v);
        return Ok(Value::Array(
            mine.into_iter()
                .filter(|x| theirs.contains(x) == keep)
                .collect(),
        ));
    }
    let (colon, op, word_at) = if c0 == b':' && matches!(c1, Some(b'-' | b'=' | b'+' | b'?')) {
        (true, c1.unwrap_or(b'-'), 2)
    } else if matches!(c0, b'-' | b'=' | b'+' | b'?') {
        (false, c0, 1)
    } else {
        (false, 0, 0)
    };
    if op != 0 {
        let word = rest.get(word_at..).unwrap_or(&[]).to_vec();
        let missing = !is_set || (colon && empty);
        return Ok(match op {
            b'-' => {
                if missing {
                    Value::Scalar(expand_single(sh, &word)?)
                } else {
                    v
                }
            }
            b'=' => {
                if missing {
                    let w = expand_single(sh, &word)?;
                    sh.set_scalar(name, w.clone());
                    Value::Scalar(w)
                } else {
                    v
                }
            }
            b'+' => {
                if missing {
                    Value::Scalar(Vec::new())
                } else {
                    Value::Scalar(expand_single(sh, &word)?)
                }
            }
            _ => {
                if missing {
                    let w = expand_single(sh, &word)?;
                    let msg = if w.is_empty() {
                        b"parameter not set".to_vec()
                    } else {
                        w
                    };
                    return Err(format!(
                        "{}: {}",
                        String::from_utf8_lossy(name),
                        String::from_utf8_lossy(&msg)
                    ));
                }
                v
            }
        });
    }
    match c0 {
        b'#' | b'%' => {
            let longest = c1 == Some(c0);
            let pat_word = rest
                .get(if longest { 2 } else { 1 }..)
                .unwrap_or(&[])
                .to_vec();
            let pat = Pattern::compile(
                &expand_pattern(sh, &crate::pattern::tokenize(&pat_word))?,
                sh.opt("extendedglob"),
            );
            Ok(map_elems(v, |s| {
                remove_match(&pat, &s, c0 == b'#', longest)
            }))
        }
        b'/' => {
            let (all, anchor, start) = match c1 {
                Some(b'/') => (true, 0u8, 2),
                Some(b'#') => (false, b'#', 2),
                Some(b'%') => (false, b'%', 2),
                _ => (false, 0, 1),
            };
            let body = rest.get(start..).unwrap_or(&[]);
            let mut split = body.len();
            let mut k = 0;
            while let Some(&c) = body.get(k) {
                // A quoted byte, and a backslash-escaped one, are both two
                // bytes that cannot be the separator: `${r/refs\/heads\//x}`
                // replaces one path prefix, it does not match `refs\`.
                if c == tok::BNULL || c == b'\\' {
                    k += 2;
                    continue;
                }
                if c == b'/' {
                    split = k;
                    break;
                }
                k += 1;
            }
            let pat_word = body.get(..split).unwrap_or(&[]).to_vec();
            let repl_word = body.get(split + 1..).unwrap_or(&[]).to_vec();
            let pat = Pattern::compile(
                &expand_pattern(sh, &crate::pattern::tokenize(&pat_word))?,
                sh.opt("extendedglob"),
            );
            let repl = expand_single(sh, &repl_word)?;
            Ok(map_elems(v, |s| replace(&pat, &s, &repl, all, anchor)))
        }
        b':' => {
            let after = rest.get(1..).unwrap_or(&[]);
            let a0 = after.first().map(|&c| tok::detok(c));
            if a0.is_some_and(|c| {
                c.is_ascii_digit() || c == b' ' || c == b'-' || c == b'(' || c == b'$'
            }) {
                return substring(sh, v, after);
            }
            apply_modifiers(v, after)
        }
        _ => Ok(v),
    }
}

fn substring(sh: &mut Shell, v: Value, spec: &[u8]) -> Result<Value, String> {
    let text = expand_single(sh, spec)?;
    let (a, b) = match text.iter().position(|&c| c == b':') {
        Some(p) => (
            text.get(..p).unwrap_or(&[]).to_vec(),
            Some(text.get(p + 1..).unwrap_or(&[]).to_vec()),
        ),
        None => (text, None),
    };
    let off = crate::arith::eval(sh, &a)?;
    let len = match b {
        Some(b) => Some(crate::arith::eval(sh, &b)?),
        None => None,
    };
    let cut = |elems: Vec<Vec<u8>>| -> Vec<Vec<u8>> {
        let n = i64::try_from(elems.len()).unwrap_or(0);
        let start = if off < 0 {
            (n + off).max(0)
        } else {
            off.min(n)
        };
        let end = match len {
            Some(l) if l < 0 => (n + l).max(start),
            Some(l) => (start + l).min(n),
            None => n,
        };
        elems
            .into_iter()
            .skip(usize::try_from(start).unwrap_or(0))
            .take(usize::try_from(end - start).unwrap_or(0))
            .collect()
    };
    Ok(match v {
        Value::Array(a) => Value::Array(cut(a)),
        other => {
            let s = other.joined();
            let chars: Vec<Vec<u8>> = match std::str::from_utf8(&s) {
                Ok(t) => t.chars().map(|c| c.to_string().into_bytes()).collect(),
                Err(_) => s.iter().map(|&c| vec![c]).collect(),
            };
            Value::Scalar(cut(chars).concat())
        }
    })
}

pub(crate) fn apply_modifiers(v: Value, spec: &[u8]) -> Result<Value, String> {
    let plain: Vec<u8> = spec.iter().map(|&c| tok::detok(c)).collect();
    let mut v = v;
    let mut i = 0;
    while let Some(&m) = plain.get(i) {
        i += 1;
        match m {
            b'h' => {
                v = map_elems(v, |s| match s.iter().rposition(|&c| c == b'/') {
                    Some(0) => b"/".to_vec(),
                    Some(p) => s.get(..p).unwrap_or(&[]).to_vec(),
                    None => b".".to_vec(),
                })
            }
            b't' => {
                v = map_elems(v, |s| {
                    s.rsplit(|&c| c == b'/').next().unwrap_or(&[]).to_vec()
                })
            }
            b'r' => {
                v = map_elems(v, |s| {
                    let slash = s.iter().rposition(|&c| c == b'/').map_or(0, |p| p + 1);
                    match s.iter().rposition(|&c| c == b'.') {
                        Some(p) if p > slash => s.get(..p).unwrap_or(&[]).to_vec(),
                        _ => s,
                    }
                })
            }
            b'e' => {
                v = map_elems(v, |s| {
                    let slash = s.iter().rposition(|&c| c == b'/').map_or(0, |p| p + 1);
                    match s.iter().rposition(|&c| c == b'.') {
                        Some(p) if p >= slash => s.get(p + 1..).unwrap_or(&[]).to_vec(),
                        _ => Vec::new(),
                    }
                })
            }
            b'l' => v = map_elems(v, |s| s.to_ascii_lowercase()),
            b'u' => v = map_elems(v, |s| s.to_ascii_uppercase()),
            b'a' | b'A' => {
                v = map_elems(v, |s| {
                    std::fs::canonicalize(String::from_utf8_lossy(&tok::unmetafy(&s)).as_ref())
                        .map(|p| tok::metafy(p.to_string_lossy().as_bytes()))
                        .unwrap_or(s)
                })
            }
            b'q' => v = map_elems(v, |s| quote(&s, 1)),
            b'Q' => v = map_elems(v, |s| unquote(&s)),
            b'g' | b's' => {
                let global = m == b'g';
                if global {
                    i += 1;
                }
                let d = plain.get(i).copied().unwrap_or(b'/');
                let rest = plain.get(i + 1..).unwrap_or(&[]);
                let p1 = rest.iter().position(|&c| c == d).unwrap_or(rest.len());
                let from = rest.get(..p1).unwrap_or(&[]).to_vec();
                let rest2 = rest.get(p1 + 1..).unwrap_or(&[]);
                let p2 = rest2.iter().position(|&c| c == d).unwrap_or(rest2.len());
                let to = rest2.get(..p2).unwrap_or(&[]).to_vec();
                i += 1 + p1 + 1 + p2 + 1;
                v = map_elems(v, |s| literal_replace(&s, &from, &to, global));
            }
            b':' => {}
            _ => return Ok(v),
        }
    }
    Ok(v)
}

fn literal_replace(s: &[u8], from: &[u8], to: &[u8], global: bool) -> Vec<u8> {
    if from.is_empty() {
        return s.to_vec();
    }
    let mut out = Vec::new();
    let mut i = 0;
    let mut done = false;
    while i < s.len() {
        if !done && s.get(i..i + from.len()) == Some(from) {
            out.extend_from_slice(to);
            i += from.len();
            done = !global;
        } else {
            out.push(s.get(i).copied().unwrap_or(0));
            i += 1;
        }
    }
    out
}

fn remove_match(pat: &Pattern, s: &[u8], front: bool, longest: bool) -> Vec<u8> {
    let raw = tok::unmetafy(s);
    let n = raw.len();
    let mut cands: Vec<usize> = (0..=n).collect();
    if longest {
        cands.reverse();
    }
    for k in cands {
        let (part, keep) = if front {
            (raw.get(..k), raw.get(k..))
        } else {
            (raw.get(n - k..), raw.get(..n - k))
        };
        if part.is_some_and(|p| pat.matches(p)) {
            return tok::metafy(keep.unwrap_or(&[]));
        }
    }
    s.to_vec()
}

fn replace(pat: &Pattern, s: &[u8], repl: &[u8], all: bool, anchor: u8) -> Vec<u8> {
    let raw = tok::unmetafy(s);
    let repl = tok::unmetafy(repl);
    let n = raw.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut replaced = false;
    while i <= n {
        if (anchor == b'#' && i > 0) || (replaced && !all) {
            break;
        }
        let ends: Vec<usize> = if anchor == b'%' {
            vec![n]
        } else {
            (i..=n).rev().collect()
        };
        let hit = ends
            .into_iter()
            .find(|&e| (e > i || anchor != 0) && raw.get(i..e).is_some_and(|p| pat.matches(p)));
        if let Some(e) = hit {
            out.extend_from_slice(&repl);
            replaced = true;
            if e == i {
                if let Some(&c) = raw.get(i) {
                    out.push(c);
                }
                i += 1;
            } else {
                i = e;
            }
            continue;
        }
        if i < n {
            out.push(raw.get(i).copied().unwrap_or(0));
        }
        i += 1;
    }
    if i <= n {
        out.extend_from_slice(raw.get(i.min(n)..).unwrap_or(&[]));
    }
    tok::metafy(&out)
}
