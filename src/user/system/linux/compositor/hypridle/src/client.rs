//! The Wayland side, and the loop that waits on it and on `loginctl`.
//!
//! Upstream's `CHypridle::run` and `enterEventLoop`, over
//! `src/user/system/linux/compositor/toolkit`: one `ext_idle_notification_v1` for each listener,
//! made with `get_input_idle_notification` where inhibitors are to be
//! ignored and `get_idle_notification` otherwise; a
//! `hyprland_lock_notification_v1` when the compositor has the notifier;
//! then their events, and the socket `loginctl` writes to
//! ([`crate::session`]) where upstream waits on two D-Bus connections.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use compositor_toolkit::protocol::lock_notify::{
    self, hyprland_lock_notification_v1 as lock_notification,
    hyprland_lock_notifier_v1 as lock_notifier,
};
use compositor_toolkit::{Client, Error, Event, IdleId, ObjectId, Value, WatchId};

use crate::idle::{Idle, Run};
use crate::log::{Level, Log};
use crate::session::{self, Request};

/// How long a `loginctl` has to send its line once connected.
const REQUEST_PATIENCE: Duration = Duration::from_secs(1);

/// The longest timeout `ext-idle-notify-v1` carries: a `uint` of
/// milliseconds.
const LONGEST: u64 = u32::MAX as u64 / 1000;

/// Commands, run the way upstream's `Hyprutils::OS::CProcess` runs them.
#[derive(Clone, Copy, Debug)]
pub struct Runner {
    /// Where lines go.
    pub log: Log,
}

impl Run for Runner {
    fn spawn(&mut self, command: &str) {
        self.log.say(Level::Log, &format!("Executing {command}"));
        // Detached, in a session of its own, as `runAsync` leaves it: a
        // locker it starts outlives hypridle.
        if let Err(error) = compositor_toolkit::spawn(command) {
            self.log
                .say(Level::Err, &format!("Failed run \"{command}\": {error}"));
        }
    }

    fn condition(&mut self, command: &str) -> bool {
        // Waited for, as upstream's `runSync` waits.
        match Command::new("/bin/sh")
            .args(["-c", command])
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) => {
                let code = status.code().unwrap_or(-1);
                self.log
                    .say(Level::Log, &format!("condition_cmd exited with {code}"));
                code == 0
            }
            Err(error) => {
                self.log.say(
                    Level::Err,
                    &format!("Failed to run condition_cmd: {command}: {error}"),
                );
                false
            }
        }
    }

    fn log(&mut self, level: Level, line: &str) {
        self.log.say(level, line);
    }
}

/// Connect, make the notifications and wait on them for ever.
///
/// # Errors
///
/// What upstream ends on: no compositor, no `ext_idle_notifier_v1`, or the
/// compositor going away.
pub fn run(idle: &mut Idle, runner: &mut Runner) -> Result<(), String> {
    let log = runner.log;
    let mut client = Client::connect()
        .map_err(|error| format!("Couldn't connect to a wayland compositor ({error})"))?;
    for global in client.globals() {
        log.say(
            Level::Log,
            &format!("  | got iface: {} v{}", global.interface, global.version),
        );
    }
    let seats = client
        .globals()
        .iter()
        .filter(|global| global.interface == "wl_seat")
        .count();
    if seats > 1 {
        log.say(
            Level::Warn,
            "Hypridle does not support multi-seat configurations. Only binding to the first seat.",
        );
    }
    let Some(version) = client.bound_version("ext_idle_notifier_v1") else {
        return Err(
            "Couldn't bind to ext-idle-notifier-v1, does your compositor support it?".to_owned(),
        );
    };
    log.say(
        Level::Log,
        &format!("   > Bound to ext_idle_notifier_v1 v{version}"),
    );

    log.say(Level::Log, &format!("found {} rules", idle.listeners.len()));
    let mut listeners: BTreeMap<IdleId, usize> = BTreeMap::new();
    for index in 0..idle.listeners.len() {
        let Some(listener) = idle.listeners.get(index) else {
            continue;
        };
        let mut seconds = listener.rule.timeout;
        if seconds > LONGEST {
            log.say(
                Level::Warn,
                &format!(
                    "rule {index}: a timeout of {seconds} s is more than ext-idle-notify's 32-bit \
                     milliseconds hold; waiting {LONGEST} s instead"
                ),
            );
            seconds = LONGEST;
        }
        let made = client
            .idle_notification(
                Duration::from_secs(seconds),
                !idle.ignores_inhibitors(index),
            )
            .map_err(|error| format!("rule {index}: {error}"))?;
        let _ = listeners.insert(made, index);
    }

    let lock = match client.bind(&lock_notify::HYPRLAND_LOCK_NOTIFIER_V1, 1, None) {
        Ok((notifier, version)) => {
            log.say(
                Level::Log,
                &format!("   > Bound to hyprland_lock_notifier_v1 v{version}"),
            );
            let made = client.new_object(&lock_notify::HYPRLAND_LOCK_NOTIFICATION_V1, 1);
            client
                .request(
                    notifier,
                    lock_notifier::request::GET_LOCK_NOTIFICATION,
                    &[Value::NewId(made)],
                )
                .map_err(|error| error.to_string())?;
            Some(made)
        }
        Err(Error::Missing(_)) => {
            log.say(
                Level::Warn,
                "Compositor is missing hyprland-lock-notify-v1!\ngeneral:on_lock_cmd and \
                 general:on_unlock_cmd will not work.",
            );
            None
        }
        Err(error) => return Err(error.to_string()),
    };
    client.flush().map_err(|error| error.to_string())?;
    log.say(Level::Log, "wayland done, listening for loginctl");

    let listener = listen(&session::path(), &log);
    let watch = listener
        .as_ref()
        .map(|listener| client.watch_fd(listener.as_raw_fd()));
    event_loop(
        &mut client,
        &listeners,
        lock,
        listener.as_ref().zip(watch),
        idle,
        runner,
    )
}

/// Bind the session socket, unless another hypridle already answers on it.
fn listen(path: &Path, log: &Log) -> Option<UnixListener> {
    if UnixStream::connect(path).is_ok() {
        log.say(
            Level::Err,
            &format!(
                "Another program is already listening for loginctl on {}\nIs hypridle already \
                 running? lock_cmd and unlock_cmd will not run from this one.",
                path.display()
            ),
        );
        return None;
    }
    let _ = std::fs::remove_file(path);
    match UnixListener::bind(path).and_then(|listener| {
        listener.set_nonblocking(true)?;
        Ok(listener)
    }) {
        Ok(listener) => {
            log.say(
                Level::Log,
                &format!(
                    "loginctl lock-session reaches lock_cmd through {}",
                    path.display()
                ),
            );
            Some(listener)
        }
        Err(error) => {
            log.say(
                Level::Err,
                &format!(
                    "Couldn't listen for loginctl on {} ({error}); lock_cmd and unlock_cmd will \
                     not run",
                    path.display()
                ),
            );
            None
        }
    }
}

fn event_loop(
    client: &mut Client,
    listeners: &BTreeMap<IdleId, usize>,
    lock: Option<ObjectId>,
    session: Option<(&UnixListener, WatchId)>,
    idle: &mut Idle,
    runner: &mut Runner,
) -> Result<(), String> {
    loop {
        let timeout = idle
            .next_retry()
            .map(|at| at.saturating_duration_since(Instant::now()));
        let events = client.dispatch(timeout).map_err(|error| match error {
            Error::Closed => "[core] Disconnected from the compositor".to_owned(),
            other => other.to_string(),
        })?;
        for event in events {
            on_event(event, listeners, lock, session, idle, runner);
        }
        idle.retry(Instant::now(), runner);
    }
}

/// One event from the loop.
fn on_event(
    event: Event,
    listeners: &BTreeMap<IdleId, usize>,
    lock: Option<ObjectId>,
    session: Option<(&UnixListener, WatchId)>,
    idle: &mut Idle,
    runner: &mut Runner,
) {
    match event {
        Event::Idled(made) => {
            if let Some(index) = listeners.get(&made) {
                idle.idled(*index, Instant::now(), runner);
            }
        }
        Event::Resumed(made) => {
            if let Some(index) = listeners.get(&made) {
                idle.resumed(*index, runner);
            }
        }
        Event::Object { object, opcode, .. } if Some(object) == lock => match opcode {
            lock_notification::event::LOCKED => idle.session_locked(runner),
            lock_notification::event::UNLOCKED => idle.session_unlocked(runner),
            _ => {}
        },
        Event::Readable(watched) => {
            let Some((listener, watch)) = session else {
                return;
            };
            if watch != watched {
                return;
            }
            while let Ok((stream, _)) = listener.accept() {
                answer(stream, idle, runner);
            }
        }
        _ => {}
    }
}

/// One `loginctl`: read its line, do it, and say so.
fn answer(stream: UnixStream, idle: &mut Idle, runner: &mut Runner) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(REQUEST_PATIENCE));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut line = String::new();
    let _ = BufReader::new(stream).read_line(&mut line);
    let reply = match Request::parse(&line) {
        Ok(Request::Lock) => {
            idle.lock_session(runner);
            "ok\n".to_owned()
        }
        Ok(Request::Unlock) => {
            idle.unlock_session(runner);
            "ok\n".to_owned()
        }
        Err(error) => {
            runner.log(Level::Warn, &format!("loginctl: {error}"));
            format!("error: {error}\n")
        }
    };
    let _ = writer.write_all(reply.as_bytes());
}
