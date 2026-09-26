//! A manager over a unit set, driven by a script of events, with its
//! actions kept for the test to look at.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::event::{
    Action, Actions, ActiveState, ClientId, Event, Exit, Pid, Reply, Request, UnitId, Whom,
};
use crate::source::{Entry, Layer, Source};
use crate::time::Instant;
use crate::unit::Condition;
use crate::value::Signal;
use crate::{Manager, NoProbe, Options, Probe};

/// The targets every test boots through, as the image ships them (§4.3).
pub(super) const TARGETS: [(&str, &str); 7] = [
    ("sysinit.target", "[Unit]\nDefaultDependencies=no\n"),
    (
        "basic.target",
        "[Unit]\nRequires=sysinit.target\nAfter=sysinit.target\n",
    ),
    (
        "multi-user.target",
        "[Unit]\nRequires=basic.target\nAfter=basic.target\nAllowIsolate=yes\n",
    ),
    (
        "rescue.target",
        "[Unit]\nRequires=sysinit.target\nAfter=sysinit.target\nAllowIsolate=yes\n",
    ),
    ("shutdown.target", "[Unit]\nDefaultDependencies=no\n"),
    ("poweroff.target", "[Unit]\nDefaultDependencies=no\n"),
    ("reboot.target", "[Unit]\nDefaultDependencies=no\n"),
];

/// A probe whose paths are a fixed list.
#[derive(Debug)]
pub(super) struct Paths(pub Vec<&'static str>);

impl Probe for Paths {
    fn test(&mut self, condition: &Condition) -> bool {
        self.0.contains(&condition.argument.as_str())
    }
}

/// The manager, the clock, and the pids handed out.
pub(super) struct Rig {
    pub manager: Manager,
    /// The units it was made with, for [`Rig::reload`].
    source: Source,
    pub now: Instant,
    next_pid: u32,
    /// Everything the manager asked for, in order.
    pub log: Actions,
}

impl Rig {
    /// A manager over `units` and the shipped targets, `default.target`
    /// being `multi-user.target`.
    pub(super) fn new(units: &[(&str, &str)]) -> Rig {
        Rig::with(units, Box::new(NoProbe), Options::default())
    }

    /// The same, with a probe and options.
    pub(super) fn with(units: &[(&str, &str)], probe: Box<dyn Probe>, options: Options) -> Rig {
        let mut source = Source::new();
        for (path, text) in TARGETS.iter().chain(units.iter()) {
            let entry = if let Some(target) = text.strip_prefix("->") {
                Entry::Alias(String::from(target))
            } else {
                Entry::File(text.as_bytes().to_vec())
            };
            assert!(source.add(Layer::Image, path, entry).is_ok(), "{path}");
        }
        if !units.iter().any(|(path, _)| *path == "default.target") {
            let entry = Entry::Alias(String::from("multi-user.target"));
            assert!(source.add(Layer::Admin, "default.target", entry).is_ok());
        }
        Rig {
            manager: Manager::new(source.clone(), probe, options),
            source,
            now: Instant::ZERO,
            next_pid: 100,
            log: Vec::new(),
        }
    }

    /// `svc daemon-reload`, over the same units.
    pub(super) fn reload(&mut self) {
        self.manager.reload(self.source.clone());
    }

    /// Feed one event now.
    pub(super) fn step(&mut self, event: Event) -> Actions {
        let actions = self.manager.step(event, self.now);
        self.log.extend(actions.iter().cloned());
        actions
    }

    /// Move the clock to `millis` and fire the timer if it is due.
    pub(super) fn at(&mut self, millis: u64) -> Actions {
        self.now = Instant::from_millis(millis);
        match self.manager.deadline() {
            Some(deadline) if deadline <= self.now => self.step(Event::Timer),
            _ => Vec::new(),
        }
    }

    /// A unit's number.
    pub(super) fn id(&self, name: &str) -> UnitId {
        self.manager
            .unit(name)
            .unwrap_or_else(|| panic!("no unit {name}"))
    }

    /// A unit's name.
    pub(super) fn name(&self, unit: UnitId) -> String {
        String::from(self.manager.name(unit).map_or("?", |n| n.as_str()))
    }

    /// A unit's active state.
    pub(super) fn state(&self, name: &str) -> ActiveState {
        self.manager
            .status(self.id(name))
            .map_or(ActiveState::Inactive, |s| s.active)
    }

    /// A unit's sub-state.
    pub(super) fn sub(&self, name: &str) -> &'static str {
        self.manager.status(self.id(name)).map_or("?", |s| s.sub)
    }

    /// Answer a spawn of `name` with a new pid.
    pub(super) fn spawned(&mut self, name: &str) -> (Pid, Actions) {
        let pid = Pid(self.next_pid);
        self.next_pid += 1;
        let unit = self.id(name);
        (pid, self.step(Event::Spawned { unit, pid }))
    }

    /// A process exits.
    pub(super) fn exit(&mut self, pid: Pid, code: i32) -> Actions {
        self.step(Event::Exited {
            pid,
            how: Exit::Code(code),
        })
    }

    /// A process is killed by a signal.
    pub(super) fn killed(&mut self, pid: Pid, signal: Signal) -> Actions {
        self.step(Event::Exited {
            pid,
            how: Exit::Signal {
                signal,
                core: false,
            },
        })
    }

    /// A unit's cgroup empties.
    pub(super) fn emptied(&mut self, name: &str) -> Actions {
        let unit = self.id(name);
        self.step(Event::Emptied { unit })
    }

    /// A client's request.
    pub(super) fn request(&mut self, request: Request) -> Actions {
        self.step(Event::Request {
            client: ClientId(1),
            request,
        })
    }

    /// Boot.
    pub(super) fn boot(&mut self) -> Actions {
        self.step(Event::Boot)
    }

    /// The units `actions` spawn, by name, in order.
    pub(super) fn spawns(&self, actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Spawn { unit, .. } => Some(self.name(*unit)),
                _ => None,
            })
            .collect()
    }

    /// The argv of each spawn in `actions`.
    pub(super) fn argvs(actions: &[Action]) -> Vec<Vec<String>> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Spawn { spec, .. } => Some(spec.command.argv.clone()),
                _ => None,
            })
            .collect()
    }

    /// The cgroups `actions` make, by path, in order.
    pub(super) fn made(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::MakeGroup { path, .. } => Some(String::from(path.as_str())),
                _ => None,
            })
            .collect()
    }

    /// What `actions` do to processes, by unit name, as short words:
    /// `TERM>group`, `TERM>123`, `kill`, `remove`.
    pub(super) fn kills(&self, actions: &[Action]) -> Vec<(String, String)> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Signal { unit, signal, whom } => {
                    let name = signal.name().unwrap_or("?");
                    let whom = match whom {
                        Whom::Group => String::from("group"),
                        Whom::Process(pid) => alloc::format!("{}", pid.0),
                    };
                    Some((self.name(*unit), alloc::format!("{name}>{whom}")))
                }
                Action::KillGroup { unit } => Some((self.name(*unit), String::from("kill"))),
                Action::RemoveGroup { unit } => Some((self.name(*unit), String::from("remove"))),
                _ => None,
            })
            .collect()
    }

    /// The replies in `actions`.
    pub(super) fn replies(actions: &[Action]) -> Vec<Reply> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Reply { reply, .. } => Some(reply.clone()),
                _ => None,
            })
            .collect()
    }

    /// The log lines in `actions`.
    pub(super) fn lines(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Log { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect()
    }
}
