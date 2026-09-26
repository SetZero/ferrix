//! The part of hyprlang `hypridle.conf` is written in, read into a list of
//! entries.
//!
//! This is hypridle's own reader only until the `clients-base` stream's
//! generic hyprlang crate lands; [`read`] and [`Entry`] are the whole of what
//! the rest of the crate sees, so the switch is this one file.
//!
//! What it reads is what hyprlang's `CConfig::parseLine` does with a line
//! (`/var/cache/hyprland-build/src/hyprlang/src/config.cpp`): a trailing
//! backslash joins the next line, `#` starts a comment and `##` is a literal
//! `#`, `$name = value` defines a variable that every later `$name` is
//! replaced with (and every environment variable is one from the start),
//! `name {` opens a category and `}` closes it, `a:b = value` is the same as
//! `b = value` inside `a { }`, and `source = <path>` reads another file in
//! place. Errors are hyprlang's own sentences, one a line, and never stop
//! the file.
//!
//! Hyprlang decides which `listener { }` block a value belongs to by whether
//! a category has closed back to the top level since the last one; each
//! entry carries that count as [`Entry::block`], which is how the model
//! tells two blocks apart the way hyprlang's anonymous special categories
//! do.
//!
//! Not carried out, each said by name rather than skipped: `# hyprlang`
//! directives (`if`, `noerror`) and `{{ }}` expressions.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How deep `source` may nest, which is what stops a file that sources
/// itself through a glob; hyprlang stops such a chain by remembering files,
/// and this does both.
const MAX_DEPTH: usize = 16;

/// One `key = value` line, with where it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The categories it is in and its key, joined by `:` as hyprlang names
    /// an option: `general:lock_cmd`, `listener:timeout`.
    pub name: String,
    /// The value, variables replaced, trimmed.
    pub value: String,
    /// Which top-level stretch it is in: the number of times a category had
    /// closed back to the top level before it. Two `listener { }` blocks
    /// differ in it; `listener:timeout` lines at the top level with no block
    /// closed between them share it, as they share an instance in hyprlang.
    pub block: usize,
    /// Where it was, for a diagnostic.
    pub at: Place,
}

/// A file and a line in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The file.
    pub file: PathBuf,
    /// The line, counting from one; a line continued with a backslash is
    /// the number of its first.
    pub line: usize,
}

impl Place {
    /// Hyprlang's prefix for an error on this line.
    #[must_use]
    pub fn say(&self, error: &str) -> String {
        format!(
            "Config error in file {} at line {}: {error}",
            self.file.display(),
            self.line
        )
    }
}

/// What reading gave: the entries in file order, and one line for each
/// thing that was wrong or not carried out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    /// Every `key = value`, sourced files' in place.
    pub entries: Vec<Entry>,
    /// The errors, in hyprlang's wording.
    pub errors: Vec<String>,
}

/// Where the files `source` names come from, so the reader is a pure
/// function of text in the tests.
pub trait Files {
    /// The text of `path`, or why it cannot be read.
    ///
    /// # Errors
    ///
    /// The file cannot be read.
    fn read(&mut self, path: &Path) -> Result<String, String>;

    /// The paths a `source` value names, from a file in `directory`: the
    /// value with `~` replaced and made absolute, and a glob expanded.
    ///
    /// # Errors
    ///
    /// Hyprlang's sentence for a glob that matched nothing.
    fn resolve(&mut self, value: &str, directory: &Path) -> Result<Vec<PathBuf>, String>;
}

/// The real file system.
#[derive(Clone, Debug)]
pub struct Disk {
    /// What `~` stands for.
    pub home: Option<PathBuf>,
}

impl Files for Disk {
    fn read(&mut self, path: &Path) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
    }

    fn resolve(&mut self, value: &str, directory: &Path) -> Result<Vec<PathBuf>, String> {
        let path = absolute(value, directory, self.home.as_deref());
        let Some(pattern) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(vec![path]);
        };
        if !pattern.contains(['*', '?', '[']) {
            return Ok(vec![path]);
        }
        // A glob in the last part of the path, which is every `source =
        // ~/.config/hypr/conf.d/*.conf` seen; one further up is said.
        let parent = path.parent().unwrap_or(directory);
        if parent.to_string_lossy().contains(['*', '?', '[']) {
            return Err(format!(
                "source= a glob before the last part of the path is not carried out yet: {value}"
            ));
        }
        let mut found: Vec<PathBuf> = std::fs::read_dir(parent)
            .map_err(|_| "source= globbing error: read error".to_owned())?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| glob_matches(pattern, name))
            })
            .map(|entry| entry.path())
            .collect();
        found.sort();
        if found.is_empty() {
            return Err("source= globbing error: found no match".to_owned());
        }
        Ok(found)
    }
}

/// `path` as hypridle's `absolutePath` makes it: `~` is the home directory,
/// and a relative path is from the file that named it.
#[must_use]
pub fn absolute(path: &str, directory: &Path, home: Option<&Path>) -> PathBuf {
    let expanded = match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    };
    if expanded.is_relative() {
        directory.join(expanded)
    } else {
        expanded
    }
}

/// Whether `name` matches the shell pattern `pattern`: `*`, `?` and
/// `[...]` sets, as glob(3) does within one path component. A name starting
/// with `.` is matched only by a pattern that does.
#[must_use]
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_from(&pattern, &name)
}

fn matches_from(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|skip| {
            name.get(skip..)
                .is_some_and(|tail| matches_from(rest, tail))
        }),
        Some(('?', rest)) => name
            .split_first()
            .is_some_and(|(_, tail)| matches_from(rest, tail)),
        Some(('[', rest)) => {
            let Some(close) = rest.iter().skip(1).position(|c| *c == ']').map(|at| at + 1) else {
                // An unclosed `[` is a literal one.
                return name
                    .split_first()
                    .is_some_and(|(c, tail)| *c == '[' && matches_from(rest, tail));
            };
            let (set, after) = rest.split_at(close);
            let after = after.get(1..).unwrap_or(&[]);
            let (negated, set) = match set.split_first() {
                Some(('!' | '^', set)) => (true, set),
                _ => (false, set),
            };
            let Some((c, tail)) = name.split_first() else {
                return false;
            };
            let mut inside = false;
            let mut index = 0;
            while let Some(first) = set.get(index) {
                if set.get(index + 1) == Some(&'-')
                    && let Some(last) = set.get(index + 2)
                {
                    inside |= first <= c && c <= last;
                    index += 3;
                } else {
                    inside |= first == c;
                    index += 1;
                }
            }
            inside != negated && matches_from(after, tail)
        }
        Some((literal, rest)) => name
            .split_first()
            .is_some_and(|(c, tail)| c == literal && matches_from(rest, tail)),
    }
}

/// Read `path` and every file it sources.
///
/// `environment` is the variables hyprlang starts with: every environment
/// variable, so `$HOME` means what it does in a shell.
pub fn read(path: &Path, environment: &[(String, String)], files: &mut dyn Files) -> Read {
    let mut reader = Reader {
        files,
        variables: environment.to_vec(),
        categories: Vec::new(),
        block: 0,
        seen: BTreeSet::new(),
        out: Read::default(),
    };
    sort_variables(&mut reader.variables);
    let _ = reader.seen.insert(path.to_path_buf());
    match reader.files.read(path) {
        Ok(text) => reader.file(path, &text, 0),
        Err(error) => reader.out.errors.push(error),
    }
    reader.out
}

/// Longest name first, so `$HOMEDIR` is not read as `$HOME` and `DIR`;
/// hyprlang sorts its variables the same way.
fn sort_variables(variables: &mut [(String, String)]) {
    variables.sort_by_key(|variable| std::cmp::Reverse(variable.0.len()));
}

struct Reader<'a> {
    files: &'a mut dyn Files,
    variables: Vec<(String, String)>,
    categories: Vec<String>,
    block: usize,
    seen: BTreeSet<PathBuf>,
    out: Read,
}

impl Reader<'_> {
    fn file(&mut self, path: &Path, text: &str, depth: usize) {
        let mut lines = text.split('\n').enumerate().peekable();
        while let Some((index, first)) = lines.next() {
            let at = Place {
                file: path.to_path_buf(),
                line: index + 1,
            };
            let mut line = first.trim_end_matches('\r').to_owned();
            let mut dangling = false;
            while line.ends_with('\\') {
                let kept = line.trim_end_matches('\\').trim_end().len();
                line.truncate(kept);
                match lines.next() {
                    Some((_, next)) => line.push_str(next.trim_end_matches('\r')),
                    None => {
                        dangling = true;
                        break;
                    }
                }
            }
            if dangling {
                self.out.errors.push(format!(
                    "Config error in file {}: Last line ends with backslash",
                    path.display()
                ));
                break;
            }
            if let Err(error) = self.line(&line, &at, depth) {
                self.out.errors.push(at.say(&error));
            }
        }
        if !self.categories.is_empty() {
            self.out.errors.push(format!(
                "Config error in file {}: Unclosed category at EOF",
                path.display()
            ));
            self.categories.clear();
            self.block += 1;
        }
    }

    fn line(&mut self, raw: &str, at: &Place, depth: usize) -> Result<(), String> {
        let trimmed = raw.trim();
        if let Some(comment) = trimmed.strip_prefix('#') {
            if comment.trim_start().starts_with("hyprlang") {
                return Err(format!(
                    "`#{comment}`: hyprlang's `# hyprlang` directives are not carried out yet"
                ));
            }
            return Ok(());
        }
        let line = strip_comment(trimmed);
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        let Some(equals) = line.find('=') else {
            return self.brace(line);
        };
        let (left, right) = line.split_at(equals);
        let left = left.trim();
        let right = right.get(1..).unwrap_or("").trim();
        if left.is_empty() {
            return Err("Empty lhs.".to_owned());
        }
        let variable = left.starts_with('$');
        let mut left = left.to_owned();
        let mut right = right.to_owned();
        for round in 0..100 {
            let mut changed = false;
            for (name, value) in &self.variables {
                let token = format!("${name}");
                if !variable && left.contains(&token) {
                    left = left.replace(&token, value);
                    changed = true;
                }
                if right.contains(&token) {
                    right = right.replace(&token, value);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
            if round == 99 {
                return Err("Expanding variables exceeded max iteration limit".to_owned());
            }
        }
        if right.contains("{{") {
            return Err(format!(
                "`{left} = {right}`: hyprlang's {{{{ }}}} expressions are not carried out yet"
            ));
        }
        if let Some(name) = left.strip_prefix('$') {
            let name = name.to_owned();
            match self.variables.iter_mut().find(|(held, _)| *held == name) {
                Some(held) => held.1 = right,
                None => {
                    self.variables.push((name, right));
                    sort_variables(&mut self.variables);
                }
            }
            return Ok(());
        }
        let right = unescape_braces(&right);
        if self.categories.is_empty() && left == "source" {
            return self.source(&right, at, depth);
        }
        let mut name = self.categories.join(":");
        if !name.is_empty() {
            name.push(':');
        }
        name.push_str(&left);
        self.out.entries.push(Entry {
            name,
            value: right,
            block: self.block,
            at: at.clone(),
        });
        Ok(())
    }

    /// A line with no `=`: a category opening or closing.
    fn brace(&mut self, line: &str) -> Result<(), String> {
        if line.contains('}') {
            if line != "}" {
                return Err("Invalid config line".to_owned());
            }
            if self.categories.pop().is_none() {
                return Err("Stray category close".to_owned());
            }
            if self.categories.is_empty() {
                self.block += 1;
            }
            return Ok(());
        }
        let Some(name) = line.strip_suffix('{') else {
            return Err("Invalid config line".to_owned());
        };
        self.categories.push(name.trim().to_owned());
        Ok(())
    }

    /// `source = <path>`: read the files it names in place, as
    /// hypridle's own `handleSource` does.
    fn source(&mut self, value: &str, at: &Place, depth: usize) -> Result<(), String> {
        if value.len() < 2 {
            return Err(format!("source path {value} bogus!"));
        }
        if depth >= MAX_DEPTH {
            return Err(format!("source= {value}: sourced files nest too deep"));
        }
        let directory = at.file.parent().unwrap_or(Path::new("/")).to_path_buf();
        let paths = self.files.resolve(value, &directory)?;
        for path in paths {
            if path == at.file {
                continue;
            }
            // Hyprlang's "skipping already included source file ... to
            // prevent circular dependency" is a warning, not an error.
            if !self.seen.insert(path.clone()) {
                continue;
            }
            let text = self
                .files
                .read(&path)
                .map_err(|_| format!("source file {} doesn't exist!", path.display()))?;
            self.file(&path, &text, depth + 1);
        }
        Ok(())
    }
}

/// A line without its comment: everything from the first `#` that is not
/// doubled, with each `##` before it made one `#`.
fn strip_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '#' {
            if characters.peek() == Some(&'#') {
                let _ = characters.next();
                out.push('#');
                continue;
            }
            break;
        }
        out.push(character);
    }
    out
}

/// Hyprlang's escapes in a value: `\{` and `\}` are the braces, `\\` is one
/// backslash, and any other backslash stays.
fn unescape_braces(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\'
            && let Some(next @ ('\\' | '{' | '}')) = characters.peek().copied()
        {
            let _ = characters.next();
            out.push(next);
            continue;
        }
        out.push(character);
    }
    out
}
