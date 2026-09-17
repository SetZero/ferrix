//! A very small argument parser.
//!
//! Hand-rolled rather than `clap`, for the reason given in `Cargo.toml`: this
//! is the program that builds the operating system, and its dependency list
//! should be short enough to read. The grammar is `command --flag --key value`,
//! which is all `cargo xtask` ever needs.

use crate::paths::Arch;
use crate::{Error, Result};

/// Parsed command line.
#[derive(Debug, Default, Clone)]
pub(crate) struct Args {
    /// The subcommand, if one was given.
    pub(crate) command: Option<String>,
    /// `--arch`, unparsed. `None` means "whatever this host is".
    pub(crate) arch: Option<String>,
    /// `--release`.
    pub(crate) release: bool,
    /// `--gdb`: stop and wait for a debugger before the first instruction.
    pub(crate) gdb: bool,
    /// `--fast`: skip the slow half of `check`.
    pub(crate) fast: bool,
    /// `--ferrousli`: `check` also runs ferrousli's gates, its own workspace.
    pub(crate) ferrousli: bool,
    /// `--zinc`: `check` also runs zinc's gates, its own workspace.
    pub(crate) zinc: bool,
    /// `--miri`: add CI's Miri steps to `check`.
    pub(crate) miri: bool,
    /// `--reset`: the image carries `ferrix.onexit=reset` in `CMDLINE.TXT`, and
    /// `test-boot` requires QEMU to see the machine reset rather than power off.
    pub(crate) reset: bool,
    /// `--net`: give the guest a virtio-net device, with `xtask`'s own gateway
    /// behind it. Off by default, because every boot that does not need a
    /// network is a boot with one fewer device on the bus and one fewer thread
    /// in this process.
    pub(crate) net: bool,
    /// `-h`/`--help`.
    pub(crate) help: bool,
    /// `--smp`, virtual CPUs.
    pub(crate) smp: u32,
    /// Whether `--smp` was given, rather than left at its default: under WHPX
    /// the default is one processor, and a count asked for is kept.
    pub(crate) smp_given: bool,
    /// `--memory`, guest RAM in MiB.
    pub(crate) memory: u32,
    /// `--timeout`, seconds `test-boot` waits for the kernel to report.
    pub(crate) timeout: u64,
    /// `--accel`, which QEMU accelerator to boot under. `None` means `tcg`,
    /// except to `run`, which asks for `auto` unless it is given `--gdb`.
    pub(crate) accel: Option<String>,
    /// `--to`, the mounted boot partition `flash` writes to. `None` means
    /// "find the only one".
    pub(crate) to: Option<String>,
    /// `--port`, the serial device `watch-serial` reads. `None` means "find
    /// the only one".
    pub(crate) port: Option<String>,
    /// `--init`, the program `test-shell` and `test-vfs` build in, and that
    /// `build` and `run` put in the kernel and at `/bin/busybox`. `{arch}` in it
    /// is replaced by each architecture's name, so one path serves `--arch all`.
    /// `ferrousli` names the busybox built against ferrousli, in `busybox.rs`.
    pub(crate) init: Option<String>,
    /// Where the gateway's `10.0.2.3:53` forwards to. No flag sets it: it is
    /// how `test-net` points the guest's DNS at the answers it serves itself,
    /// so that the test says the same thing on a machine with no network.
    /// `None` is the host's own resolver.
    pub(crate) resolver: Option<std::net::SocketAddrV4>,
    /// `--display`: a virtio-gpu device on the bus, and for `run` a window
    /// that shows it. `test-display` turns it on.
    pub(crate) display: bool,
    /// `--input`: a virtio keyboard and a virtio tablet on the bus, which
    /// QMP's `input-send-event` drives. `test-input` turns it on, and
    /// `--display` brings them as well.
    pub(crate) input: bool,
    /// Where QEMU serves QMP. No flag sets it: `test-display` picks a port to
    /// ask QEMU for its screendump over.
    pub(crate) qmp_port: Option<u16>,
}

impl Args {
    /// Parse an iterator of arguments, `cargo xtask` and the command name
    /// having already been stripped by the caller.
    pub(crate) fn parse(raw: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = Args {
            smp: 4,
            memory: 512,
            timeout: 120,
            ..Args::default()
        };

        let mut items = raw.peekable();
        while let Some(item) = items.next() {
            match item.as_str() {
                "-h" | "--help" => args.help = true,
                "--release" => args.release = true,
                "--gdb" => args.gdb = true,
                "--fast" => args.fast = true,
                "--ferrousli" => args.ferrousli = true,
                "--zinc" => args.zinc = true,
                "--miri" => args.miri = true,
                "--reset" => args.reset = true,
                "--net" => args.net = true,
                "--display" => args.display = true,
                "--input" => args.input = true,
                "--arch" => args.arch = Some(value(&mut items, "--arch")?),
                "--smp" => {
                    args.smp = number(&mut items, "--smp")?;
                    args.smp_given = true;
                }
                "--memory" => args.memory = number(&mut items, "--memory")?,
                "--timeout" => args.timeout = number(&mut items, "--timeout")?,
                "--accel" => args.accel = Some(value(&mut items, "--accel")?),
                "--to" => args.to = Some(value(&mut items, "--to")?),
                "--port" => args.port = Some(value(&mut items, "--port")?),
                "--init" => args.init = Some(value(&mut items, "--init")?),
                other if other.starts_with('-') => {
                    return Err(Error::new(format!("unknown option `{other}`")));
                }
                other if args.command.is_none() => args.command = Some(other.to_owned()),
                other => {
                    return Err(Error::new(format!("unexpected argument `{other}`")));
                }
            }
        }

        Ok(args)
    }

    /// The architectures this invocation applies to.
    ///
    /// `--arch all` is the reason this returns a list: a change that breaks
    /// one architecture and not the others is the characteristic failure of a
    /// multi-architecture kernel, and the default local workflow should catch
    /// it. `both` stays a synonym for `all`, so that a habit formed when there
    /// were two does not quietly drop the third.
    pub(crate) fn arches(&self) -> Result<Vec<Arch>> {
        match self.arch.as_deref() {
            None => Ok(vec![Arch::host()]),
            Some("both" | "all") => Ok(Arch::ALL.to_vec()),
            Some(name) => Ok(vec![Arch::parse(name)?]),
        }
    }

    /// The single architecture for a command that can only mean one.
    pub(crate) fn single_arch(&self) -> Result<Arch> {
        let arches = self.arches()?;
        match arches.as_slice() {
            [one] => Ok(*one),
            _ => Err(Error::new("this command needs exactly one --arch")),
        }
    }
}

/// Take the value following a `--key`.
fn value(items: &mut impl Iterator<Item = String>, key: &str) -> Result<String> {
    items
        .next()
        .ok_or_else(|| Error::new(format!("{key} needs a value")))
}

/// Take and parse a numeric value following a `--key`.
fn number<T: std::str::FromStr>(items: &mut impl Iterator<Item = String>, key: &str) -> Result<T> {
    let raw = value(items, key)?;
    raw.parse()
        .map_err(|_| Error::new(format!("{key} wants a number, got `{raw}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &[&str]) -> Result<Args> {
        Args::parse(line.iter().map(|item| (*item).to_owned()))
    }

    #[test]
    fn takes_a_command_and_its_options() {
        let args = parse(&[
            "test-boot",
            "--arch",
            "aarch64",
            "--release",
            "--timeout",
            "30",
        ])
        .unwrap();

        assert_eq!(args.command.as_deref(), Some("test-boot"));
        assert_eq!(args.single_arch().unwrap(), Arch::AArch64);
        assert!(args.release);
        assert_eq!(args.timeout, 30);
    }

    #[test]
    fn miri_is_off_unless_asked_for() {
        let args = parse(&["check", "--fast", "--miri"]).unwrap();
        assert!(args.fast && args.miri);
        let plain = parse(&["check"]).unwrap();
        assert!(!plain.fast && !plain.miri, "both are opt-in");
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let args = parse(&["build"]).unwrap();
        assert_eq!(args.smp, 4);
        assert!(!args.smp_given, "the default is not a count asked for");
        assert_eq!(args.memory, 512);
        assert_eq!(args.timeout, 120);
        assert!(!args.release);
    }

    #[test]
    fn arch_all_expands_to_every_architecture() {
        for spelling in ["all", "both"] {
            let args = parse(&["build", "--arch", spelling]).unwrap();
            assert_eq!(
                args.arches().unwrap(),
                vec![Arch::X86_64, Arch::AArch64, Arch::Armv7a],
                "`{spelling}` must not leave an architecture out"
            );
            assert!(
                args.single_arch().is_err(),
                "a command needing one architecture must refuse several"
            );
        }
    }

    #[test]
    fn accel_defaults_to_none_and_is_taken_verbatim() {
        assert_eq!(parse(&["run"]).unwrap().accel, None);
        assert_eq!(
            parse(&["run", "--accel", "whpx"]).unwrap().accel.as_deref(),
            Some("whpx")
        );
    }

    #[test]
    fn takes_the_board_options() {
        let args = parse(&[
            "deploy",
            "--to",
            "/media/sebastian/bootfs",
            "--port",
            "/dev/ttyACM0",
        ])
        .unwrap();
        assert_eq!(args.to.as_deref(), Some("/media/sebastian/bootfs"));
        assert_eq!(args.port.as_deref(), Some("/dev/ttyACM0"));
    }

    #[test]
    fn the_board_options_default_to_discovery() {
        let args = parse(&["flash"]).unwrap();
        assert_eq!(args.to, None, "no --to means find the only card");
        assert_eq!(args.port, None, "no --port means find the only port");
    }

    #[test]
    fn rejects_an_unknown_option() {
        assert!(parse(&["build", "--wat"]).is_err());
    }

    #[test]
    fn rejects_a_missing_or_unparseable_value() {
        assert!(parse(&["build", "--arch"]).is_err());
        assert!(parse(&["build", "--smp", "lots"]).is_err());
        let given = parse(&["run", "--smp", "2"]).expect("a count parses");
        assert!(
            given.smp_given && given.smp == 2,
            "a count asked for is kept and marked"
        );
    }

    #[test]
    fn rejects_a_second_bare_argument() {
        assert!(
            parse(&["build", "run"]).is_err(),
            "two commands is a typo, not a request"
        );
    }

    #[test]
    fn ferrousli_is_off_unless_asked_for() {
        assert!(!parse(&["check"]).unwrap().ferrousli);
        let args = parse(&["check", "--fast", "--ferrousli"]).unwrap();
        assert!(args.fast && args.ferrousli);
    }

    #[test]
    fn zinc_is_off_unless_asked_for() {
        assert!(!parse(&["check"]).unwrap().zinc);
        assert!(parse(&["check", "--zinc"]).unwrap().zinc);
    }

    #[test]
    fn reset_is_off_unless_asked_for() {
        assert!(!parse(&["test-boot"]).unwrap().reset);
        assert!(parse(&["test-boot", "--reset"]).unwrap().reset);
    }

    #[test]
    fn net_is_off_unless_asked_for() {
        assert!(
            !parse(&["run"]).unwrap().net,
            "a boot has no network device unless one was asked for"
        );
        assert!(
            parse(&["run", "--net"]).unwrap().net,
            "--net turns the device and the gateway on"
        );
    }
}
