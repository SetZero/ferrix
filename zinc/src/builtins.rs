//! Builtin commands. Arguments arrive expanded and metafied.

use crate::ast::{Assign, AssignValue};
use crate::exec::{self, write_fd};
use crate::lex::AliasDef;
use crate::shell::{Flow, Shell, Value, Var};
use crate::tok;

const BUILTINS: &[&[u8]] = &[
    b":",
    b".",
    b"[",
    b"alias",
    b"autoload",
    b"bg",
    b"bindkey",
    b"break",
    b"builtin",
    b"cd",
    b"chdir",
    b"command",
    b"compdef",
    b"continue",
    b"declare",
    b"echo",
    b"emulate",
    b"eval",
    b"exec",
    b"exit",
    b"export",
    b"false",
    b"fg",
    b"float",
    b"functions",
    b"getopts",
    b"hash",
    b"integer",
    b"jobs",
    b"kill",
    b"let",
    b"local",
    b"logout",
    b"print",
    b"printf",
    b"pwd",
    b"read",
    b"readonly",
    b"rehash",
    b"return",
    b"set",
    b"setopt",
    b"shift",
    b"source",
    b"test",
    b"trap",
    b"true",
    b"type",
    b"typeset",
    b"ulimit",
    b"umask",
    b"unalias",
    b"unfunction",
    b"unhash",
    b"unset",
    b"unsetopt",
    b"wait",
    b"whence",
    b"where",
    b"which",
    b"zle",
    b"zmodload",
    b"zstyle",
];

pub(crate) fn is_builtin(name: &[u8]) -> bool {
    BUILTINS.contains(&name)
}

fn out(sh: &Shell, fd: i32, bytes: &[u8]) -> i32 {
    let _ = sh;
    i32::from(!write_fd(fd, &tok::unmetafy(bytes)))
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(&tok::unmetafy(b)).into_owned()
}

/// Run builtin `args[0]`.
pub(crate) fn run(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let Some(name) = args.first() else { return 0 };
    let rest = args.get(1..).unwrap_or(&[]);
    match name.as_slice() {
        b":" | b"true" | b"hash" | b"rehash" | b"unhash" | b"zmodload" | b"zstyle" | b"bindkey"
        | b"compdef" | b"zle" | b"ulimit" => 0,
        b"false" => 1,
        b"echo" => echo(sh, rest),
        b"print" => print(sh, rest),
        b"printf" => printf(sh, rest),
        b"cd" | b"chdir" => cd(sh, rest),
        b"pwd" => {
            let mut p = sh.get(b"PWD").map(|v| v.joined()).unwrap_or_default();
            p.push(b'\n');
            out(sh, 1, &p)
        }
        b"exit" | b"logout" | b"return" => {
            if let Some(n) = rest.first() {
                match std::str::from_utf8(n)
                    .ok()
                    .and_then(|t| t.trim().parse::<i32>().ok())
                {
                    Some(v) => sh.status = v,
                    None => {
                        sh.error(&format!("{}: bad number: {}", lossy(name), lossy(n)));
                        return 1;
                    }
                }
            }
            sh.flow = if name == b"return" && (!sh.locals.is_empty() || sh.subshell_source()) {
                Flow::Return
            } else {
                Flow::Exit
            };
            sh.status
        }
        b"break" | b"continue" => {
            let n = rest
                .first()
                .and_then(|a| std::str::from_utf8(a).ok()?.parse::<u32>().ok())
                .unwrap_or(1);
            if sh.loop_depth == 0 {
                sh.error(&format!(
                    "{}: not in while, until, select, or repeat loop",
                    lossy(name)
                ));
                return 1;
            }
            sh.flow = if name == b"break" {
                Flow::Break(n.max(1))
            } else {
                Flow::Continue(n.max(1))
            };
            0
        }
        b"set" => set(sh, rest),
        b"shift" => {
            let n = rest
                .first()
                .and_then(|a| std::str::from_utf8(a).ok()?.parse::<usize>().ok())
                .unwrap_or(1);
            if n > sh.positional.len() {
                sh.error("shift: shift count must be <= $#");
                return 1;
            }
            let _gone: Vec<_> = sh.positional.drain(..n).collect();
            0
        }
        b"export" | b"local" | b"typeset" | b"declare" | b"readonly" | b"integer" | b"float" => {
            typeset_expanded(sh, name, rest.to_vec(), &[])
        }
        b"unset" => {
            let funcs = rest.first().is_some_and(|a| a == b"-f");
            for n in rest.iter().filter(|a| a.first() != Some(&b'-')) {
                if funcs {
                    let _f = sh.functions.remove(n);
                } else {
                    sh.unset(n);
                }
            }
            0
        }
        b"unfunction" => {
            for n in rest {
                let _f = sh.functions.remove(n);
            }
            0
        }
        b"source" | b"." => source(sh, name, rest),
        b"eval" => {
            let text = rest.join(&b' ');
            exec::run_string(sh, &text);
            sh.status
        }
        b"test" => crate::cond::test(sh, rest),
        b"[" => {
            if rest.last().map(Vec::as_slice) != Some(b"]") {
                sh.error("[: ']' expected");
                return 2;
            }
            crate::cond::test(sh, rest.get(..rest.len() - 1).unwrap_or(&[]))
        }
        b"alias" => alias(sh, rest),
        b"unalias" => {
            for n in rest.iter().filter(|a| a.first() != Some(&b'-')) {
                let _a = sh.aliases.remove(n);
            }
            if rest.first().is_some_and(|a| a == b"-a") {
                sh.aliases.clear();
            }
            0
        }
        b"setopt" | b"unsetopt" => {
            if rest.is_empty() {
                let mut on: Vec<&String> = sh
                    .options
                    .iter()
                    .filter(|(_, v)| **v)
                    .map(|(k, _)| k)
                    .collect();
                on.sort();
                let text: String = on.iter().map(|k| format!("{k}\n")).collect();
                return out(sh, 1, text.as_bytes());
            }
            for o in rest {
                let _ok = sh.set_option(o, name == b"setopt");
            }
            0
        }
        b"emulate" => {
            if rest.is_empty() {
                return out(sh, 1, b"zsh\n");
            }
            0
        }
        b"type" | b"whence" | b"which" | b"where" => whence(sh, name, rest),
        b"command" => {
            if rest.first().is_some_and(|a| a == b"-v" || a == b"-V") {
                return whence(sh, b"command", rest.get(1..).unwrap_or(&[]));
            }
            0
        }
        b"read" => read(sh, rest),
        b"let" => {
            let mut v = 0;
            for e in rest {
                match crate::arith::eval(sh, e) {
                    Ok(x) => v = x,
                    Err(msg) => {
                        sh.error(&msg);
                        return 2;
                    }
                }
            }
            i32::from(v == 0)
        }
        b"trap" => trap(sh, rest),
        b"wait" => {
            let pids: Vec<i32> = if rest.is_empty() {
                std::mem::take(&mut sh.jobs)
            } else {
                rest.iter()
                    .filter_map(|a| std::str::from_utf8(a).ok()?.parse().ok())
                    .collect()
            };
            let mut st = 0;
            for p in pids {
                st = exec::wait_pid(p);
            }
            st
        }
        b"jobs" => {
            let text: String = sh
                .jobs
                .iter()
                .enumerate()
                .map(|(i, p)| format!("[{}]  running  {p}\n", i + 1))
                .collect();
            out(sh, 1, text.as_bytes())
        }
        b"fg" | b"bg" => {
            sh.error(&format!("{}: no job control in this shell", lossy(name)));
            1
        }
        b"kill" => kill(sh, rest),
        b"umask" => umask(sh, rest),
        b"functions" => {
            let mut names: Vec<&Vec<u8>> = sh.functions.keys().collect();
            names.sort();
            let text: Vec<u8> = names
                .iter()
                .flat_map(|n| [n.as_slice(), b" () { ... }\n"].concat())
                .collect();
            out(sh, 1, &text)
        }
        b"autoload" => autoload(sh, rest),
        b"getopts" => getopts(sh, rest),
        _ => {
            sh.error(&format!("{}: not implemented", lossy(name)));
            1
        }
    }
}

impl Shell {
    fn subshell_source(&self) -> bool {
        self.source_depth > 0
    }
}

/// Interpret echo/print escapes. Returns the text and whether `\c` ended it.
fn escapes(s: &[u8]) -> (Vec<u8>, bool) {
    let mut outb = Vec::new();
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        i += 1;
        if c != b'\\' {
            outb.push(c);
            continue;
        }
        if s.get(i) == Some(&b'c') {
            return (outb, true);
        }
        let start = i - 1;
        let mut j = i;
        while let Some(&n) = s.get(j) {
            if n == b'\\' {
                break;
            }
            j += 1;
            if j - i >= 5 {
                break;
            }
        }
        let chunk: Vec<u8> = [&b"\\"[..], s.get(i..=i).unwrap_or(&[])].concat();
        let _ = chunk;
        // Reuse $'...' for one escape at a time.
        let seq_end = match s.get(i) {
            Some(b'x') => (i + 3).min(s.len()),
            Some(b'0') => (i + 4).min(s.len()),
            Some(b'u') => (i + 5).min(s.len()),
            Some(b'U') => (i + 9).min(s.len()),
            Some(_) => i + 1,
            None => i,
        };
        let seq = s.get(start..seq_end).unwrap_or(&[]);
        let decoded = if s.get(i) == Some(&b'0') {
            let digits: Vec<u8> = s
                .get(i + 1..seq_end)
                .unwrap_or(&[])
                .iter()
                .copied()
                .take_while(|d| (b'0'..=b'7').contains(d))
                .collect();
            i += 1 + digits.len();
            let n =
                u32::from_str_radix(std::str::from_utf8(&digits).unwrap_or("0"), 8).unwrap_or(0);
            outb.push(u8::try_from(n & 0xff).unwrap_or(0));
            continue;
        } else {
            tok::unmetafy(&crate::expand::dollar_quote(seq))
        };
        let consumed = match s.get(i) {
            Some(b'x' | b'u' | b'U') => {
                let max = match s.get(i) {
                    Some(b'x') => 2,
                    Some(b'u') => 4,
                    _ => 8,
                };
                1 + s
                    .get(i + 1..)
                    .unwrap_or(&[])
                    .iter()
                    .take(max)
                    .take_while(|d| d.is_ascii_hexdigit())
                    .count()
            }
            Some(_) => 1,
            None => 0,
        };
        if consumed == 0 {
            outb.push(b'\\');
        } else {
            let full = s.get(start..i + consumed).unwrap_or(&[]);
            outb.extend(tok::unmetafy(&crate::expand::dollar_quote(full)));
            let _ = decoded;
        }
        i += consumed;
    }
    (outb, false)
}

fn echo(sh: &Shell, args: &[Vec<u8>]) -> i32 {
    let (mut newline, mut interpret) = (true, !sh.opt("bsdecho"));
    let mut k = 0;
    while let Some(a) = args.get(k) {
        if a.len() > 1 && a.first() == Some(&b'-') && a.iter().skip(1).all(|c| b"neE".contains(c)) {
            for c in a.iter().skip(1) {
                match c {
                    b'n' => newline = false,
                    b'e' => interpret = true,
                    _ => interpret = false,
                }
            }
            k += 1;
        } else {
            break;
        }
    }
    let joined = tok::unmetafy(&args.get(k..).unwrap_or(&[]).join(&b' '));
    let (mut text, stopped) = if interpret {
        escapes(&joined)
    } else {
        (joined, false)
    };
    if newline && !stopped {
        text.push(b'\n');
    }
    i32::from(!write_fd(1, &text))
}

fn print(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let (mut newline, mut raw, mut lines, mut fd, mut nul) = (true, false, false, 1, false);
    let mut k = 0;
    while let Some(a) = args.get(k) {
        if a == b"--" || a == b"-" {
            k += 1;
            break;
        }
        if a.len() < 2 || a.first() != Some(&b'-') {
            break;
        }
        let mut stop = false;
        for &c in a.iter().skip(1) {
            match c {
                b'n' => newline = false,
                b'r' | b'R' => raw = true,
                b'l' => lines = true,
                b'N' => nul = true,
                b'u' => {
                    fd = args
                        .get(k + 1)
                        .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
                        .unwrap_or(1);
                    k += 1;
                }
                b'f' => {
                    let fmt_args = args.get(k + 1..).unwrap_or(&[]);
                    return printf(sh, fmt_args);
                }
                b'P' | b'c' | b'a' | b'D' | b'o' | b'O' | b'i' | b'm' | b'e' | b'E' | b'z'
                | b's' | b'S' => {}
                _ => {
                    stop = true;
                }
            }
        }
        if stop {
            break;
        }
        k += 1;
    }
    let items: Vec<Vec<u8>> = args
        .get(k..)
        .unwrap_or(&[])
        .iter()
        .map(|a| {
            let a = tok::unmetafy(a);
            if raw { a } else { escapes(&a).0 }
        })
        .collect();
    let sep: &[u8] = if nul {
        b"\0"
    } else if lines {
        b"\n"
    } else {
        b" "
    };
    let mut text = items.join(sep);
    if newline {
        text.push(if nul { 0 } else { b'\n' });
    }
    i32::from(!write_fd(fd, &text))
}

fn printf(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let mut k = 0;
    if args.first().is_some_and(|a| a == b"--") {
        k = 1;
    }
    let Some(fmt) = args.get(k).map(|f| tok::unmetafy(f)) else {
        sh.error("printf: not enough arguments");
        return 1;
    };
    let vals: Vec<Vec<u8>> = args
        .get(k + 1..)
        .unwrap_or(&[])
        .iter()
        .map(|a| tok::unmetafy(a))
        .collect();
    let mut outb = Vec::new();
    let mut next = 0usize;
    let mut status = 0;
    loop {
        let consumed_before = next;
        let mut i = 0;
        while let Some(&c) = fmt.get(i) {
            i += 1;
            if c == b'\\' {
                let start = i - 1;
                let (t, _) = escapes(fmt.get(start..(start + 2).min(fmt.len())).unwrap_or(&[]));
                outb.extend(t);
                i = (start + 2).min(fmt.len());
                continue;
            }
            if c != b'%' {
                outb.push(c);
                continue;
            }
            if fmt.get(i) == Some(&b'%') {
                outb.push(b'%');
                i += 1;
                continue;
            }
            let spec_start = i;
            while fmt.get(i).is_some_and(|c| b"-+ #0123456789.*".contains(c)) {
                i += 1;
            }
            let spec = String::from_utf8_lossy(fmt.get(spec_start..i).unwrap_or(&[])).into_owned();
            let conv = fmt.get(i).copied().unwrap_or(b's');
            i += 1;
            let arg = vals.get(next).cloned();
            next += 1;
            let left = spec.contains('-');
            let zero = spec.starts_with('0') || spec.contains("-0") || spec.contains("+0");
            let (w, prec) = match spec
                .trim_start_matches(['-', '+', ' ', '#', '0'])
                .split_once('.')
            {
                Some((w, p)) => (
                    w.parse::<usize>().unwrap_or(0),
                    Some(p.parse::<usize>().unwrap_or(0)),
                ),
                None => (
                    spec.trim_start_matches(['-', '+', ' ', '#', '0'])
                        .parse::<usize>()
                        .unwrap_or(0),
                    None,
                ),
            };
            let body: Vec<u8> = match conv {
                b'd' | b'i' | b'u' | b'x' | b'X' | b'o' | b'c' if conv != b'c' => {
                    let t = String::from_utf8_lossy(&arg.clone().unwrap_or_default())
                        .trim()
                        .to_owned();
                    let n = if t.is_empty() {
                        0
                    } else if let Some(ch) = t.strip_prefix('\'').or_else(|| t.strip_prefix('"')) {
                        ch.chars().next().map_or(0, |c| i64::from(u32::from(c)))
                    } else {
                        match crate::arith::eval(sh, t.as_bytes()) {
                            Ok(v) => v,
                            Err(_) => {
                                sh.error(&format!("printf: {t}: invalid number"));
                                status = 1;
                                0
                            }
                        }
                    };
                    let s = match conv {
                        b'x' => format!("{n:x}"),
                        b'X' => format!("{n:X}"),
                        b'o' => format!("{n:o}"),
                        _ => {
                            if spec.contains('+') && n >= 0 {
                                format!("+{n}")
                            } else {
                                n.to_string()
                            }
                        }
                    };
                    s.into_bytes()
                }
                b'c' => arg
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .take(1)
                    .collect(),
                b'b' => escapes(&arg.clone().unwrap_or_default()).0,
                b'q' => crate::param::quote_word(&arg.clone().unwrap_or_default()),
                b'f' | b'e' | b'g' | b'F' | b'E' | b'G' => {
                    let t = String::from_utf8_lossy(&arg.clone().unwrap_or_default())
                        .trim()
                        .to_owned();
                    let f: f64 = t.parse().unwrap_or(0.0);
                    format!("{:.*}", prec.unwrap_or(6), f).into_bytes()
                }
                _ => {
                    let mut s = arg.clone().unwrap_or_default();
                    if let Some(p) = prec {
                        s.truncate(p);
                    }
                    s
                }
            };
            let pad = w.saturating_sub(String::from_utf8_lossy(&body).chars().count());
            if left {
                outb.extend(&body);
                outb.extend(std::iter::repeat_n(b' ', pad));
            } else {
                let fill = if zero && conv != b's' { b'0' } else { b' ' };
                outb.extend(std::iter::repeat_n(fill, pad));
                outb.extend(&body);
            }
        }
        if next >= vals.len() || next == consumed_before {
            break;
        }
    }
    if !write_fd(1, &outb) {
        return 1;
    }
    status
}

fn cd(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let args: Vec<&Vec<u8>> = args
        .iter()
        .filter(|a| !matches!(a.as_slice(), b"-q" | b"-s" | b"-L" | b"-P"))
        .collect();
    let old = sh
        .get(b"PWD")
        .map(|v| v.joined())
        .unwrap_or_else(|| b"/".to_vec());
    let (target, show) = match args.first() {
        None => (
            sh.get(b"HOME")
                .map(|v| v.joined())
                .unwrap_or_else(|| b"/".to_vec()),
            false,
        ),
        Some(a) if a.as_slice() == b"-" => (
            sh.get(b"OLDPWD")
                .map(|v| v.joined())
                .unwrap_or_else(|| old.clone()),
            true,
        ),
        Some(a) => ((*a).clone(), false),
    };
    let joined = if target.first() == Some(&b'/') {
        target.clone()
    } else {
        let mut p = old.clone();
        if p.last() != Some(&b'/') {
            p.push(b'/');
        }
        p.extend_from_slice(&target);
        p
    };
    let mut parts: Vec<&[u8]> = Vec::new();
    for c in joined.split(|&c| c == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                let _up = parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut norm = b"/".to_vec();
    norm.extend(parts.join(&b'/'));
    if std::env::set_current_dir(lossy(&norm)).is_err() {
        let msg = if std::path::Path::new(&lossy(&norm)).exists() {
            "not a directory"
        } else {
            "no such file or directory"
        };
        sh.error(&format!("cd: {msg}: {}", lossy(&target)));
        return 1;
    }
    sh.set_scalar(b"OLDPWD", old);
    sh.set_scalar(b"PWD", norm.clone());
    if let Some(v) = sh.vars.get_mut(&b"PWD"[..]) {
        v.export = true;
    }
    if show {
        norm.push(b'\n');
        let _ = out(sh, 1, &norm);
    }
    0
}

fn set(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    if args.is_empty() {
        let mut names: Vec<&Vec<u8>> = sh.vars.keys().collect();
        names.sort();
        let mut text = Vec::new();
        for n in names {
            text.extend_from_slice(n);
            text.push(b'=');
            text.extend(sh.vars.get(n).map(|v| v.value.joined()).unwrap_or_default());
            text.push(b'\n');
        }
        return out(sh, 1, &text);
    }
    let mut k = 0;
    while let Some(a) = args.get(k) {
        if a == b"--" {
            sh.positional = args.get(k + 1..).unwrap_or(&[]).to_vec();
            return 0;
        }
        let on = match a.first() {
            Some(b'-') => true,
            Some(b'+') => false,
            _ => break,
        };
        if a.len() == 1 {
            k += 1;
            break;
        }
        for &c in a.iter().skip(1) {
            match c {
                b'o' => {
                    k += 1;
                    if let Some(o) = args.get(k) {
                        let _ok = sh.set_option(o, on);
                    }
                }
                b'A' => {
                    if let Some(n) = args.get(k + 1) {
                        let vals = args.get(k + 2..).unwrap_or(&[]).to_vec();
                        sh.set_value(n, Value::Array(vals));
                    }
                    return 0;
                }
                b'e' => drop(sh.set_option(b"errexit", on)),
                b'x' => drop(sh.set_option(b"xtrace", on)),
                b'u' => drop(sh.set_option(b"unset", !on)),
                b'f' => drop(sh.set_option(b"glob", !on)),
                b'v' => drop(sh.set_option(b"verbose", on)),
                _ => {}
            }
        }
        k += 1;
    }
    if k < args.len() {
        sh.positional = args.get(k..).unwrap_or(&[]).to_vec();
    }
    0
}

/// `typeset` and relatives from the parser: words unexpanded, assignments
/// parsed.
pub(crate) fn typeset(sh: &mut Shell, words: &[Vec<u8>], args: &[Assign]) {
    let expanded = match crate::expand::expand_words(sh, words) {
        Ok(w) => w,
        Err(e) => {
            sh.error(&e);
            sh.status = 1;
            return;
        }
    };
    let Some((name, rest)) = expanded.split_first() else {
        return;
    };
    sh.status = typeset_expanded(sh, name, rest.to_vec(), args);
}

fn typeset_expanded(sh: &mut Shell, cmd: &[u8], words: Vec<Vec<u8>>, args: &[Assign]) -> i32 {
    let (mut global, mut array, mut assoc, mut integer, mut export, mut readonly) = (
        cmd == b"export",
        false,
        false,
        cmd == b"integer",
        cmd == b"export",
        cmd == b"readonly",
    );
    let mut names: Vec<Vec<u8>> = Vec::new();
    for w in words {
        match w.first() {
            Some(b'-') | Some(b'+') if names.is_empty() && w.len() > 1 => {
                let on = w.first() == Some(&b'-');
                for &c in w.iter().skip(1) {
                    match c {
                        b'g' => global = on,
                        b'a' => array = on,
                        b'A' => assoc = on,
                        b'i' => integer = on,
                        b'x' => export = on,
                        b'r' => readonly = on,
                        _ => {}
                    }
                }
            }
            _ => names.push(w),
        }
    }
    let local = !global && !sh.locals.is_empty();
    let mut all: Vec<Assign> = names
        .into_iter()
        .map(|n| match n.iter().position(|&c| c == b'=') {
            Some(p) => Assign {
                name: n.get(..p).unwrap_or(&[]).to_vec(),
                value: AssignValue::None.clone_with(n.get(p + 1..).unwrap_or(&[])),
                append: false,
            },
            None => Assign {
                name: n,
                value: AssignValue::None,
                append: false,
            },
        })
        .collect();
    all.extend(args.iter().cloned());
    let mut status = 0;
    for a in &all {
        let name = if a.name.iter().any(|&c| tok::is_tok(c)) {
            match crate::expand::expand_single(sh, &a.name) {
                Ok(n) => n,
                Err(e) => {
                    sh.error(&e);
                    status = 1;
                    continue;
                }
            }
        } else {
            a.name.clone()
        };
        if local {
            sh.make_local(&name);
        }
        if !sh.vars.contains_key(&name) || local {
            let value = if assoc {
                Value::Assoc(Vec::new())
            } else if array {
                Value::Array(Vec::new())
            } else {
                Value::Scalar(Vec::new())
            };
            let keep_export = sh.vars.get(&name).is_some_and(|v| v.export);
            let _old = sh.vars.insert(
                name.clone(),
                Var {
                    value,
                    export: keep_export,
                    readonly: false,
                    integer,
                },
            );
        }
        if let Some(v) = sh.vars.get_mut(&name) {
            v.integer |= integer;
            v.export |= export;
            if assoc && !matches!(v.value, Value::Assoc(_)) {
                v.value = Value::Assoc(Vec::new());
            } else if array && !matches!(v.value, Value::Array(_)) {
                v.value = Value::Array(Vec::new());
            }
        }
        if !matches!(a.value, AssignValue::None) {
            let fixed = Assign {
                name: name.clone(),
                value: a.value.clone(),
                append: a.append,
            };
            if let Err(e) = exec::assign(sh, &fixed, false) {
                sh.error(&e);
                status = 1;
            }
        }
        if readonly && let Some(v) = sh.vars.get_mut(&name) {
            v.readonly = true;
        }
    }
    status
}

impl AssignValue {
    /// A scalar assignment of literal bytes (from an expanded `name=value`).
    fn clone_with(&self, literal: &[u8]) -> AssignValue {
        let mut w = Vec::with_capacity(literal.len() * 2);
        for &c in literal {
            w.push(tok::BNULL);
            w.push(c);
        }
        AssignValue::Scalar(w)
    }
}

fn source(sh: &mut Shell, name: &[u8], args: &[Vec<u8>]) -> i32 {
    let Some(file) = args.first() else {
        sh.error(&format!("{}: not enough arguments", lossy(name)));
        return 1;
    };
    let path = if file.contains(&b'/') {
        Some(file.clone())
    } else {
        exec::find_program(sh, file).or_else(|| Some(file.clone()))
    };
    let Some(path) = path else { return 1 };
    let text = match std::fs::read(lossy(&path)) {
        Ok(t) => t,
        Err(_) => {
            sh.error(&format!(
                "{}: no such file or directory: {}",
                lossy(name),
                lossy(file)
            ));
            return 1;
        }
    };
    let saved = if args.len() > 1 {
        Some(std::mem::replace(
            &mut sh.positional,
            args.get(1..).unwrap_or(&[]).to_vec(),
        ))
    } else {
        None
    };
    sh.source_depth += 1;
    sh.status = 0;
    exec::run_string(sh, &tok::metafy(&text));
    sh.source_depth -= 1;
    if sh.flow == Flow::Return && sh.locals.is_empty() {
        sh.flow = Flow::Normal;
    }
    if let Some(p) = saved {
        sh.positional = p;
    }
    sh.status
}

fn alias(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let (mut global, mut suffix) = (false, false);
    let mut defs = Vec::new();
    for a in args {
        match a.as_slice() {
            b"-g" => global = true,
            b"-s" => suffix = true,
            b"--" | b"-L" | b"-r" => {}
            _ => defs.push(a),
        }
    }
    if defs.is_empty() {
        let mut names: Vec<(&Vec<u8>, &AliasDef)> = sh.aliases.iter().collect();
        names.sort_by(|x, y| x.0.cmp(y.0));
        let mut text = Vec::new();
        for (n, d) in names {
            text.extend_from_slice(n);
            text.push(b'=');
            text.extend(crate::param::quote_word(&d.text));
            text.push(b'\n');
        }
        return out(sh, 1, &text);
    }
    let mut status = 0;
    for d in defs {
        match d.iter().position(|&c| c == b'=') {
            Some(p) => {
                let name = d.get(..p).unwrap_or(&[]).to_vec();
                let def = AliasDef {
                    text: d.get(p + 1..).unwrap_or(&[]).to_vec(),
                    global,
                };
                if suffix {
                    let _o = sh.suffix_aliases.insert(name, def);
                } else {
                    let _o = sh.aliases.insert(name, def);
                }
            }
            None => match sh.aliases.get(d) {
                Some(def) => {
                    let mut text = d.clone();
                    text.push(b'=');
                    text.extend(crate::param::quote_word(&def.text));
                    text.push(b'\n');
                    let _ = out(sh, 1, &text);
                }
                None => status = 1,
            },
        }
    }
    status
}

fn whence(sh: &mut Shell, cmd: &[u8], args: &[Vec<u8>]) -> i32 {
    let mut verbose = cmd == b"type";
    let mut all_paths = false;
    let names: Vec<&Vec<u8>> = args
        .iter()
        .filter(|a| {
            if a.first() == Some(&b'-') && a.len() > 1 {
                verbose |= a.contains(&b'v');
                all_paths |= a.contains(&b'p');
                false
            } else {
                true
            }
        })
        .collect();
    let _ = all_paths;
    let mut status = 0;
    for n in names {
        let text = if let Some(a) = sh.aliases.get(n) {
            if verbose || cmd == b"which" {
                format!("{} is an alias for {}\n", lossy(n), lossy(&a.text))
            } else {
                format!("{}\n", lossy(&a.text))
            }
        } else if crate::lex::reserved(n).is_some() {
            if verbose || cmd == b"which" {
                format!("{} is a reserved word\n", lossy(n))
            } else {
                format!("{}\n", lossy(n))
            }
        } else if sh.functions.contains_key(n) {
            if verbose {
                format!("{} is a shell function\n", lossy(n))
            } else {
                format!("{}\n", lossy(n))
            }
        } else if is_builtin(n) {
            if verbose {
                format!("{} is a shell builtin\n", lossy(n))
            } else if cmd == b"which" {
                format!("{}: shell built-in command\n", lossy(n))
            } else {
                format!("{}\n", lossy(n))
            }
        } else if let Some(p) = exec::find_program(sh, n) {
            if verbose {
                format!("{} is {}\n", lossy(n), lossy(&p))
            } else {
                format!("{}\n", lossy(&p))
            }
        } else {
            status = 1;
            if verbose || cmd == b"which" {
                format!("{} not found\n", lossy(n))
            } else {
                String::new()
            }
        };
        let _ = out(sh, 1, text.as_bytes());
    }
    status
}

fn read(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let (mut raw, mut array, mut fd, mut delim, mut count) =
        (false, false, 0, b'\n', None::<usize>);
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut k = 0;
    while let Some(a) = args.get(k) {
        k += 1;
        if a.first() == Some(&b'-') && a.len() > 1 && names.is_empty() {
            let mut j = 1;
            while let Some(&c) = a.get(j) {
                j += 1;
                let mut optarg = |j: &mut usize| -> Vec<u8> {
                    if *j < a.len() {
                        let v = a.get(*j..).unwrap_or(&[]).to_vec();
                        *j = a.len();
                        v
                    } else {
                        k += 1;
                        args.get(k - 1).cloned().unwrap_or_default()
                    }
                };
                match c {
                    b'r' => raw = true,
                    b'A' => array = true,
                    b'u' => {
                        fd = std::str::from_utf8(&optarg(&mut j))
                            .ok()
                            .and_then(|t| t.parse().ok())
                            .unwrap_or(0)
                    }
                    b'd' => delim = optarg(&mut j).first().copied().unwrap_or(0),
                    b'k' | b'n' => {
                        count = Some(
                            std::str::from_utf8(&optarg(&mut j))
                                .ok()
                                .and_then(|t| t.parse().ok())
                                .unwrap_or(1),
                        );
                    }
                    b't' => {
                        let _timeout = optarg(&mut j);
                    }
                    _ => {}
                }
            }
            continue;
        }
        names.push(a.clone());
    }
    if let Some(first) = names.first_mut()
        && let Some(q) = first.iter().position(|&c| c == b'?')
    {
        let prompt = first.split_off(q);
        let _ = write_fd(2, &tok::unmetafy(prompt.get(1..).unwrap_or(&[])));
    }
    let mut line = Vec::new();
    let mut got_any = false;
    let mut byte = [0u8; 1];
    loop {
        if count.is_some_and(|n| line.len() >= n) {
            break;
        }
        // SAFETY: byte is one writable byte.
        let n = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
        if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if n <= 0 {
            break;
        }
        got_any = true;
        let c = byte[0];
        if count.is_none() && c == delim {
            break;
        }
        if !raw && c == b'\\' && count.is_none() {
            // SAFETY: byte is one writable byte.
            let m = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
            if m <= 0 {
                break;
            }
            if byte[0] != b'\n' {
                line.push(byte[0]);
            }
            continue;
        }
        line.push(c);
    }
    let ended = !got_any;
    let line = tok::metafy(&line);
    let ifs = sh
        .get(b"IFS")
        .map_or_else(|| b" \t\n".to_vec(), |v| v.joined());
    let is_ifs = |c: &u8| ifs.contains(c);
    if array {
        let name = names.first().cloned().unwrap_or_else(|| b"reply".to_vec());
        let words: Vec<Vec<u8>> = line
            .split(is_ifs)
            .filter(|w| !w.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        sh.set_value(&name, Value::Array(words));
    } else if names.len() <= 1 {
        let name = names.first().cloned().unwrap_or_else(|| b"REPLY".to_vec());
        let value = if count.is_some() || names.is_empty() {
            line
        } else {
            let trimmed: Vec<u8> = line
                .iter()
                .copied()
                .skip_while(|c| is_ifs(c) && c.is_ascii_whitespace())
                .collect();
            let end = trimmed
                .iter()
                .rposition(|c| !(is_ifs(c) && c.is_ascii_whitespace()))
                .map_or(0, |p| p + 1);
            trimmed.get(..end).unwrap_or(&[]).to_vec()
        };
        sh.set_scalar(&name, value);
    } else {
        let mut rest: &[u8] = &line;
        for (idx, name) in names.iter().enumerate() {
            while rest
                .first()
                .is_some_and(|c| is_ifs(c) && c.is_ascii_whitespace())
            {
                rest = rest.get(1..).unwrap_or(&[]);
            }
            if idx + 1 == names.len() {
                let end = rest
                    .iter()
                    .rposition(|c| !(is_ifs(c) && c.is_ascii_whitespace()))
                    .map_or(0, |p| p + 1);
                sh.set_scalar(name, rest.get(..end).unwrap_or(&[]).to_vec());
            } else {
                let end = rest.iter().position(is_ifs).unwrap_or(rest.len());
                sh.set_scalar(name, rest.get(..end).unwrap_or(&[]).to_vec());
                rest = rest.get((end + 1).min(rest.len())..).unwrap_or(&[]);
            }
        }
    }
    i32::from(ended)
}

fn trap(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let Some((action, sigs)) = args.split_first() else {
        return 0;
    };
    for s in sigs {
        if matches!(s.as_slice(), b"EXIT" | b"0") {
            sh.exit_trap = if action == b"-" || action.is_empty() {
                None
            } else {
                Some(action.clone())
            };
        } else if let Some(n) = signal_number(s) {
            let disp = if action.is_empty() {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            // SAFETY: SIG_IGN and SIG_DFL are valid dispositions.
            let _old = unsafe { libc::signal(n, disp) };
        }
    }
    0
}

fn signal_number(s: &[u8]) -> Option<i32> {
    let t = String::from_utf8_lossy(s);
    let t = t.trim_start_matches("SIG");
    if let Ok(n) = t.parse() {
        return Some(n);
    }
    Some(match t {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "KILL" => libc::SIGKILL,
        "TERM" => libc::SIGTERM,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "PIPE" => libc::SIGPIPE,
        "ALRM" => libc::SIGALRM,
        "CHLD" => libc::SIGCHLD,
        "CONT" => libc::SIGCONT,
        "STOP" => libc::SIGSTOP,
        "TSTP" => libc::SIGTSTP,
        "WINCH" => libc::SIGWINCH,
        _ => return None,
    })
}

fn kill(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let mut sig = libc::SIGTERM;
    let mut status = 0;
    for a in args {
        if let Some(name) = a.strip_prefix(b"-") {
            let name = name
                .strip_prefix(b"s")
                .filter(|n| n.is_empty())
                .unwrap_or(name);
            match signal_number(name) {
                Some(n) => sig = n,
                None => {
                    sh.error(&format!("kill: unknown signal: {}", lossy(a)));
                    return 1;
                }
            }
            continue;
        }
        match std::str::from_utf8(a)
            .ok()
            .and_then(|t| t.parse::<i32>().ok())
        {
            // SAFETY: kill has no memory-safety preconditions.
            Some(pid) => {
                if unsafe { libc::kill(pid, sig) } != 0 {
                    sh.error(&format!("kill: kill {pid} failed: no such process"));
                    status = 1;
                }
            }
            None => {
                sh.error(&format!("kill: illegal pid: {}", lossy(a)));
                status = 1;
            }
        }
    }
    status
}

fn umask(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    match args.first() {
        None => {
            // SAFETY: umask has no memory-safety preconditions.
            let m = unsafe { libc::umask(0o022) };
            // SAFETY: as above; restores the value just read.
            let _r = unsafe { libc::umask(m) };
            out(sh, 1, format!("{m:03o}\n").as_bytes())
        }
        Some(a) => match u32::from_str_radix(&lossy(a), 8) {
            Ok(m) => {
                // SAFETY: umask has no memory-safety preconditions.
                let _old = unsafe { libc::umask(m as libc::mode_t) };
                0
            }
            Err(_) => {
                sh.error(&format!("umask: bad umask: {}", lossy(a)));
                1
            }
        },
    }
}

fn autoload(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let fpath: Vec<Vec<u8>> = match sh.get(b"fpath") {
        Some(Value::Array(a)) => a,
        Some(v) => v
            .joined()
            .split(|&c| c == b':')
            .map(<[u8]>::to_vec)
            .collect(),
        None => sh
            .get(b"FPATH")
            .map(|v| {
                v.joined()
                    .split(|&c| c == b':')
                    .map(<[u8]>::to_vec)
                    .collect()
            })
            .unwrap_or_default(),
    };
    let mut status = 0;
    for name in args
        .iter()
        .filter(|a| a.first() != Some(&b'-') && a.first() != Some(&b'+'))
    {
        let file = fpath
            .iter()
            .map(|d| [d.as_slice(), b"/", name.as_slice()].concat())
            .find(|p| std::path::Path::new(&lossy(p)).is_file());
        let Some(file) = file else {
            status = 1;
            continue;
        };
        let Ok(text) = std::fs::read(lossy(&file)) else {
            continue;
        };
        let mut lx = crate::lex::Lexer::new(tok::metafy(&text), sh.lex_opts());
        let parsed = {
            let mut p = crate::parse::Parser::new(&mut lx, &*sh);
            p.parse_all()
        };
        match parsed {
            Ok(body) => {
                let _old = sh.functions.insert(
                    name.clone(),
                    crate::shell::Function {
                        body: std::rc::Rc::new(body),
                    },
                );
            }
            Err(e) => {
                sh.error(&format!("{}: {}", lossy(&file), e.msg));
                status = 1;
            }
        }
    }
    status
}

fn getopts(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let (Some(spec), Some(var)) = (args.first(), args.get(1)) else {
        sh.error("getopts: not enough arguments");
        return 1;
    };
    let list: Vec<Vec<u8>> = if args.len() > 2 {
        args.get(2..).unwrap_or(&[]).to_vec()
    } else {
        sh.positional.clone()
    };
    let optind: usize = sh
        .get(b"OPTIND")
        .and_then(|v| std::str::from_utf8(&v.joined()).ok()?.parse().ok())
        .unwrap_or(1);
    let pos: usize = sh.optpos;
    let Some(arg) = list.get(optind.saturating_sub(1)) else {
        sh.set_scalar(var, b"?".to_vec());
        return 1;
    };
    if arg.first() != Some(&b'-') || arg.len() < 2 || arg == b"--" {
        if arg == b"--" {
            sh.set_scalar(b"OPTIND", (optind + 1).to_string().into_bytes());
        }
        sh.set_scalar(var, b"?".to_vec());
        return 1;
    }
    let idx = pos.max(1);
    let c = arg.get(idx).copied().unwrap_or(b'?');
    let at_end = idx + 1 >= arg.len();
    let takes = spec
        .iter()
        .position(|&s| s == c)
        .is_some_and(|p| spec.get(p + 1) == Some(&b':'));
    if !spec.contains(&c) || c == b':' {
        sh.set_scalar(var, b"?".to_vec());
        sh.error(&format!("bad option: -{}", char::from(c)));
    } else {
        sh.set_scalar(var, vec![c]);
    }
    if takes {
        let (val, next) = if !at_end {
            (arg.get(idx + 1..).unwrap_or(&[]).to_vec(), optind + 1)
        } else {
            (list.get(optind).cloned().unwrap_or_default(), optind + 2)
        };
        sh.set_scalar(b"OPTARG", val);
        sh.set_scalar(b"OPTIND", next.to_string().into_bytes());
        sh.optpos = 1;
    } else if at_end {
        sh.set_scalar(b"OPTIND", (optind + 1).to_string().into_bytes());
        sh.optpos = 1;
    } else {
        sh.optpos = idx + 1;
    }
    0
}
