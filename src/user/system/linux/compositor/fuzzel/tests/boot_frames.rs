//! The pictures `cargo xtask test-compositor`'s fuzzel boot requires, drawn
//! on the host.
//!
//! The boot starts fuzzel with `data/boot/fuzzel.ini` over an empty screen,
//! then has `/bin/vkbd` type `pat`, and requires QEMU's screendump to be
//! these pictures pixel for pixel. They are made the way the guest makes
//! them: fuzzel's own drawing of the same entries (`data/applications`) with
//! the same font and icons, composited by `src/user/system/linux/compositor/render` as the
//! compositor composites a layer surface on the overlay layer, centred where
//! `src/user/system/linux/compositor/layout` puts one with no anchor. The images are blessed with
//! `COMPOSITOR_RENDER_BLESS=1` and kept with the renderer's.

// An integration test's helpers are not inside a `#[test]` function, so the
// workspace's ban on `expect` and `panic` reaches them; a fixture that
// cannot be built should stop the test loudly, as `two_clients.rs` says.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a test's fixtures should fail loudly, and the workspace's ban is about the program"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use compositor_fuzzel::config::{self, Config, Env};
use compositor_fuzzel::desktop::{self, Search, lower};
use compositor_fuzzel::geometry::{Geometry, Scaling};
use compositor_fuzzel::icon::{Found, Kind};
use compositor_fuzzel::launcher::Launcher;
use compositor_fuzzel::paint::{Look, Painter, Pictures};
use compositor_layout::{Monitor, MonitorId, Rect, Settings, State};
use compositor_render::{Canvas, Damage, Format, LayerFrame, Style, Styles, Surface, Target};

/// The screen a judged boot has.
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;

fn data(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(relative)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The boot's configuration, as fuzzel loads it on Ferrix.
fn boot_config() -> Config {
    let text = read(&data("boot/fuzzel.ini"));
    let loaded = config::from_text(
        &text,
        Path::new("/.config/fuzzel/fuzzel.ini"),
        &Env::default(),
        &[],
        false,
    );
    assert!(
        loaded
            .diagnostics
            .iter()
            .all(|d| d.level != config::Level::Error),
        "{:?}",
        loaded.diagnostics
    );
    loaded.config
}

/// The entries the image carries, as fuzzel reads them there.
fn entries(config: &Config) -> Vec<desktop::Application> {
    let search = Search {
        terminal: config.terminal.clone(),
        filter_desktop: config.filter_desktop,
        desktops: vec!["Hyprland".to_owned()],
        ..Search::default()
    };
    let mut apps = Vec::new();
    for name in ["terminal", "pattern", "top"] {
        let file = format!("{name}.desktop");
        let text = read(&data(&format!("applications/{file}")));
        apps.extend(desktop::parse_desktop_file(
            &text,
            &file,
            &lower(name),
            &format!("/usr/share/applications/{file}"),
            &search,
        ));
    }
    desktop::sort_by_title(&mut apps);
    apps
}

/// fuzzel's frame for `launcher`, as the `ARGB8888` bytes it hands the
/// compositor, and its size.
fn frame(config: &Config, launcher: &Launcher) -> (Vec<u8>, u32, u32) {
    let mut fonts = compositor_text::Fonts::new();
    let font = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../../../assets/fonts/liberation/LiberationSerif-Regular.ttf");
    assert_eq!(fonts.add_file(&font), 1, "{}", font.display());
    // `dpi-aware=no`: 96 DPI at scale 1, whatever the screen reports.
    let scaling = Scaling {
        scale: 1.0,
        dpi: 96.0,
        by_dpi: false,
    };
    let look = Look::new(fonts, config, scaling);
    let geometry = Geometry::new(config, &look.metrics, scaling, 0, 0);
    // Each entry's icon, where the image's hicolor has it.
    let icons = launcher
        .apps
        .iter()
        .map(|app| {
            app.icon_name.as_ref().map(|name| Found {
                path: data(&format!("icons/{name}.svg")),
                kind: Kind::Svg,
            })
        })
        .collect();
    let mut painter = Painter {
        look,
        geometry,
        icons,
        have_icons: true,
        pictures: Pictures::default(),
        input_offset: 0,
    };
    let (width, height) = (
        u32::try_from(geometry.width).unwrap_or(0),
        u32::try_from(geometry.height).unwrap_or(0),
    );
    let mut pixmap = compositor_text::tiny_skia::Pixmap::new(width, height).expect("a pixmap");
    painter.draw(&mut pixmap.as_mut(), launcher, config);
    let mut argb = vec![0u8; width as usize * height as usize * 4];
    compositor_image::to_argb8888(&pixmap.as_ref(), &mut argb, width as usize * 4);
    (argb, width, height)
}

/// The whole screen with fuzzel's frame on it.
fn screen(argb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let monitor = Rect::new(0, 0, i64::from(WIDTH), i64::from(HEIGHT));
    let mut state = State::new(Settings::default());
    let _ = state
        .add_monitor(Monitor {
            id: MonitorId(1),
            name: "Virtual-1".to_owned(),
            rect: monitor,
            reserved: compositor_layout::Gaps::all(0),
            scale: 1.0,
            transform: Default::default(),
            description: String::new(),
            made: <(String, String, String)>::default(),
        })
        .expect("a monitor");
    let layout = state.layout().remove(0);
    let request = compositor_layout::layers::Request {
        size: (width, height),
        ..compositor_layout::layers::Request::default()
    };
    let (placements, _) = compositor_layout::layers::place(monitor, &[request]);
    let placed = placements.first().expect("a placement");
    let surface =
        Surface::new(argb, width, height, width * 4, Format::Argb8888).expect("fuzzel's buffer");
    let layers = [LayerFrame {
        rect: placed.rect,
        above: true,
        surface: Some(surface),
        dim_around: false,
        blur: false,
        xray: false,
    }];
    let mut canvas = Canvas::new(WIDTH, HEIGHT).expect("a canvas");
    let full = Damage::full(WIDTH, HEIGHT);
    let _ = compositor_render::render_with_layers(
        &mut canvas,
        &layout,
        (0, 0),
        &Styles::plain(&Style::default()),
        &BTreeMap::new(),
        &layers,
        &full,
    );
    let mut pixels = vec![0; WIDTH as usize * HEIGHT as usize * 4];
    let mut target = Target::new(&mut pixels, WIDTH, HEIGHT, WIDTH * 4).expect("a target");
    canvas.present(&mut target, &full).expect("the frame fits");
    pixels
}

fn expected_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../render/tests/data")
}

#[test]
fn fuzzel_opened_is_the_boots_first_picture() {
    let config = boot_config();
    let launcher = Launcher::new(&config, entries(&config));
    assert_eq!(launcher.matches.list.len(), 3);
    let (argb, width, height) = frame(&config, &launcher);
    compositor_render::golden::check_in(
        &expected_dir(),
        "fuzzel-listed",
        WIDTH,
        HEIGHT,
        &screen(&argb, width, height),
    );
}

#[test]
fn typing_pat_ranks_the_test_pattern_first() {
    let config = boot_config();
    let mut launcher = Launcher::new(&config, entries(&config));
    for c in ["p", "a", "t"] {
        let _ = launcher.type_text(c);
    }
    let first = launcher
        .selected()
        .and_then(|at| launcher.apps.get(at))
        .map(desktop::Application::title_string);
    assert_eq!(first.as_deref(), Some("Test Pattern"));
    let (argb, width, height) = frame(&config, &launcher);
    compositor_render::golden::check_in(
        &expected_dir(),
        "fuzzel-typed-pat",
        WIDTH,
        HEIGHT,
        &screen(&argb, width, height),
    );
}
