//! `getty-generator DIR`: a getty for every console (`docs/INIT.md` §8.1).
//!
//! For each `console=` on the kernel command line it writes the link
//! `DIR/multi-user.target.wants/getty@NAME.service`, the name without the
//! option's speed (`console=ttyS0,115200` is `ttyS0`); with none, or with no
//! `/proc/cmdline` to read, it writes one for `console`. Only the link's
//! name is read, so a link that cannot be made is written as an empty file
//! of that name instead.

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::symlink;
use std::path::Path;

/// The template every link points at.
const TEMPLATE: &str = "/lib/ferrix/units/getty@.service";

/// The terminals the command line names, in its order, without repeats.
fn consoles(command_line: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for word in command_line.split_whitespace() {
        let Some(value) = word.strip_prefix("console=") else {
            continue;
        };
        let name = value.split(',').next().unwrap_or(value);
        let name = name.strip_prefix("/dev/").unwrap_or(name);
        if !name.is_empty() && !name.contains('/') && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }
    if names.is_empty() {
        names.push("console".to_owned());
    }
    names
}

/// Write the links into `directory`.
fn generate(directory: &Path, command_line: &str) -> io::Result<()> {
    let wants = directory.join("multi-user.target.wants");
    fs::create_dir_all(&wants)?;
    for name in consoles(command_line) {
        let link = wants.join(format!("getty@{name}.service"));
        if symlink(TEMPLATE, &link).is_err() {
            drop(fs::File::create(&link)?);
        }
    }
    Ok(())
}

fn main() {
    let Some(directory) = std::env::args_os().nth(1) else {
        let _ = writeln!(io::stderr(), "usage: getty-generator DIR");
        std::process::exit(2);
    };
    let command_line = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    if let Err(error) = generate(Path::new(&directory), &command_line) {
        let _ = writeln!(io::stderr(), "getty-generator: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::consoles;

    #[test]
    fn a_console_option_names_a_getty_without_its_speed() {
        assert_eq!(
            consoles("ro console=ttyS0,115200n8 console=tty0 console=ttyS0"),
            ["ttyS0", "tty0"]
        );
    }

    #[test]
    fn no_console_option_is_the_console() {
        assert_eq!(consoles("ferrix.init=/sbin/init"), ["console"]);
        assert_eq!(consoles(""), ["console"]);
    }

    #[test]
    fn a_path_is_its_name_under_dev() {
        assert_eq!(consoles("console=/dev/ttyAMA0"), ["ttyAMA0"]);
    }
}
