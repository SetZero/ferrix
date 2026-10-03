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

    // Bold is a weight, and the bright form of a colour is a colour of its
    // own, eight past it, drawn in whatever weight the pen has.
    grid.write(b"\r\x1b[1;32mg");
    let painted = cell(&grid, 0);
    assert_eq!((painted.colour, painted.bold), (2, true));
    grid.write(b"\r\x1b[22;94mb");
    let painted = cell(&grid, 0);
    assert_eq!((painted.colour, painted.bold), (12, false));
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
    assert_eq!(grid.line(0), "done");
    grid.write(b"\r\x1b]0;a title\x1b\\again");
    assert_eq!(grid.line(0), "again");
    // A character set picked: `ESC ( B` used to leave its `B`.
    grid.write(b"\r\x1b(Bthird");
    assert_eq!(grid.line(0), "third");
}

/// An `OSC 8` hyperlink is its text alone: Claude Code's sign-in screen
/// sends its address as one, which was drawn as `8;id=…;https://…` twice
/// over and the address again.
#[test]
fn a_hyperlink_is_its_text() {
    let mut grid = Grid::new(30, 2);
    grid.write(b"\x1b]8;id=iofw5e;https://claude.com/a\x1b\\https://claude.com/a\x1b]8;;\x1b\\");
    assert_eq!(grid.line(0), "https://claude.com/a");
    // A string cut short by another sequence ends there, and that sequence
    // is still read.
    grid.write(b"\r\n\x1b]8;;x\x1b[2Ggo");
    assert_eq!(grid.line(1), " go");
}

/// `CSI G` and `CSI d` put the cursor in a column or a row alone; Claude
/// Code's TUI places every word with `G`, and without it its words ran
/// together, as `WelcometoClaudeCode`.
#[test]
fn a_column_or_row_alone_places_the_cursor() {
    let mut grid = Grid::new(20, 3);
    grid.write(b"\x1b[HClaude\x1b[8GCode\x1b[14Gv2");
    assert_eq!(grid.line(0), "Claude Code  v2");
    grid.write(b"\x1b[3d\x1b[Gend\x1b[99G!");
    assert_eq!(grid.line(2), "end                !");
    grid.write(b"\x1b[1;3H\x1b[3X");
    assert_eq!(grid.line(0), "Cl   e Code  v2");
    grid.write(b"\x1b[1;5H\x1b[Ex\x1b[Fy");
    assert_eq!(
        (grid.line(0).chars().next(), grid.line(1).chars().next()),
        (Some('y'), Some('x'))
    );
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

/// A sequence with an intermediate byte, or one another terminal's prefix
/// marks as its own, ends at its final byte and leaves nothing: `CSI 2 SP q`
/// sets the cursor's shape, which zsh's vi mode sends, and used to leave its
/// `q` on the screen.
#[test]
fn a_sequence_is_dropped_whole_whatever_its_bytes() {
    let mut grid = Grid::new(20, 1);
    grid.write(b"\x1b[2 qa\x1b[>0cb\x1b[>4;1mc\x1b[=5ud");
    assert_eq!(grid.line(0), "abcd");
}

/// `CSI s` and `ESC 7` save the cursor, and `CSI u` and `ESC 8` put it back:
/// btop's menus centre each line by moving right from a saved cursor and
/// returning to it.
#[test]
fn a_saved_cursor_comes_back() {
    let mut grid = Grid::new(10, 3);
    grid.write(b"\x1b[2;3f\x1b[s\x1b[3;9fx\x1b[uy");
    assert_eq!(lines(&grid), ["", "  y", "        x"]);
    grid.write(b"\x1b[1;2f\x1b[31m\x1b7\x1b[0m\x1b[3;1f\x1b8z");
    assert_eq!(grid.line(0), " z");
    assert_eq!(
        grid.cell(1, 0).map(|cell| cell.colour),
        Some(1),
        "and the pen"
    );
}

/// btop asks for the alternate screen and then never clears it: it draws its
/// boxes and moves about in them. The alternate screen must start blank, or
/// the shell's text shows through every cell btop leaves alone -- and the
/// shell's screen must come back as it was when btop ends.
#[test]
fn the_alternate_screen_starts_blank_and_gives_the_screen_back() {
    let mut grid = Grid::new(10, 3);
    grid.write(b"$ ps\r\nchrome\r\n$ btop");
    let cursor = grid.cursor();
    grid.write(b"\x1b[?1049h");
    assert!(grid.alternate());
    assert_eq!(lines(&grid), ["", "", ""], "nothing of the shell's shows");
    grid.write(b"\x1b[2;2f\xe2\x94\x80cpu");
    assert_eq!(lines(&grid), ["", " \u{2500}cpu", ""]);

    grid.write(b"\x1b[?1049l");
    assert!(!grid.alternate());
    assert_eq!(lines(&grid), ["$ ps", "chrome", "$ btop"]);
    assert_eq!(grid.cursor(), cursor, "1049 puts the cursor back");

    // 1047 is the screens without the cursor.
    grid.write(b"\x1b[?1047h\x1b[1;1f\x1b[?1047l");
    assert_eq!(grid.cursor(), (0, 0));
    assert_eq!(lines(&grid), ["$ ps", "chrome", "$ btop"]);
}

/// What scrolls off the alternate screen is gone, rather than landing in the
/// ordinary screen's scrollback, and there is no looking back while it shows.
#[test]
fn the_alternate_screen_keeps_no_scrollback() {
    let mut grid = Grid::new(4, 2);
    grid.write(b"one\r\ntwo\r\nsix");
    assert_eq!(grid.history(), 1);
    grid.write(b"\x1b[?1049ha\r\nb\r\nc\r\nd");
    assert_eq!(lines(&grid), ["c", "d"]);
    assert_eq!(grid.history(), 1);
    grid.scroll_view(1);
    assert_eq!(grid.view(), 0);
    grid.write(b"\x1b[?1049l");
    assert_eq!(lines(&grid), ["two", "six"]);
    grid.scroll_view(1);
    assert_eq!(grid.shown(0, 0).map(|(cell, _)| cell.ch), Some('o'));
}

/// A window resized while a program has the alternate screen gives back an
/// ordinary screen that fits it.
#[test]
fn a_resize_under_the_alternate_screen_fits_the_ordinary_one_too() {
    let mut grid = Grid::new(6, 3);
    grid.write(b"abcdef\r\nghijkl\r\nmn");
    grid.write(b"\x1b[?1049h\x1b[1;1fxyz");
    grid.resize(3, 2);
    assert_eq!(lines(&grid), ["xyz", ""]);
    grid.write(b"\x1b[?1049l");
    assert_eq!(grid.size(), (3, 2));
    // The cursor's row stays in view, so the top row went to the scrollback.
    assert_eq!(lines(&grid), ["ghi", "mn"]);
    assert_eq!(grid.history(), 1);
    assert_eq!(grid.cursor(), (2, 1));
}

/// A normal prompt update damages the changed character and the two cursor
/// positions, rather than the entire terminal window.
#[test]
fn a_small_write_has_small_cell_damage() {
    let before = Grid::new(80, 24);
    let mut after = before.clone();
    after.write(b"x");
    assert_eq!(
        after.damage_since(&before.snapshot()),
        Some(crate::grid::CellDamage {
            left: 0,
            top: 0,
            width: 2,
            height: 1,
        })
    );
}

/// The common idle pass leaves the grid and its cursor alone, so it owes no
/// Wayland buffer commit at all.
#[test]
fn an_unchanged_grid_has_no_damage() {
    let grid = Grid::new(80, 24);
    assert_eq!(grid.damage_since(&grid.snapshot()), None);
}

/// Incremental rasterisation produces the same bytes as painting the updated
/// terminal from scratch. This holds the client-side fast path to its full
/// redraw reference picture.
#[test]
fn cell_damage_paints_the_same_picture_as_a_full_redraw() {
    let mut before = Grid::new(8, 2);
    before.write(b"hello");
    let mut after = before.clone();
    after.write(b"!");
    let damage = after
        .damage_since(&before.snapshot())
        .expect("a changed grid");
    let colours = paint::Colours::default();
    let (width, height) = (8 * paint::CELL.0, 2 * paint::CELL.1);
    let mut incremental = vec![0u8; width * height * 4];
    paint::draw(
        &mut incremental,
        (width, height),
        width * 4,
        &before,
        &colours,
        1,
    );
    paint::draw_damage(
        &mut incremental,
        (width, height),
        width * 4,
        &after,
        &colours,
        1,
        damage,
    );
    let mut full = vec![0u8; width * height * 4];
    paint::draw(&mut full, (width, height), width * 4, &after, &colours, 1);
    assert_eq!(incremental, full);
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

/// Braille is drawn rather than looked up, one bit a dot: btop's graphs are
/// nothing else, and Hack has none of it, so each was the hollow box.
#[test]
fn braille_is_eight_dots_by_the_bits_of_its_code_point() {
    let dots = |ch: char| {
        let cell = paint::braille(ch).expect("a braille pattern");
        // Which of the eight slots has ink at its centre, left column then
        // right, top to bottom.
        let (slot_width, slot_height) = (paint::CELL.0 / 2, paint::CELL.1 / 4);
        let mut on = Vec::new();
        for column in 0..2 {
            for row in 0..4 {
                let (x, y) = (
                    column * slot_width + slot_width / 2,
                    row * slot_height + slot_height / 2,
                );
                if cell
                    .get(y * paint::CELL.0 + x)
                    .is_some_and(|&value| value == 0xFF)
                {
                    on.push((column, row));
                }
            }
        }
        (on, cell.iter().filter(|&&value| value != 0).count())
    };
    assert_eq!(dots('\u{2800}'), (vec![], 0), "the blank pattern is blank");
    // Dot 1 is the top left, dot 4 the top right, dot 7 and dot 8 the bottom
    // row, which Unicode numbered last.
    assert_eq!(dots('\u{2801}').0, [(0, 0)]);
    assert_eq!(dots('\u{2808}').0, [(1, 0)]);
    assert_eq!(dots('\u{2840}').0, [(0, 3)]);
    assert_eq!(dots('\u{2880}').0, [(1, 3)]);
    // btop's lowest bar, both bottom dots, and a full cell.
    assert_eq!(dots('\u{28C0}').0, [(0, 3), (1, 3)]);
    let (all, ink) = dots('\u{28FF}');
    assert_eq!(all.len(), 8);
    // Each dot is the same square, and none of them touch.
    assert_eq!(ink, 8 * dots('\u{2801}').1);
    assert!(paint::braille('\u{2900}').is_none() && paint::braille('a').is_none());
}

/// A btop graph, written the way btop writes one, is painted as the dots it
/// asks for and scrolls as it asks: the pixels, not only the cells.
///
/// btop's `Draw::Graph` is a row of braille a line, each line reached by
/// `CSI 1 B` and `CSI width D` from the end of the one above, in an exact
/// colour per line. On every update it drops each line's first character,
/// adds a new last one, and writes the lines again from the same place.
/// The screenshots of 2026-09-27 showed a flat line and a spike where a
/// filled history was expected; this holds the terminal to drawing exactly
/// what the program sent, which with the same bytes replayed from btop on a
/// busy host is a filled history. The flat line was an idle guest.
#[test]
fn a_btop_graph_paints_its_dots_and_scrolls() {
    // Two lines of five cells: a rising ramp, bottom line filled first. Each
    // cell holds two samples, left and right, of zero to eight dots high.
    let samples: [u32; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 3, 0];
    // The dots of one column, counted up from the bottom: dots 7, 3, 2, 1 on
    // the left and 8, 6, 5, 4 on the right.
    let column = |height: u32, right: bool| -> u32 {
        let bits: [u32; 4] = if right {
            [0x80, 0x20, 0x10, 0x08]
        } else {
            [0x40, 0x04, 0x02, 0x01]
        };
        bits.iter()
            .take(usize::try_from(height.min(4)).unwrap_or(4))
            .sum()
    };
    // Line 0 is the top: it holds whatever is above four dots.
    let part = |line: u32, height: u32| {
        if line == 0 {
            height.saturating_sub(4)
        } else {
            height.min(4)
        }
    };
    let graph = |from: usize| -> Vec<u8> {
        let mut bytes = b"\x1b[2;3f".to_vec();
        for line in 0..2u32 {
            if line > 0 {
                bytes.extend_from_slice(b"\x1b[1B\x1b[5D");
            }
            bytes.extend_from_slice(if line == 0 {
                b"\x1b[38;2;220;80;60m"
            } else {
                b"\x1b[38;2;80;200;90m"
            });
            for cell in 0..5 {
                let (left, right) = (samples[from + cell * 2], samples[from + cell * 2 + 1]);
                let bits = column(part(line, left), false) | column(part(line, right), true);
                let ch = char::from_u32(0x2800 + bits).expect("a braille pattern");
                bytes.extend_from_slice(ch.to_string().as_bytes());
            }
        }
        bytes
    };
    let colours = paint::Colours::default();
    let (columns, rows) = (10, 4);
    let (width, height) = (columns * paint::CELL.0, rows * paint::CELL.1);
    let mut grid = Grid::new(columns, rows);
    let mut pixels = vec![0u8; width * height * 4];
    paint::draw(&mut pixels, (width, height), width * 4, &grid, &colours, 1);

    // Whether the middle of a dot's slot is inked, in `pixels`.
    let (slot_width, slot_height) = (paint::CELL.0 / 2, paint::CELL.1 / 4);
    let inked = |pixels: &[u8], x: usize, y: usize| {
        let at = (y * width + x) * 4;
        let bg = colours.background;
        pixels.get(at..at + 3) != Some(&[bg.b, bg.g, bg.r][..])
    };
    let check = |pixels: &[u8], from: usize| {
        for cell in 0..5 {
            for (half, sample) in [samples[from + cell * 2], samples[from + cell * 2 + 1]]
                .into_iter()
                .enumerate()
            {
                // Eight dot rows, top to bottom, over the graph's two lines.
                for dot in 0..8u32 {
                    let (line, slot) = (dot / 4, dot % 4);
                    let x = (2 + cell) * paint::CELL.0 + half * slot_width + slot_width / 2;
                    let y = (1 + usize::try_from(line).unwrap_or(0)) * paint::CELL.1
                        + usize::try_from(slot).unwrap_or(0) * slot_height
                        + slot_height / 2;
                    let expected = 8 - dot <= sample;
                    assert_eq!(
                        inked(pixels, x, y),
                        expected,
                        "sample {sample} at cell {cell}, half {half}, dot row {dot}, from {from}"
                    );
                }
            }
        }
    };

    for from in 0..=2 {
        let before = grid.snapshot();
        grid.write(&graph(from));
        if let Some(damage) = grid.damage_since(&before) {
            paint::draw_damage(
                &mut pixels,
                (width, height),
                width * 4,
                &grid,
                &colours,
                1,
                damage,
            );
        }
        check(&pixels, from);
        // What the window shows after the update is what a full repaint of
        // the grid would show: nothing of the previous frame is left over.
        let mut full = vec![0u8; width * height * 4];
        paint::draw(&mut full, (width, height), width * 4, &grid, &colours, 1);
        assert!(pixels == full, "the update from {from} left stale pixels");
    }
    // A full sample's column is a bar: the top line's dots are in its own
    // colour, the lower line's in the other.
    let at = |x: usize, y: usize| {
        let i = (y * width + x) * 4;
        (pixels[i + 2], pixels[i + 1], pixels[i])
    };
    let x = (2 + 3) * paint::CELL.0 + slot_width / 2;
    assert_eq!(at(x, paint::CELL.1 + slot_height / 2), (220, 80, 60));
    assert_eq!(at(x, 2 * paint::CELL.1 + slot_height / 2), (80, 200, 90));
}

/// The block elements btop's `block` graphs and meters use fill the cell
/// from its bottom, an eighth at a time, the whole width, and the full block
/// is the whole cell: rows of them stack without a gap.
#[test]
fn block_elements_fill_the_cell_from_the_bottom() {
    let colours = paint::Colours::default();
    let (width, height) = (8 * paint::CELL.0, paint::CELL.1);
    let mut grid = Grid::new(8, 1);
    grid.write("▁▂▃▄▅▆▇█".as_bytes());
    let mut pixels = vec![0u8; width * height * 4];
    paint::draw(&mut pixels, (width, height), width * 4, &grid, &colours, 1);
    let bg = colours.background;
    let ink = |x: usize, y: usize| {
        let at = (y * width + x) * 4;
        pixels.get(at..at + 3) != Some(&[bg.b, bg.g, bg.r][..])
    };
    let mut last = 0;
    for eighths in 1..=8 {
        let left = (eighths - 1) * paint::CELL.0;
        // Rows with any ink, and each of them inked across the whole cell.
        let tall = (0..paint::CELL.1)
            .filter(|&y| (0..paint::CELL.0).any(|x| ink(left + x, y)))
            .count();
        for y in paint::CELL.1 - tall..paint::CELL.1 {
            assert!(
                (0..paint::CELL.0).all(|x| ink(left + x, y)),
                "{eighths}/8 has a gap in row {y}"
            );
        }
        let want = paint::CELL.1 * eighths / 8;
        assert!(
            tall >= want && tall <= want + 1 && tall > last,
            "{eighths}/8 is {tall} rows high"
        );
        last = tall;
    }
    assert_eq!(last, paint::CELL.1, "the full block is the whole cell");
}

#[test]
fn a_window_holds_as_many_cells_as_the_font_fits_in_it() {
    // Hack's cell is 12x24, so a 1024x768 window is 85 by 32, with the four
    // pixels the columns do not fill left as background.
    assert_eq!(paint::fits(1024, 768, 1), (85, 32));
    // At scale two a cell is twice the size, so half as many fit.
    assert_eq!(paint::fits(1024, 768, 2), (42, 16));
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
/// Asked of `src/user/system/linux/compositor/layout` rather than worked out here: the gaps and
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
            transform: Default::default(),
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
/// where `src/user/system/linux/compositor/layout` puts it, the border and the background
/// `src/user/system/linux/compositor/render` gives it, and the terminal's pixels inside.
#[test]
fn a_terminal_running_a_program_is_the_expected_image() {
    use std::collections::BTreeMap;

    use compositor_render::{Canvas, Damage, Format, Style, Surface, Target};

    let layout = one_window();
    let (width, height) = window_size();
    let buffer = version_frame(width, height);
    let surface = Surface::new(
        &buffer,
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
    compositor_render::golden::check_in(
        &directory,
        "terminal-hyprctl-version",
        WIDTH,
        HEIGHT,
        &pixels,
    );
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
    // covers this font's top-left pixel.
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

/// Text is UTF-8: a character of several bytes is one cell, and a sequence
/// that is not UTF-8 is one U+FFFD where it goes wrong, not a cell a byte.
#[test]
fn utf8_is_a_character_a_cell_and_a_bad_sequence_is_one_replacement() {
    let mut grid = Grid::new(12, 1);
    // robbyrussell's arrow and agnoster's separator, between ASCII.
    grid.write("a➜b\u{E0B0}ü".as_bytes());
    assert_eq!(grid.line(0), "a➜b\u{E0B0}ü");
    assert_eq!(grid.cursor(), (5, 0));

    // A character split across two writes, as a read of a pipe may split it.
    let mut grid = Grid::new(12, 1);
    let arrow = "➜".as_bytes();
    grid.write(&arrow[..1]);
    grid.write(&arrow[1..]);
    assert_eq!(grid.line(0), "➜");

    // Cut short by ASCII, a lone continuation byte, an overlong encoding
    // and a byte no UTF-8 starts with: each one U+FFFD, and what follows is
    // itself.
    let mut grid = Grid::new(12, 1);
    grid.write(b"\xE2\x9Cx\x80\xC0\xAF\xFFy");
    assert_eq!(grid.line(0), "\u{FFFD}x\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}y");

    // An escape sequence in the middle ends the character and is still one.
    let mut grid = Grid::new(12, 1);
    grid.write(b"\xE2\x1b[31mz");
    assert_eq!(grid.line(0), "\u{FFFD}z");
    assert_eq!(grid.cell(1, 0).map(|cell| cell.colour), Some(1));
}

/// agnoster's segments are backgrounds, and a 256-colour sequence is one
/// colour, not three codes.
#[test]
fn a_background_is_kept_and_an_extended_colour_is_one_colour() {
    let mut grid = Grid::new(12, 1);
    grid.write(b"\x1b[44;30ma\x1b[49mb\x1b[104mc\x1b[0md");
    let background = |column: usize| grid.cell(column, 0).and_then(|cell| cell.background);
    assert_eq!(background(0), Some(4));
    assert_eq!(grid.cell(0, 0).map(|cell| cell.colour), Some(0));
    assert_eq!(background(1), None);
    assert_eq!(background(2), Some(12), "bright blue");
    assert_eq!(background(3), None, "the reset takes the background too");

    // `38;5;31` is colour 31 of 256 (a dark cyan-blue), not a `31` for red.
    let mut grid = Grid::new(12, 1);
    grid.write(b"\x1b[38;5;31ma\x1b[38;5;9mb\x1b[48;2;255;255;0mc");
    let cell = |column: usize| grid.cell(column, 0).copied().expect("a cell");
    assert_ne!(cell(0).colour, 1);
    assert_eq!(
        (cell(1).colour, cell(1).bold),
        (9, false),
        "9 is bright red"
    );
    assert_eq!(cell(2).background, Some(3 | 8), "bright yellow");
}

/// A colour the sixteen do not have is kept exactly, beside the nearest of
/// them; one of the sixteen, or a reset, takes it away again.
#[test]
fn an_exact_colour_is_kept_as_it_is() {
    let mut grid = Grid::new(8, 1);
    grid.write(b"\x1b[38;2;64;64;64;48;2;1;2;3ma\x1b[38;5;31mb\x1b[31mc\x1b[0md\x1b[38;5;9me");
    let cell = |column: usize| grid.cell(column, 0).copied().expect("a cell");
    assert_eq!(
        (cell(0).rgb, cell(0).background_rgb),
        (Some([64, 64, 64]), Some([1, 2, 3]))
    );
    // A dark grey is black among the sixteen, and still not bold: the weight
    // is `1`'s to say.
    assert_eq!((cell(0).colour, cell(0).bold), (0, false));
    // 256-colour 31 at xterm's levels: 0, 95, 135, 175, 215, 255.
    assert_eq!(cell(1).rgb, Some([0, 135, 175]));
    assert_eq!(cell(1).background_rgb, Some([1, 2, 3]));
    assert_eq!(
        (cell(2).rgb, cell(2).colour),
        (None, 1),
        "31 is the palette's red"
    );
    assert_eq!((cell(3).rgb, cell(3).background_rgb), (None, None));
    assert_eq!(
        (cell(4).rgb, cell(4).colour, cell(4).bold),
        (None, 9, false)
    );
}

/// btop's dim labels -- `info`, `terminate`, `kill` -- are a dark grey on
/// its own near-black background, and must be drawn in it: rounded to the
/// sixteen, they were black on black.
#[test]
fn a_dim_label_is_painted_in_its_own_grey() {
    let mut grid = Grid::new(1, 1);
    grid.write(b"\x1b[?25l\x1b[48;2;0;0;0m\x1b[38;2;64;64;64m\xe2\x96\x88");
    let (width, height) = paint::CELL;
    let mut pixels = vec![0u8; width * height * 4];
    paint::draw(
        &mut pixels,
        (width, height),
        width * 4,
        &grid,
        &paint::Colours::default(),
        1,
    );
    // A full block covers the cell: its middle is the grey itself, and a
    // grey is the same whatever order the buffer's channels are in.
    let middle = (height / 2 * width + width / 2) * 4;
    assert_eq!(pixels.get(middle..middle + 3), Some(&[64_u8, 64, 64][..]));
}

/// The prompts' characters are drawn from the font, not as the box every
/// character it lacks is drawn as.
#[test]
fn a_prompts_characters_have_glyphs_of_their_own() {
    let frame = |text: &str| {
        let mut grid = Grid::new(1, 1);
        grid.write(text.as_bytes());
        // Cursor off, so the cell is the glyph alone.
        grid.write(b"\x1b[?25l");
        let (width, height) = paint::CELL;
        let mut pixels = vec![0u8; width * height * 4];
        paint::draw(
            &mut pixels,
            (width, height),
            width * 4,
            &grid,
            &paint::Colours::default(),
            1,
        );
        pixels
    };
    // A character no face here has: CJK.
    let missing = frame("\u{4E2D}");
    // agnoster's status markers are the stand-ins the generator draws them as.
    for present in ["➜", "\u{E0B0}", "\u{E0A0}", "±", "─", "…", "✘", "⚡", "⚙"] {
        assert_ne!(frame(present), missing, "{present:?} is drawn as the box");
    }
}

/// agnoster's prompt as zinc renders it, byte for byte: a black segment, a
/// blue one, each ended by a separator drawn in the colour it ends.
#[test]
fn agnosters_prompt_is_two_segments_and_their_separators() {
    let mut grid = Grid::new(32, 1);
    grid.write(
        "\x1b[39m\x1b[0m\x1b[49m\x1b[40m\x1b[39m root@ferrix \x1b[44m\x1b[30m\u{E0B0}\
         \x1b[30m / \x1b[49m\x1b[34m\u{E0B0}\x1b[39m "
            .as_bytes(),
    );
    assert_eq!(grid.line(0), " root@ferrix \u{E0B0} / \u{E0B0}");
    let cell = |column: usize| grid.cell(column, 0).copied().expect("a cell");
    // The context segment: default text on black.
    assert_eq!(
        (cell(1).ch, cell(1).background, cell(1).colour),
        ('r', Some(0), 7)
    );
    // Its separator: black on the blue of the segment after it.
    assert_eq!((cell(13).background, cell(13).colour), (Some(4), 0));
    // The directory: black on blue.
    assert_eq!(
        (cell(15).ch, cell(15).background, cell(15).colour),
        ('/', Some(4), 0)
    );
    // The last separator: blue on the terminal's own background.
    assert_eq!((cell(17).background, cell(17).colour), (None, 4));
}

/// A row that scrolls off the top is kept, and the screen can look back at
/// it without the program's rows moving.
#[test]
fn scrolled_rows_are_kept_and_can_be_looked_back_at() {
    let mut grid = Grid::new(6, 2);
    grid.write(b"one\r\ntwo\r\nthree\r\nfour");
    assert_eq!(lines(&grid), ["three", "four"]);
    assert_eq!(grid.history(), 2);

    grid.scroll_view(1);
    let shown = |grid: &Grid, row: usize| -> String {
        (0..6)
            .filter_map(|column| grid.shown(column, row).map(|(cell, _)| cell.ch))
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    assert_eq!([shown(&grid, 0), shown(&grid, 1)], ["two", "three"]);
    // The cursor is on the live row that is now below the screen.
    assert_eq!(grid.shown_cursor(), None);

    // Further back than the history is the oldest row.
    grid.scroll_view(10);
    assert_eq!(grid.view(), 2);
    assert_eq!(shown(&grid, 0), "one");

    // Output while looking back keeps the same text in view.
    grid.write(b"\r\nfive");
    assert_eq!(grid.view(), 3);
    assert_eq!(shown(&grid, 0), "one");

    grid.view_live();
    assert_eq!([shown(&grid, 0), shown(&grid, 1)], ["four", "five"]);

    // `clear`'s `CSI 3 J` forgets the scrollback.
    grid.write(b"\x1b[3J");
    assert_eq!(grid.history(), 0);
}

/// A drag selects from one edge between cells to another, across rows, and
/// what is copied is the text: a line break where a row ended, none where it
/// only ran on, and no trailing blanks.
#[test]
fn a_selection_copies_the_text_it_covers() {
    let mut grid = Grid::new(8, 3);
    grid.write(b"hello   \r\nabcdefghij");
    // `abcdefgh` ran on into `ij`.
    let (from, to) = (grid.point(1, 0, false), grid.point(0, 2, true));
    grid.select(from, crate::grid::Unit::Cell);
    assert_eq!(
        grid.selected(),
        None,
        "a press that has not moved selects nothing"
    );
    grid.extend(to);
    assert_eq!(grid.selected().as_deref(), Some("ello\nabcdefghi"));
    assert!(grid.shown(1, 0).is_some_and(|(_, selected)| selected));
    assert!(!grid.shown(0, 0).is_some_and(|(_, selected)| selected));

    // Dragged backwards is the same selection.
    grid.select(to, crate::grid::Unit::Cell);
    grid.extend(from);
    assert_eq!(grid.selected().as_deref(), Some("ello\nabcdefghi"));

    grid.deselect();
    assert_eq!(grid.selected(), None);
}

/// A double click takes the word, and a path is one word; a triple click
/// takes the line.
#[test]
fn a_double_click_is_a_word_and_a_triple_click_a_line() {
    let mut grid = Grid::new(30, 2);
    grid.write(b"ls /usr/share (here)");
    grid.select(grid.point(6, 0, false), crate::grid::Unit::Word);
    assert_eq!(grid.selected().as_deref(), Some("/usr/share"));
    grid.select(grid.point(16, 0, false), crate::grid::Unit::Word);
    assert_eq!(grid.selected().as_deref(), Some("here"));
    grid.select(grid.point(2, 0, false), crate::grid::Unit::Line);
    assert_eq!(grid.selected().as_deref(), Some("ls /usr/share (here)"));
}

/// A selection is held in the text's own lines, so output that scrolls the
/// screen carries the selection up with the text.
#[test]
fn a_selection_moves_with_its_text_as_the_screen_scrolls() {
    let mut grid = Grid::new(6, 2);
    grid.write(b"keep\r\n");
    grid.select(grid.point(0, 0, false), crate::grid::Unit::Line);
    grid.write(b"x\r\ny");
    assert_eq!(grid.selected().as_deref(), Some("keep"));
    // Scrolled off the top, it is still what a copy takes, and it is
    // highlighted once the screen looks back at it.
    assert!(!grid.shown(0, 0).is_some_and(|(_, selected)| selected));
    grid.scroll_view(1);
    assert!(grid.shown(0, 0).is_some_and(|(_, selected)| selected));
}

/// Looking back and selecting damage the cells that changed on the screen,
/// and paint the picture a full redraw does.
#[test]
fn looking_back_and_selecting_damage_what_they_change() {
    let mut grid = Grid::new(8, 2);
    grid.write(b"one\r\ntwo\r\nthree");
    let before = grid.snapshot();
    grid.select(grid.point(0, 1, false), crate::grid::Unit::Cell);
    grid.extend(grid.point(1, 1, true));
    assert_eq!(
        grid.damage_since(&before),
        Some(crate::grid::CellDamage {
            left: 0,
            top: 1,
            width: 2,
            height: 1,
        })
    );

    let colours = paint::Colours::default();
    let (width, height) = (8 * paint::CELL.0, 2 * paint::CELL.1);
    let mut incremental = vec![0u8; width * height * 4];
    paint::draw(
        &mut incremental,
        (width, height),
        width * 4,
        &grid,
        &colours,
        1,
    );
    let before = grid.snapshot();
    grid.scroll_view(1);
    let damage = grid
        .damage_since(&before)
        .expect("looking back changes the screen");
    paint::draw_damage(
        &mut incremental,
        (width, height),
        width * 4,
        &grid,
        &colours,
        1,
        damage,
    );
    let mut full = vec![0u8; width * height * 4];
    paint::draw(&mut full, (width, height), width * 4, &grid, &colours, 1);
    assert_eq!(incremental, full);
}

/// A window that shrinks below the cursor keeps the cursor's row in view by
/// moving the rows above into the scrollback.
#[test]
fn shrinking_below_the_cursor_moves_rows_into_the_scrollback() {
    let mut grid = Grid::new(6, 4);
    grid.write(b"a\r\nb\r\nc\r\nd");
    grid.resize(6, 2);
    assert_eq!(lines(&grid), ["c", "d"]);
    assert_eq!(grid.cursor(), (1, 1));
    assert_eq!(grid.history(), 2);
}

/// `CSI ? 2004 h` asks for pastes to be bracketed, and `l` stops it.
#[test]
fn bracketed_paste_is_a_mode_a_program_turns_on_and_off() {
    let mut grid = Grid::new(4, 2);
    assert!(!grid.bracketed_paste());
    grid.write(b"\x1b[?2004h");
    assert!(grid.bracketed_paste());
    grid.write(b"\x1b[?2004l");
    assert!(!grid.bracketed_paste());
    assert_eq!(lines(&grid), ["", ""]);
}
