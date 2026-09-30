//! Cross-checks of the `printf` conversions against the host C library, which
//! the unit test binary links.
//!
//! Many `double`s, random bit patterns and boundary values, are formatted
//! with `%e`, `%f`, `%g` and `%a` at many precisions, flags and widths, by
//! this library's `snprintf` thunk and by glibc's, and the text must be the
//! same. Integers and pointers get the same treatment. A `long double` cannot
//! be passed from Rust, so a small assembly shim puts one where the calling
//! convention wants it -- on the stack for an x87 one, in q0 for AArch64's
//! binary128 -- and calls either `snprintf`. On ARMv7-A a `long double` is a
//! `double`, which the `double` tests cover.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test reports failure by panicking"
)]

use core::ffi::{c_char, c_int};
use std::ffi::CString;

unsafe extern "C" {
    #[link_name = "snprintf"]
    fn host_snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> c_int;
    #[link_name = "ferrousli_test_snprintf"]
    fn our_snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> c_int;
    #[cfg(not(target_arch = "arm"))]
    fn ferrousli_test_call_long_double(
        function: *const (),
        buf: *mut c_char,
        n: usize,
        fmt: *const c_char,
        value: *const [u8; 16],
    ) -> c_int;
}

// ferrousli_test_call_long_double(function, buf, n, fmt, value): calls
// function(buf, n, fmt, long double) with the 16 bytes at `value` as the
// long double, in the stack slot the psABI puts it in, and no vector
// registers.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_test_call_long_double,\"ax\",@progbits",
    ".globl ferrousli_test_call_long_double",
    "ferrousli_test_call_long_double:",
    "push %rbx",
    "sub $16, %rsp",
    "mov (%r8), %rax",
    "mov %rax, 0(%rsp)",
    "mov 8(%r8), %rax",
    "mov %rax, 8(%rsp)",
    "mov %rdi, %r11",
    "mov %rsi, %rdi",
    "mov %rdx, %rsi",
    "mov %rcx, %rdx",
    "xor %eax, %eax",
    "call *%r11",
    "add $16, %rsp",
    "pop %rbx",
    "ret",
    ".popsection",
    options(att_syntax),
);

// The same on AArch64: the long double goes in q0, the first vector register,
// and the call is a tail call, so the function returns to the test.
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".pushsection .text.ferrousli_test_call_long_double,\"ax\",@progbits",
    ".globl ferrousli_test_call_long_double",
    "ferrousli_test_call_long_double:",
    "ldr q0, [x4]",
    "mov x9, x0",
    "mov x0, x1",
    "mov x1, x2",
    "mov x2, x3",
    "br x9",
    ".popsection",
);

/// A buffer large enough for every format these tests use.
const BUF: usize = 8192;

/// The text and count one `snprintf` call produced.
fn text(buf: &[u8], n: c_int) -> (String, c_int) {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    (String::from_utf8_lossy(&buf[..end]).into_owned(), n)
}

/// Formats one `double` both ways.
fn both_double(fmt: &str, value: f64) -> ((String, c_int), (String, c_int)) {
    let fmt = CString::new(fmt).unwrap();
    let mut ours = vec![0_u8; BUF];
    let mut host = vec![0_u8; BUF];
    // SAFETY: the buffers hold `BUF` bytes and the format takes one double.
    let n = unsafe { our_snprintf(ours.as_mut_ptr().cast(), BUF, fmt.as_ptr(), value) };
    // SAFETY: as above.
    let m = unsafe { host_snprintf(host.as_mut_ptr().cast(), BUF, fmt.as_ptr(), value) };
    (text(&ours, n), text(&host, m))
}

/// Formats one 64-bit integer both ways.
fn both_word(fmt: &str, value: u64) -> ((String, c_int), (String, c_int)) {
    let fmt = CString::new(fmt).unwrap();
    let mut ours = vec![0_u8; BUF];
    let mut host = vec![0_u8; BUF];
    // SAFETY: the buffers hold `BUF` bytes and the format takes one word.
    let n = unsafe { our_snprintf(ours.as_mut_ptr().cast(), BUF, fmt.as_ptr(), value) };
    // SAFETY: as above.
    let m = unsafe { host_snprintf(host.as_mut_ptr().cast(), BUF, fmt.as_ptr(), value) };
    (text(&ours, n), text(&host, m))
}

/// Formats one `long double`, given as its 16 bytes, both ways.
#[cfg(not(target_arch = "arm"))]
fn both_long(fmt: &str, value: [u8; 16]) -> ((String, c_int), (String, c_int)) {
    let fmt = CString::new(fmt).unwrap();
    let mut ours = vec![0_u8; BUF];
    let mut host = vec![0_u8; BUF];
    let our_function = our_snprintf as *const ();
    let host_function = host_snprintf as *const ();
    // SAFETY: the shim calls the function with the buffer, its size, the
    // format and the long double, which is what the format takes.
    let n = unsafe {
        ferrousli_test_call_long_double(
            our_function,
            ours.as_mut_ptr().cast(),
            BUF,
            fmt.as_ptr(),
            &raw const value,
        )
    };
    // SAFETY: as above.
    let m = unsafe {
        ferrousli_test_call_long_double(
            host_function,
            host.as_mut_ptr().cast(),
            BUF,
            fmt.as_ptr(),
            &raw const value,
        )
    };
    (text(&ours, n), text(&host, m))
}

/// A small, fixed-seed generator, so failures reproduce.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Boundary `double`s.
fn boundary_doubles() -> Vec<f64> {
    let mut values = vec![
        0.0,
        -0.0,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::from_bits(0x000f_ffff_ffff_ffff),
        f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
        0.5,
        1.5,
        2.5,
        0.125,
        0.375,
        9.5,
        99.5,
        999_999.5,
        1e23,
        1e22,
        5e-324,
        0.1,
        0.2,
        0.3,
        1.0 / 3.0,
        2.0_f64.powi(53) - 1.0,
        2.0_f64.powi(63),
        2.0_f64.powi(64),
        123_456_789.0,
        9_007_199_254_740_993.0,
        1.000_000_000_000_000_2,
        0.999_999_999_999_999_9,
    ];
    for exp in -30..=30 {
        values.push(10.0_f64.powi(exp));
        values.push(2.0_f64.powi(exp * 30));
    }
    values
}

/// Every flag set the conversions are tried with.
const FLAGS: [&str; 8] = ["", "+", " ", "#", "0", "-", "+#0", "- #"];

/// Whether `host` is glibc's known misprint of `ours` for `%#g`: when
/// rounding carries into a new power of ten and the exponent form is chosen,
/// glibc 2.43 drops the zeros that `#` must keep, printing `1.e+06` for `%#g`
/// of 999999.5. C requires `1.00000e+06`, which musl prints too.
fn glibc_alt_g_carry(fmt: &str, ours: &str, host: &str) -> bool {
    if !fmt.contains('#') || !fmt.ends_with(['g', 'G']) {
        return false;
    }
    let Some(dot) = ours.find("1.0") else {
        return false;
    };
    let zeros = ours[dot + 2..].chars().take_while(|c| *c == '0').count();
    let after = &ours[dot + 2 + zeros..];
    after.starts_with(['e', 'E']) && format!("{}{after}", &ours[..dot + 2]).trim() == host.trim()
}

fn compare(fmt: &str, ours: (String, c_int), host: (String, c_int), what: &str) {
    if ours != host && glibc_alt_g_carry(fmt, &ours.0, &host.0) {
        return;
    }
    assert_eq!(ours, host, "{fmt} of {what}");
}

#[test]
fn doubles_match_glibc_at_many_precisions_flags_and_widths() {
    if !crate::host_glibc::is_recorded() {
        return;
    }
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut values = boundary_doubles();
    for _ in 0..3000 {
        values.push(f64::from_bits(random.next()));
    }
    for _ in 0..1000 {
        // Exactly representable decimals with ties, and values near powers
        // of ten.
        let integer = random.below(2_000_000) as f64;
        let scale = 2.0_f64.powi(-(random.below(12) as i32));
        values.push(integer * scale);
    }
    for value in values {
        let what = format!("{value:e} ({:#x})", value.to_bits());
        for conversion in ['e', 'f', 'g', 'a', 'E', 'G', 'A'] {
            let flags = FLAGS[random.below(FLAGS.len() as u64) as usize];
            let width = if random.below(3) == 0 { "25" } else { "" };
            let precision = match conversion {
                'f' => random.below(40),
                'a' | 'A' => random.below(18),
                _ => random.below(45),
            };
            let fmt = format!("%{flags}{width}.{precision}{conversion}");
            let (ours, host) = both_double(&fmt, value);
            compare(&fmt, ours, host, &what);
            let fmt = format!("%{flags}{width}{conversion}");
            let (ours, host) = both_double(&fmt, value);
            compare(&fmt, ours, host, &what);
        }
    }
}

#[test]
fn subnormal_and_huge_doubles_match_glibc_at_long_precisions() {
    if !crate::host_glibc::is_recorded() {
        return;
    }
    let values = [
        f64::from_bits(1),
        f64::from_bits(3),
        f64::from_bits(0x000f_ffff_ffff_ffff),
        f64::MIN_POSITIVE,
        f64::MAX,
        1e308,
        1e-300,
    ];
    for value in values {
        for fmt in [
            "%.1100e", "%.1080f", "%.1100g", "%.0f", "%#.1070g", "%.767e",
        ] {
            let (ours, host) = both_double(fmt, value);
            compare(fmt, ours, host, &format!("{value:e}"));
        }
    }
}

#[test]
fn integers_and_pointers_match_glibc() {
    if !crate::host_glibc::is_recorded() {
        return;
    }
    let mut random = Random(0x2545_f491_4f6c_dd1d);
    let lengths = ["hh", "h", "", "l", "ll", "j", "z", "t"];
    let integer_flags = ["", "+", " ", "#", "0", "-", "-+ #"];
    for round in 0..20000 {
        let value = match round % 4 {
            0 => random.next(),
            1 => random.below(1000),
            2 => random.next() >> random.below(64),
            _ => (random.below(3) as i64 - 1) as u64,
        };
        let length = lengths[random.below(lengths.len() as u64) as usize];
        let flags = integer_flags[random.below(integer_flags.len() as u64) as usize];
        let conversion = ['d', 'i', 'o', 'u', 'x', 'X'][random.below(6) as usize];
        let width = match random.below(3) {
            0 => String::new(),
            _ => random.below(30).to_string(),
        };
        let precision = match random.below(3) {
            0 => String::new(),
            1 => ".".to_owned(),
            _ => format!(".{}", random.below(25)),
        };
        let fmt = format!("%{flags}{width}{precision}{length}{conversion}");
        let (ours, host) = both_word(&fmt, value);
        compare(&fmt, ours, host, &format!("{value:#x}"));
    }
    for value in [0_u64, 1, 0x1234, u64::MAX, 0x7fff_ffff_ffff] {
        for fmt in ["%p", "%20p", "%-20p|", "%+p", "% p", "%020p"] {
            let (ours, host) = both_word(fmt, value);
            compare(fmt, ours, host, &format!("{value:#x}"));
        }
    }
}

/// The 16 bytes of an x87 `long double`.
#[cfg(target_arch = "x86_64")]
fn long_double(mantissa: u64, sign_exponent: u16) -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
    bytes[8..10].copy_from_slice(&sign_exponent.to_le_bytes());
    bytes
}

#[cfg(target_arch = "x86_64")]
#[test]
fn long_doubles_match_glibc() {
    if !crate::host_glibc::is_recorded() {
        return;
    }
    let mut random = Random(0x1405_7b7e_f767_814f);
    let mut values = vec![
        long_double(0, 0),
        long_double(0, 0x8000),
        long_double(1, 0),
        long_double(0x7fff_ffff_ffff_ffff, 0),
        long_double(1 << 63, 1),
        long_double(u64::MAX, 0x7ffe),
        long_double(1 << 63, 0x7fff),
        long_double(1 << 63, 0xffff),
        long_double(0xc000_0000_0000_0000, 0x7fff),
        long_double(0xc000_0000_0000_0000, 0xffff),
        long_double(1 << 63, 16383),
        long_double(0xc000_0000_0000_0000, 16383),
        long_double(0xa000_0000_0000_0000, 16384),
        long_double(0xf800_0000_0000_0000, 16386),
    ];
    for _ in 0..3000 {
        let sign = if random.below(2) == 0 { 0 } else { 0x8000 };
        let (mantissa, exponent) = match random.below(8) {
            // Subnormal.
            0 => (random.next() >> 1, 0),
            // Near one, where %f is short.
            1..=3 => (
                random.next() | 1 << 63,
                16383 - 60 + random.below(120) as u16,
            ),
            // Anywhere.
            _ => (random.next() | 1 << 63, 1 + random.below(0x7ffe) as u16),
        };
        values.push(long_double(mantissa, sign | exponent));
    }
    for value in values {
        let what = format!("{:x?}", &value[..10]);
        for conversion in ['e', 'f', 'g', 'a', 'G', 'A'] {
            let precision = match conversion {
                'f' => random.below(25),
                'a' | 'A' => random.below(18),
                _ => random.below(40),
            };
            let flags = FLAGS[random.below(FLAGS.len() as u64) as usize];
            let fmt = format!("%{flags}.{precision}L{conversion}");
            let (ours, host) = both_long(&fmt, value);
            compare(&fmt, ours, host, &what);
            let fmt = format!("%{flags}L{conversion}");
            let (ours, host) = both_long(&fmt, value);
            compare(&fmt, ours, host, &what);
        }
    }
    for value in [long_double(1, 0), long_double(u64::MAX, 0x7ffe)] {
        for fmt in ["%.5000Le", "%.0Lf", "%.4990Lg"] {
            let (ours, host) = both_long(fmt, value);
            compare(fmt, ours, host, "an extreme long double");
        }
    }
}

/// The 16 bytes of a binary128 `long double`: sign, 15-bit biased exponent,
/// and the 112-bit fraction.
#[cfg(target_arch = "aarch64")]
fn quad(negative: bool, exponent: u16, fraction: u128) -> [u8; 16] {
    let bits = u128::from(negative) << 127
        | u128::from(exponent & 0x7fff) << 112
        | (fraction & ((1 << 112) - 1));
    bits.to_le_bytes()
}

#[cfg(target_arch = "aarch64")]
#[test]
fn quad_long_doubles_match_glibc() {
    if !crate::host_glibc::is_recorded() {
        return;
    }
    let mut random = Random(0x6a09_e667_f3bc_c908);
    let mut values = vec![
        quad(false, 0, 0),
        quad(true, 0, 0),
        quad(false, 0, 1),
        quad(false, 0, (1 << 112) - 1),
        quad(false, 1, 0),
        quad(false, 0x7ffe, (1 << 112) - 1),
        quad(false, 0x7fff, 0),
        quad(true, 0x7fff, 0),
        quad(false, 0x7fff, 1 << 111),
        quad(true, 0x7fff, 1),
        quad(false, 16383, 0),
        quad(false, 16383, 1 << 111),
        quad(false, 16384, 1 << 110),
        quad(true, 16386, 0xf << 108),
    ];
    for _ in 0..3000 {
        let negative = random.below(2) == 1;
        let fraction = (u128::from(random.next()) << 64 | u128::from(random.next())) >> 16;
        let exponent = match random.below(8) {
            0 => 0,
            1..=3 => 16383 - 60 + random.below(120) as u16,
            _ => 1 + random.below(0x7ffe) as u16,
        };
        values.push(quad(negative, exponent, fraction));
    }
    for value in values {
        let what = format!("{:#034x}", u128::from_le_bytes(value));
        for conversion in ['e', 'f', 'g', 'a', 'G', 'A'] {
            let precision = match conversion {
                'f' => random.below(25),
                'a' | 'A' => random.below(32),
                _ => random.below(40),
            };
            let flags = FLAGS[random.below(FLAGS.len() as u64) as usize];
            let fmt = format!("%{flags}.{precision}L{conversion}");
            let (ours, host) = both_long(&fmt, value);
            compare(&fmt, ours, host, &what);
            let fmt = format!("%{flags}L{conversion}");
            let (ours, host) = both_long(&fmt, value);
            compare(&fmt, ours, host, &what);
        }
    }
    for value in [quad(false, 0, 1), quad(false, 0x7ffe, (1 << 112) - 1)] {
        for fmt in ["%.12000Le", "%.0Lf", "%.11990Lg"] {
            let (ours, host) = both_long(fmt, value);
            compare(fmt, ours, host, "an extreme long double");
        }
    }
}
