#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]

use std::time::Duration;

use crate::args::{Options, parse, unshell};
use crate::clock::{COAST, position};
use crate::scale::Fit;

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn init_scripts_become_arguments() {
    assert_eq!(
        unshell(&owned(&["-c", "/a.bav /b.m4a 20"])),
        owned(&["/a.bav", "/b.m4a", "20"]),
        "a line"
    );
    assert_eq!(
        unshell(&owned(&["-c", "/a b.bav\n/b.m4a\n"])),
        owned(&["/a b.bav", "/b.m4a"]),
        "lines"
    );
    assert!(unshell(&owned(&["-i"])).is_empty(), "nothing asked for");
    assert_eq!(
        unshell(&owned(&["/a.bav", "/b.m4a"])),
        owned(&["/a.bav", "/b.m4a"]),
        "as typed"
    );
}

#[test]
fn arguments_parse() {
    assert_eq!(
        parse(&owned(&["v", "s", "20"])),
        Ok(Options {
            video: "v".into(),
            song: "s".into(),
            seconds: Some(20)
        }),
        "with seconds"
    );
    assert_eq!(
        parse(&owned(&["v", "s"])).unwrap().seconds,
        None,
        "to the end"
    );
    assert!(parse(&owned(&["v"])).is_err(), "too few");
    assert!(parse(&owned(&["v", "s", "x"])).is_err(), "not a number");
}

#[test]
fn the_card_clock_coasts_only_so_far() {
    assert_eq!(
        position(1_000, Duration::from_millis(5), true),
        6_000,
        "runs on"
    );
    assert_eq!(
        position(1_000, Duration::from_secs(5), true),
        1_000 + COAST.as_micros() as u64,
        "stops coasting"
    );
    assert_eq!(
        position(1_000, Duration::from_secs(5), false),
        5_001_000,
        "the wall does not stop"
    );
}

#[test]
fn a_four_by_three_picture_fills_the_height_of_a_wide_screen() {
    let fit = Fit::new(512, 384, 1280, 800);
    assert_eq!(
        (fit.x, fit.y, fit.width, fit.height),
        (107, 0, 1066, 800),
        "pillar-boxed"
    );
    let fit = Fit::new(512, 384, 1024, 768);
    assert_eq!(
        (fit.x, fit.y, fit.width, fit.height),
        (0, 0, 1024, 768),
        "exactly twice"
    );
    assert_eq!(fit.source_row(767), Some(383), "last row");
    assert_eq!(fit.source_column(1), Some(0), "doubled");
    let fit = Fit::new(512, 384, 640, 1000);
    assert_eq!(
        (fit.x, fit.y, fit.width, fit.height),
        (0, 260, 640, 480),
        "letter-boxed"
    );
}

#[test]
fn picture_rows_map_to_screen_rows() {
    let fit = Fit::new(4, 3, 8, 6);
    assert_eq!(fit.screen_rows(1, 1), 2..4, "one row, doubled");
    assert_eq!(fit.screen_rows(0, 2), 0..6, "all");
}

#[test]
fn drawing_scales_and_greys() {
    let fit = Fit::new(2, 2, 4, 4);
    let shades = [0, 15, 15, 0];
    let mut pixels = vec![0xaa_u8; 4 * 4 * 4];
    fit.draw(&shades, &mut pixels, 16, 0..4, false);
    let grey = |pixels: &[u8], x: usize, y: usize| pixels[y * 16 + x * 4];
    let row0: Vec<u8> = (0..4).map(|x| grey(&pixels, x, 0)).collect();
    assert_eq!(row0, [0, 0, 255, 255], "row 0");
    assert_eq!(
        [grey(&pixels, 0, 3), grey(&pixels, 3, 3)],
        [255, 0],
        "row 3"
    );
    assert_eq!(pixels[3], 255, "opaque");
    fit.draw(&shades, &mut pixels, 16, 0..1, true);
    assert_eq!(grey(&pixels, 0, 0), 255, "inverted");
    assert_eq!(grey(&pixels, 0, 1), 0, "only the rows asked for");
}

#[test]
fn drawing_stops_at_a_short_buffer() {
    let fit = Fit::new(2, 2, 4, 4);
    let mut pixels = vec![0_u8; 20];
    fit.draw(&[15; 4], &mut pixels, 16, 0..4, false);
    assert_eq!(
        pixels[..16],
        [255, 255, 255, 255].repeat(4)[..],
        "the row that fits"
    );
}
