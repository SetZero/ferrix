//! The configuration against excerpts of a real `hyprlock.conf`, and
//! against upstream's defaults.

use std::path::Path;

use compositor_config::Color;

use super::{Config, Gradient, Layout, Widget};

/// The shape of the customer's file, with its paths and serials replaced.
const EXCERPT: &str = r###"
$font = Ubuntu
$tool = /usr/local/bin/wallpaper-tool

general {
    hide_cursor = true
    ignore_empty_input = true
}

auth {
    pam {
        enabled = true
    }
}

animations {
    enabled = true
    bezier = linear, 1, 1, 0, 0
    animation = fadeIn, 1, 5, linear
    animation = fadeOut, 1, 5, linear
    animation = inputFieldDots, 1, 2, linear
}

background {
    monitor = desc:Dell Inc. DELL U2415 SERIAL1
    color = rgb(1c1b22)
    reload_cmd = $tool lockshot SERIAL1
    reload_time = 20
    blur_passes = 0
}

background {
    monitor = desc:Lenovo Group Limited R27qe Gen2 SERIAL2
    color = rgb(1c1b22)
}

shape {
    monitor =
    size = 520, 190
    color = rgba(0, 0, 0, 0.35)
    rounding = 24
    position = 0, 240
    halign = center
    valign = center
}

label {
    monitor =
    text = $TIME
    color = rgba(255, 255, 255, 0.95)
    font_size = 96
    font_family = $font Light
    shadow_passes = 2
    shadow_size = 4
    position = 0, 262
}

label {
    monitor =
    text = cmd[update:60000] date +"%A, %-d %B"
    font_family = $font
}

input-field {
    monitor = desc:Lenovo Group Limited R27qe Gen2 SERIAL2
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

image {
    monitor = desc:Lenovo Group Limited R27qe Gen2 SERIAL2
    size = 128
    rounding = 10
    border_size = 0
    position = 0, 128
    valign = bottom
}
"###;

fn excerpt() -> Config {
    Config::parse(EXCERPT, Path::new("/hyprlock.conf"))
}

#[test]
fn the_excerpt_reads_without_a_diagnostic() {
    let config = excerpt();
    assert_eq!(config.diagnostics, []);
    assert!(config.general.hide_cursor);
    assert!(config.general.ignore_empty_input);
    assert!(config.auth.pam);
    assert_eq!(config.general.fail_timeout, 2000);
}

#[test]
fn widgets_come_kind_by_kind_in_upstream_order() {
    let kinds: Vec<&str> = excerpt().widgets.iter().map(Widget::kind).collect();
    assert_eq!(
        kinds,
        [
            "background",
            "background",
            "shape",
            "image",
            "input-field",
            "label",
            "label"
        ]
    );
}

#[test]
fn values_are_typed_as_upstream_types_them() {
    let config = excerpt();
    let Some(Widget::Background(background)) = config.widgets.first() else {
        panic!("no background first");
    };
    assert_eq!(background.color, Color(0xFF1C_1B22));
    assert_eq!(
        background.reload_cmd,
        "/usr/local/bin/wallpaper-tool lockshot SERIAL1"
    );
    assert_eq!(background.reload_time, 20);
    // The defaults of what it did not set.
    assert!((background.brightness - 0.8172).abs() < 1e-9);
    assert_eq!(background.zindex, -1);

    let Some(Widget::Shape(shape)) = config.widgets.get(2) else {
        panic!("no shape");
    };
    assert_eq!(shape.size, Layout::pixels(520.0, 190.0));
    assert_eq!(shape.color, Color(0x5900_0000));
    assert_eq!(shape.rounding, 24);

    let Some(Widget::InputField(field)) = config.widgets.get(4) else {
        panic!("no input field");
    };
    assert_eq!(
        field.placeholder_text,
        "<span foreground=\"#cccccc\">Password</span>"
    );
    assert_eq!(
        field.fail_text,
        "<span foreground=\"#ff8888\">$PAMFAIL</span>"
    );
    assert_eq!(field.outer_color, Gradient::solid(0x59FF_FFFF));
    assert!(!field.fade_on_empty);
    assert!((field.dots_size - 0.25).abs() < 1e-9);
    assert!(field.capslock_color.fallback);

    let labels: Vec<_> = config
        .widgets
        .iter()
        .filter_map(|widget| match widget {
            Widget::Label(label) => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(labels.len(), 2);
    assert_eq!(labels[0].font_family, "Ubuntu Light");
    assert_eq!(labels[0].text, "$TIME");
    assert_eq!(labels[0].shadow.passes, 2);
    assert_eq!(labels[0].shadow.size, 4);
    assert_eq!(labels[1].text, "cmd[update:60000] date +\"%A, %-d %B\"");
    // Unset: upstream's defaults.
    assert_eq!(labels[1].font_size, 16);
    assert_eq!(labels[1].color, Color(0xFFFF_FFFF));
}

#[test]
fn animations_take_the_file_s_speeds_and_inherit_the_rest() {
    let config = excerpt();
    assert!((config.animations.duration("fadeIn") - 500.0).abs() < 1e-3);
    assert!((config.animations.duration("inputFieldDots") - 200.0).abs() < 1e-3);
    // Not written: `inputField`'s parent is `global`, 8.
    assert!((config.animations.duration("inputFieldFade") - 800.0).abs() < 1e-3);
    assert_eq!(config.animations.get("inputFieldColors").bezier, "linear");
    let mut off = excerpt();
    off.animations.disable("fadeIn");
    assert!(off.animations.duration("fadeIn").abs() < 1e-3);
}

#[test]
fn monitor_matching_is_by_connector_or_description_prefix() {
    let config = excerpt();
    let lenovo = config.widgets_for("DP-2", "Lenovo Group Limited R27qe Gen2 SERIAL2 (DP-2)");
    let kinds: Vec<&str> = lenovo.iter().map(|widget| widget.kind()).collect();
    // The background sorts first by its zindex of -1; the rest keep the
    // file's order.
    assert_eq!(
        kinds,
        [
            "background",
            "shape",
            "image",
            "input-field",
            "label",
            "label"
        ]
    );
    let qemu = config.widgets_for("Virtual-1", "RHT QEMU Monitor");
    let kinds: Vec<&str> = qemu.iter().map(|widget| widget.kind()).collect();
    assert_eq!(kinds, ["shape", "label", "label"]);
}

#[test]
fn bad_lines_are_diagnostics_and_the_rest_applies() {
    let config = Config::parse(
        "general {\n  no_such = 1\n  fail_timeout = soon\n}\nlabel {\n  size = 1\n  font_size = 30\n}\nanimations {\n  animation = nope, 1, 2, linear\n  animation = fadeIn, 1, 3, wobbly\n}\n",
        Path::new("/f.conf"),
    );
    let messages: Vec<&str> = config
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "config option <general:no_such> does not exist.",
            "cannot parse \"soon\" as an int.",
            "config option <label:size> does not exist.",
            "no such animation",
            "no such bezier",
        ]
    );
    let Some(Widget::Label(label)) = config.widgets.first() else {
        panic!("no label");
    };
    assert_eq!(label.font_size, 30);
    assert_eq!(config.animations.get("fadeIn").bezier, "default");
}

#[test]
fn layouts_and_gradients_parse_as_upstream() {
    assert_eq!(
        Layout::parse("50%, -20"),
        Ok(Layout {
            x: 50.0,
            y: -20.0,
            relative_x: true,
            relative_y: false
        })
    );
    assert_eq!(
        Layout::parse("50%, 10%").map(|layout| layout.absolute((1920.0, 1080.0))),
        Ok((960.0, 108.0))
    );
    assert!(Layout::parse("1").is_err());
    assert!(Layout::parse("1, 2, 3").is_err());
    let (gradient, error) = Gradient::parse("rgba(33ccffee) rgba(00ff99ee) 45deg");
    assert_eq!(error, None);
    assert_eq!(gradient.colors, [Color(0xEE33_CCFF), Color(0xEE00_FF99)]);
    assert!((gradient.angle - 45f32.to_radians()).abs() < 1e-6);
    let (gradient, error) = Gradient::parse("rgba(255, 255, 255, 0.35)");
    assert_eq!(error, None);
    assert_eq!(gradient.colors, [Color(0x59FF_FFFF)]);
}
