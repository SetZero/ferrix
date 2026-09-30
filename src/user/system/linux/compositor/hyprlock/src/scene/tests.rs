//! Frames drawn from configurations, with text drawn as solid boxes so
//! that where each widget lands can be read off the pixels.

use std::path::Path;
use std::rc::Rc;

use tiny_skia::Pixmap;

use super::{Assets, Scene, Screen, TextRequest, View};
use crate::auth::{REJECTED, Verdict};

fn refused() -> Verdict {
    Verdict::Failed {
        text: REJECTED.to_owned(),
        retry_after_ms: FAIL_DELAY_MS,
    }
}
use crate::config::Config;
use crate::format::Context;
use crate::session::{FAIL_DELAY_MS, KeyPress, Session};

/// Text as a box: half the point size wide per character, four thirds of
/// it high, in the request's colour.
#[derive(Default)]
struct Boxes {
    asked: Vec<TextRequest>,
}

impl Assets for Boxes {
    fn text(&mut self, request: &TextRequest) -> Option<Rc<Pixmap>> {
        self.asked.push(request.clone());
        let chars = u32::try_from(request.text.chars().count()).ok()?;
        let size = u32::try_from(request.size).ok()?;
        let mut pixmap = Pixmap::new((chars * size / 2).max(1), (size * 4 / 3).max(1))?;
        let [r, g, b] =
            [16, 8, 0].map(|shift| u8::try_from((request.color >> shift) & 0xFF).unwrap_or(0));
        pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
        Some(Rc::new(pixmap))
    }

    fn image(&mut self, _path: &Path) -> Option<Rc<Pixmap>> {
        None
    }
}

const FILE: &str = r###"
general {
    ignore_empty_input = true
}
background {
    monitor = desc:Lenovo Group Limited R27qe Gen2
    color = rgb(1c1b22)
    reload_cmd = /nowhere/booru-wallpaper lockshot SERIAL
    reload_time = 20
}
shape {
    monitor =
    size = 520, 190
    color = rgba(0, 0, 0, 0.35)
    rounding = 24
    position = 0, 240
}
label {
    monitor =
    text = $TIME
    color = rgba(255, 255, 255, 0.95)
    font_size = 96
    position = 0, 262
}
label {
    monitor =
    text = cmd[update:60000] date
    font_size = 22
    position = 0, 190
}
input-field {
    monitor = desc:Lenovo Group Limited R27qe Gen2
    size = 320, 56
    outline_thickness = 2
    dots_spacing = 0.3
    outer_color = rgba(255, 255, 255, 0.35)
    inner_color = rgba(0, 0, 0, 0.45)
    font_color = rgb(240, 240, 240)
    fade_on_empty = false
    placeholder_text = <span foreground="##cccccc">Password</span>
    fail_text = <span foreground="##ff8888">$PAMFAIL</span>
    check_color = rgba(255, 255, 255, 0.7)
    fail_color = rgba(255, 100, 100, 0.9)
    rounding = 28
    position = 0, -60
}
"###;

fn lenovo() -> Screen {
    Screen {
        name: "DP-2".to_owned(),
        description: "Lenovo Group Limited R27qe Gen2 SERIAL (DP-2)".to_owned(),
        width: 1920,
        height: 1080,
    }
}

fn context(session: &Session) -> Context {
    Context {
        user: "ferrix".to_owned(),
        now: 1_790_446_050,
        attempts: session.attempts,
        fail: session.fail_text.clone(),
        pam_fail: session.pam_fail.clone(),
        ..Context::default()
    }
}

fn frame(scene: &mut Scene, session: &Session, now: u64, assets: &mut Boxes) -> Pixmap {
    let Some(mut pixmap) = Pixmap::new(1920, 1080) else {
        panic!("pixmap");
    };
    let context = context(session);
    let view = View {
        now,
        opacity: 1.0,
        session,
        context: &context,
        screenshot: None,
    };
    let _ = scene.draw(&mut pixmap.as_mut(), &view, assets);
    pixmap
}

fn rgba(pixmap: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
    pixmap
        .pixel(x, y)
        .map(|pixel| {
            let pixel = pixel.demultiply();
            (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha())
        })
        .unwrap_or_default()
}

fn key(code: u32, keysym: &'static str, text: &'static str, pressed: bool) -> KeyPress<'static> {
    KeyPress {
        code,
        keysym,
        text,
        pressed,
        repeat: false,
        control: false,
        caps_lock: false,
        num_lock: false,
    }
}

#[test]
fn the_customer_s_layout_lands_where_upstream_puts_it() {
    let config = Config::parse(FILE, Path::new("/hyprlock.conf"));
    assert_eq!(config.diagnostics, []);
    let session = Session::new(true, 2000, 0, 0);
    let mut scene = Scene::new(&config, &lenovo(), &context(&session), 0);
    assert_eq!(scene.len(), 5);
    let mut assets = Boxes::default();
    let pixmap = frame(&mut scene, &session, 0, &mut assets);
    // The background's colour, since its command has not answered.
    assert_eq!(rgba(&pixmap, 5, 5), (0x1c, 0x1b, 0x22, 255));
    // The clock panel: 0.35 black over the colour, from x 700 and y 205.
    let panel = rgba(&pixmap, 720, 300);
    assert!(panel.0 < 0x1c && panel.0 > 0x0c, "{panel:?}");
    assert_eq!(rgba(&pixmap, 690, 300), (0x1c, 0x1b, 0x22, 255));
    // The clock: "18:07" at 96 points is 240 x 128 here, centred and 262
    // up from the middle: x 840..1080, y 540 - 262 - 64 = 214.
    assert_eq!(rgba(&pixmap, 845, 220).0, 0xF3);
    assert_eq!(rgba(&pixmap, 835, 220), panel);
    let clock = assets.asked.iter().find(|request| request.size == 96);
    assert_eq!(clock.map(|request| request.text.as_str()), Some("18:07"));
    // The field: 320 x 56 centred, 60 below the middle, a 2-pixel outline.
    // Its inside is 0.45 black over the colour.
    let inside = rgba(&pixmap, 960, 600 - 14);
    assert!(inside.0 < 0x12, "{inside:?}");
    // The placeholder asked for in the field's font size: 56 / 4 = 14.
    let placeholder = assets.asked.iter().find(|request| request.size == 14);
    assert_eq!(
        placeholder.map(|request| request.text.as_str()),
        Some("<span foreground=\"#cccccc\">Password</span>")
    );
}

#[test]
fn another_screen_gets_only_what_names_every_screen() {
    let config = Config::parse(FILE, Path::new("/hyprlock.conf"));
    let session = Session::new(true, 2000, 0, 0);
    let qemu = Screen {
        name: "Virtual-1".to_owned(),
        description: "RHT QEMU Monitor".to_owned(),
        width: 1280,
        height: 800,
    };
    let scene = Scene::new(&config, &qemu, &context(&session), 0);
    assert_eq!(scene.len(), 3);
    assert!(!scene.has_field());
}

#[test]
fn commands_run_and_their_output_is_shown_trimmed() {
    let config = Config::parse(FILE, Path::new("/hyprlock.conf"));
    let session = Session::new(true, 2000, 0, 0);
    let mut scene = Scene::new(&config, &lenovo(), &context(&session), 0);
    let (jobs, _) = scene.due(&context(&session), 0);
    let lines: Vec<&str> = jobs.iter().map(|job| job.line.as_str()).collect();
    assert_eq!(lines, ["/nowhere/booru-wallpaper lockshot SERIAL", " date"]);
    // The wallpaper script is not there: no output, and the colour stays.
    for job in &jobs {
        let output = if job.line.contains("date") {
            "Saturday, 26 September\n"
        } else {
            ""
        };
        scene.finished(job.widget, output, 5);
    }
    let mut assets = Boxes::default();
    let pixmap = frame(&mut scene, &session, 5, &mut assets);
    assert_eq!(rgba(&pixmap, 5, 5), (0x1c, 0x1b, 0x22, 255));
    assert!(
        assets
            .asked
            .iter()
            .any(|request| request.text == "Saturday, 26 September")
    );
    // Nothing more until the next update, a minute on.
    assert_eq!(scene.due(&context(&session), 1000).0, []);
    assert_eq!(scene.due(&context(&session), 20_000).0.len(), 1);
    assert_eq!(scene.due(&context(&session), 60_000).0.len(), 1);
}

#[test]
fn dots_then_the_check_then_the_failure() {
    let config = Config::parse(FILE, Path::new("/hyprlock.conf"));
    let mut session = Session::new(true, 2000, 0, 0);
    let mut scene = Scene::new(&config, &lenovo(), &context(&session), 0);
    let mut assets = Boxes::default();
    for (code, letter) in [(30, "a"), (31, "s"), (32, "d")] {
        let _ = session.key(&key(code, letter, letter, true), 0);
        let _ = session.key(&key(code, letter, letter, false), 0);
    }
    let _ = frame(&mut scene, &session, 0, &mut assets);
    // inputFieldDots is 8 tenths of a second by default; by 1000 ms the
    // three dots are all there. Each is 14 pixels, 4 apart, centred:
    // 3 * 18 - 4 = 50 wide, from 960 - 25.
    let pixmap = frame(&mut scene, &session, 1000, &mut assets);
    let field_middle = 1080 / 2 + 60;
    for dot in 0..3 {
        let x = 935 + dot * 18 + 7;
        assert_eq!(rgba(&pixmap, x, field_middle).0, 240, "dot {dot}");
    }
    assert!(rgba(&pixmap, 935 + 3 * 18 + 7, field_middle).0 < 20);

    // Enter: the field empties, the check runs, the outline turns
    // check_color.
    let _ = session.key(&key(28, "Return", "", true), 1000);
    assert!(session.checking());
    session.answered(refused(), 1000);
    let _ = frame(&mut scene, &session, 1000, &mut assets);
    let checking = frame(&mut scene, &session, 2900, &mut assets);
    let outline = rgba(&checking, 960, 600 - 28 - 1);
    assert!(outline.0 > 150, "{outline:?}");

    // The delay passes: fail_color, and the fail text as the placeholder.
    assert!(session.tick(1000 + FAIL_DELAY_MS));
    let _ = frame(&mut scene, &session, 1000 + FAIL_DELAY_MS, &mut assets);
    let failing = frame(
        &mut scene,
        &session,
        1000 + FAIL_DELAY_MS + 1000,
        &mut assets,
    );
    let outline = rgba(&failing, 960, 600 - 28 - 1);
    assert!(outline.0 > outline.1 + 40, "{outline:?}");
    assert!(
        assets
            .asked
            .iter()
            .any(|request| request.text
                == "<span foreground=\"#ff8888\">Authentication failed</span>"),
        "{:?}",
        assets
            .asked
            .iter()
            .map(|request| &request.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_click_runs_what_is_under_it() {
    let config = Config::parse(
        "shape {\n  size = 100, 50\n  onclick = echo clicked\n}\nshape {\n  size = 10, 10\n  position = 300, 0\n}\n",
        Path::new("/c.conf"),
    );
    let session = Session::new(true, 2000, 0, 0);
    let scene = Scene::new(&config, &lenovo(), &context(&session), 0);
    // The first shape is centred on a 1920x1080 screen.
    assert_eq!(scene.click(960.0, 540.0), ["echo clicked"]);
    assert_eq!(scene.click(5.0, 5.0), Vec::<String>::new());
    // The second has no onclick.
    assert_eq!(scene.click(1265.0, 540.0), Vec::<String>::new());
}
