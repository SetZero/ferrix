//! Ferrix as a guest of the phone's own crosvm, started from here over adb
//! the way the launcher app starts it on the phone, with its console streamed
//! to the window line by line.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{phone, runs};

/// Where the guest's image and control socket are on the phone.
const VM_DIR: &str = "/data/local/tmp/ferrix-vm";
const CROSVM: &str = "/apex/com.android.virt/bin/crosvm";

/// One console line, and when it arrived.
#[derive(Clone, Serialize)]
struct Line {
    t: f64,
    line: String,
}

/// How a guest ended.
#[derive(Clone, Serialize)]
struct Ended {
    status: Option<i32>,
    seconds: f64,
    record: Option<String>,
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Start a guest with `cpus` vCPUs and `memory` MiB. Its lines go to the
/// window as `vm-line`, and its end as `vm-ended`, with the console kept as a
/// run record.
///
/// With `stats`, the guest's pid 1 is Ferrix's stat service,
/// `/sbin/ferrix-statd`, for that many seconds, 0 for until stopped: crosvm's
/// `-p` puts the options in the guest's `bootargs`, and the loader hands
/// every `ferrix.*` one to the kernel.
pub fn start(
    app: AppHandle,
    running: Arc<Mutex<Option<Child>>>,
    cpus: u32,
    memory: u32,
    stats: Option<u32>,
) -> Result<(), String> {
    let mut slot = running.lock().map_err(|_| "poisoned")?;
    if slot.is_some() {
        return Err("a guest is already running".into());
    }
    let command = format!(
        "cd {VM_DIR} && [ -f ferrix.Image ] || {{ echo 'FERRIX-VM no image in {VM_DIR}: plug the phone in with the helper running'; exit 3; }}; \
         rm -f crosvm.sock; exec {CROSVM} run --disable-sandbox -m {memory} --cpus {cpus} {params}\
         -s {VM_DIR}/crosvm.sock --serial type=stdout,num=1 ferrix.Image 2>/dev/null",
        params = match stats {
            Some(seconds) => format!(
                "-p ferrix.init=/sbin/ferrix-statd -p ferrix.statd.seconds={seconds} "
            ),
            None => String::new(),
        }
    );
    let mut child = Command::new("adb")
        .arg("-s")
        .arg(phone::serial())
        .arg("shell")
        .arg(format!("su -c \"{command}\""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not run adb: {error}"))?;
    let stdout = child.stdout.take().ok_or("no output from adb")?;
    *slot = Some(child);
    drop(slot);

    thread::spawn(move || {
        let began = Instant::now();
        let mut console = String::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let line = line.trim_end_matches('\r').to_string();
            console.push_str(&line);
            console.push('\n');
            let _ = app.emit("vm-line", Line { t: now(), line });
        }
        let status = running
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .and_then(|mut child| child.wait().ok())
            .and_then(|status| status.code());
        let record = runs::save_vm(&console).ok();
        let _ = app.emit(
            "vm-ended",
            Ended { status, seconds: began.elapsed().as_secs_f64(), record },
        );
    });
    Ok(())
}

/// Tell the running guest's crosvm to `stop`, `suspend` or `resume`.
pub fn control(command: &str) -> Result<(), String> {
    if !matches!(command, "stop" | "suspend" | "resume") {
        return Err(format!("no such command: {command}"));
    }
    let status = Command::new("adb")
        .arg("-s")
        .arg(phone::serial())
        .arg("shell")
        .arg(format!("su -c '{CROSVM} {command} {VM_DIR}/crosvm.sock'"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("crosvm {command} failed")) }
}
