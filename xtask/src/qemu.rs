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
use crate::btrfs_disk;
use crate::cargo;
use crate::paths::{self, Arch, Firmware};
use crate::symbolize::Symbolizer;
use crate::test_disk;
use crate::{Error, Result};

/// What the kernel prints when it has finished its self-checks.
pub(crate) const SUCCESS_MARKER: &str = "FERRIX-BOOT-OK";
/// What the panic handler prints. Seeing this ends the test immediately: the
/// kernel will not recover, and waiting out the timeout only hides the reason.
pub(crate) const PANIC_MARKER: &str = "FERRIX-PANIC";

/// The command line `--reset` puts in the image's `CMDLINE.TXT`.
pub(crate) const RESET_CMDLINE: &str = "ferrix.onexit=reset\n";

/// What the kernel says once it has read that option.
const RESET_ARMED: &str = "power    ferrix.onexit=reset: the machine resets when boot ends";

/// What it says as it acts on it.
const RESETTING: &str = "power    resetting, as ferrix.onexit=reset asks";

/// The loader's first line, which only a machine that really reset prints twice.
const LOADER_BANNER: &str = "Ferrix loader ";
/// How long to keep reading after the panic marker. The marker line names the
/// failure; the lines after it say where and on which processor, and a log
/// that stops at the marker loses them.
const PANIC_REPORT_GRACE: Duration = Duration::from_secs(2);

/// The exit status QEMU reports when the x86-64 kernel writes 0x10 to the
/// `isa-debug-exit` port: `(value << 1) | 1`.
const DEBUG_EXIT_SUCCESS: i32 = 33;

/// Boot the image with the serial port attached to this terminal.
pub(crate) fn run(arch: Arch, image: &Path, args: &Args) -> Result<()> {
    let (mut command, network) = qemu_command(arch, image, args)?;
    if args.gdb {
        let _ = command.args(["-s", "-S"]);
        println!("  waiting for a debugger on localhost:1234");
    }
    println!("  {arch}: booting (quit with Ctrl-A x)\n");
    let booted = cargo::run(command, "qemu");
    report_network(&network);
    booted
}

/// Boot the image headless and require the kernel to report success.
pub(crate) fn test_boot(arch: Arch, image: &Path, kernel: &Path, args: &Args) -> Result<()> {
    println!("  {arch}: booting under QEMU (timeout {}s)", args.timeout);
    let watched = watch(arch, image, kernel, args, SUCCESS_MARKER)?;
    match watched.verdict {
        Verdict::Reached => {
            let code = watched.status.code().unwrap_or(0);
            if code != 0 && code != DEBUG_EXIT_SUCCESS {
                return Err(Error::new(format!(
                    "the {arch} kernel reported success but QEMU exited {code}"
                )));
            }
            if let Some(problem) = entropy_problem(&watched.lines) {
                return Err(Error::new(format!(
                    "{arch}: {problem}.\n  Serial output is in {}",
                    watched.log.display()
                )));
            }
            if let Some(problem) = devmgr_problem(&watched.lines) {
                return Err(Error::new(format!(
                    "{arch}: {problem}.\n  Serial output is in {}",
                    watched.log.display()
                )));
            }
            if let Some(problem) = iommu_problem(&watched.lines) {
                return Err(Error::new(format!(
                    "{arch}: {problem}.\n  Serial output is in {}",
                    watched.log.display()
                )));
            }
            if let Some(problem) = fault_problem(arch, &watched.lines) {
                return Err(Error::new(format!(
                    "{arch}: {problem}.\n  Serial output is in {}",
                    watched.log.display()
                )));
            }
            if args.reset {
                if let Some(problem) = reset_problem(&watched) {
                    return Err(Error::new(format!(
                        "{arch}: {problem}.\n  Serial output is in {}",
                        watched.log.display()
                    )));
                }
                println!("  {arch}: the machine reset, as ferrix.onexit=reset asked");
            }
            println!("  {arch}: boot ok");
            Ok(())
        }
        Verdict::Panicked => Err(panicked(arch, &watched.log)),
        Verdict::Silent => Err(Error::new(format!(
            "the {arch} kernel never printed `{SUCCESS_MARKER}` within {}s.\n  \
             Serial output is in {}",
            args.timeout,
            watched.log.display()
        ))),
    }
}

/// Why a `--reset` boot did not show a reset, if it did not.
///
/// The kernel has to have read the option from the image's `CMDLINE.TXT` and
/// said it was resetting, and the loader then has to have started again. A
/// power-off cannot do that: under `-action shutdown=pause` it only pauses
/// QEMU, the debug-exit write on x86-64 included.
fn reset_problem(watched: &Watched) -> Option<String> {
    let first = |text: &str| watched.lines.iter().position(|line| line.contains(text));
    if first(RESET_ARMED).is_none() {
        return Some(format!(
            "--reset: the kernel never said `{RESET_ARMED}`, so the image's CMDLINE.TXT did not reach it"
        ));
    }
    let Some(at) = first(RESETTING) else {
        return Some(format!("--reset: the kernel never said `{RESETTING}`"));
    };
    if watched
        .lines
        .iter()
        .skip(at)
        .any(|line| line.contains(LOADER_BANNER))
    {
        None
    } else {
        Some(format!(
            "--reset: the loader did not start again after `{RESETTING}`, so the machine did not reset"
        ))
    }
}

/// What stage 10's PCI check prints after the number of bytes a virtio-rng
/// device wrote into memory the kernel gave it.
const ENTROPY_READ: &str = " entropy bytes read by DMA";

/// What it prints after the number of those requests that completed by MSI-X.
const BY_MSIX: &str = " completions by MSI-X";

/// The number written just before `suffix` in `line`.
fn count_before(line: &str, suffix: &str) -> Option<u32> {
    let before = line.split(suffix).next()?;
    before
        .rsplit(' ')
        .next()
        .and_then(|count| count.parse::<u32>().ok())
}

/// Why a boot that reached the marker still failed the DMA check, if it did.
///
/// The kernel skips a virtio-rng device that refuses or stalls rather than
/// halting, because on a hypervisor somebody else configured — libvirt adds
/// one to every guest — that is not the kernel's fault. Every machine this
/// tool boots has one it configured itself, so here a boot that read no
/// entropy has lost DMA. A device may legitimately write fewer bytes than it
/// was asked for, so any positive count passes.
fn entropy_problem(lines: &[String]) -> Option<String> {
    let Some(line) = lines.iter().find(|line| line.contains(ENTROPY_READ)) else {
        return Some("the kernel never reported reading entropy by DMA".to_owned());
    };
    if !count_before(line, ENTROPY_READ).is_some_and(|count| count > 0) {
        return Some(format!(
            "the kernel read no entropy by DMA: `{}`",
            line.trim()
        ));
    }
    // Every machine this tool boots has an interrupt controller that takes
    // messages, so a completion that had to be polled is a lost interrupt.
    if !count_before(line, BY_MSIX).is_some_and(|count| count > 0) {
        return Some(format!(
            "no entropy request completed by MSI-X: `{}`",
            line.trim()
        ));
    }
    None
}

/// What stage 10's IOMMU discovery prints after the number of PCI functions
/// firmware puts behind a unit.
const BEHIND_IOMMU: &str = " PCI functions behind one";

/// What it prints after the number of functions whose description it could
/// not follow.
const UNRESOLVED: &str = " unresolved";

/// Why a boot that reached the marker still failed IOMMU discovery, if it did.
///
/// Every machine this tool boots has an IOMMU it configured — `intel-iommu` on
/// `q35`, `iommu=smmuv3` on `virt` — and firmware that describes it. A boot that
/// places no PCI function behind one has lost that description, and one that
/// leaves a function unresolved reads it differently from the firmware that
/// wrote it.
fn iommu_problem(lines: &[String]) -> Option<String> {
    let Some(line) = lines.iter().find(|line| line.contains(BEHIND_IOMMU)) else {
        return Some("the kernel never reported where its IOMMUs are".to_owned());
    };
    if !count_before(line, BEHIND_IOMMU).is_some_and(|count| count > 0) {
        return Some(format!(
            "no PCI function was placed behind an IOMMU: `{}`",
            line.trim()
        ));
    }
    if count_before(line, UNRESOLVED) != Some(0) {
        return Some(format!(
            "an IOMMU description could not be followed: `{}`",
            line.trim()
        ));
    }
    None
}

/// What stage 10's PCI check prints after the number of writes outside a
/// translated domain that its unit faulted.
const FAULTED: &str = " out-of-domain writes faulted";

/// What stage 10's devmgr line ends with.
const DEVMGR_FAILED: &str = " failed";

/// Why a boot whose image carries `devmgr` failed to start its drivers, if
/// it did: the devmgr line must say at least one started and none failed.
/// Every machine this tool boots has a virtio-blk disk for it. An image
/// without `devmgr` prints that it was not started, which is not a failure.
fn devmgr_problem(lines: &[String]) -> Option<String> {
    let Some(line) = lines.iter().find(|line| line.contains("  devmgr   ")) else {
        return Some("the kernel never reported on devmgr".to_owned());
    };
    if line.contains("not started") {
        return None;
    }
    let started = line
        .split(" started")
        .next()
        .and_then(|before| before.rsplit(' ').next())
        .and_then(|count| count.parse::<u32>().ok());
    let failed = count_before(line, DEVMGR_FAILED);
    match (started, failed) {
        (Some(started), Some(0)) if started > 0 => None,
        _ => Some(format!(
            "devmgr started no driver, or one failed: `{}`",
            line.trim()
        )),
    }
}

/// Why a boot on a machine whose IOMMU translates still failed the stage 10
/// exit criterion's out-of-domain check, if it did.
///
/// On the machines this tool boots, x86-64 and AArch64 send the entropy
/// device's DMA through a translated domain. ARMv7-A does not — U-Boot keeps
/// its virtio devices from offering the platform's translation, which the exit
/// criterion states as degraded trusted mode — so it is not asked.
fn fault_problem(arch: Arch, lines: &[String]) -> Option<String> {
    if arch == Arch::Armv7a {
        return None;
    }
    let Some(line) = lines.iter().find(|line| line.contains(FAULTED)) else {
        return Some("the kernel never reported an out-of-domain write".to_owned());
    };
    if count_before(line, FAULTED).is_some_and(|count| count > 0) {
        None
    } else {
        Some(format!(
            "no write outside a translated domain faulted: `{}`",
            line.trim()
        ))
    }
}

/// Boot an image with a program and a script built in, and require the
/// script's output, in order, and its exit status.
///
/// The lines are looked for *after* the boot marker, so a kernel that happened
/// to print one of them during its self-checks cannot satisfy the test.
pub(crate) fn test_shell(arch: Arch, image: &Path, kernel: &Path, args: &Args) -> Result<()> {
    println!(
        "  {arch}: running the built-in script under QEMU (timeout {}s)",
        args.timeout
    );
    let watched = watch(arch, image, kernel, args, crate::shell::EXITED)?;
    let log = watched.log.display();
    let after_boot = watched
        .lines
        .iter()
        .position(|line| line.contains(SUCCESS_MARKER))
        .and_then(|at| watched.lines.get(at..))
        .unwrap_or_default();

    if let Some(line) = after_boot
        .iter()
        .find(|line| line.contains(crate::shell::NOT_STARTED))
    {
        return Err(Error::new(format!(
            "{arch}: {}\n  Serial output is in {log}",
            line.trim()
        )));
    }
    match watched.verdict {
        Verdict::Reached => {}
        Verdict::Panicked => return Err(panicked(arch, &watched.log)),
        Verdict::Silent => {
            return Err(Error::new(format!(
                "{arch}: the shell never exited within {}s.\n  Serial output is in {log}",
                args.timeout
            )));
        }
    }

    let mut remaining = after_boot.iter();
    for want in crate::shell::EXPECTED {
        if !remaining.any(|line| line.trim_end() == *want) {
            return Err(Error::new(format!(
                "{arch}: the script's output is missing `{want}`, or it came out of order.\n  \
                 Serial output is in {log}"
            )));
        }
    }
    let status = format!("{} {}", crate::shell::EXITED, crate::shell::STATUS);
    if !after_boot.iter().any(|line| line.trim() == status) {
        return Err(Error::new(format!(
            "{arch}: the shell did not exit with {}.\n  Serial output is in {log}",
            crate::shell::STATUS
        )));
    }
    println!(
        "  {arch}: busybox ran the script and exited with {}",
        crate::shell::STATUS
    );
    Ok(())
}

/// Boot an image whose kernel runs `vfs::COMMANDS`, and judge each command by
/// its status and its output.
///
/// As with [`test_shell`], only what follows the boot marker counts.
pub(crate) fn test_vfs(arch: Arch, image: &Path, kernel: &Path, args: &Args) -> Result<()> {
    let commands = crate::vfs::COMMANDS;
    let applets = crate::vfs::APPLETS;
    println!(
        "  {arch}: running {} programs and {} applets under QEMU (timeout {}s)",
        commands.len(),
        applets.len(),
        args.timeout
    );
    let watched = watch(arch, image, kernel, args, crate::vfs::DONE)?;
    let log = watched.log.display();
    let after_boot = watched
        .lines
        .iter()
        .position(|line| line.contains(SUCCESS_MARKER))
        .and_then(|at| watched.lines.get(at..))
        .unwrap_or_default();
    let ending = match watched.verdict {
        Verdict::Reached => None,
        Verdict::Panicked => Some("the kernel panicked".to_owned()),
        Verdict::Silent => Some(format!(
            "the programs did not all finish within {}s",
            args.timeout
        )),
    };

    // The exit criterion and the applets are judged and reported apart, so
    // that the criterion's line means what it did before the applets were
    // added, whatever they do.
    let groups = [
        (
            commands,
            0,
            "programs",
            "stage 8's exit programs all passed",
        ),
        (
            applets,
            commands.len(),
            "applets",
            "stage 8's applets all passed",
        ),
    ];
    let mut failures = Vec::new();
    for (group, first, what, all_passed) in groups {
        match (crate::vfs::judge(group, first, after_boot), &ending) {
            (Ok(passed), None) => {
                for line in passed {
                    println!("  {arch}: {line}");
                }
                println!("  {arch}: {all_passed}");
            }
            (Ok(_), Some(_)) => {}
            (Err(failed), _) => failures.push(format!(
                "{} of {} {what} failed:\n    {}",
                failed.len(),
                group.len(),
                failed.join("\n    ")
            )),
        }
    }
    if ending.is_none() && failures.is_empty() {
        return Ok(());
    }
    let why = match (ending, failures.is_empty()) {
        (Some(ending), true) => format!("{ending}."),
        (Some(ending), false) => format!("{ending}; {}", failures.join("\n  ")),
        (None, _) => failures.join("\n  "),
    };
    Err(Error::new(format!(
        "{arch}: {why}\n  Serial output is in {log}"
    )))
}

/// How a watched boot ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The line being waited for arrived.
    Reached,
    /// The kernel panicked first.
    Panicked,
    /// Neither, before the timeout or before QEMU closed the port.
    Silent,
}

/// What watching a boot produced.
#[derive(Debug)]
struct Watched {
    /// Every line the guest printed, in order.
    lines: Vec<String>,
    /// How it ended.
    verdict: Verdict,
    /// QEMU's exit status.
    status: std::process::ExitStatus,
    /// Where the serial output was saved.
    log: PathBuf,
}

/// Boot headless, echo and save the serial port, and stop at the first line
/// containing `until`, at a panic, or at the timeout.
///
/// `kernel` is the ELF the image was built from, which is what a panic
/// report's backtrace addresses are resolved against.
fn watch(arch: Arch, image: &Path, kernel: &Path, args: &Args, until: &str) -> Result<Watched> {
    let symbols = Symbolizer::open(kernel);
    let (mut command, network) = qemu_command(arch, image, args)?;
    let _ = command.stdout(Stdio::piped()).stderr(Stdio::inherit());

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
    // Every line is stamped with the seconds since QEMU was started, on the
    // screen and in the log. A boot that stops says *where* it stopped either
    // way; only a stamp says whether it was slow getting there or hung, which
    // on a loaded host are two different problems with the same last line.
    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.timeout);
    let mut lines = Vec::new();
    let mut verdict = Verdict::Silent;

    // Under `--reset` the marker is not the end: the kernel still has to say it
    // is resetting, and the loader to start again because it did.
    let mut resetting = false;
    let mut restarted = false;
    while verdict == Verdict::Silent || (args.reset && verdict == Verdict::Reached && !restarted) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        match receiver.recv_timeout(remaining) {
            Ok(line) => {
                let at = started.elapsed().as_secs_f64();
                println!("  {at:6.2} | {line}");
                writeln!(log, "{at:6.2} | {line}")?;
                if verdict == Verdict::Silent && line.contains(until) {
                    verdict = Verdict::Reached;
                    // A kernel that has not said it will reset by its marker
                    // never will, and a power-off only pauses QEMU: waiting
                    // for the loader again would be waiting for the timeout.
                    if args.reset && !lines.iter().any(|seen: &String| seen.contains(RESET_ARMED)) {
                        restarted = true;
                    }
                } else if line.contains(PANIC_MARKER) {
                    verdict = Verdict::Panicked;
                }
                if args.reset && verdict == Verdict::Reached {
                    if line.contains(RESETTING) {
                        resetting = true;
                    } else if resetting && line.contains(LOADER_BANNER) {
                        restarted = true;
                    }
                }
                lines.push(line);
            }
            // The guest closed the serial port: QEMU is on its way out, so stop
            // reading and judge on the exit status below.
            Err(mpsc::RecvTimeoutError::Disconnected | mpsc::RecvTimeoutError::Timeout) => break,
        }
    }

    if verdict == Verdict::Panicked {
        take_panic_report(&receiver, &mut log, symbols.as_ref())?;
    }
    let status = finish(&mut child, verdict != Verdict::Silent)?;
    drop(receiver);
    let _ = reader.join();
    log.flush()?;
    // After QEMU has gone, so that the numbers are final and the gateway's
    // thread is not still being fed while they are read.
    report_network(&network);

    Ok(Watched {
        lines,
        verdict,
        status,
        log: log_path,
    })
}

/// The error for a boot that panicked.
fn panicked(arch: Arch, log: &Path) -> Error {
    Error::new(format!(
        "the {arch} kernel panicked during boot; see {}",
        log.display()
    ))
}

/// Copy the rest of a panic report to the terminal and the log, naming the
/// function beside each backtrace address when `symbols` can.
///
/// Stops when the guest goes quiet for [`PANIC_REPORT_GRACE`] or closes the
/// port. The verdict is already decided; this is only so that it arrives with
/// its reasons.
pub(crate) fn take_panic_report(
    receiver: &mpsc::Receiver<String>,
    log: &mut impl Write,
    symbols: Option<&Symbolizer>,
) -> Result<()> {
    while let Ok(line) = receiver.recv_timeout(PANIC_REPORT_GRACE) {
        let line = symbols
            .and_then(|symbols| symbols.annotate(&line))
            .unwrap_or(line);
        println!("       | {line}");
        writeln!(log, "       | {line}")?;
        if line.contains(SUCCESS_MARKER) || line.contains(PANIC_MARKER) {
            // Another processor's report, or something worse; either way the
            // first one is what failed the boot, and waiting for more of them
            // could go on for as long as the guest keeps printing.
            break;
        }
    }
    log.flush()?;
    Ok(())
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
fn qemu_command(arch: Arch, image: &Path, args: &Args) -> Result<(Command, Network)> {
    let binary = paths::which(arch.qemu_binary()).ok_or_else(|| {
        Error::new(format!(
            "{} is not on PATH.\n  Install QEMU (Debian/Ubuntu: `qemu-system-x86` and \
             `qemu-system-arm`; Windows: `winget install SoftwareFreedomConservancy.QEMU`).",
            arch.qemu_binary()
        ))
    })?;

    let firmware = paths::find_firmware(arch)?;
    let accelerator = accelerator(arch, &binary, args.accel.as_deref())?;

    let mut command = Command::new(binary);
    let _ = command.current_dir(paths::workspace_root());

    let _ = command.args(["-accel", &accelerator]);
    let _ = command.args([
        "-m",
        &args.memory.to_string(),
        "-smp",
        &args.smp.to_string(),
        "-display",
        "none",
        "-monitor",
        "none",
        "-serial",
        "stdio",
    ]);
    // A guest that reboots on a triple fault turns a crash into an endless
    // loop, which in CI is a timeout with no cause in the log. Not under
    // `--reset`, whose point is that the machine starts again: there a reset
    // restarts firmware and the loader, and a power-off only pauses QEMU, so
    // the loader can appear a second time only if the kernel reset it.
    if args.reset {
        let _ = command.args(["-action", "shutdown=pause"]);
    } else {
        let _ = command.arg("-no-reboot");
    }

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
                // A VT-d unit for stage 10's IOMMU domains. Interrupt
                // remapping stays off: the kernel's MSI-X messages are
                // compatibility format, and nothing drives remapping.
                "-device",
                "intel-iommu,intremap=off",
            ]);
            // The image on the first port of q35's AHCI controller, where a
            // bare `-drive` puts it, spelled out only to give it `bootindex`.
            // Without one OVMF tries the virtio test disk first, because its
            // slot comes before the controller's, and says so:
            //
            //     BdsDxe: failed to load Boot0002 "UEFI Misc Device" from
            //     PciRoot(0x0)/Pci(0x3,0x0): Not Found
            //
            // which is harmless and the kind of noise that trains people to
            // skim the boot log.
            let _ = command.args([
                "-drive",
                &format!("format=raw,file={},if=none,id=disk", display(image)),
                "-device",
                "ide-hd,drive=disk,bus=ide.0,bootindex=0",
            ]);
        }
        Arch::AArch64 | Arch::Armv7a => {
            // The same `virt` machine for both: a GICv2, a PL011 at the same
            // address, the architected timer, a virtio disk. Only the CPU
            // differs, and with it the width of everything the CPU does.
            let cpu = if arch == Arch::AArch64 {
                "cortex-a72"
            } else {
                "cortex-a7"
            };
            let _ = command.args([
                // An SMMUv3 for stage 10's IOMMU domains.
                "-machine",
                "virt,iommu=smmuv3",
                "-cpu",
                cpu,
                "-drive",
                &format!("format=raw,file={},if=none,id=disk", display(image)),
                "-device",
                "virtio-blk-device,drive=disk",
            ]);
            if arch == Arch::AArch64 {
                // A framebuffer for the panic screen. `virt` has no display
                // device, and firmware offers graphics output only when there
                // is one; ramfb is the simplest one edk2 drives, and it needs
                // no display attached. ARMv7-A boots through U-Boot, which
                // this has not been tried with.
                let _ = command.args(["-device", "ramfb"]);
            }
        }
    }

    // A virtio device on PCI, on every machine, for stage 10's enumeration to
    // find: a 64-bit BAR to size, MSI-X and virtio's vendor capabilities to
    // walk. An entropy source because it needs no backend and nothing on the
    // guest side depends on it, so it changes what firmware and the kernel see
    // on the bus and nothing else.
    //
    // `disable-legacy=on,iommu_platform=on` sends the device's DMA through the
    // machine's IOMMU, which QEMU otherwise lets virtio bypass, and which the
    // out-of-domain fault stage 10 exits on needs. Not on ARMv7-A: U-Boot
    // 2025.10's virtio-pci driver fails a heap assertion
    // (`do_check_inuse_chunk`) and resets when a device offers
    // VIRTIO_F_ACCESS_PLATFORM, with or without an SMMU, while the loader is
    // still running on its boot services.
    let rng = if arch == Arch::Armv7a {
        "virtio-rng-pci,disable-legacy=on"
    } else {
        "virtio-rng-pci,disable-legacy=on,iommu_platform=on"
    };
    let _ = command.args(["-device", rng]);
    attach_test_disk(&mut command, arch)?;
    attach_btrfs_disk(&mut command, arch)?;
    let network = attach_network(&mut command, arch, args)?;

    match &firmware {
        Firmware::Pflash { code, vars } => {
            let vars = prepare_vars(arch, code, vars.as_deref())?;
            let _ = command.args([
                "-drive",
                &format!("if=pflash,format=raw,readonly=on,file={}", display(code)),
            ]);
            let _ = command.args([
                "-drive",
                &format!("if=pflash,format=raw,file={}", display(&vars)),
            ]);
        }
        // U-Boot runs from RAM and keeps no variables: there is no store to
        // prepare, and so none for a previous run to have poisoned.
        Firmware::Bios(uboot) => {
            let _ = command.args(["-bios", &display(uboot)]);
        }
    }

    Ok((command, network))
}

/// The MAC address the guest's virtio-net device carries.
///
/// QEMU's own default for the first NIC, kept so that a guest driver, a DHCP
/// lease and a packet capture all name the guest the same way whether the
/// frames went through this gateway or through anything else.
const GUEST_MAC: &str = "52:54:00:12:34:56";

/// What `--net` leaves behind for the caller to hold: on a UNIX host, the
/// gateway serving the guest's wire.
#[cfg(unix)]
type Network = Option<crate::gateway::Gateway>;

/// And on a host with no `UnixDatagram` in `std`, nothing that can exist.
#[cfg(not(unix))]
type Network = Option<std::convert::Infallible>;

/// Give the guest a network device, or deliberately no network at all.
///
/// Without `--net` this is the argument it always was, and the comment is the
/// one that was on it, because the reason has not changed: QEMU adds a network
/// device by default, and on AArch64 firmware then finds a PCI option ROM built
/// for x86 and says so —
///
/// ```text
/// Image type X64 can't be loaded on AARCH64 UEFI system.
/// ```
///
/// — which is alarming, unrelated to us, and exactly the kind of noise that
/// trains people to skim the boot log.
///
/// With `--net` the device is a virtio-net on PCI whose backend is a pair of
/// UNIX datagram sockets, with `xtask`'s own gateway on the other end of them;
/// `gateway` says why that rather than `-netdev user`. `-netdev` is enough on
/// its own to stop QEMU adding its default device, so `-net none` is not also
/// passed: QEMU warns about mixing the two families.
///
/// The same virtio flags as every other device this tool attaches, and the same
/// ARMv7-A exception: U-Boot 2025.10's virtio-pci driver fails a heap assertion
/// and resets when a device offers `VIRTIO_F_ACCESS_PLATFORM`.
#[cfg(unix)]
fn attach_network(command: &mut Command, arch: Arch, args: &Args) -> Result<Network> {
    if !args.net {
        let _ = command.args(["-net", "none"]);
        return Ok(None);
    }
    let gateway = crate::gateway::Gateway::start(&gateway_directory())?;
    println!(
        "  {arch}: network through xtask's gateway: guest {}, gateway {}, DNS {}",
        crate::gateway::GUEST_IP,
        crate::gateway::GATEWAY_IP,
        crate::gateway::DNS_IP
    );
    let _ = command.args([
        "-netdev",
        &format!(
            "dgram,id=net0,local.type=unix,local.path={},remote.type=unix,remote.path={}",
            display(gateway.qemu_socket()),
            display(gateway.host_socket())
        ),
    ]);
    let flags = if arch == Arch::Armv7a {
        "disable-legacy=on"
    } else {
        "disable-legacy=on,iommu_platform=on"
    };
    let _ = command.args([
        "-device",
        &format!("virtio-net-pci,netdev=net0,mac={GUEST_MAC},{flags}"),
    ]);
    Ok(Some(gateway))
}

/// The same, where `std` has no UNIX datagram socket to build the backend on.
#[cfg(not(unix))]
fn attach_network(command: &mut Command, _arch: Arch, args: &Args) -> Result<Network> {
    if args.net {
        return Err(Error::new(
            "--net needs a UNIX datagram socket for QEMU's `dgram` backend, which this \
             platform's std does not have",
        ));
    }
    let _ = command.args(["-net", "none"]);
    Ok(None)
}

/// Where the gateway's socket files go.
///
/// The temporary directory rather than `build/`, and for one reason: a UNIX
/// socket address is a `sun_path` of 108 bytes including its terminator, and a
/// checkout under a worktree under a home directory spends most of that before
/// the file name starts. QEMU's error for a name that does not fit is about a
/// path being too long, which reads as a bug in this tool rather than as an
/// operating system limit.
#[cfg(unix)]
fn gateway_directory() -> PathBuf {
    std::env::temp_dir()
}

/// Say what the gateway saw, once the guest has stopped talking to it.
///
/// A network test that fails says the guest never got an address, or never
/// resolved a name. Which of those is a driver that never transmitted and which
/// is a gateway that dropped what it was given is not visible from the guest's
/// side at all, and is exactly what these counters answer.
#[cfg(unix)]
fn report_network(network: &Network) {
    if let Some(gateway) = network {
        println!("  gateway: {}", gateway.counters().report());
        if gateway.counters().frames_in() == 0 {
            println!("  gateway: the guest never transmitted a frame");
        }
    }
}

/// The same, where there is never a gateway to report on.
#[cfg(not(unix))]
fn report_network(_network: &Network) {}

/// Attach the test disk as a second virtio device on PCI: a block device, for
/// stage 10's ring-3 driver to read sectors from.
///
/// `test_disk` says what each sector holds, and writes the image on demand, so
/// nothing has to run before QEMU does; the layout goes to this command's
/// output, since the serial log holds only what the guest printed. Read-only,
/// because a driver test that could write would change what the next one reads.
///
/// Added after the entropy device, so that one keeps its slot, and with the
/// same flags for the same reasons: DMA through the IOMMU, except on ARMv7-A,
/// where U-Boot resets when a device offers `VIRTIO_F_ACCESS_PLATFORM`. No
/// `bootindex`: the first sector holds no partition table and no filesystem,
/// so firmware that looks at the disk finds nothing to boot and goes on to the
/// image.
fn attach_test_disk(command: &mut Command, arch: Arch) -> Result<()> {
    let disk = test_disk::ensure()?;
    println!(
        "  {arch}: test disk {} as virtio-blk-pci: {}",
        display(&disk),
        test_disk::describe()
    );
    let _ = command.args([
        "-drive",
        &format!(
            "file={},if=none,format=raw,id=testdisk,readonly=on",
            display(&disk)
        ),
    ]);
    let device = if arch == Arch::Armv7a {
        "virtio-blk-pci,drive=testdisk,disable-legacy=on"
    } else {
        "virtio-blk-pci,drive=testdisk,disable-legacy=on,iommu_platform=on"
    };
    let _ = command.args(["-device", device]);
    Ok(())
}

/// Attach the btrfs fixture as a third virtio device on PCI, after the
/// pattern disk so that it is `vdb`: the disk stage 11's exit mounts. The
/// same flags as the pattern disk, for the same reasons, and read-only, since
/// the mount is.
fn attach_btrfs_disk(command: &mut Command, arch: Arch) -> Result<()> {
    let disk = btrfs_disk::ensure()?;
    println!(
        "  {arch}: btrfs disk {} as virtio-blk-pci: {}",
        display(&disk),
        btrfs_disk::describe()
    );
    let _ = command.args([
        "-drive",
        &format!(
            "file={},if=none,format=raw,id=btrfsdisk,readonly=on",
            display(&disk)
        ),
    ]);
    let device = if arch == Arch::Armv7a {
        "virtio-blk-pci,drive=btrfsdisk,disable-legacy=on"
    } else {
        "virtio-blk-pci,drive=btrfsdisk,disable-legacy=on,iommu_platform=on"
    };
    let _ = command.args(["-device", device]);
    Ok(())
}

/// The hardware accelerator this host's QEMU would use, if it has one.
///
/// Named per platform rather than probed, because the name is the only thing
/// that varies: each of these is the one interface its operating system
/// exposes for running guest instructions on the processor directly.
const fn host_accelerator() -> Option<&'static str> {
    if cfg!(target_os = "windows") {
        // The Windows Hypervisor Platform. Present on Windows 10 and 11, but
        // only once the optional feature is turned on, which is why `auto`
        // asks QEMU rather than assuming.
        Some("whpx")
    } else if cfg!(target_os = "linux") {
        Some("kvm")
    } else if cfg!(target_os = "macos") {
        Some("hvf")
    } else {
        None
    }
}

/// Decide which accelerator to boot under.
///
/// # Why this is a choice and not a default
///
/// `tcg` emulates the processor, including its `MMU`, and an emulated `MMU`
/// has no `TLB` to speak of: it resolves every access through the page tables
/// as it finds them. That makes it wonderfully reproducible and it makes it
/// blind to an entire class of bug, because a stale translation cannot be
/// stale in a cache that does not exist. A hardware accelerator runs on the
/// real `MMU`, with the real `TLB`, and sees them.
///
/// **This is not an abstract preference.** `arch::flush_tlb` on x86-64 spared
/// global entries — which is nearly every mapping the kernel makes — and every
/// boot test passed anyway for as long as every boot test ran under `tcg`. The
/// first run under `whpx` failed three different self-checks. So the default
/// stays `tcg`, because reproducibility is what a boot test is for and `CI`
/// has no hypervisor to offer; `auto` is for the machine in front of you,
/// which usually does.
fn accelerator(arch: Arch, binary: &Path, requested: Option<&str>) -> Result<String> {
    let requested = requested.unwrap_or("tcg");
    if requested == "tcg" {
        return Ok("tcg".to_owned());
    }

    let available = available_accelerators(binary);
    let supported = |name: &str| available.iter().any(|found| found == name);

    if requested != "auto" {
        // Asked for by name: refuse rather than quietly emulating. Somebody
        // who typed `--accel kvm` wants to know it did not happen.
        if !supported(requested) {
            return Err(Error::new(format!(
                "this QEMU has no `{requested}` accelerator; it offers {}",
                available.join(", ")
            )));
        }
        return Ok(requested.to_owned());
    }

    // `auto`, which never fails: it falls back to emulation, since the whole
    // point is that it works on whatever machine it is run on. A guest of a
    // different architecture than the host has nothing to accelerate — there
    // are no Arm instructions for an x86 processor to run directly.
    if arch != Arch::host() {
        return Ok("tcg".to_owned());
    }
    match host_accelerator() {
        Some(name) if supported(name) && usable(binary, name) => Ok(name.to_owned()),
        _ => Ok("tcg".to_owned()),
    }
}

/// Whether this QEMU can actually *initialise* `name` on this machine.
///
/// Being built with an accelerator and being allowed to use it are different
/// questions, and only the first is answerable from a list. `/dev/kvm` is
/// `root:kvm` on most distributions, so a developer who has never been added
/// to that group has a QEMU that offers `kvm` and cannot open it; the
/// Windows Hypervisor Platform is an optional feature that may be off. In
/// both cases QEMU exits immediately with an error, which for `auto` — which
/// promises to work on whatever machine it is run on — must mean "fall back to
/// emulation", not "fail the boot test".
///
/// There is no way to ask the question without answering it: QEMU initialises
/// an accelerator only when it starts a machine. So this starts the smallest
/// one there is (`-M none`, no devices, CPU halted) and watches. Failure is
/// prompt and is an exit; success is QEMU sitting there waiting, which is what
/// the deadline is for. The asymmetry is the signal.
fn usable(binary: &Path, name: &str) -> bool {
    let Ok(mut child) = Command::new(binary)
        .args(["-accel", name])
        .args([
            "-M",
            "none",
            "-display",
            "none",
            "-monitor",
            "none",
            "-serial",
            "none",
            "-nodefaults",
            "-no-user-config",
            // Halted, so a successful probe runs no guest instructions.
            "-S",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match child.try_wait() {
            // Exited on its own inside the window: the accelerator did not
            // come up. QEMU has no other reason to leave this quickly.
            Ok(Some(_)) => return false,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }

    // Still running, so the accelerator initialised. Nothing to wait for.
    let _ = child.kill();
    let _ = child.wait();
    true
}

/// The accelerators this QEMU binary was built with.
///
/// Asked of the binary rather than assumed, because whether one is *usable* is
/// a property of the machine — Hyper-V switched on, `/dev/kvm` readable — and
/// a list QEMU itself prints is the closest thing to an answer that does not
/// involve starting a guest. An empty list on error, which sends `auto` to
/// `tcg` and gives a named request the error it deserves.
fn available_accelerators(binary: &Path) -> Vec<String> {
    let Ok(output) = Command::new(binary).args(["-accel", "help"]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        // The first line is a heading, and every other line is one name.
        .filter(|line| !line.is_empty() && !line.contains(' '))
        .map(str::to_owned)
        .collect()
}

/// A path as QEMU wants it: forward slashes, even on Windows, because a
/// backslash inside a `-drive` argument is taken as an escape.
fn display(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Produce the writable UEFI variable store QEMU needs beside the firmware.
///
/// It has to be writable and it has to survive between runs, so it is copied
/// into `build/` rather than used from the read-only system location. On the
/// Arm `virt` machine both pflash images additionally have to be exactly the
/// same size, which is why this pads.
fn prepare_vars(arch: Arch, code: &Path, template: Option<&Path>) -> Result<PathBuf> {
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

    let mut contents = match template {
        Some(source) => std::fs::read(source)
            .map_err(|error| Error::new(format!("reading {}: {error}", source.display())))?,
        // No template: an all-zero store is not a valid variable store, and
        // EDK2 responds by formatting one, which is what we want anyway.
        None => Vec::new(),
    };

    if arch != Arch::X86_64 {
        let code_size = std::fs::metadata(code)?.len() as usize;
        contents.resize(code_size, 0);
    }

    std::fs::write(&target, contents)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::{Arch, devmgr_problem, entropy_problem, fault_problem, iommu_problem};

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn a_devmgr_line_must_say_one_started_and_none_failed() {
        let good = lines(&["  devmgr   8 devices, 1 drivers, 2 started, 0 failed"]);
        assert_eq!(devmgr_problem(&good), None, "two started");
        let none = lines(&["  devmgr   8 devices, 1 drivers, 0 started, 0 failed"]);
        assert!(devmgr_problem(&none).is_some(), "nothing started");
        let failed = lines(&["  devmgr   8 devices, 1 drivers, 1 started, 1 failed"]);
        assert!(devmgr_problem(&failed).is_some(), "one failed");
        let absent = lines(&["  devmgr   not started: the image carries no /sbin/devmgr"]);
        assert_eq!(devmgr_problem(&absent), None, "an image without devmgr");
        let silent = lines(&["FERRIX-BOOT-OK stages 1-11"]);
        assert!(devmgr_problem(&silent).is_some(), "no line at all");
    }

    #[test]
    fn a_boot_that_read_entropy_passes_however_little_it_read() {
        for count in ["64", "1"] {
            let boot = lines(&[
                &format!(
                    "  pci      2 functions, 1 virtio transports, {count} entropy bytes read by DMA, 1 completions by MSI-X"
                ),
                "FERRIX-BOOT-OK stages 1-11",
            ]);
            assert_eq!(entropy_problem(&boot), None, "{count} bytes");
        }
    }

    #[test]
    fn a_boot_that_read_none_or_never_said_fails() {
        let none = lines(&["  pci      1 virtio transports, 0 entropy bytes read by DMA"]);
        assert!(entropy_problem(&none).is_some(), "zero bytes");
        let silent = lines(&["FERRIX-BOOT-OK stages 1-11"]);
        assert!(entropy_problem(&silent).is_some(), "no line at all");
    }

    #[test]
    fn a_boot_whose_completion_was_polled_fails() {
        let polled = lines(&["  pci      64 entropy bytes read by DMA, 0 completions by MSI-X"]);
        assert!(entropy_problem(&polled).is_some(), "no MSI-X");
        let old = lines(&["  pci      64 entropy bytes read by DMA"]);
        assert!(
            entropy_problem(&old).is_some(),
            "a kernel that does not say"
        );
    }

    #[test]
    fn a_boot_that_placed_functions_behind_an_iommu_passes() {
        let boot = lines(&[
            "  iommu    1 VT-d units, 0 SMMUv3s; 6 PCI functions behind one, 0 bypassing, 0 unresolved",
            "  iommu    pci 0000:00:02.0 behind the VtD unit at 0xfed90000 as stream 0x10",
        ]);
        assert_eq!(iommu_problem(&boot), None);
    }

    #[test]
    fn a_boot_with_nothing_behind_an_iommu_or_anything_unresolved_fails() {
        let nothing = lines(&[
            "  iommu    0 VT-d units, 0 SMMUv3s; 0 PCI functions behind one, 2 bypassing, 0 unresolved",
        ]);
        assert!(iommu_problem(&nothing).is_some(), "nothing placed");
        let unresolved = lines(&[
            "  iommu    0 VT-d units, 1 SMMUv3s; 1 PCI functions behind one, 0 bypassing, 1 unresolved",
        ]);
        assert!(iommu_problem(&unresolved).is_some(), "one unresolved");
        let silent = lines(&["FERRIX-BOOT-OK stages 1-11"]);
        assert!(iommu_problem(&silent).is_some(), "no line at all");
    }

    #[test]
    fn a_translating_machine_must_show_its_out_of_domain_fault() {
        let faulted = lines(&[
            "  pci      2 functions, 64 entropy bytes read by DMA, 1 completions by MSI-X, 1 out-of-domain writes faulted",
        ]);
        assert_eq!(fault_problem(Arch::X86_64, &faulted), None);
        let none = lines(&[
            "  pci      2 functions, 64 entropy bytes read by DMA, 1 completions by MSI-X, 0 out-of-domain writes faulted",
        ]);
        assert!(
            fault_problem(Arch::AArch64, &none).is_some(),
            "nothing faulted"
        );
        let silent = lines(&["FERRIX-BOOT-OK stages 1-11"]);
        assert!(
            fault_problem(Arch::X86_64, &silent).is_some(),
            "no line at all"
        );
        assert_eq!(
            fault_problem(Arch::Armv7a, &none),
            None,
            "ARMv7-A is not asked"
        );
    }
}
