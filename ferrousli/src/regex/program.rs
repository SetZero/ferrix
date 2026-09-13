//! Compiling a syntax tree into a Thompson automaton, and running any part of
//! it forwards or backwards over any part of the string.
//!
//! Every node compiles to a contiguous run of instructions that is entered at
//! its first and left only by falling through to the one after its last. So a
//! node, or the rest of a concatenation from one child on, or the remaining
//! iterations of a repetition, is a range of instructions `[start, end)` that
//! can be run on its own: forwards from a position, to find every position it
//! can end at, or backwards from a position, to find every position it can
//! start at. [`super::solve`] uses both to split a match among subexpressions.
//!
//! A counted repetition is expanded, `a{2,4}` into `a a (a (a)?)?`, so the
//! remaining iterations after any count are also a range. A node inside a
//! repetition is compiled once per copy; its range is that of the first copy,
//! which is as good as any, since every copy's jumps stay inside it.
//!
//! Each simulation keeps a set of instructions and steps it one byte at a
//! time, so it takes time proportional to the length of the range of the
//! string times the number of instructions, whatever the pattern.
//!
//! A back-reference cannot be simulated this way. Here it is widened to match
//! any string, so a simulation of a pattern with back-references finds a
//! superset of the true positions, which the solver then checks.

use super::parse::{Assertion, Ast, ByteSet, INFINITE, Kind, NodeId};
use crate::growable::Growable;

/// One instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Inst {
    /// Consume this byte.
    Byte(u8),
    /// Consume a byte of this set.
    Set(u32),
    /// Continue at both.
    Split(u32, u32),
    /// Continue there.
    Jmp(u32),
    /// Continue if the assertion holds here.
    Assert(Assertion),
    /// A back-reference, simulated as any string.
    Backref(u32),
    /// The whole pattern matched.
    Match,
}

/// A node's width when it is not fixed.
pub(super) const VARIABLE: u32 = u32::MAX;

/// The most instructions a pattern may compile to.
const MAX_INSTS: usize = 1 << 22;

/// A compiled pattern.
#[derive(Debug)]
pub(super) struct Program {
    /// The instructions; the last is `Match`.
    insts: Growable<Inst>,
    /// Each node's range `[start, end)`, from its first copy.
    ranges: Growable<(u32, u32)>,
    /// The start of each copy of every repetition, a repetition's contiguous.
    copies: Growable<u32>,
    /// Each repetition's first index into `copies`, and how many it has.
    copy_index: Growable<(u32, u32)>,
    /// Each node's width, or [`VARIABLE`].
    widths: Growable<u32>,
    /// Whether each node contains a group or a back-reference.
    captures: Growable<bool>,
    /// For each child list entry, the fixed width of it and the children after
    /// it in its list, or [`VARIABLE`].
    suffix_widths: Growable<u32>,
    /// Where each instruction's predecessors start in `preds`; one entry more
    /// than there are instructions.
    pred_start: Growable<u32>,
    /// The predecessors of each instruction, grouped by instruction.
    preds: Growable<u32>,
}

/// An allocation failed.
#[derive(Debug, Clone, Copy)]
pub(super) struct NoMemory;

/// A result that fails only for lack of memory.
pub(super) type Mem<T> = Result<T, NoMemory>;

/// Turns a failed growth into [`NoMemory`].
fn grew(ok: bool) -> Mem<()> {
    if ok { Ok(()) } else { Err(NoMemory) }
}

impl Program {
    /// Compiles `ast`.
    pub(super) fn compile(ast: &Ast) -> Mem<Self> {
        let n = ast.nodes.len();
        let mut p = Self {
            insts: Growable::new(),
            ranges: Growable::new(),
            copies: Growable::new(),
            copy_index: Growable::new(),
            widths: Growable::new(),
            captures: Growable::new(),
            suffix_widths: Growable::new(),
            pred_start: Growable::new(),
            preds: Growable::new(),
        };
        grew(p.ranges.reserve(n) && p.copy_index.reserve(n) && p.widths.reserve(n))?;
        grew(p.captures.reserve(n) && p.suffix_widths.reserve(ast.children.len()))?;
        for _ in 0..n {
            let _ = p.ranges.push((u32::MAX, u32::MAX));
            let _ = p.copy_index.push((0, 0));
        }
        for _ in 0..ast.children.len() {
            let _ = p.suffix_widths.push(VARIABLE);
        }
        p.measure(ast);
        p.emit_node(ast, ast.root)?;
        let _ = p.emit(Inst::Match)?;
        p.link()?;
        Ok(p)
    }

    /// Instruction `pc`, or `Match` past the end.
    pub(super) fn inst(&self, pc: u32) -> Inst {
        self.insts.as_slice().get(pc as usize).copied().unwrap_or(Inst::Match)
    }

    /// The number of instructions.
    pub(super) fn len(&self) -> usize {
        self.insts.len()
    }

    /// The `Match` instruction's address.
    pub(super) fn match_pc(&self) -> u32 {
        (self.insts.len() as u32).saturating_sub(1)
    }

    /// Node `id`'s range.
    pub(super) fn range(&self, id: NodeId) -> (u32, u32) {
        self.ranges.as_slice().get(id as usize).copied().unwrap_or((0, 0))
    }

    /// Node `id`'s width, or [`VARIABLE`].
    pub(super) fn width(&self, id: NodeId) -> u32 {
        self.widths.as_slice().get(id as usize).copied().unwrap_or(VARIABLE)
    }

    /// Whether node `id` contains a group or a back-reference.
    pub(super) fn captures(&self, id: NodeId) -> bool {
        self.captures.as_slice().get(id as usize).copied().unwrap_or(true)
    }

    /// The fixed width of child list entries from `index` to the end of its
    /// list, or [`VARIABLE`].
    pub(super) fn suffix_width(&self, index: u32) -> u32 {
        self.suffix_widths.as_slice().get(index as usize).copied().unwrap_or(VARIABLE)
    }

    /// Where the iterations of repetition `id` after the first `count` start.
    /// The range from there to the repetition's end matches exactly the
    /// strings those iterations can.
    pub(super) fn remaining(&self, id: NodeId, count: u32, max: u32) -> u32 {
        let (first, copies) = self.copy_index.as_slice().get(id as usize).copied().unwrap_or((0, 0));
        let index = if count < copies {
            count
        } else if max == INFINITE {
            copies.saturating_sub(1)
        } else {
            return self.range(id).1;
        };
        self.copies
            .as_slice()
            .get((first + index) as usize)
            .copied()
            .unwrap_or_else(|| self.range(id).1)
    }

    /// Works out every node's width and whether it captures. Children come
    /// before their parents, so one pass in order suffices.
    fn measure(&mut self, ast: &Ast) {
        for id in 0..ast.nodes.len() as u32 {
            let (width, captures) = match ast.kind(id) {
                Kind::Empty | Kind::Assert(_) => (0, false),
                Kind::Byte(_) | Kind::Set(_) => (1, false),
                Kind::Backref(_) => (VARIABLE, true),
                Kind::Group(_, inner) => (self.width(inner), true),
                Kind::Concat(first, count) => {
                    let mut total = 0u32;
                    let mut captures = false;
                    let mut i = count;
                    while i > 0 {
                        i -= 1;
                        let child = ast.child(first, i);
                        let w = self.width(child);
                        total = if w == VARIABLE || total == VARIABLE {
                            VARIABLE
                        } else {
                            total.checked_add(w).filter(|&t| t != VARIABLE).unwrap_or(VARIABLE)
                        };
                        captures |= self.captures(child);
                        if let Some(slot) = self.suffix_widths.get_mut((first + i) as usize) {
                            *slot = total;
                        }
                    }
                    (total, captures)
                }
                Kind::Alt(first, count) => {
                    let mut width = None;
                    let mut captures = false;
                    for i in 0..count {
                        let child = ast.child(first, i);
                        let w = self.width(child);
                        width = match width {
                            None => Some(w),
                            Some(prior) if prior == w => Some(w),
                            Some(_) => Some(VARIABLE),
                        };
                        captures |= self.captures(child);
                    }
                    (width.unwrap_or(0), captures)
                }
                Kind::Repeat(inner, min, max) => {
                    let w = self.width(inner);
                    let width = if w == 0 {
                        0
                    } else if w != VARIABLE && min == max {
                        w.checked_mul(min).filter(|&t| t != VARIABLE).unwrap_or(VARIABLE)
                    } else {
                        VARIABLE
                    };
                    (width, self.captures(inner))
                }
            };
            if let Some(slot) = self.widths.get_mut(id as usize) {
                *slot = width;
            } else {
                let _ = self.widths.push(width);
            }
            if let Some(slot) = self.captures.get_mut(id as usize) {
                *slot = captures;
            } else {
                let _ = self.captures.push(captures);
            }
        }
    }

    /// Appends an instruction and returns its address.
    fn emit(&mut self, inst: Inst) -> Mem<u32> {
        let pc = self.insts.len();
        if pc >= MAX_INSTS || !self.insts.push(inst) {
            return Err(NoMemory);
        }
        Ok(pc as u32)
    }

    /// The next instruction's address.
    fn here(&self) -> u32 {
        self.insts.len() as u32
    }

    /// Replaces the instruction at `pc`.
    fn patch(&mut self, pc: u32, inst: Inst) {
        if let Some(slot) = self.insts.get_mut(pc as usize) {
            *slot = inst;
        }
    }

    /// Compiles node `id`, recording its range the first time.
    fn emit_node(&mut self, ast: &Ast, id: NodeId) -> Mem<()> {
        let start = self.here();
        match ast.kind(id) {
            Kind::Empty => {}
            Kind::Byte(b) => {
                let _ = self.emit(Inst::Byte(b))?;
            }
            Kind::Set(s) => {
                let _ = self.emit(Inst::Set(s))?;
            }
            Kind::Assert(a) => {
                let _ = self.emit(Inst::Assert(a))?;
            }
            Kind::Backref(n) => {
                let _ = self.emit(Inst::Backref(n))?;
            }
            Kind::Group(_, inner) => self.emit_node(ast, inner)?,
            Kind::Concat(first, count) => {
                for i in 0..count {
                    self.emit_node(ast, ast.child(first, i))?;
                }
            }
            Kind::Alt(first, count) => self.emit_alternation(ast, first, count)?,
            Kind::Repeat(inner, min, max) => self.emit_repeat(ast, id, inner, min, max)?,
        }
        let end = self.here();
        if let Some(range) = self.ranges.get_mut(id as usize)
            && range.0 == u32::MAX
        {
            *range = (start, end);
        }
        Ok(())
    }

    /// Compiles an alternation: a split before each alternative but the last,
    /// and a jump to the end after each.
    fn emit_alternation(&mut self, ast: &Ast, first: u32, count: u32) -> Mem<()> {
        let mut jumps = Growable::new();
        for i in 0..count {
            let child = ast.child(first, i);
            if i + 1 == count {
                self.emit_node(ast, child)?;
                break;
            }
            let split = self.emit(Inst::Split(0, 0))?;
            self.emit_node(ast, child)?;
            let jump = self.emit(Inst::Jmp(0))?;
            grew(jumps.push(jump))?;
            let next = self.here();
            self.patch(split, Inst::Split(split + 1, next));
        }
        let end = self.here();
        for &jump in jumps.as_slice() {
            self.patch(jump, Inst::Jmp(end));
        }
        Ok(())
    }

    /// Compiles a repetition: `min` copies, then a loop if it is unbounded, or
    /// nested optional copies up to `max`.
    fn emit_repeat(&mut self, ast: &Ast, id: NodeId, inner: NodeId, min: u32, max: u32) -> Mem<()> {
        let mut starts = Growable::new();
        for _ in 0..min {
            grew(starts.push(self.here()))?;
            self.emit_node(ast, inner)?;
        }
        if max == INFINITE {
            let top = self.here();
            grew(starts.push(top))?;
            let split = self.emit(Inst::Split(top + 1, 0))?;
            self.emit_node(ast, inner)?;
            let _ = self.emit(Inst::Jmp(top))?;
            let end = self.here();
            self.patch(split, Inst::Split(top + 1, end));
        } else {
            let mut splits = Growable::new();
            for _ in min..max {
                let split = self.here();
                grew(starts.push(split))?;
                let _ = self.emit(Inst::Split(split + 1, 0))?;
                grew(splits.push(split))?;
                self.emit_node(ast, inner)?;
            }
            let end = self.here();
            for &split in splits.as_slice() {
                self.patch(split, Inst::Split(split + 1, end));
            }
        }
        let recorded = self.copy_index.as_slice().get(id as usize).is_some_and(|&(_, n)| n > 0);
        if !recorded {
            let first = self.copies.len() as u32;
            grew(self.copies.reserve(starts.len()))?;
            for &s in starts.as_slice() {
                let _ = self.copies.push(s);
            }
            if let Some(slot) = self.copy_index.get_mut(id as usize) {
                *slot = (first, starts.len() as u32);
            }
        }
        Ok(())
    }

    /// The instructions that can follow `pc`.
    fn successors(&self, pc: u32) -> [Option<u32>; 2] {
        match self.inst(pc) {
            Inst::Byte(_) | Inst::Set(_) | Inst::Assert(_) => [Some(pc + 1), None],
            Inst::Backref(_) => [Some(pc + 1), Some(pc)],
            Inst::Jmp(t) => [Some(t), None],
            Inst::Split(a, b) => [Some(a), Some(b)],
            Inst::Match => [None, None],
        }
    }

    /// Builds the predecessor lists the backward simulation walks.
    fn link(&mut self) -> Mem<()> {
        let n = self.insts.len();
        grew(self.pred_start.reserve(n + 1))?;
        for _ in 0..=n {
            let _ = self.pred_start.push(0);
        }
        // Count, then turn the counts into starting offsets.
        for pc in 0..n as u32 {
            for next in self.successors(pc).into_iter().flatten() {
                if let Some(count) = self.pred_start.get_mut(next as usize + 1) {
                    *count += 1;
                }
            }
        }
        let mut total = 0;
        for slot in self.pred_start.as_mut_slice() {
            total += *slot;
            *slot = total;
        }
        grew(self.preds.reserve(total as usize))?;
        for _ in 0..total {
            let _ = self.preds.push(0);
        }
        let mut fill: Growable<u32> = Growable::new();
        grew(fill.reserve(n))?;
        for &start in self.pred_start.as_slice().iter().take(n) {
            let _ = fill.push(start);
        }
        for pc in 0..n as u32 {
            for next in self.successors(pc).into_iter().flatten() {
                let Some(at) = fill.get_mut(next as usize) else {
                    continue;
                };
                if let Some(slot) = self.preds.get_mut(*at as usize) {
                    *slot = pc;
                }
                *at += 1;
            }
        }
        Ok(())
    }

    /// The predecessors of `pc`.
    fn predecessors(&self, pc: u32) -> &[u32] {
        let starts = self.pred_start.as_slice();
        let from = starts.get(pc as usize).copied().unwrap_or(0) as usize;
        let to = starts.get(pc as usize + 1).copied().unwrap_or(0) as usize;
        self.preds.as_slice().get(from..to).unwrap_or(&[])
    }
}

/// The string being matched, and what the assertions need to know about it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Input<'a> {
    /// The string, without its NUL.
    pub(super) bytes: &'a [u8],
    /// `REG_NOTBOL`: its start is not the start of a line.
    pub(super) notbol: bool,
    /// `REG_NOTEOL`: its end is not the end of a line.
    pub(super) noteol: bool,
    /// `REG_NEWLINE`: newlines separate lines.
    pub(super) newline: bool,
    /// `REG_ICASE`, for back-references.
    pub(super) icase: bool,
}

/// Whether `b` is a word character: a letter, a digit or `_`.
fn is_word(b: Option<u8>) -> bool {
    b.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Input<'_> {
    /// Whether `assertion` holds at `pos`.
    pub(super) fn holds(&self, assertion: Assertion, pos: usize) -> bool {
        let before = pos.checked_sub(1).and_then(|i| self.bytes.get(i)).copied();
        let after = self.bytes.get(pos).copied();
        match assertion {
            Assertion::LineStart => (pos == 0 && !self.notbol) || (self.newline && before == Some(b'\n')),
            Assertion::LineEnd => (after.is_none() && !self.noteol) || (self.newline && after == Some(b'\n')),
            Assertion::BufferStart => pos == 0,
            Assertion::BufferEnd => after.is_none(),
            Assertion::WordBoundary => is_word(before) != is_word(after),
            Assertion::NotWordBoundary => is_word(before) == is_word(after),
            Assertion::WordStart => !is_word(before) && is_word(after),
            Assertion::WordEnd => is_word(before) && !is_word(after),
        }
    }

    /// The byte at `pos`.
    pub(super) fn byte(&self, pos: usize) -> Option<u8> {
        self.bytes.get(pos).copied()
    }
}

/// A set of string positions from `lo` to `hi`.
#[derive(Debug)]
pub(super) struct Positions {
    /// The lowest position the set can hold.
    lo: usize,
    /// The highest.
    hi: usize,
    /// One bit per position.
    words: Growable<u64>,
}

impl Positions {
    /// An empty set for `lo..=hi`.
    fn new(lo: usize, hi: usize) -> Mem<Self> {
        let count = (hi.saturating_sub(lo) + 1).div_ceil(64);
        let mut words = Growable::new();
        grew(words.reserve(count))?;
        for _ in 0..count {
            let _ = words.push(0);
        }
        Ok(Self { lo, hi, words })
    }

    /// Adds `pos`.
    fn insert(&mut self, pos: usize) {
        let Some(offset) = pos.checked_sub(self.lo) else {
            return;
        };
        if let Some(word) = self.words.get_mut(offset / 64) {
            *word |= 1 << (offset % 64);
        }
    }

    /// Whether `pos` is in the set.
    pub(super) fn contains(&self, pos: usize) -> bool {
        let Some(offset) = pos.checked_sub(self.lo) else {
            return false;
        };
        pos <= self.hi
            && self
                .words
                .as_slice()
                .get(offset / 64)
                .is_some_and(|w| w & (1 << (offset % 64)) != 0)
    }

    /// The highest position in the set that is at most `pos`.
    pub(super) fn highest_at_most(&self, pos: usize) -> Option<usize> {
        let top = pos.min(self.hi).checked_sub(self.lo)?;
        let mut index = top / 64;
        let mut word = self.words.as_slice().get(index).copied()? & (u64::MAX >> (63 - top % 64));
        loop {
            if word != 0 {
                return Some(self.lo + index * 64 + 63 - word.leading_zeros() as usize);
            }
            index = index.checked_sub(1)?;
            word = self.words.as_slice().get(index).copied()?;
        }
    }
}

/// A set of instructions, with the start position each was reached from.
#[derive(Debug)]
struct PcSet {
    /// For each instruction, the generation in which it was last added.
    mark: Growable<u32>,
    /// For each instruction, the start position recorded with it.
    start: Growable<usize>,
    /// The current generation.
    generation: u32,
    /// The instructions in the set, in the order added.
    list: Growable<u32>,
}

impl PcSet {
    /// A set for `n` instructions.
    fn new(n: usize) -> Mem<Self> {
        let mut set = Self {
            mark: Growable::new(),
            start: Growable::new(),
            generation: 1,
            list: Growable::new(),
        };
        grew(set.mark.reserve(n) && set.start.reserve(n) && set.list.reserve(n))?;
        for _ in 0..n {
            let _ = set.mark.push(0);
            let _ = set.start.push(0);
        }
        Ok(set)
    }

    /// Empties the set.
    fn clear(&mut self) {
        self.list.truncate(0);
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            for m in self.mark.as_mut_slice() {
                *m = 0;
            }
            self.generation = 1;
        }
    }

    /// Whether `pc` is in the set.
    fn contains(&self, pc: u32) -> bool {
        self.mark.as_slice().get(pc as usize) == Some(&self.generation)
    }

    /// Adds `pc` with `start`; returns `false` if it was already there.
    fn insert(&mut self, pc: u32, start: usize) -> bool {
        let generation = self.generation;
        let Some(mark) = self.mark.get_mut(pc as usize) else {
            return false;
        };
        if *mark == generation {
            return false;
        }
        *mark = generation;
        if let Some(slot) = self.start.get_mut(pc as usize) {
            *slot = start;
        }
        let _ = self.list.push(pc);
        true
    }

    /// The start recorded with `pc`.
    fn start_of(&self, pc: u32) -> usize {
        self.start.as_slice().get(pc as usize).copied().unwrap_or(usize::MAX)
    }

    /// Instruction number `i` of the list.
    fn nth(&self, i: usize) -> Option<u32> {
        self.list.as_slice().get(i).copied()
    }
}

/// The simulations' working memory, reused from one run to the next.
#[derive(Debug)]
pub(super) struct Machine<'a> {
    /// The program.
    prog: &'a Program,
    /// The byte sets its instructions name.
    sets: &'a [ByteSet],
    /// The string.
    pub(super) input: Input<'a>,
    /// The instructions live at the current position.
    cur: PcSet,
    /// The instructions live at the next position.
    next: PcSet,
    /// The pending instructions of a closure.
    stack: Growable<u32>,
}

impl<'a> Machine<'a> {
    /// A machine for running `prog` over `input`.
    pub(super) fn new(prog: &'a Program, sets: &'a [ByteSet], input: Input<'a>) -> Mem<Self> {
        let n = prog.len();
        let mut stack = Growable::new();
        grew(stack.reserve(n.saturating_mul(2)))?;
        Ok(Self {
            prog,
            sets,
            input,
            cur: PcSet::new(n)?,
            next: PcSet::new(n)?,
            stack,
        })
    }

    /// The program.
    pub(super) fn program(&self) -> &'a Program {
        self.prog
    }

    /// Whether instruction `pc`, if it consumes, consumes the byte at `pos`.
    fn consumes(&self, pc: u32, pos: usize) -> bool {
        let Some(b) = self.input.byte(pos) else {
            return false;
        };
        match self.prog.inst(pc) {
            Inst::Byte(c) => c == b,
            Inst::Set(s) => self.sets.get(s as usize).is_some_and(|set| set.contains(b)),
            Inst::Backref(_) => true,
            _ => false,
        }
    }

    /// Adds `pc` and everything reachable from it without consuming, at `pos`,
    /// to `next` (or `cur` if `into_cur`), keeping to `lo..=hi` and not going
    /// past `hi`.
    fn close_forward(&mut self, into_cur: bool, pc: u32, pos: usize, start: usize, lo: u32, hi: u32) -> Mem<()> {
        self.stack.truncate(0);
        grew(self.stack.push(pc))?;
        while let Some(q) = self.stack.pop() {
            if q < lo || q > hi {
                continue;
            }
            let set = if into_cur { &mut self.cur } else { &mut self.next };
            if !set.insert(q, start) || q == hi {
                continue;
            }
            match self.prog.inst(q) {
                Inst::Jmp(t) => grew(self.stack.push(t))?,
                Inst::Split(a, b) => grew(self.stack.push(b) && self.stack.push(a))?,
                Inst::Assert(k) if self.input.holds(k, pos) => grew(self.stack.push(q + 1))?,
                Inst::Backref(_) => grew(self.stack.push(q + 1))?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Adds `pc` and everything that reaches it without consuming, at `pos`,
    /// to `next` (or `cur`), keeping to `lo..=hi`.
    fn close_backward(&mut self, into_cur: bool, pc: u32, pos: usize, lo: u32, hi: u32) -> Mem<()> {
        self.stack.truncate(0);
        grew(self.stack.push(pc))?;
        while let Some(q) = self.stack.pop() {
            if q < lo || q > hi {
                continue;
            }
            let set = if into_cur { &mut self.cur } else { &mut self.next };
            if !set.insert(q, 0) {
                continue;
            }
            for &p in self.prog.predecessors(q) {
                if p < lo || p >= hi {
                    continue;
                }
                let epsilon = match self.prog.inst(p) {
                    Inst::Jmp(_) | Inst::Split(_, _) => true,
                    Inst::Assert(k) => self.input.holds(k, pos),
                    Inst::Backref(_) => p + 1 == q,
                    _ => false,
                };
                if epsilon {
                    grew(self.stack.push(p))?;
                }
            }
        }
        Ok(())
    }

    /// Every position in `from..=to` at which the range `[start, end)`, run
    /// from `from`, can end.
    pub(super) fn forward(&mut self, start: u32, end: u32, from: usize, to: usize) -> Mem<Positions> {
        let mut found = Positions::new(from, to)?;
        self.cur.clear();
        self.close_forward(true, start, from, 0, start, end)?;
        let mut pos = from;
        loop {
            if self.cur.contains(end) {
                found.insert(pos);
            }
            if pos >= to {
                break;
            }
            self.next.clear();
            let mut i = 0;
            while let Some(q) = self.cur.nth(i) {
                i += 1;
                if q == end || !self.consumes(q, pos) {
                    continue;
                }
                let target = if matches!(self.prog.inst(q), Inst::Backref(_)) { q } else { q + 1 };
                self.close_forward(false, target, pos + 1, 0, start, end)?;
            }
            core::mem::swap(&mut self.cur, &mut self.next);
            pos += 1;
            if self.cur.list.as_slice().is_empty() {
                break;
            }
        }
        Ok(found)
    }

    /// Every position in `lo..=to` from which the range `[start, end)` can
    /// match up to `to`.
    pub(super) fn backward(&mut self, start: u32, end: u32, lo: usize, to: usize) -> Mem<Positions> {
        let mut found = Positions::new(lo, to)?;
        self.cur.clear();
        self.close_backward(true, end, to, start, end)?;
        let mut pos = to;
        loop {
            if self.cur.contains(start) {
                found.insert(pos);
            }
            if pos <= lo {
                break;
            }
            self.next.clear();
            let mut i = 0;
            while let Some(q) = self.cur.nth(i) {
                i += 1;
                for &p in self.prog.predecessors(q) {
                    if p < start || p >= end {
                        continue;
                    }
                    let consuming = match self.prog.inst(p) {
                        Inst::Byte(_) | Inst::Set(_) => p + 1 == q,
                        Inst::Backref(_) => p == q,
                        _ => false,
                    };
                    if consuming && self.consumes(p, pos - 1) {
                        self.close_backward(false, p, pos - 1, start, end)?;
                    }
                }
            }
            core::mem::swap(&mut self.cur, &mut self.next);
            pos -= 1;
            if self.cur.list.as_slice().is_empty() {
                break;
            }
        }
        Ok(found)
    }

    /// The leftmost-longest match of the whole program starting at or after
    /// `from`, as its start and end. With `first`, any match will do, and the
    /// first found is returned.
    pub(super) fn search(&mut self, from: usize, first: bool) -> Mem<Option<(usize, usize)>> {
        let accept = self.prog.match_pc();
        let n = self.input.bytes.len();
        let mut best: Option<(usize, usize)> = None;
        self.cur.clear();
        let mut pos = from;
        loop {
            // Threads are kept in order of their start, and a new one starting
            // here comes last, so the first to reach an instruction has the
            // leftmost start.
            if best.is_none() {
                self.close_forward(true, 0, pos, pos, 0, accept)?;
            }
            if self.cur.contains(accept) {
                let start = self.cur.start_of(accept);
                let better = match best {
                    None => true,
                    Some((s, e)) => start < s || (start == s && pos > e),
                };
                if better {
                    best = Some((start, pos));
                }
                if first {
                    return Ok(best);
                }
            }
            if pos >= n {
                break;
            }
            self.next.clear();
            let mut i = 0;
            while let Some(q) = self.cur.nth(i) {
                i += 1;
                let start = self.cur.start_of(q);
                if best.is_some_and(|(s, _)| start > s) || q == accept || !self.consumes(q, pos) {
                    continue;
                }
                let target = if matches!(self.prog.inst(q), Inst::Backref(_)) { q } else { q + 1 };
                self.close_forward(false, target, pos + 1, start, 0, accept)?;
            }
            core::mem::swap(&mut self.cur, &mut self.next);
            pos += 1;
            if best.is_some() && self.cur.list.as_slice().is_empty() {
                break;
            }
        }
        Ok(best)
    }
}
