//! Substitution (zsh's `subst.c`): the expansions done before a command
//! runs — process, parameter, command and arithmetic substitution, quote
//! removal, brace expansion, `~` and `=` expansion — and the colon
//! modifiers.
//!
//! Words are lists of metafied, tokenized strings, as zsh's linked lists
//! are; a node is an index into the list and an expansion may replace one
//! word with several.

use crate::hist::{CASMOD_CAPS, CASMOD_LOWER, CASMOD_NONE, CASMOD_UPPER};
use crate::math::MNumber;
use crate::options::*;
use crate::params::*;
use crate::shell::{ERRFLAG_HARD, ERRFLAG_INT, Shell};
use crate::sort::*;
use crate::tok::{
    self, BAR, BNULL, BNULLKEEP, DASH, DNULL, EQUALS, HAT, INANG, INBRACE, INBRACK, INPAR,
};
use crate::tok::{
    INPARMATH, MARKER, META, NULARG, OUTANGPROC, OUTBRACE, OUTBRACK, OUTPAR, OUTPARMATH,
};
use crate::tok::{POUND, QSTRING, QTICK, QUEST, SNULL, STAR, STRING, TICK, TILDE};
use crate::utils::{Qt, at, from, lossy, sub};

pub(crate) const PREFORK_TYPESET: i32 = 0x01;
pub(crate) const PREFORK_ASSIGN: i32 = 0x02;
pub(crate) const PREFORK_SINGLE: i32 = 0x04;
pub(crate) const PREFORK_SPLIT: i32 = 0x08;
pub(crate) const PREFORK_SHWORDSPLIT: i32 = 0x10;
pub(crate) const PREFORK_NOSHWORDSPLIT: i32 = 0x20;
pub(crate) const PREFORK_SUBEXP: i32 = 0x40;
pub(crate) const PREFORK_KEY_VALUE: i32 = 0x80;
pub(crate) const PREFORK_NO_UNTOK: i32 = 0x100;

pub(crate) const MULTSUB_WS_AT_START: i32 = 1;
pub(crate) const MULTSUB_WS_AT_END: i32 = 2;
pub(crate) const MULTSUB_PARAM_NAME: i32 = 4;

/// `LF_ARRAY`: the list came from an array.
pub(crate) const LF_ARRAY: i32 = 1;

const QT_NONE: i32 = 0;
const QT_BACKSLASH: i32 = 1;
const QT_SINGLE: i32 = 2;
const QT_DOUBLE: i32 = 3;
const QT_DOLLARS: i32 = 4;
const QT_SINGLE_OPTIONAL: i32 = 6;
const QT_BACKSLASH_PATTERN: i32 = 7;
const QT_BACKSLASH_SHOWNULL: i32 = 8;
const QT_QUOTEDZPUTS: i32 = 9;

fn qt_of(n: i32) -> Qt {
    match n {
        1 => Qt::Backslash,
        2 => Qt::Single,
        3 => Qt::Double,
        4 => Qt::Dollars,
        5 => Qt::Backtick,
        6 => Qt::SingleOptional,
        7 => Qt::BackslashPattern,
        8 => Qt::BackslashShownull,
        _ => Qt::None,
    }
}

fn nulstring() -> Vec<u8> {
    vec![NULARG]
}

fn is_dash(c: u8) -> bool {
    c == b'-' || c == DASH
}

fn isstring(c: u8) -> bool {
    c == b'$' || c == STRING || c == QSTRING
}

fn isbrack(c: u8) -> bool {
    c == b'[' || c == INBRACK
}

/// A list of words being expanded, with zsh's list flags.
#[derive(Debug, Default, Clone)]
pub(crate) struct WordList {
    pub(crate) words: Vec<Vec<u8>>,
    pub(crate) flags: i32,
}

impl WordList {
    pub(crate) fn one(w: Vec<u8>) -> WordList {
        WordList {
            words: vec![w],
            flags: 0,
        }
    }
}

impl Shell {
    /// zsh's `keyvalpairelement`.
    fn keyvalpairelement(&mut self, list: &mut WordList, node: usize) -> Option<usize> {
        let start = list.words.get(node)?.clone();
        if at(&start, 0) != INBRACK {
            return None;
        }
        let end = from(&start, 1).iter().position(|&c| c == OUTBRACK)? + 1;
        let plus = at(&start, end + 1) == b'+' && at(&start, end + 2) == EQUALS;
        if !(at(&start, end + 1) == EQUALS || plus) {
            return None;
        }
        let mut key = self.singsub(sub(&start, 1, end));
        tok::untokenize(&mut key);
        let marker = if plus {
            vec![MARKER, b'+']
        } else {
            vec![MARKER]
        };
        if let Some(slot) = list.words.get_mut(node) {
            *slot = marker;
        }
        list.words.insert(node + 1, key);
        let vstart = if plus { end + 3 } else { end + 2 };
        let mut val = self.singsub(from(&start, vstart));
        tok::untokenize(&mut val);
        list.words.insert(node + 2, val);
        Some(node + 2)
    }

    /// zsh's `prefork`.
    pub(crate) fn prefork(&mut self, list: &mut WordList, flags: i32, ret_flags: &mut i32) {
        let asssub = flags & PREFORK_TYPESET != 0 && self.isset(KSHTYPESET);
        let mut node = 0usize;
        while node < list.words.len() {
            if flags & (PREFORK_SINGLE | PREFORK_ASSIGN) == PREFORK_ASSIGN
                && let Some(ins) = self.keyvalpairelement(list, node)
            {
                node = ins + 1;
                *ret_flags |= PREFORK_KEY_VALUE;
                continue;
            }
            if self.errflag() {
                return;
            }
            if self.isset(SHFILEEXPANSION) {
                if let Some(w) = list.words.get_mut(node) {
                    let mut c = std::mem::take(w);
                    self.filesub(&mut c, flags & (PREFORK_TYPESET | PREFORK_ASSIGN));
                    if let Some(w) = list.words.get_mut(node) {
                        *w = c;
                    }
                }
            } else {
                match self.stringsubst(
                    list,
                    node,
                    flags & !(PREFORK_TYPESET | PREFORK_ASSIGN),
                    ret_flags,
                    asssub,
                ) {
                    Some(n) => node = n,
                    None => return,
                }
            }
            node += 1;
        }
        if self.isset(SHFILEEXPANSION) {
            let mut node = 0usize;
            while node < list.words.len() {
                match self.stringsubst(
                    list,
                    node,
                    flags & !(PREFORK_TYPESET | PREFORK_ASSIGN),
                    ret_flags,
                    asssub,
                ) {
                    Some(n) => node = n,
                    None => return,
                }
                node += 1;
            }
        }
        let mut node = 0usize;
        let mut keep = false;
        let mut stop: Option<usize> = None;
        while node < list.words.len() {
            if Some(node) == stop {
                keep = false;
            }
            let nonempty = list.words.get(node).is_some_and(|w| !w.is_empty());
            if nonempty {
                if let Some(w) = list.words.get_mut(node) {
                    crate::utils::remnulargs(w);
                }
                if !self.isset(IGNOREBRACES) && flags & PREFORK_SINGLE == 0 {
                    if !keep {
                        stop = Some(node + 1);
                    }
                    while let Some(mut w) = list.words.get(node).cloned() {
                        let has = self.hasbraces(&mut w);
                        if let Some(slot) = list.words.get_mut(node) {
                            *slot = w;
                        }
                        if !has {
                            break;
                        }
                        keep = true;
                        let before = list.words.len();
                        node = self.xpandbraces(&mut list.words, node);
                        let grown = list.words.len() + 1 - before;
                        if let Some(st) = stop.as_mut()
                            && *st > node
                        {
                            *st += grown - 1;
                        }
                    }
                }
                if !self.isset(SHFILEEXPANSION)
                    && let Some(w) = list.words.get_mut(node)
                {
                    let mut c = std::mem::take(w);
                    self.filesub(&mut c, flags & (PREFORK_TYPESET | PREFORK_ASSIGN));
                    if let Some(w) = list.words.get_mut(node) {
                        *w = c;
                    }
                }
            } else if flags & PREFORK_SINGLE == 0 && *ret_flags & PREFORK_KEY_VALUE == 0 && !keep {
                let _ = list.words.remove(node);
                if let Some(st) = stop.as_mut()
                    && *st > node
                {
                    *st -= 1;
                }
                if self.errflag() {
                    return;
                }
                continue;
            }
            if self.errflag() {
                return;
            }
            node += 1;
        }
    }

    /// zsh's `stringsubstquote`: expand the `$'...'` at `*pos` in `s`.
    fn stringsubstquote(&self, s: &[u8], pos: &mut usize) -> Vec<u8> {
        let strdpos = *pos;
        let (strsub, misc) =
            self.getkeystring(from(s, strdpos + 2), crate::utils::GETKEYS_DOLLARS_QUOTE);
        let len = misc.used + 2;
        let rest = from(s, strdpos + len);
        let strret = if strdpos != 0 {
            let mut r = sub(s, 0, strdpos).to_vec();
            r.extend_from_slice(&strsub);
            r.extend_from_slice(rest);
            r
        } else if !rest.is_empty() {
            let mut r = strsub.clone();
            r.extend_from_slice(rest);
            r
        } else if !strsub.is_empty() {
            strsub.clone()
        } else {
            nulstring()
        };
        *pos = strdpos + strsub.len();
        strret
    }

    /// zsh's `stringsubst`.
    #[expect(clippy::too_many_lines, reason = "zsh's stringsubst")]
    fn stringsubst(
        &mut self,
        list: &mut WordList,
        node: usize,
        pf_flags: i32,
        ret_flags: &mut i32,
        asssub: bool,
    ) -> Option<usize> {
        let mut pf_flags = pf_flags;
        let mut node = node;
        let mut str3 = list.words.get(node)?.clone();
        let mut i = 0usize;
        while !self.errflag() && i < str3.len() {
            let c = at(&str3, i);
            if (c == INANG || c == OUTANGPROC || (i == 0 && c == EQUALS))
                && at(&str3, i + 1) == INPAR
            {
                let (subst, rest) = if c == INANG || c == OUTANGPROC {
                    self.getproc(&str3, i)
                } else {
                    self.getoutputfile(&str3, i)
                };
                if self.errflag() {
                    return None;
                }
                let (subst, rest) = match subst {
                    Some(s) => (s, rest),
                    None => (Vec::new(), str3.len()),
                };
                let mut snew = sub(&str3, 0, i).to_vec();
                snew.extend_from_slice(&subst);
                let newi = snew.len();
                snew.extend_from_slice(from(&str3, rest));
                str3 = snew;
                i = newi;
                if let Some(w) = list.words.get_mut(node) {
                    *w = str3.clone();
                }
            } else {
                i += 1;
            }
        }
        let mut i = 0usize;
        while !self.errflag() && i < str3.len() {
            let c = at(&str3, i);
            let qt = c == QSTRING;
            if qt || c == STRING {
                let c1 = at(&str3, i + 1);
                if c1 == INPAR || c1 == INPARMATH {
                    if !qt {
                        list.flags |= LF_ARRAY;
                    }
                    // comsub, with str pointing at the parenthesis.
                    {
                        let ni =
                            self.comsub(list, &mut node, &mut str3, i + 1, c1, qt, pf_flags)?;
                        i = ni;
                        continue;
                    }
                } else if c1 == INBRACK {
                    let mut j = i + 1;
                    if crate::glob::skipparens(INBRACK, OUTBRACK, &str3, &mut j) != 0 {
                        self.zerr("closing bracket missing");
                        return None;
                    }
                    let expr = sub(&str3, i + 2, j - 1).to_vec();
                    let rest = from(&str3, j).to_vec();
                    let prefix = sub(&str3, 0, i).to_vec();
                    let (news, ni) = self.arithsubst(&expr, &prefix, &rest);
                    str3 = news;
                    i = ni;
                    if let Some(w) = list.words.get_mut(node) {
                        *w = str3.clone();
                    }
                    continue;
                } else if c1 == SNULL {
                    let mut p = i;
                    str3 = self.stringsubstquote(&str3, &mut p);
                    i = p;
                    if let Some(w) = list.words.get_mut(node) {
                        *w = str3.clone();
                    }
                    continue;
                } else {
                    if (self.isset(SHWORDSPLIT) && pf_flags & PREFORK_NOSHWORDSPLIT == 0)
                        || pf_flags & PREFORK_SPLIT != 0
                    {
                        pf_flags |= PREFORK_SHWORDSPLIT;
                    }
                    let mut pos = i;
                    let n = self.paramsubst(
                        list,
                        node,
                        &mut pos,
                        qt,
                        pf_flags & (PREFORK_SINGLE | PREFORK_SHWORDSPLIT | PREFORK_SUBEXP),
                        ret_flags,
                    );
                    match n {
                        Some(n) if !self.errflag() => {
                            node = n;
                            str3 = list.words.get(node)?.clone();
                            i = pos;
                            continue;
                        }
                        _ => return None,
                    }
                }
            } else if c == QTICK || c == TICK {
                let qt = c == QTICK;
                if c == TICK {
                    list.flags |= LF_ARRAY;
                }
                {
                    let ni = self.comsub(list, &mut node, &mut str3, i, c, qt, pf_flags)?;
                    i = ni;
                    continue;
                }
            } else if asssub && (c == b'=' || c == EQUALS) && i != 0 {
                pf_flags |= PREFORK_SINGLE;
            }
            i += 1;
        }
        if self.errflag() { None } else { Some(node) }
    }

    /// The `comsub:` part of `stringsubst`: `start` is at the `(`, `((` or
    /// backquote. Returns where scanning goes on.
    #[expect(
        clippy::too_many_arguments,
        reason = "the shared locals of zsh's stringsubst"
    )]
    fn comsub(
        &mut self,
        list: &mut WordList,
        node: &mut usize,
        str3: &mut Vec<u8>,
        start: usize,
        c: u8,
        qt: bool,
        pf_flags: i32,
    ) -> Option<usize> {
        let mut qt = qt;
        let endchar;
        let body_start;
        let body_end;
        let after;
        let str2: usize;
        if c == INPAR {
            endchar = OUTPAR;
            let mut j = start;
            let _ = crate::glob::skipparens(INPAR, OUTPAR, str3, &mut j);
            body_start = start + 1;
            body_end = j - 1;
            after = j;
            str2 = start - 1;
        } else if c == INPARMATH {
            let mut mathpar = 1;
            let mut j = start;
            while mathpar > 0 && j < str3.len() {
                j += 1;
                let cj = at(str3, j);
                if cj == OUTPARMATH {
                    mathpar -= 1;
                } else if cj == INPARMATH {
                    mathpar += 1;
                }
            }
            if at(str3, j) != OUTPARMATH {
                self.zerr("failed to find end of math substitution");
                return None;
            }
            let prefix = sub(str3, 0, start - 1).to_vec();
            let expr = sub(str3, start + 1, j.saturating_sub(1)).to_vec();
            let rest = from(str3, j + 1).to_vec();
            if self.isset(EXECOPT) {
                let (news, ni) = self.arithsubst(&expr, &prefix, &rest);
                *str3 = news;
                if let Some(w) = list.words.get_mut(*node) {
                    *w = str3.clone();
                }
                return Some(ni);
            }
            let mut news = prefix.clone();
            news.push(at(str3, start - 1));
            *str3 = news;
            if let Some(w) = list.words.get_mut(*node) {
                *w = str3.clone();
            }
            return Some(start);
        } else {
            endchar = c;
            let mut j = start + 1;
            while at(str3, j) != endchar {
                if j >= str3.len() {
                    self.zerr("failed to find end of command substitution");
                    return None;
                }
                j += 1;
            }
            body_start = start + 1;
            body_end = j;
            after = j + 1;
            str2 = start;
        }
        let mut cmd = sub(str3, body_start, body_end).to_vec();
        let mut k = 0;
        while k < cmd.len() {
            let ck = at(&cmd, k);
            if tok::is_tok(ck)
                && ck != NULARG
                && !(endchar != OUTPAR
                    && ck == BNULL
                    && (at(&cmd, k + 1) == b'$'
                        || at(&cmd, k + 1) == b'\\'
                        || at(&cmd, k + 1) == b'`'
                        || (qt && at(&cmd, k + 1) == b'"')))
                && let Some(slot) = cmd.get_mut(k)
            {
                *slot = tok::detok(ck);
            }
            k += 1;
        }
        let rest = from(str3, after).to_vec();
        let prefix = sub(str3, 0, str2).to_vec();
        qt = qt || pf_flags & PREFORK_SINGLE != 0;
        let Some(mut pl) = self.getoutput(&cmd, qt) else {
            self.zerr("parse error in command substitution");
            return None;
        };
        if pl.is_empty() {
            let mut news = prefix.clone();
            let ni = news.len();
            news.extend_from_slice(&rest);
            *str3 = news;
            if let Some(w) = list.words.get_mut(*node) {
                *w = str3.clone();
            }
            return Some(ni);
        }
        let mut s = pl.remove(0);
        if !qt && pf_flags & PREFORK_SINGLE != 0 && self.isset(GLOBSUBST) {
            crate::pattern::shtokenize(&mut s, self.isset(SHGLOB));
        }
        let mut l1prefix = prefix.clone();
        if !pl.is_empty() {
            let mut first = prefix.clone();
            first.extend_from_slice(&s);
            if let Some(w) = list.words.get_mut(*node) {
                *w = first;
            }
            let n = pl.len();
            let last = pl.pop().unwrap_or_default();
            for (off, w) in pl.into_iter().enumerate() {
                list.words.insert(*node + 1 + off, w);
            }
            list.words.insert(*node + n, last.clone());
            *node += n;
            s = last;
            l1prefix.clear();
        }
        let mut news = l1prefix;
        news.extend_from_slice(&s);
        let ni = news.len();
        news.extend_from_slice(&rest);
        *str3 = news;
        if let Some(w) = list.words.get_mut(*node) {
            *w = str3.clone();
        }
        Some(ni)
    }

    /// zsh's `quotesubst`.
    pub(crate) fn quotesubst(&self, s: &[u8]) -> Vec<u8> {
        let mut st = s.to_vec();
        let mut i = 0;
        while i < st.len() {
            if at(&st, i) == STRING && at(&st, i + 1) == SNULL {
                st = self.stringsubstquote(&st, &mut i);
            } else {
                i += 1;
            }
        }
        crate::utils::remnulargs(&mut st);
        st
    }

    /// zsh's `globlist`.
    pub(crate) fn globlist(&mut self, list: &mut WordList, flags: i32) {
        self.badcshglob = 0;
        let mut node = 0usize;
        while !self.errflag() && node < list.words.len() {
            if flags & PREFORK_KEY_VALUE != 0
                && list.words.get(node).is_some_and(|w| at(w, 0) == MARKER)
            {
                node += 3;
            } else {
                node = self.zglob(&mut list.words, node, flags & PREFORK_NO_UNTOK != 0);
            }
        }
        if self.noerrs != 0 {
            self.badcshglob = 0;
        } else if self.badcshglob == 1 {
            self.zerr("no match");
        }
    }

    /// zsh's `singsub`: expand to one word.
    pub(crate) fn singsub(&mut self, s: &[u8]) -> Vec<u8> {
        let mut list = WordList::one(s.to_vec());
        let mut rf = 0;
        self.prefork(&mut list, PREFORK_SINGLE, &mut rf);
        if self.errflag() {
            return s.to_vec();
        }
        list.words.into_iter().next().unwrap_or_default()
    }

    /// zsh's `multsub`: `(scalar, array, isarr, empty)`.
    fn multsub(
        &mut self,
        s: &[u8],
        pf_flags: i32,
        want_array: bool,
        sep: Option<&[u8]>,
        ms_flags: &mut i32,
    ) -> (Vec<u8>, Option<Vec<Vec<u8>>>, i32, bool) {
        let mut x = s.to_vec();
        let mut start = 0usize;
        if pf_flags & PREFORK_SPLIT != 0 {
            while start < x.len() {
                let c0 = at(&x, start);
                let (c, l) = if c0 == META {
                    (at(&x, start + 1) ^ 32, 2)
                } else {
                    (c0, 1)
                };
                if !self.iwsep(c) {
                    break;
                }
                *ms_flags |= MULTSUB_WS_AT_START;
                start += l;
            }
        }
        let mut words: Vec<Vec<u8>> = Vec::new();
        if pf_flags & PREFORK_SPLIT != 0 {
            let (mut inq, mut inp) = (false, 0i32);
            let mut cur_start = start;
            let mut k = start;
            while k < x.len() {
                if at(&x, k) == DASH
                    && let Some(slot) = x.get_mut(k)
                {
                    *slot = b'-';
                }
                let ck = at(&x, k);
                let mut rawc: i32 = -1;
                let mut l;
                if tok::is_tok(ck) {
                    rawc = i32::from(ck);
                    l = 1;
                } else {
                    let (len, wc) = crate::utils::mb_metacharlenconv(self, from(&x, k));
                    l = len.max(1);
                    let wc = wc.unwrap_or_else(|| u32::from(ck));
                    if !inq && inp == 0 && self.wcsitype(wc, crate::utils::ISEP) {
                        words.push(sub(&x, cur_start, k).to_vec());
                        k += l;
                        let mut ended = true;
                        while k < x.len() {
                            let ck2 = at(&x, k);
                            if tok::is_tok(ck2) {
                                rawc = i32::from(ck2);
                                l = 1;
                                ended = false;
                                break;
                            }
                            let (len2, wc2) = crate::utils::mb_metacharlenconv(self, from(&x, k));
                            l = len2.max(1);
                            if !self
                                .wcsitype(wc2.unwrap_or_else(|| u32::from(ck2)), crate::utils::ISEP)
                            {
                                ended = false;
                                break;
                            }
                            k += l;
                        }
                        if ended || k >= x.len() {
                            *ms_flags |= MULTSUB_WS_AT_END;
                            cur_start = x.len() + 1;
                            break;
                        }
                        cur_start = k;
                        if rawc < 0 {
                            continue;
                        }
                    }
                }
                match u8::try_from(rawc).unwrap_or(0) {
                    DNULL | SNULL | TICK if rawc >= 0 => inq = !inq,
                    INPAR if rawc >= 0 => inp += 1,
                    OUTPAR if rawc >= 0 => inp -= 1,
                    BNULL | BNULLKEEP if rawc >= 0 => {
                        k += l;
                        l = crate::utils::mb_metacharlen(self, from(&x, k)).max(1);
                    }
                    _ => {}
                }
                k += l;
            }
            if cur_start <= x.len() {
                words.push(from(&x, cur_start).to_vec());
            }
        } else {
            words.push(from(&x, start).to_vec());
        }
        let mut sublist = WordList { words, flags: 0 };
        self.prefork(&mut sublist, pf_flags, ms_flags);
        if self.errflag() {
            return (Vec::new(), None, 0, false);
        }
        let l = sublist.words.len();
        if l > 1 || (sublist.flags & LF_ARRAY != 0 && want_array) {
            if want_array && (l > 1 || sublist.flags & LF_ARRAY != 0) {
                return (Vec::new(), Some(sublist.words), SCANPM_MATCHMANY, false);
            }
            let joined = self.sepjoin(&sublist.words, sep);
            return (joined, None, 0, false);
        }
        if l == 1 {
            (
                sublist.words.into_iter().next().unwrap_or_default(),
                None,
                0,
                false,
            )
        } else {
            (Vec::new(), None, 0, true)
        }
    }

    /// zsh's `filesub`.
    pub(crate) fn filesub(&mut self, namptr: &mut Vec<u8>, assign: i32) {
        let _ = self.filesubstr(namptr, assign);
        if assign == 0 {
            return;
        }
        let mut eql: Option<usize> = None;
        if assign & PREFORK_TYPESET != 0 {
            if namptr.len() > 1
                && let Some(p) = from(namptr, 1).iter().position(|&c| c == EQUALS)
            {
                let subi = p + 1;
                eql = Some(subi);
                let c1 = at(namptr, subi + 1);
                if c1 == TILDE || c1 == EQUALS {
                    let mut st = from(namptr, subi + 1).to_vec();
                    if self.filesubstr(&mut st, assign) {
                        namptr.truncate(subi + 1);
                        namptr.extend(st);
                    }
                }
            } else {
                return;
            }
        }
        let mut ptr = 0usize;
        while let Some(p) = from(namptr, ptr).iter().position(|&c| c == b':') {
            let subi = ptr + p;
            let len = subi;
            let c1 = at(namptr, subi + 1);
            if eql.is_none_or(|e| subi > e) && (c1 == TILDE || c1 == EQUALS) {
                let mut st = from(namptr, subi + 1).to_vec();
                if self.filesubstr(&mut st, assign) {
                    namptr.truncate(subi + 1);
                    namptr.extend(st);
                }
            }
            ptr = len + 1;
        }
    }

    /// zsh's `equalsubstr`.
    pub(crate) fn equalsubstr(&mut self, s: &[u8], assign: i32, nomatch: bool) -> Option<Vec<u8>> {
        let isend2 = |c: u8| c == 0 || c == INPAR || (assign != 0 && c == b':');
        let mut pp = 0;
        while pp < s.len() && !isend2(at(s, pp)) {
            pp += 1;
        }
        let mut cmdstr = sub(s, 0, pp).to_vec();
        tok::untokenize(&mut cmdstr);
        crate::utils::remnulargs(&mut cmdstr);
        if cmdstr == [NULARG] {
            cmdstr.clear();
        }
        let Some(cnam) = self.findcmd(&cmdstr, true, false) else {
            if nomatch {
                self.zerr(&format!("{} not found", lossy(&cmdstr)));
            }
            return None;
        };
        let mut r = cnam;
        r.extend_from_slice(from(s, pp));
        Some(r)
    }

    /// zsh's `filesubstr`: `~` and `=` expansion at the start of the word.
    pub(crate) fn filesubstr(&mut self, namptr: &mut Vec<u8>, assign: i32) -> bool {
        let s = namptr.clone();
        let isend = |c: u8| c == 0 || c == b'/' || c == INPAR || (assign != 0 && c == b':');
        if at(&s, 0) == TILDE && at(&s, 1) != b'=' && at(&s, 1) != EQUALS {
            let mut s = s;
            if at(&s, 1) == DASH
                && let Some(slot) = s.get_mut(1)
            {
                *slot = b'-';
            }
            let (val, used) = crate::utils::zstrtol(from(&s, 1), 10);
            let ptr = 1 + used;
            let c1 = at(&s, 1);
            let end1 = s.len() <= 1;
            if end1 || isend(c1) {
                let mut r = self.home.clone();
                r.extend_from_slice(from(&s, 1));
                *namptr = r;
                return true;
            } else if c1 == b'+' && (s.len() <= 2 || isend(at(&s, 2))) {
                let mut r = self.pwd.clone();
                r.extend_from_slice(from(&s, 2));
                *namptr = r;
                return true;
            } else if c1 == b'-' && (s.len() <= 2 || isend(at(&s, 2))) {
                let mut r = self.oldpwd().unwrap_or_else(|| self.pwd.clone());
                r.extend_from_slice(from(&s, 2));
                *namptr = r;
                return true;
            } else if c1 == INBRACK
                && let Some(p2) = from(&s, 2).iter().position(|&c| c == OUTBRACK)
            {
                let p2 = p2 + 2;
                let mut tmp = sub(&s, 2, p2).to_vec();
                tok::untokenize(&mut tmp);
                crate::utils::remnulargs(&mut tmp);
                let res = self.subst_string_by_hook(b"zsh_directory_name", Some(b"n"), &tmp);
                if let Some(first) = res.and_then(|a| a.into_iter().next()) {
                    let mut r = first;
                    r.extend_from_slice(from(&s, p2 + 1));
                    *namptr = r;
                    return true;
                }
                if self.isset(NOMATCH) && self.isset(EXECOPT) {
                    self.zerr(&format!("no directory expansion: ~[{}]", lossy(&tmp)));
                }
                return false;
            } else if !self.inblank(c1)
                && (ptr >= s.len() || isend(at(&s, ptr)))
                && (!c1.is_ascii_digit() || ptr < 4)
                && used > 0
                || (!self.inblank(c1)
                    && (c1 == b'+' || c1 == b'-')
                    && isend(at(&s, ptr))
                    && used > 0)
            {
                let v = val.abs();
                let Some(ds) = self.dstackent(c1, v) else {
                    return false;
                };
                let mut r = ds;
                r.extend_from_slice(from(&s, ptr));
                *namptr = r;
                return true;
            } else {
                let pe = self.itype_end(&s, 1, crate::utils::IUSER, false);
                if pe != 1 {
                    if pe < s.len() && !isend(at(&s, pe)) {
                        return false;
                    }
                    let mut untok = sub(&s, 1, pe).to_vec();
                    tok::untokenize(&mut untok);
                    let Some(hom) = self.getnameddir(&untok) else {
                        if self.isset(NOMATCH) && self.isset(EXECOPT) {
                            self.zerr(&format!(
                                "no such user or named directory: {}",
                                lossy(&untok)
                            ));
                        }
                        return false;
                    };
                    let mut r = hom;
                    r.extend_from_slice(from(&s, pe));
                    *namptr = r;
                    return true;
                }
            }
        } else if at(&s, 0) == EQUALS
            && self.isset(crate::options::EQUALS)
            && s.len() > 1
            && at(&s, 1) != INPAR
            && let Some(expn) = self.equalsubstr(from(&s, 1), assign, self.isset(NOMATCH))
        {
            *namptr = expn;
            return true;
        }
        false
    }

    /// zsh's `strcatsub`: `pb` + `src` + `s`, tokenized for GLOB_SUBST.
    fn strcatsub(&self, pb: &[u8], src: &[u8], s: Option<&[u8]>, glbsub: bool) -> (Vec<u8>, usize) {
        let mut dest = pb.to_vec();
        let mut mid = src.to_vec();
        if glbsub {
            crate::pattern::shtokenize(&mut mid, self.isset(SHGLOB));
        }
        dest.extend(mid);
        let pos = dest.len();
        if let Some(s) = s {
            dest.extend_from_slice(s);
        }
        (dest, pos)
    }

    /// zsh's `get_intarg`: `(value, delimiter length)`, -1 on error.
    fn get_intarg(&mut self, s: &[u8], i: &mut usize) -> (i64, usize) {
        let (t, arglen) = self.get_strarg(s, *i);
        if t >= s.len() {
            return (-1, 0);
        }
        let p = sub(s, *i + arglen, t).to_vec();
        *i = t + arglen;
        let Ok(p) = self.parsestr(&p) else {
            return (-1, 0);
        };
        let p = self.singsub(&p);
        if self.errflag() {
            return (-1, 0);
        }
        let ret = self.mathevali(&p);
        if self.errflag() {
            return (-1, 0);
        }
        (ret.abs(), arglen)
    }

    /// zsh's `subst_parse_str` for `(e)`.
    fn subst_parse_str(&mut self, sp: &mut Vec<u8>, single: bool, err: bool) -> bool {
        let r = if err {
            self.parsestr(sp).ok()
        } else {
            let mut t = sp.clone();
            tok::untokenize(&mut t);
            crate::dquote::parse_dquote_string(&t, self.lex_opts()).ok()
        };
        let Some(mut s) = r else {
            return true;
        };
        if !single {
            let mut qt = false;
            for c in &mut s {
                if !qt {
                    if *c == QSTRING {
                        *c = STRING;
                    } else if *c == QTICK {
                        *c = TICK;
                    }
                }
                if *c == DNULL {
                    qt = !qt;
                }
            }
        }
        *sp = s;
        false
    }

    /// zsh's `substevalchar` for `(#)`.
    fn substevalchar(&mut self, p: &[u8]) -> Option<Vec<u8>> {
        let ires = self.mathevali(p);
        if self.errflag() {
            return None;
        }
        if self.isset(MULTIBYTE)
            && ires > 127
            && let Some(ch) = u32::try_from(ires & 0xFFFF_FFFF)
                .ok()
                .and_then(char::from_u32)
        {
            let mut buf = [0u8; 4];
            return Some(tok::metafy(ch.encode_utf8(&mut buf).as_bytes()));
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "C's %c"
        )]
        let b = ires as u8;
        Some(tok::metafy(&[b]))
    }

    /// zsh's `untok_and_escape`.
    fn untok_and_escape(&mut self, s: &[u8], escapes: bool, tok_arg: bool) -> Vec<u8> {
        let mut dst: Option<Vec<u8>> = None;
        if escapes && (at(s, 0) == STRING || at(s, 0) == QSTRING) && s.len() > 1 {
            let name = from(s, 1);
            if name.iter().all(|&c| self.iident(c)) {
                dst = Some(self.getsparam(name).unwrap_or_default());
            }
        }
        let mut dst = match dst {
            Some(d) => d,
            None => {
                let mut d = s.to_vec();
                tok::untokenize(&mut d);
                if escapes {
                    let (k, _) = self.getkeystring(&d, crate::utils::GETKEYS_SEP);
                    d = tok::metafy(&k);
                }
                d
            }
        };
        if tok_arg {
            crate::pattern::shtokenize(&mut dst, self.isset(SHGLOB));
        }
        dst
    }

    /// zsh's `check_colon_subscript`: `(expression, end index)`.
    fn check_colon_subscript(&mut self, s: &[u8], i: usize) -> Option<(Vec<u8>, usize)> {
        let c = at(s, i);
        if i >= s.len() || self.ialpha(c) || c == b'&' {
            return None;
        }
        if c == b':' {
            return Some((b"0".to_vec(), i));
        }
        let end = match self.parse_subscript(from(s, i), false, b':') {
            Some(e) => i + e,
            None => {
                let e = self.parse_subscript(from(s, i), false, 0)?;
                i + e
            }
        };
        let t = sub(s, i, end).to_vec();
        let Ok(t) = self.parsestr(&t) else {
            return None;
        };
        let mut t = self.singsub(&t);
        crate::utils::remnulargs(&mut t);
        tok::untokenize(&mut t);
        Some((t, end))
    }

    /// zsh's `dopadding`.
    #[expect(clippy::too_many_arguments, reason = "zsh's dopadding")]
    fn dopadding(
        &self,
        s: &[u8],
        prenum: i64,
        postnum: i64,
        preone: Option<&[u8]>,
        postone: Option<&[u8]>,
        premul: &[u8],
        postmul: &[u8],
        multi_width: i32,
    ) -> Vec<u8> {
        let def: Vec<u8> = match &self.ifs {
            Some(i) if i.is_empty() => Vec::new(),
            other => {
                let t = other
                    .clone()
                    .unwrap_or_else(|| crate::utils::DEFAULT_IFS.to_vec());
                let l = crate::utils::mb_metacharlen(self, &t);
                sub(&t, 0, l).to_vec()
            }
        };
        let preone: Option<Vec<u8>> = preone.map(|p| {
            if p.is_empty() {
                def.clone()
            } else {
                p.to_vec()
            }
        });
        let postone: Option<Vec<u8>> = postone.map(|p| {
            if p.is_empty() {
                def.clone()
            } else {
                p.to_vec()
            }
        });
        let premul = if premul.is_empty() {
            def.clone()
        } else {
            premul.to_vec()
        };
        let postmul = if postmul.is_empty() {
            def.clone()
        } else {
            postmul.to_vec()
        };
        let w = |x: &[u8]| -> i64 {
            i64::try_from(crate::utils::mb_metastrlen(self, x, multi_width)).unwrap_or(0)
        };
        let ls = w(s);
        let lpreone = preone.as_deref().map_or(0, w);
        let lpostone = postone.as_deref().map_or(0, w);
        let lpremul = w(&premul);
        let lpostmul = w(&postmul);
        if prenum + postnum == ls {
            return s.to_vec();
        }
        // Character iteration: (bytes, width) for each character.
        let chars = |x: &[u8]| -> Vec<(Vec<u8>, i64)> {
            let mut out = Vec::new();
            let mut i = 0;
            while i < x.len() {
                let (len, wc) = crate::utils::mb_metacharlenconv(self, from(x, i));
                let len = len.max(1);
                let cw = match (multi_width, wc) {
                    (0, _) => 1,
                    (1, Some(c)) => i64::from(crate::utils::wcwidth(c).max(0)),
                    (_, Some(c)) => i64::from(crate::utils::wcwidth(c) > 0),
                    (_, None) => 1,
                };
                out.push((sub(x, i, i + len).to_vec(), cw));
                i += len;
            }
            out
        };
        let mut r: Vec<u8> = Vec::new();
        let sch = chars(s);
        // Skip `width` from the front of a character list, returning the rest.
        let skip = |list: &[(Vec<u8>, i64)], width: i64| -> usize {
            let mut f = width;
            let mut k = 0;
            while f > 0 && k < list.len() {
                f -= list.get(k).map_or(1, |c| c.1);
                k += 1;
            }
            k
        };
        // Take `width` from the front, returning how many characters.
        let take = |list: &[(Vec<u8>, i64)], from_k: usize, width: i64| -> usize {
            let mut c = width;
            let mut k = from_k;
            while c > 0 && k < list.len() {
                c -= list.get(k).map_or(1, |x| x.1);
                k += 1;
            }
            k
        };
        let push_range = |r: &mut Vec<u8>, list: &[(Vec<u8>, i64)], a: usize, b: usize| {
            for item in list.get(a..b.min(list.len())).unwrap_or(&[]) {
                r.extend_from_slice(&item.0);
            }
        };
        let pre1 = preone.as_deref().map(chars);
        let post1 = postone.as_deref().map(chars);
        let prem = chars(&premul);
        let postm = chars(&postmul);
        let mut sk: usize;
        if prenum != 0 {
            if postnum != 0 {
                let ls2 = ls / 2;
                let f = prenum - ls2;
                if f <= 0 {
                    sk = skip(&sch, -f);
                    let e = take(&sch, sk, prenum);
                    push_range(&mut r, &sch, sk, e);
                    sk = e;
                } else {
                    if f <= lpreone {
                        if let Some(p1) = &pre1 {
                            let k = skip(p1, lpreone - f);
                            push_range(&mut r, p1, k, p1.len());
                        }
                    } else {
                        let mut f = f - lpreone;
                        if lpremul != 0 {
                            let m = f % lpremul;
                            if m != 0 {
                                let k = skip(&prem, lpremul - m);
                                push_range(&mut r, &prem, k, prem.len());
                            }
                            for _ in 0..f / lpremul {
                                let e = take(&prem, 0, lpremul);
                                push_range(&mut r, &prem, 0, e);
                            }
                            f = 0;
                        }
                        let _ = f;
                        if let Some(p1) = &pre1 {
                            push_range(&mut r, p1, 0, p1.len());
                        }
                    }
                    let e = take(&sch, 0, ls2);
                    push_range(&mut r, &sch, 0, e);
                    sk = e;
                }
                let ls2 = ls - ls2;
                let f = postnum - ls2;
                if f <= 0 {
                    let e = take(&sch, sk, postnum);
                    push_range(&mut r, &sch, sk, e);
                } else {
                    push_range(&mut r, &sch, sk, sch.len());
                    if f <= lpostone {
                        if let Some(p1) = &post1 {
                            let e = take(p1, 0, f);
                            push_range(&mut r, p1, 0, e);
                        }
                    } else {
                        let mut f = f;
                        if let Some(p1) = &post1 {
                            f -= lpostone;
                            push_range(&mut r, p1, 0, p1.len());
                        }
                        if lpostmul != 0 {
                            for _ in 0..f / lpostmul {
                                r.extend_from_slice(&postmul);
                            }
                            let m = f % lpostmul;
                            if m != 0 {
                                let e = take(&postm, 0, m);
                                push_range(&mut r, &postm, 0, e);
                            }
                        }
                    }
                }
            } else {
                let f = prenum - ls;
                if f <= 0 {
                    let k = skip(&sch, -f);
                    let e = take(&sch, k, prenum);
                    push_range(&mut r, &sch, k, e);
                } else {
                    if f <= lpreone {
                        if let Some(p1) = &pre1 {
                            let k = skip(p1, lpreone - f);
                            push_range(&mut r, p1, k, p1.len());
                        }
                    } else {
                        let f2 = f - lpreone;
                        if lpremul != 0 {
                            let m = f2 % lpremul;
                            if m != 0 {
                                let k = skip(&prem, lpremul - m);
                                let e = take(&prem, k, m);
                                push_range(&mut r, &prem, k, e);
                            }
                            for _ in 0..f2 / lpremul {
                                let e = take(&prem, 0, lpremul);
                                push_range(&mut r, &prem, 0, e);
                            }
                        }
                        if let Some(p1) = &pre1 {
                            push_range(&mut r, p1, 0, p1.len());
                        }
                    }
                    r.extend_from_slice(s);
                }
            }
        } else if postnum != 0 {
            let f = postnum - ls;
            if f <= 0 {
                let e = take(&sch, 0, postnum);
                push_range(&mut r, &sch, 0, e);
            } else {
                r.extend_from_slice(s);
                if f <= lpostone {
                    if let Some(p1) = &post1 {
                        let e = take(p1, 0, f);
                        push_range(&mut r, p1, 0, e);
                    }
                } else {
                    let mut f = f;
                    if let Some(p1) = &post1 {
                        f -= lpostone;
                        push_range(&mut r, p1, 0, p1.len());
                    }
                    if lpostmul != 0 {
                        for _ in 0..f / lpostmul {
                            let e = postm.len();
                            push_range(&mut r, &postm, 0, e);
                        }
                        let m = f % lpostmul;
                        if m != 0 {
                            let e = take(&postm, 0, m);
                            push_range(&mut r, &postm, 0, e);
                        }
                    }
                }
            }
        }
        r
    }

    /// zsh's `arithsubst`: `prefix` + value + `rest`, and where the value
    /// ends.
    fn arithsubst(&mut self, a: &[u8], prefix: &[u8], rest: &[u8]) -> (Vec<u8>, usize) {
        let a = self.singsub(a);
        let v = self.matheval(&a);
        let b = match v {
            MNumber::Float(d) if self.outputradix == 0 => {
                convfloat_underscore(d, self.outputunderscore)
            }
            other => {
                self.convbase_underscore(other.as_int(), self.outputradix, self.outputunderscore)
            }
        };
        let mut t = prefix.to_vec();
        t.extend_from_slice(&b);
        let pos = t.len();
        t.extend_from_slice(rest);
        (t, pos)
    }

    /// zsh's `dstackent`.
    fn dstackent(&mut self, ch: u8, val: i64) -> Option<Vec<u8>> {
        let backwards = ch == if self.isset(PUSHDMINUS) { b'+' } else { b'-' };
        let mut val = val;
        if !backwards {
            if val == 0 {
                return Some(self.pwd.clone());
            }
            val -= 1;
        }
        let stack = self.dirstack.clone();
        let n = i64::try_from(stack.len()).unwrap_or(0);
        if backwards {
            if val == n {
                return Some(self.pwd.clone());
            }
            if val < n {
                return stack
                    .get(usize::try_from(n - 1 - val).unwrap_or(0))
                    .cloned();
            }
        } else if val < n {
            return stack.get(usize::try_from(val).unwrap_or(0)).cloned();
        }
        if self.isset(NOMATCH) {
            self.zerr("not enough directory stack entries.");
        }
        None
    }

    /// zsh's `modify`: apply the colon modifiers at `*ptr` in `spec`.
    #[expect(clippy::too_many_lines, reason = "zsh's modify")]
    pub(crate) fn modify(&mut self, s: &mut Vec<u8>, spec: &[u8], ptr: &mut usize, inbrace: bool) {
        let mut spec = spec.to_vec();
        let mut test: Option<Vec<u8>> = None;
        while at(&spec, *ptr) == b':' {
            let mut count = 0i32;
            let lptr = *ptr;
            *ptr += 1;
            let (mut wall, mut gbal) = (false, false);
            let mut rec: i64 = 1;
            let mut c = 0u8;
            let mut sep: Option<Vec<u8>> = None;
            while c == 0 && *ptr < spec.len() {
                let ch = at(&spec, *ptr);
                match ch {
                    b'a' | b'A' | b'c' | b'r' | b'e' | b'l' | b'u' | b'q' | b'Q' | b'P' => c = ch,
                    b'h' | b't' => {
                        c = ch;
                        if inbrace && at(&spec, *ptr + 1).is_ascii_digit() {
                            while at(&spec, *ptr + 1).is_ascii_digit() {
                                count = 10 * count + i32::from(at(&spec, *ptr + 1) - b'0');
                                *ptr += 1;
                            }
                        }
                    }
                    b's' => {
                        c = ch;
                        *ptr += 1;
                        let p1 = *ptr;
                        let (clen, del) = crate::utils::mb_metacharlenconv(self, from(&spec, p1));
                        let d0 = at(&spec, p1);
                        let del = del.unwrap_or_else(|| {
                            u32::from(if d0 == META {
                                at(&spec, p1 + 1) ^ 32
                            } else {
                                d0
                            })
                        });
                        let p1b = p1 + clen.max(1);
                        let mut p2 = p1b;
                        let mut charlen = 0usize;
                        while p2 < spec.len() {
                            let cc = at(&spec, p2);
                            if (cc == BNULL || cc == b'\\') && p2 + 1 < spec.len() {
                                if cc == b'\\'
                                    && let Some(x) = spec.get_mut(p2)
                                {
                                    *x = BNULL;
                                }
                                charlen = 2;
                                p2 += charlen;
                                continue;
                            }
                            let (cl, d2) = crate::utils::mb_metacharlenconv(self, from(&spec, p2));
                            charlen = cl.max(1);
                            let d2 = d2.unwrap_or_else(|| {
                                u32::from(if cc == META {
                                    at(&spec, p2 + 1) ^ 32
                                } else {
                                    cc
                                })
                            });
                            if d2 == del {
                                break;
                            }
                            p2 += charlen;
                        }
                        if p2 >= spec.len() {
                            self.zerr("bad substitution");
                            return;
                        }
                        let p1end = p2;
                        p2 += charlen;
                        let mut p3 = p2;
                        charlen = 0;
                        while p3 < spec.len() {
                            let cc = at(&spec, p3);
                            if (cc == BNULL || cc == b'\\') && p3 + 1 < spec.len() {
                                if cc == b'\\'
                                    && let Some(x) = spec.get_mut(p3)
                                {
                                    *x = BNULL;
                                }
                                charlen = 2;
                                p3 += charlen;
                                continue;
                            }
                            let (cl, d3) = crate::utils::mb_metacharlenconv(self, from(&spec, p3));
                            charlen = cl.max(1);
                            let d3 = d3.unwrap_or_else(|| {
                                u32::from(if cc == META {
                                    at(&spec, p3 + 1) ^ 32
                                } else {
                                    cc
                                })
                            });
                            if d3 == del {
                                break;
                            }
                            p3 += charlen;
                        }
                        let left = sub(&spec, p1b, p1end).to_vec();
                        if !left.is_empty() {
                            self.hsubl = Some(left);
                        }
                        let Some(mut hl) = self.hsubl.clone() else {
                            self.zerr("no previous substitution");
                            return;
                        };
                        hl.retain(|&x| !tok::is_null(x) || x == BNULLKEEP);
                        if !self.isset(HISTSUBSTPATTERN) {
                            tok::untokenize(&mut hl);
                        }
                        self.hsubl = Some(hl);
                        let mut hr = Vec::new();
                        let right = sub(&spec, p2, p3).to_vec();
                        let mut k = 0;
                        while k < right.len() {
                            let x = at(&right, k);
                            if tok::is_null(x) && x != BNULLKEEP {
                                if x == BNULL
                                    && (at(&right, k + 1) == b'&' || at(&right, k + 1) == b'\\')
                                {
                                    hr.push(b'\\');
                                }
                            } else {
                                hr.push(x);
                            }
                            k += 1;
                        }
                        self.hsubr = Some(hr);
                        *ptr = p3.saturating_sub(1);
                        if p3 < spec.len() {
                            *ptr += charlen;
                        }
                    }
                    b'&' => c = b's',
                    b'g' => {
                        *ptr += 1;
                        gbal = true;
                    }
                    b'w' => {
                        wall = true;
                        *ptr += 1;
                    }
                    b'W' => {
                        wall = true;
                        *ptr += 1;
                        let (p1, cl) = self.get_strarg(&spec, *ptr);
                        sep = Some(sub(&spec, *ptr + cl, p1).to_vec());
                        *ptr = p1 + cl;
                        c = 0;
                    }
                    b'f' => {
                        rec = -1;
                        *ptr += 1;
                    }
                    b'F' => {
                        *ptr += 1;
                        let mut p = *ptr;
                        let (r, _) = self.get_intarg(&spec, &mut p);
                        rec = r;
                        *ptr = p;
                    }
                    _ => {
                        *ptr = lptr;
                        return;
                    }
                }
            }
            *ptr += 1;
            if c == 0 {
                *ptr = lptr;
                return;
            }
            if rec < 0 {
                test = Some(s.clone());
            }
            while rec != 0 {
                rec -= 1;
                if wall {
                    let mut all: Option<Vec<u8>> = None;
                    let src = s.clone();
                    let mut t = 0usize;
                    let mut e = 0usize;
                    while let Some(tt) = self.findword(&src, &mut e, sep.as_deref()) {
                        let word = sub(&src, tt, e).to_vec();
                        let mut copy = word.clone();
                        match c {
                            b'a' => {
                                let _ = self.chabspath(&mut copy);
                            }
                            b'A' => {
                                let _ = self.chrealpath(&mut copy, b'A');
                            }
                            b'c' => {
                                if let Some(c2) = self.equalsubstr(&copy, 0, false) {
                                    copy = c2;
                                }
                            }
                            b'h' => {
                                let _ = crate::hist::remtpath(&mut copy, count);
                            }
                            b'r' => {
                                let _ = crate::hist::remtext(&mut copy);
                            }
                            b'e' => {
                                let _ = crate::hist::rembutext(&mut copy);
                            }
                            b't' => {
                                let _ = crate::hist::remlpaths(&mut copy, count);
                            }
                            b'l' => copy = self.casemodify(&word, CASMOD_LOWER),
                            b'u' => copy = self.casemodify(&word, CASMOD_UPPER),
                            b's' => {
                                if let (Some(l), Some(r)) = (self.hsubl.clone(), self.hsubr.clone())
                                {
                                    let _ = self.hist_subst(&mut copy, &l, &r, gbal);
                                }
                            }
                            b'q' => copy = self.quotestring(&copy, Qt::BackslashShownull),
                            b'Q' => {
                                let one = self.noerrs;
                                let oef = self.errflag.get();
                                self.noerrs = 1;
                                let _ = self.parse_subst_string(&mut copy);
                                self.noerrs = one;
                                self.errflag.set(oef | (self.errflag.get() & ERRFLAG_INT));
                                crate::utils::remnulargs(&mut copy);
                                tok::untokenize(&mut copy);
                            }
                            b'P' => {
                                if at(&copy, 0) != b'/' {
                                    let mut here = tok::metafy(&self.zgetcwd());
                                    if here.last() != Some(&b'/') {
                                        here.push(b'/');
                                    }
                                    here.extend(copy);
                                    copy = here;
                                }
                                copy = self.xsymlink(&copy).unwrap_or(copy);
                            }
                            _ => {}
                        }
                        let mut piece = all.take().unwrap_or_default();
                        piece.extend_from_slice(sub(&src, t, tt));
                        piece.extend(copy);
                        all = Some(piece);
                        t = e;
                    }
                    *s = all.unwrap_or_default();
                } else {
                    match c {
                        b'a' => {
                            let _ = self.chabspath(s);
                        }
                        b'A' => {
                            let _ = self.chrealpath(s, b'A');
                        }
                        b'c' => {
                            if let Some(c2) = self.equalsubstr(s, 0, false) {
                                *s = c2;
                            }
                        }
                        b'h' => {
                            let _ = crate::hist::remtpath(s, count);
                        }
                        b'r' => {
                            let _ = crate::hist::remtext(s);
                        }
                        b'e' => {
                            let _ = crate::hist::rembutext(s);
                        }
                        b't' => {
                            let _ = crate::hist::remlpaths(s, count);
                        }
                        b'l' => *s = self.casemodify(s, CASMOD_LOWER),
                        b'u' => *s = self.casemodify(s, CASMOD_UPPER),
                        b's' => {
                            if let (Some(l), Some(r)) = (self.hsubl.clone(), self.hsubr.clone()) {
                                let _ = self.hist_subst(s, &l, &r, gbal);
                            }
                        }
                        b'q' => *s = self.quotestring(s, Qt::Backslash),
                        b'Q' => {
                            let one = self.noerrs;
                            let oef = self.errflag.get();
                            self.noerrs = 1;
                            let _ = self.parse_subst_string(s);
                            self.noerrs = one;
                            self.errflag.set(oef | (self.errflag.get() & ERRFLAG_INT));
                            crate::utils::remnulargs(s);
                            tok::untokenize(s);
                        }
                        b'P' => {
                            if at(s, 0) != b'/' {
                                let mut here = tok::metafy(&self.zgetcwd());
                                if here.last() != Some(&b'/') {
                                    here.push(b'/');
                                }
                                here.extend_from_slice(s);
                                *s = here;
                            }
                            *s = self.xsymlink(s).unwrap_or_else(|| s.clone());
                        }
                        _ => {}
                    }
                }
                if rec < 0 {
                    if test.as_deref() == Some(s.as_slice()) {
                        rec = 0;
                    } else {
                        test = Some(s.clone());
                    }
                }
            }
        }
    }

    /// zsh's `parse_subst_string`: tokenize `s` as a word, then expand
    /// `$'...'`. True on a lexing error.
    pub(crate) fn parse_subst_string(&mut self, s: &mut Vec<u8>) -> bool {
        if s.is_empty() || s.as_slice() == [NULARG] {
            return false;
        }
        let mut t = s.clone();
        tok::untokenize(&mut t);
        let mut lx = crate::lex::Lexer::new(t.clone(), self.lex_opts());
        let Some(tokd) = lx.gettokstr_sub() else {
            *s = t;
            return true;
        };
        *s = tokd;
        let mut i = 0;
        while i < s.len() {
            if at(s, i) == STRING && at(s, i + 1) == SNULL {
                let (tt, misc) =
                    self.getkeystring(from(s, i + 2), crate::utils::GETKEYS_DOLLARS_QUOTE);
                let len = misc.used + 2;
                let mut ns = sub(s, 0, i).to_vec();
                ns.extend_from_slice(&tt);
                let ni = ns.len();
                ns.extend_from_slice(from(s, i + len));
                *s = ns;
                i = ni;
            } else {
                i += 1;
            }
        }
        false
    }
}

/// The state of one `${...}` (zsh's many locals in `paramsubst`).
struct Ps {
    isarr: i32,
    plan9: bool,
    globsubst: i32,
    evalchar: bool,
    getlen: i32,
    whichlen: i32,
    chkset: bool,
    vunset: i32,
    wantt: bool,
    spbreak: i32,
    val: Vec<u8>,
    aval: Vec<Vec<u8>>,
    flags: i32,
    flnum: i64,
    sortit: i32,
    indord: bool,
    unique: bool,
    casmod: i32,
    quotemod: i32,
    quotetype: i32,
    quoteerr: bool,
    mods: i32,
    shsplit: i32,
    ssub: bool,
    sep: Option<Vec<u8>>,
    spsep: Option<Vec<u8>>,
    premul: Option<Vec<u8>>,
    postmul: Option<Vec<u8>>,
    preone: Option<Vec<u8>>,
    postone: Option<Vec<u8>>,
    replstr: Option<Vec<u8>>,
    prenum: i64,
    postnum: i64,
    multi_width: i32,
    copied: bool,
    arrasg: i32,
    eval: bool,
    aspar: bool,
    presc: i32,
    getkeys: i32,
    nojoin: i32,
    inbrace: bool,
    hkeys: i32,
    hvals: i32,
    horrible_offset_hack: bool,
    ms_flags: i32,
    quoted_array_with_offset: bool,
}

impl Shell {
    /// zsh's `paramsubst`. `*pos` is the index of the `$` in `list[n]`; on
    /// return it is where scanning goes on.
    #[expect(clippy::too_many_lines, reason = "zsh's paramsubst is one procedure")]
    fn paramsubst(
        &mut self,
        l: &mut WordList,
        n: usize,
        pos: &mut usize,
        qt: bool,
        pf_flags: i32,
        ret_flags: &mut i32,
    ) -> Option<usize> {
        let mut n = n;
        let mut buf: Vec<u8> = l.words.get(n)?.clone();
        let aptr = *pos;
        let mut ostr_start = 0usize;
        let mut ps = Ps {
            isarr: 0,
            plan9: self.isset(RCEXPANDPARAM),
            globsubst: i32::from(self.isset(GLOBSUBST)),
            evalchar: false,
            getlen: 0,
            whichlen: 0,
            chkset: false,
            vunset: 0,
            wantt: false,
            spbreak: i32::from(
                pf_flags & PREFORK_SHWORDSPLIT != 0 && pf_flags & PREFORK_SINGLE == 0 && !qt,
            ),
            val: Vec::new(),
            aval: Vec::new(),
            flags: 0,
            flnum: 0,
            sortit: SORTIT_ANYOLDHOW,
            indord: false,
            unique: false,
            casmod: CASMOD_NONE,
            quotemod: 0,
            quotetype: QT_NONE,
            quoteerr: false,
            mods: 0,
            shsplit: 0,
            ssub: pf_flags & PREFORK_SINGLE != 0,
            sep: None,
            spsep: None,
            premul: None,
            postmul: None,
            preone: None,
            postone: None,
            replstr: None,
            prenum: 0,
            postnum: 0,
            multi_width: 0,
            copied: false,
            arrasg: 0,
            eval: false,
            aspar: false,
            presc: 0,
            getkeys: -1,
            nojoin: if pf_flags & PREFORK_SHWORDSPLIT != 0 {
                i32::from(!self.ifs.as_ref().is_none_or(|i| !i.is_empty()) && !qt)
            } else {
                0
            },
            inbrace: false,
            hkeys: 0,
            hvals: 0,
            horrible_offset_hack: false,
            ms_flags: 0,
            quoted_array_with_offset: false,
        };
        let has_ifs = self.ifs.as_ref().is_some_and(|i| !i.is_empty());
        if pf_flags & PREFORK_SHWORDSPLIT != 0 {
            ps.nojoin = i32::from(!has_ifs && !qt);
        }
        let mut s = aptr + 1;
        let c = at(&buf, s);
        let iident_start = self.itype_end(&buf, s, crate::utils::IIDENT, true) != s;
        if !iident_start
            && c != b'#'
            && c != POUND
            && !is_dash(c)
            && !matches!(
                c,
                b'!' | b'$'
                    | STRING
                    | QSTRING
                    | b'?'
                    | QUEST
                    | b'*'
                    | STAR
                    | b'@'
                    | b'{'
                    | INBRACE
                    | b'='
                    | EQUALS
                    | HAT
                    | b'^'
                    | b'~'
                    | TILDE
                    | b'+'
            )
        {
            if let Some(x) = buf.get_mut(aptr) {
                *x = b'$';
            }
            if let Some(w) = l.words.get_mut(n) {
                *w = buf;
            }
            *pos = s;
            return Some(n);
        }
        if c == INBRACE {
            ps.inbrace = true;
            s += 1;
            let c = at(&buf, s);
            if c == b'!' && at(&buf, s + 1) != OUTBRACE && self.emulation_is(EMULATE_KSH) {
                ps.hkeys = SCANPM_WANTKEYS;
                s += 1;
            } else if c == b'(' || c == INPAR {
                let mut escapes = false;
                let mut tok_arg = false;
                s += 1;
                loop {
                    let c = at(&buf, s);
                    if c == b')' || c == OUTPAR || s >= buf.len() {
                        break;
                    }
                    let tt;
                    let mut flagerr = false;
                    match c {
                        b'~' | TILDE => tok_arg = !tok_arg,
                        b'A' => ps.arrasg += 1,
                        b'@' => ps.nojoin = 2,
                        b'*' | STAR => ps.flags |= crate::glob::SUB_EGLOB,
                        b'M' => ps.flags |= crate::glob::SUB_MATCH,
                        b'R' => ps.flags |= crate::glob::SUB_REST,
                        b'B' => ps.flags |= crate::glob::SUB_BIND,
                        b'E' => ps.flags |= crate::glob::SUB_EIND,
                        b'N' => ps.flags |= crate::glob::SUB_LEN,
                        b'S' => ps.flags |= crate::glob::SUB_SUBSTR,
                        b'I' => {
                            s += 1;
                            let (v, _) = self.get_intarg(&buf, &mut s);
                            ps.flnum = v;
                            if v < 0 {
                                flagerr = true;
                            } else {
                                s -= 1;
                            }
                        }
                        b'L' => ps.casmod = CASMOD_LOWER,
                        b'U' => ps.casmod = CASMOD_UPPER,
                        b'C' => ps.casmod = CASMOD_CAPS,
                        b'o' => {
                            if ps.sortit == 0 {
                                ps.sortit |= SORTIT_SOMEHOW;
                            }
                        }
                        b'O' => ps.sortit |= SORTIT_BACKWARDS,
                        b'i' => ps.sortit |= SORTIT_IGNORING_CASE,
                        b'n' => ps.sortit |= SORTIT_NUMERICALLY,
                        b'-' | DASH => ps.sortit |= SORTIT_NUMERICALLY_SIGNED,
                        b'a' => {
                            ps.sortit |= SORTIT_SOMEHOW;
                            ps.indord = true;
                        }
                        b'D' => ps.mods |= 1,
                        b'V' => ps.mods |= 2,
                        b'q' => {
                            if ps.quotetype == QT_DOLLARS || ps.quotetype == QT_BACKSLASH_PATTERN {
                                flagerr = true;
                            } else if is_dash(at(&buf, s + 1)) || at(&buf, s + 1) == b'+' {
                                if ps.quotemod != 0 {
                                    flagerr = true;
                                } else {
                                    s += 1;
                                    ps.quotemod = 1;
                                    ps.quotetype = if at(&buf, s) == b'+' {
                                        QT_QUOTEDZPUTS
                                    } else {
                                        QT_SINGLE_OPTIONAL
                                    };
                                }
                            } else if ps.quotetype == QT_SINGLE_OPTIONAL {
                                flagerr = true;
                            } else {
                                ps.quotemod += 1;
                                ps.quotetype += 1;
                            }
                        }
                        b'b' => {
                            if ps.quotemod != 0 || ps.quotetype != QT_NONE {
                                flagerr = true;
                            } else {
                                ps.quotemod = 1;
                                ps.quotetype = QT_BACKSLASH_PATTERN;
                            }
                        }
                        b'Q' => ps.quotemod -= 1,
                        b'X' => ps.quoteerr = true,
                        b'e' => ps.eval = true,
                        b'P' => ps.aspar = true,
                        b'c' => ps.whichlen = 1,
                        b'w' => ps.whichlen = 2,
                        b'W' => ps.whichlen = 3,
                        b'f' => ps.spsep = Some(b"\n".to_vec()),
                        b'F' => ps.sep = Some(b"\n".to_vec()),
                        b'0' => ps.spsep = Some(vec![META, 32]),
                        b's' | b'j' => {
                            tt = c == b's';
                            let (t, arglen) = self.get_strarg(&buf, s + 1);
                            if t < buf.len() {
                                let arg = sub(&buf, s + 1 + arglen, t).to_vec();
                                let v = self.untok_and_escape(&arg, escapes, tok_arg);
                                if tt {
                                    ps.spsep = Some(v);
                                } else {
                                    ps.sep = Some(v);
                                }
                                s = t + arglen - 1;
                            } else {
                                flagerr = true;
                            }
                        }
                        b'l' | b'r' => {
                            tt = c == b'l';
                            s += 1;
                            let del0 = s;
                            let (num, dellen) = self.get_intarg(&buf, &mut s);
                            if num < 0 {
                                flagerr = true;
                            } else {
                                if tt {
                                    ps.prenum = num;
                                } else {
                                    ps.postnum = num;
                                }
                                let same = |b: &[u8], a: usize, bb: usize| {
                                    dellen > 0 && sub(b, a, a + dellen) == sub(b, bb, bb + dellen)
                                };
                                if !same(&buf, del0, s) {
                                    s -= 1;
                                } else {
                                    let (t, arglen) = self.get_strarg(&buf, s);
                                    if t >= buf.len() {
                                        flagerr = true;
                                    } else {
                                        let arg = sub(&buf, s + arglen, t).to_vec();
                                        let v = self.untok_and_escape(&arg, escapes, tok_arg);
                                        if tt {
                                            ps.premul = Some(v);
                                        } else {
                                            ps.postmul = Some(v);
                                        }
                                        s = t + arglen;
                                        if !same(&buf, del0, s) {
                                            s -= 1;
                                        } else {
                                            let (t, arglen) = self.get_strarg(&buf, s);
                                            if t >= buf.len() {
                                                flagerr = true;
                                            } else {
                                                let arg = sub(&buf, s + arglen, t).to_vec();
                                                let v =
                                                    self.untok_and_escape(&arg, escapes, tok_arg);
                                                if tt {
                                                    ps.preone = Some(v);
                                                } else {
                                                    ps.postone = Some(v);
                                                }
                                                s = t + arglen - 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        b'm' => ps.multi_width += 1,
                        b'p' => escapes = true,
                        b'k' => ps.hkeys = SCANPM_WANTKEYS,
                        b'v' => ps.hvals = SCANPM_WANTVALS,
                        b't' => ps.wantt = true,
                        b'%' => ps.presc += 1,
                        b'g' => {
                            let (t, arglen) = self.get_strarg(&buf, s + 1);
                            if ps.getkeys < 0 {
                                ps.getkeys = 0;
                            }
                            if t < buf.len() {
                                let mut k = s + 1 + arglen;
                                while k < t {
                                    match at(&buf, k) {
                                        b'e' => ps.getkeys |= crate::utils::GETKEY_EMACS as i32,
                                        b'o' => ps.getkeys |= crate::utils::GETKEY_OCTAL_ESC as i32,
                                        b'c' => ps.getkeys |= crate::utils::GETKEY_CTRL as i32,
                                        _ => {
                                            flagerr = true;
                                            break;
                                        }
                                    }
                                    k += 1;
                                }
                                s = t + arglen - 1;
                            } else {
                                flagerr = true;
                            }
                        }
                        b'z' => ps.shsplit = crate::hist::LEXFLAGS_ACTIVE,
                        b'Z' => {
                            let (t, arglen) = self.get_strarg(&buf, s + 1);
                            if t < buf.len() {
                                let mut k = s + 1 + arglen;
                                while k < t {
                                    match at(&buf, k) {
                                        b'c' => ps.shsplit |= crate::hist::LEXFLAGS_COMMENTS_KEEP,
                                        b'C' => ps.shsplit |= crate::hist::LEXFLAGS_COMMENTS_STRIP,
                                        b'n' => ps.shsplit |= crate::hist::LEXFLAGS_NEWLINE,
                                        _ => {
                                            flagerr = true;
                                            break;
                                        }
                                    }
                                    k += 1;
                                }
                                s = t + arglen - 1;
                            } else {
                                flagerr = true;
                            }
                        }
                        b'u' => ps.unique = true,
                        b'#' | POUND => ps.evalchar = true,
                        b'_' => {
                            let (t, arglen) = self.get_strarg(&buf, s + 1);
                            if t < buf.len() && t == s + 1 + arglen {
                                s = t + arglen - 1;
                            } else {
                                flagerr = true;
                            }
                        }
                        _ => flagerr = true,
                    }
                    if flagerr {
                        let mut shown = from(&buf, aptr + 1).to_vec();
                        tok::untokenize(&mut shown);
                        let offset = s - aptr + 1;
                        self.zerr(&format!(
                            "error in flags near position {offset} in '${}'",
                            lossy(&shown)
                        ));
                        return None;
                    }
                    s += 1;
                }
                s += 1;
            }
        }
        let premul = ps.premul.clone().unwrap_or_else(|| b" ".to_vec());
        let postmul = ps.postmul.clone().unwrap_or_else(|| b" ".to_vec());
        loop {
            let c = at(&buf, s);
            if c == b'^' || c == HAT {
                s += 1;
                let c2 = at(&buf, s);
                if c2 == b'^' || c2 == HAT {
                    ps.plan9 = false;
                    s += 1;
                } else {
                    ps.plan9 = true;
                }
            } else if c == b'=' || c == EQUALS {
                s += 1;
                let c2 = at(&buf, s);
                if c2 == b'=' || c2 == EQUALS {
                    ps.spbreak = 0;
                    if ps.nojoin < 2 {
                        ps.nojoin = 0;
                    }
                    s += 1;
                } else {
                    ps.spbreak = 2;
                    if ps.nojoin < 2 {
                        ps.nojoin = i32::from(!has_ifs);
                    }
                }
            } else if (c == b'#' || c == POUND)
                && (ps.inbrace || !self.isset(POSIXIDENTIFIERS))
                && {
                    let cc = at(&buf, s + 1);
                    self.itype_end(&buf, s + 1, crate::utils::IIDENT, false) != s + 1
                        || matches!(
                            cc,
                            b'*' | STAR | b'@' | b'?' | QUEST | b'$' | STRING | QSTRING
                        )
                        || ((cc == b'#' || cc == POUND) && at(&buf, s + 2) == OUTBRACE)
                        || is_dash(cc)
                        || (cc == b':' && is_dash(at(&buf, s + 2)))
                        || (isstring(cc)
                            && (at(&buf, s + 2) == INBRACE || at(&buf, s + 2) == INPAR))
                }
            {
                ps.getlen = 1 + ps.whichlen;
                s += 1;
            } else if c == b'~' || c == TILDE {
                s += 1;
                let c2 = at(&buf, s);
                if c2 == b'~' || c2 == TILDE {
                    ps.globsubst = 0;
                    s += 1;
                } else {
                    ps.globsubst = 2;
                }
            } else if c == b'+' {
                if self.itype_end(&buf, s + 1, crate::utils::IIDENT, false) != s + 1
                    || (ps.aspar
                        && isstring(at(&buf, s + 1))
                        && (at(&buf, s + 2) == INBRACE || at(&buf, s + 2) == INPAR))
                {
                    ps.chkset = true;
                    s += 1;
                } else if !ps.inbrace {
                    if let Some(x) = buf.get_mut(aptr) {
                        *x = b'$';
                    }
                    if let Some(w) = l.words.get_mut(n) {
                        *w = buf;
                    }
                    *pos = aptr + 1;
                    return Some(n);
                } else {
                    self.zerr("bad substitution");
                    return None;
                }
            } else if ps.inbrace && tok::is_null(c) && c != BNULL {
                s += 1;
            } else {
                break;
            }
        }
        if qt {
            ps.globsubst = 0;
        }
        // The name, or a nested substitution.
        let mut idbeg = s;
        let mut idbeg_buf: Option<Vec<u8>> = None;
        let mut v: Option<Value> = None;
        let fetch_needed;
        let mut subexp = ps.inbrace
            && s > 0
            && at(&buf, s - 1) != 0
            && isstring(at(&buf, s))
            && matches!(at(&buf, s + 1), INBRACE | INPAR | INPARMATH);
        if subexp {
            let quoted = at(&buf, s) == QSTRING;
            let vstart = s;
            s += 1;
            let outtok = match at(&buf, s) {
                INBRACE => OUTBRACE,
                INPAR => OUTPAR,
                _ => OUTPARMATH,
            };
            let open = at(&buf, s);
            let _ = crate::glob::skipparens(open, outtok, &buf, &mut s);
            let inner = sub(&buf, vstart, s).to_vec();
            let (sv, av, isarr, empty) =
                self.multsub(&inner, PREFORK_SUBEXP, !ps.aspar, None, &mut ps.ms_flags);
            ps.val = sv;
            ps.isarr = isarr;
            if let Some(a) = av {
                ps.aval = a;
            }
            if empty && quoted {
                ps.isarr = -1;
                ps.aval = Vec::new();
                ps.aspar = false;
            } else if ps.aspar {
                idbeg_buf = Some(ps.val.clone());
            }
            if at(&ps.val, 0) == NULARG {
                ps.val.remove(0);
            }
            while tok::is_null(at(&buf, s)) && s < buf.len() {
                s += 1;
            }
            if ps.ms_flags & MULTSUB_PARAM_NAME != 0 {
                if ps.isarr != 0 {
                    if ps.aval.len() > 1 {
                        self.zerr("parameter name reference used with array");
                        return None;
                    }
                    ps.val = ps.aval.first().cloned().unwrap_or_default();
                    ps.isarr = 0;
                }
                // Behave as if the name had been written here.
                let mut nb = sub(&buf, 0, s).to_vec();
                let name_at = nb.len();
                nb.extend_from_slice(&ps.val);
                nb.extend_from_slice(from(&buf, s));
                buf = nb;
                s = name_at;
                idbeg = s;
                idbeg_buf = None;
                subexp = false;
                fetch_needed = ps.aspar && pf_flags & PREFORK_SUBEXP == 0;
            } else {
                fetch_needed = false;
            }
            v = None;
        } else {
            fetch_needed = ps.aspar;
        }
        if fetch_needed {
            let mut i = s;
            match self.fetchvalue(&mut buf, &mut i, 1, if qt { SCANPM_DQUOTED } else { 0 }) {
                Some(mut fv) => {
                    s = i;
                    ps.val = self.getstrvalue(Some(&mut fv));
                    idbeg_buf = Some(ps.val.clone());
                    subexp = true;
                }
                None => ps.vunset = 1,
            }
        }
        if ps.aspar && pf_flags & PREFORK_SUBEXP != 0 {
            ps.aspar = false;
            *ret_flags |= MULTSUB_PARAM_NAME;
        }
        if !subexp || ps.aspar {
            let mut scanflags = ps.hkeys | ps.hvals;
            if ps.arrasg != 0 {
                scanflags |= SCANPM_ASSIGNING;
            }
            if qt {
                scanflags |= SCANPM_DQUOTED;
            }
            if ps.chkset {
                scanflags |= SCANPM_CHECKING;
            }
            let bracks = if ps.wantt {
                -1
            } else if !self.isset(KSHARRAYS) || ps.inbrace {
                1
            } else {
                -1
            };
            let fetched = if subexp {
                let mut ov = ps.val.clone();
                let mut i = 0;
                self.fetchvalue(&mut ov, &mut i, bracks, scanflags)
            } else {
                let mut i = s;
                let r = self.fetchvalue(&mut buf, &mut i, bracks, scanflags);
                s = i;
                r
            };
            match fetched {
                None => ps.vunset = 1,
                Some(fv) => {
                    if self.pm_flags(&fv.pm) & PM_UNSET != 0 || fv.flags & VALFLAG_EMPTY != 0 {
                        ps.vunset = 1;
                    }
                    v = Some(fv);
                }
            }
            if ps.wantt {
                let mut kept = false;
                if let Some(fv) = &v {
                    let f = self.pm_flags(&fv.pm);
                    if f & PM_DECLARED != 0 || f & PM_UNSET == 0 {
                        let mut t = match pm_type(f) {
                            PM_ARRAY => "array",
                            PM_INTEGER => "integer",
                            PM_EFLOAT | PM_FFLOAT => "float",
                            PM_HASHED => "association",
                            _ => "scalar",
                        }
                        .to_owned();
                        if self.pm(&fv.pm).is_some_and(|p| p.level != 0) {
                            t.push_str("-local");
                        }
                        for (bit, name) in [
                            (PM_LEFT, "-left"),
                            (PM_RIGHT_B, "-right_blanks"),
                            (PM_RIGHT_Z, "-right_zeros"),
                            (PM_LOWER, "-lower"),
                            (PM_UPPER, "-upper"),
                            (PM_READONLY, "-readonly"),
                            (PM_TAGGED, "-tag"),
                            (PM_TIED, "-tied"),
                            (PM_EXPORTED, "-export"),
                            (PM_UNIQUE, "-unique"),
                            (PM_HIDE, "-hide"),
                            (PM_HIDEVAL, "-hideval"),
                            (PM_SPECIAL, "-special"),
                        ] {
                            if f & bit != 0 {
                                t.push_str(name);
                            }
                        }
                        ps.val = t.into_bytes();
                        ps.vunset = 0;
                        kept = true;
                    }
                }
                if !kept {
                    ps.val = Vec::new();
                }
                v = None;
                ps.isarr = 0;
            }
        }
        // Convert v into val/aval, applying further subscripts.
        loop {
            let use_v = v.is_some();
            if !use_v
                && !((ps.inbrace || (!self.isset(KSHARRAYS) && ps.vunset != 0))
                    && isbrack(at(&buf, s)))
            {
                break;
            }
            let mut fv = match v.take() {
                Some(fv) => fv,
                None => {
                    if !isbrack(at(&buf, s)) {
                        break;
                    }
                    if ps.vunset != 0 {
                        ps.val = Vec::new();
                        ps.isarr = 0;
                    }
                    let mut p = Param::new(if ps.isarr != 0 { PM_ARRAY } else { PM_SCALAR });
                    p.u = if ps.isarr != 0 {
                        U::Arr(ps.aval.clone())
                    } else {
                        U::Str(ps.val.clone())
                    };
                    let mut nv = Value::new(PmRef::Transient(Box::new(p), Vec::new()));
                    nv.isarr = ps.isarr;
                    nv.end = -1;
                    let os = s;
                    let mut i = s;
                    let r = self.getindex(
                        &mut buf,
                        &mut i,
                        &mut nv,
                        if qt { SCANPM_DQUOTED } else { 0 },
                    );
                    s = i;
                    if r != 0 || s == os {
                        break;
                    }
                    nv
                }
            };
            ps.isarr = fv.isarr;
            if ps.isarr != 0 {
                if fv.isarr == SCANPM_WANTINDEX {
                    ps.isarr = 0;
                    fv.isarr = 0;
                    ps.val = self.pm_name(&fv.pm);
                } else {
                    ps.aval = self.getarrvalue(Some(&mut fv));
                }
            } else {
                if self.pm_flags(&fv.pm) & PM_ARRAY != 0 {
                    let arrlen = i64::try_from(self.getafn(&fv.pm).len()).unwrap_or(0);
                    if fv.start < 0 {
                        fv.start += arrlen + i64::from(fv.flags & VALFLAG_INV != 0);
                    }
                    if fv.flags & VALFLAG_INV == 0 && (fv.start < 0 || fv.start >= arrlen) {
                        ps.vunset = 1;
                    }
                }
                if ps.vunset == 0 {
                    fv.flags |= VALFLAG_SUBST;
                    ps.val = self.getstrvalue(Some(&mut fv));
                }
            }
            ps.horrible_offset_hack = matches!(
                self.pm(&fv.pm).map(|p| &p.gsu),
                Some(Gsu::VarArray(ArrVar::Pparams))
            ) || matches!(fv.pm, PmRef::Argv);
            if !ps.inbrace {
                break;
            }
        }
        if ps.inbrace {
            let c = at(&buf, s);
            if !is_dash(c)
                && !matches!(
                    c,
                    b'+' | b':'
                        | b'%'
                        | b'/'
                        | b'='
                        | EQUALS
                        | b'#'
                        | POUND
                        | b'?'
                        | QUEST
                        | b'}'
                        | OUTBRACE
                )
            {
                self.zerr("bad substitution");
                return None;
            }
        }
        if ps.isarr != 0 {
            if ps.nojoin != 0 {
                ps.isarr = -1;
            }
            if qt && ps.getlen == 0 && ps.isarr > 0 {
                ps.val = self.sepjoin(&ps.aval, ps.sep.as_deref());
                ps.isarr = 0;
            }
        }
        let idend = s;
        let idname: Vec<u8> = match &idbeg_buf {
            Some(b) => b.clone(),
            None => sub(&buf, idbeg, idend).to_vec(),
        };
        if ps.inbrace {
            while tok::is_null(at(&buf, s)) && s < buf.len() {
                s += 1;
            }
        }
        let mut colf = at(&buf, s) == b':';
        if colf {
            s += 1;
        }
        let mut fstr = s;
        let mut fstr_rest: Vec<u8>;
        let expr_end;
        if ps.inbrace {
            let mut bct = 1;
            while fstr < buf.len() {
                let c = at(&buf, fstr);
                if c == INBRACE {
                    bct += 1;
                } else if c == OUTBRACE {
                    bct -= 1;
                    if bct == 0 {
                        break;
                    }
                }
                fstr += 1;
            }
            if bct != 0 {
                self.zerr("closing brace expected");
                return None;
            }
            expr_end = fstr;
            fstr_rest = from(&buf, fstr + 1).to_vec();
        } else {
            expr_end = buf.len();
            fstr_rest = Vec::new();
        }
        // The operator text, as zsh sees it once the closing brace is a NUL.
        let mut expr: Vec<u8> = sub(&buf, 0, expr_end).to_vec();
        let c = at(&expr, s);
        let op_here = ps.inbrace
            && (c == b'+'
                || is_dash(c)
                || c == b':'
                || c == b'='
                || c == EQUALS
                || c == b'%'
                || c == b'#'
                || c == POUND
                || c == b'?'
                || c == QUEST
                || c == b'/');
        let mut goto_colonsubscript = false;
        if op_here {
            let eglob = self.isset(EXTENDEDGLOB);
            if ps.flnum == 0 {
                ps.flnum += 1;
            }
            if c == b'%' {
                ps.flags |= crate::glob::SUB_END;
            }
            if (c == b'%' || c == b'#' || c == POUND) && c == at(&expr, s + 1) {
                s += 1;
                ps.flags |= crate::glob::SUB_LONG;
            }
            s += 1;
            let opc = at(&expr, s - 1);
            if opc == b'/' {
                ps.flags = (if ps.flags & crate::glob::SUB_SUBSTR != 0 {
                    0
                } else {
                    crate::glob::SUB_LONG
                }) | (ps.flags & crate::glob::SUB_EGLOB);
                let mut cc = at(&expr, s);
                if cc == b'/' {
                    ps.flags |= crate::glob::SUB_GLOBAL;
                    s += 1;
                    cc = at(&expr, s);
                }
                if cc == b'#' || cc == POUND {
                    ps.flags |= crate::glob::SUB_START;
                    s += 1;
                }
                if at(&expr, s) == b'%' {
                    ps.flags |= crate::glob::SUB_END;
                    s += 1;
                }
                if ps.flags & (crate::glob::SUB_START | crate::glob::SUB_END) == 0 {
                    ps.flags |= crate::glob::SUB_SUBSTR;
                }
                let mut ptr = s;
                while ptr < expr.len() && at(&expr, ptr) != b'/' {
                    let pc = at(&expr, ptr);
                    if (pc == BNULL || pc == BNULLKEEP || pc == b'\\') && ptr + 1 < expr.len() {
                        if at(&expr, ptr + 1) == b'/' {
                            let _ = expr.remove(ptr);
                        } else {
                            ptr += 1;
                        }
                    }
                    ptr += 1;
                }
                ps.replstr = Some(if ptr < expr.len() && ptr + 1 < expr.len() {
                    from(&expr, ptr + 1).to_vec()
                } else {
                    Vec::new()
                });
                expr.truncate(ptr);
            }
            if colf {
                ps.flags |= crate::glob::SUB_ALL;
            }
            if ps.flags
                & (crate::glob::SUB_MATCH
                    | crate::glob::SUB_REST
                    | crate::glob::SUB_BIND
                    | crate::glob::SUB_EIND
                    | crate::glob::SUB_LEN)
                == 0
            {
                ps.flags |= crate::glob::SUB_REST;
            }
            if colf && ps.vunset == 0 {
                let empty = if ps.isarr != 0 {
                    ps.aval.is_empty()
                } else {
                    ps.val.is_empty() || ps.val == [NULARG]
                };
                ps.vunset = -i32::from(empty);
            }
            let word = from(&expr, s).to_vec();
            match opc {
                b'+' | b'-' | DASH => {
                    let mut dash = opc != b'+';
                    if opc == b'+' {
                        if ps.vunset != 0 {
                            ps.val = Vec::new();
                            ps.copied = true;
                            ps.isarr = 0;
                        } else {
                            ps.vunset = 1;
                            dash = true;
                        }
                    }
                    if dash && ps.vunset != 0 {
                        let split_flags = if ps.spbreak != 0 {
                            PREFORK_SHWORDSPLIT | if ps.aspar { 0 } else { PREFORK_SPLIT }
                        } else {
                            PREFORK_NOSHWORDSPLIT
                        };
                        let (sv, av, isarr, _) =
                            self.multsub(&word, split_flags, !ps.aspar, None, &mut ps.ms_flags);
                        ps.val = sv;
                        ps.isarr = isarr;
                        if let Some(a) = av {
                            ps.aval = a;
                        }
                        ps.copied = true;
                        ps.spbreak = 0;
                        if ps.globsubst != 2 {
                            ps.globsubst = 0;
                        }
                    }
                }
                b':' if !(at(&expr, s) == b'=' || at(&expr, s) == EQUALS) => {
                    s -= 1;
                    goto_colonsubscript = true;
                }
                b':' | b'=' | EQUALS => {
                    let word = if opc == b':' {
                        ps.vunset = 1;
                        s += 1;
                        from(&expr, s).to_vec()
                    } else {
                        word
                    };
                    if ps.vunset != 0 {
                        if ps.spsep.is_some() || ps.arrasg == 0 {
                            let (sv, _, isarr, _) = self.multsub(
                                &word,
                                if ps.spbreak != 0 {
                                    PREFORK_SINGLE
                                } else {
                                    PREFORK_NOSHWORDSPLIT
                                },
                                false,
                                None,
                                &mut ps.ms_flags,
                            );
                            ps.val = sv;
                            ps.isarr = isarr;
                        } else {
                            let split_flags = if ps.spbreak != 0 {
                                PREFORK_SPLIT | PREFORK_SHWORDSPLIT
                            } else {
                                PREFORK_NOSHWORDSPLIT
                            };
                            let (sv, av, isarr, _) =
                                self.multsub(&word, split_flags, true, None, &mut ps.ms_flags);
                            ps.val = sv;
                            ps.isarr = isarr;
                            if let Some(a) = av {
                                ps.aval = a;
                            }
                            ps.spbreak = 0;
                        }
                        if ps.arrasg != 0 {
                            let t: Vec<Vec<u8>> = if ps.spsep.is_some() || ps.spbreak != 0 {
                                ps.aval = self.sepsplit(&ps.val, ps.spsep.as_deref(), false);
                                ps.isarr = if ps.nojoin != 0 { 1 } else { 2 };
                                let mut a = ps.aval.clone();
                                if a.last().is_some_and(Vec::is_empty) {
                                    let _ = a.pop();
                                }
                                if a.first().is_some_and(Vec::is_empty) {
                                    a.remove(0);
                                }
                                a
                            } else if ps.isarr == 0 {
                                if ps.val.is_empty() && ps.arrasg > 1 {
                                    Vec::new()
                                } else {
                                    vec![ps.val.clone()]
                                }
                            } else {
                                ps.aval.clone()
                            };
                            let a: Vec<Vec<u8>> = t
                                .into_iter()
                                .map(|mut x| {
                                    tok::untokenize(&mut x);
                                    x
                                })
                                .collect();
                            if ps.arrasg > 1 {
                                if let Some(pmr) = self.sethparam(&idname, a) {
                                    let _ = pmr;
                                    ps.aval = self.gethparam_scan(&idname, ps.hkeys | ps.hvals);
                                }
                            } else {
                                let _ = self.setaparam(&idname, a);
                            }
                            ps.isarr = 1;
                            ps.arrasg = 0;
                        } else {
                            let mut vv = ps.val.clone();
                            tok::untokenize(&mut vv);
                            let _ = self.setsparam(&idname, vv);
                        }
                        ps.copied = true;
                        if ps.isarr != 0 {
                            if ps.nojoin != 0 {
                                ps.isarr = -1;
                            }
                            if qt
                                && ps.getlen == 0
                                && ps.isarr > 0
                                && ps.spsep.is_none()
                                && ps.spbreak < 2
                            {
                                ps.val = self.sepjoin(&ps.aval, ps.sep.as_deref());
                                ps.isarr = 0;
                            }
                            ps.sep = None;
                            ps.spsep = None;
                            ps.spbreak = 0;
                        }
                    }
                }
                b'?' | QUEST => {
                    if ps.vunset != 0 {
                        if self.isset(EXECOPT) {
                            let msg = if word.is_empty() {
                                b"parameter not set".to_vec()
                            } else {
                                word.clone()
                            };
                            self.zerr(&format!("{}: {}", lossy(&idname), lossy(&msg)));
                            self.errflag.set(self.errflag.get() | (ERRFLAG_HARD));
                            if !self.isset(INTERACTIVE) {
                                // SAFETY: getpid has no preconditions.
                                if i64::from(self.mypid) == i64::from(unsafe { libc::getpid() }) {
                                    self.zexit(1, crate::signals::ZEXIT_NORMAL);
                                } else {
                                    crate::shell::exit_now(1);
                                }
                            }
                        }
                        return None;
                    }
                }
                _ => {
                    // %, #, / : pattern matching.
                    let mut pat = word.clone();
                    let one = self.noerrs;
                    let oef = self.errflag.get();
                    if !ps.quoteerr {
                        self.noerrs = 1;
                    }
                    let haserr = self.parse_subst_string(&mut pat);
                    self.noerrs = one;
                    if !ps.quoteerr {
                        self.errflag.set(oef | (self.errflag.get() & ERRFLAG_INT));
                        if haserr {
                            crate::pattern::shtokenize(&mut pat, self.isset(SHGLOB));
                        }
                    } else if haserr || self.errflag() {
                        self.zerr(&format!(
                            "parse error in ${{...{}...}} substitution",
                            char::from(opc)
                        ));
                        return None;
                    }
                    let pat = self.singsub(&pat);
                    if ps.vunset == 0 && ps.isarr != 0 {
                        for a in &mut ps.aval {
                            tok::untokenize(a);
                        }
                        ps.copied = true;
                        let saved = self.opts[EXTENDEDGLOB];
                        if ps.flags & crate::glob::SUB_EGLOB != 0 {
                            self.opts[EXTENDEDGLOB] = true;
                        }
                        let mut aval = std::mem::take(&mut ps.aval);
                        self.getmatcharr(
                            &mut aval,
                            &pat,
                            ps.flags,
                            i32::try_from(ps.flnum).unwrap_or(1),
                            ps.replstr.clone(),
                        );
                        ps.aval = aval;
                        self.opts[EXTENDEDGLOB] = saved;
                        let _ = eglob;
                    } else {
                        if ps.vunset != 0 {
                            if ps.vunset > 0 && !self.isset(UNSET) {
                                self.zerr(&format!("{}: parameter not set", lossy(&idname)));
                                return None;
                            }
                            ps.val = Vec::new();
                        }
                        if !ps.copied {
                            ps.copied = true;
                            tok::untokenize(&mut ps.val);
                        }
                        let saved = self.opts[EXTENDEDGLOB];
                        if ps.flags & crate::glob::SUB_EGLOB != 0 {
                            self.opts[EXTENDEDGLOB] = true;
                        }
                        let mut val = std::mem::take(&mut ps.val);
                        let _ = self.getmatch(
                            &mut val,
                            &pat,
                            ps.flags,
                            i32::try_from(ps.flnum).unwrap_or(1),
                            ps.replstr.clone(),
                        );
                        ps.val = val;
                        self.opts[EXTENDEDGLOB] = saved;
                    }
                }
            }
        } else if ps.inbrace && (at(&expr, s) == b'^' || at(&expr, s) == HAT) {
            let mut shortest = true;
            s += 1;
            if at(&expr, s) == b'^' || at(&expr, s) == HAT {
                shortest = false;
                s += 1;
            }
            let name = from(&expr, s).to_vec();
            if self.itype_end(&name, 0, crate::utils::IIDENT, false) < name.len() {
                let mut shown = name.clone();
                tok::untokenize(&mut shown);
                self.zerr(&format!("not an identifier: {}", lossy(&shown)));
                return None;
            }
            if ps.vunset != 0 {
                if ps.vunset > 0 && !self.isset(UNSET) {
                    self.zerr(&format!("{}: parameter not set", lossy(&idname)));
                    return None;
                }
                ps.val = Vec::new();
            } else {
                let zip = self
                    .getaparam(&name)
                    .or_else(|| self.getsparam(&name).map(|v| vec![v]));
                if ps.isarr == 0 {
                    ps.aval = vec![ps.val.clone()];
                    ps.isarr = 1;
                }
                match zip {
                    Some(zip) => {
                        let alen = ps.aval.len();
                        let ziplen = zip.len();
                        let outlen = if shortest ^ (alen > ziplen) {
                            alen
                        } else {
                            ziplen
                        };
                        if !shortest && (alen == 0 || ziplen == 0) {
                            if ziplen != 0 {
                                ps.aval = zip;
                            }
                        } else {
                            let mut out = Vec::with_capacity(outlen * 2);
                            for i in 0..outlen {
                                out.push(ps.aval.get(i % alen.max(1)).cloned().unwrap_or_default());
                                out.push(zip.get(i % ziplen.max(1)).cloned().unwrap_or_default());
                            }
                            ps.aval = out;
                            ps.copied = true;
                        }
                    }
                    None => {
                        if !self.isset(UNSET) {
                            self.zerr(&format!("{}: parameter not set", lossy(&name)));
                            return None;
                        }
                        ps.val = Vec::new();
                    }
                }
            }
        } else if ps.inbrace && matches!(at(&expr, s), b'|' | BAR | b'*' | STAR) {
            let intersect = matches!(at(&expr, s), b'*' | STAR);
            s += 1;
            let name = from(&expr, s).to_vec();
            if self.itype_end(&name, 0, crate::utils::IIDENT, false) < name.len() {
                let mut shown = name.clone();
                tok::untokenize(&mut shown);
                self.zerr(&format!("not an identifier: {}", lossy(&shown)));
                return None;
            }
            match self.getaparam(&name) {
                Some(compare) => {
                    let set: std::collections::HashSet<Vec<u8>> = compare.into_iter().collect();
                    if ps.vunset == 0 && ps.isarr != 0 {
                        ps.copied = true;
                        let mut kept = Vec::new();
                        for mut a in std::mem::take(&mut ps.aval) {
                            tok::untokenize(&mut a);
                            let present = set.contains(&a);
                            if intersect == present {
                                kept.push(a);
                            }
                        }
                        ps.aval = kept;
                    } else if ps.vunset != 0 {
                        if ps.vunset > 0 && !self.isset(UNSET) {
                            self.zerr(&format!("{}: parameter not set", lossy(&idname)));
                            return None;
                        }
                        ps.val = Vec::new();
                    } else {
                        let present = set.contains(&ps.val);
                        if intersect != present {
                            ps.val = Vec::new();
                        }
                    }
                }
                None => {
                    if intersect && ps.vunset == 0 {
                        if ps.isarr != 0 {
                            ps.aval = Vec::new();
                        } else {
                            ps.val = Vec::new();
                        }
                    }
                }
            }
            if ps.vunset != 0 {
                if ps.vunset > 0 && !self.isset(UNSET) {
                    self.zerr(&format!("{}: parameter not set", lossy(&idname)));
                    return None;
                }
                ps.val = Vec::new();
            }
        } else {
            goto_colonsubscript = true;
        }
        if goto_colonsubscript {
            if ps.chkset {
                ps.val = if ps.vunset != 0 {
                    b"0".to_vec()
                } else {
                    b"1".to_vec()
                };
                ps.isarr = 0;
            } else if ps.vunset != 0 {
                if ps.vunset > 0 && !self.isset(UNSET) {
                    self.zerr(&format!("{}: parameter not set", lossy(&idname)));
                    return None;
                }
                ps.val = Vec::new();
            }
            if colf
                && ps.inbrace
                && let Some((off_text, off_end)) = self.check_colon_subscript(&expr, s)
            {
                let mut offset = self.mathevali(&off_text);
                let mut length: i64 = 0;
                let mut length_set = false;
                let mut offset_hack_argzero = false;
                if self.errflag() {
                    return None;
                }
                let mut end2 = off_end;
                if end2 < expr.len() && at(&expr, end2) != b':' {
                    self.zerr(&format!("invalid subscript: {}", lossy(&off_text)));
                    return None;
                }
                if end2 < expr.len()
                    && let Some((len_text, nextp)) = self.check_colon_subscript(&expr, end2 + 1)
                {
                    end2 = nextp;
                    if end2 < expr.len() && at(&expr, end2) != b':' {
                        self.zerr(&format!("invalid length: {}", lossy(&len_text)));
                        return None;
                    }
                    length = self.mathevali(&len_text);
                    length_set = true;
                    if self.errflag() {
                        return None;
                    }
                }
                if !ps.aval.is_empty() && ps.isarr == 0 {
                    ps.quoted_array_with_offset = true;
                }
                if ps.isarr != 0 || ps.quoted_array_with_offset {
                    if ps.horrible_offset_hack {
                        if offset == 0 {
                            offset_hack_argzero = true;
                        } else if offset > 0 {
                            offset -= 1;
                        }
                    }
                    let mut alen = i64::try_from(ps.aval.len()).unwrap_or(0);
                    if offset < 0 {
                        offset += alen;
                        if offset < 0 {
                            offset = 0;
                        }
                    }
                    if offset_hack_argzero {
                        alen += 1;
                    }
                    if length_set {
                        if length < 0 {
                            length += alen - offset;
                        }
                        if length < 0 {
                            self.zerr(&format!(
                                "substring expression: {} < {}",
                                length + offset,
                                offset
                            ));
                            return None;
                        }
                    } else {
                        length = alen;
                    }
                    if offset > alen {
                        offset = alen;
                    }
                    if offset + length > alen {
                        length = alen - offset;
                    }
                    let mut count = length;
                    let mut newarr = Vec::new();
                    if count > 0 && offset_hack_argzero {
                        newarr.push(self.argzero.clone());
                        count -= 1;
                    }
                    let off = usize::try_from(offset).unwrap_or(0);
                    for k in 0..usize::try_from(count).unwrap_or(0) {
                        newarr.push(ps.aval.get(off + k).cloned().unwrap_or_default());
                    }
                    ps.aval = newarr;
                } else {
                    let val = ps.val.clone();
                    let nchars =
                        i64::try_from(crate::utils::mb_metastrlen0(self, &val)).unwrap_or(0);
                    if offset < 0 {
                        offset += nchars;
                        if offset < 0 {
                            offset = 0;
                        }
                    }
                    let given_offset = offset;
                    if length_set && length < 0 {
                        length -= offset;
                    }
                    let mut sp = 0usize;
                    let mut o = offset;
                    while sp < val.len() && o > 0 {
                        sp += crate::utils::mb_metacharlen(self, from(&val, sp)).max(1);
                        o -= 1;
                    }
                    if length_set {
                        if length < 0 {
                            length += nchars;
                            if length < 0 {
                                self.zerr(&format!(
                                    "substring expression: {} < {}",
                                    length + given_offset,
                                    given_offset
                                ));
                                return None;
                            }
                        }
                        let mut ep = sp;
                        let mut len = length;
                        while ep < val.len() && len > 0 {
                            ep += crate::utils::mb_metacharlen(self, from(&val, ep)).max(1);
                            len -= 1;
                        }
                        ps.val = sub(&val, sp, ep).to_vec();
                    } else {
                        ps.val = from(&val, sp).to_vec();
                    }
                }
                if end2 >= expr.len() {
                    colf = false;
                } else {
                    s = end2 + 1;
                }
            }
            if colf {
                let ms = s.saturating_sub(1);
                if !self.isset(KSHARRAYS) || ps.inbrace {
                    let mut mi = ms;
                    if ps.isarr == 0 {
                        let mut val = std::mem::take(&mut ps.val);
                        self.modify(&mut val, &expr, &mut mi, ps.inbrace);
                        ps.val = val;
                    } else {
                        let mut out = Vec::with_capacity(ps.aval.len());
                        let mut last = ms;
                        for a in std::mem::take(&mut ps.aval) {
                            let mut a = a;
                            let mut ss = ms;
                            self.modify(&mut a, &expr, &mut ss, ps.inbrace);
                            last = ss;
                            out.push(a);
                        }
                        if out.is_empty() {
                            let mut t = Vec::new();
                            let mut ss = ms;
                            self.modify(&mut t, &expr, &mut ss, ps.inbrace);
                            last = ss;
                        }
                        ps.aval = out;
                        mi = last;
                    }
                    s = mi;
                    ps.copied = true;
                    if ps.inbrace && s < expr.len() {
                        if at(&expr, s) == b':' && !tok::is_meta(at(&expr, s + 1)) {
                            self.zerr(&format!(
                                "unrecognized modifier `{}'",
                                char::from(at(&expr, s + 1))
                            ));
                        } else {
                            self.zerr("unrecognized modifier");
                        }
                        return None;
                    }
                }
            }
            if !ps.inbrace {
                fstr = s;
            }
        }
        if !ps.inbrace {
            fstr_rest = from(&buf, fstr.min(buf.len())).to_vec();
        }
        if self.errflag() {
            return None;
        }
        if ps.evalchar {
            let one = self.noerrs;
            let oef = self.errflag.get();
            if !ps.quoteerr {
                self.noerrs = 1;
            }
            let mut haserr = false;
            if ps.isarr != 0 {
                let mut out = Vec::new();
                for a in &ps.aval.clone() {
                    match self.substevalchar(a) {
                        Some(v) => out.push(v),
                        None => {
                            haserr = true;
                            break;
                        }
                    }
                }
                ps.aval = out;
            } else {
                match self.substevalchar(&ps.val.clone()) {
                    Some(v) => ps.val = v,
                    None => haserr = true,
                }
            }
            self.noerrs = one;
            if !ps.quoteerr {
                self.errflag.set(oef | (self.errflag.get() & ERRFLAG_INT));
            }
            if haserr || self.errflag() {
                return None;
            }
            ps.ms_flags = 0;
        }
        if ps.getlen != 0 {
            let len: i64 = if ps.isarr != 0 {
                let sl = ps.sep.as_deref().map_or(1, |s| {
                    i64::try_from(crate::utils::mb_metastrlen0(self, s)).unwrap_or(1)
                });
                if ps.getlen == 1 {
                    i64::try_from(ps.aval.len()).unwrap_or(0)
                } else if ps.getlen == 2 {
                    if ps.aval.is_empty() {
                        0
                    } else {
                        let mut total = -sl;
                        for a in &ps.aval {
                            total += sl
                                + i64::try_from(crate::utils::mb_metastrlen(
                                    self,
                                    a,
                                    ps.multi_width,
                                ))
                                .unwrap_or(0);
                        }
                        total
                    }
                } else {
                    ps.aval
                        .iter()
                        .map(|a| {
                            i64::try_from(self.wordcount(
                                a,
                                ps.spsep.as_deref(),
                                i32::from(ps.getlen > 3),
                            ))
                            .unwrap_or(0)
                        })
                        .sum()
                }
            } else if ps.getlen < 3 {
                i64::try_from(crate::utils::mb_metastrlen(self, &ps.val, ps.multi_width))
                    .unwrap_or(0)
            } else {
                i64::try_from(self.wordcount(
                    &ps.val,
                    ps.spsep.as_deref(),
                    i32::from(ps.getlen > 3),
                ))
                .unwrap_or(0)
            };
            ps.val = len.to_string().into_bytes();
            ps.isarr = 0;
            ps.ms_flags = 0;
        }
        if ps.isarr != 0 {
            l.flags |= LF_ARRAY;
        } else {
            l.flags &= !LF_ARRAY;
        }
        if ps.isarr > 0 && !ps.plan9 && ps.aval.is_empty() {
            ps.val = Vec::new();
            ps.isarr = 0;
        } else if ps.isarr != 0 && ps.aval.len() == 1 {
            ps.val = ps.aval.first().cloned().unwrap_or_default();
            ps.isarr = 0;
        }
        if ps.ssub
            || ps.spbreak != 0
            || ps.spsep.is_some()
            || ps.sep.is_some()
            || ps.quoted_array_with_offset
        {
            let force_split = !ps.ssub && (ps.spbreak != 0 || ps.spsep.is_some());
            if ps.isarr != 0 || ps.quoted_array_with_offset {
                if ps.nojoin == 0 || ps.sep.is_some() {
                    ps.val = self.sepjoin(&ps.aval, ps.sep.as_deref());
                    ps.isarr = 0;
                } else if force_split
                    && (ps.spsep.is_some()
                        || ps.nojoin == 2
                        || (self.ifs.is_none() && ps.isarr < 0))
                {
                    let jsep = if ps.nojoin == 1 {
                        None
                    } else {
                        ps.spsep.clone()
                    };
                    ps.val = self.sepjoin(&ps.aval, jsep.as_deref());
                    ps.isarr = 0;
                }
                if ps.isarr == 0 {
                    ps.ms_flags = 0;
                }
            }
            if force_split && ps.isarr == 0 {
                ps.aval = self.sepsplit(&ps.val, ps.spsep.as_deref(), false);
                if ps.aval.is_empty() {
                    ps.val = Vec::new();
                } else if ps.aval.len() == 1 {
                    ps.val = ps.aval.first().cloned().unwrap_or_default();
                } else {
                    ps.isarr = if ps.nojoin != 0 { 1 } else { 2 };
                }
            }
            if ps.isarr != 0 {
                l.flags |= LF_ARRAY;
            } else {
                l.flags &= !LF_ARRAY;
            }
        }
        if ps.casmod != CASMOD_NONE {
            ps.copied = true;
            if ps.isarr != 0 {
                ps.aval = ps
                    .aval
                    .iter()
                    .map(|a| self.casemodify(a, ps.casmod))
                    .collect();
            } else {
                ps.val = self.casemodify(&ps.val, ps.casmod);
            }
        }
        if ps.getkeys >= 0 {
            ps.copied = true;
            let gk = u32::try_from(ps.getkeys).unwrap_or(0);
            if ps.isarr != 0 {
                ps.aval = ps
                    .aval
                    .iter()
                    .map(|a| tok::metafy(&self.getkeystring(a, gk).0))
                    .collect();
            } else {
                ps.val = tok::metafy(&self.getkeystring(&ps.val, gk).0);
            }
        }
        if ps.presc != 0 {
            let ops = self.opts[PROMPTSUBST];
            let opb = self.opts[PROMPTBANG];
            let opp = self.opts[PROMPTPERCENT];
            if ps.presc < 2 {
                self.opts[PROMPTPERCENT] = true;
                self.opts[PROMPTSUBST] = false;
                self.opts[PROMPTBANG] = false;
            }
            if ps.isarr != 0 {
                ps.copied = true;
                let mut out = Vec::new();
                for mut a in std::mem::take(&mut ps.aval) {
                    tok::untokenize(&mut a);
                    out.push(self.promptexpand(&a, false, None, None).0);
                }
                ps.aval = out;
            } else {
                ps.copied = true;
                let mut v = std::mem::take(&mut ps.val);
                tok::untokenize(&mut v);
                ps.val = self.promptexpand(&v, false, None, None).0;
            }
            self.opts[PROMPTSUBST] = ops;
            self.opts[PROMPTBANG] = opb;
            self.opts[PROMPTPERCENT] = opp;
        }
        if ps.quotemod != 0 {
            let (pre, post) = if ps.quotemod > 0 {
                match ps.quotetype {
                    QT_DOLLARS => (2usize, 1usize),
                    QT_SINGLE_OPTIONAL | QT_BACKSLASH | QT_BACKSLASH_PATTERN => (0, 0),
                    _ => (1, 1),
                }
            } else {
                (0, 0)
            };
            let quote_one = |sh: &mut Shell, x: &[u8]| -> Vec<u8> {
                if ps.quotetype == QT_QUOTEDZPUTS {
                    sh.quotedzputs(x)
                } else if ps.quotetype > QT_BACKSLASH {
                    let tmp = sh.quotestring(x, qt_of(ps.quotetype));
                    let mut r = Vec::with_capacity(tmp.len() + pre + post);
                    let qch = if ps.quotetype != QT_DOUBLE {
                        b'\''
                    } else {
                        b'"'
                    };
                    if pre == 2 {
                        r.push(b'$');
                        r.push(qch);
                    } else if pre == 1 {
                        r.push(qch);
                    }
                    r.extend(tmp);
                    if post == 1 {
                        r.push(qch);
                    }
                    r
                } else {
                    sh.quotestring(x, Qt::BackslashShownull)
                }
            };
            if ps.isarr != 0 {
                ps.copied = true;
                if ps.quotemod > 0 {
                    ps.aval = ps.aval.clone().iter().map(|a| quote_one(self, a)).collect();
                } else {
                    let one = self.noerrs;
                    let oef = self.errflag.get();
                    if !ps.quoteerr {
                        self.noerrs = 1;
                    }
                    let mut haserr = false;
                    for a in &mut ps.aval {
                        haserr |= self.parse_subst_string(a);
                        crate::utils::remnulargs(a);
                        tok::untokenize(a);
                    }
                    self.noerrs = one;
                    if !ps.quoteerr {
                        self.errflag.set(oef | (self.errflag.get() & ERRFLAG_INT));
                    } else if haserr || self.errflag() {
                        self.zerr("parse error in parameter value");
                        return None;
                    }
                }
            } else {
                ps.copied = true;
                if ps.quotemod > 0 {
                    ps.val = quote_one(self, &ps.val.clone());
                } else {
                    let one = self.noerrs;
                    let oef = self.errflag.get();
                    if !ps.quoteerr {
                        self.noerrs = 1;
                    }
                    let mut v = std::mem::take(&mut ps.val);
                    let haserr = self.parse_subst_string(&mut v);
                    self.noerrs = one;
                    if !ps.quoteerr {
                        self.errflag.set(oef | (self.errflag.get() & ERRFLAG_INT));
                    } else if haserr || self.errflag() {
                        self.zerr("parse error in parameter value");
                        return None;
                    }
                    crate::utils::remnulargs(&mut v);
                    if v == [NULARG] {
                        v.clear();
                    }
                    tok::untokenize(&mut v);
                    ps.val = v;
                }
            }
        }
        if ps.mods != 0 {
            ps.copied = true;
            let apply = |sh: &mut Shell, x: &[u8]| -> Vec<u8> {
                let mut r = x.to_vec();
                if ps.mods & 1 != 0 {
                    r = sh.substnamedir(&r);
                }
                if ps.mods & 2 != 0 {
                    r = sh.nicedup(&r);
                }
                r
            };
            if ps.isarr != 0 {
                ps.aval = ps.aval.clone().iter().map(|a| apply(self, a)).collect();
            } else {
                ps.val = apply(self, &ps.val.clone());
            }
        }
        if ps.shsplit != 0 {
            let mut words: Vec<Vec<u8>> = Vec::new();
            if ps.isarr != 0 {
                for mut a in ps.aval.clone() {
                    tok::untokenize(&mut a);
                    words.extend(self.bufferwords(&a, ps.shsplit));
                }
                ps.isarr = 0;
            } else {
                let mut v = ps.val.clone();
                tok::untokenize(&mut v);
                words = self.bufferwords(&v, ps.shsplit);
            }
            if words.is_empty() {
                ps.val = Vec::new();
            } else if words.len() == 1 {
                ps.val = words.into_iter().next().unwrap_or_default();
            } else {
                ps.aval = words;
                ps.isarr = if ps.nojoin != 0 { 1 } else { 2 };
                l.flags |= LF_ARRAY;
            }
            ps.copied = true;
        }
        if ps.isarr != 0 && ps.ssub {
            ps.val = self.sepjoin(&ps.aval, None);
            ps.isarr = 0;
            l.flags &= !LF_ARRAY;
        }
        // Text before the substitution and after it.
        let mut ostr: Vec<u8> = sub(&buf, ostr_start, aptr).to_vec();
        if ps.ms_flags & MULTSUB_WS_AT_START != 0 && aptr > ostr_start {
            l.words.insert(n, ostr.clone());
            n += 1;
            ostr.clear();
            ostr_start = aptr;
        }
        let _ = ostr_start;
        if ps.ms_flags & MULTSUB_WS_AT_END != 0 && !fstr_rest.is_empty() {
            l.words.insert(n + 1, fstr_rest.clone());
            fstr_rest.clear();
        }
        if ps.arrasg != 0 && ps.isarr == 0 {
            l.flags |= LF_ARRAY;
            ps.aval = vec![ps.val.clone()];
            ps.isarr = 1;
        }
        let globsubst = ps.globsubst != 0;
        if ps.isarr != 0 {
            let on = n;
            if ps.unique && ps.aval.len() > 1 {
                uniqarray(&mut ps.aval);
            }
            if ps.aval.len() < 2 && !ps.plan9 {
                // Empty array or single element: drop quote markers around
                // it and put the value in place.
                let mut pre = ostr.clone();
                let mut post = fstr_rest.clone();
                if pre.last() == Some(&DNULL) && post.first() == Some(&DNULL) {
                    let _ = pre.pop();
                    post.remove(0);
                }
                let mut y = pre;
                let first = ps.aval.first().cloned().unwrap_or_default();
                y.extend_from_slice(&first);
                *pos = y.len();
                y.extend(post);
                if let Some(w) = l.words.get_mut(n) {
                    *w = y;
                }
                return Some(n);
            }
            if ps.sortit != SORTIT_ANYOLDHOW {
                if ps.indord {
                    if ps.sortit & SORTIT_BACKWARDS != 0 {
                        ps.aval.reverse();
                    }
                } else {
                    let mut a = std::mem::take(&mut ps.aval);
                    self.strmetasort(&mut a, ps.sortit);
                    ps.aval = a;
                }
            }
            let pad = |sh: &Shell, x: &[u8]| -> Vec<u8> {
                if ps.prenum != 0 || ps.postnum != 0 {
                    sh.dopadding(
                        x,
                        ps.prenum,
                        ps.postnum,
                        ps.preone.as_deref(),
                        ps.postone.as_deref(),
                        &premul,
                        &postmul,
                        ps.multi_width,
                    )
                } else {
                    x.to_vec()
                }
            };
            if ps.plan9 {
                // RC_EXPAND_PARAM: every element takes a copy of the rest of
                // the word, expanded once.
                let mut tl = WordList::one({
                    let mut m = vec![MARKER];
                    m.extend_from_slice(&fstr_rest);
                    m
                });
                if !ps.eval {
                    let mut rf = *ret_flags;
                    let r = self.stringsubst(
                        &mut tl,
                        0,
                        i32::from(ps.ssub) * PREFORK_SINGLE,
                        &mut rf,
                        false,
                    );
                    *ret_flags = rf;
                    r?;
                }
                let mut first_set = true;
                let mut insert_at = n;
                for x0 in ps.aval.clone() {
                    let mut x = pad(self, &x0);
                    if ps.eval && self.subst_parse_str(&mut x, qt && ps.nojoin == 0, ps.quoteerr) {
                        return None;
                    }
                    for tn in &tl.words {
                        if at(tn, 0) != MARKER {
                            break;
                        }
                        let (mut y, _) = self.strcatsub(&ostr, &x, Some(from(tn, 1)), globsubst);
                        if qt && y.is_empty() && ps.isarr != 2 {
                            y = nulstring();
                        }
                        if first_set {
                            if let Some(w) = l.words.get_mut(n) {
                                *w = y;
                            }
                            first_set = false;
                        } else {
                            insert_at += 1;
                            l.words.insert(insert_at, y);
                        }
                    }
                }
                for tn in tl.words.iter().skip_while(|t| at(t, 0) == MARKER) {
                    let mut y = tn.clone();
                    if qt && y.is_empty() && ps.isarr != 2 {
                        y = nulstring();
                    }
                    if first_set {
                        if let Some(w) = l.words.get_mut(n) {
                            *w = y;
                        }
                        first_set = false;
                    } else {
                        insert_at += 1;
                        l.words.insert(insert_at, y);
                    }
                }
                if first_set {
                    let _ = l.words.remove(n);
                    *pos = 0;
                    return Some(n.wrapping_sub(1));
                }
                n = insert_at;
                let _ = on;
                *pos = l.words.get(n).map_or(0, Vec::len);
                return Some(n);
            }
            let count = ps.aval.len();
            let mut x = pad(self, &ps.aval.first().cloned().unwrap_or_default());
            if ps.eval && self.subst_parse_str(&mut x, qt && ps.nojoin == 0, ps.quoteerr) {
                return None;
            }
            let (mut y, _) = self.strcatsub(&ostr, &x, None, globsubst);
            if qt && y.is_empty() && ps.isarr != 2 {
                y = nulstring();
            }
            if let Some(w) = l.words.get_mut(n) {
                *w = y;
            }
            let mut idx = n;
            for i in 1..count - 1 {
                let mut x = pad(self, &ps.aval.get(i).cloned().unwrap_or_default());
                if ps.eval && self.subst_parse_str(&mut x, qt && ps.nojoin == 0, ps.quoteerr) {
                    return None;
                }
                let y = if qt && x.is_empty() && ps.isarr != 2 {
                    nulstring()
                } else {
                    let mut y = x;
                    if globsubst {
                        crate::pattern::shtokenize(&mut y, self.isset(SHGLOB));
                    }
                    y
                };
                idx += 1;
                l.words.insert(idx, y);
            }
            let mut x = pad(self, &ps.aval.get(count - 1).cloned().unwrap_or_default());
            if ps.eval && self.subst_parse_str(&mut x, qt && ps.nojoin == 0, ps.quoteerr) {
                return None;
            }
            let (mut y, p) = self.strcatsub(&[], &x, Some(&fstr_rest), globsubst);
            if qt && y.is_empty() && ps.isarr != 2 {
                y = nulstring();
            }
            idx += 1;
            l.words.insert(idx, y);
            // zsh returns the original node with *str at the start of its
            // data, so the scan goes on over the words it inserted.
            let _ = p;
            *pos = 0;
            let _ = on;
            return Some(n);
        }
        let mut x = if ps.prenum != 0 || ps.postnum != 0 {
            self.dopadding(
                &ps.val,
                ps.prenum,
                ps.postnum,
                ps.preone.as_deref(),
                ps.postone.as_deref(),
                &premul,
                &postmul,
                ps.multi_width,
            )
        } else {
            ps.val.clone()
        };
        if ps.eval && self.subst_parse_str(&mut x, qt && ps.nojoin == 0, ps.quoteerr) {
            return None;
        }
        let (mut y, p) = self.strcatsub(&ostr, &x, Some(&fstr_rest), globsubst);
        *pos = p;
        if qt && y.is_empty() {
            y = nulstring();
        }
        if let Some(w) = l.words.get_mut(n) {
            *w = y;
        }
        if ps.eval {
            *pos = 0;
        }
        Some(n)
    }

    /// The values (or keys) of the association `name`, for `${(AA)...}`.
    fn gethparam_scan(&mut self, name: &[u8], flags: i32) -> Vec<Vec<u8>> {
        if flags & SCANPM_WANTKEYS != 0 && flags & SCANPM_WANTVALS != 0 {
            let keys = self.gethkparam(name).unwrap_or_default();
            let vals = self.gethparam(name).unwrap_or_default();
            keys.into_iter()
                .zip(vals)
                .flat_map(|(k, v)| [k, v])
                .collect()
        } else if flags & SCANPM_WANTKEYS != 0 {
            self.gethkparam(name).unwrap_or_default()
        } else {
            self.gethparam(name).unwrap_or_default()
        }
    }
}
