//! The command line: fuzzel's `getopt_long` table and what each option
//! overrides.
//!
//! Every option fuzzel 1.12 takes is accepted, with its short letter, its
//! argument rules (`--lines=5`, `--lines 5`, `-l5`, `-l 5`; an optional
//! argument only after `=`), getopt's unambiguous abbreviations
//! (`--dm` is not one, `--dmenu0` and `--dmenu` share it), and fuzzel's own
//! message for a bad value. An option's value is checked when it is read,
//! as fuzzel checks it before it loads the configuration, and applied after
//! the configuration, which it overrides.

use crate::config::{
    ANCHORS, Config, DmenuMode, DpiAware, Fields, KeyboardFocus, Layer, MatchMode, MessageMode,
    PtOrPx, Rgba, ScalingFilter,
};
use crate::dmenu::nth_option;

/// How an option takes its argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arg {
    None,
    Required,
    Optional,
}

/// What an option does to the configuration.
type Setter = fn(&mut Config, Option<&str>) -> Result<(), String>;

/// One option: its long name, its short letter, its argument, and either a
/// setter or a meaning of its own.
struct Opt {
    long: &'static str,
    short: Option<char>,
    arg: Arg,
    set: Option<Setter>,
}

/// `sscanf("%u")`: leading space, digits, anything after ignored.
fn scan_u32(value: &str) -> Option<u32> {
    let value = value.trim_start();
    let value = value.strip_prefix('+').unwrap_or(value);
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn colour(name: &str, value: &str) -> Result<Rgba, String> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.len() == 8
        && let Ok(v) = u32::from_str_radix(digits, 16)
    {
        return Ok(Rgba(v));
    }
    Err(format!("{name}: {value}: invalid color"))
}

fn pt_or_px(value: &str) -> Result<PtOrPx, String> {
    if let Some(number) = value.strip_suffix("px") {
        return number
            .parse::<i64>()
            .map(PtOrPx::Px)
            .map_err(|_| format!("{value}: invalid px value (must be on the form 12px)"));
    }
    value
        .parse::<f32>()
        .map(PtOrPx::Pt)
        .map_err(|_| format!("{value}: invalid size"))
}

/// The value an option with a required argument was given.
fn need(value: Option<&str>) -> &str {
    value.unwrap_or_default()
}

macro_rules! number {
    ($field:expr, $message:literal) => {{
        fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
            let v = need(v);
            let n = scan_u32(v).ok_or_else(|| format!(concat!("{}: ", $message), v))?;
            $field(c, n);
            Ok(())
        }
        Some(set as Setter)
    }};
}

macro_rules! color {
    ($name:literal, $field:ident) => {{
        fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
            c.colors.$field = colour($name, need(v))?;
            Ok(())
        }
        Some(set as Setter)
    }};
}

macro_rules! flag {
    ($body:expr) => {{
        fn set(c: &mut Config, _: Option<&str>) -> Result<(), String> {
            $body(c);
            Ok(())
        }
        Some(set as Setter)
    }};
}

macro_rules! text {
    ($body:expr) => {{
        fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
            $body(c, need(v).to_owned());
            Ok(())
        }
        Some(set as Setter)
    }};
}

/// fuzzel's `longopts`, in its order.
fn table() -> Vec<Opt> {
    let o = |long, short, arg, set| Opt {
        long,
        short,
        arg,
        set,
    };
    use Arg::{None as N, Optional as O, Required as R};
    vec![
        o("config", None, R, None),
        o("check-config", None, N, None),
        o(
            "namespace",
            Some('n'),
            R,
            text!(|c: &mut Config, v| c.namespace = v),
        ),
        o(
            "cache",
            None,
            R,
            text!(|c: &mut Config, v| c.cache = Some(v)),
        ),
        o(
            "output",
            Some('o'),
            R,
            text!(|c: &mut Config, v| c.output = Some(v)),
        ),
        o("font", Some('f'), R, text!(|c: &mut Config, v| c.font = v)),
        o(
            "use-bold",
            None,
            N,
            flag!(|c: &mut Config| c.use_bold = true),
        ),
        o("dpi-aware", Some('D'), R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                c.dpi_aware = match need(v) {
                    "auto" => DpiAware::Auto,
                    "no" => DpiAware::No,
                    "yes" => DpiAware::Yes,
                    other => {
                        return Err(format!(
                            "{other}: invalid value for dpi-aware: must be one of 'auto', 'no', or 'yes'"
                        ));
                    }
                };
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "gamma-correct",
            None,
            N,
            flag!(|c: &mut Config| c.gamma_correct = true),
        ),
        o(
            "icon-theme",
            None,
            R,
            text!(|c: &mut Config, v| c.icon_theme = v),
        ),
        o(
            "no-icons",
            Some('I'),
            N,
            flag!(|c: &mut Config| c.icons_enabled = false),
        ),
        o(
            "hide-before-typing",
            None,
            N,
            flag!(|c: &mut Config| c.hide_before_typing = true),
        ),
        o("fields", Some('F'), R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                let mut fields = 0;
                for f in need(v).split([',', ' ']).filter(|f| !f.is_empty()) {
                    fields |= match f {
                        "filename" => Fields::FILENAME,
                        "name" => Fields::NAME,
                        "generic" => Fields::GENERIC,
                        "exec" => Fields::EXEC,
                        "categories" => Fields::CATEGORIES,
                        "keywords" => Fields::KEYWORDS,
                        "comment" => Fields::COMMENT,
                        _ => {
                            return Err(format!(
                                "{f}: invalid field name: must be one of 'filename', 'name', \
                                 'generic', 'exec', 'categories', 'keywords', 'comment'"
                            ));
                        }
                    };
                }
                c.fields = Fields(fields);
                Ok(())
            }
            Some(set as Setter)
        }),
        o("password", None, O, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                if let Some(v) = v {
                    let mut chars = v.chars();
                    let first = chars.next();
                    if chars.next().is_some() {
                        return Err(format!(
                            "{v}: password character must be a single character"
                        ));
                    }
                    c.password.character = first;
                    c.password.character_set = true;
                } else if !c.password.character_set {
                    c.password.character = Some('*');
                }
                c.password.enabled = true;
                Ok(())
            }
            Some(set as Setter)
        }),
        o("anchor", Some('a'), R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                let v = need(v);
                c.anchor = ANCHORS
                    .iter()
                    .find(|(name, _)| *name == v)
                    .map(|(_, a)| *a)
                    .ok_or_else(|| format!("{v}: invalid anchor"))?;
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "x-margin",
            None,
            R,
            number!(
                |c: &mut Config, n| c.x_margin = n,
                "invalid horizontal margin"
            ),
        ),
        o(
            "y-margin",
            None,
            R,
            number!(
                |c: &mut Config, n| c.y_margin = n,
                "invalid vertical margin"
            ),
        ),
        o("select", None, R, None),
        o("select-index", None, R, None),
        o(
            "lines",
            Some('l'),
            R,
            number!(|c: &mut Config, n| c.lines = n, "invalid line count"),
        ),
        o(
            "minimal-lines",
            None,
            N,
            flag!(|c: &mut Config| c.minimal_lines = true),
        ),
        o(
            "hide-prompt",
            None,
            N,
            flag!(|c: &mut Config| c.hide_prompt = true),
        ),
        o(
            "width",
            Some('w'),
            R,
            number!(|c: &mut Config, n| c.chars = n, "invalid width"),
        ),
        o(
            "tabs",
            None,
            R,
            number!(|c: &mut Config, n| c.tabs = n, "invalid tab count"),
        ),
        o(
            "horizontal-pad",
            Some('x'),
            R,
            number!(|c: &mut Config, n| c.pad.x = n, "invalid padding"),
        ),
        o(
            "vertical-pad",
            Some('y'),
            R,
            number!(|c: &mut Config, n| c.pad.y = n, "invalid padding"),
        ),
        o(
            "inner-pad",
            Some('P'),
            R,
            number!(|c: &mut Config, n| c.pad.inner = n, "invalid padding"),
        ),
        o(
            "background-color",
            Some('b'),
            R,
            color!("background-color", background),
        ),
        o("text-color", Some('t'), R, color!("text-color", text)),
        o("message-color", None, R, color!("message-color", message)),
        o("prompt-color", None, R, color!("prompt-color", prompt)),
        o(
            "placeholder-color",
            None,
            R,
            color!("placeholder-color", placeholder),
        ),
        // fuzzel's message for this one says prompt-color.
        o("input-color", None, R, color!("prompt-color", input)),
        o("match-color", Some('m'), R, color!("match-color", matched)),
        o(
            "selection-color",
            Some('s'),
            R,
            color!("selection-color", selection),
        ),
        o(
            "selection-text-color",
            Some('S'),
            R,
            color!("selection-text-color", selection_text),
        ),
        o(
            "selection-match-color",
            Some('M'),
            R,
            color!("selection-match-color", selection_match),
        ),
        o(
            "selection-radius",
            None,
            R,
            number!(
                |c: &mut Config, n| c.border.selection_radius = n,
                "invalid selection border radius (must be an integer)"
            ),
        ),
        o("counter-color", None, R, color!("counter-color", counter)),
        o(
            "border-width",
            Some('B'),
            R,
            number!(
                |c: &mut Config, n| c.border.width = n,
                "invalid border width (must be an integer)"
            ),
        ),
        o(
            "border-radius",
            Some('r'),
            R,
            number!(
                |c: &mut Config, n| c.border.radius = n,
                "invalid border radius (must be an integer)"
            ),
        ),
        o("border-color", Some('C'), R, color!("border-color", border)),
        o(
            "prompt",
            Some('p'),
            R,
            text!(|c: &mut Config, v| c.prompt = v),
        ),
        o(
            "prompt-only",
            None,
            R,
            text!(|c: &mut Config, v| {
                c.prompt = v;
                c.prompt_only = true;
            }),
        ),
        o(
            "placeholder",
            None,
            R,
            text!(|c: &mut Config, v| c.placeholder = v),
        ),
        o(
            "search",
            None,
            R,
            text!(|c: &mut Config, v| c.search_text = v),
        ),
        o(
            "terminal",
            Some('T'),
            R,
            text!(|c: &mut Config, v| c.terminal = Some(v)),
        ),
        o(
            "show-actions",
            None,
            N,
            flag!(|c: &mut Config| c.show_actions = true),
        ),
        o("match-mode", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                c.match_mode = match need(v) {
                    "exact" => MatchMode::Exact,
                    "fzf" => MatchMode::Fzf,
                    "fuzzy" => MatchMode::Fuzzy,
                    other => {
                        return Err(format!(
                            "{other}: invalid match-mode. Must be 'exact', 'fuzzy' or 'fzf'"
                        ));
                    }
                };
                Ok(())
            }
            Some(set as Setter)
        }),
        o("filter-desktop", None, O, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                match v {
                    None => c.filter_desktop = true,
                    Some(v) if v.eq_ignore_ascii_case("no") => c.filter_desktop = false,
                    Some(v) => return Err(format!("{v}: invalid filter-desktop option")),
                }
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "fuzzy-min-length",
            None,
            R,
            number!(
                |c: &mut Config, n| c.fuzzy.min_length = n as usize,
                "invalid fuzzy min length (must be an integer)"
            ),
        ),
        o(
            "fuzzy-max-length-discrepancy",
            None,
            R,
            number!(
                |c: &mut Config, n| c.fuzzy.max_length_discrepancy = n as usize,
                "invalid fuzzy max length discrepancy (must be an integer)"
            ),
        ),
        o(
            "fuzzy-max-distance",
            None,
            R,
            number!(
                |c: &mut Config, n| c.fuzzy.max_distance = n as usize,
                "invalid fuzzy max distance (must be an integer)"
            ),
        ),
        o("line-height", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                c.line_height = Some(pt_or_px(need(v))?);
                Ok(())
            }
            Some(set as Setter)
        }),
        o("letter-spacing", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                c.letter_spacing = pt_or_px(need(v))?;
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "launch-prefix",
            None,
            R,
            text!(|c: &mut Config, v| c.launch_prefix = Some(v)),
        ),
        o("layer", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                let v = need(v);
                c.layer = if v.eq_ignore_ascii_case("top") {
                    Layer::Top
                } else if v.eq_ignore_ascii_case("overlay") {
                    Layer::Overlay
                } else {
                    return Err(format!(
                        "{v}: invalid layer. Must be one of 'top', 'overlay'"
                    ));
                };
                Ok(())
            }
            Some(set as Setter)
        }),
        o("keyboard-focus", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                let v = need(v);
                c.keyboard_focus = if v.eq_ignore_ascii_case("exclusive") {
                    KeyboardFocus::Exclusive
                } else if v.eq_ignore_ascii_case("on-demand") {
                    KeyboardFocus::OnDemand
                } else {
                    return Err(format!(
                        "{v}: invalid keyboard-focus. Must be one of 'exclusive', 'on-demand'"
                    ));
                };
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "no-exit-on-keyboard-focus-loss",
            None,
            N,
            flag!(|c: &mut Config| c.exit_on_keyboard_focus_loss = false),
        ),
        o(
            "list-executables-in-path",
            None,
            N,
            flag!(|c: &mut Config| c.list_executables_in_path = true),
        ),
        o("render-workers", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                let v = need(v);
                c.render_workers =
                    scan_u32(v)
                        .and_then(|n| u16::try_from(n).ok())
                        .ok_or_else(|| {
                            format!("{v}: invalid value for render-workers (must be an integer)")
                        })?;
                Ok(())
            }
            Some(set as Setter)
        }),
        o("match-workers", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                let v = need(v);
                c.match_workers =
                    scan_u32(v)
                        .and_then(|n| u16::try_from(n).ok())
                        .ok_or_else(|| {
                            format!("{v}: invalid value for match-workers (must be an integer)")
                        })?;
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "no-sort",
            None,
            N,
            flag!(|c: &mut Config| c.sort_result = false),
        ),
        o(
            "counter",
            None,
            N,
            flag!(|c: &mut Config| c.match_counter = true),
        ),
        o(
            "delayed-filter-ms",
            None,
            R,
            number!(
                |c: &mut Config, n| c.delayed_filter_ms = n,
                "invalid delayed-filter-ms (must be an integer)"
            ),
        ),
        o(
            "delayed-filter-limit",
            None,
            R,
            number!(
                |c: &mut Config, n| c.delayed_filter_limit = n,
                "invalid delayed-filter-limit (must be an integer)"
            ),
        ),
        o("scaling-filter", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                use ScalingFilter as S;
                c.scaling_filter = match need(v) {
                    "none" => S::None,
                    "nearest" => S::Nearest,
                    "bilinear" => S::Bilinear,
                    "box" => S::Box,
                    "linear" => S::Linear,
                    "cubic" => S::Cubic,
                    "lanczos2" => S::Lanczos2,
                    "lanczos3" => S::Lanczos3,
                    "lanczos3-stretched" => S::Lanczos3Stretched,
                    other => return Err(format!("{other}: invalid scaling-filter")),
                };
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "auto-select",
            None,
            N,
            flag!(|c: &mut Config| c.auto_select = true),
        ),
        o(
            "no-mouse",
            None,
            N,
            flag!(|c: &mut Config| c.enable_mouse = false),
        ),
        o(
            "dmenu",
            Some('d'),
            N,
            flag!(|c: &mut Config| c.dmenu.enabled = true),
        ),
        o(
            "dmenu0",
            None,
            N,
            flag!(|c: &mut Config| {
                c.dmenu.enabled = true;
                c.dmenu.delim = 0;
            }),
        ),
        o(
            "no-run-if-empty",
            Some('R'),
            N,
            flag!(|c: &mut Config| c.dmenu.exit_immediately_if_empty = true),
        ),
        o(
            "index",
            None,
            N,
            flag!(|c: &mut Config| c.dmenu.mode = DmenuMode::Index),
        ),
        o("nth-delimiter", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                let v = need(v);
                match (v.len(), v.chars().next()) {
                    (1, Some(one)) => c.dmenu.nth_delim = one,
                    _ => {
                        return Err(format!(
                            "{v}: invalid nth-delimiter. Must be a single ASCII character"
                        ));
                    }
                }
                Ok(())
            }
            Some(set as Setter)
        }),
        o(
            "with-nth",
            None,
            R,
            text!(|c: &mut Config, v: String| c.dmenu.with_nth = nth_option(&v)),
        ),
        o(
            "accept-nth",
            None,
            R,
            text!(|c: &mut Config, v: String| c.dmenu.accept_nth = nth_option(&v)),
        ),
        o(
            "match-nth",
            None,
            R,
            text!(|c: &mut Config, v: String| c.dmenu.match_nth = nth_option(&v)),
        ),
        o(
            "only-match",
            None,
            N,
            flag!(|c: &mut Config| c.dmenu.only_match = true),
        ),
        o(
            "mesg",
            None,
            R,
            text!(|c: &mut Config, v| c.message = Some(v)),
        ),
        o("mesg-mode", None, R, {
            fn set(c: &mut Config, v: Option<&str>) -> Result<(), String> {
                c.message_mode = match need(v) {
                    "wrap" => MessageMode::Wrap,
                    "expand" => MessageMode::Expand,
                    other => return Err(format!("{other}: invalid mesg-mode")),
                };
                Ok(())
            }
            Some(set as Setter)
        }),
        o("override", None, R, None),
        o("log-level", None, R, None),
        o("log-colorize", None, O, None),
        o("log-no-syslog", None, N, None),
        o("version", Some('v'), N, None),
        o(
            "print-timing-info",
            None,
            N,
            flag!(|c: &mut Config| c.print_timing_info = true),
        ),
        o("help", Some('h'), N, None),
        // `-i` is case-insensitive search in other launchers; fuzzel takes
        // and ignores it.
        o("", Some('i'), N, None),
    ]
}

/// fuzzel's log levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// `none`.
    None,
    /// `error`.
    Error,
    /// `warning`, the default.
    Warning,
    /// `info`.
    Info,
}

/// What the command line said.
#[derive(Debug)]
pub struct Cli {
    /// `--config`.
    pub config: Option<String>,
    /// `--check-config`.
    pub check_config: bool,
    /// `--select`.
    pub select: Option<String>,
    /// `--select-index`.
    pub select_index: Option<usize>,
    /// `--override`s, in order.
    pub overrides: Vec<String>,
    /// `--log-level`.
    pub log_level: LogLevel,
    /// `--version`.
    pub version: bool,
    /// `--help`.
    pub help: bool,
    /// Whether the program was started as `dmenu`.
    pub as_dmenu: bool,
    setters: Vec<(Setter, Option<String>)>,
}

impl Cli {
    /// Apply the options to a loaded configuration, as fuzzel applies its
    /// `cmdline_overrides`, then fuzzel's dmenu adjustments.
    pub fn apply(&self, config: &mut Config) {
        if self.as_dmenu {
            config.dmenu.enabled = true;
        }
        for (set, value) in &self.setters {
            let _ = set(config, value.as_deref());
        }
        if config.dmenu.enabled {
            config.fields = Fields(if config.dmenu.match_nth.is_some() {
                Fields::NTH
            } else {
                Fields::NAME
            });
            if config.prompt_only {
                config.lines = 0;
                config.match_counter = false;
                config.dmenu.exit_immediately_if_empty = false;
            }
        }
    }
}

/// Parse `args` (without the program name), as `getopt_long` with fuzzel's
/// table. `program` is `argv[0]`: a program called `dmenu` is in dmenu mode.
///
/// # Errors
///
/// fuzzel's message for an unknown option, a missing argument or a bad
/// value; fuzzel exits 1 with it.
pub fn parse(program: &str, args: &[String]) -> Result<Cli, String> {
    let table = table();
    let mut cli = Cli {
        config: None,
        check_config: false,
        select: None,
        select_index: None,
        overrides: Vec::new(),
        log_level: LogLevel::Warning,
        version: false,
        help: false,
        as_dmenu: std::path::Path::new(program)
            .file_name()
            .is_some_and(|name| name == "dmenu"),
        setters: Vec::new(),
    };
    let mut scratch = Config::defaults(1);
    let mut at = 0;
    while let Some(word) = args.get(at) {
        at += 1;
        if word == "--" {
            break;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (long, None),
            };
            let opt =
                find_long(&table, name).ok_or_else(|| format!("error: {word}: invalid option"))?;
            let value = match opt.arg {
                Arg::None if inline.is_some() => {
                    return Err(format!("error: {word}: invalid option"));
                }
                Arg::None | Arg::Optional => inline,
                Arg::Required => match inline {
                    Some(value) => Some(value),
                    None => {
                        let next = args
                            .get(at)
                            .cloned()
                            .ok_or_else(|| format!("error: {word}: missing required argument"))?;
                        at += 1;
                        Some(next)
                    }
                },
            };
            take(&mut cli, &mut scratch, opt, value)?;
            continue;
        }
        let Some(shorts) = word.strip_prefix('-').filter(|s| !s.is_empty()) else {
            // Not an option: fuzzel leaves it where getopt put it and never
            // reads it.
            continue;
        };
        for (index, letter) in shorts.char_indices() {
            let opt = table
                .iter()
                .find(|opt| opt.short == Some(letter))
                .ok_or_else(|| format!("error: {word}: invalid option"))?;
            if opt.arg == Arg::None {
                take(&mut cli, &mut scratch, opt, None)?;
                continue;
            }
            let rest = shorts.get(index + letter.len_utf8()..).unwrap_or_default();
            let value = if rest.is_empty() {
                let next = args
                    .get(at)
                    .cloned()
                    .ok_or_else(|| format!("error: {word}: missing required argument"))?;
                at += 1;
                next
            } else {
                rest.to_owned()
            };
            take(&mut cli, &mut scratch, opt, Some(value))?;
            break;
        }
    }
    if cli.select.is_some() && cli.select_index.is_some_and(|index| index != 0) {
        return Err("--select and --select-index cannot be used at the same time".to_owned());
    }
    if scratch.prompt_only && scratch.hide_prompt {
        return Err("--prompt-only and --hide-prompt cannot be used at the same time".to_owned());
    }
    Ok(cli)
}

/// getopt's long-option match: exact, else the one option `name` is a
/// prefix of.
fn find_long<'a>(table: &'a [Opt], name: &str) -> Option<&'a Opt> {
    if name.is_empty() {
        return None;
    }
    if let Some(exact) = table.iter().find(|opt| opt.long == name) {
        return Some(exact);
    }
    let mut prefixed = table.iter().filter(|opt| opt.long.starts_with(name));
    match (prefixed.next(), prefixed.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// Act on one option.
fn take(
    cli: &mut Cli,
    scratch: &mut Config,
    opt: &Opt,
    value: Option<String>,
) -> Result<(), String> {
    if let Some(set) = opt.set {
        set(scratch, value.as_deref())?;
        cli.setters.push((set, value));
        return Ok(());
    }
    let value = value.unwrap_or_default();
    match opt.long {
        "config" => cli.config = Some(value),
        "check-config" => cli.check_config = true,
        "select" => cli.select = Some(value),
        "select-index" => {
            let digits: String = value
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            cli.select_index =
                Some(digits.parse().map_err(|_| {
                    format!("{value}: invalid selected index (must be an integer)")
                })?);
        }
        "override" => cli.overrides.push(value),
        "log-level" => {
            cli.log_level = match value.as_str() {
                "info" => LogLevel::Info,
                "warning" => LogLevel::Warning,
                "error" => LogLevel::Error,
                "none" => LogLevel::None,
                _ => {
                    return Err(format!(
                        "-l,--log-level: {value}: argument must be one of \"info\", \"warning\", \"error\", \"none\""
                    ));
                }
            };
        }
        "version" => cli.version = true,
        "help" => cli.help = true,
        // `--log-colorize` and `--log-no-syslog` change how fuzzel logs to a
        // terminal and to syslog; this logs plainly to standard error, and
        // Ferrix has no syslog.
        _ => {}
    }
    Ok(())
}

/// `--version`'s line.
#[must_use]
pub fn version() -> String {
    "fuzzel version: 1.12.0 (Ferrix) -cairo +png +svg(resvg) -assertions".to_owned()
}

/// `--help`'s text: fuzzel's own list of options.
#[must_use]
pub fn usage(program: &str) -> String {
    let mut out = format!("Usage: {program} [OPTION]...\n\nOptions:\n");
    for opt in table() {
        if opt.long.is_empty() {
            continue;
        }
        let short = opt
            .short
            .map_or_else(|| "   ".to_owned(), |c| format!("-{c},"));
        let arg = match opt.arg {
            Arg::None => "",
            Arg::Required => "=VALUE",
            Arg::Optional => "[=VALUE]",
        };
        out.push_str(&format!("  {short}--{}{arg}\n", opt.long));
    }
    out.push_str("\nAll colors are RGBA - i.e. 8-digit hex values, without prefix.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, parse};
    use crate::config::{Config, DmenuMode, Fields, Rgba};

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn applied(words: &[&str]) -> Config {
        let mut config = Config::defaults(1);
        if let Ok(cli) = parse("fuzzel", &args(words)) {
            cli.apply(&mut config);
        }
        config
    }

    #[test]
    fn the_hypr_launcher_wrapper() {
        // The user's own wrapper runs exactly this.
        let c = applied(&[
            "--keyboard-focus=on-demand",
            "--no-exit-on-keyboard-focus-loss",
        ]);
        assert_eq!(c.keyboard_focus, crate::config::KeyboardFocus::OnDemand);
        assert!(!c.exit_on_keyboard_focus_loss);
    }

    #[test]
    fn spellings() {
        let c = applied(&[
            "-l5",
            "-w",
            "20",
            "--lines=7",
            "-dI",
            "--prompt",
            "run: ",
            "--back=11223344",
        ]);
        assert_eq!((c.lines, c.chars), (7, 20));
        assert!(c.dmenu.enabled && !c.icons_enabled);
        assert_eq!(c.prompt, "run: ");
        assert_eq!(c.colors.background, Rgba(0x1122_3344));
        // dmenu mode matches the name alone.
        assert_eq!(c.fields, Fields(Fields::NAME));
        let c = applied(&["--dmenu", "--index", "--match-nth=2", "--password"]);
        assert_eq!(c.dmenu.mode, DmenuMode::Index);
        assert_eq!(c.fields, Fields(Fields::NTH));
        assert_eq!(c.password.character, Some('*'));
        let c = applied(&["--prompt-only=Sure? "]);
        assert!(c.prompt_only);
    }

    #[test]
    fn fuzzels_messages() {
        let err = |words: &[&str]| parse("fuzzel", &args(words)).err();
        assert_eq!(
            err(&["--bogus"]),
            Some("error: --bogus: invalid option".to_owned())
        );
        assert_eq!(
            err(&["--dm"]),
            Some("error: --dm: invalid option".to_owned())
        );
        assert_eq!(
            err(&["--lines"]),
            Some("error: --lines: missing required argument".to_owned())
        );
        assert_eq!(err(&["-l", "x"]), Some("x: invalid line count".to_owned()));
        assert_eq!(
            err(&["-b", "123"]),
            Some("background-color: 123: invalid color".to_owned())
        );
        assert_eq!(
            err(&["--anchor=middle"]),
            Some("middle: invalid anchor".to_owned())
        );
        assert_eq!(
            err(&["--match-mode=regex"]),
            Some("regex: invalid match-mode. Must be 'exact', 'fuzzy' or 'fzf'".to_owned())
        );
        assert_eq!(
            err(&["--prompt-only=x", "--hide-prompt"]),
            Some("--prompt-only and --hide-prompt cannot be used at the same time".to_owned())
        );
    }

    #[test]
    fn meta_options() {
        let cli = parse(
            "/usr/bin/dmenu",
            &args(&[
                "--config",
                "/x.ini",
                "--check-config",
                "--override=lines=3",
                "--log-level=info",
                "-i",
            ]),
        );
        let cli = cli.ok();
        assert_eq!(
            cli.as_ref().and_then(|c| c.config.clone()).as_deref(),
            Some("/x.ini")
        );
        assert!(cli.as_ref().is_some_and(|c| c.check_config && c.as_dmenu));
        assert_eq!(
            cli.as_ref().map(|c| c.overrides.clone()),
            Some(vec!["lines=3".to_owned()])
        );
        assert_eq!(cli.as_ref().map(|c| c.log_level), Some(LogLevel::Info));
    }
}
