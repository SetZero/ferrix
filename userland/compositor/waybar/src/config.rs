//! The configuration file: where it is found, what it includes, and which
//! bars it asks for on which output.
//!
//! This is `src/config.cpp` of waybar, rule for rule:
//!
//! * The file is `-c`'s, or the first of `config` and `config.jsonc` found in
//!   `$WAYBAR_CONFIG_DIR`, then `$XDG_CONFIG_HOME/waybar/`,
//!   `$HOME/.config/waybar/`, `$HOME/waybar/`, `/etc/xdg/waybar/` and
//!   `./resources/` -- each directory tried with both names before the next.
//!   Paths go through `wordexp`, which here means `~` and `$VAR`/`${VAR}`.
//! * The root is one bar object or an array of them. `include` (a string or
//!   an array of strings) in a bar object names more files; each is parsed
//!   and merged into that object, and **what is already set wins**: an
//!   included file fills in keys, it never overrides one, while two objects
//!   under the same key are merged the same way, recursively.
//! * `output` picks outputs: a string or an array of strings, each matched
//!   by equality against the output's name (`DP-1`) or its identifier, which
//!   is the `xdg_output` description with its ` (DP-1)` suffix cut off. `!`
//!   in front negates, `*` in an array matches anything, `$NAME` is taken
//!   from the environment. With no `output`, `output-dimensions` may keep a
//!   bar off outputs too small or too large; with neither, every output gets
//!   the bar.

use std::path::{Path, PathBuf};

use crate::diag::Diagnostics;
use crate::json::{self, Value};

/// The environment and file system the configuration is read from, so the
/// rules above are testable without either.
pub trait Env {
    /// An environment variable.
    fn var(&self, name: &str) -> Option<String>;
    /// Whether a file exists (`access(F_OK)`).
    fn exists(&self, path: &Path) -> bool;
    /// A file's text.
    ///
    /// # Errors
    ///
    /// When it cannot be read.
    fn read(&self, path: &Path) -> std::io::Result<String>;
}

/// The real process environment and file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct System;

impl Env for System {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
}

/// The environment variable that names a directory searched first.
pub const CONFIG_PATH_ENV: &str = "WAYBAR_CONFIG_DIR";

/// The directories searched, in order, after [`CONFIG_PATH_ENV`].
/// Upstream also has `SYSCONFDIR "/xdg/waybar/"`, which on a distribution
/// build is `/etc/xdg/waybar/` again.
pub const CONFIG_DIRS: [&str; 5] = [
    "$XDG_CONFIG_HOME/waybar/",
    "$HOME/.config/waybar/",
    "$HOME/waybar/",
    "/etc/xdg/waybar/",
    "./resources/",
];

/// `wordexp` as far as a config path uses it: a leading `~` and `$VAR` or
/// `${VAR}` anywhere. An unset variable expands to nothing, as in the shell.
/// Field splitting and globbing are not done; a path with a space or a `*`
/// is taken as written.
#[must_use]
pub fn expand(path: &str, env: &dyn Env) -> String {
    let mut out = String::new();
    let mut rest = path;
    if let Some(after) = rest.strip_prefix('~')
        && (after.is_empty() || after.starts_with('/'))
    {
        out.push_str(&env.var("HOME").unwrap_or_default());
        rest = after;
    }
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let mut name = String::new();
        if chars.peek() == Some(&'{') {
            let _ = chars.next();
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                name.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    name.push(c);
                    let _ = chars.next();
                } else {
                    break;
                }
            }
        }
        if name.is_empty() {
            out.push('$');
        } else {
            out.push_str(&env.var(&name).unwrap_or_default());
        }
    }
    out
}

/// `tryExpandPath`: `base` joined with `name`, expanded, if the result exists.
fn try_expand(base: &str, name: &str, env: &dyn Env) -> Option<PathBuf> {
    let joined = if name.is_empty() {
        PathBuf::from(expand(base, env))
    } else {
        Path::new(&expand(base, env)).join(name)
    };
    env.exists(&joined).then_some(joined)
}

/// `findConfigPath`: the first of `names` in the search directories.
#[must_use]
pub fn find(names: &[&str], env: &dyn Env) -> Option<PathBuf> {
    if let Some(dir) = env.var(CONFIG_PATH_ENV) {
        for name in names {
            if let Some(found) = try_expand(&dir, name, env) {
                return Some(found);
            }
        }
    }
    for dir in CONFIG_DIRS {
        for name in names {
            if let Some(found) = try_expand(dir, name, env) {
                return Some(found);
            }
        }
    }
    None
}

/// `findIncludePath`: the name itself (absolute, `~`, or relative to the
/// working directory), else under [`CONFIG_PATH_ENV`], else under each
/// search directory.
fn find_include(name: &str, env: &dyn Env) -> Option<PathBuf> {
    if let Some(found) = try_expand(name, "", env) {
        return Some(found);
    }
    if let Some(dir) = env.var(CONFIG_PATH_ENV)
        && let Some(found) = try_expand(&dir, name, env)
    {
        return Some(found);
    }
    CONFIG_DIRS
        .iter()
        .find_map(|dir| try_expand(dir, name, env))
}

/// How deep includes may go before waybar calls it recursion.
const MAX_INCLUDE_DEPTH: usize = 100;

/// A configuration, read.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The file it was read from.
    pub path: PathBuf,
    /// The merged document: one bar object or an array of them.
    pub root: Value,
}

/// Read the configuration at `path` (or search for one), with its includes.
///
/// # Errors
///
/// As waybar fails: no file ("Missing required resource files"), one that
/// cannot be opened, one that is not JSON, or includes nested past 100.
pub fn load(path: Option<&Path>, env: &dyn Env, diag: &mut Diagnostics) -> Result<Config, String> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => find(&["config", "config.jsonc"], env)
            .ok_or_else(|| "Missing required resource files".to_owned())?,
    };
    diag.info(format!("Using configuration file {}", path.display()));
    let mut root = Value::Null;
    setup(&mut root, &path, 0, env, diag)?;
    Ok(Config { path, root })
}

/// `setupConfig`: parse `file` and merge it into `dst`.
fn setup(
    dst: &mut Value,
    file: &Path,
    depth: usize,
    env: &dyn Env,
    diag: &mut Diagnostics,
) -> Result<(), String> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err("Aborting due to likely recursive include in config files".to_owned());
    }
    let text = env
        .read(file)
        .map_err(|_| "Can't open config file".to_owned())?;
    let mut parsed = json::parse(&text).map_err(|error| format!("Error parsing JSON: {error}"))?;
    if let Value::Array(parts) = &mut parsed {
        for part in parts {
            resolve_includes(part, depth, env, diag)?;
        }
    } else {
        resolve_includes(&mut parsed, depth, env, diag)?;
    }
    merge(dst, parsed, diag);
    Ok(())
}

/// `resolveConfigIncludes`: every file `config["include"]` names, merged in.
fn resolve_includes(
    config: &mut Value,
    depth: usize,
    env: &dyn Env,
    diag: &mut Diagnostics,
) -> Result<(), String> {
    let names: Vec<String> = match config.get("include") {
        Value::Array(items) => items.iter().map(Value::as_string).collect(),
        Value::String(name) => vec![name.clone()],
        _ => return Ok(()),
    };
    for name in names {
        diag.info(format!("Including resource file: {name}"));
        match find_include(&name, env) {
            Some(found) => setup(config, &found, depth + 1, env, diag)?,
            None => diag.warn(format!("Unable to find resource file: {name}")),
        }
    }
    Ok(())
}

/// `mergeConfig`: `src` into `dst`, keeping what `dst` has.
fn merge(dst: &mut Value, src: Value, diag: &mut Diagnostics) {
    if dst.is_null() {
        *dst = src;
        return;
    }
    let (Value::Object(_), Value::Object(members)) = (&*dst, &src) else {
        diag.error("Cannot merge config, conflicting or invalid JSON types".to_owned());
        return;
    };
    for (key, value) in members.clone() {
        match dst.get_mut(&key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                merge(existing, value, diag);
            }
            Some(_) => {}
            None => dst.set(&key, value),
        }
    }
}

/// An output as waybar knows it when it picks bars for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// `xdg_output.name`: the connector, `DP-1`.
    pub name: String,
    /// `xdg_output.description`, as the compositor sent it.
    pub description: String,
    /// Logical width.
    pub width: i32,
    /// Logical height.
    pub height: i32,
}

impl Output {
    /// The identifier waybar matches `output` against: the description up
    /// to its first ` (`, which Hyprland's `Make Model Serial (DP-1)` and
    /// every wlroots compositor's put the connector after.
    #[must_use]
    pub fn identifier(&self) -> &str {
        self.description
            .find(" (")
            .and_then(|at| self.description.get(..at))
            .unwrap_or(&self.description)
    }
}

/// `isValidOutput`: whether `config` asks for a bar on `output`.
#[must_use]
pub fn wants(config: &Value, output: &Output, env: &dyn Env, diag: &mut Diagnostics) -> bool {
    let matches = |wanted: &str, diag: &mut Diagnostics| -> bool {
        if let Some(variable) = wanted.strip_prefix('$') {
            if let Some(value) = env.var(variable) {
                return value == output.name || value == output.identifier();
            }
            diag.warn(format!("The environment value is unknown: {wanted}"));
        }
        wanted == output.name || wanted == output.identifier()
    };
    match config.get("output") {
        Value::Array(items) => {
            for item in items {
                let Some(wanted) = item.as_str() else {
                    continue;
                };
                if let Some(negated) = wanted.strip_prefix('!') {
                    if matches(negated, diag) {
                        return false;
                    }
                    continue;
                }
                if matches(wanted, diag) || wanted.starts_with('*') {
                    return true;
                }
            }
            return false;
        }
        Value::String(wanted) if !wanted.is_empty() => {
            return match wanted.strip_prefix('!') {
                Some(negated) => !matches(negated, diag),
                None => matches(wanted, diag),
            };
        }
        _ => {}
    }
    let dimensions = match config.get("output-dimensions") {
        Value::String(one) => vec![one.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    for entry in dimensions {
        let mut words = entry.splitn(3, ' ');
        let (Some(dimension), Some(comparator), Some(value)) =
            (words.next(), words.next(), words.next())
        else {
            diag.warn(format!(
                "Ignoring malformed 'output-dimensions' entry (expected '<dimension> <comparator> <value>'): '{entry}'"
            ));
            continue;
        };
        let Ok(value) = value.trim().parse::<i32>() else {
            diag.warn(format!(
                "Ignoring 'output-dimensions' entry with non-integer value: '{entry}'"
            ));
            continue;
        };
        let actual = match dimension {
            "height" => output.height,
            "width" => output.width,
            _ => continue,
        };
        if (comparator == "<" && actual >= value) || (comparator == ">" && actual <= value) {
            return false;
        }
    }
    true
}

/// `getOutputConfigs`: the bar objects that ask for `output`, in order.
#[must_use]
pub fn bars_for<'a>(
    root: &'a Value,
    output: &Output,
    env: &dyn Env,
    diag: &mut Diagnostics,
) -> Vec<&'a Value> {
    match root {
        Value::Array(items) => items
            .iter()
            .filter(|bar| bar.is_object() && wants(bar, output, env, diag))
            .collect(),
        bar if wants(bar, output, env, diag) => vec![bar],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use super::{Env, Output, bars_for, expand, find, load, wants};
    use crate::diag::Diagnostics;
    use crate::json::{Value, parse};

    /// A made-up machine: variables and files.
    #[derive(Default)]
    struct Fake {
        vars: BTreeMap<String, String>,
        files: BTreeMap<PathBuf, String>,
    }

    impl Fake {
        fn var(mut self, name: &str, value: &str) -> Self {
            let _ = self.vars.insert(name.to_owned(), value.to_owned());
            self
        }
        fn file(mut self, path: &str, text: &str) -> Self {
            let _ = self.files.insert(PathBuf::from(path), text.to_owned());
            self
        }
    }

    impl Env for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }
        fn read(&self, path: &Path) -> std::io::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        }
    }

    #[test]
    fn paths_expand_home_and_variables() {
        let env = Fake::default().var("HOME", "/home/u").var("X", "x");
        assert_eq!(expand("~/a", &env), "/home/u/a");
        assert_eq!(expand("$HOME/b/${X}y", &env), "/home/u/b/xy");
        assert_eq!(expand("$UNSET/c", &env), "/c");
        assert_eq!(expand("a~b$", &env), "a~b$");
    }

    #[test]
    fn the_search_takes_config_before_config_jsonc_in_each_directory() {
        let env = Fake::default()
            .var("HOME", "/home/u")
            .file("/home/u/.config/waybar/config.jsonc", "{}")
            .file("/etc/xdg/waybar/config", "{}");
        assert_eq!(
            find(&["config", "config.jsonc"], &env),
            Some(PathBuf::from("/home/u/.config/waybar/config.jsonc"))
        );
        let env = env.var("WAYBAR_CONFIG_DIR", "/etc/xdg/waybar");
        assert_eq!(
            find(&["config", "config.jsonc"], &env),
            Some(PathBuf::from("/etc/xdg/waybar/config"))
        );
    }

    #[test]
    fn an_include_fills_in_but_never_overrides() {
        let env = Fake::default()
            .var("HOME", "/h")
            .file(
                "/h/.config/waybar/config",
                "{\"include\": \"extra.json\", \"height\": 40, \"cpu\": {\"interval\": 5}}",
            )
            .file(
                "/h/.config/waybar/extra.json",
                "{\"height\": 20, \"spacing\": 6, \"cpu\": {\"interval\": 1, \"format\": \"x\"}}",
            );
        let mut diag = Diagnostics::default();
        let config = load(None, &env, &mut diag).map(|config| config.root);
        let root = config.unwrap_or(Value::Null);
        assert_eq!(root.get("height"), &Value::Int(40));
        assert_eq!(root.get("spacing"), &Value::Int(6));
        assert_eq!(root.get("cpu").get("interval"), &Value::Int(5));
        assert!(root.get("cpu").get("format").is("x"));
    }

    #[test]
    fn a_recursive_include_is_refused() {
        let env = Fake::default().file("/c", "{\"include\": \"/c\"}");
        let mut diag = Diagnostics::default();
        let result = load(Some(Path::new("/c")), &env, &mut diag);
        assert_eq!(
            result.err(),
            Some("Aborting due to likely recursive include in config files".to_owned())
        );
    }

    fn output(name: &str, description: &str) -> Output {
        Output {
            name: name.to_owned(),
            description: description.to_owned(),
            width: 2560,
            height: 1440,
        }
    }

    #[test]
    fn output_is_matched_by_equality_against_name_or_identifier() {
        let env = Fake::default();
        let mut diag = Diagnostics::default();
        let lenovo = output("DP-1", "Lenovo Group Limited R27qe Gen2 UTP03KBB (DP-1)");
        assert_eq!(
            lenovo.identifier(),
            "Lenovo Group Limited R27qe Gen2 UTP03KBB"
        );
        let full = parse("{\"output\": \"Lenovo Group Limited R27qe Gen2 UTP03KBB\"}")
            .unwrap_or(Value::Null);
        let serial = parse("{\"output\": \"UTP03KBB\"}").unwrap_or(Value::Null);
        let name = parse("{\"output\": \"DP-1\"}").unwrap_or(Value::Null);
        let not = parse("{\"output\": \"!DP-1\"}").unwrap_or(Value::Null);
        assert!(wants(&full, &lenovo, &env, &mut diag));
        assert!(
            !wants(&serial, &lenovo, &env, &mut diag),
            "equality, not substring"
        );
        assert!(wants(&name, &lenovo, &env, &mut diag));
        assert!(!wants(&not, &lenovo, &env, &mut diag));
        let qemu = output("Virtual-1", "Virtual-1");
        assert!(!wants(&full, &qemu, &env, &mut diag));
    }

    #[test]
    fn an_output_array_takes_negations_and_a_star() {
        let env = Fake::default().var("MAIN", "DP-2");
        let mut diag = Diagnostics::default();
        let config = parse("{\"output\": [\"!DP-1\", \"*\"]}").unwrap_or(Value::Null);
        assert!(!wants(&config, &output("DP-1", ""), &env, &mut diag));
        assert!(wants(&config, &output("DP-3", ""), &env, &mut diag));
        let config = parse("{\"output\": [\"$MAIN\"]}").unwrap_or(Value::Null);
        assert!(wants(&config, &output("DP-2", ""), &env, &mut diag));
        assert!(!wants(&config, &output("DP-3", ""), &env, &mut diag));
    }

    #[test]
    fn output_dimensions_keep_a_bar_off_small_screens() {
        let env = Fake::default();
        let mut diag = Diagnostics::default();
        let config = parse("{\"output-dimensions\": \"width > 2000\"}").unwrap_or(Value::Null);
        assert!(wants(&config, &output("DP-1", ""), &env, &mut diag));
        let mut small = output("DP-1", "");
        small.width = 1920;
        assert!(!wants(&config, &small, &env, &mut diag));
    }

    #[test]
    fn an_array_root_gives_one_bar_per_matching_object() {
        let env = Fake::default();
        let mut diag = Diagnostics::default();
        let root =
            parse("[{\"output\": \"DP-1\", \"n\": 1}, {\"n\": 2}, 3]").unwrap_or(Value::Null);
        let bars = bars_for(&root, &output("DP-1", ""), &env, &mut diag);
        assert_eq!(bars.len(), 2);
        let bars = bars_for(&root, &output("DP-2", ""), &env, &mut diag);
        assert_eq!(bars.len(), 1);
    }
}
