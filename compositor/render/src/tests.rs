use std::collections::BTreeMap;

use compositor_config::{Gaps, NoSources, parse};
use compositor_layout::{Monitor, MonitorId, MonitorLayout, Settings, State, WindowId};

use crate::golden::{self, Mismatch};
use crate::{
    Canvas, Color, Damage, Error, Format, Pattern, Rect, Style, Surface, Target, damage_between,
    outer, render,
};

const BG: u32 = 0x0020_4060;

/// `Style::default()` with the drop shadow off.
///
/// Hyprland has shadows on by default, so a default frame has them; a test
/// that asks what colour a gap or a border is has to say it wants the one
/// with nothing over it, or it is asking about the shadow.
fn unshadowed() -> Style {
    Style {
        shadow: None,
        ..Style::default()
    }
}

/// An `XRGB8888` value as the canvas presents it, X byte `0xFF`.
const fn shown(xrgb: u32) -> u32 {
    xrgb | 0xFF00_0000
}

fn canvas(width: u32, height: u32) -> Canvas {
    let mut canvas = Canvas::new(width, height).unwrap();
    canvas.clear(Color(BG), &Damage::full(width, height));
    let _ = canvas.take_damage();
    canvas
}

/// Every pixel of `canvas` in row order.
fn pixels(canvas: &Canvas) -> Vec<u32> {
    (0..canvas.height())
        .flat_map(|y| (0..canvas.width()).map(move |x| (x, y)))
        .map(|(x, y)| canvas.pixel(x, y).unwrap())
        .collect()
}

// -- The frame of two pattern clients ----------------------------------------

const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const CHECKERBOARD: WindowId = WindowId(1);
const GRADIENT: WindowId = WindowId(2);

/// A 1024x768 monitor with the default gaps and border, the checkerboard
/// opened first and the gradient second, as dwindle tiles them.
fn two_clients() -> (State, MonitorLayout) {
    let mut state = State::new(Settings::default());
    let _ = state
        .add_monitor(Monitor {
            id: MonitorId(1),
            rect: Rect::new(0, 0, i64::from(WIDTH), i64::from(HEIGHT)),
            reserved: Gaps::all(0),
        })
        .unwrap();
    let _ = state.open_window(CHECKERBOARD).unwrap();
    let _ = state.open_window(GRADIENT).unwrap();
    let layout = state.layout().remove(0);
    (state, layout)
}

/// Each window's pattern, drawn at its client rectangle's size.
fn client_buffers(layout: &MonitorLayout) -> BTreeMap<WindowId, (Vec<u8>, u32, u32, Format)> {
    layout
        .windows
        .iter()
        .map(|placed| {
            let pattern = if placed.window == CHECKERBOARD {
                Pattern::Checkerboard
            } else {
                Pattern::Gradient
            };
            let (width, height) = (placed.rect.width as u32, placed.rect.height as u32);
            (
                placed.window,
                (pattern.draw(width, height), width, height, pattern.format()),
            )
        })
        .collect()
}

fn surfaces(
    buffers: &BTreeMap<WindowId, (Vec<u8>, u32, u32, Format)>,
) -> BTreeMap<WindowId, Surface<'_>> {
    buffers
        .iter()
        .map(|(&window, (data, width, height, format))| {
            (
                window,
                Surface::new(data, *width, *height, width * 4, *format).unwrap(),
            )
        })
        .collect()
}

/// The two clients' frame after the dispatchers in `after` have run,
/// presented into a dumb-buffer-shaped target with no padding, as bytes.
///
/// The dispatchers are `compositor/layout`'s own, by the names a keybind
/// writes, so the picture a keybind makes and the picture this blesses come
/// from one piece of code.
fn frame_after(after: &[(&str, &str)]) -> Vec<u8> {
    frame_with(&Style::default(), after)
}

/// The same, drawn with `style`.
fn frame_with(style: &Style, after: &[(&str, &str)]) -> Vec<u8> {
    let (mut state, mut layout) = two_clients();
    for (name, argument) in after {
        let changes = state.dispatch_str(name, argument).unwrap();
        assert!(!changes.is_empty(), "{name} {argument} changed nothing");
        layout = state.layout().remove(0);
    }
    let buffers = client_buffers(&layout);
    let mut canvas = Canvas::new(WIDTH, HEIGHT).unwrap();
    let full = Damage::full(WIDTH, HEIGHT);
    let produced = render(
        &mut canvas,
        &layout,
        (0, 0),
        style,
        &surfaces(&buffers),
        &full,
    );
    assert_eq!(produced, full);
    let mut bytes = vec![0; WIDTH as usize * HEIGHT as usize * 4];
    let mut target = Target::new(&mut bytes, WIDTH, HEIGHT, WIDTH * 4).unwrap();
    canvas.present(&mut target, &full).unwrap();
    assert_eq!(bytes, canvas.data());
    bytes
}

/// The frame with nothing dispatched: the second window opened is the
/// focused one, which dwindle puts on the right.
fn two_client_frame() -> Vec<u8> {
    frame_after(&[])
}

/// A bar across the top, and the two clients tiled in what is left.
///
/// The bar's height is 30 and its exclusive zone all of it, which is what
/// `compositor/pattern --bar 30` asks for; where it goes is
/// `compositor_layout::layers`' answer and what it leaves is the monitor's
/// reserved strip, so the picture is made by the same two crates the
/// compositor uses.
fn bar_and_two_clients_frame() -> Vec<u8> {
    const BAR: u32 = 30;

    let monitor = Rect::new(0, 0, i64::from(WIDTH), i64::from(HEIGHT));
    let request = compositor_layout::layers::Request {
        top: true,
        left: true,
        right: true,
        size: (0, BAR),
        exclusive_zone: BAR.cast_signed(),
        ..compositor_layout::layers::Request::default()
    };
    let (placements, reserved) = compositor_layout::layers::place(monitor, &[request]);
    let bar = placements[0];

    let mut state = State::new(Settings::default());
    let _ = state
        .add_monitor(Monitor {
            id: MonitorId(1),
            rect: monitor,
            reserved: Gaps::all(0),
        })
        .unwrap();
    let _ = state.set_reserved(MonitorId(1), reserved).unwrap();
    let _ = state.open_window(CHECKERBOARD).unwrap();
    let _ = state.open_window(GRADIENT).unwrap();
    let layout = state.layout().remove(0);

    let buffers = client_buffers(&layout);
    // The bar draws the checkerboard, which is what the test client does.
    let bar_pixels = Pattern::Checkerboard.draw(bar.rect.width as u32, bar.rect.height as u32);
    let bar_surface = Surface::new(
        &bar_pixels,
        bar.rect.width as u32,
        bar.rect.height as u32,
        bar.rect.width as u32 * 4,
        Pattern::Checkerboard.format(),
    )
    .unwrap();

    let mut canvas = Canvas::new(WIDTH, HEIGHT).unwrap();
    let full = Damage::full(WIDTH, HEIGHT);
    let layers = [crate::LayerFrame {
        rect: bar.rect,
        above: true,
        surface: Some(bar_surface),
    }];
    let produced = crate::render_with_layers(
        &mut canvas,
        &layout,
        (0, 0),
        &Style::default(),
        &surfaces(&buffers),
        &layers,
        &full,
    );
    assert_eq!(produced, full);
    canvas.data().to_vec()
}

#[test]
fn a_bar_takes_its_strip_and_the_windows_tile_under_it() {
    let frame = bar_and_two_clients_frame();
    golden::check("layer-bar-two-clients", WIDTH, HEIGHT, &frame);

    // The windows really moved down: the frame differs from the one with no
    // bar over far more than the bar's own strip.
    let plain = two_client_frame();
    let differing = frame
        .chunks(4)
        .zip(plain.chunks(4))
        .filter(|(one, other)| one != other)
        .count();
    assert!(
        differing > (WIDTH as usize) * 30,
        "only {differing} pixels changed; the bar drew and nothing moved"
    );
}

/// The same two clients with Hyprland's two decorations on: corners cut to
/// `decoration:rounding` and the unfocused window at
/// `decoration:inactive_opacity`.
fn decorated_style() -> Style {
    let config = parse(
        "test",
        "decoration:rounding = 12\n\
         decoration:inactive_opacity = 0.6\n\
         decoration:shadow:range = 12\n\
         decoration:shadow:render_power = 2\n\
         decoration:dim_inactive = 1\n\
         decoration:dim_strength = 0.4\n",
        &mut NoSources,
    )
    .config;
    let style = Style::from_config(&config);
    assert_eq!(style.rounding, 12);
    assert!((style.inactive_opacity - 0.6).abs() < 0.001);
    assert!((style.active_opacity - 1.0).abs() < f32::EPSILON);
    assert!((style.dim - 0.4).abs() < 0.001);
    let shadow = style.shadow.expect("shadows are on by default");
    assert_eq!((shadow.range, shadow.power), (12, 2));
    assert_eq!(shadow.color, Color(0xEE1A_1A1A), "Hyprland's own default");
    assert_eq!(
        shadow.rounding, 12,
        "the shadow follows the window's corners"
    );
    style
}

fn decorated_frame() -> Vec<u8> {
    frame_with(&decorated_style(), &[])
}

/// The shadow's shape, against the rules `shadow.glsl` has: nothing past the
/// range, and a falloff towards the window that never turns back.
#[test]
fn a_shadow_fades_out_over_its_range_and_no_further() {
    let (_, layout) = two_clients();
    let placed = layout
        .windows
        .iter()
        .find(|placed| placed.focused)
        .copied()
        .unwrap();
    let frame = decorated_frame();
    let at = |x: i64, y: i64| -> u32 {
        let start = ((y * i64::from(WIDTH) + x) * 4) as usize;
        u32::from_le_bytes([
            frame[start],
            frame[start + 1],
            frame[start + 2],
            frame[start + 3],
        ])
    };
    let background = shown(Style::default().background.0 & 0x00FF_FFFF);
    // Above the focused window, at its horizontal middle: the only shadow
    // that can reach there is its own, since the other window is beside it
    // and 20 pixels is not the distance between them.
    let range = decorated_style().shadow.expect("a shadow").range;
    let top = placed.rect.y - placed.border.max(0);
    let middle = placed.rect.x + placed.rect.width / 2;
    assert!(top - range > 0, "the window is too near the top edge");

    assert_eq!(
        at(middle, top - range - 1),
        background,
        "the shadow reached past its range"
    );
    // Near the window it is plainly there. One pixel inside the range it is
    // not: a power of two puts the alpha at `(1/12)² × 0.93`, which is less
    // than half a step of the background's grey and rounds away. That is
    // what a falloff is, and a test that asked for a visible pixel at the
    // far edge would be asking the shadow to have a hard edge.
    assert_ne!(at(middle, top - 2), background, "there is no shadow");

    // Each step towards the window is at least as far towards the shadow's
    // own colour as the one before. Towards, not darker: Hyprland's default
    // shadow is `0xee1a1a1a` and its background `0x111111`, so its shadow is
    // the lighter of the two and a test that asked for darker would be
    // asking about this tree's background rather than about the falloff.
    let greys: Vec<u32> = (1..range)
        .map(|step| at(middle, top - range + step) & 0xFF)
        .collect();
    assert!(
        greys.windows(2).all(|pair| pair[1] >= pair[0]),
        "the falloff is not monotonic: {greys:?}"
    );
    assert_eq!(
        greys.iter().min(),
        greys.first(),
        "it does not start at the background: {greys:?}"
    );
    assert!(
        greys.iter().min() < greys.iter().max(),
        "the shadow is flat: {greys:?}"
    );
}

/// `decoration:dim_inactive` lays black over a window that is not focused,
/// and over no other.
#[test]
fn dimming_darkens_the_unfocused_window_alone() {
    let (_, layout) = two_clients();
    let frame = decorated_frame();
    let plain = frame_with(
        &Style {
            dim: 0.0,
            ..decorated_style()
        },
        &[],
    );
    let at = |bytes: &[u8], x: i64, y: i64| -> u32 {
        let start = ((y * i64::from(WIDTH) + x) * 4) as usize;
        u32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ])
    };
    for placed in &layout.windows {
        let (x, y) = (
            placed.rect.x + placed.rect.width / 2,
            placed.rect.y + placed.rect.height / 2,
        );
        let (dimmed, undimmed) = (at(&frame, x, y), at(&plain, x, y));
        if placed.focused {
            assert_eq!(dimmed, undimmed, "the focused window was dimmed");
        } else {
            assert_ne!(dimmed, undimmed, "the unfocused window was not dimmed");
            assert!(
                (dimmed & 0xFF) <= (undimmed & 0xFF),
                "dimming made it lighter"
            );
        }
    }
}

#[test]
fn rounding_and_opacity_make_a_different_picture() {
    let frame = decorated_frame();
    golden::check("decorated-two-clients", WIDTH, HEIGHT, &frame);

    // The corner of the focused window: outside the rounding it is the
    // background, inside it the border. With no rounding both are border.
    let plain = two_client_frame();
    let differing = frame
        .chunks(4)
        .zip(plain.chunks(4))
        .filter(|(one, other)| one != other)
        .count();
    assert!(differing > 0, "the decorations changed nothing");
}

/// A rounded window's very corner is not the window: the rounding cut it
/// away, and what shows there is whatever is behind. Along the same edge,
/// away from the corner, the border is drawn as it always was.
#[test]
fn a_rounded_corner_shows_what_is_behind_it() {
    let (_, layout) = two_clients();
    // The focused window, so the border on its edge is the active colour.
    let placed = layout
        .windows
        .iter()
        .find(|placed| placed.focused)
        .copied()
        .unwrap();
    let border = placed.border.max(0);
    let outer_x = (placed.rect.x - border) as u32;
    let outer_y = (placed.rect.y - border) as u32;
    let middle = outer_x + (placed.rect.width / 2) as u32;

    // With the shadow off: a corner that is cut shows the background, and a
    // shadow over it would be neither.
    let bytes = frame_with(
        &Style {
            shadow: None,
            ..decorated_style()
        },
        &[],
    );
    let at = |x: u32, y: u32| -> u32 {
        let start = ((y * WIDTH + x) * 4) as usize;
        u32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ])
    };
    let background = shown(Style::default().background.0 & 0x00FF_FFFF);
    assert_eq!(at(outer_x, outer_y), background, "the corner was not cut");
    assert_eq!(
        at(outer_x, outer_y + 1),
        background,
        "the row below the corner was not cut either"
    );
    // The same edge, away from the corner: the border, one pixel of it,
    // which is `general:border_size`'s default.
    assert_eq!(
        at(middle, outer_y),
        shown(0x00FF_FFFF),
        "the border is not on the edge"
    );

    // With no rounding the corner is the border, which is what makes the
    // check above worth having.
    let square = frame_with(&unshadowed(), &[]);
    let start = ((outer_y * WIDTH + outer_x) * 4) as usize;
    assert_eq!(
        u32::from_le_bytes([
            square[start],
            square[start + 1],
            square[start + 2],
            square[start + 3],
        ]),
        shown(0x00FF_FFFF)
    );
}

#[test]
fn two_pattern_clients_tiled_by_dwindle_match_the_expected_image() {
    golden::check("dwindle-two-clients", WIDTH, HEIGHT, &two_client_frame());
}

/// `movefocus l`: the same two windows in the same places, with the active
/// border on the other one.
///
/// This is the state `cargo xtask test-compositor` requires from a
/// screendump after sending the keybind through QEMU, so it is blessed here
/// -- by calling the renderer with rectangles from the layout -- and compared
/// there, where the picture came from two programs talking Wayland.
#[test]
fn moving_the_focus_moves_the_active_border_and_nothing_else() {
    let moved = frame_after(&[("movefocus", "l")]);
    golden::check("dwindle-two-clients-focus-left", WIDTH, HEIGHT, &moved);

    // Only the borders differ from the tiled frame: a `movefocus` that moved
    // a window would be a different picture and a different bug.
    let tiled = two_client_frame();
    let differing = moved
        .chunks(4)
        .zip(tiled.chunks(4))
        .filter(|(one, other)| one != other)
        .count();
    assert!(differing > 0, "the active border did not move");
    assert!(
        differing < (WIDTH as usize) * (HEIGHT as usize) / 4,
        "{differing} pixels changed for a border's worth of colour; a window moved"
    );
}

/// `movewindow r` with the focus on the left window: the two swap places,
/// which is the other keybind stage 18's exit criterion names.
#[test]
fn moving_a_window_swaps_the_two_of_them() {
    let swapped = frame_after(&[("movefocus", "l"), ("movewindow", "r")]);
    golden::check("dwindle-two-clients-swapped", WIDTH, HEIGHT, &swapped);

    // The patterns changed sides, so far more than a border differs.
    let tiled = two_client_frame();
    let differing = swapped
        .chunks(4)
        .zip(tiled.chunks(4))
        .filter(|(one, other)| one != other)
        .count();
    assert!(
        differing > (WIDTH as usize) * (HEIGHT as usize) / 4,
        "only {differing} pixels changed; the windows did not swap"
    );
}

#[test]
fn the_expected_image_check_fails_on_exactly_the_pixel_altered() {
    // The negative control: the frame the golden test passes, with one
    // pixel inside the gradient changed, must be reported as that one pixel.
    let mut frame = two_client_frame();
    let file = std::fs::read(golden::path("dwindle-two-clients")).unwrap();
    let (_, _, expected) = golden::decode(&file).unwrap();
    assert_eq!(golden::compare(&expected, &frame, WIDTH), []);

    let (x, y) = (700u32, 400u32);
    let offset = (y * WIDTH + x) as usize * 4;
    let before = u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap());
    frame[offset] ^= 0x01;
    let after = u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap());
    assert_eq!(
        golden::compare(&expected, &frame, WIDTH),
        [Mismatch {
            x,
            y,
            expected: before,
            actual: after,
        }]
    );
}

#[test]
fn the_expected_image_format_round_trips_and_is_strict() {
    let frame = two_client_frame();
    let file = golden::encode(WIDTH, HEIGHT, &frame);
    assert!(file.len() < 64 * 1024, "{} bytes", file.len());
    assert_eq!(golden::decode(&file), Ok((WIDTH, HEIGHT, frame)));

    let mut long = file.clone();
    long.push(0);
    assert!(golden::decode(&long).is_err());
    assert!(golden::decode(&file[..file.len() - 1]).is_err());
    // A first row may not repeat the row above it.
    let mut header = golden::encode(1, 1, &[1, 2, 3, 4]);
    let tag = header.len() - 7;
    header.truncate(tag);
    header.push(0);
    assert!(golden::decode(&header).is_err());
}

#[test]
fn the_tiled_frame_has_its_gaps_and_borders_on_exact_pixels() {
    let (_, layout) = two_clients();
    let rects: Vec<Rect> = layout.windows.iter().map(|placed| placed.rect).collect();
    // gaps_out 20 to the monitor, gaps_in 5 on each inner edge, border 1.
    assert_eq!(
        rects,
        [Rect::new(21, 21, 485, 726), Rect::new(518, 21, 485, 726)]
    );
    let frame = frame_with(&unshadowed(), &[]);
    let at = |x: u32, y: u32| {
        let offset = (y * WIDTH + x) as usize * 4;
        u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap())
    };
    let style = unshadowed();
    let bg = style.background.0 | 0xFF00_0000;
    let inactive = style.inactive_border.0;
    let active = style.active_border.0;
    assert_eq!((inactive, active), (0xFF44_4444, 0xFFFF_FFFF));

    // Along row 300: the outer gap, the checkerboard's border and pixels,
    // its right border, the inner gap of 10, the gradient's border.
    assert_eq!(at(19, 300), bg);
    assert_eq!(at(20, 300), inactive);
    assert_eq!(at(21, 300), shown(Pattern::Checkerboard.pixel(0, 279, 726)));
    assert_eq!(
        at(505, 300),
        shown(Pattern::Checkerboard.pixel(484, 279, 726))
    );
    assert_eq!(at(506, 300), inactive);
    for x in 507..517 {
        assert_eq!(at(x, 300), bg, "the gap at x {x}");
    }
    assert_eq!(at(517, 300), active);
    assert_eq!(at(518, 300), shown(Pattern::Gradient.pixel(0, 279, 726)));
    assert_eq!(at(1002, 300), shown(Pattern::Gradient.pixel(484, 279, 726)));
    assert_eq!(at(1003, 300), active);
    assert_eq!(at(1004, 300), bg);

    // Down column 100: the border rows and the gap to the monitor's edge.
    assert_eq!(at(100, 19), bg);
    assert_eq!(at(100, 20), inactive);
    assert_eq!(at(100, 21), shown(Pattern::Checkerboard.pixel(79, 0, 726)));
    assert_eq!(
        at(100, 746),
        shown(Pattern::Checkerboard.pixel(79, 725, 726))
    );
    assert_eq!(at(100, 747), inactive);
    assert_eq!(at(100, 748), bg);
    // The border's corners are drawn, and the pixel past them diagonally is
    // not.
    assert_eq!(at(20, 20), inactive);
    assert_eq!(at(1003, 747), active);
    assert_eq!(at(1004, 748), bg);
    assert_eq!(at(19, 19), bg);

    // Every pixel of the frame is opaque in its X byte.
    assert!(frame.chunks(4).all(|pixel| pixel[3] == 0xFF));
}

#[test]
fn the_style_follows_the_configuration() {
    let parsed = parse(
        "t.conf",
        "general:col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg\n\
         general:col.inactive_border = 0xff595959\n",
        &mut NoSources,
    );
    assert_eq!(parsed.diagnostics, []);
    let style = Style::from_config(&parsed.config);
    assert_eq!(style.active_border, Color(0xEE33_CCFF));
    assert_eq!(style.inactive_border, Color(0xFF59_5959));
    assert_eq!(style.background, Style::BACKGROUND);
}

// -- Operations, pixel by pixel ----------------------------------------------

#[test]
fn a_border_lies_outside_its_rectangle_on_exact_pixels() {
    let mut canvas = canvas(20, 16);
    let full = Damage::full(20, 16);
    let color = Color(0xFF12_3456);
    canvas.border(Rect::new(5, 5, 6, 4), 2, color, &full);
    for y in 0..16 {
        for x in 0..20 {
            let outer = (3..13).contains(&x) && (3..11).contains(&y);
            let inner = (5..11).contains(&x) && (5..9).contains(&y);
            let expected = if outer && !inner { 0x0012_3456 } else { BG };
            assert_eq!(canvas.pixel(x, y), Some(shown(expected)), "({x}, {y})");
        }
    }
    assert_eq!(canvas.take_damage().area(), 10 * 8 - 6 * 4);
}

#[test]
fn a_translucent_border_is_blended_once_at_its_corners() {
    let mut canvas = canvas(8, 8);
    canvas.border(
        Rect::new(2, 2, 4, 4),
        1,
        Color(0x80FF_0000),
        &Damage::full(8, 8),
    );
    // Straight red at alpha 128 over BG: 128 + 32 × 127 / 255, and the
    // other channels BG's × 127 / 255, each rounded.
    let blended = shown(0x0090_2030);
    assert_eq!(canvas.pixel(1, 1), Some(blended));
    assert_eq!(canvas.pixel(3, 1), Some(blended));
    assert_eq!(canvas.pixel(6, 6), Some(blended));
    assert_eq!(canvas.pixel(2, 2), Some(shown(BG)));
}

#[test]
fn source_over_matches_hand_computed_values() {
    let mut canvas = canvas(4, 2);
    let full = Damage::full(4, 2);
    // Premultiplied ARGB8888: half alpha, nothing, opaque, alpha 1.
    let row = [0x8040_2010u32, 0x0000_0000, 0xFF12_3456, 0x0101_0101];
    let data: Vec<u8> = row.iter().flat_map(|pixel| pixel.to_le_bytes()).collect();
    let surface = Surface::new(&data, 4, 1, 16, Format::Argb8888).unwrap();
    canvas.composite(&surface, Rect::new(0, 0, 4, 1), &full);
    // s + d × (255 − a) / 255 rounded, per channel, over 0x204060.
    //   a 128: R 64 + 15.94, G 32 + 31.87, B 16 + 47.81
    //   a 1:   R 1 + 31.87,  G 1 + 63.75,  B 1 + 95.62
    assert_eq!(canvas.pixel(0, 0), Some(shown(0x0050_4040)));
    assert_eq!(canvas.pixel(1, 0), Some(shown(BG)));
    assert_eq!(canvas.pixel(2, 0), Some(shown(0x0012_3456)));
    assert_eq!(canvas.pixel(3, 0), Some(shown(0x0021_4161)));

    // A straight colour: 0x40FF8000 is premultiplied to R 64, G 32.125.
    //   R 64 + 32 × 191 / 255 = 87.97, G 32.125 + 47.94 = 80.06,
    //   B 96 × 191 / 255 = 71.91.
    canvas.fill(Rect::new(0, 1, 1, 1), Color(0x40FF_8000), &full);
    assert_eq!(canvas.pixel(0, 1), Some(shown(0x0058_5048)));
    // No alpha draws nothing and damages nothing.
    let _ = canvas.take_damage();
    canvas.fill(Rect::new(1, 1, 1, 1), Color(0x00FF_FFFF), &full);
    assert_eq!(canvas.pixel(1, 1), Some(shown(BG)));
    assert!(canvas.damage().is_empty());

    // Exhaustively for one destination: every alpha, one colour value each.
    for alpha in 0..=255u32 {
        let mut canvas = canvas_of(1, 1);
        let value = alpha / 2;
        let pixel = (alpha << 24) | (value << 16) | (value << 8) | value;
        let data = pixel.to_le_bytes();
        let surface = Surface::new(&data, 1, 1, 4, Format::Argb8888).unwrap();
        canvas.composite(&surface, Rect::new(0, 0, 1, 1), &Damage::full(1, 1));
        let channel = |d: u32| value + (d * (255 - alpha) + 127) / 255;
        let expected = (channel(0x20) << 16) | (channel(0x40) << 8) | channel(0x60);
        assert_eq!(canvas.pixel(0, 0), Some(shown(expected)), "alpha {alpha}");
    }
}

fn canvas_of(width: u32, height: u32) -> Canvas {
    canvas(width, height)
}

#[test]
fn an_xrgb_surface_is_copied_opaque_whatever_its_x_byte() {
    let mut canvas = canvas(3, 1);
    let data: Vec<u8> = [0x0011_2233u32, 0x8044_5566, 0xFF77_8899]
        .iter()
        .flat_map(|pixel| pixel.to_le_bytes())
        .collect();
    let surface = Surface::new(&data, 3, 1, 12, Format::Xrgb8888).unwrap();
    canvas.composite(&surface, Rect::new(0, 0, 3, 1), &Damage::full(3, 1));
    assert_eq!(
        pixels(&canvas),
        [shown(0x0011_2233), shown(0x0044_5566), shown(0x0077_8899)]
    );
}

#[test]
fn a_surface_is_drawn_one_to_one_and_cropped_to_its_rectangle() {
    for format in [Format::Xrgb8888, Format::Argb8888] {
        // A 4x3 surface of distinct opaque pixels, into a 3x4 rectangle at
        // (1, 1) on a 6x6 canvas: column 3 is cropped, row 3 is background.
        let value = |x: u32, y: u32| 0xFF00_0000 | (x << 16) | (y << 8) | 0x7F;
        let data: Vec<u8> = (0..3)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .flat_map(|(x, y)| value(x, y).to_le_bytes())
            .collect();
        let surface = Surface::new(&data, 4, 3, 16, format).unwrap();
        let mut canvas = canvas(6, 6);
        canvas.composite(&surface, Rect::new(1, 1, 3, 4), &Damage::full(6, 6));
        for y in 0..6 {
            for x in 0..6 {
                let expected = if (1..4).contains(&x) && (1..4).contains(&y) {
                    value(x - 1, y - 1)
                } else {
                    shown(BG)
                };
                assert_eq!(canvas.pixel(x, y), Some(expected), "{format:?} ({x}, {y})");
            }
        }
        assert_eq!(canvas.take_damage(), Damage::from(Rect::new(1, 1, 3, 3)));
    }
}

#[test]
fn a_padded_surface_draws_as_a_tight_one() {
    for format in [Format::Xrgb8888, Format::Argb8888] {
        let (width, height) = (37, 21);
        let tight = Pattern::Gradient.draw(width, height);
        let stride = width * 4 + 12;
        let mut padded = vec![0x5Au8; (stride * height) as usize];
        for (row, source) in padded
            .chunks_mut(stride as usize)
            .zip(tight.chunks(width as usize * 4))
        {
            row[..source.len()].copy_from_slice(source);
        }
        let rect = Rect::new(3, 2, i64::from(width), i64::from(height));
        let full = Damage::full(50, 30);
        let mut a = canvas(50, 30);
        let mut b = canvas(50, 30);
        a.composite(
            &Surface::new(&tight, width, height, width * 4, format).unwrap(),
            rect,
            &full,
        );
        b.composite(
            &Surface::new(&padded, width, height, stride, format).unwrap(),
            rect,
            &full,
        );
        assert_eq!(a.data(), b.data(), "{format:?}");
    }
}

#[test]
fn presenting_honours_a_stride_wider_than_the_pixels() {
    let (width, height) = (5u32, 4u32);
    let stride = width * 4 + 8;
    let mut canvas = canvas(width, height);
    canvas.fill(
        Rect::new(1, 1, 3, 2),
        Color(0xFFAB_CDEF),
        &Damage::full(width, height),
    );
    // The last row may stop at its last pixel.
    let mut bytes = vec![0xEEu8; (stride * (height - 1) + width * 4) as usize];
    let mut target = Target::new(&mut bytes, width, height, stride).unwrap();
    canvas
        .present(&mut target, &Damage::full(width, height))
        .unwrap();
    for y in 0..height {
        let row = &bytes[(y * stride) as usize..];
        for x in 0..width {
            let offset = x as usize * 4;
            let pixel = u32::from_le_bytes(row[offset..offset + 4].try_into().unwrap());
            assert_eq!(Some(pixel), canvas.pixel(x, y), "({x}, {y})");
        }
        if y + 1 < height {
            assert_eq!(
                row[width as usize * 4..stride as usize],
                [0xEE; 8],
                "padding"
            );
        }
    }
}

#[test]
fn presenting_writes_only_the_damage_and_needs_the_canvas_size() {
    let mut canvas = canvas(4, 4);
    canvas.clear(Color(0x0000_00FF), &Damage::full(4, 4));
    let mut bytes = vec![0u8; 64];
    let mut target = Target::new(&mut bytes, 4, 4, 16).unwrap();
    canvas
        .present(&mut target, &Damage::from(Rect::new(1, 2, 2, 1)))
        .unwrap();
    for (index, pixel) in bytes.chunks(4).enumerate() {
        let inside = index == 9 || index == 10;
        let expected: &[u8] = if inside { &[0xFF, 0, 0, 0xFF] } else { &[0; 4] };
        assert_eq!(pixel, expected, "pixel {index}");
    }

    let mut small = vec![0u8; 36];
    let mut target = Target::new(&mut small, 3, 3, 12).unwrap();
    assert_eq!(
        canvas.present(&mut target, &Damage::full(4, 4)),
        Err(Error::Mismatch {
            canvas: (4, 4),
            target: (3, 3)
        })
    );
}

#[test]
fn drawing_is_clipped_to_the_damage_and_reports_what_it_wrote() {
    let mut canvas = canvas(10, 10);
    let damage: Damage = [Rect::new(1, 1, 3, 3), Rect::new(6, 2, 10, 2)]
        .into_iter()
        .collect();
    canvas.fill(Rect::new(0, 0, 10, 10), Color(0xFF00_FF00), &damage);
    for y in 0..10 {
        for x in 0..10 {
            let expected = if damage.contains(i64::from(x), i64::from(y)) {
                0x0000_FF00
            } else {
                BG
            };
            assert_eq!(canvas.pixel(x, y), Some(shown(expected)), "({x}, {y})");
        }
    }
    // What was written is the damage on the canvas: the second rectangle
    // loses its columns past the edge.
    let written = canvas.take_damage();
    assert_eq!(written.area(), 9 + 8);
    assert_eq!(written, damage.clipped(canvas.bounds()));
    assert!(canvas.damage().is_empty());

    // Nothing is drawn outside the canvas or with no damage.
    canvas.fill(
        Rect::new(20, 20, 5, 5),
        Color(0xFFFF_FFFF),
        &Damage::full(10, 10),
    );
    canvas.fill(Rect::new(0, 0, 10, 10), Color(0xFFFF_FFFF), &Damage::new());
    assert!(canvas.take_damage().is_empty());
}

#[test]
fn overlapping_damage_blends_a_translucent_fill_once() {
    let damage: Damage = [Rect::new(0, 0, 3, 3), Rect::new(1, 1, 3, 3)]
        .into_iter()
        .collect();
    assert_eq!(damage.area(), 9 + 9 - 4);
    let mut canvas = canvas(4, 4);
    canvas.fill(Rect::new(0, 0, 4, 4), Color(0x80FF_0000), &damage);
    assert_eq!(canvas.pixel(0, 0), Some(shown(0x0090_2030)));
    assert_eq!(canvas.pixel(2, 2), Some(shown(0x0090_2030)), "the overlap");
    assert_eq!(canvas.pixel(3, 0), Some(shown(BG)));
}

#[test]
fn damage_is_a_region_of_disjoint_rectangles() {
    let mut damage = Damage::new();
    damage.add(Rect::new(0, 0, 10, 10));
    damage.add(Rect::new(2, 2, 3, 3));
    assert_eq!(damage.rects(), [Rect::new(0, 0, 10, 10)], "contained");
    damage.add(Rect::new(5, 5, 10, 10));
    damage.add(Rect::new(0, 0, 0, 5));
    assert_eq!(damage.area(), 100 + 100 - 25);
    for (i, a) in damage.rects().iter().enumerate() {
        for b in &damage.rects()[i + 1..] {
            assert_eq!(crate::damage::intersect(*a, *b), None, "{a:?} and {b:?}");
        }
    }
    assert!(damage.contains(14, 14));
    assert!(!damage.contains(15, 14));
    assert!(!damage.contains(12, 2));
    assert_eq!(damage.bounds(), Some(Rect::new(0, 0, 15, 15)));
    assert_eq!(damage.clipped(Rect::new(0, 0, 8, 8)).area(), 64);

    // Past the limit it becomes its bounding box, and fills up from there
    // again: never more rectangles than the limit and the box that replaced
    // them, and never a pixel dropped, since a collapse only ever adds.
    let mut many = Damage::new();
    for i in 0..40 {
        many.add(Rect::new(i * 2, 0, 1, 1));
        assert!(many.rects().len() <= crate::damage::MAX_RECTS + 1);
    }
    assert!(many.rects().len() < 40, "the rectangles were collapsed");
    for i in 0..40 {
        assert!(
            many.contains(i * 2, 0),
            "the column at {} was dropped",
            i * 2
        );
    }
    assert_eq!(many.bounds(), Some(Rect::new(0, 0, 79, 1)));
    // The gaps between the columns the first collapse swallowed are in the
    // region now; the ones added after it are still their own rectangles.
    assert!(many.contains(1, 0));
    assert!(!many.contains(67, 0));
}

#[test]
fn a_frame_with_partial_damage_leaves_the_rest_alone() {
    let (mut state, before) = two_clients();
    let buffers = client_buffers(&before);
    let surfaces = surfaces(&buffers);
    let style = Style::default();
    let mut canvas = Canvas::new(WIDTH, HEIGHT).unwrap();
    let _ = render(
        &mut canvas,
        &before,
        (0, 0),
        &style,
        &surfaces,
        &Damage::full(WIDTH, HEIGHT),
    );
    let first = canvas.data().to_vec();

    // Moving focus to the checkerboard changes both borders and nothing
    // else, so the damage is the two outer rectangles.
    let bind = parse("t.conf", "bind = SUPER, H, movefocus, l\n", &mut NoSources)
        .config
        .binds
        .remove(0);
    assert!(!state.dispatch_bind(&bind).unwrap().is_empty());
    let after = state.layout().remove(0);
    let damage = damage_between(&before, &after, (0, 0));
    let expected: Damage = before.windows.iter().map(outer).collect();
    assert_eq!(damage, expected);

    let _ = canvas.take_damage();
    let produced = render(&mut canvas, &after, (0, 0), &style, &surfaces, &damage);
    assert_eq!(produced, damage);
    assert_eq!(canvas.take_damage(), damage);

    // A full redraw of the new layout is the same frame.
    let mut fresh = Canvas::new(WIDTH, HEIGHT).unwrap();
    let _ = render(
        &mut fresh,
        &after,
        (0, 0),
        &style,
        &surfaces,
        &Damage::full(WIDTH, HEIGHT),
    );
    assert_eq!(canvas.data(), fresh.data());
    // And the damaged frame differs from the first only inside the damage.
    for (index, (old, new)) in first.chunks(4).zip(canvas.data().chunks(4)).enumerate() {
        let (x, y) = (
            index as i64 % i64::from(WIDTH),
            index as i64 / i64::from(WIDTH),
        );
        if old != new {
            assert!(
                damage.contains(x, y),
                "({x}, {y}) changed outside the damage"
            );
        }
    }
    assert_eq!(canvas.pixel(20, 300), Some(0xFFFF_FFFF));
    assert_eq!(canvas.pixel(517, 300), Some(0xFF44_4444));
}

#[test]
fn a_layout_on_a_monitor_away_from_the_origin_is_drawn_in_its_coordinates() {
    let mut state = State::new(Settings::default());
    let _ = state
        .add_monitor(Monitor {
            id: MonitorId(7),
            rect: Rect::new(1920, 100, 200, 100),
            reserved: Gaps::all(0),
        })
        .unwrap();
    let _ = state.open_window(WindowId(3)).unwrap();
    let layout = state.layout().remove(0);
    assert_eq!(layout.windows[0].rect, Rect::new(1941, 121, 158, 58));
    let mut canvas = Canvas::new(200, 100).unwrap();
    let produced = render(
        &mut canvas,
        &layout,
        (1920, 100),
        &unshadowed(),
        &BTreeMap::new(),
        &Damage::full(200, 100),
    );
    assert_eq!(produced, Damage::full(200, 100));
    assert_eq!(
        canvas.pixel(20, 20),
        Some(0xFFFF_FFFF),
        "the focused border"
    );
    assert_eq!(canvas.pixel(21, 21), Some(0xFF11_1111), "no surface yet");
    assert_eq!(canvas.pixel(19, 20), Some(0xFF11_1111));
    assert_eq!(
        damage_between(
            &layout,
            &MonitorLayout {
                windows: Vec::new(),
                ..layout.clone()
            },
            (1920, 100)
        ),
        Damage::from(Rect::new(20, 20, 160, 60))
    );
}

#[test]
fn buffers_are_checked_when_described() {
    let data = [0u8; 16];
    assert!(Surface::new(&data, 2, 2, 8, Format::Argb8888).is_ok());
    assert_eq!(
        Surface::new(&data, 2, 2, 7, Format::Argb8888).unwrap_err(),
        Error::Stride {
            width: 2,
            stride: 7
        }
    );
    assert_eq!(
        Surface::new(&data, 2, 3, 8, Format::Xrgb8888).unwrap_err(),
        Error::Short {
            needed: 24,
            len: 16
        }
    );
    assert_eq!(
        Surface::new(&data, 0, 1, 0, Format::Xrgb8888).unwrap_err(),
        Error::Size {
            width: 0,
            height: 1
        }
    );
    let mut bytes = [0u8; 16];
    assert!(Target::new(&mut bytes, 1, 1, 4).is_ok());
    assert!(Target::new(&mut bytes, 17, 1, 68).is_err());
    assert!(Canvas::new(crate::MAX_SIZE + 1, 1).is_err());
    assert_eq!(
        Format::from_wl_shm(Format::Xrgb8888.wl_shm()),
        Some(Format::Xrgb8888)
    );
    assert_eq!(Format::from_wl_shm(2), None);
    // DRM_FORMAT_ARGB8888 and DRM_FORMAT_XRGB8888 from drm_fourcc.h.
    assert_eq!(Format::Argb8888.fourcc(), 0x3432_5241);
    assert_eq!(Format::Xrgb8888.fourcc(), 0x3432_5258);
}

#[test]
fn the_patterns_are_what_their_docs_say() {
    let board = Pattern::Checkerboard.draw(40, 20);
    assert_eq!(board.len(), 40 * 20 * 4);
    assert_eq!(board[..4], [0xE0, 0xE0, 0xE0, 0x00]);
    assert_eq!(Pattern::Checkerboard.pixel(16, 0, 20), Pattern::DARK);
    assert_eq!(Pattern::Checkerboard.pixel(16, 16, 20), Pattern::LIGHT);
    assert_eq!(Pattern::Gradient.pixel(0, 0, 20), 0xFF00_0080);
    assert_eq!(Pattern::Gradient.pixel(33, 9, 20), 0xFF10_0080);
    // Bottom half, alpha 0xC0. Cell 1 of the gradient is 1 × 8 = 8, so red
    // and green are 8 × 192 / 255 = 6.02, and blue 128 × 192 / 255 = 96.4.
    assert_eq!(Pattern::Gradient.pixel(16, 16, 20), 0xC006_0660);
    // Premultiplied: no channel above alpha.
    for y in 0..40 {
        let pixel = Pattern::Gradient.pixel(1000, y * 16, 640);
        let alpha = pixel >> 24;
        assert!(
            [16, 8, 0]
                .iter()
                .all(|shift| (pixel >> shift) & 0xFF <= alpha)
        );
    }
}

/// The blur is behind a window that can be seen through and nowhere else.
///
/// Every pixel it changes is inside the gradient's rectangle: the gradient
/// is the only surface with an alpha channel, and blurring behind an opaque
/// window costs a pyramid of passes and changes nothing anybody can see.
#[test]
fn only_a_translucent_window_has_its_background_blurred() {
    let (_, layout) = two_clients();
    let style = Style {
        blur: Some((16, 2)),
        shadow: None,
        dim: 0.0,
        ..Style::default()
    };
    let blurred = frame_with(&style, &[]);
    let plain = frame_with(
        &Style {
            blur: None,
            ..style
        },
        &[],
    );

    let inside = |placed: &compositor_layout::Placed, x: i64, y: i64| {
        x >= placed.rect.x
            && x < placed.rect.x + placed.rect.width
            && y >= placed.rect.y
            && y < placed.rect.y + placed.rect.height
    };
    let gradient = layout
        .windows
        .iter()
        .find(|placed| placed.window == GRADIENT)
        .copied()
        .unwrap();
    let checkerboard = layout
        .windows
        .iter()
        .find(|placed| placed.window == CHECKERBOARD)
        .copied()
        .unwrap();

    let mut changed = 0_u32;
    let mut stray = 0_u32;
    let mut over_the_opaque = 0_u32;
    for y in 0..i64::from(HEIGHT) {
        for x in 0..i64::from(WIDTH) {
            let at = ((y * i64::from(WIDTH) + x) * 4) as usize;
            if blurred.get(at..at + 4) == plain.get(at..at + 4) {
                continue;
            }
            changed += 1;
            if inside(&checkerboard, x, y) {
                over_the_opaque += 1;
            } else if !inside(&gradient, x, y) {
                stray += 1;
            }
        }
    }
    assert!(changed > 1000, "the blur changed only {changed} pixels");
    assert_eq!(
        over_the_opaque, 0,
        "the opaque window's pixels were blurred"
    );
    assert_eq!(stray, 0, "{stray} pixels outside any window changed");
}

/// And over something that is not flat, it blurs: the canvas's own pixels,
/// which is where a window over another window would read from.
#[test]
fn the_canvas_blurs_what_is_drawn_on_it() {
    let mut canvas = Canvas::new(128, 128).unwrap();
    let full = Damage::full(128, 128);
    canvas.clear(Color(0xFF00_0000), &full);
    canvas.fill(Rect::new(32, 32, 64, 64), Color(0xFFFF_FFFF), &full);
    let before: Vec<u32> = (0..128).map(|x| canvas.pixel(x, 64).unwrap()).collect();

    canvas.blur(Rect::new(0, 0, 128, 128), 0, 8, 2, &full);
    let after: Vec<u32> = (0..128).map(|x| canvas.pixel(x, 64).unwrap()).collect();
    assert_ne!(before, after, "nothing was blurred");

    // The hard edge at 32 became a gradient: the pixel just outside the
    // square is no longer the background it was.
    assert_eq!(before[30] & 0xFF, 0x00);
    assert!(after[30] & 0xFF > 0, "the edge did not spread outwards");
    // And the frame is still opaque, which presenting depends on.
    assert!(after.iter().all(|pixel| pixel >> 24 == 0xFF));
}

/// `blur:enabled = 0` turns it off, and then the frame is the one with no
/// blur at all -- to the byte, which is what a compositor that skipped the
/// pass rather than running it with no effect produces.
#[test]
fn the_blur_can_be_turned_off() {
    let config = parse("test", "decoration:blur:enabled = 0\n", &mut NoSources).config;
    let style = Style::from_config(&config);
    assert_eq!(style.blur, None);
    assert_eq!(
        frame_with(&style, &[]),
        frame_with(
            &Style {
                blur: None,
                ..Style::default()
            },
            &[]
        )
    );
}
