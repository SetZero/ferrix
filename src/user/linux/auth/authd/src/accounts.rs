//! Who the accounts are: `/etc/passwd`, read afresh at every use, so an
//! account added or renamed is seen at once (`docs/AUTH.md` §5.2).

use std::path::Path;

/// One `/etc/passwd` line's name, uid and gid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Account {
    /// The login name.
    pub(crate) name: String,
    /// The uid.
    pub(crate) uid: u32,
    /// The primary gid.
    pub(crate) gid: u32,
}

/// Every account in the file at `path`; none when it cannot be read. A line
/// that is not seven fields with numeric ids is skipped, as musl's `getpwent`
/// skips it.
pub(crate) fn all(path: &Path) -> Vec<Account> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().filter_map(parse).collect()
}

fn parse(line: &str) -> Option<Account> {
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() != 7 {
        return None;
    }
    let name = *fields.first()?;
    if name.is_empty() || name.starts_with(['+', '-', '#']) {
        return None;
    }
    Some(Account {
        name: name.to_owned(),
        uid: fields.get(2)?.parse().ok()?,
        gid: fields.get(3)?.parse().ok()?,
    })
}

/// The account called `name`.
pub(crate) fn by_name(path: &Path, name: &str) -> Option<Account> {
    all(path).into_iter().find(|account| account.name == name)
}

/// The first account whose uid is `uid`, as `getpwuid` answers.
pub(crate) fn by_uid(path: &Path, uid: u32) -> Option<Account> {
    all(path).into_iter().find(|account| account.uid == uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_read_as_musl_reads_them() {
        assert_eq!(
            parse("ferrix:x:1000:1000:ferrix:/home/ferrix:/bin/zsh"),
            Some(Account {
                name: "ferrix".to_owned(),
                uid: 1000,
                gid: 1000,
            })
        );
        for bad in [
            "",
            "short:x:1",
            "no:x:uid:0:gecos:/:/bin/sh",
            "+nis::0:0:::",
            "a:x:1:1:g:/:/bin/sh:extra",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
