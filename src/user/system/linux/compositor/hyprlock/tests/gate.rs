//! The pictures `cargo xtask test-compositor --boot hyprlock` requires of
//! the guest's screen, drawn here from `tests/data/gate.conf` by the same
//! code the guest runs, and composited onto a black frame by
//! `src/user/system/linux/compositor/render` as hyprix composites a lock surface.
//!
//! Bless with `COMPOSITOR_RENDER_BLESS=1` after a change that is meant to
//! move a pixel, look at what was written, and commit it.

#![expect(
    clippy::expect_used,
    reason = "the helpers of a test, whose job is to stop it with a clear message"
)]

use std::path::{Path, PathBuf};

use compositor_hyprlock::assets::System;
use compositor_hyprlock::auth::{REJECTED, Verdict};
use compositor_hyprlock::config::Config;
use compositor_hyprlock::format::Context;
use compositor_hyprlock::scene::{Scene, Screen, View};
use compositor_hyprlock::session::{KeyPress, Session};

/// `authd`'s hold on a refusal, the `hyprlock` policy's `FailDelaySec`.
const FAIL_DELAY_MS: u64 = 2000;
use compositor_render::{Canvas, Damage, Format, Rect, Surface, golden};

/// The screen test-compositor's boots have.
const SIZE: (u32, u32) = (1024, 768);

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn config() -> Config {
    let path = data().join("gate.conf");
    let text = std::fs::read_to_string(&path).expect("gate.conf");
    let config = Config::parse(&text, &path);
    assert_eq!(config.diagnostics, [], "gate.conf reads cleanly");
    config
}

fn assets() -> System {
    let fonts =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../../assets/fonts/liberation");
    System::with_font_dirs(&[&fonts])
}

fn context(session: &Session) -> Context {
    Context {
        // hyprix is init on the guest, and hyprlock runs as root.
        user: "root".to_owned(),
        gecos: "root".to_owned(),
        attempts: session.attempts,
        fail: session.fail_text.clone(),
        pam_fail: session.pam_fail.clone(),
        pam_prompt: Some("Password: ".to_owned()),
        ..Context::default()
    }
}

/// A frame of the lock surface as the guest's screen shows it: the
/// premultiplied RGBA the toolkit copies into `ARGB8888`, composited onto
/// hyprix's black canvas.
fn screen(scene: &mut Scene, session: &Session, now: u64, assets: &mut System) -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(SIZE.0, SIZE.1).expect("pixmap");
    let context = context(session);
    let view = View {
        now,
        opacity: 1.0,
        session,
        context: &context,
        screenshot: None,
    };
    let _ = scene.draw(&mut pixmap.as_mut(), &view, assets);
    let argb: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| [pixel.blue(), pixel.green(), pixel.red(), pixel.alpha()])
        .collect();
    let surface =
        Surface::new(&argb, SIZE.0, SIZE.1, SIZE.0 * 4, Format::Argb8888).expect("surface");
    let mut canvas = Canvas::new(SIZE.0, SIZE.1).expect("canvas");
    let damage = Damage::full(SIZE.0, SIZE.1);
    canvas.composite(
        &surface,
        Rect::new(0, 0, i64::from(SIZE.0), i64::from(SIZE.1)),
        &damage,
    );
    canvas.data().to_vec()
}

fn tap(session: &mut Session, code: u32, keysym: &'static str, text: &'static str) {
    for pressed in [true, false] {
        let _ = session.key(
            &KeyPress {
                code,
                keysym,
                text,
                pressed,
                repeat: false,
                control: false,
                caps_lock: false,
                num_lock: false,
            },
            0,
        );
    }
}

/// The scene the guest's `hyprlock-gate` draws, with a session.
fn locked() -> (Scene, Session, System) {
    let config = config();
    let session = Session::new(true, config.general.fail_timeout, 0, 0);
    let screen_of = Screen {
        name: "Virtual-1".to_owned(),
        description: String::new(),
        width: SIZE.0,
        height: SIZE.1,
    };
    let scene = Scene::new(&config, &screen_of, &context(&session), 0);
    (scene, session, assets())
}

#[test]
fn the_locked_screen() {
    let (mut scene, session, mut assets) = locked();
    let frame = screen(&mut scene, &session, 0, &mut assets);
    golden::check_in(&data(), "hyprlock-locked", SIZE.0, SIZE.1, &frame);
}

#[test]
fn five_dots() {
    let (mut scene, mut session, mut assets) = locked();
    let _ = screen(&mut scene, &session, 0, &mut assets);
    for (code, letter) in [(17, "w"), (19, "r"), (24, "o"), (49, "n"), (34, "g")] {
        tap(&mut session, code, letter, letter);
    }
    let frame = screen(&mut scene, &session, 0, &mut assets);
    golden::check_in(&data(), "hyprlock-dots", SIZE.0, SIZE.1, &frame);
}

#[test]
fn a_wrong_password_s_screen() {
    let (mut scene, mut session, mut assets) = locked();
    let _ = screen(&mut scene, &session, 0, &mut assets);
    // "wrong", then Return: refused after PAM's delay.
    for (code, letter) in [(17, "w"), (19, "r"), (24, "o"), (49, "n"), (34, "g")] {
        tap(&mut session, code, letter, letter);
    }
    tap(&mut session, 28, "Return", "");
    session.answered(
        Verdict::Failed {
            text: REJECTED.to_owned(),
            retry_after_ms: FAIL_DELAY_MS,
        },
        0,
    );
    let _ = screen(&mut scene, &session, 0, &mut assets);
    let _ = session.tick(FAIL_DELAY_MS);
    let frame = screen(&mut scene, &session, FAIL_DELAY_MS, &mut assets);
    golden::check_in(&data(), "hyprlock-failed", SIZE.0, SIZE.1, &frame);
}
