//! `test-steam-store`: Valve's Steam client on the `run-compositor
//! --everything` desktop, its 64-bit side on ferrousli, up to its sign-in
//! window, and with a test account signed in and showing its store
//! (`docs/STEAM.md` §1, stage 22's first exit step).
//!
//! # What boots
//!
//! The `--everything` desktop as `run-compositor` builds it: the same
//! merged volume (`crate::everything`), the same archive (`desktop`, with
//! ferrousli's loader at `/lib64` in glibc's place), the same lines that
//! start yserver and Steam (`desktop.sh`, then `client.sh` as uid 1000), the
//! same 16 GiB. What it leaves out is what a person watching wants and a
//! gate does not: the host's `hyprland.conf` and dotfiles, the terminal,
//! Chrome's window (its environment and its loader stay), the wallpaper and
//! the clipboard; and the 3D card, since QMP's `screendump` reads nothing
//! from `egl-headless` (`test-xwindow` leaves it out for the same reason),
//! so hyprix composites in software. Steam draws in software either way.
//! Without Chrome's window Steam's main window has the screen to itself, and
//! the boot has one browser starting rather than two.
//! `test-steam-window` is the other Steam boot, and a different one: its
//! own volume, `run.sh`, and the 64-bit side on the volume's glibc.
//!
//! In their place, `tools/common/steam/store-watch.sh` lists hyprix's
//! windows each time they change -- place, size and title -- which is how
//! the gate knows where the sign-in window's fields are and when the main
//! window has come.
//!
//! # The two steps
//!
//! The sign-in window: hyprix lists a window titled "Sign in to Steam", and
//! within [`DRAWN_WITHIN`] the screen has the colours of a drawn one, as
//! `test-steam-window` judges it, and the window its two fields. That needs
//! nobody's account.
//!
//! The store, when the account file is there ([`account_file`]): the gate
//! clicks the account name field, types the name, clicks the password field,
//! types the password, and presses Sign in, all through QMP on a US layout,
//! which the desktop is given. Then it waits for Steam's main window and
//! judges its store page on the screen ([`store_look`]). A sign-in window
//! that shows its error in red fails as refused, and one whose fields have
//! gone after [`GUARD_AFTER`] as Steam Guard asking for a code. Without the
//! file it says the store step was skipped and how to enable it, and passes
//! on the first step alone.
//!
//! # The account's secrecy
//!
//! The name and password go into the guest as key events and nowhere else:
//! every line the guest says once hyprix has started is redacted of both
//! before it is printed or logged (`Watching::redact`), and so is the
//! gate's error. A screen dump taken after they were typed may show the
//! name -- in its field, or in the store's header -- so it is kept only
//! shrunk [`SHRINK`] times, too small to read ([`shrunk`]). The volume is
//! attached under `snapshot=on`, so nothing the client keeps of the sign-in
//! survives the boot.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::boot::{absolute, build_desktop_image, button_event, press};
use super::browser::chrome_libc;
use super::desktop::{Backdrop, desktop, with_steam_volume};
use super::run::DESKTOP_DEFAULTS;
use super::steam_window::{self, DRAWN_COLOURS, SIGN_IN};
use super::{EITHER, Programs};
use crate::args::Args;
use crate::display::{DEVICE_ID, Image, Qmp, free_port, parse_ppm};
use crate::paths::{self, Arch};
use crate::ports::{Content, File};
use crate::qemu::Watching;
use crate::{Error, Result};

/// The watcher of hyprix's windows.
const WATCH: &[u8] = include_bytes!("../../../steam/store-watch.sh");

/// Where [`WATCH`] is in the image.
const WATCH_PATH: &str = "steam/store-watch.sh";

/// What the watcher prints before each window of a snapshot, then the
/// snapshot's number.
const WINDOW: &str = "steam-store: window ";

/// What it prints after a snapshot's windows, then the number.
const WINDOWS: &str = "steam-store: windows ";

/// What it prints before the connection log's answer to a logon.
const LOGON: &str = "steam-store: logon: ";

/// What `client.sh` prints when the client exits, then the status: 42 is
/// the restart after an update, anything else is the end of Steam.
const EXITED: &str = "steam-window: the client exited ";

/// What `desktop.sh` prints when it gives up before starting the client.
const NO_X: &str = "steam-desktop: no X server";

/// The title of Steam's main window.
const MAIN: &str = "Steam";

/// The least a window titled [`MAIN`] measures to be the main window: the
/// updater's is titled so too, at 400x129.
const MAIN_LEAST: (i32, i32) = (800, 500);

/// The keyboard layout the desktop is given, which [`key_for`] types for.
const LAYOUT: &str = "us";

/// The screen, as `run-compositor` makes it.
const SCREEN: (u32, u32) = crate::wallpaper::SCREEN;

/// The sign-in window's size as its places below were measured, on the
/// screen of 2026-09-29's `test-steam-window`: a place in a window of
/// another size is scaled to it.
const SIGN_IN_SIZE: (i32, i32) = (700, 440);

/// Where to click in the account name field, in the window.
const NAME_AT: (i32, i32) = (230, 140);

/// Where to click in the password field.
const PASSWORD_AT: (i32, i32) = (230, 214);

/// Where to click on Sign in.
const SIGN_IN_AT: (i32, i32) = (230, 305);

/// Where across a field its colour is looked for: right of the middle,
/// past where a name or a password's dots reach.
const PROBE_X: i32 = 395;

/// The fields' own grey.
const FIELD: [u8; 3] = [50, 53, 60];

/// How far a pixel may be from a colour, in each channel, and still be it:
/// the screen is composited, and scaled where the window is.
const NEAR: u8 = 8;

/// How long after Sign in the sign-in window may stay up before the gate
/// looks at what it shows instead of its fields.
const GUARD_AFTER: Duration = Duration::from_secs(180);

/// How often the sign-in window is looked at while it stays up after Sign
/// in.
const LOOK_EVERY: Duration = Duration::from_secs(15);

/// At least this many pixels of the sign-in window in its error's red
/// ([`is_error_red`]) is the sign-in refused: the message under Sign in
/// and the fields' outlines. A fresh window has none.
const REFUSED: usize = 200;

/// How long after Sign in the main window has to come: the client checks
/// the sign-in, may update again, and starts its browser's page in software.
const LOGIN: Duration = Duration::from_secs(900);

/// How long after the main window the store has to be on the screen.
const STORE: Duration = Duration::from_secs(900);

/// How often the store is looked for.
const STORE_EVERY: Duration = Duration::from_secs(10);

/// How often the watcher's lines are read while waiting for a window.
const POLL: Duration = Duration::from_secs(5);

/// How long the sign-in window has to be drawn in once hyprix lists it:
/// its frame comes first, and its page from the browser helper after,
/// between a second and some twenty on nazuna.
const DRAWN_WITHIN: Duration = Duration::from_secs(120);

/// The pause between two keys, and around a click: the keys cross hyprix,
/// yserver and the client's browser, on a loaded machine.
const KEY_GAP: Duration = Duration::from_millis(80);

/// By how much a screen dump taken after the account was typed is shrunk
/// before it is kept: a 16-pixel line of text becomes two pixels of grey,
/// and the page's layout and colours still show.
const SHRINK: usize = 8;

/// Of Steam's main window, at least this share must be the store's dark
/// blues ([`is_steam_blue`]). The store page Chrome drew on nazuna at
/// 1920x1000 on 2026-09-30 was 32% of them below its own header, with a
/// sale's banner taking a third of the page; the sign-in screen, 0.02%.
const STORE_BLUE: f64 = 0.15;

/// And it must have at least this many colours: the store's art, its
/// banner and its capsules, where an empty page, an error or the library
/// of an account with no games has a few hundred. That page had 314,674;
/// the sign-in screen has about 2,000.
const STORE_COLOURS: usize = 10_000;

/// A Steam test account: its name and password, which nothing prints.
struct Account {
    name: String,
    password: String,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Account { <redacted> }")
    }
}

/// Where the account is kept: `FERRIX_STEAM_ACCOUNT_FILE`, or
/// `~/.config/ferrix/steam-test-account`. Two lines, the account's name and
/// its password, readable by its owner alone; never in a checkout.
fn account_file() -> Option<PathBuf> {
    std::env::var_os("FERRIX_STEAM_ACCOUNT_FILE")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".config/ferrix/steam-test-account")))
}

/// The account in `path`, or `None` when there is no such file.
///
/// # Errors
///
/// A file others may read, one that cannot be read, or one that is not two
/// lines the gate can type. No error names what the file holds.
fn read_account(path: &Path) -> Result<Option<Account>> {
    if !path.exists() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|error| Error::new(format!("{}: {error}", path.display())))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(Error::new(format!(
                "{} holds a password and others may read it (mode {:o}): chmod 600 it",
                path.display(),
                mode & 0o777
            )));
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
    parse_account(&text)
        .map(Some)
        .map_err(|why| Error::new(format!("{}: {why}", path.display())))
}

/// The account in a file's `text`: the first line its name, the second its
/// password, each without its line's end.
fn parse_account(text: &str) -> std::result::Result<Account, &'static str> {
    let mut lines = text.lines().map(|line| line.trim_end_matches('\r'));
    let name = lines.next().unwrap_or_default().trim();
    let password = lines.next().unwrap_or_default();
    if name.is_empty() || password.is_empty() {
        return Err("want two lines, the account's name and then its password");
    }
    if lines.any(|line| !line.trim().is_empty()) {
        return Err("want two lines, the account's name and then its password, and nothing after");
    }
    if !name
        .chars()
        .chain(password.chars())
        .all(|c| key_for(c).is_some())
    {
        return Err(
            "the name or the password has a character other than printable ASCII, \
                    which the gate cannot type",
        );
    }
    Ok(Account {
        name: name.to_owned(),
        password: password.to_owned(),
    })
}

/// The key QMP names for `c` on a US layout, and whether Shift is held for
/// it; `None` for anything but printable ASCII.
fn key_for(c: char) -> Option<(&'static str, bool)> {
    const LETTERS: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    const PLAIN: [(char, &str); 12] = [
        (' ', "spc"),
        ('-', "minus"),
        ('=', "equal"),
        ('[', "bracket_left"),
        (']', "bracket_right"),
        ('\\', "backslash"),
        (';', "semicolon"),
        ('\'', "apostrophe"),
        ('`', "grave_accent"),
        (',', "comma"),
        ('.', "dot"),
        ('/', "slash"),
    ];
    const SHIFTED: [(char, &str); 21] = [
        ('!', "1"),
        ('@', "2"),
        ('#', "3"),
        ('$', "4"),
        ('%', "5"),
        ('^', "6"),
        ('&', "7"),
        ('*', "8"),
        ('(', "9"),
        (')', "0"),
        ('_', "minus"),
        ('+', "equal"),
        ('{', "bracket_left"),
        ('}', "bracket_right"),
        ('|', "backslash"),
        (':', "semicolon"),
        ('"', "apostrophe"),
        ('~', "grave_accent"),
        ('<', "comma"),
        ('>', "dot"),
        ('?', "slash"),
    ];
    let index = |base: char| usize::try_from(u32::from(c) - u32::from(base)).ok();
    match c {
        'a'..='z' => index('a')
            .and_then(|at| LETTERS.get(at))
            .map(|key| (*key, false)),
        'A'..='Z' => index('A')
            .and_then(|at| LETTERS.get(at))
            .map(|key| (*key, true)),
        '0'..='9' => index('0')
            .and_then(|at| DIGITS.get(at))
            .map(|key| (*key, false)),
        _ => PLAIN
            .iter()
            .find(|(plain, _)| *plain == c)
            .map(|(_, key)| (*key, false))
            .or_else(|| {
                SHIFTED
                    .iter()
                    .find(|(shifted, _)| *shifted == c)
                    .map(|(_, key)| (*key, true))
            }),
    }
}

/// A window of hyprix's, as the watcher lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    at: (i32, i32),
    size: (i32, i32),
    title: String,
}

impl Listed {
    /// A place measured in a [`SIGN_IN_SIZE`] window, on the screen.
    fn place(&self, (x, y): (i32, i32)) -> (i32, i32) {
        (
            self.at.0 + x * self.size.0 / SIGN_IN_SIZE.0,
            self.at.1 + y * self.size.1 / SIGN_IN_SIZE.1,
        )
    }

    /// Whether this is Steam's main window.
    fn is_main(&self) -> bool {
        self.title == MAIN && self.size.0 >= MAIN_LEAST.0 && self.size.1 >= MAIN_LEAST.1
    }

    /// Its area.
    fn area(&self) -> i64 {
        i64::from(self.size.0) * i64::from(self.size.1)
    }
}

/// One of the watcher's window lines, `<x>,<y> <w>,<h> <floating> <title>`.
fn parse_listed(text: &str) -> Option<Listed> {
    let mut words = text.splitn(4, ' ');
    let pair = |word: Option<&str>| -> Option<(i32, i32)> {
        let (a, b) = word?.split_once(',')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    };
    let at = pair(words.next())?;
    let size = pair(words.next())?;
    let _floating = words.next()?;
    let title = words.next().unwrap_or_default().trim().to_owned();
    Some(Listed { at, size, title })
}

/// The windows of the watcher's last whole snapshot in `lines`, or `None`
/// before its first.
fn latest_windows(lines: &[String]) -> Option<Vec<Listed>> {
    let number = lines.iter().rev().find_map(|line| {
        let (_, rest) = line.split_once(WINDOWS)?;
        let (number, end) = rest.split_once(':')?;
        (end.trim() == "end").then(|| number.trim().to_owned())
    })?;
    let prefix = format!("{number}: ");
    Some(
        lines
            .iter()
            .filter_map(|line| line.split_once(WINDOW))
            .filter_map(|(_, rest)| rest.strip_prefix(&prefix))
            .filter_map(parse_listed)
            .collect(),
    )
}

/// Why Steam will not show a window any more, if a line says it ended.
fn steam_ended(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        if line.contains(NO_X) {
            return Some(
                "desktop.sh found no X server on :0 and never started the client".to_owned(),
            );
        }
        let (_, status) = line.split_once(EXITED)?;
        (status.trim() != "42").then(|| format!("the client exited {}", status.trim()))
    })
}

/// The connection log's answers to the client's logons, as the watcher
/// said them.
fn logons(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.split_once(LOGON))
        .map(|(_, answer)| answer.trim().to_owned())
        .collect()
}

/// The titles of `windows`, for a message.
fn titles(windows: &[Listed]) -> String {
    if windows.is_empty() {
        return "none".to_owned();
    }
    windows
        .iter()
        .map(|window| format!("{:?}", window.title))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The pixel at `(x, y)`, if it is on the screen.
fn pixel(screen: &Image, x: i32, y: i32) -> Option<[u8; 3]> {
    let x = usize::try_from(x).ok().filter(|x| *x < screen.width)?;
    let y = usize::try_from(y).ok().filter(|y| *y < screen.height)?;
    let at = (y * screen.width + x) * 3;
    let bytes = screen.pixels.get(at..at + 3)?;
    Some([*bytes.first()?, *bytes.get(1)?, *bytes.get(2)?])
}

/// Whether `pixel` is within [`NEAR`] of `colour` in every channel.
fn near(pixel: [u8; 3], colour: [u8; 3]) -> bool {
    pixel
        .iter()
        .zip(colour)
        .all(|(have, want)| have.abs_diff(want) <= NEAR)
}

/// Whether most of the 9x9 patch around `(x, y)` is [`FIELD`]'s grey.
fn field_at(screen: &Image, (x, y): (i32, i32)) -> bool {
    let mut grey = 0;
    for dy in -4..=4 {
        for dx in -4..=4 {
            if pixel(screen, x + dx, y + dy).is_some_and(|colour| near(colour, FIELD)) {
                grey += 1;
            }
        }
    }
    grey * 10 >= 81 * 6
}

/// Whether `pixel` is the red the sign-in window says an error in: far
/// redder than it is green or blue.
fn is_error_red([r, g, b]: [u8; 3]) -> bool {
    r >= 150 && r >= g.saturating_add(60) && r >= b.saturating_add(60)
}

/// How many pixels of `window` on `screen` are [`is_error_red`].
fn error_red(screen: &Image, window: &Listed) -> usize {
    let (columns, rows) = covered(screen, window);
    rows.flat_map(|y| columns.clone().map(move |x| (x, y)))
        .filter(|(x, y)| pixel(screen, *x, *y).is_some_and(is_error_red))
        .count()
}

/// Whether `pixel` is one of the store's dark blues: its header's `#171d25`,
/// its menu bar's `#192534`, the page's `#0f1924` to `#1b2838` -- dark, and
/// bluer than it is red or green, where the sign-in window's greys are not.
fn is_steam_blue([r, g, b]: [u8; 3]) -> bool {
    b <= 110 && b >= r.saturating_add(12) && b >= g.saturating_add(6)
}

/// What the store judge measured in Steam's main window.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StoreLook {
    /// The share of the window that is [`is_steam_blue`].
    blue: f64,
    /// How many colours the window has.
    colours: usize,
}

impl StoreLook {
    /// Whether this is the store page.
    fn is_store(self) -> bool {
        self.blue >= STORE_BLUE && self.colours >= STORE_COLOURS
    }
}

impl std::fmt::Display for StoreLook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:.1}% of Steam's window the store's dark blues (at least {:.0}% wanted), {} \
             colours (at least {STORE_COLOURS})",
            self.blue * 100.0,
            STORE_BLUE * 100.0,
            self.colours
        )
    }
}

/// The part of the screen `window` covers, as columns and rows.
fn covered(screen: &Image, window: &Listed) -> (std::ops::Range<i32>, std::ops::Range<i32>) {
    let width = i32::try_from(screen.width).unwrap_or(i32::MAX);
    let height = i32::try_from(screen.height).unwrap_or(i32::MAX);
    let columns = window.at.0.clamp(0, width)..(window.at.0 + window.size.0).clamp(0, width);
    let rows = window.at.1.clamp(0, height)..(window.at.1 + window.size.1).clamp(0, height);
    (columns, rows)
}

/// Measure Steam's main window, `window`, on `screen` for the store.
fn store_look(screen: &Image, window: &Listed) -> StoreLook {
    let (columns, rows) = covered(screen, window);
    let mut blue = 0usize;
    let mut all = 0usize;
    let mut colours = std::collections::HashSet::new();
    for y in rows {
        for colour in columns.clone().filter_map(|x| pixel(screen, x, y)) {
            blue += usize::from(is_steam_blue(colour));
            all += 1;
            let _ = colours.insert(colour);
        }
    }
    StoreLook {
        blue: share(blue, all),
        colours: colours.len(),
    }
}

/// `part` of `whole`, as a fraction; none of nothing.
fn share(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    let to_float = |count: usize| f64::from(u32::try_from(count).unwrap_or(u32::MAX));
    to_float(part) / to_float(whole)
}

/// The commonest colours in `window` on `screen`, each rounded down to a
/// multiple of 16, with their shares: what a store judge that failed saw.
fn commonest(screen: &Image, window: &Listed) -> String {
    let (columns, rows) = covered(screen, window);
    let mut counts = std::collections::BTreeMap::<[u8; 3], usize>::new();
    let mut all = 0usize;
    for y in rows {
        for x in columns.clone() {
            if let Some(colour) = pixel(screen, x, y) {
                *counts
                    .entry(colour.map(|channel| channel & 0xf0))
                    .or_default() += 1;
                all += 1;
            }
        }
    }
    let mut counts: Vec<([u8; 3], usize)> = counts.into_iter().collect();
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    counts
        .iter()
        .take(8)
        .map(|([r, g, b], count)| {
            format!("#{r:02x}{g:02x}{b:02x} {:.1}%", share(*count, all) * 100.0)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `screen` shrunk `by` times each way, each pixel the mean of the block it
/// stands for: the layout and the colours, and no text to read.
fn shrunk(screen: &Image, by: usize) -> Image {
    let by = by.max(1);
    let width = screen.width / by;
    let height = screen.height / by;
    let mut pixels = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        for column in 0..width {
            pixels.extend(mean(screen, (column * by, row * by), by));
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

/// The mean colour of the `by`-pixel square of `screen` whose top left is
/// `at`.
fn mean(screen: &Image, at: (usize, usize), by: usize) -> [u8; 3] {
    let mut sum = [0usize; 3];
    for y in at.1..at.1 + by {
        let start = (y * screen.width + at.0) * 3;
        let line = screen.pixels.get(start..start + by * 3).unwrap_or_default();
        let (line, _) = line.as_chunks::<3>();
        for colour in line {
            for (total, channel) in sum.iter_mut().zip(colour) {
                *total += usize::from(*channel);
            }
        }
    }
    sum.map(|total| u8::try_from(total / (by * by).max(1)).unwrap_or(u8::MAX))
}

/// `image` as a binary PPM.
fn ppm(image: &Image) -> Vec<u8> {
    let mut bytes = format!("P6\n{} {}\n255\n", image.width, image.height).into_bytes();
    bytes.extend_from_slice(&image.pixels);
    bytes
}

/// `cargo xtask test-steam-store`.
///
/// # Errors
///
/// When a volume is missing, the image cannot be built, the boot fails, or
/// a step is not met; never with the account in the message.
pub(crate) fn test_steam_store(args: &Args) -> Result<()> {
    let arch = Arch::X86_64;
    let file = account_file();
    let account = match &file {
        Some(path) => read_account(path)?,
        None => None,
    };
    let args = everything_args(args)?;
    let programs = Programs::build(arch)?;
    let (config, mut carried) = desktop(arch, config(), SCREEN, false, Backdrop::Any, &args)?;
    carried.ports.push(File {
        path: WATCH_PATH.to_owned(),
        mode: 0o644,
        content: Content::Bytes(WATCH.to_vec()),
    });
    on_ferrousli(&carried.ports)?;
    let (image, kernel) =
        build_desktop_image(arch, &programs, &config, carried, &args, DESKTOP_DEFAULTS)?;
    let port = free_port()?;
    let qemu_args = Args {
        display: true,
        size: Some(SCREEN),
        qmp_port: Some(port),
        ..args.clone()
    };
    let shots = paths::build_dir(arch).join("steam-store");
    let _ = std::fs::remove_dir_all(&shots);
    std::fs::create_dir_all(&shots)
        .map_err(|error| Error::new(format!("making {}: {error}", shots.display())))?;
    println!(
        "  {arch}: Steam on the --everything desktop, its 64-bit side on ferrousli, {} MiB, \
         sign-in within {}s; screens in {}",
        args.memory,
        args.timeout,
        shots.display()
    );
    let secrets: Vec<String> = account
        .iter()
        .flat_map(|account| [account.name.clone(), account.password.clone()])
        .collect();
    let mut verdict: Option<Result<()>> = None;
    let hook = |watching: &mut Watching<'_>| -> Result<()> {
        watching.stop_when_done();
        for secret in &secrets {
            watching.redact(secret);
        }
        let mut qmp = Qmp::connect(port, Instant::now() + Duration::from_secs(10))?;
        let mut gate = Gate {
            watching,
            qmp: &mut qmp,
            shots: &shots,
            typed: false,
        };
        verdict = Some(gate.drive(account.as_ref(), file.as_deref(), args.timeout));
        Ok(())
    };
    let ran = crate::qemu::watch_then(arch, &image, &kernel, &qemu_args, EITHER, hook);
    let verdict = ran.and_then(|_| {
        verdict.unwrap_or_else(|| Err(Error::new(format!("{arch}: hyprix never started"))))
    });
    verdict.map_err(|error| Error::new(crate::qemu::redacted(&error.to_string(), &secrets)))
}

/// `args` as `run-compositor --everything` has them for Steam, less what
/// the module's header says a gate leaves out.
fn everything_args(args: &Args) -> Result<Args> {
    let arch = Arch::X86_64;
    let mut args = args.clone();
    // What `--everything` stands for (`Args::everything`), but the
    // clipboard, which has nobody to share with here, and the 3D card: QMP's
    // `screendump` has no surface to read from `egl-headless`, as
    // `test-xwindow` found. Steam draws in software either way; the card
    // only composites.
    args.everything = true;
    args.chrome = true;
    args.gl = false;
    args.no_gl = true;
    args.display = true;
    args.release = true;
    args.clipboard = false;
    args.layout = Some(LAYOUT.to_owned());
    args.variant = None;
    args.wallpaper = Some("none".to_owned());
    args.net = true;
    // Its error names the script that makes the volume.
    let _ = steam_window::volume()?;
    if !with_steam_volume(&args, arch) {
        return Err(Error::new(
            "the --everything desktop would not start Steam: docs/STEAM.md §1",
        ));
    }
    args.data_image = Some(crate::everything::volume()?);
    if !args.memory_given {
        args.memory = steam_window::MEMORY;
    }
    if !args.timeout_given {
        args.timeout = steam_window::TIMEOUT;
    }
    if args.accel.is_none() {
        args.accel = Some("kvm".to_owned());
    }
    chrome_libc(&mut args);
    if !crate::chrome::on_ferrousli(&args) {
        return Err(Error::new(
            "test-steam-store runs Steam's 64-bit side on ferrousli, as the --everything \
             desktop does; test-steam-window runs it on the volume's glibc",
        ));
    }
    Ok(args)
}

/// The desktop's configuration: the watcher, then what `run-compositor
/// --everything` adds for Chrome on ferrousli but its window, for yserver
/// and for Steam, in its order.
fn config() -> String {
    format!(
        "# Written into the initramfs by `cargo xtask test-steam-store` (docs/STEAM.md): the\n\
         # --everything desktop's Steam, with a watcher of hyprix's windows.\n\
         exec-once = /bin/busybox sh /{WATCH_PATH}\n\
         # As `run-compositor --everything` adds them for Chrome on ferrousli, less its window.\n\
         {}{}\n{}\n{}",
        crate::chrome::DESKTOP_ENV,
        crate::chrome::window_library_path(true),
        crate::yserver::desktop_config(),
        steam_window::desktop_config(),
    )
}

/// Refuse an archive whose `/lib64` is not ferrousli's loader: files under
/// it, and no link of it to the volume's glibc.
fn on_ferrousli(carried: &[File]) -> Result<()> {
    let loader = carried
        .iter()
        .any(|file| file.path.starts_with("lib64/") && matches!(file.content, Content::Bytes(_)));
    let linked = carried.iter().any(|file| file.path == "lib64");
    match (loader, linked) {
        (true, false) => {
            println!(
                "  x86_64: /lib64 holds ferrousli's loader, so Steam's 64-bit side runs on it"
            );
            Ok(())
        }
        (false, _) => Err(Error::new(
            "the desktop's archive has no loader in /lib64, so Steam's 64-bit side would not \
             run on ferrousli as the --everything desktop's does",
        )),
        (true, true) => Err(Error::new(
            "the desktop's archive has ferrousli's loader in /lib64 and a link of /lib64 to \
             the volume's glibc after it, which the initramfs refuses; one of the volumes' \
             desktop_files does not leave out a path the archive has files under",
        )),
    }
}

/// The hook's state: the guest, QEMU, where screens go, and whether the
/// account has been typed, after which a screen is kept only [`shrunk`].
struct Gate<'a, 'b> {
    watching: &'a mut Watching<'b>,
    qmp: &'a mut Qmp,
    shots: &'a Path,
    typed: bool,
}

impl std::fmt::Debug for Gate<'_, '_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Gate")
            .field("typed", &self.typed)
            .finish_non_exhaustive()
    }
}

impl Gate<'_, '_> {
    /// Both steps, or the first alone without an `account`.
    fn drive(
        &mut self,
        account: Option<&Account>,
        file: Option<&Path>,
        timeout: u64,
    ) -> Result<()> {
        let began = Instant::now();
        let sign_in = self.wait_for(
            "Steam's sign-in window",
            began + Duration::from_secs(timeout),
            |windows| {
                windows
                    .iter()
                    .find(|window| window.title == SIGN_IN)
                    .cloned()
            },
        )?;
        println!(
            "  steam-store: hyprix lists Steam's sign-in window after {}s, {}x{} at {},{}",
            began.elapsed().as_secs(),
            sign_in.size.0,
            sign_in.size.1,
            sign_in.at.0,
            sign_in.at.1
        );
        let (sign_in, size) = self.drawn(sign_in)?;
        let Some(account) = account else {
            println!(
                "  steam-store: the store step was skipped: no account file at {}. To run it, \
                 put a Steam test account's name and password there, one a line, mode 0600, \
                 with Steam Guard off (docs/STEAM.md §1); FERRIX_STEAM_ACCOUNT_FILE names \
                 another file",
                file.map_or_else(
                    || "~/.config/ferrix/steam-test-account".to_owned(),
                    |file| file.display().to_string()
                )
            );
            return Ok(());
        };
        self.sign_in(&sign_in, size, account)?;
        let main = self.signed_in(&sign_in)?;
        self.store(main)
    }

    /// Wait until the sign-in window `window` is drawn: [`DRAWN_COLOURS`]
    /// on the screen, as `test-steam-window` requires, and both its fields
    /// where they are, which an empty frame -- its gradient and its close
    /// button, 83 colours -- is not. Give the window as it is then listed,
    /// and the screen's size.
    fn drawn(&mut self, window: Listed) -> Result<(Listed, (usize, usize))> {
        let listed = Instant::now();
        let mut window = window;
        loop {
            let windows = self.read_windows()?;
            if let Some(now) = windows.into_iter().find(|window| window.title == SIGN_IN) {
                window = now;
            }
            let (screen, kept) = self.dump("sign-in")?;
            let colours = steam_window::colours(&screen);
            let fields = [NAME_AT, PASSWORD_AT]
                .iter()
                .filter(|at| field_at(&screen, window.place((PROBE_X, at.1))))
                .count();
            if colours >= DRAWN_COLOURS && fields == 2 {
                println!(
                    "  steam-store: Steam's sign-in window is drawn, its fields and {colours} \
                     colours, {}s after it was listed, from the --everything desktop's client \
                     on ferrousli: {}",
                    listed.elapsed().as_secs(),
                    kept.display()
                );
                return Ok((window, (screen.width, screen.height)));
            }
            if listed.elapsed() >= DRAWN_WITHIN {
                return Err(Error::new(format!(
                    "hyprix lists Steam's sign-in window, but {}s later the screen has \
                     {colours} colours (a drawn one has {DRAWN_COLOURS}) and {fields} of the \
                     window's two fields, whose grey {FIELD:?} is looked for at x {} in it: {}",
                    DRAWN_WITHIN.as_secs(),
                    PROBE_X,
                    kept.display()
                )));
            }
        }
    }

    /// Read the guest's lines until `found` finds a window in the watcher's
    /// latest list, `deadline` passes, or Steam ends.
    fn wait_for(
        &mut self,
        what: &str,
        deadline: Instant,
        found: impl Fn(&[Listed]) -> Option<Listed>,
    ) -> Result<Listed> {
        loop {
            let windows = self.read_windows()?;
            if let Some(window) = found(&windows) {
                return Ok(window);
            }
            if let Some(why) = steam_ended(self.watching.after()) {
                return Err(Error::new(format!(
                    "{what} did not come: {why}; the `steam-window:` lines say how far it got"
                )));
            }
            if Instant::now() >= deadline {
                return Err(Error::new(format!(
                    "{what} did not come in time; hyprix's windows were: {}. The \
                     `steam-window:` lines say how far the client got",
                    titles(&windows)
                )));
            }
        }
    }

    /// Read the guest's lines for [`POLL`], and give the watcher's latest
    /// windows.
    fn read_windows(&mut self) -> Result<Vec<Listed>> {
        let until = Instant::now() + POLL;
        let _ = self.watching.read_more(until, |_| false)?;
        // `read_more` returns early only when the serial port closed.
        if Instant::now() < until {
            return Err(Error::new("QEMU stopped while Steam was watched"));
        }
        Ok(latest_windows(self.watching.after()).unwrap_or_default())
    }

    /// Dump the screen and keep it as `<name>.ppm`: whole before the
    /// account was typed, [`shrunk`] after.
    fn dump(&mut self, name: &str) -> Result<(Image, PathBuf)> {
        let latest = self.shots.join("latest.ppm");
        self.qmp.screendump(Some(DEVICE_ID), &latest)?;
        let bytes = std::fs::read(&latest)
            .map_err(|error| Error::new(format!("reading {}: {error}", latest.display())))?;
        let screen = parse_ppm(&bytes)?;
        let kept = self.shots.join(format!("{name}.ppm"));
        if self.typed {
            let _ = std::fs::remove_file(&latest);
            write(&kept, &ppm(&shrunk(&screen, SHRINK)))?;
        } else {
            std::fs::rename(&latest, &kept)
                .map_err(|error| Error::new(format!("keeping {}: {error}", kept.display())))?;
        }
        Ok((screen, kept))
    }

    /// Type the account into the drawn sign-in window `window`, on a screen
    /// of `size`, and press Sign in.
    fn sign_in(&mut self, window: &Listed, size: (usize, usize), account: &Account) -> Result<()> {
        self.typed = true;
        self.click(window.place(NAME_AT), size)?;
        type_text(self.qmp, &account.name)?;
        self.click(window.place(PASSWORD_AT), size)?;
        type_text(self.qmp, &account.password)?;
        self.click(window.place(SIGN_IN_AT), size)?;
        println!(
            "  steam-store: typed the test account's name and password (neither is shown) \
             and pressed Sign in"
        );
        Ok(())
    }

    /// Put the pointer at `at` on a screen of `size` and click there.
    fn click(&mut self, at: (i32, i32), size: (usize, usize)) -> Result<()> {
        let tablet = |at: i32, across: usize| {
            let across = i32::try_from(across.max(1)).unwrap_or(i32::MAX);
            at.clamp(0, across) * 0x7FFF / across
        };
        self.qmp.input_send_event(&[
            absolute("x", tablet(at.0, size.0)),
            absolute("y", tablet(at.1, size.1)),
        ])?;
        std::thread::sleep(KEY_GAP * 4);
        self.qmp.input_send_event(&[button_event("left", true)])?;
        std::thread::sleep(KEY_GAP);
        self.qmp.input_send_event(&[button_event("left", false)])?;
        std::thread::sleep(KEY_GAP * 4);
        Ok(())
    }

    /// Wait for Steam's main window after Sign in, the sign-in window `was`
    /// gone; or say why it did not come.
    fn signed_in(&mut self, was: &Listed) -> Result<Listed> {
        let pressed = Instant::now();
        let mut looked = pressed;
        loop {
            let windows = self.read_windows()?;
            let asking = windows.iter().any(|window| window.title == SIGN_IN);
            let main = windows
                .iter()
                .filter(|window| window.is_main())
                .max_by_key(|window| window.area());
            if let (Some(main), false) = (main, asking) {
                println!(
                    "  steam-store: signed in: hyprix lists Steam's main window after {}s, \
                     {}x{}; the connection log's answers: {:?}",
                    pressed.elapsed().as_secs(),
                    main.size.0,
                    main.size.1,
                    logons(self.watching.after())
                );
                return Ok(main.clone());
            }
            if let Some(why) = steam_ended(self.watching.after()) {
                return Err(Error::new(format!(
                    "Steam's main window did not come: {why}"
                )));
            }
            if asking && looked.elapsed() >= LOOK_EVERY {
                looked = Instant::now();
                self.still_asking(was, pressed.elapsed())?;
            }
            if pressed.elapsed() >= LOGIN {
                let (_, kept) = self.dump("not-signed-in")?;
                return Err(Error::new(format!(
                    "Steam did not sign in within {}s of Sign in: hyprix's windows are {}, and \
                     the connection log's answers {:?}. With the sign-in window still up, the \
                     name or password was refused (check the account file), Steam is refusing \
                     sign-ins after several failures, or the keys did not reach the fields. \
                     The screen, shrunk past reading: {}",
                    LOGIN.as_secs(),
                    titles(&windows),
                    logons(self.watching.after()),
                    kept.display()
                )));
            }
        }
    }

    /// The sign-in window `window` is still up `after` Sign in: fail if it
    /// shows its error in red, which is the sign-in refused, or, from
    /// [`GUARD_AFTER`], if its account name field has gone, which is the
    /// client asking for more.
    fn still_asking(&mut self, window: &Listed, after: Duration) -> Result<()> {
        let (screen, kept) = self.dump("after-sign-in")?;
        let red = error_red(&screen, window);
        if red >= REFUSED {
            return Err(Error::new(format!(
                "Steam refused the sign-in: {}s after Sign in its sign-in window shows its error \
                 in red ({red} pixels) and still has its fields. The name or the password in the \
                 account file is not the account's, or Steam is refusing sign-ins from this \
                 address after several failures, which passes within the hour. The screen, \
                 shrunk past reading: {}",
                after.as_secs(),
                kept.display()
            )));
        }
        if after < GUARD_AFTER || field_at(&screen, window.place((PROBE_X, NAME_AT.1))) {
            return Ok(());
        }
        Err(Error::new(format!(
            "Steam's sign-in window is still up {}s after Sign in, and its account name field \
             is gone from it: the client is asking for something after the password, which for \
             a test account is a Steam Guard code. The account must have Steam Guard turned \
             off: the mobile authenticator needs the phone, and email Steam Guard sends a code \
             for every new machine, which each boot of this gate is (its volume is a \
             snapshot); the gate can type neither. The screen, shrunk past reading: {}",
            after.as_secs(),
            kept.display()
        )))
    }

    /// Look for the store page in Steam's main window `main` every
    /// [`STORE_EVERY`], until [`STORE`].
    fn store(&mut self, mut main: Listed) -> Result<()> {
        let began = Instant::now();
        loop {
            let until = Instant::now() + STORE_EVERY;
            let _ = self.watching.read_more(until, |_| false)?;
            if let Some(now) = latest_windows(self.watching.after())
                .unwrap_or_default()
                .into_iter()
                .filter(Listed::is_main)
                .max_by_key(Listed::area)
            {
                main = now;
            }
            let (screen, kept) = self.dump("store")?;
            let look = store_look(&screen, &main);
            if look.is_store() {
                println!(
                    "  steam-store: Steam's store is on the screen {}s after its main window: \
                     {look}; the screen, shrunk past reading: {}",
                    began.elapsed().as_secs(),
                    kept.display()
                );
                return Ok(());
            }
            println!("  steam-store: not the store yet: {look}");
            if began.elapsed() >= STORE {
                return Err(Error::new(format!(
                    "Steam's main window did not show its store within {}s: {look}; its \
                     commonest colours are {}. The screen, shrunk past reading: {}",
                    STORE.as_secs(),
                    commonest(&screen, &main),
                    kept.display()
                )));
            }
        }
    }
}

/// Type `text` as key presses on a US layout. Nothing of it is said, in an
/// error either: QEMU's reply to a key is not passed on.
fn type_text(qmp: &mut Qmp, text: &str) -> Result<()> {
    for c in text.chars() {
        let (key, shifted) = key_for(c)
            .ok_or_else(|| Error::new("the account has a character the gate cannot type"))?;
        let keys: &[&str] = if shifted { &["shift", key] } else { &[key] };
        press(qmp, keys).map_err(|_| {
            Error::new("QEMU refused a key the gate typed into Steam's sign-in window")
        })?;
        std::thread::sleep(KEY_GAP);
    }
    Ok(())
}

/// Write `bytes` to `path`.
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)
        .map_err(|error| Error::new(format!("writing {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two lines, name then password; a line's end is not the password's,
    /// and nothing the gate cannot type or a third line is taken. No
    /// error says what the file held.
    #[test]
    fn an_account_file_is_two_typeable_lines() {
        let account = parse_account("tester\r\np4ss word!\r\n").expect("an account");
        assert_eq!(account.name, "tester");
        assert_eq!(account.password, "p4ss word!");
        assert_eq!(format!("{account:?}"), "Account { <redacted> }");
        for bad in [
            "",
            "tester\n",
            "tester\n\n",
            "tester\npässword\n",
            "a\nb\nc\n",
        ] {
            let why = parse_account(bad).expect_err(bad);
            assert!(
                !why.contains("tester") && !why.contains("pässword"),
                "{why}"
            );
        }
    }

    /// Every printable ASCII character has a key, Shift where a US keyboard
    /// wants it, and nothing else has one.
    #[test]
    fn every_printable_character_has_a_key() {
        for c in ' '..='~' {
            assert!(key_for(c).is_some(), "{c:?}");
        }
        assert_eq!(key_for('a'), Some(("a", false)));
        assert_eq!(key_for('Z'), Some(("z", true)));
        assert_eq!(key_for('7'), Some(("7", false)));
        assert_eq!(key_for('&'), Some(("7", true)));
        assert_eq!(key_for('_'), Some(("minus", true)));
        assert_eq!(key_for('é'), None);
        assert_eq!(key_for('\n'), None);
    }

    /// The last whole snapshot wins; a title keeps its spaces; a snapshot
    /// with no end line yet is not read.
    #[test]
    fn the_watchers_latest_snapshot_is_read() {
        let lines: Vec<String> = [
            "[  12.0] steam-store: window 1: 0,0 1920,1080 0 term",
            "steam-store: windows 1: end",
            "steam-store: window 2: 610,320 700,440 1 Sign in to Steam",
            "steam-store: windows 2: end",
            "steam-store: window 3: 0,0 1920,1080 0 Steam",
        ]
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
        let windows = latest_windows(&lines).expect("a snapshot");
        assert_eq!(
            windows,
            vec![Listed {
                at: (610, 320),
                size: (700, 440),
                title: SIGN_IN.to_owned()
            }]
        );
        assert_eq!(latest_windows(&lines[..0]), None);
        assert!(!windows[0].is_main());
        let updater = parse_listed("760,475 400,129 1 Steam").expect("a window");
        assert!(!updater.is_main());
        let main = parse_listed("0,0 1920,1080 0 Steam").expect("a window");
        assert!(main.is_main());
        let empty = vec!["steam-store: windows 4: end".to_owned()];
        assert_eq!(latest_windows(&empty), Some(Vec::new()));
    }

    /// A place measured in the sign-in window lands in a window of its size
    /// wherever it is, and scales with a window of another.
    #[test]
    fn a_place_in_the_sign_in_window_follows_the_window() {
        let window = Listed {
            at: (610, 320),
            size: (700, 440),
            title: SIGN_IN.to_owned(),
        };
        assert_eq!(window.place(NAME_AT), (840, 460));
        let doubled = Listed {
            size: (1400, 880),
            ..window
        };
        assert_eq!(doubled.place(SIGN_IN_AT), (610 + 460, 320 + 610));
    }

    /// ferrousli's loader in `/lib64` and no link there is the `--everything`
    /// desktop; a link after it, or no loader, is not.
    #[test]
    fn only_ferrousli_s_loader_in_lib64_is_the_desktops() {
        let loader = File {
            path: "lib64/ld-linux-x86-64.so.2".to_owned(),
            mode: 0o755,
            content: Content::Bytes(Vec::new()),
        };
        let link = File {
            path: "lib64".to_owned(),
            mode: 0o777,
            content: Content::Link("/data/usr/lib64".to_owned()),
        };
        assert!(on_ferrousli(std::slice::from_ref(&loader)).is_ok());
        assert!(on_ferrousli(&[loader, link.clone()]).is_err());
        assert!(on_ferrousli(&[link]).is_err());
    }

    /// The client's restart after an update is not its end; any other
    /// status is, and so is no X server.
    #[test]
    fn steam_has_ended_only_on_a_final_exit() {
        let said = |line: &str| vec![line.to_owned()];
        assert_eq!(
            steam_ended(&said("steam-window: the client exited 42")),
            None
        );
        assert!(steam_ended(&said("steam-window: the client exited 139")).is_some());
        assert!(steam_ended(&said("steam-desktop: no X server on :0 after 120s")).is_some());
        assert_eq!(
            steam_ended(&said("steam-window: starting the client, try 2")),
            None
        );
    }

    /// A screen of `size` filled with `colour`.
    fn filled(size: (usize, usize), colour: [u8; 3]) -> Image {
        Image {
            width: size.0,
            height: size.1,
            pixels: colour.repeat(size.0 * size.1),
        }
    }

    /// Paint rows `rows` of `screen`, all across, `colour`.
    fn paint_rows(screen: &mut Image, rows: std::ops::Range<usize>, colour: [u8; 3]) {
        for y in rows {
            for x in 0..screen.width {
                let at = (y * screen.width + x) * 3;
                screen.pixels[at..at + 3].copy_from_slice(&colour);
            }
        }
    }

    /// A window of the store's dark blues with art in it is the store; the
    /// same art on the sign-in window's grey is not, nor the blues bare.
    /// Outside the window nothing counts.
    #[test]
    fn the_store_is_its_dark_blues_and_its_art() {
        let window = Listed {
            at: (0, 0),
            size: (200, 100),
            title: MAIN.to_owned(),
        };
        // Art: a block of 12,800 colours, one a pixel.
        let art = |screen: &mut Image| {
            for y in 0..64 {
                for x in 0..200 {
                    let at = (y * screen.width + x) * 3;
                    let colour = [u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 200];
                    screen.pixels[at..at + 3].copy_from_slice(&colour);
                }
            }
        };
        let mut store = filled((400, 100), [0x19, 0x25, 0x34]);
        art(&mut store);
        let look = store_look(&store, &window);
        assert!(look.is_store(), "{look}");
        assert!(
            look.colours >= STORE_COLOURS && (look.blue - 0.36).abs() < 0.01,
            "{look}"
        );

        let mut grey = filled((400, 100), [0x19, 0x1a, 0x1e]);
        art(&mut grey);
        assert!(!store_look(&grey, &window).is_store());
        let bare = store_look(&filled((400, 100), [0x0f, 0x19, 0x24]), &window);
        assert!(!bare.is_store() && bare.blue > 0.99, "{bare}");
        for (colour, blue) in [
            ([0x17, 0x1d, 0x25], true),
            ([0x1b, 0x28, 0x38], true),
            ([50, 53, 60], false),
            ([25, 26, 30], false),
            ([12, 179, 255], false),
        ] {
            assert_eq!(is_steam_blue(colour), blue, "{colour:?}");
        }
        let away = Listed {
            at: (500, 500),
            ..window
        };
        assert_eq!(store_look(&store, &away).colours, 0);
    }

    /// The sign-in window's error is its red, counted in the window alone;
    /// its blue button and grey fields are not red.
    #[test]
    fn a_refused_sign_in_is_red() {
        let window = Listed {
            at: (0, 0),
            size: (100, 50),
            title: SIGN_IN.to_owned(),
        };
        let mut screen = filled((200, 50), FIELD);
        paint_rows(&mut screen, 0..2, [12, 179, 255]);
        assert_eq!(error_red(&screen, &window), 0);
        paint_rows(&mut screen, 40..45, [195, 87, 85]);
        assert_eq!(error_red(&screen, &window), 500);
        assert!(error_red(&screen, &window) >= REFUSED);
    }

    /// The field's grey is found where most of a patch is it, and not on
    /// the window's darker background or a field holding text.
    #[test]
    fn a_field_is_its_grey() {
        let mut screen = filled((40, 40), [33, 35, 40]);
        paint_rows(&mut screen, 10..30, FIELD);
        assert!(field_at(&screen, (20, 20)));
        assert!(!field_at(&screen, (20, 5)));
        assert!(!field_at(&screen, (-50, 20)));
    }

    /// A shrunk screen is each block's mean: one-pixel text in black and
    /// white is an even grey, and a plain colour stays itself.
    #[test]
    fn a_shrunk_screen_has_no_text_left() {
        let mut text = filled((16, 8), [255, 255, 255]);
        for (index, pixel) in text.pixels.chunks_mut(3).enumerate() {
            if (index + index / 16) % 2 == 0 {
                pixel.copy_from_slice(&[0, 0, 0]);
            }
        }
        let small = shrunk(&text, 8);
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.pixels, vec![127, 127, 127, 127, 127, 127]);
        let plain = shrunk(&filled((16, 16), [0x1b, 0x28, 0x38]), 8);
        assert_eq!(plain.pixels, [0x1b, 0x28, 0x38].repeat(4));
        assert!(ppm(&small).starts_with(b"P6\n2 1\n255\n"));
    }
}
