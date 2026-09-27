//! Inputs the fuzzer found (`tests/fuzz/fuzz_targets/svc_manager.rs`),
//! replayed as host tests: the harness's unit set and event script, and its
//! three properties on the cgroup actions, run on every `cargo test`.

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::event::{Action, ClientId, Event, Exit, Pid, Request, UnitId};
use crate::source::{Entry, Layer, Source};
use crate::{Instant, Manager, NoProbe, Options};

/// The unit set every run starts from, beside the fuzzed service: the
/// harness's, word for word.
const UNITS: [(&str, &str); 9] = [
    ("sysinit.target", "[Unit]\nDefaultDependencies=no\n"),
    (
        "basic.target",
        "[Unit]\nRequires=sysinit.target\nAfter=sysinit.target\n",
    ),
    (
        "multi-user.target",
        "[Unit]\nRequires=basic.target\nAfter=basic.target\nAllowIsolate=yes\n",
    ),
    ("rescue.target", "[Unit]\nAllowIsolate=yes\n"),
    ("shutdown.target", "[Unit]\nDefaultDependencies=no\n"),
    ("poweroff.target", "[Unit]\nDefaultDependencies=no\n"),
    (
        "a.service",
        "[Service]\nExecStart=/bin/a\nExecReload=/bin/kick\nRestart=always\nRestartSec=1\n",
    ),
    (
        "b.service",
        "[Unit]\nAfter=a.service\nBindsTo=a.service\n[Service]\nType=oneshot\nExecStart=/bin/b\nRemainAfterExit=yes\n",
    ),
    (
        "c.service",
        "[Unit]\nWants=fuzz.service\n[Service]\nType=forking\nExecStart=/bin/c\nKillMode=mixed\nSlice=user-1.slice\n",
    ),
];

const NAMES: [&str; 5] = [
    "a.service",
    "b.service",
    "c.service",
    "fuzz.service",
    "multi-user.target",
];

/// The harness's manager: its unit set, `unit` as `fuzz.service`'s file,
/// and `multi-user.target` the default wanting three of them.
fn manager_for(unit: &[u8]) -> Result<Manager, String> {
    let mut source = Source::new();
    for (path, text) in UNITS {
        source
            .add(Layer::Image, path, Entry::File(text.as_bytes().to_vec()))
            .map_err(|error| format!("{path}: {error:?}"))?;
    }
    source
        .add(Layer::Image, "fuzz.service", Entry::File(unit.to_vec()))
        .map_err(|error| format!("fuzz.service: {error:?}"))?;
    source
        .add(
            Layer::Image,
            "default.target",
            Entry::Alias(String::from("multi-user.target")),
        )
        .map_err(|error| format!("default.target: {error:?}"))?;
    for name in ["a.service", "b.service", "c.service"] {
        source
            .add(
                Layer::Image,
                &format!("multi-user.target.wants/{name}"),
                Entry::Alias(String::from(name)),
            )
            .map_err(|error| format!("{name}: {error:?}"))?;
    }
    Ok(Manager::new(source, Box::new(NoProbe), Options::default()))
}

/// Run the harness on `data`, and answer the first property it breaks.
fn replay(data: &[u8]) -> Result<(), String> {
    let (unit, script) = match data.iter().position(|&byte| byte == 0) {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (data, &[][..]),
    };
    let mut manager = manager_for(unit)?;
    let mut made: BTreeSet<UnitId> = BTreeSet::new();
    let mut pids: Vec<Pid> = Vec::new();
    let mut now: u64 = 0;
    let mut next_pid: u32 = 100;
    let mut check = |step: usize, actions: Vec<Action>| -> Result<(), String> {
        for action in actions {
            match action {
                Action::MakeGroup { unit, .. } if !made.insert(unit) => {
                    return Err(format!("step {step}: a cgroup made twice ({unit:?})"));
                }
                Action::RemoveGroup { unit } if !made.remove(&unit) => {
                    return Err(format!(
                        "step {step}: a cgroup removed that was not made ({unit:?})"
                    ));
                }
                Action::Spawn { unit, .. } if !made.contains(&unit) => {
                    return Err(format!(
                        "step {step}: a spawn into a cgroup never made ({unit:?})"
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    };
    check(0, manager.step(Event::Boot, Instant::ZERO))?;
    for (step, pair) in script.chunks(2).enumerate() {
        let (&op, arg) = match pair {
            [op, arg] => (op, *arg),
            [op] => (op, 0),
            _ => break,
        };
        let named = NAMES[usize::from(arg) % NAMES.len()];
        let unit = manager.unit(named);
        let event = match (op % 13, unit) {
            (0, Some(unit)) => {
                let pid = Pid(next_pid);
                next_pid += 1;
                pids.push(pid);
                Event::Spawned { unit, pid }
            }
            (1, _) if !pids.is_empty() => {
                let pid = pids.remove(usize::from(arg) % pids.len());
                Event::Exited {
                    pid,
                    how: Exit::Code(i32::from(arg % 3)),
                }
            }
            (2, Some(unit)) => Event::Emptied { unit },
            (3, _) => {
                now = manager
                    .deadline()
                    .map_or(now + 1000, |at| at.as_nanos() / 1_000_000)
                    .max(now);
                Event::Timer
            }
            (4, _) => Event::Request {
                client: ClientId(1),
                request: Request::start(named),
            },
            (5, _) => Event::Request {
                client: ClientId(1),
                request: Request::stop(named),
            },
            (6, _) => Event::Request {
                client: ClientId(1),
                request: Request::restart(named),
            },
            (7, Some(unit)) => Event::Ready { unit, status: None },
            (8, Some(unit)) => Event::OomKilled { unit },
            (9, _) => Event::Request {
                client: ClientId(1),
                request: Request::Poweroff,
            },
            (10, _) => Event::Request {
                client: ClientId(1),
                request: Request::Isolate(String::from("rescue.target")),
            },
            (11, _) => Event::Request {
                client: ClientId(1),
                request: Request::Scope {
                    unit: format!("s-{arg}.scope"),
                    slice: None,
                    pids: vec![Pid(9000 + u32::from(arg))],
                },
            },
            (12, _) => Event::Request {
                client: ClientId(1),
                request: Request::Reload(String::from(named)),
            },
            _ => continue,
        };
        check(step + 1, manager.step(event, Instant::from_millis(now)))?;
    }
    Ok(())
}

/// CI run 36294744287 on main 0c55ae64: `Isolate(rescue.target)` and a
/// restart of `b.service`, which `BindsTo=a.service`, and a spawn with no
/// cgroup.
#[test]
fn an_isolate_then_a_restart_spawns_into_a_made_cgroup() {
    let input = [
        0x53, 0x00, 0x00, 0x5b, 0x53, 0xbb, 0xbb, 0xbb, 0xbb, 0x65, 0x72, 0x76, 0x69, 0x63, 0x65,
        0x5d, 0x0a, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x45, 0x78, 0x64, 0x3a, 0x65,
        0x73, 0x63, 0x75, 0x65, 0x2e, 0x74, 0x61, 0x72, 0x67, 0x65, 0x74,
    ];
    assert_eq!(replay(&input), Ok(()));
}
