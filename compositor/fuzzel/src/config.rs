//! `fuzzel.ini`: fuzzel's configuration, read the way fuzzel reads it.
//!
//! A port of fuzzel's `config.c` (1.12, codeberg.org/dnkl/fuzzel): the file
//! is found where fuzzel looks for it, every line is split, unquoted and
//! checked by the same rules, every option has fuzzel's default, and a line
//! fuzzel would refuse is refused here with fuzzel's own words. A bad line is
//! a diagnostic and the rest of the file still counts, as it does for fuzzel
//! when it is not checking the file (`--check-config` makes the first error
//! the end, here too).
//!
//! Every option fuzzel has is parsed and kept, including the ones this
//! program does not carry out; what those do on Ferrix is the caller's to
//! say (`crate::unsupported`), because the line itself is valid and fuzzel
//! would take it.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::keysym;

/// A colour as the file writes it: `0xRRGGBBAA`, not premultiplied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u32);

impl Rgba {
    /// The alpha byte.
    #[must_use]
    pub fn alpha(self) -> u8 {
        (self.0 & 0xff) as u8
    }

    /// The colour as a premultiplied `ARGB8888` pixel, rounded the way
    /// fuzzel's `rgba2pixman` rounds its sixteen-bit channels and then taken
    /// to eight bits.
    #[must_use]
    pub fn premultiplied(self) -> u32 {
        let [r, g, b, a] = self.0.to_be_bytes();
        let scale = |channel: u8| -> u32 {
            // pixman_color_t is sixteen bits a channel: c * 257, then
            // c * a / 0xffff, then the eight bits pixman keeps of it.
            let wide = u32::from(channel) * 257;
            let alpha = u32::from(a) * 257;
            (wide * alpha / 0xffff) >> 8
        };
        (u32::from(a) << 24) | (scale(r) << 16) | (scale(g) << 8) | scale(b)
    }
}

/// A size written either in points (`16`) or in pixels (`16px`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PtOrPx {
    /// Points, scaled by the DPI or the output's scale.
    Pt(f32),
    /// Pixels, used as they are.
    Px(i64),
}

impl PtOrPx {
    /// The size in pixels at `dpi` and `scale`, as fuzzel's
    /// `pt_or_px_as_pixels` works it out.
    #[must_use]
    pub fn pixels(self, scale: f32, dpi: f32, by_dpi: bool) -> i64 {
        match self {
            Self::Px(px) => px,
            Self::Pt(pt) => {
                let scale = if by_dpi { 1.0 } else { f64::from(scale) };
                let dpi = if by_dpi { f64::from(dpi) } else { 96.0 };
                (f64::from(pt) * scale * dpi / 72.0).round() as i64
            }
        }
    }
}

/// `dpi-aware`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpiAware {
    /// `auto`: by DPI when every output's scale is one.
    Auto,
    /// `yes`.
    Yes,
    /// `no`.
    No,
}

/// `match-mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    /// `exact`.
    Exact,
    /// `fzf`.
    Fzf,
    /// `fuzzy`.
    Fuzzy,
}

/// `message-mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageMode {
    /// `wrap`.
    Wrap,
    /// `expand`.
    Expand,
}

/// `[dmenu] mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmenuMode {
    /// `text`: print the entry.
    Text,
    /// `index`: print its position in the input.
    Index,
}

/// `scaling-filter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalingFilter {
    /// `none`.
    None,
    /// `nearest`.
    Nearest,
    /// `bilinear`.
    Bilinear,
    /// `box`.
    Box,
    /// `linear`.
    Linear,
    /// `cubic`.
    Cubic,
    /// `lanczos2`.
    Lanczos2,
    /// `lanczos3`.
    Lanczos3,
    /// `lanczos3-stretched`.
    Lanczos3Stretched,
}

/// `layer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// `top`.
    Top,
    /// `overlay`.
    Overlay,
}

impl Layer {
    /// The `zwlr_layer_shell_v1.layer` value.
    #[must_use]
    pub fn wire(self) -> u32 {
        match self {
            Self::Top => 2,
            Self::Overlay => 3,
        }
    }
}

/// `keyboard-focus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardFocus {
    /// `exclusive`.
    Exclusive,
    /// `on-demand`.
    OnDemand,
}

impl KeyboardFocus {
    /// The `zwlr_layer_surface_v1.keyboard_interactivity` value.
    #[must_use]
    pub fn wire(self) -> u32 {
        match self {
            Self::Exclusive => 1,
            Self::OnDemand => 2,
        }
    }
}

/// `anchor`: the `zwlr_layer_surface_v1.anchor` bits, which is how fuzzel
/// keeps it too (`center` is no edge at all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor(pub u32);

/// The anchor names, in fuzzel's order, with their edges: top 1, bottom 2,
/// left 4, right 8.
pub const ANCHORS: &[(&str, Anchor)] = &[
    ("top-left", Anchor(1 | 4)),
    ("top", Anchor(1)),
    ("top-right", Anchor(1 | 8)),
    ("left", Anchor(4)),
    ("center", Anchor(0)),
    ("right", Anchor(8)),
    ("bottom-left", Anchor(2 | 4)),
    ("bottom", Anchor(2)),
    ("bottom-right", Anchor(2 | 8)),
];

/// `fields`: which parts of an entry are matched against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Fields(pub u8);

impl Fields {
    /// `name`.
    pub const NAME: u8 = 0x01;
    /// `filename`.
    pub const FILENAME: u8 = 0x02;
    /// `generic`.
    pub const GENERIC: u8 = 0x04;
    /// `exec`.
    pub const EXEC: u8 = 0x08;
    /// `comment`.
    pub const COMMENT: u8 = 0x10;
    /// `keywords`.
    pub const KEYWORDS: u8 = 0x20;
    /// `categories`.
    pub const CATEGORIES: u8 = 0x40;
    /// `--match-nth`'s column, which no line of the file can name.
    pub const NTH: u8 = 0x80;

    /// Whether `bit` is one of these.
    #[must_use]
    pub fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

/// What a key binding does: fuzzel's `bind_action_*`, named as
/// `[key-bindings]` names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `cancel`.
    Cancel,
    /// `cursor-home`.
    CursorHome,
    /// `cursor-end`.
    CursorEnd,
    /// `cursor-left`.
    CursorLeft,
    /// `cursor-left-word`.
    CursorLeftWord,
    /// `cursor-right`.
    CursorRight,
    /// `cursor-right-word`.
    CursorRightWord,
    /// `delete-line`.
    DeleteLine,
    /// `delete-prev`.
    DeletePrev,
    /// `delete-prev-word`.
    DeletePrevWord,
    /// `delete-line-backward`.
    DeleteLineBackward,
    /// `delete-next`.
    DeleteNext,
    /// `delete-next-word`.
    DeleteNextWord,
    /// `delete-line-forward`.
    DeleteLineForward,
    /// `insert-selected`.
    InsertSelected,
    /// `expunge`.
    Expunge,
    /// `clipboard-paste`.
    ClipboardPaste,
    /// `primary-paste`.
    PrimaryPaste,
    /// `execute`.
    Execute,
    /// `execute-or-next`.
    ExecuteOrNext,
    /// `execute-input`.
    ExecuteInput,
    /// `prev`.
    Prev,
    /// `prev-with-wrap`.
    PrevWithWrap,
    /// `prev-page`.
    PrevPage,
    /// `next`.
    Next,
    /// `next-with-wrap`.
    NextWithWrap,
    /// `next-page`.
    NextPage,
    /// `first`.
    First,
    /// `last`.
    Last,
    /// `custom-N`, one to nineteen: execute, then exit with `9 + N`.
    Custom(u8),
}

/// The fixed actions and their names, in fuzzel's order.
const ACTIONS: &[(&str, Action)] = &[
    ("cancel", Action::Cancel),
    ("cursor-home", Action::CursorHome),
    ("cursor-end", Action::CursorEnd),
    ("cursor-left", Action::CursorLeft),
    ("cursor-left-word", Action::CursorLeftWord),
    ("cursor-right", Action::CursorRight),
    ("cursor-right-word", Action::CursorRightWord),
    ("delete-line", Action::DeleteLine),
    ("delete-prev", Action::DeletePrev),
    ("delete-prev-word", Action::DeletePrevWord),
    ("delete-line-backward", Action::DeleteLineBackward),
    ("delete-next", Action::DeleteNext),
    ("delete-next-word", Action::DeleteNextWord),
    ("delete-line-forward", Action::DeleteLineForward),
    ("insert-selected", Action::InsertSelected),
    ("expunge", Action::Expunge),
    ("clipboard-paste", Action::ClipboardPaste),
    ("primary-paste", Action::PrimaryPaste),
    ("execute", Action::Execute),
    ("execute-or-next", Action::ExecuteOrNext),
    ("execute-input", Action::ExecuteInput),
    ("prev", Action::Prev),
    ("prev-with-wrap", Action::PrevWithWrap),
    ("prev-page", Action::PrevPage),
    ("next", Action::Next),
    ("next-with-wrap", Action::NextWithWrap),
    ("next-page", Action::NextPage),
    ("first", Action::First),
    ("last", Action::Last),
];

impl Action {
    /// The action `[key-bindings]` calls `name`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        if let Some((_, action)) = ACTIONS.iter().find(|(known, _)| *known == name) {
            return Some(*action);
        }
        let number = name.strip_prefix("custom-")?;
        // Only the spellings fuzzel's table has: `custom-1` to `custom-19`.
        let value = number.parse::<u8>().ok()?;
        (number == value.to_string() && (1..=19).contains(&value)).then_some(Self::Custom(value))
    }

    /// Its name in `[key-bindings]`.
    #[must_use]
    pub fn name(self) -> String {
        if let Self::Custom(number) = self {
            return format!("custom-{number}");
        }
        ACTIONS
            .iter()
            .find(|(_, action)| *action == self)
            .map_or_else(String::new, |(name, _)| (*name).to_owned())
    }
}

/// The modifiers a binding needs, exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mods {
    /// `Shift`.
    pub shift: bool,
    /// `Mod1`, which is Alt.
    pub alt: bool,
    /// `Control`.
    pub ctrl: bool,
    /// `Mod4`, which is Super.
    pub logo: bool,
}

impl Mods {
    /// The modifiers as `compositor/xkb`'s mask bits.
    #[must_use]
    pub fn mask(self) -> u32 {
        use compositor_xkb::generated::{CONTROL, MOD1, MOD4, SHIFT};
        (if self.shift { SHIFT } else { 0 })
            | (if self.ctrl { CONTROL } else { 0 })
            | (if self.alt { MOD1 } else { 0 })
            | (if self.logo { MOD4 } else { 0 })
    }

    /// As fuzzel's `modifiers_to_str` writes them: `Control+Mod1+Mod4+Shift+`.
    fn prefix(self) -> String {
        let mut out = String::new();
        for (held, name) in [
            (self.ctrl, "Control+"),
            (self.alt, "Mod1+"),
            (self.logo, "Mod4+"),
            (self.shift, "Shift+"),
        ] {
            if held {
                out.push_str(name);
            }
        }
        out
    }
}

/// One key binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// What it does.
    pub action: Action,
    /// What must be held.
    pub mods: Mods,
    /// The keysym, as the keymaps spell it.
    pub sym: &'static str,
    /// The file and line it came from; `None` for a default.
    pub origin: Option<(String, u32)>,
}

/// `--password`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Password {
    /// Whether input is hidden.
    pub enabled: bool,
    /// What each character is drawn as; `None` draws nothing at all.
    pub character: Option<char>,
    /// Whether `password-character` set it, which `--password` alone does
    /// not override.
    pub character_set: bool,
}

/// The fuzzy matcher's limits, which only the command line sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuzzy {
    /// Shorter search strings are not fuzzy-matched.
    pub min_length: usize,
    /// How much longer or shorter a fuzzy match may be.
    pub max_length_discrepancy: usize,
    /// The largest Levenshtein distance allowed.
    pub max_distance: usize,
}

/// `[dmenu]` and the dmenu command-line options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dmenu {
    /// `--dmenu`.
    pub enabled: bool,
    /// `mode`.
    pub mode: DmenuMode,
    /// `exit-immediately-if-empty`.
    pub exit_immediately_if_empty: bool,
    /// `--only-match`.
    pub only_match: bool,
    /// Between entries: a newline, or NUL with `--dmenu0`.
    pub delim: u8,
    /// Between columns, for the `nth` options.
    pub nth_delim: char,
    /// `--with-nth`.
    pub with_nth: Option<String>,
    /// `--accept-nth`.
    pub accept_nth: Option<String>,
    /// `--match-nth`.
    pub match_nth: Option<String>,
}

/// The colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// `background`.
    pub background: Rgba,
    /// `border`.
    pub border: Rgba,
    /// `text`.
    pub text: Rgba,
    /// `message`.
    pub message: Rgba,
    /// `prompt`.
    pub prompt: Rgba,
    /// `input`.
    pub input: Rgba,
    /// `match`.
    pub matched: Rgba,
    /// `selection`.
    pub selection: Rgba,
    /// `selection-text`.
    pub selection_text: Rgba,
    /// `selection-match`.
    pub selection_match: Rgba,
    /// `counter`.
    pub counter: Rgba,
    /// `placeholder`.
    pub placeholder: Rgba,
}

/// Padding, in pixels before scaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pad {
    /// `horizontal-pad`.
    pub x: u32,
    /// `vertical-pad`.
    pub y: u32,
    /// `inner-pad`.
    pub inner: u32,
}

/// `[border]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Border {
    /// `width`.
    pub width: u32,
    /// `radius`.
    pub radius: u32,
    /// `selection-radius`.
    pub selection_radius: u32,
}

/// Everything `fuzzel.ini` and the command line can say.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "fuzzel's own configuration is this many switches"
)]
pub struct Config {
    /// `output`.
    pub output: Option<String>,
    /// `prompt`.
    pub prompt: String,
    /// `placeholder`.
    pub placeholder: String,
    /// `--search`.
    pub search_text: String,
    /// `message` (`--mesg`).
    pub message: Option<String>,
    /// `message-mode`.
    pub message_mode: MessageMode,
    /// `--prompt-only`.
    pub prompt_only: bool,
    /// `fields`.
    pub fields: Fields,
    /// `namespace`: `launcher`, which is what a `layerrule` matches.
    pub namespace: String,
    /// `password-character` and `--password`.
    pub password: Password,
    /// `terminal`.
    pub terminal: Option<String>,
    /// `launch-prefix`.
    pub launch_prefix: Option<String>,
    /// `font`.
    pub font: String,
    /// `use-bold`.
    pub use_bold: bool,
    /// `dpi-aware`.
    pub dpi_aware: DpiAware,
    /// `gamma-correct-blending`.
    pub gamma_correct: bool,
    /// `render-workers`.
    pub render_workers: u16,
    /// `match-workers`.
    pub match_workers: u16,
    /// `filter-desktop`.
    pub filter_desktop: bool,
    /// `icons-enabled`.
    pub icons_enabled: bool,
    /// `icon-theme`.
    pub icon_theme: String,
    /// `hide-before-typing`.
    pub hide_before_typing: bool,
    /// `hide-prompt`.
    pub hide_prompt: bool,
    /// `show-actions`.
    pub show_actions: bool,
    /// `[key-bindings]`, defaults first.
    pub bindings: Vec<Binding>,
    /// `match-mode`.
    pub match_mode: MatchMode,
    /// `sort-result`.
    pub sort_result: bool,
    /// `match-counter`.
    pub match_counter: bool,
    /// `delayed-filter-ms`.
    pub delayed_filter_ms: u32,
    /// `delayed-filter-limit`.
    pub delayed_filter_limit: u32,
    /// The fuzzy matcher's limits.
    pub fuzzy: Fuzzy,
    /// `[dmenu]`.
    pub dmenu: Dmenu,
    /// `anchor`.
    pub anchor: Anchor,
    /// `x-margin`.
    pub x_margin: u32,
    /// `y-margin`.
    pub y_margin: u32,
    /// `lines`.
    pub lines: u32,
    /// `minimal-lines`.
    pub minimal_lines: bool,
    /// `width`, in characters.
    pub chars: u32,
    /// `tabs`.
    pub tabs: u32,
    /// The paddings.
    pub pad: Pad,
    /// `[colors]`.
    pub colors: Colors,
    /// `[border]`.
    pub border: Border,
    /// `image-size-ratio`.
    pub image_size_ratio: f32,
    /// `scaling-filter`.
    pub scaling_filter: ScalingFilter,
    /// `line-height`; `None` is the font's own.
    pub line_height: Option<PtOrPx>,
    /// `letter-spacing`.
    pub letter_spacing: PtOrPx,
    /// `layer`.
    pub layer: Layer,
    /// `keyboard-focus`.
    pub keyboard_focus: KeyboardFocus,
    /// `exit-on-keyboard-focus-loss`.
    pub exit_on_keyboard_focus_loss: bool,
    /// `list-executables-in-path`.
    pub list_executables_in_path: bool,
    /// `cache`.
    pub cache: Option<String>,
    /// `auto-select`.
    pub auto_select: bool,
    /// `--print-timing-info`.
    pub print_timing_info: bool,
    /// `enable-mouse`.
    pub enable_mouse: bool,
}

impl Config {
    /// fuzzel's defaults, from `config_load`. `workers` is what
    /// `sysconf(_SC_NPROCESSORS_ONLN)` answered.
    #[must_use]
    pub fn defaults(workers: u16) -> Self {
        Self {
            output: None,
            prompt: "> ".to_owned(),
            placeholder: String::new(),
            search_text: String::new(),
            message: None,
            message_mode: MessageMode::Wrap,
            prompt_only: false,
            fields: Fields(Fields::FILENAME | Fields::NAME | Fields::GENERIC),
            namespace: "launcher".to_owned(),
            password: Password::default(),
            terminal: None,
            launch_prefix: None,
            font: "monospace".to_owned(),
            use_bold: false,
            dpi_aware: DpiAware::Auto,
            gamma_correct: false,
            render_workers: workers,
            match_workers: workers,
            filter_desktop: false,
            icons_enabled: true,
            icon_theme: "default".to_owned(),
            hide_before_typing: false,
            hide_prompt: false,
            show_actions: false,
            bindings: default_bindings(),
            match_mode: MatchMode::Fzf,
            sort_result: true,
            match_counter: false,
            delayed_filter_ms: 300,
            delayed_filter_limit: 20000,
            fuzzy: Fuzzy {
                min_length: 3,
                max_length_discrepancy: 2,
                max_distance: 1,
            },
            dmenu: Dmenu {
                enabled: false,
                mode: DmenuMode::Text,
                exit_immediately_if_empty: false,
                only_match: false,
                delim: b'\n',
                nth_delim: '\t',
                with_nth: None,
                accept_nth: None,
                match_nth: None,
            },
            anchor: Anchor(0),
            x_margin: 0,
            y_margin: 0,
            lines: 15,
            minimal_lines: false,
            chars: 30,
            tabs: 8,
            pad: Pad {
                x: 40,
                y: 8,
                inner: 0,
            },
            colors: Colors {
                background: Rgba(0xfdf6_e3ff),
                border: Rgba(0x002b_36ff),
                text: Rgba(0x657b_83ff),
                message: Rgba(0x657b_83ff),
                prompt: Rgba(0x586e_75ff),
                input: Rgba(0x657b_83ff),
                matched: Rgba(0xcb4b_16ff),
                selection: Rgba(0xeee8_d5ff),
                selection_text: Rgba(0x586e_75ff),
                selection_match: Rgba(0xcb4b_16ff),
                counter: Rgba(0x93a1_a1ff),
                placeholder: Rgba(0x93a1_a1ff),
            },
            border: Border {
                width: 1,
                radius: 10,
                selection_radius: 0,
            },
            image_size_ratio: 0.5,
            scaling_filter: ScalingFilter::Box,
            line_height: None,
            letter_spacing: PtOrPx::Pt(0.0),
            layer: Layer::Overlay,
            keyboard_focus: KeyboardFocus::Exclusive,
            exit_on_keyboard_focus_loss: true,
            list_executables_in_path: false,
            cache: None,
            auto_select: false,
            print_timing_info: false,
            enable_mouse: true,
        }
    }
}

/// fuzzel's `add_default_key_bindings`, in its order.
#[must_use]
pub fn default_bindings() -> Vec<Binding> {
    const NONE: Mods = Mods {
        shift: false,
        alt: false,
        ctrl: false,
        logo: false,
    };
    const ALT: Mods = Mods { alt: true, ..NONE };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const SHIFT: Mods = Mods {
        shift: true,
        ..NONE
    };
    const CTRL_SHIFT: Mods = Mods {
        ctrl: true,
        shift: true,
        ..NONE
    };
    use Action as A;
    let table: &[(Action, Mods, &str)] = &[
        (A::Cancel, NONE, "Escape"),
        (A::Cancel, CTRL, "g"),
        (A::Cancel, CTRL, "c"),
        (A::Cancel, CTRL, "bracketleft"),
        (A::CursorHome, NONE, "Home"),
        (A::CursorHome, CTRL, "a"),
        (A::CursorEnd, NONE, "End"),
        (A::CursorEnd, CTRL, "e"),
        (A::CursorLeft, CTRL, "b"),
        (A::CursorLeft, NONE, "Left"),
        (A::CursorLeftWord, ALT, "b"),
        (A::CursorLeftWord, CTRL, "Left"),
        (A::CursorRight, CTRL, "f"),
        (A::CursorRight, NONE, "Right"),
        (A::CursorRightWord, ALT, "f"),
        (A::CursorRightWord, CTRL, "Right"),
        (A::DeleteLine, CTRL_SHIFT, "BackSpace"),
        (A::DeletePrev, NONE, "BackSpace"),
        (A::DeletePrev, CTRL, "h"),
        (A::DeletePrevWord, CTRL, "BackSpace"),
        (A::DeletePrevWord, CTRL, "w"),
        (A::DeletePrevWord, ALT, "BackSpace"),
        (A::DeleteNext, NONE, "Delete"),
        (A::DeleteNext, NONE, "KP_Delete"),
        (A::DeleteNext, CTRL, "d"),
        (A::DeleteNextWord, ALT, "d"),
        (A::DeleteNextWord, CTRL, "Delete"),
        (A::DeleteNextWord, CTRL, "KP_Delete"),
        (A::DeleteLineBackward, CTRL, "u"),
        (A::DeleteLineForward, CTRL, "k"),
        (A::InsertSelected, CTRL, "Tab"),
        (A::Expunge, SHIFT, "Delete"),
        (A::Expunge, SHIFT, "KP_Delete"),
        (A::ClipboardPaste, CTRL, "v"),
        (A::ClipboardPaste, NONE, "XF86Paste"),
        (A::PrimaryPaste, SHIFT, "Insert"),
        (A::PrimaryPaste, SHIFT, "KP_Insert"),
        (A::Execute, NONE, "Return"),
        (A::Execute, NONE, "KP_Enter"),
        (A::Execute, CTRL, "y"),
        (A::ExecuteOrNext, NONE, "Tab"),
        (A::ExecuteInput, SHIFT, "Return"),
        (A::ExecuteInput, SHIFT, "KP_Enter"),
        (A::Prev, NONE, "Up"),
        (A::Prev, CTRL, "p"),
        (A::PrevWithWrap, NONE, "ISO_Left_Tab"),
        (A::PrevPage, NONE, "Page_Up"),
        (A::PrevPage, NONE, "KP_Page_Up"),
        (A::Next, NONE, "Down"),
        (A::Next, CTRL, "n"),
        (A::NextPage, NONE, "Page_Down"),
        (A::NextPage, NONE, "KP_Page_Down"),
        (A::First, CTRL, "Home"),
        (A::Last, CTRL, "End"),
        (A::Custom(1), ALT, "1"),
        (A::Custom(2), ALT, "2"),
        (A::Custom(3), ALT, "3"),
        (A::Custom(4), ALT, "4"),
        (A::Custom(5), ALT, "5"),
        (A::Custom(6), ALT, "6"),
        (A::Custom(7), ALT, "7"),
        (A::Custom(8), ALT, "8"),
        (A::Custom(9), ALT, "9"),
        (A::Custom(10), ALT, "0"),
        (A::Custom(11), ALT, "exclam"),
        (A::Custom(12), ALT, "at"),
        (A::Custom(13), ALT, "numbersign"),
        (A::Custom(14), ALT, "dollar"),
        (A::Custom(15), ALT, "percent"),
        (A::Custom(16), ALT, "dead_circumflex"),
        (A::Custom(17), ALT, "ampersand"),
        (A::Custom(18), ALT, "asterisk"),
        (A::Custom(19), ALT, "parenleft"),
    ];
    table
        .iter()
        .filter_map(|(action, mods, name)| {
            // Every name above is in the keymaps; one that were not would be
            // a key no keyboard here can press, so leaving it out loses
            // nothing.
            keysym::canonical(name).map(|sym| Binding {
                action: *action,
                mods: *mods,
                sym,
                origin: None,
            })
        })
        .collect()
}

/// How loud a diagnostic is, as fuzzel's log classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// ` err`.
    Error,
    /// `warn`.
    Warning,
    /// `info`.
    Info,
}

/// One line fuzzel would log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// How loud.
    pub level: Level,
    /// The text after fuzzel's `<class>: <source>:<line>: ` prefix, which
    /// names fuzzel's C source and so is not reproduced.
    pub text: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = match self.level {
            Level::Error => " err",
            Level::Warning => "warn",
            Level::Info => "info",
        };
        write!(f, "{class}: config: {}", self.text)
    }
}

/// The environment the file is looked for in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// `HOME`.
    pub home: Option<String>,
    /// `XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<String>,
    /// `XDG_CONFIG_DIRS`.
    pub xdg_config_dirs: Option<String>,
    /// `TERMINAL`.
    pub terminal: Option<String>,
}

impl Env {
    /// This process's.
    #[must_use]
    pub fn from_process() -> Self {
        let get = |name: &str| std::env::var(name).ok();
        Self {
            home: get("HOME"),
            xdg_config_home: get("XDG_CONFIG_HOME"),
            xdg_config_dirs: get("XDG_CONFIG_DIRS"),
            terminal: get("TERMINAL"),
        }
    }
}

/// What loading gave.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    /// The configuration, with every line that could be taken taken.
    pub config: Config,
    /// Whether fuzzel's `config_load` would have returned true.
    pub ok: bool,
    /// Which file was read, if one was.
    pub path: Option<PathBuf>,
    /// What fuzzel would have logged, in order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Where fuzzel looks for its file, in order: `XDG_CONFIG_HOME` (only when
/// absolute) or `~/.config`, then each of `XDG_CONFIG_DIRS` or `/etc/xdg`.
#[must_use]
pub fn search_path(env: &Env) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match (&env.xdg_config_home, &env.home) {
        (Some(home), _) if home.starts_with('/') => {
            out.push(PathBuf::from(format!("{home}/fuzzel/fuzzel.ini")));
        }
        (_, Some(home)) => out.push(PathBuf::from(format!("{home}/.config/fuzzel/fuzzel.ini"))),
        _ => {}
    }
    let dirs = env
        .xdg_config_dirs
        .as_deref()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or("/etc/xdg");
    out.extend(
        dirs.split(':')
            .filter(|dir| !dir.is_empty())
            .map(|dir| PathBuf::from(format!("{dir}/fuzzel/fuzzel.ini"))),
    );
    out
}

/// Load the configuration as fuzzel's `config_load` does: `explicit` is
/// `--config`, `overrides` are the `--override` values in order, and `fatal`
/// is `--check-config`'s errors-are-fatal.
#[must_use]
pub fn load(explicit: Option<&Path>, env: &Env, overrides: &[String], fatal: bool) -> Loaded {
    let mut parser = Parser::new(env, fatal);
    let found = match explicit {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => Some((path.to_path_buf(), bytes)),
            Err(error) => {
                parser.error(format!(
                    "{}: failed to open: {}",
                    path.display(),
                    errno(&error)
                ));
                return parser.finish(!fatal, None);
            }
        },
        None => search_path(env)
            .into_iter()
            .find_map(|path| std::fs::read(&path).ok().map(|bytes| (path, bytes))),
    };
    let Some((path, bytes)) = found else {
        parser.push(
            Level::Warning,
            "no configuration found, using defaults".to_owned(),
        );
        // fuzzel returns before applying `--override`, and before checking
        // the bindings for collisions: with no file, neither happens.
        return parser.finish(true, None);
    };
    parser.read(&bytes, path, overrides)
}

/// Load `text` as though it were the file at `path`, which is what [`load`]
/// does once it has found one.
#[must_use]
pub fn from_text(text: &str, path: &Path, env: &Env, overrides: &[String], fatal: bool) -> Loaded {
    Parser::new(env, fatal).read(text.as_bytes(), path.to_path_buf(), overrides)
}

/// `strerror` and the number, as fuzzel's `LOG_ERRNO` prints them.
fn errno(error: &std::io::Error) -> String {
    let text = match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        _ => error.to_string(),
    };
    match error.raw_os_error() {
        Some(number) => format!("{text} ({number})"),
        None => text,
    }
}

/// Where a line is, for the diagnostic.
struct Context<'a> {
    path: &'a str,
    line: u32,
    section: &'a str,
    key: Option<&'a str>,
    value: Option<&'a str>,
}

impl Context<'_> {
    /// fuzzel's `log_contextual` prefix and `message`.
    fn say(&self, message: &str) -> String {
        let mut out = format!("{}:{}: [{}]", self.path, self.line, self.section);
        if let Some(key) = self.key {
            out.push('.');
            out.push_str(key);
        }
        if let Some(value) = self.value {
            out.push_str(": ");
            out.push_str(value);
        }
        out.push_str(": ");
        out.push_str(message);
        out
    }
}

/// The sections, as fuzzel's `section_info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Main,
    Colors,
    Border,
    Dmenu,
    KeyBindings,
}

impl Section {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "main" => Self::Main,
            "colors" => Self::Colors,
            "border" => Self::Border,
            "dmenu" => Self::Dmenu,
            "key-bindings" => Self::KeyBindings,
            _ => return None,
        })
    }
}

/// The state of a load.
struct Parser {
    config: Config,
    diagnostics: Vec<Diagnostic>,
    fatal: bool,
    home: Option<String>,
}

/// fuzzel's `isspace` in the C locale.
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

/// Split `kv` as fuzzel's `parse_key_value` does. `with_section` is true for
/// `--override`, whose key may be `section.key`. Returns the section (when
/// asked for), the key and the value; a `None` value is a line with no
/// value, and a `None` key one with no key.
fn split_key_value(
    kv: &str,
    with_section: bool,
) -> (Option<String>, Option<String>, Option<String>) {
    let kv = kv.trim_start_matches(is_space);
    let mut section = with_section.then(|| "main".to_owned());
    if kv.starts_with('=') {
        return (section, None, None);
    }
    let kv = kv.trim_end_matches(is_space);
    let mut key_start = 0;
    let mut key_end = kv.len();
    let mut value: Option<&str> = None;
    let mut want_section = with_section;
    for (at, c) in kv.char_indices() {
        if c == '.' && want_section {
            want_section = false;
            section = kv.get(..at).map(str::to_owned);
            let rest = kv.get(at + 1..).unwrap_or_default();
            if rest.is_empty() || rest.starts_with('=') {
                return (section, None, None);
            }
            key_start = at + 1;
        } else if c == '=' {
            key_end = at;
            let rest = kv.get(at + 1..).unwrap_or_default();
            if !rest.is_empty() {
                value = Some(rest);
            }
            break;
        }
    }
    let key = kv
        .get(key_start..key_end)
        .unwrap_or_default()
        .trim_end_matches(is_space)
        .to_owned();
    let Some(value) = value else {
        return (section, Some(key), None);
    };
    let value = value.trim_start_matches(is_space);
    (section, Some(key), Some(unquote(value)))
}

/// Take the quotes off a value quoted whole, and the backslash off `\\` and
/// off the quote character, which is all fuzzel unescapes.
fn unquote(value: &str) -> String {
    let mut chars = value.chars();
    let (Some(first), Some(last)) = (chars.next(), value.chars().next_back()) else {
        return value.to_owned();
    };
    if !((first == '"' && last == '"') || (first == '\'' && last == '\'')) {
        return value.to_owned();
    }
    let quote = first;
    let inner: Vec<char> = if value.chars().count() >= 2 {
        let all: Vec<char> = value.chars().collect();
        all.get(1..all.len() - 1)
            .map(<[char]>::to_vec)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut out = String::new();
    let mut at = 0;
    while let Some(&c) = inner.get(at) {
        if c == '\\' && matches!(inner.get(at + 1), Some(&next) if next == '\\' || next == quote) {
            if let Some(&next) = inner.get(at + 1) {
                out.push(next);
            }
            at += 2;
            continue;
        }
        out.push(c);
        at += 1;
    }
    out
}

/// `strtoul(s, &end, 10)` accepting the whole string, as fuzzel's
/// `str_to_ulong`: leading space and a sign are allowed, a negative number
/// wraps, and an empty string is zero.
fn strtoul(text: &str, radix: u32) -> Option<u64> {
    let trimmed = text.trim_start_matches(is_space);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, trimmed.get(1..).unwrap_or_default()),
        Some(b'+') => (false, trimmed.get(1..).unwrap_or_default()),
        _ => (false, trimmed),
    };
    let digits = if radix == 16 {
        digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
            .unwrap_or(digits)
    } else {
        digits
    };
    if digits.is_empty() {
        // No conversion: `end` is the start, so only an empty string ends
        // there.
        return text.is_empty().then_some(0);
    }
    let value = u64::from_str_radix(digits, radix).ok()?;
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

impl Parser {
    fn new(env: &Env, fatal: bool) -> Self {
        let workers = std::thread::available_parallelism()
            .ok()
            .and_then(|count| u16::try_from(count.get()).ok())
            .unwrap_or(1);
        let mut config = Config::defaults(workers);
        if let Some(terminal) = &env.terminal {
            config.terminal = Some(format!("{terminal} -e"));
        }
        Self {
            config,
            diagnostics: Vec::new(),
            fatal,
            home: env.home.clone(),
        }
    }

    /// Parse a file that was found, apply the overrides, and finish.
    fn read(mut self, bytes: &[u8], path: PathBuf, overrides: &[String]) -> Loaded {
        self.push(
            Level::Info,
            format!("loading configuration from {}", path.display()),
        );
        let shown = path.display().to_string();
        let parsed = self.parse_file(bytes, &shown);
        let ok = parsed && self.apply_overrides(overrides);
        let fatal = self.fatal;
        self.finish(ok || !fatal, Some(path))
    }

    fn push(&mut self, level: Level, text: String) {
        self.diagnostics.push(Diagnostic { level, text });
    }

    fn error(&mut self, text: String) {
        self.push(Level::Error, text);
    }

    fn finish(self, ok: bool, path: Option<PathBuf>) -> Loaded {
        Loaded {
            config: self.config,
            ok,
            path,
            diagnostics: self.diagnostics,
        }
    }

    /// fuzzel's `parse_config_file`. False only when `fatal` and a line was
    /// refused.
    fn parse_file(&mut self, bytes: &[u8], path: &str) -> bool {
        let text = String::from_utf8_lossy(bytes);
        let mut section = Some(Section::Main);
        let mut section_name = "main".to_owned();
        for (at, raw) in text.split_inclusive('\n').enumerate() {
            let number = u32::try_from(at + 1).unwrap_or(u32::MAX);
            let line = raw.trim_start_matches(is_space);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_suffix('\n').unwrap_or(line);
            // A comment starts at a blank followed by `#`, from the second
            // character on.
            let mut cut = line.len();
            let chars: Vec<(usize, char)> = line.char_indices().collect();
            for pair in chars.windows(2).skip(1) {
                if let [(at, blank), (_, '#')] = pair
                    && (*blank == ' ' || *blank == '\t')
                {
                    cut = *at + 1;
                    break;
                }
            }
            let key_value = line.get(..cut).unwrap_or(line).trim_end_matches(is_space);

            if let Some(rest) = key_value.strip_prefix('[') {
                if !self.open_section(rest, &mut section, &mut section_name, path, number) {
                    return false;
                }
                continue;
            }

            let Some(current) = section else {
                // The last section name was invalid: its keys are ignored.
                continue;
            };
            let (_, key, value) = split_key_value(key_value, false);
            let context = Context {
                path,
                line: number,
                section: &section_name,
                key: key.as_deref(),
                value: value.as_deref(),
            };
            let (Some(key), Some(value)) = (key.as_deref(), value.as_deref()) else {
                let missing = if key.is_none() { "key" } else { "value" };
                let text = Context {
                    value: None,
                    ..context
                }
                .say(&format!("syntax error: key/value pair has no {missing}"));
                self.error(text);
                if self.fatal {
                    return false;
                }
                continue;
            };
            if !self.option(current, key, value, &context) && self.fatal {
                return false;
            }
        }
        true
    }

    /// A `[section]` line: the section it opens, or a diagnostic and no
    /// section, whose keys are then ignored. False when that ends the parse.
    fn open_section(
        &mut self,
        rest: &str,
        section: &mut Option<Section>,
        section_name: &mut String,
        path: &str,
        number: u32,
    ) -> bool {
        match header(rest) {
            Ok((known, name)) => {
                *section = Some(known);
                name.clone_into(section_name);
                true
            }
            Err((shown, message)) => {
                *section = None;
                let context = Context {
                    path,
                    line: number,
                    section: shown.unwrap_or(section_name),
                    key: None,
                    value: None,
                };
                self.error(context.say(&message));
                !self.fatal
            }
        }
    }

    /// fuzzel's `overrides_apply`, then `resolve_key_binding_collisions`.
    fn apply_overrides(&mut self, overrides: &[String]) -> bool {
        for (at, item) in overrides.iter().enumerate() {
            let number = u32::try_from(at + 1).unwrap_or(u32::MAX);
            let (section, key, value) = split_key_value(item, true);
            let section = section.unwrap_or_default();
            let context = Context {
                path: "override",
                line: number,
                section: &section,
                key: key.as_deref(),
                value: value.as_deref(),
            };
            let (Some(key), Some(value)) = (key.as_deref(), value.as_deref()) else {
                let missing = if key.is_none() { "key" } else { "value" };
                let text = context.say(&format!("syntax error: key/value pair has no {missing}"));
                self.error(text);
                if self.fatal {
                    return false;
                }
                continue;
            };
            if section.is_empty() {
                let text = context.say("empty section name");
                self.error(text);
                if self.fatal {
                    return false;
                }
                continue;
            }
            let Some(known) = Section::named(&section) else {
                let text = context.say(&format!("invalid section name: {section}"));
                self.error(text);
                if self.fatal {
                    return false;
                }
                continue;
            };
            if !self.option(known, key, value, &context) && self.fatal {
                return false;
            }
        }
        self.resolve_collisions()
    }

    /// Remove every binding that repeats an earlier one's modifiers and key
    /// for another action, saying so, as fuzzel does.
    fn resolve_collisions(&mut self) -> bool {
        let mut ok = true;
        let mut at = 1;
        while at < self.config.bindings.len() {
            let Some(one) = self.config.bindings.get(at).cloned() else {
                break;
            };
            let earlier = self
                .config
                .bindings
                .get(..at)
                .unwrap_or_default()
                .iter()
                .rev()
                .find(|other| {
                    other.action != one.action && other.mods == one.mods && other.sym == one.sym
                })
                .map(|other| other.action);
            if let Some(taken) = earlier {
                let (path, line) = one
                    .origin
                    .clone()
                    .unwrap_or_else(|| ("(null)".to_owned(), 0));
                self.error(format!(
                    "{path}:{line}: [key-bindings].{}: {}{} already mapped to '{}'",
                    one.action.name(),
                    one.mods.prefix(),
                    one.sym,
                    taken.name()
                ));
                ok = false;
                let _ = self.config.bindings.remove(at);
                continue;
            }
            at += 1;
        }
        ok
    }

    /// One `key=value` in `section`. False, with a diagnostic, for a line
    /// fuzzel refuses.
    fn option(&mut self, section: Section, key: &str, value: &str, context: &Context<'_>) -> bool {
        let result = match section {
            Section::Main => self.main(key, value),
            Section::Colors => self.colors(key, value),
            Section::Border => self.border(key, value),
            Section::Dmenu => self.dmenu(key, value),
            Section::KeyBindings => self.key_bindings(key, value, context),
        };
        match result {
            Ok(()) => true,
            Err(message) => {
                if !message.is_empty() {
                    let text = context.say(&message);
                    self.error(text);
                }
                false
            }
        }
    }

    fn main(&mut self, key: &str, value: &str) -> Result<(), String> {
        let c = &mut self.config;
        match key {
            "include" => return self.include(value),
            "namespace" => value.clone_into(&mut c.namespace),
            "output" => c.output = Some(value.to_owned()),
            "font" => value.clone_into(&mut c.font),
            "use-bold" => c.use_bold = boolean(value)?,
            "dpi-aware" => {
                c.dpi_aware = if value == "auto" {
                    DpiAware::Auto
                } else if boolean(value)? {
                    DpiAware::Yes
                } else {
                    DpiAware::No
                };
            }
            "gamma-correct-blending" => c.gamma_correct = boolean(value)?,
            "render-workers" => c.render_workers = uint16(value)?,
            "match-workers" => c.match_workers = uint16(value)?,
            "prompt" => value.clone_into(&mut c.prompt),
            "placeholder" => value.clone_into(&mut c.placeholder),
            "message" => c.message = Some(value.to_owned()),
            "message-mode" => {
                c.message_mode = one_of(
                    value,
                    &[("wrap", MessageMode::Wrap), ("expand", MessageMode::Expand)],
                )?;
            }
            "icon-theme" => value.clone_into(&mut c.icon_theme),
            "icons-enabled" => c.icons_enabled = boolean(value)?,
            "hide-before-typing" => c.hide_before_typing = boolean(value)?,
            "list-executables-in-path" => c.list_executables_in_path = boolean(value)?,
            "fields" => c.fields = fields(value)?,
            "password-character" => {
                let mut chars = value.chars();
                let first = chars.next();
                if chars.next().is_some() {
                    return Err(
                        "password character must be a single character, or empty".to_owned()
                    );
                }
                c.password.character = first;
                c.password.character_set = true;
            }
            "filter-desktop" => c.filter_desktop = boolean(value)?,
            "match-mode" => {
                c.match_mode = one_of(
                    value,
                    &[
                        ("exact", MatchMode::Exact),
                        ("fzf", MatchMode::Fzf),
                        ("fuzzy", MatchMode::Fuzzy),
                    ],
                )?;
            }
            "sort-result" => c.sort_result = boolean(value)?,
            "match-counter" => c.match_counter = boolean(value)?,
            "delayed-filter-ms" => c.delayed_filter_ms = uint32(value)?,
            "delayed-filter-limit" => c.delayed_filter_limit = uint32(value)?,
            "show-actions" => c.show_actions = boolean(value)?,
            "terminal" => c.terminal = Some(value.to_owned()),
            "launch-prefix" => c.launch_prefix = Some(value.to_owned()),
            "anchor" => c.anchor = anchor(value)?,
            "x-margin" => c.x_margin = uint32(value)?,
            "y-margin" => c.y_margin = uint32(value)?,
            "lines" => c.lines = uint32(value)?,
            "minimal-lines" => c.minimal_lines = boolean(value)?,
            "hide-prompt" => c.hide_prompt = boolean(value)?,
            "width" => c.chars = uint32(value)?,
            "tabs" => c.tabs = uint32(value)?,
            "horizontal-pad" => c.pad.x = uint32(value)?,
            "vertical-pad" => c.pad.y = uint32(value)?,
            "inner-pad" => c.pad.inner = uint32(value)?,
            "line-height" => c.line_height = Some(pt_or_px(value)?),
            "letter-spacing" => c.letter_spacing = pt_or_px(value)?,
            "image-size-ratio" => {
                let ratio = decimal(value)?;
                if !(0.0..=1.0).contains(&ratio) {
                    return Err("not in range 0.0 - 1.0".to_owned());
                }
                c.image_size_ratio = ratio;
            }
            "scaling-filter" => {
                use ScalingFilter as S;
                c.scaling_filter = one_of(
                    value,
                    &[
                        ("none", S::None),
                        ("nearest", S::Nearest),
                        ("bilinear", S::Bilinear),
                        ("box", S::Box),
                        ("linear", S::Linear),
                        ("cubic", S::Cubic),
                        ("lanczos2", S::Lanczos2),
                        ("lanczos3", S::Lanczos3),
                        ("lanczos3-stretched", S::Lanczos3Stretched),
                    ],
                )?;
            }
            "layer" => {
                c.layer = if value.eq_ignore_ascii_case("top") {
                    Layer::Top
                } else if value.eq_ignore_ascii_case("overlay") {
                    Layer::Overlay
                } else {
                    return Err("not one of 'top', 'overlay'".to_owned());
                };
            }
            "keyboard-focus" => {
                c.keyboard_focus = if value.eq_ignore_ascii_case("exclusive") {
                    KeyboardFocus::Exclusive
                } else if value.eq_ignore_ascii_case("on-demand") {
                    KeyboardFocus::OnDemand
                } else {
                    return Err("not one of 'exclusive', 'on-demand'".to_owned());
                };
            }
            "exit-on-keyboard-focus-loss" => c.exit_on_keyboard_focus_loss = boolean(value)?,
            "cache" => c.cache = Some(value.to_owned()),
            "auto-select" => c.auto_select = boolean(value)?,
            "enable-mouse" => c.enable_mouse = boolean(value)?,
            _ => return Err(format!("not a valid option: {key}")),
        }
        Ok(())
    }

    /// `include=`: another file, parsed with its own section scope. An
    /// error inside it has been said already, so a failure here carries no
    /// message of its own.
    fn include(&mut self, value: &str) -> Result<(), String> {
        let path = if let Some(rest) = value.strip_prefix("~/") {
            let Some(home) = self.home.clone() else {
                return Err("failed to expand '~'".to_owned());
            };
            format!("{home}/{rest}")
        } else {
            value.to_owned()
        };
        if !path.starts_with('/') {
            return Err("not an absolute path".to_owned());
        }
        let bytes =
            std::fs::read(&path).map_err(|error| format!("failed to open: {}", errno(&error)))?;
        let ok = self.parse_file(&bytes, &path);
        self.push(
            Level::Info,
            format!("imported sub-configuration from {path}"),
        );
        if ok { Ok(()) } else { Err(String::new()) }
    }

    fn colors(&mut self, key: &str, value: &str) -> Result<(), String> {
        let c = &mut self.config.colors;
        let slot = match key {
            "background" => &mut c.background,
            "text" => &mut c.text,
            "message" => &mut c.message,
            "prompt" => &mut c.prompt,
            "placeholder" => &mut c.placeholder,
            "input" => &mut c.input,
            "match" => &mut c.matched,
            "selection" => &mut c.selection,
            "selection-text" => &mut c.selection_text,
            "selection-match" => &mut c.selection_match,
            "counter" => &mut c.counter,
            "border" => &mut c.border,
            _ => return Err(format!("not a valid option: {key}")),
        };
        *slot = color(value)?;
        Ok(())
    }

    fn border(&mut self, key: &str, value: &str) -> Result<(), String> {
        let b = &mut self.config.border;
        match key {
            "width" => b.width = uint32(value)?,
            "radius" => b.radius = uint32(value)?,
            "selection-radius" => b.selection_radius = uint32(value)?,
            _ => return Err(format!("not a valid option: {key}")),
        }
        Ok(())
    }

    fn dmenu(&mut self, key: &str, value: &str) -> Result<(), String> {
        let d = &mut self.config.dmenu;
        match key {
            "mode" => {
                d.mode = one_of(
                    value,
                    &[("text", DmenuMode::Text), ("index", DmenuMode::Index)],
                )?;
            }
            "exit-immediately-if-empty" => d.exit_immediately_if_empty = boolean(value)?,
            _ => return Err(format!("not a valid option: {key}")),
        }
        Ok(())
    }

    fn key_bindings(
        &mut self,
        key: &str,
        value: &str,
        context: &Context<'_>,
    ) -> Result<(), String> {
        let Some(action) = Action::named(key) else {
            return Err(format!("not a valid action: {key}"));
        };
        let bindings = &mut self.config.bindings;
        if value.eq_ignore_ascii_case("none") {
            bindings.retain(|binding| binding.action != action);
            return Ok(());
        }
        let mut new = Vec::new();
        for combo in value.split(' ').filter(|combo| !combo.is_empty()) {
            let (modifiers, name) = match combo.rfind('+') {
                None => (None, combo),
                Some(at) => (combo.get(..at), combo.get(at + 1..).unwrap_or_default()),
            };
            let mods = match modifiers {
                None => Mods::default(),
                Some(text) => parse_modifiers(text)?,
            };
            let Some(sym) = keysym::canonical(name) else {
                return Err(format!("not a valid XKB key name: {name}"));
            };
            new.push(Binding {
                action,
                mods,
                sym,
                origin: Some((context.path.to_owned(), context.line)),
            });
        }
        if new.is_empty() {
            return Err("empty binding not allowed (set to 'none' to unmap)".to_owned());
        }
        bindings.retain(|binding| binding.action != action);
        bindings.extend(new);
        Ok(())
    }
}

/// What a line starting `[` says, `rest` being what follows the bracket:
/// the section it opens and its name, or the section name the diagnostic
/// shows (`None` for the current one) and fuzzel's complaint.
fn header(rest: &str) -> Result<(Section, &str), (Option<&str>, String)> {
    if rest.starts_with(']') {
        return Err((None, "empty section name".to_owned()));
    }
    let Some((name, trailing)) = rest.split_once(']') else {
        return Err((Some(rest), "syntax error: no closing ']'".to_owned()));
    };
    if !trailing.is_empty() {
        return Err((
            Some(name),
            "section declaration contains trailing characters".to_owned(),
        ));
    }
    Section::named(name)
        .map(|known| (known, name))
        .ok_or_else(|| (Some(name), format!("invalid section name: {name}")))
}

/// fuzzel's `parse_modifiers`: `+`-separated XKB modifier names, or a
/// prefix of `none` (the `strncmp` fuzzel uses), which is no modifier.
fn parse_modifiers(text: &str) -> Result<Mods, String> {
    if "none".starts_with(text) {
        return Ok(Mods::default());
    }
    let mut mods = Mods::default();
    for name in text.split('+').filter(|name| !name.is_empty()) {
        match name {
            "Shift" => mods.shift = true,
            "Control" => mods.ctrl = true,
            "Mod1" => mods.alt = true,
            "Mod4" => mods.logo = true,
            _ => return Err(format!("not a valid modifier name: {name}")),
        }
    }
    Ok(mods)
}

/// `value_to_bool`.
fn boolean(value: &str) -> Result<bool, String> {
    let is = |words: &[&str]| words.iter().any(|word| value.eq_ignore_ascii_case(word));
    if is(&["on", "true", "yes", "1"]) {
        Ok(true)
    } else if is(&["off", "false", "no", "0"]) {
        Ok(false)
    } else {
        Err("invalid boolean value".to_owned())
    }
}

/// `value_to_uint32`.
fn uint32(value: &str) -> Result<u32, String> {
    strtoul(value, 10)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("invalid integer value, or outside range 0-{}", u32::MAX))
}

/// `value_to_uint16`, whose message names the thirty-two-bit limit, as
/// fuzzel's does.
fn uint16(value: &str) -> Result<u16, String> {
    strtoul(value, 10)
        .and_then(|v| u16::try_from(v).ok())
        .ok_or_else(|| format!("invalid integer value, or outside range 0-{}", u32::MAX))
}

/// `value_to_double`, which is `strtof`.
fn decimal(value: &str) -> Result<f32, String> {
    value
        .trim_start_matches(is_space)
        .parse::<f32>()
        .map_err(|_| "invalid decimal value".to_owned())
}

/// `value_to_pt_or_px`.
fn pt_or_px(value: &str) -> Result<PtOrPx, String> {
    if let Some(number) = value.strip_suffix("px") {
        let trimmed = number.trim_start_matches(is_space);
        return trimmed
            .parse::<i64>()
            .map(PtOrPx::Px)
            .map_err(|_| "invalid px value (must be on the form 12px)".to_owned());
    }
    decimal(value).map(PtOrPx::Pt)
}

/// `value_to_color`, which takes alpha.
fn color(value: &str) -> Result<Rgba, String> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.len() != 8 {
        return Err("not a valid color value".to_owned());
    }
    strtoul(digits, 16)
        .and_then(|v| u32::try_from(v).ok())
        .map(Rgba)
        .ok_or_else(|| "not a valid color value".to_owned())
}

/// `value_to_enum`: case-insensitive, and the valid words named on failure.
fn one_of<T: Copy>(value: &str, map: &[(&str, T)]) -> Result<T, String> {
    if let Some((_, found)) = map
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(value))
    {
        return Ok(*found);
    }
    let names: Vec<String> = map.iter().map(|(name, _)| format!("'{name}'")).collect();
    Err(format!("not one of {}", names.join(", ")))
}

/// `anchor`, matched with `strcmp`.
fn anchor(value: &str) -> Result<Anchor, String> {
    ANCHORS
        .iter()
        .find(|(name, _)| *name == value)
        .map(|(_, anchor)| *anchor)
        .ok_or_else(|| {
            // fuzzel's message, whose missing comma runs two names together.
            format!(
                "invalid anchor \"{value}\", must be one of: \"center\", \"top-left\", \"top\", \
                 \"top-right\", \"right\", \"bottom-right\", \"bottom\"\"bottom-left\", \"left\""
            )
        })
}

/// `fields`.
fn fields(value: &str) -> Result<Fields, String> {
    let mut out = 0;
    for field in value.split(',').filter(|field| !field.is_empty()) {
        out |= match field {
            "filename" => Fields::FILENAME,
            "name" => Fields::NAME,
            "generic" => Fields::GENERIC,
            "exec" => Fields::EXEC,
            "categories" => Fields::CATEGORIES,
            "keywords" => Fields::KEYWORDS,
            "comment" => Fields::COMMENT,
            _ => {
                return Err(format!(
                    "invalid field name \"{field}\", must be one of: \"filename\", \"name\", \
                     \"generic\", \"exec\", \"categories\", \"keywords\", \"comment\""
                ));
            }
        };
    }
    Ok(Fields(out))
}

#[cfg(test)]
mod tests;
