//! The unit directories, as the caller fills them, and loading a unit from
//! them (§4.1).
//!
//! Three directories hold units, each overriding the one below it:
//!
//! | [`Layer`] | Directory | Written by |
//! |---|---|---|
//! | [`Runtime`](Layer::Runtime) | `/run/ferrix/units` | generators, at every boot |
//! | [`Admin`](Layer::Admin) | `/etc/ferrix/units` | the administrator; `svc enable` |
//! | [`Image`](Layer::Image) | `/lib/ferrix/units` | the image |
//!
//! The crate reads no directory. The backend walks the three and hands each
//! entry to [`Source::add`] under its path relative to its directory, as an
//! [`Entry`]: a file's bytes, a link to `/dev/null` (masked), or a link to
//! another unit's name (an alias). A link to a file under the link's own
//! name, `/etc/…/x.service -> /lib/…/x.service`, the backend follows and
//! hands in as the file, since only the name a link points at means
//! anything here. What [`Source::add`] takes:
//!
//! * `name.type`: a unit, a template (`getty@.service`) or an instance;
//! * `name.type.d/file.conf`: a drop-in;
//! * `name.type.wants/other.type` and `name.type.requires/other.type`: a
//!   dependency, named by the link's own name, whatever it points at.
//!
//! [`Source::load`] then does what systemd's unit loading does: the file
//! from the highest directory that has the name, or its template's; aliases
//! followed; drop-ins of the unit, its aliases and its template from all
//! three directories, a file in a higher directory hiding one of the same
//! name below it, applied in file-name order; specifiers expanded; and the
//! sections parsed by the unit's kind.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use crate::ini::{self, Document, Section, SyntaxError};
use crate::kind::{self, Config, UnitError};
use crate::limits;
use crate::name::{NameError, UnitType};
use crate::specifier;
use crate::unit::{Dependency, Install, UnitSection};
use crate::{UnitName, Warnings};

/// How many links an alias may go through before the chain is refused.
const MAX_LINKS: usize = 32;

/// One of the three unit directories, highest first in [`Layer::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// `/run/ferrix/units`.
    Runtime,
    /// `/etc/ferrix/units`.
    Admin,
    /// `/lib/ferrix/units`.
    Image,
}

impl Layer {
    /// The directories, the one that wins first.
    pub const ALL: [Layer; 3] = [Layer::Runtime, Layer::Admin, Layer::Image];

    /// Its path.
    pub fn directory(self) -> &'static str {
        match self {
            Layer::Runtime => "/run/ferrix/units",
            Layer::Admin => "/etc/ferrix/units",
            Layer::Image => "/lib/ferrix/units",
        }
    }
}

/// What a directory entry is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A file, with its bytes. An empty one masks, as in systemd.
    File(Vec<u8>),
    /// A link to `/dev/null`: masked.
    Masked,
    /// A link to another unit, by its name: the entry's own name is an
    /// alias of it.
    Alias(String),
}

impl Entry {
    /// Whether it masks what it names.
    fn masks(&self) -> bool {
        match self {
            Entry::Masked => true,
            Entry::File(bytes) => bytes.is_empty(),
            Entry::Alias(_) => false,
        }
    }
}

/// A path [`Source::add`] does not take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAUnitPath(pub String);

impl fmt::Display for NotAUnitPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a unit, a drop-in or a dependency link",
            self.0
        )
    }
}

/// Why a unit did not load: systemd's load states other than `loaded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// `not-found`: no file under the name, its aliases or its template,
    /// for a kind that needs one.
    NotFound,
    /// `masked`: linked to `/dev/null`, or an empty file.
    Masked,
    /// Not a unit name, or an alias to something that is not one.
    Name(NameError),
    /// `error`: aliases that go round, or on too long.
    LinkLoop,
    /// `error`: a file that does not parse.
    Syntax(SyntaxError),
    /// `bad-setting`: settings the kind refuses.
    Refused(UnitError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::NotFound => f.write_str("not found"),
            LoadError::Masked => f.write_str("masked"),
            LoadError::Name(error) => write!(f, "bad name: {error}"),
            LoadError::LinkLoop => f.write_str("too many levels of aliases"),
            LoadError::Syntax(error) => {
                write!(f, "{}:{}: {}", error.file, error.line, error.message)
            }
            LoadError::Refused(error) => write!(f, "bad setting: {error}"),
        }
    }
}

impl From<NameError> for LoadError {
    fn from(error: NameError) -> Self {
        LoadError::Name(error)
    }
}

/// A loaded unit: everything its files say, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Its name: the one its file is under, after aliases.
    pub name: UnitName,
    /// The other names it was reached by, in the order they were followed.
    pub aliases: Vec<UnitName>,
    /// The file it was loaded from; `None` for a kind that needs none.
    pub fragment: Option<Arc<str>>,
    /// The drop-ins applied, in order.
    pub drop_ins: Vec<Arc<str>>,
    /// `[Unit]`, with the `.wants/` and `.requires/` links added.
    pub unit: UnitSection,
    /// `[Install]`.
    pub install: Install,
    /// The kind's own settings.
    pub config: Config,
    /// What loading it warned about.
    pub warnings: Warnings,
}

/// The unit directories' contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    entries: BTreeMap<(Layer, String), Entry>,
}

/// The unit name a directory under the unit directory is for, if the
/// directory is `name.type` and `suffix`.
fn directory_of<'a>(directory: &'a str, suffix: &str) -> Option<&'a str> {
    directory
        .strip_suffix(suffix)
        .filter(|name| UnitName::parse(name).is_ok())
}

impl Source {
    /// Empty directories.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one entry, at `path` relative to `layer`'s directory. A later
    /// entry at the same path replaces an earlier one.
    ///
    /// # Errors
    ///
    /// When the path is not one of the forms the module lists; the backend
    /// skips such an entry, as systemd ignores a stray file.
    pub fn add(&mut self, layer: Layer, path: &str, entry: Entry) -> Result<(), NotAUnitPath> {
        let valid = match path.split_once('/') {
            None => UnitName::parse(path).is_ok(),
            Some((directory, file)) if !file.contains('/') => {
                (directory_of(directory, ".d").is_some()
                    && file.ends_with(".conf")
                    && file.len() > ".conf".len())
                    || ((directory_of(directory, ".wants").is_some()
                        || directory_of(directory, ".requires").is_some())
                        && UnitName::parse(file).is_ok())
            }
            Some(_) => false,
        };
        if !valid {
            return Err(NotAUnitPath(path.to_owned()));
        }
        let _ = self.entries.insert((layer, path.to_owned()), entry);
        Ok(())
    }

    /// Every unit, template and alias name with an entry of its own, once.
    pub fn names(&self) -> Vec<UnitName> {
        let mut names: Vec<UnitName> = self
            .entries
            .keys()
            .filter(|(_, path)| !path.contains('/'))
            .filter_map(|(_, path)| UnitName::parse(path).ok())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// The highest entry at `path`, and its layer.
    fn find(&self, path: &str) -> Option<(Layer, &Entry)> {
        Layer::ALL.into_iter().find_map(|layer| {
            self.entries
                .get(&(layer, path.to_owned()))
                .map(|entry| (layer, entry))
        })
    }

    /// The entries in `directory` of `layer`, by file name, in name order.
    fn directory<'a>(
        &'a self,
        layer: Layer,
        directory: &str,
    ) -> impl Iterator<Item = (&'a str, &'a Entry)> + 'a {
        let prefix = format!("{directory}/");
        self.entries
            .range((layer, prefix.clone())..)
            .take_while(move |((at, path), _)| *at == layer && path.starts_with(&prefix))
            .filter_map(|((_, path), entry)| path.split_once('/').map(|(_, file)| (file, entry)))
    }

    /// The entry for `name`, or for its template, and the path it is at.
    fn lookup(&self, name: &UnitName) -> Option<(Layer, String, &Entry)> {
        let own = self
            .find(name.as_str())
            .map(|(layer, entry)| (layer, name.to_string(), entry));
        own.or_else(|| {
            let template = name.template()?;
            self.find(template.as_str())
                .map(|(layer, entry)| (layer, template.to_string(), entry))
        })
    }

    /// Follow aliases from `name` to the unit they end at: its name, the
    /// file it has if any, and the names followed on the way.
    fn resolve(&self, name: &UnitName) -> Result<Resolved<'_>, LoadError> {
        let mut current = name.clone();
        let mut aliases = Vec::new();
        for _ in 0..MAX_LINKS {
            let Some((layer, path, entry)) = self.lookup(&current) else {
                return Ok((current, None, aliases));
            };
            if entry.masks() {
                return Err(LoadError::Masked);
            }
            match entry {
                Entry::File(bytes) => return Ok((current, Some((layer, path, bytes)), aliases)),
                Entry::Alias(target) => {
                    let mut target = UnitName::parse(target)?;
                    if let (true, Some(instance)) = (target.is_template(), current.instance()) {
                        target = target.instantiate(instance)?;
                    }
                    aliases.push(core::mem::replace(&mut current, target));
                }
                Entry::Masked => return Err(LoadError::Masked),
            }
        }
        Err(LoadError::LinkLoop)
    }

    /// Load the unit `name`.
    ///
    /// # Errors
    ///
    /// Why it did not load, as [`LoadError`] says; warnings do not stop it.
    pub fn load(&self, name: &str) -> Result<Unit, LoadError> {
        let asked = UnitName::parse(name)?;
        if asked.is_template() {
            return Err(LoadError::Name(NameError::Template));
        }
        let (name, fragment, aliases) = self.resolve(&asked)?;
        let kind = kind::of(name.unit_type());
        let mut warnings = Warnings::new();
        let mut document = Document::default();
        let fragment = match fragment {
            Some((layer, path, bytes)) => {
                let file: Arc<str> = format!("{}/{path}", layer.directory()).into();
                document = ini::parse(&file, bytes, &mut warnings).map_err(LoadError::Syntax)?;
                Some(file)
            }
            None if kind.needs_file() => return Err(LoadError::NotFound),
            None => None,
        };
        let mut names = Vec::with_capacity(aliases.len() + 1);
        names.push(name.clone());
        names.extend(aliases.iter().cloned());
        let drop_ins = self.drop_ins(&names, &mut document, &mut warnings)?;
        expand(&mut document, &name, &mut warnings);

        let empty = Section::empty("");
        let section = |title: &str| document.section(title).unwrap_or(&empty);
        for (title, section) in &document.sections {
            let known =
                title == "Unit" || title == "Install" || kind.section() == Some(title.as_str());
            if !known
                && !title.starts_with("X-")
                && let Some(first) = section.assignments.first()
            {
                warnings.at(first, format!("Unknown section '{title}'. Ignoring."));
            }
        }
        let mut unit = UnitSection::parse(section("Unit"), &mut warnings);
        if name.unit_type() == UnitType::Service {
            unit.start_limit_from(section("Service"), &mut warnings);
        }
        let install = Install::parse(section("Install"), &mut warnings);
        let config = kind
            .parse(&name, kind.section().map_or(&empty, section), &mut warnings)
            .map_err(LoadError::Refused)?;
        self.links(&names, &mut unit);
        Ok(Unit {
            name,
            aliases,
            fragment,
            drop_ins,
            unit,
            install,
            config,
            warnings,
        })
    }

    /// Every directory `names` and their templates have drop-ins or links
    /// in, as `suffix` directories: the unit's own first.
    fn directories(names: &[UnitName], suffix: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in names {
            for candidate in [Some(name.clone()), name.template()].into_iter().flatten() {
                let directory = format!("{candidate}{suffix}");
                if !out.contains(&directory) {
                    out.push(directory);
                }
            }
        }
        out
    }

    /// Merge the drop-ins of `names` into `document`, in file-name order, a
    /// file in a higher layer hiding one of the same name lower down, and
    /// say which were applied.
    fn drop_ins(
        &self,
        names: &[UnitName],
        document: &mut Document,
        warnings: &mut Warnings,
    ) -> Result<Vec<Arc<str>>, LoadError> {
        let directories = Self::directories(names, ".d");
        let mut chosen: BTreeMap<&str, (Layer, &str, &Entry)> = BTreeMap::new();
        for layer in Layer::ALL {
            for directory in &directories {
                for (file, entry) in self.directory(layer, directory) {
                    let _ = chosen.entry(file).or_insert((layer, directory, entry));
                }
            }
        }
        let mut applied = Vec::new();
        for (file, (layer, directory, entry)) in chosen {
            let Entry::File(bytes) = entry else {
                continue;
            };
            if entry.masks() {
                continue;
            }
            let path: Arc<str> = format!("{}/{directory}/{file}", layer.directory()).into();
            let drop_in = ini::parse(&path, bytes, warnings).map_err(LoadError::Syntax)?;
            document.merge(drop_in);
            applied.push(path);
        }
        Ok(applied)
    }

    /// Add the `.wants/` and `.requires/` links of `names` to `unit`.
    fn links(&self, names: &[UnitName], unit: &mut UnitSection) {
        let kinds = [
            (".wants", Dependency::Wants),
            (".requires", Dependency::Requires),
        ];
        for (suffix, dependency) in kinds {
            let directories = Self::directories(names, suffix);
            let entries = Layer::ALL.into_iter().flat_map(|layer| {
                directories
                    .iter()
                    .flat_map(move |directory| self.directory(layer, directory))
            });
            for (file, entry) in entries {
                if let Ok(other) = UnitName::parse(file)
                    && !entry.masks()
                    && !other.is_template()
                {
                    unit.add(dependency, other);
                }
            }
        }
    }
}

/// What following aliases ends at: the unit's name, its file if it has one
/// (layer, path and bytes), and the names followed.
type Resolved<'a> = (
    UnitName,
    Option<(Layer, String, &'a Vec<u8>)>,
    Vec<UnitName>,
);

/// Expand the specifiers in every value, dropping an assignment whose
/// specifiers do not resolve.
///
/// The resource keys are left as written: their `%` is a percentage, and
/// systemd's parsers for them expand nothing either.
fn expand(document: &mut Document, name: &UnitName, warnings: &mut Warnings) {
    for section in document.sections.values_mut() {
        section.assignments.retain_mut(|assignment| {
            if limits::KEYS.iter().any(|(key, _)| *key == assignment.key) {
                return true;
            }
            match specifier::expand(&assignment.value, name) {
                Ok(value) => {
                    assignment.value = value;
                    true
                }
                Err(error) => {
                    warnings.at(
                        assignment,
                        format!(
                            "Failed to resolve unit specifiers in '{}' ({error}), ignoring.",
                            assignment.value
                        ),
                    );
                    false
                }
            }
        });
    }
}
