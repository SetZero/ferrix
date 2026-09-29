//! Ferrix's own USB serial port, during a native boot: the phone is off adb
//! then, but once Ferrix's `usbdev` is up it presents a CDC-ACM port, which
//! Linux's `cdc_acm` makes a `/dev/ttyACM*`. Its lines go to the window as
//! they arrive, as a guest's do, and the whole of it is kept as a run record.
//!
//! The port is known by its USB identity, not its name: pid.codes' test
//! vendor and product, `1209:0001`, with the product string Ferrix gives it.
//! Any other `ttyACM` on this machine is left alone.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::io::Write;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::runs;

/// The identity `src/lib/drivers/usb/usb-device` gives Ferrix's port.
const VENDOR: &str = "1209";
const PRODUCT: &str = "0001";
const PRODUCT_NAME: &str = "Ferrix console";

/// Linux's `O_NOCTTY`: open a terminal without making it the process's
/// controlling terminal. Without it, a monitor started with `setsid`, as
/// the phone's notes start it, took the port as its controlling terminal,
/// and the phone leaving USB hung it up: `SIGHUP` ended the monitor
/// silently. The same value on every Linux architecture this runs on.
const O_NOCTTY: i32 = 0o400;

/// The line `usbdev` restarts Ferrix on (`src/user/native/drivers/usb/usbdev`), after a
/// newline of its own. The port's line discipline echoes what Ferrix sends
/// back to it until `stty -echo` takes, so usbdev may be holding a piece of
/// its own log as the start of a line. The command must be a whole line,
/// and on the phone (run bootandroid1) it went unrecognised without this.
const REBOOT: &[u8] = b"\nferrix-usbdev: reboot\n";

/// The port being streamed, while there is one.
static PORT: Mutex<Option<String>> = Mutex::new(None);

/// How often to look for the port while it is not there.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// One line from the port, and when it arrived.
#[derive(Clone, Serialize)]
struct Line {
    t: f64,
    line: String,
}

/// The port coming or going.
#[derive(Clone, Serialize)]
struct State {
    /// `/dev/ttyACM*` while it is there, `None` once it went.
    port: Option<String>,
    /// The record kept of it, once it went.
    record: Option<String>,
    seconds: f64,
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Watch for Ferrix's port for as long as the app runs, streaming each one
/// that appears until it goes.
pub fn watch(app: AppHandle) {
    loop {
        if let Some(port) = find() {
            stream(&app, &port);
        }
        thread::sleep(LOOK_EVERY);
    }
}

/// A `ttyACM` whose USB device is Ferrix's, as `/dev/<name>`.
fn find() -> Option<String> {
    let entries = fs::read_dir("/sys/class/tty").ok()?;
    entries.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("ttyACM") {
            return None;
        }
        // `device` is the ACM interface; its parent is the USB device.
        let usb = entry.path().join("device/..");
        let read = |file: &str| fs::read_to_string(usb.join(file)).map(|s| s.trim().to_string()).ok();
        let ours = read("idVendor").as_deref() == Some(VENDOR)
            && read("idProduct").as_deref() == Some(PRODUCT)
            && read("product").as_deref() == Some(PRODUCT_NAME);
        ours.then(|| format!("/dev/{name}"))
    })
}

/// Read `port` line by line until it goes, then keep what came as a record.
fn stream(app: &AppHandle, port: &str) {
    let began = Instant::now();
    // Raw, with no echo: the line discipline would otherwise send the
    // phone's own lines back to it, and cook CRs.
    let _ = Command::new("stty")
        .args(["-F", port, "raw", "-echo"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let Ok(file) = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOCTTY)
        .open(Path::new(port))
    else {
        return;
    };
    if let Ok(mut current) = PORT.lock() {
        *current = Some(port.to_string());
    }
    let _ = app.emit("usb-state", State { port: Some(port.to_string()), record: None, seconds: 0.0 });
    let mut console = String::new();
    for line in BufReader::new(file).split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line).trim_end_matches('\r').to_string();
        console.push_str(&line);
        console.push('\n');
        let _ = app.emit("usb-line", Line { t: now(), line });
    }
    if let Ok(mut current) = PORT.lock() {
        *current = None;
    }
    let record = (!console.is_empty()).then(|| runs::save("usb", &console).ok()).flatten();
    let _ = app.emit("usb-state", State { port: None, record, seconds: began.elapsed().as_secs_f64() });
}

/// Ask Ferrix to restart, over its port: on the phone that is the watchdog
/// reset every run ends with, and Android comes back.
pub fn reboot() -> Result<(), String> {
    let port = PORT
        .lock()
        .map_err(|_| "poisoned")?
        .clone()
        .ok_or("Ferrix's USB port is not up")?;
    let mut file = OpenOptions::new()
        .write(true)
        .custom_flags(O_NOCTTY)
        .open(&port)
        .map_err(|error| format!("{port}: {error}"))?;
    file.write_all(REBOOT).map_err(|error| format!("{port}: {error}"))
}
