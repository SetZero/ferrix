use super::{
    DECIMATE, Fit, GUEST_PLAYER, GUEST_SONG, GUEST_VIDEO, HEIGHT, WIDTH, WINDOW, correlation,
    desktop_entry, fit, mono, picture_mismatches, prepared_signal, progress, window_config,
    windows,
};
use crate::display::Image;

fn owned(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

#[test]
fn the_fit_is_read_from_the_screen_line() {
    let lines = owned(&[
        "  init     starting",
        "badapple: screen 1280x800 Virtual-1 fit 107 0 1066 800 video 512x384 6572 frames at 30/1",
    ]);
    assert_eq!(
        fit(&lines),
        Some(Fit {
            x: 107,
            y: 0,
            width: 1066,
            height: 800
        })
    );
    assert_eq!(fit(&owned(&["badapple: ready"])), None);
}

#[test]
fn progress_lines_give_both_times() {
    let lines = owned(&[
        "badapple: frame 300 at 10000 ms, song at 10012 ms",
        "badapple: frame 600 at 20000 ms, song at 19990 ms",
        "badapple: ready",
    ]);
    assert_eq!(progress(&lines), vec![(10_000, 10_012), (20_000, 19_990)]);
}

/// A screen showing `shades` at `fit`, inverted or not.
fn screen(fit: Fit, shades: &[u8], invert: bool) -> Image {
    let (width, height) = (fit.x * 2 + fit.width, fit.y * 2 + fit.height);
    let mut pixels = vec![0_u8; width * height * 3];
    let (w, h) = (usize::from(WIDTH), usize::from(HEIGHT));
    for y in 0..fit.height {
        for x in 0..fit.width {
            let shade = shades[(y * h / fit.height) * w + x * w / fit.width];
            let shade = if invert { 15 - shade } else { shade };
            let at = ((fit.y + y) * width + fit.x + x) * 3;
            pixels[at..at + 3].fill(shade * 17);
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

fn pattern() -> Vec<u8> {
    let (w, h) = (usize::from(WIDTH), usize::from(HEIGHT));
    (0..w * h).map(|i| ((i % w + i / w) % 16) as u8).collect()
}

#[test]
fn a_faithful_screen_matches_everywhere() {
    for fit in [
        Fit {
            x: 107,
            y: 0,
            width: 1066,
            height: 800,
        },
        Fit {
            x: 0,
            y: 0,
            width: 1024,
            height: 768,
        },
    ] {
        let shades = pattern();
        let (wrong, checked, _) = picture_mismatches(&screen(fit, &shades, false), fit, &shades);
        assert_eq!(wrong, 0, "{fit:?}");
        assert_eq!(checked, usize::from(WIDTH) * usize::from(HEIGHT), "{fit:?}");
    }
}

#[test]
fn an_inverted_or_moved_screen_does_not() {
    let fit = Fit {
        x: 107,
        y: 0,
        width: 1066,
        height: 800,
    };
    let shades = pattern();
    let (wrong, checked, first) = picture_mismatches(&screen(fit, &shades, true), fit, &shades);
    assert_eq!(wrong, checked, "inverted fails everywhere");
    assert_eq!(first.len(), 5, "and says where");
    let moved = Fit { x: 108, ..fit };
    let (wrong, ..) = picture_mismatches(&screen(moved, &shades, false), fit, &shades);
    assert!(wrong > 0, "a picture a pixel off is not the picture");
}

/// Something song-like: a few tones whose mix changes every 100 ms.
fn song(frames: usize, seed: u64) -> Vec<f64> {
    let mut state = seed;
    // Each seed its own key.
    let pitch = 1.0 + seed as f64 * 0.13;
    let mut amplitude = [0.0_f64; 3];
    (0..frames)
        .map(|n| {
            if n % 4800 == 0 {
                for a in &mut amplitude {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    *a = (state >> 33) as f64 / f64::from(u32::MAX) * 8000.0;
                }
            }
            let t = n as f64 / 48_000.0;
            amplitude
                .iter()
                .zip([110.0, 220.0, 330.0])
                .map(|(a, hz)| a * (2.0 * core::f64::consts::PI * hz * pitch * t).sin())
                .sum()
        })
        .collect()
}

#[test]
fn the_song_is_found_at_its_lag() {
    let reference = song(48_000 * 10, 1);
    // Heard 30 ms late, with a 40 ms gap four seconds in, as an underrun
    // would leave.
    let mut heard = vec![0.0; 1440];
    heard.extend_from_slice(&reference[..48_000 * 4]);
    heard.extend(std::iter::repeat_n(0.0, 1920));
    heard.extend_from_slice(&reference[48_000 * 4..]);
    let found = windows(&prepared_signal(&heard), &prepared_signal(&reference));
    assert!(found.len() >= 4, "{found:?}");
    for (at, lag, score) in &found {
        assert!(*score > 0.99, "{at} s: {score}");
        assert!(lag.abs() <= 40, "{at} s: lag {lag}");
    }
}

#[test]
fn another_song_is_not_found() {
    let reference = song(48_000 * 6, 1);
    let other = song(48_000 * 6, 2);
    let found = windows(&prepared_signal(&other), &prepared_signal(&reference));
    assert!(!found.is_empty());
    assert!(found.iter().all(|(_, _, score)| *score < 0.9), "{found:?}");
}

#[test]
fn silence_correlates_with_nothing() {
    assert!(correlation(&[0.0; 100], &[1.0; 100]).abs() < 1e-12);
    assert!(correlation(&[], &[]).abs() < 1e-12);
    let ramp: Vec<f64> = (0..100).map(f64::from).collect();
    assert!((correlation(&ramp, &ramp) - 1.0).abs() < 1e-12);
}

#[test]
fn stereo_bytes_become_mono() {
    let bytes = [0x10, 0x00, 0x30, 0x00, 0xff, 0xff, 0x01, 0x00, 0x05];
    assert_eq!(
        mono(&bytes),
        vec![32.0, 0.0],
        "a trailing odd byte is dropped"
    );
    assert_eq!(WINDOW, 2 * 48_000 / DECIMATE);
}

#[test]
fn a_window_s_last_fit_is_the_one_on_the_screen() {
    let lines = owned(&[
        "badapple: window 982x726 fit 7 0 968 726 video 512x384 6572 frames at 30/1",
        "badapple: ready",
        "badapple: window 1024x768 fit 0 0 1024 768",
    ]);
    assert_eq!(
        fit(&lines),
        Some(Fit {
            x: 0,
            y: 0,
            width: 1024,
            height: 768
        })
    );
}

#[test]
fn the_desktop_starts_what_the_image_carries() {
    let entry = desktop_entry();
    let exec = format!("Exec=/{GUEST_PLAYER} /{GUEST_VIDEO} /{GUEST_SONG}\n");
    assert!(
        entry.starts_with("[Desktop Entry]\nType=Application\n"),
        "{entry}"
    );
    assert!(entry.contains(&exec), "{entry}");
    let config = window_config(12);
    assert!(
        config.contains(&format!(
            "exec-once = /{GUEST_PLAYER} /{GUEST_VIDEO} /{GUEST_SONG} 12\n"
        )),
        "{config}"
    );
    assert!(
        config.contains("windowrule = fullscreen, match:class ^(badapple)$"),
        "{config}"
    );
}
