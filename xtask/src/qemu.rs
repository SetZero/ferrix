//! Booting the image under QEMU.
//!
//! `run` attaches the guest's serial port to this terminal. `test-boot` does
//! the same thing headless, watches for the kernel's report, and turns it into
//! an exit status — which makes it the only check in this repository that can
//! tell us the operating system runs, as opposed to compiling.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::args::Args;
use crate::cargo;
use crate::paths::{self, Arch, Firmware};
use crate::{Error, Result};

/// What the kernel prints when it has finished its self-checks.
pub(crate) const SUCCESS_MARKER: &str = "FERRIX-BOOT-OK";
/// What the panic handler prints. Seeing this ends the test immediately: the
/// kernel will not recover, and waiting out the timeout only hides the reason.
pub(crate) const PANIC_MARKER: &str = "FERRIX-PANIC";

/// The exit status QEMU reports when the x86-64 kernel writes 0x10 to the
/// `isa-debug-exit` port: `(value << 1) | 1`.
const DEBUG_EXIT_SUCCESS: i32 = 33;

/// Boot the image with the serial port attached to this terminal.
pub(crate) fn run(arch: Arch, image: &Path, args: &Args) -> Result<()> {
    let mut command = qemu_command(arch, image, args)?;
    if args.gdb {
        let _ = command.args(["-s", "-S"]);
        println!("  waiting for a debugger on localhost:1234");
    }
    println!("  {arch}: booting (quit with Ctrl-A x)\n");
    cargo::run(command, "qemu")
}

/// Boot the image headless and require the kernel to report success.
pub(crate) fn test_boot(arch: Arch, image: &Path, args: &Args) -> Result<()> {
    let mut command = qemu_command(arch, image, args)?;
    let _ = command.stdout(Stdio::piped()).stderr(Stdio::inherit());

    println!("  {arch}: booting under QEMU (timeout {}s)", args.timeout);
    let mut child = command
        .spawn()
        .map_err(|error| Error::new(format!("could not start QEMU: {error}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new("QEMU produced no stdout to read"))?;

    // A reader thread and a channel, rather than a non-blocking read: the guest
    // may say nothing for seconds at a time, and the timeout has to apply to
    // the boot as a whole rather than to each line.
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let log_path = paths::build_dir(arch).join("serial.log");
    let mut log = std::fs::File::create(&log_path)?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let mut outcome = None;

    while outcome.is_none() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        match receiver.recv_timeout(remaining) {
            Ok(line) => {
                println!("    | {line}");
                writeln!(log, "{line}")?;
                if line.contains(SUCCESS_MARKER) {
                    outcome = Some(Ok(()));
                } else if line.contains(PANIC_MARKER) {
                    outcome = Some(Err(Error::new(format!(
                        "the {arch} kernel panicked during boot; see {}",
                        log_path.display()
                    ))));
                }
            }
            // The guest closed the serial port: QEMU is on its way out, so stop
            // reading and judge on the exit status below.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => break,
        }
    }

    let status = finish(&mut child, outcome.is_some())?;
    drop(receiver);
    let _ = reader.join();
    log.flush()?;

    match outcome {
        Some(Ok(())) => {
            let code = status.code().unwrap_or(0);
            if code != 0 && code != DEBUG_EXIT_SUCCESS {
                return Err(Error::new(format!(
                    "the {arch} kernel reported success but QEMU exited {code}"
                )));
            }
            println!("  {arch}: boot ok");
            Ok(())
        }
        Some(Err(error)) => Err(error),
        None => Err(Error::new(format!(
            "the {arch} kernel never printed `{SUCCESS_MARKER}` within {}s.\n  \
             Serial output is in {}",
            args.timeout,
            log_path.display()
        ))),
    }
}

/// Wait for QEMU to exit, killing it if the boot already reached a verdict.
fn finish(child: &mut std::process::Child, decided: bool) -> Result<std::process::ExitStatus> {
    if decided {
        // Give the guest a moment to shut itself down cleanly, so a working
        // power-off path is exercised rather than always being papered over.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.kill();
    Ok(child.wait()?)
}

/// Assemble the QEMU command line for `arch`.
fn qemu_command(arch: Arch, image: &Path, args: &Args) -> Result<Command> {
    let binary = paths::which(arch.qemu_binary()).ok_or_else(|| {
        Error::new(format!(
            "{} is not on PATH.\n  Install QEMU (Debian/Ubuntu: `qemu-system-x86` and \
             `qemu-system-arm`; Windows: `winget install SoftwareFreedomConservancy.QEMU`).",
            arch.qemu_binary()
        ))
    })?;

    let firmware = paths::find_firmware(arch)?;
    let vars = prepare_vars(arch, &firmware)?;

    let mut command = Command::new(binary);
    let _ = command.current_dir(paths::workspace_root());

    let _ = command.args([
        "-m",
        &args.memory.to_string(),
        "-smp",
        &args.smp.to_string(),
        // A guest that reboots on a triple fault turns a crash into an endless
        // loop, which in CI is a timeout with no cause in the log.
        "-no-reboot",
        "-display",
        "none",
        "-monitor",
        "none",
        "-serial",
        "stdio",
        // No network device. QEMU adds one by default, and on AArch64 that
        // means firmware finds a PCI option ROM built for x86 and says so:
        //
        //     Image type X64 can't be loaded on AARCH64 UEFI system.
        //
        // Which is alarming, unrelated to us, and exactly the kind of noise
        // that trains people to skim the boot log. There is no network driver
        // to exercise until stage 10; this comes back with one.
        "-net",
        "none",
    ]);

    match arch {
        Arch::X86_64 => {
            let _ = command.args([
                "-machine",
                "q35",
                // SMEP and SMAP are the two features the kernel relies on to
                // keep ring 0 out of user pages, so emulate a CPU that has them.
                "-cpu",
                "qemu64,+pdpe1gb,+smep,+smap",
                // A controlled way for the guest to end the test: writing 0x10
                // to port 0xF4 exits QEMU with status 33.
                "-device",
                "isa-debug-exit,iobase=0xf4,iosize=0x04",
            ]);
            let _ = command.args(["-drive", &format!("format=raw,file={}", display(image))]);
        }
        Arch::AArch64 => {
            let _ = command.args([
                "-machine",
                "virt",
                "-cpu",
                "cortex-a72",
                "-drive",
                &format!("format=raw,file={},if=none,id=disk", display(image)),
                "-device",
                "virtio-blk-device,drive=disk",
            ]);
        }
    }

    let _ = command.args([
        "-drive",
        &format!(
            "if=pflash,format=raw,readonly=on,file={}",
            display(&firmware.code)
        ),
    ]);
    let _ = command.args([
        "-drive",
        &format!("if=pflash,format=raw,file={}", display(&vars)),
    ]);

    Ok(command)
}

/// A path as QEMU wants it: forward slashes, even on Windows, because a
/// backslash inside a `-drive` argument is taken as an escape.
fn display(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Produce the writable UEFI variable store QEMU needs beside the firmware.
///
/// It has to be writable and it has to survive between runs, so it is copied
/// into `build/` rather than used from the read-only system location. On
/// AArch64 the `virt` machine additionally requires both pflash images to be
/// exactly the same size, which is why this pads.
fn prepare_vars(arch: Arch, firmware: &Firmware) -> Result<PathBuf> {
    let directory = paths::build_dir(arch);
    std::fs::create_dir_all(&directory)?;
    let target = directory.join("uefi-vars.fd");

    // **Always rewritten, never reused.** UEFI variables persist across boots by
    // design -- that is what they are for -- and firmware records its boot
    // options in them. A store carried over from a previous run can therefore
    // hold entries describing an image that is no longer there, and the symptom
    // is firmware skipping our disk entirely and dropping to the EFI shell,
    // with nothing in the log to say why.
    //
    // That is not hypothetical: it happened here, after a run that deliberately
    // booted the wrong architecture's image to isolate a firmware message. A
    // boot test whose result depends on what the previous boot test left behind
    // is not a test, so this starts from a known state every time.

    let mut contents = match &firmware.vars {
        Some(source) => std::fs::read(source)
            .map_err(|error| Error::new(format!("reading {}: {error}", source.display())))?,
        // No template: an all-zero store is not a valid variable store, and
        // EDK2 responds by formatting one, which is what we want anyway.
        None => Vec::new(),
    };

    if arch == Arch::AArch64 {
        let code_size = std::fs::metadata(&firmware.code)?.len() as usize;
        contents.resize(code_size, 0);
    }

    std::fs::write(&target, contents)?;
    Ok(target)
}
