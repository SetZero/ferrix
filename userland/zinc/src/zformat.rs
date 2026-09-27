//! `zformat`: zsh's `zsh/zutil` formatter, following zsh 5.9's
//! `Src/Modules/zutil.c` (`bin_zformat`, `zformat_substring`).
//!
//! oh-my-zsh's git prompt is `vcs_info`, and `vcs_info` builds every piece of
//! it with `zformat -f`. Without the builtin each prompt inside a git
//! repository printed `VCS_INFO_formats:87: command not found: zformat` and
//! showed no branch.
//!
//! * `zformat -f param format c:value ...` replaces each `%c` in `format` by
//!   its value: `%5c` pads it on the right to five characters, `%-5c` on the
//!   left, `%.2c` cuts it to two, and `%3(c.true.false)` chooses a branch by
//!   whether `c`'s value, as a number, is 3 (0 when no number is given, and
//!   an absent `c` counts as 0). A `%`-sequence whose character has no value
//!   is kept as written, so a prompt's own `%F{red}` passes through; `%%` is
//!   `%` and `%)` is `)`, as zutil.c gives them by presetting the two.
//! * `zformat -a array sep left:right ...` pads each left side to the widest
//!   one so the separators line up; a string with no colon is left alone,
//!   and one with nothing after its colon loses the colon. `\:` in a left
//!   side is a colon that does not split.
//!
//! Widths count characters, not bytes.

use crate::shell::{Shell, Value};
use crate::tok;

/// Run `zformat` with `args`, the builtin's name not among them.
pub(crate) fn run(sh: &mut Shell, args: &[Vec<u8>]) -> i32 {
    let args: Vec<Vec<u8>> = args.iter().map(|arg| tok::unmetafy(arg)).collect();
    let Some(option) = args.first() else {
        sh.error_at("zformat", "not enough arguments");
        return 1;
    };
    match option.as_slice() {
        b"-f" => {
            let (Some(param), Some(format)) = (args.get(1), args.get(2)) else {
                sh.error_at("zformat", "not enough arguments");
                return 1;
            };
            let mut specs = Specs::new();
            for spec in args.get(3..).unwrap_or_default() {
                let (Some(&key), Some(b':')) = (spec.first(), spec.get(1)) else {
                    return invalid(sh, spec);
                };
                if !key.is_ascii() {
                    return invalid(sh, spec);
                }
                specs.set(key, spec.get(2..).unwrap_or_default().to_vec());
            }
            let result = format_string(format, &specs);
            sh.set_value(param, Value::Scalar(tok::metafy(&result)));
            0
        }
        b"-a" => {
            let (Some(param), Some(sep)) = (args.get(1), args.get(2)) else {
                sh.error_at("zformat", "not enough arguments");
                return 1;
            };
            let lines = align(sep, args.get(3..).unwrap_or_default());
            sh.set_value(
                param,
                Value::Array(lines.iter().map(|line| tok::metafy(line)).collect()),
            );
            0
        }
        _ => {
            sh.error_at(
                "zformat",
                &format!("invalid option: {}", String::from_utf8_lossy(option)),
            );
            1
        }
    }
}

/// zsh's complaint about a spec that is not `c:value`.
fn invalid(sh: &Shell, spec: &[u8]) -> i32 {
    sh.error_at(
        "zformat",
        &format!("invalid argument: {}", String::from_utf8_lossy(spec)),
    );
    1
}

/// The value each character stands for.
struct Specs {
    values: Vec<Option<Vec<u8>>>,
}

impl Specs {
    /// With `%` and `)` standing for themselves, as zutil.c presets them.
    fn new() -> Specs {
        let mut specs = Specs {
            values: vec![None; 128],
        };
        specs.set(b'%', b"%".to_vec());
        specs.set(b')', b")".to_vec());
        specs
    }

    fn set(&mut self, key: u8, value: Vec<u8>) {
        if let Some(slot) = self.values.get_mut(usize::from(key)) {
            *slot = Some(value);
        }
    }

    fn get(&self, key: u8) -> Option<&[u8]> {
        self.values.get(usize::from(key))?.as_deref()
    }
}

/// `format` with every `%`-sequence replaced.
fn format_string(format: &[u8], specs: &Specs) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    substitute(format, &mut at, specs, None, true, &mut out);
    out
}

/// Copy `text` from `at` into `out`, replacing `%`-sequences, until `end` or
/// the end of the text; `at` is left on `end`. With `emit` false nothing is
/// written, which is how the branch a ternary does not take is walked past.
fn substitute(
    text: &[u8],
    at: &mut usize,
    specs: &Specs,
    end: Option<u8>,
    emit: bool,
    out: &mut Vec<u8>,
) {
    while let Some(&byte) = text.get(*at) {
        if Some(byte) == end {
            return;
        }
        if byte != b'%' {
            if emit {
                out.push(byte);
            }
            *at += 1;
            continue;
        }
        let start = *at;
        *at += 1;
        let right = text.get(*at) == Some(&b'-');
        let mut right = right;
        if right {
            *at += 1;
        }
        let min = number(text, at);
        let testit = text.get(*at) == Some(&b'(');
        if testit && text.get(*at + 1) == Some(&b'-') {
            right = true;
            *at += 1;
        }
        let mut max = None;
        if text.get(*at) == Some(&b'.') || testit {
            *at += 1;
            max = number(text, at);
        }
        let Some(&key) = text.get(*at) else {
            // A `%` at the very end, and whatever width came with it.
            if emit {
                out.extend_from_slice(text.get(start..).unwrap_or_default());
            }
            return;
        };
        *at += 1;
        if testit {
            ternary(text, at, specs, key, (min, max, right), emit, out);
            continue;
        }
        match specs.get(key) {
            Some(value) if emit => pad(value, min, max, right, out),
            Some(_) => {}
            None => {
                if emit {
                    out.extend_from_slice(text.get(start..*at).unwrap_or_default());
                }
            }
        }
    }
}

/// `%N(c<d>true<d>false)`: the delimiter follows `c`, and the parenthesis
/// closes it. `at` is just after `c`.
fn ternary(
    text: &[u8],
    at: &mut usize,
    specs: &Specs,
    key: u8,
    (min, max, right): (Option<usize>, Option<usize>, bool),
    emit: bool,
    out: &mut Vec<u8>,
) {
    let test = i64::try_from(min.or(max).unwrap_or(0)).unwrap_or(i64::MAX);
    let test = if right { -test } else { test };
    let value = specs.get(key).map_or(0, integer);
    let yes = value == test;
    let Some(&delimiter) = text.get(*at) else {
        return;
    };
    *at += 1;
    substitute(text, at, specs, Some(delimiter), emit && yes, out);
    if text.get(*at) == Some(&delimiter) {
        *at += 1;
    }
    substitute(text, at, specs, Some(b')'), emit && !yes, out);
    if text.get(*at) == Some(&b')') {
        *at += 1;
    }
}

/// A run of digits at `at`, consumed.
fn number(text: &[u8], at: &mut usize) -> Option<usize> {
    let mut value: Option<usize> = None;
    while let Some(&digit) = text.get(*at).filter(|byte| byte.is_ascii_digit()) {
        value = Some(
            value
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(usize::from(digit - b'0')),
        );
        *at += 1;
    }
    value
}

/// A spec's value read as a number, as zsh's arithmetic reads an integer:
/// anything that is not one counts as 0, as an unset variable would.
fn integer(value: &[u8]) -> i64 {
    std::str::from_utf8(value)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

/// `value` cut to `max` characters and padded to `min`, on the left when
/// `right`.
fn pad(value: &[u8], min: Option<usize>, max: Option<usize>, right: bool, out: &mut Vec<u8>) {
    let text = String::from_utf8_lossy(value);
    let cut: String = match max {
        Some(max) => text.chars().take(max).collect(),
        None => text.into_owned(),
    };
    let fill = min.unwrap_or(0).saturating_sub(cut.chars().count());
    if right {
        out.extend(std::iter::repeat_n(b' ', fill));
    }
    out.extend_from_slice(cut.as_bytes());
    if !right {
        out.extend(std::iter::repeat_n(b' ', fill));
    }
}

/// `-a`'s lines: each `left:right` with its left side padded so the
/// separators line up.
fn align(sep: &[u8], specs: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let split: Vec<(Vec<u8>, Option<Vec<u8>>)> = specs.iter().map(|spec| split(spec)).collect();
    let widest = split
        .iter()
        .filter(|(_, right)| right.as_ref().is_some_and(|right| !right.is_empty()))
        .map(|(left, _)| String::from_utf8_lossy(left).chars().count())
        .max()
        .unwrap_or(0);
    split
        .into_iter()
        .map(|(left, right)| match right {
            Some(right) if !right.is_empty() => {
                let mut line = left.clone();
                let width = String::from_utf8_lossy(&left).chars().count();
                line.extend(std::iter::repeat_n(b' ', widest.saturating_sub(width)));
                line.extend_from_slice(sep);
                line.extend_from_slice(&right);
                line
            }
            _ => left,
        })
        .collect()
}

/// A spec cut at its first colon that no backslash escapes, the escapes
/// removed from its left side; `None` on the right when it has no colon.
fn split(spec: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    let mut left = Vec::new();
    let mut at = 0;
    while let Some(&byte) = spec.get(at) {
        match byte {
            b'\\' if spec.get(at + 1) == Some(&b':') => {
                left.push(b':');
                at += 2;
            }
            b':' => return (left, Some(spec.get(at + 1..).unwrap_or_default().to_vec())),
            _ => {
                left.push(byte);
                at += 1;
            }
        }
    }
    (left, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(text: &str, specs: &[(u8, &str)]) -> String {
        let mut table = Specs::new();
        for &(key, value) in specs {
            table.set(key, value.as_bytes().to_vec());
        }
        String::from_utf8(format_string(text.as_bytes(), &table)).unwrap()
    }

    /// Each expected value is what zsh 5.9's `zformat -f` printed for it.
    #[test]
    fn f_matches_zsh() {
        let specs = [
            (b'b', "main"),
            (b'a', "rebase"),
            (b'u', "*"),
            (b's', "git"),
            (b'c', "3"),
            (b'z', "0"),
        ];
        assert_eq!(
            format(
                "[%b|%a] %F{red}%u%% %5s| %-5s| %.2s| %3(c.yes.no) %(z.empty.full) %1(c.one.x%)y)",
                &specs
            ),
            "[main|rebase] %F{red}*% git  |   git| gi| yes empty x)y"
        );
        assert_eq!(format("%x %", &[(b'x', "1")]), "1 %");
        assert_eq!(format("%(-1c.neg.pos)", &[(b'c', "-1")]), "neg");
        assert_eq!(
            format(
                "[%(q.t.f)] [%(2c.two.not)] [%(c:%s is set:%s unset)] [%-3(c.a.b)] [%q]",
                &[(b'c', "2"), (b's', "S")]
            ),
            "[t] [two] [S unset] [b] [%q]"
        );
        assert_eq!(
            format("%(c.%(d.both.c-only).none)", &[(b'c', "0"), (b'd', "0")]),
            "both"
        );
        assert_eq!(format("%3.1s|%-4.2s|", &[(b's', "hello")]), "h  |  he|");
    }

    /// `vcs_info`'s default git format, with a prompt escape in a value.
    #[test]
    fn vcs_info_s_format_comes_out_as_zsh_s() {
        assert_eq!(
            format(
                "%F{blue}(%s)-[%b]%u%c-%f",
                &[
                    (b's', "git"),
                    (b'b', "main"),
                    (b'u', "%F{red}*"),
                    (b'c', "")
                ]
            ),
            "%F{blue}(git)-[main]%F{red}*-%f"
        );
    }

    /// What zsh 5.9's `zformat -a` printed.
    #[test]
    fn a_matches_zsh() {
        let specs: Vec<Vec<u8>> = ["a:first", "long\\:left:second", "nocolon", "empty:"]
            .iter()
            .map(|spec| spec.as_bytes().to_vec())
            .collect();
        let lines: Vec<String> = align(b" -- ", &specs)
            .into_iter()
            .map(|line| String::from_utf8(line).unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                "a         -- first",
                "long:left -- second",
                "nocolon",
                "empty"
            ]
        );
        let wide: Vec<Vec<u8>> = ["é:x", "ab:y"]
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect();
        let lines: Vec<String> = align(b":", &wide)
            .into_iter()
            .map(|line| String::from_utf8(line).unwrap())
            .collect();
        assert_eq!(lines, ["é :x", "ab:y"]);
    }
}
