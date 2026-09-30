//! Starting what was chosen: fuzzel's `application_execute`.
//!
//! The `Exec` line has two layers of escaping. The Desktop Entry
//! specification's string escapes (`\s`, `\n`, `\t`, `\r`, `\;`, `\\`) come
//! off first; then the line is split into arguments by the specification's
//! quoting rules, as fuzzel's `tokenize_cmdline` splits it, and any argument
//! that starts with `%` -- a field code, `%u`, `%F` -- is dropped, since
//! fuzzel starts an application with no files. The program is then run
//! directly, not through a shell, with standard input on `/dev/null`, in the
//! entry's `Path`; fuzzel does not wait for it.

use std::process::{Command, Stdio};

use crate::desktop::Application;

/// Why a command line could not be run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A backslash before a character the specification does not escape.
    Escape(char),
    /// A quote that does not start an argument.
    Quoting,
    /// A quote that is never closed.
    Unterminated(char),
    /// Nothing left to run.
    Empty,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Escape(c) => write!(f, "invalid escaped exec argument character: {c}"),
            Self::Quoting => f.write_str(
                "command line contains non-specification-compliant quoting (arguments must be \
                 quoted in whole)",
            ),
            Self::Unterminated(q) => write!(
                f,
                "unterminated {} quote",
                if *q == '"' { "double" } else { "single" }
            ),
            Self::Empty => f.write_str("entry has no command to run"),
        }
    }
}

/// Take the specification's string escapes off `exec`.
///
/// # Errors
///
/// A backslash before any other character, as fuzzel refuses it.
pub fn unescape(exec: &str) -> Result<String, Refused> {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('s') => ' ',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some(';') => ';',
            Some('\\') => '\\',
            Some(other) => return Err(Refused::Escape(other)),
            None => return Err(Refused::Escape('\0')),
        });
    }
    Ok(out)
}

/// Split an unescaped command line into arguments, as fuzzel's
/// `tokenize_cmdline` does, dropping field codes.
///
/// # Errors
///
/// A quote in the middle of an argument, or one never closed.
pub fn tokenize(line: &str) -> Result<Vec<String>, Refused> {
    let mut chars: Vec<char> = line.chars().collect();
    let mut args = Vec::new();
    let mut push = |arg: String| {
        if !arg.starts_with('%') {
            args.push(arg);
        }
    };
    let mut p = 0;
    let mut start = 0;
    let mut open: Option<char> = None;
    while let Some(&c) = chars.get(p) {
        if c == '\\' {
            if open != Some('\'') {
                let next = chars.get(p + 1).copied();
                let escapes = (open != Some('"') && matches!(next, Some('\'' | ' ')))
                    || matches!(next, Some('$' | '"' | '`' | '\\'));
                if escapes {
                    let _ = chars.remove(p);
                }
            }
        } else if open.is_none() && (c == '\'' || c == '"') {
            if start != p {
                return Err(Refused::Quoting);
            }
            open = Some(c);
            start = p + 1;
        } else if Some(c) == open {
            open = None;
            push(chars.get(start..p).unwrap_or_default().iter().collect());
            start = p + 1;
        } else if c == ' ' && open.is_none() {
            if p > start {
                push(chars.get(start..p).unwrap_or_default().iter().collect());
            }
            start = p + 1;
        }
        p += 1;
    }
    if let Some(quote) = open {
        return Err(Refused::Unterminated(quote));
    }
    if p > start {
        push(chars.get(start..p).unwrap_or_default().iter().collect());
    }
    Ok(args)
}

/// What to run for `app` (or, with none, the typed `input`), with
/// `launch_prefix` in front: the arguments, the directory, and the
/// environment `launch-prefix` hands on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// The program and its arguments.
    pub argv: Vec<String>,
    /// Where to start it: `Path`.
    pub dir: Option<String>,
    /// `DESKTOP_ENTRY_*`, set only with a `launch-prefix`.
    pub env: Vec<(String, String)>,
    /// The line as fuzzel logs it: `executing <id>: "<line>"`.
    pub line: String,
}

/// Work out what executing `app` runs.
///
/// # Errors
///
/// What [`unescape`] and [`tokenize`] refuse, and an entry with no `Exec`.
pub fn plan(
    app: Option<&Application>,
    input: &str,
    launch_prefix: Option<&str>,
) -> Result<Launch, Refused> {
    let exec = match app {
        Some(app) => app.exec.clone().ok_or(Refused::Empty)?,
        None => input.to_owned(),
    };
    let mut env = Vec::new();
    let unescaped = unescape(&exec)?;
    let line = match launch_prefix {
        Some(prefix) => {
            if let Some(app) = app {
                let mut set = |key: &str, value: Option<&String>| {
                    if let Some(value) = value {
                        env.push((key.to_owned(), value.clone()));
                    }
                };
                set("DESKTOP_ENTRY_ID", app.id.as_ref());
                set("FUZZEL_DESKTOP_FILE_ID", app.id.as_ref());
                set("DESKTOP_ENTRY_PATH", app.desktop_file_path.as_ref());
                set("DESKTOP_ENTRY_ACTION", app.action_id.as_ref());
                set("DESKTOP_ENTRY_NAME", app.original_name.as_ref());
                set("DESKTOP_ENTRY_NAME_L", app.localized_name.as_ref());
                set("DESKTOP_ENTRY_COMMENT", app.comment_text.as_ref());
                set("DESKTOP_ENTRY_COMMENT_L", app.comment_text.as_ref());
                set("DESKTOP_ENTRY_ICON", app.icon_name.as_ref());
                set(
                    "DESKTOP_ENTRY_GENERICNAME",
                    app.original_generic_name.as_ref(),
                );
                set(
                    "DESKTOP_ENTRY_GENERICNAME_L",
                    app.localized_generic_name.as_ref(),
                );
                set("DESKTOP_ENTRY_ACTION_NAME", app.action_name.as_ref());
                set("DESKTOP_ENTRY_ACTION_NAME_L", app.action_name.as_ref());
                if app.action_id.is_some() {
                    set("DESKTOP_ENTRY_ACTION_ICON", app.icon_name.as_ref());
                }
            }
            format!("{prefix} {unescaped}")
        }
        None => unescaped,
    };
    let argv = tokenize(&line)?;
    if argv.is_empty() {
        return Err(Refused::Empty);
    }
    Ok(Launch {
        argv,
        dir: app.and_then(|app| app.path.clone()),
        env,
        line,
    })
}

/// Start `launch` and leave it running. Answers what fuzzel logs when it
/// could not: a directory that cannot be entered is said and the program
/// started where fuzzel is, as fuzzel's child does; a program that cannot be
/// executed is a failure.
///
/// # Errors
///
/// The program could not be executed; the text is fuzzel's
/// `<exec>: failed to execute: <why>`.
pub fn start(launch: &Launch, warnings: &mut Vec<String>) -> Result<(), String> {
    let Some((program, args)) = launch.argv.split_first() else {
        return Err("nothing to execute".to_owned());
    };
    let mut command = Command::new(program);
    let _ = command
        .args(args)
        .stdin(Stdio::null())
        .envs(launch.env.iter().cloned());
    if let Some(dir) = &launch.dir {
        if std::path::Path::new(dir).is_dir() {
            let _ = command.current_dir(dir);
        } else {
            warnings.push(format!(
                "failed to chdir to {dir}: No such file or directory (2)"
            ));
        }
    }
    command
        .spawn()
        .map(|_child| ())
        .map_err(|error| format!("{}: failed to execute: {}", launch.line, strerror(&error)))
}

/// `strerror` alone: an error's text without Rust's `(os error N)`.
pub(crate) fn error_text(error: &std::io::Error) -> String {
    let text = error.to_string();
    text.rsplit_once(" (os error ")
        .map_or(text.as_str(), |(before, _)| before)
        .to_owned()
}

/// An error as fuzzel's `LOG_ERRNO_P` prints one: `strerror`, then the
/// number in brackets.
pub(crate) fn strerror(error: &std::io::Error) -> String {
    let text = error_text(error);
    match error.raw_os_error() {
        Some(number) => format!("{text} ({number})"),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::{Refused, plan, tokenize, unescape};
    use crate::desktop::Application;

    fn words(line: &str) -> Vec<String> {
        tokenize(line).unwrap_or_default()
    }

    #[test]
    fn field_codes_and_quotes() {
        assert_eq!(words("firefox %u"), vec!["firefox"]);
        assert_eq!(
            words("\"/opt/My App/run\" --flag %F"),
            vec!["/opt/My App/run", "--flag"]
        );
        assert_eq!(words("sh -c 'echo  hi'"), vec!["sh", "-c", "echo  hi"]);
        assert_eq!(words("a\\ b c"), vec!["a b", "c"]);
        assert_eq!(
            words("echo \"a \\\"b\\\" \\$x\""),
            vec!["echo", "a \"b\" $x"]
        );
        assert_eq!(words("  spaced   out  "), vec!["spaced", "out"]);
        assert_eq!(tokenize("foo\"bar\""), Err(Refused::Quoting));
        assert_eq!(tokenize("sh -c 'x"), Err(Refused::Unterminated('\'')));
    }

    #[test]
    fn string_escapes_come_off_first() {
        assert_eq!(unescape("a\\sb\\\\c\\;"), Ok("a b\\c;".to_owned()));
        assert_eq!(unescape("bad\\q"), Err(Refused::Escape('q')));
        // `\\"` in the file is a `\"` for the tokenizer.
        let line = unescape("echo \"say \\\\\"hi\\\\\"\"").unwrap_or_default();
        assert_eq!(words(&line), vec!["echo", "say \"hi\""]);
    }

    #[test]
    fn a_plan() {
        let app = Application {
            id: Some("term.desktop".to_owned()),
            exec: Some("/bin/term /bin/zinc".to_owned()),
            path: Some("/".to_owned()),
            ..Application::default()
        };
        let launch = plan(Some(&app), "", None).unwrap_or_else(|_| unreachable_plan());
        assert_eq!(launch.argv, vec!["/bin/term", "/bin/zinc"]);
        assert_eq!(launch.dir.as_deref(), Some("/"));
        assert!(launch.env.is_empty());
        let prefixed =
            plan(Some(&app), "", Some("runapp --")).unwrap_or_else(|_| unreachable_plan());
        assert_eq!(
            prefixed.argv,
            vec!["runapp", "--", "/bin/term", "/bin/zinc"]
        );
        assert!(
            prefixed
                .env
                .contains(&("DESKTOP_ENTRY_ID".to_owned(), "term.desktop".to_owned()))
        );
        // With nothing selected, what was typed is the command.
        let typed = plan(None, "echo hi", None).unwrap_or_else(|_| unreachable_plan());
        assert_eq!(typed.argv, vec!["echo", "hi"]);
        assert_eq!(plan(None, "%u", None), Err(Refused::Empty));
    }

    #[test]
    fn a_missing_program_is_said_as_fuzzel_says_it() {
        let launch = plan(None, "surely-not-a-program-here arg", None)
            .unwrap_or_else(|_| unreachable_plan());
        let mut warnings = Vec::new();
        assert_eq!(
            super::start(&launch, &mut warnings),
            Err(
                "surely-not-a-program-here arg: failed to execute: No such file or directory (2)"
                    .to_owned()
            )
        );
    }

    fn unreachable_plan() -> super::Launch {
        super::Launch {
            argv: Vec::new(),
            dir: None,
            env: Vec::new(),
            line: String::new(),
        }
    }
}
