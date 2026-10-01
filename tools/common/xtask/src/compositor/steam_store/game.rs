//! `test-steam-game`: on from `test-steam-store`'s store, a native Linux
//! game installed from Steam and started by it (stage 22's second exit step,
//! less its GPU path and its sound).
//!
//! The game is Teeworlds ([`APP`]): free, and free to claim (`isfreeapp`,
//! so steamcmd's `app_license_request` adds it to an account, where
//! `OpenTTD`'s package refuses both that and the store's web route); a Linux
//! build that Steam runs without a container (its recommended runtime is
//! `native` and no compatibility tool is mapped to it, where Battle for
//! Wesnoth, Endless Sky and `DDNet` get the Steam Linux Runtime's, which
//! needs pressure-vessel); about 10 MB; drawing with SDL 2 and OpenGL, which
//! llvmpipe serves.
//!
//! `tools/common/steam/game-watch.sh` does the guest's half: once the client
//! has signed in, it hands it `steam://install/<app>`, as a second `steam`
//! would, says how the app's manifest changes until it is installed, hands
//! it `steam://rungameid/<app>`, and says when the game's process runs. The
//! gate follows on the screen: each window that is not Steam's main one is
//! dumped cropped to itself, Steam's dialogs and the game's, and none of
//! them shows the account. The game is started when hyprix lists a window
//! whose title begins [`TITLE`] and that window has [`DRAWN`] colours.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{
    Content, Error, File, Gate, Image, Listed, Result, SIGN_IN, covered, pixel, ppm, steam_ended,
    titles, write,
};

/// The game: Teeworlds' app id.
const APP: &str = "380840";

/// What the game's window's title begins with.
const TITLE: &str = "Teeworlds";

/// The guest's half.
const WATCH: &[u8] = include_bytes!("../../../../steam/game-watch.sh");

/// Where the archive carries it.
const WATCH_PATH: &str = "steam/game-watch.sh";

/// The line the desktop's configuration starts it with.
pub(super) const EXEC_ONCE: &str = "exec-once = /bin/busybox sh /steam/game-watch.sh\n";

/// The line the guest's half says as it asks for the install.
const ASKED: &str = "steam-game: handing the client steam://install/";

/// The line it says when no manifest comes of the install.
const NO_MANIFEST: &str = "steam-game: no manifest ";

/// How many rows at the top of Steam's main window are its header, which
/// names the account, and are never kept.
const HEADER: i32 = 120;

/// How wide the Install button is: 200 pixels on a 1878-pixel window.
const INSTALL_WIDE: std::ops::RangeInclusive<i32> = 120..=320;

/// How tall the Install button is: 32 pixels.
const INSTALL_TALL: std::ops::RangeInclusive<i32> = 20..=56;

/// How much of its bounds the Install button's blue fills at least: its
/// label is cut out of it.
const INSTALL_FILLED: f64 = 0.7;

/// How far right of the Install button the Cancel button's grey is
/// looked for: the gap between them is 14 pixels.
const CANCEL_AFTER: i32 = 40;

/// Where the middle of the dialog's "Create an application shortcut" box
/// is from the Install button's top left: 22 pixels square, up and right
/// of it (measured 2026-10-01).
const SHORTCUT_BOX: (i32, i32) = (123, -245);

/// Half the side of the square the tick is looked for in.
const SHORTCUT_HALF: i32 = 10;

/// How many pixels of the button's blue a ticked box has at least: its
/// tick has about forty.
const TICKED: usize = 12;

/// How long the dialog is given to draw the box unticked.
const UNTICK_SHOWS: Duration = Duration::from_secs(2);

/// How long after the install was asked for the gate looks for the Install
/// dialog.
const DIALOG_WITHIN: Duration = Duration::from_secs(120);

/// The line it says once the game is installed.
const INSTALLED: &str = "steam-game: installed in ";

/// How long after the store the game may take to install and start.
const PLAYED: Duration = Duration::from_secs(3600);

/// How many colours the game's window has once it draws: Teeworlds' menu,
/// drawn over its map, has thousands; an empty window is
/// one.
const DRAWN: usize = 64;

/// How long the game's window has to draw once hyprix lists it.
const DRAWN_WITHIN: Duration = Duration::from_secs(180);

/// The guest's half, as the archive carries it.
pub(super) fn watcher() -> File {
    File {
        path: WATCH_PATH.to_owned(),
        mode: 0o644,
        content: Content::Bytes(WATCH.to_vec()),
    }
}

impl Gate<'_, '_> {
    /// From the store: follow the install and the start, dump each window
    /// that comes, and require the game's drawn.
    pub(super) fn game(&mut self) -> Result<()> {
        let began = Instant::now();
        let mut seen: Vec<Listed> = Vec::new();
        let mut asked_at: Option<Instant> = None;
        let mut pressed = false;
        loop {
            let windows = self.read_windows()?;
            if let Some(why) = steam_ended(self.watching.after()) {
                return Err(Error::new(format!(
                    "Steam ended before its game was started: {why}"
                )));
            }
            if self.guest_says(NO_MANIFEST) {
                let kept = self.dump_main(&windows)?;
                return Err(Error::new(format!(
                    "Steam did not start installing Teeworlds after steam://install/{APP}: no \
                     manifest came. Either the test account does not have it in its library \
                     (add it once: steamcmd's `+login <account> <password> \
                     +app_license_request {APP}` claims it, as it is free; docs/STEAM.md §1), \
                     or Steam asks something in its main window that the gate does not answer: \
                     {}. The gate presses nothing on a store page: a paid button there looks \
                     like the free one",
                    kept.map_or_else(
                        || "no main window to show".to_owned(),
                        |kept| format!("the main window less its header, {}", kept.display())
                    )
                )));
            }
            let installed = self.guest_says(INSTALLED);
            if installed
                && let Some(game) = windows
                    .iter()
                    .find(|window| window.title.starts_with(TITLE))
            {
                return self.drawn_game(game.clone(), began);
            }
            // The windows that came before the install was asked for, the
            // friends list among them, are Steam's own.
            let asked = self.guest_says(ASKED);
            if asked && asked_at.is_none() {
                asked_at = Some(Instant::now());
            }
            if !pressed && asked_at.is_some_and(|at| at.elapsed() < DIALOG_WITHIN) {
                pressed = self.press_install(&windows)?;
            }
            let new: Vec<Listed> = windows
                .iter()
                .filter(|window| !window.is_main() && window.title != SIGN_IN)
                .filter(|window| !seen.contains(window))
                .cloned()
                .collect();
            for window in new {
                seen.push(window.clone());
                if asked {
                    self.came(&window, seen.len())?;
                }
            }
            if began.elapsed() >= PLAYED {
                return Err(Error::new(format!(
                    "Teeworlds (app {APP}) was not installed and started within {}s of the \
                     store{}; hyprix's windows are {}. The guest's `steam-game:` lines say how \
                     far it got, and each window that came is in {}",
                    PLAYED.as_secs(),
                    if installed { " (it was installed)" } else { "" },
                    titles(&windows),
                    self.shots.display()
                )));
            }
        }
    }

    /// Press the Install button of the Install dialog Steam opens in its main
    /// window among `windows` for `steam://install`, if the dialog is there
    /// ([`install_button`]); keep the main window, less what may show the
    /// account ([`page_only`]), as `install-dialog.ppm` first. Give whether
    /// it was pressed.
    fn press_install(&mut self, windows: &[Listed]) -> Result<bool> {
        let Some(main) = windows
            .iter()
            .filter(|window| window.is_main())
            .max_by_key(|window| window.area())
        else {
            return Ok(false);
        };
        let screen = self.screen()?;
        let Some(button) = install_button(&screen, main) else {
            return Ok(false);
        };
        let kept = self.shots.join("install-dialog.ppm");
        write(&kept, &ppm(&page_only(&screen, main, windows)))?;
        self.untick_shortcut(&screen, &button)?;
        let at = (
            button.at.0 + button.size.0 / 2,
            button.at.1 + button.size.1 / 2,
        );
        self.click(at, (screen.width, screen.height))?;
        println!(
            "  steam-game: pressed Install in Steam's Install dialog, its button {}x{} at {},{}: \
             {}",
            button.size.0,
            button.size.1,
            button.at.0,
            button.at.1,
            kept.display()
        );
        Ok(true)
    }

    /// Untick the Install dialog's "Create an application shortcut", whose
    /// box is at [`SHORTCUT_BOX`] from `button` on `screen`, if it is
    /// ticked, and require it unticked after. With it ticked, Steam ran
    /// `xdg-icon-resource`, which the volume does not have, and the client
    /// then ended "pure virtual method called" (2026-10-01); a gate needs
    /// no shortcut.
    fn untick_shortcut(&mut self, screen: &Image, button: &Listed) -> Result<()> {
        let at = (button.at.0 + SHORTCUT_BOX.0, button.at.1 + SHORTCUT_BOX.1);
        if !ticked(screen, at) {
            return Ok(());
        }
        self.click(at, (screen.width, screen.height))?;
        std::thread::sleep(UNTICK_SHOWS);
        if ticked(&self.screen()?, at) {
            return Err(Error::new(format!(
                "the Install dialog's \"Create an application shortcut\" stayed ticked after the \
                 gate clicked its box at {},{}",
                at.0, at.1
            )));
        }
        println!("  steam-game: unticked \"Create an application shortcut\" in the Install dialog");
        Ok(())
    }

    /// The whole screen, kept nowhere.
    fn screen(&mut self) -> Result<Image> {
        let latest = self.shots.join("latest.ppm");
        self.qmp
            .screendump(Some(crate::display::DEVICE_ID), &latest)?;
        let bytes = std::fs::read(&latest)
            .map_err(|error| Error::new(format!("reading {}: {error}", latest.display())))?;
        let _ = std::fs::remove_file(&latest);
        crate::display::parse_ppm(&bytes)
    }

    /// Whether the guest says a line containing `what`.
    fn guest_says(&self, what: &str) -> bool {
        self.watching.after().iter().any(|line| line.contains(what))
    }

    /// A window that is not Steam's main one came after the install was
    /// asked for, the `number`th the gate has seen: dump it, cropped to
    /// itself. Nothing is pressed in it.
    fn came(&mut self, window: &Listed, number: usize) -> Result<()> {
        let kept = self.dump_window(&format!("window-{number}"), window)?;
        println!(
            "  steam-game: hyprix lists {:?}, {}x{} at {},{}: {}",
            window.title,
            window.size.0,
            window.size.1,
            window.at.0,
            window.at.1,
            kept.display()
        );
        Ok(())
    }

    /// Keep Steam's main window among `windows` as `main-window.ppm`, less
    /// what may show the account ([`page_only`]), and give where; nothing
    /// without a main window.
    fn dump_main(&mut self, windows: &[Listed]) -> Result<Option<PathBuf>> {
        let Some(main) = windows
            .iter()
            .filter(|window| window.is_main())
            .max_by_key(|window| window.area())
        else {
            return Ok(None);
        };
        let page = page_only(&self.screen()?, main, windows);
        let kept = self.shots.join("main-window.ppm");
        write(&kept, &ppm(&page))?;
        Ok(Some(kept))
    }

    /// Wait for the game's window `game` to draw: [`DRAWN`] colours in it.
    fn drawn_game(&mut self, mut game: Listed, began: Instant) -> Result<()> {
        let listed = Instant::now();
        loop {
            let (screen, kept) = self.dump_window_image("game", &game)?;
            let colours = colours_in(&screen);
            if colours >= DRAWN {
                println!(
                    "  steam-game: Teeworlds, installed from Steam and started by it, draws in \
                     its window {:?}, {}x{}, {colours} colours, {}s after the store: {}",
                    game.title,
                    game.size.0,
                    game.size.1,
                    began.elapsed().as_secs(),
                    kept.display()
                );
                return Ok(());
            }
            if listed.elapsed() >= DRAWN_WITHIN {
                return Err(Error::new(format!(
                    "hyprix lists Teeworlds' window {:?}, but {}s later it has {colours} colours \
                     (a drawn one has {DRAWN}): {}",
                    game.title,
                    DRAWN_WITHIN.as_secs(),
                    kept.display()
                )));
            }
            let windows = self.read_windows()?;
            if let Some(now) = windows
                .into_iter()
                .find(|window| window.title.starts_with(TITLE))
            {
                game = now;
            } else if let Some(why) = steam_ended(self.watching.after()) {
                return Err(Error::new(format!(
                    "Teeworlds' window went before it drew: {why}"
                )));
            }
        }
    }

    /// Dump the screen and keep `window` of it as `<name>.ppm`, whole.
    fn dump_window(&mut self, name: &str, window: &Listed) -> Result<PathBuf> {
        self.dump_window_image(name, window).map(|(_, kept)| kept)
    }

    /// Dump the screen, keep `window` of it as `<name>.ppm`, whole, and give
    /// that part.
    fn dump_window_image(&mut self, name: &str, window: &Listed) -> Result<(Image, PathBuf)> {
        let part = cropped(&self.screen()?, window);
        let kept = self.shots.join(format!("{name}.ppm"));
        write(&kept, &ppm(&part))?;
        Ok((part, kept))
    }
}

/// Steam's main window `main` on `screen` without what may show the
/// account: its top [`HEADER`] rows, and every other of `windows` over it,
/// blacked out.
fn page_only(screen: &Image, main: &Listed, windows: &[Listed]) -> Image {
    let below = Listed {
        at: (main.at.0, main.at.1 + HEADER),
        size: (main.size.0, (main.size.1 - HEADER).max(0)),
        title: String::new(),
    };
    let mut page = cropped(screen, &below);
    let (columns, rows) = covered(screen, &below);
    let others: Vec<&Listed> = windows.iter().filter(|window| *window != main).collect();
    for (row, y) in rows.enumerate() {
        for (column, x) in columns.clone().enumerate() {
            let over = others.iter().any(|window| {
                (window.at.0..window.at.0 + window.size.0).contains(&x)
                    && (window.at.1..window.at.1 + window.size.1).contains(&y)
            });
            let at = (row * page.width + column) * 3;
            if over && let Some(pixel) = page.pixels.get_mut(at..at + 3) {
                pixel.fill(0);
            }
        }
    }
    page
}
/// Whether `pixel` is the blue of the Install dialog's Install button, its
/// gradient from `#47bfff` to `#1a44c2`: far bluer than it is red.
fn is_install_blue([r, g, b]: [u8; 3]) -> bool {
    b >= 180 && b >= r.saturating_add(90) && b >= g.saturating_add(10)
}

/// Whether `pixel` is the grey of the dialog's Cancel button, `#3d4450`
/// (61, 68, 80) on 2026-10-01, and not the dialog's darker `#282c32`
/// around it.
fn is_cancel_grey([r, g, b]: [u8; 3]) -> bool {
    (53..=70).contains(&r) && (60..=77).contains(&g) && (72..=90).contains(&b)
}

/// Where the Install dialog's Install button is in `window` on `screen`:
/// a patch of [`is_install_blue`] with a button's shape
/// ([`INSTALL_WIDE`], [`INSTALL_TALL`], [`INSTALL_FILLED`]), and right of
/// it, level with it, the Cancel button's grey. The dialog's drive bar is
/// the same blue and wider than a button; nothing else on the screen has
/// a Cancel beside it.
fn install_button(screen: &Image, window: &Listed) -> Option<Listed> {
    let (columns, rows) = covered(screen, window);
    let blue = |x: i32, y: i32| {
        columns.contains(&x)
            && rows.contains(&y)
            && pixel(screen, x, y).is_some_and(is_install_blue)
    };
    let mut visited = std::collections::HashSet::new();
    for y in rows.clone() {
        for x in columns.clone() {
            if !blue(x, y) || !visited.insert((x, y)) {
                continue;
            }
            let (count, low, high) = patch(&blue, &mut visited, (x, y));
            let button = Listed {
                at: low,
                size: (high.0 - low.0 + 1, high.1 - low.1 + 1),
                title: String::new(),
            };
            if is_install_shape(count, &button) && cancel_beside(screen, &button) {
                return Some(button);
            }
        }
    }
    None
}

/// Whether a patch of `count` pixels within `bounds` has the Install
/// button's shape.
fn is_install_shape(count: usize, bounds: &Listed) -> bool {
    let (wide, tall) = bounds.size;
    let area = usize::try_from(wide * tall).unwrap_or(usize::MAX);
    INSTALL_WIDE.contains(&wide)
        && INSTALL_TALL.contains(&tall)
        && super::share(count, area) >= INSTALL_FILLED
}

/// Whether most of a 9x9 patch a little right of `button`'s right edge,
/// level with its middle, is [`is_cancel_grey`].
fn cancel_beside(screen: &Image, button: &Listed) -> bool {
    let x = button.at.0 + button.size.0 + CANCEL_AFTER;
    let y = button.at.1 + button.size.1 / 2;
    let grey = (-4..=4)
        .flat_map(|dy| (-4..=4).map(move |dx| (x + dx, y + dy)))
        .filter(|(x, y)| pixel(screen, *x, *y).is_some_and(is_cancel_grey))
        .count();
    grey * 10 >= 81 * 6
}

/// Whether the checkbox centred at `at` on `screen` is ticked: its tick is
/// the Install button's blue, and an empty box has none.
fn ticked(screen: &Image, at: (i32, i32)) -> bool {
    let blue = (-SHORTCUT_HALF..=SHORTCUT_HALF)
        .flat_map(|dy| (-SHORTCUT_HALF..=SHORTCUT_HALF).map(move |dx| (at.0 + dx, at.1 + dy)))
        .filter(|(x, y)| pixel(screen, *x, *y).is_some_and(is_install_blue))
        .count();
    blue >= TICKED
}

/// The patch of `inside` pixels that `start` is in, `start` already in
/// `visited`: how many pixels, and its least and greatest corners.
fn patch(
    inside: &impl Fn(i32, i32) -> bool,
    visited: &mut std::collections::HashSet<(i32, i32)>,
    start: (i32, i32),
) -> (usize, (i32, i32), (i32, i32)) {
    let mut stack = vec![start];
    let mut count = 0;
    let (mut low, mut high) = (start, start);
    while let Some((x, y)) = stack.pop() {
        count += 1;
        low = (low.0.min(x), low.1.min(y));
        high = (high.0.max(x), high.1.max(y));
        for next in [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)] {
            if inside(next.0, next.1) && visited.insert(next) {
                stack.push(next);
            }
        }
    }
    (count, low, high)
}

/// The part of `screen` that `window` covers.
fn cropped(screen: &Image, window: &Listed) -> Image {
    let (columns, rows) = covered(screen, window);
    let width = usize::try_from(columns.end - columns.start).unwrap_or(0);
    let height = usize::try_from(rows.end - rows.start).unwrap_or(0);
    let mut pixels = Vec::with_capacity(width * height * 3);
    for y in rows {
        for x in columns.clone() {
            pixels.extend(pixel(screen, x, y).unwrap_or_default());
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

/// How many colours `image` has.
fn colours_in(image: &Image) -> usize {
    let (pixels, _) = image.pixels.as_chunks::<3>();
    pixels
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window is cut from the screen where it is, and what lies off the
    /// screen is left out.
    #[test]
    fn a_window_is_cropped_from_the_screen() {
        let mut screen = Image {
            width: 4,
            height: 3,
            pixels: Vec::new(),
        };
        for at in 0..12u8 {
            screen.pixels.extend([at, 0, 0]);
        }
        let window = Listed {
            at: (1, 1),
            size: (2, 2),
            title: TITLE.to_owned(),
        };
        let part = cropped(&screen, &window);
        assert_eq!((part.width, part.height), (2, 2));
        assert_eq!(part.pixels, vec![5, 0, 0, 6, 0, 0, 9, 0, 0, 10, 0, 0]);
        let off = Listed {
            at: (3, 2),
            size: (5, 5),
            ..window
        };
        let part = cropped(&screen, &off);
        assert_eq!((part.width, part.height), (1, 1));
        assert_eq!(colours_in(&part), 1);
        assert_eq!(colours_in(&screen), 12);
    }

    /// Paint `xs` by `ys` of `screen` `colour`.
    fn paint(
        screen: &mut Image,
        xs: std::ops::Range<usize>,
        ys: std::ops::Range<usize>,
        colour: [u8; 3],
    ) {
        for y in ys {
            for x in xs.clone() {
                let at = (y * screen.width + x) * 3;
                screen.pixels[at..at + 3].copy_from_slice(&colour);
            }
        }
    }

    /// The Install dialog as it was dumped on 2026-10-01, in its colours:
    /// its button is found beside Cancel; the wider drive bar of the same
    /// blue is not, nor a button-shaped blue with no Cancel beside it.
    #[test]
    fn the_install_button_is_the_blue_one_beside_cancel() {
        let (width, height) = (900usize, 400usize);
        let mut screen = Image {
            width,
            height,
            pixels: [40, 44, 50].repeat(width * height),
        };
        paint(&mut screen, 20..520, 100..146, [26, 159, 255]);
        paint(&mut screen, 110..310, 250..282, [49, 132, 226]);
        // The label, cut out of the button.
        paint(&mut screen, 180..240, 260..272, [255, 255, 255]);
        paint(&mut screen, 324..524, 250..282, [61, 68, 80]);
        let window = Listed {
            at: (0, 0),
            size: (900, 400),
            title: "Steam".to_owned(),
        };
        let button = install_button(&screen, &window).expect("the Install button");
        assert_eq!((button.at, button.size), ((110, 250), (200, 32)));
        // The shortcut box, empty, then with a tick of 6x8 in the blue.
        let tick = (110 + 123, 250 - 245);
        assert!(!ticked(&screen, tick));
        paint(&mut screen, 230..236, 2..10, [26, 159, 255]);
        assert!(ticked(&screen, tick));
        paint(&mut screen, 324..524, 250..282, [40, 44, 50]);
        assert_eq!(install_button(&screen, &window), None);
        for (colour, blue) in [
            ([57, 153, 236], true),
            ([35, 94, 207], true),
            ([26, 159, 255], true),
            ([61, 68, 80], false),
            ([0x75, 0xb0, 0x22], false),
        ] {
            assert_eq!(is_install_blue(colour), blue, "{colour:?}");
        }
        assert!(is_cancel_grey([61, 68, 80]));
        assert!(!is_cancel_grey([40, 44, 50]));
    }
}
