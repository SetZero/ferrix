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
//! `sh -c '...'`. Chrome's `--user-agent` is what asked for it.

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

#[cfg(test)]
mod tests {
    use super::words;

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
    fn an_unclosed_quote_or_nothing_is_refused() {
        assert!(words("echo 'a").is_err());
        assert!(words("echo \"a").is_err());
        assert!(words("echo \"a\\").is_err());
        assert!(words("  ").is_err());
        assert!(words("''").is_ok());
    }
}
