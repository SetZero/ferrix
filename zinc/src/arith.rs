//! Arithmetic evaluation: `$(( ))`, `(( ))`, `let`, integer parameters.
//! Integers only for now, with zsh's C-like precedence (zsh's `math.c`,
//! where `**` binds tighter than the multiplicative operators and looser than
//! unary minus is not the rule: unary operators bind tightest).

use crate::shell::Shell;

/// Evaluate `expr` (metafied, already substituted) in `sh`.
pub(crate) fn eval(sh: &mut Shell, expr: &[u8]) -> Result<i64, String> {
    let mut p = Arith {
        sh,
        s: expr,
        i: 0,
        skip: 0,
        depth: 0,
    };
    p.ws();
    if p.i >= p.s.len() {
        return Ok(0);
    }
    let v = p.comma()?;
    p.ws();
    if p.i < p.s.len() {
        let rest = String::from_utf8_lossy(p.s.get(p.i..).unwrap_or(&[])).into_owned();
        return Err(format!("bad math expression: illegal character: {rest}"));
    }
    Ok(v)
}

/// An lvalue or rvalue while parsing.
enum Operand {
    Num(i64),
    Var(Vec<u8>),
}

struct Arith<'a> {
    sh: &'a mut Shell,
    s: &'a [u8],
    i: usize,
    /// Inside the unevaluated side of `&&`, `||` or `?:`.
    skip: u32,
    depth: u32,
}

impl Arith<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn eat(&mut self, op: &[u8]) -> bool {
        self.ws();
        if self.s.get(self.i..).is_some_and(|r| r.starts_with(op)) {
            self.i += op.len();
            true
        } else {
            false
        }
    }

    /// Eat `op` unless it is the start of a longer operator in `not`.
    fn eat_not(&mut self, op: &[u8], not: &[&[u8]]) -> bool {
        self.ws();
        let rest = self.s.get(self.i..).unwrap_or(&[]);
        if not.iter().any(|n| rest.starts_with(n)) {
            return false;
        }
        self.eat(op)
    }

    fn value(&mut self, o: &Operand) -> Result<i64, String> {
        match o {
            Operand::Num(n) => Ok(*n),
            Operand::Var(name) => {
                let v = self.sh.get(name).map(|v| v.joined()).unwrap_or_default();
                let trimmed: Vec<u8> = v
                    .iter()
                    .copied()
                    .filter(|c| !c.is_ascii_whitespace())
                    .collect();
                if trimmed.is_empty() {
                    return Ok(0);
                }
                if let Ok(n) = std::str::from_utf8(&trimmed).unwrap_or("x").parse::<i64>() {
                    return Ok(n);
                }
                if self.depth > 64 {
                    return Err("math recursion limit exceeded".to_owned());
                }
                let mut sub = Arith {
                    sh: self.sh,
                    s: &v,
                    i: 0,
                    skip: self.skip,
                    depth: self.depth + 1,
                };
                sub.comma()
            }
        }
    }

    fn assign(&mut self, o: &Operand, v: i64) -> Result<i64, String> {
        match o {
            Operand::Var(name) => {
                if self.skip == 0 {
                    self.sh.set_scalar(name, v.to_string().into_bytes());
                }
                Ok(v)
            }
            Operand::Num(_) => Err("bad math expression: lvalue required".to_owned()),
        }
    }

    fn comma(&mut self) -> Result<i64, String> {
        let mut v = self.assignment()?;
        while self.eat(b",") {
            v = self.assignment()?;
        }
        Ok(v)
    }

    fn assignment(&mut self) -> Result<i64, String> {
        let save = self.i;
        self.ws();
        if let Some(name) = self.ident() {
            const OPS: &[&[u8]] = &[
                b"**=", b"<<=", b">>=", b"&&=", b"||=", b"^^=", b"+=", b"-=", b"*=", b"/=", b"%=",
                b"&=", b"|=", b"^=",
            ];
            for op in OPS {
                if self.eat(op) {
                    let rhs = self.assignment()?;
                    let o = Operand::Var(name);
                    let cur = self.value(&o)?;
                    let bin = op.get(..op.len() - 1).unwrap_or(&[]);
                    let v = self.binop(bin, cur, rhs)?;
                    return self.assign(&o, v);
                }
            }
            if self.eat_not(b"=", &[b"=="]) {
                let rhs = self.assignment()?;
                return self.assign(&Operand::Var(name), rhs);
            }
        }
        self.i = save;
        self.ternary()
    }

    fn ternary(&mut self) -> Result<i64, String> {
        let c = self.binary(0)?;
        if !self.eat(b"?") {
            return Ok(c);
        }
        if c == 0 {
            self.skip += 1;
        }
        let a = self.assignment()?;
        if c == 0 {
            self.skip -= 1;
        }
        if !self.eat(b":") {
            return Err("bad math expression: ':' expected".to_owned());
        }
        if c != 0 {
            self.skip += 1;
        }
        let b = self.assignment()?;
        if c != 0 {
            self.skip -= 1;
        }
        Ok(if c != 0 { a } else { b })
    }

    /// Binary operators by precedence level, loosest first.
    fn binary(&mut self, level: usize) -> Result<i64, String> {
        const LEVELS: &[&[&[u8]]] = &[
            &[b"||", b"^^"],
            &[b"&&"],
            &[b"|"],
            &[b"^"],
            &[b"&"],
            &[b"==", b"!="],
            &[b"<=", b">=", b"<", b">"],
            &[b"<<", b">>"],
            &[b"+", b"-"],
            &[b"*", b"/", b"%"],
            &[b"**"],
        ];
        let Some(ops) = LEVELS.get(level) else {
            return self.unary();
        };
        let mut left = self.binary(level + 1)?;
        'outer: loop {
            for &op in *ops {
                let longer: &[&[u8]] = match op {
                    b"|" => &[b"||", b"|="],
                    b"&" => &[b"&&", b"&="],
                    b"^" => &[b"^^", b"^="],
                    b"<" => &[b"<<", b"<="],
                    b">" => &[b">>", b">="],
                    b"*" => &[b"**", b"*="],
                    b"+" => &[b"++", b"+="],
                    b"-" => &[b"--", b"-="],
                    b"/" => &[b"/="],
                    b"%" => &[b"%="],
                    b"<<" => &[b"<<="],
                    b">>" => &[b">>="],
                    b"**" => &[b"**="],
                    b"&&" => &[b"&&="],
                    b"||" => &[b"||="],
                    _ => &[],
                };
                if self.eat_not(op, longer) {
                    let short = (op == b"&&" && left == 0) || (op == b"||" && left != 0);
                    if short {
                        self.skip += 1;
                    }
                    let right = if op == b"**" {
                        self.binary(level)?
                    } else {
                        self.binary(level + 1)?
                    };
                    if short {
                        self.skip -= 1;
                    }
                    left = self.binop(op, left, right)?;
                    continue 'outer;
                }
            }
            return Ok(left);
        }
    }

    fn binop(&self, op: &[u8], a: i64, b: i64) -> Result<i64, String> {
        Ok(match op {
            b"||" => i64::from(a != 0 || b != 0),
            b"^^" => i64::from((a != 0) != (b != 0)),
            b"&&" => i64::from(a != 0 && b != 0),
            b"|" => a | b,
            b"^" => a ^ b,
            b"&" => a & b,
            b"==" => i64::from(a == b),
            b"!=" => i64::from(a != b),
            b"<=" => i64::from(a <= b),
            b">=" => i64::from(a >= b),
            b"<" => i64::from(a < b),
            b">" => i64::from(a > b),
            b"<<" => a.wrapping_shl(u32::try_from(b).unwrap_or(0)),
            b">>" => a.wrapping_shr(u32::try_from(b).unwrap_or(0)),
            b"+" => a.wrapping_add(b),
            b"-" => a.wrapping_sub(b),
            b"*" => a.wrapping_mul(b),
            b"/" | b"%" => {
                if b == 0 {
                    if self.skip > 0 {
                        return Ok(0);
                    }
                    return Err("division by zero".to_owned());
                }
                if op == b"/" {
                    a.wrapping_div(b)
                } else {
                    a.wrapping_rem(b)
                }
            }
            b"**" => {
                if b < 0 {
                    0
                } else {
                    a.wrapping_pow(u32::try_from(b).unwrap_or(u32::MAX))
                }
            }
            _ => return Err("bad math expression".to_owned()),
        })
    }

    fn unary(&mut self) -> Result<i64, String> {
        if self.eat(b"++") || self.eat(b"--") {
            let inc = self.s.get(self.i - 1) == Some(&b'+');
            self.ws();
            let Some(name) = self.ident() else {
                return Err("bad math expression: lvalue required".to_owned());
            };
            let o = Operand::Var(name);
            let v = self.value(&o)? + if inc { 1 } else { -1 };
            return self.assign(&o, v);
        }
        if self.eat(b"!") {
            return Ok(i64::from(self.unary()? == 0));
        }
        if self.eat(b"~") {
            return Ok(!self.unary()?);
        }
        if self.eat_not(b"-", &[b"--"]) {
            return Ok(self.unary()?.wrapping_neg());
        }
        if self.eat_not(b"+", &[b"++"]) {
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<i64, String> {
        let o = self.primary()?;
        if matches!(o, Operand::Var(_)) {
            if self.eat(b"++") {
                let v = self.value(&o)?;
                let _new = self.assign(&o, v + 1)?;
                return Ok(v);
            }
            if self.eat(b"--") {
                let v = self.value(&o)?;
                let _new = self.assign(&o, v - 1)?;
                return Ok(v);
            }
        }
        self.value(&o)
    }

    fn ident(&mut self) -> Option<Vec<u8>> {
        let start = self.i;
        if !self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_' || *c >= 0x80)
        {
            return None;
        }
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c >= 0x80)
        {
            self.i += 1;
        }
        self.s.get(start..self.i).map(<[u8]>::to_vec)
    }

    fn primary(&mut self) -> Result<Operand, String> {
        self.ws();
        if self.eat(b"(") {
            let v = self.comma()?;
            if !self.eat(b")") {
                return Err("bad math expression: ')' expected".to_owned());
            }
            return Ok(Operand::Num(v));
        }
        if self.eat(b"$") {
            // A `$name` left in by a caller that did not substitute.
            return self.primary();
        }
        if self.eat(b"##") {
            let c = self.s.get(self.i).copied().unwrap_or(0);
            self.i += 1;
            return Ok(Operand::Num(i64::from(c)));
        }
        if self.eat(b"#") {
            let Some(name) = self.ident() else {
                return Err("bad math expression".to_owned());
            };
            let v = self.sh.get(&name).map(|v| v.joined()).unwrap_or_default();
            return Ok(Operand::Num(v.first().map_or(0, |&c| i64::from(c))));
        }
        if let Some(name) = self.ident() {
            return Ok(Operand::Var(name));
        }
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'#' || *c == b'.')
        {
            self.i += 1;
        }
        let lit = std::str::from_utf8(self.s.get(start..self.i).unwrap_or(&[])).unwrap_or("");
        if lit.is_empty() {
            return Err("bad math expression: operand expected at end of string".to_owned());
        }
        parse_number(lit)
            .map(Operand::Num)
            .ok_or_else(|| format!("bad math expression: {lit}"))
    }
}

fn parse_number(lit: &str) -> Option<i64> {
    if let Some((base, digits)) = lit.split_once('#') {
        let base: u32 = base.parse().ok()?;
        return i64::from_str_radix(digits, base).ok();
    }
    if let Some(hex) = lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(bin) = lit.strip_prefix("0b") {
        return i64::from_str_radix(bin, 2).ok();
    }
    if lit.contains('.') {
        return lit.parse::<f64>().ok().map(|f| f as i64);
    }
    lit.parse().ok()
}
