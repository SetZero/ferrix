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
}

/// `$name`.
pub(crate) fn expand_simple(sh: &mut Shell, name: &[u8], dq: bool) -> Result<Expansion, String> {
    let value = sh.get(name).unwrap_or(Value::Scalar(Vec::new()));
    Ok(Expansion {
        value,
        splat: !dq || name == b"@",
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
    quote: u8,
    unquote: bool,
    indirect: bool,
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
            crate::expand::dollar_quote(&raw)
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

/// `${...}`, given the text between the braces.
#[expect(
    clippy::too_many_lines,
    reason = "zsh's paramsubst is one long procedure too"
)]
pub(crate) fn expand_brace(sh: &mut Shell, inner: &[u8], dq: bool) -> Result<Expansion, String> {
    let mut f = Flags::default();
    let mut i = 0;
    if inner.first() == Some(&INPAR) {
        let end = inner
            .iter()
            .position(|&c| c == OUTPAR)
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
            Some(b'=' | b'~' | b'^') if next.is_some() => i += 1,
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
            });
        }
    }
    let mut splat = f.splat || !dq || name == b"@";
    // Subscript. The brackets are tokenized or plain, depending on where the
    // word they came from was quoted.
    if inner.get(i).is_some_and(|&c| tok::detok(c) == b'[') {
        let end = close_sub(inner, i + 1);
        let sub = inner.get(i + 1..end).unwrap_or(&[]).to_vec();
        i = end + 1;
        let (v, whole) = subscript(sh, value.take(), &sub)?;
        if whole {
            splat = !dq || matches!(sub.first().map(|&c| tok::detok(c)), Some(b'@'));
        }
        value = v;
    }
    if isset {
        return Ok(Expansion {
            value: Value::Scalar(if value.is_some() { b"1" } else { b"0" }.to_vec()),
            splat: false,
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
    v = apply_op(sh, v, is_set, &name, &rest)?;
    if length {
        let n = match &v {
            Value::Array(a) => a.len(),
            other => char_len(&other.joined()),
        };
        return Ok(Expansion {
            value: Value::Scalar(n.to_string().into_bytes()),
            splat: false,
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
    Ok(Expansion { value: v, splat })
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

/// Apply a subscript. Returns the new value and whether it selected the
/// whole array (`[@]`, `[*]`).
fn subscript(
    sh: &mut Shell,
    v: Option<Value>,
    sub: &[u8],
) -> Result<(Option<Value>, bool), String> {
    let plain: Vec<u8> = sub.iter().map(|&c| tok::detok(c)).collect();
    if plain == b"@" || plain == b"*" {
        return Ok((v, true));
    }
    let Some(v) = v else { return Ok((None, false)) };
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
        let elems: Vec<(Vec<u8>, Vec<u8>)> = match &v {
            Value::Array(a) => a
                .iter()
                .enumerate()
                .map(|(n, x)| ((n + 1).to_string().into_bytes(), x.clone()))
                .collect(),
            Value::Assoc(p) => p.clone(),
            Value::Scalar(s) => vec![(b"1".to_vec(), s.clone())],
        };
        let reverse = flags.contains(&b'R') || flags.contains(&b'I');
        let want_index = flags.contains(&b'i') || flags.contains(&b'I');
        let by_key = flags.contains(&b'k') || flags.contains(&b'K');
        let mut it: Box<dyn Iterator<Item = &(Vec<u8>, Vec<u8>)>> = if reverse {
            Box::new(elems.iter().rev())
        } else {
            Box::new(elems.iter())
        };
        let found = it.find(|(k, x)| pat.matches(&tok::unmetafy(if by_key { k } else { x })));
        return Ok((
            Some(Value::Scalar(match found {
                Some((k, x)) => {
                    if want_index {
                        k.clone()
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
            })),
            false,
        ));
    }
    if let Value::Assoc(pairs) = &v {
        let key = expand_single(sh, sub)?;
        return Ok((
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, x)| Value::Scalar(x.clone())),
            false,
        ));
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
    let elems: Vec<Vec<u8>> = match &v {
        Value::Array(x) => x.clone(),
        other => {
            let s = other.joined();
            match std::str::from_utf8(&s) {
                Ok(t) => t.chars().map(|c| c.to_string().into_bytes()).collect(),
                Err(_) => s.iter().map(|&c| vec![c]).collect(),
            }
        }
    };
    let n = i64::try_from(elems.len()).unwrap_or(i64::MAX);
    let norm = |x: i64| if x < 0 { n + x + 1 } else { x };
    let (lo, hi) = (norm(lo), hi.map(norm));
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
    Ok((
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
    ))
}

#[expect(clippy::too_many_lines, reason = "one arm per operator")]
fn apply_op(
    sh: &mut Shell,
    v: Value,
    is_set: bool,
    name: &[u8],
    rest: &[u8],
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
                if c == tok::BNULL {
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
            modifiers(v, after)
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

fn modifiers(v: Value, spec: &[u8]) -> Result<Value, String> {
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
