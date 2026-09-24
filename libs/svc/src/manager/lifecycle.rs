//! Boot (§8.1), shutdown (§8.2), scopes on request (§5.6) and the
//! directory's OPENs (§6).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

use super::graph::{DRIVERS_SLICE, INIT_SCOPE, ROOT_SLICE, SHUTDOWN};
use super::{Manager, Mode, OpKind, Open, Shutdown};
use crate::UnitName;
use crate::event::{
    Action, ActiveState, ClientId, Name, OpResult, Pid, PowerAction, Reply, Token, UnitId,
};
use crate::kind::Config;
use crate::name::UnitType;

/// How long the last step of a shutdown waits for the cgroups it killed.
const KILL_WAIT: Duration = Duration::from_secs(90);

impl Manager {
    /// Start the system: load every unit the directories have, mark the
    /// cgroups init did not make as up, and start `default.target` or the
    /// one `ferrix.target=` names.
    pub(super) fn boot(&mut self) {
        if self.booted {
            return;
        }
        self.booted = true;
        let names: Vec<UnitName> = self.source.names();
        for name in names.iter().filter(|name| !name.is_template()) {
            let _ = self.ensure(name.as_str());
        }
        for name in [ROOT_SLICE, INIT_SCOPE, DRIVERS_SLICE] {
            let _ = self.ensure(name);
        }
        let target = self
            .options
            .target
            .clone()
            .unwrap_or_else(|| String::from("default.target"));
        let line = format!("booting {target}");
        self.log(None, line);
        let Some(unit) = self.ensure(&target) else {
            let line = format!("{target} is not a unit name; starting rescue.target");
            self.log(None, line);
            self.rescue();
            return;
        };
        match self.transaction(unit, OpKind::Start, Mode::Replace) {
            Ok(op) => self.boot = op,
            Err(why) => {
                let line = format!("{why}; starting rescue.target");
                self.log(Some(unit), line);
                self.rescue();
            }
        }
    }

    /// Boot failed: isolate `rescue.target`, one shell on the console.
    pub(super) fn rescue(&mut self) {
        let Some(unit) = self.ensure("rescue.target") else {
            return;
        };
        if let Err(why) = self.transaction(unit, OpKind::Start, Mode::Isolate) {
            let line = format!("rescue.target could not start either: {why}");
            self.log(Some(unit), line);
        }
    }

    /// Bring the machine down (§8.2): start the power target, which stops
    /// every unit that conflicts with `shutdown.target`, in reverse order.
    pub(super) fn power(&mut self, client: Option<ClientId>, action: PowerAction) {
        if self.shutdown.is_some() {
            if let Some(client) = client {
                self.reply(
                    client,
                    Reply::Refused(String::from("the machine is going down already")),
                );
            }
            return;
        }
        self.shutdown = Some(Shutdown {
            action,
            anchor: None,
            killing: None,
            deadline: None,
            done: false,
        });
        let target = match action {
            PowerAction::Poweroff => "poweroff.target",
            PowerAction::Reboot => "reboot.target",
        };
        let line = format!("going down: {target}");
        self.log(None, line);
        let loaded = |manager: &mut Self, name: &str| {
            manager
                .ensure(name)
                .filter(|&unit| manager.slot(unit).is_some_and(|s| s.loaded.is_ok()))
        };
        let anchor = loaded(self, target).or_else(|| loaded(self, SHUTDOWN));
        let op = anchor.map(|unit| self.transaction(unit, OpKind::Start, Mode::Irreversible));
        match op {
            Some(Ok(Some(id))) => {
                if let Some(op) = self.ops.get_mut(&id) {
                    op.clients.extend(client);
                }
                if let Some(shutdown) = self.shutdown.as_mut() {
                    shutdown.anchor = Some(id);
                }
            }
            Some(Err(why)) => {
                self.log(None, why);
                self.finish_client(client);
                self.kill_the_rest();
            }
            _ => {
                self.finish_client(client);
                self.kill_the_rest();
            }
        }
    }

    fn finish_client(&mut self, client: Option<ClientId>) {
        if let Some(client) = client {
            self.reply(client, Reply::Done(OpResult::Done));
        }
    }

    /// Every unit that stops has stopped: write `cgroup.kill` in every
    /// cgroup left beside `init.scope`, deepest first, and wait for them.
    pub(super) fn kill_the_rest(&mut self) {
        let Some(shutdown) = self.shutdown.as_mut() else {
            return;
        };
        if shutdown.killing.is_some() {
            return;
        }
        shutdown.anchor = None;
        let mut groups: Vec<(usize, String, UnitId, bool)> = Vec::new();
        for index in 0..self.units.len() {
            let Ok(index) = u32::try_from(index) else {
                continue;
            };
            let unit = UnitId(index);
            let Some(slot) = self.slot(unit) else {
                continue;
            };
            let Some(path) = &slot.group else {
                continue;
            };
            let spared = slot.perpetual
                || slot.adopted
                || path.as_str().is_empty()
                || path.as_str().starts_with("drivers.slice");
            if slot.made && !spared {
                groups.push((
                    path.depth(),
                    String::from(path.as_str()),
                    unit,
                    slot.populated,
                ));
            }
        }
        groups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut waiting = Vec::new();
        for (_, _, unit, populated) in groups {
            self.emit(Action::KillGroup { unit });
            if populated {
                waiting.push(unit);
            }
        }
        let deadline = self.now + KILL_WAIT;
        if let Some(shutdown) = self.shutdown.as_mut() {
            shutdown.deadline = Some(deadline);
            shutdown.killing = Some(waiting);
        }
        self.maybe_power_off();
    }

    /// A cgroup emptied during the last step.
    pub(super) fn shutdown_emptied(&mut self, unit: UnitId) {
        if let Some(waiting) = self.shutdown.as_mut().and_then(|s| s.killing.as_mut()) {
            waiting.retain(|&other| other != unit);
        }
        self.maybe_power_off();
    }

    /// The last step's wait ran out.
    pub(super) fn shutdown_timer(&mut self) {
        let now = self.now;
        let Some(shutdown) = self.shutdown.as_mut() else {
            return;
        };
        if shutdown.deadline.is_some_and(|at| at <= now) && !shutdown.done {
            shutdown.killing = Some(Vec::new());
            self.log(
                None,
                String::from("cgroups still populated after cgroup.kill; going down anyway"),
            );
            self.maybe_power_off();
        }
    }

    /// Power off once every killed cgroup is empty.
    fn maybe_power_off(&mut self) {
        let Some(shutdown) = self.shutdown.as_mut() else {
            return;
        };
        if shutdown.done || !shutdown.killing.as_ref().is_some_and(Vec::is_empty) {
            return;
        }
        shutdown.done = true;
        shutdown.deadline = None;
        let action = shutdown.action;
        self.emit(Action::Power(action));
    }

    /// `svc scope`: make a scope for processes init did not start.
    pub(super) fn scope(
        &mut self,
        client: ClientId,
        name: &str,
        slice: Option<&str>,
        pids: Vec<Pid>,
    ) {
        if self.shutdown.is_some() {
            self.reply(
                client,
                Reply::Refused(String::from("the machine is going down")),
            );
            return;
        }
        let parsed = UnitName::parse(name)
            .ok()
            .filter(|n| n.unit_type() == UnitType::Scope);
        let slice = match slice.map(UnitName::parse) {
            Some(Ok(slice)) if slice.unit_type() == UnitType::Slice => Some(slice),
            None => None,
            Some(_) => {
                self.reply(client, Reply::Refused(format!("{name}: not a slice")));
                return;
            }
        };
        let Some(unit) = parsed.and_then(|_| self.ensure(name)) else {
            self.reply(client, Reply::Refused(format!("{name}: not a scope name")));
            return;
        };
        if pids.is_empty()
            || self
                .slot(unit)
                .is_some_and(|s| s.active != ActiveState::Inactive || s.perpetual)
        {
            self.reply(
                client,
                Reply::Refused(format!("{name}: exists already, or no processes")),
            );
            return;
        }
        if let Some(slot) = self.slot_mut(unit) {
            if let Ok(loaded) = slot.loaded.as_mut()
                && let Config::Scope(scope) = &mut loaded.config
                && slice.is_some()
            {
                scope.slice = slice;
            }
            slot.scope_pids = pids;
        }
        let group = self.group_path(unit);
        if let Some(slot) = self.slot_mut(unit) {
            slot.group = group;
        }
        self.unresolved.push(unit);
        self.resolve();
        match self.transaction(unit, OpKind::Start, Mode::Replace) {
            Ok(Some(id)) => {
                if let Some(op) = self.ops.get_mut(&id) {
                    op.clients.push(client);
                }
            }
            Ok(None) => self.reply(client, Reply::Done(OpResult::Done)),
            Err(why) => self.reply(client, Reply::Refused(why)),
        }
    }

    /// An OPEN (§6): route it to the provider, starting the provider first
    /// if it is not running; refuse it if the unit did not declare the
    /// name or nobody offers it.
    pub(super) fn open(&mut self, from: UnitId, name: Name, end: Token) {
        let declared = match self
            .slot(from)
            .and_then(|s| s.loaded.as_ref().ok())
            .map(|u| &u.config)
        {
            Some(Config::Service(service)) => service.uses.contains(&name.0),
            _ => false,
        };
        let provider = (0..self.units.len())
            .filter_map(|index| u32::try_from(index).ok().map(UnitId))
            .find(|&unit| {
                matches!(
                    self.slot(unit).and_then(|s| s.loaded.as_ref().ok()).map(|u| &u.config),
                    Some(Config::Service(service)) if service.offers.contains(&name.0)
                )
            });
        let (true, Some(provider)) = (declared, provider) else {
            let line = format!("{}: refused OPEN of {}", self.display(from), name.0);
            self.log(Some(from), line);
            self.emit(Action::Refuse {
                to: from,
                name,
                end,
            });
            return;
        };
        let up = self
            .slot(provider)
            .is_some_and(|s| s.active == ActiveState::Active);
        if up {
            self.emit(Action::Route {
                to: provider,
                name,
                end,
            });
            return;
        }
        self.opens.push(Open {
            provider,
            from,
            name,
            end,
        });
        let queued = self.slot(provider).is_some_and(|s| s.op.is_some());
        if !queued && let Err(why) = self.transaction(provider, OpKind::Start, Mode::Replace) {
            self.log(Some(provider), why);
            self.refuse_opens(provider);
        }
    }

    /// A provider came up: route what waited for it.
    pub(super) fn deliver_opens(&mut self, provider: UnitId) {
        let (ready, waiting): (Vec<Open>, Vec<Open>) = self
            .opens
            .drain(..)
            .partition(|open| open.provider == provider);
        self.opens = waiting;
        for open in ready {
            self.emit(Action::Route {
                to: open.provider,
                name: open.name,
                end: open.end,
            });
        }
    }

    /// A provider failed to come up: refuse what waited for it.
    pub(super) fn refuse_opens(&mut self, provider: UnitId) {
        let (failed, waiting): (Vec<Open>, Vec<Open>) = self
            .opens
            .drain(..)
            .partition(|open| open.provider == provider);
        self.opens = waiting;
        for open in failed {
            self.emit(Action::Refuse {
                to: open.from,
                name: open.name,
                end: open.end,
            });
        }
    }
}
