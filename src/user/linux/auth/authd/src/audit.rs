//! The audit log (`docs/AUTH.md` §3.6): one line per conversation's end and
//! per credential change, on standard output for init's per-unit log and
//! appended with `fsync` to `/var/log/ferrix/auth.log`.
//!
//! A line never holds a secret, its length or a hash. What it holds is
//! chosen by the caller of [`Audit::line`] from fixed words and names that
//! the protocol's alphabets already limit, so a line cannot be forged into
//! two by a name with a newline in it.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// The size past which the log is rotated once, to `auth.log.1`.
const ROTATE_AT: u64 = 1024 * 1024;

/// Where audit lines go.
#[derive(Debug)]
pub(crate) struct Audit {
    path: PathBuf,
    file: Option<File>,
}

impl Audit {
    /// A log at `path`, opened when first written.
    pub(crate) fn at(path: &Path) -> Audit {
        Audit {
            path: path.to_owned(),
            file: None,
        }
    }

    /// Write one line: `auth: ` and the fields, space separated.
    pub(crate) fn line(&mut self, fields: &[(&str, &str)]) {
        let mut text = String::from("auth:");
        for (key, value) in fields {
            let value: String = value
                .chars()
                .map(|c| {
                    if c.is_whitespace() || c.is_control() {
                        '_'
                    } else {
                        c
                    }
                })
                .collect();
            text.push_str(&format!(" {key}={value}"));
        }
        say(&text);
        if let Err(error) = self.append(&text) {
            say(&format!(
                "authd: the audit log {} could not be written: {error}",
                self.path.display()
            ));
        }
    }

    fn append(&mut self, text: &str) -> io::Result<()> {
        if std::fs::metadata(&self.path).is_ok_and(|meta| meta.len() > ROTATE_AT) {
            self.file = None;
            std::fs::rename(&self.path, self.path.with_extension("log.1"))?;
        }
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .append(true)
                    .create(true)
                    .mode(0o600)
                    .open(&self.path)?,
            );
        }
        let file = self.file.as_mut().ok_or(io::ErrorKind::NotFound)?;
        writeln!(file, "{text}")?;
        file.sync_data()
    }
}

/// One line on standard output, which init keeps in the unit's log.
pub(crate) fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
