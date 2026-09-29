//! The program, on a machine with a screen and a sound card: straight on
//! the card as init, or in a window when a compositor is running.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;
use std::time::Duration;

use media_bav::Video;
use media_pcm::RATE;

use crate::args::{Options, parse, unshell};
use crate::clock::Clock;
use crate::scale::Fit;
use crate::screen::Screen;
use crate::sound;
use crate::step::{Shown, Step, Stepper};

pub(crate) fn say(text: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "badapple: {text}");
    let _ = out.flush();
}

pub(crate) fn fail(text: &str) -> ! {
    say(&format!("failed: {text}"));
    rest()
}

/// Init does not exit; a program in a window does, once it is closed.
fn rest() -> ! {
    if std::process::id() != 1 {
        std::process::exit(1);
    }
    loop {
        // SAFETY: `pause` takes nothing and only waits.
        let _ = unsafe { libc::pause() };
    }
}

/// The song, playing on a thread of its own.
pub(crate) struct Song {
    thread: Option<JoinHandle<Result<sound::Played, String>>>,
    /// Set to stop it early: the window was closed.
    pub(crate) stop: Arc<AtomicBool>,
}

/// Start the song, telling `clock` where it is; without a card, the clock
/// follows the wall.
pub(crate) fn start_song(options: &Options, clock: &Arc<Clock>) -> Song {
    let stop = Arc::new(AtomicBool::new(false));
    let thread = match sound::open() {
        Ok(playback) => {
            let config = playback.config();
            say(&format!(
                "sound {} Hz, {} channels, period {}, buffer {}",
                config.rate, config.channels, config.period, config.buffer
            ));
            let clock = Arc::clone(clock);
            let stop = Arc::clone(&stop);
            let (path, seconds) = (options.song.clone(), options.seconds);
            Some(std::thread::spawn(move || {
                let played = sound::play(&path, seconds, playback, &clock, &stop);
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
    Song { thread, stop }
}

/// Wait for the song, and say how the picture and the song went. A second
/// call says nothing of the song, which the first took.
pub(crate) fn finish(song: &mut Song, shown: Shown) {
    let played = match song.thread.take().map(JoinHandle::join) {
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
}

/// The frames to show: all of them, or the first `seconds`' worth.
pub(crate) fn last_frame(options: &Options, header: &media_bav::Header) -> u32 {
    options.seconds.map_or(header.frames, |seconds| {
        let limit = u64::from(seconds) * u64::from(header.rate_num) / u64::from(header.rate_den);
        header.frames.min(u32::try_from(limit).unwrap_or(u32::MAX))
    })
}

/// Every ten seconds of video, a line saying where the picture and the song
/// are, which `xtask test-badapple` checks the two against each other with.
#[derive(Debug)]
pub(crate) struct Progress {
    every: u32,
    next: u32,
}

impl Progress {
    pub(crate) fn new(header: &media_bav::Header) -> Self {
        let every = (u32::from(header.rate_num) * 10 / u32::from(header.rate_den).max(1)).max(1);
        Self { every, next: every }
    }

    pub(crate) fn shown(&mut self, stepper: &Stepper<'_>, frame: u32, song: u64) {
        if frame >= self.next {
            say(&format!(
                "frame {frame} at {} ms, song at {} ms",
                stepper.time_of(frame) / 1000,
                song / 1000
            ));
            while self.next <= frame {
                self.next += self.every;
            }
        }
    }
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
    // A compositor is running: be a window in it. Otherwise the card is
    // this program's alone.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        crate::window::run(&options, video);
        std::process::exit(0);
    }
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
    let mut song = start_song(&options, &clock);
    say("ready");
    let mut stepper = Stepper::new(video, last_frame(&options, &header));
    show(&mut stepper, &mut screen, &fit, &clock);
    finish(&mut song, stepper.shown);
    rest()
}

/// Show every frame by the clock, straight on the card.
fn show(stepper: &mut Stepper<'_>, screen: &mut Screen, fit: &Fit, clock: &Clock) {
    let invert = cfg!(feature = "negative-control");
    let mut progress = Progress::new(stepper.header());
    loop {
        match stepper.advance(clock.micros()) {
            Step::Ended => return,
            Step::Wait(wait) => {
                std::thread::sleep(Duration::from_micros(wait.clamp(1_000, 20_000)));
            }
            Step::Show { frame, rows } => {
                if let Some((top, bottom)) = rows {
                    let rows = fit.screen_rows(usize::from(top), usize::from(bottom));
                    let pitch = screen.pitch();
                    fit.draw(
                        stepper.shades(),
                        screen.pixels(),
                        pitch,
                        rows.clone(),
                        invert,
                    );
                    if let Err(error) = screen.show(fit.y + rows.start, rows.len()) {
                        say(&format!("showing frame {frame}: {error}"));
                    }
                }
                progress.shown(stepper, frame, clock.micros());
            }
        }
    }
}
