//! The boots of the desktop a person uses: fuzzel against its own
//! `fuzzel.ini`, the user's own launcher bind, and `run-compositor
//! --everything`'s desktop with this machine's real `hyprland.conf`.
//!
//! The last two carry files off this machine, so they skip, and say why,
//! where it has none. Their desktop is not the tree's, so what they require
//! is what the compositor lists and starts, not a picture.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{
    Carried, EITHER, MARKER, Programs, SETTLE, Wanted, boot_and_dump_carrying, build,
    desktop_programs, everything_config, judge_still_running, judged_image, press, say_the_marker,
    with_chrome, with_the_transcript,
};
use crate::args::Args;
use crate::display::{DEVICE_ID, Qmp, free_port};
use crate::paths::{self, Arch};
use crate::qemu::Watching;
use crate::{Error, Result};

/// A twenty-fifth boot: fuzzel, the launcher, against its own `fuzzel.ini`.
///
/// It is started as the desktop comes up, and three pictures are required:
/// fuzzel over the empty screen with every entry listed and the first
/// selected; the list after `/bin/vkbd` has typed `pat` through
/// `zwp_virtual_keyboard_v1`, with the test pattern ranked first and
/// selected; and after `vkbd` has pressed Return, fuzzel gone and the
/// pattern's window it started tiled alone. The first two are the pictures
/// `src/user/linux/compositor/fuzzel`'s own host test makes of the same frames -- fuzzel's
/// drawing composited by `src/user/linux/compositor/render` as the compositor composites a
/// layer surface -- so a launcher that drew one pixel differently on Ferrix
/// fails here.
pub(super) fn test_fuzzel(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let fuzzel = build(arch, "compositor-fuzzel", "fuzzel")?;
    let carried = Carried {
        ports: crate::fuzzel::boot_files(&fuzzel)?,
        ..Carried::none()
    };
    let (screens, said) = boot_and_dump_carrying(
        arch,
        programs,
        crate::fuzzel::BOOT_CONFIG,
        (carried, None),
        &Wanted {
            states: &crate::fuzzel::EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &crate::fuzzel::AWAITING,
        },
        &crate::fuzzel::BINDS,
        args,
    )?;
    crate::fuzzel::judge(arch, screens.len(), &said)
}

/// A boot of the user's own launcher bind: `SUPER+R` in their real
/// `hyprland.conf`, which runs their `hypr-launcher` script, which runs
/// "their" fuzzel -- `crate::dotfiles` carries the script unchanged and links
/// the fuzzel it names to `/bin/fuzzel`.
///
/// Their desktop is not the tree's, so no picture can be required of it:
/// what is required is that after the keypress `hyprctl layers` lists a
/// surface in fuzzel's namespace, `launcher`. The script's own toggle, a
/// second press running `pkill -x fuzzel`, is not asked (docs/BACKLOG.md). The
/// screen with fuzzel on it is kept in the build directory as
/// `fuzzel-user.ppm`. A machine without the user's file, or whose file
/// binds no `#!/bin/sh` launcher, skips the boot and says so.
pub(super) fn test_fuzzel_user(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let Some((config, carried, script)) = fuzzel_user_setup(arch)? else {
        return Ok(());
    };
    let (image, kernel) = judged_image(arch, programs, &config, carried, None, args)?;
    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("fuzzel-user.ppm");
    let mut said = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        watching.stop_when_done();
        said = drive_fuzzel_user(arch, port, &dump, watching)?;
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    judge_still_running(arch, &said)?;
    if !said
        .iter()
        .any(|line| line.contains(&format!("started {script}")))
    {
        return Err(Error::new(format!(
            "{arch}: hyprix never said it started {script}"
        )));
    }
    println!(
        "  {arch}: the user's SUPER R ran their own {script} unchanged, which opened fuzzel on their \
         fuzzel.ini"
    );
    Ok(())
}

/// The user's real config, the image's ports carrying their launcher script
/// and a built fuzzel, and the script's absolute path -- or `None` when the
/// machine has no such config or it binds no `#!/bin/sh` launcher, in which
/// case [`test_fuzzel_user`] skips the boot and says why.
fn fuzzel_user_setup(arch: Arch) -> Result<Option<(String, Carried, String)>> {
    let Some(home) = std::env::var_os("HOME") else {
        println!("  {arch}: no HOME; the user's launcher boot is skipped");
        return Ok(None);
    };
    let conf = Path::new(&home).join(".config/hypr/hyprland.conf");
    let Ok(text) = std::fs::read_to_string(&conf) else {
        println!(
            "  {arch}: no {}; the user's launcher boot is skipped",
            conf.display()
        );
        return Ok(None);
    };
    let dotfiles = crate::dotfiles::carried(&conf)?;
    let Some(script) = dotfiles
        .iter()
        .find(|file| {
            matches!(&file.content, crate::ports::Content::Bytes(bytes) if bytes.starts_with(b"#!/bin/sh"))
                && !file.path.starts_with(crate::dotfiles::CONFIG_HOME)
        })
        .map(|file| format!("/{}", file.path))
    else {
        println!("  {arch}: {} binds no #!/bin/sh launcher; the boot is skipped", conf.display());
        return Ok(None);
    };
    let fuzzel = build(arch, "compositor-fuzzel", "fuzzel")?;
    let bytes = std::fs::read(&fuzzel)
        .map_err(|error| Error::new(format!("reading {}: {error}", fuzzel.display())))?;
    let mut ports = dotfiles;
    ports.push(crate::ports::File {
        path: "bin/fuzzel".to_owned(),
        mode: 0o755,
        content: crate::ports::Content::Bytes(bytes),
    });
    ports.extend(crate::fuzzel::files(None, false)?);
    let carried = Carried {
        busybox: crate::busybox::installed_program(arch),
        ports,
        ..Carried::none()
    };
    if carried.busybox.is_none() {
        println!(
            "  {arch}: no busybox on this machine for the script's /bin/sh; the boot is skipped"
        );
        return Ok(None);
    }
    let config = format!(
        "{text}\n# Added by `cargo xtask test-compositor --boot fuzzel-user`.\n\
         bind = , F12, exec, /bin/hyprctl layers\n"
    );
    Ok(Some((config, carried, script)))
}

/// Presses the user's own bind through QMP and returns the transcript: fuzzel
/// must open, proven by `hyprctl layers` naming its `launcher` namespace, and
/// the screen is dumped to `dump`.
fn drive_fuzzel_user(
    arch: Arch,
    port: u16,
    dump: &Path,
    watching: &mut Watching<'_>,
) -> Result<Vec<String>> {
    let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
    let up = watching.read_more(Instant::now() + SETTLE, |lines| {
        lines.iter().any(|line| line.contains(MARKER))
    })?;
    if !up && !watching.lines().iter().any(|line| line.contains(MARKER)) {
        return Err(Error::new(format!(
            "{arch}: the compositor never printed `{MARKER}`"
        )));
    }
    say_the_marker(watching, arch);
    std::thread::sleep(Duration::from_secs(5));
    press(&mut qmp, &["meta_l", "r"])?;
    println!("  {arch}: pressed SUPER R, the user's own launcher bind");
    // Asked until fuzzel's surface is listed: the user's fonts are
    // twenty files, and reading them under emulation takes a while.
    let listed = |lines: &[String]| {
        lines
            .iter()
            .any(|line| line.contains("namespace: launcher"))
    };
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut open = false;
    while !open && Instant::now() < deadline {
        press(&mut qmp, &["f12"])?;
        open = watching.read_more(Instant::now() + Duration::from_secs(10), |lines| {
            listed(lines)
        })?;
    }
    if !open {
        return Err(with_the_transcript(
            &Error::new(format!("{arch}: SUPER R never put fuzzel's surface up")),
            watching,
        ));
    }
    std::thread::sleep(Duration::from_secs(3));
    qmp.screendump(Some(DEVICE_ID), dump)?;
    println!("  {arch}: fuzzel is up; the screen is {}", dump.display());
    // Not asked here: the script's own toggle, a second SUPER R running
    // `pkill -x fuzzel`. That needs `pkill` to find a process by a `comm`
    // this exec path gives it, which is its own question -- docs/BACKLOG.md.
    let said: Vec<String> = watching
        .lines()
        .iter()
        .chain(watching.after())
        .cloned()
        .collect();
    Ok(said)
}

/// A boot of `run-compositor --everything` with no other configuration
/// named: proves the fix for the customer's report that `/bin/waybar` on
/// such a desktop found no `~/.config/waybar` -- [`everything_config`] now
/// carries this machine's own `hyprland.conf`, its dotfiles and fonts, the
/// same way `--config` does. After the marker, `hyprctl layers` must list a
/// `waybar` namespace; the user's own `SUPER Q` (`$terminal = foot`) must
/// open a window, `/bin/foot` being term; `SUPER RETURN`, appended, must
/// start `/bin/term`; and `/bin/vdagent`, appended, must have been started.
///
/// Skips, and says why, on a machine with no `~/.config/hypr/hyprland.conf`:
/// the fix has nothing of the customer's to carry there.
pub(super) fn test_everything_desktop(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let Some((config, carried, edid)) = everything_desktop_setup(arch)? else {
        println!(
            "  {arch}: no ~/.config/hypr/hyprland.conf; the --everything desktop boot is skipped"
        );
        return Ok(());
    };
    let (image, kernel) = judged_image(arch, programs, &config, carried, Some(&edid), args)?;
    let port = free_port()?;
    let mut qemu_args = args.clone();
    qemu_args.display = true;
    qemu_args.qmp_port = Some(port);
    let dump = paths::build_dir(arch).join("everything-desktop.ppm");
    let mut said = Vec::new();
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        watching.stop_when_done();
        said = drive_everything_desktop(arch, port, &dump, watching)?;
        Ok(())
    };
    let _ = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook)?;
    judge_still_running(arch, &said)?;
    println!(
        "  {arch}: the --everything desktop carried the user's real hyprland.conf: waybar drew, \
         and a terminal was a key away"
    );
    Ok(())
}

/// The `--everything` desktop's config -- this machine's own `hyprland.conf`
/// with the boot's own `F12` (`hyprctl layers`) debug bind added -- and the
/// ports it needs: the dotfiles and fonts [`crate::dotfiles`] carries beside
/// it, waybar and fuzzel built for `arch`, and this machine's monitor's EDID
/// with the kernel argument that hands it over. `None` when there is no such
/// file, no such monitor, or no busybox to run the desktop's scripts with.
fn everything_desktop_setup(arch: Arch) -> Result<Option<(String, Carried, String)>> {
    let mut everything_args = Args {
        everything: true,
        ..Args::default()
    };
    let Some(config) = everything_config(&mut everything_args)? else {
        return Ok(None);
    };
    let Some(conf_path) = everything_args.config.as_deref() else {
        return Ok(None);
    };
    // The screen named as run-compositor names it, by this machine's
    // monitor's EDID: the user's waybar matches its output by that name, and
    // shows no bar on a screen that is only `Virtual-1`.
    let Some(edid) = crate::edid::for_run(None)? else {
        println!("  {arch}: no monitor here for the screen's EDID; the boot is skipped");
        return Ok(None);
    };
    let mut ports = crate::dotfiles::carried(Path::new(conf_path))?;
    ports.extend(desktop_programs(arch)?);
    ports.extend(edid.files);
    ports.extend(crate::fuzzel::files(None, false)?);
    // zinc for the terminals to run, as run-compositor carries it.
    let carried = Carried {
        busybox: crate::busybox::installed_program(arch),
        zinc: crate::zinc::build(arch)?,
        ports,
        ..Carried::none()
    };
    if carried.busybox.is_none() {
        println!(
            "  {arch}: no busybox on this machine for the desktop's scripts; the boot is skipped"
        );
        return Ok(None);
    }
    // Chrome's lines as run-compositor adds them, so its environment is the
    // one the user's clients get; Chrome itself is not carried, and fails to
    // start as any missing program does.
    let chrome = Args {
        chrome: true,
        everything: true,
        ..Args::default()
    };
    let config = with_chrome(config, &chrome, arch);
    let config = format!(
        "{}\n# Added by `cargo xtask test-compositor --boot everything-desktop`.\n\
         bind = , F12, exec, /bin/hyprctl layers\n\
         bind = , F11, exec, /bin/hyprctl clients\n",
        config.trim_end()
    );
    Ok(Some((config, carried, edid.argument)))
}

/// Presses `F12` (`hyprctl layers`) until waybar's bar is listed, screendumps
/// it, then presses `SUPER RETURN` and requires `/bin/term` to have started.
/// Returns the transcript.
fn drive_everything_desktop(
    arch: Arch,
    port: u16,
    dump: &Path,
    watching: &mut Watching<'_>,
) -> Result<Vec<String>> {
    let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
    let up = watching.read_more(Instant::now() + SETTLE, |lines| {
        lines.iter().any(|line| line.contains(MARKER))
    })?;
    if !up && !watching.lines().iter().any(|line| line.contains(MARKER)) {
        return Err(Error::new(format!(
            "{arch}: the compositor never printed `{MARKER}`"
        )));
    }
    say_the_marker(watching, arch);
    // Asked until waybar's bar is listed: the user's fonts are twenty
    // files, and reading them under emulation takes a while.
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut has_bar = false;
    while !has_bar && Instant::now() < deadline {
        press(&mut qmp, &["f12"])?;
        has_bar = watching.read_more(Instant::now() + Duration::from_secs(10), |lines| {
            lines.iter().any(|line| line.contains("namespace: waybar"))
        })?;
    }
    if !has_bar {
        return Err(with_the_transcript(
            &Error::new(format!(
                "{arch}: waybar never put its bar up on the --everything desktop"
            )),
            watching,
        ));
    }
    std::thread::sleep(Duration::from_secs(2));
    qmp.screendump(Some(DEVICE_ID), dump)?;
    println!(
        "  {arch}: waybar is up on the --everything desktop; the screen is {}",
        dump.display()
    );
    // The user's own `bind = $mainMod, Q, exec, $terminal`, `$terminal =
    // foot`: `/bin/foot` is term, which must open a window on the shell.
    press(&mut qmp, &["meta_l", "q"])?;
    println!("  {arch}: pressed SUPER Q, the user's own $terminal (foot) bind");
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut window = false;
    while !window && Instant::now() < deadline {
        press(&mut qmp, &["f11"])?;
        window = watching.read_more(Instant::now() + Duration::from_secs(10), |lines| {
            lines
                .iter()
                .any(|line| line.starts_with("Window ") && line.contains(" -> "))
        })?;
    }
    if !window {
        return Err(with_the_transcript(
            &Error::new(format!(
                "{arch}: SUPER Q (the user's `$terminal = foot`) never opened a window"
            )),
            watching,
        ));
    }
    std::thread::sleep(Duration::from_secs(3));
    qmp.screendump(Some(DEVICE_ID), &dump.with_extension("foot.ppm"))?;
    println!("  {arch}: SUPER Q opened term in foot's place");
    press(&mut qmp, &["meta_l", "ret"])?;
    println!("  {arch}: pressed SUPER RETURN, the appended terminal bind");
    let opened = watching.read_more(Instant::now() + Duration::from_secs(15), |lines| {
        lines.iter().any(|line| line.contains("started /bin/term"))
    })?;
    let said: Vec<String> = watching
        .lines()
        .iter()
        .chain(watching.after())
        .cloned()
        .collect();
    if !opened {
        return Err(Error::new(format!(
            "{arch}: SUPER RETURN never started /bin/term on the --everything desktop"
        )));
    }
    for wanted in ["started foot", "started /bin/vdagent"] {
        if !said.iter().any(|line| line.contains(wanted)) {
            return Err(Error::new(format!("{arch}: hyprix never said `{wanted}`")));
        }
    }
    if let Some(line) = said
        .iter()
        .find(|line| line.contains("term: failed") || line.contains("term: usage"))
    {
        return Err(Error::new(format!("{arch}: {line}")));
    }
    println!("  {arch}: SUPER RETURN opened a terminal too, and vdagent was started");
    Ok(said)
}
