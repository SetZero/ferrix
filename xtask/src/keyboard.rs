//! The keyboard layout of the machine `run-compositor` runs on, which is the
//! desktop's layout when `--layout` and `--variant` say nothing.
//!
//! QEMU's window passes on where a key is, not what it means: the guest's
//! keymap decides that. So a person on a German keyboard who boots the
//! desktop without `--layout de` types an American keyboard's letters -- `z`
//! for `y`, and the hyphen where the `ß` is -- in the terminal and in Chrome
//! alike. They have said which keyboard they have once already, to their
//! own system, and this reads it from there.
//!
//! Two files say it, both written by the system's own tools: Debian's
//! `/etc/default/keyboard` (`dpkg-reconfigure keyboard-configuration`,
//! and what Ubuntu's installer writes), and systemd's `/etc/vconsole.conf`
//! (`localectl set-x11-keymap` keeps `XKBLAYOUT` there on Arch, Fedora and
//! the rest). Both are shell assignments. Neither exists on Windows or macOS,
//! and there the desktop keeps the compositor's default, as it always did.

/// The files that say the layout, in the order they are asked.
const FILES: [&str; 2] = ["/etc/default/keyboard", "/etc/vconsole.conf"];

/// This machine's layout and variant, as `input:kb_layout` and
/// `input:kb_variant` take them, and the file that said so. `None` when no
/// file names a layout.
pub(crate) fn host() -> Option<(String, Option<String>, &'static str)> {
    FILES.iter().find_map(|&path| {
        let text = std::fs::read_to_string(path).ok()?;
        let (layout, variant) = parse(&text)?;
        Some((layout, variant, path))
    })
}

/// `XKBLAYOUT` and `XKBVARIANT` from a file of shell assignments, with the
/// quotes a shell would take off taken off. An empty variant is none.
fn parse(text: &str) -> Option<(String, Option<String>)> {
    let value = |name: &str| {
        text.lines().rev().find_map(|line| {
            let rest = line.trim().strip_prefix(name)?.strip_prefix('=')?;
            let rest = rest.trim();
            let unquoted = rest
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .or_else(|| {
                    rest.strip_prefix('\'')
                        .and_then(|inner| inner.strip_suffix('\''))
                })
                .unwrap_or(rest);
            Some(unquoted.trim().to_owned())
        })
    };
    let layout = value("XKBLAYOUT").filter(|layout| !layout.is_empty())?;
    let variant = value("XKBVARIANT").filter(|variant| !variant.is_empty());
    Some((layout, variant))
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn debians_file_names_the_layout_and_its_variant() {
        let text = "# KEYBOARD CONFIGURATION FILE\n\nXKBMODEL=\"pc105\"\n\
                    XKBLAYOUT=\"de\"\nXKBVARIANT=\"nodeadkeys\"\nXKBOPTIONS=\"\"\n\n\
                    BACKSPACE=\"guess\"\n";
        assert_eq!(
            parse(text),
            Some(("de".to_owned(), Some("nodeadkeys".to_owned())))
        );
    }

    #[test]
    fn an_empty_variant_is_none_and_a_file_without_a_layout_is_nothing() {
        assert_eq!(
            parse("KEYMAP=de-latin1\nXKBLAYOUT=de,us\nXKBVARIANT=\n"),
            Some(("de,us".to_owned(), None))
        );
        assert_eq!(parse("KEYMAP=us\nFONT=eurlatgr\n"), None);
        assert_eq!(parse("XKBLAYOUT=\"\"\n"), None);
    }
}
