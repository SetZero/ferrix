//! The file, the rules and the socket's lines, without a compositor.
//!
//! The configurations are short excerpts written here, in the shapes the
//! user's own `hypridle.conf` uses; the file itself is read only by the
//! host-side probe, never committed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::{self, Config};
use crate::idle::{Idle, Run};
use crate::log::{Level, Log};
use crate::session::Request;

const MAIN: &str = "/home/u/.config/hypr/hypridle.conf";

fn load(text: &str) -> Config {
    load_with(text, &[])
}

fn load_with(text: &str, environment: &[(&str, &str)]) -> Config {
    let environment: Vec<(String, String)> = environment
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    config::load_text(text, Path::new(MAIN), &environment)
}

/// The shape of the user's file: comments after values, a `general`
/// block, two listeners.
const LIKE_THE_USERS: &str = "\
# hypridle -- when to lock.

general {
    lock_cmd = pidof hyprlock || hyprlock       # avoid starting a second instance
    before_sleep_cmd = loginctl lock-session    # lock before the machine suspends
    after_sleep_cmd = hyprctl dispatch dpms on  # wake the screens on resume
    ignore_dbus_inhibit = false                 # respect apps asking not to idle
}

# Lock after a while.
listener {
    timeout = 600
    on-timeout = loginctl lock-session
}

listener {
    timeout = 900
    on-timeout = hyprctl dispatch dpms off
    on-resume = hyprctl dispatch dpms on
}
";

#[test]
fn the_users_shape_reads_with_no_error() {
    let config = load(LIKE_THE_USERS);
    assert_eq!(config.errors, Vec::<String>::new());
    assert_eq!(config.general.lock_cmd, "pidof hyprlock || hyprlock");
    assert_eq!(config.general.before_sleep_cmd, "loginctl lock-session");
    assert_eq!(config.general.after_sleep_cmd, "hyprctl dispatch dpms on");
    assert!(!config.general.ignore_dbus_inhibit);
    assert_eq!(config.general.inhibit_sleep, 2, "the default");
    assert_eq!(config.rules.len(), 2);
    assert_eq!(config.rules[0].timeout, 600);
    assert_eq!(config.rules[0].on_timeout, "loginctl lock-session");
    assert_eq!(config.rules[0].on_resume, "");
    assert_eq!(config.rules[1].timeout, 900);
    assert_eq!(config.rules[1].on_timeout, "hyprctl dispatch dpms off");
    assert_eq!(config.rules[1].on_resume, "hyprctl dispatch dpms on");
}

#[test]
fn what_ferrix_cannot_do_in_the_users_shape_is_said_once_an_option() {
    let notes = config::ferrix_notes(&load(LIKE_THE_USERS));
    assert_eq!(notes.len(), 3, "{notes:#?}");
    assert!(notes[0].starts_with("general:before_sleep_cmd: Ferrix has no suspend"));
    assert!(notes[1].starts_with("general:after_sleep_cmd: Ferrix has no suspend"));
    assert!(notes[2].starts_with("general:ignore_dbus_inhibit = false: Ferrix has no D-Bus"));
}

#[test]
fn defaults_are_upstreams() {
    let config = load("listener {\n timeout = 1\n}\n");
    let rule = &config.rules[0];
    assert!(!rule.ignore_inhibit);
    assert_eq!(rule.condition_cmd, "");
    assert_eq!(rule.condition_retry, 0);
    assert!(!config.general.ignore_wayland_inhibit);
    assert!(config::ferrix_notes(&config).is_empty());
}

#[test]
fn a_doubled_hash_is_a_hash_and_a_single_one_a_comment() {
    let config = load("general:lock_cmd = echo ##1 # and not this\nlistener:timeout = 5\n");
    assert_eq!(config.general.lock_cmd, "echo #1");
}

#[test]
fn the_colon_shorthand_is_the_block() {
    let config = load("general:on_lock_cmd = a\nlistener:timeout = 5\nlistener:on-timeout = b\n");
    assert_eq!(config.errors, Vec::<String>::new());
    assert_eq!(config.general.on_lock_cmd, "a");
    assert_eq!(config.rules.len(), 1, "one instance until a block closes");
    assert_eq!(config.rules[0].on_timeout, "b");
}

#[test]
fn a_listener_without_a_timeout_is_left_out_and_said() {
    let config = load("listener {\n on-timeout = a\n}\nlistener {\n timeout = 3\n}\n");
    assert_eq!(config.rules.len(), 1);
    assert_eq!(config.rules[0].timeout, 3);
    assert_eq!(
        config.errors,
        vec!["Category has a missing timeout setting"]
    );
}

#[test]
fn no_listener_at_all_is_said_and_not_fatal() {
    let config = load("general {\n lock_cmd = a\n}\n");
    assert!(config.rules.is_empty());
    assert_eq!(config.errors, vec!["No rules configured"]);
    assert_eq!(config.general.lock_cmd, "a");
}

#[test]
fn an_unknown_option_is_said_with_its_line_and_the_rest_applies() {
    let config =
        load("general {\n lock_cmd = a\n lock_command = b\n}\nlistener {\n timeout = 1\n}\n");
    assert_eq!(config.general.lock_cmd, "a");
    assert_eq!(
        config.errors,
        vec![format!(
            "Config error in file {MAIN} at line 3: config option <general:lock_command> does not exist."
        )]
    );
    assert_eq!(config.rules.len(), 1);
}

#[test]
fn bad_lines_are_hyprlangs_sentences() {
    let config = load("what\n}\ngeneral {\n x } \n= 1\n}\nlistener:timeout = soon\ngeneral {\n");
    let said: Vec<&str> = config
        .errors
        .iter()
        .map(|error| error.rsplit(": ").next().unwrap())
        .collect();
    assert_eq!(
        said,
        vec![
            "Invalid config line",
            "Stray category close",
            "Invalid config line",
            "Empty lhs.",
            "Unclosed category at EOF",
            // The values are typed after the file is read, as upstream's
            // `postParse` reads them; the instance exists, so its timeout is
            // the one missing.
            "cannot parse \"soon\" as an int.",
            "Category has a missing timeout setting",
        ]
    );
}

#[test]
fn variables_and_the_environment_are_expanded() {
    let config = load_with(
        "$lock = hyprlock --immediate\n$lockfull = no\ngeneral:lock_cmd = $lock\n\
         general:unlock_cmd = $lockfull\nlistener:timeout = 1\nlistener:on-timeout = echo $HOME\n",
        &[("HOME", "/home/u")],
    );
    assert_eq!(config.general.lock_cmd, "hyprlock --immediate");
    assert_eq!(config.general.unlock_cmd, "no", "the longer name first");
    assert_eq!(config.rules[0].on_timeout, "echo /home/u");
}

#[test]
fn a_backslash_joins_the_next_line() {
    let config = load("general:lock_cmd = one \\\n two\nlistener:timeout = 1\n");
    assert_eq!(config.general.lock_cmd, "one two");
}

/// A directory of files for `source`, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("hypridle-test-{}", std::process::id()));
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn source_reads_another_file_in_place() {
    let scratch = Scratch::new(&[
        (
            ".config/hypr/hypridle.conf",
            "source = ./idle.d/*.conf\nsource = ~/extra.conf\n",
        ),
        (
            ".config/hypr/idle.d/a.conf",
            "listener {\n timeout = 1\n}\n",
        ),
        (
            ".config/hypr/idle.d/b.conf",
            "listener {\n timeout = 2\n}\n",
        ),
        (
            "extra.conf",
            "general:lock_cmd = x\nsource = ~/extra.conf\n",
        ),
    ]);
    let home = scratch.0.to_string_lossy().into_owned();
    let config = config::load(
        &scratch.0.join(".config/hypr/hypridle.conf"),
        &[("HOME".to_owned(), home)],
    );
    assert_eq!(config.errors, Vec::<String>::new());
    assert_eq!(
        config
            .rules
            .iter()
            .map(|rule| rule.timeout)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(config.general.lock_cmd, "x");
}

#[test]
fn a_missing_source_is_said() {
    let config = load_with(
        "source = ~/nothing.conf\nlistener:timeout = 1\n",
        &[("HOME", "/nonexistent-hypridle-home")],
    );
    assert_eq!(config.errors.len(), 1, "{:#?}", config.errors);
    assert!(
        config.errors[0].starts_with(&format!("Config error in file {MAIN} at line 1: source")),
        "{:#?}",
        config.errors
    );
    assert_eq!(config.rules.len(), 1);
}

#[test]
fn expressions_and_directives_are_hyprlangs() {
    let config = load(
        "$minutes = 5\nlistener {\n timeout = {{ $minutes * 60 }}\n}\n\
         # hyprlang noerror true\nnonsense = 1\n# hyprlang noerror false\n",
    );
    assert_eq!(config.errors, Vec::<String>::new());
    assert_eq!(config.rules[0].timeout, 300);
}

#[test]
fn ints_are_read_as_hyprlang_reads_them() {
    for (text, want) in [
        ("1", 1),
        ("-3", -3),
        ("0x10", 16),
        ("true", 1),
        ("yes", 1),
        ("on", 1),
        ("false", 0),
        ("no", 0),
        ("off", 0),
    ] {
        assert_eq!(config::parse_int(text), Ok(want), "{text}");
    }
    assert!(config::parse_int("1.5").is_err());
    assert!(config::parse_int("").is_err());
}

#[test]
fn the_file_is_looked_for_where_upstream_looks() {
    let environment = |name: &str| match name {
        "XDG_CONFIG_HOME" => Some("/x".to_owned()),
        "HOME" => Some("/home/u".to_owned()),
        "XDG_CONFIG_DIRS" => Some("/d1:/d2".to_owned()),
        _ => None,
    };
    let only = |wanted: &'static str| move |path: &Path| path == Path::new(wanted);
    for wanted in [
        "/x/hypr/hypridle.conf",
        "/home/u/.config/hypr/hypridle.conf",
        "/d2/hypr/hypridle.conf",
        "/etc/xdg/hypr/hypridle.conf",
    ] {
        assert_eq!(
            config::find(&environment, &only(wanted)),
            Some(PathBuf::from(wanted))
        );
    }
    assert_eq!(config::find(&environment, &|_| false), None);
}

/// What was run and said.
#[derive(Default)]
struct Recorded {
    spawned: Vec<String>,
    asked: Vec<String>,
    conditions: Vec<bool>,
    lines: Vec<String>,
}

impl Run for Recorded {
    fn spawn(&mut self, command: &str) {
        self.spawned.push(command.to_owned());
    }

    fn condition(&mut self, command: &str) -> bool {
        self.asked.push(command.to_owned());
        if self.conditions.is_empty() {
            true
        } else {
            self.conditions.remove(0)
        }
    }

    fn log(&mut self, _level: Level, line: &str) {
        self.lines.push(line.to_owned());
    }
}

fn idle(text: &str) -> Idle {
    let config = load(text);
    Idle::new(config.general, config.rules)
}

#[test]
fn a_timeout_runs_on_timeout_and_the_next_input_on_resume() {
    let mut state = idle(LIKE_THE_USERS);
    let mut run = Recorded::default();
    state.idled(1, Instant::now(), &mut run);
    state.resumed(1, &mut run);
    assert_eq!(
        run.spawned,
        vec!["hyprctl dispatch dpms off", "hyprctl dispatch dpms on"]
    );
}

#[test]
fn a_resume_with_no_timeout_before_it_runs_nothing() {
    let mut state = idle(LIKE_THE_USERS);
    let mut run = Recorded::default();
    state.resumed(1, &mut run);
    assert!(run.spawned.is_empty());
    assert!(
        run.lines
            .iter()
            .any(|line| line.starts_with("Skipping onResumed"))
    );
}

#[test]
fn an_empty_on_resume_is_passed_over() {
    let mut state = idle(LIKE_THE_USERS);
    let mut run = Recorded::default();
    state.idled(0, Instant::now(), &mut run);
    state.resumed(0, &mut run);
    assert_eq!(run.spawned, vec!["loginctl lock-session"]);
    assert!(
        run.lines
            .iter()
            .any(|line| line == "Ignoring, onRestore is empty.")
    );
}

#[test]
fn a_condition_that_says_no_is_asked_again_until_it_says_yes() {
    let mut state = idle(
        "listener {\n timeout = 1\n on-timeout = off\n on-resume = on\n \
         condition_cmd = check\n condition_retry = 5\n}\n",
    );
    let mut run = Recorded {
        conditions: vec![false, false, true],
        ..Recorded::default()
    };
    let start = Instant::now();
    state.idled(0, start, &mut run);
    assert!(run.spawned.is_empty());
    assert_eq!(state.next_retry(), Some(start + Duration::from_secs(5)));
    state.retry(start + Duration::from_secs(1), &mut run);
    assert_eq!(run.asked.len(), 1, "not yet due");
    state.retry(start + Duration::from_secs(5), &mut run);
    assert!(run.spawned.is_empty());
    state.retry(start + Duration::from_secs(10), &mut run);
    assert_eq!(run.spawned, vec!["off"]);
    assert_eq!(state.next_retry(), None);
    state.resumed(0, &mut run);
    assert_eq!(run.spawned, vec!["off", "on"]);
}

#[test]
fn input_ends_every_listeners_wait_for_its_condition() {
    let mut state = idle(
        "listener {\n timeout = 1\n on-timeout = a\n condition_cmd = c\n condition_retry = 5\n}\n\
         listener {\n timeout = 2\n on-timeout = b\n}\n",
    );
    let mut run = Recorded {
        conditions: vec![false],
        ..Recorded::default()
    };
    state.idled(0, Instant::now(), &mut run);
    assert!(state.next_retry().is_some());
    state.resumed(1, &mut run);
    assert_eq!(state.next_retry(), None);
}

#[test]
fn a_condition_with_no_retry_is_asked_once() {
    let mut state = idle("listener {\n timeout = 1\n on-timeout = a\n condition_cmd = c\n}\n");
    let mut run = Recorded {
        conditions: vec![false],
        ..Recorded::default()
    };
    state.idled(0, Instant::now(), &mut run);
    assert_eq!(state.next_retry(), None);
    assert!(run.spawned.is_empty());
}

#[test]
fn lock_session_runs_lock_cmd_and_the_compositors_lock_runs_on_lock_cmd() {
    let mut state = idle(
        "general {\n lock_cmd = L\n unlock_cmd = U\n on_lock_cmd = OL\n on_unlock_cmd = OU\n}\n\
         listener:timeout = 1\n",
    );
    let mut run = Recorded::default();
    state.lock_session(&mut run);
    state.session_locked(&mut run);
    assert!(state.locked);
    state.unlock_session(&mut run);
    state.session_unlocked(&mut run);
    assert!(!state.locked);
    assert_eq!(run.spawned, vec!["L", "OL", "U", "OU"]);
}

#[test]
fn inhibitors_are_ignored_by_either_option() {
    let state =
        idle("listener {\n timeout = 1\n}\nlistener {\n timeout = 2\n ignore_inhibit = true\n}\n");
    assert!(!state.ignores_inhibitors(0));
    assert!(state.ignores_inhibitors(1));
    let state = idle("general:ignore_wayland_inhibit = 1\nlistener {\n timeout = 1\n}\n");
    assert!(state.ignores_inhibitors(0));
}

#[test]
fn loginctls_verbs_are_the_socket_lines() {
    for verb in ["lock-session", "lock-sessions"] {
        assert_eq!(Request::from_verb(verb), Some(Request::Lock));
    }
    for verb in ["unlock-session", "unlock-sessions"] {
        assert_eq!(Request::from_verb(verb), Some(Request::Unlock));
    }
    assert_eq!(Request::from_verb("terminate-session"), None);
    assert_eq!(Request::parse(Request::Lock.line()), Ok(Request::Lock));
    assert_eq!(Request::parse("unlock-session 3\n"), Ok(Request::Unlock));
    assert!(Request::parse("suspend\n").is_err());
}

#[test]
fn log_lines_are_upstreams() {
    let log = Log::default();
    assert_eq!(log.format(Level::Log, "x").as_deref(), Some("[LOG] x"));
    assert_eq!(
        log.format(Level::Crit, "x").as_deref(),
        Some("[CRITICAL] x")
    );
    assert_eq!(log.format(Level::None, "x").as_deref(), Some("x"));
    assert_eq!(log.format(Level::Trace, "x"), None);
    let quiet = Log {
        quiet: true,
        verbose: false,
    };
    assert_eq!(quiet.format(Level::Err, "x"), None);
}
