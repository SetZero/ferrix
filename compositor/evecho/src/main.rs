//! Say what every input device is, then say everything it sends.
//!
//! The program `xtask test-input` runs as init: it opens every
//! `/dev/input/eventN`, prints a line naming each and the event types it
//! reports, prints `evecho: ready`, and then prints one line per event for
//! as long as it runs. QEMU's `input-send-event` puts events in at the far
//! end, so a line here is the whole path -- QEMU, the virtio-input device,
//! `user/input`, the kernel's input core, the evdev node -- proven at once.
//!
//! It reports and carries on wherever it can: a device that cannot be opened
//! is one device, not the end of the program, because a machine with a
//! keyboard and a broken tablet should still echo the keyboard.

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

/// Only Linux, and Ferrix through its Linux ABI, have `/dev/input`;
/// elsewhere the crate builds so its tables are tested on any host.
#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Write as _;
    use std::path::PathBuf;

    use compositor_evecho::{Device, event_nodes, init, line};
    use ferrix_linux_abi::input::Event;
    #[cfg(feature = "negative-control")]
    use ferrix_linux_abi::input::{EV_KEY, KEY_RESERVED};

    /// What the test waits for before it sends anything.
    const READY: &str = "evecho: ready";

    /// How long a poll waits before looking again, in milliseconds. Nothing
    /// depends on the timeout -- the poll wakes on an event -- but a finite
    /// one lets the program notice a device that has gone.
    const POLL_MILLIS: libc::c_int = 1000;

    pub(crate) fn run() {
        let mut out = std::io::stdout();
        let paths = match arguments() {
            Some(paths) => paths,
            None => match event_nodes() {
                Ok(paths) => paths,
                Err(error) => {
                    say(&mut out, &format!("evecho: no /dev/input: {error}"));
                    return;
                }
            },
        };

        let mut devices = Vec::new();
        for path in paths {
            match Device::open(&path) {
                Ok(device) => {
                    let node = node_name(device.path());
                    say(
                        &mut out,
                        &format!("evecho: {node} {}", device.description().line()),
                    );
                    // A compositor takes its devices, so that a key it acts
                    // on does not also reach whatever else is reading. A
                    // refusal is worth saying and not worth stopping for.
                    if let Err(error) = device.grab(true) {
                        say(&mut out, &format!("evecho: {node} not grabbed: {error}"));
                    }
                    devices.push((node, device));
                }
                Err(error) => say(
                    &mut out,
                    &format!("evecho: {} failed: {error}", path.display()),
                ),
            }
        }
        say(&mut out, &format!("{READY} {} devices", devices.len()));

        echo(&mut out, &devices);
    }

    /// Print every event, for as long as there is a device to read.
    fn echo(out: &mut std::io::Stdout, devices: &[(String, Device)]) {
        let mut events = Vec::new();
        loop {
            let mut waits: Vec<libc::pollfd> = devices
                .iter()
                .map(|(_, device)| libc::pollfd {
                    fd: device.raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            if waits.is_empty() {
                // Nothing to read, and as init the program may not exit.
                // SAFETY: pause has no preconditions.
                let _ = unsafe { libc::pause() };
                continue;
            }
            let count = waits.len();
            // SAFETY: `waits` is a live array of exactly `count` entries.
            let ready =
                unsafe { libc::poll(waits.as_mut_ptr(), count as libc::nfds_t, POLL_MILLIS) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                say(out, &format!("evecho: poll failed: {error}"));
                return;
            }
            for (wait, (node, device)) in waits.iter().zip(devices) {
                if wait.revents & libc::POLLIN != 0 {
                    drain(out, node, device, &mut events);
                }
            }
        }
    }

    /// Read what one device has and print it, reusing `events` so that a
    /// program echoing a tablet does not allocate per report.
    fn drain(out: &mut std::io::Stdout, node: &str, device: &Device, events: &mut Vec<Event>) {
        events.clear();
        if let Err(error) = device.read_events(events) {
            say(out, &format!("evecho: {node} read failed: {error}"));
            return;
        }
        for event in events {
            say(out, &report(node, *event));
        }
    }

    /// One event's line.
    #[cfg(not(feature = "negative-control"))]
    fn report(node: &str, event: Event) -> String {
        line(node, event.r#type, event.code, event.value)
    }

    /// One event's line, with every key called `KEY_RESERVED`: the negative
    /// control, which shows `xtask test-input` reads the events rather than
    /// the shape of the output.
    #[cfg(feature = "negative-control")]
    fn report(node: &str, event: Event) -> String {
        let code = if event.r#type == EV_KEY {
            KEY_RESERVED
        } else {
            event.code
        };
        line(node, event.r#type, code, event.value)
    }

    /// The paths named on the command line, or `None` for all of them.
    ///
    /// Through [`init::unshell`], because as init the program is handed a
    /// shell's arguments.
    fn arguments() -> Option<Vec<PathBuf>> {
        let given = std::env::args().skip(1).collect();
        let paths: Vec<PathBuf> = init::unshell(given)
            .into_iter()
            .map(PathBuf::from)
            .collect();
        (!paths.is_empty()).then_some(paths)
    }

    /// A node's name without its directory, which is what a line says.
    fn node_name(path: &std::path::Path) -> String {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    fn say(out: &mut std::io::Stdout, text: &str) {
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
}
