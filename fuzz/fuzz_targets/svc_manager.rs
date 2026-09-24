//! Fuzz the service manager's state machine (`libs/svc`, `Manager::step`).
//!
//! Init is pid 1: a panic in the manager is the machine. Its events come
//! from processes that exit when they like, cgroups that empty when their
//! last process goes, clients that ask for anything, and a clock. This
//! target builds a manager over a small unit set plus one fuzzed service,
//! then feeds it a script of events chosen by the input, in any order the
//! bytes choose, including orders no backend would produce.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it, the cgroup actions stay
//! consistent, which is what the Groups backend relies on:
//!
//! 1. **A cgroup is made once**: no `MakeGroup` for a unit whose cgroup the
//!    manager already made and has not removed.
//! 2. **A cgroup is removed only once made**: no `RemoveGroup` for a unit
//!    whose cgroup does not exist.
//! 3. **Every spawn is into a cgroup the manager made**.

#![no_main]

use std::collections::BTreeSet;

use ferrix_svc::event::{Action, ClientId, Event, Exit, Pid, Request, UnitId};
use ferrix_svc::source::{Entry, Layer, Source};
use ferrix_svc::{Instant, Manager, NoProbe, Options};
use libfuzzer_sys::fuzz_target;

/// The unit set every run starts from, beside the fuzzed service.
const UNITS: [(&str, &str); 9] = [
    ("sysinit.target", "[Unit]\nDefaultDependencies=no\n"),
    ("basic.target", "[Unit]\nRequires=sysinit.target\nAfter=sysinit.target\n"),
    ("multi-user.target", "[Unit]\nRequires=basic.target\nAfter=basic.target\nAllowIsolate=yes\n"),
    ("rescue.target", "[Unit]\nAllowIsolate=yes\n"),
    ("shutdown.target", "[Unit]\nDefaultDependencies=no\n"),
    ("poweroff.target", "[Unit]\nDefaultDependencies=no\n"),
    ("a.service", "[Service]\nExecStart=/bin/a\nRestart=always\nRestartSec=1\n"),
    ("b.service", "[Unit]\nAfter=a.service\nBindsTo=a.service\n[Service]\nType=oneshot\nExecStart=/bin/b\nRemainAfterExit=yes\n"),
    ("c.service", "[Unit]\nWants=fuzz.service\n[Service]\nType=forking\nExecStart=/bin/c\nKillMode=mixed\nSlice=user-1.slice\n"),
];

const NAMES: [&str; 5] = ["a.service", "b.service", "c.service", "fuzz.service", "multi-user.target"];

fuzz_target!(|data: &[u8]| {
    let (unit, script) = match data.iter().position(|&byte| byte == 0) {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (data, &[][..]),
    };
    let mut source = Source::new();
    for (path, text) in UNITS {
        source.add(Layer::Image, path, Entry::File(text.as_bytes().to_vec())).expect("a unit path");
    }
    source.add(Layer::Image, "fuzz.service", Entry::File(unit.to_vec())).expect("a unit path");
    source.add(Layer::Image, "default.target", Entry::Alias(String::from("multi-user.target"))).expect("an alias");
    for name in ["a.service", "b.service", "c.service"] {
        source
            .add(Layer::Image, &format!("multi-user.target.wants/{name}"), Entry::Alias(String::from(name)))
            .expect("a link");
    }
    let mut manager = Manager::new(source, Box::new(NoProbe), Options::default());
    let mut made: BTreeSet<UnitId> = BTreeSet::new();
    let mut pids: Vec<Pid> = Vec::new();
    let mut now: u64 = 0;
    let mut next_pid: u32 = 100;
    let mut check = |actions: Vec<Action>| {
        for action in actions {
            match action {
                Action::MakeGroup { unit, .. } => {
                    assert!(made.insert(unit), "a cgroup made twice");
                }
                Action::RemoveGroup { unit } => {
                    assert!(made.remove(&unit), "a cgroup removed that was not made");
                }
                Action::Spawn { unit, .. } => {
                    assert!(made.contains(&unit), "a spawn into a cgroup never made");
                }
                _ => {}
            }
        }
    };
    check(manager.step(Event::Boot, Instant::ZERO));
    for pair in script.chunks(2) {
        let (&op, arg) = match pair {
            [op, arg] => (op, *arg),
            [op] => (op, 0),
            _ => break,
        };
        let unit = manager.unit(NAMES[usize::from(arg) % NAMES.len()]);
        let event = match (op % 12, unit) {
            (0, Some(unit)) => {
                let pid = Pid(next_pid);
                next_pid += 1;
                pids.push(pid);
                Event::Spawned { unit, pid }
            }
            (1, _) if !pids.is_empty() => {
                let pid = pids.remove(usize::from(arg) % pids.len());
                Event::Exited { pid, how: Exit::Code(i32::from(arg % 3)) }
            }
            (2, Some(unit)) => Event::Emptied { unit },
            (3, _) => {
                now = manager.deadline().map_or(now + 1000, |at| at.as_nanos() / 1_000_000).max(now);
                Event::Timer
            }
            (4, _) => Event::Request { client: ClientId(1), request: Request::start(NAMES[usize::from(arg) % NAMES.len()]) },
            (5, _) => Event::Request { client: ClientId(1), request: Request::stop(NAMES[usize::from(arg) % NAMES.len()]) },
            (6, _) => Event::Request { client: ClientId(1), request: Request::restart(NAMES[usize::from(arg) % NAMES.len()]) },
            (7, Some(unit)) => Event::Ready { unit, status: None },
            (8, Some(unit)) => Event::OomKilled { unit },
            (9, _) => Event::Request { client: ClientId(1), request: Request::Poweroff },
            (10, _) => Event::Request { client: ClientId(1), request: Request::Isolate(String::from("rescue.target")) },
            (11, _) => Event::Request {
                client: ClientId(1),
                request: Request::Scope { unit: format!("s-{arg}.scope"), slice: None, pids: vec![Pid(9000 + u32::from(arg))] },
            },
            _ => continue,
        };
        check(manager.step(event, Instant::from_millis(now)));
    }
});
