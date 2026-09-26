//! `adbd`: the device end of the Android Debug Bridge, for Ferrix.
//!
//! `docs/ADB.md` is the design. This is its first step: adb over TCP, which
//! the host reaches with `adb connect <address>:<port>`. It offers the
//! host's everyday commands:
//!
//! * `adb shell <command>` and an interactive `adb shell`, on a
//!   pseudo-terminal;
//! * `adb push` and `adb pull`, through the first sync protocol;
//! * `adb reboot`;
//! * `adb forward tcp:<host> tcp:<device>`, a stream to a port in Ferrix.
//!
//! What the wire looks like is `libs/proto/adb`, tested on the host; what is
//! here is threads, files, processes and sockets.
//!
//! # Who may connect
//!
//! Anyone who reaches the port gets a root shell, as on a debug Android
//! build: there is no authentication yet (`docs/ADB.md` §5). So nothing
//! starts adbd by default. An image carries it only when built with
//! `--adbd`, and it runs only when something runs it; under QEMU the port
//! is reachable only through a `--forward` on the host's loopback.
//!
//! # Usage
//!
//! `adbd [--port N] [--usb PATH] [--test]`. `--test` makes `reboot:` end
//! adbd with status 0 rather than restart the machine, which is how `xtask
//! test-adb` ends its run. `--usb PATH` also serves adb over USB: `PATH` is
//! the socket `usbdev` bridges its adb interface to (`/tmp/adbd-usb`), and
//! adbd connects to it again whenever it is dropped.
//!
//! Started as pid 1 (`ferrix.init=/bin/adbd`, as on the phone), it serves
//! TCP and USB both, reaps what is orphaned to it, and never exits.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ferrix_adb::message::{
    self, CLSE, CNXN, HEADER_BYTES, Header, MAX_PAYLOAD, OKAY, OPEN, VERSION, WRTE,
};

mod services;

/// The port adb over TCP uses unless told otherwise.
const DEFAULT_PORT: u16 = 5555;

/// The socket `usbdev` bridges adb's USB interface to.
const USB_SOCKET: &str = "/tmp/adbd-usb";

/// The largest payload over USB: `usbdev`'s ring for one transfer
/// (`native/drivers/usbdev/src/adb.rs`).
const USB_MAX_PAYLOAD: u32 = 4096;

/// How the program was asked to run.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    port: u16,
    /// Serve adb over USB through `usbdev`'s socket.
    usb: bool,
    /// `reboot:` ends adbd rather than restarting the machine.
    pub(crate) test: bool,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        port: DEFAULT_PORT,
        usb: std::process::id() == 1,
        test: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                options.port = args
                    .next()
                    .and_then(|port| port.parse().ok())
                    .ok_or("--port needs a port number")?;
            }
            "--test" => options.test = true,
            "--usb" => options.usb = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(options)
}

fn main() {
    let options = match options() {
        Ok(options) => options,
        Err(why) => {
            eprintln!("adbd: {why}; usage: adbd [--port N] [--usb] [--test]");
            std::process::exit(2);
        }
    };
    if std::process::id() == 1 {
        let _reaping = thread::spawn(reap);
    }
    if options.usb {
        let _usb = thread::spawn(move || usb(options));
    }
    tcp(options);
    // Pid 1 may not end: with no TCP, the USB thread carries on alone.
    loop {
        thread::park();
    }
}

/// Listen on TCP and serve each host that connects.
fn tcp(options: Options) {
    let listener = match TcpListener::bind(("0.0.0.0", options.port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("adbd: cannot listen on port {}: {error}", options.port);
            if std::process::id() != 1 {
                std::process::exit(1);
            }
            return;
        }
    };
    println!("adbd: listening on 0.0.0.0:{}", options.port);
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let peer = stream
                    .peer_addr()
                    .map_or_else(|_| String::from("?"), |address| address.to_string());
                println!("adbd: a host connected from {peer}");
                let _ = stream.set_nodelay(true);
                let Ok(reading) = stream.try_clone() else {
                    continue;
                };
                let _serving = thread::spawn(move || {
                    if let Err(error) = serve(reading, stream, options, MAX_PAYLOAD) {
                        println!("adbd: the connection from {peer} ended: {error}");
                    }
                });
            }
            Err(error) => eprintln!("adbd: accept: {error}"),
        }
    }
}

/// Serve adb over USB: connect to `usbdev`'s socket, and again whenever it
/// is dropped or not there yet.
fn usb(options: Options) {
    let mut said = false;
    loop {
        match UnixStream::connect(USB_SOCKET) {
            Ok(stream) => {
                println!("adbd: serving USB through {USB_SOCKET}");
                said = false;
                if let Ok(reading) = stream.try_clone()
                    && let Err(error) = serve(reading, stream, options, USB_MAX_PAYLOAD)
                {
                    println!("adbd: USB ended: {error}");
                }
            }
            Err(error) if !said => {
                println!("adbd: no USB yet ({USB_SOCKET}: {error}); trying again");
                said = true;
            }
            Err(_) => {}
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// As pid 1: take back every process orphaned to it, so none stays a zombie.
/// A service's own `wait` may lose its child to this, which costs nothing:
/// shell v1 passes back no status.
fn reap() {
    loop {
        // SAFETY: a plain call; a null status pointer is allowed.
        let reaped = unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) };
        if reaped < 0 {
            thread::sleep(Duration::from_secs(1));
        }
    }
}

/// The connection's socket, shared by every stream that writes to it: one
/// whole message at a time.
#[derive(Clone)]
pub(crate) struct Wire {
    socket: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Wire {
    fn send(&self, command: u32, arg0: u32, arg1: u32, data: &[u8]) -> io::Result<()> {
        let bytes = message::message(command, arg0, arg1, data);
        let mut socket = self
            .socket
            .lock()
            .map_err(|_| io::Error::other("the socket's lock is poisoned"))?;
        socket.write_all(&bytes)
    }
}

/// A service's way out: bytes to the host on its stream, one `WRTE` in
/// flight at a time, each waiting for the host's `OKAY`.
pub(crate) struct Outgoing {
    wire: Wire,
    local: u32,
    remote: u32,
    max_payload: usize,
    acknowledged: Receiver<()>,
}

impl Outgoing {
    /// Send `bytes`, as many `WRTE`s as the payload limit needs.
    pub(crate) fn write(&self, bytes: &[u8]) -> io::Result<()> {
        for chunk in bytes.chunks(self.max_payload.max(1)) {
            self.wire.send(WRTE, self.local, self.remote, chunk)?;
            self.acknowledged
                .recv()
                .map_err(|_| io::Error::other("the host closed the stream"))?;
        }
        Ok(())
    }

    /// Close the stream from this end.
    pub(crate) fn close(&self) {
        let _ = self.wire.send(CLSE, self.local, self.remote, &[]);
    }
}

/// What the reader keeps of an open stream.
struct Open {
    remote: u32,
    /// The host's bytes, for the service.
    incoming: Sender<Vec<u8>>,
    /// The host's `OKAY`s, for [`Outgoing::write`].
    acknowledged: Sender<()>,
}

/// Read one whole message: its header and payload.
fn read_message(socket: &mut impl Read, max_payload: u32) -> io::Result<(Header, Vec<u8>)> {
    let mut head = [0; HEADER_BYTES];
    socket.read_exact(&mut head)?;
    let header = Header::decode(&head, max_payload)
        .map_err(|error| io::Error::other(format!("a bad header: {error:?}")))?;
    let mut data = vec![0; header.data_length as usize];
    socket.read_exact(&mut data)?;
    Ok((header, data))
}

/// Serve one host until it goes, reading from `reading` and writing to
/// `writing`, with payloads of at most `limit`.
fn serve(
    mut reading: impl Read,
    writing: impl Write + Send + 'static,
    options: Options,
    limit: u32,
) -> io::Result<()> {
    let wire = Wire {
        socket: Arc::new(Mutex::new(Box::new(writing))),
    };
    let mut max_payload = limit;
    let mut streams: HashMap<u32, Open> = HashMap::new();
    let mut next_local = 1_u32;
    loop {
        let (header, data) = read_message(&mut reading, MAX_PAYLOAD.max(1024 * 1024))?;
        match header.command {
            CNXN => {
                max_payload = header.arg1.clamp(1024, limit);
                let banner = message::banner("ferrix", "ferrix", "ferrix");
                wire.send(CNXN, VERSION, max_payload, banner.as_bytes())?;
            }
            OPEN => {
                let local = next_local;
                next_local = next_local.wrapping_add(1).max(1);
                let name = String::from_utf8_lossy(message::service_name(&data)).into_owned();
                let (incoming, from_host) = mpsc::channel();
                let (acknowledged, acks) = mpsc::channel();
                let outgoing = Outgoing {
                    wire: wire.clone(),
                    local,
                    remote: header.arg0,
                    max_payload: max_payload as usize,
                    acknowledged: acks,
                };
                if services::start(&name, outgoing, from_host, options) {
                    wire.send(OKAY, local, header.arg0, &[])?;
                    let _ = streams.insert(
                        local,
                        Open {
                            remote: header.arg0,
                            incoming,
                            acknowledged,
                        },
                    );
                } else {
                    println!("adbd: no service {name:?}");
                    wire.send(CLSE, 0, header.arg0, &[])?;
                }
            }
            WRTE => {
                if let Some(open) = streams.get(&header.arg1) {
                    // Taken at once: the service's queue is its buffer.
                    let _ = open.incoming.send(data);
                    wire.send(OKAY, header.arg1, open.remote, &[])?;
                }
            }
            OKAY => {
                if let Some(open) = streams.get(&header.arg1) {
                    let _ = open.acknowledged.send(());
                }
            }
            CLSE => {
                // Dropping the senders tells the service the host is gone.
                let _ = streams.remove(&header.arg1);
            }
            _ => {}
        }
    }
}
