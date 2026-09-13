//! Tests for the floating-point parsers.
//!
//! The tables in `tests/data/float` hold hard cases: exact halfway points
//! between adjacent numbers and the same nudged up and down by far less than
//! a unit in the last place, the largest finite values and the overflow
//! threshold, the subnormal boundaries, and random inputs of every length and
//! exponent. Each row was computed with exact rational arithmetic and agreed
//! with the host glibc's `strtof`, `strtod`, `strtold` or `strtof128` before
//! it was written down. The `errno` column is musl's decimal rule.

use super::*;
use crate::float::{BINARY128, X87_EXTENDED};
use std::ffi::CString;

/// Checks every row of a table: `input`, bit pattern, and whether `ERANGE`.
fn check_table(table: &str, format: &Format) {
    let mut rows = 0;
    for line in table.lines() {
        let mut fields = line.split('\t');
        let (Some(input), Some(bits), Some(erange)) = (fields.next(), fields.next(), fields.next())
        else {
            panic!("malformed row: {line}");
        };
        let bits = u128::from_str_radix(bits.trim_start_matches("0x"), 16).expect("hex bits");
        let parsed = parse(input.as_bytes(), format);
        assert_eq!(
            parsed.bits,
            bits,
            "{format:?} {}: got {:#x}, want {bits:#x}",
            &input[..input.len().min(80)],
            parsed.bits
        );
        assert_eq!(
            parsed.end,
            input.len(),
            "end of {}",
            &input[..input.len().min(80)]
        );
        let want = if erange == "1" { errno::ERANGE } else { 0 };
        assert_eq!(
            parsed.error,
            want,
            "errno for {}",
            &input[..input.len().min(80)]
        );
        rows += 1;
    }
    assert!(rows > 100);
}

#[test]
fn hard_float_cases() {
    check_table(include_str!("../../tests/data/float/f32.txt"), &BINARY32);
}

#[test]
fn hard_double_cases() {
    check_table(include_str!("../../tests/data/float/f64.txt"), &BINARY64);
}

#[test]
fn hard_x87_extended_cases() {
    check_table(
        include_str!("../../tests/data/float/x87.txt"),
        &X87_EXTENDED,
    );
}

#[test]
fn hard_binary128_cases() {
    check_table(include_str!("../../tests/data/float/f128.txt"), &BINARY128);
}

/// A small deterministic generator for test inputs.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }

    fn digits(&mut self, count: u64) -> String {
        (0..count)
            .map(|_| char::from(b'0' + self.below(10) as u8))
            .collect()
    }
}

/// A random decimal input in the grammar Rust's `parse` shares with C.
fn random_decimal(rng: &mut Lcg, max_digits: u64, max_exponent: i64) -> String {
    let mut s = String::new();
    if rng.below(2) == 0 {
        s.push('-');
    }
    let whole = rng.below(max_digits);
    s += &rng.digits(whole);
    if rng.below(2) == 0 || whole == 0 {
        s.push('.');
        let fraction = rng.below(max_digits) + u64::from(whole == 0);
        s += &rng.digits(fraction);
    }
    if rng.below(3) != 0 {
        let exponent = rng.below(2 * max_exponent as u64 + 1) as i64 - max_exponent;
        s += &format!("e{exponent}");
    }
    s
}

#[test]
fn double_and_float_agree_with_rusts_parser() {
    let mut rng = Lcg(1);
    for round in 0..40_000 {
        let (digits, exponent) = match round % 4 {
            0 => (8, 25),
            1 => (20, 330),
            2 => (400, 400),
            _ => (30, 50),
        };
        let input = random_decimal(&mut rng, digits, exponent);
        let want = input.parse::<f64>().expect("Rust parses it");
        let got = parse(input.as_bytes(), &BINARY64);
        assert_eq!(got.bits, u128::from(want.to_bits()), "double {input}");
        assert_eq!(got.end, input.len());
        let want = input.parse::<f32>().expect("Rust parses it");
        let got = parse(input.as_bytes(), &BINARY32);
        assert_eq!(got.bits, u128::from(want.to_bits()), "float {input}");
    }
}

#[test]
fn the_fast_path_agrees_with_exact_division() {
    let mut rng = Lcg(2);
    for _ in 0..20_000 {
        let count = rng.below(15) + 1;
        let mut digits = rng.digits(count);
        digits.replace_range(..1, &(rng.below(9) + 1).to_string());
        let exponent = rng.below(45) as i64 - 22;
        let values = || digits.bytes().map(|b| b - b'0');
        let fast = float::decimal(&BINARY64, false, values(), count, exponent);
        let head = digits.parse::<u64>().expect("digits");
        let exact =
            float::exact::<64>(&BINARY64, false, head, core::iter::empty(), count, exponent);
        assert_eq!(Ok(fast.bits), exact.map(|r| r.bits), "{digits}e{exponent}");
        let count = count.min(7);
        let exponent = exponent.clamp(-10, 10);
        let digits = &digits[..count as usize];
        let fast = float::decimal(
            &BINARY32,
            true,
            digits.bytes().map(|b| b - b'0'),
            count,
            exponent,
        );
        let head = digits.parse::<u64>().expect("digits");
        let exact = float::exact::<64>(&BINARY32, true, head, core::iter::empty(), count, exponent);
        assert_eq!(Ok(fast.bits), exact.map(|r| r.bits), "{digits}e{exponent}");
    }
}

/// Parses `input` as a `double`: the value, the end, and the error.
fn double(input: &str) -> (f64, usize, c_int) {
    let parsed = parse(input.as_bytes(), &BINARY64);
    (f64::from_bits(parsed.bits as u64), parsed.end, parsed.error)
}

#[test]
fn the_subject_ends_where_c_says() {
    let cases: &[(&str, f64, usize)] = &[
        ("1e", 1.0, 1),
        ("1e+", 1.0, 1),
        ("1E-x", 1.0, 1),
        ("2e3", 2000.0, 3),
        ("  +.5e1x", 5.0, 7),
        ("1.2.3", 1.2, 3),
        ("1.", 1.0, 2),
        ("0x", 0.0, 1),
        ("0X", 0.0, 1),
        ("0x.", 0.0, 1),
        ("0x.p1", 0.0, 1),
        ("0xg", 0.0, 1),
        ("0x1p", 1.0, 3),
        ("0x1p-", 1.0, 3),
        ("0x1P+4", 16.0, 6),
        ("0x1.8p1", 3.0, 7),
        ("0x.8", 0.5, 4),
        ("0x10", 16.0, 4),
        ("inf", f64::INFINITY, 3),
        ("-INFINITY", f64::NEG_INFINITY, 9),
        ("infinit", f64::INFINITY, 3),
        ("Infinityx", f64::INFINITY, 8),
        ("\t\n\x0b\x0c\r 7", 7.0, 7),
    ];
    for &(input, value, end) in cases {
        assert_eq!(double(input), (value, end, 0), "{input:?}");
    }
    for (input, end) in [
        ("nan", 3),
        ("NaN(", 3),
        ("nan()", 5),
        ("nan(abc_19)", 11),
        ("nan(a-b)", 3),
        ("nanx", 3),
    ] {
        let (value, got, error) = double(input);
        assert!(value.is_nan() && value.is_sign_positive(), "{input}");
        assert_eq!((got, error), (end, 0), "{input}");
    }
    let (value, _, _) = double("-nan");
    assert!(value.is_nan() && value.is_sign_negative());
}

#[test]
fn no_subject_is_einval_and_consumes_nothing() {
    for input in [
        "", " ", "+", "-", ".", "-.e1", "e5", "in", "na", "x", "+-1", " .x",
    ] {
        assert_eq!(double(input), (0.0, 0, errno::EINVAL), "{input:?}");
    }
}

#[test]
fn zeros_keep_their_sign() {
    for input in ["-0", "-0.000", "-0x0p99", "-0e999999999999999999", "-0x"] {
        let (value, _, error) = double(input);
        assert_eq!(
            (value.to_bits(), error),
            ((-0.0_f64).to_bits(), 0),
            "{input}"
        );
    }
}

#[test]
fn overflow_and_underflow_set_erange_by_musls_rules() {
    let cases: &[(&str, f64, c_int)] = &[
        ("1e309", f64::INFINITY, errno::ERANGE),
        (
            "-1e99999999999999999999999",
            f64::NEG_INFINITY,
            errno::ERANGE,
        ),
        ("1e-400", 0.0, errno::ERANGE),
        ("1e-99999999999999999999999", 0.0, errno::ERANGE),
        ("4.9e-324", 5e-324, errno::ERANGE),
        (
            "2.2250738585072011e-308",
            2.225_073_858_507_201e-308,
            errno::ERANGE,
        ),
        // Exactly the smallest subnormal and the smallest normal numbers.
        ("0x1p-1074", 5e-324, 0),
        ("0x1p-1022", 2.2250738585072014e-308, 0),
        // Hexadecimal: only a zero result underflows.
        ("0x1.8p-1074", 1e-323, 0),
        ("0x1p-1075", 0.0, errno::ERANGE),
        ("0x1.0000001p-1075", 5e-324, 0),
        ("0x1p1024", f64::INFINITY, errno::ERANGE),
        ("0x1.fffffffffffff8p1023", f64::INFINITY, errno::ERANGE),
        ("0x1.fffffffffffff7ffp1023", f64::MAX, 0),
        (
            "-0x1p99999999999999999999",
            f64::NEG_INFINITY,
            errno::ERANGE,
        ),
        ("0x1p-99999999999999999999", 0.0, errno::ERANGE),
    ];
    for &(input, value, error) in cases {
        let (got, _, got_error) = double(input);
        assert_eq!(
            (got.to_bits(), got_error),
            (value.to_bits(), error),
            "{input}"
        );
    }
}

#[test]
fn hexadecimal_rounds_ties_to_even_and_keeps_every_digit() {
    let cases: &[(&str, u64)] = &[
        ("0x1.00000000000008p0", 0x3ff0_0000_0000_0000),
        ("0x1.00000000000018p0", 0x3ff0_0000_0000_0002),
        (
            "0x1.000000000000080000000000000000000000000000001p0",
            0x3ff0_0000_0000_0001,
        ),
        (
            "0x0000000000000000000000000000000000000001p0",
            0x3ff0_0000_0000_0000,
        ),
        (
            "0x.000000000000000000000000000000000000000000000001p192",
            0x3ff0_0000_0000_0000,
        ),
        (
            "0x1111111111111111111111111111111111111111111111111p-192",
            0x3ff1_1111_1111_1111,
        ),
        ("0x1.111111111111281", 0x3ff1_1111_1111_1113),
        ("0x1.11111111111111", 0x3ff1_1111_1111_1111),
    ];
    for &(input, bits) in cases {
        assert_eq!(double(input).0.to_bits(), bits, "{input}");
    }
}

#[test]
fn very_long_inputs_are_correctly_rounded() {
    let mut ones = String::from(".");
    ones += &"1".repeat(40_000);
    assert_eq!(double(&ones).0, 0.111_111_111_111_111_1);
    // 2^-1075, the halfway point below the smallest subnormal, followed by
    // zeros and then a 1 far past the 768th digit.
    let half = "2.4703282292062327208828439643411068618252990130716238221279284125033775363510437593264991818081799618989828234772285886546332835517796989819938739800539093906315035659515570226392290858392449105184435931802849936536152500319370457678249219365623669863658480757001585769269903706311928279558551332927834338409351978015531246597263579574622766465272827220056374006485499977096599470454020828166226237857393450736339007967761930577506740176324673600968951340535537458516661134223766678604162159680461914467291840300530057530849048765391711386591646239524912623653881879636239373280423891018672348497668235089863388587925628302755995657524455507255189313690836254779186948667994968324049705821028513185451396213837722826145437693412532098591327667236328125e-324";
    let mut above = half.replace("e-324", "");
    above += &"0".repeat(5000);
    above += "1e-324";
    assert_eq!(double(half), (0.0, half.len(), errno::ERANGE));
    assert_eq!(double(&above).0, 5e-324);
    let (_, _, error) = double(&above);
    assert_eq!(error, errno::ERANGE);
}

#[test]
fn formats_bound_their_digits_and_limbs() {
    assert_eq!(BINARY64.max_digits(), 770);
    assert!(BINARY64.max_digits() >= 768);
    assert!(X87_EXTENDED.max_digits() >= 11_515);
    assert!(BINARY128.max_digits() >= 11_564);
    assert!(BINARY32.max_digits() >= 113);
}

#[test]
fn strtold_writes_ten_bytes_of_extended_precision() {
    let check = |input: &str, want: u128, consumed: usize| {
        let text = CString::new(input).expect("no NUL");
        let mut out = [0xaa_u8; 12];
        let mut end = null_mut();
        // SAFETY: a C string, a place for `endptr`, and room for ten bytes.
        unsafe { strtold_x87(text.as_ptr(), &raw mut end, out.as_mut_ptr()) };
        let mut bytes = [0_u8; 16];
        bytes[..10].copy_from_slice(&out[..10]);
        assert_eq!(u128::from_le_bytes(bytes), want, "{input}");
        assert_eq!(&out[10..], &[0xaa, 0xaa], "wrote past ten bytes");
        // SAFETY: `end` points into `text`.
        let moved = unsafe { end.cast_const().offset_from(text.as_ptr()) };
        assert_eq!(moved, consumed as isize);
    };
    check("1", 0x3fff_8000_0000_0000_0000, 1);
    check("-2.5x", 0xc000_a000_0000_0000_0000, 4);
    check("0x1p-16445", 1, 10);
    check("inf", 0x7fff_8000_0000_0000_0000, 3);
    check("-nan", 0xffff_c000_0000_0000_0000, 4);
    check("1e5000", 0x7fff_8000_0000_0000_0000, 6);
    check("0x8.111111111111113p0", 0x4002_8111_1111_1111_1113, 21);
}
