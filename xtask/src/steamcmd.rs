//! `test-steamcmd`: Valve's steamcmd on Ferrix, logging in to Steam
//! anonymously (`docs/I386.md`, I5a).
//!
//! steamcmd is the Steam client's core without its display: a 32-bit glibc
//! program, run here by Debian's i386 `ld-linux.so.2` in compatibility mode on
//! the x86-64 kernel. It updates itself from Valve's servers on its first run,
//! exits 42 to be started again as its own `steamcmd.sh` would, and then
//! connects to Steam, fetches the client configuration and its user's
//! information, and quits. Each of those steps prints a line, and the last
//! one, "Waiting for user info...OK", says the login was whole.
//!
//! # The volume, and the network
//!
//! `scripts/fetch/fetch-steamcmd.sh` makes the volume: Debian's `libc6:i386`
//! and steamcmd, unpacked and not yet updated. It carries no `ferrix-root`
//! label, so the kernel mounts it at `/data`, attached under QEMU's
//! `snapshot=on`, so the update steamcmd writes beside itself is thrown away
//! with the run and every run starts from the same files. glibc names its
//! paths absolutely, so the initramfs links them into `/data` ([`LINKS`]).
//!
//! # Under KVM
//!
//! Steam's runtime reads the processor's clock rate from cpufreq or from
//! `/proc/cpuinfo`'s `cpu MHz`, and stops without it ("Unable to determine
//! CPU Frequency"). Ferrix prints `cpu MHz` when its counter is the TSC, whose
//! rate it measured against the HPET; under TCG the counter is the HPET
//! itself, whose rate says nothing about the processor's, and the line is not
//! printed rather than wrong. So the gate boots under KVM unless `--accel`
//! names another accelerator, and under TCG it fails at that line.
//!
//! # The internet
//!
//! This is the one gate that needs the internet: the boot runs with `--net`,
//! through xtask's gateway, and steamcmd talks to Valve. A failure to reach
//! Valve is a failure here, which is why the gate is run on demand, as
//! `test-chrome` is, and not in the image row.

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, busybox, cargo, fat, initramfs, native, qemu, rustc, shell, zinc};

/// glibc's i386 paths, each a link into the volume: the loader where
/// steamcmd's `PT_INTERP` names it, and the directory it searches; and the
/// certificate bundle its OpenSSL opens.
const LINKS: &[(&str, &str)] = &[
    (
        "lib/ld-linux.so.2",
        "/data/usr/lib/i386-linux-gnu/ld-linux.so.2",
    ),
    ("lib/i386-linux-gnu", "/data/usr/lib/i386-linux-gnu"),
    ("usr/lib/i386-linux-gnu", "/data/usr/lib/i386-linux-gnu"),
    (
        "etc/ssl/certs/ca-certificates.crt",
        "/data/etc/ssl/certs/ca-certificates.crt",
    ),
];

/// The script: a lease for `eth0` from the gateway, as `test-net` asks for
/// one, then steamcmd as `steamcmd.sh` runs it, started again while it exits
/// 42 after installing an update, at most five times.
const SCRIPT: &str = r#"export PATH=/bin HOME=/tmp
udhcpc -i eth0 -n -q -t 5 -T 2 || exit 5
cd /data/steamcmd || exit 3
export LD_LIBRARY_PATH=/data/steamcmd/linux32
status=42
runs=0
while [ $status -eq 42 ] && [ $runs -lt 5 ]; do
    runs=$((runs + 1))
    ./linux32/steamcmd +login anonymous +quit
    status=$?
    echo "steamcmd-gate: run $runs exited $status"
done
[ $status -eq 0 ] || exit 4
echo steamcmd-gate: logged in
exit 17
"#;

/// What the script exits with when steamcmd logged in and quit.
const STATUS: i32 = 17;

/// The script's own line once steamcmd has exited 0.
const LOGGED_IN: &str = "steamcmd-gate: logged in";

/// steamcmd's line for the login's last step. Its terminal colour codes sit
/// between the words and the `OK`, so the two are looked for separately.
const USER_INFO: &str = "Waiting for user info";

/// Memory for the guest: steamcmd unpacks its update in memory first.
const MEMORY: u32 = 2048;

/// Seconds for the boot: two updates downloaded through the gateway, and
/// unpacked, under TCG.
const TIMEOUT: u64 = 3600;

/// Where `scripts/fetch/fetch-steamcmd.sh` writes, unless
/// `FERRIX_STEAMCMD_VOLUME` names another directory.
fn volume() -> Result<std::path::PathBuf> {
    let directory = match std::env::var_os("FERRIX_STEAMCMD_VOLUME") {
        Some(directory) => std::path::PathBuf::from(directory),
        None => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or_else(|| Error::new("neither HOME nor USERPROFILE is set"))?;
            std::path::PathBuf::from(home).join(".local/share/ferrix/steamcmd")
        }
    };
    let image = directory.join("steamcmd.img");
    if !image.is_file() {
        return Err(Error::new(format!(
            "{} is not there: scripts/fetch/fetch-steamcmd.sh makes it",
            image.display()
        )));
    }
    Ok(image)
}

/// Boot a shell whose script runs steamcmd until it has logged in.
///
/// # Errors
///
/// When the volume is missing, the image cannot be built, the boot fails, or
/// steamcmd did not log in and quit.
pub(crate) fn test_steamcmd(args: &Args) -> Result<()> {
    let arch = match args.arches()?.as_slice() {
        [Arch::X86_64] => Arch::X86_64,
        _ => {
            return Err(Error::new(
                "test-steamcmd runs on x86-64: steamcmd is a 32-bit x86 program",
            ));
        }
    };
    let mut args = args.clone();
    args.data_image = Some(volume()?);
    args.net = true;
    // Under a hardware accelerator unless told otherwise: steamcmd needs the
    // processor's clock rate, which the kernel knows only from the TSC it has
    // measured, and under TCG its counter is the HPET (see the module's
    // header).
    if args.accel.is_none() {
        args.accel = Some("kvm".to_owned());
    }
    if !args.memory_given {
        args.memory = MEMORY;
    }
    if !args.timeout_given {
        args.timeout = TIMEOUT;
    }

    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose shell runs steamcmd on Debian's i386 glibc");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, SCRIPT)?;
    let natives = native::build(arch, args.release)?;
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    // The project's busybox for `udhcpc` and the `ip` its lease script runs;
    // zinc stays the shell.
    let busybox = busybox::program(arch)?;
    let archive = initramfs::build(Some(&busybox), &natives, Some(&bytes), &rustc::files(LINKS))?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!(
        "  {arch}: running steamcmd on Ferrix with {} MiB (timeout {}s), on the network",
        args.memory, args.timeout
    );
    let lines = qemu::watch_then(arch, &image, &kernel, &args, shell::EXITED, |_| Ok(()))?;
    judge(arch, &lines)
}

/// Whether the script's lines say steamcmd logged in and quit.
fn judge(arch: Arch, lines: &[String]) -> Result<()> {
    let after_boot = lines
        .iter()
        .position(|line| line.contains(qemu::SUCCESS_MARKER))
        .and_then(|at| lines.get(at..))
        .unwrap_or_default();
    let exited = after_boot
        .iter()
        .find_map(|line| line.trim().strip_prefix(shell::EXITED))
        .map(str::trim);
    let user_info = after_boot
        .iter()
        .any(|line| line.contains(USER_INFO) && line.trim_end().ends_with("OK"));
    let logged_in = after_boot
        .iter()
        .any(|line| line.trim_end().ends_with(LOGGED_IN));
    match exited {
        Some(status) if status == STATUS.to_string() && user_info && logged_in => {
            println!("  {arch}: steamcmd updated itself, logged in to Steam anonymously and quit");
            Ok(())
        }
        Some("3") => Err(Error::new(format!(
            "{arch}: /data/steamcmd is not there: is the volume attached?"
        ))),
        Some("5") => Err(Error::new(format!(
            "{arch}: udhcpc got no lease for eth0 from the gateway"
        ))),
        Some("4") => Err(Error::new(format!(
            "{arch}: steamcmd did not exit 0; its `steamcmd-gate: run` lines say how it ended"
        ))),
        other => Err(Error::new(format!(
            "{arch}: the steamcmd script ended with {other:?}, and steamcmd's user-info line \
             was {}",
            if user_info { "there" } else { "missing" }
        ))),
    }
}
