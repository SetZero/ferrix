//! The loop's other sources, and the one `poll` over all of them.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, ChildStdout, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::loop_sources::{ChildOutput, Command, Waker};
use crate::{ChildId, Event, TimerId, WatchId};

/// One timer.
#[derive(Clone, Copy, Debug)]
struct Timer {
    due: Instant,
    every: Option<Duration>,
}

/// What one `poll` found ready.
#[derive(Debug, Default)]
pub(crate) struct Ready {
    pub(crate) wayland: bool,
    pub(crate) children: Vec<ChildId>,
    pub(crate) signals: bool,
    pub(crate) watches: Vec<WatchId>,
    pub(crate) waker: bool,
}

/// Timers, signals, watched descriptors and the waker.
#[derive(Debug, Default)]
pub(crate) struct Sources {
    timers: BTreeMap<TimerId, Timer>,
    next_timer: u32,
    signalfd: Option<OwnedFd>,
    blocked: Vec<i32>,
    watches: BTreeMap<WatchId, i32>,
    next_watch: u32,
    waker: Option<Arc<OwnedFd>>,
}

impl Sources {
    pub(crate) fn add_timer(&mut self, after: Duration, every: Option<Duration>) -> TimerId {
        self.next_timer = self.next_timer.wrapping_add(1);
        let id = TimerId(self.next_timer);
        let _ = self.timers.insert(
            id,
            Timer {
                due: Instant::now() + after,
                every: every.filter(|every| !every.is_zero()),
            },
        );
        id
    }

    pub(crate) fn cancel_timer(&mut self, timer: TimerId) {
        let _ = self.timers.remove(&timer);
    }

    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.timers.values().map(|timer| timer.due).min()
    }

    pub(crate) fn fire_timers(&mut self, pending: &mut Vec<Event>) {
        let now = Instant::now();
        let mut done = Vec::new();
        for (id, timer) in &mut self.timers {
            if timer.due > now {
                continue;
            }
            pending.push(Event::Timer(*id));
            match timer.every {
                Some(every) => {
                    let next = timer.due + every;
                    // A program that was busy for several periods gets one
                    // tick, not a burst.
                    timer.due = if next <= now { now + every } else { next };
                }
                None => done.push(*id),
            }
        }
        for id in done {
            let _ = self.timers.remove(&id);
        }
    }

    pub(crate) fn watch_signals(&mut self, signals: &[i32]) -> std::io::Result<()> {
        for signal in signals {
            if !self.blocked.contains(signal) {
                self.blocked.push(*signal);
            }
        }
        let set = signal_set(&self.blocked);
        #[expect(
            unsafe_code,
            reason = "AUDIT: pthread_sigmask with a set built by sigaddset on a local, and no old set wanted"
        )]
        // SAFETY: `set` is an initialised sigset_t on the stack.
        let masked = unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, core::ptr::null_mut())
        };
        if masked != 0 {
            return Err(std::io::Error::from_raw_os_error(masked));
        }
        let existing = self.signalfd.as_ref().map_or(-1, |fd| fd.as_raw_fd());
        #[expect(
            unsafe_code,
            reason = "AUDIT: signalfd4 is not in std; the set is a local and the descriptor is ours or -1"
        )]
        // SAFETY: as the reason says.
        let fd = unsafe {
            libc::signalfd(
                existing,
                &raw const set,
                libc::SFD_NONBLOCK | libc::SFD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if existing < 0 {
            #[expect(
                unsafe_code,
                reason = "AUDIT: signalfd just returned this new descriptor and nothing else holds it"
            )]
            // SAFETY: as the reason says.
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            self.signalfd = Some(owned);
        }
        Ok(())
    }

    pub(crate) fn watch_fd(&mut self, fd: i32) -> WatchId {
        self.next_watch = self.next_watch.wrapping_add(1);
        let id = WatchId(self.next_watch);
        let _ = self.watches.insert(id, fd);
        id
    }

    pub(crate) fn unwatch_fd(&mut self, watch: WatchId) {
        let _ = self.watches.remove(&watch);
    }

    pub(crate) fn waker(&mut self) -> std::io::Result<Waker> {
        if let Some(fd) = &self.waker {
            return Ok(Waker { fd: Arc::clone(fd) });
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: eventfd is not in std; the flags are constants"
        )]
        // SAFETY: no pointers are involved.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        #[expect(
            unsafe_code,
            reason = "AUDIT: eventfd just returned this new descriptor and nothing else holds it"
        )]
        // SAFETY: as the reason says.
        let owned = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        self.waker = Some(Arc::clone(&owned));
        Ok(Waker { fd: owned })
    }

    /// Wait for any source, at most `wait`.
    pub(crate) fn poll(
        &self,
        wayland: i32,
        wants_write: bool,
        children: &Children,
        wait: Option<Duration>,
    ) -> std::io::Result<Ready> {
        #[derive(Clone, Copy)]
        enum Source {
            Wayland,
            Child(ChildId),
            Signals,
            Watch(WatchId),
            Waker,
        }
        let mut fds = Vec::new();
        let mut sources = Vec::new();
        let mut add = |fd: i32, events: i16, source: Source| {
            fds.push(libc::pollfd {
                fd,
                events,
                revents: 0,
            });
            sources.push(source);
        };
        let write = if wants_write { libc::POLLOUT } else { 0 };
        add(wayland, libc::POLLIN | write, Source::Wayland);
        for (id, fd) in children.readable_fds() {
            add(fd, libc::POLLIN, Source::Child(id));
        }
        if let Some(fd) = &self.signalfd {
            add(fd.as_raw_fd(), libc::POLLIN, Source::Signals);
        }
        for (id, fd) in &self.watches {
            add(*fd, libc::POLLIN, Source::Watch(*id));
        }
        if let Some(fd) = &self.waker {
            add(fd.as_raw_fd(), libc::POLLIN, Source::Waker);
        }
        let timeout = wait.map_or(-1, |wait| {
            // Rounded up: a timer due in half a millisecond must not spin.
            let millis = wait.as_micros().div_ceil(1000);
            i32::try_from(millis).unwrap_or(i32::MAX)
        });
        let count = libc::nfds_t::try_from(fds.len()).unwrap_or(0);
        #[expect(
            unsafe_code,
            reason = "AUDIT: poll over a Vec of pollfd whose length is passed beside it"
        )]
        // SAFETY: `fds` is a live, initialised array of `count` entries.
        let polled = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) };
        let mut ready = Ready::default();
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(ready);
            }
            return Err(error);
        }
        for (fd, source) in fds.iter().zip(sources) {
            if fd.revents == 0 {
                continue;
            }
            match source {
                Source::Wayland => ready.wayland = true,
                Source::Child(id) => ready.children.push(id),
                Source::Signals => ready.signals = true,
                Source::Watch(id) => ready.watches.push(id),
                Source::Waker => ready.waker = true,
            }
        }
        Ok(ready)
    }

    /// Turn what was ready (besides the socket and the children) into events.
    pub(crate) fn collect(&mut self, ready: &Ready, pending: &mut Vec<Event>) {
        if ready.signals
            && let Some(fd) = &self.signalfd
        {
            loop {
                #[expect(
                    unsafe_code,
                    reason = "AUDIT: signalfd_siginfo is plain integers; all zeroes is a valid value"
                )]
                // SAFETY: as the reason says.
                let mut info: libc::signalfd_siginfo = unsafe { core::mem::zeroed() };
                let size = size_of::<libc::signalfd_siginfo>();
                #[expect(
                    unsafe_code,
                    reason = "AUDIT: read into a local signalfd_siginfo of exactly its size, from our own signalfd"
                )]
                // SAFETY: as the reason says.
                let read = unsafe { libc::read(fd.as_raw_fd(), (&raw mut info).cast(), size) };
                if usize::try_from(read).ok() != Some(size) {
                    break;
                }
                pending.push(Event::Signal(i32::try_from(info.ssi_signo).unwrap_or(0)));
            }
        }
        for id in &ready.watches {
            pending.push(Event::Readable(*id));
        }
        if ready.waker
            && let Some(fd) = &self.waker
        {
            let mut count = 0u64;
            #[expect(
                unsafe_code,
                reason = "AUDIT: read of eight bytes into a local u64 from our own eventfd"
            )]
            // SAFETY: as the reason says.
            let _ =
                unsafe { libc::read(fd.as_raw_fd(), (&raw mut count).cast(), size_of::<u64>()) };
            pending.push(Event::Woken);
        }
    }
}

/// A set holding `signals`.
pub(crate) fn signal_set(signals: &[i32]) -> libc::sigset_t {
    #[expect(
        unsafe_code,
        reason = "AUDIT: sigset_t is plain data; sigemptyset initialises it next"
    )]
    // SAFETY: as the reason says.
    let mut set: libc::sigset_t = unsafe { core::mem::zeroed() };
    #[expect(unsafe_code, reason = "AUDIT: sigemptyset on a local sigset_t")]
    // SAFETY: `set` is a local the call writes into.
    let _ = unsafe { libc::sigemptyset(&raw mut set) };
    for signal in signals {
        #[expect(unsafe_code, reason = "AUDIT: sigaddset on a local sigset_t")]
        // SAFETY: `set` is a local the call writes into.
        let _ = unsafe { libc::sigaddset(&raw mut set, *signal) };
    }
    set
}

/// What a child's process must do between `fork` and `exec`: forget the
/// signals this program blocked for its signalfd (a blocked mask is
/// inherited, and a script that could not be sent `SIGTERM` would outlive
/// everything), and, if `pdeathsig`, go when this program does.
pub(crate) fn prepare_child(command: &mut std::process::Command, pdeathsig: bool) {
    let hook = move || {
        let empty = signal_set(&[]);
        #[expect(
            unsafe_code,
            reason = "AUDIT: pthread_sigmask in the child after fork is async-signal-safe and reads a local"
        )]
        // SAFETY: as the reason says.
        let _ = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &raw const empty, core::ptr::null_mut())
        };
        if pdeathsig {
            // Refused where the kernel lacks it; the child then only
            // outlives this program, which is upstream's behaviour too.
            #[expect(
                unsafe_code,
                reason = "AUDIT: prctl(PR_SET_PDEATHSIG) takes two integers and is async-signal-safe"
            )]
            // SAFETY: no memory is involved.
            let _ = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) };
        }
        Ok(())
    };
    #[expect(
        unsafe_code,
        reason = "AUDIT: pre_exec runs the hook in the forked child; it only calls async-signal-safe functions"
    )]
    // SAFETY: the hook allocates nothing and takes no lock.
    unsafe {
        let _ = command.pre_exec(hook);
    }
}

/// One child.
#[derive(Debug)]
struct Running {
    child: Child,
    stdout: Option<ChildStdout>,
    output: ChildOutput,
    buffer: Vec<u8>,
}

/// The children [`crate::Client::run`] started.
#[derive(Debug, Default)]
pub(crate) struct Children {
    running: BTreeMap<ChildId, Running>,
    next: u32,
}

impl Children {
    pub(crate) fn run(&mut self, command: &Command) -> std::io::Result<ChildId> {
        let mut process = std::process::Command::new("/bin/sh");
        let _ = process
            .arg("-c")
            .arg(&command.line)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0);
        for (name, value) in &command.env {
            let _ = process.env(name, value);
        }
        prepare_child(&mut process, true);
        let mut child = process.spawn()?;
        let stdout = child.stdout.take();
        if let Some(stdout) = &stdout {
            set_nonblocking(stdout.as_raw_fd());
        }
        self.next = self.next.wrapping_add(1);
        let id = ChildId(self.next);
        let _ = self.running.insert(
            id,
            Running {
                child,
                stdout,
                output: command.output,
                buffer: Vec::new(),
            },
        );
        Ok(id)
    }

    pub(crate) fn kill(&mut self, id: ChildId) {
        if let Some(running) = self.running.get(&id)
            && let Ok(pid) = i32::try_from(running.child.id())
        {
            #[expect(
                unsafe_code,
                reason = "AUDIT: kill of the process group this child leads; a gone group is ESRCH"
            )]
            // SAFETY: no memory is involved.
            let _ = unsafe { libc::kill(-pid, libc::SIGTERM) };
        }
    }

    pub(crate) fn kill_all(&mut self) {
        let ids: Vec<ChildId> = self.running.keys().copied().collect();
        for id in ids {
            self.kill(id);
        }
    }

    /// Whether a child's output has ended and it has not been reaped yet,
    /// which the loop looks at again shortly rather than waiting for ever.
    pub(crate) fn reaping(&self) -> bool {
        self.running
            .values()
            .any(|running| running.stdout.is_none())
    }

    fn readable_fds(&self) -> Vec<(ChildId, i32)> {
        self.running
            .iter()
            .filter_map(|(id, running)| running.stdout.as_ref().map(|out| (*id, out.as_raw_fd())))
            .collect()
    }

    /// Read what the ready children wrote, and reap the ones that are done.
    pub(crate) fn collect(&mut self, ready: &[ChildId], pending: &mut Vec<Event>) {
        for id in ready {
            let Some(running) = self.running.get_mut(id) else {
                continue;
            };
            let mut chunk = [0u8; 4096];
            while let Some(stdout) = running.stdout.as_mut() {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        running.stdout = None;
                        break;
                    }
                    Ok(count) => {
                        running
                            .buffer
                            .extend_from_slice(chunk.get(..count).unwrap_or(&[]));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        running.stdout = None;
                        break;
                    }
                }
            }
            if running.output == ChildOutput::Lines {
                while let Some(end) = running.buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = running.buffer.drain(..=end).collect();
                    let text = String::from_utf8_lossy(line.get(..end).unwrap_or(&[]));
                    pending.push(Event::ChildLine {
                        child: *id,
                        line: text.trim_end_matches('\r').to_owned(),
                    });
                }
            }
        }
        let mut reaped = Vec::new();
        for (id, running) in &mut self.running {
            if running.stdout.is_some() {
                continue;
            }
            if let Ok(Some(status)) = running.child.try_wait() {
                let output = String::from_utf8_lossy(&running.buffer).into_owned();
                pending.push(Event::ChildExited {
                    child: *id,
                    status: status.code(),
                    output,
                });
                reaped.push(*id);
            }
        }
        for id in reaped {
            let _ = self.running.remove(&id);
        }
    }
}

fn set_nonblocking(fd: i32) {
    #[expect(
        unsafe_code,
        reason = "AUDIT: fcntl F_GETFL on a pipe this program owns"
    )]
    // SAFETY: no memory is involved.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        #[expect(
            unsafe_code,
            reason = "AUDIT: fcntl F_SETFL on a pipe this program owns"
        )]
        // SAFETY: no memory is involved.
        let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
}
