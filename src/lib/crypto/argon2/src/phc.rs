//! The PHC string a hash is stored as:
//! `$argon2id$v=19$m=65536,t=3,p=1$<salt>$<hash>`.
//!
//! This is the format the reference implementation's `argon2` tool,
//! libsodium and `RustCrypto`'s `password-hash` write: the algorithm, the
//! version in decimal, the three costs in the order `m`, `t`, `p`, then the
//! salt and the tag in standard base64 (RFC 4648 §4) with the padding left
//! off. It is read strictly: another algorithm, another version, the costs
//! in another order, an extra parameter, padding, or a character outside
//! the alphabet are each [`ParseError`]s. `authd` writes every string it
//! reads, so leniency would only ever accept a string somebody else made.

use crate::{Block, Error, Inputs, Params, equal, hash};

/// The longest salt or tag a stored string may carry.
pub const MAX_FIELD: usize = 64;

/// The shortest tag a stored string may carry, so a string cannot claim a
/// check of a few bits.
pub const MIN_TAG: usize = 16;

/// The standard base64 alphabet.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The version number as the string writes it: `0x13` is 19.
const VERSION_FIELD: &str = "v=19";

/// Why a string is not a stored Argon2id hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// It does not start `$argon2id$`.
    Algorithm,
    /// Its version is not `v=19`.
    Version,
    /// Its costs are not `m=…,t=…,p=…` with values RFC 9106 allows.
    Params,
    /// Its salt or tag is not unpadded base64 of an allowed length.
    Field,
    /// It has fields missing or left over.
    Shape,
}

/// A stored hash: the costs, the salt and the tag.
#[derive(Clone, PartialEq, Eq)]
pub struct Encoded {
    /// The costs it was made with.
    pub params: Params,
    salt: [u8; MAX_FIELD],
    salt_len: usize,
    tag: [u8; MAX_FIELD],
    tag_len: usize,
}

impl core::fmt::Debug for Encoded {
    /// The costs and the lengths; not the salt or the tag, which together
    /// are what an offline guesser needs.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Encoded")
            .field("params", &self.params)
            .field("salt", &self.salt_len)
            .field("tag", &self.tag_len)
            .finish()
    }
}

impl Encoded {
    /// A stored hash of `tag` under `params` and `salt`.
    ///
    /// # Errors
    ///
    /// [`ParseError::Field`] for a salt shorter than [`crate::MIN_SALT`] or
    /// a tag shorter than [`MIN_TAG`], or either longer than
    /// [`MAX_FIELD`]; [`ParseError::Params`] for costs RFC 9106 does not
    /// allow.
    pub fn new(params: Params, salt: &[u8], tag: &[u8]) -> Result<Encoded, ParseError> {
        let _ = params.blocks().map_err(|_| ParseError::Params)?;
        let mut encoded = Encoded {
            params,
            salt: [0; MAX_FIELD],
            salt_len: salt.len(),
            tag: [0; MAX_FIELD],
            tag_len: tag.len(),
        };
        let fits = |len: usize, min: usize| (min..=MAX_FIELD).contains(&len);
        if !fits(salt.len(), crate::MIN_SALT) || !fits(tag.len(), MIN_TAG) {
            return Err(ParseError::Field);
        }
        encoded
            .salt
            .get_mut(..salt.len())
            .ok_or(ParseError::Field)?
            .copy_from_slice(salt);
        encoded
            .tag
            .get_mut(..tag.len())
            .ok_or(ParseError::Field)?
            .copy_from_slice(tag);
        Ok(encoded)
    }

    /// The salt.
    #[must_use]
    pub fn salt(&self) -> &[u8] {
        self.salt.get(..self.salt_len).unwrap_or(&[])
    }

    /// The tag.
    #[must_use]
    pub fn tag(&self) -> &[u8] {
        self.tag.get(..self.tag_len).unwrap_or(&[])
    }

    /// Read a stored hash.
    ///
    /// # Errors
    ///
    /// [`ParseError`], saying which part was wrong.
    pub fn parse(text: &str) -> Result<Encoded, ParseError> {
        let mut fields = text.split('$');
        if fields.next() != Some("") {
            return Err(ParseError::Shape);
        }
        if fields.next() != Some("argon2id") {
            return Err(ParseError::Algorithm);
        }
        if fields.next() != Some(VERSION_FIELD) {
            return Err(ParseError::Version);
        }
        let params = parse_params(fields.next().ok_or(ParseError::Shape)?)?;
        let mut salt = [0_u8; MAX_FIELD];
        let salt_len = decode(fields.next().ok_or(ParseError::Shape)?, &mut salt)?;
        let mut tag = [0_u8; MAX_FIELD];
        let tag_len = decode(fields.next().ok_or(ParseError::Shape)?, &mut tag)?;
        if fields.next().is_some() {
            return Err(ParseError::Shape);
        }
        Encoded::new(
            params,
            salt.get(..salt_len).unwrap_or(&[]),
            tag.get(..tag_len).unwrap_or(&[]),
        )
    }

    /// Whether `password` is the one this hash was made from, compared in
    /// constant time. `memory` must hold at least [`Params::blocks`] blocks,
    /// and is zeroed after.
    ///
    /// # Errors
    ///
    /// [`Error`] when the hash cannot be computed: too little memory, or
    /// costs RFC 9106 does not allow.
    pub fn verify(&self, password: &[u8], memory: &mut [Block]) -> Result<bool, Error> {
        let mut computed = [0_u8; MAX_FIELD];
        let out = computed.get_mut(..self.tag_len).ok_or(Error::Output)?;
        hash(
            &self.params,
            &Inputs {
                password,
                salt: self.salt(),
                secret: &[],
                associated: &[],
            },
            memory,
            out,
        )?;
        let same = equal(out, self.tag());
        computed.fill(0);
        let _ = core::hint::black_box(&computed);
        Ok(same)
    }
}

impl core::fmt::Display for Encoded {
    /// The string form, as [`Encoded::parse`] reads it.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "$argon2id${VERSION_FIELD}$m={},t={},p={}$",
            self.params.memory_kib, self.params.passes, self.params.lanes
        )?;
        encode(self.salt(), f)?;
        f.write_str("$")?;
        encode(self.tag(), f)
    }
}

/// `m=…,t=…,p=…`, in that order and no other.
fn parse_params(text: &str) -> Result<Params, ParseError> {
    let mut parts = text.split(',');
    let mut value = |key: &str| -> Result<u32, ParseError> {
        let part = parts.next().ok_or(ParseError::Params)?;
        let digits = part
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or(ParseError::Params)?;
        // Decimal with no sign and no leading zero, as the encoders write.
        if digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
        {
            return Err(ParseError::Params);
        }
        digits.parse().map_err(|_| ParseError::Params)
    };
    let params = Params {
        memory_kib: value("m")?,
        passes: value("t")?,
        lanes: value("p")?,
    };
    if parts.next().is_some() {
        return Err(ParseError::Params);
    }
    let _ = params.blocks().map_err(|_| ParseError::Params)?;
    Ok(params)
}

/// Write `bytes` in unpadded standard base64.
fn encode(bytes: &[u8], f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    let symbol = |index: u32| {
        ALPHABET
            .get(index as usize & 63)
            .copied()
            .map_or('A', char::from)
    };
    for chunk in bytes.chunks(3) {
        let mut group = [0_u8; 3];
        for (slot, byte) in group.iter_mut().zip(chunk) {
            *slot = *byte;
        }
        let [a, b, c] = group;
        let word = (u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(c);
        let symbols = chunk.len() + 1;
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i < symbols {
                core::fmt::Write::write_char(f, symbol(word >> shift))?;
            }
        }
    }
    Ok(())
}

/// Read unpadded standard base64 into `out`, giving the bytes read.
fn decode(text: &str, out: &mut [u8; MAX_FIELD]) -> Result<usize, ParseError> {
    let value = |symbol: u8| -> Result<u32, ParseError> {
        ALPHABET
            .iter()
            .position(|&s| s == symbol)
            .map(|p| p as u32)
            .ok_or(ParseError::Field)
    };
    let symbols = text.as_bytes();
    // A group of four symbols is three bytes; one left over is no byte at
    // all, which no encoder writes.
    if symbols.len() % 4 == 1 {
        return Err(ParseError::Field);
    }
    let mut written = 0;
    for group in symbols.chunks(4) {
        let mut word = 0_u32;
        for (i, &symbol) in group.iter().enumerate() {
            word |= value(symbol)? << (18 - 6 * i as u32);
        }
        let bytes = group.len() - 1;
        let unused = word & ((1_u32 << (8 * (3 - bytes) as u32)) - 1);
        // The bits past the last byte must be zero, so one string is the
        // only spelling of its bytes.
        if bytes < 3 && unused != 0 {
            return Err(ParseError::Field);
        }
        for (i, shift) in [16, 8, 0].into_iter().enumerate().take(bytes) {
            let slot = out.get_mut(written + i).ok_or(ParseError::Field)?;
            *slot = (word >> shift) as u8;
        }
        written += bytes;
    }
    Ok(written)
}
