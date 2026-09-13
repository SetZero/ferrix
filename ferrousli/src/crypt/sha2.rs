//! SHA-256 and SHA-512, and the `$5$` and `$6$` password hashes built on them.
//!
//! The digests follow FIPS 180-4. The password hashes follow Ulrich Drepper's
//! SHA-crypt specification in the form musl 1.2.5's `crypt_sha256.c` and
//! `crypt_sha512.c` implement it (MIT), including musl's limits, which the
//! original design does not have: a key of at most 256 bytes, a salt of at
//! most 16, and `rounds=` between 1000 and 9999999. A `rounds=` that is empty,
//! begins with anything but a digit, is unterminated, or asks for more than
//! the maximum is refused rather than read as part of the salt, so that a hash
//! never depends on the host's `ULONG_MAX`. A salt holding a newline or a
//! colon is refused, because `/etc/shadow` is line- and colon-separated.
//!
//! musl hashes a known key on every call and compares it, both as a self test
//! and to leave known bytes on the stack it used. Those vectors are unit tests
//! here instead.

use super::{Writer, byte};

/// The longest key, as musl limits it against denial of service. The cost is
/// quadratic in the key's length.
const KEY_MAX: usize = 256;
/// The longest salt the specification allows.
const SALT_MAX: usize = 16;
/// The rounds used when the setting does not say.
const ROUNDS_DEFAULT: u32 = 5000;
/// The fewest rounds; a smaller number is raised to it.
const ROUNDS_MIN: u32 = 1000;
/// The most rounds, as musl limits it; a larger number is refused.
const ROUNDS_MAX: u32 = 9_999_999;

/// A SHA-2 digest, as the password hash uses one.
pub(super) trait Sha: Sized {
    /// The digest: 32 bytes for SHA-256, 64 for SHA-512.
    type Digest: AsRef<[u8]> + Copy;

    /// A state with nothing added.
    fn new() -> Self;
    /// Adds `data` to what is hashed.
    fn update(&mut self, data: &[u8]);
    /// The digest of everything added.
    fn finish(self) -> Self::Digest;
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

/// SHA-256's initial state, FIPS 180-4 §5.3.3.
const H256: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// SHA-256's round constants, FIPS 180-4 §4.2.2.
const K256: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// A SHA-256 state: the bytes added, the chaining value, and a partial block.
#[derive(Debug)]
pub(super) struct Sha256 {
    /// How many bytes have been added.
    len: u64,
    /// The chaining value.
    h: [u32; 8],
    /// The bytes of the block that is not yet full.
    buf: [u8; 64],
}

impl Sha for Sha256 {
    type Digest = [u8; 32];

    fn new() -> Self {
        Sha256 {
            len: 0,
            h: H256,
            buf: [0; 64],
        }
    }

    fn update(&mut self, data: &[u8]) {
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
            process256(&mut self.h, &block);
            rest = tail;
        }

        while let Some((block, tail)) = rest.split_at_checked(64) {
            process256(&mut self.h, block);
            rest = tail;
        }
        if let Some(slot) = self.buf.get_mut(..rest.len()) {
            slot.copy_from_slice(rest);
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.len % 64 != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());

        let mut digest = [0u8; 32];
        for (chunk, word) in digest.chunks_exact_mut(4).zip(self.h.iter()) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

/// Mixes one 64-byte block into `h`.
fn process256(h: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (word, chunk) in w.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(chunk.try_into().unwrap_or([0; 4]));
    }
    for i in 16..64 {
        let at = |k: usize| w.get(k).copied().unwrap_or(0);
        let r0 = at(i - 15).rotate_right(7) ^ at(i - 15).rotate_right(18) ^ (at(i - 15) >> 3);
        let r1 = at(i - 2).rotate_right(17) ^ at(i - 2).rotate_right(19) ^ (at(i - 2) >> 10);
        let value = r1
            .wrapping_add(at(i - 7))
            .wrapping_add(r0)
            .wrapping_add(at(i - 16));
        if let Some(slot) = w.get_mut(i) {
            *slot = value;
        }
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for (k, word) in K256.iter().zip(w.iter()) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = g ^ (e & (f ^ g));
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*k)
            .wrapping_add(*word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) | (c & (a | b));
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *slot = slot.wrapping_add(value);
    }
}

// ---------------------------------------------------------------------------
// SHA-512
// ---------------------------------------------------------------------------

/// SHA-512's initial state, FIPS 180-4 §5.3.5.
const H512: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

/// SHA-512's round constants, FIPS 180-4 §4.2.3.
const K512: [u64; 80] = [
    0x428a_2f98_d728_ae22,
    0x7137_4491_23ef_65cd,
    0xb5c0_fbcf_ec4d_3b2f,
    0xe9b5_dba5_8189_dbbc,
    0x3956_c25b_f348_b538,
    0x59f1_11f1_b605_d019,
    0x923f_82a4_af19_4f9b,
    0xab1c_5ed5_da6d_8118,
    0xd807_aa98_a303_0242,
    0x1283_5b01_4570_6fbe,
    0x2431_85be_4ee4_b28c,
    0x550c_7dc3_d5ff_b4e2,
    0x72be_5d74_f27b_896f,
    0x80de_b1fe_3b16_96b1,
    0x9bdc_06a7_25c7_1235,
    0xc19b_f174_cf69_2694,
    0xe49b_69c1_9ef1_4ad2,
    0xefbe_4786_384f_25e3,
    0x0fc1_9dc6_8b8c_d5b5,
    0x240c_a1cc_77ac_9c65,
    0x2de9_2c6f_592b_0275,
    0x4a74_84aa_6ea6_e483,
    0x5cb0_a9dc_bd41_fbd4,
    0x76f9_88da_8311_53b5,
    0x983e_5152_ee66_dfab,
    0xa831_c66d_2db4_3210,
    0xb003_27c8_98fb_213f,
    0xbf59_7fc7_beef_0ee4,
    0xc6e0_0bf3_3da8_8fc2,
    0xd5a7_9147_930a_a725,
    0x06ca_6351_e003_826f,
    0x1429_2967_0a0e_6e70,
    0x27b7_0a85_46d2_2ffc,
    0x2e1b_2138_5c26_c926,
    0x4d2c_6dfc_5ac4_2aed,
    0x5338_0d13_9d95_b3df,
    0x650a_7354_8baf_63de,
    0x766a_0abb_3c77_b2a8,
    0x81c2_c92e_47ed_aee6,
    0x9272_2c85_1482_353b,
    0xa2bf_e8a1_4cf1_0364,
    0xa81a_664b_bc42_3001,
    0xc24b_8b70_d0f8_9791,
    0xc76c_51a3_0654_be30,
    0xd192_e819_d6ef_5218,
    0xd699_0624_5565_a910,
    0xf40e_3585_5771_202a,
    0x106a_a070_32bb_d1b8,
    0x19a4_c116_b8d2_d0c8,
    0x1e37_6c08_5141_ab53,
    0x2748_774c_df8e_eb99,
    0x34b0_bcb5_e19b_48a8,
    0x391c_0cb3_c5c9_5a63,
    0x4ed8_aa4a_e341_8acb,
    0x5b9c_ca4f_7763_e373,
    0x682e_6ff3_d6b2_b8a3,
    0x748f_82ee_5def_b2fc,
    0x78a5_636f_4317_2f60,
    0x84c8_7814_a1f0_ab72,
    0x8cc7_0208_1a64_39ec,
    0x90be_fffa_2363_1e28,
    0xa450_6ceb_de82_bde9,
    0xbef9_a3f7_b2c6_7915,
    0xc671_78f2_e372_532b,
    0xca27_3ece_ea26_619c,
    0xd186_b8c7_21c0_c207,
    0xeada_7dd6_cde0_eb1e,
    0xf57d_4f7f_ee6e_d178,
    0x06f0_67aa_7217_6fba,
    0x0a63_7dc5_a2c8_98a6,
    0x113f_9804_bef9_0dae,
    0x1b71_0b35_131c_471b,
    0x28db_77f5_2304_7d84,
    0x32ca_ab7b_40c7_2493,
    0x3c9e_be0a_15c9_bebc,
    0x431d_67c4_9c10_0d4c,
    0x4cc5_d4be_cb3e_42b6,
    0x597f_299c_fc65_7e2a,
    0x5fcb_6fab_3ad6_faec,
    0x6c44_198c_4a47_5817,
];

/// A SHA-512 state, as [`Sha256`] but over 128-byte blocks.
#[derive(Debug)]
pub(super) struct Sha512 {
    /// How many bytes have been added.
    len: u64,
    /// The chaining value.
    h: [u64; 8],
    /// The bytes of the block that is not yet full.
    buf: [u8; 128],
}

impl Sha for Sha512 {
    type Digest = [u8; 64];

    fn new() -> Self {
        Sha512 {
            len: 0,
            h: H512,
            buf: [0; 128],
        }
    }

    fn update(&mut self, data: &[u8]) {
        let mut rest = data;
        let filled = usize::try_from(self.len % 128).unwrap_or(0);
        self.len = self.len.wrapping_add(data.len() as u64);

        if filled > 0 {
            let space = 128 - filled;
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
            process512(&mut self.h, &block);
            rest = tail;
        }

        while let Some((block, tail)) = rest.split_at_checked(128) {
            process512(&mut self.h, block);
            rest = tail;
        }
        if let Some(slot) = self.buf.get_mut(..rest.len()) {
            slot.copy_from_slice(rest);
        }
    }

    fn finish(mut self) -> [u8; 64] {
        // A message of 2^64 bits or more cannot be reached through this
        // interface, so the high half of the 128-bit length is always zero.
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.len % 128 != 112 {
            self.update(&[0]);
        }
        self.update(&0u64.to_be_bytes());
        self.update(&bits.to_be_bytes());

        let mut digest = [0u8; 64];
        for (chunk, word) in digest.chunks_exact_mut(8).zip(self.h.iter()) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

/// Mixes one 128-byte block into `h`.
fn process512(h: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u64; 80];
    for (word, chunk) in w.iter_mut().zip(block.chunks_exact(8)) {
        *word = u64::from_be_bytes(chunk.try_into().unwrap_or([0; 8]));
    }
    for i in 16..80 {
        let at = |k: usize| w.get(k).copied().unwrap_or(0);
        let r0 = at(i - 15).rotate_right(1) ^ at(i - 15).rotate_right(8) ^ (at(i - 15) >> 7);
        let r1 = at(i - 2).rotate_right(19) ^ at(i - 2).rotate_right(61) ^ (at(i - 2) >> 6);
        let value = r1
            .wrapping_add(at(i - 7))
            .wrapping_add(r0)
            .wrapping_add(at(i - 16));
        if let Some(slot) = w.get_mut(i) {
            *slot = value;
        }
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for (k, word) in K512.iter().zip(w.iter()) {
        let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
        let ch = g ^ (e & (f ^ g));
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*k)
            .wrapping_add(*word);
        let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
        let maj = (a & b) | (c & (a | b));
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *slot = slot.wrapping_add(value);
    }
}

// ---------------------------------------------------------------------------
// The password hashes
// ---------------------------------------------------------------------------

/// The order SHA-256's digest is written in, three bytes to four characters.
const PERM256: [[u8; 3]; 10] = [
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

/// The order SHA-512's digest is written in.
const PERM512: [[u8; 3]; 21] = [
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

/// `$5$`: the SHA-256 hash of `key` under `setting`, written to `out` with a
/// NUL. Returns the length without the NUL, or `None` if the setting is not
/// one this understands.
pub(super) fn sha256crypt(key: &[u8], setting: &[u8], out: &mut [u8]) -> Option<usize> {
    shacrypt::<Sha256>(b'5', key, setting, out, &PERM256, &[31, 30])
}

/// `$6$`: the SHA-512 hash of `key` under `setting`, as [`sha256crypt`].
pub(super) fn sha512crypt(key: &[u8], setting: &[u8], out: &mut [u8]) -> Option<usize> {
    shacrypt::<Sha512>(b'6', key, setting, out, &PERM512, &[63])
}

/// Adds the first `n` bytes of `digest` repeated to `ctx`.
fn repeat<H: Sha>(ctx: &mut H, n: usize, digest: &[u8]) {
    let mut left = n;
    while left > digest.len() {
        ctx.update(digest);
        left -= digest.len();
    }
    ctx.update(digest.get(..left).unwrap_or(&[]));
}

/// The rounds `setting` asks for, and the salt that follows.
///
/// The count is `None` when the setting does not name one, which is both the
/// default and the reason the output does not repeat it.
fn rounds_of(setting: &[u8]) -> Option<(Option<u32>, &[u8])> {
    let Some(digits) = setting.strip_prefix(b"rounds=".as_slice()) else {
        return Some((None, setting));
    };
    let mut count: u32 = 0;
    let mut rest = digits;
    let mut seen = 0;
    while let Some((digit @ b'0'..=b'9', tail)) = rest.split_first() {
        count = count
            .checked_mul(10)?
            .checked_add(u32::from(digit - b'0'))?;
        rest = tail;
        seen += 1;
    }
    if seen == 0 {
        return None;
    }
    let salt = rest.strip_prefix(b"$".as_slice())?;
    if count > ROUNDS_MAX {
        return None;
    }
    Some((Some(count.max(ROUNDS_MIN)), salt))
}

/// The salt at the front of `setting`: up to [`SALT_MAX`] bytes, ending at a
/// `$` or the end, and holding neither a newline nor a colon.
fn salt_of(setting: &[u8]) -> Option<&[u8]> {
    let mut len = 0;
    for &character in setting.iter().take(SALT_MAX) {
        if character == b'$' {
            break;
        }
        if character == b'\n' || character == b':' {
            return None;
        }
        len += 1;
    }
    setting.get(..len)
}

/// The body of both hashes: `tag` is `5` or `6`, and `perm` and `tail` say how
/// the digest is written out.
fn shacrypt<H: Sha>(
    tag: u8,
    key: &[u8],
    setting: &[u8],
    out: &mut [u8],
    perm: &[[u8; 3]],
    tail: &[u8],
) -> Option<usize> {
    if key.len() > KEY_MAX {
        return None;
    }
    let head = [b'$', tag, b'$'];
    let rest = setting.strip_prefix(head.as_slice())?;
    let (asked, rest) = rounds_of(rest)?;
    let salt = salt_of(rest)?;
    let rounds = asked.unwrap_or(ROUNDS_DEFAULT);

    // B = H(key salt key)
    let mut ctx = H::new();
    ctx.update(key);
    ctx.update(salt);
    ctx.update(key);
    let b = ctx.finish();

    // A = H(key salt repeated-B alternating-B-and-key)
    let mut ctx = H::new();
    ctx.update(key);
    ctx.update(salt);
    repeat(&mut ctx, key.len(), b.as_ref());
    let mut bit = key.len();
    while bit > 0 {
        if bit & 1 == 1 {
            ctx.update(b.as_ref());
        } else {
            ctx.update(key);
        }
        bit >>= 1;
    }
    let mut a = ctx.finish();

    // DP = H(key repeated as many times as it is long). Quadratic in the key.
    let mut ctx = H::new();
    for _ in 0..key.len() {
        ctx.update(key);
    }
    let dp = ctx.finish();

    // DS = H(salt repeated 16 + A[0] times)
    let mut ctx = H::new();
    for _ in 0..16 + u16::from(byte(a.as_ref(), 0)) {
        ctx.update(salt);
    }
    let ds = ctx.finish();

    // A = f(A, DP, DS), `rounds` times.
    for round in 0..rounds {
        let mut ctx = H::new();
        if round % 2 == 1 {
            repeat(&mut ctx, key.len(), dp.as_ref());
        } else {
            ctx.update(a.as_ref());
        }
        if round % 3 != 0 {
            ctx.update(ds.as_ref().get(..salt.len()).unwrap_or(&[]));
        }
        if round % 7 != 0 {
            repeat(&mut ctx, key.len(), dp.as_ref());
        }
        if round % 2 == 1 {
            ctx.update(a.as_ref());
        } else {
            repeat(&mut ctx, key.len(), dp.as_ref());
        }
        a = ctx.finish();
    }

    // $5$rounds=n$salt$hash, the rounds only if the setting named them.
    let mut writer = Writer::new(out);
    writer.push(&head)?;
    if let Some(count) = asked {
        writer.push(b"rounds=")?;
        writer.push_uint(count)?;
        writer.push(b"$")?;
    }
    writer.push(salt)?;
    writer.push(b"$")?;
    for group in perm {
        let [first, second, third] = *group;
        let value = (u32::from(byte(a.as_ref(), first)) << 16)
            | (u32::from(byte(a.as_ref(), second)) << 8)
            | u32::from(byte(a.as_ref(), third));
        writer.push_base64(value, 4)?;
    }
    let mut value = 0u32;
    for &index in tail {
        value = (value << 8) | u32::from(byte(a.as_ref(), index));
    }
    writer.push_base64(value, tail.len() + 1)?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest256(data: &[u8]) -> [u8; 32] {
        let mut ctx = Sha256::new();
        ctx.update(data);
        ctx.finish()
    }

    fn digest512(data: &[u8]) -> [u8; 64] {
        let mut ctx = Sha512::new();
        ctx.update(data);
        ctx.finish()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn hash(which: u8, key: &[u8], setting: &[u8]) -> Option<String> {
        let mut out = [0u8; 128];
        let len = if which == b'5' {
            sha256crypt(key, setting, &mut out)?
        } else {
            sha512crypt(key, setting, &mut out)?
        };
        Some(String::from_utf8_lossy(out.get(..len)?).into_owned())
    }

    #[test]
    fn sha256_matches_fips_180_4() {
        assert_eq!(
            hex(&digest256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&digest256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&digest256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha512_matches_fips_180_4() {
        assert_eq!(
            hex(&digest512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            hex(&digest512(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
    }

    #[test]
    fn a_long_message_spans_blocks() {
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&digest256(&million)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn the_hashes_match_musls_vectors() {
        // musl's own self-test key and settings, from `crypt_sha256.c` and
        // `crypt_sha512.c` (MIT).
        let key = b"Xy01@#\x01\x02\x80\x7f\xff\r\n\x81\t !";
        assert_eq!(
            hash(b'5', key, b"$5$rounds=1234$abc0123456789$").as_deref(),
            Some("$5$rounds=1234$abc0123456789$3VfDjPt05VHFn47C/ojFZ6KRPYrOjj1lLbH.dkF3bZ6")
        );
        assert_eq!(
            hash(b'6', key, b"$6$rounds=1234$abc0123456789$").as_deref(),
            Some(
                "$6$rounds=1234$abc0123456789$BCpt8zLrc/RcyuXmCDOE1ALqMXB2MH6n1g891HhFj8.w7\
                 LxGv.FTkqq6Vxc/km3Y0jE0j24jY5PIv/oOu6reg1"
            )
        );
    }

    #[test]
    fn the_default_is_5000_rounds_and_is_not_written_out() {
        // Drepper's specification, example 1 for each hash.
        assert_eq!(
            hash(b'5', b"Hello world!", b"$5$saltstring").as_deref(),
            Some("$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5")
        );
        assert_eq!(
            hash(b'6', b"Hello world!", b"$6$saltstring").as_deref(),
            Some(
                "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJu\
                 esI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
            )
        );
    }

    #[test]
    fn rounds_are_clamped_below_and_refused_above() {
        // 10 rounds is raised to 1000, and the setting written out says so.
        let hashed = hash(b'5', b"password", b"$5$rounds=10$salt$").unwrap();
        assert!(hashed.starts_with("$5$rounds=1000$salt$"), "{hashed}");
        // More than musl's maximum is refused rather than clamped.
        assert_eq!(hash(b'5', b"password", b"$5$rounds=10000000$salt$"), None);
        // So is a count that is empty, unterminated, or not a number.
        assert_eq!(hash(b'5', b"password", b"$5$rounds=$salt$"), None);
        assert_eq!(hash(b'5', b"password", b"$5$rounds=99salt$"), None);
        assert_eq!(hash(b'5', b"password", b"$5$rounds=x99$salt$"), None);
    }

    #[test]
    fn a_salt_is_cut_and_checked() {
        // Only the first 16 bytes of a salt are used.
        let long = hash(b'5', b"password", b"$5$0123456789abcdefghij$").unwrap();
        let cut = hash(b'5', b"password", b"$5$0123456789abcdef$").unwrap();
        assert_eq!(long, cut);
        // A salt that would break /etc/shadow's format is refused.
        assert_eq!(hash(b'5', b"password", b"$5$bad:salt$"), None);
        assert_eq!(hash(b'5', b"password", b"$5$bad\nsalt$"), None);
    }

    #[test]
    fn another_hashs_setting_is_refused() {
        assert_eq!(hash(b'5', b"password", b"$6$salt$"), None);
        assert_eq!(hash(b'6', b"password", b"$5$salt$"), None);
        assert_eq!(hash(b'5', b"password", b"nosetting"), None);
    }

    #[test]
    fn a_key_longer_than_the_limit_is_refused() {
        let long = vec![b'x'; KEY_MAX + 1];
        assert_eq!(hash(b'5', &long, b"$5$salt$"), None);
        let limit = vec![b'x'; KEY_MAX];
        assert!(hash(b'5', &limit, b"$5$salt$").is_some());
    }
}
