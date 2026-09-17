//! Being Ferrix's first program.
//!
//! The kernel starts init the way it starts a shell (`kernel/src/init.rs`):
//! `sh -i` when nothing was built in, and `sh -c <script>` when something
//! was. A program that is init therefore receives a shell's arguments
//! whether or not it is a shell, and `-i` is not an option any compositor
//! program has.
//!
//! Both programs that run as init -- `hyprix` under `xtask test-compositor`
//! and `evecho` under `xtask test-input` -- need the same rule, so it is
//! written once, here in the lowest crate they share.

/// The arguments a shell would have been given, as the program's own.
///
/// `-i` alone means "nothing was asked for", so the program's own defaults.
/// `-c <words>` means "run this", and what a program run as init runs is
/// itself, so the words become its arguments.
///
/// A script holding a newline is one argument a line, because an argument
/// may hold a space: `--exec /bin/pattern checkerboard one` is one argument
/// and four words. A script on one line is split on whitespace, which is
/// what somebody typing one means.
#[must_use]
pub fn unshell(args: Vec<String>) -> Vec<String> {
    let words = |script: &String| -> Vec<String> {
        if script.contains('\n') {
            script
                .split('\n')
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        } else {
            script.split_whitespace().map(str::to_owned).collect()
        }
    };
    match args.split_first() {
        Some((first, rest)) if first == "-i" && rest.is_empty() => Vec::new(),
        Some((first, rest)) if first == "-c" => rest.iter().flat_map(words).collect(),
        _ => args,
    }
}

#[cfg(test)]
mod tests {
    use super::unshell;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn an_interactive_shells_arguments_are_no_arguments() {
        assert_eq!(unshell(owned(&["-i"])), Vec::<String>::new());
        assert_eq!(unshell(Vec::new()), Vec::<String>::new());
    }

    #[test]
    fn a_scripts_words_are_the_programs_arguments() {
        assert_eq!(
            unshell(owned(&["-c", "--headless 800x600"])),
            owned(&["--headless", "800x600"])
        );
    }

    /// A line may hold a space, so a script with newlines is split on them
    /// and on nothing else.
    #[test]
    fn a_script_with_lines_is_one_argument_a_line() {
        assert_eq!(
            unshell(owned(&["-c", "--exec\n/bin/pattern checkerboard one\n"])),
            owned(&["--exec", "/bin/pattern checkerboard one"])
        );
    }

    /// `-i` with something after it is not an interactive shell's line, and
    /// the arguments are the program's own.
    #[test]
    fn anything_else_is_passed_through() {
        assert_eq!(
            unshell(owned(&["/dev/input/event0"])),
            owned(&["/dev/input/event0"])
        );
        assert_eq!(unshell(owned(&["-i", "-v"])), owned(&["-i", "-v"]));
    }
}
