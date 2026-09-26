//! Host tests.

use std::path::{Path, PathBuf};

use crate::number::format_float;
use crate::reader::{MAX_LINE_LEN, MAX_SOURCE_DEPTH};
use crate::source::matches;
use crate::{Diagnostic, Document, Schema, SpecialKey, parse, parse_file, value};

/// What hyprlock's `ConfigManager::init` declares, cut down.
fn hyprlock() -> Schema {
    Schema::new()
        .options(&[
            "general:hide_cursor",
            "general:grace",
            "auth:pam:enabled",
            "auth:fingerprint:enabled",
            "animations:enabled",
        ])
        .special(
            "background",
            SpecialKey::Anonymous,
            &["monitor", "path", "color", "blur_passes"],
        )
        .special(
            "label",
            SpecialKey::Anonymous,
            &[
                "monitor",
                "text",
                "color",
                "font_size",
                "font_family",
                "position",
                "halign",
                "valign",
            ],
        )
        .special(
            "input-field",
            SpecialKey::Anonymous,
            &["monitor", "size", "position", "placeholder_text"],
        )
        .keyword("bezier")
        .keyword("animation")
        .source()
}

/// What hypridle's `ConfigManager::init` declares.
fn hypridle() -> Schema {
    Schema::new()
        .options(&[
            "general:lock_cmd",
            "general:unlock_cmd",
            "general:before_sleep_cmd",
            "general:after_sleep_cmd",
            "general:ignore_dbus_inhibit",
        ])
        .special(
            "listener",
            SpecialKey::Anonymous,
            &["timeout", "on-timeout", "on-resume", "ignore_inhibit"],
        )
        .source()
}

/// A keyed category, as Hyprland's `device { name = … }`.
fn devices() -> Schema {
    Schema::new().option("general:gaps").special(
        "device",
        SpecialKey::Key("name".to_owned()),
        &["sensitivity", "enabled"],
    )
}

fn doc(schema: &Schema, text: &str) -> Document {
    parse(schema, text, Path::new(""))
}

/// The document, which must have no diagnostics.
fn clean(schema: &Schema, text: &str) -> Document {
    let document = doc(schema, text);
    assert_eq!(document.diagnostics, [], "parses without errors");
    document
}

/// The diagnostics' lines and messages.
fn messages(schema: &Schema, text: &str) -> Vec<(usize, String)> {
    doc(schema, text)
        .diagnostics
        .into_iter()
        .map(|diagnostic| (diagnostic.line, diagnostic.message))
        .collect()
}

fn owned(pairs: &[(usize, &str)]) -> Vec<(usize, String)> {
    pairs
        .iter()
        .map(|(line, message)| (*line, (*message).to_owned()))
        .collect()
}

/// A fresh scratch directory under `TMPDIR`.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("compositor-hyprlang-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir.canonicalize().expect("the scratch directory exists")
}

// -- Whole files ---------------------------------------------------------------

const HYPRLOCK: &str = r###"# BACKGROUND
$font = Ubuntu
$fontsize = 20

general {
    hide_cursor = true
    grace = 0
}

auth {
    pam {
        enabled = true
    }
    fingerprint:enabled = false
}

animations {
    enabled = true
    bezier = linear, 1, 1, 0, 0
    animation = fadeIn, 1, 5, linear
}

background {
    monitor =
    path = screenshot
    blur_passes = 3
}

background {
    monitor = DP-1
    color = rgba(25, 20, 20, 1.0)
}

background {
    monitor = HDMI-A-1
    path = ~/Pictures/wall.png   # a comment after a value
}

label {
    monitor =
    text = cmd[update:60000] date +"%A, %-d %B"
    color = rgba(242, 243, 244, 0.75)
    font_size = $fontsize
    font_family = $font Light
    position = 0, 300
}

label {
    text = <span foreground="##cccccc">$USER</span>
    font_family = $font
}
"###;

#[test]
fn a_hyprlock_conf_reads_as_hyprlock_reads_it() {
    let document = clean(&hyprlock(), HYPRLOCK);
    assert_eq!(document.get("general:hide_cursor"), Some("true"));
    assert_eq!(document.get("general:grace"), Some("0"));
    assert_eq!(document.get("auth:pam:enabled"), Some("true"));
    assert_eq!(document.get("auth:fingerprint:enabled"), Some("false"));
    assert_eq!(document.get("animations:enabled"), Some("true"));

    let backgrounds: Vec<_> = document.instances_of("background").collect();
    assert_eq!(backgrounds.len(), 3, "three blocks are three backgrounds");
    let monitors: Vec<_> = backgrounds
        .iter()
        .map(|instance| instance.get("monitor"))
        .collect();
    assert_eq!(monitors, [Some(""), Some("DP-1"), Some("HDMI-A-1")]);
    assert_eq!(
        backgrounds.first().and_then(|b| b.get("blur_passes")),
        Some("3")
    );
    assert_eq!(backgrounds.get(1).and_then(|b| b.get("path")), None);
    assert_eq!(
        backgrounds.get(1).and_then(|b| b.get("color")),
        Some("rgba(25, 20, 20, 1.0)")
    );
    assert_eq!(
        backgrounds.get(2).and_then(|b| b.get("path")),
        Some("~/Pictures/wall.png")
    );
    assert!(backgrounds.iter().all(|instance| instance.key.is_none()));
    let lines: Vec<_> = backgrounds.iter().map(|instance| instance.line).collect();
    assert_eq!(lines, [24, 30, 35], "an instance begins at its first value");

    let labels: Vec<_> = document.instances_of("label").collect();
    assert_eq!(labels.len(), 2);
    let first = labels.first().expect("a first label");
    assert_eq!(
        first.get("text"),
        Some(r#"cmd[update:60000] date +"%A, %-d %B""#)
    );
    assert_eq!(
        first.get("font_size"),
        Some("20"),
        "$fontsize is not $font + size"
    );
    assert_eq!(first.get("font_family"), Some("Ubuntu Light"));
    assert_eq!(first.get("position"), Some("0, 300"));
    assert_eq!(first.setting("font_family").map(|s| s.line), Some(44));
    let second = labels.get(1).expect("a second label");
    assert_eq!(
        second.get("text"),
        Some(r##"<span foreground="#cccccc">$USER</span>"##),
        "## is #, and with no environment $USER stays"
    );
    assert_eq!(second.get("font_family"), Some("Ubuntu"));

    let beziers: Vec<_> = document.keywords_named("bezier").collect();
    assert_eq!(beziers.len(), 1);
    let bezier = beziers.first().expect("a bezier");
    assert_eq!(bezier.value, "linear, 1, 1, 0, 0");
    assert_eq!(bezier.categories, ["animations"]);
    assert_eq!(bezier.line, 19);
    let animations: Vec<_> = document
        .keywords_named("animation")
        .map(|keyword| keyword.value.as_str())
        .collect();
    assert_eq!(animations, ["fadeIn, 1, 5, linear"]);

    assert_eq!(
        document.variables,
        [
            ("font".to_owned(), "Ubuntu".to_owned()),
            ("fontsize".to_owned(), "20".to_owned())
        ]
    );
}

const HYPRIDLE: &str = "general {
    lock_cmd = pidof hyprlock || hyprlock       # avoid starting multiple hyprlock instances.
    before_sleep_cmd = loginctl lock-session    # lock before suspend.
    after_sleep_cmd = hyprctl dispatch dpms on  # to avoid having to press a key twice.
}

listener {
    timeout = 150                                # 2.5min.
    on-timeout = brightnessctl -s set 10         # set monitor backlight to minimum.
    on-resume = brightnessctl -r                 # monitor backlight restore.
}

listener {
    timeout = 330
    on-timeout = hyprctl dispatch dpms off
    on-resume = hyprctl dispatch dpms on && brightnessctl -r
}
";

#[test]
fn a_hypridle_conf_reads_as_hypridle_reads_it() {
    let document = clean(&hypridle(), HYPRIDLE);
    assert_eq!(
        document.get("general:lock_cmd"),
        Some("pidof hyprlock || hyprlock")
    );
    assert_eq!(
        document.get("general:before_sleep_cmd"),
        Some("loginctl lock-session")
    );
    assert_eq!(
        document.get("general:after_sleep_cmd"),
        Some("hyprctl dispatch dpms on")
    );
    let listeners: Vec<_> = document.instances_of("listener").collect();
    assert_eq!(listeners.len(), 2);
    let timeouts: Vec<_> = listeners.iter().map(|l| l.get("timeout")).collect();
    assert_eq!(timeouts, [Some("150"), Some("330")]);
    assert_eq!(
        listeners.first().and_then(|l| l.get("on-timeout")),
        Some("brightnessctl -s set 10")
    );
    assert_eq!(
        listeners.get(1).and_then(|l| l.get("on-resume")),
        Some("hyprctl dispatch dpms on && brightnessctl -r")
    );
    let setting = document.options.first().expect("an option");
    assert_eq!(
        (setting.name.as_str(), setting.line),
        ("general:lock_cmd", 2)
    );
}

#[test]
fn an_anonymous_instance_is_made_whatever_its_first_value_says() {
    let document = clean(&hypridle(), "listener {\n timeout = soon\n}\n");
    let listener = document
        .instances_of("listener")
        .next()
        .expect("a listener");
    assert_eq!(listener.get("timeout"), Some("soon"), "values stay text");
    let document = clean(
        &hypridle(),
        "listener {\n on-resume = x\n}\nlistener {\n}\n",
    );
    assert_eq!(document.instances.len(), 1, "an empty block makes nothing");
}

// -- Comments -----------------------------------------------------------------

#[test]
fn a_line_beginning_with_a_hash_is_a_comment_whatever_follows() {
    let document = clean(
        &hypridle(),
        "################\n### MONITORS ###\n  ## general:lock_cmd = x\n#general {\n",
    );
    assert_eq!(document, Document::default());
}

#[test]
fn a_double_hash_is_a_hash_and_a_single_one_a_comment() {
    let schema = Schema::new().options(&["a:b"]);
    let value = |text: &str| {
        clean(&schema, &format!("a:b = {text}\n"))
            .get("a:b")
            .map(ToOwned::to_owned)
    };
    assert_eq!(value("x # y").as_deref(), Some("x"));
    assert_eq!(value("x ## y").as_deref(), Some("x # y"));
    assert_eq!(value("##cccccc").as_deref(), Some("#cccccc"));
    assert_eq!(value("a##b#c").as_deref(), Some("a#b"));
    assert_eq!(
        value("x###y").as_deref(),
        Some("x##y"),
        "hyprlang skips past the kept #"
    );
    assert_eq!(value("x####y").as_deref(), Some("x##"));
    assert_eq!(value("x#").as_deref(), Some("x"));
    assert_eq!(value("x##").as_deref(), Some("x#"));
}

// -- Diagnostics --------------------------------------------------------------

#[test]
fn every_complaint_uses_hyprlangs_words() {
    let schema = hypridle();
    assert_eq!(
        messages(
            &schema,
            "just words\n} x\n}\n= value\ngeneral {\n  nope = 1\n"
        ),
        owned(&[
            (1, "Invalid config line"),
            (2, "Invalid config line"),
            (3, "Stray category close"),
            (4, "Empty lhs."),
            (6, "config option <general:nope> does not exist."),
            (0, "Unclosed category at EOF"),
        ])
    );
    assert_eq!(
        messages(&schema, "general {\n}\ngeneral:lock_cmd = a \\"),
        owned(&[(0, "Last line ends with backslash")])
    );
    assert_eq!(
        messages(&schema, "# hyprlang endif\n"),
        owned(&[(1, "stray endif")])
    );
}

#[test]
fn every_line_is_read_after_a_bad_one() {
    let document = doc(
        &hypridle(),
        "general {\n  bogus = 1\n  lock_cmd = a\n}\n}\ngeneral:unlock_cmd = b\n",
    );
    assert_eq!(document.diagnostics.len(), 2);
    assert_eq!(document.get("general:lock_cmd"), Some("a"));
    assert_eq!(document.get("general:unlock_cmd"), Some("b"));
}

#[test]
fn an_unknown_name_at_the_top_is_not_hyprlangs_to_complain_about() {
    // `configSetValueSafe` returns "not found, no error" for a name with no
    // `:` -- it is probably a handler -- and no handler takes it.
    assert_eq!(messages(&hypridle(), "whatever = 1\n"), []);
    assert_eq!(
        messages(&hypridle(), "what:ever = 1\n"),
        owned(&[(1, "config option <what:ever> does not exist.")])
    );
}

#[test]
fn a_diagnostic_prints_as_hyprlang_prints_it() {
    let at = |file: &str, line: usize| Diagnostic {
        file: PathBuf::from(file),
        line,
        message: "Invalid config line".to_owned(),
    };
    assert_eq!(
        at("/h/hyprlock.conf", 3).to_string(),
        "Config error in file /h/hyprlock.conf at line 3: Invalid config line"
    );
    assert_eq!(
        at("", 3).to_string(),
        "Config error at line 3: Invalid config line"
    );
    assert_eq!(
        at("/h/hyprlock.conf", 0).to_string(),
        "Config error in file /h/hyprlock.conf: Invalid config line"
    );
    assert_eq!(at("", 0).to_string(), "Config error: Invalid config line");
}

#[test]
fn diagnostics_carry_the_file_they_are_about() {
    let document = parse(&hypridle(), "}\n", Path::new("/etc/hypridle.conf"));
    let diagnostic = document.diagnostics.first().expect("a diagnostic");
    assert_eq!(diagnostic.file, Path::new("/etc/hypridle.conf"));
    assert_eq!(diagnostic.line, 1);
}

// -- Continued lines ----------------------------------------------------------

#[test]
fn a_backslash_continues_a_line_numbered_by_its_first() {
    let schema = hypridle();
    let document = clean(
        &schema,
        "general {\n  lock_cmd = one   \\\n\ttwo \\\n three\n  unlock_cmd = x\\\n}\n}\n",
    );
    let lock = document.options.first().expect("lock_cmd");
    assert_eq!(
        lock.value, "one\ttwo three",
        "spaces before the backslash go"
    );
    assert_eq!(lock.line, 2);
    let unlock = document.options.get(1).expect("unlock_cmd");
    assert_eq!(unlock.value, "x}", "the next line is joined as it is");
    assert_eq!(unlock.line, 5);
    assert_eq!(
        messages(&schema, "general {\n}\n\\\ngeneral:lock_cmd = y\n}\n"),
        owned(&[(5, "Stray category close")]),
        "a lone backslash joins nothing to the next line, and numbering goes on"
    );
}

#[test]
fn a_crlf_line_and_a_last_line_without_newline_read_the_same() {
    let document = clean(
        &hypridle(),
        "general {\r\n  lock_cmd = a\r\n}\r\ngeneral:unlock_cmd = b",
    );
    assert_eq!(document.get("general:lock_cmd"), Some("a"));
    assert_eq!(document.get("general:unlock_cmd"), Some("b"));
}

// -- Directives ---------------------------------------------------------------

#[test]
fn hyprlang_if_reads_the_environment_and_variables() {
    let schema = hypridle().environment([("WAYLAND_DISPLAY", "wayland-1"), ("EMPTY", "")]);
    let text = "\
# hyprlang if WAYLAND_DISPLAY
general:lock_cmd = wayland
# hyprlang endif
# hyprlang if !WAYLAND_DISPLAY
general:lock_cmd = tty
# hyprlang endif
# hyprlang if EMPTY
general:unlock_cmd = empty
# hyprlang endif
# hyprlang if MISSING
general:unlock_cmd = missing
this line would be an error
# hyprlang endif
# hyprlang if !MISSING
general:before_sleep_cmd = not missing
# hyprlang endif
$mine = yes
# hyprlang if mine
general:after_sleep_cmd = mine
# hyprlang endif
";
    let document = clean(&schema, text);
    assert_eq!(document.get("general:lock_cmd"), Some("wayland"));
    assert_eq!(document.get("general:unlock_cmd"), None);
    assert_eq!(
        document.get("general:before_sleep_cmd"),
        Some("not missing")
    );
    assert_eq!(document.get("general:after_sleep_cmd"), Some("mine"));
}

#[test]
fn a_skipped_block_skips_its_categories_too() {
    let document = clean(
        &hypridle(),
        "# hyprlang if NOPE\nlistener {\n  timeout = 1\n}\n}\n# hyprlang endif\n",
    );
    assert_eq!(document.instances, []);
}

#[test]
fn a_nested_if_is_decided_by_itself_alone_as_in_hyprlang() {
    // `parseLine` looks at the innermost condition only.
    let schema = hypridle().environment([("SET", "1")]);
    let document = clean(
        &schema,
        "# hyprlang if NOPE\n# hyprlang if SET\ngeneral:lock_cmd = inner\n# hyprlang endif\ngeneral:unlock_cmd = outer\n# hyprlang endif\n",
    );
    assert_eq!(document.get("general:lock_cmd"), Some("inner"));
    assert_eq!(document.get("general:unlock_cmd"), None);
}

#[test]
fn hyprlang_noerror_silences_line_errors_until_turned_off() {
    let schema = hypridle();
    assert_eq!(
        messages(
            &schema,
            "# hyprlang noerror true\nbad line\n# hyprlang noerror false\nworse line\n# hyprlang noerror\nbad\n"
        ),
        owned(&[(4, "Invalid config line")])
    );
    assert_eq!(
        messages(&schema, "# hyprlang noerror\ngeneral {\n"),
        owned(&[(0, "Unclosed category at EOF")]),
        "a file's own complaints are not a line's"
    );
}

// -- Variables and expressions ------------------------------------------------

#[test]
fn variables_expand_longest_first_and_on_both_sides() {
    let schema = hypridle();
    let document = clean(
        &schema,
        "$cmd = lock\n$cmdline = never\ngeneral:$cmd_cmd = $cmdline and $cmd\n$cat:x = y\n",
    );
    assert_eq!(document.get("general:lock_cmd"), Some("never and lock"));
    assert!(
        document.variables.iter().any(|(name, _)| name == "cat:x"),
        "a name that begins with $ is a variable's, and not expanded"
    );
}

#[test]
fn a_variable_may_use_another_and_be_set_again() {
    let schema = hypridle();
    let document = clean(
        &schema,
        "$a = one\n$b = $a two\n$a = three\ngeneral:lock_cmd = $b $a\n",
    );
    assert_eq!(document.get("general:lock_cmd"), Some("one two three"));
    assert_eq!(
        document.variables,
        [
            ("a".to_owned(), "three".to_owned()),
            ("b".to_owned(), "one two".to_owned())
        ]
    );
}

#[test]
fn a_variable_that_names_itself_hits_hyprlangs_iteration_limit() {
    let text = "$b = $c\n$c = $b\ngeneral:lock_cmd = $b\n";
    assert_eq!(
        messages(&hypridle(), text),
        owned(&[(3, "Expanding variables exceeded max iteration limit")])
    );
}

#[test]
fn a_variable_that_doubles_stops_at_the_length_limit() {
    // `$b`'s value is `$b$b`, so every pass doubles the line. hyprlang would
    // grow it until memory ran out.
    let text = "$d = $\n$b = $db$db\ngeneral:lock_cmd = $b\n";
    let document = doc(&hypridle(), text);
    let message = &document.diagnostics.first().expect("a diagnostic").message;
    assert_eq!(
        *message,
        format!("Expanding variables exceeded max length of {MAX_LINE_LEN} bytes")
    );
    assert_eq!(document.get("general:lock_cmd"), None);
}

#[test]
fn the_environment_is_expanded_and_a_file_variable_replaces_it() {
    let schema = hypridle().environment([("HOME", "/home/me"), ("HOMEDIR", "/x")]);
    let document = clean(
        &schema,
        "general:lock_cmd = $HOME/lock $HOMEDIR\n$HOME = /elsewhere\ngeneral:unlock_cmd = $HOME\n",
    );
    assert_eq!(document.get("general:lock_cmd"), Some("/home/me/lock /x"));
    assert_eq!(document.get("general:unlock_cmd"), Some("/elsewhere"));
    assert_eq!(
        document.variables,
        [("HOME".to_owned(), "/elsewhere".to_owned())],
        "the environment is not the file's"
    );
}

#[test]
fn a_variable_is_not_expanded_in_a_category_name() {
    assert_eq!(
        messages(&hypridle(), "$c = general\n$c {\n  lock_cmd = a\n}\n"),
        owned(&[(3, "config option <$c:lock_cmd> does not exist.")])
    );
}

#[test]
fn expressions_are_worked_out_in_float() {
    let schema = Schema::new().options(&["a:b", "a:c", "a:d", "a:e", "a:f"]);
    let document = clean(
        &schema,
        "$w = 10\na:b = {{$w * 2}}px\na:c = {{w + 1.5}}\na:d = {{10 / 4}} and {{0.1 + 0.2}}\na:e = \\{{w + 1}} \\} \\\\ \\x\na:f = {{1 / 0}}\n",
    );
    assert_eq!(document.get("a:b"), Some("20px"));
    assert_eq!(
        document.get("a:c"),
        Some("11.5"),
        "a bare name is a variable"
    );
    assert_eq!(document.get("a:d"), Some("2.5 and 0.3"));
    assert_eq!(
        document.get("a:e"),
        Some(r"{{w + 1}} } \ \x"),
        "escapes come out"
    );
    assert_eq!(document.get("a:f"), Some("inf"));
}

#[test]
fn expression_errors_drop_the_line() {
    let schema = Schema::new().options(&["a:b"]);
    assert_eq!(
        messages(
            &schema,
            "$s = text\na:b = {{}}\na:b = {{1 % 2}}\na:b = {{s + 1}}\na:b = {{1 + x}}\na:b = {{ }}\n"
        ),
        owned(&[
            (2, "Expression is empty"),
            (3, "Invalid expression type: supported +, -, *, /"),
            (
                4,
                "Failed to parse expression: value 1 holds a variable that does not look like a number"
            ),
            (
                5,
                "Failed to parse expression: value 1 does not look like a number or the variable doesn't exist"
            ),
            (6, "Invalid expression type: supported +, -, *, /"),
        ])
    );
    assert_eq!(doc(&schema, "a:b = {{1 + x}}\n").get("a:b"), None);
    assert_eq!(
        clean(&schema, "a:b = {{1 + 2\n").get("a:b"),
        Some("{{1 + 2"),
        "an unclosed expression is text"
    );
}

#[test]
fn a_float_prints_as_std_format_prints_it() {
    assert_eq!(format_float(3.0), "3");
    assert_eq!(format_float(-2.5), "-2.5");
    assert_eq!(format_float(0.001), "0.001");
    assert_eq!(format_float(0.0001), "1e-04");
    assert_eq!(format_float(1e20), "1e+20");
    assert_eq!(format_float(123_456.0), "123456");
    assert_eq!(format_float(f32::NEG_INFINITY), "-inf");
}

// -- Categories, specials and keywords ----------------------------------------

#[test]
fn nested_categories_join_with_colons_and_a_path_may_be_one_line() {
    let schema = hyprlock();
    let nested = clean(&schema, "auth {\n  pam {\n    enabled = false\n  }\n}\n");
    let flat = clean(&schema, "auth:pam:enabled = false\n");
    assert_eq!(nested.get("auth:pam:enabled"), Some("false"));
    assert_eq!(flat.get("auth:pam:enabled"), Some("false"));
    let mixed = clean(&schema, "auth {\n  pam:enabled = 1\n}\n");
    assert_eq!(mixed.get("auth:pam:enabled"), Some("1"));
}

#[test]
fn a_name_set_twice_answers_the_last() {
    let document = clean(&hypridle(), "general:lock_cmd = a\ngeneral:lock_cmd = b\n");
    assert_eq!(document.options.len(), 2);
    assert_eq!(document.get("general:lock_cmd"), Some("b"));
}

#[test]
fn top_level_shorthand_fills_one_instance_until_a_top_level_close() {
    let schema = hyprlock();
    let document = clean(
        &schema,
        "background:monitor = a\nbackground:color = red\ngeneral {\n  grace = 1\n}\nbackground:monitor = b\nbackground {\n  monitor = c\n}\nbackground {\n  monitor = d\n}\n",
    );
    let backgrounds: Vec<_> = document.instances_of("background").collect();
    let monitors: Vec<_> = backgrounds.iter().map(|b| b.get("monitor")).collect();
    // Nothing closes the instance `background:monitor = b` began before
    // the block after it opens, so hyprlang fills it from that block too.
    assert_eq!(monitors, [Some("a"), Some("c"), Some("d")]);
    let first = backgrounds.first().expect("a first");
    assert_eq!(first.get("color"), Some("red"));
    let second = backgrounds.get(1).expect("a second");
    let values: Vec<_> = second.values.iter().map(|v| v.value.as_str()).collect();
    assert_eq!(values, ["b", "c"]);
}

#[test]
fn an_unknown_value_in_a_special_category_is_an_error_unless_ignored() {
    let strict = hyprlock();
    assert_eq!(
        messages(&strict, "background {\n  monitor = a\n  bogus = 1\n}\n"),
        owned(&[(3, "config option <background:bogus> does not exist.")])
    );
    let lenient =
        Schema::new().special_ignoring_missing("plugin", SpecialKey::Anonymous, &["name"]);
    let document = clean(&lenient, "plugin {\n  name = a\n  bogus = 1\n}\n");
    assert_eq!(document.instances.len(), 1);
    assert_eq!(document.instances.first().map(|i| i.values.len()), Some(1));
    assert_eq!(
        messages(&lenient, "plugin {\n  bogus = 1\n  name = b\n}\n"),
        owned(&[(2, "config option <plugin:bogus> does not exist.")]),
        "hyprlang ignores missing values only in an instance already made"
    );
}

#[test]
fn a_keyed_category_has_one_instance_per_key() {
    let schema = devices();
    let document = clean(
        &schema,
        "device {\n  name = mouse\n  sensitivity = 1\n}\ndevice {\n  name = pad\n  enabled = no\n}\ndevice {\n  name = mouse\n  enabled = yes\n}\n",
    );
    let devices: Vec<_> = document.instances_of("device").collect();
    assert_eq!(devices.len(), 2);
    let mouse = devices.first().expect("a mouse");
    assert_eq!(mouse.key.as_deref(), Some("mouse"));
    assert_eq!(mouse.get("name"), Some("mouse"));
    assert_eq!(mouse.get("sensitivity"), Some("1"));
    assert_eq!(
        mouse.get("enabled"),
        Some("yes"),
        "a later block adds to it"
    );
    let pad = devices.get(1).expect("a pad");
    assert_eq!(
        (pad.key.as_deref(), pad.get("enabled")),
        (Some("pad"), Some("no"))
    );
}

#[test]
fn a_keyed_block_must_begin_with_its_key() {
    let schema = devices();
    let document = doc(&schema, "device {\n  sensitivity = 1\n  name = x\n}\n");
    let messages: Vec<_> = document
        .diagnostics
        .iter()
        .map(|d| (d.line, d.message.as_str()))
        .collect();
    assert_eq!(
        messages,
        [(
            2,
            "special category's first value must be the key. Key for <device> is <name>"
        )]
    );
    // hyprlang has made an instance keyed `0` by then, and the block's other
    // lines go to it.
    let instance = document.instances.first().expect("an instance");
    assert_eq!(instance.key.as_deref(), Some("x"));
    assert_eq!(instance.get("sensitivity"), None);
}

#[test]
fn a_bracketed_key_names_an_instance_in_one_line() {
    let schema = devices();
    let document = clean(
        &schema,
        "device[mouse]:sensitivity = 2\ndevice {\n  name = mouse\n  enabled = 1\n}\ndevice[pad]:enabled = 0\n",
    );
    let devices: Vec<_> = document.instances_of("device").collect();
    assert_eq!(devices.len(), 2);
    let mouse = devices.first().expect("a mouse");
    assert_eq!(
        (
            mouse.key.as_deref(),
            mouse.get("sensitivity"),
            mouse.get("enabled")
        ),
        (Some("mouse"), Some("2"), Some("1"))
    );
    assert_eq!(devices.get(1).and_then(|d| d.key.as_deref()), Some("pad"));
}

#[test]
fn an_option_is_tried_before_a_keyword() {
    let schema = Schema::new().option("animations:bezier").keyword("bezier");
    let document = clean(&schema, "animations {\n  bezier = a\n}\nbezier = b\n");
    assert_eq!(document.get("animations:bezier"), Some("a"));
    let keywords: Vec<_> = document.keywords.iter().map(|k| k.value.as_str()).collect();
    assert_eq!(keywords, ["b"]);
}

#[test]
fn an_unscoped_keyword_matches_anywhere_and_a_scoped_one_only_in_place() {
    let unscoped = Schema::new().keyword("bezier");
    let document = clean(
        &unscoped,
        "bezier = top\nanimations {\n  bezier = in\n  inner {\n    bezier = deep\n  }\n}\n",
    );
    let found: Vec<_> = document
        .keywords_named("bezier")
        .map(|k| (k.value.as_str(), k.categories.join(":")))
        .collect();
    assert_eq!(
        found,
        [
            ("top", String::new()),
            ("in", "animations".to_owned()),
            ("deep", "animations:inner".to_owned())
        ]
    );

    let scoped = Schema::new().keyword("animations:bezier");
    let document = doc(
        &scoped,
        "bezier = top\nanimations {\n  bezier = in\n  inner {\n    bezier = deep\n  }\n}\nanimations:bezier = flat\n",
    );
    let found: Vec<_> = document
        .keywords
        .iter()
        .map(|k| (k.name.as_str(), k.value.as_str()))
        .collect();
    // The whole category path, exactly -- which hyprlang also finds in a
    // top-level line that writes that path out.
    assert_eq!(found, [("bezier", "in"), ("animations:bezier", "flat")]);
    let lines: Vec<_> = document
        .diagnostics
        .iter()
        .map(|d| (d.line, d.message.as_str()))
        .collect();
    assert_eq!(
        lines,
        [(5, "config option <animations:inner:bezier> does not exist.")]
    );
}

// -- source -------------------------------------------------------------------

#[test]
fn source_reads_relative_home_and_globbed_paths_in_place() {
    let root = scratch("source");
    let conf = root.join("hypr");
    let parts = conf.join("parts");
    std::fs::create_dir_all(&parts).expect("a parts directory");
    std::fs::write(conf.join("colors.conf"), "$accent = ##ff0000\n").expect("write");
    std::fs::write(parts.join("b.conf"), "listener {\n  timeout = 2\n}\n").expect("write");
    std::fs::write(
        parts.join("a.conf"),
        "listener {\n  timeout = 1\n}\nsource = ../nested/c.conf\n",
    )
    .expect("write");
    std::fs::write(parts.join("skip.txt"), "listener {\n  timeout = 99\n}\n").expect("write");
    std::fs::write(
        parts.join(".hidden.conf"),
        "listener {\n  timeout = 98\n}\n",
    )
    .expect("write");
    std::fs::create_dir_all(conf.join("nested")).expect("a nested directory");
    std::fs::write(
        conf.join("nested").join("c.conf"),
        "general:unlock_cmd = from c\n}\n",
    )
    .expect("write");
    std::fs::write(root.join("home.conf"), "general:after_sleep_cmd = home\n").expect("write");

    let main = conf.join("hypridle.conf");
    let text = "source = colors.conf\ngeneral:lock_cmd = $accent\nsource = ./parts/*.conf\nsource = ~/home.conf\n";
    std::fs::write(&main, text).expect("write");
    let schema = hypridle().environment([("HOME", root.display().to_string())]);
    let document = parse_file(&schema, &main).expect("the file reads");

    assert_eq!(document.get("general:lock_cmd"), Some("#ff0000"));
    assert_eq!(document.get("general:unlock_cmd"), Some("from c"));
    assert_eq!(document.get("general:after_sleep_cmd"), Some("home"));
    let timeouts: Vec<_> = document
        .instances_of("listener")
        .filter_map(|l| l.get("timeout"))
        .collect();
    assert_eq!(
        timeouts,
        ["1", "2"],
        "the glob is sorted, and skips .hidden and .txt"
    );
    let listener = document
        .instances_of("listener")
        .next()
        .expect("a listener");
    assert_eq!(listener.file, parts.join("a.conf"));
    let lines: Vec<_> = document
        .diagnostics
        .iter()
        .map(|d| (d.file.clone(), d.line, d.message.as_str()))
        .collect();
    assert_eq!(
        lines,
        [(
            conf.join("nested").join("c.conf"),
            2,
            "Stray category close"
        )],
        "a sourced file's diagnostics name it"
    );
    std::fs::remove_dir_all(&root).expect("clean up");
}

#[test]
fn source_complains_as_hyprlock_does() {
    let root = scratch("source-errors");
    let main = root.join("hyprlock.conf");
    let text = "source = x\nsource = missing.conf\nsource = none-*.conf\ngeneral {\n  source = missing.conf\n}\n";
    std::fs::write(&main, text).expect("write");
    let document = parse_file(&hyprlock(), &main).expect("the file reads");
    let lines: Vec<_> = document
        .diagnostics
        .iter()
        .map(|d| (d.line, d.message.as_str()))
        .collect();
    assert_eq!(
        lines,
        [
            (1, "source path x bogus!"),
            (2, "source= globbing error: found no match"),
            (3, "source= globbing error: found no match"),
            (5, "source= globbing error: found no match"),
        ],
        "source is a keyword in any category"
    );
    std::fs::remove_dir_all(&root).expect("clean up");
}

#[test]
fn source_reads_each_file_once_so_loops_end() {
    let root = scratch("source-loop");
    let main = root.join("hypridle.conf");
    std::fs::write(
        &main,
        "source = hypridle.conf\nsource = other.conf\nsource = other.conf\n",
    )
    .expect("write");
    std::fs::write(
        root.join("other.conf"),
        "source = hypridle.conf\nsource = other.conf\nlistener {\n  timeout = 1\n}\n",
    )
    .expect("write");
    let document = parse_file(&hypridle(), &main).expect("the file reads");
    assert_eq!(document.diagnostics, []);
    assert_eq!(document.instances.len(), 1);
    std::fs::remove_dir_all(&root).expect("clean up");
}

#[test]
fn source_nesting_is_bounded() {
    let root = scratch("source-depth");
    let files = MAX_SOURCE_DEPTH + 2;
    for index in 0..files {
        let text = format!("source = {}.conf\n", index + 1);
        std::fs::write(root.join(format!("{index}.conf")), text).expect("write");
    }
    let document = parse_file(&hypridle(), &root.join("0.conf")).expect("the file reads");
    let messages: Vec<_> = document
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [format!("source= nesting deeper than {MAX_SOURCE_DEPTH} files").as_str()]
    );
    std::fs::remove_dir_all(&root).expect("clean up");
}

#[test]
fn a_sourced_file_that_cannot_be_read_is_a_diagnostic_about_it() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = scratch("source-unreadable");
    let locked = root.join("locked.conf");
    std::fs::write(&locked, "general:lock_cmd = secret\n").expect("write");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let main = root.join("hypridle.conf");
    std::fs::write(&main, "source = locked.conf\n").expect("write");
    let readable = std::fs::read(&locked).is_ok();
    let document = parse_file(&hypridle(), &main).expect("the file reads");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    std::fs::remove_dir_all(&root).expect("clean up");
    if readable {
        // Root reads it anyway; there is nothing to see.
        return;
    }
    let diagnostic = document.diagnostics.first().expect("a diagnostic");
    assert_eq!(
        (
            diagnostic.file.as_path(),
            diagnostic.line,
            diagnostic.message.as_str()
        ),
        (locked.as_path(), 0, "File failed to open")
    );
    assert_eq!(document.get("general:lock_cmd"), None);
}

#[test]
fn source_is_only_read_when_the_schema_asks() {
    let schema = Schema::new().keyword("source");
    let document = clean(&schema, "source = /nowhere/at/all.conf\n");
    let keywords: Vec<_> = document
        .keywords_named("source")
        .map(|k| k.value.as_str())
        .collect();
    assert_eq!(keywords, ["/nowhere/at/all.conf"]);
}

#[test]
fn a_file_that_cannot_be_read_is_an_error_for_the_caller() {
    let missing = Path::new("/nonexistent/compositor-hyprlang/hyprlock.conf");
    assert!(parse_file(&hyprlock(), missing).is_err());
}

#[test]
fn a_glob_matches_stars_and_question_marks() {
    assert!(matches("*.conf", "a.conf"));
    assert!(matches("*.conf", ".conf"));
    assert!(!matches("*.conf", "a.conf.bak"));
    assert!(matches("a?c", "abc"));
    assert!(!matches("a?c", "ac"));
    assert!(matches("*a*b*", "xxaYYbzz"));
    assert!(!matches("*a*b", "xxaYYbzz"));
    assert!(matches("**", ""));
    assert!(matches("é?", "éß"));
}

// -- Values -------------------------------------------------------------------

#[test]
fn int_reads_as_config_string_to_int_does() {
    assert_eq!(value::int("0xFF"), Ok(255));
    assert_eq!(value::int("0x"), Err("invalid hex 0x".to_owned()));
    assert_eq!(value::int("0xZZ"), Err("invalid hex 0xZZ".to_owned()));
    assert_eq!(
        value::int("0xffffffffffffffffff"),
        Err("invalid hex 0xffffffffffffffffff".to_owned())
    );
    assert_eq!(value::int("rgba(255, 0, 0, 0.5)"), Ok(0x80FF_0000));
    assert_eq!(value::int("rgba(242, 243, 244, 0.75)"), Ok(0xBFF2_F3F4));
    assert_eq!(value::int("rgba(ff000080)"), Ok(0x80FF_0000));
    assert_eq!(
        value::int("rgba(0, 0, 0, 2)"),
        Ok(0xFE00_0000),
        "alpha wraps as uint8_t"
    );
    assert_eq!(
        value::int("rgba(0x10, true, 0, 1)"),
        Ok(0xFF10_0100),
        "parts are ints"
    );
    assert_eq!(value::int("rgb(10, 20, 30)"), Ok(0xFF0A_141E));
    assert_eq!(value::int("rgb(0a141e)"), Ok(0xFF0A_141E));
    assert_eq!(value::int("true"), Ok(1));
    assert_eq!(value::int("yes please"), Ok(1));
    assert_eq!(value::int("one"), Ok(1), "a word starting `on`");
    assert_eq!(value::int("off"), Ok(0));
    assert_eq!(value::int("none"), Ok(0), "a word starting `no`");
    assert_eq!(value::int("-12"), Ok(-12));
    assert_eq!(value::int("007"), Ok(7));
    assert_eq!(
        value::int("1.5"),
        Err("cannot parse \"1.5\" as an int.".to_owned())
    );
    assert_eq!(
        value::int(""),
        Err("cannot parse \"\" as an int.".to_owned())
    );
    assert_eq!(
        value::int("-"),
        Err("cannot parse \"-\" as an int.".to_owned())
    );
    assert_eq!(
        value::int(" 1"),
        Err("cannot parse \" 1\" as an int.".to_owned())
    );
    assert_eq!(
        value::int("99999999999999999999"),
        Err("stoll threw: stoll".to_owned())
    );
    assert_eq!(
        value::int("rgba(1, 2)"),
        Err(
            "rgba() expects length of 8 characters (4 bytes) or 4 comma separated values"
                .to_owned()
        )
    );
    assert_eq!(
        value::int("rgb(1234567)"),
        Err(
            "rgb() expects length of 6 characters (3 bytes) or 3 comma separated values".to_owned()
        )
    );
    assert_eq!(
        value::int("rgb(1, x, 3)"),
        Err("failed parsing 1, x, 3".to_owned())
    );
    assert_eq!(
        value::int("rgba(1, 2, 3, x)"),
        Err("failed parsing 1, 2, 3, x".to_owned())
    );
    assert_eq!(
        value::int("rgb(zzzzzz)"),
        Err("invalid hex zzzzzz".to_owned())
    );
}

#[test]
fn color_splits_argb() {
    assert_eq!(
        value::color("rgba(242, 243, 244, 0.75)"),
        Ok((242, 243, 244, 191))
    );
    assert_eq!(value::color("rgb(0a141e)"), Ok((10, 20, 30, 255)));
    assert!(value::color("blue").is_err());
}

#[test]
fn float_reads_the_leading_number_as_stof_does() {
    assert_eq!(value::float("1.5"), Ok(1.5));
    assert_eq!(value::float("  -2"), Ok(-2.0));
    assert_eq!(value::float("520,"), Ok(520.0));
    assert_eq!(value::float("8.5px"), Ok(8.5));
    assert_eq!(value::float(".5"), Ok(0.5));
    assert_eq!(value::float("5."), Ok(5.0));
    assert_eq!(value::float("1e3x"), Ok(1000.0));
    assert_eq!(value::float("1e"), Ok(1.0));
    assert_eq!(value::float("0x10"), Ok(16.0));
    assert_eq!(value::float("0x1.8p1"), Ok(3.0));
    assert_eq!(value::float("0xg"), Ok(0.0));
    assert_eq!(value::float("inf"), Ok(f64::INFINITY));
    assert!(value::float("nan").is_ok_and(f64::is_nan));
    let failed = Err("failed parsing a float: stof".to_owned());
    assert_eq!(value::float(""), failed);
    assert_eq!(value::float("px"), failed);
    assert_eq!(value::float("."), failed);
    assert_eq!(value::float("1e39"), failed, "out of a float's range");
    assert_eq!(value::float("1e-50"), failed, "underflows a float");
}

#[test]
fn vec2_splits_at_one_space_as_hyprlang_does() {
    assert_eq!(
        value::vec2("520, 190"),
        Ok((520.0, 190.0)),
        "stof reads `520,` as 520"
    );
    assert_eq!(value::vec2("0 -20"), Ok((0.0, -20.0)));
    assert_eq!(value::vec2("-0.5, 1e2"), Ok((-0.5, 100.0)));
    assert_eq!(
        value::vec2("520,190"),
        Err("failed parsing a vec2: no space".to_owned())
    );
    assert_eq!(
        value::vec2("520\t190"),
        Err("failed parsing a vec2: no space".to_owned())
    );
    assert_eq!(
        value::vec2("520,  190"),
        Err("failed parsing a vec2: too many args".to_owned())
    );
    assert_eq!(
        value::vec2("1 2 3"),
        Err("failed parsing a vec2: too many args".to_owned())
    );
    assert_eq!(
        value::vec2("a 2"),
        Err("failed parsing a vec2: stof".to_owned())
    );
    assert_eq!(
        value::vec2("1 "),
        Err("failed parsing a vec2: stof".to_owned())
    );
}
