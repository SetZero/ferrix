//! Prompt expansion: the `%` escapes of zsh's `prompt.c`, and the
//! `PROMPT_SUBST` substitution that runs before them.

use crate::shell::{Shell, Value};

fn hostname() -> Vec<u8> {
    std::fs::read("/etc/hostname")
        .or_else(|_| std::fs::read("/proc/sys/kernel/hostname"))
        .map(|h| {
            h.into_iter()
                .take_while(|&c| c != b'\n' && c != b'.')
                .collect()
        })
        .unwrap_or_else(|_| b"localhost".to_vec())
}

/// Substitute `$(...)`, `${...}` and `$((...))` in a prompt, as
/// `PROMPT_SUBST` asks. The text is tokenized as if it stood inside double
/// quotes, so only those three expansions happen: a quote, a backslash or a
/// glob character in a prompt is a character of the prompt.
///
/// A prompt that fails to parse or to expand is left as it was written.
/// Reporting it is not an option here -- this runs before every prompt, so
/// one bad theme would scroll the error past everything else.
fn substitute(sh: &mut Shell, ps: &[u8]) -> Vec<u8> {
    let opts = sh.lex_opts();
    let Ok(parsed) = crate::dquote::parse_dquote_string(ps, opts) else {
        return ps.to_vec();
    };
    crate::expand::expand_single(sh, &parsed).unwrap_or_else(|_e| ps.to_vec())
}

/// The SGR sequence a `%F`/`%K` colour spec turns into.
///
/// These are the codes zsh's terminal capabilities produce on a 256-colour
/// terminal: the eight names and 0-7 are the plain codes, 8-15 the bright
/// ones, and the rest go through `38;5;n`. `default`, and any name the
/// terminal would not know, is the default colour rather than an error --
/// which is what zsh does with it too.
fn colour(spec: &[u8], bg: bool) -> Vec<u8> {
    let (plain, bright, extended) = if bg {
        (40, 100, "48;5;")
    } else {
        (30, 90, "38;5;")
    };
    let named = match spec {
        b"black" => Some(0),
        b"red" => Some(1),
        b"green" => Some(2),
        b"yellow" => Some(3),
        b"blue" => Some(4),
        b"magenta" => Some(5),
        b"cyan" => Some(6),
        b"white" => Some(7),
        _ => None,
    };
    let n = named.or_else(|| {
        std::str::from_utf8(spec)
            .ok()?
            .parse::<u16>()
            .ok()
            .filter(|&n| n <= 255)
    });
    let body = match n {
        Some(n @ 0..=7) => format!("{}", plain + n),
        Some(n @ 8..=15) => format!("{}", bright + n - 8),
        Some(n) => format!("{extended}{n}"),
        None => format!("{}", plain + 9),
    };
    format!("\x1b[{body}m").into_bytes()
}

/// Whether the condition of a `%(c.true.false)` holds, where `arg` is the
/// number written before the condition character.
///
/// The conditions that need no more than the shell's own state are here;
/// the ones that count what is on the screen or ask the clock (`l`, `t`,
/// `D`, `e`, `S`, …) are not, and read as false.
fn condition(sh: &Shell, cond: u8, arg: i64) -> bool {
    let number = |name: &[u8]| -> i64 {
        sh.get(name)
            .map(|v| v.joined())
            .and_then(|v| {
                std::str::from_utf8(&crate::tok::unmetafy(&v))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()
            })
            .unwrap_or(0)
    };
    match cond {
        // SAFETY: geteuid and getegid have no preconditions.
        b'!' => (unsafe { libc::geteuid() }) == 0,
        b'#' => i64::from(unsafe { libc::geteuid() }) == arg,
        b'g' => i64::from(unsafe { libc::getegid() }) == arg,
        b'?' => i64::from(sh.status) == arg,
        b'j' => i64::try_from(sh.jobs.ids().len()).unwrap_or(0) >= arg,
        b'L' => number(b"SHLVL") >= arg,
        b'v' => {
            let len = match sh.get(b"psvar") {
                Some(Value::Array(a)) => a.len(),
                Some(_) => 1,
                None => 0,
            };
            i64::try_from(len).unwrap_or(0) >= arg
        }
        _ => false,
    }
}

/// One branch of a ternary: everything up to the next unnested `end`.
///
/// A `%` and the character after it are taken together, so that an escape
/// holding the delimiter -- or the `)` of a nested `%(` -- does not end the
/// branch early.
fn branch(ps: &[u8], i: &mut usize, end: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut depth = 0_u32;
    while let Some(&c) = ps.get(*i) {
        if c == b'%' {
            if ps.get(*i + 1) == Some(&b'(') {
                depth += 1;
            }
            out.push(c);
            *i += 1;
            if let Some(&next) = ps.get(*i) {
                out.push(next);
                *i += 1;
            }
            continue;
        }
        if c == b')' && depth > 0 {
            depth -= 1;
        } else if c == end && depth == 0 {
            *i += 1;
            return out;
        }
        out.push(c);
        *i += 1;
    }
    out
}

/// Expand `%` escapes in `ps`, after `PROMPT_SUBST` if it is set.
pub(crate) fn expand(sh: &mut Shell, ps: &[u8]) -> Vec<u8> {
    // The substitution happens first, so that what a theme's function
    // prints is itself read for `%` escapes -- which is how agnoster, whose
    // whole prompt is one `$(build_prompt)`, gets its colours.
    let ps: &[u8] = &if sh.opt("promptsubst") {
        substitute(sh, ps)
    } else {
        ps.to_vec()
    };
    escapes(sh, ps)
}

/// The `%` escapes alone, over text a `PROMPT_SUBST` has already been
/// through. A ternary's chosen branch comes back through here.
fn escapes(sh: &Shell, ps: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    let pwd = sh.get(b"PWD").map(|v| v.joined()).unwrap_or_default();
    let home = sh.get(b"HOME").map(|v| v.joined()).unwrap_or_default();
    while let Some(&c) = ps.get(i) {
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        let digits = i;
        while ps.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let num: Option<u16> = std::str::from_utf8(ps.get(digits..i).unwrap_or(&[]))
            .ok()
            .and_then(|d| d.parse().ok());
        let Some(&e) = ps.get(i) else { break };
        i += 1;
        match e {
            b'%' => out.push(b'%'),
            // `%(c.true.false)`, where the character after the condition is
            // the one that separates the branches.
            b'(' => {
                let digits = i;
                while ps.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                let arg: i64 = std::str::from_utf8(ps.get(digits..i).unwrap_or(&[]))
                    .ok()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(0);
                let Some(&cond) = ps.get(i) else { break };
                i += 1;
                let Some(&sep) = ps.get(i) else { break };
                i += 1;
                let yes = branch(ps, &mut i, sep);
                let no = branch(ps, &mut i, b')');
                let taken = if condition(sh, cond, arg) { yes } else { no };
                out.extend(escapes(sh, &taken));
            }
            b'F' | b'K' => {
                let spec = if ps.get(i) == Some(&b'{') {
                    let start = i + 1;
                    match ps
                        .get(start..)
                        .and_then(|r| r.iter().position(|&c| c == b'}'))
                    {
                        Some(end) => {
                            i = start + end + 1;
                            ps.get(start..start + end).unwrap_or(&[]).to_vec()
                        }
                        None => {
                            i = ps.len();
                            Vec::new()
                        }
                    }
                } else {
                    // `%3F` is the colour the number names.
                    num.map(|n| n.to_string().into_bytes()).unwrap_or_default()
                };
                out.extend(colour(&spec, e == b'K'));
            }
            b'f' => out.extend_from_slice(b"\x1b[39m"),
            b'k' => out.extend_from_slice(b"\x1b[49m"),
            b'B' => out.extend_from_slice(b"\x1b[1m"),
            // There is no "not bold" on a terminal, so zsh resets
            // everything here, and so does this.
            b'b' => out.extend_from_slice(b"\x1b[0m"),
            b'U' => out.extend_from_slice(b"\x1b[4m"),
            b'u' => out.extend_from_slice(b"\x1b[24m"),
            b'S' => out.extend_from_slice(b"\x1b[7m"),
            b's' => out.extend_from_slice(b"\x1b[27m"),
            b'#' => {
                // SAFETY: geteuid has no preconditions.
                out.push(if unsafe { libc::geteuid() } == 0 {
                    b'#'
                } else {
                    b'%'
                });
            }
            b'm' | b'M' => out.extend(hostname()),
            b'n' => out.extend(
                sh.get(b"USER")
                    .map(|v| v.joined())
                    .unwrap_or_else(|| b"root".to_vec()),
            ),
            b'?' => out.extend(sh.status.to_string().into_bytes()),
            b'd' | b'/' => out.extend_from_slice(&pwd),
            b'~' => {
                if !home.is_empty() && home != b"/" && pwd.starts_with(&home) {
                    out.push(b'~');
                    out.extend_from_slice(pwd.get(home.len()..).unwrap_or(&[]));
                } else {
                    out.extend_from_slice(&pwd);
                }
            }
            b'c' | b'.' | b'C' => {
                out.extend_from_slice(
                    pwd.rsplit(|&c| c == b'/')
                        .find(|p| !p.is_empty())
                        .unwrap_or(b"/"),
                );
            }
            // The markers around a raw escape sequence. What they enclose
            // is already literal, and the width it does not take is
            // measured by skipping escapes rather than by these.
            b'{' | b'}' => {}
            other => {
                out.push(b'%');
                out.push(other);
            }
        }
    }
    out
}
