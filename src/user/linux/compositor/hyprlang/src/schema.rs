//! What a program's file may hold.

/// How a special category's instances are told apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecialKey {
    /// Every block is a new instance (`anonymousKeyBased = true`).
    Anonymous,
    /// Instances are named by this option's value, which must be the first
    /// line of a block (`key = "name"`), as Hyprland's `device { name = }`.
    Key(String),
}

/// A special category's declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Special {
    pub(crate) name: String,
    pub(crate) key: SpecialKey,
    /// `ignoreMissing`: an unknown option inside it is not an error.
    pub(crate) ignore_missing: bool,
    /// The options an instance may set, names relative to the category.
    pub(crate) options: Vec<String>,
}

/// What a file may hold: hyprlang's `addConfigValue`,
/// `addSpecialCategory`, `addSpecialConfigValue` and `registerHandler`,
/// without the types and defaults, which are the program's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub(crate) options: Vec<String>,
    pub(crate) specials: Vec<Special>,
    pub(crate) keywords: Vec<String>,
    pub(crate) source: bool,
    /// The variables a parse starts with, as `CConfig::clearState` starts
    /// with the process's environment.
    pub(crate) environment: Vec<(String, String)>,
}

impl Schema {
    /// An empty schema: every line is an unknown option.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An ordinary option, by its full name: `"general:lock_cmd"`,
    /// `"auth:pam:enabled"`.
    #[must_use]
    pub fn option(mut self, name: &str) -> Self {
        self.options.push(name.to_owned());
        self
    }

    /// Several ordinary options at once.
    #[must_use]
    pub fn options(mut self, names: &[&str]) -> Self {
        self.options
            .extend(names.iter().map(|name| (*name).to_owned()));
        self
    }

    /// A special category, `"background"`, `"listener"`, whose options are
    /// `options` (relative names: `"monitor"`, `"on-timeout"`).
    #[must_use]
    pub fn special(mut self, name: &str, key: SpecialKey, options: &[&str]) -> Self {
        self.specials.push(Special {
            name: name.to_owned(),
            key,
            ignore_missing: false,
            options: options.iter().map(|option| (*option).to_owned()).collect(),
        });
        self
    }

    /// The same, with `ignoreMissing`: an option the category does not
    /// have is dropped silently rather than reported.
    #[must_use]
    pub fn special_ignoring_missing(
        mut self,
        name: &str,
        key: SpecialKey,
        options: &[&str],
    ) -> Self {
        self.specials.push(Special {
            name: name.to_owned(),
            key,
            ignore_missing: true,
            options: options.iter().map(|option| (*option).to_owned()).collect(),
        });
        self
    }

    /// A keyword: a line handed to the program as it is, in order, rather
    /// than stored (`registerHandler`). `"bezier"` matches in any category;
    /// `"animations:bezier"` only inside `animations { }`.
    #[must_use]
    pub fn keyword(mut self, name: &str) -> Self {
        self.keywords.push(name.to_owned());
        self
    }

    /// Start every parse with these variables, as hyprlang starts with the
    /// environment (`clearState` sets `variables = envVariables`): `$HOME`
    /// in a value is the home directory. A file's own `$name = …` replaces
    /// one of the same name. Given explicitly so that a test stays pure;
    /// a program passes [`Schema::process_environment`].
    #[must_use]
    pub fn environment<I, K, V>(mut self, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.environment.extend(
            variables
                .into_iter()
                .map(|(name, value)| (name.into(), value.into())),
        );
        self
    }

    /// [`Schema::environment`] with this process's environment, which is
    /// what hyprlock and hypridle get upstream.
    #[must_use]
    pub fn process_environment(self) -> Self {
        let variables: Vec<(String, String)> = std::env::vars().collect();
        self.environment(variables)
    }

    /// Read `source = path` lines as hyprlock and hypridle do: the file is
    /// parsed there and then, into the same document.
    #[must_use]
    pub fn source(mut self) -> Self {
        self.source = true;
        self
    }
}
