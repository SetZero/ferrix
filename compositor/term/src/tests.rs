//! What the grid and the keys do, which is every rule that does not need a
//! window.

use crate::grid::Grid;
use crate::keys;
use crate::paint;

fn lines(grid: &Grid) -> Vec<String> {
    (0..grid.size().1).map(|row| grid.line(row)).collect()
}

#[test]
fn text_lands_where_the_cursor_is_and_moves_it_on() {
    let mut grid = Grid::new(10, 3);
    grid.write(b"hello");
    assert_eq!(grid.line(0), "hello");
    assert_eq!(grid.cursor(), (5, 0));

    // A carriage return is the start of the line; a newline is the next row.
    grid.write(b"\r\nworld");
    assert_eq!(lines(&grid), ["hello", "world", ""]);
    assert_eq!(grid.cursor(), (5, 1));

    // Backspace moves back; what is typed then overwrites.
    grid.write(b"\x08!");
    assert_eq!(grid.line(1), "worl!");

    // A tab goes to the next multiple of eight.
    grid.write(b"\r\t");
    assert_eq!(grid.cursor().0, 8);
}

#[test]
fn a_line_that_runs_off_the_end_wraps_and_the_screen_scrolls() {
    let mut grid = Grid::new(4, 2);
    grid.write(b"abcdef");
    assert_eq!(lines(&grid), ["abcd", "ef"]);
    assert_eq!(grid.scrolled(), 0);

    // The third line has nowhere to go, so everything moves up one.
    grid.write(b"\r\nghij");
    assert_eq!(lines(&grid), ["ef", "ghij"]);
    assert_eq!(grid.scrolled(), 1);
}

#[test]
fn the_cursor_sequences_move_it_and_stay_on_the_grid() {
    let mut grid = Grid::new(10, 4);
    // Row and column count from one.
    grid.write(b"\x1b[2;3H");
    assert_eq!(grid.cursor(), (2, 1));
    grid.write(b"\x1b[A");
    assert_eq!(grid.cursor(), (2, 0));
    grid.write(b"\x1b[2B");
    assert_eq!(grid.cursor(), (2, 2));
    grid.write(b"\x1b[4C");
    assert_eq!(grid.cursor(), (6, 2));
    grid.write(b"\x1b[2D");
    assert_eq!(grid.cursor(), (4, 2));

    // Past the edge is the edge, never off it.
    grid.write(b"\x1b[99C");
    assert_eq!(grid.cursor(), (9, 2));
    grid.write(b"\x1b[99B");
    assert_eq!(grid.cursor(), (9, 3));
    grid.write(b"\x1b[99;99H");
    assert_eq!(grid.cursor(), (9, 3));
}

#[test]
fn erasing_clears_what_it_says_and_no_more() {
    let mut grid = Grid::new(6, 3);
    grid.write(b"aaaaaa\r\nbbbbbb\r\ncccccc");

    // To the end of the line, from the middle of the second row.
    grid.write(b"\x1b[2;4H\x1b[K");
    assert_eq!(lines(&grid), ["aaaaaa", "bbb", "cccccc"]);

    // To the start of the line.
    grid.write(b"\x1b[3;3H\x1b[1K");
    assert_eq!(lines(&grid), ["aaaaaa", "bbb", "   ccc"]);

    // The whole screen, which also takes the cursor home.
    grid.write(b"\x1b[2J");
    assert_eq!(lines(&grid), ["", "", ""]);
    assert_eq!(grid.cursor(), (0, 0));
}

#[test]
fn a_colour_sequence_paints_the_cells_that_follow_it() {
    let mut grid = Grid::new(8, 1);
    grid.write(b"\x1b[31mred\x1b[0m.");
    let cell = |grid: &Grid, column: usize| grid.cell(column, 0).copied().expect("a cell");
    assert_eq!(cell(&grid, 0).colour, 1);
    assert!(!cell(&grid, 0).bold);
    assert_eq!(cell(&grid, 3).colour, 7, "the reset put the pen back");

    // Bold, and the bright form of a colour, which is the same eight.
    grid.write(b"\r\x1b[1;32mg");
    let painted = cell(&grid, 0);
    assert_eq!((painted.colour, painted.bold), (2, true));
    grid.write(b"\r\x1b[94mb");
    let painted = cell(&grid, 0);
    assert_eq!((painted.colour, painted.bold), (4, true));
}

/// A sequence the terminal does not know is dropped, not drawn: a terminal
/// that printed the bytes of one would fill its screen with rubbish the
/// first time a program asked it a question.
#[test]
fn an_unknown_sequence_leaves_nothing_behind() {
    let mut grid = Grid::new(20, 1);
    grid.write(b"\x1b[6nok");
    assert_eq!(grid.line(0), "ok");
    grid.write(b"\r\x1b]0;a title\x07done");
    // `ESC ]` is not understood either, and what follows it is text: what
    // matters is that the escape itself left nothing.
    assert!(!grid.line(0).contains('\x1b'));
}

#[test]
fn the_cursor_can_be_hidden_and_shown() {
    let mut grid = Grid::new(4, 1);
    assert!(grid.cursor_visible());
    grid.write(b"\x1b[?25l");
    assert!(!grid.cursor_visible());
    grid.write(b"\x1b[?25h");
    assert!(grid.cursor_visible());
}

#[test]
fn resizing_keeps_what_is_still_on_the_grid() {
    let mut grid = Grid::new(6, 2);
    grid.write(b"abcdef\r\nghijkl");
    grid.resize(3, 2);
    assert_eq!(lines(&grid), ["abc", "ghi"]);
    assert_eq!(grid.cursor(), (2, 1), "the cursor stays on the grid");

    grid.resize(6, 3);
    assert_eq!(lines(&grid), ["abc", "ghi", ""]);
}

#[test]
fn a_window_holds_as_many_cells_as_the_font_fits_in_it() {
    // Spleen is 8x16, so a 1024x768 window is 128 by 48.
    assert_eq!(paint::fits(1024, 768, 1), (128, 48));
    // At scale two a cell is twice the size, so half as many fit.
    assert_eq!(paint::fits(1024, 768, 2), (64, 24));
    // A window too small for one cell still has one: a grid with no cells
    // has nowhere to put a character.
    assert_eq!(paint::fits(3, 3, 1), (1, 1));
}

#[test]
fn the_keys_a_terminal_sends_are_the_ones_xterm_sends() {
    let sends = |keysym: &str| keys::bytes(keysym, false);
    // A letter is itself, and the keymap has already done the shifting: the
    // keysym for a shifted `q` is `Q`.
    assert_eq!(sends("q"), Some(b"q".to_vec()));
    assert_eq!(sends("Q"), Some(b"Q".to_vec()));
    assert_eq!(sends("Return"), Some(b"\r".to_vec()));
    assert_eq!(sends("BackSpace"), Some(b"\x7F".to_vec()));
    assert_eq!(sends("Up"), Some(b"\x1b[A".to_vec()));
    assert_eq!(sends("Right"), Some(b"\x1b[C".to_vec()));
    assert_eq!(sends("space"), Some(b" ".to_vec()));
    // A modifier sends nothing at all.
    assert_eq!(sends("Shift_L"), None);

    // Control and a letter is the letter's low five bits: control-C is the
    // interrupt character the line discipline looks for.
    assert_eq!(keys::bytes("c", true), Some(vec![3]));
    assert_eq!(keys::bytes("C", true), Some(vec![3]));
    assert_eq!(keys::bytes("d", true), Some(vec![4]));
    assert_eq!(keys::control_byte('['), Some(0x1B));
    assert_eq!(keys::control_byte('?'), Some(0x7F));
    assert_eq!(keys::control_byte('1'), None);
}

// ---------------------------------------------------------------------------
// The picture
//
// What a terminal draws is a grid of glyphs, and the expected image is
// blessed from the same code the window draws with. `cargo xtask
// test-compositor` compares a screendump of the terminal on Ferrix against
// it.
// ---------------------------------------------------------------------------

/// The window the expected image is of.
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;

/// The layout of a terminal that is the only window on a 1024x768 monitor,
/// which is what `cargo xtask test-compositor` boots.
///
/// Asked of `compositor/layout` rather than worked out here: the gaps and
/// the border are the configuration's, and a test that assumed them would
/// be a test that broke when a default changed.
fn one_window() -> compositor_layout::MonitorLayout {
    use compositor_layout::{Monitor, MonitorId, Rect, Settings, State, WindowId};

    let mut state = State::new(Settings::default());
    let _ = state
        .add_monitor(Monitor {
            id: MonitorId(1),
            name: "Virtual-1".to_owned(),
            rect: Rect::new(0, 0, i64::from(WIDTH), i64::from(HEIGHT)),
            reserved: compositor_layout::Gaps::all(0),
            scale: 1.0,
            description: String::new(),
            made: <(String, String, String)>::default(),
        })
        .expect("a monitor");
    let _ = state.open_window(WindowId(1)).expect("a window");
    state.layout().remove(0)
}

/// The size that window's client area is.
fn window_size() -> (usize, usize) {
    let layout = one_window();
    let placed = layout.windows.first().expect("a window");
    (
        usize::try_from(placed.rect.width).unwrap_or(0),
        usize::try_from(placed.rect.height).unwrap_or(0),
    )
}

/// The grid a terminal shows after `hyprctl version` has run in it.
fn version_grid(columns: usize, rows: usize) -> Grid {
    let mut grid = Grid::new(columns, rows);
    // What `hyprctl version` prints, as the terminal sees it: the slave's
    // `ONLCR` turns every newline into a carriage return and a newline.
    grid.write(b"hyprix 0.1.0\r\n\r\nno flags were set\r\n");
    grid
}

/// The grid a terminal shows after `pattern --scroll 80` has run in it:
/// eighty numbered lines through a grid of fewer rows, so it scrolled.
fn scrolled_grid(columns: usize, rows: usize) -> Grid {
    let mut grid = Grid::new(columns, rows);
    for line in 0..80 {
        grid.write(format!("scrolling line {line}\r\n").as_bytes());
    }
    grid
}

/// The rows a terminal paints again are the rows that changed, and painting
/// only those over the buffer it holds is the picture painting all of them
/// makes -- to the byte, over a scroll, which changes every row, and over
/// one more line, which changes two.
#[test]
fn painting_the_changed_rows_is_painting_the_whole() {
    let (width, height) = (400usize, 200usize);
    let (columns, rows) = paint::fits(width, height, 1);
    let colours = paint::Colours::default();
    let mut grid = Grid::new(columns, rows);
    grid.write(b"one\r\ntwo\r\n");
    let mut kept = vec![0u8; width * height * 4];
    paint::draw(&mut kept, (width, height), width * 4, &grid, &colours, 1);
    let mut painted: Vec<paint::Painted> = (0..rows)
        .map(|row| paint::Painted::of(&grid, row))
        .collect();

    // One more line: two rows change, the one written and the one the
    // cursor moved to. Then more lines than there are rows: every row.
    for (bytes, rows_changed) in [
        (b"three\r\n".to_vec(), 1..=2),
        (b"x\r\n".repeat(rows + 3), rows..=rows),
    ] {
        grid.write(&bytes);
        let now: Vec<paint::Painted> = (0..rows)
            .map(|row| paint::Painted::of(&grid, row))
            .collect();
        let changed: Vec<usize> = (0..rows).filter(|&row| painted[row] != now[row]).collect();
        assert!(
            rows_changed.contains(&changed.len()),
            "{} rows changed",
            changed.len()
        );
        for &row in &changed {
            paint::draw_rows(
                &mut kept,
                (width, height),
                width * 4,
                &grid,
                &colours,
                1,
                row..row + 1,
            );
        }
        painted = now;
        let mut whole = vec![0u8; width * height * 4];
        paint::draw(&mut whole, (width, height), width * 4, &grid, &colours, 1);
        assert!(
            kept == whole,
            "the rows painted apart differ from the whole"
        );
    }
}

/// A terminal's picture, drawn the way its window draws it.
fn version_frame(width: usize, height: usize) -> Vec<u8> {
    let (columns, rows) = paint::fits(width, height, 1);
    let mut pixels = vec![0u8; width * height * 4];
    paint::draw(
        &mut pixels,
        (width, height),
        width * 4,
        &version_grid(columns, rows),
        &paint::Colours::default(),
        1,
    );
    pixels
}

/// The screen a terminal running `hyprctl version` makes: the compositor's
/// frame, with the terminal's own buffer as the window's surface.
///
/// This is what `cargo xtask test-compositor` requires from a screendump of
/// the guest, so it is drawn the way the compositor draws one: the window
/// where `compositor/layout` puts it, the border and the background
/// `compositor/render` gives it, and the terminal's pixels inside.
#[test]
fn a_terminal_running_a_program_is_the_expected_image() {
    let (width, height) = window_size();
    screen_of(&version_frame(width, height), "terminal-hyprctl-version");
}

/// The same, after `pattern --scroll 80` ran in it: what `cargo xtask
/// test-compositor`'s scroll boot requires of the guest, where every one of
/// those lines was painted and damaged a row at a time.
#[test]
fn a_terminal_that_scrolled_is_the_expected_image() {
    let (width, height) = window_size();
    let (columns, rows) = paint::fits(width, height, 1);
    let mut pixels = vec![0u8; width * height * 4];
    paint::draw(
        &mut pixels,
        (width, height),
        width * 4,
        &scrolled_grid(columns, rows),
        &paint::Colours::default(),
        1,
    );
    screen_of(&pixels, "terminal-scrolled");
}

/// The screen a terminal showing `buffer` makes, compared with the image
/// blessed as `name`.
fn screen_of(buffer: &[u8], name: &str) {
    use std::collections::BTreeMap;

    use compositor_render::{Canvas, Damage, Format, Style, Surface, Target};

    let layout = one_window();
    let (width, height) = window_size();
    let surface = Surface::new(
        buffer,
        u32::try_from(width).unwrap_or(0),
        u32::try_from(height).unwrap_or(0),
        u32::try_from(width * 4).unwrap_or(0),
        // A terminal has no transparency of its own.
        Format::Xrgb8888,
    )
    .expect("the terminal's buffer");
    let mut surfaces = BTreeMap::new();
    let window = layout.windows.first().expect("a window").window;
    let _ = surfaces.insert(window, surface);

    let mut canvas = Canvas::new(WIDTH, HEIGHT).expect("a canvas");
    let full = Damage::full(WIDTH, HEIGHT);
    let produced = compositor_render::render(
        &mut canvas,
        &layout,
        (0, 0),
        &Style::default(),
        &surfaces,
        &full,
    );
    assert_eq!(produced, full);
    let mut pixels = vec![0; WIDTH as usize * HEIGHT as usize * 4];
    let mut target = Target::new(&mut pixels, WIDTH, HEIGHT, WIDTH * 4).expect("a target");
    canvas.present(&mut target, &full).expect("the frame fits");

    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("render")
        .join("tests")
        .join("data");
    compositor_render::golden::check_in(&directory, name, WIDTH, HEIGHT, &pixels);
}

/// The glyphs land where the grid says, and the cursor is drawn as a block.
#[test]
fn the_grid_is_drawn_cell_by_cell() {
    let (width, height) = (WIDTH as usize, HEIGHT as usize);
    let frame = version_frame(width, height);
    let at = |x: usize, y: usize| -> u32 {
        let start = (y * width + x) * 4;
        u32::from_le_bytes([
            frame[start],
            frame[start + 1],
            frame[start + 2],
            frame[start + 3],
        ])
    };
    let background = paint::Colours::default().background;
    let as_pixel = |colour: ferrix_fbtext::Rgb| {
        u32::from(colour.b) | (u32::from(colour.g) << 8) | (u32::from(colour.r) << 16)
    };
    // The corner of the first cell is background: no glyph's top-left pixel
    // is set in this font.
    assert_eq!(at(0, 0), as_pixel(background));
    // Somewhere in the first row of glyphs there is text, which is not the
    // background.
    let row = (0..width).map(|x| at(x, 6)).collect::<Vec<_>>();
    assert!(
        row.iter().any(|pixel| *pixel != as_pixel(background)),
        "the first line drew nothing"
    );
    // The cursor is a filled block on the line after the last one written:
    // `hyprctl version` prints three lines, so it is on the fourth.
    let cursor = paint::Colours::default().cursor;
    let (cx, cy) = (2, 3 * paint::CELL.1 + 8);
    assert_eq!(at(cx, cy), as_pixel(cursor), "the cursor is not a block");
}
