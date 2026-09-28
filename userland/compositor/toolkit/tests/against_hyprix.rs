//! The runtime against the real compositor, in one process.
//!
//! `hyprix::run` serves a socket in this process, headless, writing each
//! frame it composes as a PPM; a client built on the toolkit connects to it
//! from a thread, does what a desktop client does, and the frame is looked
//! at. Nothing is mocked: the server is the one Ferrix boots.

#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a test's fixtures should fail loudly, and the workspace's ban is about the compositor"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use compositor_toolkit::tiny_skia;
use compositor_toolkit::{
    Anchor, ChildOutput, Client, Command, Event, KeyboardInteractivity, Layer, LayerOptions,
    ToplevelOptions,
};
use hyprix::{Options, Renderer};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

fn workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("toolkit-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a directory to work in");
    path
}

/// Run the compositor for `millis`, with `client` connected from a thread;
/// give back the client's answer and the last frame as RGB rows.
fn with_compositor<T: Send + 'static>(
    name: &str,
    millis: u64,
    client: impl FnOnce(&Path) -> T + Send + 'static,
) -> (T, Vec<u8>) {
    let work = workspace(name);
    let socket = work.join("wayland");
    let frames = work.join("frames");
    let config = work.join("hyprland.conf");
    std::fs::write(
        &config,
        "decoration:blur:noise = 0\nanimations:enabled = false\n",
    )
    .expect("a configuration");
    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        dump: Some(frames.clone()),
        deadline: Some(millis),
        config: Some(config),
        renderer: Renderer::Software,
        ..Options::default()
    };
    let path = socket.clone();
    let started = std::thread::spawn(move || {
        for _ in 0..400 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        client(&path)
    });
    let ran = hyprix::run(&options);
    let answer = started.join().expect("the client finished");
    let _ = ran.expect("the compositor ran");
    let frame = last_frame(&frames);
    let _ = std::fs::remove_dir_all(&work);
    (answer, frame)
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

/// Turn the loop until `wanted` says stop or `patience` runs out; every
/// event goes through `each`.
fn until(
    client: &mut Client,
    patience: Duration,
    mut each: impl FnMut(&mut Client, &Event) -> bool,
) -> Result<(), String> {
    let end = Instant::now() + patience;
    while Instant::now() < end {
        let events = client
            .dispatch(Some(Duration::from_millis(50)))
            .map_err(|error| error.to_string())?;
        for event in &events {
            if each(client, event) {
                return Ok(());
            }
        }
    }
    Err("ran out of patience".to_owned())
}

#[test]
fn a_bar_on_a_layer_surface_is_drawn_where_it_asked() {
    let (answer, frame) = with_compositor("bar", 3000, |socket| {
        let mut client = Client::connect_to(socket).map_err(|error| error.to_string())?;
        let outputs = client.outputs().len();
        let described = client
            .outputs()
            .first()
            .map(|output| (output.mode, output.name.clone()));
        let bar = client
            .layer_surface(&LayerOptions {
                layer: Layer::Top,
                namespace: "toolkit-test".to_owned(),
                size: (0, 40),
                anchor: Anchor::TOP.with(Anchor::LEFT).with(Anchor::RIGHT),
                exclusive_zone: 40,
                keyboard: KeyboardInteractivity::None,
                ..LayerOptions::default()
            })
            .map_err(|error| error.to_string())?;
        let mut size = None;
        until(&mut client, Duration::from_secs(2), |client, event| {
            if let Event::Configure {
                surface,
                width,
                height,
            } = event
                && *surface == bar
            {
                size = Some((*width, *height));
                let _ = client.draw(bar, |pixmap| {
                    pixmap.fill(tiny_skia::Color::from_rgba8(200, 30, 40, 255));
                });
                client.request_frame(bar);
            }
            matches!(event, Event::Frame { surface, .. } if *surface == bar)
        })?;
        // Kept connected until the compositor ends, so its last frame is
        // one drawn with the bar still there.
        let _ = until(&mut client, Duration::from_secs(5), |_, _| false);
        Ok::<_, String>((outputs, described, size))
    });
    let (outputs, described, size) = answer.expect("the client worked");
    assert_eq!(outputs, 1, "one headless screen");
    let (mode, _name) = described.expect("described");
    assert_eq!(mode, (WIDTH as i32, HEIGHT as i32));
    assert_eq!(size, Some((WIDTH, 40)), "stretched across by its anchors");
    assert_eq!(
        pixel(&frame, 10, 10),
        (200, 30, 40),
        "the bar is drawn at the top"
    );
    assert_eq!(pixel(&frame, WIDTH - 10, 39), (200, 30, 40));
    assert_ne!(pixel(&frame, 10, 45), (200, 30, 40), "and nowhere below it");
}

#[test]
fn a_window_is_tiled_by_the_compositor_and_drawn_there() {
    let (answer, frame) = with_compositor("toplevel", 3000, |socket| {
        let mut client = Client::connect_to(socket).map_err(|error| error.to_string())?;
        let window = client
            .toplevel(&ToplevelOptions {
                title: "toolkit test".to_owned(),
                app_id: "toolkit-test".to_owned(),
                size: (200, 100),
            })
            .map_err(|error| error.to_string())?;
        let mut size = None;
        until(&mut client, Duration::from_secs(2), |client, event| {
            if let Event::Configure {
                surface,
                width,
                height,
            } = event
                && *surface == window
            {
                size = Some((*width, *height));
                client.set_title(window, "toolkit test, retitled");
                let _ = client.draw(window, |pixmap| {
                    pixmap.fill(tiny_skia::Color::from_rgba8(30, 160, 90, 255));
                });
                client.request_frame(window);
            }
            matches!(event, Event::Frame { surface, .. } if *surface == window)
        })?;
        let _ = until(&mut client, Duration::from_secs(5), |_, _| false);
        Ok::<_, String>(size)
    });
    let (width, height) = answer.expect("the client worked").expect("configured");
    // A tiling compositor gives the one window the screen less its gaps,
    // not the 200x100 asked for.
    assert!(
        width > 200 && height > 100 && width <= WIDTH && height <= HEIGHT,
        "tiled to {width}x{height}"
    );
    assert_eq!(
        pixel(&frame, WIDTH / 2, HEIGHT / 2),
        (30, 160, 90),
        "the window is drawn where it was tiled"
    );
}

/// Lock, draw a colour on the lock surface, and either unlock or leave
/// the lock held when the client goes; the last frame and what the client
/// saw come back.
fn locked(name: &str, unlock: bool) -> (Option<(u32, u32)>, Vec<u8>) {
    let (answer, frame) = with_compositor(name, 3000, move |socket| {
        let mut client = Client::connect_to(socket).map_err(|error| error.to_string())?;
        client.lock().map_err(|error| error.to_string())?;
        let output = client
            .outputs()
            .first()
            .and_then(|output| output.id)
            .ok_or("no output")?;
        let surface = client
            .lock_surface(output)
            .map_err(|error| error.to_string())?;
        let mut locked = false;
        let mut configured = None;
        until(&mut client, Duration::from_secs(2), |client, event| {
            match event {
                Event::Configure {
                    surface: which,
                    width,
                    height,
                } if *which == surface => {
                    configured = Some((*width, *height));
                    let _ = client.draw(surface, |pixmap| {
                        pixmap.fill(tiny_skia::Color::from_rgba8(10, 120, 60, 255));
                    });
                }
                Event::Locked => locked = true,
                _ => {}
            }
            locked
        })?;
        let _ = until(&mut client, Duration::from_millis(400), |_, _| false);
        if unlock {
            client.unlock().map_err(|error| error.to_string())?;
        }
        // Kept connected until the compositor ends, so its last frame is
        // one drawn with this client still there.
        let _ = until(&mut client, Duration::from_secs(5), |_, _| false);
        Ok::<_, String>(configured)
    });
    (answer.expect("the client worked"), frame)
}

#[test]
fn the_lock_covers_the_screen() {
    let (configured, frame) = locked("lock", false);
    assert_eq!(configured, Some((WIDTH, HEIGHT)));
    assert_eq!(
        pixel(&frame, 320, 240),
        (10, 120, 60),
        "the lock is what is drawn"
    );
}

#[test]
fn an_unlock_gives_the_screen_back() {
    let (_, frame) = locked("unlock", true);
    assert_ne!(pixel(&frame, 320, 240), (10, 120, 60), "the lock came off");
}

#[test]
fn timers_children_and_signals_come_back_as_events() {
    let (answer, _) = with_compositor("loop", 2500, |socket| {
        let mut client = Client::connect_to(socket).map_err(|error| error.to_string())?;
        let started = Instant::now();
        let timer = client.add_timer(Duration::from_millis(100), None);
        let lines = client
            .run(&Command::new("echo one; echo two"))
            .map_err(|error| error.to_string())?;
        let mut whole = Command::new("printf 'a\\nb'; exit 3");
        whole.output = ChildOutput::Whole;
        let whole = client.run(&whole).map_err(|error| error.to_string())?;
        let signal = libc::SIGUSR1;
        client
            .watch_signals(&[signal])
            .map_err(|error| error.to_string())?;
        let waker = client.waker().map_err(|error| error.to_string())?;
        let mut seen = Vec::new();
        until(&mut client, Duration::from_secs(2), |_, event| {
            match event {
                Event::Timer(id) if *id == timer => {
                    // Not before it was due; how much after is the machine's.
                    let late = started.elapsed() >= Duration::from_millis(100);
                    seen.push(format!("timer, not early: {late}"));
                    #[expect(
                        unsafe_code,
                        reason = "AUDIT: raise a signal this thread has blocked for its signalfd"
                    )]
                    // SAFETY: raise is async-signal-safe and takes an integer.
                    let _ = unsafe { libc::raise(signal) };
                    waker.wake();
                }
                Event::ChildLine { child, line } if *child == lines => {
                    seen.push(format!("line {line}"));
                }
                Event::ChildExited {
                    child,
                    status,
                    output,
                } if *child == whole => {
                    seen.push(format!("whole {status:?} {output:?}"));
                }
                Event::ChildExited { child, status, .. } if *child == lines => {
                    seen.push(format!("lines done {status:?}"));
                }
                Event::Signal(number) if *number == signal => seen.push("signal".to_owned()),
                Event::Woken => seen.push("woken".to_owned()),
                _ => {}
            }
            seen.len() >= 7
        })?;
        Ok::<_, String>(seen)
    });
    let mut seen = answer.expect("the client worked");
    seen.sort();
    assert_eq!(
        seen,
        [
            "line one",
            "line two",
            "lines done Some(0)",
            "signal",
            "timer, not early: true",
            "whole Some(3) \"a\\nb\"",
            "woken",
        ]
    );
}

#[test]
fn a_tooltip_hangs_under_the_bar_it_belongs_to() {
    let (answer, frame) = with_compositor("popup", 3000, |socket| {
        let mut client = Client::connect_to(socket).map_err(|error| error.to_string())?;
        let bar = client
            .layer_surface(&LayerOptions {
                namespace: "waybar".to_owned(),
                size: (0, 40),
                anchor: Anchor::TOP.with(Anchor::LEFT).with(Anchor::RIGHT),
                exclusive_zone: 40,
                ..LayerOptions::default()
            })
            .map_err(|error| error.to_string())?;
        let mut tooltip = None;
        let mut placed = None;
        let _ = until(&mut client, Duration::from_secs(2), |client, event| {
            if let Event::Configure {
                surface,
                width,
                height,
            } = event
            {
                if *surface == bar {
                    fill(client, bar, (0, 0, 200));
                    tooltip = tooltip.or_else(|| open_tooltip(client, bar));
                } else if Some(*surface) == tooltip {
                    placed = Some((*width, *height));
                    fill(client, *surface, (0, 200, 0));
                }
            }
            false
        });
        // Kept connected until the compositor ends, so its last frame is
        // one drawn with the bar and its tooltip still there: a client that
        // has gone takes its surfaces with it.
        let _ = until(&mut client, Duration::from_secs(5), |_, _| false);
        Ok::<_, String>(placed)
    });
    let placed = answer.expect("the client worked");
    assert_eq!(
        placed,
        Some((100, 30)),
        "the popup was configured at its size"
    );
    // Centred under the anchor rectangle (anchor bottom, gravity bottom):
    // x 75..175, y 40..70.
    assert_eq!(
        pixel(&frame, 125, 55),
        (0, 200, 0),
        "the tooltip is under the bar"
    );
    assert_eq!(
        pixel(&frame, 300, 20),
        (0, 0, 200),
        "the bar is still there"
    );
}

fn fill(client: &mut Client, surface: compositor_toolkit::SurfaceId, (r, g, b): (u8, u8, u8)) {
    let _ = client.draw(surface, |pixmap| {
        pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
    });
}

fn open_tooltip(
    client: &mut Client,
    bar: compositor_toolkit::SurfaceId,
) -> Option<compositor_toolkit::SurfaceId> {
    use compositor_toolkit::{PopupOptions, Rect};
    client
        .popup(
            bar,
            &PopupOptions {
                size: (100, 30),
                anchor_rect: Rect {
                    x: 100,
                    y: 0,
                    width: 50,
                    height: 40,
                },
                ..PopupOptions::default()
            },
        )
        .ok()
}

#[test]
fn a_cursor_picture_is_taken_replaced_and_given_up_without_a_protocol_error() {
    let (answer, _) = with_compositor("cursor-image", 2000, |socket| {
        let mut client = Client::connect_to(socket).map_err(|error| error.to_string())?;
        let _window = client
            .toplevel(&ToplevelOptions {
                title: "cursor".to_owned(),
                app_id: "toolkit-test".to_owned(),
                size: (100, 100),
            })
            .map_err(|error| error.to_string())?;
        let arrow = [0xff_u8; 2 * 3 * 4];
        assert!(
            client.set_cursor_image(2, 2, (0, 0), &arrow).is_err(),
            "six pixels are not two by two"
        );
        client
            .set_cursor_image(2, 3, (1, 1), &arrow)
            .map_err(|error| error.to_string())?;
        client
            .set_cursor_image(3, 2, (0, 1), &arrow)
            .map_err(|error| error.to_string())?;
        let _ = client.roundtrip().map_err(|error| error.to_string())?;
        client.set_cursor(compositor_toolkit::CursorShape::default());
        client.roundtrip().map_err(|error| error.to_string())
    });
    let _ = answer.expect("the compositor took every request");
}
