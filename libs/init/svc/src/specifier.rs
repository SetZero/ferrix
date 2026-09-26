//! Specifiers: `%i` and its siblings, as `systemd.unit(5)` lists them.
//!
//! Every value of a unit file is expanded once, when the unit is loaded, so
//! that `getty@.service`'s `ExecStart=/sbin/getty %i` reads
//! `/sbin/getty console` in `getty@console.service`. The ones kept are those
//! a unit name answers, and the fixed directories of a system manager; the
//! ones that need the machine (`%H`, `%m`, `%u` and the rest) are refused by
//! name, and the assignment carrying one is dropped with a warning, which is
//! what systemd does with a specifier it cannot resolve.

use alloc::string::String;
use core::fmt;

use crate::UnitName;
use crate::name::unescape;

/// A `%` that could not be expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecifierError {
    /// A specifier this manager does not have.
    Unknown(char),
    /// A `%` at the very end.
    Dangling,
}

impl fmt::Display for SpecifierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpecifierError::Unknown(c) => write!(f, "unknown or unsupported specifier '%{c}'"),
            SpecifierError::Dangling => f.write_str("a '%' at the end of the value"),
        }
    }
}

/// `%j`: the part of the prefix after its last `-`.
fn last_dash(prefix: &str) -> &str {
    prefix.rsplit_once('-').map_or(prefix, |(_, last)| last)
}

/// Expand every specifier in `text` for the unit `name`.
///
/// | | Is |
/// |---|---|
/// | `%n` | the full name, `getty@console.service` |
/// | `%N` | the name without its suffix, `getty@console` |
/// | `%p` / `%P` | the prefix, `getty`, as written / unescaped |
/// | `%i` / `%I` | the instance, `console`, as written / unescaped; empty for a plain unit |
/// | `%j` / `%J` | the prefix after its last `-`, as written / unescaped |
/// | `%f` | `/` and the unescaped instance, or the unescaped prefix for a plain unit |
/// | `%t`, `%S`, `%C`, `%L`, `%E` | `/run`, `/var/lib`, `/var/cache`, `/var/log`, `/etc` |
/// | `%%` | a `%` |
///
/// # Errors
///
/// A specifier not in the table, or a `%` that ends the text.
pub fn expand(text: &str, name: &UnitName) -> Result<String, SpecifierError> {
    if !text.contains('%') {
        return Ok(String::from(text));
    }
    let instance = name.instance().unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let specifier = chars.next().ok_or(SpecifierError::Dangling)?;
        match specifier {
            '%' => out.push('%'),
            'n' => out.push_str(name.as_str()),
            'N' => out.push_str(name.stem()),
            'p' => out.push_str(name.prefix()),
            'P' => out.push_str(&unescape(name.prefix())),
            'i' => out.push_str(instance),
            'I' => out.push_str(&unescape(instance)),
            'j' => out.push_str(last_dash(name.prefix())),
            'J' => out.push_str(&unescape(last_dash(name.prefix()))),
            'f' => {
                let what = if name.is_instance() {
                    instance
                } else {
                    name.prefix()
                };
                let unescaped = unescape(what);
                if !unescaped.starts_with('/') {
                    out.push('/');
                }
                out.push_str(&unescaped);
            }
            't' => out.push_str("/run"),
            'S' => out.push_str("/var/lib"),
            'C' => out.push_str("/var/cache"),
            'L' => out.push_str("/var/log"),
            'E' => out.push_str("/etc"),
            other => return Err(SpecifierError::Unknown(other)),
        }
    }
    Ok(out)
}
