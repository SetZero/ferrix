//! The account `authd` accepted, as `/etc/passwd` and `/etc/group` say it.

/// What `login` needs of an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Account {
    pub(crate) name: String,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) home: String,
    pub(crate) shell: String,
    /// Its supplementary groups: its own gid, and every group naming it.
    pub(crate) groups: Vec<u32>,
}

impl Account {
    /// Whether its shell lets it log in: not empty, `nologin` or `false`.
    pub(crate) fn may_log_in(&self) -> bool {
        let base = self.shell.rsplit('/').next().unwrap_or_default();
        self.shell.starts_with('/') && !matches!(base, "" | "nologin" | "false")
    }
}

/// Whether `name` could be an account's: POSIX's portable characters, not
/// starting with `-`, at most 32 bytes.
pub(crate) fn is_a_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Whether getty's `TERM` is one to keep: a plain name, as terminfo's are,
/// at most 64 bytes (the certification consultant's F7).
pub(crate) fn is_a_term(term: &str) -> bool {
    !term.is_empty()
        && term.len() <= 64
        && term
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// `name`'s account, from the two files.
pub(crate) fn find(name: &str, passwd: &str, group: &str) -> Option<Account> {
    let passwd = std::fs::read_to_string(passwd).ok()?;
    let group = std::fs::read_to_string(group).unwrap_or_default();
    parse(name, &passwd, &group)
}

/// `name`'s account in the files' text.
pub(crate) fn parse(name: &str, passwd: &str, group: &str) -> Option<Account> {
    let (uid, gid, home, shell) = passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let [user, _, uid, gid, _, home, shell] = fields.as_slice() else {
            return None;
        };
        (*user == name).then(|| {
            Some((
                uid.parse::<u32>().ok()?,
                gid.parse::<u32>().ok()?,
                (*home).to_owned(),
                (*shell).to_owned(),
            ))
        })?
    })?;
    let mut groups = vec![gid];
    for line in group.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        let [_, _, id, members] = fields.as_slice() else {
            continue;
        };
        let Ok(id) = id.parse::<u32>() else {
            continue;
        };
        if members.split(',').any(|member| member == name) && !groups.contains(&id) {
            groups.push(id);
        }
    }
    Some(Account {
        name: name.to_owned(),
        uid,
        gid,
        home,
        shell,
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/:/bin/sh\nferrix:x:1000:1000:ferrix:/home/ferrix:/bin/zsh\nshut:x:1002:1002::/:/sbin/nologin\n";
    const GROUP: &str = "root:x:0:\nferrix:x:1000:\naudio:x:29:ferrix,other\nvideo:x:44:other\n";

    #[test]
    fn an_account_has_its_ids_home_shell_and_groups() {
        let ferrix = parse("ferrix", PASSWD, GROUP);
        assert_eq!(
            ferrix,
            Some(Account {
                name: "ferrix".to_owned(),
                uid: 1000,
                gid: 1000,
                home: "/home/ferrix".to_owned(),
                shell: "/bin/zsh".to_owned(),
                groups: vec![1000, 29],
            })
        );
        assert!(ferrix.is_some_and(|account| account.may_log_in()));
        assert!(parse("shut", PASSWD, GROUP).is_some_and(|account| !account.may_log_in()));
        assert_eq!(parse("nobody", PASSWD, GROUP), None);
    }

    #[test]
    fn names_and_terms_are_plain() {
        assert!(is_a_name("ferrix"));
        for bad in ["", "-r", "a b", "a/b", "a:b", &"x".repeat(33)] {
            assert!(!is_a_name(bad), "{bad:?}");
        }
        assert!(is_a_term("xterm-256color"));
        assert!(is_a_term("linux"));
        for bad in ["", "a b", "x\u{1b}[2J", "a/b", &"x".repeat(65)] {
            assert!(!is_a_term(bad), "{bad:?}");
        }
    }
}
