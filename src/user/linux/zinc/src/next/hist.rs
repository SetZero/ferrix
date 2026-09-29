//! The parts of zsh's `hist.c` expansion needs: the history-style colon
//! modifiers (`:h`, `:t`, `:r`, `:e`, `:a`, `:A`, `:s`), case modification,
//! and splitting a string into shell words (`bufferwords`, for `${(z)}`).

use crate::options::*;
use crate::shell::Shell;
use crate::tok::{self, META, POUND};
use crate::utils::{at, from, sub};

pub(crate) const CASMOD_NONE: i32 = 0;
pub(crate) const CASMOD_UPPER: i32 = 1;
pub(crate) const CASMOD_LOWER: i32 = 2;
pub(crate) const CASMOD_CAPS: i32 = 3;

pub(crate) const HFILE_APPEND: i32 = 0x0001;
pub(crate) const HFILE_SKIPOLD: i32 = 0x0002;
pub(crate) const HFILE_SKIPDUPS: i32 = 0x0004;
pub(crate) const HFILE_SKIPFOREIGN: i32 = 0x0008;
pub(crate) const HFILE_FAST: i32 = 0x0010;
pub(crate) const HFILE_NO_REWRITE: i32 = 0x0020;
pub(crate) const HFILE_USE_OPTIONS: i32 = 0x8000;

pub(crate) const LEXFLAGS_ACTIVE: i32 = 0x0001;
pub(crate) const LEXFLAGS_ZLE: i32 = 0x0002;
pub(crate) const LEXFLAGS_COMMENTS_KEEP: i32 = 0x0004;
pub(crate) const LEXFLAGS_COMMENTS_STRIP: i32 = 0x0008;
pub(crate) const LEXFLAGS_NEWLINE: i32 = 0x0010;

impl Shell {
    /// zsh's `chabspath`. Returns false for a path that climbs above `/`
    /// oddly.
    pub(crate) fn chabspath(&self, junk: &mut Vec<u8>) -> bool {
        if junk.is_empty() {
            return true;
        }
        if at(junk, 0) != b'/' {
            let mut here = tok::metafy(&self.zgetcwd());
            if here.last() != Some(&b'/') {
                here.push(b'/');
            }
            here.extend_from_slice(junk);
            *junk = here;
        }
        let src = junk.clone();
        let mut dest: Vec<u8> = Vec::with_capacity(src.len());
        let mut cur = 0usize;
        loop {
            let c = at(&src, cur);
            if c == b'/' && cur < src.len() {
                dest.push(b'/');
                cur += 1;
                while at(&src, cur) == b'/' && cur < src.len() {
                    cur += 1;
                }
            } else if cur >= src.len() {
                while dest.len() > 1 && dest.last() == Some(&b'/') {
                    let _ = dest.pop();
                }
                break;
            } else if c == b'.'
                && at(&src, cur + 1) == b'.'
                && (cur + 2 >= src.len() || at(&src, cur + 2) == b'/')
            {
                if cur == 0 || dest.is_empty() || (dest.len() > 2 && dest.ends_with(b"../")) {
                    dest.extend_from_slice(b"..");
                    cur += 2;
                } else if dest.len() > 1 {
                    // Back up over the last component.
                    let _ = dest.pop();
                    while dest.len() > 1 && dest.last() != Some(&b'/') {
                        let _ = dest.pop();
                    }
                    if dest.last() != Some(&b'/') {
                        let _ = dest.pop();
                    }
                    cur += 2;
                    if at(&src, cur) == b'/' && cur < src.len() {
                        cur += 1;
                    }
                } else if dest.len() == 1 {
                    cur += 2;
                } else {
                    return false;
                }
            } else if c == b'.' && (at(&src, cur + 1) == b'/' || cur + 1 >= src.len()) {
                cur += 1;
                while at(&src, cur) == b'/' && cur < src.len() {
                    cur += 1;
                }
            } else {
                while cur < src.len() && at(&src, cur) != b'/' {
                    let b = at(&src, cur);
                    dest.push(b);
                    cur += 1;
                    if b == META {
                        dest.push(at(&src, cur));
                        cur += 1;
                    }
                }
            }
        }
        *junk = dest;
        true
    }

    /// zsh's `chrealpath`: resolve symbolic links (`:A` after `chabspath`,
    /// `:P` on its own).
    pub(crate) fn chrealpath(&self, junk: &mut Vec<u8>, mode: u8) -> bool {
        if junk.is_empty() {
            return true;
        }
        if mode == b'A' && !self.chabspath(junk) {
            return false;
        }
        if at(junk, 0) != b'/' {
            return false;
        }
        let raw = tok::unmetafy(junk);
        // Resolve the longest prefix that exists, keep the rest literally.
        let mut cut = raw.len();
        loop {
            let prefix = sub(&raw, 0, cut);
            let path = if prefix.is_empty() {
                b"/".as_slice()
            } else {
                prefix
            };
            use std::os::unix::ffi::OsStrExt;
            if let Ok(real) = std::fs::canonicalize(std::ffi::OsStr::from_bytes(path)) {
                let mut out = real.as_os_str().as_bytes().to_vec();
                out.extend_from_slice(from(&raw, cut));
                *junk = tok::metafy(&out);
                return true;
            }
            match sub(&raw, 0, cut).iter().rposition(|&c| c == b'/') {
                Some(0) | None => {
                    *junk = tok::metafy(&raw);
                    return true;
                }
                Some(p) => cut = p,
            }
        }
    }

    /// zsh's `zgetcwd`.
    pub(crate) fn zgetcwd(&self) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        std::env::current_dir()
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .unwrap_or_else(|_| tok::unmetafy(&self.pwd))
    }

    /// zsh's `casemodify`.
    pub(crate) fn casemodify(&self, s: &[u8], how: i32) -> Vec<u8> {
        let mut out = Vec::with_capacity(s.len());
        let mut nextupper = true;
        if self.isset(MULTIBYTE) {
            let mut i = 0;
            while i < s.len() {
                let (len, wc) = crate::utils::mb_metacharlenconv(self, from(s, i));
                let len = len.max(1);
                let Some(wc) = wc else {
                    out.extend_from_slice(sub(s, i, i + len));
                    i += len;
                    nextupper = true;
                    continue;
                };
                let ch = char::from_u32(wc);
                let mut newc: Option<char> = None;
                match how {
                    CASMOD_LOWER => {
                        if let Some(c) = ch.filter(|c| c.is_uppercase()) {
                            newc = c.to_lowercase().next();
                        }
                    }
                    CASMOD_UPPER => {
                        if let Some(c) = ch.filter(|c| c.is_lowercase()) {
                            newc = c.to_uppercase().next();
                        }
                    }
                    _ => {
                        if !crate::utils::is_combining(wc) {
                            if !crate::utils::iswalnum(wc) {
                                nextupper = true;
                            } else if nextupper {
                                if let Some(c) = ch.filter(|c| c.is_lowercase()) {
                                    newc = c.to_uppercase().next();
                                }
                                nextupper = false;
                            } else if let Some(c) = ch.filter(|c| c.is_uppercase()) {
                                newc = c.to_lowercase().next();
                            }
                        }
                    }
                }
                match newc {
                    Some(c) => {
                        let mut buf = [0u8; 4];
                        out.extend(tok::metafy(c.encode_utf8(&mut buf).as_bytes()));
                    }
                    None => out.extend_from_slice(sub(s, i, i + len)),
                }
                i += len;
            }
            return out;
        }
        let mut i = 0;
        while i < s.len() {
            let mut c = at(s, i);
            if c == META {
                i += 1;
                c = at(s, i) ^ 32;
            }
            i += 1;
            let mut modified = false;
            match how {
                CASMOD_LOWER => {
                    if c.is_ascii_uppercase() {
                        c = c.to_ascii_lowercase();
                        modified = true;
                    }
                }
                CASMOD_UPPER => {
                    if c.is_ascii_lowercase() {
                        c = c.to_ascii_uppercase();
                        modified = true;
                    }
                }
                _ => {
                    if !self.ialnum(c) {
                        nextupper = true;
                    } else if nextupper {
                        if c.is_ascii_lowercase() {
                            c = c.to_ascii_uppercase();
                            modified = true;
                        }
                        nextupper = false;
                    } else if c.is_ascii_uppercase() {
                        c = c.to_ascii_lowercase();
                        modified = true;
                    }
                }
            }
            if (modified || true) && tok::is_meta(c) {
                out.push(META);
                out.push(c ^ 32);
            } else {
                out.push(c);
            }
        }
        out
    }

    /// zsh's `subst` for `:s/l/r/`: `false` if nothing was substituted.
    pub(crate) fn hist_subst(
        &mut self,
        s: &mut Vec<u8>,
        inp: &[u8],
        out: &[u8],
        gbal: bool,
    ) -> bool {
        let (mut inp, mut gbal) = (inp.to_vec(), gbal);
        if inp.is_empty() {
            inp = s.clone();
            gbal = false;
        }
        if self.isset(HISTSUBSTPATTERN) {
            let mut fl = crate::glob::SUB_LONG | crate::glob::SUB_REST | crate::glob::SUB_RETFAIL;
            if gbal {
                fl |= crate::glob::SUB_GLOBAL;
            }
            let mut k = 0;
            if matches!(at(&inp, 0), b'#' | POUND) {
                fl |= crate::glob::SUB_START;
                k += 1;
            }
            if at(&inp, k) == b'%' {
                k += 1;
                fl |= crate::glob::SUB_END;
            }
            if k == 0 {
                fl |= crate::glob::SUB_SUBSTR;
            }
            let mut pin = from(&inp, k).to_vec();
            let mut pout = out.to_vec();
            if self.parse_subst_string(&mut pin) || self.errflag() {
                return false;
            }
            if self.parse_subst_string(&mut pout) || self.errflag() {
                return false;
            }
            let pin = self.singsub(&pin);
            return self.getmatch(s, &pin, fl, 1, Some(pout));
        }
        let Some(first) = find_sub(s, &inp, 0) else {
            return false;
        };
        let sptr = convamps(out, &inp);
        let mut pos = first;
        loop {
            let mut ns = sub(s, 0, pos).to_vec();
            ns.extend_from_slice(&sptr);
            let after = pos + inp.len();
            let next_start = ns.len();
            ns.extend_from_slice(from(s, after));
            *s = ns;
            if !gbal {
                break;
            }
            match find_sub(s, &inp, next_start) {
                Some(p) => pos = p,
                None => break,
            }
        }
        true
    }

    /// zsh's `bufferwords`: split `buf` into shell words with the lexer.
    pub(crate) fn bufferwords(&mut self, buf: &[u8], flags: i32) -> Vec<Vec<u8>> {
        let mut words = Vec::new();
        let mut text = buf.to_vec();
        tok::untokenize(&mut text);
        text.push(b' ');
        let mut opts = self.lex_opts();
        opts.rcquotes = false;
        opts.comments = flags & (LEXFLAGS_COMMENTS_KEEP | LEXFLAGS_COMMENTS_STRIP) != 0;
        opts.aliases = false;
        let mut lx = crate::lex::Lexer::new(text, opts);
        lx.lexflags = flags | LEXFLAGS_ACTIVE;
        let noalias = NoAliases(opts);
        loop {
            lx.ctxtlex(&noalias);
            match lx.tok {
                crate::lex::Tok::Endinput => break,
                crate::lex::Tok::Lexerr => {
                    if let Some(s) = &lx.tokstr {
                        let mut p = s.clone();
                        tok::untokenize(&mut p);
                        if p.last() == Some(&b' ') {
                            let _ = p.pop();
                        }
                        if !p.is_empty() {
                            words.push(p);
                        }
                    }
                    break;
                }
                _ => {}
            }
            if let Some(s) = &lx.tokstr {
                let mut p = match lx.tok {
                    crate::lex::Tok::Envarray => {
                        let mut t = s.clone();
                        t.extend_from_slice(b"=(");
                        t
                    }
                    crate::lex::Tok::Dinpar => {
                        let mut t = b"((".to_vec();
                        t.extend_from_slice(s);
                        t.extend_from_slice(b"))");
                        t
                    }
                    _ => s.clone(),
                };
                if !p.is_empty() {
                    tok::untokenize(&mut p);
                    if lx.input.rest().is_empty() && p.last() == Some(&b' ') {
                        let _ = p.pop();
                    }
                    words.push(p);
                }
            } else if lx.tok.is_redir() && lx.tokfd >= 0 {
                words.push(format!("{}{}", lx.tokfd, lx.tok.text()).into_bytes());
            } else if lx.tok != crate::lex::Tok::Newlin {
                let t = lx.tok.text();
                if !t.is_empty() {
                    words.push(t.as_bytes().to_vec());
                }
            }
        }
        words
    }
}

/// zsh's `remtpath` (`:h`).
pub(crate) fn remtpath(s: &mut Vec<u8>, count: i32) -> bool {
    if s.is_empty() {
        *s = b".".to_vec();
        return false;
    }
    let mut end = s.len() as isize - 1;
    while end >= 0 && at(s, usize::try_from(end).unwrap_or(0)) == b'/' {
        end -= 1;
    }
    if count == 0 {
        while end >= 0 && at(s, usize::try_from(end).unwrap_or(0)) != b'/' {
            end -= 1;
        }
    }
    if end < 0 {
        *s = if at(s, 0) == b'/' {
            b"/".to_vec()
        } else {
            b".".to_vec()
        };
        return false;
    }
    let end = usize::try_from(end).unwrap_or(0);
    if count != 0 {
        let mut count = count;
        let mut p = 0usize;
        while p < end {
            if at(s, p) == b'/' {
                count -= 1;
                if count <= 0 {
                    let cut = if p == 0 { 1 } else { p };
                    s.truncate(cut);
                    return true;
                }
                while at(s, p + 1) == b'/' {
                    p += 1;
                }
            }
            p += 1;
        }
        return true;
    }
    let mut str_ = end;
    while str_ > 0 && at(s, str_ - 1) == b'/' {
        str_ -= 1;
    }
    if str_ == 0 {
        str_ = 1;
        if at(s, str_) == b'/' && at(s, str_ + 1) != b'/' {
            str_ += 1;
        }
    }
    s.truncate(str_);
    true
}

/// zsh's `remtext` (`:r`).
pub(crate) fn remtext(s: &mut Vec<u8>) -> bool {
    let mut i = s.len();
    while i > 0 {
        i -= 1;
        let c = at(s, i);
        if c == b'/' {
            break;
        }
        if c == b'.' {
            s.truncate(i);
            return true;
        }
    }
    false
}

/// zsh's `rembutext` (`:e`).
pub(crate) fn rembutext(s: &mut Vec<u8>) -> bool {
    let mut i = s.len();
    while i > 0 {
        i -= 1;
        let c = at(s, i);
        if c == b'/' {
            break;
        }
        if c == b'.' {
            *s = from(s, i + 1).to_vec();
            return true;
        }
    }
    s.clear();
    false
}

/// zsh's `remlpaths` (`:t`).
pub(crate) fn remlpaths(s: &mut Vec<u8>, count: i32) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut count = count;
    let mut end = s.len() as isize - 1;
    if at(s, usize::try_from(end).unwrap_or(0)) == b'/' {
        while end >= 0 && at(s, usize::try_from(end).unwrap_or(0)) == b'/' {
            end -= 1;
        }
        s.truncate(usize::try_from(end + 1).unwrap_or(0));
    }
    let mut p = end;
    loop {
        while p >= 0 {
            let pu = usize::try_from(p).unwrap_or(0);
            if at(s, pu) == b'/' {
                count -= 1;
                if count > 0 {
                    if p > 0 {
                        p -= 1;
                        break;
                    }
                    return true;
                }
                *s = from(s, pu + 1).to_vec();
                return true;
            }
            p -= 1;
        }
        while p >= 0 && at(s, usize::try_from(p).unwrap_or(0)) == b'/' {
            p -= 1;
        }
        if p <= 0 {
            break;
        }
    }
    false
}

/// zsh's `convamps`: `&` in the replacement is the matched text.
fn convamps(out: &[u8], inp: &[u8]) -> Vec<u8> {
    let mut r = Vec::with_capacity(out.len());
    let mut i = 0;
    while i < out.len() {
        let c = at(out, i);
        if c == b'\\' {
            i += 1;
            if i < out.len() {
                r.push(at(out, i));
            }
        } else if c == b'&' {
            r.extend_from_slice(inp);
        } else {
            r.push(c);
        }
        i += 1;
    }
    r
}

fn find_sub(hay: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(hay.len()));
    }
    hay.get(start..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + start)
}

/// The lexer environment for splitting words: no aliases expand.
struct NoAliases(crate::lex::LexOpts);

impl crate::lex::LexEnv for NoAliases {
    fn alias(&self, _name: &[u8]) -> Option<crate::lex::AliasDef> {
        None
    }

    fn suffix_alias(&self, _ext: &[u8]) -> Option<crate::lex::AliasDef> {
        None
    }

    fn opts(&self) -> crate::lex::LexOpts {
        self.0
    }
}

impl Shell {
    /// zsh's `savehistfile`. zinc-next keeps no history ring yet, so there is
    /// nothing to write.
    pub(crate) fn savehistfile(&mut self, _fname: Option<&[u8]>, _err: bool, _writeflags: i32) {}

    /// zsh's `saveandpophiststack`: no `fc -p` stack exists without the
    /// history ring.
    pub(crate) fn saveandpophiststack(&mut self, _pop_through: i32, _writeflags: i32) -> bool {
        false
    }

    /// zsh's `resizehistents`: no ring to trim.
    pub(crate) fn resizehistents(&mut self) {}
}
