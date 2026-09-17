//! Where a boot that is meant to be watched puts the guest's screen.
//!
//! Every `test-*` command in this tool is headless on purpose: it asks QEMU
//! for a screendump over QMP and compares pixels, which is a judgement a
//! machine can make and repeat. `cargo xtask run --display` and `cargo xtask
//! run-compositor` are the other half — a person watching the compositor draw
//! — and that needs a window, which is the one part of QEMU that is not the
//! same on two hosts.
//!
//! # Why `-display default` is not enough
//!
//! QEMU's `default` is whichever local backend was compiled in, and a build
//! without one has nothing to fall back to: it fails at startup rather than
//! booting headless. The two QEMUs this project is developed on are exactly
//! those two cases.
//!
//! * The Windows build (`winget install SoftwareFreedomConservancy.QEMU`)
//!   offers `gtk` and `sdl`, so a window opens and `default` would have been
//!   enough.
//! * The Linux box the gates run on builds QEMU from source, headless, and
//!   offers `none`, `spice-app` and `dbus` — no `gtk`, no `sdl`, and no
//!   session to open a window on anyway, since it is reached over `ssh`. Its
//!   build does have VNC, which needs no display of the host's at all.
//!
//! So the backend is chosen by asking QEMU what it has (`-display help`) and
//! this host whether there is a session to open a window on, rather than by
//! asking which operating system this is: a headless Linux desktop machine
//! and a Windows one with a cut-down QEMU are both real, and the question
//! "can a window open here" is the one that decides.
//!
//! # VNC is the fallback, on the loopback
//!
//! A VNC server has no window and no session: it draws into a socket, which
//! is why it is what a machine reached over `ssh` can show a screen on. It
//! also has no password here — `-vnc` without `password=on` accepts any
//! client — so the default address is `127.0.0.1`, and reaching it from
//! another machine is a tunnel the person opens deliberately:
//!
//! ```text
//! ssh -L 5900:127.0.0.1:5900 <host>
//! ```
//!
//! `--vnc <display>` asks for VNC even where a window could have opened, and
//! is the one way to put the screen on an address that is not the loopback.
//! Nothing here binds a wider one on its own.

use std::path::Path;
use std::process::Command;

use crate::args::Args;
use crate::{Error, Result};

/// The local backends worth opening a window with, best first.
///
/// `gtk` before `sdl` because its menu bar lists the guest's consoles by
/// name, and a boot with a virtio-gpu has two of them: firmware's head and
/// the card the compositor draws on. `sdl` switches between them with
/// `Ctrl-Alt-<n>` and says nothing about what they are.
const LOCAL: [&str; 2] = ["gtk", "sdl"];

/// Where a boot's screen goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Window {
    /// Nowhere. Every `test-*` boot, and any `run` without `--display`: QEMU
    /// draws into memory and a screendump is how anything reads it.
    Headless,
    /// A window on this host, opened by one of QEMU's local backends.
    Local(&'static str),
    /// A VNC server at this `<host>:<display>`, which a viewer connects to.
    Vnc(String),
}

/// Which of the guest's screens a person means: the device id of the
/// virtio-gpu the compositor draws on, on a machine that has one.
///
/// A boot with a card has two heads, and the card is the second: firmware
/// takes the machine's own display device -- `q35`'s VGA, `virt`'s `ramfb` --
/// and the kernel's driver takes the virtio-gpu. QEMU shows the first console
/// unless something says otherwise, so a window opened with nothing said
/// shows the head the loader's messages went to and stays there while the
/// compositor draws on the other one.
pub(crate) type Card<'a> = Option<&'a str>;

impl Window {
    /// The `-display` argument this is, pointed at `card` where the backend
    /// can be told which console to serve.
    pub(crate) fn arguments(&self, card: Card<'_>) -> [String; 2] {
        let backend = match self {
            Window::Headless => "none".to_owned(),
            // `show-tabs=on`: the window has a tab per console and the
            // compositor's is not the one it opens on. GTK can be told to
            // show the tab bar but not which tab to start on, so the bar is
            // made visible and `announce` says which tab and its shortcut.
            Window::Local("gtk") => "gtk,show-tabs=on".to_owned(),
            Window::Local(name) => (*name).to_owned(),
            // VNC serves one console and can be told which, so a viewer
            // connects straight to the card rather than to firmware's head.
            Window::Vnc(address) => match card {
                Some(card) => format!("vnc={address},display={card},head=0"),
                None => format!("vnc={address}"),
            },
        };
        ["-display".to_owned(), backend]
    }

    /// Say where the screen is, for the person who asked to watch it.
    ///
    /// A VNC server needs the most saying: it is running, it is showing the
    /// guest, and nothing on this machine has opened it yet.
    pub(crate) fn announce(&self, card: Card<'_>) {
        match self {
            Window::Headless => {}
            Window::Local(name) => {
                println!("  screen: a window, through QEMU's {name} backend");
                if card.is_some() {
                    // The window opens on the card, because `qemu` creates it
                    // before the head firmware drew on. The other console is
                    // still there, with the loader's text on it, and a person
                    // who wants it can reach it the way QEMU always offers.
                    println!(
                        "    it opens on {card}, where the compositor draws",
                        card = card.unwrap_or_default()
                    );
                    println!(
                        "    the loader's own head is the tab beside it (View menu; \
                         Ctrl-Alt-2 is unreliable on a German layout, where it is AltGr)"
                    );
                }
            }
            Window::Vnc(address) => {
                println!("  screen: VNC on {address}, port {}", port(address));
                println!(
                    "    no window backend here, so QEMU is serving the screen instead.\n    \
                     Connect a viewer to it; from another machine, tunnel first:\n      \
                     ssh -L {port}:127.0.0.1:{port} <this host>",
                    port = port(address)
                );
            }
        }
    }
}

/// Which port a VNC address serves: display *n* is 5900 + *n*, as everything
/// that speaks the protocol has it. An address whose display cannot be read
/// is reported as the default, which is the only thing left to say about it.
fn port(address: &str) -> u16 {
    address
        .rsplit(':')
        .next()
        .and_then(|display| display.parse::<u16>().ok())
        .map_or(5900, |display| display.saturating_add(5900))
}

/// Whether `args` asks for a screen somebody watches, rather than one only a
/// screendump reads.
fn wanted(args: &Args) -> bool {
    match args.command.as_deref() {
        // `--display` is both "put a virtio-gpu on the bus" and, to `run`,
        // "and show it": the flag's documentation has said so since the card
        // arrived.
        Some("run") => args.display,
        Some("run-compositor") => true,
        _ => false,
    }
}

/// Whether this boot should have the card as its only screen.
///
/// QEMU shows its first console and can be told to show another only over
/// VNC, so on a machine with two heads a window opens on firmware's -- the
/// one the loader's text went to -- and the compositor draws on a console
/// nobody asked for. A boot that is watched therefore goes without the
/// machine's own display device: `q35`'s VGA, `virt`'s `ramfb`.
///
/// What that costs is where the loader's framebuffer comes from. With the
/// VGA gone, firmware's graphics output is the virtio-gpu, so the panic
/// screen and the card the ring-3 driver takes over are the same device,
/// which `docs/DISPLAY.md` §2.4 keeps them apart for. That is a trade a
/// watched boot can make and a judged one must not, which is why this asks
/// the command and not the person: every `test-*` boot keeps both heads and
/// compares the card by screendump, exactly as it did before.
pub(crate) fn sole_screen(args: &Args) -> bool {
    wanted(args)
}

/// Whether this host has somewhere to open a window.
///
/// On Windows and macOS the answer is yes: a process that can start QEMU can
/// open a window. On a POSIX host it is a question, and the answer is in the
/// environment — an X display or a Wayland one. A machine reached over `ssh`
/// without forwarding has neither, and that is the case this exists for.
#[cfg(unix)]
fn session() -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// Whether this host has somewhere to open a window: it does.
#[cfg(not(unix))]
fn session() -> bool {
    true
}

/// Decide where `args` puts the screen, asking `binary` what it can do.
///
/// # Errors
///
/// When `--vnc` was given to a boot that has no screen, or when neither a
/// window nor VNC can be had from this QEMU.
pub(crate) fn choose(binary: &Path, args: &Args) -> Result<Window> {
    if !wanted(args) {
        if args.vnc.is_some() {
            return Err(Error::new(
                "--vnc asks where to put a screen, and this boot has none.\n  \
                 `cargo xtask run --display --vnc :0`, or `cargo xtask run-compositor --vnc :0`.",
            ));
        }
        return Ok(Window::Headless);
    }
    let offered = offered(binary);
    pick(&offered, args.vnc.as_deref(), session(), || vnc(binary))
}

/// Choose from what QEMU offers, what was asked for, and whether a window
/// could open at all.
///
/// The VNC question is a closure because answering it starts QEMU a second
/// time, and the common case — a host with a window backend and no `--vnc` —
/// never needs to ask.
fn pick(
    offered: &[String],
    asked: Option<&str>,
    session: bool,
    vnc: impl FnOnce() -> bool,
) -> Result<Window> {
    if let Some(address) = asked {
        if !vnc() {
            return Err(Error::new(
                "this QEMU was built without VNC, so --vnc has nowhere to serve the screen.",
            ));
        }
        return Ok(Window::Vnc(address_of(address)));
    }
    if session
        && let Some(name) = LOCAL
            .iter()
            .find(|name| offered.iter().any(|offer| offer == *name))
    {
        return Ok(Window::Local(name));
    }
    if vnc() {
        return Ok(Window::Vnc(address_of(DEFAULT_VNC)));
    }
    Err(Error::new(format!(
        "this QEMU cannot show a screen: it offers {}, none of which opens a window here, \
         and it has no VNC either.\n  \
         Install a QEMU built with GTK or SDL (Debian/Ubuntu: the distribution's \
         `qemu-system-x86` has both), or run the headless `cargo xtask test-compositor`, \
         which compares screendumps instead.{}",
        if offered.is_empty() {
            "nothing".to_owned()
        } else {
            offered.join(", ")
        },
        if session {
            ""
        } else {
            "\n  This host also has no DISPLAY and no WAYLAND_DISPLAY, so a window \
             backend would have had nowhere to open."
        }
    )))
}

/// Where VNC goes when nothing said: the loopback's first display, for the
/// reason the module documentation gives.
const DEFAULT_VNC: &str = "127.0.0.1:0";

/// The address a `--vnc` value means.
///
/// QEMU's own spellings are kept as they are — `:1`, `127.0.0.1:0`,
/// `0.0.0.0:2` — so a person who knows the option loses nothing by giving it
/// here. A bare number is the display alone, which QEMU would refuse, and is
/// read as that display on the loopback.
fn address_of(asked: &str) -> String {
    if asked.contains(':') {
        return asked.to_owned();
    }
    format!("127.0.0.1:{asked}")
}

/// The display backends this QEMU was built with.
///
/// A QEMU that cannot be asked — one that fails, or prints something this
/// does not recognise — offers nothing as far as this is concerned, and the
/// fallback below decides what happens. It is not an error on its own: the
/// answer to "can this show a screen" is still no, and the message that says
/// so is a better one than a parse failure here.
fn offered(binary: &Path) -> Vec<String> {
    let mut command = Command::new(binary);
    let _ = command.args(["-display", "help"]);
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    let names = listed(&String::from_utf8_lossy(&output.stdout));
    if names.is_empty() {
        // Some builds print the listing on stderr, and a QEMU that refused
        // the option printed its complaint there too: either way what comes
        // back is what the listing parser makes of it.
        listed(&String::from_utf8_lossy(&output.stderr))
    } else {
        names
    }
}

/// The backend names under `Available display backend types:`, which is one a
/// line until the blank line before the note about suboptions.
fn listed(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut reading = false;
    for line in text.lines() {
        let line = line.trim();
        if !reading {
            reading = line.ends_with("display backend types:");
            continue;
        }
        if line.is_empty() {
            break;
        }
        names.push(line.to_owned());
    }
    names
}

/// Whether this QEMU has VNC.
///
/// `-vnc help` prints the option's suboptions on a build that has it, and a
/// complaint on one built with `--disable-vnc`, which does not take the
/// option at all. It is the *output* that answers, not the status: QEMU exits
/// 1 from printing this help, on both the QEMUs this is developed against,
/// and a first version of this read the status and concluded that a QEMU
/// whose VNC works has none.
///
/// Asking is a QEMU start, which is why the caller only asks when the answer
/// matters.
fn vnc(binary: &Path) -> bool {
    let mut command = Command::new(binary);
    let _ = command.args(["-vnc", "help"]);
    command.output().is_ok_and(|output| {
        takes_vnc(&String::from_utf8_lossy(&output.stdout))
            || takes_vnc(&String::from_utf8_lossy(&output.stderr))
    })
}

/// Whether what `-vnc help` printed is the option's own help.
///
/// `qemu_opts_print_help` heads the listing with the option group's name,
/// which for this one is `vnc options:`.
fn takes_vnc(text: &str) -> bool {
    text.lines().any(|line| line.trim() == "vnc options:")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The listing this host's QEMU prints, which the parser is written for.
    const WINDOWS_HELP: &str = "\
Available display backend types:
none
gtk
sdl
egl-headless
curses
spice-app
dbus

Some display backends support suboptions, which can be set with
   -display backend,option=value,option=value...
";

    /// The listing the headless Linux box prints, which is the case the
    /// fallback exists for.
    const HEADLESS_HELP: &str = "\
Available display backend types:
none
spice-app
dbus

Some display backends support suboptions, which can be set with
   -display backend,option=value,option=value...
";

    #[test]
    fn the_backends_qemu_lists_are_read_off_its_help() {
        assert_eq!(
            listed(WINDOWS_HELP),
            [
                "none",
                "gtk",
                "sdl",
                "egl-headless",
                "curses",
                "spice-app",
                "dbus"
            ]
        );
        assert_eq!(listed(HEADLESS_HELP), ["none", "spice-app", "dbus"]);
        assert!(listed("qemu-system-x86_64: -display help: invalid option").is_empty());
    }

    #[test]
    fn a_host_with_a_window_backend_opens_a_window() {
        let offered = listed(WINDOWS_HELP);
        let chosen = pick(&offered, None, true, || {
            panic!("VNC must not be asked about")
        });
        assert_eq!(chosen.unwrap(), Window::Local("gtk"));
    }

    #[test]
    fn sdl_serves_when_gtk_was_not_built_in() {
        let offered = vec!["none".to_owned(), "sdl".to_owned()];
        let chosen = pick(&offered, None, true, || {
            panic!("VNC must not be asked about")
        });
        assert_eq!(chosen.unwrap(), Window::Local("sdl"));
    }

    #[test]
    fn a_headless_host_serves_the_screen_over_vnc() {
        let offered = listed(HEADLESS_HELP);
        assert_eq!(
            pick(&offered, None, false, || true).unwrap(),
            Window::Vnc("127.0.0.1:0".to_owned())
        );
        // And so does a host whose QEMU has no window backend even though a
        // session is there to open one on.
        assert_eq!(
            pick(&offered, None, true, || true).unwrap(),
            Window::Vnc("127.0.0.1:0".to_owned())
        );
    }

    #[test]
    fn a_session_is_not_enough_when_nothing_can_show_a_screen() {
        let offered = listed(HEADLESS_HELP);
        let refused = pick(&offered, None, true, || false)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("spice-app"), "{refused}");
        assert!(refused.contains("test-compositor"), "{refused}");
    }

    #[test]
    fn asking_for_vnc_puts_the_screen_there_even_where_a_window_could_open() {
        let offered = listed(WINDOWS_HELP);
        assert_eq!(
            pick(&offered, Some(":2"), true, || true).unwrap(),
            Window::Vnc(":2".to_owned())
        );
    }

    /// What QEMU 9.2.4 and 11.1 both print for `-vnc help`, from a process
    /// that then exits 1: the status says nothing, the listing says it all.
    const VNC_HELP: &str = "vnc options:
  audiodev=<str>
  connections=<num>
  display=<str>
  head=<num>
  password=<bool (on/off)>
";

    #[test]
    fn vnc_is_judged_by_what_the_help_printed_not_by_the_status() {
        assert!(takes_vnc(VNC_HELP));
        assert!(!takes_vnc(
            "qemu-system-x86_64: -vnc: VNC support is disabled"
        ));
        assert!(!takes_vnc(""));
    }

    #[test]
    fn a_bare_display_number_is_that_display_on_the_loopback() {
        assert_eq!(address_of("3"), "127.0.0.1:3");
        assert_eq!(address_of(":3"), ":3");
        assert_eq!(address_of("0.0.0.0:1"), "0.0.0.0:1");
    }

    #[test]
    fn a_vnc_display_names_the_port_a_viewer_connects_to() {
        assert_eq!(port("127.0.0.1:0"), 5900);
        assert_eq!(port(":2"), 5902);
        assert_eq!(port("0.0.0.0:11"), 5911);
    }

    #[test]
    fn the_display_argument_is_what_qemu_takes() {
        assert_eq!(
            Window::Headless.arguments(None),
            ["-display".to_owned(), "none".to_owned()]
        );
        assert_eq!(
            Window::Local("sdl").arguments(None),
            ["-display".to_owned(), "sdl".to_owned()]
        );
        assert_eq!(
            Window::Vnc("127.0.0.1:0".to_owned()).arguments(None),
            ["-display".to_owned(), "vnc=127.0.0.1:0".to_owned()]
        );
    }

    #[test]
    fn a_window_on_a_machine_with_a_card_can_reach_the_cards_console() {
        // GTK cannot be told which tab to open on, so it is told to show
        // them; VNC serves one console and is pointed at the card.
        assert_eq!(
            Window::Local("gtk").arguments(Some("gpu0")),
            ["-display".to_owned(), "gtk,show-tabs=on".to_owned()]
        );
        assert_eq!(
            Window::Vnc(":1".to_owned()).arguments(Some("gpu0")),
            [
                "-display".to_owned(),
                "vnc=:1,display=gpu0,head=0".to_owned()
            ]
        );
    }
}
