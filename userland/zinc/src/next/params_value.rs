//! The value side of zsh's `params.c`: subscripts (`getarg`, `getindex`),
//! reading a parameter through a [`Value`], and assigning scalars, numbers,
//! arrays and associations.

use crate::hashtable::HashTable;
use crate::math::MNumber;
use crate::options::*;
use crate::params::*;
use crate::shell::Shell;
use crate::tok::{self, INBRACK, INPAR, OUTBRACK, OUTPAR, STAR};
use crate::utils::{at, from, lossy, mb_metacharlen, sub};

impl Shell {
    /// zsh's `get_strarg`: from the delimiter at `i`, the index of the
    /// closing delimiter and its length (0 at the end of the string).
    pub(crate) fn get_strarg(&self, s: &[u8], i: usize) -> (usize, usize) {
        let (len, del) = crate::utils::mb_metacharlenconv(self, from(s, i));
        if len == 0 || i >= s.len() {
            return (i, 0);
        }
        let c = at(s, i);
        let del =
            del.unwrap_or_else(|| u32::from(if c == tok::META { at(s, i + 1) ^ 32 } else { c }));
        let mut j = i + len;
        let (del, ctok) = match del {
            0x28 => (u32::from(b')'), 0),
            0x5b => (u32::from(b']'), 0),
            0x7b => (u32::from(b'}'), 0),
            0x3c => (u32::from(b'>'), 0),
            d if d == u32::from(INPAR) => (d, OUTPAR),
            d if d == u32::from(tok::INANG) => (d, tok::OUTANG),
            d if d == u32::from(tok::INBRACE) => (d, tok::OUTBRACE),
            d if d == u32::from(INBRACK) => (d, OUTBRACK),
            d => (d, 0),
        };
        if ctok != 0 {
            while j < s.len() && at(s, j) != ctok {
                j += 1;
            }
            return (j, len);
        }
        let mut l = 0;
        while j < s.len() {
            let (l2, d2) = crate::utils::mb_metacharlenconv(self, from(s, j));
            l = l2;
            let cj = at(s, j);
            let d2 = d2.unwrap_or_else(|| {
                u32::from(if cj == tok::META {
                    at(s, j + 1) ^ 32
                } else {
                    cj
                })
            });
            if d2 == del {
                break;
            }
            j += l2.max(1);
        }
        (j, if j >= s.len() { 0 } else { l })
    }

    /// zsh's `parse_subscript`: tokenize the subscript starting at `s` as in
    /// double quotes, up to `endchar`. Returns the offset of `endchar`.
    pub(crate) fn parse_subscript(&self, s: &[u8], sub: bool, endchar: u8) -> Option<usize> {
        if s.is_empty() || at(s, 0) == endchar {
            return None;
        }
        let mut t = s.to_vec();
        tok::untokenize(&mut t);
        let mut lx = crate::lex::Lexer::new(t, self.lex_opts());
        let mut buf = Vec::with_capacity(s.len());
        let err = lx.dquote_parse(&mut buf, endchar, sub);
        if err != 0 {
            return None;
        }
        Some(buf.len())
    }

    /// zsh's `parse_subscript` returning the tokenized text too.
    pub(crate) fn parse_subscript_text(
        &self,
        s: &[u8],
        sub: bool,
        endchar: u8,
    ) -> Option<(Vec<u8>, usize)> {
        if s.is_empty() || at(s, 0) == endchar {
            return None;
        }
        let mut t = s.to_vec();
        tok::untokenize(&mut t);
        let mut lx = crate::lex::Lexer::new(t, self.lex_opts());
        let mut buf = Vec::with_capacity(s.len());
        if lx.dquote_parse(&mut buf, endchar, sub) != 0 {
            return None;
        }
        let n = buf.len();
        Some((buf, n))
    }

    /// zsh's `parsestr`: tokenize `s` as the inside of double quotes.
    pub(crate) fn parsestr(&mut self, s: &[u8]) -> Result<Vec<u8>, ()> {
        let mut t = s.to_vec();
        tok::untokenize(&mut t);
        match crate::dquote::parse_dquote_string(&t, self.lex_opts()) {
            Ok(v) => Ok(v),
            Err(err) => {
                if !self.errflag_int() {
                    if err > 32 && err < 127 {
                        self.zerr(&format!(
                            "parse error near `{}'",
                            char::from(u8::try_from(err).unwrap_or(b'?'))
                        ));
                    } else {
                        self.zerr("parse error");
                    }
                }
                Err(())
            }
        }
    }

    // ------------------------------------------------------------------
    // Scanning associations.
    // ------------------------------------------------------------------

    /// The elements of the association `r` in scan order, unset ones left
    /// out, as references a [`Value`] can hold.
    pub(crate) fn hash_elements(&mut self, r: &PmRef) -> Vec<(Vec<u8>, PmRef)> {
        if let Some(Gsu::Module(m)) = self.pm(r).map(|p| p.gsu.clone()) {
            return self.module_scan_hash(m, r);
        }
        match self.pm(r).map(|p| &p.u) {
            Some(U::Hash(t)) => t
                .iter()
                .filter(|(_, p)| p.flags & PM_UNSET == 0)
                .map(|(k, _)| (k.clone(), PmRef::Elem(Box::new(r.clone()), k.clone())))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// One element of the association `r` (zsh's `ht->getnode`).
    pub(crate) fn hash_element(&mut self, r: &PmRef, key: &[u8]) -> Option<PmRef> {
        if let Some(Gsu::Module(m)) = self.pm(r).map(|p| p.gsu.clone()) {
            return self.module_hash_getnode(m, r, key);
        }
        match self.pm(r).map(|p| &p.u) {
            Some(U::Hash(t)) if t.get(key).is_some() => {
                Some(PmRef::Elem(Box::new(r.clone()), key.to_vec()))
            }
            _ => None,
        }
    }

    /// zsh's `paramvalarr`.
    fn paramvalarr(
        &mut self,
        r: &PmRef,
        flags: i32,
        scan: Option<(&crate::pattern::Patprog, &[u8])>,
        found: &mut Option<PmRef>,
    ) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::new();
        for (key, er) in self.hash_elements(r) {
            if !out.is_empty()
                && flags & SCANPM_MATCHMANY == 0
                && flags & (SCANPM_MATCHVAL | SCANPM_MATCHKEY | SCANPM_KEYMATCH) != 0
            {
                break;
            }
            if flags & SCANPM_KEYMATCH != 0 {
                let mut t = key.clone();
                crate::pattern::tokenize(&mut t);
                crate::utils::remnulargs(&mut t);
                let Some((_, scanstr)) = scan else { continue };
                match self.patcompile(&t, 0, None) {
                    Some(prog) if self.pattry(&prog, scanstr) => {}
                    _ => continue,
                }
            } else if flags & SCANPM_MATCHKEY != 0 {
                match scan {
                    Some((prog, _)) if self.pattry(prog, &key) => {}
                    _ => continue,
                }
            }
            *found = Some(er.clone());
            if flags & SCANPM_WANTKEYS != 0 {
                out.push(key.clone());
                if flags & (SCANPM_WANTVALS | SCANPM_MATCHVAL) == 0 {
                    continue;
                }
            }
            let isarr = pm_type(self.pm_flags(&er)) & (PM_ARRAY | PM_HASHED);
            let mut v = Value::new(er);
            v.isarr = i32::try_from(isarr).unwrap_or(0);
            let val = self.getstrvalue(Some(&mut v));
            if flags & SCANPM_MATCHVAL != 0 {
                let hit = scan.is_some_and(|(prog, _)| self.pattry(prog, &val));
                if hit {
                    if flags & SCANPM_WANTVALS != 0 || flags & SCANPM_WANTKEYS == 0 {
                        out.push(val);
                    }
                } else if flags & SCANPM_WANTKEYS != 0 {
                    let _ = out.pop();
                }
            } else {
                out.push(val);
            }
        }
        out
    }

    /// zsh's `getvaluearr`.
    pub(crate) fn getvaluearr(&mut self, v: &mut Value) -> Option<Vec<Vec<u8>>> {
        self.getvaluearr_scan(v, None)
    }

    fn getvaluearr_scan(
        &mut self,
        v: &mut Value,
        scan: Option<(&crate::pattern::Patprog, &[u8])>,
    ) -> Option<Vec<Vec<u8>>> {
        if let Some(a) = &v.arr {
            return Some(a.clone());
        }
        let flags = self.pm_flags(&v.pm);
        if pm_type(flags) == PM_ARRAY {
            let a = self.getafn(&v.pm);
            v.arr = Some(a.clone());
            Some(a)
        } else if pm_type(flags) == PM_HASHED {
            let mut found = None;
            let pm = v.pm.clone();
            let a = self.paramvalarr(&pm, v.isarr, scan, &mut found);
            v.found = found;
            v.start = 0;
            v.end = i64::try_from(a.len()).unwrap_or(0) + 1;
            v.arr = Some(a.clone());
            Some(a)
        } else {
            None
        }
    }

    /// zsh's `issetvar`.
    pub(crate) fn issetvar(&mut self, name: &[u8]) -> bool {
        let mut i = 0;
        let Some(mut v) = self.getvalue(name, &mut i, 1) else {
            return false;
        };
        if i < name.len() {
            return false;
        }
        if v.isarr & !SCANPM_ARRONLY != 0 {
            return v.end > 1;
        }
        let slice = v.start != 0 || v.end != -1;
        let flags = self.pm_flags(&v.pm);
        if pm_type(flags) != PM_ARRAY || !slice {
            return !slice && flags & PM_UNSET == 0;
        }
        if v.end == 0 {
            return false;
        }
        let Some(arr) = self.getvaluearr(&mut v) else {
            return false;
        };
        i64::try_from(arr.len()).unwrap_or(0) >= v.end.abs()
    }

    // ------------------------------------------------------------------
    // Subscripts.
    // ------------------------------------------------------------------

    /// zsh's `getarg`. `*str` is the index in `s` of the subscript, left
    /// at the character ending it.
    #[expect(clippy::too_many_lines, reason = "zsh's getarg is one procedure")]
    #[expect(clippy::too_many_arguments, reason = "as zsh's")]
    fn getarg(
        &mut self,
        s: &mut [u8],
        str_i: &mut usize,
        inv: &mut bool,
        v: &mut Value,
        a2: bool,
        w: &mut i64,
        prevcharlen: &mut usize,
        nextcharlen: &mut usize,
        flags: i32,
    ) -> i64 {
        let (mut hasbeg, mut word, mut rev, mut ind, mut down, mut keymatch) =
            (false, false, false, false, false, false);
        let mut needtok = false;
        let mut inpar = 0;
        let mut sep: Option<Vec<u8>> = None;
        let mut num: i64 = 1;
        let mut beg: i64 = 0;
        let mut r: i64 = 0;
        let mut quote_arg = false;
        let ishash = !self.isset(EXECOPT) || pm_type(self.pm_flags(&v.pm)) == PM_HASHED;
        *prevcharlen = 1;
        *nextcharlen = 1;
        let mut i = *str_i;
        if matches!(at(s, i), b'(' | INPAR) {
            let mut escapes = false;
            i += 1;
            while !matches!(at(s, i), b')' | OUTPAR) && i < s.len() {
                match at(s, i) {
                    b'r' => {
                        rev = true;
                        keymatch = false;
                        down = false;
                        ind = false;
                    }
                    b'R' => {
                        rev = true;
                        down = true;
                        keymatch = false;
                        ind = false;
                    }
                    b'k' => {
                        keymatch = ishash;
                        rev = true;
                        down = false;
                        ind = false;
                    }
                    b'K' => {
                        keymatch = ishash;
                        rev = true;
                        down = true;
                        ind = false;
                    }
                    b'i' => {
                        rev = true;
                        ind = true;
                        down = false;
                        keymatch = false;
                    }
                    b'I' => {
                        rev = true;
                        ind = true;
                        down = true;
                        keymatch = false;
                    }
                    b'w' => word = true,
                    b'f' => {
                        word = true;
                        sep = Some(b"\n".to_vec());
                    }
                    b'e' => quote_arg = true,
                    b'n' | b'b' | b's' => {
                        let flag = at(s, i);
                        let (t, arglen) = self.get_strarg(s, i + 1);
                        if t >= s.len() {
                            // flagerr
                            num = 1;
                            word = false;
                            rev = false;
                            ind = false;
                            down = false;
                            keymatch = false;
                            sep = None;
                            i = str_i.wrapping_sub(1);
                            break;
                        }
                        let inner = sub(s, i + 1 + arglen, t).to_vec();
                        match flag {
                            b'n' => {
                                num = self.mathevalarg(&inner).0;
                                if num == 0 {
                                    num = 1;
                                }
                            }
                            b'b' => {
                                hasbeg = true;
                                beg = self.mathevalarg(&inner).0;
                                if beg > 0 {
                                    beg -= 1;
                                }
                            }
                            _ => {
                                sep = Some(if escapes {
                                    self.getkeystring(&inner, crate::utils::GETKEYS_SEP).0
                                } else {
                                    inner
                                });
                            }
                        }
                        i = t + arglen - 1;
                    }
                    b'p' => escapes = true,
                    _ => {
                        num = 1;
                        word = false;
                        rev = false;
                        ind = false;
                        down = false;
                        keymatch = false;
                        sep = None;
                        i = str_i.wrapping_sub(1);
                        break;
                    }
                }
                i += 1;
            }
            if i != *str_i {
                i = i.wrapping_add(1);
            }
        }
        if num < 0 {
            down = !down;
            num = -num;
        }
        if v.isarr & SCANPM_WANTKEYS != 0 {
            *inv = ind || v.isarr & SCANPM_WANTVALS == 0;
        } else if v.isarr & SCANPM_WANTVALS != 0 {
            *inv = false;
        } else {
            if v.isarr != 0 {
                if ind {
                    v.isarr |= SCANPM_WANTKEYS;
                    v.isarr &= !SCANPM_WANTVALS;
                } else if rev {
                    v.isarr |= SCANPM_WANTVALS;
                }
                if !down && keymatch && ishash {
                    v.isarr &= !SCANPM_MATCHMANY;
                }
            }
            *inv = ind;
        }
        let start = i;
        let mut t = i;
        let mut depth = 0;
        let mut c;
        loop {
            c = at(s, t);
            if t >= s.len() {
                c = 0;
                break;
            }
            if !((c != OUTBRACK && (ishash || c != b',')) || depth != 0 || inpar != 0) {
                break;
            }
            if tok::is_null(c) {
                let c1 = at(s, t + 1);
                if matches!(c1, b'[' | b']' | b'(' | b')' | b'{' | b'}') {
                    if ishash
                        && depth != 0
                        && let Some(slot) = s.get_mut(t)
                    {
                        *slot = tok::detok(c);
                    }
                    needtok = true;
                    t += 1;
                } else if c1 != b'"'
                    && let Some(slot) = s.get_mut(t)
                {
                    *slot = tok::detok(c);
                }
                t += 1;
                continue;
            }
            if c == b'[' || c == INBRACK {
                depth += 1;
            } else if c == b']' || c == OUTBRACK {
                depth -= 1;
            }
            if c == b'(' || c == INPAR {
                inpar += 1;
            } else if c == b')' || c == OUTPAR {
                inpar -= 1;
            }
            if self.ispecial(c) {
                needtok = true;
            }
            t += 1;
        }
        if c == 0 {
            return 0;
        }
        *str_i = t;
        let tt = t;
        if !self.isset(EXECOPT) {
            return 0;
        }
        let mut sarg = sub(s, start, t).to_vec();
        if ishash && (keymatch || !rev) {
            crate::utils::remnulargs(&mut sarg);
            if sarg == [tok::NULARG] {
                sarg.clear();
            }
        }
        if needtok {
            match self.parsestr(&sarg) {
                Ok(p) => sarg = p,
                Err(()) => return 0,
            }
            sarg = self.singsub(&sarg);
        } else if rev {
            crate::utils::remnulargs(&mut sarg);
            if sarg == [tok::NULARG] {
                sarg.clear();
            }
        }
        let at_tt_comma = at(s, tt) == b',';
        if !rev {
            if ishash {
                let pm = v.pm.clone();
                if !self.has_hash(&pm)
                    && !matches!(self.pm(&pm).map(|p| &p.gsu), Some(Gsu::Module(_)))
                {
                    if flags & SCANPM_CHECKING != 0 {
                        return 0;
                    }
                    let mut pmr = pm.clone();
                    self.sethfn(&mut pmr, Some(HashTable::new(17)));
                }
                tok::untokenize(&mut sarg);
                match self.hash_element(&pm, &sarg) {
                    Some(er) => v.pm = er,
                    None => {
                        v.pm = self.create_hash_element(&pm, &sarg);
                    }
                }
                v.isarr = if *inv { SCANPM_WANTINDEX } else { 0 };
                v.start = 0;
                *inv = false;
                v.end = -1;
                *w = -1;
                r = i64::from(self.isset(KSHARRAYS));
            } else {
                r = self.mathevalarg(&sarg).0;
                if self.isset(KSHARRAYS) && r >= 0 {
                    r += 1;
                }
            }
            if word && v.isarr == 0 {
                let val = self.getstrvalue(Some(v));
                let n = i64::try_from(self.wordcount(&val, sep.as_deref(), 0)).unwrap_or(0);
                if r < 0 {
                    r += n + 1;
                }
                if r < 1 {
                    r = 1;
                }
                if r > n {
                    r = n;
                }
                if val.is_empty() {
                    return 0;
                }
                let mut p = 0usize;
                let mut d;
                loop {
                    d = self.findword(&val, &mut p, sep.as_deref());
                    if d.is_none() {
                        break;
                    }
                    r -= 1;
                    if r == 0 {
                        break;
                    }
                }
                let Some(d) = d else { return 0 };
                if !a2 && !at_tt_comma {
                    *w = i64::try_from(p).unwrap_or(0);
                }
                return i64::try_from(if a2 { p } else { d + 1 }).unwrap_or(0);
            } else if v.isarr == 0 && !word {
                let val = self.getstrvalue(Some(v));
                if r > 0 {
                    let mut nchars = r;
                    let mut t = 0usize;
                    let mut lastcharlen = 1;
                    while nchars > 0 && t < val.len() {
                        lastcharlen = mb_metacharlen(self, from(&val, t)).max(1);
                        t += lastcharlen;
                        nchars -= 1;
                    }
                    r = i64::try_from(t).unwrap_or(0) + nchars;
                    if nchars == 0 {
                        *prevcharlen = lastcharlen;
                    }
                    if t < val.len() {
                        *nextcharlen = mb_metacharlen(self, from(&val, t)).max(1);
                    }
                } else if r == 0 {
                    *prevcharlen = 0;
                    if !val.is_empty() {
                        *nextcharlen = mb_metacharlen(self, &val).max(1);
                    }
                } else {
                    let nchars =
                        i64::try_from(crate::utils::mb_metastrlen0(self, &val)).unwrap_or(0) + r;
                    if nchars < 0 {
                        r -= i64::try_from(val.len()).unwrap_or(0);
                    } else {
                        let mut left = nchars;
                        let mut t = 0usize;
                        let mut lastcharlen = 1;
                        while left > 0 && t < val.len() {
                            lastcharlen = mb_metacharlen(self, from(&val, t)).max(1);
                            t += lastcharlen;
                            left -= 1;
                        }
                        r = -i64::try_from(val.len() - t.min(val.len())).unwrap_or(0);
                        *prevcharlen = lastcharlen;
                        if t < val.len() {
                            *nextcharlen = mb_metacharlen(self, from(&val, t)).max(1);
                        }
                    }
                }
            }
        } else {
            if v.isarr == 0 && !word && !quote_arg {
                let l = sarg.len();
                if a2 {
                    if l == 0 || at(&sarg, 0) != b'*' {
                        sarg.insert(0, b'*');
                    }
                } else if l == 0 || at(&sarg, l - 1) != b'*' || (l > 1 && at(&sarg, l - 2) == b'\\')
                {
                    sarg.push(b'*');
                }
            }
            let pprog = if keymatch {
                None
            } else {
                if quote_arg {
                    tok::untokenize(&mut sarg);
                    if v.isarr == 0 && !word {
                        if a2 {
                            sarg.insert(0, STAR);
                        } else {
                            sarg.push(STAR);
                        }
                    }
                } else {
                    crate::pattern::tokenize(&mut sarg);
                }
                crate::utils::remnulargs(&mut sarg);
                self.patcompile(&sarg, 0, None)
            };
            if v.isarr != 0 {
                let ta: Option<Vec<Vec<u8>>>;
                if ishash {
                    if keymatch {
                        v.isarr |= SCANPM_KEYMATCH;
                    } else {
                        if pprog.is_none() {
                            return 1;
                        }
                        if ind {
                            v.isarr |= SCANPM_MATCHKEY;
                        } else {
                            v.isarr |= SCANPM_MATCHVAL;
                        }
                    }
                    if down {
                        v.isarr |= SCANPM_MATCHMANY;
                    }
                    let dummy = crate::pattern::Patprog::empty();
                    let prog_ref = pprog.as_ref().unwrap_or(&dummy);
                    let got = self.getvaluearr_scan(v, Some((prog_ref, &sarg)));
                    if let Some(a) = &got
                        && (!a.is_empty()
                            || (v.isarr & SCANPM_MATCHMANY != 0
                                && v.isarr & (SCANPM_MATCHKEY | SCANPM_MATCHVAL | SCANPM_KEYMATCH)
                                    != 0))
                    {
                        *inv = v.flags & VALFLAG_INV != 0;
                        *w = v.end;
                        return 1;
                    }
                    ta = got;
                } else {
                    ta = Some(self.getarrvalue(Some(v)));
                }
                let Some(ta) = ta.filter(|a| !a.is_empty()) else {
                    return i64::from(!down);
                };
                let len = i64::try_from(ta.len()).unwrap_or(0);
                if beg < 0 {
                    beg += len;
                }
                if down {
                    if beg < 0 {
                        return 0;
                    }
                } else if beg >= len {
                    return len + 1;
                }
                if beg >= 0 && beg < len {
                    if down {
                        if !hasbeg {
                            beg = len - 1;
                        }
                        let mut k = beg;
                        r = 1 + beg;
                        while k >= 0 {
                            let e = ta
                                .get(usize::try_from(k).unwrap_or(0))
                                .cloned()
                                .unwrap_or_default();
                            if pprog.as_ref().is_some_and(|p| self.pattry(p, &e)) {
                                num -= 1;
                                if num == 0 {
                                    return r;
                                }
                            }
                            r -= 1;
                            k -= 1;
                        }
                    } else {
                        let mut k = beg;
                        r = 1 + beg;
                        while k < len {
                            let e = ta
                                .get(usize::try_from(k).unwrap_or(0))
                                .cloned()
                                .unwrap_or_default();
                            if pprog.as_ref().is_some_and(|p| self.pattry(p, &e)) {
                                num -= 1;
                                if num == 0 {
                                    return r;
                                }
                            }
                            r += 1;
                            k += 1;
                        }
                    }
                }
            } else if word {
                let d = self.getstrvalue(Some(v));
                let ta = self.sepsplit(&d, sep.as_deref(), true);
                let len = i64::try_from(ta.len()).unwrap_or(0);
                if beg < 0 {
                    beg += len;
                }
                if down {
                    if beg < 0 {
                        return 0;
                    }
                } else if beg >= len {
                    return len + 1;
                }
                if beg >= 0 && beg < len {
                    let matches = |sh: &Shell, k: i64| {
                        let e = ta
                            .get(usize::try_from(k).unwrap_or(0))
                            .cloned()
                            .unwrap_or_default();
                        pprog.as_ref().is_some_and(|p| sh.pattry_noref(p, &e))
                    };
                    if down {
                        if !hasbeg {
                            beg = len - 1;
                        }
                        let mut k = beg;
                        r = 1 + beg;
                        loop {
                            if k < 0 {
                                return 0;
                            }
                            if matches(self, k) {
                                num -= 1;
                                if num == 0 {
                                    break;
                                }
                            }
                            k -= 1;
                            r -= 1;
                        }
                    } else {
                        let mut k = beg;
                        r = 1 + beg;
                        loop {
                            if k >= len {
                                return 0;
                            }
                            if matches(self, k) {
                                num -= 1;
                                if num == 0 {
                                    break;
                                }
                            }
                            k += 1;
                            r += 1;
                        }
                    }
                }
                if a2 {
                    r += 1;
                }
                let mut p = 0usize;
                let mut idx = 0usize;
                while let Some(t) = self.findword(&d, &mut p, sep.as_deref()) {
                    if t >= d.len() {
                        break;
                    }
                    r -= 1;
                    if r == 0 {
                        let rr = i64::try_from(t).unwrap_or(0) + if a2 { -1 } else { 1 };
                        if !a2 && !at_tt_comma {
                            *w = rr + i64::try_from(ta.get(idx).map_or(0, Vec::len)).unwrap_or(0)
                                - 1;
                        }
                        return rr;
                    }
                    idx += 1;
                }
                return if a2 { -1 } else { 0 };
            } else {
                let d = self.getstrvalue(Some(v));
                if d.is_empty() {
                    return 0;
                }
                let len = i64::try_from(crate::utils::mb_metastrlen0(self, &d)).unwrap_or(0);
                let slen = i64::try_from(d.len()).unwrap_or(0);
                if beg < 0 {
                    beg += len;
                }
                let de = d.len();
                let pat_try = |sh: &Shell, text: &[u8]| {
                    pprog.as_ref().is_some_and(|p| sh.pattry_noref(p, text))
                };
                if beg >= 0 && beg < len {
                    if a2 {
                        if down {
                            let mut nmatches = 0;
                            let mut lastpos: Option<usize> = None;
                            if !hasbeg {
                                beg = len;
                            }
                            let mut t = 0usize;
                            let mut rr = 0;
                            while rr <= beg {
                                if pat_try(self, sub(&d, 0, t)) {
                                    nmatches += 1;
                                    lastpos = Some(t);
                                }
                                if t == de {
                                    break;
                                }
                                t += mb_metacharlen(self, from(&d, t)).max(1);
                                rr += 1;
                            }
                            if nmatches >= num {
                                if num > 1 {
                                    let mut left = nmatches - num;
                                    let mut t = 0usize;
                                    loop {
                                        if pat_try(self, sub(&d, 0, t)) {
                                            if left == 0 {
                                                lastpos = Some(t);
                                                break;
                                            }
                                            left -= 1;
                                        }
                                        if t >= de {
                                            break;
                                        }
                                        t += mb_metacharlen(self, from(&d, t)).max(1);
                                    }
                                }
                                return i64::try_from(lastpos.unwrap_or(0)).unwrap_or(0);
                            }
                        } else {
                            let mut t = 0usize;
                            while beg > 0 && t <= de {
                                t += mb_metacharlen(self, from(&d, t)).max(1);
                                beg -= 1;
                            }
                            loop {
                                if pat_try(self, sub(&d, 0, t)) {
                                    num -= 1;
                                    if num == 0 {
                                        return i64::try_from(t).unwrap_or(0);
                                    }
                                }
                                if t >= de {
                                    break;
                                }
                                t += mb_metacharlen(self, from(&d, t)).max(1);
                            }
                        }
                    } else if down {
                        let mut nmatches = 0;
                        let mut lastpos: Option<usize> = None;
                        if !hasbeg {
                            beg = len;
                        }
                        let mut t = 0usize;
                        let mut rr = 0;
                        while rr <= beg {
                            if pat_try(self, from(&d, t)) {
                                nmatches += 1;
                                lastpos = Some(t);
                            }
                            if t == de {
                                break;
                            }
                            t += mb_metacharlen(self, from(&d, t)).max(1);
                            rr += 1;
                        }
                        if nmatches >= num {
                            if num > 1 {
                                let mut left = nmatches - num;
                                let mut t = 0usize;
                                loop {
                                    if pat_try(self, from(&d, t)) {
                                        if left == 0 {
                                            lastpos = Some(t);
                                            break;
                                        }
                                        left -= 1;
                                    }
                                    if t >= de {
                                        break;
                                    }
                                    t += mb_metacharlen(self, from(&d, t)).max(1);
                                }
                            }
                            let mut lp = lastpos.unwrap_or(0);
                            let lcl = mb_metacharlen(self, from(&d, lp)).max(1);
                            lp += lcl;
                            *prevcharlen = lcl;
                            *nextcharlen = mb_metacharlen(self, from(&d, lp)).max(1);
                            return i64::try_from(lp).unwrap_or(0);
                        }
                        let mut rr = beg + 1;
                        let mut t = beg;
                        while t >= 0 {
                            if pat_try(self, from(&d, usize::try_from(t).unwrap_or(0))) {
                                num -= 1;
                                if num == 0 {
                                    return rr;
                                }
                            }
                            rr -= 1;
                            t -= 1;
                        }
                    } else {
                        let mut t = 0usize;
                        while beg > 0 && t <= de {
                            t += mb_metacharlen(self, from(&d, t)).max(1);
                            beg -= 1;
                        }
                        loop {
                            if pat_try(self, from(&d, t)) {
                                num -= 1;
                                if num == 0 {
                                    let lcl = mb_metacharlen(self, from(&d, t)).max(1);
                                    let t2 = t + lcl;
                                    *prevcharlen = lcl;
                                    *nextcharlen = mb_metacharlen(self, from(&d, t2)).max(1);
                                    return i64::try_from(t2).unwrap_or(0);
                                }
                            }
                            if t >= de {
                                break;
                            }
                            t += mb_metacharlen(self, from(&d, t)).max(1);
                        }
                    }
                }
                return if down { 0 } else { slen + 1 };
            }
        }
        r
    }

    /// Create the element `key` of the association `hash` (unset).
    pub(crate) fn create_hash_element(&mut self, hash: &PmRef, key: &[u8]) -> PmRef {
        let mut hr = hash.clone();
        let _ = self.with_pm(&mut hr, |p| {
            if let U::Hash(t) = &mut p.u
                && !t.contains(key)
            {
                let _ = t.insert(key.to_vec(), Param::new(PM_SCALAR | PM_UNSET | PM_HASHELEM));
            }
        });
        PmRef::Elem(Box::new(hash.clone()), key.to_vec())
    }

    /// zsh's `getindex`. `s` holds the text from the `[` at `*pptr`.
    #[expect(clippy::too_many_lines, reason = "zsh's getindex is one procedure")]
    pub(crate) fn getindex(
        &mut self,
        s: &mut [u8],
        pptr: &mut usize,
        v: &mut Value,
        flags: i32,
    ) -> i32 {
        let open = *pptr;
        if let Some(slot) = s.get_mut(open) {
            *slot = b'[';
        }
        let parsed =
            self.parse_subscript_text(from(s, open + 1), flags & SCANPM_DQUOTED != 0, b']');
        let close = match &parsed {
            Some((text, n)) => {
                // Put the tokenized subscript back in place.
                let end = open + 1 + n;
                for (k, &b) in text.iter().enumerate() {
                    if let Some(slot) = s.get_mut(open + 1 + k) {
                        *slot = b;
                    }
                }
                Some(end)
            }
            None => None,
        };
        let mut tbrack = open + 1;
        let stop = close.unwrap_or(s.len());
        while tbrack < s.len() && tbrack != stop {
            let c = at(s, tbrack);
            if tok::is_null(c) {
                tbrack += 1;
                if tbrack >= s.len() {
                    break;
                }
                tbrack += 1;
                continue;
            }
            if tok::is_tok(c)
                && let Some(slot) = s.get_mut(tbrack)
            {
                *slot = tok::detok(c);
            }
            tbrack += 1;
        }
        if tbrack < s.len() && close.is_some() {
            if let Some(slot) = s.get_mut(tbrack) {
                *slot = OUTBRACK;
            }
        } else {
            self.zerr("invalid subscript");
            *pptr = tbrack;
            return 1;
        }
        let mut si = open + 1;
        if matches!(at(s, si), b'*' | b'@') && si + 1 == tbrack {
            let unset_value = self.is_unset_value(v);
            if (v.isarr != 0 || unset_value) && at(s, si) == b'@' {
                v.isarr |= SCANPM_ISVAR_AT;
            }
            v.start = 0;
            v.end = -1;
            si += 2;
        } else {
            let mut we: i64 = 0;
            let mut dummy: i64 = 0;
            let (mut startprevlen, mut startnextlen) = (1usize, 1usize);
            let mut inv = false;
            let mut start = self.getarg(
                s,
                &mut si,
                &mut inv,
                v,
                false,
                &mut we,
                &mut startprevlen,
                &mut startnextlen,
                flags,
            );
            if inv {
                if v.isarr == 0 && start != 0 {
                    let t = self.getstrvalue(Some(v));
                    if start > 0 {
                        let target = usize::try_from(start)
                            .unwrap_or(0)
                            .saturating_sub(startprevlen);
                        let mut nstart: i64 = 0;
                        let mut p = 0usize;
                        let mut hit = false;
                        while p < t.len() {
                            p += mb_metacharlen(self, from(&t, p)).max(1);
                            if p < target {
                                nstart += 1;
                            } else {
                                if p == target {
                                    nstart += 1;
                                } else {
                                    p = target;
                                }
                                hit = true;
                                break;
                            }
                        }
                        let _ = hit;
                        start = nstart + i64::try_from(target).unwrap_or(0)
                            - i64::try_from(p).unwrap_or(0)
                            + 1;
                    } else {
                        let startoff = start + i64::try_from(t.len()).unwrap_or(0);
                        if startoff < 0 {
                            start = startoff;
                        } else {
                            let mut p = 0usize;
                            let lim = usize::try_from(startoff).unwrap_or(0);
                            while p < lim {
                                p += mb_metacharlen(self, from(&t, p)).max(1);
                            }
                            start = -i64::try_from(crate::utils::mb_metastrlen0(self, from(&t, p)))
                                .unwrap_or(0);
                        }
                    }
                }
                if start > 0 && (self.isset(KSHARRAYS) || self.pm_flags(&v.pm) & PM_HASHED != 0) {
                    start -= 1;
                }
                if v.isarr != SCANPM_WANTINDEX {
                    v.flags |= VALFLAG_INV;
                    v.isarr = 0;
                    v.start = start;
                    v.end = start + 1;
                }
                if at(s, si) == b',' {
                    self.zerr("invalid subscript");
                    if let Some(slot) = s.get_mut(tbrack) {
                        *slot = b']';
                    }
                    *pptr = tbrack + 1;
                    return 1;
                }
                if si == tbrack {
                    si += 1;
                }
            } else {
                let mut com = at(s, si) == b',';
                let end = if com {
                    si += 1;
                    let (mut p1, mut p2) = (0usize, 0usize);
                    let mut inv2 = false;
                    self.getarg(
                        s, &mut si, &mut inv2, v, true, &mut dummy, &mut p1, &mut p2, flags,
                    )
                } else if we != 0 {
                    we
                } else {
                    start
                };
                if start != end {
                    com = true;
                }
                let mut end = end;
                if start > 0 {
                    start -= i64::try_from(startprevlen).unwrap_or(0);
                } else if start == 0 && end == 0 {
                    if self.isset(KSHZEROSUBSCRIPT) {
                        end = i64::try_from(startnextlen).unwrap_or(0);
                    } else {
                        v.flags |= VALFLAG_EMPTY;
                        start = -1;
                        com = true;
                    }
                }
                if si == tbrack {
                    si += 1;
                    if v.isarr != 0
                        && !com
                        && (v.isarr & SCANPM_MATCHMANY == 0
                            || v.isarr & (SCANPM_MATCHKEY | SCANPM_MATCHVAL | SCANPM_KEYMATCH) == 0)
                    {
                        v.isarr = 0;
                    }
                    v.start = start;
                    v.end = end;
                } else {
                    si = *pptr;
                }
            }
        }
        if let Some(slot) = s.get_mut(tbrack) {
            *slot = b']';
        }
        *pptr = si;
        0
    }

    /// `IS_UNSET_VALUE(v)`.
    fn is_unset_value(&self, v: &Value) -> bool {
        match self.pm(&v.pm) {
            None => true,
            Some(p) => {
                p.flags & PM_UNSET != 0
                    || self.pm_name(&v.pm).is_empty() && !matches!(v.pm, PmRef::Argv)
            }
        }
    }

    /// zsh's `getvalue`.
    pub(crate) fn getvalue(&mut self, s: &[u8], i: &mut usize, bracks: i32) -> Option<Value> {
        let mut buf = s.to_vec();
        self.fetchvalue(&mut buf, i, bracks, 0)
    }

    /// zsh's `fetchvalue`: the parameter named at `*pptr` in `s`, with its
    /// subscript when `bracks` > 0. `*pptr` is left after what was read.
    pub(crate) fn fetchvalue(
        &mut self,
        s: &mut [u8],
        pptr: &mut usize,
        bracks: i32,
        flags: i32,
    ) -> Option<Value> {
        let t = *pptr;
        let mut i = t;
        let c = at(s, i);
        let mut ppar: i64 = 0;
        if c.is_ascii_digit() {
            if bracks >= 0 {
                let (v, end) = crate::utils::zstrtol(from(s, i), 10);
                ppar = v;
                i += end;
            } else {
                ppar = i64::from(c - b'0');
                i += 1;
            }
        } else {
            let ie = self.itype_end(s, i, crate::utils::IIDENT, false);
            if ie != i {
                i = ie;
            } else if c == tok::QUEST {
                if let Some(x) = s.get_mut(i) {
                    *x = b'?';
                }
                i += 1;
            } else if c == tok::POUND {
                if let Some(x) = s.get_mut(i) {
                    *x = b'#';
                }
                i += 1;
            } else if c == tok::STRING || c == tok::QSTRING {
                if let Some(x) = s.get_mut(i) {
                    *x = b'$';
                }
                i += 1;
            } else if c == STAR {
                if let Some(x) = s.get_mut(i) {
                    *x = b'*';
                }
                i += 1;
            } else if c == b'-' || c == tok::DASH {
                if let Some(x) = s.get_mut(i) {
                    *x = b'-';
                }
                i += 1;
            } else if matches!(c, b'#' | b'?' | b'$' | b'!' | b'@' | b'*') {
                i += 1;
            } else {
                return None;
            }
        }
        let mut v;
        if ppar != 0 {
            v = Value::new(PmRef::Argv);
            v.start = ppar - 1;
            v.end = ppar;
        } else {
            let name = sub(s, t, i).to_vec();
            let isvarat = name == b"@";
            let lookup: &[u8] = if at(&name, 0) == b'0' { b"0" } else { &name };
            let lookup = lookup.to_vec();
            *pptr = i;
            let pmflags = self.getparamnode(&lookup).map(|p| p.flags)?;
            if pmflags & PM_UNSET != 0 && pmflags & PM_DECLARED == 0 {
                return None;
            }
            v = Value::new(PmRef::Name(lookup.clone()));
            if pm_type(pmflags) & (PM_ARRAY | PM_HASHED) != 0 {
                v.isarr = flags | if isvarat { SCANPM_ISVAR_AT } else { 0 };
                if v.isarr == 0 {
                    v.isarr = SCANPM_ARRONLY;
                }
            }
            if bracks > 0 && matches!(at(s, i), b'[' | INBRACK) {
                if self.getindex(s, &mut i, &mut v, flags) != 0 {
                    *pptr = i;
                    return Some(v);
                }
            } else if flags & SCANPM_ASSIGNING == 0
                && v.isarr != 0
                && self.itype_end(&name, 0, crate::utils::IIDENT, true) != 0
                && self.isset(KSHARRAYS)
            {
                v.end = 1;
                v.isarr = 0;
            }
        }
        if bracks == 0 && i < s.len() {
            return None;
        }
        *pptr = i;
        Some(v)
    }

    /// zsh's `getstrvalue`.
    #[expect(clippy::too_many_lines, reason = "zsh's getstrvalue")]
    pub(crate) fn getstrvalue(&mut self, v: Option<&mut Value>) -> Vec<u8> {
        let Some(v) = v else { return Vec::new() };
        let flags = self.pm_flags(&v.pm);
        if v.flags & VALFLAG_INV != 0 && flags & PM_HASHED == 0 {
            return v.start.to_string().into_bytes();
        }
        let mut s: Vec<u8> = match pm_type(flags) {
            PM_HASHED | PM_ARRAY => {
                if pm_type(flags) == PM_HASHED && v.isarr == 0 && self.emulation_is(EMULATE_KSH) {
                    let mut idx = b"[0]".to_vec();
                    let mut p = 0;
                    if self.getindex(&mut idx, &mut p, v, 0) == 0 {
                        return self.getstrvalue(Some(v));
                    }
                    return idx;
                }
                let ss = self.getvaluearr(v).unwrap_or_default();
                if v.isarr != 0 {
                    return self.sepjoin(&ss, None);
                }
                if v.start < 0 {
                    v.start += i64::try_from(ss.len()).unwrap_or(0);
                }
                return if v.start < 0 {
                    Vec::new()
                } else {
                    ss.get(usize::try_from(v.start).unwrap_or(usize::MAX))
                        .cloned()
                        .unwrap_or_default()
                };
            }
            PM_INTEGER => {
                let n = self.getifn(&v.pm);
                let base = self.pm(&v.pm).map_or(0, |p| p.base);
                self.convbase(n, base)
            }
            PM_EFLOAT | PM_FFLOAT => {
                let f = self.getffn(&v.pm);
                let base = self.pm(&v.pm).map_or(0, |p| p.base);
                convfloat(f, base, flags)
            }
            _ => self.getsfn(&v.pm),
        };
        if v.flags & VALFLAG_SUBST != 0 {
            let width = self.pm(&v.pm).map_or(0, |p| p.width);
            if flags & (PM_LEFT | PM_RIGHT_B | PM_RIGHT_Z) != 0 {
                let fwidth = if width != 0 {
                    usize::try_from(width).unwrap_or(0)
                } else {
                    crate::utils::mb_metastrlen0(self, &s)
                };
                match flags & (PM_LEFT | PM_RIGHT_B | PM_RIGHT_Z) {
                    x if x == PM_LEFT || x == PM_LEFT | PM_RIGHT_Z => {
                        let mut t = 0;
                        if flags & PM_RIGHT_Z != 0 {
                            while at(&s, t) == b'0' {
                                t += 1;
                            }
                        } else {
                            while self.iblank(at(&s, t)) && t < s.len() {
                                t += 1;
                            }
                        }
                        let mut tend = t;
                        let mut t0 = 0;
                        while t0 < fwidth && tend < s.len() {
                            tend += mb_metacharlen(self, from(&s, tend)).max(1);
                            t0 += 1;
                        }
                        let pad = fwidth - t0;
                        let mut out = sub(&s, t, tend).to_vec();
                        out.extend(std::iter::repeat_n(b' ', pad));
                        s = out;
                    }
                    _ => {
                        let charlen = crate::utils::mb_metastrlen0(self, &s);
                        if charlen < fwidth {
                            let mut zero = true;
                            let mut valprefend = 0usize;
                            if flags & PM_RIGHT_Z != 0 {
                                let mut t = 0;
                                while self.iblank(at(&s, t)) && t < s.len() {
                                    t += 1;
                                }
                                if flags & (PM_INTEGER | PM_EFLOAT | PM_FFLOAT) != 0
                                    && at(&s, t) == b'-'
                                {
                                    t += 1;
                                }
                                if flags & PM_INTEGER != 0 {
                                    if self.isset(CBASES)
                                        && at(&s, t) == b'0'
                                        && at(&s, t + 1) == b'x'
                                    {
                                        t += 2;
                                    } else if let Some(h) =
                                        from(&s, t).iter().position(|&c| c == b'#')
                                    {
                                        t += h + 1;
                                    }
                                }
                                valprefend = t;
                                if t >= s.len() {
                                    zero = false;
                                } else if flags & (PM_INTEGER | PM_EFLOAT | PM_FFLOAT) != 0 {
                                } else if !at(&s, t).is_ascii_digit() {
                                    zero = false;
                                }
                            }
                            let pad = fwidth - charlen;
                            let fill = if flags & PM_RIGHT_B != 0 || !zero {
                                b' '
                            } else {
                                b'0'
                            };
                            let mut out = sub(&s, 0, valprefend).to_vec();
                            out.extend(std::iter::repeat_n(fill, pad));
                            out.extend_from_slice(from(&s, valprefend));
                            s = out;
                        } else {
                            let mut skip = charlen - fwidth;
                            let mut t = 0;
                            while skip > 0 {
                                t += mb_metacharlen(self, from(&s, t)).max(1);
                                skip -= 1;
                            }
                            s = from(&s, t).to_vec();
                        }
                    }
                }
            }
            match flags & (PM_LOWER | PM_UPPER) {
                PM_LOWER => s = self.casemodify(&s, crate::hist::CASMOD_LOWER),
                PM_UPPER => s = self.casemodify(&s, crate::hist::CASMOD_UPPER),
                _ => {}
            }
        }
        if v.start == 0 && v.end == -1 {
            return s;
        }
        let len = i64::try_from(s.len()).unwrap_or(0);
        if v.start < 0 {
            v.start += len;
            if v.start < 0 {
                v.start = 0;
            }
        }
        if v.end < 0 {
            v.end += len;
            if v.end >= 0 {
                let e = usize::try_from(v.end).unwrap_or(0);
                if e < s.len() {
                    v.end += i64::try_from(mb_metacharlen(self, from(&s, e)).max(1)).unwrap_or(1);
                }
            }
        }
        if v.start > len {
            return Vec::new();
        }
        let st = usize::try_from(v.start).unwrap_or(0);
        if v.end <= v.start {
            return Vec::new();
        }
        let en = usize::try_from(v.end).unwrap_or(0).min(s.len());
        sub(&s, st, en).to_vec()
    }

    /// zsh's `getarrvalue`.
    pub(crate) fn getarrvalue(&mut self, v: Option<&mut Value>) -> Vec<Vec<u8>> {
        let Some(v) = v else {
            return vec![Vec::new()];
        };
        if self.is_unset_value(v) {
            return Vec::new();
        }
        if v.flags & VALFLAG_INV != 0 {
            return vec![v.start.to_string().into_bytes()];
        }
        let s = self.getvaluearr(v).unwrap_or_default();
        if v.start == 0 && v.end == -1 {
            return s;
        }
        let n = i64::try_from(s.len()).unwrap_or(0);
        if v.start < 0 {
            v.start += n;
        }
        if v.end < 0 {
            v.end += n + 1;
        }
        if v.end <= v.start {
            Vec::new()
        } else if v.start < 0 {
            vec![Vec::new()]
        } else if n <= v.start {
            // Keep $ary[i,j] consistent for indexes past the end: empty
            // elements, as many as the range asks for.
            let k = usize::try_from(v.end - (v.start + 1)).unwrap_or(0).min(1);
            vec![Vec::new(); k]
        } else {
            let st = usize::try_from(v.start).unwrap_or(0);
            let en = usize::try_from(v.end).unwrap_or(0).min(s.len());
            s.get(st..en).map(<[Vec<u8>]>::to_vec).unwrap_or_default()
        }
    }

    /// zsh's `getintvalue`.
    pub(crate) fn getintvalue(&mut self, v: Option<&mut Value>) -> i64 {
        let Some(v) = v else { return 0 };
        if v.flags & VALFLAG_INV != 0 {
            return v.start;
        }
        if v.isarr != 0 {
            let arr = self.getarrvalue(Some(v));
            let scal = self.sepjoin(&arr, None);
            return self.mathevali(&scal);
        }
        let flags = self.pm_flags(&v.pm);
        if pm_type(flags) == PM_INTEGER {
            return self.getifn(&v.pm);
        }
        if flags & (PM_EFLOAT | PM_FFLOAT) != 0 {
            #[expect(clippy::cast_possible_truncation, reason = "zsh casts to zlong")]
            return self.getffn(&v.pm) as i64;
        }
        let s = self.getstrvalue(Some(v));
        self.mathevali(&s)
    }

    /// zsh's `getnumvalue`.
    pub(crate) fn getnumvalue(&mut self, v: Option<&mut Value>) -> MNumber {
        let Some(v) = v else { return MNumber::Int(0) };
        if v.flags & VALFLAG_INV != 0 {
            return MNumber::Int(v.start);
        }
        if v.isarr != 0 {
            let arr = self.getarrvalue(Some(v));
            let scal = self.sepjoin(&arr, None);
            return self.matheval(&scal);
        }
        let flags = self.pm_flags(&v.pm);
        if pm_type(flags) == PM_INTEGER {
            MNumber::Int(self.getifn(&v.pm))
        } else if flags & (PM_EFLOAT | PM_FFLOAT) != 0 {
            MNumber::Float(self.getffn(&v.pm))
        } else {
            let s = self.getstrvalue(Some(v));
            self.matheval(&s)
        }
    }

    // ------------------------------------------------------------------
    // Assigning.
    // ------------------------------------------------------------------

    /// zsh's `assignstrvalue`; `val` `None` is NULL.
    #[expect(clippy::too_many_lines, reason = "zsh's assignstrvalue")]
    pub(crate) fn assignstrvalue(&mut self, v: &mut Value, val: Option<Vec<u8>>, flags: i32) {
        if !self.isset(EXECOPT) {
            return;
        }
        let pflags = self.pm_flags(&v.pm);
        let name = self.pm_name(&v.pm);
        if pflags & PM_READONLY != 0 {
            self.zerr(&format!("read-only variable: {}", lossy(&name)));
            return;
        }
        if pflags & PM_RESTRICTED != 0 && self.isset(RESTRICTED) {
            self.zerr(&format!("{}: restricted", lossy(&name)));
            return;
        }
        if pflags & PM_HASHED != 0 && v.isarr & (SCANPM_MATCHMANY | SCANPM_ARRONLY) != 0 {
            self.zerr(&format!(
                "{}: attempt to set slice of associative array",
                lossy(&name)
            ));
            return;
        }
        if v.flags & VALFLAG_EMPTY != 0 {
            self.zerr(&format!(
                "{}: assignment to invalid subscript range",
                lossy(&name)
            ));
            return;
        }
        self.set_pm_flags(&mut v.pm, |f| f & !PM_UNSET);
        match pm_type(pflags) {
            PM_SCALAR => {
                if v.start == 0 && v.end == -1 {
                    let vl = val.as_ref().map_or(0, Vec::len);
                    self.setsfn(&mut v.pm, val);
                    let p = self.pm(&v.pm).map(|p| (p.flags, p.width));
                    if let Some((f, w)) = p
                        && f & (PM_LEFT | PM_RIGHT_B | PM_RIGHT_Z) != 0
                        && w == 0
                    {
                        let _ =
                            self.with_pm(&mut v.pm, |p| p.width = i32::try_from(vl).unwrap_or(0));
                    }
                } else {
                    let z = self.getsfn(&v.pm);
                    let zlen = i64::try_from(z.len()).unwrap_or(0);
                    if v.flags & VALFLAG_INV != 0 && !self.isset(KSHARRAYS) {
                        v.start -= 1;
                        v.end -= 1;
                    }
                    if v.start < 0 {
                        v.start += zlen;
                        if v.start < 0 {
                            v.start = 0;
                        }
                    }
                    if v.start > zlen {
                        v.start = zlen;
                    }
                    if v.end < 0 {
                        v.end += zlen;
                        if v.end < 0 {
                            v.end = 0;
                        } else if v.end >= zlen {
                            v.end = zlen;
                        } else if self.isset(MULTIBYTE) {
                            let e = usize::try_from(v.end).unwrap_or(0);
                            v.end += i64::try_from(mb_metacharlen(self, from(&z, e)).max(1))
                                .unwrap_or(1);
                        } else {
                            v.end += 1;
                        }
                    } else if v.end > zlen {
                        v.end = zlen;
                    }
                    let st = usize::try_from(v.start).unwrap_or(0);
                    let en = usize::try_from(v.end).unwrap_or(0).max(st);
                    let mut x = sub(&z, 0, st).to_vec();
                    x.extend_from_slice(val.as_deref().unwrap_or(&[]));
                    x.extend_from_slice(from(&z, en));
                    self.setsfn(&mut v.pm, Some(x));
                }
            }
            PM_INTEGER => {
                if let Some(val) = &val {
                    let ival = if flags & ASSPM_ENV_IMPORT != 0 {
                        crate::utils::zstrtol_underscore(val, 0, true).0
                    } else {
                        self.mathevali(val)
                    };
                    self.setifn(&mut v.pm, ival);
                    let w = self.pm(&v.pm).map_or(1, |p| p.width);
                    if self.pm_flags(&v.pm) & (PM_LEFT | PM_RIGHT_B | PM_RIGHT_Z) != 0 && w == 0 {
                        let _ = self.with_pm(&mut v.pm, |p| {
                            p.width = i32::try_from(val.len()).unwrap_or(0)
                        });
                    }
                }
                let lastbase = self.lastbase;
                if self.pm(&v.pm).is_some_and(|p| p.base == 0) && lastbase != -1 {
                    let _ = self.with_pm(&mut v.pm, |p| p.base = lastbase);
                }
            }
            PM_EFLOAT | PM_FFLOAT => {
                if let Some(val) = &val {
                    let mn = if flags & ASSPM_ENV_IMPORT != 0 {
                        MNumber::Float(lossy(val).trim().parse().unwrap_or(0.0))
                    } else {
                        self.matheval(val)
                    };
                    let d = match mn {
                        MNumber::Float(d) => d,
                        MNumber::Int(i) => i as f64,
                    };
                    self.setffn(&mut v.pm, d);
                    let w = self.pm(&v.pm).map_or(1, |p| p.width);
                    if self.pm_flags(&v.pm) & (PM_LEFT | PM_RIGHT_B | PM_RIGHT_Z) != 0 && w == 0 {
                        let _ = self.with_pm(&mut v.pm, |p| {
                            p.width = i32::try_from(val.len()).unwrap_or(0)
                        });
                    }
                }
            }
            PM_ARRAY => {
                self.setarrvalue(v, vec![val.unwrap_or_default()]);
            }
            PM_HASHED => match v.found.clone() {
                None => {
                    self.zerr(&format!(
                        "{}: attempt to set associative array to scalar",
                        lossy(&name)
                    ));
                    return;
                }
                Some(mut f) => self.setsfn(&mut f, val),
            },
            _ => {}
        }
        let (env, pflags, ename) = match self.pm(&v.pm) {
            Some(p) => (p.env, p.flags, p.ename.is_some()),
            None => return,
        };
        if (!env
            && pflags & PM_EXPORTED == 0
            && !(self.isset(ALLEXPORT) && pflags & PM_HASHELEM == 0))
            || pflags & PM_ARRAY != 0
            || ename
        {
            return;
        }
        let mut r = v.pm.clone();
        self.export_param(&mut r);
    }

    pub(crate) fn setstrvalue(&mut self, v: &mut Value, val: Option<Vec<u8>>) {
        self.assignstrvalue(v, val, 0);
    }

    /// zsh's `setnumvalue`.
    pub(crate) fn setnumvalue(&mut self, v: &mut Value, val: MNumber) {
        if !self.isset(EXECOPT) {
            return;
        }
        let flags = self.pm_flags(&v.pm);
        let name = self.pm_name(&v.pm);
        if flags & PM_READONLY != 0 {
            self.zerr(&format!("read-only variable: {}", lossy(&name)));
            return;
        }
        if flags & PM_RESTRICTED != 0 && self.isset(RESTRICTED) {
            self.zerr(&format!("{}: restricted", lossy(&name)));
            return;
        }
        match pm_type(flags) {
            PM_SCALAR | PM_ARRAY => {
                let p = match val {
                    MNumber::Int(l) => {
                        self.convbase_underscore(l, self.outputradix, self.outputunderscore)
                    }
                    #[expect(clippy::cast_possible_truncation, reason = "zsh casts to zlong")]
                    MNumber::Float(d) if self.outputradix != 0 => {
                        self.convbase_underscore(d as i64, self.outputradix, self.outputunderscore)
                    }
                    MNumber::Float(d) => convfloat_underscore(d, self.outputunderscore),
                };
                self.setstrvalue(v, Some(p));
            }
            PM_INTEGER => {
                self.setifn(&mut v.pm, mnumber_int(val));
                self.setstrvalue(v, None);
            }
            PM_EFLOAT | PM_FFLOAT => {
                let d = match val {
                    MNumber::Int(l) => l as f64,
                    MNumber::Float(d) => d,
                };
                self.setffn(&mut v.pm, d);
                self.setstrvalue(v, None);
            }
            _ => {}
        }
    }

    /// zsh's `setarrvalue`.
    pub(crate) fn setarrvalue(&mut self, v: &mut Value, val: Vec<Vec<u8>>) {
        if !self.isset(EXECOPT) {
            return;
        }
        let flags = self.pm_flags(&v.pm);
        let name = self.pm_name(&v.pm);
        if flags & PM_READONLY != 0 {
            self.zerr(&format!("read-only variable: {}", lossy(&name)));
            return;
        }
        if flags & PM_RESTRICTED != 0 && self.isset(RESTRICTED) {
            self.zerr(&format!("{}: restricted", lossy(&name)));
            return;
        }
        if pm_type(flags) & (PM_ARRAY | PM_HASHED) == 0 {
            self.zerr(&format!(
                "{}: attempt to assign array value to non-array",
                lossy(&name)
            ));
            return;
        }
        if v.flags & VALFLAG_EMPTY != 0 {
            self.zerr(&format!(
                "{}: assignment to invalid subscript range",
                lossy(&name)
            ));
            return;
        }
        if v.start == 0 && v.end == -1 {
            if pm_type(flags) == PM_HASHED {
                self.arrhashsetfn(&mut v.pm, val, 0);
            } else {
                self.setafn(&mut v.pm, Some(val));
            }
        } else if v.start == -1 && v.end == 0 && pm_type(flags) == PM_HASHED {
            self.arrhashsetfn(&mut v.pm, val, ASSPM_AUGMENT);
        } else if pm_type(flags) == PM_HASHED {
            self.zerr(&format!(
                "{}: attempt to set slice of associative array",
                lossy(&name)
            ));
        } else {
            let old = self.getafn(&v.pm);
            let pre = i64::try_from(old.len()).unwrap_or(0);
            if v.flags & VALFLAG_INV != 0 && !self.isset(KSHARRAYS) {
                if v.start > 0 {
                    v.start -= 1;
                }
                v.end -= 1;
            }
            if v.start < 0 {
                v.start += pre;
                if v.start < 0 {
                    v.start = 0;
                }
            }
            if v.end < 0 {
                v.end += pre + 1;
                if v.end < 0 {
                    v.end = 0;
                }
            }
            if v.end < v.start {
                v.end = v.start;
            }
            let st = usize::try_from(v.start).unwrap_or(0);
            let en = usize::try_from(v.end).unwrap_or(0);
            let mut new: Vec<Vec<u8>> = Vec::with_capacity(st + val.len() + old.len());
            for k in 0..st {
                new.push(old.get(k).cloned().unwrap_or_default());
            }
            new.extend(val);
            if en < old.len() {
                new.extend(old.get(en..).unwrap_or(&[]).iter().cloned());
            }
            self.setafn(&mut v.pm, Some(new));
        }
    }

    /// zsh's `arrhashsetfn`: set an association from key/value pairs.
    fn arrhashsetfn(&mut self, r: &mut PmRef, val: Vec<Vec<u8>>, flags: i32) {
        let alen = val
            .iter()
            .filter(|e| e.first() != Some(&tok::MARKER))
            .count();
        if alen % 2 != 0 {
            self.zerr("bad set of key/value pairs for associative array");
            return;
        }
        let mut ht: Option<HashTable<Param>> = None;
        if flags & ASSPM_AUGMENT != 0 {
            let taken = self.with_pm(r, |p| match std::mem::take(&mut p.u) {
                U::Hash(t) => Some(*t),
                other => {
                    p.u = other;
                    None
                }
            });
            ht = taken.flatten();
            if ht.is_none() && matches!(self.pm(r).map(|p| &p.gsu), Some(Gsu::Module(_))) {
                // A module's hash: rebuild its table from its elements.
                let mut t = HashTable::new(17);
                for (k, er) in self.hash_elements(r) {
                    let mut ev = Value::new(er);
                    let s = self.getstrvalue(Some(&mut ev));
                    let mut p = Param::new(PM_SCALAR | PM_HASHELEM);
                    p.u = U::Str(s);
                    let _ = t.insert(k, p);
                }
                ht = Some(t);
            }
        }
        if alen != 0 && (flags & ASSPM_AUGMENT == 0 || ht.is_none()) {
            ht = Some(HashTable::new(17));
        }
        let saved = self.paramtab_override.take();
        self.paramtab_override = ht.map(Box::new);
        let mut it = val.into_iter();
        while let Some(mut e) = it.next() {
            let mut augment = false;
            if e.first() == Some(&tok::MARKER) {
                augment = e.get(1) == Some(&b'+');
                let Some(k) = it.next() else { break };
                e = k;
            }
            let key = e;
            let v2 = it.next();
            if self.createparam(&key, PM_SCALAR | PM_UNSET).is_none() {
                // Exists already.
            }
            let mut ev = Value::new(PmRef::Name(key));
            if augment {
                ev.start = i64::from(i32::MAX);
            }
            self.assignstrvalue(&mut ev, v2, if augment { ASSPM_AUGMENT } else { 0 });
        }
        let ht = self.paramtab_override.take().map(|b| *b);
        self.paramtab_override = saved;
        self.sethfn(r, ht);
    }

    /// zsh's `getiparam`.
    pub(crate) fn getiparam(&mut self, s: &[u8]) -> i64 {
        let mut i = 0;
        match self.getvalue(s, &mut i, 1) {
            Some(mut v) => self.getintvalue(Some(&mut v)),
            None => 0,
        }
    }

    /// zsh's `getnparam`.
    pub(crate) fn getnparam(&mut self, s: &[u8]) -> MNumber {
        let mut i = 0;
        match self.getvalue(s, &mut i, 1) {
            Some(mut v) => self.getnumvalue(Some(&mut v)),
            None => MNumber::Int(0),
        }
    }

    /// zsh's `getsparam`.
    pub(crate) fn getsparam(&mut self, s: &[u8]) -> Option<Vec<u8>> {
        let mut i = 0;
        let mut v = self.getvalue(s, &mut i, 0)?;
        Some(self.getstrvalue(Some(&mut v)))
    }

    /// zsh's `getsparam_u`: unmetafied.
    pub(crate) fn getsparam_u(&mut self, s: &[u8]) -> Option<Vec<u8>> {
        self.getsparam(s).map(|v| tok::unmetafy(&v))
    }

    /// zsh's `getaparam`.
    pub(crate) fn getaparam(&mut self, s: &[u8]) -> Option<Vec<Vec<u8>>> {
        if s.first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        let mut i = 0;
        let v = self.getvalue(s, &mut i, 0)?;
        if pm_type(self.pm_flags(&v.pm)) == PM_ARRAY {
            Some(self.getafn(&v.pm))
        } else {
            None
        }
    }

    /// zsh's `gethparam`: the values of an association.
    pub(crate) fn gethparam(&mut self, s: &[u8]) -> Option<Vec<Vec<u8>>> {
        self.gethparam_flags(s, SCANPM_WANTVALS)
    }

    /// zsh's `gethkparam`: the keys of an association.
    pub(crate) fn gethkparam(&mut self, s: &[u8]) -> Option<Vec<Vec<u8>>> {
        self.gethparam_flags(s, SCANPM_WANTKEYS)
    }

    fn gethparam_flags(&mut self, s: &[u8], flags: i32) -> Option<Vec<Vec<u8>>> {
        if s.first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        let mut i = 0;
        let v = self.getvalue(s, &mut i, 0)?;
        if pm_type(self.pm_flags(&v.pm)) != PM_HASHED {
            return None;
        }
        let mut found = None;
        Some(self.paramvalarr(&v.pm, flags, None, &mut found))
    }

    /// zsh's `check_warn_pm`.
    fn check_warn_pm(&mut self, r: &PmRef, pmtype: &str, created: bool, may_warn_nested: bool) {
        if !may_warn_nested && !created {
            return;
        }
        let Some(pm) = self.pm(r) else { return };
        let (level, flags) = (pm.level, pm.flags);
        if created && self.isset(WARNCREATEGLOBAL) {
            if self.locallevel <= self.forklevel || level != 0 {
                return;
            }
        } else if !created && self.isset(WARNNESTEDVAR) {
            if level >= self.locallevel {
                return;
            }
        } else {
            return;
        }
        if flags & PM_SPECIAL != 0 {
            return;
        }
        if let Some(fname) = self.innermost_function_name() {
            let name = self.pm_name(r);
            let msg = if created {
                format!(
                    "{pmtype} parameter {} created globally in function {}",
                    lossy(&name),
                    lossy(&fname)
                )
            } else {
                format!(
                    "{pmtype} parameter {} set in enclosing scope in function {}",
                    lossy(&name),
                    lossy(&fname)
                )
            };
            self.zwarn(&msg);
        }
    }

    /// zsh's `assignsparam`.
    pub(crate) fn assignsparam(&mut self, s: &[u8], val: Vec<u8>, flags: i32) -> Option<PmRef> {
        let mut flags = flags;
        let mut val = val;
        if !self.isident(s) {
            self.zerr(&format!("not an identifier: {}", lossy(s)));
            self.errflag_set_error();
            return None;
        }
        let mut created = false;
        let mut v: Option<Value>;
        if let Some(ss) = s.iter().position(|&c| c == b'[') {
            let base = sub(s, 0, ss).to_vec();
            let mut i = 0;
            match self.getvalue(&base, &mut i, 1) {
                None => {
                    let _ = self.createparam(&base, PM_ARRAY);
                    created = true;
                }
                Some(bv) => {
                    if self.pm_flags(&bv.pm) & PM_READONLY != 0 {
                        let n = self.pm_name(&bv.pm);
                        self.zerr(&format!("read-only variable: {}", lossy(&n)));
                        return None;
                    }
                    flags &= !ASSPM_WARN;
                    let mut pr = bv.pm;
                    self.set_pm_flags(&mut pr, |f| f & !PM_DEFAULTED);
                }
            }
            v = None;
        } else {
            let mut i = 0;
            v = self.getvalue(s, &mut i, 1);
            match &v {
                None => {
                    let _ = self.createparam(s, PM_SCALAR);
                    created = true;
                }
                Some(cur) => {
                    let f = self.pm_flags(&cur.pm);
                    if ((f & PM_ARRAY != 0 && flags & ASSPM_AUGMENT == 0) || f & PM_HASHED != 0)
                        && f & (PM_SPECIAL | PM_TIED) == 0
                        && !self.isset(KSHARRAYS)
                    {
                        self.unsetparam(s);
                        let _ = self.createparam(s, PM_SCALAR);
                        v = None;
                    }
                }
            }
        }
        let mut v = match v {
            Some(v) => v,
            None => {
                let mut i = 0;
                let mut buf = s.to_vec();
                self.fetchvalue(&mut buf, &mut i, 1, 0)?
            }
        };
        if flags & ASSPM_WARN != 0 {
            let r = v.pm.clone();
            self.check_warn_pm(&r, "scalar", created, true);
        }
        self.set_pm_flags(&mut v.pm, |f| f & !PM_DEFAULTED);
        if flags & ASSPM_AUGMENT != 0 {
            let ptype = pm_type(self.pm_flags(&v.pm));
            if v.start == 0 && v.end == -1 {
                match ptype {
                    PM_SCALAR => v.start = i64::from(i32::MAX),
                    PM_INTEGER | PM_EFLOAT | PM_FFLOAT => {
                        let rhs = self.matheval(&val);
                        let lhs = self.getnumvalue(Some(&mut v));
                        let sum = match (lhs, rhs) {
                            (MNumber::Float(a), MNumber::Float(b)) => MNumber::Float(a + b),
                            (MNumber::Float(a), MNumber::Int(b)) => MNumber::Float(a + b as f64),
                            (MNumber::Int(a), MNumber::Int(b)) => MNumber::Int(a.wrapping_add(b)),
                            #[expect(
                                clippy::cast_possible_truncation,
                                reason = "zsh casts to zlong"
                            )]
                            (MNumber::Int(a), MNumber::Float(b)) => {
                                MNumber::Int(a.wrapping_add(b as i64))
                            }
                        };
                        self.setnumvalue(&mut v, sum);
                        return Some(v.pm);
                    }
                    PM_ARRAY => {
                        if self.isset(KSHARRAYS) {
                            v.end = 1;
                            val = self.kshappend(&mut v, val);
                        } else {
                            v.start = i64::try_from(self.getafn(&v.pm).len()).unwrap_or(0);
                            v.end = v.start + 1;
                        }
                    }
                    _ => {}
                }
            } else {
                match ptype {
                    PM_SCALAR => {
                        if v.end > 0 {
                            v.start = v.end;
                        } else {
                            let l = i64::try_from(self.getsfn(&v.pm).len()).unwrap_or(0);
                            v.start = l + v.end + 1;
                            v.end = v.start;
                        }
                    }
                    PM_INTEGER | PM_EFLOAT | PM_FFLOAT => {
                        self.zerr("attempt to add to slice of a numeric variable");
                        return None;
                    }
                    PM_ARRAY => val = self.kshappend(&mut v, val),
                    _ => {}
                }
            }
        }
        self.assignstrvalue(&mut v, Some(val), flags);
        Some(v.pm)
    }

    /// The `kshappend` arm of `assignsparam`: append to the slice's last
    /// element.
    fn kshappend(&mut self, v: &mut Value, val: Vec<u8>) -> Vec<u8> {
        let sstart = if v.end > 0 { v.end - 1 } else { v.end };
        v.start = sstart;
        v.isarr = 0;
        let mut var = self.getstrvalue(Some(v));
        v.start = sstart;
        var.extend(val);
        var
    }

    /// zsh's `setsparam`.
    pub(crate) fn setsparam(&mut self, s: &[u8], val: Vec<u8>) -> Option<PmRef> {
        self.assignsparam(s, val, ASSPM_WARN)
    }

    /// zsh's `assignaparam`.
    #[expect(clippy::too_many_lines, reason = "zsh's assignaparam")]
    pub(crate) fn assignaparam(
        &mut self,
        s: &[u8],
        val: Vec<Vec<u8>>,
        flags: i32,
    ) -> Option<PmRef> {
        let mut val = val;
        if !self.isident(s) {
            self.zerr(&format!("not an identifier: {}", lossy(s)));
            self.errflag_set_error();
            return None;
        }
        let mut created = false;
        let mut may_warn_nested = true;
        let mut v: Option<Value> = None;
        if let Some(ss) = s.iter().position(|&c| c == b'[') {
            let base = sub(s, 0, ss).to_vec();
            let mut i = 0;
            match self.getvalue(&base, &mut i, 1) {
                None => {
                    let _ = self.createparam(&base, PM_ARRAY);
                    created = true;
                }
                Some(bv) => {
                    may_warn_nested = false;
                    if pm_type(self.pm_flags(&bv.pm)) == PM_HASHED {
                        let n = self.pm_name(&bv.pm);
                        self.zerr(&format!(
                            "{}: attempt to set slice of associative array",
                            lossy(&n)
                        ));
                        self.errflag_set_error();
                        return None;
                    }
                }
            }
        } else {
            let mut i = 0;
            let mut buf = s.to_vec();
            match self.fetchvalue(&mut buf, &mut i, 1, SCANPM_ASSIGNING) {
                None => {
                    let _ = self.createparam(s, PM_ARRAY);
                    created = true;
                }
                Some(mut cur) => {
                    let f = self.pm_flags(&cur.pm);
                    if pm_type(f) & (PM_ARRAY | PM_HASHED) == 0 && f & (PM_SPECIAL | PM_TIED) == 0 {
                        let uniq = f & PM_UNIQUE;
                        if flags & ASSPM_AUGMENT != 0 {
                            let old = self.getstrvalue(Some(&mut cur));
                            val.insert(0, old);
                        }
                        self.unsetparam(s);
                        let _ = self.createparam(s, PM_ARRAY | uniq);
                    } else {
                        v = Some(cur);
                    }
                }
            }
        }
        let mut v = match v {
            Some(v) => v,
            None => {
                let mut i = 0;
                let mut buf = s.to_vec();
                self.fetchvalue(&mut buf, &mut i, 1, SCANPM_ASSIGNING)?
            }
        };
        if flags & ASSPM_WARN != 0 {
            let r = v.pm.clone();
            self.check_warn_pm(&r, "array", created, may_warn_nested);
        }
        self.set_pm_flags(&mut v.pm, |f| f & !PM_DEFAULTED);
        let ptype = pm_type(self.pm_flags(&v.pm));
        if flags & ASSPM_KEY_VALUE != 0 {
            if ptype & PM_ARRAY != 0 {
                let orig = if flags & ASSPM_AUGMENT != 0 {
                    self.getafn(&v.pm)
                } else {
                    Vec::new()
                };
                let mut full: Vec<Option<Vec<u8>>> = orig.into_iter().map(Some).collect();
                let mut nextind: usize = 0;
                let mut it = val.into_iter();
                while let Some(e) = it.next() {
                    if e.first() == Some(&tok::MARKER) {
                        let augment = e.get(1) == Some(&b'+');
                        let idx_text = it.next().unwrap_or_default();
                        let mut idx = self.mathevali(&idx_text);
                        if idx < 0 || (!self.isset(KSHARRAYS) && idx == 0) {
                            self.zerr(&format!(
                                "bad subscript for direct array assignment: {}",
                                lossy(&idx_text)
                            ));
                            return None;
                        }
                        if !self.isset(KSHARRAYS) {
                            idx -= 1;
                        }
                        let k = usize::try_from(idx).unwrap_or(0);
                        let value = it.next().unwrap_or_default();
                        while full.len() <= k {
                            full.push(None);
                        }
                        if let Some(slot) = full.get_mut(k) {
                            *slot = match (augment, slot.take()) {
                                (true, Some(mut old)) => {
                                    old.extend(value);
                                    Some(old)
                                }
                                _ => Some(value),
                            };
                        }
                        nextind = k + 1;
                    } else {
                        while full.len() <= nextind {
                            full.push(None);
                        }
                        if let Some(slot) = full.get_mut(nextind) {
                            *slot = Some(e);
                        }
                        nextind += 1;
                    }
                }
                let full: Vec<Vec<u8>> = full.into_iter().map(Option::unwrap_or_default).collect();
                self.setarrvalue(&mut v, full);
                return Some(v.pm);
            } else if ptype & PM_HASHED != 0 {
                for chunk in val.chunks(3) {
                    if chunk.first().and_then(|e| e.first()) != Some(&tok::MARKER) {
                        self.zerr("bad [key]=value syntax for associative array");
                        return None;
                    }
                }
            } else {
                self.zerr("invalid use of [key]=value assignment syntax");
                return None;
            }
        }
        if flags & ASSPM_AUGMENT != 0 {
            if v.start == 0 && v.end == -1 {
                if ptype & PM_ARRAY != 0 {
                    v.start = i64::try_from(self.getafn(&v.pm).len()).unwrap_or(0);
                    v.end = v.start + 1;
                } else if ptype & PM_HASHED != 0 {
                    v.start = -1;
                    v.end = 0;
                }
            } else if v.end > 0 {
                v.start = v.end;
                v.end -= 1;
            } else if ptype & PM_ARRAY != 0 {
                v.end += i64::try_from(self.getafn(&v.pm).len()).unwrap_or(0);
                v.start = v.end + 1;
            }
        }
        self.setarrvalue(&mut v, val);
        Some(v.pm)
    }

    /// zsh's `setaparam`.
    pub(crate) fn setaparam(&mut self, s: &[u8], val: Vec<Vec<u8>>) -> Option<PmRef> {
        self.assignaparam(s, val, ASSPM_WARN)
    }

    /// zsh's `sethparam`.
    pub(crate) fn sethparam(&mut self, s: &[u8], val: Vec<Vec<u8>>) -> Option<PmRef> {
        if !self.isident(s) {
            self.zerr(&format!("not an identifier: {}", lossy(s)));
            self.errflag_set_error();
            return None;
        }
        if s.contains(&b'[') {
            self.zerr("nested associative arrays not yet supported");
            self.errflag_set_error();
            return None;
        }
        if !self.isset(EXECOPT) {
            return None;
        }
        let mut checkcreate = false;
        let mut i = 0;
        let mut buf = s.to_vec();
        let mut v = self.fetchvalue(&mut buf, &mut i, 1, SCANPM_ASSIGNING);
        match &v {
            None => {
                let _ = self.createparam(s, PM_HASHED);
                checkcreate = true;
            }
            Some(cur) if pm_type(self.pm_flags(&cur.pm)) & PM_HASHED == 0 => {
                if self.pm_flags(&cur.pm) & PM_SPECIAL == 0 {
                    self.unsetparam(s);
                    let _ = self.createparam(s, PM_HASHED);
                    v = None;
                } else {
                    self.zerr(&format!(
                        "{}: can't change type of a special parameter",
                        lossy(s)
                    ));
                    return None;
                }
            }
            _ => {}
        }
        let mut v = match v {
            Some(v) => v,
            None => {
                let mut i = 0;
                let mut buf = s.to_vec();
                self.fetchvalue(&mut buf, &mut i, 1, SCANPM_ASSIGNING)?
            }
        };
        let r = v.pm.clone();
        self.check_warn_pm(&r, "associative array", checkcreate, true);
        self.set_pm_flags(&mut v.pm, |f| f & !PM_DEFAULTED);
        self.setarrvalue(&mut v, val);
        Some(v.pm)
    }

    /// zsh's `assignnparam`.
    pub(crate) fn assignnparam(&mut self, s: &[u8], val: MNumber, flags: i32) -> Option<PmRef> {
        if !self.isident(s) {
            self.zerr(&format!("not an identifier: {}", lossy(s)));
            self.errflag_set_error();
            return None;
        }
        if !self.isset(EXECOPT) {
            return None;
        }
        let has_sub = s.contains(&b'[');
        let mut i = 0;
        let mut v = self.getvalue(s, &mut i, 1);
        let mut was_unset = false;
        if let Some(cur) = &v {
            let f = self.pm_flags(&cur.pm);
            if f & (PM_ARRAY | PM_HASHED) != 0
                && f & (PM_SPECIAL | PM_TIED) == 0
                && !self.isset(KSHARRAYS)
                && !has_sub
            {
                let mut r = cur.pm.clone();
                let _ = self.unsetparam_pm(&mut r, false, true);
                was_unset = true;
                v = None;
            }
        }
        let mut v = match v {
            Some(v) => {
                if flags & ASSPM_WARN != 0 {
                    let r = v.pm.clone();
                    self.check_warn_pm(&r, "numeric", false, true);
                }
                v
            }
            None => {
                let (base, ss) = match s.iter().position(|&c| c == b'[') {
                    Some(p) => (sub(s, 0, p).to_vec(), true),
                    None => (s.to_vec(), false),
                };
                let ptype = if ss {
                    PM_ARRAY
                } else if self.isset(POSIXIDENTIFIERS) {
                    PM_SCALAR
                } else if matches!(val, MNumber::Int(_)) {
                    PM_INTEGER
                } else {
                    PM_FFLOAT
                };
                let _ = self.createparam(&base, ptype);
                if !ss && matches!(val, MNumber::Int(_)) {
                    let radix = self.outputradix;
                    if let Some(p) = self.paramtab_mut().get_mut(&base) {
                        p.base = radix;
                    }
                }
                let mut i = 0;
                let v = self.getvalue(s, &mut i, 1)?;
                if flags & ASSPM_WARN != 0 {
                    let r = v.pm.clone();
                    self.check_warn_pm(&r, "numeric", !was_unset, true);
                }
                v
            }
        };
        self.set_pm_flags(&mut v.pm, |f| f & !PM_DEFAULTED);
        self.setnumvalue(&mut v, val);
        Some(v.pm)
    }

    pub(crate) fn setnparam(&mut self, s: &[u8], val: MNumber) -> Option<PmRef> {
        self.assignnparam(s, val, ASSPM_WARN)
    }

    pub(crate) fn assigniparam(&mut self, s: &[u8], val: i64, flags: i32) -> Option<PmRef> {
        self.assignnparam(s, MNumber::Int(val), flags)
    }

    pub(crate) fn setiparam(&mut self, s: &[u8], val: i64) -> Option<PmRef> {
        self.assignnparam(s, MNumber::Int(val), ASSPM_WARN)
    }

    /// zsh's `setiparam_no_convert`.
    pub(crate) fn setiparam_no_convert(&mut self, s: &[u8], val: i64) -> Option<PmRef> {
        let text = self.convbase(val, 10);
        self.assignsparam(s, text, ASSPM_WARN)
    }
}
