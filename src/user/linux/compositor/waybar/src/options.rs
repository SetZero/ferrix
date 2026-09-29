//! What of waybar's options this waybar carries out, for the probe.
//!
//! One table for the bar object and one per module kind, each option with
//! how far it is carried out and, where it is not, why. An option waybar
//! itself does not read is reported as such -- a typo, or an option from
//! another version.

/// How far an option is carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// As upstream.
    Done,
    /// In part; why.
    Partly(&'static str),
    /// Not at all; why.
    Not(&'static str),
    /// Not an option waybar reads.
    Unknown,
}

use Support::{Done, Not, Partly};

/// The bar object's own options.
const BAR: &[(&str, Support)] = &[
    ("output", Done),
    ("output-dimensions", Done),
    ("layer", Done),
    (
        "position",
        Partly("left and right bars are laid out as rows"),
    ),
    ("height", Done),
    ("width", Done),
    ("spacing", Done),
    ("margin", Done),
    ("margin-top", Done),
    ("margin-right", Done),
    ("margin-bottom", Done),
    ("margin-left", Done),
    ("modules-left", Done),
    ("modules-center", Done),
    ("modules-right", Done),
    ("name", Done),
    ("mode", Done),
    (
        "modes",
        Not("custom bar modes are not read; the presets are"),
    ),
    ("exclusive", Done),
    ("passthrough", Done),
    ("fixed-center", Done),
    ("no-center", Done),
    ("start_hidden", Not("the bar always starts shown")),
    ("include", Done),
    ("ipc", Not("sway's bar IPC; there is no sway")),
    ("id", Not("sway's bar IPC; there is no sway")),
    (
        "reload_style_on_change",
        Not("the stylesheet is read at start only"),
    ),
    ("on-sigusr1", Not("SIGUSR1 always toggles the bars")),
    ("on-sigusr2", Not("SIGUSR2's reload is not carried out")),
    ("expand-left", Not("sections are never expanded")),
    ("expand-center", Not("sections are never expanded")),
    ("expand-right", Not("sections are never expanded")),
    ("gtk-layer-shell", Done),
];

/// What every module reads (`AModule`, `ALabel`).
const COMMON: &[(&str, Support)] = &[
    ("format", Done),
    ("format-alt", Done),
    ("format-alt-click", Done),
    ("format-icons", Done),
    ("tooltip", Done),
    ("tooltip-format", Done),
    ("max-length", Done),
    ("min-length", Not("min-width-chars is not carried out")),
    ("rotate", Not("labels are never rotated")),
    ("align", Not("labels are always centred")),
    ("justify", Not("labels are always centred")),
    ("interval", Done),
    ("states", Done),
    ("on-click", Done),
    ("on-click-middle", Done),
    ("on-click-right", Done),
    ("on-click-backward", Done),
    ("on-click-forward", Done),
    ("on-click-release", Done),
    ("on-click-middle-release", Done),
    ("on-click-right-release", Done),
    ("on-double-click", Done),
    ("on-double-click-middle", Done),
    ("on-double-click-right", Done),
    ("on-triple-click", Done),
    ("on-triple-click-middle", Done),
    ("on-triple-click-right", Done),
    ("on-scroll-up", Done),
    ("on-scroll-down", Done),
    ("on-scroll-left", Done),
    ("on-scroll-right", Done),
    ("smooth-scrolling-threshold", Done),
    ("reverse-scrolling", Not("scrolls are never reversed")),
    ("reverse-mouse-scrolling", Not("scrolls are never reversed")),
    (
        "on-click-copy",
        Not("there is no clipboard to copy into here"),
    ),
    ("on-update", Not("nothing is run on an update")),
    ("expand", Not("modules are never expanded")),
    ("actions", Not("module actions are not carried out")),
    (
        "cursor",
        Partly("the hand over a clickable module is; a named cursor is not"),
    ),
    ("menu", Not("GtkBuilder menus are not carried out")),
    ("menu-file", Not("GtkBuilder menus are not carried out")),
    ("menu-actions", Not("GtkBuilder menus are not carried out")),
    ("hosts", Not("host-specific modules are not carried out")),
    (
        "disable-on-sleep",
        Not("there is no logind to say when the machine sleeps"),
    ),
];

/// Each module kind's own options.
fn own(kind: &str) -> Option<&'static [(&'static str, Support)]> {
    Some(match kind {
        "custom" => &[
            ("exec", Done),
            ("exec-if", Done),
            ("exec-on-event", Done),
            ("return-type", Done),
            ("restart-interval", Done),
            ("signal", Done),
            ("escape", Done),
            ("hide-empty-text", Done),
            ("image-path", Not("custom images are not carried out")),
            ("image-name", Not("custom images are not carried out")),
            ("icon-size", Not("custom images are not carried out")),
            ("icon", Not("custom images are not carried out")),
            ("icon-spacing", Done),
            ("swap-icon-label", Not("the image is never shown")),
        ],
        "hyprland/window" => &[
            ("separate-outputs", Done),
            ("rewrite", Not("regex replacements are not carried out")),
            ("fallback", Done),
            ("icon", Not("application icons are not carried out")),
            ("icon-size", Not("application icons are not carried out")),
        ],
        "cpu" => &[],
        "memory" => &[("unit", Done)],
        "network" => &[
            ("interface", Done),
            ("family", Partly("IPv4 only: the address is SIOCGIFADDR's")),
            ("rfkill", Not("Ferrix has no rfkill")),
            ("format-ethernet", Done),
            ("format-wifi", Not("Ferrix has no nl80211: never wifi")),
            ("format-linked", Done),
            ("format-disconnected", Done),
            ("format-disabled", Not("Ferrix has no rfkill")),
            ("tooltip-format-ethernet", Done),
            (
                "tooltip-format-wifi",
                Not("Ferrix has no nl80211: never wifi"),
            ),
            ("tooltip-format-linked", Done),
            ("tooltip-format-disconnected", Done),
        ],
        "pulseaudio" => &[
            ("format-muted", Done),
            ("format-bluetooth", Done),
            ("format-bluetooth-muted", Done),
            ("format-source", Done),
            ("format-source-muted", Done),
            ("scroll-step", Done),
            ("max-volume", Done),
            (
                "ignored-sinks",
                Not("only the server's default sink is read"),
            ),
            (
                "sink-mapping",
                Not("only the server's default sink is read"),
            ),
            ("target", Not("only the server's default sink is read")),
        ],
        "tray" => &[
            ("icon-size", Not("no StatusNotifierItem without D-Bus")),
            ("spacing", Not("no StatusNotifierItem without D-Bus")),
            (
                "show-passive-items",
                Not("no StatusNotifierItem without D-Bus"),
            ),
            (
                "reverse-direction",
                Not("no StatusNotifierItem without D-Bus"),
            ),
            ("ignore-list", Not("no StatusNotifierItem without D-Bus")),
            ("icons", Not("no StatusNotifierItem without D-Bus")),
        ],
        "clock" => &[
            ("timezone", Not("one time zone, the local one")),
            ("timezones", Not("one time zone, the local one")),
            ("locale", Not("the C locale only")),
            ("calendar", Not("{calendar} is empty")),
            (
                "timezone-tooltip-format",
                Not("one time zone, the local one"),
            ),
        ],
        _ => return None,
    })
}

/// A module's kind from its name: `custom/ws-1` is `custom`, `cpu#two` is
/// `cpu`.
#[must_use]
pub fn kind(name: &str) -> String {
    let reference = name.split_once('#').map_or(name, |(r, _)| r);
    if reference.starts_with("custom/") {
        "custom".to_owned()
    } else {
        reference.to_owned()
    }
}

/// Whether a bar key is a module's configuration rather than an option.
#[must_use]
pub fn is_module_name(key: &str) -> bool {
    own(&kind(key)).is_some() || key.contains('/')
}

/// How the bar option `key` is carried out.
#[must_use]
pub fn bar(key: &str) -> Support {
    BAR.iter()
        .find(|(name, _)| *name == key)
        .map_or(Support::Unknown, |(_, support)| *support)
}

/// Whether module `kind` is here at all, and how.
#[must_use]
pub fn module(kind: &str) -> Option<Support> {
    let support = match kind {
        "tray" => Not("StatusNotifierItem needs D-Bus, which Ferrix has not: always hidden"),
        "pulseaudio" => Partly(
            "the default sink alone; Ferrix's desktop runs pulsed only beside Chrome's sound, \
             and with no server the chip shows waybar's starting values",
        ),
        "clock" => Partly("the C locale, one time zone, no calendar"),
        "custom" | "hyprland/window" | "cpu" | "memory" | "network" => Done,
        _ => return None,
    };
    Some(support)
}

/// How option `key` of a module of `kind` is carried out.
#[must_use]
pub fn option(kind: &str, key: &str) -> Support {
    let format_state = |key: &str| key.starts_with("format-") || key.starts_with("tooltip-format-");
    own(kind)
        .and_then(|list| list.iter().find(|(name, _)| *name == key))
        .or_else(|| COMMON.iter().find(|(name, _)| *name == key))
        .map(|(_, support)| *support)
        .unwrap_or(
            if format_state(key) && matches!(kind, "cpu" | "memory" | "network" | "pulseaudio") {
                // `format-<state>` for a `states` name.
                Done
            } else {
                Support::Unknown
            },
        )
}

#[cfg(test)]
mod tests {
    use super::{Support, bar, kind, module, option};

    #[test]
    fn the_tables_answer_for_the_users_options() {
        assert_eq!(bar("height"), Support::Done);
        assert_eq!(bar("hieght"), Support::Unknown);
        assert_eq!(kind("custom/ws-1"), "custom");
        assert_eq!(kind("cpu#two"), "cpu");
        assert_eq!(option("custom", "exec"), Support::Done);
        assert_eq!(option("custom", "on-click-right"), Support::Done);
        assert_eq!(option("pulseaudio", "scroll-step"), Support::Done);
        assert!(matches!(option("pulseaudio", "target"), Support::Not(_)));
        assert!(matches!(option("tray", "icon-size"), Support::Not(_)));
        assert_eq!(option("cpu", "format-critical"), Support::Done);
        assert_eq!(option("cpu", "frobnicate"), Support::Unknown);
        assert!(matches!(module("tray"), Some(Support::Not(_))));
        assert_eq!(module("sway/workspaces"), None);
    }
}
