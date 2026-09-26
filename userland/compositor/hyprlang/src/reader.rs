//! The reader: `getNextLine`, `CConfig::parseLine`, `configSetValueSafe`,
//! `parseVariable`, `parseComment`, `parseExpression` and the `source`
//! handler of hyprlock and hypridle, over one [`Document`].
//!
//! Each function names the hyprlang code it follows. Where that code has a
//! quirk -- a nested `# hyprlang if` that looks only at the innermost
//! condition, `\\` in a value made `\`, a special category's `ignoreMissing`
//! honoured only in the block that made the instance -- the quirk is kept,
//! because a file that works under hyprlock has to read the same here.

use std::fs;
use std::path::{Path, PathBuf};

use crate::number::{format_float, stof32, trim, words};
use crate::parse::{Diagnostic, Document, Instance, Keyword, Setting};
use crate::schema::{Schema, Special, SpecialKey};
use crate::source;

/// How often a line's variables are expanded again before hyprlang gives
/// up (`Expanding variables exceeded max iteration limit`).
const MAX_ITERATIONS: usize = 100;

/// The longest a line may grow by expanding variables. hyprlang has no such
/// bound and grows `$b` whose value is `$b$b` until memory runs out; this
/// stops it with a diagnostic instead.
pub(crate) const MAX_LINE_LEN: usize = 64 * 1024;

/// How deep `source` may nest. A file is never read twice (hypridle's
/// `alreadyIncludedSourceFiles`), which already ends every loop; this bounds
/// a chain of distinct files as well.
pub(crate) const MAX_SOURCE_DEPTH: usize = 32;

/// What the reader keeps about an instance that the [`Instance`] does not.
struct Meta {
    /// Its category, an index into [`Schema::specials`].
    special: usize,
    /// hyprlang's `values[key]`: the key's value for a keyed category, the
    /// anonymous number for an anonymous one.
    key_value: String,
    /// `anonymousID`: 0 unless an anonymous block made it.
    anonymous_id: usize,
}

/// Read `text`, the file `file`, against `schema`.
pub(crate) fn read(schema: &Schema, text: &str, file: &Path) -> Document {
    let mut specials: Vec<usize> = (0..schema.specials.len()).collect();
    // `addSpecialCategory` sorts the descriptors longest name first.
    specials.sort_by_key(|index| {
        core::cmp::Reverse(
            schema
                .specials
                .get(*index)
                .map_or(0, |special| special.name.len()),
        )
    });
    let mut variables = schema.environment.clone();
    // The constructor sorts the environment longest name first.
    variables.sort_by_key(|(name, _)| core::cmp::Reverse(name.len()));
    let mut reader = Reader {
        schema,
        specials,
        document: Document::default(),
        variables,
        defined: Vec::new(),
        categories: Vec::new(),
        current_special: None,
        current_key: String::new(),
        meta: Vec::new(),
        conditions: Vec::new(),
        no_error: false,
        included: Vec::new(),
    };
    if !file.as_os_str().is_empty() {
        reader.included.push(source::weakly_canonical(file));
    }
    reader.file(text, file, 0);
    reader.finish()
}

/// `CConfigImpl`'s state while a file is read.
struct Reader<'s> {
    schema: &'s Schema,
    /// Indices into `schema.specials`, longest name first.
    specials: Vec<usize>,
    document: Document,
    /// Every variable, the environment's too, longest name first.
    variables: Vec<(String, String)>,
    /// The names the file defined, in the order it first did.
    defined: Vec<String>,
    /// The categories open, outermost first.
    categories: Vec<String>,
    /// `currentSpecialCategory`, an index into the document's instances.
    current_special: Option<usize>,
    /// `currentSpecialKey`.
    current_key: String,
    /// One per instance.
    meta: Vec<Meta>,
    /// `currentFlags.ifDatas`: whether each open `# hyprlang if` failed.
    conditions: Vec<bool>,
    /// `currentFlags.noError`.
    no_error: bool,
    /// Every file read so far, as `weakly_canonical` has it.
    included: Vec<PathBuf>,
}

/// `line` with its comment cut and each `##` made `#`, byte for byte as
/// `parseLine` does it: after an escape the search resumes one past the
/// `#` it kept, so `###` is `##` and `a##b#c` is `a#b`.
fn strip_comment(line: &str) -> String {
    let mut bytes = line.as_bytes().to_vec();
    let mut at = bytes.iter().position(|byte| *byte == b'#');
    while let Some(hash) = at {
        if bytes.get(hash + 1) == Some(&b'#') {
            let _ = bytes.remove(hash + 1);
            at = find_byte(&bytes, b'#', hash + 2);
        } else {
            bytes.truncate(hash);
            break;
        }
    }
    into_string(bytes)
}

/// The first `byte` at or after `from`.
fn find_byte(bytes: &[u8], byte: u8, from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|found| *found == byte)
        .map(|at| at + from)
}

/// The first `pattern` in `text` at or after the byte `from`.
fn find_from(text: &str, pattern: &str, from: usize) -> Option<usize> {
    text.get(from..)?.find(pattern).map(|at| at + from)
}

/// Bytes that came from a `String` with ASCII bytes taken out: still
/// UTF-8, but read lossily rather than trusted.
fn into_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// The value with hyprlang's escapes taken out: `\{` is `{`, `\}` is `}`
/// and `\\` is `\`; any other backslash stays.
fn remove_escapes(value: String) -> String {
    let mut bytes = value.into_bytes();
    let mut at = 0;
    while at + 1 < bytes.len() {
        if bytes.get(at) != Some(&b'\\') {
            at += 1;
            continue;
        }
        match bytes.get(at + 1) {
            Some(b'\\') => {
                let _ = bytes.remove(at);
                at += 1;
            }
            Some(b'{' | b'}') => {
                let _ = bytes.remove(at);
            }
            _ => at += 1,
        }
    }
    into_string(bytes)
}

/// Whether a keyword registered as `name` handles `lhs` inside
/// `categories`: the handler loop of `parseLine`, `allowFlags` off. An
/// unscoped name matches in any category; a scoped one walks the
/// categories and must use them all up.
fn handles(name: &str, lhs: &str, categories: &[String]) -> bool {
    let unscoped = !name.contains(':');
    let handler = name.strip_prefix(':').unwrap_or(name);
    if unscoped {
        return handler == lhs;
    }
    let mut at = 0;
    let mut depth = 0;
    while let Some(category) = categories.get(depth) {
        let Some(colon) = find_from(handler, ":", at) else {
            break;
        };
        if handler.get(at..colon) != Some(category.as_str()) {
            break;
        }
        at = colon + 1;
        depth += 1;
    }
    depth == categories.len() && handler.get(at..) == Some(lhs)
}

/// Whether an instance of `special` has the value `field`: one of its
/// options, or its key.
fn has_field(special: &Special, field: &str) -> bool {
    special.options.iter().any(|option| option == field)
        || matches!(&special.key, SpecialKey::Key(key) if key == field)
}

/// Whether `field` is `special`'s key.
fn is_key(special: &Special, field: &str) -> bool {
    matches!(&special.key, SpecialKey::Key(key) if key == field)
}

impl<'s> Reader<'s> {
    fn finish(mut self) -> Document {
        let variables = &self.variables;
        self.document.variables = self
            .defined
            .iter()
            .filter_map(|name| variables.iter().find(|(found, _)| found == name).cloned())
            .collect();
        self.document
    }

    fn diagnose(&mut self, file: &Path, line: usize, message: String) {
        self.document.diagnostics.push(Diagnostic {
            file: file.to_path_buf(),
            line,
            message,
        });
    }

    /// `parseFile`: every line, a continued one joined (`getNextLine`),
    /// and the file's own complaints at its end.
    fn file(&mut self, text: &str, file: &Path, depth: usize) {
        // `std::getline`: `\n` ends a line, and a last one needs none.
        let mut lines = text.split_terminator('\n');
        let mut physical = 0;
        while let Some(first) = lines.next() {
            physical += 1;
            let number = physical;
            let mut line = first.to_owned();
            let mut unfinished = false;
            while let Some(body) = line.strip_suffix('\\') {
                line = body.trim_end_matches([' ', '\t']).to_owned();
                let Some(next) = lines.next() else {
                    unfinished = true;
                    break;
                };
                physical += 1;
                line.push_str(next);
            }
            if unfinished {
                self.diagnose(file, 0, "Last line ends with backslash".to_owned());
                break;
            }
            if let Err(message) = self.line(&line, file, number, depth)
                && !self.no_error
            {
                self.diagnose(file, number, message);
            }
        }
        if !self.categories.is_empty() {
            self.diagnose(file, 0, "Unclosed category at EOF".to_owned());
            self.categories.clear();
        }
    }

    /// `parseLine`.
    fn line(&mut self, raw: &str, file: &Path, number: usize, depth: usize) -> Result<(), String> {
        let line = trim(raw);
        if let Some(comment) = line.strip_prefix('#') {
            return self.comment(comment);
        }
        if self.conditions.last() == Some(&true) {
            return Ok(());
        }
        let stripped = strip_comment(line);
        let line = trim(&stripped);
        if line.is_empty() {
            return Ok(());
        }
        let Some((left, right)) = line.split_once('=') else {
            return self.category(line);
        };
        let mut lhs = trim(left).to_owned();
        let mut rhs = trim(right).to_owned();
        if lhs.is_empty() {
            return Err("Empty lhs.".to_owned());
        }
        let is_variable = lhs.starts_with('$');
        self.expand(&mut lhs, &mut rhs, is_variable)?;
        if let Some(name) = lhs.strip_prefix('$')
            && is_variable
        {
            self.define(name, rhs);
            return Ok(());
        }
        let rhs = remove_escapes(rhs);
        let (found, result) = self.set(&lhs, &rhs, file, number);
        if found {
            return result;
        }
        self.handle(&lhs, &rhs, file, number, depth)
            .unwrap_or(result)
    }

    /// A line without `=`: `name {` or `}`.
    fn category(&mut self, line: &str) -> Result<(), String> {
        if line.contains('}') {
            if line != "}" {
                return Err("Invalid config line".to_owned());
            }
            if self.categories.pop().is_none() {
                return Err("Stray category close".to_owned());
            }
            if self.categories.is_empty() {
                self.current_key.clear();
                self.current_special = None;
            }
            return Ok(());
        }
        let Some(name) = line.strip_suffix('{') else {
            return Err("Invalid config line".to_owned());
        };
        self.categories.push(trim(name).to_owned());
        Ok(())
    }

    /// The variable loop of `parseLine`: every `$name` replaced, longest
    /// first, and `{{a op b}}` worked out, again until nothing changes.
    fn expand(&self, lhs: &mut String, rhs: &mut String, is_variable: bool) -> Result<(), String> {
        for iteration in 0..MAX_ITERATIONS {
            let mut any = false;
            for (name, value) in &self.variables {
                let pattern = format!("${name}");
                let in_lhs = !is_variable && lhs.contains(&pattern);
                let in_rhs = rhs.contains(&pattern);
                if in_lhs {
                    *lhs = replace(lhs, &pattern, value)?;
                }
                if in_rhs {
                    *rhs = replace(rhs, &pattern, value)?;
                }
                any |= in_lhs || in_rhs;
            }
            self.expressions(rhs)?;
            if !any {
                break;
            }
            if iteration + 1 == MAX_ITERATIONS {
                return Err("Expanding variables exceeded max iteration limit".to_owned());
            }
        }
        Ok(())
    }

    /// Each `{{a op b}}` not escaped with an odd run of backslashes,
    /// replaced by its value.
    fn expressions(&self, rhs: &mut String) -> Result<(), String> {
        while rhs.contains("{{") {
            let mut first = rhs.find("{{");
            while let Some(at) = first {
                if at == 0 {
                    break;
                }
                let backslashes = rhs.as_bytes().get(..at).map_or(0, |before| {
                    before
                        .iter()
                        .rev()
                        .take_while(|byte| **byte == b'\\')
                        .count()
                });
                if backslashes % 2 == 0 {
                    break;
                }
                first = find_from(rhs, "{{", at + 1);
            }
            let Some(begin) = first else {
                break;
            };
            let Some(end) = find_from(rhs, "}}", begin + 2) else {
                break;
            };
            let value = self.expression(rhs.get(begin + 2..end).unwrap_or_default())?;
            *rhs = format!(
                "{}{}{}",
                rhs.get(..begin).unwrap_or_default(),
                format_float(value),
                rhs.get(end + 2..).unwrap_or_default()
            );
        }
        Ok(())
    }

    /// `parseExpression`: `a op b`, each a number or a variable's name,
    /// worked out in `float`. The errors are hyprlang's, including its
    /// calling the right-hand side "value 1" too.
    fn expression(&self, text: &str) -> Result<f32, String> {
        if text.is_empty() {
            return Err("Expression is empty".to_owned());
        }
        let args = words(text);
        let arg = |index: usize| args.get(index).copied().unwrap_or_default();
        let operator = arg(1);
        if !matches!(operator, "+" | "-" | "*" | "/") {
            return Err("Invalid expression type: supported +, -, *, /".to_owned());
        }
        let left = self.operand(arg(0))?;
        let right = self.operand(arg(2))?;
        Ok(match operator {
            "+" => left + right,
            "-" => left - right,
            "*" => left * right,
            _ => left / right,
        })
    }

    fn operand(&self, arg: &str) -> Result<f32, String> {
        match self.variables.iter().find(|(name, _)| name == arg) {
            Some((_, value)) => stof32(value).ok_or_else(|| {
                "Failed to parse expression: value 1 holds a variable that does not look like a number"
                    .to_owned()
            }),
            None => stof32(arg).ok_or_else(|| {
                "Failed to parse expression: value 1 does not look like a number or the variable doesn't exist"
                    .to_owned()
            }),
        }
    }

    /// `parseVariable`: set it, or add it and sort longest first again.
    fn define(&mut self, name: &str, value: String) {
        if !self.defined.iter().any(|defined| defined == name) {
            self.defined.push(name.to_owned());
        }
        if let Some((_, old)) = self.variables.iter_mut().find(|(found, _)| found == name) {
            *old = value;
            return;
        }
        self.variables.push((name.to_owned(), value));
        self.variables
            .sort_by_key(|(name, _)| core::cmp::Reverse(name.len()));
    }

    /// `getVariable`: the environment first, then the variables.
    fn variable(&self, name: &str) -> Option<&str> {
        self.schema
            .environment
            .iter()
            .chain(&self.variables)
            .find(|(found, _)| found == name)
            .map(|(_, value)| value.as_str())
    }

    /// `parseComment`: the `# hyprlang` directives.
    fn comment(&mut self, text: &str) -> Result<(), String> {
        let comment = trim(text);
        if !comment.starts_with("hyprlang") {
            return Ok(());
        }
        let args = words(comment);
        let arg = |index: usize| args.get(index).copied().unwrap_or_default();
        let mut condition = "";
        for (index, word) in args.iter().enumerate().skip(1) {
            match *word {
                "noerror" => {
                    self.no_error =
                        matches!(arg(2), "true" | "yes" | "enable" | "enabled" | "set" | "");
                    break;
                }
                "endif" => {
                    if self.conditions.pop().is_none() {
                        return Err("stray endif".to_owned());
                    }
                    break;
                }
                "if" => {
                    condition = arg(index + 1);
                    break;
                }
                _ => {}
            }
        }
        if !condition.is_empty() {
            let (negated, name) = match condition.strip_prefix('!') {
                Some(name) => (true, name),
                None => (false, condition),
            };
            let failed = match self.variable(name) {
                Some(value) => negated != value.is_empty(),
                None => !negated,
            };
            self.conditions.push(failed);
        }
        Ok(())
    }

    /// The special category a name begins with, as `name:`, longest first.
    fn special_for<'n>(&self, name: &'n str) -> Option<(usize, &'s Special, &'n str)> {
        let schema: &'s Schema = self.schema;
        self.specials.iter().find_map(|index| {
            let special = schema.specials.get(*index)?;
            let field = name
                .strip_prefix(special.name.as_str())?
                .strip_prefix(':')?;
            Some((*index, special, field))
        })
    }

    fn special_of(&self, instance: usize) -> Option<&'s Special> {
        let schema: &'s Schema = self.schema;
        schema.specials.get(self.meta.get(instance)?.special)
    }

    /// A new instance of `schema.specials[special]`.
    fn create(&mut self, special: usize, key_value: String, file: &Path, number: usize) -> usize {
        let schema: &'s Schema = self.schema;
        let (category, key) = match schema.specials.get(special) {
            Some(found) => (
                found.name.clone(),
                matches!(found.key, SpecialKey::Key(_)).then(|| key_value.clone()),
            ),
            None => (String::new(), None),
        };
        self.document.instances.push(Instance {
            category,
            key,
            values: Vec::new(),
            file: file.to_path_buf(),
            line: number,
        });
        self.meta.push(Meta {
            special,
            key_value,
            anonymous_id: 0,
        });
        self.document.instances.len() - 1
    }

    /// Record `field = value` in `instance`; setting the key renames it.
    fn set_in(&mut self, instance: usize, field: &str, value: &str, file: &Path, number: usize) {
        let key = self
            .special_of(instance)
            .is_some_and(|special| is_key(special, field));
        if let Some(found) = self.document.instances.get_mut(instance) {
            found.values.push(Setting {
                name: field.to_owned(),
                value: value.to_owned(),
                file: file.to_path_buf(),
                line: number,
            });
            if key {
                found.key = Some(value.to_owned());
            }
        }
        if key && let Some(meta) = self.meta.get_mut(instance) {
            value.clone_into(&mut meta.key_value);
        }
    }

    /// The instance a `name[key]:field` names, made if it is new.
    fn bracketed(&mut self, name: &str, key: &str, file: &Path, number: usize) -> Option<usize> {
        let (special_index, special, _) = self.special_for(name)?;
        let existing = self
            .meta
            .iter()
            .rposition(|meta| meta.special == special_index && meta.key_value == key);
        if existing.is_some() {
            return existing;
        }
        let instance = self.create(special_index, key.to_owned(), file, number);
        if let SpecialKey::Key(field) = &special.key {
            self.set_in(instance, field, key, file, number);
        }
        Some(instance)
    }

    /// `configSetValueSafe`: whether `command = value` named an option or
    /// a special category's value, and what was wrong with it.
    fn set(
        &mut self,
        command: &str,
        value: &str,
        file: &Path,
        number: usize,
    ) -> (bool, Result<(), String>) {
        let schema: &'s Schema = self.schema;
        let mut name = String::new();
        for category in &self.categories {
            name.push_str(category);
            name.push(':');
        }
        name.push_str(command);

        let mut overridden = None;
        if let (Some(left), Some(right)) = (name.find('['), name.rfind(']'))
            && left < right
        {
            let key = name.get(left + 1..right).unwrap_or_default().to_owned();
            key.clone_into(&mut self.current_key);
            name = format!(
                "{}{}",
                name.get(..left).unwrap_or_default(),
                name.get(right + 1..).unwrap_or_default()
            );
            overridden = self.bracketed(&name, &key, file, number);
        }

        if schema.options.contains(&name) {
            self.document.options.push(Setting {
                name,
                value: value.to_owned(),
                file: file.to_path_buf(),
                line: number,
            });
            return (true, Ok(()));
        }

        let mut found: Option<(usize, String)> = None;
        if let Some(instance) = overridden {
            if let Some(special) = self.special_of(instance)
                && let Some(field) = name.get(special.name.len() + 1..)
                && has_field(special, field)
            {
                found = Some((instance, field.to_owned()));
            }
        } else {
            // The instance being filled, by prefix alone, as hyprlang has it.
            if let Some(instance) = self.current_special
                && let Some(special) = self.special_of(instance)
                && name.starts_with(special.name.as_str())
                && let Some(field) = name.get(special.name.len() + 1..)
                && has_field(special, field)
            {
                found = Some((instance, field.to_owned()));
            }
            // "probably a handler"
            if !name.contains(':') {
                return (false, Ok(()));
            }
            if found.is_none() {
                match self.existing(&name, value) {
                    Existing::Found(instance, field) => found = Some((instance, field)),
                    Existing::Ignored => return (false, Ok(())),
                    Existing::None => {}
                }
            }
            if found.is_none() {
                match self.new_instance(&name, value, file, number) {
                    Ok(made) => found = made,
                    Err(message) => return (true, Err(message)),
                }
            }
        }

        let Some((instance, field)) = found else {
            return (
                false,
                Err(format!("config option <{name}> does not exist.")),
            );
        };
        self.set_in(instance, &field, value, file, number);
        (true, Ok(()))
    }

    /// The loop over `specialCategories`: an instance already made that
    /// this line belongs to -- by the key it sets, for a keyed category's
    /// key, or else by `currentSpecialKey`.
    fn existing(&mut self, name: &str, value: &str) -> Existing {
        let schema: &'s Schema = self.schema;
        for (index, meta) in self.meta.iter().enumerate() {
            let Some(special) = schema.specials.get(meta.special) else {
                continue;
            };
            let Some(field) = name
                .strip_prefix(special.name.as_str())
                .and_then(|rest| rest.strip_prefix(':'))
            else {
                continue;
            };
            let wanted = if is_key(special, field) {
                value
            } else {
                self.current_key.as_str()
            };
            if meta.key_value != wanted {
                continue;
            }
            self.current_special = Some(index);
            if has_field(special, field) {
                return Existing::Found(index, field.to_owned());
            }
            if special.ignore_missing {
                return Existing::Ignored;
            }
            return Existing::None;
        }
        Existing::None
    }

    /// The loop over `specialCategoryDescriptors`: a line that begins a new
    /// instance. An anonymous category makes one for any of its values; a
    /// keyed one only for its key, and complains otherwise (having made an
    /// instance keyed `0` all the same, as hyprlang does).
    fn new_instance(
        &mut self,
        name: &str,
        value: &str,
        file: &Path,
        number: usize,
    ) -> Result<Option<(usize, String)>, String> {
        let Some((special_index, special, field)) = self.special_for(name) else {
            return Ok(None);
        };
        if !has_field(special, field) {
            return Ok(None);
        }
        let field = field.to_owned();
        let instance = self.create(special_index, "0".to_owned(), file, number);
        self.current_special = Some(instance);
        match &special.key {
            SpecialKey::Anonymous => {
                let id = self
                    .meta
                    .iter()
                    .map(|meta| meta.anonymous_id)
                    .max()
                    .unwrap_or(0)
                    + 1;
                if let Some(meta) = self.meta.get_mut(instance) {
                    meta.anonymous_id = id;
                    meta.key_value = id.to_string();
                }
                self.current_key = id.to_string();
            }
            SpecialKey::Key(key) => {
                if field != *key {
                    return Err(format!(
                        "special category's first value must be the key. Key for <{}> is <{key}>",
                        special.name
                    ));
                }
                value.clone_into(&mut self.current_key);
            }
        }
        Ok(Some((instance, field)))
    }

    /// The handlers: `source`, then the schema's keywords. `None` if none
    /// takes the line.
    fn handle(
        &mut self,
        lhs: &str,
        rhs: &str,
        file: &Path,
        number: usize,
        depth: usize,
    ) -> Option<Result<(), String>> {
        let schema: &'s Schema = self.schema;
        let mut result = None;
        if schema.source && handles("source", lhs, &self.categories) {
            result = Some(self.source(rhs, file, depth));
        }
        if schema
            .keywords
            .iter()
            .any(|keyword| handles(keyword, lhs, &self.categories))
        {
            self.document.keywords.push(Keyword {
                name: lhs.to_owned(),
                value: rhs.to_owned(),
                categories: self.categories.clone(),
                file: file.to_path_buf(),
                line: number,
            });
            result = Some(Ok(()));
        }
        result
    }

    /// hyprlock's and hypridle's `handleSource`.
    fn source(&mut self, raw: &str, file: &Path, depth: usize) -> Result<(), String> {
        if raw.len() < 2 {
            return Err(format!("source path {raw} bogus!"));
        }
        let home = self
            .schema
            .environment
            .iter()
            .find(|(name, _)| name == "HOME")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_default();
        let dir = file.parent().unwrap_or_else(|| Path::new(""));
        let pattern = source::absolute(raw, dir, &home);
        let Some(paths) = source::glob(&pattern) else {
            return Err("source= globbing error: found no match".to_owned());
        };
        let current = source::weakly_canonical(file);
        for found in paths {
            let path = source::weakly_canonical(&found);
            if path.as_os_str().is_empty() || path == current {
                continue;
            }
            if self.included.contains(&path) {
                continue;
            }
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => continue,
                Err(_) => {
                    return Err(format!("source file {} doesn't exist!", path.display()));
                }
            }
            if depth >= MAX_SOURCE_DEPTH {
                return Err(format!(
                    "source= nesting deeper than {MAX_SOURCE_DEPTH} files"
                ));
            }
            self.included.push(path.clone());
            match fs::read(&path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    self.file(&text, &path, depth + 1);
                }
                Err(_) => self.diagnose(&path, 0, "File failed to open".to_owned()),
            }
        }
        Ok(())
    }
}

/// What the loop over existing instances found.
enum Existing {
    /// This instance's value.
    Found(usize, String),
    /// Nothing, and the category ignores missing values.
    Ignored,
    /// Nothing.
    None,
}

/// `replaceInString`, refusing to grow past [`MAX_LINE_LEN`].
fn replace(text: &str, pattern: &str, value: &str) -> Result<String, String> {
    let count = text.matches(pattern).count();
    let grown = count.saturating_mul(value.len()).saturating_add(text.len());
    if grown > MAX_LINE_LEN + count.saturating_mul(pattern.len()) {
        return Err(format!(
            "Expanding variables exceeded max length of {MAX_LINE_LEN} bytes"
        ));
    }
    Ok(text.replace(pattern, value))
}
