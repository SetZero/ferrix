//! The tests of `Condition…=` and `Assert…=` (§4.3), which look at the
//! machine and so are the backend's.
//!
//! The manager asks whether a test passed and applies `!` and `|` itself.
//! A test Ferrix has nothing to answer with -- a virtualisation it does not
//! detect, a host name it does not have -- fails, so a plain condition skips
//! its unit and a negated one holds.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use ferrix_svc::Probe;
use ferrix_svc::unit::{Condition, Test};

/// The machine, as the conditions see it.
#[derive(Debug)]
pub(crate) struct Machine {
    /// The kernel command line, when `/proc/cmdline` has it.
    pub(crate) command_line: String,
}

impl Probe for Machine {
    fn test(&mut self, condition: &Condition) -> bool {
        let argument = condition.argument.as_str();
        let path = Path::new(argument);
        match condition.test {
            Test::PathExists => path.exists(),
            Test::PathIsDirectory => path.is_dir(),
            Test::PathIsSymbolicLink => {
                fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
            }
            Test::PathIsReadWrite => {
                fs::metadata(path).is_ok_and(|meta| !meta.permissions().readonly())
            }
            Test::DirectoryNotEmpty => {
                fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
            }
            Test::FileNotEmpty => {
                fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
            }
            Test::FileIsExecutable => fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0),
            Test::KernelCommandLine => self.command_line.split_whitespace().any(|word| {
                word == argument
                    || (!argument.contains('=')
                        && word.split_once('=').is_some_and(|(key, _)| key == argument))
            }),
            Test::Environment => std::env::vars()
                .any(|(name, value)| argument == name || argument == format!("{name}={value}")),
            Test::Architecture => argument == architecture(),
            _ => false,
        }
    }
}

/// This machine's architecture, in systemd's words.
fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86-64",
        "aarch64" => "arm64",
        "arm" => "arm",
        other => other,
    }
}
