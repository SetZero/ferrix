//! A command line from `exec`, `exec-once` or a bind, as the words a program
//! is started with.
//!
//! Hyprland hands such a line to `/bin/sh -c`, so its configurations quote
//! an argument that holds spaces. This compositor starts the program itself,
//! and splits the line as the shell would for the quoting alone: `'...'`
//! keeps everything, `"..."` keeps everything but a backslash before `"`,
//! `\`, `$` or `` ` ``, and a backslash outside quotes keeps the character
//! after it. Nothing is expanded -- no `$variable`, glob or `~` -- and there
//! are no pipes or redirections; a line that wants those can say
//! `sh -c '...'`. Chrome's `--user-agent` is what asked for it. Leading
//! `NAME=value` words set the program's environment, as they do before a
//! command in `sh` ([`assignments`]).

/// The words of `command`, or why it has none.
///
/// # Errors
///
/// An empty command, or one whose quote is never closed.
pub fn words(command: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut characters = command.chars();
    while let Some(character) = characters.next() {
        match character {
            _ if character.is_whitespace() => {
                if let Some(word) = word.take() {
                    words.push(word);
                }
            }
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match characters.next() {
                        Some('\'') => break,
                        Some(character) => word.push(character),
                        None => return Err("a ' that is never closed".to_owned()),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match characters.next() {
                        Some('"') => break,
                        Some('\\') => match characters.next() {
                            Some(escaped @ ('"' | '\\' | '$' | '`')) => word.push(escaped),
                            Some(other) => {
                                word.push('\\');
                                word.push(other);
                            }
                            None => return Err("a \" that is never closed".to_owned()),
                        },
                        Some(character) => word.push(character),
                        None => return Err("a \" that is never closed".to_owned()),
                    }
                }
            }
            '\\' => {
                // A backslash at the very end stands for itself, as it does
                // in `sh -c`.
                let escaped = characters.next().unwrap_or('\\');
                word.get_or_insert_with(String::new).push(escaped);
            }
            _ => word.get_or_insert_with(String::new).push(character),
        }
    }
    if let Some(word) = word {
        words.push(word);
    }
    if words.is_empty() {
        return Err("an empty command".to_owned());
    }
    Ok(words)
}

/// The leading `NAME=value` words of `words`, which `sh` takes as the
/// program's environment rather than as the program, and the words after
/// them. `NAME` is a shell name: a letter or `_`, then letters, digits or `_`.
/// Unlike `sh`, a quoted `'NAME=value'` counts too: the quoting is gone by
/// now.
#[must_use]
pub fn assignments(words: &[String]) -> (Vec<(String, String)>, &[String]) {
    let mut assigned = Vec::new();
    let mut rest = words;
    while let Some((word, after)) = rest.split_first() {
        let Some((name, value)) = word.split_once('=') else {
            break;
        };
        let mut characters = name.chars();
        let named = characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && characters.all(|next| next.is_ascii_alphanumeric() || next == '_');
        if !named {
            break;
        }
        assigned.push((name.to_owned(), value.to_owned()));
        rest = after;
    }
    (assigned, rest)
}

/// Why a command did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotStarted {
    /// No such file: the program, or a script's `#!` interpreter when the
    /// program itself is `there`.
    Missing {
        /// The program's word, as the command named it.
        program: String,
        /// Whether that path exists, so what is missing is its interpreter.
        there: bool,
    },
    /// Any other reason, as a sentence.
    Other(String),
}

impl From<String> for NotStarted {
    fn from(reason: String) -> Self {
        Self::Other(reason)
    }
}

impl core::fmt::Display for NotStarted {
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Missing { there: false, .. } => out.write_str("No such file or directory"),
            Self::Missing { there: true, .. } => {
                out.write_str("its #! interpreter is not on this system")
            }
            Self::Other(reason) => out.write_str(reason),
        }
    }
}

/// The programs already said to be missing, so that a bind pressed on every
/// click -- `bindn = , mouse:272, exec, <a Python script>` -- is said once,
/// not once a click. Every other reason is said each time.
#[derive(Debug, Default)]
pub struct Missing {
    said: std::collections::BTreeSet<String>,
}

impl Missing {
    /// The line to log for `command` not starting, or `None` when its
    /// program was already said to be missing.
    pub fn line(&mut self, command: &str, why: &NotStarted) -> Option<String> {
        match why {
            NotStarted::Missing { program, there } => {
                if !self.said.insert(program.clone()) {
                    return None;
                }
                let what = if *there {
                    format!("{program}'s #! interpreter is not on this system")
                } else {
                    format!("{program} does not exist on this system")
                };
                Some(format!(
                    "hyprix: {what}, so `{command}` did not start; nothing that starts it is \
                     reported again"
                ))
            }
            NotStarted::Other(reason) => Some(format!("hyprix: {command} did not start: {reason}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Missing, NotStarted, assignments, words};

    fn split(command: &str) -> Vec<String> {
        words(command).unwrap()
    }

    #[test]
    fn plain_words_split_at_whitespace() {
        assert_eq!(
            split("  kitty   --single-instance\t-e zsh "),
            ["kitty", "--single-instance", "-e", "zsh"]
        );
    }

    #[test]
    fn a_quoted_argument_keeps_its_spaces_and_commas() {
        assert_eq!(
            split("chrome '--user-agent=Mozilla/5.0 (Ferrix x86_64) (KHTML, like Gecko)' page"),
            [
                "chrome",
                "--user-agent=Mozilla/5.0 (Ferrix x86_64) (KHTML, like Gecko)",
                "page"
            ]
        );
        assert_eq!(split(r#"echo "a  b" c"#), ["echo", "a  b", "c"]);
    }

    #[test]
    fn quotes_join_the_word_they_touch() {
        assert_eq!(
            split(r#"--name="two words"x 'a'"b"c"#),
            ["--name=two wordsx", "abc"]
        );
        assert_eq!(split("say ''"), ["say", ""]);
    }

    #[test]
    fn backslashes_escape_as_the_shell_does() {
        assert_eq!(split(r"echo a\ b \'"), ["echo", "a b", "'"]);
        assert_eq!(split(r#"echo "\"\\\$\n""#), ["echo", r#""\$\n"#]);
        assert_eq!(split(r"echo '\n'"), ["echo", r"\n"]);
        assert_eq!(split(r"echo \"), ["echo", r"\"]);
    }

    #[test]
    fn nothing_is_expanded() {
        assert_eq!(split("echo $HOME ~ *"), ["echo", "$HOME", "~", "*"]);
    }

    #[test]
    fn leading_assignments_are_the_environment() {
        let line = split("HOME=/dev/shm _X1= chrome --flag=1 A=b");
        let (assigned, rest) = assignments(&line);
        assert_eq!(
            assigned,
            [
                ("HOME".to_owned(), "/dev/shm".to_owned()),
                ("_X1".to_owned(), String::new())
            ]
        );
        assert_eq!(rest, ["chrome", "--flag=1", "A=b"]);
        let line = split("1A=b prog");
        assert_eq!(assignments(&line).1, ["1A=b", "prog"]);
        let line = split("=b prog");
        assert_eq!(assignments(&line).1, ["=b", "prog"]);
        let line = split("A=b");
        assert!(assignments(&line).1.is_empty());
    }

    #[test]
    fn a_missing_program_is_said_once_and_other_reasons_every_time() {
        let mut missing = Missing::default();
        let fx = NotStarted::Missing {
            program: "/home/u/.local/bin/fx".to_owned(),
            there: false,
        };
        let press = missing.line("/home/u/.local/bin/fx press", &fx).unwrap();
        assert!(
            press.contains("/home/u/.local/bin/fx does not exist on this system"),
            "{press}"
        );
        assert!(
            press.contains("`/home/u/.local/bin/fx press` did not start"),
            "{press}"
        );
        assert_eq!(missing.line("/home/u/.local/bin/fx release", &fx), None);
        assert_eq!(missing.line("/home/u/.local/bin/fx press", &fx), None);
        let script = NotStarted::Missing {
            program: "/home/u/.local/bin/py".to_owned(),
            there: true,
        };
        let line = missing.line("/home/u/.local/bin/py", &script).unwrap();
        assert!(
            line.contains("py's #! interpreter is not on this system"),
            "{line}"
        );
        let denied = NotStarted::Other("Permission denied (os error 13)".to_owned());
        for _ in 0..2 {
            assert_eq!(
                missing.line("/bin/locked", &denied).as_deref(),
                Some("hyprix: /bin/locked did not start: Permission denied (os error 13)")
            );
        }
    }

    #[test]
    fn an_unclosed_quote_or_nothing_is_refused() {
        assert!(words("echo 'a").is_err());
        assert!(words("echo \"a").is_err());
        assert!(words("echo \"a\\").is_err());
        assert!(words("  ").is_err());
        assert!(words("''").is_ok());
    }
}
