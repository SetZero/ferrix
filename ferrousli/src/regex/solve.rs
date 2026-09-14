//! Splitting a match among the pattern's subexpressions, POSIX's way.
//!
//! [`Machine::search`] finds where the leftmost-longest match starts and ends.
//! POSIX then asks that each subexpression, from left to right, match the
//! longest string it can while the whole still matches there, and that a
//! repeated group report its last iteration.
//!
//! The solver works top down on goals of the form "node `n` matches exactly
//! `s..e`". For a concatenation, the first child's end `m` must be a position
//! the child can reach running forwards from `s`, and a position the rest of
//! the children can start at running backwards from `e`; the highest such `m`
//! is the longest the child can be. A repetition splits off its iterations the
//! same way, each as long as it can be, and an alternation takes its first
//! alternative that matches the span. A node without groups inside it needs
//! no split at all. Each split is two runs of the automaton over the span, so
//! the whole costs a small multiple of the search, not the exponential time a
//! backtracking matcher can take.
//!
//! Without back-references the simulations are exact, so the first candidate
//! always leads to a solution and nothing is undone. With back-references
//! they over-approximate, and the solver backtracks: each split remembers its
//! other candidates, in the order POSIX prefers them, and a back-reference
//! that does not repeat its group's text sends it back to the latest choice.
//! That search can take exponential time, as it does in every POSIX matcher,
//! but only for patterns with back-references.

use super::parse::{Ast, Kind, NodeId};
use super::program::{Machine, Mem, NoMemory, Positions, Program, VARIABLE, grew};
use crate::growable::Growable;

/// A group that has not matched.
pub(super) const UNSET: usize = usize::MAX;

/// Something the solver has still to show.
#[derive(Debug, Clone, Copy)]
enum Goal {
    /// Node `id` matches exactly `s..e`.
    Node {
        /// The node.
        id: NodeId,
        /// Where its match starts.
        s: usize,
        /// Where it ends.
        e: usize,
    },
    /// The children from `i` on, of the `count` from `first`, match `s..e`.
    Concat {
        /// The child list.
        first: u32,
        /// Its length.
        count: u32,
        /// The first child still to place.
        i: u32,
        /// Where child `i` starts.
        s: usize,
        /// Where the last child ends.
        e: usize,
    },
    /// Repetition `id`, `done` iterations in, matches the rest, `s..e`.
    Repeat {
        /// The repetition node.
        id: NodeId,
        /// How many iterations are behind.
        done: u32,
        /// Where the next iteration starts.
        s: usize,
        /// Where the repetition ends.
        e: usize,
    },
    /// Group `group` matched `s..e`.
    Capture {
        /// The group's number.
        group: u32,
        /// Where it starts.
        s: usize,
        /// Where it ends.
        e: usize,
    },
}

/// What to try when the solver comes back to a choice.
#[derive(Debug, Clone, Copy)]
enum Retry {
    /// Split the concatenation or repetition `goal` at the candidates
    /// `cands[next..end]` instead, in that order.
    Split {
        /// A [`Goal::Concat`] or [`Goal::Repeat`].
        goal: Goal,
        /// The next candidate to try.
        next: usize,
        /// One past the last.
        end: usize,
    },
    /// Try the alternatives from `next` of the `count` from `first`, over
    /// `s..e`.
    Alt {
        /// The child list.
        first: u32,
        /// Its length.
        count: u32,
        /// The next alternative to try.
        next: u32,
        /// The span's start.
        s: usize,
        /// Its end.
        e: usize,
    },
    /// Repeat no times over an empty span, instead of once.
    NoIteration,
}

/// A point the solver can come back to.
#[derive(Debug, Clone, Copy)]
struct Choice {
    /// Where the goals as they stood are saved in `saved`.
    goals_at: usize,
    /// How long the trail of captures was.
    trail_at: usize,
    /// Where this choice's candidates start in `cands`.
    cands_at: usize,
    /// What to try instead.
    retry: Retry,
}

/// The positions a range can start at, running backwards from `to`, kept
/// while the iterations of one repetition are split off one by one.
#[derive(Debug)]
struct Cached {
    /// The range's first instruction.
    start: u32,
    /// One past its last.
    end: u32,
    /// The lowest position the run went down to.
    lo: usize,
    /// Where the run started.
    to: usize,
    /// What it found.
    set: Positions,
}

/// The candidates in `ends` and `starts` from `hi` down to `lo`. The highest
/// is returned; with `all`, the rest are appended to `out` in descending
/// order.
fn collect(
    ends: &Positions,
    starts: &Positions,
    lo: usize,
    hi: usize,
    all: bool,
    out: &mut Growable<usize>,
) -> Mem<Option<usize>> {
    let mut best = None;
    let mut next = ends.highest_at_most(hi);
    while let Some(m) = next {
        if m < lo {
            break;
        }
        if starts.contains(m) {
            if best.is_none() {
                best = Some(m);
                if !all {
                    break;
                }
            } else {
                grew(out.push(m))?;
            }
        }
        next = m.checked_sub(1).and_then(|p| ends.highest_at_most(p));
    }
    Ok(best)
}

/// The solver, over one [`Machine`].
#[derive(Debug)]
pub(super) struct Solver<'m, 'a> {
    /// The pattern.
    ast: &'a Ast,
    /// Its program.
    prog: &'a Program,
    /// The simulations.
    machine: &'m mut Machine<'a>,
    /// Whether candidates beyond the first are kept: the pattern has
    /// back-references.
    backtrack: bool,
    /// The goals still to show, the next on top.
    goals: Growable<Goal>,
    /// The goals as they stood at each choice.
    saved: Growable<Goal>,
    /// The choices to come back to, the latest on top.
    choices: Growable<Choice>,
    /// The candidates of split choices.
    cands: Growable<usize>,
    /// Each capture's value before it was last set, to undo on backtracking.
    trail: Growable<(u32, usize, usize)>,
    /// Each group's match, [`UNSET`] if it has none; group 0 is unused.
    captures: Growable<(usize, usize)>,
    /// The last backward run of a repetition's remaining iterations.
    cache: Option<Cached>,
}

impl<'m, 'a> Solver<'m, 'a> {
    /// A solver for `ast` over `machine`.
    pub(super) fn new(ast: &'a Ast, machine: &'m mut Machine<'a>) -> Mem<Self> {
        let prog = machine.program();
        let mut captures = Growable::new();
        grew(captures.reserve(ast.groups as usize + 1))?;
        for _ in 0..=ast.groups {
            let _ = captures.push((UNSET, UNSET));
        }
        Ok(Self {
            ast,
            prog,
            machine,
            backtrack: ast.has_backrefs,
            goals: Growable::new(),
            saved: Growable::new(),
            choices: Growable::new(),
            cands: Growable::new(),
            trail: Growable::new(),
            captures,
            cache: None,
        })
    }

    /// Group `group`'s match after a successful [`Solver::solve`].
    pub(super) fn capture(&self, group: usize) -> (usize, usize) {
        self.captures
            .as_slice()
            .get(group)
            .copied()
            .unwrap_or((UNSET, UNSET))
    }

    /// Every position the whole pattern can end at from `s`, as far as the
    /// simulation can tell.
    pub(super) fn ends_from(&mut self, s: usize) -> Mem<Positions> {
        let accept = self.prog.match_pc();
        let n = self.machine.input.bytes.len();
        self.machine.forward(0, accept, s, n)
    }

    /// Splits a match of the whole pattern over `s..e` among its groups.
    /// Returns whether there is such a match, which without back-references
    /// there always is when the search found `s..e`.
    pub(super) fn solve(&mut self, s: usize, e: usize) -> Mem<bool> {
        for slot in self.captures.as_mut_slice() {
            *slot = (UNSET, UNSET);
        }
        self.goals.truncate(0);
        self.saved.truncate(0);
        self.choices.truncate(0);
        self.cands.truncate(0);
        self.trail.truncate(0);
        self.push(Goal::Node {
            id: self.ast.root,
            s,
            e,
        })?;
        loop {
            let Some(goal) = self.goals.pop() else {
                return Ok(true);
            };
            if self.run(goal)? {
                continue;
            }
            loop {
                let Some(choice) = self.choices.pop() else {
                    return Ok(false);
                };
                self.restore(choice)?;
                if self.retry(choice)? {
                    break;
                }
            }
        }
    }

    /// Pushes a goal.
    fn push(&mut self, goal: Goal) -> Mem<()> {
        grew(self.goals.push(goal))
    }

    /// Records a choice, with the goals as they stand now.
    fn push_choice(&mut self, cands_at: usize, retry: Retry) -> Mem<()> {
        let goals_at = self.saved.len();
        grew(self.saved.reserve(self.goals.len()))?;
        for &goal in self.goals.as_slice() {
            let _ = self.saved.push(goal);
        }
        grew(self.choices.push(Choice {
            goals_at,
            trail_at: self.trail.len(),
            cands_at,
            retry,
        }))
    }

    /// Puts the goals and captures back as they were at `choice`.
    fn restore(&mut self, choice: Choice) -> Mem<()> {
        while self.trail.len() > choice.trail_at {
            let Some((group, so, eo)) = self.trail.pop() else {
                break;
            };
            if let Some(slot) = self.captures.as_mut_slice().get_mut(group as usize) {
                *slot = (so, eo);
            }
        }
        self.goals.truncate(0);
        let saved = self.saved.as_slice().get(choice.goals_at..).unwrap_or(&[]);
        grew(self.goals.reserve(saved.len()))?;
        for &goal in saved {
            let _ = self.goals.push(goal);
        }
        Ok(())
    }

    /// Lets go of what `choice` saved, once it has nothing left to try.
    fn forget(&mut self, choice: Choice) {
        self.saved.truncate(choice.goals_at);
        self.cands.truncate(choice.cands_at);
    }

    /// Takes the next option of `choice`. Returns `false` if it has none.
    fn retry(&mut self, choice: Choice) -> Mem<bool> {
        match choice.retry {
            Retry::Split { goal, next, end } => {
                let Some(m) = self.cands.as_slice().get(next).copied() else {
                    self.forget(choice);
                    return Ok(false);
                };
                if next + 1 < end {
                    grew(self.choices.push(Choice {
                        retry: Retry::Split {
                            goal,
                            next: next + 1,
                            end,
                        },
                        ..choice
                    }))?;
                } else {
                    self.forget(choice);
                }
                self.continue_split(goal, m)?;
                Ok(true)
            }
            Retry::Alt {
                first,
                count,
                next,
                s,
                e,
            } => {
                self.forget(choice);
                self.alternative(first, count, next, s, e)
            }
            Retry::NoIteration => {
                self.forget(choice);
                Ok(true)
            }
        }
    }

    /// Works on one goal. Returns `false` if it cannot be met.
    fn run(&mut self, goal: Goal) -> Mem<bool> {
        match goal {
            Goal::Capture { group, s, e } => {
                self.set_capture(group, s, e)?;
                Ok(true)
            }
            Goal::Node { id, s, e } => self.node(id, s, e),
            Goal::Concat { .. } => self.concat(goal),
            Goal::Repeat { .. } => self.repeat(goal),
        }
    }

    /// Node `id` matches `s..e`.
    fn node(&mut self, id: NodeId, s: usize, e: usize) -> Mem<bool> {
        if !self.prog.captures(id) {
            // The span came from a simulation of this node, which is exact
            // for a node without groups or back-references.
            return Ok(true);
        }
        match self.ast.kind(id) {
            Kind::Backref(group) => Ok(self.repeats_group(group, s, e)),
            Kind::Group(group, inner) => {
                self.push(Goal::Capture { group, s, e })?;
                self.push(Goal::Node { id: inner, s, e })?;
                Ok(true)
            }
            Kind::Concat(first, count) => {
                self.push(Goal::Concat {
                    first,
                    count,
                    i: 0,
                    s,
                    e,
                })?;
                Ok(true)
            }
            Kind::Alt(first, count) => self.alternative(first, count, 0, s, e),
            Kind::Repeat(..) => {
                self.push(Goal::Repeat { id, done: 0, s, e })?;
                Ok(true)
            }
            Kind::Empty | Kind::Byte(_) | Kind::Set(_) | Kind::Assert(_) => Ok(true),
        }
    }

    /// The first alternative, from `from`, that matches `s..e`.
    fn alternative(&mut self, first: u32, count: u32, from: u32, s: usize, e: usize) -> Mem<bool> {
        let mut i = from;
        while i < count {
            let child = self.ast.child(first, i);
            i += 1;
            if !self.can_match(child, s, e)? {
                continue;
            }
            if self.backtrack && i < count {
                self.push_choice(
                    self.cands.len(),
                    Retry::Alt {
                        first,
                        count,
                        next: i,
                        s,
                        e,
                    },
                )?;
            }
            self.push(Goal::Node { id: child, s, e })?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Whether node `id` can match `s..e`, as far as the simulation can tell.
    fn can_match(&mut self, id: NodeId, s: usize, e: usize) -> Mem<bool> {
        let width = self.prog.width(id);
        if width != VARIABLE && Some(width as usize) != e.checked_sub(s) {
            return Ok(false);
        }
        let (start, end) = self.prog.range(id);
        Ok(self.machine.forward(start, end, s, e)?.contains(e))
    }

    /// Places the next child of a concatenation.
    fn concat(&mut self, goal: Goal) -> Mem<bool> {
        let Goal::Concat {
            first,
            count,
            i,
            s,
            e,
        } = goal
        else {
            return Ok(false);
        };
        let child = self.ast.child(first, i);
        if i + 1 >= count {
            self.push(Goal::Node { id: child, s, e })?;
            return Ok(true);
        }
        if !self.backtrack {
            // Without back-references, a fixed width on either side forces
            // the split.
            let width = self.prog.width(child);
            let rest = self.prog.suffix_width(first + i + 1);
            let forced = if width != VARIABLE {
                s.checked_add(width as usize)
            } else if rest != VARIABLE {
                e.checked_sub(rest as usize)
            } else {
                None
            };
            if let Some(m) = forced {
                if m < s || m > e {
                    return Ok(false);
                }
                self.continue_split(goal, m)?;
                return Ok(true);
            }
        }
        let (start, end) = self.prog.range(child);
        let ends = self.machine.forward(start, end, s, e)?;
        let next = self.ast.child(first, i + 1);
        let last = self.ast.child(first, count - 1);
        let rest_start = self.prog.range(next).0;
        let rest_end = self.prog.range(last).1;
        let starts = self.machine.backward(rest_start, rest_end, s, e)?;
        self.split(goal, &ends, &starts, s, e)
    }

    /// Splits `goal` at its best candidate in `lo..=hi`, remembering the
    /// others when backtracking.
    fn split(
        &mut self,
        goal: Goal,
        ends: &Positions,
        starts: &Positions,
        lo: usize,
        hi: usize,
    ) -> Mem<bool> {
        let cands_at = self.cands.len();
        let Some(m) = collect(ends, starts, lo, hi, self.backtrack, &mut self.cands)? else {
            return Ok(false);
        };
        let end = self.cands.len();
        if end > cands_at {
            self.push_choice(
                cands_at,
                Retry::Split {
                    goal,
                    next: cands_at,
                    end,
                },
            )?;
        }
        self.continue_split(goal, m)?;
        Ok(true)
    }

    /// Pushes the goals that splitting `goal` at `m` leaves.
    fn continue_split(&mut self, goal: Goal, m: usize) -> Mem<()> {
        match goal {
            Goal::Concat {
                first,
                count,
                i,
                s,
                e,
            } => {
                self.push(Goal::Concat {
                    first,
                    count,
                    i: i + 1,
                    s: m,
                    e,
                })?;
                self.push(Goal::Node {
                    id: self.ast.child(first, i),
                    s,
                    e: m,
                })
            }
            Goal::Repeat { id, done, s, e } => {
                let Kind::Repeat(inner, _, _) = self.ast.kind(id) else {
                    return Ok(());
                };
                self.push(Goal::Repeat {
                    id,
                    done: done.saturating_add(1),
                    s: m,
                    e,
                })?;
                self.push(Goal::Node { id: inner, s, e: m })
            }
            Goal::Node { .. } | Goal::Capture { .. } => Ok(()),
        }
    }

    /// Splits off a repetition's next iteration, as long as it can be.
    fn repeat(&mut self, goal: Goal) -> Mem<bool> {
        let Goal::Repeat { id, done, s, e } = goal else {
            return Ok(false);
        };
        let Kind::Repeat(inner, min, max) = self.ast.kind(id) else {
            return Ok(false);
        };
        if s == e {
            if done < min {
                // The iterations POSIX requires, all empty.
                self.push(Goal::Repeat {
                    id,
                    done: done + 1,
                    s,
                    e,
                })?;
                self.push(Goal::Node { id: inner, s, e })?;
            } else if done == 0 && self.can_match(inner, s, e)? {
                // An optional repetition over an empty span iterates once if
                // it can, so `(a*)*` against "b" reports its group at 0,0.
                if self.backtrack {
                    self.push_choice(self.cands.len(), Retry::NoIteration)?;
                }
                self.push(Goal::Node { id: inner, s, e })?;
            }
            return Ok(true);
        }
        if done >= max {
            return Ok(false);
        }
        // An iteration beyond the minimum must make progress.
        let lo = if done < min { s } else { s + 1 };
        if !self.backtrack {
            let width = self.prog.width(inner);
            if width != VARIABLE && width > 0 {
                let m = s.saturating_add(width as usize);
                if m > e {
                    return Ok(false);
                }
                self.continue_split(goal, m)?;
                return Ok(true);
            }
        }
        let (start, end) = self.prog.range(inner);
        let ends = self.machine.forward(start, end, s, e)?;
        let rest_start = self.prog.remaining(id, done.saturating_add(1), max);
        let rest_end = self.prog.range(id).1;
        let cached = match self.cache.take() {
            Some(c) if c.start == rest_start && c.end == rest_end && c.to == e && c.lo <= s => c,
            _ => Cached {
                start: rest_start,
                end: rest_end,
                lo: s,
                to: e,
                set: self.machine.backward(rest_start, rest_end, s, e)?,
            },
        };
        let result = self.split(goal, &ends, &cached.set, lo, e);
        self.cache = Some(cached);
        result
    }

    /// Sets group `group`'s match, remembering the old one when backtracking.
    fn set_capture(&mut self, group: u32, s: usize, e: usize) -> Mem<()> {
        let Some(slot) = self.captures.as_mut_slice().get_mut(group as usize) else {
            return Ok(());
        };
        let old = *slot;
        *slot = (s, e);
        if self.backtrack {
            grew(self.trail.push((group, old.0, old.1)))?;
        }
        Ok(())
    }

    /// Whether `s..e` repeats group `group`'s match, a letter's case aside
    /// with `REG_ICASE`.
    fn repeats_group(&self, group: u32, s: usize, e: usize) -> bool {
        let (so, eo) = self.capture(group as usize);
        let bytes = self.machine.input.bytes;
        let (Some(text), Some(here)) = (bytes.get(so..eo), bytes.get(s..e)) else {
            return false;
        };
        if self.machine.input.icase {
            text.eq_ignore_ascii_case(here)
        } else {
            text == here
        }
    }
}
