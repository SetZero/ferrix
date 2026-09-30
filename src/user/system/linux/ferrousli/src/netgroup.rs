//! `netdb.h`'s netgroups: `innetgr`, whether a host, user and domain are in
//! a netgroup, which glibc's `libnss_compat` asks for its `+@group` lines.
//!
//! glibc answers from the name service switch's `netgroup` database. There
//! are no switch modules here, so the one source is glibc's `files` one,
//! `/etc/netgroup`, read directly. A system without the file -- Debian's
//! default switch names only NIS for netgroups -- has no netgroups, and every
//! question is answered 0, as glibc answers it with no NIS server.
//!
//! # The file
//!
//! Each line names a netgroup and then its members, separated by blanks; a
//! line ending in a backslash goes on onto the next, and a line whose first
//! character that is not blank is `#` is a comment. A member is a triple,
//! `(host,user,domain)`, or the name of another netgroup, whose members are
//! this one's too. The first line naming a group is its definition.
//!
//! # Matching
//!
//! As glibc's `innetgr`: a triple matches when each of its three fields
//! does. An empty field matches anything, and so does a null argument; any
//! other field matches only the same string, the host and domain without
//! regard to ASCII case and the user exactly. So `-`, which names no host,
//! matches only a caller who asks for the host `-`. Nested groups are
//! searched depth first, each group once, so a cycle ends.

use core::ffi::{CStr, c_char, c_int};

use crate::dirent::close_quietly;
use crate::errno;
use crate::fcntl::AT_FDCWD;
use crate::growable::Growable;
use crate::syscall::{self, nr};

/// `_PATH_NETGROUP`.
const PATH_NETGROUP: &CStr = c"/etc/netgroup";

/// `O_RDONLY | O_CLOEXEC`, which every architecture here numbers alike.
const O_RDONLY_CLOEXEC: usize = 0o2_000_000;

/// What `innetgr` was asked: each field, or `None` for any.
#[derive(Debug, Clone, Copy)]
struct Query<'a> {
    /// The host.
    host: Option<&'a [u8]>,
    /// The user.
    user: Option<&'a [u8]>,
    /// The domain.
    domain: Option<&'a [u8]>,
}

/// One token of a line: a triple's text between its parentheses, or a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    /// `(host,user,domain)`, without the parentheses.
    Triple(&'a [u8]),
    /// A netgroup's name.
    Word(&'a [u8]),
}

/// Whether `b` separates tokens. A newline inside a logical line is one a
/// backslash continued, and the backslash before it separates too.
fn is_blank(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\\')
}

/// The logical lines of `data`: each physical line, joined to the next where
/// it ends in a backslash. Each is the raw bytes, continuations included.
fn logical_lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = 0;
        let len = loop {
            match rest.iter().skip(end).position(|&b| b == b'\n') {
                None => break rest.len(),
                Some(at) => {
                    let newline = end + at;
                    if newline > 0 && rest.get(newline - 1) == Some(&b'\\') {
                        end = newline + 1;
                        continue;
                    }
                    break newline;
                }
            }
        };
        let line = rest.get(..len).unwrap_or_default();
        rest = rest.get(len + 1..).unwrap_or_default();
        Some(line)
    })
}

/// The tokens of one logical line.
fn tokens(line: &[u8]) -> impl Iterator<Item = Token<'_>> {
    let mut rest = line;
    core::iter::from_fn(move || {
        let start = rest.iter().position(|&b| !is_blank(b))?;
        rest = rest.get(start..).unwrap_or_default();
        if rest.first() == Some(&b'(') {
            let inner = rest.get(1..).unwrap_or_default();
            let close = inner.iter().position(|&b| b == b')').unwrap_or(inner.len());
            rest = inner.get(close + 1..).unwrap_or_default();
            return Some(Token::Triple(inner.get(..close).unwrap_or_default()));
        }
        let end = rest
            .iter()
            .position(|&b| is_blank(b) || b == b'(')
            .unwrap_or(rest.len());
        let word = rest.get(..end).unwrap_or_default();
        rest = rest.get(end..).unwrap_or_default();
        Some(Token::Word(word))
    })
}

/// `field` with blanks trimmed from both ends, or `None` if that leaves it
/// empty, which matches anything.
fn field(raw: &[u8]) -> Option<&[u8]> {
    let start = raw.iter().position(|&b| !is_blank(b))?;
    let end = raw.iter().rposition(|&b| !is_blank(b))?;
    raw.get(start..=end)
}

/// Whether the triple whose text is `triple` matches `query`. A triple
/// without exactly three fields matches nothing.
fn triple_matches(triple: &[u8], query: &Query<'_>) -> bool {
    let mut parts = triple.split(|&b| b == b',');
    let (Some(host), Some(user), Some(domain), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let caseless = |asked: Option<&[u8]>, have: &[u8]| {
        asked.is_none_or(|asked| field(have).is_none_or(|have| have.eq_ignore_ascii_case(asked)))
    };
    let exact = |asked: Option<&[u8]>, have: &[u8]| {
        asked.is_none_or(|asked| field(have).is_none_or(|have| have == asked))
    };
    caseless(query.host, host) && exact(query.user, user) && caseless(query.domain, domain)
}

/// The members of the group `name`, from its first definition in `data`.
fn definition<'a>(data: &'a [u8], name: &[u8]) -> Option<impl Iterator<Item = Token<'a>>> {
    logical_lines(data).find_map(|line| {
        let mut words = tokens(line);
        match words.next() {
            Some(Token::Word(first)) if first == name && first.first() != Some(&b'#') => {
                Some(words)
            }
            _ => None,
        }
    })
}

/// Whether `group`, or a group nested in it, in the netgroup file `data`
/// holds a triple matching `query`. Running out of memory answers `false`.
fn in_group(data: &[u8], group: &[u8], query: &Query<'_>) -> bool {
    let mut pending: Growable<&[u8]> = Growable::new();
    let mut seen: Growable<&[u8]> = Growable::new();
    if !pending.push(group) {
        return false;
    }
    while let Some(name) = pending.pop() {
        if seen.as_slice().contains(&name) {
            continue;
        }
        if !seen.push(name) {
            return false;
        }
        let Some(members) = definition(data, name) else {
            continue;
        };
        for member in members {
            match member {
                Token::Triple(triple) => {
                    if triple_matches(triple, query) {
                        return true;
                    }
                }
                Token::Word(nested) => {
                    if !pending.push(nested) {
                        return false;
                    }
                }
            }
        }
    }
    false
}

/// Reads all of `/etc/netgroup` into `data`. Returns `false` if it cannot be
/// read whole.
fn read_netgroup(data: &mut Growable<u8>) -> bool {
    // SAFETY: the kernel reads the path, a C string literal.
    let ret = unsafe {
        syscall::syscall4(
            nr::OPENAT,
            AT_FDCWD as usize,
            PATH_NETGROUP.as_ptr().addr(),
            O_RDONLY_CLOEXEC,
            0,
        )
    };
    let Ok(fd) = errno::decode(ret) else {
        return false;
    };
    let fd = fd as c_int;
    let mut chunk = [0u8; 4096];
    let whole = loop {
        // SAFETY: the kernel writes at most the chunk's length into it.
        let ret = unsafe {
            syscall::syscall3(
                nr::READ,
                fd as usize,
                chunk.as_mut_ptr().addr(),
                chunk.len(),
            )
        };
        match errno::decode(ret) {
            Ok(0) => break true,
            Ok(n) => {
                if !data.reserve(n) {
                    break false;
                }
                for &b in chunk.get(..n).unwrap_or_default() {
                    let _ = data.push(b);
                }
            }
            Err(errno::EINTR) => {}
            Err(_) => break false,
        }
    };
    close_quietly(fd);
    whole
}

/// The bytes of `s`, or `None` for a null pointer, which matches anything.
///
/// # Safety
///
/// `s` must be null or a NUL-terminated string.
unsafe fn argument<'a>(s: *const c_char) -> Option<&'a [u8]> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string.
    Some(unsafe { CStr::from_ptr(s) }.to_bytes())
}

/// 1 if the netgroup `netgroup`, or one nested in it, has a member matching
/// `host`, `user` and `domain`, each null for any; 0 if not, or if there is
/// no such group.
///
/// # Safety
///
/// Each argument must be null or a NUL-terminated string.
#[cfg_attr(not(test), unsafe(no_mangle))]
pub unsafe extern "C" fn innetgr(
    netgroup: *const c_char,
    host: *const c_char,
    user: *const c_char,
    domain: *const c_char,
) -> c_int {
    // SAFETY: the caller passes null or a string.
    let Some(group) = (unsafe { argument(netgroup) }) else {
        return 0;
    };
    let query = Query {
        // SAFETY: as above.
        host: unsafe { argument(host) },
        // SAFETY: as above.
        user: unsafe { argument(user) },
        // SAFETY: as above.
        domain: unsafe { argument(domain) },
    };
    let mut data = Growable::new();
    if !read_netgroup(&mut data) {
        return 0;
    }
    c_int::from(in_group(data.as_slice(), group, &query))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &[u8] = b"# the machines\n\
        servers (alpha,,example.org) (BETA,-,) \\\n\
        \t(gamma, root ,)\n\
        admins (,alice,) (,bob,example.org)\n\
        everyone servers admins (bad) (x,y)\n\
        loop1 loop2\n\
        loop2 loop1 (,carol,)\n\
        servers (never,,)\n";

    fn ask(group: &str, host: Option<&str>, user: Option<&str>, domain: Option<&str>) -> bool {
        let query = Query {
            host: host.map(str::as_bytes),
            user: user.map(str::as_bytes),
            domain: domain.map(str::as_bytes),
        };
        in_group(FILE, group.as_bytes(), &query)
    }

    #[test]
    fn a_line_ending_in_a_backslash_goes_on() {
        let lines: Vec<&[u8]> = logical_lines(b"a b \\\nc\nd\n").collect();
        assert_eq!(lines, [&b"a b \\\nc"[..], b"d"]);
        let words: Vec<Token<'_>> = tokens(b"g (h, u ,d)x \\\n y").collect();
        assert_eq!(
            words,
            [
                Token::Word(b"g"),
                Token::Triple(b"h, u ,d"),
                Token::Word(b"x"),
                Token::Word(b"y"),
            ]
        );
    }

    #[test]
    fn an_empty_field_or_a_null_argument_matches_anything() {
        assert!(ask(
            "servers",
            Some("alpha"),
            Some("anyone"),
            Some("example.org")
        ));
        assert!(ask("servers", Some("ALPHA"), None, Some("EXAMPLE.org")));
        assert!(!ask("servers", Some("alpha"), None, Some("example.com")));
        assert!(ask("servers", Some("beta"), None, None));
        assert!(ask("servers", Some("gamma"), Some("root"), Some("any")));
        assert!(!ask("servers", Some("gamma"), Some("ROOT"), None));
        assert!(ask("admins", Some("anyhost"), Some("alice"), None));
        assert!(!ask("admins", None, Some("bob"), Some("example.com")));
    }

    #[test]
    fn a_dash_names_nothing_and_a_later_definition_is_ignored() {
        assert!(!ask("servers", Some("beta"), Some("alice"), None));
        assert!(ask("servers", Some("beta"), Some("-"), None));
        assert!(!ask("servers", Some("never"), None, None));
        assert!(!ask("the", None, None, None));
    }

    #[test]
    fn nested_groups_are_searched_and_a_cycle_ends() {
        assert!(ask("everyone", Some("alpha"), None, Some("example.org")));
        assert!(ask("everyone", None, Some("alice"), None));
        assert!(!ask("everyone", Some("x"), Some("y"), None));
        assert!(ask("loop1", None, Some("carol"), None));
        assert!(!ask("loop1", None, Some("dave"), None));
        assert!(!ask("nosuchgroup", None, None, None));
    }
}
