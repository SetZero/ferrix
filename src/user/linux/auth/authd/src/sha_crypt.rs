//! The `$5$` and `$6$` password hashes: Ulrich Drepper's "Unix crypt using
//! SHA-256 and SHA-512" (2007), which glibc, musl and Ferrix's own C library
//! (`src/user/linux/ferrousli/src/crypt/sha2.rs`) all implement.
//!
//! `authd` reads them only to take a credential over: a seed written with
//! `openssl passwd -6` or `mkpasswd`, or a line lifted from another
//! machine's `/etc/shadow`. The first password that checks against one is
//! hashed again with Argon2id and the old hash is dropped (`docs/AUTH.md`
//! §4.1). This is hyprlock's check, written and tested against Drepper's
//! vectors on that stream's branch (`5f89d787`), moved here when hyprlock
//! stopped reading hashes itself.
//!
//! Only the check is here, not the making of a hash: `authd` compares
//! a typed password with an imported hash and never writes one.
//!
//! What is not here, and is refused rather than guessed at (see
//! [`Refused`]): MD5's `$1$`, DES, blowfish's `$2?$`, and yescrypt's `$y$`,
//! which is what Debian, Ubuntu, Fedora and Arch write today. A shadow file
//! copied from one of those checks no password, and says so.

use crate::sha::{Digest, Sha256, Sha512};

/// The alphabet password hashes are written in.
const B64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Rounds when a setting names none.
const DEFAULT_ROUNDS: u32 = 5000;
/// The fewest rounds a setting may ask for; fewer are raised to this.
const MIN_ROUNDS: u32 = 1000;
/// The most; more are lowered to this.
const MAX_ROUNDS: u32 = 999_999_999;
/// The longest salt; a longer one is cut here.
const SALT_MAX: usize = 16;

/// The order SHA-256's digest is written in, three bytes to four characters,
/// then bytes 31 and 30 as three.
const ORDER256: [[usize; 3]; 10] = [
    [0, 10, 20],
    [21, 1, 11],
    [12, 22, 2],
    [3, 13, 23],
    [24, 4, 14],
    [15, 25, 5],
    [6, 16, 26],
    [27, 7, 17],
    [18, 28, 8],
    [9, 19, 29],
];

/// The order SHA-512's digest is written in, then byte 63 as two characters.
const ORDER512: [[usize; 3]; 21] = [
    [0, 21, 42],
    [22, 43, 1],
    [44, 2, 23],
    [3, 24, 45],
    [25, 46, 4],
    [47, 5, 26],
    [6, 27, 48],
    [28, 49, 7],
    [50, 8, 29],
    [9, 30, 51],
    [31, 52, 10],
    [53, 11, 32],
    [12, 33, 54],
    [34, 55, 13],
    [56, 14, 35],
    [15, 36, 57],
    [37, 58, 16],
    [59, 17, 38],
    [18, 39, 60],
    [40, 61, 19],
    [62, 20, 41],
];

/// Why a stored hash could not be checked at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// A hash of a kind this does not compute, named by its prefix.
    Kind(String),
    /// A `$5$` or `$6$` hash that is not well formed.
    Malformed,
}

/// Whether `password` is the one `stored` is the hash of.
///
/// # Errors
///
/// [`Refused`] for a hash this cannot check, which is neither a match nor a
/// mismatch: the caller says it could not authenticate, as PAM says
/// `pam_authenticate failed` rather than `Authentication failed`.
pub(crate) fn verify(password: &[u8], stored: &str) -> Result<bool, Refused> {
    let computed = if let Some(setting) = stored.strip_prefix("$6$") {
        hash::<Sha512>("6", password, setting, &ORDER512)?
    } else if let Some(setting) = stored.strip_prefix("$5$") {
        hash::<Sha256>("5", password, setting, &ORDER256)?
    } else {
        let kind = match stored.get(..3) {
            Some(prefix) if prefix.starts_with('$') => prefix.trim_end_matches('$').to_owned(),
            _ => "DES".to_owned(),
        };
        return Err(Refused::Kind(kind));
    };
    Ok(same(computed.as_bytes(), stored.as_bytes()))
}

/// Compare without stopping at the first difference, so how long the check
/// takes says nothing about how much of a guess was right.
fn same(one: &[u8], other: &[u8]) -> bool {
    if one.len() != other.len() {
        return false;
    }
    one.iter()
        .zip(other)
        .fold(0u8, |differ, (a, b)| differ | (a ^ b))
        == 0
}

/// The whole hash string of `password` under `setting` (what follows
/// `$5$` or `$6$`).
fn hash<H: Digest>(
    id: &str,
    password: &[u8],
    setting: &str,
    order: &[[usize; 3]],
) -> Result<String, Refused> {
    let (rounds, rest) = match setting.strip_prefix("rounds=") {
        Some(after) => {
            let (digits, rest) = after.split_once('$').ok_or(Refused::Malformed)?;
            let count: u64 = digits.parse().map_err(|_| Refused::Malformed)?;
            let count = u32::try_from(count.clamp(u64::from(MIN_ROUNDS), u64::from(MAX_ROUNDS)))
                .unwrap_or(MAX_ROUNDS);
            (Some(count), rest)
        }
        None => (None, setting),
    };
    let salt_end = rest.find('$').unwrap_or(rest.len()).min(SALT_MAX);
    let salt = rest.get(..salt_end).ok_or(Refused::Malformed)?.as_bytes();
    let digest = rounds_of::<H>(password, salt, rounds.unwrap_or(DEFAULT_ROUNDS));
    let mut out = format!("${id}$");
    if let Some(count) = rounds {
        out.push_str(&format!("rounds={count}$"));
    }
    out.push_str(std::str::from_utf8(salt).map_err(|_| Refused::Malformed)?);
    out.push('$');
    let byte = |at: usize| u32::from(digest.get(at).copied().unwrap_or(0));
    for [a, b, c] in order {
        encode(&mut out, (byte(*a) << 16) | (byte(*b) << 8) | byte(*c), 4);
    }
    if H::LEN == 64 {
        encode(&mut out, byte(63), 2);
    } else {
        encode(&mut out, (byte(31) << 8) | byte(30), 3);
    }
    Ok(out)
}

/// Write the low `count` six-bit groups of `word`, least significant first.
fn encode(out: &mut String, mut word: u32, count: usize) {
    for _ in 0..count {
        let at = usize::try_from(word & 0x3f).unwrap_or(0);
        out.push(char::from(B64.get(at).copied().unwrap_or(b'.')));
        word >>= 6;
    }
}

/// Add `len` bytes of `digest` repeated.
fn repeated<H: Digest>(context: &mut H, digest: &[u8], len: usize) {
    let mut left = len;
    while left >= digest.len() && !digest.is_empty() {
        context.update(digest);
        left -= digest.len();
    }
    context.update(digest.get(..left).unwrap_or(&[]));
}

/// The digest after `rounds` rounds: steps 1 to 21 of Drepper's
/// specification.
fn rounds_of<H: Digest>(password: &[u8], salt: &[u8], rounds: u32) -> Vec<u8> {
    // B: password, salt, password.
    let mut b = H::new();
    b.update(password);
    b.update(salt);
    b.update(password);
    let b = b.finish();
    // A: password, salt, B repeated to the password's length, then for each
    // bit of that length B or the password.
    let mut a = H::new();
    a.update(password);
    a.update(salt);
    repeated(&mut a, &b, password.len());
    let mut length = password.len();
    while length > 0 {
        if length & 1 == 1 {
            a.update(&b);
        } else {
            a.update(password);
        }
        length >>= 1;
    }
    let a = a.finish();
    // P: the password hashed as many times as it has bytes, cut to its
    // length.
    let mut dp = H::new();
    for _ in 0..password.len() {
        dp.update(password);
    }
    let dp = dp.finish();
    let p = sequence(&dp, password.len());
    // S: the salt hashed 16 + A[0] times, cut to the salt's length.
    let mut ds = H::new();
    for _ in 0..16 + usize::from(a.first().copied().unwrap_or(0)) {
        ds.update(salt);
    }
    let ds = ds.finish();
    let s = sequence(&ds, salt.len());
    let mut c = a;
    for round in 0..rounds {
        let mut next = H::new();
        if round & 1 == 1 {
            next.update(&p);
        } else {
            next.update(&c);
        }
        if round % 3 != 0 {
            next.update(&s);
        }
        if round % 7 != 0 {
            next.update(&p);
        }
        if round & 1 == 1 {
            next.update(&c);
        } else {
            next.update(&p);
        }
        c = next.finish();
    }
    c
}

/// `digest` repeated to `len` bytes.
fn sequence(digest: &[u8], len: usize) -> Vec<u8> {
    digest.iter().copied().cycle().take(len).collect()
}

#[cfg(test)]
mod tests {
    use super::{Refused, verify};

    #[test]
    fn drepper_s_own_examples_check() {
        // From the specification's test vectors, and what `openssl passwd`
        // writes for the same inputs on the host.
        assert_eq!(
            verify(
                b"Hello world!",
                "$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5"
            ),
            Ok(true)
        );
        assert_eq!(
            verify(
                b"Hello world!",
                "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
            ),
            Ok(true)
        );
        assert_eq!(
            verify(
                b"Hello world!",
                "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v."
            ),
            Ok(true)
        );
        // The specification's: a rounds count below the minimum is raised
        // to it, and the hash says the count it used.
        assert_eq!(
            verify(
                b"the minimum number is still observed",
                "$5$rounds=1000$roundstoolow$yfvwcWrQ8l/K0DAWyuPMDNHpIVlTQebY9l/gL972bIC"
            ),
            Ok(true)
        );
    }

    #[test]
    fn a_wrong_password_does_not() {
        assert_eq!(
            verify(
                b"Hello world",
                "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
            ),
            Ok(false)
        );
    }

    #[test]
    fn hashes_it_does_not_compute_are_refused_by_kind() {
        assert_eq!(
            verify(b"x", "$y$j9T$abc$def"),
            Err(Refused::Kind("$y".to_owned()))
        );
        assert_eq!(
            verify(b"x", "$1$salt$hash"),
            Err(Refused::Kind("$1".to_owned()))
        );
        assert_eq!(
            verify(b"x", "abJnggxhB/yWI"),
            Err(Refused::Kind("DES".to_owned()))
        );
        assert_eq!(
            verify(b"x", "$6$rounds=many$salt$hash"),
            Err(Refused::Malformed)
        );
    }
}
