//! The Wayland side, and the loop that waits on it and on `loginctl`.
//!
//! Upstream's `CHypridle::run` and `enterEventLoop`: bind
//! `ext_idle_notifier_v1`, the first `wl_seat` and, when the compositor has
//! it, `hyprland_lock_notifier_v1`; make one notification for each listener,
//! with `get_input_idle_notification` where inhibitors are to be ignored and
//! `get_idle_notification` otherwise; then wait for their events. Where
//! upstream waits on two D-Bus connections as well, this waits on the
//! socket `loginctl` writes to ([`crate::session`]).
//!
//! No surface, no buffer: an idle daemon draws nothing, so this speaks the
//! wire protocol directly, as `lswt` does, rather than through a client
//! runtime.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use compositor_protocol::core::{self, wl_display, wl_registry};
use compositor_protocol::idle_notify::{
    self, ext_idle_notification_v1 as notification, ext_idle_notifier_v1 as notifier,
};
use compositor_protocol::lock_notify::{
    self, hyprland_lock_notification_v1 as lock_notification,
    hyprland_lock_notifier_v1 as lock_notifier,
};
use compositor_socket::{Connection, RecvError};
use compositor_wire::{Arg, ArgType, Interface, ObjectId, Reader, Writer};

use crate::idle::{Idle, Run};
use crate::log::{Level, Log};
use crate::session::{self, Request};

/// The objects made before the globals are known.
mod id {
    use compositor_wire::ObjectId;

    pub(super) const DISPLAY: ObjectId = ObjectId(1);
    pub(super) const REGISTRY: ObjectId = ObjectId(2);
    pub(super) const SYNC: ObjectId = ObjectId(3);
    /// The first id handed out afterwards.
    pub(super) const FIRST_FREE: u32 = 4;
}

/// How long to wait for the compositor to list its globals.
const PATIENCE: Duration = Duration::from_secs(20);

/// How often children are looked at to be reaped while any is running.
const REAP: Duration = Duration::from_secs(1);

/// How long a `loginctl` has to send its line once connected.
const REQUEST_PATIENCE: Duration = Duration::from_secs(1);

/// Commands, run the way upstream's `Hyprutils::OS::CProcess` runs them.
#[derive(Debug)]
pub struct Runner {
    /// Where lines go.
    pub log: Log,
    /// What was started and has not been reaped.
    children: Vec<Child>,
}

impl Runner {
    /// A runner with nothing started.
    #[must_use]
    pub fn new(log: Log) -> Self {
        Self {
            log,
            children: Vec::new(),
        }
    }

    /// Wait for whatever has exited, so no child is left a zombie.
    fn reap(&mut self) {
        self.children
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    }
}

impl Run for Runner {
    fn spawn(&mut self, command: &str) {
        self.log.say(Level::Log, &format!("Executing {command}"));
        match Command::new("/bin/sh")
            .args(["-c", command])
            .stdin(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                self.log.say(
                    Level::Log,
                    &format!("Process Created with pid {}", child.id()),
                );
                self.children.push(child);
            }
            Err(error) => {
                self.log
                    .say(Level::Err, &format!("Failed run \"{command}\": {error}"));
            }
        }
    }

    fn condition(&mut self, command: &str) -> bool {
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

/// What the compositor said, once read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Said {
    Idled(usize),
    Resumed(usize),
    Locked,
    Unlocked,
}

/// The connection and the objects on it.
struct Wayland {
    connection: Connection,
    out: Writer,
    next: u32,
    /// Every global: name, interface, version.
    globals: Vec<(u32, String, u32)>,
    synced: bool,
    notifier: Option<(ObjectId, u32)>,
    seat: Option<ObjectId>,
    lock_notifier: Option<ObjectId>,
    lock_notification: Option<ObjectId>,
    /// Each notification and the listener it is for.
    notifications: BTreeMap<ObjectId, usize>,
}

impl Wayland {
    fn connect(socket: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(socket)
            .map_err(|error| format!("connecting to {}: {error}", socket.display()))?;
        let connection =
            Connection::new(stream).map_err(|error| format!("the connection: {error}"))?;
        let mut wayland = Self {
            connection,
            out: Writer::new(),
            next: id::FIRST_FREE,
            globals: Vec::new(),
            synced: false,
            notifier: None,
            seat: None,
            lock_notifier: None,
            lock_notification: None,
            notifications: BTreeMap::new(),
        };
        wayland.request(
            id::DISPLAY,
            wl_display::request::GET_REGISTRY,
            &[ArgType::NewId],
            &[Arg::NewId(id::REGISTRY)],
        );
        wayland.request(
            id::DISPLAY,
            wl_display::request::SYNC,
            &[ArgType::NewId],
            &[Arg::NewId(id::SYNC)],
        );
        Ok(wayland)
    }

    fn fresh(&mut self) -> ObjectId {
        let id = ObjectId(self.next);
        self.next = self.next.saturating_add(1);
        id
    }

    fn request(
        &mut self,
        sender: ObjectId,
        opcode: u16,
        signature: &'static [ArgType],
        args: &[Arg<'_>],
    ) {
        let _ = self.out.write(sender, opcode, signature, args);
    }

    fn bind(&mut self, name: u32, interface: &'static str, version: u32) -> ObjectId {
        let id = self.fresh();
        self.request(
            id::REGISTRY,
            wl_registry::request::BIND,
            &[ArgType::Uint, ArgType::AnyNewId],
            &[
                Arg::Uint(name),
                Arg::AnyNewId {
                    interface,
                    version,
                    id,
                },
            ],
        );
        id
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.out.is_empty() {
            return self
                .connection
                .flush()
                .map_err(|error| format!("writing: {error:?}"));
        }
        let (bytes, fds) = self.out.take();
        self.connection
            .send(&bytes, &fds)
            .map_err(|error| format!("writing: {error:?}"))
    }

    /// Read what has arrived. `Ok(false)` is the compositor gone.
    fn receive(&mut self, log: &Log, said: &mut Vec<Said>) -> Result<bool, String> {
        loop {
            match self.connection.receive() {
                Ok(0) | Err(RecvError::Closed) => return Ok(false),
                Ok(_) => {}
                Err(RecvError::WouldBlock) => break,
                Err(error) => return Err(format!("reading: {error:?}")),
            }
        }
        let fds = self.connection.fds();
        let bytes = self.connection.bytes().to_vec();
        let mut reader = Reader::new(&bytes, &fds);
        while !reader.is_done() {
            let Ok(header) = reader.peek() else {
                break;
            };
            let Some(interface) = self.interface_of(header.sender) else {
                // A notification this destroyed can still have an event on
                // the way; its signature is empty either way.
                if reader.skip(0).is_err() {
                    break;
                }
                continue;
            };
            let Some(method) = interface.event(header.opcode) else {
                return Err(format!("{} has no event {}", interface.name, header.opcode));
            };
            let (_, args) = match reader.read(method.signature) {
                Ok(read) => read,
                Err(compositor_wire::Error::Incomplete { .. }) => break,
                Err(error) => return Err(format!("{}.{}: {error:?}", interface.name, method.name)),
            };
            self.event(header.sender, header.opcode, &args, log, said)?;
        }
        let (consumed, claimed) = (reader.consumed(), reader.descriptors_taken());
        if consumed > 0 {
            self.connection.consume(consumed, claimed);
        }
        Ok(true)
    }

    fn interface_of(&self, id: ObjectId) -> Option<&'static Interface> {
        if self.notifications.contains_key(&id) {
            return Some(&idle_notify::EXT_IDLE_NOTIFICATION_V1);
        }
        if Some(id) == self.notifier.map(|(id, _)| id) {
            return Some(&idle_notify::EXT_IDLE_NOTIFIER_V1);
        }
        if Some(id) == self.seat {
            return Some(&core::WL_SEAT);
        }
        if Some(id) == self.lock_notifier {
            return Some(&lock_notify::HYPRLAND_LOCK_NOTIFIER_V1);
        }
        if Some(id) == self.lock_notification {
            return Some(&lock_notify::HYPRLAND_LOCK_NOTIFICATION_V1);
        }
        Some(match id {
            id::DISPLAY => &core::WL_DISPLAY,
            id::REGISTRY => &core::WL_REGISTRY,
            id::SYNC => &core::WL_CALLBACK,
            _ => return None,
        })
    }

    fn event(
        &mut self,
        sender: ObjectId,
        opcode: u16,
        args: &[Arg<'_>],
        log: &Log,
        said: &mut Vec<Said>,
    ) -> Result<(), String> {
        if sender == id::DISPLAY && opcode == wl_display::event::ERROR {
            let text = args.get(2).and_then(Arg::as_str).unwrap_or("");
            return Err(format!("the compositor refused hypridle: {text}"));
        }
        if sender == id::REGISTRY && opcode == wl_registry::event::GLOBAL {
            if let (Some(name), Some(interface), Some(version)) = (
                args.first().and_then(Arg::as_uint),
                args.get(1).and_then(Arg::as_str),
                args.get(2).and_then(Arg::as_uint),
            ) {
                log.say(
                    Level::Log,
                    &format!("  | got iface: {interface} v{version}"),
                );
                self.globals.push((name, interface.to_owned(), version));
            }
            return Ok(());
        }
        if sender == id::REGISTRY && opcode == wl_registry::event::GLOBAL_REMOVE {
            if let Some(name) = args.first().and_then(Arg::as_uint) {
                log.say(Level::Log, &format!("  | removed iface {name}"));
            }
            return Ok(());
        }
        if sender == id::SYNC && opcode == core::wl_callback::event::DONE {
            self.synced = true;
            return Ok(());
        }
        if let Some(index) = self.notifications.get(&sender).copied() {
            match opcode {
                notification::event::IDLED => said.push(Said::Idled(index)),
                notification::event::RESUMED => said.push(Said::Resumed(index)),
                _ => {}
            }
            return Ok(());
        }
        if Some(sender) == self.lock_notification {
            match opcode {
                lock_notification::event::LOCKED => said.push(Said::Locked),
                lock_notification::event::UNLOCKED => said.push(Said::Unlocked),
                _ => {}
            }
        }
        Ok(())
    }

    /// The first global of an interface.
    fn global(&self, interface: &str) -> Option<(u32, u32)> {
        self.globals
            .iter()
            .find(|(_, held, _)| held == interface)
            .map(|(name, _, version)| (*name, *version))
    }
}

/// Connect, bind, make the notifications and wait on them for ever.
///
/// # Errors
///
/// What upstream ends on: no compositor, no `ext_idle_notifier_v1`, or the
/// compositor going away.
pub fn run(socket: &Path, idle: &mut Idle, runner: &mut Runner) -> Result<(), String> {
    let log = runner.log;
    let mut wayland = Wayland::connect(socket)
        .map_err(|error| format!("Couldn't connect to a wayland compositor ({error})"))?;
    wayland.flush()?;
    let started = Instant::now();
    let mut said = Vec::new();
    while !wayland.synced {
        if started.elapsed() > PATIENCE {
            return Err("the compositor never finished listing its globals".to_owned());
        }
        if !wayland.receive(&log, &mut said)? {
            return Err("the compositor closed the connection".to_owned());
        }
        let _ = wait(
            &[wayland.connection.as_raw_fd()],
            Some(Duration::from_millis(100)),
        );
    }

    if let Some((name, version)) = wayland.global("ext_idle_notifier_v1") {
        let version = version.min(2);
        let bound = wayland.bind(name, "ext_idle_notifier_v1", version);
        wayland.notifier = Some((bound, version));
        log.say(
            Level::Log,
            &format!("   > Bound to ext_idle_notifier_v1 v{version}"),
        );
    }
    if let Some((name, version)) = wayland.global("hyprland_lock_notifier_v1") {
        let version = version.min(1);
        wayland.lock_notifier = Some(wayland.bind(name, "hyprland_lock_notifier_v1", version));
        log.say(
            Level::Log,
            &format!("   > Bound to hyprland_lock_notifier_v1 v{version}"),
        );
    }
    let seats = wayland
        .globals
        .iter()
        .filter(|(_, interface, _)| interface == "wl_seat")
        .count();
    if seats > 1 {
        log.say(
            Level::Warn,
            "Hypridle does not support multi-seat configurations. Only binding to the first seat.",
        );
    }
    if let Some((name, version)) = wayland.global("wl_seat") {
        // Nothing of the seat's is read; the lowest version is the fewest
        // events to parse.
        let _ = version;
        wayland.seat = Some(wayland.bind(name, "wl_seat", 1));
        log.say(Level::Log, "   > Bound to wl_seat v1");
    }
    let Some((notifier_id, notifier_version)) = wayland.notifier else {
        return Err(
            "Couldn't bind to ext-idle-notifier-v1, does your compositor support it?".to_owned(),
        );
    };
    let Some(seat) = wayland.seat else {
        return Err("the compositor offers no wl_seat to be idle on".to_owned());
    };

    log.say(Level::Log, &format!("found {} rules", idle.listeners.len()));
    for index in 0..idle.listeners.len() {
        let Some(listener) = idle.listeners.get(index) else {
            continue;
        };
        let millis = listener.rule.timeout.saturating_mul(1000);
        let timeout = u32::try_from(millis).unwrap_or_else(|_| {
            log.say(
                Level::Warn,
                &format!(
                    "rule {index}: a timeout of {} s is more than ext-idle-notify's 32-bit \
                     milliseconds hold; waiting {} s instead",
                    listener.rule.timeout,
                    u32::MAX / 1000
                ),
            );
            u32::MAX
        });
        let mut opcode = if idle.ignores_inhibitors(index) {
            notifier::request::GET_INPUT_IDLE_NOTIFICATION
        } else {
            notifier::request::GET_IDLE_NOTIFICATION
        };
        if opcode == notifier::request::GET_INPUT_IDLE_NOTIFICATION && notifier_version < 2 {
            log.say(
                Level::Warn,
                &format!(
                    "rule {index}: the compositor's ext_idle_notifier_v1 is version 1, which has \
                     no get_input_idle_notification; this rule is held off by idle inhibitors"
                ),
            );
            opcode = notifier::request::GET_IDLE_NOTIFICATION;
        }
        let made = wayland.fresh();
        wayland.request(
            notifier_id,
            opcode,
            &[
                ArgType::NewId,
                ArgType::Uint,
                ArgType::Object { nullable: false },
            ],
            &[Arg::NewId(made), Arg::Uint(timeout), Arg::Object(seat)],
        );
        let _ = wayland.notifications.insert(made, index);
    }
    if let Some(lock) = wayland.lock_notifier {
        let made = wayland.fresh();
        wayland.request(
            lock,
            lock_notifier::request::GET_LOCK_NOTIFICATION,
            &[ArgType::NewId],
            &[Arg::NewId(made)],
        );
        wayland.lock_notification = Some(made);
    } else {
        log.say(
            Level::Warn,
            "Compositor is missing hyprland-lock-notify-v1!\ngeneral:on_lock_cmd and \
             general:on_unlock_cmd will not work.",
        );
    }
    wayland.flush()?;
    log.say(Level::Log, "wayland done, listening for loginctl");

    let path = session::path();
    let listener = listen(&path, &log);
    event_loop(&mut wayland, listener.as_ref(), idle, runner, &said)
}

/// Bind the session socket, unless another hypridle already answers on it.
fn listen(path: &PathBuf, log: &Log) -> Option<UnixListener> {
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
    wayland: &mut Wayland,
    listener: Option<&UnixListener>,
    idle: &mut Idle,
    runner: &mut Runner,
    early: &[Said],
) -> Result<(), String> {
    let log = runner.log;
    for said in early {
        apply(said, idle, runner);
    }
    loop {
        runner.reap();
        let now = Instant::now();
        let mut timeout = idle
            .next_retry()
            .map(|at| at.saturating_duration_since(now));
        if !runner.children.is_empty() {
            timeout = Some(timeout.map_or(REAP, |held| held.min(REAP)));
        }
        let mut fds = vec![wayland.connection.as_raw_fd()];
        if let Some(listener) = listener {
            fds.push(listener.as_raw_fd());
        }
        let ready = wait(&fds, timeout)
            .map_err(|error| format!("[core] Polling fds failed with {error}"))?;
        if ready.contains(&wayland.connection.as_raw_fd()) {
            let mut said = Vec::new();
            if !wayland.receive(&log, &mut said)? {
                return Err("[core] Disconnected from pollfd id 1".to_owned());
            }
            for event in &said {
                apply(event, idle, runner);
            }
        }
        if let Some(listener) = listener
            && ready.contains(&listener.as_raw_fd())
        {
            while let Ok((stream, _)) = listener.accept() {
                answer(stream, idle, runner);
            }
        }
        idle.retry(Instant::now(), runner);
        wayland.flush()?;
    }
}

fn apply(said: &Said, idle: &mut Idle, runner: &mut Runner) {
    match said {
        Said::Idled(index) => idle.idled(*index, Instant::now(), runner),
        Said::Resumed(index) => idle.resumed(*index, runner),
        Said::Locked => idle.session_locked(runner),
        Said::Unlocked => idle.session_unlocked(runner),
    }
}

/// One `loginctl`: read its line, do it, and say so.
fn answer(stream: UnixStream, idle: &mut Idle, runner: &mut Runner) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(REQUEST_PATIENCE));
    let mut line = String::new();
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
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

/// Wait for any of `fds` to be readable, or for `timeout`.
fn wait(fds: &[i32], timeout: Option<Duration>) -> std::io::Result<Vec<i32>> {
    let mut polled: Vec<libc::pollfd> = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let millis = timeout.map_or(-1, |timeout| {
        // Rounded up, so a retry just short of due is not a busy loop.
        let whole = timeout
            .as_millis()
            .saturating_add(u128::from(timeout.subsec_nanos() % 1_000_000 != 0));
        i32::try_from(whole).unwrap_or(i32::MAX)
    });
    let count = libc::nfds_t::try_from(polled.len()).unwrap_or(0);
    loop {
        // SAFETY: `polled` holds `count` initialised `pollfd`s and lives for
        // the whole call.
        let waited = unsafe { libc::poll(polled.as_mut_ptr(), count, millis) };
        if waited >= 0 {
            return Ok(polled
                .iter()
                .filter(|poll| poll.revents != 0)
                .map(|poll| poll.fd)
                .collect());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
