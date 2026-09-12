//! Watching a real serial port for the kernel's report.
//!
//! The counterpart to `qemu::test_boot` for hardware. QEMU has no STM32MP1
//! model, so the only way to run this kernel on that board is to put it on an
//! SD card and watch what comes out of the debug UART — and once a human is
//! watching a terminal by eye, the boot test stops being a check and becomes
//! an impression. This makes it a check again: the same two markers, the same
//! verdict, the same exit status.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::qemu::{PANIC_MARKER, SUCCESS_MARKER};
use crate::{Error, Result};

/// The line speed every board this targets uses, and what the STM32MP157-DK's
/// device tree asks for in `stdout-path`: `serial0:115200n8`.
const BAUD: &str = "115200";

/// Where USB-serial adapters and the ST-LINK's virtual port appear.
///
/// The DK's ST-LINK presents a CDC-ACM device, so `ttyACM0` is the usual
/// answer; an FTDI cable on the same header is `ttyUSB0`. Both are searched
/// because which one a given board and cable produce is not worth remembering.
const CANDIDATE_PREFIXES: [&str; 2] = ["ttyACM", "ttyUSB"];

/// Watch `port` until the kernel reports, or the patience runs out.
pub(crate) fn watch(kernel: Option<&Path>, args: &Args) -> Result<()> {
    // `deploy` knows which kernel it just flashed; `watch-serial` on its own
    // is watching whatever is already on the board, and says so by passing
    // none rather than guessing at a build in this tree.
    let symbols = kernel.and_then(crate::symbolize::Symbolizer::open);
    let port = match args.port.as_deref() {
        Some(given) => PathBuf::from(given),
        None => discover()?,
    };

    configure(&port)?;
    println!(
        "  watching {} at {BAUD} baud (timeout {}s, Ctrl-C to stop)",
        port.display(),
        args.timeout
    );

    // Opened read-only and read on a thread, for `qemu::test_boot`'s reason:
    // a board may say nothing for seconds and the deadline is for the boot as
    // a whole. Unlike QEMU there is no child process to reap — the port stays
    // open whether or not anything is driving it, so a timeout here means
    // "nothing arrived", never "the thing exited".
    let file = std::fs::File::open(&port)
        .map_err(|error| Error::new(format!("opening {}: {error}", port.display())))?;

    let (sender, receiver) = mpsc::channel();
    let _reader = std::thread::spawn(move || {
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let log_path = crate::paths::build_dir(crate::paths::Arch::Armv7a).join("board-serial.log");
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut log = std::fs::File::create(&log_path)?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout);

    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(Error::new(format!(
                "nothing said `{SUCCESS_MARKER}` on {} within {}s.\n  \
                 What arrived is in {}.\n  \
                 If the log is empty the board is not talking: check the cable, the \
                 jumper on the boot pins, and that {BAUD} is the right speed.",
                port.display(),
                args.timeout,
                log_path.display()
            )));
        };
        match receiver.recv_timeout(remaining) {
            Ok(line) => {
                println!("    | {line}");
                writeln!(log, "{line}")?;
                log.flush()?;
                if line.contains(SUCCESS_MARKER) {
                    println!("  board: boot ok");
                    return Ok(());
                }
                if line.contains(PANIC_MARKER) {
                    crate::qemu::take_panic_report(&receiver, &mut log, symbols.as_ref())?;
                    return Err(Error::new(format!(
                        "the kernel panicked on the board; see {}",
                        log_path.display()
                    )));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::new(format!(
                    "{} closed while waiting; was it unplugged?",
                    port.display()
                )));
            }
            // The deadline is rechecked at the top of the loop.
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// The one serial port on this machine, if there is exactly one.
///
/// Refuses to choose between several rather than picking the lowest number: a
/// developer with a board and a USB-serial cable plugged in has two, and
/// guessing wrong means watching a port nothing is driving and concluding the
/// board is dead.
fn discover() -> Result<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return Err(Error::new(
            "cannot read /dev to find a serial port; pass --port",
        ));
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if CANDIDATE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            found.push(entry.path());
        }
    }
    found.sort();

    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(Error::new(
            "no serial port found (looked for /dev/ttyACM* and /dev/ttyUSB*).\n  \
             Plug the board's USB debug port in, or pass --port.",
        )),
        several => Err(Error::new(format!(
            "several serial ports are present; say which with --port:\n    {}",
            several
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n    ")
        ))),
    }
}

/// Put the port in raw mode at the right speed.
///
/// Through `stty` rather than `tcsetattr`, because the alternative is either a
/// dependency or a hand-written `termios` binding per platform, and this is a
/// build tool whose dependency list is meant to stay readable. The cost is one
/// process and a Unix-only command, which is the same platform the device node
/// it is configuring only exists on.
fn configure(port: &Path) -> Result<()> {
    if !port.exists() {
        return Err(Error::new(format!(
            "{} does not exist.\n  \
             The board's USB debug port creates it when plugged in; `ls /dev/ttyACM*` \
             after connecting.",
            port.display()
        )));
    }

    if cfg!(not(unix)) {
        println!("  (not Unix: leaving the port's settings alone)");
        return Ok(());
    }

    let Some(stty) = crate::paths::which("stty") else {
        return Err(Error::new(
            "stty is not on PATH, so the port cannot be set up",
        ));
    };

    let output = Command::new(stty)
        .arg("-F")
        .arg(port)
        .args([
            BAUD, "raw", "-echo", "-echoe", "-echok", "-crtscts", "-ixon",
        ])
        .output()
        .map_err(|error| Error::new(format!("running stty: {error}")))?;

    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        let why = why.trim();
        return Err(Error::new(format!(
            "could not configure {}: {why}\n  \
             A permission error here means your user is not in the group owning the \
             port — usually `dialout`: `sudo usermod -aG dialout $USER`, then log in again.",
            port.display()
        )));
    }
    Ok(())
}
