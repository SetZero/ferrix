//! The manager (§3): [`Manager::step`] takes what happened and returns what
//! to do, and [`Manager::deadline`] says when it next wants a
//! [`Event::Timer`].
//!
//! It keeps a slot per unit it has heard of, numbered by [`UnitId`], and a
//! queue of *operations*: a pending start or stop of one unit each, which
//! systemd calls a job (§4.3 says why the word is not used here). A request
//! becomes a transaction ([`transaction`]): every operation it pulls in,
//! checked for conflicts and ordering cycles, then merged into the queue.
//! An operation runs as soon as no operation it is ordered after is still
//! queued, so what is not ordered runs at once ([`ops`]). Each kind drives
//! its units toward what the operation asks ([`service`] for services,
//! [`kinds`] for the rest), and boot, shutdown and the directory are
//! [`lifecycle`]'s.

mod graph;
mod kinds;
mod lifecycle;
mod ops;
mod service;
mod transaction;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::UnitName;
use crate::event::{
    Action, Actions, ActiveState, ClientId, Event, GroupPath, Name, Pid, PowerAction, Reply,
    Request, Role, Status, Token, UnitId,
};
use crate::restart::{Ended, Policy};
use crate::source::{LoadError, Source, Unit};
use crate::time::Instant;
use crate::unit::{Condition, Dependency};

pub use transaction::Mode;

/// Runs the tests of `Condition…=` and `Assert…=`, which look at the
/// machine and so are the backend's (a path's existence, the kernel command
/// line). It answers whether the test passed, before any `!`.
pub trait Probe {
    /// Run one test.
    fn test(&mut self, condition: &Condition) -> bool;
}

/// A probe for a machine with nothing on it: every test fails, so a plain
/// condition skips its unit and a negated one holds.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProbe;

impl Probe for NoProbe {
    fn test(&mut self, _: &Condition) -> bool {
        false
    }
}

/// How the manager was started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// `ferrix.target=` on the kernel command line: the target to boot
    /// instead of `default.target`.
    pub target: Option<String>,
}

/// An operation's number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct OpId(u64);

/// What an operation asks of its unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum OpKind {
    /// Start it.
    Start,
    /// Check that it is active, for `Requisite=`; start nothing.
    Verify,
    /// Stop it.
    Stop,
    /// Stop it, then start it: a [`OpKind::Stop`] until it is down, then a
    /// [`OpKind::Start`].
    Restart,
}

impl OpKind {
    /// Whether it is ordered as a start: after what the unit is `After=`.
    fn starts(self) -> bool {
        matches!(self, OpKind::Start | OpKind::Verify)
    }
}

/// A queued operation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Op {
    unit: UnitId,
    kind: OpKind,
    /// Whether it has begun.
    running: bool,
    /// The clients to answer when it ends.
    clients: Vec<ClientId>,
    /// A shutdown's: nothing but another shutdown replaces it.
    irreversible: bool,
    /// An automatic restart, already counted against the start limit.
    counted: bool,
}

/// A unit's own state, beneath its [`ActiveState`]: systemd's sub-states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Sub {
    /// Not running.
    Dead,
    /// Failed.
    Failed,
    /// Up: a target, a slice, a scope, a builtin.
    Active,
    /// Killing what a previous run left in the cgroup before a start.
    Cleaning,
    /// Running `ExecStartPre=`.
    StartPre,
    /// Running `ExecStart=`, waiting for readiness.
    Start,
    /// Running `ExecStartPost=`.
    StartPost,
    /// Up, with a main process or a populated cgroup.
    Running,
    /// Up with nothing running: `RemainAfterExit=`.
    Exited,
    /// Running `ExecStop=`.
    Stop,
    /// Sent the polite signal, waiting.
    StopSigterm,
    /// Sent `SIGKILL` or wrote `cgroup.kill`, waiting.
    StopSigkill,
    /// Running `ExecStopPost=`.
    StopPost,
    /// Waiting to be restarted.
    AutoRestart,
    /// Mounting.
    Mounting,
    /// Unmounting.
    Unmounting,
}

impl Sub {
    /// systemd's name for it.
    fn name(self) -> &'static str {
        match self {
            Sub::Dead => "dead",
            Sub::Failed => "failed",
            Sub::Active => "active",
            Sub::Cleaning => "cleaning",
            Sub::StartPre => "start-pre",
            Sub::Start => "start",
            Sub::StartPost => "start-post",
            Sub::Running => "running",
            Sub::Exited => "exited",
            Sub::Stop => "stop",
            Sub::StopSigterm => "stop-sigterm",
            Sub::StopSigkill => "stop-sigkill",
            Sub::StopPost => "stop-post",
            Sub::AutoRestart => "auto-restart",
            Sub::Mounting => "mounting",
            Sub::Unmounting => "unmounting",
        }
    }
}

/// Everything the manager knows about one unit.
#[derive(Debug, Clone)]
struct Slot {
    name: UnitName,
    loaded: Result<Unit, LoadError>,
    /// Its dependencies, resolved to units: its own, its kind's implied
    /// ones, and its default ones.
    edges: BTreeMap<Dependency, Vec<UnitId>>,
    active: ActiveState,
    sub: Sub,
    /// Where its cgroup is, if it has one.
    group: Option<GroupPath>,
    /// Whether the cgroup exists.
    made: bool,
    /// Whether the cgroup has processes, as far as the manager knows.
    populated: bool,
    /// Its queued operation.
    op: Option<OpId>,
    /// `Restart=` and the start limit, for units that start processes.
    policy: Option<Policy>,
    /// How it last ended.
    result: Option<Ended>,
    /// A `notify` service's `STATUS=`.
    status: Option<String>,
    /// Always active, never stopped: `-.slice`, `init.scope`, builtins.
    perpetual: bool,
    /// A cgroup init adopts but never makes, removes or kills:
    /// `drivers.slice` (§7.3).
    adopted: bool,
    /// When its timer runs out.
    deadline: Option<Instant>,
    main: Option<Pid>,
    control: Option<Pid>,
    /// A spawn asked for and not yet answered.
    spawning: Option<Role>,
    /// Which command of the current list runs.
    step: usize,
    /// Whether the service got as far as running, so `ExecStop=` applies.
    started: bool,
    /// Whether a stop was asked for, so it does not restart.
    stopping: bool,
    /// The processes a scope is made with.
    scope_pids: Vec<Pid>,
}

/// An OPEN waiting for its provider to start.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Open {
    provider: UnitId,
    from: UnitId,
    name: Name,
    end: Token,
}

/// Where a shutdown has got to (§8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shutdown {
    action: PowerAction,
    /// The power target's operation, while units stop.
    anchor: Option<OpId>,
    /// The cgroups written `cgroup.kill` that are still populated, once
    /// every unit that stops has.
    killing: Option<Vec<UnitId>>,
    /// When to stop waiting for them.
    deadline: Option<Instant>,
    /// Whether the machine has been told to go down.
    done: bool,
}

/// The service manager.
pub struct Manager {
    source: Source,
    probe: Box<dyn Probe>,
    options: Options,
    units: Vec<Slot>,
    /// Every name a unit answers to, aliases included.
    names: BTreeMap<String, UnitId>,
    /// Units whose edges are still to resolve.
    unresolved: Vec<UnitId>,
    ops: BTreeMap<OpId, Op>,
    next_op: u64,
    /// The processes the manager started, and whose they are.
    pids: BTreeMap<Pid, (UnitId, Role)>,
    opens: Vec<Open>,
    booted: bool,
    /// The boot target's operation, until it ends.
    boot: Option<OpId>,
    shutdown: Option<Shutdown>,
    now: Instant,
    out: Actions,
}

impl fmt::Debug for Manager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Manager")
            .field("units", &self.units.len())
            .field("ops", &self.ops.len())
            .field("booted", &self.booted)
            .field("shutdown", &self.shutdown)
            .finish_non_exhaustive()
    }
}

impl Manager {
    /// A manager over the units in `source`, testing conditions with
    /// `probe`. Nothing starts until [`Event::Boot`].
    pub fn new(source: Source, probe: Box<dyn Probe>, options: Options) -> Self {
        Self {
            source,
            probe,
            options,
            units: Vec::new(),
            names: BTreeMap::new(),
            unresolved: Vec::new(),
            ops: BTreeMap::new(),
            next_op: 0,
            pids: BTreeMap::new(),
            opens: Vec::new(),
            booted: false,
            boot: None,
            shutdown: None,
            now: Instant::ZERO,
            out: Vec::new(),
        }
    }

    /// Everything that happened, in; everything to do about it, out.
    /// `now` comes from the Clock backend, so a test replays time too.
    pub fn step(&mut self, event: Event, now: Instant) -> Actions {
        self.now = now;
        match event {
            Event::Boot => self.boot(),
            Event::Spawned { unit, pid } => self.spawned(unit, pid),
            Event::Execed { unit, pid } => self.execed(unit, pid),
            Event::SpawnFailed { unit, error } => self.spawn_failed(unit, error),
            Event::Exited { pid, how } => self.exited(pid, how),
            Event::Emptied { unit } => self.emptied(unit),
            Event::OomKilled { unit } => self.oom_killed(unit),
            Event::Ready { unit, status } => self.ready(unit, status),
            Event::MainPid { unit, pid } => self.main_pid(unit, pid),
            Event::Timer => self.timer(),
            Event::Request { client, request } => self.request(client, request),
            Event::Open { from, name, end } => self.open(from, name, end),
            Event::Mounted { unit, result } => self.mounted(unit, result),
            Event::Unmounted { unit, result } => self.unmounted(unit, result),
        }
        self.dispatch();
        core::mem::take(&mut self.out)
    }

    /// When `step` next wants a [`Event::Timer`], if ever.
    pub fn deadline(&self) -> Option<Instant> {
        let units = self.units.iter().filter_map(|slot| slot.deadline);
        let shutdown = self.shutdown.as_ref().and_then(|s| s.deadline);
        units.chain(shutdown).min()
    }

    /// Replace the unit files: `svc daemon-reload`, after the backend has
    /// read the directories again. Every known unit is loaded again and its
    /// dependencies resolved again; what runs is left running.
    pub fn reload(&mut self, source: Source) {
        self.source = source;
        let ids: Vec<UnitId> = (0..self.units.len())
            .filter_map(|index| u32::try_from(index).ok().map(UnitId))
            .collect();
        for id in ids {
            let Some(name) = self.slot(id).map(|slot| slot.name.clone()) else {
                continue;
            };
            let loaded = self.source.load(name.as_str());
            if let Some(slot) = self.slot_mut(id) {
                slot.loaded = loaded;
            }
            self.unresolved.push(id);
        }
        self.resolve();
    }

    /// The unit a name names, if the manager has heard of it.
    pub fn unit(&self, name: &str) -> Option<UnitId> {
        self.names.get(name).copied()
    }

    /// A unit's name.
    pub fn name(&self, unit: UnitId) -> Option<&UnitName> {
        self.slot(unit).map(|slot| &slot.name)
    }

    /// A unit's cgroup, once it has one.
    pub fn group(&self, unit: UnitId) -> Option<&GroupPath> {
        self.slot(unit).and_then(|slot| slot.group.as_ref())
    }

    /// A unit's state, as `svc status` shows it.
    pub fn status(&self, unit: UnitId) -> Option<Status> {
        let slot = self.slot(unit)?;
        let load = match &slot.loaded {
            Ok(_) => "loaded",
            Err(LoadError::NotFound) => "not-found",
            Err(LoadError::Masked) => "masked",
            Err(LoadError::Refused(_)) => "bad-setting",
            Err(_) => "error",
        };
        Some(Status {
            unit,
            name: String::from(slot.name.as_str()),
            description: slot
                .loaded
                .as_ref()
                .ok()
                .and_then(|u| u.unit.description.clone()),
            load,
            active: slot.active,
            sub: slot.sub.name(),
            main: slot.main,
            result: slot.result,
            status: slot.status.clone(),
        })
    }

    /// The loaded units' states, in number order.
    pub fn statuses(&self) -> Vec<Status> {
        (0..self.units.len())
            .filter_map(|index| u32::try_from(index).ok())
            .filter_map(|index| self.status(UnitId(index)))
            .collect()
    }

    fn slot(&self, unit: UnitId) -> Option<&Slot> {
        self.units.get(usize::try_from(unit.0).ok()?)
    }

    fn slot_mut(&mut self, unit: UnitId) -> Option<&mut Slot> {
        self.units.get_mut(usize::try_from(unit.0).ok()?)
    }

    fn emit(&mut self, action: Action) {
        self.out.push(action);
    }

    fn log(&mut self, unit: Option<UnitId>, line: String) {
        self.emit(Action::Log { unit, line });
    }

    fn reply(&mut self, client: ClientId, reply: Reply) {
        self.emit(Action::Reply { client, reply });
    }

    /// A control client's request.
    fn request(&mut self, client: ClientId, request: Request) {
        match request {
            Request::Start(name, mode) => self.request_op(client, &name, OpKind::Start, mode),
            Request::Stop(name, mode) => self.request_op(client, &name, OpKind::Stop, mode),
            Request::Restart(name, mode) => {
                self.request_op(client, &name, OpKind::Restart, mode);
            }
            Request::Isolate(name) => self.request_op(client, &name, OpKind::Start, Mode::Isolate),
            Request::ResetFailed(name) => self.reset_failed(client, name.as_deref()),
            Request::Status(name) => {
                let statuses = match name {
                    Some(name) => self
                        .ensure(&name)
                        .and_then(|unit| self.status(unit))
                        .into_iter()
                        .collect(),
                    None => self.statuses(),
                };
                self.reply(client, Reply::Status(statuses));
            }
            Request::Poweroff => self.power(Some(client), PowerAction::Poweroff),
            Request::Reboot => self.power(Some(client), PowerAction::Reboot),
            Request::Scope { unit, slice, pids } => {
                self.scope(client, &unit, slice.as_deref(), pids);
            }
        }
    }

    /// `svc reset-failed`.
    fn reset_failed(&mut self, client: ClientId, name: Option<&str>) {
        let units: Vec<UnitId> = match name {
            Some(name) => self.ensure(name).into_iter().collect(),
            None => (0..self.units.len())
                .filter_map(|index| u32::try_from(index).ok().map(UnitId))
                .collect(),
        };
        for unit in units {
            let Some(slot) = self.slot_mut(unit) else {
                continue;
            };
            if slot.active == ActiveState::Failed {
                slot.active = ActiveState::Inactive;
                slot.sub = Sub::Dead;
                slot.result = None;
            }
            if let Some(policy) = slot.policy.as_mut() {
                policy.reset();
            }
        }
        self.reply(client, Reply::Done(crate::event::OpResult::Done));
    }
}
