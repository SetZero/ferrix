//! Sorting strings (zsh's `sort.c`): the comparison behind `(o)`, `(O)`,
//! `(on)`, `(oi)` and glob ordering.

use crate::shell::Shell;
use crate::tok::META;
use crate::utils::at;

pub(crate) const SORTIT_ANYOLDHOW: i32 = 0;
pub(crate) const SORTIT_IGNORING_CASE: i32 = 1;
pub(crate) const SORTIT_NUMERICALLY: i32 = 2;
pub(crate) const SORTIT_NUMERICALLY_SIGNED: i32 = 4;
pub(crate) const SORTIT_BACKWARDS: i32 = 8;
pub(crate) const SORTIT_IGNORING_BACKSLASHES: i32 = 16;
pub(crate) const SORTIT_SOMEHOW: i32 = 32;

/// One element being sorted (zsh's `struct sortelt`).
struct SortElt {
    cmp: Vec<u8>,
    /// Length when the string holds an embedded NUL, else -1.
    len: i64,
}

impl Shell {
    /// `strcoll` in the shell's collation locale.
    pub(crate) fn strcoll(&self, a: &[u8], b: &[u8]) -> i32 {
        let a = cstr(a);
        let b = cstr(b);
        match a.cmp(b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }

    /// zsh's `eltpcmp`.
    fn eltpcmp(
        &self,
        ae: &SortElt,
        be: &SortElt,
        sortdir: i32,
        sortnobslash: bool,
        sortnumeric: i32,
    ) -> i32 {
        let mut as_ = 0usize;
        let mut bs = 0usize;
        let a = &ae.cmp;
        let b = &be.cmp;
        if ae.len != -1 || be.len != -1 {
            let mut laststarta = 0usize;
            let mut len = if ae.len != -1 {
                if be.len != -1 && ae.len > be.len {
                    be.len
                } else {
                    ae.len
                }
            } else {
                be.len
            };
            let (mut ca, mut cb) = (0usize, 0usize);
            while at(a, ca) == at(b, cb) && len > 0 {
                len -= 1;
                if at(a, ca) == 0 {
                    if ae.len == -1 || be.len == -1 {
                        break;
                    }
                    laststarta = ca + 1;
                }
                ca += 1;
                cb += 1;
            }
            if at(a, ca) == at(b, cb) && ae.len != be.len {
                if ae.len != -1 {
                    if be.len != -1 {
                        return i32::try_from(ae.len - be.len).unwrap_or(0) * sortdir;
                    }
                    return sortdir;
                }
                return -sortdir;
            }
            as_ = laststarta;
            bs = laststarta;
        }
        if sortnobslash {
            while at(a, as_) != 0 && at(b, bs) != 0 {
                if at(a, as_) == b'\\' {
                    as_ += 1;
                }
                if at(b, bs) == b'\\' {
                    bs += 1;
                }
                if at(a, as_) != at(b, bs) || at(a, as_) == 0 {
                    break;
                }
                as_ += 1;
                bs += 1;
            }
        }
        let mut cmp = self.strcoll(a.get(as_..).unwrap_or(&[]), b.get(bs..).unwrap_or(&[]));
        if sortnumeric != 0 {
            let ao = 0usize;
            let mut mul = 0;
            while at(a, as_) == at(b, bs) && at(a, as_) != 0 {
                as_ += 1;
                bs += 1;
            }
            if sortnumeric < 0 {
                if at(a, as_) == b'-'
                    && at(a, as_ + 1).is_ascii_digit()
                    && at(b, bs).is_ascii_digit()
                {
                    cmp = -1;
                    mul = 1;
                } else if at(b, bs) == b'-'
                    && at(b, bs + 1).is_ascii_digit()
                    && at(a, as_).is_ascii_digit()
                {
                    cmp = 1;
                    mul = 1;
                }
            }
            if mul == 0 && (at(a, as_).is_ascii_digit() || at(b, bs).is_ascii_digit()) {
                while as_ > ao && at(a, as_ - 1).is_ascii_digit() {
                    as_ -= 1;
                    bs -= 1;
                }
                mul = if sortnumeric < 0 && as_ > ao && at(a, as_ - 1) == b'-' {
                    -1
                } else {
                    1
                };
                if at(a, as_).is_ascii_digit() && at(b, bs).is_ascii_digit() {
                    while at(a, as_) == b'0' {
                        as_ += 1;
                    }
                    while at(b, bs) == b'0' {
                        bs += 1;
                    }
                    while at(a, as_).is_ascii_digit() && at(a, as_) == at(b, bs) {
                        as_ += 1;
                        bs += 1;
                    }
                    if at(a, as_).is_ascii_digit() || at(b, bs).is_ascii_digit() {
                        cmp = mul * (i32::from(at(a, as_)) - i32::from(at(b, bs)));
                        while at(a, as_).is_ascii_digit() && at(b, bs).is_ascii_digit() {
                            as_ += 1;
                            bs += 1;
                        }
                        if at(a, as_).is_ascii_digit() && !at(b, bs).is_ascii_digit() {
                            return mul * sortdir;
                        }
                        if at(b, bs).is_ascii_digit() && !at(a, as_).is_ascii_digit() {
                            return -mul * sortdir;
                        }
                    }
                }
            }
        }
        sortdir * cmp
    }

    /// zsh's `zstrcmp` on unmetafied strings.
    pub(crate) fn zstrcmp(&self, a: &[u8], b: &[u8], sortflags: i32) -> i32 {
        let ae = SortElt {
            cmp: a.to_vec(),
            len: -1,
        };
        let be = SortElt {
            cmp: b.to_vec(),
            len: -1,
        };
        let numeric = if sortflags & SORTIT_NUMERICALLY_SIGNED != 0 {
            -1
        } else {
            i32::from(sortflags & SORTIT_NUMERICALLY != 0)
        };
        self.eltpcmp(
            &ae,
            &be,
            1,
            sortflags & SORTIT_IGNORING_BACKSLASHES != 0,
            numeric,
        )
    }

    /// zsh's `strmetasort` on metafied strings.
    pub(crate) fn strmetasort(&self, array: &mut Vec<Vec<u8>>, sortwhat: i32) {
        if array.len() < 2 {
            return;
        }
        let mut elts: Vec<(SortElt, Vec<u8>)> = Vec::with_capacity(array.len());
        for orig in array.drain(..) {
            let has_meta = orig.contains(&META);
            let mut needlen = false;
            let mut src: Vec<u8> = if has_meta {
                let mut out = Vec::with_capacity(orig.len());
                let mut i = 0;
                while i < orig.len() {
                    let c = at(&orig, i);
                    if c == META {
                        i += 1;
                        let v = at(&orig, i) ^ 32;
                        if v == 0 {
                            needlen = true;
                        }
                        out.push(v);
                    } else {
                        out.push(c);
                    }
                    i += 1;
                }
                out
            } else {
                orig.clone()
            };
            if sortwhat & SORTIT_IGNORING_CASE != 0 {
                if self.isset(crate::options::MULTIBYTE) {
                    let text = String::from_utf8_lossy(&src).to_lowercase().into_bytes();
                    if std::str::from_utf8(&src).is_ok() {
                        src = text;
                    } else {
                        src = src.iter().map(u8::to_ascii_lowercase).collect();
                    }
                } else {
                    src = src.iter().map(u8::to_ascii_lowercase).collect();
                }
            }
            if sortwhat & SORTIT_IGNORING_BACKSLASHES != 0 {
                src.retain(|&c| c != b'\\');
            }
            let len = if needlen {
                i64::try_from(src.len()).unwrap_or(0)
            } else {
                -1
            };
            elts.push((SortElt { cmp: src, len }, orig));
        }
        let sortdir = if sortwhat & SORTIT_BACKWARDS != 0 {
            -1
        } else {
            1
        };
        let numeric = if sortwhat & SORTIT_NUMERICALLY_SIGNED != 0 {
            -1
        } else {
            i32::from(sortwhat & SORTIT_NUMERICALLY != 0)
        };
        elts.sort_by(|x, y| self.eltpcmp(&x.0, &y.0, sortdir, false, numeric).cmp(&0));
        array.extend(elts.into_iter().map(|(_, o)| o));
    }
}

/// The part of `s` before its first NUL, as `strcoll` sees it.
fn cstr(s: &[u8]) -> &[u8] {
    match s.iter().position(|&c| c == 0) {
        Some(p) => s.get(..p).unwrap_or(&[]),
        None => s,
    }
}
