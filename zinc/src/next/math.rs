//! Arithmetic evaluation (zsh's `math.c`): an operator-precedence parser that
//! evaluates as it parses, with integer and floating point values, zsh's
//! or C's precedences, assignment to parameters and math functions.

use crate::options::*;
use crate::params::{PM_EFLOAT, PM_FFLOAT, PM_INTEGER, pm_type};
use crate::shell::Shell;
use crate::tok::{self, DASH, DNULL};
use crate::utils::{at, from, sub};

/// zsh's `mnumber`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MNumber {
    Int(i64),
    Float(f64),
}

impl MNumber {
    pub(crate) fn is_float(self) -> bool {
        matches!(self, MNumber::Float(_))
    }
    pub(crate) fn as_int(self) -> i64 {
        match self {
            MNumber::Int(i) => i,
            #[expect(clippy::cast_possible_truncation, reason = "zsh casts to zlong")]
            MNumber::Float(d) => d as i64,
        }
    }
    pub(crate) fn as_float(self) -> f64 {
        match self {
            MNumber::Int(i) => i as f64,
            MNumber::Float(d) => d,
        }
    }
}

const LR: u32 = 0x0000;
const RL: u32 = 0x0001;
const BOOL: u32 = 0x0002;
const OP_A2: u32 = 0x0004;
const OP_A2IR: u32 = 0x0008;
const OP_A2IO: u32 = 0x0010;
const OP_E2: u32 = 0x0020;
const OP_E2IO: u32 = 0x0040;
const OP_OP: u32 = 0x0080;
const OP_OPF: u32 = 0x0100;

const M_INPAR: usize = 0;
const M_OUTPAR: usize = 1;
const NOT: usize = 2;
const COMP: usize = 3;
const POSTPLUS: usize = 4;
const POSTMINUS: usize = 5;
const UPLUS: usize = 6;
const UMINUS: usize = 7;
const AND: usize = 8;
const XOR: usize = 9;
const OR: usize = 10;
const MUL: usize = 11;
const DIV: usize = 12;
const MOD: usize = 13;
const PLUS: usize = 14;
const MINUS: usize = 15;
const SHLEFT: usize = 16;
const SHRIGHT: usize = 17;
const LES: usize = 18;
const LEQ: usize = 19;
const GRE: usize = 20;
const GEQ: usize = 21;
const DEQ: usize = 22;
const NEQ: usize = 23;
const DAND: usize = 24;
const DOR: usize = 25;
const DXOR: usize = 26;
const QUEST: usize = 27;
const COLON: usize = 28;
const EQ: usize = 29;
const PLUSEQ: usize = 30;
const MINUSEQ: usize = 31;
const MULEQ: usize = 32;
const DIVEQ: usize = 33;
const MODEQ: usize = 34;
const ANDEQ: usize = 35;
const XOREQ: usize = 36;
const OREQ: usize = 37;
const SHLEFTEQ: usize = 38;
const SHRIGHTEQ: usize = 39;
const DANDEQ: usize = 40;
const DOREQ: usize = 41;
const DXOREQ: usize = 42;
const COMMA: usize = 43;
const EOI: usize = 44;
const PREPLUS: usize = 45;
const PREMINUS: usize = 46;
const NUM: usize = 47;
const ID: usize = 48;
const POWER: usize = 49;
const CID: usize = 50;
const POWEREQ: usize = 51;
const FUNC: usize = 52;

const C_PREC: [i32; 53] = [
    1, 137, 2, 2, 2, 2, 2, 2, 9, 10, 11, 4, 4, 4, 5, 5, 6, 6, 7, 7, 7, 7, 8, 8, 12, 14, 13, 15, 16,
    17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 18, 200, 2, 2, 0, 0, 3, 0, 17, 0,
];

const Z_PREC: [i32; 53] = [
    1, 137, 2, 2, 2, 2, 2, 2, 4, 5, 6, 8, 8, 8, 9, 9, 3, 3, 10, 10, 10, 10, 11, 11, 12, 13, 13, 14,
    15, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 200, 2, 2, 0, 0, 7, 0, 16, 0,
];

const TYPE: [u32; 53] = [
    LR,
    LR | OP_OP | OP_OPF,
    RL,
    RL,
    RL | OP_OP | OP_OPF,
    RL | OP_OP | OP_OPF,
    RL,
    RL,
    LR | OP_A2IO,
    LR | OP_A2IO,
    LR | OP_A2IO,
    LR | OP_A2,
    LR | OP_A2,
    LR | OP_A2,
    LR | OP_A2,
    LR | OP_A2,
    LR | OP_A2IO,
    LR | OP_A2IO,
    LR | OP_A2IR,
    LR | OP_A2IR,
    LR | OP_A2IR,
    LR | OP_A2IR,
    LR | OP_A2IR,
    LR | OP_A2IR,
    BOOL | OP_A2IO,
    BOOL | OP_A2IO,
    LR | OP_A2IO,
    RL | OP_OP,
    RL | OP_OP,
    RL | OP_E2,
    RL | OP_E2,
    RL | OP_E2,
    RL | OP_E2,
    RL | OP_E2,
    RL | OP_E2,
    RL | OP_E2IO,
    RL | OP_E2IO,
    RL | OP_E2IO,
    RL | OP_E2IO,
    RL | OP_E2IO,
    BOOL | OP_E2IO,
    BOOL | OP_E2IO,
    RL | OP_A2IO,
    RL | OP_A2,
    RL | OP_OP,
    RL,
    RL,
    LR | OP_OPF,
    LR | OP_OPF,
    RL | OP_A2,
    LR | OP_OPF,
    RL | OP_E2,
    LR | OP_OPF,
];

const STACKSZ: usize = 100;
const MAX_MLEVEL: i32 = 256;

fn ty(t: usize) -> u32 {
    TYPE.get(t).copied().unwrap_or(0)
}

/// One stack entry (zsh's `struct mathvalue`).
#[derive(Debug, Clone)]
struct MathValue {
    lval: Option<Vec<u8>>,
    /// Whether the value still has to be read from `lval`.
    unset: bool,
    val: MNumber,
}

/// The state of one evaluation.
struct Eval {
    s: Vec<u8>,
    ptr: usize,
    stack: Vec<MathValue>,
    yyval: MNumber,
    yylval: Vec<u8>,
    unary: bool,
    prec: &'static [i32; 53],
    mtok: usize,
    noeval: i32,
}

impl Eval {
    fn prec(&self, t: usize) -> i32 {
        self.prec.get(t).copied().unwrap_or(200)
    }
    fn toprec(&self) -> i32 {
        self.prec(COMMA) + 1
    }
    fn argprec(&self) -> i32 {
        self.prec(COMMA) - 1
    }
    fn c(&self, i: usize) -> u8 {
        at(&self.s, i)
    }
}

impl Shell {
    fn getmathparam(&mut self, mv: &MathValue) -> MNumber {
        let lval = mv.lval.clone().unwrap_or_default();
        let mut i = 0;
        let Some(mut v) = self.getvalue(&lval, &mut i, 1) else {
            if self.unset_opt(UNSET) {
                self.zerr(&format!(
                    "{}: parameter not set",
                    crate::utils::lossy(&lval)
                ));
            }
            return if self.isset(FORCEFLOAT) {
                MNumber::Float(0.0)
            } else {
                MNumber::Int(0)
            };
        };
        let r = self.getnumvalue(Some(&mut v));
        match r {
            MNumber::Int(l) if self.isset(FORCEFLOAT) => MNumber::Float(l as f64),
            other => other,
        }
    }

    /// zsh's `mathevall`: `(value, end index, last token)`.
    fn mathevall(&mut self, s: &[u8], top: bool) -> (MNumber, usize, usize) {
        if self.mlevel >= MAX_MLEVEL {
            self.zerr(&format!(
                "math recursion limit exceeded: {}",
                crate::utils::lossy(s)
            ));
            return (MNumber::Int(0), 0, EOI);
        }
        self.mlevel += 1;
        let saved_lastbase = self.lastbase;
        let mut e = Eval {
            s: s.to_vec(),
            ptr: 0,
            stack: Vec::with_capacity(8),
            yyval: MNumber::Int(0),
            yylval: Vec::new(),
            unary: true,
            prec: if self.isset(CPRECEDENCES) {
                &C_PREC
            } else {
                &Z_PREC
            },
            mtok: EOI,
            noeval: self.noeval,
        };
        self.lastbase = -1;
        let pc = if top { e.toprec() } else { e.argprec() };
        self.mathparse(&mut e, pc);
        if e.mtok == M_OUTPAR && !self.errflag() {
            self.zerr("bad math expression: unexpected ')'");
        }
        let ret = if self.errflag() {
            MNumber::Int(0)
        } else {
            match e.stack.first() {
                Some(mv) if mv.unset => {
                    let mv = mv.clone();
                    self.getmathparam(&mv)
                }
                Some(mv) => mv.val,
                None => MNumber::Int(0),
            }
        };
        self.mlevel -= 1;
        if self.mlevel != 0 {
            self.lastbase = saved_lastbase;
        }
        self.lastmathval = ret;
        (ret, e.ptr, e.mtok)
    }

    fn lexconstant(&mut self, e: &mut Eval) -> usize {
        let mut nptr = e.ptr;
        if matches!(e.c(nptr), b'-' | DASH) {
            nptr += 1;
        }
        if e.c(nptr) == b'0' {
            nptr += 1;
            let lowchar = e.c(nptr).to_ascii_lowercase();
            if lowchar == b'x' || lowchar == b'b' {
                let (v, end) = crate::utils::zstrtol_underscore(from(&e.s, e.ptr), 0, true);
                e.ptr += end;
                self.lastbase = if lowchar == b'b' { 2 } else { 16 };
                e.yyval = if self.isset(FORCEFLOAT) {
                    MNumber::Float(v as f64)
                } else {
                    MNumber::Int(v)
                };
                return NUM;
            } else if self.isset(OCTALZEROES) {
                let mut p2 = nptr;
                while e.c(p2).is_ascii_digit() || e.c(p2) == b'_' {
                    p2 += 1;
                }
                if p2 > nptr && !matches!(e.c(p2), b'.' | b'e' | b'E' | b'#') {
                    let (v, end) = crate::utils::zstrtol_underscore(from(&e.s, e.ptr), 0, true);
                    e.ptr += end;
                    self.lastbase = 8;
                    e.yyval = if self.isset(FORCEFLOAT) {
                        MNumber::Float(v as f64)
                    } else {
                        MNumber::Int(v)
                    };
                    return NUM;
                }
                nptr = p2;
            }
        }
        while e.c(nptr).is_ascii_digit() || e.c(nptr) == b'_' {
            nptr += 1;
        }
        if matches!(e.c(nptr), b'.' | b'e' | b'E') {
            if e.c(nptr) == b'.' {
                nptr += 1;
                while e.c(nptr).is_ascii_digit() || e.c(nptr) == b'_' {
                    nptr += 1;
                }
            }
            if matches!(e.c(nptr), b'e' | b'E') {
                nptr += 1;
                if matches!(e.c(nptr), b'+' | b'-' | DASH) {
                    nptr += 1;
                }
                while e.c(nptr).is_ascii_digit() || e.c(nptr) == b'_' {
                    nptr += 1;
                }
            }
            let text: Vec<u8> = sub(&e.s, e.ptr, nptr)
                .iter()
                .filter(|&&c| c != b'_')
                .map(|&c| if c == DASH { b'-' } else { c })
                .collect();
            let (d, used) = strtod(&text);
            if used == 0 || at(&text, used) == b'.' {
                self.zerr("bad floating point constant");
                return EOI;
            }
            // strtod may stop early; count the underscores it passed over.
            let mut consumed = 0;
            let mut k = e.ptr;
            while consumed < used && k < nptr {
                if e.c(k) != b'_' {
                    consumed += 1;
                }
                k += 1;
            }
            e.ptr = k;
            if e.c(e.ptr) == b'.' {
                self.zerr("bad floating point constant");
                return EOI;
            }
            e.yyval = MNumber::Float(d);
        } else {
            let (mut v, end) = crate::utils::zstrtol_underscore(from(&e.s, e.ptr), 10, true);
            e.ptr += end;
            if e.c(e.ptr) == b'#' {
                e.ptr += 1;
                self.lastbase = i32::try_from(v).unwrap_or(10);
                let (v2, end2) = crate::utils::zstrtol_underscore(
                    from(&e.s, e.ptr),
                    u32::try_from(v).unwrap_or(10),
                    true,
                );
                v = v2;
                e.ptr += end2;
            }
            e.yyval = if self.isset(FORCEFLOAT) {
                MNumber::Float(v as f64)
            } else {
                MNumber::Int(v)
            };
        }
        NUM
    }

    #[expect(clippy::too_many_lines, reason = "zsh's zzlex")]
    fn zzlex(&mut self, e: &mut Eval) -> usize {
        e.yyval = MNumber::Int(0);
        loop {
            let mut cct = false;
            let c = e.c(e.ptr);
            let at_end = e.ptr >= e.s.len();
            e.ptr += 1;
            let n = e.c(e.ptr);
            match c {
                _ if at_end => {
                    e.ptr -= 1;
                    return EOI;
                }
                b'+' => {
                    if n == b'+' {
                        e.ptr += 1;
                        return if e.unary { PREPLUS } else { POSTPLUS };
                    }
                    if n == b'=' {
                        e.ptr += 1;
                        return PLUSEQ;
                    }
                    return if e.unary { UPLUS } else { PLUS };
                }
                b'-' | DASH => {
                    if matches!(n, b'-' | DASH) {
                        e.ptr += 1;
                        return if e.unary { PREMINUS } else { POSTMINUS };
                    }
                    if n == b'=' {
                        e.ptr += 1;
                        return MINUSEQ;
                    }
                    if e.unary {
                        if n.is_ascii_digit() || n == b'.' {
                            let ctype = self.lexconstant(e);
                            if ctype == NUM {
                                e.yyval = match e.yyval {
                                    MNumber::Float(d) => MNumber::Float(-d),
                                    MNumber::Int(l) => MNumber::Int(l.wrapping_neg()),
                                };
                            }
                            return ctype;
                        }
                        return UMINUS;
                    }
                    return MINUS;
                }
                b'(' => return M_INPAR,
                b')' => return M_OUTPAR,
                b'!' => {
                    if n == b'=' {
                        e.ptr += 1;
                        return NEQ;
                    }
                    return NOT;
                }
                b'~' => return COMP,
                b'&' => {
                    if n == b'&' {
                        e.ptr += 1;
                        if e.c(e.ptr) == b'=' {
                            e.ptr += 1;
                            return DANDEQ;
                        }
                        return DAND;
                    } else if n == b'=' {
                        e.ptr += 1;
                        return ANDEQ;
                    }
                    return AND;
                }
                b'|' => {
                    if n == b'|' {
                        e.ptr += 1;
                        if e.c(e.ptr) == b'=' {
                            e.ptr += 1;
                            return DOREQ;
                        }
                        return DOR;
                    } else if n == b'=' {
                        e.ptr += 1;
                        return OREQ;
                    }
                    return OR;
                }
                b'^' => {
                    if n == b'^' {
                        e.ptr += 1;
                        if e.c(e.ptr) == b'=' {
                            e.ptr += 1;
                            return DXOREQ;
                        }
                        return DXOR;
                    } else if n == b'=' {
                        e.ptr += 1;
                        return XOREQ;
                    }
                    return XOR;
                }
                b'*' => {
                    if n == b'*' {
                        e.ptr += 1;
                        if e.c(e.ptr) == b'=' {
                            e.ptr += 1;
                            return POWEREQ;
                        }
                        return POWER;
                    }
                    if n == b'=' {
                        e.ptr += 1;
                        return MULEQ;
                    }
                    return MUL;
                }
                b'/' => {
                    if n == b'=' {
                        e.ptr += 1;
                        return DIVEQ;
                    }
                    return DIV;
                }
                b'%' => {
                    if n == b'=' {
                        e.ptr += 1;
                        return MODEQ;
                    }
                    return MOD;
                }
                b'<' => {
                    if n == b'<' {
                        e.ptr += 1;
                        if e.c(e.ptr) == b'=' {
                            e.ptr += 1;
                            return SHLEFTEQ;
                        }
                        return SHLEFT;
                    } else if n == b'=' {
                        e.ptr += 1;
                        return LEQ;
                    }
                    return LES;
                }
                b'>' => {
                    if n == b'>' {
                        e.ptr += 1;
                        if e.c(e.ptr) == b'=' {
                            e.ptr += 1;
                            return SHRIGHTEQ;
                        }
                        return SHRIGHT;
                    } else if n == b'=' {
                        e.ptr += 1;
                        return GEQ;
                    }
                    return GRE;
                }
                b'=' => {
                    if n == b'=' {
                        e.ptr += 1;
                        return DEQ;
                    }
                    return EQ;
                }
                b'$' => {
                    e.yyval = MNumber::Int(i64::from(self.mypid));
                    return NUM;
                }
                b'?' => {
                    if e.unary {
                        e.yyval = MNumber::Int(i64::from(self.lastval()));
                        return NUM;
                    }
                    return QUEST;
                }
                b':' => return COLON,
                b',' => return COMMA,
                b'[' => {
                    if n.is_ascii_digit() {
                        let (base, end) = crate::utils::zstrtol(from(&e.s, e.ptr), 10);
                        e.ptr += end;
                        if e.c(e.ptr) != b']' || !e.c(e.ptr + 1).is_ascii_digit() {
                            self.zerr("bad base syntax");
                            return EOI;
                        }
                        e.ptr += 1;
                        self.lastbase = i32::try_from(base).unwrap_or(10);
                        let (v, end) = crate::utils::zstrtol(
                            from(&e.s, e.ptr),
                            u32::try_from(base).unwrap_or(10),
                        );
                        e.ptr += end;
                        e.yyval = MNumber::Int(v);
                        return NUM;
                    }
                    let mut ok = false;
                    if n == b'#' {
                        let mut sign = 1;
                        e.ptr += 1;
                        if e.c(e.ptr) == b'#' {
                            sign = -1;
                            e.ptr += 1;
                        }
                        let mut checkradix = false;
                        if e.c(e.ptr).is_ascii_digit() || e.c(e.ptr) == b'_' {
                            if e.c(e.ptr).is_ascii_digit() {
                                let (r, end) = crate::utils::zstrtol(from(&e.s, e.ptr), 10);
                                e.ptr += end;
                                self.outputradix = sign * i32::try_from(r).unwrap_or(10);
                                checkradix = true;
                            }
                            if e.c(e.ptr) == b'_' {
                                e.ptr += 1;
                                if e.c(e.ptr).is_ascii_digit() {
                                    let (u, end) = crate::utils::zstrtol(from(&e.s, e.ptr), 10);
                                    e.ptr += end;
                                    self.outputunderscore = i32::try_from(u).unwrap_or(3);
                                } else {
                                    self.outputunderscore = 3;
                                }
                            }
                            if e.c(e.ptr) == b']' {
                                ok = true;
                                if checkradix {
                                    let r = self.outputradix.abs();
                                    if !(2..=36).contains(&r) {
                                        self.zerr(&format!(
                                            "invalid base (must be 2 to 36 inclusive): {}",
                                            self.outputradix
                                        ));
                                        return EOI;
                                    }
                                }
                                e.ptr += 1;
                            }
                        }
                    }
                    if !ok {
                        self.zerr("bad output format specification");
                        return EOI;
                    }
                }
                b' ' | b'\t' | b'\n' | b'"' | DNULL => {}
                _ => {
                    e.ptr -= 1;
                    let c = e.c(e.ptr);
                    if c.is_ascii_digit() || c == b'.' {
                        return self.lexconstant(e);
                    }
                    if c == b'#' {
                        e.ptr += 1;
                        if matches!(e.c(e.ptr), b'\\' | b'#') {
                            let optr = e.ptr;
                            e.ptr += 1;
                            if e.ptr >= e.s.len() {
                                self.zerr("bad math expression: character missing after ##");
                                return EOI;
                            }
                            let (_, misc) =
                                self.getkeystring(from(&e.s, e.ptr), crate::utils::GETKEYS_MATH);
                            match misc.next {
                                Some(nx) => {
                                    e.ptr += nx;
                                    e.yyval = MNumber::Int(i64::from(misc.chr));
                                    return NUM;
                                }
                                None => {
                                    self.zerr("bad math expression: bad character after ##");
                                    e.ptr = optr;
                                    return EOI;
                                }
                            }
                        }
                        cct = true;
                    }
                    let ie = self.itype_end(&e.s, e.ptr, crate::utils::IIDENT, false);
                    if ie != e.ptr {
                        let p = e.ptr;
                        e.ptr = ie;
                        let word = sub(&e.s, p, ie);
                        if ie - p == 3 && !self.emulation_is(EMULATE_SH) {
                            if word.eq_ignore_ascii_case(b"nan") {
                                e.yyval = MNumber::Float(f64::NAN);
                                return NUM;
                            } else if word.eq_ignore_ascii_case(b"inf") {
                                e.yyval = MNumber::Float(f64::INFINITY);
                                return NUM;
                            }
                        }
                        let mut func = false;
                        if e.c(e.ptr) == b'[' || (!cct && e.c(e.ptr) == b'(') {
                            let op = e.c(e.ptr);
                            let cp = if op == b'[' { b']' } else { b')' };
                            func = op == b'(';
                            e.ptr += 1;
                            let mut l = 1;
                            while e.ptr < e.s.len() && l != 0 {
                                let ch = e.c(e.ptr);
                                if ch == op {
                                    l += 1;
                                }
                                if ch == cp {
                                    l -= 1;
                                }
                                if ch == b'\\' && e.ptr + 1 < e.s.len() {
                                    e.ptr += 1;
                                }
                                e.ptr += 1;
                            }
                        }
                        e.yylval = sub(&e.s, p, e.ptr).to_vec();
                        return if func {
                            FUNC
                        } else if cct {
                            CID
                        } else {
                            ID
                        };
                    } else if cct {
                        e.yyval = MNumber::Int(i64::try_from(self.pparams().len()).unwrap_or(0));
                        return NUM;
                    }
                    return EOI;
                }
            }
        }
    }

    fn mpush(&mut self, e: &mut Eval, val: MNumber, lval: Option<Vec<u8>>, getme: bool) {
        if e.stack.len() >= STACKSZ {
            self.zerr("stack overflow");
            let _ = e.stack.pop();
        }
        e.stack.push(MathValue {
            lval,
            unset: getme,
            val,
        });
    }

    fn mpop(&mut self, e: &mut Eval, noget: bool) -> (MNumber, MathValue) {
        let Some(mut mv) = e.stack.pop() else {
            return (
                MNumber::Int(0),
                MathValue {
                    lval: None,
                    unset: false,
                    val: MNumber::Int(0),
                },
            );
        };
        if mv.unset && !noget {
            mv.val = self.getmathparam(&mv);
            mv.unset = false;
        }
        let v = if self.errflag() {
            MNumber::Int(0)
        } else {
            mv.val
        };
        (v, mv)
    }

    fn getcvar(&mut self, s: &[u8]) -> MNumber {
        match self.getsparam(s) {
            None => MNumber::Int(0),
            Some(t) => {
                if self.isset(MULTIBYTE) {
                    let (_, wc) = crate::utils::mb_metacharlenconv(self, &t);
                    if let Some(wc) = wc {
                        return MNumber::Int(i64::from(wc));
                    }
                }
                let c = at(&t, 0);
                MNumber::Int(i64::from(if c == tok::META { at(&t, 1) ^ 32 } else { c }))
            }
        }
    }

    fn setmathvar(&mut self, e: &Eval, mv: &MathValue, v: MNumber) -> MNumber {
        let Some(lval) = &mv.lval else {
            self.zerr("bad math expression: lvalue required");
            return MNumber::Int(0);
        };
        if e.noeval != 0 {
            return v;
        }
        let mut name = lval.clone();
        tok::untokenize(&mut name);
        match self.setnparam(&name, v) {
            Some(r) => match pm_type(self.pm_flags(&r)) {
                PM_INTEGER => MNumber::Int(v.as_int()),
                PM_EFLOAT | PM_FFLOAT => MNumber::Float(v.as_float()),
                _ => v,
            },
            None => v,
        }
    }

    fn callmathfunc(&mut self, o: &[u8]) -> MNumber {
        let Some(open) = o.iter().position(|&c| c == b'(') else {
            return MNumber::Int(0);
        };
        let n = sub(o, 0, open).to_vec();
        let mut a = sub(o, open + 1, o.len().saturating_sub(1)).to_vec();
        let Some(f) = self.getmathfunc(&n, true) else {
            self.zerr(&format!("unknown function: {}", crate::utils::lossy(&n)));
            return MNumber::Int(0);
        };
        if f.string && !f.user {
            return self.call_string_mathfunc(&f, &n, &a);
        }
        let mut sargs: Vec<Vec<u8>> = Vec::new();
        let mut nargs: Vec<MNumber> = Vec::new();
        if f.user {
            sargs.push(n.clone());
        }
        let mut argc = 0;
        if f.string {
            if a.is_empty() {
                sargs.push(Vec::new());
                argc += 1;
            }
        } else {
            let skip = a.iter().take_while(|&&c| self.iblank(c)).count();
            a.drain(..skip);
        }
        while !a.is_empty() {
            argc += 1;
            let mtok;
            if f.user {
                if f.string {
                    sargs.push(std::mem::take(&mut a));
                    mtok = EOI;
                } else {
                    let (marg, end, t) = self.mathevall(&a, false);
                    mtok = t;
                    sargs.push(match marg {
                        MNumber::Float(d) => crate::params::convfloat(d, 0, 0),
                        MNumber::Int(l) => self.convbase(l, 10),
                    });
                    a = from(&a, end).to_vec();
                }
            } else {
                let (marg, end, t) = self.mathevall(&a, false);
                mtok = t;
                nargs.push(marg);
                a = from(&a, end).to_vec();
            }
            if self.errflag() || mtok != COMMA {
                break;
            }
        }
        if let Some(&c) = a.first()
            && !self.errflag()
        {
            self.zerr(&format!(
                "bad math expression: illegal character: {}",
                char::from(c)
            ));
        }
        if !self.errflag() {
            if argc >= f.minargs
                && (f.maxargs < 0 || i32::try_from(argc).is_ok_and(|a| a <= f.maxargs))
            {
                if f.user {
                    let shfnam = f.module.clone().unwrap_or_else(|| n.clone());
                    if self.getshfunc(&shfnam).is_none() {
                        self.zerr(&format!(
                            "no such function: {}",
                            crate::utils::lossy(&shfnam)
                        ));
                    } else {
                        let _ = self.doshfunc_by_name(&shfnam, sargs, true);
                        return self.lastmathval;
                    }
                } else {
                    return self.call_numeric_mathfunc(&f, &n, &nargs);
                }
            } else {
                self.zerr(&format!(
                    "wrong number of arguments: {}",
                    crate::utils::lossy(o)
                ));
            }
        }
        MNumber::Int(0)
    }

    fn notzero(&mut self, a: MNumber) -> bool {
        if matches!(a, MNumber::Int(0)) {
            self.zerr("division by zero");
            return false;
        }
        true
    }

    #[expect(clippy::too_many_lines, reason = "zsh's op")]
    fn op(&mut self, e: &mut Eval, what: usize) {
        if self.errflag() {
            return;
        }
        if e.stack.is_empty() {
            self.zerr("bad math expression: stack empty");
            return;
        }
        let tp = ty(what);
        if tp & (OP_A2 | OP_A2IR | OP_A2IO | OP_E2 | OP_E2IO) != 0 {
            let (mut b, _) = self.mpop(e, false);
            let (mut a, amv) = self.mpop(e, what == EQ);
            if self.errflag() {
                return;
            }
            let a_unset = amv.unset;
            if tp & (OP_A2IO | OP_E2IO) != 0 {
                a = MNumber::Int(a.as_int());
                b = MNumber::Int(b.as_int());
            } else if a.is_float() != b.is_float() && what != COMMA && (!a_unset || what != EQ) {
                a = MNumber::Float(a.as_float());
                b = MNumber::Float(b.as_float());
            }
            let c: MNumber = if e.noeval != 0 {
                MNumber::Int(0)
            } else {
                let float = tp & OP_A2IR == 0 && a.is_float();
                let (al, bl) = (a.as_int(), b.as_int());
                let (ad, bd) = (a.as_float(), b.as_float());
                let bool_i = |x: bool| MNumber::Int(i64::from(x));
                match what {
                    AND | ANDEQ => MNumber::Int(al & bl),
                    XOR | XOREQ => MNumber::Int(al ^ bl),
                    OR | OREQ => MNumber::Int(al | bl),
                    MUL | MULEQ => {
                        if float {
                            MNumber::Float(ad * bd)
                        } else {
                            MNumber::Int(al.wrapping_mul(bl))
                        }
                    }
                    DIV | DIVEQ => {
                        if !self.notzero(b) {
                            return;
                        }
                        if float {
                            MNumber::Float(ad / bd)
                        } else if bl == -1 {
                            MNumber::Int(al.wrapping_neg())
                        } else {
                            MNumber::Int(al / bl)
                        }
                    }
                    MOD | MODEQ => {
                        if !self.notzero(b) {
                            return;
                        }
                        if float {
                            MNumber::Float(ad % bd)
                        } else if bl == -1 {
                            MNumber::Int(0)
                        } else {
                            MNumber::Int(al % bl)
                        }
                    }
                    PLUS | PLUSEQ => {
                        if float {
                            MNumber::Float(ad + bd)
                        } else {
                            MNumber::Int(al.wrapping_add(bl))
                        }
                    }
                    MINUS | MINUSEQ => {
                        if float {
                            MNumber::Float(ad - bd)
                        } else {
                            MNumber::Int(al.wrapping_sub(bl))
                        }
                    }
                    SHLEFT | SHLEFTEQ => {
                        MNumber::Int(al.wrapping_shl(u32::try_from(bl & 63).unwrap_or(0)))
                    }
                    SHRIGHT | SHRIGHTEQ => {
                        MNumber::Int(al.wrapping_shr(u32::try_from(bl & 63).unwrap_or(0)))
                    }
                    LES => bool_i(if a.is_float() { ad < bd } else { al < bl }),
                    LEQ => bool_i(if a.is_float() { ad <= bd } else { al <= bl }),
                    GRE => bool_i(if a.is_float() { ad > bd } else { al > bl }),
                    GEQ => bool_i(if a.is_float() { ad >= bd } else { al >= bl }),
                    #[expect(clippy::float_cmp, reason = "zsh compares exactly")]
                    DEQ => bool_i(if a.is_float() { ad == bd } else { al == bl }),
                    #[expect(clippy::float_cmp, reason = "zsh compares exactly")]
                    NEQ => bool_i(if a.is_float() { ad != bd } else { al != bl }),
                    DAND | DANDEQ => bool_i(al != 0 && bl != 0),
                    DOR | DOREQ => bool_i(al != 0 || bl != 0),
                    DXOR | DXOREQ => bool_i((al != 0) != (bl != 0)),
                    COMMA | EQ => b,
                    POWER | POWEREQ => {
                        if !float && bl < 0 {
                            MNumber::Float((al as f64).powf(bl as f64))
                        } else if !float {
                            let mut r: i64 = 1;
                            let mut n = bl;
                            while n > 0 {
                                r = r.wrapping_mul(al);
                                n -= 1;
                            }
                            MNumber::Int(r)
                        } else {
                            if bd <= 0.0 && !self.notzero(a) {
                                return;
                            }
                            #[expect(
                                clippy::cast_possible_truncation,
                                reason = "zsh casts to zlong"
                            )]
                            if ad < 0.0 && (bd as i64) as f64 != bd {
                                self.zerr("bad math expression: imaginary power");
                                return;
                            }
                            MNumber::Float(ad.powf(bd))
                        }
                    }
                    _ => MNumber::Int(0),
                }
            };
            if tp & (OP_E2 | OP_E2IO) != 0 {
                let c = self.setmathvar(e, &amv, c);
                self.mpush(e, c, amv.lval, false);
            } else {
                self.mpush(e, c, None, false);
            }
            return;
        }
        let Some(top) = e.stack.last().cloned() else {
            return;
        };
        let mut spval = if top.unset {
            self.getmathparam(&top)
        } else {
            top.val
        };
        match what {
            NOT => {
                spval = MNumber::Int(i64::from(match spval {
                    MNumber::Float(d) => d == 0.0,
                    MNumber::Int(l) => l == 0,
                }));
            }
            COMP => spval = MNumber::Int(!spval.as_int()),
            POSTPLUS | POSTMINUS => {
                let delta = if what == POSTPLUS { 1 } else { -1 };
                let a = match spval {
                    MNumber::Float(d) => MNumber::Float(d + f64::from(delta)),
                    MNumber::Int(l) => MNumber::Int(l.wrapping_add(i64::from(delta))),
                };
                let _ = self.setmathvar(e, &top, a);
            }
            UPLUS => {}
            UMINUS => {
                spval = match spval {
                    MNumber::Float(d) => MNumber::Float(-d),
                    MNumber::Int(l) => MNumber::Int(l.wrapping_neg()),
                };
            }
            QUEST => {
                let (c, _) = self.mpop(e, false);
                let (b, _) = self.mpop(e, false);
                let (a, _) = self.mpop(e, false);
                if self.errflag() {
                    return;
                }
                let t = match a {
                    MNumber::Float(d) => d != 0.0,
                    MNumber::Int(l) => l != 0,
                };
                self.mpush(e, if t { b } else { c }, None, false);
                return;
            }
            COLON => {
                self.zerr("bad math expression: ':' without '?'");
                return;
            }
            PREPLUS | PREMINUS => {
                let delta = if what == PREPLUS { 1 } else { -1 };
                spval = match spval {
                    MNumber::Float(d) => MNumber::Float(d + f64::from(delta)),
                    MNumber::Int(l) => MNumber::Int(l.wrapping_add(i64::from(delta))),
                };
                let _ = self.setmathvar(e, &top, spval);
            }
            _ => {
                self.zerr("bad math expression: out of integers");
                return;
            }
        }
        if let Some(last) = e.stack.last_mut() {
            last.val = spval;
            last.unset = false;
            last.lval = None;
        }
    }

    fn bop(&mut self, e: &mut Eval, tk: usize) {
        let Some(top) = e.stack.last().cloned() else {
            return;
        };
        let spval = if top.unset {
            let v = self.getmathparam(&top);
            if let Some(last) = e.stack.last_mut() {
                last.val = v;
                last.unset = false;
            }
            v
        } else {
            top.val
        };
        let tst = spval.as_int() != 0;
        match tk {
            DAND | DANDEQ if !tst => e.noeval += 1,
            DOR | DOREQ if tst => e.noeval += 1,
            _ => {}
        }
    }

    /// zsh's `matheval`.
    pub(crate) fn matheval(&mut self, s: &[u8]) -> MNumber {
        if self.mlevel == 0 {
            self.outputradix = 0;
            self.outputunderscore = 0;
        }
        let s = if at(s, 0) == tok::NULARG {
            from(s, 1)
        } else {
            s
        };
        if s.is_empty() {
            return MNumber::Int(0);
        }
        let (x, end, _) = self.mathevall(s, true);
        if end < s.len() {
            self.zerr(&format!(
                "bad math expression: illegal character: {}",
                char::from(at(s, end))
            ));
        }
        x
    }

    /// zsh's `mathevali`.
    pub(crate) fn mathevali(&mut self, s: &[u8]) -> i64 {
        self.matheval(s).as_int()
    }

    /// zsh's `mathevalarg`: `(value, index where the expression stopped)`.
    pub(crate) fn mathevalarg(&mut self, s: &[u8]) -> (i64, usize) {
        let off = usize::from(at(s, 0) == tok::NULARG);
        let s2 = from(s, off);
        if s2.is_empty() {
            self.zerr("bad math expression: empty string");
            return (0, off);
        }
        let (x, mut end, mtok) = self.mathevall(s2, false);
        if mtok == COMMA {
            end = end.saturating_sub(1);
        }
        (x.as_int(), end + off)
    }

    fn checkunary(&mut self, e: &mut Eval, mtokc: usize, mptr: usize) {
        let tp = ty(mtokc);
        let errmsg = if tp & (OP_A2 | OP_A2IR | OP_A2IO | OP_E2 | OP_E2IO | OP_OP) != 0 {
            if e.unary { 1 } else { 0 }
        } else if !e.unary {
            2
        } else {
            0
        };
        if errmsg != 0 {
            let errtype = if errmsg == 2 { "operator" } else { "operand" };
            let mut p = mptr;
            while self.inblank(e.c(p)) && p < e.s.len() {
                p += 1;
            }
            let rest = from(&e.s, p);
            if rest.is_empty() {
                self.zerr(&format!(
                    "bad math expression: {errtype} expected at end of string"
                ));
            } else {
                let len = crate::utils::ztrlen(rest);
                let shown: Vec<u8> = {
                    let mut out = Vec::new();
                    let mut k = 0;
                    let mut n = 0;
                    while k < rest.len() && n < 10 {
                        if at(rest, k) == tok::META {
                            out.push(at(rest, k));
                            k += 1;
                        }
                        out.push(at(rest, k));
                        k += 1;
                        n += 1;
                    }
                    out
                };
                self.zerr(&format!(
                    "bad math expression: {errtype} expected at `{}{}'",
                    crate::utils::lossy(&shown),
                    if len > 10 { "..." } else { "" }
                ));
            }
        }
        e.unary = tp & OP_OPF == 0;
    }

    fn mathparse(&mut self, e: &mut Eval, pc: i32) {
        if self.errflag() {
            return;
        }
        let mut optr = e.ptr;
        e.mtok = self.zzlex(e);
        if pc == e.toprec() && e.mtok == EOI {
            return;
        }
        let t = e.mtok;
        self.checkunary(e, t, optr);
        while e.prec(e.mtok) <= pc {
            if self.errflag() {
                return;
            }
            match e.mtok {
                NUM => {
                    let v = e.yyval;
                    self.mpush(e, v, None, false);
                }
                ID => {
                    let l = e.yylval.clone();
                    let get = e.noeval == 0;
                    self.mpush(e, MNumber::Int(0), Some(l), get);
                }
                CID => {
                    let l = e.yylval.clone();
                    let v = if e.noeval != 0 {
                        MNumber::Int(0)
                    } else {
                        self.getcvar(&l)
                    };
                    self.mpush(e, v, Some(l), false);
                }
                FUNC => {
                    let l = e.yylval.clone();
                    let saved_noeval = self.noeval;
                    self.noeval = e.noeval;
                    let v = if e.noeval != 0 {
                        MNumber::Int(0)
                    } else {
                        self.callmathfunc(&l)
                    };
                    self.noeval = saved_noeval;
                    self.mpush(e, v, Some(l), false);
                }
                M_INPAR => {
                    let top = e.toprec();
                    self.mathparse(e, top);
                    if e.mtok != M_OUTPAR {
                        if !self.errflag() {
                            self.zerr("bad math expression: ')' expected");
                        }
                        return;
                    }
                }
                QUEST => {
                    let q = match e.stack.last().cloned() {
                        Some(top) => {
                            let v = if top.unset {
                                self.getmathparam(&top)
                            } else {
                                top.val
                            };
                            if let Some(last) = e.stack.last_mut() {
                                last.val = v;
                                last.unset = false;
                            }
                            match v {
                                MNumber::Float(d) => d != 0.0,
                                MNumber::Int(l) => l != 0,
                            }
                        }
                        None => false,
                    };
                    if !q {
                        e.noeval += 1;
                    }
                    let p = e.prec(COLON) - 1;
                    self.mathparse(e, p);
                    if !q {
                        e.noeval -= 1;
                    }
                    if e.mtok != COLON {
                        if !self.errflag() {
                            self.zerr("bad math expression: ':' expected");
                        }
                        return;
                    }
                    if q {
                        e.noeval += 1;
                    }
                    let p = e.prec(QUEST);
                    self.mathparse(e, p);
                    if q {
                        e.noeval -= 1;
                    }
                    self.op(e, QUEST);
                    continue;
                }
                _ => {
                    let otok = e.mtok;
                    let onoeval = e.noeval;
                    if ty(otok) & 3 == BOOL {
                        self.bop(e, otok);
                    }
                    let p = e.prec(otok) - i32::from(ty(otok) & 3 != RL);
                    self.mathparse(e, p);
                    e.noeval = onoeval;
                    self.op(e, otok);
                    continue;
                }
            }
            optr = e.ptr;
            e.mtok = self.zzlex(e);
            let t = e.mtok;
            self.checkunary(e, t, optr);
        }
    }
}

/// C's `strtod` on ASCII: the value and how many bytes were used.
pub(crate) fn strtod(s: &[u8]) -> (f64, usize) {
    let mut i = 0;
    while matches!(at(s, i), b' ' | b'\t' | b'\n') {
        i += 1;
    }
    let start = i;
    if matches!(at(s, i), b'+' | b'-') {
        i += 1;
    }
    let digits_start = i;
    while at(s, i).is_ascii_digit() {
        i += 1;
    }
    let mut had_digits = i > digits_start;
    if at(s, i) == b'.' {
        let k = i + 1;
        let mut j = k;
        while at(s, j).is_ascii_digit() {
            j += 1;
        }
        if j > k || had_digits {
            had_digits |= j > k;
            i = j;
        }
    }
    if !had_digits {
        return (0.0, 0);
    }
    if matches!(at(s, i), b'e' | b'E') {
        let mut j = i + 1;
        if matches!(at(s, j), b'+' | b'-') {
            j += 1;
        }
        let k = j;
        while at(s, j).is_ascii_digit() {
            j += 1;
        }
        if j > k {
            i = j;
        }
    }
    let text = String::from_utf8_lossy(sub(s, start, i)).into_owned();
    (text.parse::<f64>().unwrap_or(0.0), i)
}

/// C's `%.*e`.
pub(crate) fn format_e(d: f64, prec: i32) -> String {
    let p = usize::try_from(prec.max(0)).unwrap_or(0);
    let s = format!("{:.*e}", p, d);
    // Rust writes `1.5e3`; C writes `1.5e+03`.
    match s.split_once('e') {
        Some((m, e)) => {
            let (sign, digits) = match e.strip_prefix('-') {
                Some(rest) => ('-', rest),
                None => ('+', e),
            };
            format!("{m}e{sign}{digits:0>2}")
        }
        None => s,
    }
}

/// C's `%.*g`.
pub(crate) fn format_g(d: f64, prec: i32) -> String {
    if d.is_nan() {
        return "nan".to_owned();
    }
    if d.is_infinite() {
        return if d < 0.0 {
            "-inf".to_owned()
        } else {
            "inf".to_owned()
        };
    }
    let p = if prec == 0 { 1 } else { prec.max(1) };
    if d == 0.0 {
        return if d.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }
    let e_form = format_e(d, p - 1);
    let exp: i32 = e_form
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);

    if exp < -4 || exp >= p {
        let (m, e) = e_form.split_once('e').unwrap_or((&e_form, "+00"));
        format!("{}e{e}", strip_zeros(m))
    } else {
        let decimals = usize::try_from(p - 1 - exp).unwrap_or(0);
        strip_zeros(&format!("{:.*}", decimals, d)).to_owned()
    }
}

fn strip_zeros(s: &str) -> &str {
    if s.contains('.') {
        let t = s.trim_end_matches('0');
        t.strip_suffix('.').unwrap_or(t)
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_c() {
        assert_eq!(format_g(0.1, 17), "0.10000000000000001");
        assert_eq!(format_g(1.5, 17), "1.5");
        assert_eq!(format_g(1e20, 17), "1e+20");
        assert_eq!(format_g(123456.0, 17), "123456");
        assert_eq!(format_e(1234.5, 3), "1.234e+03");
        assert_eq!(format_e(0.00012, 1), "1.2e-04");
    }

    #[test]
    fn reads_floats_like_strtod() {
        assert_eq!(strtod(b"1.5x"), (1.5, 3));
        assert_eq!(strtod(b"1e5"), (100000.0, 3));
        assert_eq!(strtod(b"1e"), (1.0, 1));
        assert_eq!(strtod(b".5"), (0.5, 2));
    }
}
