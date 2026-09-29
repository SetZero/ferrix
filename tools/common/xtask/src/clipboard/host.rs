//! The host's clipboard for a desktop somebody watches, shared with the
//! guest's over vdagent by xtask itself rather than by QEMU.
//!
//! `qemu-vdagent,clipboard=on` makes QEMU the guest agent's peer, and QEMU in
//! turn hands the clipboard to its UI -- but a GTK window only does that when
//! QEMU was built with `gtk_clipboard`, which is off by default and off in
//! both QEMUs this host has (Ubuntu's 10.2.1 and the hand build:
//! `CONFIG_GTK_CLIPBOARD` undefined). So in a real window the port carried
//! nothing, and copy and paste did not cross.
//!
//! So on a Wayland host the port's far end is a socket this module listens
//! on, as `test-clipboard`'s is, and it speaks vdagent to the guest's agent
//! for the whole run: the host's clipboard comes from `wl-paste --watch`, or
//! on GNOME, which has no protocol for it, from the X11 selection read
//! through GTK ([`watch`]); the guest's goes to `wl-copy`. Without a Wayland session, or without
//! wl-clipboard, QEMU keeps the port, which a VNC viewer's clipboard still
//! reaches (QEMU's VNC server is a clipboard peer whatever its GTK has).
//!
//! The grab serials are kept as QEMU keeps them (`ui/vdagent.c`): a host grab
//! carries the count and then adds one, a guest grab below the count is the
//! loser of a race and dropped, and a guest's capabilities start the count
//! over.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use ferrix_vdagent::chunk::Reassembler;
use ferrix_vdagent::message::{ClipboardType, Message, Selection, Shape, Types};

use super::{HOST_CAPS, send};
use crate::args::Args;
use crate::paths::Arch;

/// The largest clipboard carried either way. The guest's agent reassembles
/// into a buffer of this size, and a host clipboard bigger than it -- an
/// image copied as text, say -- is left where it is rather than cut.
const MOST: usize = 1024 * 1024;

/// How long a read of the port waits before the loop looks at the host's
/// clipboard again.
const TICK: Duration = Duration::from_millis(100);

/// The bridge, for as long as the boot runs: dropping it stops the watcher and
/// the thread and removes the socket.
pub(crate) struct Bridge {
    socket: PathBuf,
    watcher: Child,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Bridge {
    /// Where QEMU is to connect the clipboard port.
    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.watcher.kill();
        let _ = self.watcher.wait();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// A bridge for `args`' boot, when it asks for the clipboard, has a port for
/// it, and the host has a Wayland clipboard to bridge to; `None` otherwise,
/// saying why when the reason is the host's.
pub(crate) fn start(arch: Arch, args: &Args) -> Option<Bridge> {
    if !args.clipboard || arch == Arch::Armv7a || args.clipboard_socket.is_some() {
        return None;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|value| value.is_empty()) {
        println!(
            "  {arch}: no WAYLAND_DISPLAY, so the clipboard is QEMU's own: a VNC viewer's \
             clipboard reaches it, this QEMU's window does not (built without gtk_clipboard)"
        );
        return None;
    }
    match bridge(arch) {
        Ok(bridge) => Some(bridge),
        Err(why) => {
            println!("  {arch}: the host's clipboard is not shared: {why}");
            None
        }
    }
}

fn bridge(arch: Arch) -> Result<Bridge, String> {
    // In the temporary directory: a Unix socket's path must fit in 108 bytes.
    let socket = std::env::temp_dir().join(format!(
        "ferrix-clipboard-host-{}-{arch}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .map_err(|error| format!("binding {}: {error}", socket.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let (tell, told) = mpsc::channel();
    let (watcher, how) = watch(tell)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("clipboard-host".to_owned())
        .spawn(move || serve(&listener, &told, &stopped, &mut WlCopy))
        .map_err(|error| error.to_string())?;
    println!(
        "  {arch}: the clipboard is shared with this host's: {how} to the guest, wl-copy from it"
    );
    Ok(Bridge {
        socket,
        watcher,
        stop,
        thread: Some(thread),
    })
}

/// Start watching the host's selection, each change's text sent on `tell`,
/// and say how it is watched.
///
/// `wl-paste --watch` where the compositor offers wlroots' data-control
/// protocol (wlroots compositors, Hyprland, KDE). GNOME's does not, and there
/// `wl-paste` can only read by taking the keyboard focus for a moment, which
/// a watcher that read on every change would keep doing to whoever is typing.
/// So there the selection is watched as an X11 client sees it through
/// Xwayland -- which Mutter keeps in step with the Wayland one, and which any
/// client may read unfocused -- by [`GTK_WATCHER`], when the host has
/// `PyGObject`.
fn watch(tell: Sender<Vec<u8>>) -> Result<(Child, &'static str), String> {
    let mut watcher = Command::new("wl-paste")
        .args(["--watch", "echo", "changed"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("wl-paste --watch: {error}"))?;
    // A compositor without data-control is refused at once.
    std::thread::sleep(Duration::from_millis(300));
    if matches!(watcher.try_wait(), Ok(None)) {
        let lines = watcher.stdout.take().ok_or("wl-paste has no output")?;
        spawn_reader(move || {
            // One line per change, the first at once for what it holds now;
            // the text is then asked for apart.
            for line in BufReader::new(lines).lines() {
                if line.is_err() {
                    break;
                }
                if let Some(text) = host_text()
                    && tell.send(text).is_err()
                {
                    break;
                }
            }
        })?;
        return Ok((watcher, "wl-paste --watch"));
    }
    if std::env::var_os("DISPLAY").is_none_or(|value| value.is_empty()) {
        return Err(
            "the compositor has no data-control protocol for wl-paste --watch, \
                    and there is no DISPLAY to watch the selection through instead"
                .to_owned(),
        );
    }
    let mut watcher = Command::new("python3")
        .args(["-c", GTK_WATCHER])
        .env("GDK_BACKEND", "x11")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("python3: {error}"))?;
    std::thread::sleep(Duration::from_millis(500));
    if !matches!(watcher.try_wait(), Ok(None)) {
        return Err(
            "the compositor has no data-control protocol for wl-paste --watch, \
                    and python3 with PyGObject's GTK 3 is not here to watch through X11 instead"
                .to_owned(),
        );
    }
    let mut records = BufReader::new(watcher.stdout.take().ok_or("python3 has no output")?);
    spawn_reader(move || {
        // `<length>\n<bytes>` per change.
        let mut header = String::new();
        loop {
            header.clear();
            if records.read_line(&mut header).unwrap_or(0) == 0 {
                break;
            }
            let Ok(len) = header.trim().parse::<usize>() else {
                break;
            };
            let mut text = vec![0_u8; len];
            if records.read_exact(&mut text).is_err() {
                break;
            }
            if !text.is_empty() && len <= MOST && tell.send(text).is_err() {
                break;
            }
        }
    })?;
    Ok((watcher, "the X11 selection, through GTK"))
}

/// Watch the clipboard as GTK 3 sees it, and print each change's text as its
/// length in bytes, a newline, then the bytes: the first at once for what it
/// holds now. Run with `GDK_BACKEND=x11`, where `owner-change` comes from
/// `XFixes` and a read needs no focus.
const GTK_WATCHER: &str = "\
import sys, gi
gi.require_version('Gtk', '3.0')
gi.require_version('Gdk', '3.0')
from gi.repository import Gtk, Gdk
clipboard = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
out = sys.stdout.buffer
def got(_, text):
    if text:
        data = text.encode()
        out.write(b'%d\\n' % len(data) + data)
        out.flush()
clipboard.connect('owner-change', lambda c, _: c.request_text(got))
clipboard.request_text(got)
Gtk.main()
";

fn spawn_reader(read: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name("clipboard-watch".to_owned())
        .spawn(read)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The host's selection as text, if it has text that fits.
fn host_text() -> Option<Vec<u8>> {
    let output = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    (output.status.success() && !output.stdout.is_empty() && output.stdout.len() <= MOST)
        .then_some(output.stdout)
}

/// Where the guest's text goes on the host.
pub(super) trait HostClipboard {
    fn copy(&mut self, text: &[u8]);
}

/// `wl-copy`, which serves the selection from a process of its own.
struct WlCopy;

impl HostClipboard for WlCopy {
    fn copy(&mut self, text: &[u8]) {
        let Ok(mut child) = Command::new("wl-copy")
            .args(["--type", "text/plain;charset=utf-8"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text);
        }
        let _ = child.wait();
    }
}

/// Take QEMU's connection and serve it until `stop`, or until it closes.
fn serve(
    listener: &UnixListener,
    host: &Receiver<Vec<u8>>,
    stop: &AtomicBool,
    clipboard: &mut dyn HostClipboard,
) {
    let stream = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(_) => std::thread::sleep(TICK),
        }
    };
    if stream.set_nonblocking(false).is_err() || stream.set_read_timeout(Some(TICK)).is_err() {
        return;
    }
    let _ended = Session::new(stream).run(host, stop, clipboard);
}

/// One connection's state.
pub(super) struct Session {
    stream: UnixStream,
    shape: Shape,
    /// QEMU's count of grabs (`last_serial`): the next host grab carries it.
    serial: u32,
    /// Whether the guest has said its capabilities, before which no grab is
    /// sent.
    greeted: bool,
    /// The host's text, what a guest request is answered with.
    text: Option<Vec<u8>>,
    /// The last text the guest gave the host, so that `wl-paste --watch`
    /// telling of it is not grabbed back into the guest.
    from_guest: Option<Vec<u8>>,
}

impl Session {
    pub(super) fn new(stream: UnixStream) -> Session {
        Session {
            stream,
            shape: Shape::QEMU_CLIPBOARD,
            serial: 0,
            greeted: false,
            text: None,
            from_guest: None,
        }
    }

    /// Serve the connection until `stop` or its end.
    pub(super) fn run(
        &mut self,
        host: &Receiver<Vec<u8>>,
        stop: &AtomicBool,
        clipboard: &mut dyn HostClipboard,
    ) -> Result<(), String> {
        self.send(&Message::AnnounceCapabilities {
            request: true,
            caps: HOST_CAPS,
        })?;
        let mut buffer = vec![0_u8; 2 * MOST];
        let mut reassembler = Reassembler::new(&mut buffer);
        let mut pending: Vec<u8> = Vec::new();
        let mut read = [0_u8; 4096];
        while !stop.load(Ordering::Relaxed) {
            match self.stream.read(&mut read) {
                Ok(0) => return Ok(()),
                Ok(got) => pending.extend_from_slice(read.get(..got).unwrap_or_default()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(format!("reading: {error}")),
            }
            while !pending.is_empty() {
                let taken = reassembler
                    .feed(&pending)
                    .map_err(|error| format!("the guest's framing: {error:?}"))?;
                if taken == 0 {
                    break;
                }
                let _ = pending.drain(..taken);
                let Some(message) = reassembler.message().map(<[u8]>::to_vec) else {
                    continue;
                };
                reassembler.take();
                self.answer(&message, clipboard)?;
            }
            while let Ok(text) = host.try_recv() {
                self.host_copied(text)?;
            }
        }
        Ok(())
    }

    /// The host's selection changed to `text`.
    fn host_copied(&mut self, text: Vec<u8>) -> Result<(), String> {
        if self.from_guest.as_ref() == Some(&text) || self.text.as_ref() == Some(&text) {
            return Ok(());
        }
        self.text = Some(text);
        self.grab()
    }

    /// Tell the guest the host holds text, if it is listening yet.
    fn grab(&mut self) -> Result<(), String> {
        if !self.greeted || self.text.is_none() {
            return Ok(());
        }
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1);
        let types = Types::new(&[ClipboardType::Utf8Text]).map_err(|error| format!("{error:?}"))?;
        self.send(&Message::ClipboardGrab {
            selection: Selection::Clipboard,
            serial: self.shape.serial.then_some(serial),
            types,
        })
    }

    fn answer(&mut self, message: &[u8], clipboard: &mut dyn HostClipboard) -> Result<(), String> {
        let Ok(message) = Message::decode(message, self.shape) else {
            return Ok(());
        };
        match message {
            Message::AnnounceCapabilities { request, caps } => {
                self.shape = Shape::from_caps(caps & HOST_CAPS);
                self.serial = 0;
                if request {
                    self.send(&Message::AnnounceCapabilities {
                        request: false,
                        caps: HOST_CAPS,
                    })?;
                }
                self.greeted = true;
                self.grab()?;
            }
            Message::ClipboardRequest {
                selection: Selection::Clipboard,
                kind: ClipboardType::Utf8Text,
            } => {
                let text = self.text.clone().unwrap_or_default();
                self.send(&Message::Clipboard {
                    selection: Selection::Clipboard,
                    kind: ClipboardType::Utf8Text,
                    data: &text,
                })?;
            }
            Message::ClipboardGrab {
                selection: Selection::Clipboard,
                serial,
                types,
            } => {
                if let Some(serial) = serial {
                    if serial < self.serial {
                        return Ok(());
                    }
                    self.serial = serial;
                }
                if types.holds(ClipboardType::Utf8Text) {
                    self.send(&Message::ClipboardRequest {
                        selection: Selection::Clipboard,
                        kind: ClipboardType::Utf8Text,
                    })?;
                }
            }
            Message::Clipboard {
                selection: Selection::Clipboard,
                kind: ClipboardType::Utf8Text,
                data,
            } if data.len() <= MOST => {
                clipboard.copy(data);
                self.from_guest = Some(data.to_vec());
                self.text = Some(data.to_vec());
            }
            _ => {}
        }
        Ok(())
    }

    fn send(&mut self, message: &Message<'_>) -> Result<(), String> {
        send(&mut self.stream, self.shape, message)
    }
}

#[cfg(test)]
mod tests;
