//! Pattern matching (zsh's `pattern.c`): patterns compiled to a program of
//! nodes in zsh's own layout, run by a backtracking matcher with globbing
//! flags, backreferences, approximate matching, exclusions and counts.
//!
//! The program is kept exactly as zsh lays it out: a byte buffer of
//! word-sized cells, a node being `op | next << 8` with its operands after
//! it, strings inline. Offsets, insertion and the matcher's pointer
//! arithmetic then follow the C one for one.

use crate::options::*;
use crate::shell::Shell;
use crate::tok::{
    self, BANG, BAR, BNULLKEEP, COMMA, DASH, HAT, INANG, INBRACK, INPAR, MARKER, META, NULARG,
};
use crate::tok::{OUTANG, OUTBRACK, OUTPAR, POUND, QUEST, STAR, TILDE};
use crate::utils::{at, from, sub};

const NSUBEXP: usize = 9;

const P_END: i64 = 0x00;
const P_EXCSYNC: i64 = 0x01;
const P_EXCEND: i64 = 0x02;
const P_BACK: i64 = 0x03;
const P_EXACTLY: i64 = 0x04;
const P_NOTHING: i64 = 0x05;
const P_ONEHASH: i64 = 0x06;
const P_TWOHASH: i64 = 0x07;
const P_GFLAGS: i64 = 0x08;
const P_ISSTART: i64 = 0x09;
const P_ISEND: i64 = 0x0a;
const P_COUNTSTART: i64 = 0x0b;
const P_COUNT: i64 = 0x0c;
const P_BRANCH: i64 = 0x20;
const P_WBRANCH: i64 = 0x21;
const P_EXCLUDE: i64 = 0x30;
const P_EXCLUDP: i64 = 0x31;
const P_ANY: i64 = 0x40;
const P_ANYOF: i64 = 0x41;
const P_ANYBUT: i64 = 0x42;
const P_STAR: i64 = 0x43;
const P_NUMRNG: i64 = 0x44;
const P_NUMFROM: i64 = 0x45;
const P_NUMTO: i64 = 0x46;
const P_NUMANY: i64 = 0x47;
const P_OPEN: i64 = 0x80;
const P_CLOSE: i64 = 0x90;

const P_CT_CURRENT: usize = 1;
const P_CT_MIN: usize = 2;
const P_CT_MAX: usize = 3;
const P_CT_PTR: usize = 4;
const P_CT_OPERAND: usize = 5;

const P_SIMPLE: i32 = 0x01;
const P_HSTART: i32 = 0x02;
const P_PURESTR: i32 = 0x04;

pub(crate) const PAT_HEAPDUP: i32 = 0x0000;
pub(crate) const PAT_FILE: i32 = 0x0001;
pub(crate) const PAT_FILET: i32 = 0x0002;
pub(crate) const PAT_ANY: i32 = 0x0004;
pub(crate) const PAT_NOANCH: i32 = 0x0008;
pub(crate) const PAT_NOGLD: i32 = 0x0010;
pub(crate) const PAT_PURES: i32 = 0x0020;
pub(crate) const PAT_STATIC: i32 = 0x0040;
pub(crate) const PAT_SCAN: i32 = 0x0080;
pub(crate) const PAT_ZDUP: i32 = 0x0100;
pub(crate) const PAT_NOTSTART: i32 = 0x0200;
pub(crate) const PAT_NOTEND: i32 = 0x0400;
pub(crate) const PAT_HAS_EXCLUDP: i32 = 0x0800;
pub(crate) const PAT_LCMATCHUC: i32 = 0x1000;

pub(crate) const GF_LCMATCHUC: i32 = 0x0100;
pub(crate) const GF_IGNCASE: i32 = 0x0200;
pub(crate) const GF_BACKREF: i32 = 0x0400;
pub(crate) const GF_MATCHREF: i32 = 0x0800;
pub(crate) const GF_MULTIBYTE: i32 = 0x1000;

pub(crate) const ZPC_SLASH: usize = 0;
pub(crate) const ZPC_NULL: usize = 1;
pub(crate) const ZPC_BAR: usize = 2;
pub(crate) const ZPC_OUTPAR: usize = 3;
pub(crate) const ZPC_TILDE: usize = 4;
pub(crate) const ZPC_SEG_COUNT: usize = 5;
pub(crate) const ZPC_INPAR: usize = 5;
pub(crate) const ZPC_QUEST: usize = 6;
pub(crate) const ZPC_STAR: usize = 7;
pub(crate) const ZPC_INBRACK: usize = 8;
pub(crate) const ZPC_INANG: usize = 9;
pub(crate) const ZPC_HAT: usize = 10;
pub(crate) const ZPC_HASH: usize = 11;
pub(crate) const ZPC_BNULLKEEP: usize = 12;
pub(crate) const ZPC_NO_KSH_GLOB: usize = 13;
pub(crate) const ZPC_KSH_QUEST: usize = 13;
pub(crate) const ZPC_KSH_STAR: usize = 14;
pub(crate) const ZPC_KSH_PLUS: usize = 15;
pub(crate) const ZPC_KSH_BANG: usize = 16;
pub(crate) const ZPC_KSH_BANG2: usize = 17;
pub(crate) const ZPC_KSH_AT: usize = 18;
pub(crate) const ZPC_COUNT: usize = 19;

pub(crate) const PP_FIRST: u8 = 1;
pub(crate) const PP_ALPHA: u8 = 1;
pub(crate) const PP_ALNUM: u8 = 2;
pub(crate) const PP_ASCII: u8 = 3;
pub(crate) const PP_BLANK: u8 = 4;
pub(crate) const PP_CNTRL: u8 = 5;
pub(crate) const PP_DIGIT: u8 = 6;
pub(crate) const PP_GRAPH: u8 = 7;
pub(crate) const PP_LOWER: u8 = 8;
pub(crate) const PP_PRINT: u8 = 9;
pub(crate) const PP_PUNCT: u8 = 10;
pub(crate) const PP_SPACE: u8 = 11;
pub(crate) const PP_UPPER: u8 = 12;
pub(crate) const PP_XDIGIT: u8 = 13;
pub(crate) const PP_IDENT: u8 = 14;
pub(crate) const PP_IFS: u8 = 15;
pub(crate) const PP_IFSSPACE: u8 = 16;
pub(crate) const PP_WORD: u8 = 17;
pub(crate) const PP_INCOMPLETE: u8 = 18;
pub(crate) const PP_INVALID: u8 = 19;
pub(crate) const PP_LAST: u8 = 19;
pub(crate) const PP_UNKWN: u8 = 20;
pub(crate) const PP_RANGE: u8 = 21;

const ZMB_VALID: i32 = 0;
const ZMB_INCOMPLETE: i32 = 1;
const ZMB_INVALID: i32 = 2;

const ZPC_CHARS: [u8; ZPC_COUNT] = [
    b'/', 0, BAR, OUTPAR, TILDE, INPAR, QUEST, STAR, INBRACK, INANG, HAT, POUND, BNULLKEEP, QUEST,
    STAR, b'+', BANG, b'!', b'@',
];

/// The strings `enable -p`/`disable -p` use.
pub(crate) const ZPC_STRINGS: [Option<&str>; ZPC_COUNT] = [
    None,
    None,
    Some("|"),
    None,
    Some("~"),
    Some("("),
    Some("?"),
    Some("*"),
    Some("["),
    Some("<"),
    Some("^"),
    Some("#"),
    None,
    Some("?("),
    Some("*("),
    Some("+("),
    Some("!("),
    Some("\\!("),
    Some("@("),
];

const COLON_STUFFS: [&str; 19] = [
    "alpha",
    "alnum",
    "ascii",
    "blank",
    "cntrl",
    "digit",
    "graph",
    "lower",
    "print",
    "punct",
    "space",
    "upper",
    "xdigit",
    "IDENT",
    "IFS",
    "IFSSPACE",
    "WORD",
    "INCOMPLETE",
    "INVALID",
];

const W: usize = 8;

/// A compiled pattern (zsh's `struct patprog` followed by its program).
#[derive(Debug, Clone)]
pub(crate) struct Patprog {
    /// The program cells, starting at cell 0 (zsh's `startoff`).
    code: Vec<u8>,
    mustoff: Option<usize>,
    patmlen: usize,
    pub(crate) globflags: i32,
    pub(crate) globend: i32,
    pub(crate) flags: i32,
    pub(crate) patnpar: usize,
    pub(crate) patstartch: u8,
    /// For PAT_PURES: the (metafied) string itself.
    pure: Vec<u8>,
}

impl Patprog {
    /// A program that matches nothing, as a placeholder.
    pub(crate) fn empty() -> Patprog {
        Patprog {
            code: vec![0; 2 * W],
            mustoff: None,
            patmlen: 0,
            globflags: 0,
            globend: 0,
            flags: PAT_PURES,
            patnpar: 0,
            patstartch: 0,
            pure: vec![MARKER],
        }
    }

    /// zsh's `dummy_patprog1`: matches anything (PAT_ANY).
    pub(crate) fn any() -> Patprog {
        let mut p = Patprog::empty();
        p.flags = PAT_ANY;
        p.pure.clear();
        p
    }

    /// The literal string a match must contain (zsh's `mustoff`), as the
    /// unmetafied bytes the program holds.
    pub(crate) fn must_string(&self) -> Option<Vec<u8>> {
        let off = self.mustoff?;
        Some(
            self.code
                .get(off..off + self.patmlen)
                .unwrap_or(&[])
                .to_vec(),
        )
    }

    /// The pure string of a PAT_PURES pattern.
    pub(crate) fn pure_string(&self) -> Option<&[u8]> {
        (self.flags & PAT_PURES != 0).then_some(self.pure.as_slice())
    }
}

fn rd(code: &[u8], cell: usize) -> i64 {
    let off = cell * W;
    code.get(off..off + W)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .map_or(0, i64::from_le_bytes)
}

fn wr(code: &mut Vec<u8>, cell: usize, v: i64) {
    let off = cell * W;
    if code.len() < off + W {
        code.resize(off + W, 0);
    }
    if let Some(slot) = code.get_mut(off..off + W) {
        slot.copy_from_slice(&v.to_le_bytes());
    }
}

fn p_op(code: &[u8], p: usize) -> i64 {
    rd(code, p) & 0xff
}

fn p_next_off(code: &[u8], p: usize) -> i64 {
    rd(code, p) >> 8
}

/// `PATNEXT(p)`.
fn patnext(code: &[u8], p: usize) -> Option<usize> {
    let off = p_next_off(code, p);
    if off == 0 {
        return None;
    }
    let off = usize::try_from(off.unsigned_abs()).unwrap_or(0);
    if p_op(code, p) == P_BACK {
        p.checked_sub(off)
    } else {
        Some(p + off)
    }
}

fn p_isbranch(code: &[u8], p: usize) -> bool {
    rd(code, p) & 0x20 != 0
}

fn p_isexclude(code: &[u8], p: usize) -> bool {
    rd(code, p) & 0x30 == 0x30
}

fn p_notdot(code: &[u8], p: usize) -> bool {
    rd(code, p) & 0x40 != 0
}

/// `P_LS_LEN(p)` and the byte offset of `P_LS_STR(p)`.
fn p_ls(code: &[u8], p: usize) -> (usize, usize) {
    (usize::try_from(rd(code, p + 1)).unwrap_or(0), (p + 2) * W)
}

/// The compiler's state.
struct Comp {
    out: Vec<u8>,
    /// Bytes emitted (zsh's `patsize`), always a whole number of cells
    /// except while adding unaligned bytes.
    size: usize,
    pat: Vec<u8>,
    parse: usize,
    npar: usize,
    flags: i32,
    globflags: i32,
    special: [u8; ZPC_COUNT],
    header_globflags: i32,
}

impl Comp {
    fn c(&self, off: usize) -> u8 {
        at(&self.pat, self.parse + off)
    }

    fn metacharinc(&mut self) {
        self.parse += if self.c(0) == META { 2 } else { 1 };
    }

    fn add_bytes(&mut self, bytes: &[u8], align: bool) {
        let start = self.size;
        let mut newsize = self.size + bytes.len();
        if align {
            newsize = (newsize + W - 1) & !(W - 1);
        }
        if self.out.len() < newsize {
            self.out.resize(newsize, 0);
        }
        for (k, &b) in bytes.iter().enumerate() {
            if let Some(slot) = self.out.get_mut(start + k) {
                *slot = b;
            }
        }
        for k in start + bytes.len()..newsize {
            if let Some(slot) = self.out.get_mut(k) {
                *slot = 0;
            }
        }
        self.size = newsize;
    }

    fn add_cell(&mut self, v: i64) {
        self.add_bytes(&v.to_le_bytes(), true);
    }

    /// `patnode`: emit a node, returning its cell.
    fn node(&mut self, op: i64) -> usize {
        let cell = self.size / W;
        self.add_cell(op);
        cell
    }

    /// `patinsert`: put `op` (and `xtra` cells) before the node at `opnd`.
    fn insert(&mut self, op: i64, opnd: usize, xtra: &[i64]) {
        let n = 1 + xtra.len();
        let at_byte = opnd * W;
        let mut cells: Vec<u8> = Vec::with_capacity(n * W);
        cells.extend_from_slice(&op.to_le_bytes());
        for x in xtra {
            cells.extend_from_slice(&x.to_le_bytes());
        }
        let tail: Vec<u8> = self.out.get(at_byte..self.size).unwrap_or(&[]).to_vec();
        self.out.truncate(at_byte);
        self.out.extend(cells);
        self.out.extend(tail);
        self.size += n * W;
    }

    /// `pattail`: link the end of the chain from `p` to `val`.
    fn tail(&mut self, p: usize, val: usize) {
        let mut scan = p;
        while let Some(t) = patnext(&self.out, scan) {
            scan = t;
        }
        let offset: i64 = if p_op(&self.out, scan) == P_BACK {
            i64::try_from(scan).unwrap_or(0) - i64::try_from(val).unwrap_or(0)
        } else {
            i64::try_from(val).unwrap_or(0) - i64::try_from(scan).unwrap_or(0)
        };
        let cur = rd(&self.out, scan);
        wr(&mut self.out, scan, cur | (offset << 8));
    }

    /// `patoptail`.
    fn optail(&mut self, p: usize, val: usize) {
        if p == 0 || !p_isbranch(&self.out, p) {
            return;
        }
        if p_op(&self.out, p) == P_BRANCH {
            self.tail(p + 1, val);
        } else {
            self.tail(p + 2, val);
        }
    }
}

impl Shell {
    /// zsh's `patcompcharsset`.
    fn patcompcharsset(&self, special: &mut [u8; ZPC_COUNT]) {
        *special = ZPC_CHARS;
        for (i, &d) in self.zpc_disables.iter().enumerate() {
            if d && let Some(s) = special.get_mut(i) {
                *s = MARKER;
            }
        }
        if !self.isset(EXTENDEDGLOB) {
            special[ZPC_TILDE] = MARKER;
            special[ZPC_HAT] = MARKER;
            special[ZPC_HASH] = MARKER;
        }
        if !self.isset(KSHGLOB) {
            for k in [
                ZPC_KSH_QUEST,
                ZPC_KSH_STAR,
                ZPC_KSH_PLUS,
                ZPC_KSH_BANG,
                ZPC_KSH_BANG2,
                ZPC_KSH_AT,
            ] {
                if let Some(s) = special.get_mut(k) {
                    *s = MARKER;
                }
            }
        }
        if self.isset(SHGLOB) {
            special[ZPC_INPAR] = MARKER;
            special[ZPC_INANG] = MARKER;
        }
    }

    /// zsh's `patcompstart`: set up for compiling file name segments.
    pub(crate) fn patcompstart(&mut self) {
        let mut special = [0u8; ZPC_COUNT];
        self.patcompcharsset(&mut special);
        self.pat_file_special = special;
        self.pat_file_globflags = if self.isset(CASEGLOB) || self.isset(CASEPATHS) {
            0
        } else {
            GF_IGNCASE
        };
        if self.isset(MULTIBYTE) {
            self.pat_file_globflags |= GF_MULTIBYTE;
        }
    }

    /// zsh's `patcompile`: compile `exp` (tokenized, metafied). With
    /// `endexp`, the index where compilation stopped is stored there.
    pub(crate) fn patcompile(
        &mut self,
        exp: &[u8],
        inflags: i32,
        endexp: Option<&mut usize>,
    ) -> Option<Patprog> {
        let mut exp = exp.to_vec();
        let mut comp = Comp {
            out: Vec::with_capacity(256),
            size: 0,
            pat: Vec::new(),
            parse: 0,
            npar: 1,
            flags: inflags & !(PAT_PURES | PAT_HAS_EXCLUDP),
            globflags: 0,
            special: [0; ZPC_COUNT],
            header_globflags: 0,
        };
        // Cell 0 is the header, as zsh's struct patprog is: no node is ever
        // at offset 0, which the compiler uses to mean "none".
        comp.add_cell(0);
        if comp.flags & PAT_FILE == 0 {
            self.patcompcharsset(&mut comp.special);
            comp.special[ZPC_SLASH] = MARKER;
            crate::utils::remnulargs(&mut exp);
            comp.globflags = if self.isset(MULTIBYTE) {
                GF_MULTIBYTE
            } else {
                0
            };
        } else {
            comp.special = self.pat_file_special;
            comp.globflags = self.pat_file_globflags;
        }
        if comp.flags & PAT_LCMATCHUC != 0 {
            comp.globflags |= GF_LCMATCHUC;
        }
        comp.header_globflags = comp.globflags;
        comp.pat = exp.clone();
        let mut flags = 0;
        let mut len = 0usize;
        let mut strp: Option<usize> = None;
        let mut pure_start = 0usize;
        if comp.flags & PAT_ANY == 0 {
            if comp.globflags & !GF_MULTIBYTE == 0 {
                let mut s = 0;
                if at(&exp, 0) == NULARG {
                    s = 1;
                }
                pure_start = s;
                let mut e = s;
                while e < exp.len()
                    && (comp.flags & PAT_FILE == 0 || at(&exp, e) != b'/')
                    && !tok::is_tok(at(&exp, e))
                {
                    e += 1;
                }
                strp = Some(e);
            }
            match strp {
                Some(e) if e >= exp.len() || at(&exp, e) == b'/' => {
                    comp.parse = e;
                    len = e - pure_start;
                    comp.flags |= PAT_PURES;
                }
                _ => {
                    strp = None;
                    comp.parse = 0;
                    self.patcompswitch(&mut comp, false, &mut flags)?;
                }
            }
        }
        let mut p = Patprog {
            code: Vec::new(),
            mustoff: None,
            patmlen: len,
            globflags: comp.header_globflags,
            globend: comp.globflags,
            flags: comp.flags,
            patnpar: comp.npar - 1,
            patstartch: 0,
            pure: Vec::new(),
        };
        if comp.flags & PAT_FILE != 0 && !self.isset(CASEGLOB) && comp.flags & PAT_PURES == 0 {
            p.globflags |= GF_IGNCASE;
            p.globend |= GF_IGNCASE;
        }
        if strp.is_some() {
            p.pure = sub(&exp, pure_start, pure_start + len).to_vec();
        } else if comp.flags & PAT_ANY == 0 {
            let code = &comp.out;
            let pscan0 = 1usize;
            if patnext(code, pscan0).is_some_and(|n| p_op(code, n) == P_END) {
                let mut pscan = Some(pscan0 + 1);
                if flags & P_PURESTR != 0 {
                    p.flags |= PAT_PURES;
                    while let Some(ps) = pscan {
                        let next = patnext(code, ps);
                        if p_op(code, ps) == P_EXACTLY {
                            let (l, off) = p_ls(code, ps);
                            p.pure = tok::metafy(code.get(off..off + l).unwrap_or(&[]));
                            break;
                        }
                        pscan = next;
                    }
                    p.patmlen = p.pure.len();
                } else {
                    if let Some(ps) = pscan
                        && p_op(code, ps) == P_EXACTLY
                        && p.globflags == 0
                    {
                        let (l, off) = p_ls(code, ps);
                        if l > 0 {
                            p.patstartch = at(code, off);
                        }
                    }
                    if flags & P_HSTART != 0 && p.globflags == 0 {
                        let mut best: Option<(usize, usize)> = None;
                        let mut ps = pscan;
                        while let Some(x) = ps {
                            if p_op(code, x) == P_EXACTLY {
                                let (l, off) = p_ls(code, x);
                                if best.is_none_or(|(bl, _)| l >= bl) {
                                    best = Some((l, off));
                                }
                            }
                            ps = patnext(code, x);
                        }
                        if let Some((l, off)) = best {
                            p.mustoff = Some(off);
                            p.patmlen = l;
                        }
                    }
                }
            }
        }
        p.flags = if strp.is_some() { comp.flags } else { p.flags };
        p.code = comp.out;
        p.code.truncate(comp.size);
        if comp.flags & PAT_FILE != 0 {
            self.pat_file_globflags = comp.globflags;
            self.pat_file_special = comp.special;
        }
        if let Some(e) = endexp {
            *e = comp.parse;
        }
        Some(p)
    }

    /// zsh's `patcompswitch`.
    fn patcompswitch(&mut self, c: &mut Comp, paren: bool, flagp: &mut i32) -> Option<usize> {
        let savglobflags = c.globflags;
        let mut gfchanged = false;
        let mut excsync = 0usize;
        *flagp = 0;
        let mut parno = 0usize;
        let starter0 = if paren && c.globflags & GF_BACKREF != 0 && c.npar <= NSUBEXP {
            parno = c.npar;
            c.npar += 1;
            Some(c.node(P_OPEN + i64::try_from(parno).unwrap_or(0)))
        } else {
            None
        };
        let mut br = c.node(P_BRANCH);
        let mut flags = 0;
        self.patcompbranch(c, &mut flags, paren)?;
        if c.globflags != savglobflags {
            gfchanged = true;
        }
        let starter = match starter0 {
            Some(s) => {
                c.tail(s, br);
                s
            }
            None => br,
        };
        *flagp |= flags & (P_HSTART | P_PURESTR);
        loop {
            let ch = c.c(0);
            let is_bar = ch == ZPC_CHARS[ZPC_BAR];
            let is_tilde = ch == c.special[ZPC_TILDE] && (c.c(1) == b'/' || !Self::seg_end(c, 1));
            if !(is_bar || is_tilde) || c.parse >= c.pat.len() {
                break;
            }
            let mut tilde = if ch == c.special[ZPC_TILDE] { 1 } else { 0 };
            c.parse += 1;
            let mut gfnode = 0usize;
            *flagp &= !P_PURESTR;
            if tilde != 0 {
                if excsync == 0 {
                    excsync = c.node(P_EXCSYNC);
                    c.optail(br, excsync);
                }
                c.globflags &= !0xff;
                if c.flags & PAT_FILET == 0 || paren {
                    br = c.node(P_EXCLUDE);
                } else {
                    br = c.node(P_EXCLUDP);
                    c.flags |= PAT_HAS_EXCLUDP;
                }
                c.add_cell(0);
                if !paren && c.special[ZPC_SLASH] == b'/' {
                    tilde += 1;
                    c.special[ZPC_SLASH] = MARKER;
                }
            } else {
                excsync = 0;
                br = c.node(P_BRANCH);
                if !paren {
                    c.globflags = 0;
                    if c.header_globflags != 0 {
                        gfnode = c.node(P_GFLAGS);
                        let g = c.globflags;
                        c.add_cell(i64::from(g));
                    }
                } else {
                    c.globflags = savglobflags;
                }
            }
            let newbr = self.patcompbranch(c, &mut flags, paren);
            if tilde == 2 {
                c.special[ZPC_SLASH] = b'/';
            }
            let newbr = newbr?;
            if gfnode != 0 {
                c.tail(gfnode, newbr);
            }
            if tilde == 0 && c.globflags != savglobflags {
                gfchanged = true;
            }
            c.tail(starter, br);
            if excsync != 0 {
                let e = c.node(P_EXCEND);
                c.optail(br, e);
            }
            *flagp |= flags & P_HSTART;
        }
        let ender_op = if paren {
            if parno != 0 {
                P_CLOSE + i64::try_from(parno).unwrap_or(0)
            } else {
                P_NOTHING
            }
        } else {
            P_END
        };
        let ender = c.node(ender_op);
        c.tail(starter, ender);
        let mut ptr = Some(starter);
        while let Some(pp) = ptr {
            if !p_isexclude(&c.out, pp) {
                c.optail(pp, ender);
            }
            ptr = patnext(&c.out, pp);
        }
        if paren {
            if c.c(0) != OUTPAR || c.parse >= c.pat.len() {
                return None;
            }
            c.parse += 1;
        } else if c.parse < c.pat.len() && !(c.flags & PAT_FILE != 0 && c.c(0) == b'/') {
            return None;
        }
        if paren && gfchanged {
            let g = c.node(P_GFLAGS);
            c.tail(ender, g);
            c.globflags = savglobflags;
            c.add_cell(i64::from(savglobflags));
        }
        Some(starter)
    }

    /// Whether `c` at the compiler's position ends a segment: zsh's
    /// `memchr(zpc_special, *patparse, ZPC_SEG_COUNT)`, where the NUL of
    /// the end of the string is one of them.
    fn seg_end(c: &Comp, off: usize) -> bool {
        let pos = c.parse + off;
        if pos >= c.pat.len() {
            return true;
        }
        let ch = at(&c.pat, pos);
        ch != 0
            && c.special
                .get(..ZPC_SEG_COUNT)
                .is_some_and(|s| s.contains(&ch))
    }

    /// zsh's `patcompbranch`.
    fn patcompbranch(&mut self, c: &mut Comp, flagp: &mut i32, paren: bool) -> Option<usize> {
        *flagp = P_PURESTR;
        let mut starter = 0usize;
        let mut chain = 0usize;
        loop {
            let at_seg = Self::seg_end(c, 0);
            let tilde_ok = c.c(0) == c.special[ZPC_TILDE]
                && c.parse < c.pat.len()
                && c.c(1) != b'/'
                && Self::seg_end(c, 1);
            if at_seg && !tilde_ok {
                break;
            }
            let mut flags = 0;
            let latest;
            if (c.c(0) == c.special[ZPC_INPAR] && c.c(1) == c.special[ZPC_HASH])
                || (c.c(0) == c.special[ZPC_KSH_AT]
                    && c.c(1) == INPAR
                    && c.c(2) == c.special[ZPC_HASH])
            {
                let pp1 = c.parse;
                let oldglobflags = c.globflags;
                c.parse += if c.c(0) == b'@' { 3 } else { 2 };
                let (ok, assert, ignore) = self.patgetglobflags(c);
                if !ok {
                    return None;
                }
                if !ignore {
                    if assert != 0 {
                        latest = c.node(assert);
                        flags = 0;
                    } else {
                        if pp1 == 0 {
                            c.header_globflags = c.globflags;
                            continue;
                        } else if c.parse >= c.pat.len() {
                            break;
                        }
                        if oldglobflags != c.globflags {
                            latest = c.node(P_GFLAGS);
                            let g = c.globflags;
                            c.add_cell(i64::from(g));
                        } else {
                            continue;
                        }
                    }
                } else if c.parse >= c.pat.len() {
                    break;
                } else {
                    continue;
                }
            } else if c.c(0) == c.special[ZPC_HAT] && c.parse < c.pat.len() {
                c.parse += 1;
                latest = self.patcompnot(c, false, &mut flags)?;
            } else {
                latest = self.patcomppiece(c, &mut flags, paren)?;
            }
            if starter == 0 {
                starter = latest;
            }
            if flags & P_PURESTR == 0 {
                *flagp &= !P_PURESTR;
            }
            if chain == 0 {
                *flagp |= flags & P_HSTART;
            } else {
                c.tail(chain, latest);
            }
            chain = latest;
        }
        if chain == 0 {
            starter = c.node(P_NOTHING);
        }
        Some(starter)
    }

    /// zsh's `patgetglobflags`: `(ok, assertion node, ignore)`.
    fn patgetglobflags(&mut self, c: &mut Comp) -> (bool, i64, bool) {
        let mut assert = 0;
        let mut ignore = true;
        let start = c.parse;
        let mut p = c.parse;
        while p < c.pat.len() && at(&c.pat, p) != OUTPAR {
            let ch = at(&c.pat, p);
            if ch == b'q' {
                while p < c.pat.len() && at(&c.pat, p) != OUTPAR {
                    p += 1;
                }
                break;
            }
            ignore = false;
            match ch {
                b'a' => {
                    let (ret, used) = crate::utils::zstrtol(from(&c.pat, p + 1), 10);
                    if !(0..=254).contains(&ret) || used == 0 {
                        return (false, 0, ignore);
                    }
                    c.globflags = (c.globflags & !0xff) | i32::try_from(ret & 0xff).unwrap_or(0);
                    p += used;
                }
                b'l' => c.globflags = (c.globflags & !GF_IGNCASE) | GF_LCMATCHUC,
                b'i' => c.globflags = (c.globflags & !GF_LCMATCHUC) | GF_IGNCASE,
                b'I' => c.globflags &= !(GF_LCMATCHUC | GF_IGNCASE),
                b'b' => c.globflags |= GF_BACKREF,
                b'B' => c.globflags &= !GF_BACKREF,
                b'm' => c.globflags |= GF_MATCHREF,
                b'M' => c.globflags &= !GF_MATCHREF,
                b's' => assert = P_ISSTART,
                b'e' => assert = P_ISEND,
                b'u' => c.globflags |= GF_MULTIBYTE,
                b'U' => c.globflags &= !GF_MULTIBYTE,
                _ => return (false, 0, ignore),
            }
            p += 1;
        }
        if at(&c.pat, p) != OUTPAR || p >= c.pat.len() {
            return (false, 0, ignore);
        }
        if assert != 0 && at(&c.pat, start + 1) != OUTPAR {
            return (false, 0, ignore);
        }
        c.parse = p + 1;
        (true, assert, ignore)
    }

    /// zsh's `patcomppiece`.
    #[expect(clippy::too_many_lines, reason = "zsh's patcomppiece")]
    fn patcomppiece(&mut self, c: &mut Comp, flagp: &mut i32, paren: bool) -> Option<usize> {
        let mut flags = 0;
        let str0 = c.parse;
        let mut patprev = c.parse;
        let mut kshchar: i32;
        loop {
            kshchar = 0;
            if c.parse < c.pat.len() && c.c(1) == INPAR {
                let ch = c.c(0);
                if ch == c.special[ZPC_KSH_PLUS] {
                    kshchar = i32::from(b'+');
                } else if ch == c.special[ZPC_KSH_BANG] || ch == c.special[ZPC_KSH_BANG2] {
                    kshchar = i32::from(b'!');
                } else if ch == c.special[ZPC_KSH_AT] {
                    kshchar = i32::from(b'@');
                } else if ch == c.special[ZPC_KSH_STAR] {
                    kshchar = i32::from(b'*');
                } else if ch == c.special[ZPC_KSH_QUEST] {
                    kshchar = i32::from(b'?');
                }
            }
            if c.special[ZPC_INPAR] != MARKER || c.c(0) != OUTPAR || paren {
                let end = c.parse >= c.pat.len();
                let ch = c.c(0);
                let in_special = end
                    || (ch != 0
                        && c.special
                            .get(..ZPC_NO_KSH_GLOB)
                            .is_some_and(|s| s.contains(&ch)));
                if kshchar != 0
                    || (in_special
                        && (end
                            || ch != c.special[ZPC_TILDE]
                            || c.c(1) == b'/'
                            || !Self::seg_end(c, 1)))
                {
                    break;
                }
            }
            patprev = c.parse;
            c.metacharinc();
        }
        let starter;
        if c.parse > str0 {
            kshchar = 0;
            flags |= P_PURESTR;
            let morelen = patprev > str0;
            let hashnext = c.c(0) == c.special[ZPC_HASH]
                || (c.c(0) == c.special[ZPC_INPAR]
                    && c.c(1) == c.special[ZPC_HASH]
                    && c.c(2) == b'c')
                || (c.c(0) == c.special[ZPC_KSH_AT]
                    && c.c(1) == INPAR
                    && c.c(2) == c.special[ZPC_HASH]
                    && c.c(3) == b'c');
            if hashnext && morelen && c.parse < c.pat.len() {
                c.parse = patprev;
            }
            if !morelen {
                flags |= P_SIMPLE;
            }
            starter = c.node(P_EXACTLY);
            let mut s0 = str0;
            if at(&c.pat, s0) == NULARG {
                s0 += 1;
            }
            let mut raw = Vec::new();
            let mut k = s0;
            while k < c.parse {
                let b = at(&c.pat, k);
                if tok::is_tok(b) {
                    raw.push(tok::detok(b));
                } else if b == META {
                    k += 1;
                    raw.push(at(&c.pat, k) ^ 32);
                } else {
                    raw.push(b);
                }
                k += 1;
            }
            let slen = raw.len();
            c.add_cell(i64::try_from(slen).unwrap_or(0));
            c.add_bytes(&raw, true);
            if c.globflags & (0xff | GF_LCMATCHUC | GF_IGNCASE) != 0
                && (c.flags & PAT_FILE == 0
                    || !(at(&raw, 0) == b'.' && (slen == 1 || (at(&raw, 1) == b'.' && slen == 2))))
            {
                flags &= !P_PURESTR;
            }
        } else {
            if kshchar != 0 {
                c.parse += 1;
            }
            let patch = c.c(0);
            c.metacharinc();
            match patch {
                QUEST => {
                    flags |= P_SIMPLE;
                    starter = c.node(P_ANY);
                }
                STAR => {
                    kshchar = -1;
                    starter = c.node(P_STAR);
                }
                INBRACK => {
                    flags |= P_SIMPLE;
                    if c.c(0) == HAT || c.c(0) == BANG {
                        c.parse += 1;
                        starter = c.node(P_ANYBUT);
                    } else {
                        starter = c.node(P_ANYOF);
                    }
                    if c.c(0) == OUTBRACK && from(&c.pat, c.parse + 1).contains(&OUTBRACK) {
                        c.parse += 1;
                        c.add_bytes(b"]", false);
                    }
                    while c.parse < c.pat.len() && c.c(0) != OUTBRACK {
                        if c.c(0) == INBRACK && c.c(1) == b':' {
                            let rest = from(&c.pat, c.parse + 2);
                            if let Some(colon) = rest.iter().position(|&b| b == b':')
                                && at(rest, colon + 1) == OUTBRACK
                            {
                                let name = sub(rest, 0, colon);
                                let ch = range_type(name);
                                c.parse += 2 + colon + 2;
                                if ch != PP_UNKWN {
                                    c.add_bytes(&[META.wrapping_add(ch)], false);
                                }
                                continue;
                            }
                        }
                        let mut charstart = c.parse;
                        c.metacharinc();
                        if c.c(0) == DASH && c.parse + 1 < c.pat.len() && c.c(1) != OUTBRACK {
                            c.add_bytes(&[META.wrapping_add(PP_RANGE)], false);
                            let cs = at(&c.pat, charstart);
                            if tok::is_tok(cs) {
                                c.add_bytes(&[tok::detok(cs)], false);
                            } else {
                                let bytes = sub(&c.pat, charstart, c.parse).to_vec();
                                c.add_bytes(&bytes, false);
                            }
                            c.parse += 1;
                            charstart = c.parse;
                            c.metacharinc();
                        }
                        let cs = at(&c.pat, charstart);
                        if tok::is_tok(cs) {
                            c.add_bytes(&[tok::detok(cs)], false);
                        } else {
                            let bytes = sub(&c.pat, charstart, c.parse).to_vec();
                            c.add_bytes(&bytes, false);
                        }
                    }
                    if c.c(0) != OUTBRACK || c.parse >= c.pat.len() {
                        return None;
                    }
                    c.parse += 1;
                    c.add_bytes(&[0], true);
                }
                INPAR => {
                    let mut flags2 = 0;
                    if kshchar == i32::from(b'!') {
                        starter = self.patcompnot(c, true, &mut flags2)?;
                    } else {
                        starter = self.patcompswitch(c, true, &mut flags2)?;
                    }
                    flags |= flags2 & P_HSTART;
                }
                INANG => {
                    let mut len = 0;
                    let mut from_v: i64 = 0;
                    let mut to_v: i64 = 0;
                    if c.c(0).is_ascii_digit() {
                        let (v, used) = crate::utils::zstrtol(from(&c.pat, c.parse), 10);
                        from_v = v;
                        c.parse += used;
                        len |= 1;
                    }
                    c.parse += 1;
                    if c.c(0).is_ascii_digit() {
                        let (v, used) = crate::utils::zstrtol(from(&c.pat, c.parse), 10);
                        to_v = v;
                        c.parse += used;
                        len |= 2;
                    }
                    if c.c(0) != OUTANG {
                        return None;
                    }
                    c.parse += 1;
                    match len {
                        3 => {
                            starter = c.node(P_NUMRNG);
                            c.add_cell(from_v);
                            c.add_cell(to_v);
                        }
                        2 => {
                            starter = c.node(P_NUMTO);
                            c.add_cell(to_v);
                        }
                        1 => {
                            starter = c.node(P_NUMFROM);
                            c.add_cell(from_v);
                        }
                        _ => starter = c.node(P_NUMANY),
                    }
                }
                POUND => return None,
                BNULLKEEP => {
                    let next = self.patcomppiece(c, flagp, paren);
                    *flagp &= !P_PURESTR;
                    return next;
                }
                _ => return None,
            }
        }
        let hash = c.c(0) == c.special[ZPC_HASH] && c.parse < c.pat.len();
        let count = !hash
            && ((c.c(0) == c.special[ZPC_INPAR]
                && c.c(1) == c.special[ZPC_HASH]
                && c.c(2) == b'c')
                || (c.c(0) == c.special[ZPC_KSH_AT]
                    && c.c(1) == INPAR
                    && c.c(2) == c.special[ZPC_HASH]
                    && c.c(3) == b'c'));
        if !hash
            && !count
            && (kshchar <= 0 || kshchar == i32::from(b'@') || kshchar == i32::from(b'!'))
        {
            *flagp = flags;
            return Some(starter);
        }
        if kshchar != 0 && (hash || count) {
            return None;
        }
        let op: i64;
        if kshchar == i32::from(b'*') {
            op = P_ONEHASH;
            *flagp = P_HSTART;
        } else if kshchar == i32::from(b'+') {
            op = P_TWOHASH;
            *flagp = P_HSTART;
        } else if kshchar == i32::from(b'?') {
            op = 0;
            *flagp = 0;
        } else if count {
            op = P_COUNT;
            c.parse += 3;
            *flagp = P_HSTART;
        } else {
            c.parse += 1;
            if c.c(0) == c.special[ZPC_HASH] && c.parse < c.pat.len() {
                op = P_TWOHASH;
                c.parse += 1;
            } else {
                op = P_ONEHASH;
            }
            *flagp = P_HSTART;
        }
        if op == P_COUNT {
            let opp = c.parse;
            let (mut min, used) = crate::utils::zstrtol(from(&c.pat, c.parse), 10);
            c.parse += used;
            if c.parse == opp {
                min = 0;
            }
            let max = if c.c(0) != b',' && c.c(0) != COMMA {
                if c.c(0) != OUTPAR {
                    return None;
                }
                min
            } else {
                c.parse += 1;
                let opp = c.parse;
                let (m, used) = crate::utils::zstrtol(from(&c.pat, c.parse), 10);
                c.parse += used;
                if c.c(0) != OUTPAR {
                    return None;
                }
                if c.parse == opp { -1 } else { m }
            };
            c.parse += 1;
            // P_COUNTSTART, then P_COUNT with its four arguments, before the
            // operand.
            c.insert(P_COUNTSTART, starter, &[P_COUNT, 0, min, max, 0]);
            let opnd = starter + 1 + P_CT_OPERAND;
            let back = c.node(P_BACK);
            c.tail(opnd, back);
            c.tail(opnd, starter + 1);
            let next = c.node(P_NOTHING);
            c.tail(starter, next);
            c.tail(starter + 1, next);
        } else if flags & P_SIMPLE != 0
            && (op == P_ONEHASH || op == P_TWOHASH)
            && p_op(&c.out, starter) == P_ANY
        {
            let cur = rd(&c.out, starter);
            if op == P_TWOHASH {
                wr(&mut c.out, starter, (cur & !0xff) | P_ANY);
                let s = c.node(P_STAR);
                c.tail(starter, s);
            } else {
                wr(&mut c.out, starter, (cur & !0xff) | P_STAR);
            }
        } else if flags & P_SIMPLE != 0 && op != 0 && c.globflags & 0xff == 0 {
            c.insert(op, starter, &[]);
        } else if op == P_ONEHASH {
            c.insert(P_WBRANCH, starter, &[0]);
            let b = c.node(P_BACK);
            c.optail(starter, b);
            c.optail(starter, starter);
            let br = c.node(P_BRANCH);
            c.tail(starter, br);
            let n = c.node(P_NOTHING);
            c.tail(starter, n);
        } else if op == P_TWOHASH {
            let next = c.node(P_WBRANCH);
            c.add_cell(0);
            c.tail(starter, next);
            let b = c.node(P_BACK);
            c.tail(b, starter);
            let br = c.node(P_BRANCH);
            c.tail(next, br);
            let n = c.node(P_NOTHING);
            c.tail(starter, n);
        } else if kshchar == i32::from(b'?') {
            c.insert(P_BRANCH, starter, &[]);
            let br = c.node(P_BRANCH);
            c.tail(starter, br);
            let next = c.node(P_NOTHING);
            c.tail(starter, next);
            c.optail(starter, next);
        }
        if c.c(0) == c.special[ZPC_HASH] && c.parse < c.pat.len() {
            return None;
        }
        Some(starter)
    }

    /// zsh's `patcompnot`.
    fn patcompnot(&mut self, c: &mut Comp, paren: bool, flagsp: &mut i32) -> Option<usize> {
        *flagsp = P_HSTART;
        let starter = c.node(P_BRANCH);
        let br = c.node(P_STAR);
        let excsync = c.node(P_EXCSYNC);
        c.tail(br, excsync);
        let excl = c.node(P_EXCLUDE);
        c.tail(starter, excl);
        c.add_cell(0);
        let mut dummy = 0;
        let br = if paren {
            self.patcompswitch(c, true, &mut dummy)?
        } else {
            self.patcompbranch(c, &mut dummy, false)?
        };
        let e = c.node(P_EXCEND);
        c.tail(br, e);
        let n = c.node(P_NOTHING);
        c.tail(excsync, n);
        c.tail(excl, n);
        Some(starter)
    }
}

/// zsh's `range_type`.
pub(crate) fn range_type(name: &[u8]) -> u8 {
    COLON_STUFFS
        .iter()
        .position(|s| s.as_bytes() == name)
        .and_then(|p| u8::try_from(p).ok())
        .map_or(PP_UNKWN, |p| p + PP_FIRST)
}

/// zsh's `pattern_range_to_string`.
pub(crate) fn pattern_range_to_string(range: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < range.len() {
        let c = at(range, i);
        if tok::is_meta(c) && c != 0 {
            let swtype = c.wrapping_sub(META);
            if swtype == 0 {
                out.push(META);
                out.push(at(range, i + 1) ^ 32);
                i += 2;
            } else if swtype == PP_RANGE {
                i += 1;
                for k in 0..2 {
                    if at(range, i) == META {
                        out.push(META);
                        out.push(at(range, i + 1));
                        i += 2;
                    } else {
                        out.push(at(range, i));
                        i += 1;
                    }
                    if k == 0 {
                        out.push(b'-');
                    }
                }
            } else if (PP_FIRST..=PP_LAST).contains(&swtype) {
                out.extend_from_slice(b"[:");
                out.extend_from_slice(
                    COLON_STUFFS
                        .get(usize::from(swtype - PP_FIRST))
                        .copied()
                        .unwrap_or("")
                        .as_bytes(),
                );
                out.extend_from_slice(b":]");
                i += 1;
            } else {
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// `$match`, `$mbegin` and `$mend`.
pub(crate) type Backrefs = (Vec<Vec<u8>>, Vec<Vec<u8>>, Vec<Vec<u8>>);

/// The result of running a pattern: where the match ended and the
/// references it asks to be set.
#[derive(Debug, Default)]
pub(crate) struct MatchInfo {
    /// Length of the match in metafied bytes (zsh's `patinlen`).
    pub(crate) len: usize,
    /// Character offset of the end (for `endp`).
    pub(crate) end_chars: usize,
    /// `$MATCH`, `$MBEGIN`, `$MEND` for `(#m)`.
    pub(crate) matchref: Option<(Vec<u8>, i64, i64)>,
    /// `$match`, `$mbegin`, `$mend` for `(#b)`.
    pub(crate) backrefs: Option<Backrefs>,
    /// Positions of each backreference, character offsets (-1 unset).
    pub(crate) positions: Vec<(i64, i64)>,
}

/// The matcher's state (zsh's `struct rpat` and the file-scope variables).
struct Run<'a> {
    sh: &'a Shell,
    code: Vec<u8>,
    /// Sync strings for P_WBRANCH/P_EXCLUDE, referred to from their cells
    /// by index + 1.
    syncs: Vec<Vec<u8>>,
    /// The unmetafied test string, with the path prefix before it.
    buf: Vec<u8>,
    patinstart: usize,
    patinend: usize,
    patinput: usize,
    patinpath: Option<usize>,
    beginp: [usize; NSUBEXP],
    endp: [usize; NSUBEXP],
    parsfound: u32,
    globdots: bool,
    patflags: i32,
    patglobflags: i32,
    errsfound: i32,
    forceerrs: i32,
    /// Byte offsets into `code` of an exact string partly matched.
    exactpos: Option<usize>,
    exactend: usize,
}

impl Run<'_> {
    fn b(&self, i: usize) -> u8 {
        at(&self.buf, i)
    }

    /// `CHARREF` on the test string: `(char, zmb indication)`.
    fn charref_buf(&self, x: usize, y: usize) -> (u32, i32) {
        charref(sub(&self.buf, x, y), self.patglobflags)
    }

    fn charnext_buf(&self, x: usize, y: usize) -> usize {
        x + charlen(sub(&self.buf, x, y), self.patglobflags)
    }

    fn charmatch(&self, chin: u32, chpa: u32) -> bool {
        charmatch(chin, chpa, self.patglobflags)
    }

    #[expect(clippy::too_many_lines, reason = "zsh's patmatch")]
    fn patmatch(&mut self, prog: usize) -> bool {
        let mut scan = Some(prog);
        let mut fail = false;
        while let Some(sc) = scan {
            if self.sh.errflag() {
                break;
            }
            let mut next = patnext(&self.code, sc);
            if !self.globdots
                && p_notdot(&self.code, sc)
                && self.patinput == self.patinstart
                && self.patinput < self.patinend
                && self.b(self.patinput) == b'.'
            {
                return false;
            }
            let op = p_op(&self.code, sc);
            match op {
                P_ANY => {
                    if self.patinput == self.patinend {
                        fail = true;
                    } else {
                        self.patinput = self.charnext_buf(self.patinput, self.patinend);
                    }
                }
                P_EXACTLY => {
                    let (mut chrop, chrend) = match self.exactpos {
                        Some(ep) => (ep, self.exactend),
                        None => {
                            let (l, off) = p_ls(&self.code, sc);
                            (off, off + l)
                        }
                    };
                    self.exactpos = None;
                    while chrop < chrend && self.patinput < self.patinend {
                        let savpatinput = self.patinput;
                        let savchrop = chrop;
                        let (chin, lin, badin) = charrefinc(
                            sub(&self.buf, self.patinput, self.patinend),
                            self.patglobflags,
                        );
                        let (chpa, lpa, badpa) =
                            charrefinc(sub(&self.code, chrop, chrend), self.patglobflags);
                        self.patinput += lin;
                        chrop += lpa;
                        if !self.charmatch(chin, chpa) || badin != badpa {
                            fail = true;
                            self.patinput = savpatinput;
                            chrop = savchrop;
                            break;
                        }
                    }
                    if chrop < chrend {
                        self.exactpos = Some(chrop);
                        self.exactend = chrend;
                        fail = true;
                    }
                }
                P_ANYOF | P_ANYBUT => {
                    if self.patinput == self.patinend {
                        fail = true;
                    } else {
                        let (cr, zmb) = self.charref_buf(self.patinput, self.patinend);
                        let range = nul_terminated(from(&self.code, (sc + 1) * W));
                        let hit = if self.patglobflags & GF_MULTIBYTE != 0 {
                            mb_patmatchrange(self.sh, &range, cr, zmb).0
                        } else {
                            patmatchrange(self.sh, &range, cr).0
                        };
                        if hit ^ (op == P_ANYOF) {
                            fail = true;
                        } else {
                            self.patinput = self.charnext_buf(self.patinput, self.patinend);
                        }
                    }
                }
                P_NUMRNG | P_NUMFROM | P_NUMTO => {
                    let mut cell = sc + 1;
                    let mut from_v: i64 = 0;
                    let mut to_v: i64 = 0;
                    if op != P_NUMTO {
                        from_v = rd(&self.code, cell);
                        cell += 1;
                    }
                    if op != P_NUMFROM {
                        to_v = rd(&self.code, cell);
                    }
                    let start = self.patinput;
                    let mut compend = self.patinput;
                    let mut comp: i64 = 0;
                    while self.patinput < self.patinend && self.b(self.patinput).is_ascii_digit() {
                        let digit = i64::from(self.b(self.patinput) - b'0');
                        let mut out_of_range = false;
                        if comp > i64::MAX / 10 {
                            out_of_range = true;
                        } else {
                            let c10 = comp * 10;
                            if i64::MAX - c10 < digit {
                                out_of_range = true;
                            } else {
                                comp = c10 + digit;
                            }
                        }
                        self.patinput += 1;
                        compend += 1;
                        if (out_of_range || comp & (1i64 << 62) != 0) && op == P_NUMFROM {
                            while self.patinput < self.patinend
                                && self.b(self.patinput).is_ascii_digit()
                            {
                                self.patinput += 1;
                            }
                        }
                    }
                    let mut save = self.patinput;
                    let mut no = 0;
                    while self.patinput > start {
                        if comp < from_v && self.patinput <= compend {
                            break;
                        }
                        if (op == P_NUMFROM || comp <= to_v)
                            && let Some(nx) = next
                            && self.patmatch(nx)
                        {
                            return true;
                        }
                        if no == 0
                            && next.is_some_and(|nx| {
                                p_op(&self.code, nx) == P_EXACTLY && {
                                    let (l, off) = p_ls(&self.code, nx);
                                    l == 0 || !at(&self.code, off).is_ascii_digit()
                                }
                            })
                            && self.patglobflags & 0xff == 0
                        {
                            return false;
                        }
                        save -= 1;
                        self.patinput = save;
                        no += 1;
                        if self.patinput < compend {
                            comp /= 10;
                        }
                    }
                    self.patinput = start;
                    fail = true;
                }
                P_NUMANY => {
                    let start = self.patinput;
                    while self.patinput < self.patinend && self.b(self.patinput).is_ascii_digit() {
                        self.patinput += 1;
                    }
                    let mut save = self.patinput;
                    let mut no = 0;
                    while self.patinput > start {
                        if let Some(nx) = next
                            && self.patmatch(nx)
                        {
                            return true;
                        }
                        if no == 0
                            && next.is_some_and(|nx| {
                                p_op(&self.code, nx) == P_EXACTLY && {
                                    let (l, off) = p_ls(&self.code, nx);
                                    l == 0 || !at(&self.code, off).is_ascii_digit()
                                }
                            })
                            && self.patglobflags & 0xff == 0
                        {
                            return false;
                        }
                        save -= 1;
                        self.patinput = save;
                        no += 1;
                    }
                    self.patinput = start;
                    fail = true;
                }
                P_NOTHING | P_BACK => {}
                P_GFLAGS => self.patglobflags = i32::try_from(rd(&self.code, sc + 1)).unwrap_or(0),
                o if (P_OPEN..P_OPEN + 10).contains(&o) => {
                    let no = usize::try_from(o - P_OPEN).unwrap_or(0);
                    let save = self.patinput;
                    if next.is_some_and(|nx| self.patmatch(nx)) {
                        if no != 0 && self.parsfound & (1 << (no - 1)) == 0 {
                            if let Some(slot) = self.beginp.get_mut(no - 1) {
                                *slot = save;
                            }
                            self.parsfound |= 1 << (no - 1);
                        }
                        return true;
                    }
                    return false;
                }
                o if (P_CLOSE..P_CLOSE + 10).contains(&o) => {
                    let no = usize::try_from(o - P_CLOSE).unwrap_or(0);
                    let save = self.patinput;
                    if next.is_some_and(|nx| self.patmatch(nx)) {
                        if no != 0 && self.parsfound & (1 << (no + 15)) == 0 {
                            if let Some(slot) = self.endp.get_mut(no - 1) {
                                *slot = save;
                            }
                            self.parsfound |= 1 << (no + 15);
                        }
                        return true;
                    }
                    return false;
                }
                P_EXCSYNC => {
                    let after = sc + 1;
                    let sid = usize::try_from(rd(&self.code, after + 1)).unwrap_or(0);
                    let pos = self.patinput - self.patinstart;
                    let errs = self.errsfound;
                    if let Some(sync) = sid.checked_sub(1).and_then(|k| self.syncs.get_mut(k)) {
                        let cur = at(sync, pos);
                        if cur != 0 && errs + 1 >= i32::from(cur) {
                            return false;
                        }
                        if let Some(slot) = sync.get_mut(pos) {
                            *slot = u8::try_from(errs + 1).unwrap_or(255);
                        }
                    }
                }
                P_EXCEND => {
                    fail = self.patinput < self.patinend;
                    if !fail {
                        return true;
                    }
                }
                P_BRANCH | P_WBRANCH => {
                    if !next.is_some_and(|nx| p_isbranch(&self.code, nx)) {
                        next = Some(sc + 1);
                    } else {
                        let mut scan2 = Some(sc);
                        let mut nxt = next;
                        while let Some(s2) = scan2 {
                            let save = self.patinput;
                            let savglobflags = self.patglobflags;
                            let saverrsfound = self.errsfound;
                            if nxt.is_some_and(|n| p_isexclude(&self.code, n)) {
                                let nx = nxt.unwrap_or(0);
                                let syncstrp = nx + 1;
                                let oldsync = rd(&self.code, syncstrp);
                                self.syncs
                                    .push(vec![0u8; self.patinend - self.patinstart + 1]);
                                let sid = self.syncs.len();
                                wr(&mut self.code, syncstrp, i64::try_from(sid).unwrap_or(0));
                                let origpatinend = self.patinend;
                                let savparsfound = self.parsfound;
                                let mut matchpt = 0usize;
                                let mut matchederrs = 0;
                                let mut ret;
                                loop {
                                    ret = self.patmatch(s2 + 1);
                                    if !ret {
                                        break;
                                    }
                                    let savforce = self.forceerrs;
                                    let savpatflags = self.patflags;
                                    self.forceerrs = -1;
                                    let savglobdots = self.globdots;
                                    matchederrs = self.errsfound;
                                    matchpt = self.patinput;
                                    self.globdots = true;
                                    let synclen = self
                                        .syncs
                                        .get(sid - 1)
                                        .and_then(|s| s.iter().position(|&b| b != 0))
                                        .unwrap_or(0);
                                    if self.patinstart + synclen != self.patinend {
                                        self.patinend = self.patinstart + synclen;
                                        self.patflags |= PAT_NOTEND;
                                    }
                                    let savpatinstart = self.patinstart;
                                    let mut en = patnext(&self.code, s2);
                                    while let Some(e) = en {
                                        if !p_isexclude(&self.code, e) {
                                            break;
                                        }
                                        self.patinput = save;
                                        self.patglobflags &= !0xff;
                                        self.errsfound = 0;
                                        let opnd = e + 2;
                                        if p_op(&self.code, e) == P_EXCLUDP
                                            && let Some(pp) = self.patinpath
                                        {
                                            self.patinput = pp;
                                            self.patinstart = pp;
                                        }
                                        if self.patmatch(opnd) {
                                            ret = false;
                                            self.parsfound = savparsfound;
                                        }
                                        if self.patinpath.is_some() {
                                            self.patinput =
                                                savpatinstart + (self.patinput - self.patinstart);
                                            self.patinstart = savpatinstart;
                                        }
                                        if !ret {
                                            break;
                                        }
                                        en = patnext(&self.code, e);
                                    }
                                    self.patinend = origpatinend;
                                    self.patflags = savpatflags;
                                    self.globdots = savglobdots;
                                    self.forceerrs = savforce;
                                    if ret {
                                        break;
                                    }
                                    self.patinput = save;
                                    self.patglobflags = savglobflags;
                                    self.errsfound = saverrsfound;
                                }
                                wr(&mut self.code, syncstrp, oldsync);
                                if ret {
                                    self.patinput = matchpt;
                                    self.errsfound = matchederrs;
                                    return true;
                                }
                                scan2 = patnext(&self.code, s2);
                                while let Some(s3) = scan2 {
                                    if !p_isexclude(&self.code, s3) {
                                        break;
                                    }
                                    scan2 = patnext(&self.code, s3);
                                }
                            } else {
                                let mut ret = true;
                                let mut pfree = false;
                                let opnd;
                                if p_op(&self.code, s2) == P_WBRANCH {
                                    let ptrp = s2 + 1;
                                    opnd = s2 + 2;
                                    if rd(&self.code, ptrp) == 0 {
                                        self.syncs
                                            .push(vec![0u8; self.patinend - self.patinstart + 1]);
                                        let sid = self.syncs.len();
                                        wr(&mut self.code, ptrp, i64::try_from(sid).unwrap_or(0));
                                        pfree = true;
                                    }
                                    let sid = usize::try_from(rd(&self.code, ptrp)).unwrap_or(0);
                                    let pos = self.patinput - self.patinstart;
                                    let errs = self.errsfound;
                                    if let Some(sync) =
                                        sid.checked_sub(1).and_then(|k| self.syncs.get_mut(k))
                                    {
                                        let cur = at(sync, pos);
                                        if cur != 0 && errs + 1 >= i32::from(cur) {
                                            ret = false;
                                        }
                                        if let Some(slot) = sync.get_mut(pos) {
                                            *slot = u8::try_from(errs + 1).unwrap_or(255);
                                        }
                                    }
                                    if ret {
                                        ret = self.patmatch(opnd);
                                    }
                                    if pfree {
                                        wr(&mut self.code, ptrp, 0);
                                    }
                                } else {
                                    opnd = s2 + 1;
                                    ret = self.patmatch(opnd);
                                }
                                if ret {
                                    return true;
                                }
                                scan2 = patnext(&self.code, s2);
                            }
                            self.patinput = save;
                            self.patglobflags = savglobflags;
                            self.errsfound = saverrsfound;
                            nxt = scan2.and_then(|s| patnext(&self.code, s));
                            if !scan2.is_some_and(|s| p_isbranch(&self.code, s)) {
                                break;
                            }
                        }
                        return false;
                    }
                }
                P_STAR | P_ONEHASH | P_TWOHASH => {
                    let mut sc = sc;
                    if op == P_STAR {
                        while next.is_some_and(|n| p_op(&self.code, n) == P_STAR) {
                            sc = next.unwrap_or(sc);
                            next = patnext(&self.code, sc);
                        }
                    }
                    let op = p_op(&self.code, sc);
                    let start = self.patinput;
                    let mut charstart = vec![false; self.patinend - self.patinput + 1];
                    let mut no: i64;
                    if op == P_STAR {
                        no = 0;
                        while self.patinput < self.patinend {
                            if let Some(slot) = charstart.get_mut(self.patinput - start) {
                                *slot = true;
                            }
                            no += 1;
                            self.patinput = self.charnext_buf(self.patinput, self.patinend);
                        }
                        if next.is_some_and(|n| p_op(&self.code, n) == P_END) {
                            return true;
                        }
                    } else {
                        if !self.globdots
                            && p_notdot(&self.code, sc + 1)
                            && self.patinput == self.patinstart
                            && self.patinput < self.patinend
                            && self.charref_buf(self.patinput, self.patinend).0 == u32::from(b'.')
                        {
                            return false;
                        }
                        no = self.patrepeat(sc + 1, &mut charstart);
                    }
                    let min = i64::from(op == P_TWOHASH);
                    let mut nextch: Option<u32> = None;
                    if let Some(nx) = next
                        && p_op(&self.code, nx) == P_EXACTLY
                        && p_ls(&self.code, nx).0 != 0
                        && self.patglobflags & 0xff == 0
                    {
                        let (l, off) = p_ls(&self.code, nx);
                        if patnext(&self.code, nx).is_some_and(|n2| p_op(&self.code, n2) == P_END)
                            && self.patflags & PAT_NOANCH == 0
                        {
                            let ptlen = self.patinend - self.patinput;
                            let base = if min != 0 {
                                self.charnext_buf(start, self.patinend)
                            } else {
                                start
                            };
                            let lenmatch = self.patinend.saturating_sub(base);
                            if l > lenmatch || l < ptlen {
                                return false;
                            }
                            self.patinput += ptlen - l;
                            scan = next;
                            continue;
                        }
                        nextch = Some(charref(sub(&self.code, off, off + l), self.patglobflags).0);
                    }
                    let savglobflags = self.patglobflags;
                    let saverrsfound = self.errsfound;
                    let mut lastcharstart = self.patinput - start;
                    if no >= min {
                        loop {
                            let try_it = match nextch {
                                None => true,
                                Some(nc) => {
                                    self.patinput < self.patinend
                                        && self.charmatch(
                                            self.charref_buf(self.patinput, self.patinend).0,
                                            nc,
                                        )
                                }
                            };
                            if try_it && next.is_some_and(|nx| self.patmatch(nx)) {
                                return true;
                            }
                            no -= 1;
                            if no < min {
                                break;
                            }
                            loop {
                                if lastcharstart == 0 {
                                    break;
                                }
                                lastcharstart -= 1;
                                if charstart.get(lastcharstart).copied().unwrap_or(false) {
                                    break;
                                }
                            }
                            self.patinput = start + lastcharstart;
                            self.patglobflags = savglobflags;
                            self.errsfound = saverrsfound;
                        }
                    }
                    return false;
                }
                P_ISSTART => {
                    if self.patinput != self.patinstart || self.patflags & PAT_NOTSTART != 0 {
                        fail = true;
                    }
                }
                P_ISEND => {
                    if self.patinput < self.patinend || self.patflags & PAT_NOTEND != 0 {
                        fail = true;
                    }
                }
                P_COUNTSTART => {
                    let cur_cell = sc + 1 + P_CT_CURRENT;
                    let savecount = rd(&self.code, cur_cell);
                    let saveptr = rd(&self.code, sc + P_CT_PTR);
                    wr(&mut self.code, cur_cell, 0);
                    let ret = self.patmatch(sc + 1);
                    wr(&mut self.code, cur_cell, savecount);
                    wr(&mut self.code, sc + P_CT_PTR, saveptr);
                    return ret;
                }
                P_COUNT => {
                    let cur = rd(&self.code, sc + P_CT_CURRENT);
                    let min = rd(&self.code, sc + P_CT_MIN);
                    let max = rd(&self.code, sc + P_CT_MAX);
                    let here = i64::try_from(self.patinput).unwrap_or(0) + 1;
                    if cur != 0 && cur >= min && here == rd(&self.code, sc + P_CT_PTR) {
                        return next.is_some_and(|nx| self.patmatch(nx));
                    }
                    wr(&mut self.code, sc + P_CT_PTR, here);
                    if max < 0 || cur < max {
                        let thistime = self.patinput;
                        wr(&mut self.code, sc + P_CT_CURRENT, cur + 1);
                        if self.patmatch(sc + P_CT_OPERAND) {
                            return true;
                        }
                        wr(&mut self.code, sc + P_CT_CURRENT, cur);
                        self.patinput = thistime;
                    }
                    if cur < min {
                        return false;
                    }
                    return next.is_some_and(|nx| self.patmatch(nx));
                }
                P_END => {
                    fail = self.patinput < self.patinend && self.patflags & PAT_NOANCH == 0;
                    if !fail {
                        return true;
                    }
                }
                _ => return false,
            }
            if fail {
                if self.errsfound < (self.patglobflags & 0xff)
                    && (self.forceerrs == -1 || self.errsfound < self.forceerrs)
                {
                    let savexact = self.exactpos;
                    let save = self.patinput;
                    let savglobflags = self.patglobflags;
                    self.errsfound += 1;
                    let saverrsfound = self.errsfound;
                    fail = false;
                    if self.patinput < self.patinend {
                        self.patinput = self.charnext_buf(self.patinput, self.patinend);
                        if p_op(&self.code, sc) != P_EXACTLY {
                            continue;
                        }
                        if self.patmatch(sc) {
                            return true;
                        }
                    }
                    if p_op(&self.code, sc) == P_EXACTLY {
                        let Some(sx) = savexact else {
                            self.exactpos = None;
                            return false;
                        };
                        let nextexact =
                            sx + charlen(sub(&self.code, sx, self.exactend), self.patglobflags);
                        if save < self.patinend {
                            let nextin = self.charnext_buf(save, self.patinend);
                            self.patglobflags = savglobflags;
                            self.errsfound = saverrsfound;
                            self.exactpos = savexact;
                            if nextin < self.patinend && nextexact < self.exactend {
                                let cin0 = self.charref_buf(save, self.patinend).0;
                                let cpa0 =
                                    charref(sub(&self.code, sx, self.exactend), self.patglobflags)
                                        .0;
                                let cin1 = self.charref_buf(nextin, self.patinend).0;
                                let cpa1 = charref(
                                    sub(&self.code, nextexact, self.exactend),
                                    self.patglobflags,
                                )
                                .0;
                                if self.charmatch(cin0, cpa1) && self.charmatch(cin1, cpa0) {
                                    self.patinput = self.charnext_buf(nextin, self.patinend);
                                    self.exactpos = Some(
                                        nextexact
                                            + charlen(
                                                sub(&self.code, nextexact, self.exactend),
                                                self.patglobflags,
                                            ),
                                    );
                                    if self.patmatch(sc) {
                                        return true;
                                    }
                                    self.patglobflags = savglobflags;
                                    self.errsfound = saverrsfound;
                                }
                            }
                            self.patinput = nextin;
                            self.exactpos = Some(nextexact);
                            if self.patmatch(sc) {
                                return true;
                            }
                            self.patinput = save;
                            self.patglobflags = savglobflags;
                            self.errsfound = saverrsfound;
                            self.exactpos = savexact;
                        }
                        let ep = self.exactpos.unwrap_or(sx);
                        self.exactpos = Some(
                            ep + charlen(sub(&self.code, ep, self.exactend), self.patglobflags),
                        );
                        continue;
                    }
                }
                self.exactpos = None;
                return false;
            }
            scan = next;
        }
        false
    }

    /// zsh's `patrepeat`.
    fn patrepeat(&mut self, p: usize, charstart: &mut [bool]) -> i64 {
        let mut count = 0;
        let mut scan = self.patinput;
        match p_op(&self.code, p) {
            P_EXACTLY => {
                let (l, off) = p_ls(&self.code, p);
                let tch = charref(sub(&self.code, off, off + l), self.patglobflags).0;
                while scan < self.patinend
                    && self.charmatch(self.charref_buf(scan, self.patinend).0, tch)
                {
                    if let Some(slot) = charstart.get_mut(scan - self.patinput) {
                        *slot = true;
                    }
                    count += 1;
                    scan = self.charnext_buf(scan, self.patinend);
                }
            }
            op @ (P_ANYOF | P_ANYBUT) => {
                let range = nul_terminated(from(&self.code, (p + 1) * W));
                while scan < self.patinend {
                    let (cr, zmb) = self.charref_buf(scan, self.patinend);
                    let hit = if self.patglobflags & GF_MULTIBYTE != 0 {
                        mb_patmatchrange(self.sh, &range, cr, zmb).0
                    } else {
                        patmatchrange(self.sh, &range, cr).0
                    };
                    if hit ^ (op == P_ANYOF) {
                        break;
                    }
                    if let Some(slot) = charstart.get_mut(scan - self.patinput) {
                        *slot = true;
                    }
                    count += 1;
                    scan = self.charnext_buf(scan, self.patinend);
                }
            }
            _ => {}
        }
        self.patinput = scan;
        count
    }
}

fn nul_terminated(s: &[u8]) -> Vec<u8> {
    s.iter().take_while(|&&b| b != 0).copied().collect()
}

/// `WCHAR_INVALID(ch)`.
fn wchar_invalid(b: u8) -> u32 {
    0xDC00 + u32::from(b)
}

/// zsh's `charref`: the character at the start of `s`, and whether it was
/// a valid, incomplete or invalid multibyte sequence.
fn charref(s: &[u8], globflags: i32) -> (u32, i32) {
    let Some(&b) = s.first() else {
        return (0, ZMB_VALID);
    };
    if globflags & GF_MULTIBYTE == 0 || b & 0x80 == 0 {
        return (u32::from(b), ZMB_VALID);
    }
    let (len, wc) = crate::utils::utf8_char(s);
    match wc {
        Some(c) if len > 1 => (c, ZMB_VALID),
        _ => {
            let need = match b {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 0,
            };
            (
                wchar_invalid(b),
                if need > s.len() {
                    ZMB_INCOMPLETE
                } else {
                    ZMB_INVALID
                },
            )
        }
    }
}

/// zsh's `charnext`: the length of the character at the start of `s`.
fn charlen(s: &[u8], globflags: i32) -> usize {
    let Some(&b) = s.first() else { return 0 };
    if globflags & GF_MULTIBYTE == 0 || b & 0x80 == 0 {
        return 1;
    }
    match crate::utils::utf8_char(s) {
        (len, Some(_)) if len > 1 => len,
        _ => 1,
    }
}

/// zsh's `charrefinc`: `(char, length, bad)`.
fn charrefinc(s: &[u8], globflags: i32) -> (u32, usize, bool) {
    let Some(&b) = s.first() else {
        return (0, 0, false);
    };
    if globflags & GF_MULTIBYTE == 0 || b & 0x80 == 0 {
        return (u32::from(b), 1, false);
    }
    match crate::utils::utf8_char(s) {
        (len, Some(c)) if len > 1 => (c, len, false),
        _ => (wchar_invalid(b), 1, true),
    }
}

fn to_lower(c: u32) -> u32 {
    char::from_u32(c).map_or(c, |ch| ch.to_lowercase().next().map_or(c, u32::from))
}

fn to_upper(c: u32) -> u32 {
    char::from_u32(c).map_or(c, |ch| ch.to_uppercase().next().map_or(c, u32::from))
}

fn is_upper(c: u32) -> bool {
    char::from_u32(c).is_some_and(char::is_uppercase)
}

fn is_lower(c: u32) -> bool {
    char::from_u32(c).is_some_and(char::is_lowercase)
}

/// `CHARMATCH`.
fn charmatch(chin: u32, chpa: u32, globflags: i32) -> bool {
    if chin == chpa {
        return true;
    }
    if globflags & GF_IGNCASE != 0 {
        let a = if is_upper(chin) { to_lower(chin) } else { chin };
        let b = if is_upper(chpa) { to_lower(chpa) } else { chpa };
        a == b
    } else if globflags & GF_LCMATCHUC != 0 {
        is_lower(chpa) && to_upper(chpa) == chin
    } else {
        false
    }
}

/// Decode one character from a metafied range string (zsh's `metacharinc`
/// in multibyte mode): `(char, bytes used)`.
fn metacharinc(s: &[u8], i: usize) -> (u32, usize) {
    let c = at(s, i);
    let b0 = if tok::is_tok(c) {
        tok::detok(c)
    } else if c == META {
        at(s, i + 1) ^ 32
    } else {
        c
    };
    let len0 = if c == META { 2 } else { 1 };
    if b0 & 0x80 == 0 {
        return (u32::from(b0), len0);
    }
    // Gather the bytes of one UTF-8 character.
    let mut raw = vec![b0];
    let mut j = i + len0;
    let need = match b0 {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    while raw.len() < need && j < s.len() {
        let cj = at(s, j);
        if cj == META {
            raw.push(at(s, j + 1) ^ 32);
            j += 2;
        } else if tok::is_tok(cj) {
            raw.push(tok::detok(cj));
            j += 1;
        } else {
            raw.push(cj);
            j += 1;
        }
    }
    match std::str::from_utf8(&raw)
        .ok()
        .and_then(|t| t.chars().next())
    {
        Some(ch) => (u32::from(ch), j - i),
        None => (wchar_invalid(b0), len0),
    }
}

fn pp_test(sh: &Shell, swtype: u8, ch: u32, zmb: i32, wide: bool) -> bool {
    let chr = char::from_u32(ch);
    match swtype {
        PP_ALPHA => chr.is_some_and(char::is_alphabetic),
        PP_ALNUM => chr.is_some_and(char::is_alphanumeric),
        PP_ASCII => ch & !0x7f == 0,
        PP_BLANK => {
            ch == 0x20
                || ch == 0x09
                || (wide
                    && ch != 0x0a
                    && chr.is_some_and(|c| {
                        c.is_whitespace()
                            && c != '\n'
                            && c != '\r'
                            && c != '\x0b'
                            && c != '\x0c'
                            && ch > 0x7f
                    }))
        }
        PP_CNTRL => chr.is_some_and(char::is_control),
        PP_DIGIT => (0x30..=0x39).contains(&ch),
        PP_GRAPH => chr.is_some_and(|c| !c.is_control() && !c.is_whitespace()),
        PP_LOWER => chr.is_some_and(char::is_lowercase),
        PP_PRINT => crate::utils::iswprint(ch),
        PP_PUNCT => chr.is_some_and(|c| {
            if c.is_ascii() {
                c.is_ascii_punctuation()
            } else {
                !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control()
            }
        }),
        PP_SPACE => chr.is_some_and(char::is_whitespace),
        PP_UPPER => chr.is_some_and(char::is_uppercase),
        PP_XDIGIT => chr.is_some_and(|c| c.is_ascii_hexdigit()),
        PP_IDENT => sh.wcsitype(ch, crate::utils::IIDENT),
        PP_IFS => sh.wcsitype(ch, crate::utils::ISEP),
        PP_IFSSPACE => ch < 128 && sh.iwsep(u8::try_from(ch).unwrap_or(0)),
        PP_WORD => sh.wcsitype(ch, crate::utils::IWORD),
        PP_INCOMPLETE => wide && zmb == ZMB_INCOMPLETE,
        PP_INVALID => wide && zmb == ZMB_INVALID,
        _ => false,
    }
}

/// zsh's `mb_patmatchrange`: whether `ch` is in `range`, with the index
/// and match type completion needs.
pub(crate) fn mb_patmatchrange(sh: &Shell, range: &[u8], ch: u32, zmb: i32) -> (bool, u32, u8) {
    let mut ind: u32 = 0;
    let mut i = 0;
    let mut mt = 0u8;
    while i < range.len() {
        let c = at(range, i);
        if tok::is_meta(c) && c != 0 {
            let swtype = c.wrapping_sub(META);
            mt = swtype;
            i += 1;
            match swtype {
                0 => {
                    let (r, l) = metacharinc(range, i - 1);
                    i = i - 1 + l;
                    if r == ch {
                        return (true, ind, 0);
                    }
                }
                PP_RANGE => {
                    let (r1, l1) = metacharinc(range, i);
                    i += l1;
                    let (r2, l2) = metacharinc(range, i);
                    i += l2;
                    if r1 <= ch && ch <= r2 {
                        return (true, ind + (ch - r1), swtype);
                    }
                    if r1 < r2 {
                        ind += r2 - r1;
                    }
                }
                t if (PP_FIRST..=PP_LAST).contains(&t) && pp_test(sh, t, ch, zmb, true) => {
                    return (true, ind, swtype);
                }
                _ => {}
            }
        } else {
            let (r, l) = metacharinc(range, i);
            i += l;
            if r == ch {
                return (true, ind, 0);
            }
        }
        ind += 1;
    }
    (false, ind, mt)
}

/// zsh's `patmatchrange` for single-byte characters.
pub(crate) fn patmatchrange(sh: &Shell, range: &[u8], ch: u32) -> (bool, u32, u8) {
    let mut ind = 0;
    let mut i = 0;
    let mut mt = 0u8;
    while i < range.len() {
        let c = at(range, i);
        if tok::is_meta(c) && c != 0 {
            let swtype = c.wrapping_sub(META);
            mt = swtype;
            match swtype {
                0 => {
                    i += 1;
                    if u32::from(at(range, i) ^ 32) == ch {
                        return (true, ind, 0);
                    }
                }
                PP_RANGE => {
                    i += 1;
                    let r1 = u32::from(if at(range, i) == META {
                        at(range, i + 1) ^ 32
                    } else {
                        at(range, i)
                    });
                    i += if at(range, i) == META { 2 } else { 1 };
                    let r2 = u32::from(if at(range, i) == META {
                        at(range, i + 1) ^ 32
                    } else {
                        at(range, i)
                    });
                    if at(range, i) == META {
                        i += 1;
                    }
                    if r1 <= ch && ch <= r2 {
                        return (true, ind + (ch - r1), swtype);
                    }
                    if r1 < r2 {
                        ind += r2 - r1;
                    }
                }
                t if (PP_FIRST..=PP_LAST).contains(&t) && pp_test(sh, t, ch, ZMB_VALID, false) => {
                    return (true, ind, swtype);
                }
                _ => {}
            }
        } else if u32::from(c) == ch {
            return (true, ind, 0);
        }
        i += 1;
        ind += 1;
    }
    (false, ind, mt)
}

/// zsh's `mb_patmatchindex`: the character at index `ind` of `range`, or
/// the class there.
pub(crate) fn mb_patmatchindex(range: &[u8], ind: u32) -> Option<(Option<u32>, u8)> {
    let mut ind = ind;
    let mut i = 0;
    while i < range.len() {
        let c = at(range, i);
        if tok::is_meta(c) && c != 0 {
            let swtype = c.wrapping_sub(META);
            i += 1;
            match swtype {
                0 => {
                    let (r, l) = metacharinc(range, i - 1);
                    i = i - 1 + l;
                    if ind == 0 {
                        return Some((Some(r), 0));
                    }
                }
                PP_RANGE => {
                    let (r1, l1) = metacharinc(range, i);
                    i += l1;
                    let (r2, l2) = metacharinc(range, i);
                    i += l2;
                    let rdiff = r2.wrapping_sub(r1);
                    if rdiff >= ind {
                        return Some((Some(r1 + ind), 0));
                    }
                    ind -= rdiff;
                }
                t if (PP_FIRST..=PP_LAST).contains(&t) && ind == 0 => {
                    return Some((None, t));
                }
                _ => {}
            }
        } else {
            let (r, l) = metacharinc(range, i);
            i += l;
            if ind == 0 {
                return Some((Some(r), 0));
            }
        }
        if ind == 0 {
            break;
        }
        ind -= 1;
    }
    None
}

impl Shell {
    /// zsh's `pattry`: does `prog` match the whole of `s` (metafied)?
    pub(crate) fn pattry(&mut self, prog: &Patprog, s: &[u8]) -> bool {
        self.pattryrefs(prog, s, None, 0, false).is_some()
    }

    /// Match without setting `$MATCH`/`$match` (for callers holding only a
    /// shared borrow; zsh sets them, but only with `(#m)`/`(#b)`).
    pub(crate) fn pattry_noref(&self, prog: &Patprog, s: &[u8]) -> bool {
        self.pattry_info(prog, s, None, 0).is_some()
    }

    /// zsh's `pattryrefs`: the match, setting the references it asks for
    /// unless `want_positions` (zsh's `nump`) is given.
    pub(crate) fn pattryrefs(
        &mut self,
        prog: &Patprog,
        s: &[u8],
        pathprefix: Option<&[u8]>,
        patoffset: i64,
        want_positions: bool,
    ) -> Option<MatchInfo> {
        let info = self.pattry_info(prog, s, pathprefix, patoffset)?;
        if !want_positions && prog.flags & PAT_FILE == 0 {
            if let Some((m, b, e)) = &info.matchref {
                let _ = self.setsparam(b"MATCH", m.clone());
                let _ = self.setiparam(b"MBEGIN", *b);
                let _ = self.setiparam(b"MEND", *e);
            }
            if let Some((m, b, e)) = &info.backrefs {
                let _ = self.setaparam(b"match", m.clone());
                let _ = self.setaparam(b"mbegin", b.clone());
                let _ = self.setaparam(b"mend", e.clone());
            }
        }
        Some(info)
    }

    /// The matching itself, with no parameters set.
    pub(crate) fn pattry_info(
        &self,
        prog: &Patprog,
        s: &[u8],
        pathprefix: Option<&[u8]>,
        patoffset: i64,
    ) -> Option<MatchInfo> {
        let s = if at(s, 0) == NULARG { from(s, 1) } else { s };
        let ksh = i64::from(!self.isset(KSHARRAYS));
        let needfullpath =
            prog.flags & PAT_HAS_EXCLUDP != 0 && pathprefix.is_some_and(|p| !p.is_empty());
        let prefix = if needfullpath {
            tok::unmetafy(pathprefix.unwrap_or(&[]))
        } else {
            Vec::new()
        };
        let raw = tok::unmetafy(s);
        let mut info = MatchInfo::default();
        if prog.flags & (PAT_PURES | PAT_ANY) != 0 {
            let ok = if prog.flags & PAT_ANY != 0 {
                true
            } else {
                let pstr = tok::unmetafy(&prog.pure);
                raw.len() >= pstr.len()
                    && raw.starts_with(&pstr)
                    && (raw.len() == pstr.len() || prog.flags & PAT_NOANCH != 0)
            };
            if !ok {
                return None;
            }
            if prog.flags & PAT_NOGLD != 0 && raw.first() == Some(&b'.') {
                return None;
            }
            let plen = tok::unmetafy(&prog.pure).len();
            info.len = if prog.flags & PAT_ANY != 0 {
                0
            } else {
                prog.pure.len()
            };
            info.end_chars = usize::try_from(patoffset).unwrap_or(0) + plen;
            if prog.globend & GF_MATCHREF != 0 && prog.flags & PAT_FILE == 0 {
                let m = tok::metafy(raw.get(..plen).unwrap_or(&[]));
                let mlen = i64::try_from(self.charsub(raw.get(..plen).unwrap_or(&[]))).unwrap_or(0);
                info.matchref = Some((m, patoffset + ksh, mlen + patoffset + ksh - 1));
            }
            return Some(info);
        }
        if prog.flags & PAT_SCAN == 0
            && let Some(off) = prog.mustoff
        {
            let must = prog.code.get(off..off + prog.patmlen).unwrap_or(&[]);
            if must.len() > raw.len()
                || (!must.is_empty() && !raw.windows(must.len()).any(|w| w == must))
            {
                return None;
            }
        }
        let mut buf = prefix.clone();
        buf.extend_from_slice(&raw);
        let mut run = Run {
            sh: self,
            code: prog.code.clone(),
            syncs: Vec::new(),
            patinstart: prefix.len(),
            patinend: buf.len(),
            patinput: prefix.len(),
            patinpath: if needfullpath { Some(0) } else { None },
            buf,
            beginp: [0; NSUBEXP],
            endp: [0; NSUBEXP],
            parsfound: 0,
            globdots: prog.flags & PAT_NOGLD == 0,
            patflags: prog.flags,
            patglobflags: prog.globflags,
            errsfound: if prog.flags & PAT_FILE != 0 {
                self.errsfound.get()
            } else {
                0
            },
            forceerrs: if prog.flags & PAT_FILE != 0 {
                self.forceerrs.get()
            } else {
                -1
            },
            exactpos: None,
            exactend: 0,
        };
        if !run.patmatch(1) {
            if prog.flags & PAT_FILE != 0 {
                self.errsfound.set(run.errsfound);
            }
            return None;
        }
        if prog.flags & PAT_FILE != 0 {
            self.errsfound.set(run.errsfound);
        }
        let start = run.patinstart;
        let matched = run.buf.get(start..run.patinput).unwrap_or(&[]).to_vec();
        info.len = tok::metafy(&matched).len();
        let mlen = i64::try_from(self.charsub(&matched)).unwrap_or(0);
        info.end_chars = usize::try_from(mlen + patoffset).unwrap_or(0);
        if prog.globend & GF_MATCHREF != 0 && prog.flags & PAT_FILE == 0 {
            info.matchref = Some((
                tok::metafy(&matched),
                patoffset + ksh,
                mlen + patoffset + ksh - 1,
            ));
        }
        if prog.patnpar > 0 {
            let mut m = Vec::new();
            let mut b = Vec::new();
            let mut e = Vec::new();
            for i in 0..prog.patnpar.min(NSUBEXP) {
                if run.parsfound & (1 << i) != 0 {
                    let bs = run.beginp.get(i).copied().unwrap_or(0);
                    let es = run.endp.get(i).copied().unwrap_or(0).max(bs);
                    let bpos = i64::try_from(self.charsub(run.buf.get(start..bs).unwrap_or(&[])))
                        .unwrap_or(0);
                    let epos = i64::try_from(self.charsub(run.buf.get(start..es).unwrap_or(&[])))
                        .unwrap_or(0);
                    m.push(tok::metafy(run.buf.get(bs..es).unwrap_or(&[])));
                    b.push((bpos + patoffset + ksh).to_string().into_bytes());
                    e.push((epos + patoffset + ksh - 1).to_string().into_bytes());
                    info.positions
                        .push((bpos + patoffset, epos + patoffset - 1));
                } else {
                    m.push(Vec::new());
                    b.push(b"-1".to_vec());
                    e.push(b"-1".to_vec());
                    info.positions.push((-1, -1));
                }
            }
            info.backrefs = Some((m, b, e));
        }
        Some(info)
    }

    /// zsh's `CHARSUB`: characters in unmetafied bytes.
    fn charsub(&self, s: &[u8]) -> usize {
        if !self.isset(MULTIBYTE) {
            return s.len();
        }
        let mut n = 0;
        let mut i = 0;
        while i < s.len() {
            let (l, _) = crate::utils::utf8_char(from(s, i));
            i += l.max(1);
            n += 1;
        }
        n
    }

    /// zsh's `haswilds`.
    pub(crate) fn haswilds(&self, s: &mut [u8]) -> bool {
        if matches!(at(s, 0), INBRACK | OUTBRACK) && s.len() == 1 {
            return false;
        }
        if at(s, 0) == b'%'
            && at(s, 1) == QUEST
            && let Some(slot) = s.get_mut(1)
        {
            *slot = b'?';
        }
        let d = &self.zpc_disables;
        for i in 0..s.len() {
            match at(s, i) {
                INPAR => {
                    let prev = if i > 0 { at(s, i - 1) } else { 0 };
                    if (!self.isset(SHGLOB) && !d[ZPC_INPAR])
                        || (i > 0
                            && self.isset(KSHGLOB)
                            && ((prev == QUEST && !d[ZPC_KSH_QUEST])
                                || (prev == STAR && !d[ZPC_KSH_STAR])
                                || (prev == b'+' && !d[ZPC_KSH_PLUS])
                                || (prev == BANG && !d[ZPC_KSH_BANG])
                                || (prev == b'!' && !d[ZPC_KSH_BANG2])
                                || (prev == b'@' && !d[ZPC_KSH_AT])))
                    {
                        return true;
                    }
                }
                BAR if !d[ZPC_BAR] => return true,
                STAR if !d[ZPC_STAR] => return true,
                INBRACK if !d[ZPC_INBRACK] => return true,
                INANG if !d[ZPC_INANG] => return true,
                QUEST if !d[ZPC_QUEST] => return true,
                POUND if self.isset(EXTENDEDGLOB) && !d[ZPC_HASH] => return true,
                HAT if self.isset(EXTENDEDGLOB) && !d[ZPC_HAT] => return true,
                _ => {}
            }
        }
        false
    }

    /// zsh's `pat_enables` (`enable -p`/`disable -p`).
    pub(crate) fn pat_enables(&mut self, cmd: &str, pats: &[Vec<u8>], enable: bool) -> i32 {
        if pats.is_empty() {
            let mut out = Vec::new();
            for (i, s) in ZPC_STRINGS.iter().enumerate() {
                let Some(s) = s else { continue };
                let dis = self.zpc_disables.get(i).copied().unwrap_or(false);
                if if enable { dis } else { !dis } {
                    continue;
                }
                if !out.is_empty() {
                    out.push(b' ');
                }
                out.extend(format!("'{s}'").bytes());
            }
            if !out.is_empty() {
                out.push(b'\n');
                self.write_stdout(&out);
            }
            return 0;
        }
        let mut ret = 0;
        for p in pats {
            match ZPC_STRINGS
                .iter()
                .position(|s| s.is_some_and(|s| s.as_bytes() == p.as_slice()))
            {
                Some(i) => {
                    if let Some(d) = self.zpc_disables.get_mut(i) {
                        *d = !enable;
                    }
                }
                None => {
                    self.zerrnam(cmd, &format!("invalid pattern: {}", crate::utils::lossy(p)));
                    ret = 1;
                }
            }
        }
        ret
    }

    /// zsh's `savepatterndisables`.
    pub(crate) fn savepatterndisables(&self) -> u32 {
        self.zpc_disables
            .iter()
            .enumerate()
            .filter(|&(_, &d)| d)
            .fold(0, |acc, (i, _)| acc | (1 << i))
    }

    /// zsh's `restorepatterndisables`.
    pub(crate) fn restorepatterndisables(&mut self, disables: u32) {
        for (i, d) in self.zpc_disables.iter_mut().enumerate() {
            *d = disables & (1 << i) != 0;
        }
    }

    /// zsh's `startpatternscope`.
    pub(crate) fn startpatternscope(&mut self) {
        let d = self.savepatterndisables();
        self.zpc_disables_stack.push(d);
    }

    /// zsh's `endpatternscope`.
    pub(crate) fn endpatternscope(&mut self) {
        if let Some(d) = self.zpc_disables_stack.pop()
            && self.isset(LOCALPATTERNS)
        {
            self.restorepatterndisables(d);
        }
    }

    /// zsh's `clearpatterndisables`.
    pub(crate) fn clearpatterndisables(&mut self) {
        self.zpc_disables = [false; ZPC_COUNT];
    }
}

/// zsh's `tokenize` (glob.c): turn pattern characters into tokens.
pub(crate) fn tokenize(s: &mut [u8]) {
    zshtokenize(s, false, false);
}

/// zsh's `shtokenize`, for GLOB_SUBST.
pub(crate) fn shtokenize(s: &mut [u8], shglob: bool) {
    zshtokenize(s, true, shglob);
}

const ZTOKENS: &[u8] = b"#$^*(())$=|{}[]`<>>?~`,-!'\"\\\\";

fn zshtokenize(s: &mut [u8], subst: bool, shglob: bool) {
    let mut bslash = false;
    let mut i = 0;
    let bnull = if subst { BNULLKEEP } else { tok::BNULL };
    while i < s.len() {
        let c = at(s, i);
        let mut reset = true;
        let mut recheck = None;
        match c {
            META => {
                i += 1;
            }
            tok::BNULL | BNULLKEEP | b'\\' => {
                if bslash {
                    if let Some(slot) = s.get_mut(i - 1) {
                        *slot = bnull;
                    }
                } else {
                    bslash = true;
                    reset = false;
                }
            }
            b'<' => {
                if !shglob {
                    if bslash {
                        if let Some(slot) = s.get_mut(i - 1) {
                            *slot = bnull;
                        }
                    } else {
                        // zsh's `goto cont`: a failed range is looked at
                        // again from the character that ended it.
                        let t = i;
                        let mut j = i + 1;
                        while at(s, j).is_ascii_digit() {
                            j += 1;
                        }
                        if matches!(at(s, j), b'-' | DASH) {
                            j += 1;
                            while at(s, j).is_ascii_digit() {
                                j += 1;
                            }
                            if at(s, j) == b'>' {
                                if let Some(slot) = s.get_mut(t) {
                                    *slot = INANG;
                                }
                                if let Some(slot) = s.get_mut(j) {
                                    *slot = OUTANG;
                                }
                                i = j;
                            } else {
                                recheck = Some(j);
                            }
                        } else {
                            recheck = Some(j);
                        }
                    }
                }
            }
            b'(' | b'|' | b')' if shglob => {}
            b'(' | b'|' | b')' | b'>' | b'^' | b'#' | b'~' | b'[' | b']' | b'*' | b'?' | b'='
            | b'-' | b'!' => {
                if let Some(p) = ZTOKENS.iter().position(|&z| z == c) {
                    if bslash {
                        if let Some(slot) = s.get_mut(i - 1) {
                            *slot = bnull;
                        }
                    } else if let Some(slot) = s.get_mut(i) {
                        *slot = POUND + u8::try_from(p).unwrap_or(0);
                    }
                }
            }
            _ => {}
        }
        if let Some(j) = recheck {
            i = j;
            continue;
        }
        if reset {
            bslash = false;
        }
        i += 1;
    }
}
