//! The compositor, two clients and a screen, in one process.
//!
//! This is the compositor's everyday gate, and the headless half of stage
//! 18's exit in `docs/ROADMAP.md`: two pattern clients connect over a real
//! socket, are tiled by the dwindle layout, draw into shared memory, and the
//! frame the compositor composed is compared pixel for pixel against the
//! image `compositor/render`'s own tests bless.
//!
//! That the two agree is the point. `compositor/render`'s expected image is
//! built by calling the renderer directly with rectangles from
//! `compositor/layout`; this one is built by two programs talking Wayland
//! over a socket to a server that works out the same rectangles from the
//! requests they sent. Nothing but the pixels is shared between the two
//! paths, so a difference is a real one.
//!
//! Everything runs in threads of one process rather than as spawned
//! binaries, so there is no target directory to find and no orphan to leave
//! behind if an assertion fails.

// An integration test's helpers are not inside a `#[test]` function, so the
// workspace's ban on `expect` and `panic` -- which is about a compositor that
// must not take every client's windows down with it -- reaches them. Here a
// fixture that cannot be built should stop the test loudly.
#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test's fixtures should fail loudly, and the workspace's ban is about the compositor"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use compositor_pattern::Shape;
use compositor_render::Pattern;
use hyprix::Options;

/// The screen, which is the size `compositor/render`'s expected image is.
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;

/// Where `compositor/render` keeps the image both paths must produce.
const EXPECTED: &str = "../render/tests/data/dwindle-two-clients.xrle";

/// The same, with a bar across the top.
const BAR_EXPECTED: &str = "../render/tests/data/layer-bar-two-clients.xrle";

/// A directory of this test's own, named for the process so two runs at once
/// do not share one.
fn workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("hyprix-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a directory to work in");
    path
}

/// Run the compositor with the clients in `patterns`, and give back the last
/// frame it drew as `XRGB8888` rows.
fn run(name: &str, patterns: &[(Pattern, &str)]) -> (Vec<u8>, String) {
    let shaped: Vec<(Pattern, &str, Shape)> = patterns
        .iter()
        .map(|(pattern, title)| (*pattern, *title, Shape::Window))
        .collect();
    run_shaped(name, &shaped)
}

/// The same, with each client given the role it asks for: a window, or a bar
/// through `zwlr_layer_shell_v1`.
fn run_shaped(name: &str, patterns: &[(Pattern, &str, Shape)]) -> (Vec<u8>, String) {
    let work = workspace(name);
    let socket = work.join("wayland");
    let frames = work.join("frames");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        dump: Some(frames.clone()),
        deadline: Some(8000),
        ..Options::default()
    };

    let clients: Vec<_> = patterns
        .iter()
        .map(|(pattern, title, shape)| (*pattern, (*title).to_owned(), *shape))
        .collect();
    let socket_for_clients = socket.clone();
    let started = std::thread::spawn(move || {
        // The clients wait for the socket rather than racing it: the
        // compositor binds it before it accepts anything.
        for _ in 0..400 {
            if socket_for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut handles = Vec::new();
        for (pattern, title, shape) in clients {
            let path = socket_for_clients.clone();
            handles.push(std::thread::spawn(move || {
                // Each client is told where to connect the way any client is.
                connect(&path, pattern, &title, shape)
            }));
            // In order, so the dwindle tree is the one the expected image was
            // built from: the first window takes the whole area and the
            // second splits it.
            std::thread::sleep(Duration::from_millis(250));
        }
        let mut lines = Vec::new();
        for handle in handles {
            lines.push(
                handle
                    .join()
                    .unwrap_or_else(|_| "a client panicked".to_owned()),
            );
        }
        lines.join("; ")
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    let clients = started.join().expect("the clients finished");

    let last = last_frame(&frames);
    let _ = std::fs::remove_dir_all(&work);
    (last, format!("{line} | {clients}"))
}

/// One client, connected to `socket`.
fn connect(socket: &Path, pattern: Pattern, title: &str, shape: Shape) -> String {
    // `WAYLAND_DISPLAY` is how every client is told, but the environment is
    // one per process and these clients share it, so the path goes in
    // directly. `socket_path` takes a name with a slash as an absolute path,
    // which is what this is.
    compositor_pattern::client::run_shaped_on(socket, pattern, title, shape)
        .unwrap_or_else(|error| format!("pattern failed: {error}"))
}

/// The last PPM in `directory`, as `XRGB8888`-order bytes.
fn last_frame(directory: &Path) -> Vec<u8> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(directory)
        .expect("the compositor wrote frames")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|kind| kind == "ppm"))
        .collect();
    names.sort();
    let last = names.last().expect("at least one frame");
    let bytes = std::fs::read(last).expect("a frame");
    let mut parts = bytes.splitn(4, |byte| *byte == b'\n');
    assert_eq!(parts.next(), Some(&b"P6"[..]));
    let size: Vec<u32> = core::str::from_utf8(parts.next().expect("a size"))
        .expect("utf-8")
        .split_whitespace()
        .map(|value| value.parse().expect("a number"))
        .collect();
    assert_eq!(size, [WIDTH, HEIGHT], "the frame is the screen's size");
    assert_eq!(parts.next(), Some(&b"255"[..]));
    parts.next().expect("pixels").to_vec()
}

/// `compositor/render`'s expected image, as the same `(red, green, blue)`
/// bytes a PPM holds.
fn expected() -> Vec<u8> {
    image(&Path::new(env!("CARGO_MANIFEST_DIR")).join(EXPECTED))
}

/// One expected image, as the same `(red, green, blue)` bytes a PPM holds.
fn image(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let word = |at: usize| -> u32 {
        let slice: [u8; 4] = bytes
            .get(at..at + 4)
            .and_then(|slice| slice.try_into().ok())
            .expect("in range");
        u32::from_le_bytes(slice)
    };
    assert_eq!(bytes.get(..16), Some(&b"ferrix-xrgb-rle\n"[..]));
    let (width, height) = (word(16), word(20));
    assert_eq!((width, height), (WIDTH, HEIGHT));

    let mut out = Vec::with_capacity((width * height * 3) as usize);
    let mut at = 24;
    let mut previous: Vec<u8> = Vec::new();
    for _ in 0..height {
        let tag = *bytes.get(at).expect("a row tag");
        at += 1;
        if tag == 0 {
            out.extend_from_slice(&previous);
            continue;
        }
        let mut row = Vec::with_capacity((width * 3) as usize);
        while row.len() < (width * 3) as usize {
            let count = u16::from_le_bytes([
                *bytes.get(at).expect("a count"),
                *bytes.get(at + 1).expect("a count"),
            ]);
            let pixel = word(at + 2);
            at += 6;
            for _ in 0..count {
                row.extend_from_slice(&[
                    ((pixel >> 16) & 0xFF) as u8,
                    ((pixel >> 8) & 0xFF) as u8,
                    (pixel & 0xFF) as u8,
                ]);
            }
        }
        out.extend_from_slice(&row);
        previous = row;
    }
    assert_eq!(at, bytes.len(), "the expected image has bytes left over");
    out
}

/// One differing pixel: where it is, what it should have been, what it was.
type Difference = (u32, u32, [u8; 3], [u8; 3]);

/// Where two images differ: how many pixels, and the first of them.
fn compare(got: &[u8], want: &[u8]) -> (usize, Option<Difference>) {
    let mut differing = 0;
    let mut first = None;
    for index in 0..(WIDTH * HEIGHT) as usize {
        let at = index * 3;
        let (Some(mine), Some(theirs)) = (got.get(at..at + 3), want.get(at..at + 3)) else {
            break;
        };
        if mine != theirs {
            differing += 1;
            if first.is_none() {
                let x = (index as u32) % WIDTH;
                let y = (index as u32) / WIDTH;
                first = Some((
                    x,
                    y,
                    [theirs[0], theirs[1], theirs[2]],
                    [mine[0], mine[1], mine[2]],
                ));
            }
        }
    }
    (differing, first)
}

#[test]
fn two_clients_are_tiled_and_drawn_exactly_as_the_renderer_says() {
    let (frame, report) = run(
        "tiled",
        &[(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")],
    );
    assert!(
        report.contains("most 2"),
        "the compositor should have had two windows: {report}"
    );
    assert!(
        !report.contains("failed"),
        "a client did not get its window: {report}"
    );

    let (differing, first) = compare(&frame, &expected());
    assert_eq!(
        differing, 0,
        "the compositor drew something else than the renderer's expected image; \
         first difference {first:?}"
    );
}

#[test]
fn the_comparison_would_notice_a_different_frame() {
    // The check above is only worth having if it fails when the picture is
    // wrong. One client rather than two is a different picture, and the
    // comparison must say so.
    let (frame, report) = run("one-client", &[(Pattern::Checkerboard, "only")]);
    assert!(report.contains("most 1"), "{report}");
    let (differing, _) = compare(&frame, &expected());
    assert!(
        differing > 0,
        "one window drew the same picture as two, which cannot be right"
    );
}

// ---------------------------------------------------------------------------
// A real toolkit
//
// The test above proves that this tree's two halves agree with each other.
// `probe/real-client.sh` proves the other thing: that an application written
// against libwayland and every other compositor, which knows nothing about
// this one, gets a window and draws in it. Every gap it found -- the
// clipboard's objects, subsurfaces, an output that described itself -- was
// one the pattern client could never have found, because the pattern client
// is written against the same crates the server is.
// ---------------------------------------------------------------------------

/// What `probe/real-client.sh` recorded.
const REAL_CLIENT: &str = include_str!("../probe/real-client.txt");

/// A `frame <name> <value>` line from the record.
fn frame_value(name: &str) -> Option<&'static str> {
    REAL_CLIENT
        .lines()
        .find_map(|line| line.strip_prefix(&format!("frame {name} ")))
}

#[test]
fn a_real_toolkit_gets_a_window_and_draws_in_it() {
    assert!(
        REAL_CLIENT
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("# foot ")),
        "probe/real-client.txt should say which client wrote it"
    );

    // It read the output's mode. A compositor whose wl_output describes
    // nothing gets `(null): 0x0+0x0@0Hz` here, which is what the first run
    // printed.
    assert!(
        REAL_CLIENT.contains("HEADLESS-1: 1024x768+0x0@60Hz hyprix"),
        "the client did not read the output's mode:\n{REAL_CLIENT}"
    );
    // It worked out its own geometry from it, which means the mode was
    // usable and not merely present.
    assert!(
        REAL_CLIENT.contains("cell width=10, height=19"),
        "the client did not lay out its terminal"
    );
    // And it ended by choice rather than by a protocol error.
    assert!(
        REAL_CLIENT.contains("client info: main.c:696: goodbye"),
        "the client did not exit cleanly"
    );
    assert!(
        !REAL_CLIENT.contains("Protocol error"),
        "the compositor refused a real client:\n{REAL_CLIENT}"
    );
    assert!(
        !REAL_CLIENT.contains("Broken pipe"),
        "the compositor went away before the client did"
    );
    assert!(
        REAL_CLIENT.contains("most 1"),
        "the compositor never gave the client a window"
    );

    // And it drew: most of the screen is its window rather than the
    // compositor's background.
    assert_eq!(frame_value("size"), Some("1024x768"));
    let drawn: u64 = frame_value("not-background")
        .and_then(|value| value.parse().ok())
        .expect("a count of drawn pixels");
    assert!(
        drawn > 600_000,
        "only {drawn} pixels of 786432 were the client's window"
    );
    let colours: u32 = frame_value("colours")
        .and_then(|value| value.parse().ok())
        .expect("a count of colours");
    assert!(colours > 1, "the frame is one flat colour, so nothing drew");
}

// ---------------------------------------------------------------------------
// Hyprland's own client
//
// `compositor/ipc`'s tests check the answers against Hyprland's source.
// `probe/hyprctl.sh` checks them against Hyprland's client: the program people
// type, and the one every script and bar is written around. If `hyprctl
// clients` prints nothing here, no Hyprland script works on this compositor
// whatever the JSON says.
// ---------------------------------------------------------------------------

/// What `probe/hyprctl.sh` recorded.
const HYPRCTL: &str = include_str!("../probe/hyprctl.txt");

/// What `<client> <command>` printed, up to the next command of either
/// client's.
fn recorded(client: &str, command: &str) -> &'static str {
    let marker = format!("\n$ {client} {command}\n");
    let start = HYPRCTL
        .find(&marker)
        .unwrap_or_else(|| panic!("{client} {command:?} is not in probe/hyprctl.txt"))
        + marker.len();
    let rest = HYPRCTL.get(start..).unwrap_or("");
    let end = rest.find("\n$ ").unwrap_or(rest.len());
    rest.get(..end).unwrap_or("").trim_end()
}

/// What Hyprland's own `hyprctl <command>` printed.
fn hyprctl(command: &str) -> &'static str {
    recorded("hyprctl", command)
}

/// What `compositor/ctl`'s `hyprctl <command>` printed.
fn ours(command: &str) -> &'static str {
    recorded("ours", command)
}

/// Every read-only command the probe ran through both clients must have got
/// one answer.
///
/// That is the whole claim `compositor/ctl` makes: a script written for
/// `hyprctl` works when the program it calls is ours, which is what Ferrix's
/// image carries because Hyprland's is not on it. The two ran against one
/// compositor in one session, so even the window addresses are comparable.
#[test]
fn our_client_and_hyprlands_get_one_answer() {
    for command in [
        "version",
        "monitors",
        "workspaces",
        "activewindow",
        "-j activewindow",
        "clients",
        "-j clients",
        "nonsense",
    ] {
        assert_eq!(
            ours(command),
            hyprctl(command),
            "`{command}` was answered differently"
        );
        assert!(!ours(command).is_empty(), "`{command}` answered nothing");
    }
}

#[test]
fn hyprlands_own_client_reads_this_compositor() {
    // The commands a person types and a script calls.
    assert!(
        hyprctl("version").contains("hyprix"),
        "version said nothing"
    );
    assert!(
        hyprctl("monitors").contains("Monitor HEADLESS-1 (ID 0):"),
        "monitors: {}",
        hyprctl("monitors")
    );
    assert!(
        hyprctl("monitors").contains("1024x768@60.00000 at 0x0"),
        "the mode is not in Hyprland's own shape"
    );
    assert!(
        hyprctl("workspaces").contains("workspace ID 1 (1) on monitor HEADLESS-1:"),
        "workspaces: {}",
        hyprctl("workspaces")
    );
    assert!(
        hyprctl("workspaces").contains("windows: 2"),
        "both windows should be on the workspace"
    );

    // Both clients are there, with the titles and the app id they set.
    let clients = hyprctl("clients");
    assert!(clients.contains("title: one"), "clients: {clients}");
    assert!(clients.contains("title: two"), "clients: {clients}");
    assert!(clients.contains("class: rocks.magical.pattern"));
}

#[test]
fn a_dispatcher_typed_at_hyprctl_moves_the_focus() {
    // The focus starts on the window opened last, and `movefocus l` is what
    // every Hyprland configuration binds.
    assert!(
        hyprctl("activewindow").contains("title: two"),
        "the focus should start on the second window"
    );
    assert_eq!(hyprctl("dispatch movefocus l").trim(), "ok");
    // The second `activewindow` in the record is after the dispatch.
    let after = HYPRCTL
        .rsplit_once("\n$ hyprctl activewindow\n")
        .map(|(_, rest)| rest)
        .expect("a second activewindow");
    assert!(
        after.contains("title: one"),
        "movefocus did not move the focus:\n{after}"
    );
}

#[test]
fn an_option_changed_at_hyprctl_re_tiles_every_window() {
    assert_eq!(hyprctl("keyword general:gaps_in 40").trim(), "ok");
    // Before: 485 wide with the default gaps. After: 450, and both windows
    // moved. A compositor that took the keyword and did not re-tile would
    // still say `ok`.
    let before = hyprctl("-j activewindow");
    assert!(before.contains("\"size\": [485, 726]"), "before: {before}");
    // The `-j clients` the probe ran after the keyword, which is the only
    // one: the ones before it were `activewindow`.
    let after = hyprctl("-j clients");
    assert!(
        after.contains("\"size\": [450, 726]"),
        "the windows were not re-tiled:\n{after}"
    );
    assert_eq!(
        after.matches("\"size\": [450, 726]").count(),
        2,
        "both windows should have been re-tiled"
    );
}

#[test]
fn an_unknown_request_is_answered_rather_than_hanging_hyprctl() {
    // Hyprland answers a line and keeps the connection. A compositor that
    // closed it instead leaves `hyprctl` with nothing to print, which is what
    // a person sees as a hang.
    assert!(
        hyprctl("nonsense").contains("unknown request nonsense"),
        "{}",
        hyprctl("nonsense")
    );
}

// ---------------------------------------------------------------------------
// The event socket
//
// `.socket2.sock` is what a bar reads. `compositor/ipc`'s tests check each
// line's shape against Hyprland's own `postEvent` calls; this checks that the
// compositor puts them on a socket a reader can get at, in the order a reader
// needs, while two clients come and go.
// ---------------------------------------------------------------------------

/// Run the compositor with a subscriber on its event socket, and give back
/// every line the subscriber read.
fn subscribed(name: &str, kill: bool) -> Vec<String> {
    let work = workspace(name);
    let socket = work.join("wayland");
    let runtime = work.join("runtime");
    std::fs::create_dir_all(&runtime).expect("a runtime directory");
    // The instance is given as a directory rather than a name, so this test
    // needs no `XDG_RUNTIME_DIR`: an environment variable is one per process
    // and the tests run in threads of one.
    let instance = runtime.join("hypr").join("ferrix-test");
    let events = instance.join(compositor_ipc::EVENT_SOCKET);
    let requests = instance.join(compositor_ipc::REQUEST_SOCKET);

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        instance: Some(instance.to_string_lossy().into_owned()),
        deadline: Some(4000),
        ..Options::default()
    };

    let listening = events.clone();
    let reader = std::thread::spawn(move || {
        for _ in 0..800 {
            if listening.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut lines = Vec::new();
        let _ = compositor_ctl::subscribe(&listening, &mut |line| lines.push(line.to_owned()));
        lines
    });

    let socket_for_clients = socket.clone();
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if socket_for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // The subscriber has to be connected before the first window, or it
        // is told the state instead of being told the window arrived.
        std::thread::sleep(Duration::from_millis(400));
        let mut handles = Vec::new();
        for (pattern, title) in [(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")] {
            let path = socket_for_clients.clone();
            handles.push(std::thread::spawn(move || {
                connect(&path, pattern, title, Shape::Window)
            }));
            std::thread::sleep(Duration::from_millis(250));
        }
        // `killactive` through the request socket, so the window really
        // closes and the socket really says so: the clients themselves run
        // until the compositor goes, as a window does.
        if kill {
            std::thread::sleep(Duration::from_millis(500));
            let _ = compositor_ctl::ask(&requests, "dispatch killactive");
        }
        for handle in handles {
            let _ = handle.join();
        }
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    clients.join().expect("the clients finished");
    let lines = reader.join().expect("the subscriber finished");
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        line.contains("subscribers 1"),
        "the compositor saw no subscriber: {line}"
    );
    lines
}

#[test]
fn a_bar_on_the_event_socket_is_told_what_happens() {
    let lines = subscribed("events", false);
    assert!(!lines.is_empty(), "the subscriber read nothing");

    // What already existed when it connected: the monitor and the workspace.
    assert!(
        lines.iter().any(|line| line == "monitoradded>>HEADLESS-1"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line == "createworkspacev2>>1,1"),
        "{lines:?}"
    );

    // Then each window arriving, with its class and title.
    let opened: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("openwindow>>"))
        .collect();
    assert_eq!(opened.len(), 2, "{lines:?}");
    assert!(
        opened[0].ends_with(",1,rocks.magical.pattern,one"),
        "{:?}",
        opened[0]
    );
    assert!(
        opened[1].ends_with(",1,rocks.magical.pattern,two"),
        "{:?}",
        opened[1]
    );

    // And the focus moving onto each, in both shapes.
    assert!(
        lines
            .iter()
            .any(|line| line == "activewindow>>rocks.magical.pattern,two"),
        "{lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("activewindowv2>>"))
            .count(),
        2,
        "{lines:?}"
    );

    // Every line is one line and holds no newline of its own, which is what
    // `formatEvent` promises a reader.
    for line in &lines {
        assert!(!line.contains('\n'), "{line:?}");
        assert!(line.contains(">>"), "{line:?}");
    }
}

/// A bar that is not told a window closed shows a window that is gone.
///
/// The window is closed by `hyprctl dispatch killactive` on the request
/// socket, which is how a person closes one: the clients themselves run
/// until the compositor goes.
#[test]
fn a_window_closing_reaches_the_socket_too() {
    let lines = subscribed("events-closing", true);
    assert!(
        lines.iter().any(|line| line.starts_with("closewindow>>")),
        "nothing said a window closed: {lines:?}"
    );
    // And the focus moved to the one that is left, rather than being left on
    // a window that no longer exists.
    let focused: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("activewindow>>"))
        .collect();
    assert!(focused.len() >= 3, "{lines:?}");
}

/// A bar through `zwlr_layer_shell_v1`, and the windows tiling under it.
///
/// This is the whole of what a Hyprland setup needs before it will start:
/// `waybar` is a layer surface, `hyprpaper` is a layer surface, and a
/// compositor that does not place them puts the windows over the bar or
/// under it.
#[test]
fn a_bar_takes_its_strip_and_the_windows_tile_under_it() {
    let (frame, report) = run_shaped(
        "bar",
        &[
            (Pattern::Checkerboard, "bar", Shape::Bar(30)),
            (Pattern::Checkerboard, "one", Shape::Window),
            (Pattern::Gradient, "two", Shape::Window),
        ],
    );
    assert!(
        report.contains("most 2"),
        "the bar should not be a window: {report}"
    );
    assert!(!report.contains("failed"), "{report}");

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BAR_EXPECTED);
    let (differing, first) = compare(&frame, &image(&path));
    assert_eq!(
        differing, 0,
        "the compositor drew something else than the renderer's expected image; \
         first difference {first:?}"
    );
}

// ---------------------------------------------------------------------------
// Animations
//
// `compositor/anim`'s tests check the curves against the numbers hyprutils'
// own algorithm produces. This checks that a window really slides: that the
// frames between two layouts hold the window at places neither layout put it,
// and that those places are on the curve rather than a straight line.
// ---------------------------------------------------------------------------

/// Run the compositor with two clients and a configuration, dispatching
/// `after` through the control socket once both windows are up, and give
/// back every frame it drew.
///
/// The deadline is wall-clock and the dispatch is a second into the run, so
/// it has to leave room for the whole animation after that: a run that ends
/// mid-slide gives a test the first part of the curve and nothing else,
/// which under a loaded machine is what a four-second deadline did.
fn frames_after(name: &str, config: &str, after: &str) -> Vec<Vec<u8>> {
    let work = workspace(name);
    let socket = work.join("wayland");
    let frames = work.join("frames");
    let runtime = work.join("runtime");
    let instance = runtime.join("hypr").join("ferrix-test");
    std::fs::create_dir_all(&instance).expect("an instance directory");
    let requests = instance.join(compositor_ipc::REQUEST_SOCKET);
    let config_path = work.join("hyprland.conf");
    std::fs::write(&config_path, config).expect("a configuration");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        instance: Some(instance.to_string_lossy().into_owned()),
        config: Some(config_path),
        dump: Some(frames.clone()),
        deadline: Some(4000),
        ..Options::default()
    };

    let socket_for_clients = socket.clone();
    let after = after.to_owned();
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if socket_for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut handles = Vec::new();
        for (pattern, title) in [(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")] {
            let path = socket_for_clients.clone();
            handles.push(std::thread::spawn(move || {
                connect(&path, pattern, title, Shape::Window)
            }));
            std::thread::sleep(Duration::from_millis(250));
        }
        // Both windows are up and settled; now make them move.
        std::thread::sleep(Duration::from_millis(900));
        let _ = compositor_ctl::ask(&requests, &after);
        for handle in handles {
            let _ = handle.join();
        }
    });

    let _ = hyprix::run(&options).expect("the compositor ran");
    clients.join().expect("the clients finished");

    let mut names: Vec<PathBuf> = std::fs::read_dir(&frames)
        .expect("the compositor wrote frames")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|kind| kind == "ppm"))
        .collect();
    names.sort();
    let drawn = names
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).expect("a frame");
            let mut parts = bytes.splitn(4, |byte| *byte == b'\n');
            let _ = parts.next();
            let _ = parts.next();
            let _ = parts.next();
            parts.next().expect("pixels").to_vec()
        })
        .collect();
    let _ = std::fs::remove_dir_all(&work);
    drawn
}

/// Where the gradient window's left edge is on a row, once both windows are
/// on the screen.
///
/// The gradient is the only thing on the screen whose pixels are none of the
/// colours everything else is: the compositor's background, the
/// checkerboard's two greys and the two border colours. So the first column
/// that is none of them is its left edge, wherever it is -- including while
/// it is drawn scaled, part-way through a move, where a measure that looked
/// for the gap between two windows finds several.
///
/// `None` until the row holds both patterns, which is the frames before the
/// second client has drawn: a screen with one window on it says nothing
/// about where two of them are.
fn gradient_edge(frame: &[u8], row: usize) -> Option<usize> {
    let checkerboard: [[u8; 3]; 2] = [[0xE0, 0xE0, 0xE0], [0x30, 0x30, 0x30]];
    let others: [[u8; 3]; 3] = [[0x11, 0x11, 0x11], [0xFF, 0xFF, 0xFF], [0x44, 0x44, 0x44]];
    let pixel = |x: usize| -> Option<&[u8]> {
        let at = (row * WIDTH as usize + x) * 3;
        frame.get(at..at + 3)
    };
    let is_gradient = |x: usize| {
        pixel(x).is_some_and(|colour| {
            !checkerboard.iter().any(|one| one == colour) && !others.iter().any(|one| one == colour)
        })
    };
    let has_checkerboard = (0..WIDTH as usize)
        .any(|x| pixel(x).is_some_and(|colour| checkerboard.iter().any(|one| one == colour)));
    if !has_checkerboard {
        return None;
    }
    let edge = (0..WIDTH as usize).find(|x| is_gradient(*x))?;
    Some(edge)
}

/// The edges from the moment the two windows were settled in their first
/// arrangement, which is the first frame the moving window is at its
/// right-hand place.
///
/// The frames before it are the two clients arriving: a window whose client
/// has not redrawn at the size it was just configured to is drawn stretched,
/// and a stretched checkerboard has greys between its two, which the edge
/// measure cannot tell from the gradient. Nothing about the move is in those
/// frames.
fn once_settled(edges: &[usize]) -> &[usize] {
    let Some(&start) = edges.iter().max() else {
        return &[];
    };
    let at = edges.iter().position(|edge| *edge == start).unwrap_or(0);
    edges.get(at..).unwrap_or(&[])
}

/// A window swapped with its neighbour slides there, and the frames on the
/// way hold it at places neither layout put it.
#[test]
fn a_window_moves_through_the_frames_between_two_layouts() {
    let frames = frames_after(
        "animated",
        // Two seconds rather than Hyprland's default 0.8, and the blur off.
        // What is measured here is where the window was in each frame that
        // was drawn, and the compositor draws a handful of them a second in
        // a debug build: a short slide is sampled two or three times, and
        // the first sample is already most of the way there. A longer one
        // is the same curve with more points on it.
        "animation = windows, 1, 20, default\ndecoration:blur:enabled = 0\n",
        "dispatch movewindow l",
    );
    assert!(frames.len() > 10, "only {} frames", frames.len());

    // The upper quarter, which is the gradient's opaque half.
    let row = HEIGHT as usize / 4;
    let measured: Vec<usize> = frames
        .iter()
        .filter_map(|frame| gradient_edge(frame, row))
        .collect();
    let seams = once_settled(&measured).to_vec();
    assert!(seams.len() > 5, "the window was never found: {seams:?}");

    // A swap moves the seam, and the frames in between hold it somewhere
    // else again: with no animation there would be two positions and no
    // more.
    let mut distinct: Vec<usize> = seams.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() > 3,
        "the seam only ever had {} positions, so nothing slid: {distinct:?}",
        distinct.len()
    );

    // It slid one way and did not wander: the seam never goes backwards
    // once it has started, which a window drawn from a stale rectangle
    // would.
    let start = seams.first().copied();
    let moving: Vec<usize> = seams
        .iter()
        .copied()
        .skip_while(|at| Some(*at) == start)
        .collect();
    let forwards = moving.windows(2).all(|pair| pair[0] <= pair[1]);
    let backwards = moving.windows(2).all(|pair| pair[0] >= pair[1]);
    assert!(forwards || backwards, "the seam wandered: {moving:?}");

    // And it is on a curve rather than a straight line. Hyprland's
    // `default` starts fast: by the middle frame of the move it is well
    // past half way, which `compositor/anim`'s own test puts at 0.843 of
    // the distance a quarter of the way in.
    let (Some(first), Some(last)) = (seams.first().copied(), seams.last().copied()) else {
        panic!("no seam at all");
    };
    let span = last.abs_diff(first);
    // Fifty pixels rather than the whole slide: what is measured is the
    // leading edge of the moving window in the frames that were *drawn*,
    // and the first of those is already some way along a curve that starts
    // fast. The distance that proves a slide is one no border, gap or
    // rounding could account for, and this is four times the widest of
    // them.
    assert!(span > 50, "the seam moved only {span} pixels: {seams:?}");
    let middle = seams.get(seams.len() / 2).copied().expect("a middle frame");
    let covered = middle.abs_diff(first);
    assert!(
        covered * 2 > span,
        "half way through the move it had covered {covered} of {span}, which is not a curve \
         that starts fast"
    );
}

/// With `animations:enabled = 0` the same swap is instant: the seam has the
/// two positions the two layouts give it and nothing between.
#[test]
fn animations_can_be_turned_off() {
    let frames = frames_after(
        "instant",
        "animations:enabled = 0\ndecoration:blur:enabled = 0\n",
        "dispatch movewindow l",
    );
    let row = HEIGHT as usize / 4;
    let measured: Vec<usize> = frames
        .iter()
        .filter_map(|frame| gradient_edge(frame, row))
        .collect();
    let order = once_settled(&measured).to_vec();
    let mut distinct: Vec<usize> = order.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() <= 2,
        "with animations off the window was at {} places: {order:?}",
        distinct.len()
    );
}

// ---------------------------------------------------------------------------
// Plugins
//
// A plugin is a program the compositor starts, which connects to the control
// socket, says what it is, and adds a dispatcher. `compositor/plug` is the
// one Ferrix carries; this speaks the same protocol from a thread, which is
// what lets a host test press the dispatcher and look at the frame.
// ---------------------------------------------------------------------------

/// Run the compositor with two clients and a plugin, and give back the last
/// frame, the compositor's line, and what `hyprctl plugin list` said.
fn with_a_plugin(name: &str) -> (Vec<u8>, String, String) {
    let work = workspace(name);
    let socket = work.join("wayland");
    let frames = work.join("frames");
    let runtime = work.join("runtime");
    let instance = runtime.join("hypr").join("ferrix-test");
    std::fs::create_dir_all(&instance).expect("an instance directory");
    let requests = instance.join(compositor_ipc::REQUEST_SOCKET);

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        instance: Some(instance.to_string_lossy().into_owned()),
        dump: Some(frames.clone()),
        deadline: Some(8000),
        ..Options::default()
    };

    let socket_for_clients = socket.clone();
    let listed = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let said = std::sync::Arc::clone(&listed);
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if socket_for_clients.exists() && requests.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut handles = Vec::new();
        for (pattern, title) in [(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")] {
            let path = socket_for_clients.clone();
            handles.push(std::thread::spawn(move || {
                connect(&path, pattern, title, Shape::Window)
            }));
            std::thread::sleep(Duration::from_millis(250));
        }
        // The plugin: the protocol `compositor/plug` speaks, from here.
        let plugin = std::os::unix::net::UnixStream::connect(&requests)
            .expect("the plugin connects to the control socket");
        let mut reading = plugin.try_clone().expect("the plugin's connection");
        let mut writing = plugin;
        let mut say = move |line: &str| {
            use std::io::Write as _;
            writing
                .write_all(line.as_bytes())
                .expect("the plugin writes");
            writing.flush().expect("the plugin flushes");
        };
        say("[[PLUGIN]]swap,ferrix,1.0,swaps the focused window with its neighbour\n");
        say("handle swapthem\n");
        std::thread::sleep(Duration::from_millis(500));

        // `hyprctl plugin list` sees it, and `hyprctl dispatch swapthem`
        // reaches it: a dispatcher the layout has never heard of.
        if let Ok(text) = compositor_ctl::ask(&requests, "plugin list") {
            said.lock().expect("the answer").push_str(&text);
        }
        let _ = compositor_ctl::ask(&requests, "dispatch swapthem");

        // The plugin answers `dispatch movewindow r`, which the compositor
        // runs on its next pass; this reads the line and sends it, as the
        // program does.
        let mut reply = String::new();
        {
            use std::io::Read as _;
            reading
                .set_read_timeout(Some(Duration::from_millis(500)))
                .expect("a read timeout");
            // Until the dispatch arrives: the two `ok`s for the hello and
            // the registration come first, and each may be a read of its
            // own.
            for _ in 0..8 {
                let mut buffer = [0u8; 512];
                match reading.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        reply.push_str(&String::from_utf8_lossy(buffer.get(..read).unwrap_or(&[])));
                    }
                    Err(_) => {}
                }
                if reply.contains("dispatch>>") {
                    break;
                }
            }
        }
        if reply.contains("dispatch>>swapthem") {
            // What `compositor/plug` sends: the two dispatchers that
            // exchange the windows, as one batch.
            say("[[BATCH]]dispatch movefocus l ; dispatch movewindow r\n");
        }
        std::thread::sleep(Duration::from_millis(500));

        let mut lines = vec![format!("plugin heard {}", reply.trim())];
        for handle in handles {
            lines.push(
                handle
                    .join()
                    .unwrap_or_else(|_| "a client panicked".to_owned()),
            );
        }
        lines.join("; ")
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    let clients = clients.join().expect("the clients finished");
    let frame = last_frame(&frames);
    let listed = listed.lock().expect("the answer").clone();
    (frame, format!("{line} | {clients}"), listed)
}

/// A plugin adds a dispatcher, the compositor hands it the dispatch, and what
/// the plugin asks for in return is what the screen shows.
#[test]
fn a_plugin_adds_a_dispatcher_and_the_compositor_hands_it_over() {
    let (frame, report, listed) = with_a_plugin("plugin");
    assert!(!report.contains("a client panicked"), "{report}");

    // `hyprctl plugin list` names it, in Hyprland's own shape.
    assert!(listed.contains("Plugin swap by ferrix:"), "{listed}");
    assert!(listed.contains("Dispatchers: swapthem"), "{listed}");

    // The compositor handed the dispatcher over rather than refusing it.
    assert!(
        report.contains("dispatch>>swapthem"),
        "the plugin was not given its dispatcher: {report}"
    );

    // And what the plugin asked for happened: `movewindow r` with the focus
    // on the gradient swaps the two, which is the picture the renderer's own
    // tests bless for that state.
    let want = image(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../render/tests/data/dwindle-two-clients-swapped.xrle"),
    );
    let (differing, first) = compare(&frame, &want);
    assert_eq!(
        differing, 0,
        "the plugin's dispatcher did not swap the windows; first difference {first:?}"
    );
}

// ---------------------------------------------------------------------------
// The clipboard
//
// Wayland's clipboard is a promise: the program that copied keeps the data,
// and the compositor passes a pipe from whoever pastes to whoever copied.
// What is tested is that the two ends meet -- text put in by one process
// comes out of another, with the compositor in between and never holding it.
// ---------------------------------------------------------------------------

/// What the copying client puts on the clipboard.
const COPIED: &str = "a line that crossed the clipboard";

#[test]
fn what_one_client_copies_another_pastes() {
    let work = workspace("clipboard");
    let socket = work.join("wayland");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        deadline: Some(8000),
        ..Options::default()
    };

    let for_clients = socket.clone();
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // The copy stays alive to answer, as every Wayland clipboard owner
        // must; the paste runs beside it.
        let copying = for_clients.clone();
        let copier = std::thread::spawn(move || {
            compositor_clip::copy(&copying, COPIED, Duration::from_secs(6))
        });
        // A moment for the selection to be set before anything asks for it:
        // a paste that arrives first is told there is nothing, which is
        // true.
        std::thread::sleep(Duration::from_millis(400));
        let pasted = compositor_clip::paste(&for_clients);
        let copied = copier.join().unwrap_or_else(|_| Err("panicked".to_owned()));
        (copied, pasted)
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    let (copied, pasted) = clients.join().expect("the clients finished");
    // Both, before either is unwrapped: a failure in one usually explains
    // the other.
    assert!(
        copied.is_ok() && pasted.is_ok(),
        "the copy said {copied:?} and the paste said {pasted:?}; the compositor said {line}"
    );
    let copied = copied.expect("the copy");
    let pasted = pasted.expect("the paste");

    assert_eq!(pasted, COPIED, "what came out is not what went in");
    assert!(
        copied.contains("asked for 1 times"),
        "the copying client was asked for its data once: {copied}"
    );
    // And the compositor counted both halves.
    assert!(
        line.contains("copied 1 pasted 1"),
        "the compositor's line does not say what the clipboard did: {line}"
    );
}

/// A bar's half of the protocol: `zwlr_foreign_toplevel_management_v1`.
///
/// `compositor/lswt` is a taskbar with the drawing taken out -- it binds the
/// manager, takes a handle for each window, and reads the title, the
/// application id and the states. What is required here is that the two
/// windows the compositor is tiling are the two it describes, with the
/// focused one marked, and that a request sent back through a handle reaches
/// the window it names: `close` on a window nobody owns is what a middle
/// click on a taskbar entry does.
#[test]
fn a_bar_is_told_which_windows_there_are_and_can_close_one() {
    let work = workspace("toplevels");
    let socket = work.join("wayland");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        deadline: Some(8000),
        ..Options::default()
    };

    let for_clients = socket.clone();
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut windows = Vec::new();
        for (pattern, title) in [(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")] {
            let path = for_clients.clone();
            windows.push(std::thread::spawn(move || {
                connect(&path, pattern, title, Shape::Window)
            }));
            std::thread::sleep(Duration::from_millis(250));
        }
        // Both windows are up by now, which is what the list is of.
        let listed = compositor_lswt::run(&for_clients, &compositor_lswt::Want::List);
        // And one of them is asked to close, from outside it.
        let closed = compositor_lswt::run(
            &for_clients,
            &compositor_lswt::Want::Close("one".to_owned()),
        );
        // A moment for the window to go, and then the list again: what the
        // bar was told is only true if the window it named is the one that
        // went.
        std::thread::sleep(Duration::from_millis(500));
        let after = compositor_lswt::run(&for_clients, &compositor_lswt::Want::List);
        for window in windows {
            let _ = window.join();
        }
        (listed, closed, after)
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    let (listed, closed, after) = clients.join().expect("the clients finished");
    let listed = listed.unwrap_or_else(|why| panic!("listing: {why}; the compositor said {line}"));

    assert_eq!(
        listed.len(),
        2,
        "the bar was told about {} windows, not two: {listed:?}; the compositor said {line}",
        listed.len()
    );
    // Both, by application id and title, in the order they opened.
    assert!(
        listed[0].contains("rocks.magical.pattern") && listed[0].contains("\"one\""),
        "the first window: {listed:?}"
    );
    assert!(
        listed[1].contains("\"two\""),
        "the second window: {listed:?}"
    );
    // The focused one is marked, and it is the one that opened last.
    assert!(
        !listed[0].contains("activated") && listed[1].contains("activated"),
        "the focused window is not the one marked: {listed:?}"
    );

    let closed = closed.unwrap_or_else(|why| panic!("closing: {why}"));
    assert!(
        closed.iter().any(|line| line.contains("closed \"one\"")),
        "the close was not sent: {closed:?}"
    );

    // And the window it named is the one that went: `pattern` leaves when
    // the compositor asks its window to close, so the list is now one.
    let after = after.unwrap_or_else(|why| panic!("listing again: {why}"));
    assert_eq!(
        after.len(),
        1,
        "after closing one window the bar sees {after:?}; the compositor said {line}"
    );
    assert!(
        after[0].contains("\"two\""),
        "the wrong window was closed: {after:?}; before: {listed:?}; the compositor said {line}"
    );
}

/// A screenshot, through `zwlr_screencopy_v1`.
///
/// `compositor/shot` is `grim` without the file format: it binds the
/// manager and a `wl_output`, is told what buffer to make, makes one, hands
/// it over and reads back what the compositor wrote into it. What is
/// required is that those pixels are the picture `compositor/render`
/// blesses for the same two windows -- so a screenshot taken over the socket
/// and a frame built by calling the renderer with rectangles agree byte for
/// byte, which is the same standard every other picture in this tree is held
/// to.
#[test]
fn a_screenshot_is_the_frame_the_renderer_blesses() {
    let work = workspace("screenshot");
    let socket = work.join("wayland");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        deadline: Some(8000),
        ..Options::default()
    };

    let for_clients = socket.clone();
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut windows = Vec::new();
        for (pattern, title) in [(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")] {
            let path = for_clients.clone();
            windows.push(std::thread::spawn(move || {
                connect(&path, pattern, title, Shape::Window)
            }));
            std::thread::sleep(Duration::from_millis(250));
        }
        // Both windows are drawn by now, which is what the picture is of.
        std::thread::sleep(Duration::from_millis(500));
        let taken = compositor_shot::take(&for_clients, 0);
        for window in windows {
            let _ = window.join();
        }
        taken
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    let taken = clients.join().expect("the clients finished");
    let taken =
        taken.unwrap_or_else(|why| panic!("the screenshot: {why}; the compositor said {line}"));

    assert_eq!(
        (taken.width, taken.height),
        (WIDTH, HEIGHT),
        "the screenshot is not the screen's size"
    );
    let want = expected();
    assert_eq!(
        taken.pixels.len(),
        want.len(),
        "the screenshot has {} bytes and the expected image {}",
        taken.pixels.len(),
        want.len()
    );
    let differing = taken
        .pixels
        .chunks_exact(3)
        .zip(want.chunks_exact(3))
        .filter(|(shot, blessed)| shot != blessed)
        .count();
    assert_eq!(
        differing,
        0,
        "{differing} of {} pixels in the screenshot are not the renderer's; the compositor said \
         {line}",
        (WIDTH * HEIGHT) as usize
    );
}

/// What is left after a window is closed from outside it.
///
/// The taskbar test says the *list* is right afterwards; this says the
/// *picture* is. They are different claims: the list comes from the layout
/// and the picture from whichever client's buffer the compositor reaches
/// for, and a compositor that held a window's client by its place in a list
/// it had just compacted would get the first right and the second wrong.
#[test]
fn the_window_left_after_a_close_is_drawn_from_its_own_buffer() {
    let work = workspace("closed");
    let socket = work.join("wayland");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        deadline: Some(12000),
        ..Options::default()
    };

    let for_clients = socket.clone();
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut windows = Vec::new();
        for (pattern, title) in [(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")] {
            let path = for_clients.clone();
            windows.push(std::thread::spawn(move || {
                connect(&path, pattern, title, Shape::Window)
            }));
            std::thread::sleep(Duration::from_millis(250));
        }
        let closed = compositor_lswt::run(
            &for_clients,
            &compositor_lswt::Want::Close("one".to_owned()),
        );
        // Long enough for the window that is left to be told its new size
        // and to draw at it.
        std::thread::sleep(Duration::from_millis(2500));
        let taken = compositor_shot::take(&for_clients, 0);
        for window in windows {
            let _ = window.join();
        }
        (closed, taken)
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    let (closed, taken) = clients.join().expect("the clients finished");
    let _ = closed.unwrap_or_else(|why| panic!("closing: {why}"));
    let taken = taken.unwrap_or_else(|why| panic!("the screenshot: {why}"));

    let want = image(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../render/tests/data/one-client-alone.xrle"),
    );
    let differing = taken
        .pixels
        .chunks_exact(3)
        .zip(want.chunks_exact(3))
        .filter(|(shot, blessed)| shot != blessed)
        .count();
    assert_eq!(
        differing, 0,
        "{differing} pixels of the window that was left are not the renderer's; the compositor \
         said {line}"
    );
}

/// A locked screen shows the lock and nothing of what was under it.
///
/// `ext-session-lock-v1` is the one protocol whose whole point is that the
/// compositor stops drawing everything else, so the check is a picture: two
/// windows are tiled, a program takes the lock and draws a checkerboard over
/// the whole screen, and a screenshot of the *locked* screen must be the
/// image `compositor/render` blesses for one -- not the windows, and not a
/// strip of them at any edge.
#[test]
fn a_locked_screen_shows_the_lock_and_none_of_the_windows() {
    let work = workspace("locked");
    let socket = work.join("wayland");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        deadline: Some(14000),
        ..Options::default()
    };

    let for_clients = socket.clone();
    let clients = std::thread::spawn(move || {
        for _ in 0..400 {
            if for_clients.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut windows = Vec::new();
        for (pattern, title) in [(Pattern::Checkerboard, "one"), (Pattern::Gradient, "two")] {
            let path = for_clients.clone();
            windows.push(std::thread::spawn(move || {
                connect(&path, pattern, title, Shape::Window)
            }));
            std::thread::sleep(Duration::from_millis(250));
        }
        // The lock is taken in a thread of its own and held, so that the
        // screenshot below is taken while it is up.
        let locking = for_clients.clone();
        let lock =
            std::thread::spawn(move || compositor_lock::lock(&locking, Duration::from_secs(4)));
        std::thread::sleep(Duration::from_millis(2500));
        let taken = compositor_shot::take(&for_clients, 0);
        let locked = lock.join().unwrap_or_else(|_| Err("panicked".to_owned()));
        for window in windows {
            let _ = window.join();
        }
        (locked, taken)
    });

    let line = hyprix::run(&options).expect("the compositor ran");
    let (locked, taken) = clients.join().expect("the clients finished");
    let locked = locked.unwrap_or_else(|why| panic!("locking: {why}; the compositor said {line}"));
    assert_eq!(
        locked.screens, 1,
        "the lock covered {} screens",
        locked.screens
    );
    assert!(
        locked.told,
        "the compositor never said the screen was covered"
    );

    let taken = taken.unwrap_or_else(|why| panic!("the screenshot: {why}"));
    let want = image(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../render/tests/data/locked-screen.xrle"),
    );
    let differing = taken
        .pixels
        .chunks_exact(3)
        .zip(want.chunks_exact(3))
        .filter(|(shot, blessed)| shot != blessed)
        .count();
    assert_eq!(
        differing, 0,
        "{differing} pixels of the locked screen are not the lock's; the compositor said {line}"
    );
}
