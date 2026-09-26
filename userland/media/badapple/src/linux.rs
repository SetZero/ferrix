//! The program, on a machine with a screen and a sound card.

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use media_bav::{Picture, Video};
use media_pcm::RATE;

use crate::args::{parse, unshell};
use crate::clock::Clock;
use crate::scale::Fit;
use crate::screen::Screen;
use crate::sound;

pub(crate) fn say(text: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "badapple: {text}");
    let _ = out.flush();
}

fn fail(text: &str) -> ! {
    say(&format!("failed: {text}"));
    rest()
}

/// Init does not exit.
fn rest() -> ! {
    loop {
        // SAFETY: `pause` takes nothing and only waits.
        let _ = unsafe { libc::pause() };
    }
}

/// How the picture kept up.
#[derive(Clone, Copy, Debug, Default)]
struct Shown {
    frames: u32,
    skipped: u32,
    /// The most a frame was shown after its time, in microseconds.
    latest: u64,
    bad: u32,
    last: Option<u32>,
}

pub(crate) fn run() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let options = parse(&unshell(&argv)).unwrap_or_else(|error| fail(&error));
    if cfg!(feature = "negative-control") {
        say("negative control");
    }
    let bytes = std::fs::read(&options.video)
        .unwrap_or_else(|error| fail(&format!("{}: {error}", options.video)));
    let video =
        Video::parse(&bytes).unwrap_or_else(|error| fail(&format!("{}: {error}", options.video)));
    let header = video.header;
    let mut screen = Screen::open().unwrap_or_else(|error| fail(&format!("the screen: {error}")));
    let fit = Fit::new(
        usize::from(header.width),
        usize::from(header.height),
        screen.width,
        screen.height,
    );
    say(&format!(
        "screen {} fit {} {} {} {} video {}x{} {} frames at {}/{}",
        screen.mode(),
        fit.x,
        fit.y,
        fit.width,
        fit.height,
        header.width,
        header.height,
        header.frames,
        header.rate_num,
        header.rate_den
    ));

    let clock = Arc::new(Clock::new(RATE));
    let song = match sound::open() {
        Ok(playback) => {
            let config = playback.config();
            say(&format!(
                "sound {} Hz, {} channels, period {}, buffer {}",
                config.rate, config.channels, config.period, config.buffer
            ));
            let clock = Arc::clone(&clock);
            let (path, seconds) = (options.song.clone(), options.seconds);
            Some(std::thread::spawn(move || {
                let played = sound::play(&path, seconds, playback, &clock);
                if played.is_err() {
                    clock.follow_the_wall();
                }
                played
            }))
        }
        Err(error) => {
            say(&format!("no sound, keeping time by the wall: {error}"));
            clock.follow_the_wall();
            None
        }
    };
    say("ready");

    let last = options.seconds.map_or(header.frames, |seconds| {
        let limit = u64::from(seconds) * u64::from(header.rate_num) / u64::from(header.rate_den);
        header.frames.min(u32::try_from(limit).unwrap_or(u32::MAX))
    });
    let shown = show(&video, &mut screen, &fit, &clock, last);

    let played = match song.map(std::thread::JoinHandle::join) {
        Some(Ok(Ok(played))) => played,
        Some(Ok(Err(error))) => {
            say(&format!("sound failed: {error}"));
            sound::Played::default()
        }
        Some(Err(_)) => {
            say("sound failed: its thread panicked");
            sound::Played::default()
        }
        None => sound::Played::default(),
    };
    say(&format!(
        "done: shown {} skipped {} bad {} latest {} ms, sound {} frames ({} ms) underruns {} bad packets {}",
        shown.frames,
        shown.skipped,
        shown.bad,
        shown.latest / 1000,
        played.frames,
        played.frames * 1000 / u64::from(RATE),
        played.underruns,
        played.bad_packets
    ));
    if let Some(frame) = shown.last {
        say(&format!("holding frame {frame}"));
    }
    rest()
}

/// Microseconds from the start at which frame `n` is shown.
fn time_of(header: &media_bav::Header, n: u32) -> u64 {
    u64::from(n) * 1_000_000 * u64::from(header.rate_den) / u64::from(header.rate_num)
}

/// Show frames `0..last` by the clock.
fn show(video: &Video<'_>, screen: &mut Screen, fit: &Fit, clock: &Clock, last: u32) -> Shown {
    let header = video.header;
    let mut picture = Picture::new(&header);
    let mut shown = Shown::default();
    let mut next = 0_u32;
    let invert = cfg!(feature = "negative-control");
    // Every ten seconds of video, a line saying where the picture and the
    // song are.
    let every = (u32::from(header.rate_num) * 10 / u32::from(header.rate_den).max(1)).max(1);
    let mut report = every;
    while next < last {
        let now = clock.micros();
        let due = u32::try_from(header.frame_at(now)).unwrap_or(u32::MAX);
        if due < next {
            let wait = time_of(&header, next).saturating_sub(now);
            std::thread::sleep(Duration::from_micros(wait.clamp(1_000, 20_000)));
            continue;
        }
        let target = due.min(last - 1);
        let mut damage: Option<(u16, u16)> = None;
        let first = next;
        while next <= target {
            match video.frame(next).map(|frame| picture.apply(frame)) {
                Some(Ok(Some((top, bottom)))) => {
                    damage =
                        Some(damage.map_or((top, bottom), |(t, b)| (t.min(top), b.max(bottom))));
                }
                Some(Ok(None)) => {}
                Some(Err(_)) | None => shown.bad += 1,
            }
            next += 1;
        }
        shown.skipped += target - first;
        if let Some((top, bottom)) = damage {
            let rows = fit.screen_rows(usize::from(top), usize::from(bottom));
            let pitch = screen.pitch();
            fit.draw(
                picture.shades(),
                screen.pixels(),
                pitch,
                rows.clone(),
                invert,
            );
            if let Err(error) = screen.show(fit.y + rows.start, rows.len()) {
                say(&format!("showing frame {target}: {error}"));
            }
        }
        shown.frames += 1;
        shown.last = Some(target);
        shown.latest = shown
            .latest
            .max(clock.micros().saturating_sub(time_of(&header, target)));
        if target >= report {
            say(&format!(
                "frame {target} at {} ms, song at {} ms",
                time_of(&header, target) / 1000,
                clock.micros() / 1000
            ));
            while report <= target {
                report += every;
            }
        }
    }
    shown
}
