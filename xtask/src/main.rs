//! Host-side driver for building, imaging and booting Ferrix.
//!
//! ```text
//! cargo xtask build     --arch x86_64 [--release] [--init PATH/{arch}/busybox]
//! cargo xtask run       --arch x86_64 [--release] [--gdb] [--smp N] [--memory M]
//!                       [--accel auto|tcg|whpx|kvm|hvf] [--init PATH/{arch}/busybox] [--net]
//! cargo xtask test-boot --arch x86_64 [--release] [--timeout SECONDS] [--reset] [--net]
//! cargo xtask test-shell --arch all --init PATH/{arch}/busybox [--timeout SECONDS]
//! cargo xtask test-vfs  --arch all --init PATH/{arch}/busybox [--timeout SECONDS]
//! cargo xtask test-threads --arch all [--timeout SECONDS]
//! cargo xtask test-rustc [--accel kvm] [--memory M] [--timeout SECONDS]
//! cargo xtask check     [--fast] [--ferrousli] [--zinc] [--miri]
//! cargo xtask remote-desktop [--host DEST] [--config PATH] [--vnc :N] [--send head]
//!                       [--viewer tigervnc|realvnc] [--layout de] [--no-viewer]
//!                       [--print-command] [--stop] [-- ARGS...]
//! cargo xtask busybox   [--arch x86_64]
//! cargo xtask ports     [--arch x86_64]
//! cargo xtask omz       --from DIRECTORY-OR-URL
//! cargo xtask zsh-functions --from DIRECTORY
//! cargo xtask flash     [--arch armv7a] [--to MOUNT]
//! cargo xtask watch-serial            [--port DEVICE] [--timeout SECONDS]
//! cargo xtask deploy    [--arch armv7a] [--to MOUNT] [--port DEVICE]
//! ```
//!
//! `build` compiles the loader and the kernel for their two different targets
//! and writes a bootable FAT32 image. `test-boot` boots that image under QEMU,
//! watches the serial port, and fails if the kernel does not report success —
//! which is the only test in this repository that can tell us the thing runs.
//!
//! `build` and `run` given a static busybox — `--init`, or the `FERRIX_INIT`
//! variable — build it into the kernel, which starts `sh -i` on the console,
//! and put it in the initramfs at `/bin/busybox` with every applet linked
//! beside it, so that the shell finds `ls` on its `PATH=/bin`.
//!
//! `omz` installs the oh-my-zsh checkout every image carries, from a directory
//! on this machine or a git repository to clone. Without it an image boots to a
//! shell with no configuration; with it, to the one oh-my-zsh gives.
//! `zsh-functions` installs zsh's own function tree -- `compinit`,
//! `is-at-least`, `add-zsh-hook` -- which oh-my-zsh calls and the image
//! carries beside it.
//!
//! `busybox` builds busybox against ferrousli, on Linux or natively on Windows,
//! and installs it for `--init ferrousli`, which names that binary instead of a
//! path and rebuilds it when it is older than ferrousli.

// AUDIT: this is a command-line build tool. Its output *is* stdout and stderr,
// and routing it through a logging facade would make `cargo xtask build` read
// like a service rather than like a build.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "AUDIT: xtask is a CLI build tool; the terminal is its interface"
)]

mod args;
mod btrfs_check;
mod btrfs_disk;
mod busybox;
mod cargo;
mod check;
mod compositor;
mod console;
mod display;
mod fat;
mod ferrousli;
mod flash;
mod gateway;
mod initramfs;
mod input;
mod jobs;
mod native;
mod net;
mod noise;
mod omz;
mod paths;
mod pe;
mod ports;
mod powerfail;
mod pty;
mod qemu;
mod remote;
mod rustc;
mod seat;
mod serial;
mod shell;
mod ssh;
mod symbolize;
mod test_disk;
mod threads;
mod uutils;
mod vfs;
mod wallpaper;
mod window;
mod workspace;
mod wsl;
mod zinc;

use std::path::PathBuf;
use std::process::ExitCode;

use args::Args;
use paths::Arch;

/// What went wrong, in a form that can be printed and turned into an exit code.
#[derive(Debug)]
pub(crate) struct Error {
    /// Human-readable description, already contextualised.
    message: String,
}

impl Error {
    /// Build an error from anything printable.
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::new(error.to_string())
    }
}

/// The result type every xtask step returns.
pub(crate) type Result<T> = std::result::Result<T, Error>;

const USAGE: &str = "\
Ferrix build driver

USAGE:
    cargo xtask <COMMAND> [OPTIONS]

COMMANDS:
    build         Compile the loader and kernel and write a bootable image
    run           Boot the image under QEMU, attached to the terminal
    run-compositor  Boot compositor/hyprix as init with a virtio-gpu, on a screen this host can show
    remote-desktop  Send this tree to another machine, boot the desktop there and watch it here over VNC
    wallpapers    Convert pictures for run-compositor's desktop and keep them on this machine
    test-btrfs    Boot, write a tree on the blank btrfs disk, and require host btrfs check to find nothing
    test-powerfail  Kill QEMU while it writes btrfs, replay at the next boot, and require btrfs check to pass, --seeds times
    test-boot     Boot the image under QEMU and assert the kernel came up
    test-shell    Boot with a static busybox built in and require its script's output
    test-vfs      Boot with busybox in the initramfs and require stage 8's exit programs and applets
    test-net      Boot with a network device and require busybox to configure it and fetch a file
    test-display  Boot compositor/blank as init with a virtio-gpu, and require its colour on every pixel
    test-compositor  Boot compositor/hyprix as init with a virtio-gpu, and require its background on every pixel
    test-video    Boot a wallpaper that moves and require the screen to show its frames in turn
    test-input    Boot compositor/evecho as init with virtio-input, send a key and a touch over QMP, and require them back
    test-seat     Boot the compositor with a client, type into it over QMP, and require the key and the keybind to land
    test-pty      Boot compositor/term as init, run a program on a pseudoterminal, and require its output back
    test-jobs     Boot an interactive shell on the console, type a session with jobs at it, and require the answers
    test-threads  Boot threads-test as init and require std::thread, Mutex, mpsc and /proc's thread count
    test-rustc    Attach the rustc volume scripts/fetch-rustc-sysroot.sh makes, run `rustc hello.rs && ./hello`
    check         Run every quality gate (fmt, clippy, layering, audits, tests, docs)
    host-clippy   check's host clippy step alone, as CI runs it
    host-test     check's host test step alone, as CI runs it
    host-doctest  check's doc test step alone, as CI runs it
    host-doc      check's documentation step alone, as CI runs it
    native-clippy check's clippy of the native programs for --arch, as CI runs it
    model-doc     Regenerate docs/generated/ from the SysML model
    busybox       Build busybox against ferrousli (x86_64) for --init ferrousli
    uutils        Build uutils/coreutils against ferrousli (x86_64), the utilities replacing busybox's
    ports         Build the programs ported onto ferrousli (x86_64: curl, btop), which images carry beside busybox
    flash         Copy the loader and kernel onto a board's boot partition
    watch-serial  Watch a real serial port for the kernel's boot report
    deploy        flash, then watch-serial: one command for a board

OPTIONS:
    --arch <x86_64|aarch64|armv7a|all>   Target architecture   [default: host]
    --release                            Build with optimisations
    --smp <N>                            Virtual CPUs          [default: 4; 1 under whpx]
    --memory <MiB>                       Guest memory          [default: 512]
    --timeout <SECONDS>                  test-boot patience    [default: 120]
    --seeds <N>                          test-powerfail cuts   [default: 8]
    --accel <auto|tcg|whpx|kvm|hvf>      QEMU accelerator      [default: auto for run
                                         without --gdb, tcg otherwise]
    --gdb                                Wait for a debugger on :1234
    --net                                run, test-boot, test-shell, test-vfs, run-compositor:
                                         a virtio-net device,
                                         behind xtask's own NAT gateway (10.0.2.2, guest 10.0.2.15);
                                         test-net turns it on whether or not it is given;
                                         run-compositor has one unless --no-net
    --no-net                             run-compositor: no network device and no gateway
    --forward <HOST>:<GUEST>             the host's 127.0.0.1:HOST leads to the guest's port GUEST,
                                         e.g. 2222:22 for sshdt; repeatable; turns --net on
    --ssh <PORT>                         run-compositor: start sshdt in the guest, reached at
                                         127.0.0.1:PORT; the keys in ~/.ssh may log in, and so may
                                         ~/.local/share/ferrix/ssh/id_ed25519, which the boot prints
    --ssh-key <FILE|KEY>                 another public key that may log in, as a file or written
                                         out; repeatable; for a client whose key is in neither place
    --display                            run, test-boot: a virtio-gpu device; run: and a window showing it
    --gl                                 that virtio-gpu is the 3D card, `virtio-gpu-gl-pci`, with the
                                         host's GPU behind it through virglrenderer; turns --display on.
                                         A QEMU built without it says so and the 2D card is used
                                         [FERRIX_QEMU names a QEMU that is not the one on PATH: a
                                         directory of its binaries, or one binary]
    --clipboard                          run, run-compositor: a virtio-serial port carrying SPICE's
                                         agent protocol, with QEMU's own host half behind it, for a
                                         clipboard shared with whoever is watching. The device only:
                                         no guest driver or agent exists yet, so nothing is shared
                                         today (docs/CLIPBOARD.md §8)
    --vnc <DISPLAY>                      run --display, run-compositor: serve the screen over VNC
                                         at e.g. `:0` (127.0.0.1) rather than in a window of this host's
    --rendernode <PATH>                  --gl on a served or headless screen: which GPU egl-headless
                                         draws on, e.g. /dev/dri/renderD128. QEMU takes the first
                                         render node otherwise, which on a machine with two GPUs is a
                                         guess. A window's GL goes to the host display's GPU regardless
    --config <PATH>                      run-compositor: the hyprland.conf the guest is given;
                                         remote-desktop: the file of answers to read instead of
                                         the ones searched [or $FERRIX_REMOTE]
    --host <DESTINATION>                 remote-desktop: the machine to boot on, as `ssh` names
                                         one, over what the config file said. No host is a
                                         default anywhere in xtask; this and that file are the
                                         only two ways one is named
    --local-port <N>                     remote-desktop: the port the tunnel listens on here
                                         [default: 5900 + the display, or the next one free]
    --send <working-tree|head>           remote-desktop: boot what you are looking at,
                                         uncommitted changes and all, or your last commit
                                         [default: working-tree]
    --no-viewer                          remote-desktop: open the tunnel and nothing else
    --viewer <tigervnc|realvnc|auto|none>
                                         remote-desktop: the viewer to open, over the config
                                         file's. TigerVNC sends keys and the guest's layout reads
                                         them; RealVNC sends characters, so the boot is given
                                         --keymap <the first --layout> as well
    --keymap <NAME>                      run --display, run-compositor over VNC: QEMU's keymap for
                                         a viewer that sends characters rather than keys (`de`,
                                         `us`, `en-gb`). Not for one that sends keys: QEMU would
                                         translate those through it too
    --print-command                      remote-desktop: say what would be sent, run and opened,
                                         and do none of it
    --stop                               remote-desktop: end a boot left running over there, and
                                         do nothing else
    --                                   remote-desktop: everything after this goes to the
                                         remote `cargo xtask` as it stands
    --layout <LIST>                      run-compositor: the keyboard layout, as input:kb_layout
                                         takes it: `de`, or `de,us` for two a switch moves between
    --variant <LIST>                     run-compositor: their variants, as input:kb_variant
                                         takes them: `nodeadkeys,` is one for the first layout only
    --wallpaper <NAME>                   run-compositor: which kept wallpaper to show, by part of its
                                         name, or `none`; one of them, another each run, otherwise
    --from <WHERE>                       wallpapers: where the pictures are, a directory here or
                                         <host>:<directory> for one `ssh` reaches
    --size <W>x<H>                       run-compositor: the guest's screen; wallpapers: the screen
                                         to cut pictures for (1920x1080 for both)
    --fast                               check: skip the cross-target clippy passes
    --ferrousli                          check: also ferrousli's fmt, clippy and tests, debug and release
    --zinc                               check: also zinc's fmt, clippy, tests and pty completion test
    --miri                               check: add CI's Miri steps (needs nightly and miri)
    --reset-root                         run, run-compositor: start the btrfs root over from a fresh install
    --tmpfs-root                         run, run-compositor: / in memory instead of on the btrfs root disk
    --reset                              test-boot: ferrix.onexit=reset in CMDLINE.TXT, and require a reset;
                                         build, run: put that CMDLINE.TXT in the image
    --to <MOUNT>                         flash: the card's mounted boot partition
    --port <DEVICE>                      watch-serial: e.g. /dev/ttyACM0
    --init <PATH|ferrousli>              The busybox; {arch} is replaced. build, run, flash, deploy: [or FERRIX_INIT]
                                         start `sh -i`, with the applets linked in /bin.
                                         `ferrousli`: the x86_64 busybox built against ferrousli, rebuilt when stale
    --interpreter <PATH|ferrousli>       test-shell: a dynamic linker, carried at --init's PT_INTERP path; {arch} is replaced
                                         `ferrousli`: ferrousli's ld-ferrousli, built from this tree
    --library <PATH|ferrousli>           test-shell: a shared library, carried in /lib; as many as needed; {arch} is replaced
                                         `ferrousli`: ferrousli linked as libc.so.6, built from this tree
    --boot <NAME>                        test-compositor: only the boots whose name holds this
    -h, --help                           This message
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\nxtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;

    if args.help {
        println!("{USAGE}");
        return Ok(());
    }

    let Some(command) = args.command.as_deref() else {
        println!("{USAGE}");
        return Err(Error::new("no command given"));
    };

    match command {
        "build" => {
            for arch in args.arches()? {
                let (image, _) = build_image(arch, &args)?;
                println!("built {}", image.display());
            }
            Ok(())
        }
        "run" => {
            let arch = args.single_arch()?;
            let (image, _) = build_image(arch, &args)?;
            // Someone at the console wants the machine in front of them, so
            // `run` takes whatever hypervisor it has: WHPX on Windows, KVM on
            // Linux, HVF on macOS, emulation where there is none. The tests
            // keep `tcg`, for the reason `qemu::accelerator` gives, and so
            // does a debugging session, whose breakpoints and single steps
            // `tcg` honours on every host and the hypervisors do not.
            let args = Args {
                accel: args
                    .accel
                    .clone()
                    .or_else(|| (!args.gdb).then(|| "auto".to_owned())),
                ..args
            };
            qemu::run(arch, &image, &args)
        }
        "test-btrfs" => btrfs_check::test_btrfs(&args, |arch| build_image(arch, &args)),
        "test-powerfail" => powerfail::test_powerfail(&args, |arch| build_parts(arch, &args)),
        "test-boot" => test_boot(&args),
        "test-shell" => {
            for arch in args.arches()? {
                // The shell the kernel starts. zinc, the shell this tree has,
                // unless `--init` names another: the same script under a
                // static busybox is what measures the ABI against somebody
                // else's binary, and both are worth running.
                let program = match args.init.as_deref() {
                    Some(init) => program_for(init, arch)?,
                    None => zinc::built(arch)?.ok_or_else(|| {
                        Error::new(format!(
                            "zinc is not built for {arch}; give --init PATH, a static \
                             shell for each architecture, instead"
                        ))
                    })?,
                };
                let loader = cargo::build_loader(arch, args.release)?;
                let kernel =
                    cargo::build_kernel_with_init(arch, args.release, &program, shell::SCRIPT)?;
                let natives = native::build(arch, args.release)?;
                // A dynamically linked shell's linker and libraries, in the
                // initramfs where the kernel and the linker will look.
                let carried = shell::carried_for(arch, &program, &args)?;
                let initramfs = initramfs::build(None, &natives, None, &carried)?;
                let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
                qemu::test_shell(arch, &image, &kernel, &args)?;
            }
            Ok(())
        }
        "test-vfs" => test_vfs(&args),
        "test-net" => test_net(&args),
        "test-display" => display::test_display(&args),
        "run-compositor" => compositor::run_compositor(&args),
        "remote-desktop" => remote::remote_desktop(&args),
        "wallpapers" => wallpaper::import(&args),
        "test-compositor" => compositor::test_compositor(&args),
        "test-video" => compositor::test_video(&args),
        "test-input" => input::test_input(&args),
        "test-seat" => seat::test_seat(&args),
        "test-pty" => pty::test_pty(&args),
        "test-jobs" => jobs::test_jobs(&args),
        "test-threads" => threads::test_threads(&args),
        "test-rustc" => rustc::test_rustc(&args),
        "check" => check::run(&args),
        "host-clippy" => check::host_clippy(),
        "host-test" => check::host_test(),
        "host-doctest" => check::host_doctest(),
        "host-doc" => check::host_doc(),
        "native-clippy" => args
            .arches()?
            .into_iter()
            .try_for_each(check::native_clippy),
        "model-doc" => check::model_doc(),
        "busybox" => busybox::build(args.single_arch()?).map(|_| ()),
        "uutils" => uutils::build(args.single_arch()?).map(|_| ()),
        "ports" => ports::build(args.single_arch()?),
        "omz" => omz::install(args.from.as_deref()),
        "zsh-functions" => omz::install_functions(args.from.as_deref()),
        "flash" => {
            let arch = args.single_arch()?;
            let (loader, kernel, initramfs) = build_board_files(arch, &args)?;
            flash::run(arch, &loader, &kernel, &initramfs, &args)
        }
        "watch-serial" => serial::watch(None, &args),
        // The whole of a board round-trip. Separate commands exist because
        // each half is useful alone — reflashing without watching, watching a
        // board someone else reset — but the common case is both, and a
        // command per step is a command per step to forget.
        "deploy" => {
            let arch = args.single_arch()?;
            let (loader, kernel, initramfs) = build_board_files(arch, &args)?;
            flash::run(arch, &loader, &kernel, &initramfs, &args)?;
            serial::watch(Some(&kernel), &args)
        }
        other => Err(Error::new(format!("unknown command `{other}`\n\n{USAGE}"))),
    }
}

/// `test-vfs`: stage 8's exit programs, then its applets, on each
/// architecture asked for.
///
/// The program goes into the initramfs, at `/bin/busybox`, rather than into
/// the kernel: loading it from a file is part of what stage 8 is for. Every
/// architecture is run even after one fails, because which of the three a
/// missing call breaks is the report.
/// `test-boot`: boot each architecture asked for and require the marker.
fn test_boot(args: &Args) -> Result<()> {
    for arch in args.arches()? {
        let (image, kernel) = build_image(arch, args)?;
        qemu::test_boot(arch, &image, &kernel, args)?;
    }
    Ok(())
}

fn test_vfs(args: &Args) -> Result<()> {
    let init = args.init.as_deref().ok_or_else(|| {
        Error::new(
            "test-vfs needs --init PATH, a static busybox for each architecture; \
             `{arch}` in the path is replaced by the architecture's name",
        )
    })?;
    let mut failed = Vec::new();
    for arch in args.arches()? {
        // Per architecture, because only the one that carries uutils runs the
        // commands that need it.
        let commands = vfs::encode(&vfs::commands(arch))?;
        let program = program_for(init, arch)?;
        let loader = cargo::build_loader(arch, args.release)?;
        let list = paths::build_dir(arch).join("init-commands");
        vfs::write_if_changed(&list, &commands)?;
        let kernel = cargo::build_kernel_with_commands(arch, args.release, &list)?;
        let natives = native::build(arch, args.release)?;
        // zinc too: the permissions commands run a set-user-id copy of it,
        // which is the one program in the image that shows an effective id.
        let shell = zinc::build(arch)?;
        let utilities = uutils::carried(arch)?;
        let initramfs = initramfs::build_with_utilities(
            Some(&program),
            &natives,
            shell.as_deref(),
            &utilities,
            &ports::installed(arch)?,
        )?;
        let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
        if let Err(error) = qemu::test_vfs(arch, &image, &kernel, args) {
            eprintln!("\n  {error}");
            failed.push(arch.name());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "stage 8's exit programs or applets failed on {}",
            failed.join(", ")
        )))
    }
}

/// `test-net`: the networking exit criterion, on each architecture asked for.
///
/// The servers the guest fetches from are threads of this process, bound to
/// the host's loopback on ports its kernel chose, so they are started before
/// the guest's programs are written down: the ports are in the arguments. The
/// gateway's DNS forwarder is pointed at the stub for the run, and `--net` is
/// on whether or not it was asked for, since a network test without a network
/// device is a test of nothing.
fn test_net(args: &Args) -> Result<()> {
    let init = args.init.as_deref().ok_or_else(|| {
        Error::new(
            "test-net needs --init PATH, a static busybox for each architecture; \
             `{arch}` in the path is replaced by the architecture's name",
        )
    })?;
    let servers = net::Servers::start()?;
    println!("  host: {}", servers.describe());
    let args = Args {
        net: true,
        resolver: Some(servers.dns()),
        ..args.clone()
    };
    let mut failed = Vec::new();
    for arch in args.arches()? {
        let program = program_for(init, arch)?;
        // The ports ride along where they are built, and the programs that
        // exercise them are added when they do.
        let ports = ports::installed(arch)?;
        let curl = ports.iter().any(|file| file.path == "bin/curl");
        let git = ports.iter().any(|file| file.path == "usr/bin/git");
        let programs = net::commands(&servers, curl, git);
        let commands = vfs::encode(&programs)?;
        let loader = cargo::build_loader(arch, args.release)?;
        let list = paths::build_dir(arch).join("net-commands");
        vfs::write_if_changed(&list, &commands)?;
        let kernel = cargo::build_kernel_with_commands(arch, args.release, &list)?;
        let natives = native::build(arch, args.release)?;
        let initramfs = initramfs::build(Some(&program), &natives, None, &ports)?;
        let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, None)?;
        if let Err(error) = qemu::test_net(arch, &image, &kernel, &programs, &args) {
            eprintln!("\n  {error}");
            failed.push(arch.name());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "the networking programs failed on {}",
            failed.join(", ")
        )))
    }
}

/// The command line an image carries: `ferrix.onexit=reset` under `--reset`, so
/// the machine resets when boot ends and `test-boot` can require that it did.
fn image_cmdline(args: &Args) -> Option<&'static str> {
    args.reset.then_some(qemu::RESET_CMDLINE)
}

/// Compile both halves for `arch` and assemble the bootable image.
///
/// The kernel ELF comes back beside the image, because it is what a panic
/// report's backtrace is resolved against: the image holds the same kernel
/// with nothing to look a symbol up in.
///
/// Given a program — `--init`, or `FERRIX_INIT` when that is absent — the
/// kernel is built with it to start `sh -i`, and the initramfs carries it at
/// `/bin/busybox` with a link beside it for every applet, so that the shell's
/// `PATH=/bin` finds `ls` where a person types it; uutils/coreutils rides
/// along at `/bin/coreutils`, its own names linked in `/usr/bin`, on the one
/// architecture it is built for. Without a program the image is the one it
/// always was. Either way the initramfs carries the tree's native
/// programs in `/sbin`, built and checked for `arch` first.
fn build_image(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf)> {
    let natives = native::build(arch, args.release)?;
    let Some(program) = optional_program(arch, args)? else {
        let (loader, kernel) = build_halves(arch, args)?;
        let image = fat::write_image(arch, &loader, &kernel, &natives, image_cmdline(args))?;
        return Ok((image, kernel));
    };
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &program, "")?;
    let shell = zinc::build(arch)?;
    let utilities = uutils::carried(arch)?;
    let initramfs = initramfs::build_with_utilities(
        Some(&program),
        &natives,
        shell.as_deref(),
        &utilities,
        &ports::installed(arch)?,
    )?;
    let image = fat::write_image_with(arch, &loader, &kernel, &initramfs, image_cmdline(args))?;
    Ok((image, kernel))
}

/// Compile what `flash` copies onto a board: the loader, the kernel and the
/// initramfs.
///
/// Given a program the kernel starts `sh -i` and the archive carries busybox,
/// exactly as [`build_image`] arranges for QEMU, so a card boots to the shell
/// an image does. Without one the files are the ones they always were. Either
/// way the archive carries the tree's native programs, as an image's does.
fn build_board_files(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf, Vec<u8>)> {
    let natives = native::build(arch, args.release)?;
    let Some(program) = optional_program(arch, args)? else {
        let (loader, kernel) = build_halves(arch, args)?;
        return Ok((loader, kernel, initramfs::build(None, &natives, None, &[])?));
    };
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &program, "")?;
    let shell = zinc::build(arch)?;
    let utilities = uutils::carried(arch)?;
    let initramfs = initramfs::build_with_utilities(
        Some(&program),
        &natives,
        shell.as_deref(),
        &utilities,
        &ports::installed(arch)?,
    )?;
    Ok((loader, kernel, initramfs))
}

/// The program `init` names for `arch`, with `{arch}` replaced by its name,
/// refused unless it is a file.
///
/// `ferrousli` is not a path: it names the busybox `cargo xtask busybox`
/// installs, built first when it is missing or older than ferrousli. Nor is
/// `blank`, the compositor's first program, which is built here.
fn program_for(init: &str, arch: Arch) -> Result<PathBuf> {
    if init == busybox::INIT_NAME {
        return busybox::program(arch);
    }
    if init == display::INIT_NAME {
        return display::build_blank(arch, false);
    }
    let program = PathBuf::from(init.replace("{arch}", arch.name()));
    if !program.is_file() {
        return Err(Error::new(format!(
            "no program at {} for {arch}",
            program.display()
        )));
    }
    Ok(program)
}

/// The program `build`, `run` and `test-boot` were given, if any: `--init`,
/// or else a non-empty `FERRIX_INIT`, the variable that has always chosen the
/// shell those commands embed.
pub(crate) fn optional_program(arch: Arch, args: &Args) -> Result<Option<PathBuf>> {
    let init = match (&args.init, std::env::var("FERRIX_INIT")) {
        (Some(init), _) => init.clone(),
        (None, Ok(init)) if !init.is_empty() => init,
        (None, Ok(_) | Err(std::env::VarError::NotPresent)) => return Ok(None),
        (None, Err(std::env::VarError::NotUnicode(_))) => {
            return Err(Error::new("FERRIX_INIT is not valid UTF-8"));
        }
    };
    program_for(&init, arch).map(Some)
}

/// Compile both halves for `arch`, without assembling an image.
///
/// A board has its own filesystem already, put there by the vendor's firmware,
/// so what it wants is the two files rather than something to write over the
/// card with.
/// What [`build_image`] assembles for a boot without a program, before it
/// is assembled: `test-powerfail` writes one image per boot from them, each
/// with its own command line.
fn build_parts(arch: Arch, args: &Args) -> Result<powerfail::Built> {
    let natives = native::build(arch, args.release)?;
    let (loader, kernel) = build_halves(arch, args)?;
    let initramfs = initramfs::build(None, &natives, None, &[])?;
    Ok(powerfail::Built {
        loader,
        kernel,
        initramfs,
    })
}

fn build_halves(arch: Arch, args: &Args) -> Result<(PathBuf, PathBuf)> {
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel(arch, args.release)?;
    Ok((loader, kernel))
}
