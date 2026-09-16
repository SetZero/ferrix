//! The line grammar, variables, categories, keywords and `source`.

use crate::{Config, Diagnostic, Parsed, Raw, SourceFile, Sources, bind, options};

/// How deep `source` may nest, so a file sourcing itself ends.
pub(crate) const MAX_SOURCE_DEPTH: usize = 16;

/// How many files `source` may read in all. The depth limit alone does not
/// bound the work: a file with two `source` lines naming itself doubles at
/// every level, sixty-five thousand reads before the depth stops it.
pub(crate) const MAX_SOURCED_FILES: usize = 64;

/// The longest a value may grow by expanding variables. `$a = $a$a` doubles
/// the variable each time the line is read, and a file sourcing itself reads
/// it again and again.
pub(crate) const MAX_VALUE_LEN: usize = 64 * 1024;

/// Parse `text`, the file called `name`, over the defaults.
pub fn parse(name: &str, text: &str, sources: &mut dyn Sources) -> Parsed {
    let mut parser = Parser {
        config: Config::default(),
        diagnostics: Vec::new(),
        sources,
        submap: None,
        sourced: 0,
    };
    parser.file(name, text, 0);
    Parsed {
        config: parser.config,
        diagnostics: parser.diagnostics,
    }
}

struct Parser<'s> {
    config: Config,
    diagnostics: Vec<Diagnostic>,
    sources: &'s mut dyn Sources,
    /// The submap `submap = name` opened, which bindings after it join.
    submap: Option<String>,
    /// How many files `source` has read so far.
    sourced: usize,
}

/// `line` with its comment removed and `##` turned into `#`.
fn strip_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '#' {
            out.push(c);
        } else if chars.next_if_eq(&'#').is_some() {
            out.push('#');
        } else {
            break;
        }
    }
    out
}

impl Parser<'_> {
    fn error(&mut self, file: &str, line: usize, message: String) {
        self.diagnostics.push(Diagnostic {
            file: file.to_owned(),
            line,
            message,
        });
    }

    fn file(&mut self, name: &str, text: &str, depth: usize) {
        let mut categories: Vec<String> = Vec::new();
        let mut last = 0;
        for (index, raw) in text.lines().enumerate() {
            let number = index + 1;
            last = number;
            if raw.trim_start().starts_with("# hyprlang") {
                self.error(
                    name,
                    number,
                    "hyprlang directives are not supported yet".to_owned(),
                );
                continue;
            }
            let stripped = strip_comment(raw);
            let line = stripped.trim();
            if line.is_empty() {
                continue;
            }
            if let Err(message) = self.line(name, line, &mut categories, depth) {
                self.error(name, number, message);
            }
        }
        if let Some(open) = categories.last() {
            let message = format!("category {open} is not closed");
            self.error(name, last, message);
        }
    }

    fn line(
        &mut self,
        name: &str,
        line: &str,
        categories: &mut Vec<String>,
        depth: usize,
    ) -> Result<(), String> {
        if line.contains("{{") {
            return Err("hyprlang expressions are not supported yet".to_owned());
        }
        let Some((left, right)) = line.split_once('=') else {
            if line == "}" {
                return categories
                    .pop()
                    .map(drop)
                    .ok_or_else(|| "unexpected } with no category open".to_owned());
            }
            if let Some(category) = line.strip_suffix('{') {
                let category = category.trim();
                if category.is_empty() {
                    return Err("a category needs a name".to_owned());
                }
                if category.contains('[') {
                    return Err(format!("keyed category {category} is not supported yet"));
                }
                categories.push(category.to_owned());
                return Ok(());
            }
            return Err(format!("invalid line: {line}"));
        };

        let left = left.trim();
        let value = expand(&self.config, right.trim())?;

        if let Some(variable) = left.strip_prefix('$') {
            let variable = variable.trim();
            if variable.is_empty() || variable.contains(char::is_whitespace) {
                return Err(format!("invalid variable name {left}"));
            }
            let _ = self.config.variables.insert(variable.to_owned(), value);
            return Ok(());
        }

        let mut key = categories.join(":");
        if !key.is_empty() {
            key.push(':');
        }
        key.push_str(left);

        match key.as_str() {
            "source" => self.source(name, &value, depth),
            "submap" => {
                self.submap = (value != "reset").then_some(value);
                Ok(())
            }
            _ => apply(&mut self.config, &key, &value, self.submap.as_deref()),
        }
    }

    fn source(&mut self, from: &str, spec: &str, depth: usize) -> Result<(), String> {
        if depth + 1 >= MAX_SOURCE_DEPTH {
            return Err(format!(
                "source file {spec}: nested deeper than {MAX_SOURCE_DEPTH} files"
            ));
        }
        if self.sourced >= MAX_SOURCED_FILES {
            return Err(format!(
                "source file {spec}: more than {MAX_SOURCED_FILES} files sourced"
            ));
        }
        let files = self.sources.resolve(spec, from)?;
        for SourceFile { name, text } in files {
            self.sourced += 1;
            self.file(&name, &text, depth + 1);
        }
        Ok(())
    }
}

/// `value` with each `$name` replaced by the longest defined variable it
/// starts with; a `$` starting no variable's name stays as written.
pub(crate) fn expand(config: &Config, value: &str) -> Result<String, String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some((before, after)) = rest.split_once('$') {
        out.push_str(before);
        let longest = config
            .variables
            .iter()
            .filter(|(name, _)| after.starts_with(name.as_str()))
            .max_by_key(|(name, _)| name.len());
        match longest.and_then(|(name, text)| Some((after.get(name.len()..)?, text))) {
            Some((remaining, text)) => {
                if out.len() + text.len() > MAX_VALUE_LEN {
                    return Err(format!(
                        "a value longer than {MAX_VALUE_LEN} bytes once variables are expanded"
                    ));
                }
                out.push_str(text);
                rest = remaining;
            }
            None => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Apply `key = value`, variables already expanded, to `config`: a keyword
/// or an option. `source` and `submap` are the parser's and never reach here
/// from a file.
pub(crate) fn apply(
    config: &mut Config,
    key: &str,
    value: &str,
    submap: Option<&str>,
) -> Result<(), String> {
    let raw = |keyword: &str| Raw {
        keyword: keyword.to_owned(),
        value: value.to_owned(),
    };
    match key {
        "windowrule" | "windowrulev2" => config.window_rules.push(raw(key)),
        "layerrule" => config.layer_rules.push(raw(key)),
        "monitor" => config.monitors.push(raw(key)),
        "workspace" => config.workspaces.push(raw(key)),
        "animation" | "bezier" => config.animations.push(raw(key)),
        "exec-once" => config.exec_once.push(value.to_owned()),
        "exec" => config.exec.push(value.to_owned()),
        "exec-shutdown" => config.exec_shutdown.push(value.to_owned()),
        "env" => {
            let fields = bind::split_fields(value, 2);
            let (Some(&variable), Some(&text)) = (fields.first(), fields.get(1)) else {
                return Err(format!("env expects NAME, value, not \"{value}\""));
            };
            config.env.push((variable.to_owned(), text.to_owned()));
        }
        "unbind" => {
            let (mods, key) = bind::parse_unbind(value)?;
            config
                .binds
                .retain(|bind| bind.mods != mods || bind.key != key);
        }
        "source" | "submap" => {
            return Err(format!("{key} is only valid in a configuration file"));
        }
        _ => {
            if let Some(letters) = key.strip_prefix("bind") {
                if let Some(bind) = bind::parse(letters, value, submap)? {
                    config.binds.push(bind);
                }
                return Ok(());
            }
            return set_option(config, key, value);
        }
    }
    Ok(())
}

fn set_option(config: &mut Config, key: &str, value: &str) -> Result<(), String> {
    let Some(default) = options::find(key) else {
        return Err(format!("config option <{key}> does not exist."));
    };
    let parsed = default
        .parse(value)
        .map_err(|error| format!("error setting value <{value}> for field <{key}>: {error}"))?;
    if let Some(slot) = config.options.get_mut(key) {
        *slot = parsed;
    }
    Ok(())
}
