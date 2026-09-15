//! Glob qualifiers: the `(...)` that may close a pattern and says which of
//! the files it matched to keep — `*(.)` only plain files, `*(N-f:g+w:)` only
//! the group-writable ones, following symbolic links.
//!
//! zsh's `glob.c`, the part after `zglob` has matched: qualifiers are read
//! right to left off the pattern, commas separate alternatives of which one
//! passing is enough, `^` negates every qualifier after it, and `-` makes
//! the ones after it look through a symbolic link.
//!
//! A trailing group is only a qualifier list if the whole of it reads as one.
//! `*(foo|bar)` is a pattern, and anything this module cannot parse is left
//! to the matcher, which is what zsh's BARE_GLOB_QUAL does.

use std::os::unix::fs::MetadataExt;

use crate::tok::{self, INPAR, OUTPAR};

/// One test a file has to pass.
#[derive(Debug, Clone)]
enum Test {
    /// `.` plain file, `/` directory, `@` symbolic link, `p` fifo, `=`
    /// socket, `%` device.
    Kind(u8),
    Uid(u32),
    Gid(u32),
    User(Vec<u8>),
    Group(Vec<u8>),
    /// `f:g+w:` and `f644`: bits that must be set, and bits that must not.
    Mode {
        on: u32,
        off: u32,
        exact: Option<u32>,
    },
    /// `m-1`, `m+7`: changed less, or more, than n units ago.
    Age {
        newer: bool,
        seconds: i64,
    },
    /// `r`, `w`, `x` for the owner's bits; `R`, `W`, `X` for everyone's.
    Permission {
        bits: u32,
    },
    /// `*`: a plain file anyone may execute.
    Executable,
}

#[derive(Debug, Clone)]
struct Qual {
    negate: bool,
    follow: bool,
    test: Test,
}

/// A parsed qualifier list.
#[derive(Debug, Default)]
pub(crate) struct Quals {
    /// One passing alternative is enough; an empty list keeps everything.
    alts: Vec<Vec<Qual>>,
    /// `N`: no matches is not an error, the word just goes.
    pub(crate) nullglob: bool,
    /// `D`: names beginning with a dot count too.
    pub(crate) dots: bool,
    /// `M`: append a slash to directories, as MARK_DIRS does.
    pub(crate) mark_dirs: bool,
    /// A history modifier the qualifiers ended with, as in `(N:t)`.
    pub(crate) modifiers: Vec<u8>,
}

/// Split a trailing qualifier list off `pat`. The stem comes back with the
/// qualifiers removed; `None` means the pattern has none and must be matched
/// whole.
pub(crate) fn split(pat: &[u8]) -> (Vec<u8>, Option<Quals>) {
    if pat.last() != Some(&OUTPAR) {
        return (pat.to_vec(), None);
    }
    // The group that closes the pattern: scan back for the `(` that opens it.
    let mut depth = 0;
    let mut open = None;
    for (i, &c) in pat.iter().enumerate().rev() {
        if c == OUTPAR {
            depth += 1;
        } else if c == INPAR {
            depth -= 1;
            if depth == 0 {
                open = Some(i);
                break;
            }
        }
    }
    let Some(open) = open else {
        return (pat.to_vec(), None);
    };
    let body = pat.get(open + 1..pat.len() - 1).unwrap_or(&[]);
    match parse(body) {
        Some(q) => (pat.get(..open).unwrap_or(&[]).to_vec(), Some(q)),
        None => (pat.to_vec(), None),
    }
}

/// Read a qualifier list. `None` if any of it is not one, so that the group
/// stays part of the pattern.
fn parse(body: &[u8]) -> Option<Quals> {
    if body.is_empty() {
        return None;
    }
    let mut q = Quals::default();
    let mut alt: Vec<Qual> = Vec::new();
    let mut negate = false;
    let mut follow = false;
    let mut i = 0;
    while let Some(&token) = body.get(i) {
        i += 1;
        let c = tok::detok(token);
        match c {
            b',' => {
                q.alts.push(std::mem::take(&mut alt));
                negate = false;
                follow = false;
            }
            b'^' => negate = !negate,
            b'-' => follow = !follow,
            b'N' => q.nullglob = !negate,
            b'D' => q.dots = !negate,
            b'M' => q.mark_dirs = !negate,
            b'.' | b'/' | b'@' | b'p' | b'=' | b'%' => {
                alt.push(Qual {
                    negate,
                    follow,
                    test: Test::Kind(c),
                });
            }
            b'*' => alt.push(Qual {
                negate,
                follow,
                test: Test::Executable,
            }),
            b'r' | b'w' | b'x' | b'R' | b'W' | b'X' => {
                let bits = match c {
                    b'r' => 0o400,
                    b'w' => 0o200,
                    b'x' => 0o100,
                    b'R' => 0o004,
                    b'W' => 0o002,
                    _ => 0o001,
                };
                alt.push(Qual {
                    negate,
                    follow,
                    test: Test::Permission { bits },
                });
            }
            b'u' | b'g' => {
                let (test, next) = owner(body, i, c)?;
                i = next;
                alt.push(Qual {
                    negate,
                    follow,
                    test,
                });
            }
            b'f' => {
                let (test, next) = mode(body, i)?;
                i = next;
                alt.push(Qual {
                    negate,
                    follow,
                    test,
                });
            }
            b'm' | b'a' | b'c' => {
                let (test, next) = age(body, i)?;
                i = next;
                alt.push(Qual {
                    negate,
                    follow,
                    test,
                });
            }
            b':' => {
                // What is left is a modifier, as in `(N:t)`.
                q.modifiers = body.get(i - 1..).unwrap_or(&[]).to_vec();
                i = body.len();
            }
            _ => return None,
        }
    }
    q.alts.push(alt);
    q.alts.retain(|a| !a.is_empty());
    Some(q)
}

/// The text a qualifier delimits, as `u:name:` does. Answers the text and
/// where the qualifier ends.
fn delimited(body: &[u8], i: usize) -> Option<(Vec<u8>, usize)> {
    let open = tok::detok(*body.get(i)?);
    let close = match open {
        b'[' => b']',
        b'{' => b'}',
        b'<' => b'>',
        c if c.is_ascii_alphanumeric() => return None,
        c => c,
    };
    let rest = body.get(i + 1..)?;
    let end = rest.iter().position(|&c| tok::detok(c) == close)?;
    let text = rest.get(..end)?.iter().map(|&c| tok::detok(c)).collect();
    Some((text, i + 1 + end + 1))
}

/// `u0`, `u:root:`, `g20`, `g:staff:`.
fn owner(body: &[u8], i: usize, which: u8) -> Option<(Test, usize)> {
    if let Some((name, next)) = delimited(body, i) {
        let by_name = if which == b'u' {
            Test::User(name)
        } else {
            Test::Group(name)
        };
        return Some((by_name, next));
    }
    let digits: Vec<u8> = body
        .get(i..)?
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let n: u32 = std::str::from_utf8(&digits).ok()?.parse().ok()?;
    let test = if which == b'u' {
        Test::Uid(n)
    } else {
        Test::Gid(n)
    };
    Some((test, i + digits.len()))
}

/// `f:g+w:`, `f644`, `f:u+rw,o-w:`.
fn mode(body: &[u8], i: usize) -> Option<(Test, usize)> {
    if let Some((spec, next)) = delimited(body, i) {
        let test = mode_spec(&spec)?;
        return Some((test, next));
    }
    let digits: Vec<u8> = body
        .get(i..)?
        .iter()
        .copied()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    let n = u32::from_str_radix(std::str::from_utf8(&digits).ok()?, 8).ok()?;
    Some((
        Test::Mode {
            on: 0,
            off: 0,
            exact: Some(n),
        },
        i + digits.len(),
    ))
}

/// The bits `who` names: `u` the owner's, `g` the group's, `o` everyone's,
/// `a` all three.
fn who_bits(who: u8, letter: u8) -> u32 {
    let (r, w, x, s) = match who {
        b'u' => (0o400, 0o200, 0o100, 0o4000),
        b'g' => (0o040, 0o020, 0o010, 0o2000),
        _ => (0o004, 0o002, 0o001, 0o1000),
    };
    match letter {
        b'r' => r,
        b'w' => w,
        b'x' => x,
        b's' => s,
        b't' => 0o1000,
        _ => 0,
    }
}

/// `g+w`, `u+rw,o-w`, `755`.
fn mode_spec(spec: &[u8]) -> Option<Test> {
    if spec.iter().all(u8::is_ascii_digit) && !spec.is_empty() {
        let n = u32::from_str_radix(std::str::from_utf8(spec).ok()?, 8).ok()?;
        return Some(Test::Mode {
            on: 0,
            off: 0,
            exact: Some(n),
        });
    }
    let (mut on, mut off) = (0u32, 0u32);
    for clause in spec.split(|&c| c == b',') {
        let at = clause
            .iter()
            .position(|&c| matches!(c, b'+' | b'-' | b'='))?;
        let whos = clause.get(..at)?;
        let op = *clause.get(at)?;
        let letters = clause.get(at + 1..)?;
        let whos: Vec<u8> = if whos.is_empty() || whos == b"a" {
            vec![b'u', b'g', b'o']
        } else {
            whos.to_vec()
        };
        for who in whos {
            if !matches!(who, b'u' | b'g' | b'o') {
                return None;
            }
            for &letter in letters {
                let bits = who_bits(who, letter);
                if bits == 0 {
                    return None;
                }
                if op == b'-' { off |= bits } else { on |= bits }
            }
        }
    }
    Some(Test::Mode {
        on,
        off,
        exact: None,
    })
}

/// `m-1`, `m+7`, `mh-2`: an age in days, or in the named unit.
fn age(body: &[u8], mut i: usize) -> Option<(Test, usize)> {
    let mut unit = 86400i64;
    if let Some(&u) = body.get(i)
        && matches!(u, b's' | b'm' | b'h' | b'd' | b'w' | b'M')
    {
        unit = match u {
            b's' => 1,
            b'm' => 60,
            b'h' => 3600,
            b'w' => 7 * 86400,
            b'M' => 30 * 86400,
            _ => 86400,
        };
        i += 1;
    }
    let sign = match body.get(i) {
        Some(&b'-') => {
            i += 1;
            true
        }
        Some(&b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let digits: Vec<u8> = body
        .get(i..)?
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let n: i64 = std::str::from_utf8(&digits).ok()?.parse().ok()?;
    Some((
        Test::Age {
            newer: sign,
            seconds: n * unit,
        },
        i + digits.len(),
    ))
}

impl Quals {
    /// True if `path` passes: any one alternative, all of whose qualifiers
    /// hold. No qualifier at all keeps everything.
    pub(crate) fn keep(&self, path: &[u8]) -> bool {
        if self.alts.is_empty() {
            return true;
        }
        self.alts
            .iter()
            .any(|alt| alt.iter().all(|q| holds(q, path)))
    }
}

/// What the file is, following the link or not.
fn stat_of(path: &[u8], follow: bool) -> Option<std::fs::Metadata> {
    let name = String::from_utf8_lossy(path).into_owned();
    if follow {
        // zsh falls back to the link itself when its target cannot be
        // statted, which makes `*(-@)` select broken symbolic links.
        std::fs::metadata(&name)
            .or_else(|_| std::fs::symlink_metadata(&name))
            .ok()
    } else {
        std::fs::symlink_metadata(&name).ok()
    }
}

fn holds(q: &Qual, path: &[u8]) -> bool {
    let yes = test_holds(&q.test, path, q.follow);
    yes != q.negate
}

fn test_holds(test: &Test, path: &[u8], follow: bool) -> bool {
    let Some(md) = stat_of(path, follow) else {
        return false;
    };
    let mode = md.mode();
    match test {
        Test::Kind(k) => {
            let kind = mode & 0o170_000;
            match k {
                b'.' => kind == 0o100_000,
                b'/' => kind == 0o040_000,
                b'@' => kind == 0o120_000,
                b'p' => kind == 0o010_000,
                b'=' => kind == 0o140_000,
                b'%' => kind == 0o020_000 || kind == 0o060_000,
                _ => false,
            }
        }
        Test::Uid(n) => md.uid() == *n,
        Test::Gid(n) => md.gid() == *n,
        Test::User(name) => user_id(name).is_some_and(|n| md.uid() == n),
        Test::Group(name) => group_id(name).is_some_and(|n| md.gid() == n),
        Test::Permission { bits } => mode & bits != 0,
        Test::Executable => mode & 0o170_000 == 0o100_000 && mode & 0o111 != 0,
        Test::Mode { on, off, exact } => match exact {
            Some(want) => mode & 0o7777 == *want,
            None => mode & on == *on && mode & off == 0,
        },
        Test::Age { newer, seconds } => {
            let now = now_seconds();
            let age = now - md.mtime();
            if *newer {
                age <= *seconds
            } else {
                age > *seconds
            }
        }
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// The id of a named user, read from the password file: there is no NSS here.
fn user_id(name: &[u8]) -> Option<u32> {
    id_in_file("/etc/passwd", name)
}

fn group_id(name: &[u8]) -> Option<u32> {
    id_in_file("/etc/group", name)
}

fn id_in_file(file: &str, name: &[u8]) -> Option<u32> {
    let text = std::fs::read(file).ok()?;
    for line in text.split(|&c| c == b'\n') {
        let mut parts = line.split(|&c| c == b':');
        if parts.next() == Some(name) {
            let _password = parts.next();
            let id = parts.next()?;
            return std::str::from_utf8(id).ok()?.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quals(s: &str) -> Option<Quals> {
        parse(s.as_bytes())
    }

    #[test]
    fn reads_what_compaudit_asks() {
        let q = quals("N-f:g+w:,-f:o+w:,-^u0u1000").expect("a qualifier list");
        assert!(q.nullglob);
        assert_eq!(q.alts.len(), 3);
        // The last alternative negates both owners, so neither may own it.
        assert_eq!(q.alts.get(2).map(Vec::len), Some(2));

        // A word read by the lexer carries zsh's in-band pattern tokens.
        let tokenized = crate::pattern::tokenize(b"N-f:g+w:,-f:o+w:,-^u0u1000");
        assert!(parse(&tokenized).is_some());
    }

    #[test]
    fn leaves_a_pattern_group_alone() {
        assert!(quals("foo|bar").is_none());
        assert!(quals("^_*").is_none());
    }

    #[test]
    fn reads_a_mode_spec() {
        match mode_spec(b"g+w") {
            Some(Test::Mode { on, off, exact }) => {
                assert_eq!(on, 0o020);
                assert_eq!(off, 0);
                assert!(exact.is_none());
            }
            other => panic!("not a mode: {other:?}"),
        }
    }

    #[test]
    fn takes_a_modifier_after_the_qualifiers() {
        let q = quals("N:t").expect("a qualifier list");
        assert!(q.nullglob);
        assert_eq!(q.modifiers, b":t".to_vec());
    }

    #[test]
    fn reads_compaudits_directory_mark_override() {
        let q = quals("/^M").expect("a qualifier list");
        assert!(!q.mark_dirs);
        assert_eq!(q.alts.first().map(Vec::len), Some(1));
        assert!(quals("M").is_some_and(|q| q.mark_dirs));
    }
}
