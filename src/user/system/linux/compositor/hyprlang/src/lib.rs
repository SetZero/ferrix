//! hyprlang: the language `hyprlock.conf` and `hypridle.conf` are written
//! in, read the way the hyprlang library (0.6, `src/config.cpp`) reads it.
//!
//! `src/user/system/linux/compositor/config` reads `hyprland.conf`, and knows every one of
//! Hyprland's options; this crate knows none. A program says what its file
//! may hold -- a [`Schema`] of option names, *special categories* and
//! keywords, as hyprlock's and hypridle's `ConfigManager.cpp` call
//! `addConfigValue`, `addSpecialCategory` and `registerHandler` -- and
//! [`parse`] hands back a [`Document`]: every option set, every special
//! category instance in file order, every keyword line, and a
//! [`Diagnostic`] for each line hyprlang would have complained about.
//! Values stay the text the file wrote; [`value`] reads them as hyprlang's
//! `INT`, `FLOAT`, `VEC2` and colours do, and each program types its own.
//!
//! # The grammar, as `CConfig::parseLine` has it
//!
//! * A line is trimmed. If its first character is `#` it is a comment, the
//!   whole of it (and `# hyprlang if VAR` / `endif` / `noerror` are
//!   directives). Otherwise `##` is a literal `#` and a single `#` starts
//!   a comment: `<span foreground="##cccccc">` is `#cccccc`.
//! * A line ending in `\` continues on the next, the spaces before the
//!   backslash dropped.
//! * `name {` opens a category and `}` closes one; they nest, and a name
//!   inside is the categories joined with `:` -- `auth { pam { enabled } }`
//!   is `auth:pam:enabled`, which may also be written as that one line.
//! * `$name = value` defines a variable; every `$name` in a later line is
//!   replaced by its value, longest names first (so `$font` does not eat
//!   `$fontsize`), before anything else is read. The environment's
//!   variables are there from the start ([`Schema::environment`]), as
//!   `CConfig::clearState` puts them there, so `$HOME` expands. `{{a + b}}`
//!   is arithmetic on numbers and variables.
//! * `name = value` inside categories is the option `cat:…:name` if the
//!   schema has it; otherwise a keyword of that name (unscoped, or scoped
//!   to exactly these categories) if the schema has one; otherwise
//!   `config option <cat:name> does not exist.`
//! * A special category is one that repeats. An *anonymous* one
//!   (`anonymousKeyBased`, which is every widget in hyprlock and hypridle's
//!   `listener`) makes a new instance for every block: three `background {
//!   }` blocks are three backgrounds. A *keyed* one (`key = "name"`) makes
//!   one instance per distinct key value, and a later block with the same
//!   key adds to it.
//! * `source = path` reads another file there and then, when the schema
//!   allows it ([`Schema::source`]), with `~` expanded and a relative path
//!   taken from the including file's directory, as hyprlock's and
//!   hypridle's `handleSource` do; a glob matches in its directory.

mod number;
mod parse;
mod reader;
mod schema;
mod source;
pub mod value;

pub use parse::{Diagnostic, Document, Instance, Keyword, Setting, parse, parse_file};
pub use schema::{Schema, SpecialKey};

#[cfg(test)]
mod tests;
