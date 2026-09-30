//! `hypridle.conf` read through `compositor/hyprlang`, into the list of
//! entries the model types.
//!
//! hypridle's schema is upstream `ConfigManager::init()`'s declarations:
//! the ten `general:` options, the anonymous special category `listener`
//! with its six, and `source`. The file starts with the environment's
//! variables, as hyprlang's `clearState` starts with them. Values come back
//! as the file wrote them; [`crate::config`] types them and applies the
//! defaults, as upstream's `postParse` reads them.

use std::path::{Path, PathBuf};

use compositor_hyprlang::{Document, Schema, SpecialKey};

/// The `general` options.
pub const GENERAL: [&str; 10] = [
    "general:lock_cmd",
    "general:unlock_cmd",
    "general:on_lock_cmd",
    "general:on_unlock_cmd",
    "general:before_sleep_cmd",
    "general:after_sleep_cmd",
    "general:ignore_dbus_inhibit",
    "general:ignore_systemd_inhibit",
    "general:ignore_wayland_inhibit",
    "general:inhibit_sleep",
];

/// A `listener`'s options.
pub const LISTENER: [&str; 6] = [
    "timeout",
    "on-timeout",
    "on-resume",
    "ignore_inhibit",
    "condition_cmd",
    "condition_retry",
];

/// One `key = value`, with where it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its full name: `general:lock_cmd`, `listener:timeout`.
    pub name: String,
    /// The value as the file wrote it, variables expanded.
    pub value: String,
    /// Which `listener` instance it is in, counting from 1; 0 for a
    /// `general` option.
    pub block: usize,
    /// Where it was, for a diagnostic.
    pub at: Place,
}

/// A file and a line in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The file.
    pub file: PathBuf,
    /// The line, counting from one.
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

/// What reading gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    /// Every option set, then every listener's, in file order.
    pub entries: Vec<Entry>,
    /// hyprlang's diagnostics, in its words.
    pub errors: Vec<String>,
}

/// hypridle's schema, starting from `environment`.
#[must_use]
pub fn schema(environment: &[(String, String)]) -> Schema {
    Schema::new()
        .options(&GENERAL)
        .special("listener", SpecialKey::Anonymous, &LISTENER)
        .source()
        .environment(environment.iter().cloned())
}

/// Read the file at `path`.
#[must_use]
pub fn read(path: &Path, environment: &[(String, String)]) -> Read {
    match compositor_hyprlang::parse_file(&schema(environment), path) {
        Ok(document) => from_document(&document),
        Err(error) => Read {
            entries: Vec::new(),
            errors: vec![format!("{}: {error}", path.display())],
        },
    }
}

/// Read `text` as the file `path`.
#[must_use]
pub fn read_text(text: &str, path: &Path, environment: &[(String, String)]) -> Read {
    from_document(&compositor_hyprlang::parse(
        &schema(environment),
        text,
        path,
    ))
}

fn from_document(document: &Document) -> Read {
    let mut entries: Vec<Entry> = document
        .options
        .iter()
        .map(|setting| Entry {
            name: setting.name.clone(),
            value: setting.value.clone(),
            block: 0,
            at: Place {
                file: setting.file.clone(),
                line: setting.line,
            },
        })
        .collect();
    for (index, instance) in document.instances_of("listener").enumerate() {
        entries.extend(instance.values.iter().map(|setting| Entry {
            name: format!("listener:{}", setting.name),
            value: setting.value.clone(),
            block: index + 1,
            at: Place {
                file: setting.file.clone(),
                line: setting.line,
            },
        }));
    }
    Read {
        entries,
        errors: document
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}
