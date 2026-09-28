//! `test-yserver`: the X server Steam will draw through, started on Ferrix
//! with no display of its own and asked who it is (stage 22, the yserver
//! feasibility pass).
//!
//! yserver (github.com/joske/yserver, v1.6.0) is built for x86-64 glibc
//! against Debian 13's libraries, and runs here on them from a data volume, as
//! Chrome does. With no DRM card it starts headless and renders through
//! Vulkan on the CPU, Mesa's lavapipe; with no input device it starts only
//! when `YSERVER_ALLOW_NO_INPUT` says so, which a patch of Ferrix's adds.
//! [`RUN`], carried in the image, starts it on `:1`, runs `xdpyinfo` against
//! it, and prints the server's log.
//!
//! `scripts/fetch/fetch-yserver.sh` builds yserver from the customer's fork
//! and makes the volume (docs/YSERVER.md §3). It carries no `ferrix-root`
//! label, so the kernel mounts it at `/data`, under QEMU's `snapshot=on`.
//! The gate needs no network, but it attaches a volume, so it runs on demand
//! as `test-steamcmd` does and is not in the image row.

use crate::args::Args;
use crate::paths::Arch;
use crate::{Error, Result, busybox, cargo, fat, initramfs, native, qemu, rustc, shell, zinc};

/// glibc's x86-64 paths and the data files yserver and its libraries name
/// absolutely, each a link into the volume.
pub(crate) const LINKS: &[(&str, &str)] = &[
    ("lib64", "/data/usr/lib64"),
    ("lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("usr/lib/x86_64-linux-gnu", "/data/usr/lib/x86_64-linux-gnu"),
    ("etc/fonts", "/data/etc/fonts"),
    ("usr/share/fonts", "/data/usr/share/fonts"),
    ("usr/share/fontconfig", "/data/usr/share/fontconfig"),
    ("usr/share/vulkan", "/data/usr/share/vulkan"),
    ("usr/share/X11", "/data/usr/share/X11"),
    ("usr/share/drirc.d", "/data/usr/share/drirc.d"),
];

/// Where [`RUN`] is in the image.
const RUN_PATH: &str = "bin/yserver-test";

/// yserver on `:1` with no card and no input device, then `xdpyinfo`
/// against it, then the server's log. Run by busybox's `sh`, whose `&` and
/// `$!` it uses; its status is `xdpyinfo`'s.
const RUN: &str = r#"export PATH=/bin:/data/usr/bin HOME=/tmp XDG_RUNTIME_DIR=/tmp RUST_LOG=info
export YSERVER_ALLOW_NO_INPUT=1 YSERVER_ALLOW_SOFTWARE_VULKAN=1
/data/yserver/yserver :1 -nolisten tcp > /tmp/yserver.log 2>&1 &
server=$!
waited=0
while [ ! -S /tmp/.X11-unix/X1 ] && [ $waited -lt 120 ]; do
    sleep 1
    waited=$((waited + 1))
done
echo "yserver-gate: the socket was there after ${waited}s"
DISPLAY=:1 xdpyinfo > /tmp/xdpyinfo.txt 2>&1
status=$?
echo "yserver-gate: xdpyinfo exited $status"
cat /tmp/xdpyinfo.txt
kill $server
sleep 2
echo "yserver-gate: the server's log follows"
cat /tmp/yserver.log
exit $status
"#;

/// The script: [`RUN`], whose status says whether `xdpyinfo` reached the
/// server.
const SCRIPT: &str = r#"export PATH=/bin HOME=/tmp
[ -x /data/yserver/yserver ] || exit 3
busybox sh /bin/yserver-test || exit 4
exit 17
"#;

/// What the script exits with when `xdpyinfo` reached the server.
const STATUS: i32 = 17;

/// Memory for the guest: lavapipe and a 130 MiB server.
pub(crate) const MEMORY: u32 = 2048;

/// Where `scripts/fetch/fetch-yserver.sh` writes, unless
/// `FERRIX_YSERVER_VOLUME` names another directory.
///
/// # Errors
///
/// The volume has not been made.
pub(crate) fn volume() -> Result<std::path::PathBuf> {
    let directory = match std::env::var_os("FERRIX_YSERVER_VOLUME") {
        Some(directory) => std::path::PathBuf::from(directory),
        None => crate::paths::volume_directory("yserver")?,
    };
    let image = directory.join("yserver.img");
    if !image.is_file() {
        return Err(Error::new(format!(
            "{} is not there: scripts/fetch/fetch-yserver.sh makes it",
            image.display()
        )));
    }
    Ok(image)
}

/// `test-yserver` or `test-xwindow`, by the command's name.
///
/// # Errors
///
/// As [`test_yserver`] and `crate::compositor::test_xwindow`.
pub(crate) fn run(command: &str, args: &Args) -> Result<()> {
    if command == "test-xwindow" {
        crate::compositor::test_xwindow(args)
    } else {
        test_yserver(args)
    }
}

/// Boot a shell whose script starts yserver and runs `xdpyinfo` against it.
///
/// # Errors
///
/// When the volume is missing, the image cannot be built, the boot fails, or
/// `xdpyinfo` did not reach the server.
pub(crate) fn test_yserver(args: &Args) -> Result<()> {
    let arch = match args.arches()?.as_slice() {
        [Arch::X86_64] => Arch::X86_64,
        _ => return Err(Error::new("test-yserver runs on x86-64")),
    };
    let mut args = args.clone();
    args.data_image = Some(volume()?);
    if !args.memory_given {
        args.memory = MEMORY;
    }

    let shell =
        zinc::built(arch)?.ok_or_else(|| Error::new("zinc could not be built for x86-64"))?;
    println!("  {arch}: building an image whose shell starts yserver from the volume");
    let loader = cargo::build_loader(arch, args.release)?;
    let kernel = cargo::build_kernel_with_init(arch, args.release, &shell, SCRIPT)?;
    let natives = native::build(arch, args.release)?;
    let bytes = std::fs::read(&shell)
        .map_err(|error| Error::new(format!("reading {}: {error}", shell.display())))?;
    let busybox = busybox::program(arch)?;
    let mut files = rustc::files(LINKS);
    files.push(crate::ports::File {
        path: RUN_PATH.to_owned(),
        mode: 0o755,
        content: crate::ports::Content::Bytes(RUN.as_bytes().to_vec()),
    });
    let archive = initramfs::build(Some(&busybox), &natives, Some(&bytes), &files)?;
    let image = fat::write_image_with(arch, &loader, &kernel, &archive, None)?;

    println!(
        "  {arch}: running yserver on Ferrix with {} MiB (timeout {}s)",
        args.memory, args.timeout
    );
    let lines = qemu::watch_then(arch, &image, &kernel, &args, shell::EXITED, |_| Ok(()))?;
    let exited = lines
        .iter()
        .find_map(|line| line.trim().strip_prefix(shell::EXITED))
        .map(str::trim);
    match exited {
        Some(status) if status == STATUS.to_string() => {
            println!("  {arch}: xdpyinfo reached yserver on Ferrix");
            Ok(())
        }
        Some("3") => Err(Error::new(format!(
            "{arch}: /data/yserver/yserver is not there: is the volume attached?"
        ))),
        Some("4") => Err(Error::new(format!(
            "{arch}: xdpyinfo did not reach yserver; the `yserver-gate:` lines and the \
             server's log say why"
        ))),
        other => Err(Error::new(format!(
            "{arch}: the yserver script ended with {other:?}"
        ))),
    }
}

/// Where [`XWINDOW_SCRIPT`] is in `test-xwindow`'s image.
pub(crate) const XWINDOW_PATH: &str = "etc/xwindow.sh";

/// `test-xwindow`'s script, started by the compositor: yserver as its client
/// on `:0`, then `xdpyinfo`, whose screen line says whether the root took the
/// compositor's screen for its own (docs/YSERVER.md, Y2), then `xev`, whose
/// window must become one of the compositor's (Y3): `hyprctl clients` lists
/// it, and xtask looks for it on the screen once the script has ended, while
/// `xev` still runs. xev names its window but gives it no class, so the
/// script sets `WM_CLASS` once it is up, which is also how a program
/// renaming its window reaches the compositor.
///
/// Then it says `xwindow: input` and waits for xtask to point at xev's
/// window, click, turn the wheel and type `a` and `z` there (Y4), until xev
/// has reported the `z`. `xwininfo` then asks for a window to be picked,
/// which grabs the pointer with a cross for a cursor, and it says
/// `xwindow: pick` for xtask to click xev's window again. xev's and
/// xwininfo's reports come out as `xwindow: xev:` and `xwindow: pick:`
/// lines, and the server's log, whose seat module says each cursor it gives
/// the compositor, as `xwindow: yserver:` lines. Every line of its own starts
/// `xwindow:`, and it ends with `xwindow: end` whatever happened.
pub(crate) const XWINDOW_SCRIPT: &str = r#"export PATH=/bin:/data/usr/bin HOME=/tmp
export RUST_LOG=info,yserver::wayland::input=debug
echo "xwindow: start"
YSERVER_BACKEND=wayland YSERVER_ALLOW_SOFTWARE_VULKAN=1 /data/yserver/yserver :0 -nolisten tcp \
    > /tmp/yserver.log 2>&1 &
waited=0
while [ ! -S /tmp/.X11-unix/X0 ] && [ $waited -lt 120 ]; do
    sleep 1
    waited=$((waited + 1))
done
echo "xwindow: the socket was there after ${waited}s"
export DISPLAY=:0
xdpyinfo > /tmp/xdpyinfo.txt 2>&1
echo "xwindow: xdpyinfo exited $?"
grep dimensions: /tmp/xdpyinfo.txt | sed 's/^/xwindow: /'
xev > /tmp/xev.txt 2>&1 &
waited=0
until xwininfo -name "Event Tester" 2>/dev/null | grep -q IsViewable || [ $waited -ge 30 ]; do
    sleep 1
    waited=$((waited + 1))
done
echo "xwindow: xev's window was viewable after ${waited}s"
xprop -name "Event Tester" -f WM_CLASS 8s -set WM_CLASS Xev
xprop -name "Event Tester" WM_NAME WM_CLASS | sed 's/^/xwindow: /'
sleep 2
# hyprix answers a request that is slow to arrive as an empty one
# (docs/BACKLOG.md, P1 flakes), so ask again if it did.
tries=0
until /bin/hyprctl clients > /tmp/clients.txt && grep -q '^Window' /tmp/clients.txt \
    || [ $tries -ge 2 ]; do
    sleep 1
    tries=$((tries + 1))
done
sed 's/^/xwindow: clients: /' /tmp/clients.txt
echo "xwindow: input"
waited=0
until [ "$(grep -c 'keysym 0x7a, z' /tmp/xev.txt)" -ge 2 ] || [ $waited -ge 60 ]; do
    sleep 1
    waited=$((waited + 1))
done
echo "xwindow: xev had the keys after ${waited}s"
xwininfo > /tmp/pick.txt 2>&1 &
pick=$!
sleep 2
echo "xwindow: pick"
waited=0
while kill -0 $pick 2>/dev/null && [ $waited -lt 30 ]; do
    sleep 1
    waited=$((waited + 1))
done
kill $pick 2>/dev/null
sed 's/^/xwindow: pick: /' /tmp/pick.txt
sed 's/^/xwindow: xev: /' /tmp/xev.txt
sed 's/^/xwindow: yserver: /' /tmp/yserver.log
echo "xwindow: end"
"#;

/// The line [`XWINDOW_SCRIPT`] ends with.
pub(crate) const XWINDOW_END: &str = "xwindow: end";

/// The line [`XWINDOW_SCRIPT`] says when xev's window is up and it waits
/// for the input.
pub(crate) const XWINDOW_INPUT: &str = "xwindow: input";

/// The line it says when `xwininfo` waits for a window to be picked.
const XWINDOW_PICK: &str = "xwindow: pick";

/// Where in xev's window the pointer is put, in the window's own
/// coordinates: right and below the subwindow, on the window itself.
const XEV_POINT: (usize, usize) = (120, 120);

/// Whether `test-xwindow`'s lines say the root window is the compositor's
/// screen: `xdpyinfo` reached the server and gave a size that is not the
/// headless 0×0, and the server said it took that size from the compositor.
pub(crate) fn judge_xwindow(arch: Arch, lines: &[String]) -> Result<()> {
    let dimensions = lines.iter().find_map(|line| {
        let (_, rest) = line.split_once("xwindow:")?;
        let size = rest
            .trim()
            .strip_prefix("dimensions:")?
            .split_whitespace()
            .next()?;
        let (width, height) = size.split_once('x')?;
        Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
    });
    let took = lines
        .iter()
        .find_map(|line| line.split_once("the root window is the compositor's screen, "))
        .map(|(_, size)| size.trim().to_owned());
    match (dimensions, took) {
        (Some((width, height)), Some(said)) if width > 0 && said == format!("{width}x{height}") => {
            println!("  {arch}: yserver's root is the compositor's screen, {width}x{height}");
            Ok(())
        }
        (dimensions, said) => Err(Error::new(format!(
            "{arch}: xdpyinfo said the screen is {dimensions:?}, and yserver said it took {said:?} \
             from the compositor; the `xwindow:` lines say more"
        ))),
    }
}

/// What xev calls its window (`WM_NAME`) and the class the script gives it
/// (`WM_CLASS`), which the compositor's window must have for its title and
/// app id.
const XEV_TITLE: &str = "Event Tester";
/// See [`XEV_TITLE`].
const XEV_CLASS: &str = "Xev";

/// xev's window as X draws it: white, with a white 50×50 subwindow at
/// (10, 10) inside a black border 4 pixels wide. The subwindow is drawn into
/// the top-level's own image only when the server redirects the top-level,
/// so finding it says the whole subtree reached the compositor.
const XEV_INNER_AT: usize = 10;
/// See [`XEV_INNER_AT`].
const XEV_INNER: usize = 50;
/// See [`XEV_INNER_AT`].
const XEV_BORDER: usize = 4;

/// Where on `screen` xev's subwindow's border has its top left corner, if
/// xev's window is on it.
pub(crate) fn find_xev(screen: &crate::display::Image) -> Option<(usize, usize)> {
    let pixel = |x: usize, y: usize| -> Option<&[u8]> {
        let at = y
            .checked_mul(screen.width)?
            .checked_add(x)?
            .checked_mul(3)?;
        screen.pixels.get(at..at.checked_add(3)?)
    };
    let white = |x: usize, y: usize| pixel(x, y).is_some_and(|rgb| rgb.iter().all(|&c| c >= 0xf0));
    let black = |x: usize, y: usize| pixel(x, y).is_some_and(|rgb| rgb.iter().all(|&c| c <= 0x10));
    let ring = XEV_INNER + 2 * XEV_BORDER;
    let is_xev = |x: usize, y: usize| {
        black(x, y)
            && white(x - 1, y)
            && white(x, y - 1)
            && (0..ring).all(|along| {
                (0..XEV_BORDER).all(|across| {
                    black(x + along, y + across)
                        && black(x + along, y + ring - 1 - across)
                        && black(x + across, y + along)
                        && black(x + ring - 1 - across, y + along)
                })
            })
            && (XEV_BORDER..ring - XEV_BORDER)
                .all(|row| (XEV_BORDER..ring - XEV_BORDER).all(|column| white(x + column, y + row)))
            && (1..=XEV_INNER_AT).all(|out| white(x - out, y) && white(x, y - out))
    };
    (XEV_INNER_AT..screen.height.saturating_sub(ring))
        .flat_map(|y| (XEV_INNER_AT..screen.width.saturating_sub(ring)).map(move |x| (x, y)))
        .find(|&(x, y)| is_xev(x, y))
}

/// Whether xev's window is one of the compositor's: `hyprctl clients` lists
/// it with xev's title and class, and the screen shows it, subwindow and all
/// (docs/YSERVER.md, Y3).
pub(crate) fn judge_xev(
    arch: Arch,
    lines: &[String],
    screen: Option<&crate::display::Image>,
    dump: &std::path::Path,
) -> Result<()> {
    let clients: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.split_once("xwindow: clients:"))
        .map(|(_, rest)| rest.trim())
        .collect();
    let has = |key: &str, value: &str| {
        clients.iter().any(|line| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(':'))
                .is_some_and(|rest| rest.trim() == value)
        })
    };
    if !has("title", XEV_TITLE) || !has("class", XEV_CLASS) {
        return Err(Error::new(format!(
            "{arch}: `hyprctl clients` has no window titled {XEV_TITLE:?} of class \
             {XEV_CLASS:?}; it said:\n{}",
            clients.join("\n")
        )));
    }
    let Some(screen) = screen else {
        return Err(Error::new(format!("{arch}: the boot took no picture")));
    };
    match find_xev(screen) {
        Some((x, y)) => {
            println!(
                "  {arch}: xev's window is the compositor's, {XEV_TITLE:?} of {XEV_CLASS:?}, \
                 its subwindow on the screen at ({x}, {y})"
            );
            Ok(())
        }
        None => Err(Error::new(format!(
            "{arch}: xev's window is not on the screen; the last picture is {}",
            dump.display()
        ))),
    }
}

/// Put input into xev's window, whose subwindow's ring [`find_xev`] found
/// at `ring` on a screen of `size`: the pointer to [`XEV_POINT`], a left
/// click, a wheel click down, and the keys `a` and `z`, which the script
/// waits for. Then, when the script says `xwininfo` waits, click there
/// again.
///
/// # Errors
///
/// What QMP says.
pub(crate) fn drive_xev(
    qmp: &mut crate::display::Qmp,
    watching: &mut qemu::Watching<'_>,
    ring: (usize, usize),
    size: (usize, usize),
) -> Result<()> {
    use crate::compositor::{absolute, button_event, press};
    use std::time::{Duration, Instant};

    let tablet = |at: usize, across: usize| {
        let at = i64::try_from(at).unwrap_or(0);
        let across = i64::try_from(across.max(1)).unwrap_or(1);
        i32::try_from(at * 0x7FFF / across).unwrap_or(0)
    };
    let x = ring.0 - XEV_INNER_AT + XEV_POINT.0;
    let y = ring.1 - XEV_INNER_AT + XEV_POINT.1;
    let pause = |millis| std::thread::sleep(Duration::from_millis(millis));
    let click = |qmp: &mut crate::display::Qmp, button: &str| -> Result<()> {
        qmp.input_send_event(&[button_event(button, true)])?;
        pause(100);
        qmp.input_send_event(&[button_event(button, false)])
    };
    qmp.input_send_event(&[
        absolute("x", tablet(x, size.0)),
        absolute("y", tablet(y, size.1)),
    ])?;
    pause(500);
    click(qmp, "left")?;
    pause(300);
    click(qmp, "wheel-down")?;
    pause(300);
    press(qmp, &["a"])?;
    pause(300);
    press(qmp, &["z"])?;
    let picking = watching.read_more(Instant::now() + Duration::from_secs(90), |lines| {
        lines
            .iter()
            .any(|line| line.contains(XWINDOW_PICK) || line.contains(XWINDOW_END))
    })?;
    if picking {
        click(qmp, "left")?;
    }
    Ok(())
}

/// xev's report, one event a paragraph, each with its first line's name:
/// the `xwindow: xev:` lines, each event starting at a line that does not
/// begin with a space.
fn xev_events(lines: &[String]) -> Vec<(String, String)> {
    let mut events: Vec<(String, String)> = Vec::new();
    for line in lines {
        let Some((_, rest)) = line.split_once("xwindow: xev: ") else {
            continue;
        };
        let rest = rest.trim_end();
        if rest.starts_with(' ') {
            if let Some((_, text)) = events.last_mut() {
                text.push(' ');
                text.push_str(rest.trim());
            }
        } else if let Some((name, _)) = rest.split_once(" event,") {
            events.push((name.to_owned(), rest.to_owned()));
        }
    }
    events
}

/// The window-relative position an xev event reports, `(x,y)`.
fn xev_position(text: &str) -> Option<(i64, i64)> {
    let (_, rest) = text.split_once(", (")?;
    let (inside, _) = rest.split_once(')')?;
    let (x, y) = inside.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Whether xev reported the input [`drive_xev`] put in, at the place it was
/// put, and `xwininfo`'s pick found xev's window; and the server gave the
/// compositor a cursor (docs/YSERVER.md, Y4).
pub(crate) fn judge_xev_input(arch: Arch, lines: &[String]) -> Result<()> {
    let events = xev_events(lines);
    let near = |text: &str| {
        xev_position(text).is_some_and(|(x, y)| {
            let (want_x, want_y) = (XEV_POINT.0 as i64, XEV_POINT.1 as i64);
            (x - want_x).abs() <= 2 && (y - want_y).abs() <= 2
        })
    };
    let has = |name: &str, detail: &str, placed: bool| {
        events
            .iter()
            .any(|(event, text)| event == name && text.contains(detail) && (!placed || near(text)))
    };
    let wanted: [(&str, &str, &str, bool); 8] = [
        ("the keyboard focus", "FocusIn", "window", false),
        ("the pointer coming in", "EnterNotify", "window", false),
        (
            "the pointer where it was put",
            "MotionNotify",
            "window",
            true,
        ),
        ("the left button", "ButtonPress", "button 1,", true),
        ("the left button let go", "ButtonRelease", "button 1,", true),
        ("the wheel", "ButtonPress", "button 5,", true),
        ("the key a", "KeyPress", "(keysym 0x61, a)", true),
        ("the key a let go", "KeyRelease", "(keysym 0x61, a)", true),
    ];
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|(_, name, detail, placed)| !has(name, detail, *placed))
        .map(|(what, ..)| *what)
        .collect();
    let picked = lines.iter().any(|line| {
        line.split_once("xwindow: pick:").is_some_and(|(_, rest)| {
            rest.contains("Window id:") && rest.contains("\"Event Tester\"")
        })
    });
    // The server's cursor, then xwininfo's cross while it grabs the pointer.
    let mut cursors: Vec<String> = lines
        .iter()
        .filter_map(|line| line.split_once("wayland: the cursor is "))
        .map(|(_, rest)| rest.trim().to_owned())
        .collect();
    let given = cursors.len();
    cursors.dedup();
    if missing.is_empty() && picked && cursors.len() >= 2 {
        println!(
            "  {arch}: xev reported {} events of the input put in, at ({}, {}) in its window; \
             xwininfo picked xev's window by a click; the compositor was given {given} cursors: \
             {}",
            events.len(),
            XEV_POINT.0,
            XEV_POINT.1,
            cursors.join(", ")
        );
        return Ok(());
    }
    let report: Vec<String> = events
        .iter()
        .map(|(_, text)| text.clone())
        .take(40)
        .collect();
    Err(Error::new(format!(
        "{arch}: xev did not report {missing:?}; xwininfo {} xev's window; the compositor was \
         given the cursors {cursors:?}, where xwininfo's grab should have changed it. xev \
         said:\n{}",
        if picked { "picked" } else { "did not pick" },
        report.join("\n")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Image;

    /// A screen of `width` × `height` grey with xev's window drawn at
    /// (`left`, `top`).
    fn screen_with_xev(width: usize, height: usize, left: usize, top: usize) -> Image {
        let mut pixels = vec![0x40; width * height * 3];
        let mut paint = |x: usize, y: usize, value: u8| {
            let at = (y * width + x) * 3;
            pixels[at..at + 3].fill(value);
        };
        for y in 0..178 {
            for x in 0..178 {
                paint(left + x, top + y, 0xff);
            }
        }
        let ring = XEV_INNER + 2 * XEV_BORDER;
        for y in 0..ring {
            for x in 0..ring {
                let edge = x < XEV_BORDER
                    || y < XEV_BORDER
                    || x >= ring - XEV_BORDER
                    || y >= ring - XEV_BORDER;
                if edge {
                    paint(left + XEV_INNER_AT + x, top + XEV_INNER_AT + y, 0);
                }
            }
        }
        Image {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn xev_is_found_where_it_is_drawn() {
        let screen = screen_with_xev(400, 300, 30, 40);
        assert_eq!(find_xev(&screen), Some((40, 50)));
    }

    #[test]
    fn a_white_window_without_the_subwindow_is_not_xev() {
        let mut screen = screen_with_xev(400, 300, 30, 40);
        for pixel in screen.pixels.chunks_exact_mut(3) {
            if pixel == [0, 0, 0] {
                pixel.fill(0xff);
            }
        }
        assert_eq!(find_xev(&screen), None);
    }

    /// xev's report of the input `drive_xev` puts in, as the script prints
    /// it, with the pick and the server's cursor.
    fn reported() -> Vec<String> {
        let xev = "FocusIn event, serial 12, synthetic NO, window 0x200001,
    mode NotifyNormal, detail NotifyNonlinear

EnterNotify event, serial 13, synthetic NO, window 0x200001,
    root 0x3c9, subw 0x0, time 100, (119,121), root:(119,121),
    mode NotifyNormal, detail NotifyNonlinear, same_screen YES,
    focus YES, state 0

MotionNotify event, serial 13, synthetic NO, window 0x200001,
    root 0x3c9, subw 0x0, time 101, (120,120), root:(120,120),
    state 0x0, is_hint 0, same_screen YES

ButtonPress event, serial 13, synthetic NO, window 0x200001,
    root 0x3c9, subw 0x0, time 102, (120,120), root:(120,120),
    state 0x0, button 1, same_screen YES

ButtonRelease event, serial 13, synthetic NO, window 0x200001,
    root 0x3c9, subw 0x0, time 103, (120,120), root:(120,120),
    state 0x100, button 1, same_screen YES

ButtonPress event, serial 13, synthetic NO, window 0x200001,
    root 0x3c9, subw 0x0, time 104, (120,120), root:(120,120),
    state 0x0, button 5, same_screen YES

KeyPress event, serial 13, synthetic NO, window 0x200001,
    root 0x3c9, subw 0x0, time 105, (120,120), root:(120,120),
    state 0x0, keycode 38 (keysym 0x61, a), same_screen YES,
    XLookupString gives 1 bytes: (61) \"a\"

KeyRelease event, serial 13, synthetic NO, window 0x200001,
    root 0x3c9, subw 0x0, time 106, (120,120), root:(120,120),
    state 0x0, keycode 38 (keysym 0x61, a), same_screen YES,
    XLookupString gives 1 bytes: (61) \"a\"";
        let mut lines: Vec<String> = xev
            .lines()
            .map(|line| format!("xwindow: xev: {line}"))
            .collect();
        lines.push("xwindow: pick: xwininfo: Window id: 0x200001 \"Event Tester\"".to_owned());
        for hot in [8, 7, 8] {
            lines.push(format!(
                "xwindow: yserver: [DEBUG yserver::wayland::input] wayland: the cursor is 16x16 \
                 at ({hot}, {hot})"
            ));
        }
        lines
    }

    #[test]
    fn xevs_report_is_read_one_event_a_paragraph() {
        let events = xev_events(&reported());
        assert_eq!(events.len(), 8);
        assert_eq!(events[3].0, "ButtonPress");
        assert_eq!(xev_position(&events[3].1), Some((120, 120)));
        assert!(events[6].1.contains("(keysym 0x61, a)"));
        assert_eq!(xev_position(&events[0].1), None, "FocusIn has no place");
    }

    #[test]
    fn the_input_must_all_be_reported_where_it_was_put() {
        assert!(judge_xev_input(Arch::X86_64, &reported()).is_ok());
        let moved: Vec<String> = reported()
            .into_iter()
            .map(|line| line.replace("(120,120)", "(60,60)"))
            .collect();
        assert!(
            judge_xev_input(Arch::X86_64, &moved).is_err(),
            "a click in the wrong place"
        );
        let unpicked: Vec<String> = reported()
            .into_iter()
            .filter(|line| !line.contains("pick:"))
            .collect();
        assert!(judge_xev_input(Arch::X86_64, &unpicked).is_err());
        let no_wheel: Vec<String> = reported()
            .into_iter()
            .map(|line| line.replace("button 5", "button 4"))
            .collect();
        assert!(judge_xev_input(Arch::X86_64, &no_wheel).is_err());
        let one_cursor: Vec<String> = reported()
            .into_iter()
            .map(|line| line.replace("(7, 7)", "(8, 8)"))
            .collect();
        assert!(
            judge_xev_input(Arch::X86_64, &one_cursor).is_err(),
            "the grab's cursor never reached the compositor"
        );
    }

    #[test]
    fn the_clients_must_name_xev() {
        let dump = std::path::Path::new("xwindow.ppm");
        let screen = screen_with_xev(400, 300, 30, 40);
        let listed = |lines: &[&str]| -> Vec<String> {
            lines
                .iter()
                .map(|line| format!("xwindow: clients: {line}"))
                .collect()
        };
        let good = listed(&[
            "Window 1 -> Event Tester:",
            "\tclass: Xev",
            "\ttitle: Event Tester",
        ]);
        assert!(judge_xev(Arch::X86_64, &good, Some(&screen), dump).is_ok());
        let untitled = listed(&["\tclass: Xev", "\ttitle: "]);
        assert!(judge_xev(Arch::X86_64, &untitled, Some(&screen), dump).is_err());
        assert!(judge_xev(Arch::X86_64, &good, None, dump).is_err());
    }
}
