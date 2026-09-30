//! The page Chrome opens on a desktop: `assets/start/index.html`, carried to
//! [`PATH`] and opened as [`URL`] by `run-compositor --chrome` and
//! `flash --compositor --chrome`.
//!
//! It is where somebody who has just been handed a Ferrix desktop finds their
//! way: one box that searches the web, opens an address, or opens a path in
//! the guest's own tree; tiles for the places in that tree worth a look,
//! which Chrome lists as it lists any directory; the keys the desktop binds;
//! and the tone the first page played, sound from a page through `/dev/snd`
//! (`docs/AUDIO.md` §4).
//!
//! The keys are this boot's: [`file()`] reads them out of the configuration
//! the desktop is about to be given, so a boot of the customer's own
//! `hyprland.conf` lists the customer's binds and not `RUN_CONFIG`'s.
//! They, the architecture, the commit and the libc Chrome runs on go into
//! the page as one JSON object in place of the `{}` it is kept with.

use std::fmt::Write as _;

use crate::args::Args;
use crate::paths::{self, Arch};
use crate::ports::{Content, File};
use crate::{Error, Result};

/// Where the page is kept in the tree.
const SOURCE: &str = "assets/start/index.html";

/// Where it goes in the image.
const PATH: &str = "usr/share/ferrix/start/index.html";

/// What Chrome is started on. No spaces, because the compositor splits
/// `exec-once` at them.
pub(crate) const URL: &str = "file:///usr/share/ferrix/start/index.html";

/// The element whose `{}` the boot's facts replace.
const SLOT: &str = r#"<script id="ferrix" type="application/json">{}</script>"#;

/// With `--chrome`, the page into `ports`, for a desktop of `arch` whose
/// configuration is `config`, at `config_path` in the image.
pub(crate) fn carry(
    arch: Arch,
    config: &str,
    config_path: &str,
    ports: &mut Vec<File>,
    args: &Args,
) -> Result<()> {
    if args.chrome {
        let ferrousli = crate::chrome::on_ferrousli(args);
        ports.push(file(arch, config, &format!("/{config_path}"), ferrousli)?);
    }
    Ok(())
}

/// The page for a desktop of `arch` whose configuration is `config`, which
/// the guest reads at `config_path`, with Chrome on ferrousli's libc or the
/// volume's glibc.
fn file(arch: Arch, config: &str, config_path: &str, ferrousli: bool) -> Result<File> {
    let path = paths::workspace_root().join(SOURCE);
    let page = std::fs::read_to_string(&path)
        .map_err(|error| Error::new(format!("{}: {error}", path.display())))?;
    let (arch, commit) = (arch.to_string(), commit());
    let facts = Facts {
        arch: &arch,
        commit: commit.as_deref(),
        libc: if ferrousli { "ferrousli" } else { "glibc" },
        config: config_path,
        binds: &binds(config),
    };
    Ok(File {
        path: PATH.to_owned(),
        mode: 0o644,
        content: Content::Bytes(fill(&page, &facts)?.into_bytes()),
    })
}

/// The commit the image was built from, short, if git can say.
fn commit() -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(paths::workspace_root())
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    let hash = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !hash.is_empty()).then_some(hash)
}

/// What the page is told about its boot.
struct Facts<'a> {
    arch: &'a str,
    commit: Option<&'a str>,
    libc: &'a str,
    config: &'a str,
    binds: &'a [Bind],
}

/// `page` with `facts` in its slot.
fn fill(page: &str, facts: &Facts<'_>) -> Result<String> {
    if !page.contains(SLOT) {
        return Err(Error::new(format!(
            "{SOURCE} has no `{SLOT}` for the boot's facts"
        )));
    }
    let mut json = String::from("{");
    let _ = write!(json, "\"arch\":{}", string(facts.arch));
    if let Some(commit) = facts.commit {
        let _ = write!(json, ",\"commit\":{}", string(commit));
    }
    let _ = write!(
        json,
        ",\"libc\":{},\"config\":{}",
        string(facts.libc),
        string(facts.config)
    );
    json.push_str(",\"binds\":[");
    for (index, bind) in facts.binds.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let keys: Vec<String> = bind.keys.iter().map(|key| string(key)).collect();
        let _ = write!(
            json,
            "{{\"keys\":[{}],\"does\":{}",
            keys.join(","),
            string(&bind.does)
        );
        if let Some(command) = &bind.command {
            let _ = write!(json, ",\"command\":{}", string(command));
        }
        json.push('}');
    }
    json.push_str("]}");
    let filled = SLOT.replacen("{}", &json, 1);
    Ok(page.replacen(SLOT, &filled, 1))
}

/// `text` as a JSON string that is also safe inside a `<script>` element:
/// `<` is escaped too, so no `</script>` in a command line can end it.
fn string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '<' | '>' | '&' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One key the desktop binds: the keys pressed together, in words, and what
/// they do, in words, with the command itself when it runs one.
#[derive(Debug, PartialEq)]
struct Bind {
    keys: Vec<String>,
    does: String,
    command: Option<String>,
}

/// The binds of `config`, in its order: every `bind` line outside a submap,
/// with `$variables` expanded as hyprland expands them. A bind inside a
/// submap only works once that submap is entered, and the key that enters
/// it is listed.
fn binds(config: &str) -> Vec<Bind> {
    let mut variables: Vec<(String, String)> = Vec::new();
    let mut submap = false;
    let mut out: Vec<Bind> = Vec::new();
    for line in config.lines() {
        let line = line.split('#').next().unwrap_or_default();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), expand(value.trim(), &variables));
        if let Some(name) = key.strip_prefix('$') {
            variables.retain(|(known, _)| known != name);
            variables.push((name.to_owned(), value));
            // Longest first, so `$mod` never eats the start of `$modifier`.
            variables.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
            continue;
        }
        if key == "submap" {
            submap = value != "reset";
            continue;
        }
        let Some(flags) = key.strip_prefix("bind") else {
            continue;
        };
        if submap || !flags.chars().all(|c| c.is_ascii_lowercase()) {
            continue;
        }
        if let Some(bind) = bind(flags, &value)
            && !out.contains(&bind)
        {
            out.push(bind);
        }
    }
    out
}

/// `text` with each `$name` in `variables` replaced by its value.
fn expand(text: &str, variables: &[(String, String)]) -> String {
    let mut text = text.to_owned();
    for (name, value) in variables {
        text = text.replace(&format!("${name}"), value);
    }
    text
}

/// One `bind<flags> = MODS, KEY, [description,] DISPATCHER[, ARGS]` line's
/// value, in words.
fn bind(flags: &str, value: &str) -> Option<Bind> {
    let described = flags.contains('d');
    let mut fields = value
        .splitn(if described { 5 } else { 4 }, ',')
        .map(str::trim);
    let mods = fields.next()?;
    let key = fields.next()?;
    let description = if described { fields.next() } else { None };
    let dispatcher = fields.next()?;
    let args = fields.next().unwrap_or_default();
    if key.is_empty() || dispatcher.is_empty() {
        return None;
    }
    let mut keys: Vec<String> = mods
        .split(|c: char| c.is_whitespace() || c == '_')
        .filter(|word| !word.is_empty())
        .map(modifier)
        .collect();
    keys.push(key_name(key));
    let command = matches!(dispatcher, "exec" | "execr").then(|| args.to_owned());
    let does = match description {
        Some(text) if !text.is_empty() => text.to_owned(),
        _ if flags.contains('m') => dragged(dispatcher),
        _ => dispatch(dispatcher, args),
    };
    Some(Bind {
        keys,
        does,
        command,
    })
}

/// A modifier as a keyboard's keycap says it.
fn modifier(word: &str) -> String {
    match word.to_ascii_uppercase().as_str() {
        "SUPER" | "WIN" | "LOGO" | "MOD4" | "META" => "Super",
        "SHIFT" => "Shift",
        "CTRL" | "CONTROL" => "Ctrl",
        "ALT" | "MOD1" => "Alt",
        _ => return word.to_owned(),
    }
    .to_owned()
}

/// A key as a keyboard's keycap says it.
fn key_name(key: &str) -> String {
    let named = match key.to_ascii_lowercase().as_str() {
        "return" | "enter" => "Return",
        "space" => "Space",
        "escape" => "Esc",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "print" => "Print",
        "mouse:272" => "Left button",
        "mouse:273" => "Right button",
        "mouse:274" => "Middle button",
        "mouse_down" => "Wheel down",
        "mouse_up" => "Wheel up",
        "xf86audioraisevolume" => "Volume up",
        "xf86audiolowervolume" => "Volume down",
        "xf86audiomute" => "Mute",
        "xf86monbrightnessup" => "Brightness up",
        "xf86monbrightnessdown" => "Brightness down",
        _ => {
            let mut chars = key.chars();
            return match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            };
        }
    };
    named.to_owned()
}

/// What a `bindm` line's dispatcher does while its button is held.
fn dragged(dispatcher: &str) -> String {
    match dispatcher {
        "movewindow" => "Drag the window".to_owned(),
        "resizewindow" => "Resize the window by dragging".to_owned(),
        other => other.to_owned(),
    }
}

/// What `dispatcher` with `args` does, in words.
fn dispatch(dispatcher: &str, args: &str) -> String {
    let direction = |word: &str| match word {
        "l" | "left" => "left",
        "r" | "right" => "right",
        "u" | "up" | "t" => "up",
        "d" | "down" | "b" => "down",
        _ => "",
    };
    match dispatcher {
        "exec" | "execr" => program(args),
        "killactive" => "Close the window".to_owned(),
        "exit" => "Leave the desktop".to_owned(),
        "fullscreen" => "Fullscreen".to_owned(),
        "togglefloating" => "Float or tile the window".to_owned(),
        "togglesplit" => "Turn the split".to_owned(),
        "pseudo" => "Pseudotile the window".to_owned(),
        "togglegroup" => "Group the windows".to_owned(),
        "togglespecialworkspace" => "Show the scratchpad".to_owned(),
        "movefocus" if !direction(args).is_empty() => format!("Focus {}", direction(args)),
        "movewindow" if !direction(args).is_empty() => {
            format!("Move the window {}", direction(args))
        }
        "movewindow" if let Some(monitor) = args.strip_prefix("mon:") => {
            format!("Move the window to monitor {monitor}")
        }
        "swapwindow" if !direction(args).is_empty() => {
            format!("Swap the window {}", direction(args))
        }
        "workspace" => workspace(args),
        "movetoworkspace" | "movetoworkspacesilent" => {
            format!("Send the window to {}", workspace(args).to_lowercase())
        }
        "submap" => format!("Enter the {args} keys"),
        "resizeactive" => "Resize the window".to_owned(),
        _ => format!("{dispatcher} {args}").trim_end().to_owned(),
    }
}

/// A workspace argument in words.
fn workspace(args: &str) -> String {
    match args {
        "e+1" | "m+1" | "r+1" => "Next workspace".to_owned(),
        "e-1" | "m-1" | "r-1" => "Previous workspace".to_owned(),
        _ => match args.strip_prefix("special:") {
            Some(name) => format!("Special workspace {name}"),
            None => format!("Workspace {args}"),
        },
    }
}

/// What an `exec` command runs, in words: a program the desktop has by what
/// it is, anything else by its name and arguments.
fn program(command: &str) -> String {
    let mut words = command
        .split_whitespace()
        .skip_while(|word| word.contains('=') && !word.starts_with('-'));
    let Some(program) = words.next() else {
        return "Run nothing".to_owned();
    };
    let name = program.rsplit('/').next().unwrap_or(program);
    let rest: Vec<&str> = words.collect();
    let known = match name {
        "chrome" | "chromium" => "New Chrome window",
        "term" | "foot" | "kitty" | "alacritty" => "Terminal",
        "fuzzel" | "wofi" | "rofi" => "Launcher",
        "lock" | "hyprlock" => "Lock the screen",
        "shot" | "grim" => "Screenshot",
        "lswt" if rest.is_empty() => "List the windows",
        "pattern" => "A test pattern",
        _ => {
            let whole = std::iter::once(name)
                .chain(rest)
                .collect::<Vec<_>>()
                .join(" ");
            return if whole.chars().count() > 56 {
                format!("{}…", whole.chars().take(56).collect::<String>())
            } else {
                whole
            };
        }
    };
    known.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(keys: &[&str], does: &str, command: Option<&str>) -> Bind {
        Bind {
            keys: keys.iter().map(|&key| key.to_owned()).collect(),
            does: does.to_owned(),
            command: command.map(str::to_owned),
        }
    }

    #[test]
    fn binds_read_as_a_person_would_say_them() {
        let config = "\
$mainMod = SUPER
$terminal = foot
bind = $mainMod, RETURN, exec, $terminal # a comment
bind = $mainMod SHIFT, L, movewindow, r
bind = SUPER, 2, workspace, 2
bindd = SUPER, D, Open the launcher, exec, fuzzel
bindm = SUPER, mouse:272, movewindow
bind = SUPER, B, exec, HOME=/dev/shm /data/chrome-window/chrome --no-sandbox file:///x
bind = SUPER, R, submap, resize
submap = resize
bind = , L, resizeactive, 10 0
submap = reset
bind = SUPER, Q, killactive
bind = SUPER, Q, killactive
";
        assert_eq!(
            binds(config),
            vec![
                bind(&["Super", "Return"], "Terminal", Some("foot")),
                bind(&["Super", "Shift", "L"], "Move the window right", None),
                bind(&["Super", "2"], "Workspace 2", None),
                bind(&["Super", "D"], "Open the launcher", Some("fuzzel")),
                bind(&["Super", "Left button"], "Drag the window", None),
                bind(
                    &["Super", "B"],
                    "New Chrome window",
                    Some("HOME=/dev/shm /data/chrome-window/chrome --no-sandbox file:///x")
                ),
                bind(&["Super", "R"], "Enter the resize keys", None),
                bind(&["Super", "Q"], "Close the window", None),
            ]
        );
    }

    #[test]
    fn an_unknown_program_is_named_with_its_arguments() {
        assert_eq!(program("/bin/hyprctl clients"), "hyprctl clients");
        assert_eq!(program("/bin/pattern gradient another"), "A test pattern");
    }

    #[test]
    fn the_facts_cannot_close_the_script_element() {
        let page = format!("<p>{SLOT}</p>");
        let facts = Facts {
            arch: "x86_64",
            commit: Some("e1c62a83"),
            libc: "ferrousli",
            config: "/etc/hyprland.conf",
            binds: &[bind(&["Super", "X"], "say \"</script>\"", Some("echo \\"))],
        };
        let filled = fill(&page, &facts).unwrap();
        assert_eq!(
            filled,
            "<p><script id=\"ferrix\" type=\"application/json\">{\"arch\":\"x86_64\",\
             \"commit\":\"e1c62a83\",\"libc\":\"ferrousli\",\"config\":\"/etc/hyprland.conf\",\
             \"binds\":[{\"keys\":[\"Super\",\"X\"],\"does\":\"say \\\"\\u003c/script\\u003e\\\"\",\
             \"command\":\"echo \\\\\"}]}</script></p>"
        );
        assert!(fill("<p></p>", &facts).is_err());
    }

    /// The page in the tree has the slot [`fill`] needs.
    #[test]
    fn the_page_has_its_slot() {
        let page = std::fs::read_to_string(paths::workspace_root().join(SOURCE)).unwrap();
        assert!(page.contains(SLOT));
    }
}
