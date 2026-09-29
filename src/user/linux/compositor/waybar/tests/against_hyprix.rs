//! The bar against the real compositor, in one process.
//!
//! `hyprix::run` serves a socket headless and writes each frame it
//! composes as a PPM; the bar connects from a thread with a config and a
//! stylesheet written here, runs its loop, and the last frame is looked at.
//! Text is drawn by a stand-in that fills the text's rectangle in the
//! text's colour, so where a label's text went -- and in what colour the
//! cascade gave it -- is in the picture.

#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a test's fixtures should fail loudly, and the workspace's ban is about the compositor"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use compositor_toolkit::Client;
use compositor_toolkit::tiny_skia::{self, Paint, PathBuilder, Pixmap, PixmapMut, Transform};
use compositor_waybar::app::App;
use compositor_waybar::css::Stylesheet;
use compositor_waybar::css::style::Style;
use compositor_waybar::diag::{Diagnostics, Level};
use compositor_waybar::json;
use compositor_waybar::layout::{Measure, TextSize};
use compositor_waybar::paint::{Images, Text, TextJob};
use hyprix::{Options, Renderer};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

/// Eight pixels a character, eighteen a line; text drawn as a block.
struct Blocks;

impl Measure for Blocks {
    fn text(&mut self, markup: &str, _: &Style, _: Option<f32>) -> TextSize {
        #[expect(clippy::cast_precision_loss, reason = "a test's character count")]
        let width = markup.chars().count() as f32 * 8.0;
        TextSize {
            width,
            height: 18.0,
            min_width: 8.0,
            char_width: 8.0,
        }
    }
}

impl Text for Blocks {
    fn draw(&mut self, pixmap: &mut PixmapMut<'_>, job: &TextJob<'_>, transform: Transform) {
        if job.markup.trim().is_empty() {
            return;
        }
        let [r, g, b, a] = job.color.unwrap_or(job.style.color).bytes();
        let mut paint = Paint::default();
        paint.set_color_rgba8(r, g, b, a);
        if let Some(rect) =
            tiny_skia::Rect::from_xywh(job.rect.x, job.rect.y, job.rect.width, job.rect.height)
        {
            pixmap.fill_path(
                &PathBuilder::from_rect(rect),
                &paint,
                tiny_skia::FillRule::Winding,
                transform,
                None,
            );
        }
    }
}

impl compositor_waybar::app::Engine for Blocks {}

struct NoImages;

impl Images for NoImages {
    fn get(&mut self, _: &Path, _: f32) -> Option<&Pixmap> {
        None
    }
}

fn workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("waybar-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a directory to work in");
    path
}

fn last_frame(directory: &Path) -> Vec<u8> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(directory)
        .expect("frames")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|kind| kind == "ppm"))
        .collect();
    names.sort();
    let bytes = std::fs::read(names.last().expect("a frame")).expect("a frame");
    let mut parts = bytes.splitn(4, |byte| *byte == b'\n');
    let _ = (parts.next(), parts.next(), parts.next());
    parts.next().expect("pixels").to_vec()
}

fn pixel(frame: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let at = ((y * WIDTH + x) * 3) as usize;
    (frame[at], frame[at + 1], frame[at + 2])
}

/// What a test's virtual pointer does, and when.
#[derive(Clone, Copy, Debug)]
enum Hand {
    /// Move to a point on the screen.
    To(u32, u32),
    /// Press and let go of the left button.
    Click,
}

/// A second client with a `zwlr_virtual_pointer_v1`, doing `moves` at
/// their times.
fn hand(socket: PathBuf, moves: Vec<(u64, Hand)>) -> std::thread::JoinHandle<()> {
    use compositor_toolkit::Value;
    use compositor_toolkit::protocol::virtual_pointer::{
        ZWLR_VIRTUAL_POINTER_MANAGER_V1, ZWLR_VIRTUAL_POINTER_V1, zwlr_virtual_pointer_manager_v1,
        zwlr_virtual_pointer_v1,
    };
    std::thread::spawn(move || {
        for _ in 0..400 {
            if socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let Ok(mut client) = Client::connect_to(&socket) else {
            return;
        };
        let Ok((manager, _)) = client.bind(&ZWLR_VIRTUAL_POINTER_MANAGER_V1, 2, None) else {
            return;
        };
        let pointer = client.new_object(&ZWLR_VIRTUAL_POINTER_V1, 2);
        let _ = client.request(
            manager,
            zwlr_virtual_pointer_manager_v1::request::CREATE_VIRTUAL_POINTER,
            &[
                Value::Object(compositor_toolkit::ObjectId::NULL),
                Value::NewId(pointer),
            ],
        );
        let _ = client.flush();
        let began = Instant::now();
        for (at, what) in moves {
            let due = began + Duration::from_millis(at);
            while Instant::now() < due {
                let _ = client.dispatch(Some(Duration::from_millis(20)));
            }
            let time = u32::try_from(began.elapsed().as_millis()).unwrap_or(0);
            let mut send = |opcode: u16, args: &[Value]| {
                let _ = client.request(pointer, opcode, args);
                let _ = client.request(pointer, zwlr_virtual_pointer_v1::request::FRAME, &[]);
            };
            match what {
                Hand::To(x, y) => send(
                    zwlr_virtual_pointer_v1::request::MOTION_ABSOLUTE,
                    &[
                        Value::Uint(time),
                        Value::Uint(x),
                        Value::Uint(y),
                        Value::Uint(WIDTH),
                        Value::Uint(HEIGHT),
                    ],
                ),
                Hand::Click => {
                    for state in [1, 0] {
                        send(
                            zwlr_virtual_pointer_v1::request::BUTTON,
                            &[Value::Uint(time), Value::Uint(0x110), Value::Uint(state)],
                        );
                    }
                }
            }
            let _ = client.flush();
        }
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            if client.dispatch(Some(Duration::from_millis(50))).is_err() {
                break;
            }
        }
    })
}

/// Run the compositor for `millis` with a bar on `config` and `style`,
/// sending each of `requests` to its control socket at its time; give back
/// the bar's log and the last frame.
fn with_bar(
    name: &str,
    millis: u64,
    config: &str,
    style: &str,
    moves: &[(u64, Hand)],
) -> (Vec<String>, Vec<u8>) {
    let work = workspace(name);
    let socket = work.join("wayland");
    let instance = work.join("hypr");
    std::fs::create_dir_all(&instance).expect("an instance directory");
    let frames = work.join("frames");
    let conf = work.join("hyprland.conf");
    std::fs::write(
        &conf,
        "decoration:blur:noise = 0\nanimations:enabled = false\n",
    )
    .expect("a configuration");
    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        dump: Some(frames.clone()),
        deadline: Some(millis),
        config: Some(conf),
        renderer: Renderer::Software,
        instance: Some(instance.to_string_lossy().into_owned()),
        ..Options::default()
    };
    let hands = hand(socket.clone(), moves.to_vec());
    let root = json::parse(config).expect("the test's config is JSON");
    let mut diag = Diagnostics::default();
    let sheet = Stylesheet::parse(style, &work.join("style.css"), &|_| None, &mut diag);
    assert!(
        diag.lines.is_empty(),
        "the test's style parses: {:?}",
        diag.lines
    );
    let path = socket.clone();
    let patience = Duration::from_millis(millis + 1000);
    let bar = std::thread::spawn(move || {
        for _ in 0..400 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let client = Client::connect_to(&path).map_err(|error| error.to_string())?;
        let mut app = App::new(
            client,
            root,
            sheet,
            Blocks,
            Box::new(NoImages),
            Level::Debug,
        )?;
        let end = Instant::now() + patience;
        let mut lines = Vec::new();
        while Instant::now() < end {
            if app.turn(Some(Duration::from_millis(50))).is_err() {
                break;
            }
            lines.extend(app.take_lines());
        }
        Ok::<_, String>(lines)
    });
    let ran = hyprix::run(&options);
    let _ = hands.join();
    let lines = bar.join().expect("the bar finished").expect("the bar ran");
    let _ = ran.expect("the compositor ran");
    let frame = last_frame(&frames);
    let _ = std::fs::remove_dir_all(&work);
    (lines, frame)
}

const STYLE: &str = r"
* { font-size: 15px; min-height: 0; }
window#waybar { background: #0000c8; color: #ffffff; }
#custom-a { background-color: #c80000; padding: 0 16px; margin: 4px 1px; color: #00c800; }
#custom-b { background-color: #c8c800; padding: 0 16px; margin: 4px 1px; color: #000000; }
.hidden-one { background-color: #ff00ff; }
";

#[test]
fn a_bar_is_drawn_with_its_modules_styled_and_laid_out() {
    let config = r#"{
        "layer": "top", "position": "top", "height": 40, "spacing": 6,
        "modules-left": ["custom/a"],
        "modules-right": ["custom/gone", "custom/b"],
        "custom/a": {"format": "abc"},
        "custom/gone": {"exec": "exit 3", "interval": "once"},
        "custom/b": {"exec": "echo hello", "interval": "once"}
    }"#;
    let (lines, frame) = with_bar("drawn", 2500, config, STYLE, &[]);
    let said = lines.join("\n");
    assert!(
        said.contains("Bar configured (width: 640, height: 40)"),
        "{said}"
    );
    // The bar's own ground, above and between the chips.
    assert_eq!(pixel(&frame, 300, 2), (0, 0, 200), "the bar's ground");
    // custom/a: 1 px margin, then 16 px of padding, then "abc" -- 24 px of
    // text -- then 16: the chip spans 1..57, 4..36.
    assert_eq!(pixel(&frame, 0, 20), (0, 0, 200), "the margin");
    assert_eq!(pixel(&frame, 2, 5), (200, 0, 0), "the chip");
    assert_eq!(pixel(&frame, 56, 35), (200, 0, 0), "the chip's far corner");
    assert_eq!(pixel(&frame, 57, 20), (0, 0, 200), "past it");
    // The text is centred in the 32 px of content: 18 tall at y 11.
    assert_eq!(pixel(&frame, 20, 15), (0, 200, 0), "the label's text");
    // custom/b sits at the right edge: "hello" is 40 px, so its chip is
    // 72 wide and ends 1 px short of 640.
    assert_eq!(pixel(&frame, 638, 20), (200, 200, 0), "the right chip");
    assert_eq!(pixel(&frame, 567, 20), (200, 200, 0), "its left edge");
    assert_eq!(pixel(&frame, 566, 20), (0, 0, 200), "its margin");
    // Below the bar, the compositor's own background.
    assert_ne!(pixel(&frame, 300, 60), (0, 0, 200), "the bar is 40 tall");
}

#[test]
fn the_pointer_prelights_a_module_and_its_tooltip_opens_below() {
    let config = r#"{
        "layer": "top", "height": 40,
        "modules-left": ["custom/a"],
        "custom/a": {"format": "abc", "tooltip-format": "tip", "on-click": "true"}
    }"#;
    let style = r"
* { font-size: 15px; min-height: 0; }
window#waybar { background: #0000c8; color: #ffffff; }
#custom-a { background-color: #c80000; padding: 0 16px; margin: 4px 1px; color: #00c800; }
#custom-a:hover { background-color: #00c8c8; }
tooltip { background-color: #c800c8; border: 2px solid #ffffff; }
tooltip label { color: #000000; }
";
    // The pointer onto the chip at 1 s; the tooltip is due 500 ms later.
    let (lines, frame) = with_bar("hover", 3000, config, style, &[(1000, Hand::To(20, 20))]);
    let said = lines.join("\n");
    assert_eq!(
        pixel(&frame, 2, 5),
        (0, 200, 200),
        ":hover restyled the chip; {said}"
    );
    // GTK's placement: under a 24 px square at the pointer less 4, centred:
    // the square's bottom middle is (28, 40). The tooltip is "tip", 24 px
    // of text in a 2 px border: 28 by 22, so x 14..42, y 40..62.
    assert_eq!(
        pixel(&frame, 15, 50),
        (255, 255, 255),
        "the tooltip's border; {said}"
    );
    assert_eq!(pixel(&frame, 28, 50), (0, 0, 0), "the tooltip's text");
    assert_eq!(pixel(&frame, 14, 40), (255, 255, 255), "its corner");
    assert_ne!(pixel(&frame, 13, 50), (255, 255, 255), "and no wider");
    assert_ne!(
        pixel(&frame, 28, 64),
        (200, 0, 200),
        "no taller than its text"
    );
}

#[test]
fn a_click_runs_on_click_and_a_json_script_restyles_its_chip() {
    let mark = std::env::temp_dir().join(format!("waybar-clicked-{}", std::process::id()));
    let _ = std::fs::remove_file(&mark);
    let config = format!(
        r#"{{
        "layer": "top", "height": 40,
        "modules-left": ["custom/a"],
        "custom/a": {{
            "exec": "for i in 1 2 3 4 5; do test -e {mark} && break; sleep 0.2; done; test -e {mark} && echo '{{\"text\": \"on\", \"class\": \"lit\"}}' || echo '{{\"text\": \"off\"}}'",
            "return-type": "json", "interval": "once",
            "on-click": "touch {mark}"
        }}
    }}"#,
        mark = mark.display()
    );
    let style = r"
* { font-size: 15px; min-height: 0; }
window#waybar { background: #0000c8; }
#custom-a { background-color: #c80000; padding: 0 16px; margin: 4px 1px; color: #00c800; }
#custom-a.lit { background-color: #c8c8c8; }
";
    // A click runs `on-click`, and -- as waybar's custom module does after
    // any click -- runs `exec` again, which now finds the mark. The two
    // start together, as upstream's do; the script waits up to a second for
    // `touch` to win, however loaded the machine.
    let (lines, frame) = with_bar(
        "click",
        5000,
        &config,
        style,
        &[(800, Hand::To(20, 20)), (1600, Hand::Click)],
    );
    let said = lines.join("\n");
    assert!(mark.exists(), "on-click ran; {said}");
    let _ = std::fs::remove_file(&mark);
    assert_eq!(
        pixel(&frame, 2, 5),
        (200, 200, 200),
        "the script's class restyled the chip; {said}"
    );
}
