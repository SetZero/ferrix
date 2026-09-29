//! Command lines and environments, as `config_parse_exec` and
//! `config_parse_environ` read them.
//!
//! An `ExecStart=` value is a program and its arguments, split and unquoted
//! as [`crate::value::words`] splits them. The first word may
//! start with prefixes: `-` ignores the command's failure, `@` makes the
//! second word `argv[0]`, `:` turns off `$VAR` expansion, and `+`, `!` and
//! `!!` ask for privileges the unit's `User=` would otherwise drop. A bare
//! `;` separates one command from the next.
//!
//! Variables are not expanded here. `$VAR` needs the environment the
//! command will run with, which is the backend's to build, so a command
//! carries its words as written and whether expansion is wanted.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::value::{self, Word, is_env_name};

/// How much privilege a command keeps, from its prefix.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Privilege {
    /// As the unit says: its `User=`, `Group=` and sandboxing.
    #[default]
    Unit,
    /// `+`: as root, without the unit's user and sandboxing.
    Full,
    /// `!`: the unit's user, keeping the capabilities it would lose.
    KeepCapabilities,
    /// `!!`: as `!`, and only where ambient capabilities are missing.
    KeepCapabilitiesIfNoAmbient,
}

/// One command of an `Exec…=` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The program: an absolute path, or a bare name to search for.
    pub path: String,
    /// The arguments, `argv[0]` first.
    pub argv: Vec<String>,
    /// `-`: a failure of this command is not the unit's.
    pub ignore_failure: bool,
    /// Whether `$VAR` and `${VAR}` are to be expanded; `:` says not.
    pub expand_environment: bool,
    /// The privilege prefix.
    pub privilege: Privilege,
}

/// The prefixes of a command's first word, whether `@` was one of them,
/// and what is left of the word.
fn prefixes(first: &str) -> Result<(Command, bool, &str), String> {
    let mut command = Command {
        path: String::new(),
        argv: Vec::new(),
        ignore_failure: false,
        expand_environment: true,
        privilege: Privilege::Unit,
    };
    let mut separate_argv0 = false;
    let mut rest = first;
    while let Some(flag @ ('-' | '@' | ':' | '+' | '!')) = rest.chars().next() {
        let after = rest.get(1..).unwrap_or("");
        let repeated = match flag {
            '-' => core::mem::replace(&mut command.ignore_failure, true),
            '@' => core::mem::replace(&mut separate_argv0, true),
            ':' => !core::mem::replace(&mut command.expand_environment, false),
            '+' if command.privilege == Privilege::Unit => {
                command.privilege = Privilege::Full;
                false
            }
            '!' if command.privilege == Privilege::Unit => {
                command.privilege = Privilege::KeepCapabilities;
                false
            }
            '!' if command.privilege == Privilege::KeepCapabilities => {
                command.privilege = Privilege::KeepCapabilitiesIfNoAmbient;
                false
            }
            _ => true,
        };
        if repeated {
            return Err(format!(
                "Conflicting or repeated prefix '{flag}' in command line"
            ));
        }
        rest = after;
    }
    Ok((command, separate_argv0, rest))
}

/// Parse an `Exec…=` value into its commands.
///
/// An empty value is no commands, which the caller reads as clearing the
/// list.
///
/// # Errors
///
/// Why the value is not a command line, in systemd's words.
pub fn commands(text: &str) -> Result<Vec<Command>, String> {
    let words = value::words(text).map_err(|error| format!("Invalid command line ({error})"))?;
    let mut out = Vec::new();
    let mut rest: &[Word] = &words;
    while let Some((first, after)) = rest.split_first() {
        let end = after
            .iter()
            .position(|word| word.bare && word.text == ";")
            .unwrap_or(after.len());
        let (arguments, next) = after.split_at(end);
        out.push(command(first, arguments)?);
        rest = next.get(1..).unwrap_or(&[]);
    }
    Ok(out)
}

/// One command: its first word, prefixes and all, and the words after it.
fn command(first: &Word, arguments: &[Word]) -> Result<Command, String> {
    let (mut command, separate_argv0, path) = prefixes(&first.text)?;
    if path.is_empty() {
        return Err("Empty path in command line".to_owned());
    }
    if !path.starts_with('/') && path.contains('/') {
        return Err(format!(
            "Neither a valid executable name nor an absolute path: {path}"
        ));
    }
    command.path = path.to_owned();
    let mut arguments = arguments.iter().map(|word| word.text.clone());
    let argv0 = if separate_argv0 {
        arguments
            .next()
            .ok_or_else(|| "Empty argv[0] with the '@' prefix".to_owned())?
    } else {
        command.path.clone()
    };
    command.argv.push(argv0);
    command.argv.extend(arguments);
    Ok(command)
}

/// What an `Environment=` value holds: its assignments, and the words that
/// were not assignments, for the caller to warn about.
pub type Assignments = (Vec<(String, String)>, Vec<String>);

/// An `Environment=` value's assignments.
///
/// # Errors
///
/// When the value does not split into words at all.
pub fn environment(text: &str) -> Result<Assignments, String> {
    let words = value::words(text).map_err(|error| format!("Invalid environment ({error})"))?;
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for word in words {
        match word.text.split_once('=') {
            Some((name, value)) if is_env_name(name) => {
                good.push((name.to_owned(), value.to_owned()));
            }
            _ => bad.push(word.text),
        }
    }
    Ok((good, bad))
}
