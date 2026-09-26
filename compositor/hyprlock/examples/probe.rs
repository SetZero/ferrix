//! `cargo run -p compositor-hyprlock --example probe [FILE]`: read a
//! `hyprlock.conf` (the user's own by default) on the host and say what
//! hyprlock on Ferrix makes of it -- every diagnostic, every line it cannot
//! carry out, and which widgets each of the named screens gets. Nothing is
//! drawn and nothing is locked.

use std::path::PathBuf;

use compositor_hyprlock::config::Config;

fn main() {
    let path = std::env::args().nth(1).map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".config/hypr/hyprlock.conf")
        },
        PathBuf::from,
    );
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            compositor_hyprlock::say(&format!("{}: {error}", path.display()));
            std::process::exit(1);
        }
    };
    let config = Config::parse(&text, &path);
    let say = compositor_hyprlock::say;
    say(&format!(
        "{}: {} widget(s)",
        path.display(),
        config.widgets.len()
    ));
    for diagnostic in &config.diagnostics {
        say(&format!("diagnostic: {diagnostic}"));
    }
    for line in config.unsupported() {
        say(&format!("unsupported: {line}"));
    }
    say(&format!(
        "general: hide_cursor {} ignore_empty_input {} fail_timeout {}",
        config.general.hide_cursor, config.general.ignore_empty_input, config.general.fail_timeout
    ));
    say(&format!(
        "animations: fadeIn {} ms, fadeOut {} ms, inputFieldDots {} ms",
        config.animations.duration("fadeIn"),
        config.animations.duration("fadeOut"),
        config.animations.duration("inputFieldDots")
    ));
    // Every `desc:` the file names, and a screen that is none of them.
    let mut screens: Vec<(String, String)> = config
        .widgets
        .iter()
        .filter_map(|widget| widget.monitor().strip_prefix("desc:"))
        .map(|description| ("DP-?".to_owned(), format!("{description} (DP-?)")))
        .collect();
    screens.sort();
    screens.dedup();
    screens.push(("Virtual-1".to_owned(), "a QEMU screen".to_owned()));
    for (name, description) in screens {
        let kinds: Vec<&str> = config
            .widgets_for(&name, &description)
            .iter()
            .map(|widget| widget.kind())
            .collect();
        say(&format!("{description}: {}", kinds.join(", ")));
    }
}
