//! libc-test's math tables, read at test time, and the checks its `mtest.h`
//! and its per-function programs make on them.
//!
//! libc-test (MIT) keeps each function's cases in C headers under
//! `src/math/sanity`, `special`, `crlibm` and `ucb`, one case a line:
//!
//! ```text
//! T(RN, 0x1.02239f3c6a8f1p+3, 0x1.5c0cd7b5a3a01p+11, 0x1.a7ebep-1, INEXACT)
//! ```
//!
//! That is the rounding mode, the arguments, the expected result, how far the
//! expected result is from the exact one in ulps, and the exceptions expected.
//! The tables are read from a libc-test checkout, never copied: set
//! `FERROUSLI_LIBC_TEST` to its path, or keep it at
//! `~/.local/share/ferrix/ferrousli-ref/libc-test`.
//!
//! [`Rules`] reproduces how each of libc-test's programs judges a case,
//! including the cases a program prints with an `X` and does not count as an
//! error; those are counted here as tolerated. A case that fails for musl as
//! well goes in the test's allow-list, with the reason. An allow-list entry
//! that no longer fails fails the test, so the list cannot go stale.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a test reports failure by panicking"
)]
#![allow(dead_code, reason = "shared by modules still being written")]

use core::ffi::c_int;
use std::path::PathBuf;

use crate::fenv::{
    FE_DIVBYZERO, FE_DOWNWARD, FE_INEXACT, FE_INVALID, FE_OVERFLOW, FE_TONEAREST, FE_TOWARDZERO,
    FE_UNDERFLOW, FE_UPWARD, feclearexcept, fesetround, fetestexcept,
};
use crate::math::support::{hex_f32, hex_f64, hexf32, hexf64, parse_hex};

/// Rounding to nearest, as the tables write it.
pub(crate) const RN: c_int = FE_TONEAREST;

/// The five exceptions C names. SSE also has a denormal-operand flag, which
/// `FE_ALL_EXCEPT` includes and libc-test's programs leave out.
const TESTED: c_int = FE_INEXACT | FE_INVALID | FE_DIVBYZERO | FE_UNDERFLOW | FE_OVERFLOW;

/// Where libc-test's math tables are.
fn directory() -> PathBuf {
    let root = std::env::var_os("FERROUSLI_LIBC_TEST").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set"))
                .join(".local/share/ferrix/ferrousli-ref/libc-test")
        },
        PathBuf::from,
    );
    root.join("src/math")
}

/// One case from a table.
#[derive(Debug)]
pub(crate) struct Row {
    /// The file and line, as `special/exp.h:12`.
    pub(crate) place: String,
    /// The rounding mode.
    pub(crate) mode: c_int,
    /// The fields between the mode and the exceptions, as written.
    fields: Vec<String>,
    /// The exceptions expected.
    pub(crate) except: c_int,
}

/// A rounding mode's value, from its name in a table.
fn parse_mode(text: &str) -> c_int {
    match text {
        "RN" => FE_TONEAREST,
        "RZ" => FE_TOWARDZERO,
        "RU" => FE_UPWARD,
        "RD" => FE_DOWNWARD,
        _ => panic!("unknown rounding mode {text}"),
    }
}

/// A rounding mode's name, for messages.
fn mode_name(mode: c_int) -> &'static str {
    match mode {
        FE_TONEAREST => "RN",
        FE_TOWARDZERO => "RZ",
        FE_UPWARD => "RU",
        FE_DOWNWARD => "RD",
        _ => "R?",
    }
}

/// A set of exceptions, from a table's `INEXACT|UNDERFLOW` or `0`.
fn parse_except(text: &str) -> c_int {
    text.split('|')
        .map(|flag| match flag.trim() {
            "0" => 0,
            "INEXACT" => FE_INEXACT,
            "INVALID" => FE_INVALID,
            "DIVBYZERO" => FE_DIVBYZERO,
            "UNDERFLOW" => FE_UNDERFLOW,
            "OVERFLOW" => FE_OVERFLOW,
            other => panic!("unknown exception {other}"),
        })
        .fold(0, |all, flag| all | flag)
}

/// A set of exceptions, for messages.
fn except_names(flags: c_int) -> String {
    let names = [
        (FE_INEXACT, "INEXACT"),
        (FE_INVALID, "INVALID"),
        (FE_DIVBYZERO, "DIVBYZERO"),
        (FE_UNDERFLOW, "UNDERFLOW"),
        (FE_OVERFLOW, "OVERFLOW"),
    ];
    let set: Vec<&str> = names
        .iter()
        .filter(|(flag, _)| flags & flag != 0)
        .map(|(_, name)| *name)
        .collect();
    if set.is_empty() {
        "0".to_owned()
    } else {
        set.join("|")
    }
}

/// An infinity or a NaN, as the tables write them.
fn parse_special(text: &str) -> Option<f64> {
    match text {
        "inf" => Some(f64::INFINITY),
        "-inf" => Some(f64::NEG_INFINITY),
        "nan" => Some(f64::NAN),
        "-nan" => Some(-f64::NAN),
        _ => None,
    }
}

impl Row {
    /// Parses a `T(...)` line.
    fn parse(place: String, line: &str) -> Self {
        let line = line.split("//").next().unwrap_or(line);
        let inner = line.strip_prefix("T(").expect("a T( line");
        let end = inner.rfind(')').expect("a closing parenthesis");
        let parts: Vec<String> = inner[..end]
            .split(',')
            .map(|part| part.trim().to_owned())
            .collect();
        assert!(parts.len() >= 3, "{place}: too few fields");
        let mode = parse_mode(&parts[0]);
        let except = parse_except(&parts[parts.len() - 1]);
        let fields = parts[1..parts.len() - 1].to_vec();
        Self {
            place,
            mode,
            fields,
            except,
        }
    }

    /// Field `index`, counting from the first argument.
    fn text(&self, index: usize) -> &str {
        self.fields
            .get(index)
            .unwrap_or_else(|| panic!("{}: no field {index}", self.place))
    }

    /// Field `index` as a `double`, which it must name exactly.
    pub(crate) fn f64(&self, index: usize) -> f64 {
        let text = self.text(index);
        parse_special(text)
            .or_else(|| parse_hex(text).and_then(hex_f64))
            .unwrap_or_else(|| panic!("{}: {text} is not exactly a double", self.place))
    }

    /// Field `index` as a `float`. As in C's initialiser, a constant that is
    /// exact only as a `double` is rounded to nearest.
    pub(crate) fn f32(&self, index: usize) -> f32 {
        let text = self.text(index);
        if let Some(value) = parse_special(text) {
            return value as f32;
        }
        let hex = parse_hex(text)
            .unwrap_or_else(|| panic!("{}: {text} is not a number", self.place));
        hex_f32(hex)
            .or_else(|| hex_f64(hex).map(|value| value as f32))
            .unwrap_or_else(|| panic!("{}: {text} is not exactly a double", self.place))
    }

    /// Field `index` as an integer.
    pub(crate) fn int(&self, index: usize) -> i64 {
        let text = self.text(index);
        match text {
            "FP_ILOGB0" | "FP_ILOGBNAN" => i64::from(c_int::MIN),
            // `INT_MAX`, as libc-test writes it.
            "-1U/2" => i64::from(c_int::MAX),
            _ => text
                .parse()
                .unwrap_or_else(|_| panic!("{}: {text} is not an integer", self.place)),
        }
    }
}

/// Every case in `files`, each named relative to libc-test's `src/math`.
pub(crate) fn rows(files: &[&str]) -> Vec<Row> {
    let directory = directory();
    let mut rows = Vec::new();
    for file in files {
        let path = directory.join(file);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "cannot read libc-test's {}: {error}. Set FERROUSLI_LIBC_TEST to a libc-test checkout",
                path.display()
            )
        });
        for (number, line) in text.lines().enumerate() {
            let line = line.trim_start();
            if line.starts_with("T(") {
                rows.push(Row::parse(format!("{file}:{}", number + 1), line));
            }
        }
    }
    assert!(!rows.is_empty(), "no cases in {files:?}");
    rows
}

/// Calls `call` in rounding mode `mode` with every flag clear, and returns its
/// result with the exceptions it raised. The mode is to nearest again after.
pub(crate) fn under<T>(mode: c_int, call: impl FnOnce() -> T) -> (T, c_int) {
    assert_eq!(fesetround(mode), 0);
    let _ = feclearexcept(TESTED);
    let value = call();
    let raised = fetestexcept(TESTED);
    let _ = fesetround(FE_TONEAREST);
    (value, raised)
}

/// `x × 2^n`, for measuring an error.
fn scale(mut x: f64, mut n: i32) -> f64 {
    while n > 1000 {
        x *= hexf64!("0x1p1000");
        n -= 1000;
    }
    while n < -1000 {
        x *= hexf64!("0x1p-1000");
        n += 1000;
    }
    x * f64::from_bits(((0x3ff + n) as u64) << 52)
}

/// `double` or `float`, as the tables and checks need them.
pub(crate) trait Float: Copy + PartialEq + core::fmt::Debug {
    /// Field `index` of `row`.
    fn field(row: &Row, index: usize) -> Self;
    /// `x`, rounded to nearest if this is `float`.
    fn narrow(x: f64) -> Self;
    /// The value as a `double`, which holds every `float` exactly.
    fn wide(self) -> f64;
    /// libc-test's `ulperr`: how far `got` is from `want`, in ulps of `want`,
    /// plus `want`'s own error.
    fn ulperr(got: Self, want: Self, dwant: f32) -> f32;
    /// Whether the magnitude is below the smallest normal number.
    fn tiny(self) -> bool;
}

impl Float for f64 {
    fn field(row: &Row, index: usize) -> Self {
        row.f64(index)
    }

    fn narrow(x: f64) -> Self {
        x
    }

    fn wide(self) -> f64 {
        self
    }

    fn ulperr(got: Self, want: Self, dwant: f32) -> f32 {
        if got.is_nan() && want.is_nan() {
            return 0.0;
        }
        if got == want {
            return if got.is_sign_negative() == want.is_sign_negative() {
                dwant
            } else {
                f32::INFINITY
            };
        }
        let (got, want) = if got.is_infinite() {
            (hexf64!("0x1p1023").copysign(got), want * 0.5)
        } else {
            (got, want)
        };
        let e = (want.to_bits() >> 52 & 0x7ff).max(1) as i32 - 0x3ff - 52;
        scale(got - want, -e) as f32 + dwant
    }

    fn tiny(self) -> bool {
        self.abs() < hexf64!("0x1p-1022")
    }
}

impl Float for f32 {
    fn field(row: &Row, index: usize) -> Self {
        row.f32(index)
    }

    fn narrow(x: f64) -> Self {
        x as f32
    }

    fn wide(self) -> f64 {
        f64::from(self)
    }

    fn ulperr(got: Self, want: Self, dwant: f32) -> f32 {
        if got.is_nan() && want.is_nan() {
            return 0.0;
        }
        if got == want {
            return if got.is_sign_negative() == want.is_sign_negative() {
                dwant
            } else {
                f32::INFINITY
            };
        }
        let (got, want) = if got.is_infinite() {
            (hexf32!("0x1p127").copysign(got), want * 0.5)
        } else {
            (got, want)
        };
        let e = (want.to_bits() >> 23 & 0xff).max(1) as i32 - 0x7f - 23;
        scale(f64::from(got - want), -e) as f32 + dwant
    }

    fn tiny(self) -> bool {
        self.abs() < hexf32!("0x1p-126")
    }
}

/// `x` as C's `%a` prints it.
pub(crate) fn hex(x: impl Float) -> String {
    let x = x.wide();
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x.is_nan() {
        return format!("{sign}nan");
    }
    if x.is_infinite() {
        return format!("{sign}inf");
    }
    let bits = x.to_bits();
    let biased = (bits >> 52 & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    if biased == 0 && fraction == 0 {
        return format!("{sign}0x0p+0");
    }
    let (lead, exponent) = if biased == 0 {
        (0, -1022)
    } else {
        (1, biased - 1023)
    };
    let digits = format!("{fraction:013x}");
    let digits = digits.trim_end_matches('0');
    if digits.is_empty() {
        format!("{sign}0x{lead}p{exponent:+}")
    } else {
        format!("{sign}0x{lead}.{digits}p{exponent:+}")
    }
}

/// libc-test's `checkcr`: the same value with the same sign, or both NaNs.
pub(crate) fn checkcr<F: Float>(got: F, want: F) -> bool {
    let (got, want) = (got.wide(), want.wide());
    if want.is_nan() {
        return got.is_nan();
    }
    got == want && got.is_sign_negative() == want.is_sign_negative()
}

/// libc-test's `checkexcept`: to nearest, inexact may differ; in another
/// mode, underflow may too.
fn checkexcept(got: c_int, want: c_int, mode: c_int) -> bool {
    if mode == FE_TONEAREST {
        got | FE_INEXACT == want | FE_INEXACT
    } else {
        got | FE_INEXACT | FE_UNDERFLOW == want | FE_INEXACT | FE_UNDERFLOW
    }
}

/// libc-test's `checkulp`: under 1.5 ulps to nearest, and under 3 otherwise.
fn checkulp(d: f32, mode: c_int) -> bool {
    if mode == FE_TONEAREST {
        d.abs() < 1.5
    } else {
        d.abs() < 3.0
    }
}

/// How a result is judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value {
    /// Within libc-test's ulp bounds.
    Ulp,
    /// Exactly the expected value.
    Exact,
    /// An integer, equal to the expected one.
    Integer,
}

/// How the exceptions are judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exceptions {
    /// `checkexcept`.
    Rounding,
    /// `checkexceptall`: exactly the expected set.
    All,
    /// `checkexceptall` with inexact ignored.
    AllButInexact,
}

/// How one of libc-test's programs judges its function's cases.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rules {
    /// How the result is judged.
    value: Value,
    /// How the exceptions are judged.
    exceptions: Exceptions,
    /// A case also passes if only inexact is missing.
    inexact_optional: bool,
    /// An ulp error below this is tolerated, if nonzero.
    tolerance: f32,
    /// An ulp error in a mode other than to nearest is tolerated.
    directed_tolerated: bool,
    /// Wrong exceptions are tolerated for a result below the normal range
    /// that raised underflow.
    underflow_tolerated: bool,
    /// To nearest, the error must also be under one ulp.
    within_one_ulp: bool,
    /// An integer result is not compared when invalid is expected.
    unless_invalid: bool,
}

impl Rules {
    /// `checkexcept` and `checkulp`, the rules for most functions.
    pub(crate) const ULP: Self = Self {
        value: Value::Ulp,
        exceptions: Exceptions::Rounding,
        inexact_optional: false,
        tolerance: 0.0,
        directed_tolerated: false,
        underflow_tolerated: false,
        within_one_ulp: false,
        unless_invalid: false,
    };

    /// `checkexceptall` and `checkcr`, for the exact functions.
    pub(crate) const EXACT: Self = Self {
        value: Value::Exact,
        exceptions: Exceptions::All,
        ..Self::ULP
    };

    /// `checkexcept` and an integer compared exactly.
    pub(crate) const INTEGER: Self = Self {
        value: Value::Integer,
        ..Self::ULP
    };

    /// Exceptions judged by `checkexcept` rather than `checkexceptall`.
    pub(crate) const fn loose(self) -> Self {
        Self {
            exceptions: Exceptions::Rounding,
            ..self
        }
    }

    /// Exceptions judged by `checkexceptall`, ignoring inexact.
    pub(crate) const fn ignore_inexact(self) -> Self {
        Self {
            exceptions: Exceptions::AllButInexact,
            ..self
        }
    }

    /// A missing inexact is accepted: `(e|INEXACT) == p->e`.
    pub(crate) const fn inexact_optional(self) -> Self {
        Self {
            inexact_optional: true,
            ..self
        }
    }

    /// An ulp error below `ulps` is tolerated.
    pub(crate) const fn tolerate(self, ulps: f32) -> Self {
        Self {
            tolerance: ulps,
            ..self
        }
    }

    /// Any ulp error outside rounding to nearest is tolerated.
    pub(crate) const fn directed(self) -> Self {
        Self {
            directed_tolerated: true,
            ..self
        }
    }

    /// `pow`'s and `exp2`'s leniency about exceptions for tiny results.
    pub(crate) const fn underflow(self) -> Self {
        Self {
            underflow_tolerated: true,
            ..self
        }
    }

    /// `hypot`'s stricter bound: under one ulp to nearest.
    pub(crate) const fn within_one_ulp(self) -> Self {
        Self {
            within_one_ulp: true,
            ..self
        }
    }

    /// No integer comparison when invalid is expected.
    pub(crate) const fn unless_invalid(self) -> Self {
        Self {
            unless_invalid: true,
            ..self
        }
    }

    /// Whether `raised` is acceptable for `row`.
    fn exceptions_pass(&self, row: &Row, raised: c_int) -> bool {
        let want = row.except;
        let pass = match self.exceptions {
            Exceptions::Rounding => checkexcept(raised, want, row.mode),
            Exceptions::All => raised == want,
            Exceptions::AllButInexact => raised | FE_INEXACT == want | FE_INEXACT,
        };
        pass || (self.inexact_optional && raised | FE_INEXACT == want)
    }
}

/// What became of one case.
#[derive(Debug)]
pub(crate) enum Verdict {
    /// It passed.
    Pass,
    /// It failed in a way libc-test does not count as an error.
    Tolerated(String),
    /// It failed.
    Fail(String),
}

impl Verdict {
    /// Both verdicts together: the worse of the two, with both messages.
    pub(crate) fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Fail(a), Self::Fail(b)) => Self::Fail(format!("{a}; {b}")),
            (Self::Fail(a), _) | (_, Self::Fail(a)) => Self::Fail(a),
            (Self::Tolerated(a), _) | (_, Self::Tolerated(a)) => Self::Tolerated(a),
            (Self::Pass, Self::Pass) => Self::Pass,
        }
    }
}

/// Judges one case: the exceptions `raised`, and the result `got` against
/// `want`, whose own error is `dy`. `call` describes the call, for a message.
pub(crate) fn judge<F: Float>(
    rules: &Rules,
    row: &Row,
    raised: c_int,
    got: F,
    want: F,
    dy: f32,
    call: impl FnOnce() -> String,
) -> Verdict {
    let mut problems = Vec::new();
    let mut tolerated = Vec::new();
    if !rules.exceptions_pass(row, raised) {
        let message = format!(
            "exceptions want {} got {}",
            except_names(row.except),
            except_names(raised)
        );
        if rules.underflow_tolerated
            && got.tiny()
            && raised | FE_INEXACT == FE_INEXACT | FE_UNDERFLOW
        {
            tolerated.push(message);
        } else {
            problems.push(message);
        }
    }
    let result = format!("want {} got {}", hex(want), hex(got));
    match rules.value {
        Value::Exact | Value::Integer => {
            if !checkcr(got, want) {
                problems.push(result);
            }
        }
        Value::Ulp => {
            let d = F::ulperr(got, want, dy);
            let out_of_bounds = !checkulp(d, row.mode)
                || (rules.within_one_ulp && row.mode == FE_TONEAREST && d.abs() >= 1.0);
            if out_of_bounds {
                let message = format!("{result}, ulperr {d:.3}");
                if (rules.tolerance > 0.0 && d.abs() < rules.tolerance)
                    || (rules.directed_tolerated && row.mode != FE_TONEAREST)
                {
                    tolerated.push(message);
                } else {
                    problems.push(message);
                }
            }
        }
    }
    let heading = || format!("{}: {} {}", row.place, mode_name(row.mode), call());
    if !problems.is_empty() {
        Verdict::Fail(format!("{}: {}", heading(), problems.join(", ")))
    } else if !tolerated.is_empty() {
        Verdict::Tolerated(format!("{}: {}", heading(), tolerated.join(", ")))
    } else {
        Verdict::Pass
    }
}

/// Judges a case whose result is an integer.
pub(crate) fn judge_int(
    rules: &Rules,
    row: &Row,
    raised: c_int,
    got: i64,
    want: i64,
    call: impl FnOnce() -> String,
) -> Verdict {
    let mut problems = Vec::new();
    if !rules.exceptions_pass(row, raised) {
        problems.push(format!(
            "exceptions want {} got {}",
            except_names(row.except),
            except_names(raised)
        ));
    }
    let compared = !(rules.unless_invalid && row.except & FE_INVALID != 0);
    if compared && got != want {
        problems.push(format!("want {want} got {got}"));
    }
    if problems.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Fail(format!(
            "{}: {} {}: {}",
            row.place,
            mode_name(row.mode),
            call(),
            problems.join(", ")
        ))
    }
}

/// A case expected to fail, and why.
#[derive(Debug)]
pub(crate) struct Allow {
    /// The case's file and line, as `special/exp.h:12`.
    pub(crate) place: &'static str,
    /// Why it fails, and that musl fails it too.
    pub(crate) reason: &'static str,
}

/// Runs `case` on every row of `files`, prints the counts, and fails if any
/// case fails that `allow` does not list, or `allow` lists one that passes.
pub(crate) fn run(name: &str, files: &[&str], allow: &[Allow], mut case: impl FnMut(&Row) -> Verdict) {
    let rows = rows(files);
    let mut passed = 0;
    let mut tolerated = 0;
    let mut failures = Vec::new();
    let mut used = vec![false; allow.len()];
    for row in &rows {
        match case(row) {
            Verdict::Pass => passed += 1,
            Verdict::Tolerated(_) => tolerated += 1,
            Verdict::Fail(message) => match allow.iter().position(|entry| entry.place == row.place) {
                Some(index) => used[index] = true,
                None => failures.push(message),
            },
        }
    }
    let allowed = used.iter().filter(|used| **used).count();
    println!(
        "libc-test {name}: {} cases, {passed} pass, {tolerated} tolerated, {allowed} allow-listed, {} fail",
        rows.len(),
        failures.len()
    );
    let stale: Vec<&str> = allow
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(entry, _)| entry.place)
        .collect();
    assert!(
        failures.is_empty() && stale.is_empty(),
        "{name}: {} failures\n{}\nallow-list entries that pass: {stale:?}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// A one-argument function's tables: `x, y, dy`.
pub(crate) fn d_d<F: Float>(
    name: &str,
    files: &[&str],
    function: impl Fn(F) -> F,
    rules: Rules,
    allow: &[Allow],
) {
    run(name, files, allow, |row| {
        let x = F::field(row, 0);
        let want = F::field(row, 1);
        let dy = row.f32(2);
        let (got, raised) = under(row.mode, || function(x));
        judge(&rules, row, raised, got, want, dy, || {
            format!("{name}({})", hex(x))
        })
    });
}

/// A two-argument function's tables: `x, x2, y, dy`.
pub(crate) fn dd_d<F: Float>(
    name: &str,
    files: &[&str],
    function: impl Fn(F, F) -> F,
    rules: Rules,
    allow: &[Allow],
) {
    run(name, files, allow, |row| {
        let x = F::field(row, 0);
        let x2 = F::field(row, 1);
        let want = F::field(row, 2);
        let dy = row.f32(3);
        let (got, raised) = under(row.mode, || function(x, x2));
        judge(&rules, row, raised, got, want, dy, || {
            format!("{name}({}, {})", hex(x), hex(x2))
        })
    });
}

/// A three-argument function's tables: `x, x2, x3, y, dy`.
pub(crate) fn ddd_d<F: Float>(
    name: &str,
    files: &[&str],
    function: impl Fn(F, F, F) -> F,
    rules: Rules,
    allow: &[Allow],
) {
    run(name, files, allow, |row| {
        let x = F::field(row, 0);
        let x2 = F::field(row, 1);
        let x3 = F::field(row, 2);
        let want = F::field(row, 3);
        let dy = row.f32(4);
        let (got, raised) = under(row.mode, || function(x, x2, x3));
        judge(&rules, row, raised, got, want, dy, || {
            format!("{name}({}, {}, {})", hex(x), hex(x2), hex(x3))
        })
    });
}

/// The tables of a function of a number and an integer: `x, i, y, dy`.
pub(crate) fn di_d<F: Float>(
    name: &str,
    files: &[&str],
    function: impl Fn(F, i64) -> F,
    rules: Rules,
    allow: &[Allow],
) {
    run(name, files, allow, |row| {
        let x = F::field(row, 0);
        let n = row.int(1);
        let want = F::field(row, 2);
        let dy = row.f32(3);
        let (got, raised) = under(row.mode, || function(x, n));
        judge(&rules, row, raised, got, want, dy, || {
            format!("{name}({}, {n})", hex(x))
        })
    });
}

/// The tables of a function returning an integer: `x, i`.
pub(crate) fn d_i<F: Float>(
    name: &str,
    files: &[&str],
    function: impl Fn(F) -> i64,
    rules: Rules,
    allow: &[Allow],
) {
    run(name, files, allow, |row| {
        let x = F::field(row, 0);
        let want = row.int(1);
        let (got, raised) = under(row.mode, || function(x));
        judge_int(&rules, row, raised, got, want, || {
            format!("{name}({})", hex(x))
        })
    });
}

/// A deterministic source of test inputs: xorshift64*.
#[derive(Debug)]
pub(crate) struct Random(u64);

impl Random {
    /// A generator seeded from a name, so each function gets its own inputs
    /// and every run the same ones.
    pub(crate) fn new(name: &str) -> Self {
        let seed = name
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        Self(seed | 1)
    }

    /// 64 random bits.
    pub(crate) fn bits(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A `double` in `[low, high]`. A range of one sign spanning more than
    /// eight binary orders of magnitude is sampled uniformly in the logarithm,
    /// so that its small numbers are reached; any other uniformly.
    pub(crate) fn in_range(&mut self, low: f64, high: f64) -> f64 {
        let unit = (self.bits() >> 11) as f64 * hexf64!("0x1p-53");
        if low > 0.0 && high / low > 256.0 {
            let (a, b) = (low.ln(), high.ln());
            return (a + unit * (b - a)).exp().clamp(low, high);
        }
        if high < 0.0 && low / high > 256.0 {
            let (a, b) = ((-high).ln(), (-low).ln());
            return -(a + unit * (b - a)).exp().clamp(-high, -low);
        }
        low * (1.0 - unit) + high * unit
    }
}

/// How many inputs each range gets when comparing with the host.
pub(crate) const SAMPLES: usize = 20_000;

/// The error of `ours` against `theirs` in ulps of `theirs`, with a
/// disagreement about NaN counted as infinite.
fn host_error<F: Float>(ours: F, theirs: F) -> f32 {
    let d = F::ulperr(ours, theirs, 0.0).abs();
    if d.is_nan() { f32::INFINITY } else { d }
}

/// Compares a one-argument function with the host C library's, which is
/// glibc's libm in the test binary, at [`SAMPLES`] inputs from each range.
/// Prints the worst error in ulps, returns it, and fails if it reaches
/// `bound`.
pub(crate) fn host_d_d<F: Float>(
    name: &str,
    ours: impl Fn(F) -> F,
    theirs: impl Fn(F) -> F,
    ranges: &[(f64, f64)],
    bound: f32,
) -> f32 {
    let mut random = Random::new(name);
    let mut worst = 0.0;
    let mut at = String::new();
    for &(low, high) in ranges {
        for _ in 0..SAMPLES {
            let x = F::narrow(random.in_range(low, high));
            let d = host_error(ours(x), theirs(x));
            if d > worst {
                worst = d;
                at = hex(x);
            }
        }
    }
    println!("glibc {name}: worst {worst:.3} ulp, at {name}({at})");
    assert!(worst < bound, "{name}: {worst} ulps from glibc at {at}, over {bound}");
    worst
}

/// [`host_d_d`] for a two-argument function, with `x` drawn from each of
/// `xs` and `y` from each of `ys`.
pub(crate) fn host_dd_d<F: Float>(
    name: &str,
    ours: impl Fn(F, F) -> F,
    theirs: impl Fn(F, F) -> F,
    xs: &[(f64, f64)],
    ys: &[(f64, f64)],
    bound: f32,
) -> f32 {
    let mut random = Random::new(name);
    let mut worst = 0.0;
    let mut at = String::new();
    for &(x_low, x_high) in xs {
        for &(y_low, y_high) in ys {
            for _ in 0..SAMPLES {
                let x = F::narrow(random.in_range(x_low, x_high));
                let y = F::narrow(random.in_range(y_low, y_high));
                let d = host_error(ours(x, y), theirs(x, y));
                if d > worst {
                    worst = d;
                    at = format!("{}, {}", hex(x), hex(y));
                }
            }
        }
    }
    println!("glibc {name}: worst {worst:.3} ulp, at {name}({at})");
    assert!(worst < bound, "{name}: {worst} ulps from glibc at {at}, over {bound}");
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_parse_as_libc_test_writes_them() {
        let row = Row::parse(
            "t.h:1".to_owned(),
            "T(RU, -0x1.8p+0,   inf, 0x1.a7ebep-1, INEXACT|UNDERFLOW) // note",
        );
        assert_eq!(row.mode, FE_UPWARD);
        assert_eq!(row.f64(0), -1.5);
        assert_eq!(row.f64(1), f64::INFINITY);
        assert_eq!(row.f32(2).to_bits(), 0x3f53_f5f0);
        assert_eq!(row.except, FE_INEXACT | FE_UNDERFLOW);
        let row = Row::parse("t.h:2".to_owned(), "T(RN, 0x1p-1, FP_ILOGB0, 0)");
        assert_eq!(row.int(1), i64::from(c_int::MIN));
        assert_eq!(row.except, 0);
    }

    #[test]
    fn ulp_errors_are_measured_as_libc_test_does() {
        assert_eq!(f64::ulperr(1.0, 1.0, 0.25), 0.25);
        assert_eq!(f64::ulperr(-0.0, 0.0, 0.0), f32::INFINITY);
        assert_eq!(f64::ulperr(f64::from_bits(0x3ff0_0000_0000_0001), 1.0, 0.0), 1.0);
        assert_eq!(f32::ulperr(f32::from_bits(0x3f80_0002), 1.0, -0.5), 1.5);
        assert_eq!(hex(-1.5f64), "-0x1.8p+0");
        assert_eq!(hex(f64::from_bits(1)), "0x0.0000000000001p-1022");
    }
}
