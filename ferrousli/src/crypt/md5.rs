//! MD5, and the `$1$` password hash built on it.
//!
//! The digest follows RFC 1321. The password hash is Poul-Henning Kamp's,
//! in the form musl 1.2.5's `crypt_md5.c` implements it (MIT): a salt of at
//! most 8 bytes, 1000 iterations, and musl's key limit of 30000 bytes, which
//! the original design does not have.
//!
//! This hash is old and weak. It is here because `/etc/shadow` entries written
//! by other systems still carry it, and a login that cannot check one cannot
//! refuse it either.

use super::{Writer, byte};

/// The longest key, as musl limits it against denial of service.
const KEY_MAX: usize = 30000;
/// The longest salt the design allows.
const SALT_MAX: usize = 8;
/// How many times the digest is folded back into itself.
const ROUNDS: usize = 1000;

/// MD5's initial state, RFC 1321 §3.3.
const INIT: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

/// The rotations of each round, four to a quarter, RFC 1321 §3.4.
const SHIFTS: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];

/// MD5's additive constants: `floor(2^32 * abs(sin(i + 1)))`.
const TAB: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// An MD5 state: the bytes added, the chaining value, and a partial block.
#[derive(Debug)]
pub(super) struct Md5 {
    /// How many bytes have been added.
    len: u64,
    /// The chaining value.
    h: [u32; 4],
    /// The bytes of the block that is not yet full.
    buf: [u8; 64],
}

impl Md5 {
    /// A state with nothing added.
    pub(super) fn new() -> Self {
        Md5 {
            len: 0,
            h: INIT,
            buf: [0; 64],
        }
    }

    /// Adds `data` to what is hashed.
    pub(super) fn update(&mut self, data: &[u8]) {
        let mut rest = data;
        let filled = usize::try_from(self.len % 64).unwrap_or(0);
        self.len = self.len.wrapping_add(data.len() as u64);

        if filled > 0 {
            let space = 64 - filled;
            let Some((head, tail)) = rest.split_at_checked(space) else {
                if let Some(slot) = self.buf.get_mut(filled..filled + rest.len()) {
                    slot.copy_from_slice(rest);
                }
                return;
            };
            if let Some(slot) = self.buf.get_mut(filled..) {
                slot.copy_from_slice(head);
            }
            let block = self.buf;
            process(&mut self.h, &block);
            rest = tail;
        }

        while let Some((block, tail)) = rest.split_at_checked(64) {
            process(&mut self.h, block);
            rest = tail;
        }
        if let Some(slot) = self.buf.get_mut(..rest.len()) {
            slot.copy_from_slice(rest);
        }
    }

    /// The digest of everything added.
    pub(super) fn finish(mut self) -> [u8; 16] {
        // The length is counted in bits, and written little-endian, which is
        // where MD5 differs from the SHA-2 family beside its word order.
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.len % 64 != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_le_bytes());

        let mut digest = [0u8; 16];
        for (chunk, word) in digest.chunks_exact_mut(4).zip(self.h.iter()) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        digest
    }
}

/// Mixes one 64-byte block into `h`.
fn process(h: &mut [u32; 4], block: &[u8]) {
    let mut words = [0u32; 16];
    for (word, chunk) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_le_bytes(chunk.try_into().unwrap_or([0; 4]));
    }

    let [mut a, mut b, mut c, mut d] = *h;
    for round in 0..64 {
        let quarter = round / 16;
        let (mixed, word) = match quarter {
            0 => (d ^ (b & (c ^ d)), round),
            1 => (c ^ (d & (b ^ c)), (5 * round + 1) % 16),
            2 => (b ^ c ^ d, (3 * round + 5) % 16),
            _ => (c ^ (b | !d), (7 * round) % 16),
        };
        let shift = SHIFTS
            .get(quarter * 4 + round % 4)
            .copied()
            .unwrap_or_default();
        let sum = a
            .wrapping_add(mixed)
            .wrapping_add(TAB.get(round).copied().unwrap_or_default())
            .wrapping_add(words.get(word).copied().unwrap_or_default());
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(sum.rotate_left(shift));
    }
    for (slot, value) in h.iter_mut().zip([a, b, c, d]) {
        *slot = slot.wrapping_add(value);
    }
}

/// The order the digest is written in, three bytes to four characters.
const PERM: [[u8; 3]; 5] = [[0, 6, 12], [1, 7, 13], [2, 8, 14], [3, 9, 15], [4, 10, 5]];

/// `$1$`: the MD5 hash of `key` under `setting`, written to `out` with a NUL.
/// Returns the length without the NUL, or `None` if the setting is not one
/// this understands.
pub(super) fn md5crypt(key: &[u8], setting: &[u8], out: &mut [u8]) -> Option<usize> {
    if key.len() > KEY_MAX {
        return None;
    }
    let rest = setting.strip_prefix(b"$1$".as_slice())?;
    let mut len = 0;
    for &character in rest.iter().take(SALT_MAX) {
        if character == b'$' {
            break;
        }
        len += 1;
    }
    let salt = rest.get(..len)?;

    // md5(key salt key)
    let mut ctx = Md5::new();
    ctx.update(key);
    ctx.update(salt);
    ctx.update(key);
    let mut md = ctx.finish();

    // md5(key $1$ salt repeated-digest, then a bit per bit of the key's
    // length: a zero byte for a one, the key's first byte for a zero. The
    // zero byte comes from the digest, which this clears to get one.
    let mut ctx = Md5::new();
    ctx.update(key);
    ctx.update(b"$1$");
    ctx.update(salt);
    let mut left = key.len();
    while left > md.len() {
        ctx.update(&md);
        left -= md.len();
    }
    ctx.update(md.get(..left).unwrap_or(&[]));
    if let Some(first) = md.first_mut() {
        *first = 0;
    }
    let mut bit = key.len();
    while bit > 0 {
        if bit & 1 == 1 {
            ctx.update(md.get(..1).unwrap_or(&[]));
        } else {
            ctx.update(key.get(..1).unwrap_or(&[]));
        }
        bit >>= 1;
    }
    md = ctx.finish();

    // md = f(md, key, salt), a thousand times.
    for round in 0..ROUNDS {
        let mut ctx = Md5::new();
        if round % 2 == 1 {
            ctx.update(key);
        } else {
            ctx.update(&md);
        }
        if round % 3 != 0 {
            ctx.update(salt);
        }
        if round % 7 != 0 {
            ctx.update(key);
        }
        if round % 2 == 1 {
            ctx.update(&md);
        } else {
            ctx.update(key);
        }
        md = ctx.finish();
    }

    let mut writer = Writer::new(out);
    writer.push(b"$1$")?;
    writer.push(salt)?;
    writer.push(b"$")?;
    for group in &PERM {
        let [first, second, third] = *group;
        let value = (u32::from(byte(&md, first)) << 16)
            | (u32::from(byte(&md, second)) << 8)
            | u32::from(byte(&md, third));
        writer.push_base64(value, 4)?;
    }
    writer.push_base64(u32::from(byte(&md, 11)), 2)?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(data: &[u8]) -> String {
        let mut ctx = Md5::new();
        ctx.update(data);
        ctx.finish()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn hash(key: &[u8], setting: &[u8]) -> Option<String> {
        let mut out = [0u8; 64];
        let len = md5crypt(key, setting, &mut out)?;
        Some(String::from_utf8_lossy(out.get(..len)?).into_owned())
    }

    #[test]
    fn md5_matches_rfc_1321() {
        assert_eq!(digest(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(digest(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(digest(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            digest(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            digest(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn a_long_message_spans_blocks() {
        let million = vec![b'a'; 1_000_000];
        assert_eq!(digest(&million), "7707d6ae4e027c70eea2a935c2296f21");
    }

    #[test]
    fn the_hash_matches_musls_vector() {
        // musl's own self-test key and setting, from `crypt_md5.c` (MIT).
        let key = b"Xy01@#\x01\x02\x80\x7f\xff\r\n\x81\t !";
        assert_eq!(
            hash(key, b"$1$abcd0123$").as_deref(),
            Some("$1$abcd0123$9Qcg8DyviekV3tDGMZynJ1")
        );
    }

    #[test]
    fn the_hash_matches_openssl() {
        // `openssl passwd -1 -salt <salt> <key>`, on the host that wrote this.
        assert_eq!(
            hash(b"password", b"$1$saltsalt$").as_deref(),
            Some("$1$saltsalt$qjXMvbEw8oaL.CzflDtaK/")
        );
        assert_eq!(
            hash(b"", b"$1$abcdefgh$").as_deref(),
            Some("$1$abcdefgh$M55TzYaaccxVGbptZWaxX/")
        );
        assert_eq!(
            hash(b"Xy01", b"$1$abcd0123$").as_deref(),
            Some("$1$abcd0123$XUxfmvQxG01oLroyL6vWu0")
        );
    }

    #[test]
    fn a_salt_is_cut_at_eight_bytes() {
        let long = hash(b"password", b"$1$0123456789$").unwrap();
        let cut = hash(b"password", b"$1$01234567$").unwrap();
        assert_eq!(long, cut);
    }

    #[test]
    fn another_hashs_setting_is_refused() {
        assert_eq!(hash(b"password", b"$5$salt$"), None);
        assert_eq!(hash(b"password", b"nosetting"), None);
    }

    #[test]
    fn a_key_longer_than_the_limit_is_refused() {
        let long = vec![b'x'; KEY_MAX + 1];
        assert_eq!(hash(&long, b"$1$salt$"), None);
    }
}
