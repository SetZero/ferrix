//! `zstyle`: the context-sensitive style database, following zsh 5.9's
//! `Src/Modules/zutil.c`.
//!
//! A style is a name with values, defined for the contexts matching a
//! pattern. A lookup names a context, and the most specific pattern that
//! matches it answers. Specificity is a weight over the pattern's characters,
//! zutil.c's: a `*` is worth 2, a `:` 3, and anything else 4, so a pattern
//! that spells out what another leaves open comes first.
//!
//! oh-my-zsh needs this to be real rather than a no-op: `vcs_info` asks
//! whether its debug styles are set, and a `zstyle` that answers yes to
//! everything makes it print its whole trace into the middle of the prompt.

use crate::pattern::Pattern;
use crate::shell::{Shell, Value};
use crate::tok;

/// One pattern's values for one style.
#[derive(Debug)]
pub(crate) struct StylePat {
    /// The pattern as it was written, which `-L` prints back.
    pat: Vec<u8>,
    prog: Pattern,
    weight: u32,
    /// `-e`: the value is code to run at each lookup, whose `reply` answers.
    eval: bool,
    vals: Vec<Vec<u8>>,
}

/// One style name and every pattern defined for it, most specific first.
#[derive(Debug)]
pub(crate) struct Style {
    name: Vec<u8>,
    pats: Vec<StylePat>,
}

/// zutil.c's specificity: `*` 2, `:` 3, anything else 4.
fn weight(pat: &[u8]) -> u32 {
    pat.iter()
        .map(|&c| match c {
            b'*' => 2,
            b':' => 3,
            _ => 4,
        })
        .sum()
}

/// Define `pattern style vals...`, replacing that pattern's values.
fn define(sh: &mut Shell, pat: &[u8], name: &[u8], vals: Vec<Vec<u8>>, eval: bool) {
    let entry = StylePat {
        pat: pat.to_vec(),
        prog: Pattern::compile(&crate::pattern::tokenize(pat), sh.opt("extendedglob")),
        weight: weight(pat),
        eval,
        vals,
    };
    let style = match sh.styles.iter().position(|s| s.name == name) {
        Some(i) => sh.styles.get_mut(i),
        None => {
            sh.styles.push(Style {
                name: name.to_vec(),
                pats: Vec::new(),
            });
            sh.styles.last_mut()
        }
    };
    let Some(style) = style else { return };
    style.pats.retain(|p| p.pat != pat);
    // Equal weights keep the order they were defined in, as zsh's insertion
    // before the first lighter pattern does.
    let at = style
        .pats
        .iter()
        .position(|p| p.weight < entry.weight)
        .unwrap_or(style.pats.len());
    style.pats.insert(at, entry);
}

/// The values `style` has for `context`, or `None` if none is defined.
fn lookup(sh: &mut Shell, context: &[u8], name: &[u8]) -> Option<Vec<Vec<u8>>> {
    let plain = tok::unmetafy(context);
    let hit = sh
        .styles
        .iter()
        .find(|s| s.name == name)?
        .pats
        .iter()
        .find(|p| p.prog.matches(&plain))?;
    if !hit.eval {
        return Some(hit.vals.clone());
    }
    // `-e`: run the value and take `reply`, which is how zsh evaluates one.
    let code = hit.vals.first().cloned().unwrap_or_default();
    let saved = sh.status;
    crate::exec::run_string(sh, &code);
    sh.status = saved;
    Some(match sh.get(b"reply") {
        Some(Value::Array(a)) => a,
        Some(Value::Scalar(s)) => vec![s],
        _ => Vec::new(),
    })
}

/// True if the string is one of the words zsh counts as true.
fn is_true(v: &[u8]) -> bool {
    matches!(tok::unmetafy(v).as_slice(), b"yes" | b"true" | b"on" | b"1")
}

/// Quote a word for `-L`, the way zsh prints a definition back.
fn quote(w: &[u8]) -> Vec<u8> {
    let plain = |c: u8| c.is_ascii_alphanumeric() || b":_./,@%+=-".contains(&c);
    if !w.is_empty() && w.iter().all(|&c| plain(c)) {
        return w.to_vec();
    }
    let mut out = vec![b'\''];
    for &c in w {
        if c == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
    out
}

/// The styles in the order zsh shows them, which is its hash table's: by
/// name, not by when they were defined.
fn by_name(sh: &Shell) -> Vec<&Style> {
    let mut styles: Vec<&Style> = sh.styles.iter().collect();
    styles.sort_by(|a, b| a.name.cmp(&b.name));
    styles
}

/// `-L`: every definition as a command that would make it again.
fn list(sh: &Shell, pat: Option<&[u8]>, name: Option<&[u8]>, long: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for style in by_name(sh) {
        if name.is_some_and(|n| n != style.name) {
            continue;
        }
        let mut shown = false;
        for p in &style.pats {
            if pat.is_some_and(|w| w != p.pat) {
                continue;
            }
            if long {
                out.extend_from_slice(b"zstyle ");
                if p.eval {
                    out.extend_from_slice(b"-e ");
                }
                out.extend(quote(&p.pat));
                out.push(b' ');
                out.extend(quote(&style.name));
            } else {
                if !shown {
                    out.extend_from_slice(&style.name);
                    out.push(b'\n');
                    shown = true;
                }
                out.extend_from_slice(b"        ");
                out.extend_from_slice(&p.pat);
            }
            for v in &p.vals {
                out.push(b' ');
                out.extend(if long { quote(v) } else { v.clone() });
            }
            out.push(b'\n');
        }
    }
    out
}

/// `-d`: forget a pattern's styles, a style, or the lot.
fn delete(sh: &mut Shell, args: &[Vec<u8>]) {
    let Some(pat) = args.first() else {
        sh.styles.clear();
        return;
    };
    let names = args.get(1..).unwrap_or(&[]);
    for style in &mut sh.styles {
        if !names.is_empty() && !names.iter().any(|n| *n == style.name) {
            continue;
        }
        style.pats.retain(|p| p.pat != *pat);
    }
    sh.styles.retain(|s| !s.pats.is_empty());
}

/// `-g name [ pattern [ style ] ]`: the patterns, the styles a pattern has,
/// or one definition's values.
fn get(sh: &Shell, args: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    let Some(pat) = args.first() else {
        let mut pats: Vec<Vec<u8>> = Vec::new();
        for style in &sh.styles {
            for p in &style.pats {
                if !pats.contains(&p.pat) {
                    pats.push(p.pat.clone());
                }
            }
        }
        return Some(pats);
    };
    match args.get(1) {
        None => Some(
            by_name(sh)
                .into_iter()
                .filter(|s| s.pats.iter().any(|p| p.pat == *pat))
                .map(|s| s.name.clone())
                .collect(),
        ),
        Some(name) => sh
            .styles
            .iter()
            .find(|s| s.name == *name)?
            .pats
            .iter()
            .find(|p| p.pat == *pat)
            .map(|p| p.vals.clone()),
    }
}

/// Run the builtin. `args` is everything after the name.
pub(crate) fn run(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let first = args.first().map(Vec::as_slice).unwrap_or(b"");
    let flag = match first {
        [b'-', f] if b"LedgabstTm".contains(f) => Some(*f),
        _ => None,
    };
    let rest = if flag.is_some() {
        args.get(1..).unwrap_or(&[])
    } else {
        args
    };
    let arg = |i: usize| rest.get(i).map(Vec::as_slice);

    // Listing: `zstyle`, `zstyle -L [pattern [style]]`.
    if args.is_empty() || flag == Some(b'L') {
        let text = list(sh, arg(0), arg(1), flag == Some(b'L'));
        return write_out(sh, &text);
    }

    match flag {
        Some(b'd') => {
            delete(sh, rest);
            0
        }
        Some(b'g') => {
            let Some(name) = arg(0) else {
                sh.error_at("zstyle", "not enough arguments");
                return 1;
            };
            let name = name.to_vec();
            match get(sh, rest.get(1..).unwrap_or(&[])) {
                Some(vals) => {
                    sh.set_value(&name, Value::Array(vals));
                    0
                }
                None => {
                    sh.set_value(&name, Value::Array(Vec::new()));
                    1
                }
            }
        }
        // The lookups: context, style, and what to do with the values.
        Some(f @ (b'a' | b'b' | b's' | b't' | b'T' | b'm')) => lookup_op(sh, f, rest),
        // A definition: pattern, style, values.
        _ => {
            let (Some(pat), Some(name)) = (arg(0), arg(1)) else {
                sh.error_at("zstyle", "not enough arguments");
                return 1;
            };
            let (pat, name) = (pat.to_vec(), name.to_vec());
            let vals = rest.get(2..).unwrap_or(&[]).to_vec();
            define(sh, &pat, &name, vals, flag == Some(b'e'));
            0
        }
    }
}

/// `-a -b -s -t -T -m`, which all begin with a context and a style.
fn lookup_op(sh: &mut Shell, flag: u8, rest: &[Vec<u8>]) -> i32 {
    let wanted = if matches!(flag, b't' | b'T') { 2 } else { 3 };
    if rest.len() < wanted {
        sh.error_at("zstyle", "not enough arguments");
        return 1;
    }
    let Some(context) = rest.first().cloned() else {
        return 1;
    };
    let Some(name) = rest.get(1).cloned() else {
        return 1;
    };
    let vals = lookup(sh, &context, &name);

    match flag {
        b'a' | b'b' | b's' => {
            let Some(target) = rest.get(2).cloned() else {
                return 1;
            };
            let found = vals.is_some();
            let vals = vals.unwrap_or_default();
            let value = match flag {
                b'a' => Value::Array(vals),
                b'b' => Value::Scalar(if vals.first().is_some_and(|v| is_true(v)) {
                    b"yes".to_vec()
                } else {
                    b"no".to_vec()
                }),
                // `-s` joins with the separator, a space unless one is given.
                _ => {
                    let sep = rest.get(3).cloned().unwrap_or_else(|| b" ".to_vec());
                    Value::Scalar(vals.join(sep.as_slice()))
                }
            };
            sh.set_value(&target, value);
            i32::from(!found)
        }
        b't' | b'T' => {
            let Some(vals) = vals else {
                // An undefined style is false for `-t` and true for `-T`,
                // which is what `-T` is for.
                return if flag == b'T' { 0 } else { 2 };
            };
            let strings = rest.get(2..).unwrap_or(&[]);
            let hit = if strings.is_empty() {
                vals.first().is_some_and(|v| is_true(v))
            } else {
                vals.iter().any(|v| strings.contains(v))
            };
            i32::from(!hit)
        }
        // `-m`: does any value match the pattern?
        _ => {
            let Some(vals) = vals else { return 1 };
            let Some(pat) = rest.get(2) else { return 1 };
            let prog = Pattern::compile(&crate::pattern::tokenize(pat), sh.opt("extendedglob"));
            i32::from(!vals.iter().any(|v| prog.matches(&tok::unmetafy(v))))
        }
    }
}

fn write_out(sh: &Shell, bytes: &[u8]) -> i32 {
    let _ = sh;
    i32::from(!crate::exec::write_fd(1, &tok::unmetafy(bytes)))
}
