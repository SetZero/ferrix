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

use compositor_render::Pattern;
use hyprix::Options;

/// The screen, which is the size `compositor/render`'s expected image is.
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;

/// Where `compositor/render` keeps the image both paths must produce.
const EXPECTED: &str = "../render/tests/data/dwindle-two-clients.xrle";

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
    let work = workspace(name);
    let socket = work.join("wayland");
    let frames = work.join("frames");

    let options = Options {
        display: socket.to_string_lossy().into_owned(),
        headless: Some((WIDTH, HEIGHT)),
        dump: Some(frames.clone()),
        deadline: Some(4000),
        ..Options::default()
    };

    let clients: Vec<_> = patterns
        .iter()
        .map(|(pattern, title)| (*pattern, (*title).to_owned()))
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
        for (pattern, title) in clients {
            let path = socket_for_clients.clone();
            handles.push(std::thread::spawn(move || {
                // Each client is told where to connect the way any client is.
                connect(&path, pattern, &title)
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
fn connect(socket: &Path, pattern: Pattern, title: &str) -> String {
    // `WAYLAND_DISPLAY` is how every client is told, but the environment is
    // one per process and these clients share it, so the path goes in
    // directly. `socket_path` takes a name with a slash as an absolute path,
    // which is what this is.
    compositor_pattern::client::run_on(socket, pattern, title)
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
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(EXPECTED);
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
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

/// What `hyprctl <command>` printed, up to the next command.
fn hyprctl(command: &str) -> &'static str {
    let marker = format!("\n$ hyprctl {command}\n");
    let start = HYPRCTL
        .find(&marker)
        .unwrap_or_else(|| panic!("{command:?} is not in probe/hyprctl.txt"))
        + marker.len();
    let rest = HYPRCTL.get(start..).unwrap_or("");
    let end = rest.find("\n$ hyprctl ").unwrap_or(rest.len());
    rest.get(..end).unwrap_or("")
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
    let after = HYPRCTL
        .rsplit_once("\n$ hyprctl -j clients\n")
        .map(|(_, rest)| rest)
        .expect("a -j clients after the keyword");
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
