//! `hypridle.conf`, as upstream hypridle's `CConfigManager` reads it.
//!
//! The options, their types and their defaults are
//! `src/config/ConfigManager.cpp`'s `init()`, and the checks after the file
//! is read are its `postParse()`: a `listener` without a `timeout` is left
//! out with "Category has a missing timeout setting", and a file with no
//! listener at all says "No rules configured" and runs anyway.
//!
//! [`ferrix_notes`] is what upstream does not have: one line for each
//! option this file sets that cannot do on Ferrix what it does on Linux,
//! because there is no D-Bus, no logind and no suspend. The line says what
//! it does here instead.

use std::path::{Path, PathBuf};

use crate::conf::{self, Entry, Read};

/// `general { }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct General {
    /// Run when the session is asked to lock: `loginctl lock-session`.
    pub lock_cmd: String,
    /// Run when it is asked to unlock: `loginctl unlock-session`.
    pub unlock_cmd: String,
    /// Run once the compositor says the session is locked.
    pub on_lock_cmd: String,
    /// Run once it says the session is unlocked.
    pub on_unlock_cmd: String,
    /// Run before the machine suspends.
    pub before_sleep_cmd: String,
    /// Run after it wakes.
    pub after_sleep_cmd: String,
    /// Whether `org.freedesktop.ScreenSaver.Inhibit` is ignored.
    pub ignore_dbus_inhibit: bool,
    /// Whether logind's `idle` inhibitors are ignored.
    pub ignore_systemd_inhibit: bool,
    /// Whether Wayland's idle inhibitors are ignored, which is which of
    /// `ext_idle_notifier_v1`'s two requests every listener is made with.
    pub ignore_wayland_inhibit: bool,
    /// 0 off, 1 on, 2 auto, 3 until locked: how the suspend is delayed.
    pub inhibit_sleep: i64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            lock_cmd: String::new(),
            unlock_cmd: String::new(),
            on_lock_cmd: String::new(),
            on_unlock_cmd: String::new(),
            before_sleep_cmd: String::new(),
            after_sleep_cmd: String::new(),
            ignore_dbus_inhibit: false,
            ignore_systemd_inhibit: false,
            ignore_wayland_inhibit: false,
            inhibit_sleep: 2,
        }
    }
}

/// One `listener { }`: upstream's `STimeoutRule`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rule {
    /// Seconds without input before `on_timeout` runs.
    pub timeout: u64,
    /// Run when the seat has been idle for `timeout`.
    pub on_timeout: String,
    /// Run at the first input after `on_timeout` ran.
    pub on_resume: String,
    /// Whether an idle inhibitor is ignored for this one.
    pub ignore_inhibit: bool,
    /// Asked before `on_timeout` runs; a nonzero exit holds it off.
    pub condition_cmd: String,
    /// Seconds between asking `condition_cmd` again, or 0 to ask once.
    pub condition_retry: i64,
}

/// The whole file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// `general { }`.
    pub general: General,
    /// The listeners, in the order their blocks are in the file.
    pub rules: Vec<Rule>,
    /// What was wrong, in hyprlang's and hypridle's wording.
    pub errors: Vec<String>,
    /// Which `general` options the file set, by name, for
    /// [`ferrix_notes`]: a default is not something the person asked for.
    pub set: Vec<String>,
}

/// What an option holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    Int,
}

/// The `general` options, as `init()` declares them.
const GENERAL: [(&str, Kind); 10] = [
    ("lock_cmd", Kind::Text),
    ("unlock_cmd", Kind::Text),
    ("on_lock_cmd", Kind::Text),
    ("on_unlock_cmd", Kind::Text),
    ("before_sleep_cmd", Kind::Text),
    ("after_sleep_cmd", Kind::Text),
    ("ignore_dbus_inhibit", Kind::Int),
    ("ignore_systemd_inhibit", Kind::Int),
    ("ignore_wayland_inhibit", Kind::Int),
    ("inhibit_sleep", Kind::Int),
];

/// The `listener` options.
const LISTENER: [(&str, Kind); 6] = [
    ("timeout", Kind::Int),
    ("on-timeout", Kind::Text),
    ("on-resume", Kind::Text),
    ("ignore_inhibit", Kind::Int),
    ("condition_cmd", Kind::Text),
    ("condition_retry", Kind::Int),
];

/// An `int` as hyprlang's `configStringToInt` reads one: `0x` hex, the
/// words it takes for true and false (by prefix, as it checks them), or a
/// decimal number.
///
/// # Errors
///
/// Hyprlang's sentence for a value that is none of those.
pub fn parse_int(value: &str) -> Result<i64, String> {
    if let Some(hex) = value.strip_prefix("0x") {
        return i64::from_str_radix(hex, 16).map_err(|_| format!("invalid hex {value}"));
    }
    if ["true", "on", "yes"]
        .iter()
        .any(|word| value.starts_with(word))
    {
        return Ok(1);
    }
    if ["false", "off", "no"]
        .iter()
        .any(|word| value.starts_with(word))
    {
        return Ok(0);
    }
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("cannot parse \"{value}\" as an int."));
    }
    value
        .parse::<i64>()
        .map_err(|error| format!("stoll threw: {error}"))
}

/// Where upstream looks for the file when `-c` does not say:
/// `Hyprutils::Path::findConfig("hypridle")`.
#[must_use]
pub fn find(
    environment: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let absolute = |value: Option<String>| value.filter(|path| Path::new(path).is_absolute());
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(home) = absolute(environment("XDG_CONFIG_HOME")) {
        bases.push(PathBuf::from(home));
    }
    if let Some(home) = absolute(environment("HOME")) {
        bases.push(Path::new(&home).join(".config"));
    }
    if let Some(dirs) = environment("XDG_CONFIG_DIRS") {
        bases.extend(
            dirs.split(':')
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
        );
    }
    bases.push(PathBuf::from("/etc/xdg"));
    bases
        .into_iter()
        .map(|base| base.join("hypr").join("hypridle.conf"))
        .find(|path| exists(path))
}

/// Read `path` and build the configuration from it.
pub fn load(path: &Path, environment: &[(String, String)], files: &mut dyn conf::Files) -> Config {
    from_read(conf::read(path, environment, files))
}

/// The configuration the entries describe.
#[must_use]
pub fn from_read(read: Read) -> Config {
    let mut config = Config {
        errors: read.errors,
        ..Config::default()
    };
    // The listeners by block, in the order the blocks first appear.
    let mut blocks: Vec<(usize, Vec<Entry>)> = Vec::new();
    for entry in read.entries {
        if let Some(key) = entry.name.strip_prefix("general:") {
            if let Err(error) = set_general(&mut config.general, key, &entry.value) {
                config.errors.push(entry.at.say(&error));
            } else if !config.set.iter().any(|held| held == key) {
                config.set.push(key.to_owned());
            }
            continue;
        }
        if let Some(key) = entry.name.strip_prefix("listener:")
            && LISTENER.iter().any(|(name, _)| *name == key)
        {
            match blocks.iter_mut().find(|(block, _)| *block == entry.block) {
                Some((_, entries)) => entries.push(entry),
                None => blocks.push((entry.block, vec![entry])),
            }
            continue;
        }
        config.errors.push(
            entry
                .at
                .say(&format!("config option <{}> does not exist.", entry.name)),
        );
    }
    if blocks.is_empty() {
        config.errors.push("No rules configured".to_owned());
    }
    for (_, entries) in blocks {
        let mut rule = Rule::default();
        let mut timeout: i64 = -1;
        for entry in &entries {
            let key = entry.name.trim_start_matches("listener:");
            let result = match key {
                "timeout" => parse_int(&entry.value).map(|value| timeout = value),
                "on-timeout" => {
                    rule.on_timeout.clone_from(&entry.value);
                    Ok(())
                }
                "on-resume" => {
                    rule.on_resume.clone_from(&entry.value);
                    Ok(())
                }
                "ignore_inhibit" => {
                    parse_int(&entry.value).map(|value| rule.ignore_inhibit = value != 0)
                }
                "condition_cmd" => {
                    rule.condition_cmd.clone_from(&entry.value);
                    Ok(())
                }
                "condition_retry" => {
                    parse_int(&entry.value).map(|value| rule.condition_retry = value)
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                config.errors.push(entry.at.say(&error));
            }
        }
        if timeout == -1 {
            config
                .errors
                .push("Category has a missing timeout setting".to_owned());
            continue;
        }
        // Upstream keeps the timeout in a `uint64_t`, so a negative one
        // would wait for ever; saying so is kinder than waiting.
        let Ok(seconds) = u64::try_from(timeout) else {
            config.errors.push(format!(
                "listener timeout {timeout} is negative; the listener is left out"
            ));
            continue;
        };
        rule.timeout = seconds;
        config.rules.push(rule);
    }
    config
}

/// Set one `general` option.
fn set_general(general: &mut General, key: &str, value: &str) -> Result<(), String> {
    let Some((_, kind)) = GENERAL.iter().find(|(name, _)| *name == key) else {
        return Err(format!("config option <general:{key}> does not exist."));
    };
    let number = match kind {
        Kind::Int => parse_int(value)?,
        Kind::Text => 0,
    };
    let text = value.to_owned();
    match key {
        "lock_cmd" => general.lock_cmd = text,
        "unlock_cmd" => general.unlock_cmd = text,
        "on_lock_cmd" => general.on_lock_cmd = text,
        "on_unlock_cmd" => general.on_unlock_cmd = text,
        "before_sleep_cmd" => general.before_sleep_cmd = text,
        "after_sleep_cmd" => general.after_sleep_cmd = text,
        "ignore_dbus_inhibit" => general.ignore_dbus_inhibit = number != 0,
        "ignore_systemd_inhibit" => general.ignore_systemd_inhibit = number != 0,
        "ignore_wayland_inhibit" => general.ignore_wayland_inhibit = number != 0,
        "inhibit_sleep" => general.inhibit_sleep = number,
        _ => {}
    }
    Ok(())
}

/// One line for each option the file set that does something else on
/// Ferrix, saying what.
#[must_use]
pub fn ferrix_notes(config: &Config) -> Vec<String> {
    let set = |key: &str| config.set.iter().any(|held| held == key);
    let general = &config.general;
    let mut notes = Vec::new();
    if !general.before_sleep_cmd.is_empty() {
        notes.push(
            "general:before_sleep_cmd: Ferrix has no suspend and no logind to announce one \
             (PrepareForSleep), so it never runs"
                .to_owned(),
        );
    }
    if !general.after_sleep_cmd.is_empty() {
        notes.push(
            "general:after_sleep_cmd: Ferrix has no suspend to wake from, so it never runs"
                .to_owned(),
        );
    }
    if set("inhibit_sleep") {
        notes.push(
            "general:inhibit_sleep: Ferrix has no suspend and no logind to delay it, so there \
             is no sleep to inhibit"
                .to_owned(),
        );
    }
    if set("ignore_dbus_inhibit") {
        notes.push(format!(
            "general:ignore_dbus_inhibit = {}: Ferrix has no D-Bus, so no program can ask \
             org.freedesktop.ScreenSaver not to idle; a Wayland idle inhibitor still holds the \
             listeners off{}",
            general.ignore_dbus_inhibit,
            if general.ignore_wayland_inhibit {
                " (except that ignore_wayland_inhibit says to ignore those too)"
            } else {
                ""
            }
        ));
    }
    if set("ignore_systemd_inhibit") {
        notes.push(format!(
            "general:ignore_systemd_inhibit = {}: Ferrix has no systemd-logind, so no \
             `systemd-inhibit --what=idle` can hold the listeners off",
            general.ignore_systemd_inhibit
        ));
    }
    notes
}
