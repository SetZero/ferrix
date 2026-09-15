//! Word expansion: quotes, `$` substitutions, braces, `~`, globbing (the
//! parts of zsh's `subst.c` and `glob.c` a script needs first).

use crate::param;
use crate::pattern::{Pattern, has_wildcards};
use crate::shell::{Shell, Value};
use crate::tok::{self, BNULL, COMMA, DNULL, EQUALS, INBRACE, INBRACK, INPAR, INPARMATH, META};
use crate::tok::{
    OUTBRACE, OUTBRACK, OUTPAR, OUTPARMATH, QSTRING, QTICK, SNULL, STRING, TICK, TILDE,
};

/// How a word's result is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Command arguments: arrays give fields, globbing applies.
    Fields,
    /// One string: arrays joined, no globbing.
    Single,
    /// A pattern: wildcards stay tokens, quoted characters stay literal.
    Pattern,
}

/// A field under construction.
#[derive(Debug, Default, Clone)]
struct Field {
    bytes: Vec<u8>,
    glob: bool,
    quoted: bool,
    expanded: bool,
}

struct Out {
    fields: Vec<Field>,
    mode: Mode,
}

impl Out {
    fn cur(&mut self) -> &mut Field {
        if self.fields.is_empty() {
            self.fields.push(Field::default());
        }
        let n = self.fields.len() - 1;
        // The vector is non-empty here.
        match self.fields.get_mut(n) {
            Some(f) => f,
            None => unreachable_field(),
        }
    }

    fn push_plain(&mut self, b: &[u8]) {
        self.cur().bytes.extend_from_slice(b);
    }

    /// Insert expansion results: the first joins the current field, the rest
    /// start new ones.
    fn push_values(&mut self, vals: &[Vec<u8>], quoted: bool) {
        let cur = self.cur();
        cur.expanded = true;
        cur.quoted |= quoted;
        for (i, v) in vals.iter().enumerate() {
            if i > 0 {
                self.fields.push(Field {
                    expanded: true,
                    quoted,
                    ..Field::default()
                });
            }
            self.push_plain(v);
        }
        if vals.is_empty() && quoted {
            self.cur().quoted = true;
        }
    }
}

#[expect(
    clippy::panic,
    reason = "unreachable: called only after pushing a field"
)]
fn unreachable_field() -> &'static mut Field {
    panic!("expansion field vector empty after push")
}

/// Expand command words into arguments (metafied).
pub(crate) fn expand_words(sh: &mut Shell, words: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, String> {
    let mut args = Vec::new();
    for w in words {
        let braced = if sh.opt("ignorebraces") {
            vec![w.clone()]
        } else {
            brace_expand(w)
        };
        for b in braced {
            let fields = expand(sh, &b, Mode::Fields)?;
            for f in fields {
                if f.glob && sh.opt("glob") && has_wildcards(&f.bytes, sh.opt("extendedglob")) {
                    let (pat, nullglob) = strip_null_qualifier(&f.bytes);
                    let matches = glob(sh, &pat);
                    if matches.is_empty() {
                        if nullglob || sh.opt("nullglob") {
                            continue;
                        }
                        if sh.opt("nomatch") {
                            let mut shown = pat.clone();
                            tok::untokenize(&mut shown);
                            return Err(format!(
                                "no matches found: {}",
                                String::from_utf8_lossy(&tok::unmetafy(&shown))
                            ));
                        }
                    } else {
                        args.extend(matches);
                        continue;
                    }
                }
                if f.bytes.is_empty() && f.expanded && !f.quoted {
                    continue;
                }
                args.push(tok::remove_nulls(&f.bytes));
            }
        }
    }
    Ok(args)
}

/// Expand to one string (assignments, redirection targets, here-strings).
pub(crate) fn expand_single(sh: &mut Shell, w: &[u8]) -> Result<Vec<u8>, String> {
    let fields = expand(sh, w, Mode::Single)?;
    let joined: Vec<Vec<u8>> = fields
        .into_iter()
        .map(|f| tok::remove_nulls(&f.bytes))
        .collect();
    Ok(joined.join(&b' '))
}

/// Expand a pattern word, keeping wildcard tokens.
pub(crate) fn expand_pattern(sh: &mut Shell, w: &[u8]) -> Result<Vec<u8>, String> {
    let fields = expand(sh, w, Mode::Pattern)?;
    let joined: Vec<Vec<u8>> = fields.into_iter().map(|f| f.bytes).collect();
    Ok(joined.join(&b' '))
}

/// Remove a trailing `(N)` glob qualifier.
fn strip_null_qualifier(p: &[u8]) -> (Vec<u8>, bool) {
    for q in [&[INPAR, b'N', OUTPAR][..], &[INPAR, b'N', b'.', OUTPAR][..]] {
        if let Some(stem) = p.strip_suffix(q) {
            return (stem.to_vec(), true);
        }
    }
    (p.to_vec(), false)
}

/// Index of the byte closing the construct opened just before `i`, counting
/// `open`/`close` tokens.
fn find_close(w: &[u8], mut i: usize, open: u8, close: u8) -> usize {
    let mut depth = 1;
    while let Some(&c) = w.get(i) {
        if c == META {
            i += 2;
            continue;
        }
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
    w.len()
}

fn expand(sh: &mut Shell, w: &[u8], mode: Mode) -> Result<Vec<Field>, String> {
    let mut out = Out {
        fields: vec![Field::default()],
        mode,
    };
    walk(sh, w, &mut out, false)?;
    Ok(out.fields)
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per token, as in zsh's stringsubst"
)]
fn walk(sh: &mut Shell, w: &[u8], out: &mut Out, mut dq: bool) -> Result<(), String> {
    let mut i = 0;
    while let Some(&c) = w.get(i) {
        i += 1;
        match c {
            META => {
                let n = w.get(i).copied().unwrap_or(0);
                i += 1;
                out.push_plain(&[META, n]);
            }
            SNULL => {
                out.cur().quoted = true;
                while let Some(&q) = w.get(i) {
                    i += 1;
                    if q == SNULL {
                        break;
                    }
                    push_literal(out, q);
                }
            }
            DNULL => {
                dq = !dq;
                out.cur().quoted = true;
            }
            BNULL => {
                let n = w.get(i).copied().unwrap_or(b'\\');
                i += 1;
                out.cur().quoted = true;
                push_literal(out, n);
            }
            STRING | QSTRING => match w.get(i).copied() {
                Some(SNULL) => {
                    let end = w
                        .get(i + 1..)
                        .and_then(|r| r.iter().position(|&b| b == SNULL))
                        .map_or(w.len(), |p| i + 1 + p);
                    let body = w.get(i + 1..end).unwrap_or(&[]);
                    let text = dollar_quote(body);
                    out.cur().quoted = true;
                    for b in text {
                        push_literal(out, b);
                    }
                    i = end + 1;
                }
                Some(INPAR) => {
                    let end = find_close(w, i + 1, INPAR, OUTPAR);
                    let cmd = tok::remove_nulls(w.get(i + 1..end).unwrap_or(&[]));
                    i = end + 1;
                    let output = crate::exec::capture(sh, &cmd);
                    insert_output(sh, out, &output, dq);
                }
                Some(INPARMATH) => {
                    let end = find_close(w, i + 1, INPARMATH, OUTPARMATH);
                    let inner = w.get(i + 1..end).unwrap_or(&[]);
                    let inner = inner.strip_prefix(b"(").unwrap_or(inner);
                    let inner = inner.strip_suffix(b")").unwrap_or(inner);
                    i = end + 1;
                    let text = expand_single(sh, inner)?;
                    let v = crate::arith::eval(sh, &text)?;
                    out.push_values(&[v.to_string().into_bytes()], dq);
                }
                Some(INBRACK) => {
                    let end = find_close(w, i + 1, INBRACK, OUTBRACK);
                    let inner = w.get(i + 1..end).unwrap_or(&[]).to_vec();
                    i = end + 1;
                    let text = expand_single(sh, &inner)?;
                    let v = crate::arith::eval(sh, &text)?;
                    out.push_values(&[v.to_string().into_bytes()], dq);
                }
                Some(INBRACE) => {
                    let end = find_close(w, i + 1, INBRACE, OUTBRACE);
                    let inner = w.get(i + 1..end).unwrap_or(&[]).to_vec();
                    i = end + 1;
                    let r = param::expand_brace(sh, &inner, dq)?;
                    insert_param(out, r, dq);
                }
                // `$#name` and `$+name`, which zsh reads as `${#name}` and
                // `${+name}`: how long the parameter is, and whether it is
                // set at all. `$#` on its own counts the arguments instead.
                Some(p) if matches!(tok::detok(p), b'#' | b'+') && starts_name(w.get(i + 1)) => {
                    let (name, len) = simple_name(w.get(i + 1..).unwrap_or(&[]));
                    let mut inner = vec![tok::detok(p)];
                    inner.extend_from_slice(&name);
                    i = take_subscript(w, i + 1 + len, &mut inner);
                    let r = param::expand_brace(sh, &inner, dq)?;
                    insert_param(out, r, dq);
                }
                Some(n) if is_param_start(n) => {
                    let (name, len) = simple_name(w.get(i..).unwrap_or(&[]));
                    i += len;
                    let ident = name
                        .first()
                        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_');
                    // `$name[sub]` subscripts in native zsh, as `${name[sub]}`.
                    let mut inner = name.clone();
                    let after = if ident && !sh.opt("ksharrays") {
                        take_subscript(w, i, &mut inner)
                    } else {
                        i
                    };
                    let r = if after == i {
                        param::expand_simple(sh, &name, dq)?
                    } else {
                        i = after;
                        param::expand_brace(sh, &inner, dq)?
                    };
                    insert_param(out, r, dq);
                }
                _ => push_literal(out, b'$'),
            },
            TICK | QTICK => {
                let end = w
                    .get(i..)
                    .and_then(|r| r.iter().position(|&b| b == c))
                    .map_or(w.len(), |p| i + p);
                let cmd = tok::remove_nulls(w.get(i..end).unwrap_or(&[]));
                i = end + 1;
                let output = crate::exec::capture(sh, &cmd);
                insert_output(sh, out, &output, dq);
            }
            TILDE if i == 1 && !dq => {
                let end = w
                    .get(i..)
                    .and_then(|r| r.iter().position(|&b| b == b'/'))
                    .map_or(w.len(), |p| i + p);
                let user = w.get(i..end).unwrap_or(&[]);
                let dir = match user {
                    b"" => sh.get(b"HOME").map(|v| v.joined()),
                    b"+" => sh.get(b"PWD").map(|v| v.joined()),
                    b"-" => sh.get(b"OLDPWD").map(|v| v.joined()),
                    _ => None,
                };
                match dir {
                    Some(d) => {
                        out.cur().expanded = true;
                        out.cur().quoted = true;
                        out.push_plain(&d);
                        i = end;
                    }
                    None => out.push_plain(b"~"),
                }
            }
            EQUALS | COMMA | tok::DASH | tok::BANG | INBRACE | OUTBRACE | TILDE => {
                push_literal(out, tok::detok(c));
            }
            t if tok::is_tok(t) && !tok::is_null(t) => {
                if dq {
                    push_literal(out, tok::detok(t));
                } else {
                    out.cur().glob = true;
                    out.push_plain(&[t]);
                }
            }
            _ => push_literal(out, c),
        }
    }
    Ok(())
}

/// A literal byte: in patterns a character that would read as a wildcard
/// is escaped so it matches itself.
fn push_literal(out: &mut Out, b: u8) {
    if tok::is_meta(b) {
        out.push_plain(&[META, b ^ 32]);
    } else if out.mode != Mode::Single && b"*?[]()|#^<>~\\".contains(&b) {
        out.push_plain(&[BNULL, b]);
    } else {
        out.push_plain(&[b]);
    }
}

/// True if a parameter name starts at `c`.
fn starts_name(c: Option<&u8>) -> bool {
    c.is_some_and(|&c| {
        let c = tok::detok(c);
        c.is_ascii_alphabetic() || c == b'_'
    })
}

/// Read a `[subscript]` at `i`, appending it to `inner` as `${name[sub]}`
/// spells one, and answer where the word goes on. Brackets reach here
/// tokenized or plain, depending on where the word was quoted, so both count;
/// an opening bracket with no closing one is not a subscript at all.
fn take_subscript(w: &[u8], i: usize, inner: &mut Vec<u8>) -> usize {
    let (open, close) = match w.get(i) {
        Some(&INBRACK) => (INBRACK, OUTBRACK),
        Some(&b'[') => (b'[', b']'),
        _ => return i,
    };
    let end = find_close(w, i + 1, open, close);
    if end >= w.len() {
        return i;
    }
    inner.push(INBRACK);
    inner.extend_from_slice(w.get(i + 1..end).unwrap_or(&[]));
    inner.push(OUTBRACK);
    end + 1
}

fn is_param_start(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || b"_@*#?$!-".contains(&c)
        || c >= 0x80
        || matches!(c, STRING | tok::QUEST | tok::POUND | tok::STAR)
}

/// The name of `$name`, `$1`, `$?` at the start of `w`, and its length.
fn simple_name(w: &[u8]) -> (Vec<u8>, usize) {
    let Some(&first) = w.first() else {
        return (Vec::new(), 0);
    };
    let first = tok::detok(first);
    if first.is_ascii_digit() {
        return (vec![first], 1);
    }
    if first.is_ascii_alphabetic() || first == b'_' || first >= 0x80 && !tok::is_tok(first) {
        let n = w
            .iter()
            .take_while(|&&c| c.is_ascii_alphanumeric() || c == b'_')
            .count();
        return (w.get(..n).unwrap_or(&[]).to_vec(), n);
    }
    (vec![first], 1)
}

fn insert_param(out: &mut Out, r: param::Expansion, dq: bool) {
    match r.value {
        Value::Array(a) if r.splat => {
            let vals: Vec<Vec<u8>> = if dq {
                a
            } else {
                a.into_iter().filter(|v| !v.is_empty()).collect()
            };
            out.push_values(&vals, dq);
        }
        other => {
            let s = other.joined();
            out.push_values(&[s], dq);
        }
    }
}

/// Insert command output: trailing newlines removed; unquoted output is
/// split at IFS characters.
fn insert_output(sh: &Shell, out: &mut Out, raw: &[u8], dq: bool) {
    let mut text = tok::metafy(raw);
    while text.last() == Some(&b'\n') {
        let _nl = text.pop();
    }
    if dq || out.mode != Mode::Fields {
        out.push_values(&[text], true);
        return;
    }
    let ifs = sh
        .get(b"IFS")
        .map_or_else(|| b" \t\n".to_vec(), |v| v.joined());
    let words: Vec<Vec<u8>> = text
        .split(|b| ifs.contains(b))
        .filter(|s| !s.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    out.push_values(&words, false);
}

/// Interpret the escapes of `$'...'`.
pub(crate) fn dollar_quote(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&c) = body.get(i) {
        i += 1;
        if c == BNULL {
            if let Some(&n) = body.get(i) {
                out.push(n);
                i += 1;
            }
            continue;
        }
        if c != b'\\' {
            out.push(c);
            continue;
        }
        let Some(&e) = body.get(i) else {
            out.push(b'\\');
            break;
        };
        i += 1;
        let simple = match e {
            b'n' => Some(b'\n'),
            b't' => Some(b'\t'),
            b'r' => Some(b'\r'),
            b'a' => Some(7),
            b'b' => Some(8),
            b'e' | b'E' => Some(27),
            b'f' => Some(12),
            b'v' => Some(11),
            b'\\' => Some(b'\\'),
            b'\'' => Some(b'\''),
            b'"' => Some(b'"'),
            _ => None,
        };
        if let Some(s) = simple {
            out.push(s);
            continue;
        }
        let (radix, max) = match e {
            b'x' => (16, 2),
            b'u' => (16, 4),
            b'U' => (16, 8),
            b'0'..=b'7' => (8, 3),
            _ => {
                out.push(b'\\');
                out.push(e);
                continue;
            }
        };
        let start = if radix == 8 { i - 1 } else { i };
        let digits: Vec<u8> = body
            .get(start..)
            .unwrap_or(&[])
            .iter()
            .copied()
            .take(max)
            .take_while(|d| char::from(*d).is_digit(radix))
            .collect();
        i = start + digits.len();
        let n =
            u32::from_str_radix(std::str::from_utf8(&digits).unwrap_or("0"), radix).unwrap_or(0);
        if matches!(e, b'u' | b'U') {
            let mut buf = [0u8; 4];
            let ch = char::from_u32(n).unwrap_or('\u{fffd}');
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(u8::try_from(n & 0xff).unwrap_or(0));
        }
    }
    tok::metafy(&out)
}

/// Brace expansion: `x{a,b}y` and `{1..3}`, outermost first.
pub(crate) fn brace_expand(w: &[u8]) -> Vec<Vec<u8>> {
    let mut i = 0;
    while let Some(&c) = w.get(i) {
        match c {
            META | BNULL => {
                i += 2;
                continue;
            }
            SNULL | DNULL => {
                let end = w
                    .get(i + 1..)
                    .and_then(|r| r.iter().position(|&b| b == c))
                    .map_or(w.len(), |p| i + 1 + p);
                i = end + 1;
                continue;
            }
            STRING | QSTRING
                if matches!(
                    w.get(i + 1),
                    Some(&INBRACE) | Some(&INPAR) | Some(&INPARMATH)
                ) =>
            {
                let (open, close) = match w.get(i + 1) {
                    Some(&INBRACE) => (INBRACE, OUTBRACE),
                    Some(&INPAR) => (INPAR, OUTPAR),
                    _ => (INPARMATH, OUTPARMATH),
                };
                i = find_close(w, i + 2, open, close) + 1;
                continue;
            }
            INBRACE => {
                let end = find_close(w, i + 1, INBRACE, OUTBRACE);
                if end < w.len() {
                    let inner = w.get(i + 1..end).unwrap_or(&[]);
                    let pre = w.get(..i).unwrap_or(&[]);
                    let post = w.get(end + 1..).unwrap_or(&[]);
                    if let Some(parts) = brace_parts(inner) {
                        let mut out = Vec::new();
                        for p in parts {
                            let mut word = pre.to_vec();
                            word.extend_from_slice(&p);
                            word.extend_from_slice(post);
                            out.extend(brace_expand(&word));
                        }
                        return out;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    vec![w.to_vec()]
}

fn brace_parts(inner: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, &c) in inner.iter().enumerate() {
        if c == INBRACE {
            depth += 1;
        } else if c == OUTBRACE {
            depth -= 1;
        } else if c == COMMA && depth == 0 {
            parts.push(inner.get(start..i).unwrap_or(&[]).to_vec());
            start = i + 1;
        }
    }
    if !parts.is_empty() {
        parts.push(inner.get(start..).unwrap_or(&[]).to_vec());
        return Some(parts);
    }
    let plain: Vec<u8> = inner.iter().map(|&c| tok::detok(c)).collect();
    let text = std::str::from_utf8(&plain).ok()?;
    let mut it = text.split("..");
    let (a, b, step) = (it.next()?, it.next()?, it.next());
    let step: i64 = step
        .map_or(Some(1i64), |s| s.parse::<i64>().ok())?
        .abs()
        .max(1);
    if let (Ok(x), Ok(y)) = (a.parse::<i64>(), b.parse::<i64>()) {
        let width = if a.starts_with('0') || b.starts_with('0') {
            a.len().max(b.len())
        } else {
            0
        };
        let mut v = Vec::new();
        let mut n = x;
        loop {
            v.push(format!("{n:0width$}").into_bytes());
            if n == y {
                break;
            }
            n = if x <= y { n + step } else { n - step };
            if (x <= y && n > y) || (x > y && n < y) {
                break;
            }
        }
        return Some(v);
    }
    let (ca, cb) = (a.chars().next()?, b.chars().next()?);
    if a.chars().count() == 1 && b.chars().count() == 1 {
        let (lo, hi) = (u32::from(ca), u32::from(cb));
        let range: Vec<u32> = if lo <= hi {
            (lo..=hi).collect()
        } else {
            (hi..=lo).rev().collect()
        };
        return Some(
            range
                .into_iter()
                .filter_map(char::from_u32)
                .map(|c| c.to_string().into_bytes())
                .collect(),
        );
    }
    None
}

/// Glob a tokenized pattern against the file system, sorted.
fn glob(sh: &Shell, pat: &[u8]) -> Vec<Vec<u8>> {
    let raw = pat.to_vec();
    let absolute = raw.first() == Some(&b'/');
    let comps: Vec<&[u8]> = raw
        .split(|&c| c == b'/')
        .filter(|c| !c.is_empty())
        .collect();
    let mut paths: Vec<Vec<u8>> = vec![if absolute { b"/".to_vec() } else { Vec::new() }];
    let dotglob = sh.opt("globdots");
    let extended = sh.opt("extendedglob");
    for (n, comp) in comps.iter().enumerate() {
        let last = n + 1 == comps.len();
        let mut next = Vec::new();
        for base in &paths {
            if !has_wildcards(comp, extended) {
                let mut p = base.clone();
                if !p.is_empty() && p.last() != Some(&b'/') {
                    p.push(b'/');
                }
                p.extend_from_slice(&tok::remove_nulls(comp));
                if last
                    || std::path::Path::new(std::ffi::OsStr::new(&*String::from_utf8_lossy(
                        &tok::unmetafy(&p),
                    )))
                    .exists()
                {
                    next.push(p);
                }
                continue;
            }
            let pattern = Pattern::compile(comp, extended);
            let dir = if base.is_empty() {
                b".".to_vec()
            } else {
                tok::unmetafy(base)
            };
            let Ok(rd) = std::fs::read_dir(std::ffi::OsStr::new(&*String::from_utf8_lossy(&dir)))
            else {
                continue;
            };
            let mut names: Vec<Vec<u8>> = Vec::new();
            for e in rd.flatten() {
                use std::os::unix::ffi::OsStrExt;
                let name = e.file_name().as_bytes().to_vec();
                if name.first() == Some(&b'.') && !dotglob && comp.first() != Some(&b'.') {
                    continue;
                }
                if !last && !e.file_type().is_ok_and(|t| t.is_dir() || t.is_symlink()) {
                    continue;
                }
                let m = tok::metafy(&name);
                if pattern.matches(&name) {
                    names.push(m);
                }
            }
            names.sort();
            for name in names {
                let mut p = base.clone();
                if !p.is_empty() && p.last() != Some(&b'/') {
                    p.push(b'/');
                }
                p.extend_from_slice(&name);
                next.push(p);
            }
        }
        paths = next;
    }
    if comps.iter().all(|c| !has_wildcards(c, extended)) {
        return Vec::new();
    }
    paths
}
