//! Transactions (§4.3): a request expanded into every operation it pulls
//! in, checked, and merged into the queue.
//!
//! A start pulls in starts of what the unit `Requires=`, `BindsTo=` and
//! `Wants=`, a check of what it has as `Requisite=`, and stops of what it
//! conflicts with, either way round. A stop pulls in stops of what
//! requires, is bound to, or is part of the unit. An operation is
//! *essential* if a chain of `Requires=`, `BindsTo=`, `Requisite=` or
//! `Conflicts=` leads to it from the one asked for; the rest came through
//! `Wants=` and may be dropped.
//!
//! Then, as systemd does: a unit that would both start and stop keeps the
//! stop if its start is not essential, and refuses the transaction if it
//! is. An ordering cycle is broken by dropping a non-essential operation
//! on it, with a warning, and a cycle of essential operations refuses the
//! transaction. Last, each operation meets the one queued for its unit, if
//! any: the same or a compatible one is kept, and an opposite one is
//! replaced, or refuses the transaction in [`Mode::Fail`].

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Manager, Op, OpId, OpKind};
use crate::event::{ActiveState, OpResult, UnitId};
use crate::unit::Dependency;

/// How a transaction meets the operations already queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Replace a queued operation it conflicts with: systemd's `replace`.
    Replace,
    /// Refuse to, and so refuse the transaction: systemd's `fail`.
    Fail,
    /// As [`Mode::Replace`], and stop every unit the transaction does not
    /// start: `svc isolate`, and rescue after a failed boot.
    Isolate,
    /// As [`Mode::Replace`], and nothing but another irreversible
    /// transaction replaces it: a shutdown.
    Irreversible,
}

/// One unit's part in a transaction being built.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Want {
    /// The units that pulled it in, and through what.
    pulls: Vec<(UnitId, Dependency)>,
    /// Whether it is the operation asked for.
    anchor: bool,
    /// Why it cannot happen, if a unit it requires does not load.
    broken: Option<String>,
}

/// A transaction being built: its starts (each a start, restart or check)
/// and its stops.
#[derive(Debug, Clone, Default)]
struct Tx {
    starts: BTreeMap<UnitId, (OpKind, Want)>,
    stops: BTreeMap<UnitId, Want>,
}

impl Tx {
    fn kind(&self, unit: UnitId) -> Option<OpKind> {
        if self.stops.contains_key(&unit) {
            return Some(OpKind::Stop);
        }
        self.starts.get(&unit).map(|(kind, _)| *kind)
    }

    fn units(&self) -> BTreeSet<UnitId> {
        self.starts
            .keys()
            .chain(self.stops.keys())
            .copied()
            .collect()
    }
}

/// Operations still to add while expanding: the unit, what to do, and who
/// pulled it through what.
type Work = Vec<(UnitId, OpKind, Option<(UnitId, Dependency)>)>;

/// Whether a pull through `dependency` makes the pulled operation as
/// essential as the one pulling.
fn binding(dependency: Dependency) -> bool {
    !matches!(dependency, Dependency::Wants)
}

impl Manager {
    /// Build a transaction for `kind` on `anchor`, check it and merge it
    /// into the queue. Returns the anchor's operation, or `None` when there
    /// is nothing to do, the unit being as asked already.
    pub(super) fn transaction(
        &mut self,
        anchor: UnitId,
        kind: OpKind,
        mode: Mode,
    ) -> Result<Option<OpId>, String> {
        let name = self
            .slot(anchor)
            .map(|s| String::from(s.name.as_str()))
            .unwrap_or_default();
        if kind != OpKind::Stop
            && let Some(Err(error)) = self.slot(anchor).map(|s| &s.loaded)
        {
            return Err(format!("{name}: {error}"));
        }
        let mut tx = Tx::default();
        self.expand(&mut tx, anchor, kind);
        if mode == Mode::Isolate {
            self.isolate(&mut tx);
        }
        self.settle_tx(&mut tx, anchor)?;
        self.merge(&tx, anchor, mode)
    }

    /// Pull in everything `kind` on `anchor` needs.
    fn expand(&self, tx: &mut Tx, anchor: UnitId, kind: OpKind) {
        let mut work: Work = Vec::new();
        work.push((anchor, kind, None));
        while let Some((unit, kind, pull)) = work.pop() {
            let fresh = add(tx, unit, kind, pull, pull.is_none());
            if !fresh {
                continue;
            }
            match kind {
                OpKind::Start | OpKind::Restart => self.pull_starts(tx, unit, kind, &mut work),
                OpKind::Stop => self.pull_stops(unit, &mut work),
                OpKind::Verify => {}
            }
        }
    }

    /// The pulls of a stop of `unit`: what requires it, is bound to it, or
    /// is part of it stops too.
    fn pull_stops(&self, unit: UnitId, work: &mut Work) {
        let dependents = self.dependents(
            unit,
            &[
                Dependency::Requires,
                Dependency::BindsTo,
                Dependency::PartOf,
            ],
        );
        for other in dependents.into_iter().filter(|&other| self.is_up(other)) {
            work.push((other, OpKind::Stop, Some((unit, Dependency::Requires))));
        }
    }

    /// The pulls of a start or a restart of `unit`.
    fn pull_starts(&self, tx: &mut Tx, unit: UnitId, kind: OpKind, work: &mut Work) {
        for dependency in [Dependency::Requires, Dependency::BindsTo, Dependency::Wants] {
            for other in self.targets(unit, dependency) {
                match self.slot(other).map(|slot| &slot.loaded) {
                    Some(Ok(_)) => work.push((other, OpKind::Start, Some((unit, dependency)))),
                    Some(Err(error)) if dependency != Dependency::Wants => {
                        let name = self
                            .slot(other)
                            .map(|s| String::from(s.name.as_str()))
                            .unwrap_or_default();
                        mark_broken(tx, unit, format!("{name}: {error}"));
                    }
                    _ => {}
                }
            }
        }
        for other in self.targets(unit, Dependency::Requisite) {
            work.push((other, OpKind::Verify, Some((unit, Dependency::Requisite))));
        }
        let mut conflicting = self.targets(unit, Dependency::Conflicts);
        conflicting.extend(self.dependents(unit, &[Dependency::Conflicts]));
        for other in conflicting {
            if self.is_up(other) || tx.starts.contains_key(&other) {
                work.push((other, OpKind::Stop, Some((unit, Dependency::Conflicts))));
            }
        }
        if kind == OpKind::Restart {
            for other in self.dependents(unit, &[Dependency::PartOf]) {
                if self.is_up(other) {
                    work.push((other, OpKind::Restart, Some((unit, Dependency::PartOf))));
                }
            }
        }
    }

    /// Whether a unit is up, or on its way up or down, or has an operation.
    fn is_up(&self, unit: UnitId) -> bool {
        self.slot(unit).is_some_and(|slot| {
            !slot.perpetual
                && (slot.op.is_some()
                    || !matches!(slot.active, ActiveState::Inactive | ActiveState::Failed))
        })
    }

    /// Stop everything up that the transaction does not start.
    fn isolate(&self, tx: &mut Tx) {
        let keep: BTreeSet<UnitId> = tx.starts.keys().copied().collect();
        for index in 0..self.units.len() {
            let Ok(index) = u32::try_from(index) else {
                continue;
            };
            let unit = UnitId(index);
            let ignored = self
                .slot(unit)
                .and_then(|slot| slot.loaded.as_ref().ok())
                .is_some_and(|u| u.unit.ignore_on_isolate);
            if !keep.contains(&unit) && !ignored && self.is_up(unit) {
                let _ = add(tx, unit, OpKind::Stop, None, false);
            }
        }
    }

    /// Resolve conflicts and broken operations, collect what nothing pulls
    /// any more, and break ordering cycles, until the transaction stands or
    /// is refused.
    fn settle_tx(&mut self, tx: &mut Tx, anchor: UnitId) -> Result<(), String> {
        loop {
            let essential = essential_set(tx, anchor);
            if let Err(unit) = resolve_conflicts(tx, &essential) {
                let name = self.display(unit);
                return Err(format!("{name} would be both started and stopped"));
            }
            let broken: Vec<(UnitId, String)> = tx
                .starts
                .iter()
                .filter_map(|(unit, (_, want))| want.broken.clone().map(|why| (*unit, why)))
                .collect();
            for (unit, why) in broken {
                if essential.contains(&(unit, true)) {
                    return Err(why);
                }
                let _ = tx.starts.remove(&unit);
            }
            collect(tx);
            let Some(cycle) = self.cycle(tx) else {
                return Ok(());
            };
            let essential = essential_set(tx, anchor);
            let victim = cycle.iter().copied().find(|unit| {
                let start = tx.starts.contains_key(unit) && !tx.stops.contains_key(unit);
                !essential.contains(&(*unit, start))
            });
            let names: Vec<String> = cycle.iter().map(|&unit| self.display(unit)).collect();
            let Some(victim) = victim else {
                return Err(format!("ordering cycle: {}", names.join(" -> ")));
            };
            let line = format!(
                "found ordering cycle {}; dropped the operation on {} to break it",
                names.join(" -> "),
                self.display(victim)
            );
            self.log(Some(victim), line);
            let _ = tx.starts.remove(&victim);
            let _ = tx.stops.remove(&victim);
        }
    }

    /// A unit's name, for messages.
    pub(super) fn display(&self, unit: UnitId) -> String {
        self.slot(unit)
            .map(|slot| String::from(slot.name.as_str()))
            .unwrap_or_default()
    }

    /// An ordering cycle among the transaction's operations, if there is
    /// one: the units on it, in order.
    fn cycle(&self, tx: &Tx) -> Option<Vec<UnitId>> {
        let units: Vec<UnitId> = tx.units().into_iter().collect();
        let waits = |a: UnitId, b: UnitId| match (tx.kind(a), tx.kind(b)) {
            (Some(ka), Some(kb)) => a != b && self.waits(a, ka, b, kb),
            _ => false,
        };
        // Depth-first, colouring: 0 unseen, 1 on the path, 2 done.
        let mut colour: BTreeMap<UnitId, u8> = BTreeMap::new();
        for &start in &units {
            if colour.get(&start).copied().unwrap_or(0) != 0 {
                continue;
            }
            let mut path: Vec<(UnitId, usize)> = Vec::new();
            path.push((start, 0));
            let _ = colour.insert(start, 1);
            while let Some(&mut (node, ref mut next)) = path.last_mut() {
                let Some(&other) = units.get(*next) else {
                    let _ = colour.insert(node, 2);
                    let _ = path.pop();
                    continue;
                };
                *next += 1;
                if !waits(node, other) {
                    continue;
                }
                match colour.get(&other).copied().unwrap_or(0) {
                    0 => {
                        let _ = colour.insert(other, 1);
                        path.push((other, 0));
                    }
                    1 => {
                        let from = path
                            .iter()
                            .position(|&(unit, _)| unit == other)
                            .unwrap_or(0);
                        let mut cycle: Vec<UnitId> = path
                            .get(from..)
                            .unwrap_or_default()
                            .iter()
                            .map(|&(u, _)| u)
                            .collect();
                        cycle.push(other);
                        return Some(cycle);
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// Whether an operation `ka` on `a` waits for one `kb` on `b`, by the
    /// units' order: starts in `After=` order, stops in the reverse, and a
    /// stop before a start whichever way the two are ordered.
    pub(super) fn waits(&self, a: UnitId, ka: OpKind, b: UnitId, kb: OpKind) -> bool {
        let a_after_b = self.after(a, b);
        let b_after_a = self.after(b, a);
        match (ka.starts(), kb.starts()) {
            (true, true) => a_after_b,
            (false, false) => b_after_a,
            (true, false) => a_after_b || b_after_a,
            (false, true) => false,
        }
    }

    /// Merge a transaction into the queue.
    fn merge(&mut self, tx: &Tx, anchor: UnitId, mode: Mode) -> Result<Option<OpId>, String> {
        let mut plan: Vec<(UnitId, OpKind)> = Vec::new();
        for unit in tx.units() {
            let Some(kind) = tx.kind(unit) else {
                continue;
            };
            if let Some(existing) = self
                .slot(unit)
                .and_then(|slot| slot.op)
                .and_then(|id| self.ops.get(&id))
            {
                let opposite = (existing.kind == OpKind::Stop) != (kind == OpKind::Stop);
                if opposite && existing.irreversible && mode != Mode::Irreversible {
                    return Err(String::from("the machine is going down"));
                }
                if opposite && mode == Mode::Fail {
                    return Err(format!(
                        "{} has an operation queued that this one would replace",
                        self.display(unit)
                    ));
                }
            }
            plan.push((unit, kind));
        }
        let mut anchor_op = None;
        for (unit, kind) in plan {
            let op = self.enqueue(unit, kind, mode == Mode::Irreversible);
            if unit == anchor {
                anchor_op = op;
            }
        }
        Ok(anchor_op)
    }

    /// Queue `kind` on `unit`, meeting what is queued there. Returns the
    /// operation that now stands for it, or `None` when nothing is to do.
    pub(super) fn enqueue(
        &mut self,
        unit: UnitId,
        kind: OpKind,
        irreversible: bool,
    ) -> Option<OpId> {
        let existing = self.slot(unit).and_then(|slot| slot.op);
        if let Some(id) = existing
            && let Some(op) = self.ops.get_mut(&id)
        {
            let same = op.kind == kind
                || (op.kind == OpKind::Start && kind == OpKind::Verify)
                || (op.kind == OpKind::Restart && kind.starts());
            if same {
                op.irreversible |= irreversible;
                return Some(id);
            }
            if op.kind == OpKind::Start && kind == OpKind::Restart && !op.running {
                op.kind = OpKind::Restart;
                return Some(id);
            }
            if op.kind == OpKind::Verify && kind.starts() {
                op.kind = kind;
                return Some(id);
            }
            self.finish(id, OpResult::Canceled);
        }
        let (active, perpetual) = self
            .slot(unit)
            .map_or((ActiveState::Inactive, false), |s| (s.active, s.perpetual));
        let idle = matches!(active, ActiveState::Inactive | ActiveState::Failed);
        if kind == OpKind::Stop && (idle || perpetual) {
            return None;
        }
        let id = OpId(self.next_op);
        self.next_op += 1;
        let _ = self.ops.insert(
            id,
            Op {
                unit,
                kind,
                running: false,
                clients: Vec::new(),
                irreversible,
                counted: false,
            },
        );
        if let Some(slot) = self.slot_mut(unit) {
            slot.op = Some(id);
        }
        Some(id)
    }
}

/// Add `kind` on `unit` to the transaction, pulled by `pull`. `true` if
/// the unit had no such operation in it yet, so its own pulls follow.
fn add(
    tx: &mut Tx,
    unit: UnitId,
    kind: OpKind,
    pull: Option<(UnitId, Dependency)>,
    anchor: bool,
) -> bool {
    if kind == OpKind::Stop {
        let fresh = !tx.stops.contains_key(&unit);
        let want = tx.stops.entry(unit).or_default();
        want.pulls.extend(pull);
        want.anchor |= anchor;
        return fresh;
    }
    let fresh = !tx.starts.contains_key(&unit);
    let entry = tx.starts.entry(unit).or_insert((kind, Want::default()));
    entry.1.pulls.extend(pull);
    entry.1.anchor |= anchor;
    // A start meeting a check becomes the start; a restart wins over both.
    let upgraded = match (entry.0, kind) {
        (OpKind::Verify, OpKind::Start | OpKind::Restart) | (OpKind::Start, OpKind::Restart) => {
            entry.0 = kind;
            true
        }
        _ => false,
    };
    fresh || upgraded
}

/// Settle the units the transaction would both start and stop. A start that
/// is not essential goes. An essential start keeps the unit, and the stop
/// goes with the non-essential starts that asked for it by conflicting with
/// the unit. A unit essential both ways is the error.
fn resolve_conflicts(tx: &mut Tx, essential: &BTreeSet<(UnitId, bool)>) -> Result<(), UnitId> {
    let both: Vec<UnitId> = tx
        .starts
        .keys()
        .filter(|unit| tx.stops.contains_key(unit))
        .copied()
        .collect();
    for unit in both {
        match (
            essential.contains(&(unit, true)),
            essential.contains(&(unit, false)),
        ) {
            (true, true) => return Err(unit),
            (true, false) => {
                let pullers: Vec<UnitId> = tx
                    .stops
                    .remove(&unit)
                    .map(|want| want.pulls.iter().map(|&(from, _)| from).collect())
                    .unwrap_or_default();
                for from in pullers
                    .into_iter()
                    .filter(|&from| !essential.contains(&(from, true)))
                {
                    let _ = tx.starts.remove(&from);
                }
            }
            (false, _) => {
                let _ = tx.starts.remove(&unit);
            }
        }
    }
    Ok(())
}

/// Mark a start as unable to happen.
fn mark_broken(tx: &mut Tx, unit: UnitId, why: String) {
    if let Some((_, want)) = tx.starts.get_mut(&unit)
        && want.broken.is_none()
    {
        want.broken = Some(why);
    }
}

/// The essential operations, as (unit, whether it is the start side).
fn essential_set(tx: &Tx, anchor: UnitId) -> BTreeSet<(UnitId, bool)> {
    let mut set = BTreeSet::new();
    if tx.starts.contains_key(&anchor) {
        let _ = set.insert((anchor, true));
    }
    if tx.stops.contains_key(&anchor) {
        let _ = set.insert((anchor, false));
    }
    loop {
        let before = set.len();
        let sides = tx
            .starts
            .iter()
            .map(|(unit, (_, want))| (*unit, true, want))
            .chain(tx.stops.iter().map(|(unit, want)| (*unit, false, want)));
        let mut new = Vec::new();
        for (unit, start, want) in sides {
            let pulled = want.pulls.iter().any(|&(from, via)| {
                binding(via) && (set.contains(&(from, true)) || set.contains(&(from, false)))
            });
            if want.anchor || pulled {
                new.push((unit, start));
            }
        }
        set.extend(new);
        if set.len() == before {
            return set;
        }
    }
}

/// Drop the operations nothing in the transaction pulls any more.
fn collect(tx: &mut Tx) {
    loop {
        let present = tx.units();
        let orphan = |want: &Want| {
            !want.anchor
                && !want.pulls.is_empty()
                && !want.pulls.iter().any(|(from, _)| present.contains(from))
        };
        let starts: Vec<UnitId> = tx
            .starts
            .iter()
            .filter(|(_, (_, w))| orphan(w))
            .map(|(u, _)| *u)
            .collect();
        let stops: Vec<UnitId> = tx
            .stops
            .iter()
            .filter(|(_, w)| orphan(w))
            .map(|(u, _)| *u)
            .collect();
        if starts.is_empty() && stops.is_empty() {
            return;
        }
        for unit in starts {
            let _ = tx.starts.remove(&unit);
        }
        for unit in stops {
            let _ = tx.stops.remove(&unit);
        }
    }
}
