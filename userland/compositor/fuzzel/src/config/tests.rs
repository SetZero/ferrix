//! `fuzzel.ini` against fuzzel's `config.c`.

use std::path::Path;

use super::{
    Action, Anchor, Diagnostic, Env, Fields, KeyboardFocus, Layer, Level, Loaded, MatchMode, Mods,
    PtOrPx, Rgba, from_text, load, search_path,
};

fn parse(text: &str) -> Loaded {
    from_text(
        text,
        Path::new("/test/fuzzel.ini"),
        &Env::default(),
        &[],
        false,
    )
}

fn errors(loaded: &Loaded) -> Vec<String> {
    loaded
        .diagnostics
        .iter()
        .filter(|d| d.level == Level::Error)
        .map(|d| d.text.clone())
        .collect()
}

/// An excerpt of the user's own file: every kind of line it has.
const EXCERPT: &str = "\
# Frosted-glass app launcher, centred on screen.

[main]
font=GFS Didot:size=16
terminal=foot
# \"overlay\" keeps it above everything, including fullscreen windows.
layer=overlay
anchor=center
width=42
lines=12
horizontal-pad=32
vertical-pad=26
inner-pad=14
line-height=26
letter-spacing=0

icons-enabled=yes
image-size-ratio=0.32
prompt=\"  \"
placeholder=Search…
filter-desktop=yes

[colors]
# Alpha is the last byte.
background=1a1b268c
text=c0caf5ff
match=21ccffff
selection=21ccff3d
border=21ccff73

[border]
width=2
radius=20

[dmenu]
exit-immediately-if-empty=yes
";

#[test]
fn the_users_excerpt_is_taken_whole() {
    let loaded = parse(EXCERPT);
    assert!(loaded.ok);
    assert_eq!(errors(&loaded), Vec::<String>::new());
    let c = &loaded.config;
    assert_eq!(c.font, "GFS Didot:size=16");
    assert_eq!(c.terminal.as_deref(), Some("foot"));
    assert_eq!(c.layer, Layer::Overlay);
    assert_eq!(c.anchor, Anchor(0));
    assert_eq!((c.chars, c.lines), (42, 12));
    assert_eq!((c.pad.x, c.pad.y, c.pad.inner), (32, 26, 14));
    assert_eq!(c.line_height, Some(PtOrPx::Pt(26.0)));
    assert_eq!(c.letter_spacing, PtOrPx::Pt(0.0));
    assert!(c.icons_enabled && c.filter_desktop);
    assert!((c.image_size_ratio - 0.32).abs() < 1e-6);
    // Quoted, so the two spaces are kept.
    assert_eq!(c.prompt, "  ");
    assert_eq!(c.placeholder, "Search…");
    assert_eq!(c.colors.background, Rgba(0x1a1b_268c));
    assert_eq!(c.colors.selection, Rgba(0x21cc_ff3d));
    assert_eq!((c.border.width, c.border.radius), (2, 20));
    assert!(c.dmenu.exit_immediately_if_empty);
    // What the file does not say is fuzzel's default.
    assert_eq!(c.namespace, "launcher");
    assert_eq!(c.match_mode, MatchMode::Fzf);
    assert_eq!(c.colors.prompt, Rgba(0x586e_75ff));
    assert_eq!(c.keyboard_focus, KeyboardFocus::Exclusive);
}

#[test]
fn defaults_are_fuzzels() {
    let loaded = parse("");
    let c = &loaded.config;
    assert_eq!(c.prompt, "> ");
    assert_eq!(c.font, "monospace");
    assert_eq!(
        c.fields,
        Fields(Fields::FILENAME | Fields::NAME | Fields::GENERIC)
    );
    assert_eq!((c.lines, c.chars, c.tabs), (15, 30, 8));
    assert_eq!((c.pad.x, c.pad.y, c.pad.inner), (40, 8, 0));
    assert_eq!((c.border.width, c.border.radius), (1, 10));
    assert_eq!(c.line_height, None);
    assert_eq!(c.icon_theme, "default");
    assert!((c.image_size_ratio - 0.5).abs() < 1e-6);
    assert_eq!(c.colors.background, Rgba(0xfdf6_e3ff));
    assert_eq!(c.delayed_filter_limit, 20000);
    assert_eq!(c.bindings.len(), 73);
}

#[test]
fn an_unknown_option_is_said_in_fuzzels_words() {
    // `fuzzel --check-config` on this file printed
    //  err: config.c:1013: …/bad.ini:2: [main].bogus: 1: not a valid option: bogus
    let loaded = parse("[main]\nbogus=1\nlines=x\n[colors]\nbackground=123\n");
    assert_eq!(
        errors(&loaded),
        vec![
            "/test/fuzzel.ini:2: [main].bogus: 1: not a valid option: bogus",
            "/test/fuzzel.ini:3: [main].lines: x: invalid integer value, or outside range 0-4294967295",
            "/test/fuzzel.ini:5: [colors].background: 123: not a valid color value",
        ]
    );
    // Not checking, so the file still loads.
    assert!(loaded.ok);
    assert_eq!(loaded.config.lines, 15);
}

#[test]
fn check_config_stops_at_the_first_error() {
    let loaded = from_text(
        "bogus=1\nlines=x\n",
        Path::new("/f.ini"),
        &Env::default(),
        &[],
        true,
    );
    assert!(!loaded.ok);
    assert_eq!(errors(&loaded).len(), 1);
}

#[test]
fn comments_need_a_blank_before_them() {
    let loaded = parse("lines=3 # three\nplaceholder=a#b\nbackground=#11223344\n");
    assert_eq!(loaded.config.lines, 3);
    assert_eq!(loaded.config.placeholder, "a#b");
    // `background` is not a [main] option.
    assert_eq!(errors(&loaded).len(), 1);
    let colors = parse("[colors]\nbackground=#11223344\n");
    assert_eq!(colors.config.colors.background, Rgba(0x1122_3344));
}

#[test]
fn quotes_and_escapes() {
    let loaded = parse("prompt=\"a \\\"b\\\" \\\\c\"\nplaceholder='it\\'s'\nmessage=\"unclosed\n");
    assert_eq!(loaded.config.prompt, "a \"b\" \\c");
    assert_eq!(loaded.config.placeholder, "it's");
    assert_eq!(loaded.config.message.as_deref(), Some("\"unclosed"));
    // Trailing spaces go unless quoted.
    assert_eq!(parse("prompt=>   \n").config.prompt, ">");
}

#[test]
fn syntax_errors() {
    let loaded =
        parse("=x\nlines\nlines=\n[]\n[colors\n[colors]x\n[nope]\nlines=4\n[main]\nlines=5\n");
    assert_eq!(
        errors(&loaded),
        vec![
            "/test/fuzzel.ini:1: [main]: syntax error: key/value pair has no key",
            "/test/fuzzel.ini:2: [main].lines: syntax error: key/value pair has no value",
            "/test/fuzzel.ini:3: [main].lines: syntax error: key/value pair has no value",
            "/test/fuzzel.ini:4: [main]: empty section name",
            "/test/fuzzel.ini:5: [colors]: syntax error: no closing ']'",
            "/test/fuzzel.ini:6: [colors]: section declaration contains trailing characters",
            "/test/fuzzel.ini:7: [nope]: invalid section name: nope",
        ]
    );
    // The key under an invalid section is ignored; the one under [main] is not.
    assert_eq!(loaded.config.lines, 5);
}

#[test]
fn values_by_type() {
    let loaded = parse(
        "icons-enabled=maybe\nimage-size-ratio=1.5\nanchor=middle\nlayer=bottom\n\
         match-mode=regex\nfields=name,bogus\nline-height=12pixels\nline-height=20px\n\
         password-character=ab\ndpi-aware=auto\nfields=exec,,keywords\nlines=-0\nwidth=+7\n",
    );
    assert_eq!(
        errors(&loaded),
        vec![
            "/test/fuzzel.ini:1: [main].icons-enabled: maybe: invalid boolean value",
            "/test/fuzzel.ini:2: [main].image-size-ratio: 1.5: not in range 0.0 - 1.0",
            "/test/fuzzel.ini:3: [main].anchor: middle: invalid anchor \"middle\", must be one of: \
             \"center\", \"top-left\", \"top\", \"top-right\", \"right\", \"bottom-right\", \
             \"bottom\"\"bottom-left\", \"left\"",
            "/test/fuzzel.ini:4: [main].layer: bottom: not one of 'top', 'overlay'",
            "/test/fuzzel.ini:5: [main].match-mode: regex: not one of 'exact', 'fzf', 'fuzzy'",
            "/test/fuzzel.ini:6: [main].fields: name,bogus: invalid field name \"bogus\", must be \
             one of: \"filename\", \"name\", \"generic\", \"exec\", \"categories\", \"keywords\", \
             \"comment\"",
            "/test/fuzzel.ini:7: [main].line-height: 12pixels: invalid decimal value",
            "/test/fuzzel.ini:9: [main].password-character: ab: password character must be a \
             single character, or empty",
        ]
    );
    let c = &loaded.config;
    assert_eq!(c.line_height, Some(PtOrPx::Px(20)));
    assert_eq!(c.fields, Fields(Fields::EXEC | Fields::KEYWORDS));
    assert_eq!((c.lines, c.chars), (0, 7));
}

#[test]
fn key_bindings() {
    let loaded = parse(
        "[key-bindings]\nnext=Control+j Mod1+Down\nprev=none\ncancel=Hyper+x\n\
         execute=Control+Retrun\nfly=Escape\ncustom-20=a\nfirst=Control+j\n",
    );
    let binds = &loaded.config.bindings;
    let next: Vec<_> = binds.iter().filter(|b| b.action == Action::Next).collect();
    assert_eq!(next.len(), 2);
    assert_eq!(
        next.first().map(|b| (b.sym, b.mods.ctrl)),
        Some(("j", true))
    );
    assert!(!binds.iter().any(|b| b.action == Action::Prev));
    assert_eq!(
        errors(&loaded),
        vec![
            "/test/fuzzel.ini:4: [key-bindings].cancel: Hyper+x: not a valid modifier name: Hyper",
            "/test/fuzzel.ini:5: [key-bindings].execute: Control+Retrun: not a valid XKB key name: \
             Retrun",
            "/test/fuzzel.ini:6: [key-bindings].fly: Escape: not a valid action: fly",
            "/test/fuzzel.ini:7: [key-bindings].custom-20: a: not a valid action: custom-20",
            "/test/fuzzel.ini:8: [key-bindings].first: Control+j already mapped to 'next'",
        ]
    );
    // The colliding binding is gone; the default `first` went when it was
    // rebound.
    assert!(!binds.iter().any(|b| b.action == Action::First));
    // A failed rebinding leaves the defaults.
    assert_eq!(
        binds.iter().filter(|b| b.action == Action::Cancel).count(),
        4
    );
}

#[test]
fn a_collision_with_a_default_is_refused() {
    // Control+k is delete-line-forward's by default; binding it to `next`
    // without unmapping that is the man page's own example of a collision.
    let loaded = parse("[key-bindings]\nnext=Control+k\n");
    assert_eq!(
        errors(&loaded),
        vec![
            "/test/fuzzel.ini:2: [key-bindings].next: Control+k already mapped to 'delete-line-forward'"
        ]
    );
    let unmapped = parse("[key-bindings]\ndelete-line-forward=none\nnext=Control+k\n");
    assert_eq!(errors(&unmapped), Vec::<String>::new());
    assert!(
        unmapped
            .config
            .bindings
            .iter()
            .any(|b| b.action == Action::Next
                && b.sym == "k"
                && b.mods
                    == Mods {
                        ctrl: true,
                        ..Mods::default()
                    })
    );
}

#[test]
fn overrides() {
    let overrides = vec![
        "lines=4".to_owned(),
        "colors.text=11223344".to_owned(),
        "nope.lines=2".to_owned(),
        ".lines=2".to_owned(),
    ];
    let loaded = from_text("", Path::new("/f"), &Env::default(), &overrides, false);
    assert_eq!(loaded.config.lines, 4);
    assert_eq!(loaded.config.colors.text, Rgba(0x1122_3344));
    assert_eq!(
        errors(&loaded),
        vec![
            "override:3: [nope].lines: 2: invalid section name: nope",
            "override:4: [].lines: 2: empty section name",
        ]
    );
}

#[test]
fn no_file_means_defaults_and_no_overrides() {
    let env = Env {
        home: Some("/nonexistent-fuzzel-home".to_owned()),
        xdg_config_dirs: Some("/nonexistent-fuzzel-dirs".to_owned()),
        ..Env::default()
    };
    let loaded = load(None, &env, &["lines=3".to_owned()], false);
    assert!(loaded.ok);
    assert_eq!(loaded.config.lines, 15);
    assert_eq!(
        loaded.diagnostics,
        vec![Diagnostic {
            level: Level::Warning,
            text: "no configuration found, using defaults".to_owned()
        }]
    );
}

#[test]
fn where_the_file_is_looked_for() {
    let env = Env {
        home: Some("/home/u".to_owned()),
        xdg_config_home: Some("relative".to_owned()),
        xdg_config_dirs: Some("/a:/b".to_owned()),
        terminal: None,
    };
    let paths: Vec<String> = search_path(&env)
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert_eq!(
        paths,
        vec![
            "/home/u/.config/fuzzel/fuzzel.ini",
            "/a/fuzzel/fuzzel.ini",
            "/b/fuzzel/fuzzel.ini"
        ]
    );
    let env = Env {
        xdg_config_home: Some("/x".to_owned()),
        ..Env::default()
    };
    let paths: Vec<String> = search_path(&env)
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    assert_eq!(
        paths,
        vec!["/x/fuzzel/fuzzel.ini", "/etc/xdg/fuzzel/fuzzel.ini"]
    );
}

#[test]
fn terminal_comes_from_the_environment_first() {
    let env = Env {
        terminal: Some("term".to_owned()),
        ..Env::default()
    };
    let loaded = from_text("", Path::new("/f"), &env, &[], false);
    assert_eq!(loaded.config.terminal.as_deref(), Some("term -e"));
    let loaded = from_text("terminal=foot", Path::new("/f"), &env, &[], false);
    assert_eq!(loaded.config.terminal.as_deref(), Some("foot"));
}

#[test]
fn include_has_its_own_section_scope() {
    let dir = crate::testdir::TestDir::new("include");
    let included = dir.write("inc.ini", "[colors]\ntext=01020304\n");
    let text = format!(
        "include={}\nlines=9\ninclude=relative.ini\ninclude=~/nope.ini\n",
        included.display()
    );
    let env = Env {
        home: Some(dir.path().display().to_string()),
        ..Env::default()
    };
    let loaded = from_text(&text, Path::new("/f"), &env, &[], false);
    assert_eq!(loaded.config.colors.text, Rgba(0x0102_0304));
    // Back in [main] after the include.
    assert_eq!(loaded.config.lines, 9);
    assert_eq!(
        errors(&loaded),
        vec![
            "/f:3: [main].include: relative.ini: not an absolute path".to_owned(),
            format!(
                "/f:4: [main].include: ~/nope.ini: failed to open: No such file or directory (2)"
            ),
        ]
    );
}

#[test]
fn premultiplied_colours() {
    assert_eq!(Rgba(0xffff_ffff).premultiplied(), 0xffff_ffff);
    assert_eq!(Rgba(0x1a1b_268c).premultiplied() >> 24, 0x8c);
    assert_eq!(Rgba(0xff00_0080).premultiplied(), 0x8080_0000);
    assert_eq!(Rgba(0x1234_5600).premultiplied(), 0);
}
