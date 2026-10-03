//! The two hyprlock boots (`docs/AUTH.md` P1.5): `hyprlock`, where a lock
//! taken with `L` refuses a wrong password through `authd` and lets the
//! right one through, and `hyprlock-unset`, where with no password set
//! hyprlock refuses to lock at all (decision 4).
//!
//! Both run `/bin/hyprlock` against a real `authd` in the image, as a
//! desktop does; the session is root in these boots, as every judged boot's
//! is, so the password is root's.

use std::path::Path;

use super::boot::{Wanted, boot_and_dump_carrying};
use super::{Carried, Programs, build};
use crate::args::Args;
use crate::paths::{self, Arch};
use crate::{Error, Result};

/// The hyprlock boot's configuration: the two windows, the German layout
/// the customer types on, and the key that runs hyprlock.
const HYPRLOCK_CONFIG: &str = "\
# Carried into the initramfs by `cargo xtask test-compositor`.
input:kb_layout = de
input:kb_variant = nodeadkeys
exec-once = /bin/pattern checkerboard one
exec-once = /bin/pattern gradient two --after one
bind = , L, exec, /bin/hyprlock -c /etc/hypr/hyprlock.conf
";

/// Root's password in the hyprlock boot's image: seeded into `authd`'s store
/// by this boot alone. Typed on a German keyboard, its last key is the one
/// an American keyboard calls Y, so it only matches if the layout is German.
const HYPRLOCK_PASSWORD: &str = "gatez";

/// What the hyprlock boot's screen must show, in order.
const HYPRLOCK_EXPECTED: [(&str, &str); 5] = [
    (
        "tiled",
        "src/user/system/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "locked by hyprlock, its field empty",
        "src/user/system/linux/compositor/hyprlock/tests/data/hyprlock-locked.xrle",
    ),
    (
        "five characters typed, five dots",
        "src/user/system/linux/compositor/hyprlock/tests/data/hyprlock-dots.xrle",
    ),
    (
        "a wrong password refused, the field in fail_color",
        "src/user/system/linux/compositor/hyprlock/tests/data/hyprlock-failed.xrle",
    ),
    (
        "the windows again, once the right password let the lock go",
        "src/user/system/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
];

/// The keys between them. Each is pressed as a chord and let go in reverse,
/// which types the letters in order; no letter twice in one, since a key
/// already held does not go down again. `y` is where a German keyboard has
/// `z`, so the password `gatez` only matches if the layout is German.
const HYPRLOCK_BINDS: [(&str, &[&str]); 4] = [
    ("L, which runs hyprlock", &["l"]),
    ("w r o n g", &["w", "r", "o", "n", "g"]),
    ("Return", &["ret"]),
    (
        "g a t e z and Return, on a German keyboard",
        &["g", "a", "t", "e", "y", "ret"],
    ),
];

/// A boot of hyprlock on the customer's layout: lock, a wrong password and
/// its failure, the right one, unlock.
///
/// It runs `/bin/hyprlock` as a desktop does, against `authd` with a
/// password for root seeded into the image (`docs/AUTH.md` §5.3): the
/// session is root in phase 1 (decision 3), so root's password is what
/// unlocks it. The seed is the gate's own and only this boot carries it;
/// no other image gets a password it did not ask for. The configuration is
/// the crate's own test data; the pictures are drawn from it on the host by
/// `src/user/system/linux/compositor/hyprlock/tests/gate.rs`, with the same code, and
/// composited as hyprix composites a lock surface.
pub(super) fn test_hyprlock(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let carried = Carried {
        ports: hyprlock_files(arch, Some(HYPRLOCK_PASSWORD))?,
        ..Carried::none()
    };
    let (screens, said) = boot_and_dump_carrying(
        arch,
        programs,
        HYPRLOCK_CONFIG,
        (carried, None),
        &Wanted {
            states: &HYPRLOCK_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["hyprlock: unlocked"],
        },
        &HYPRLOCK_BINDS,
        args,
    )?;
    if screens.len() != HYPRLOCK_EXPECTED.len() {
        return Err(Error::new(format!(
            "{arch}: {} of {} pictures were taken",
            screens.len(),
            HYPRLOCK_EXPECTED.len()
        )));
    }
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    for wanted in [
        "hyprix: the session is locked",
        "hyprlock: locked",
        // authd's audit lines: the refusal and the acceptance were its.
        "service=hyprlock account=root",
        "result=failed",
        "hyprlock: Authentication failed",
        "result=accepted",
        "hyprlock: authenticated",
        "hyprlock: unlocked",
        "hyprix: the session is unlocked",
    ] {
        if !has(wanted) {
            return Err(Error::new(format!(
                "{arch}: the hyprlock boot did not say `{wanted}`"
            )));
        }
    }
    println!(
        "  {arch}: hyprlock locked the screen, authd refused a wrong password and hyprlock showed \
         its fail colour, authd took the right one typed on a German keyboard, and the screen \
         came back"
    );
    Ok(())
}

/// The second hyprlock boot: the same image with no password seeded, where
/// `L` must not lock (`docs/AUTH.md` §5.4, decision 4) and hyprlock must say
/// why.
pub(super) fn test_hyprlock_unset(arch: Arch, programs: &Programs, args: &Args) -> Result<()> {
    let carried = Carried {
        ports: hyprlock_files(arch, None)?,
        ..Carried::none()
    };
    let (_, said) = boot_and_dump_carrying(
        arch,
        programs,
        HYPRLOCK_CONFIG,
        (carried, None),
        &Wanted {
            states: &HYPRLOCK_UNSET_EXPECTED,
            others: &[],
            moving: None,
            pointer: None,
            awaiting: &["not locking"],
        },
        &HYPRLOCK_UNSET_BINDS,
        args,
    )?;
    let has = |wanted: &str| said.iter().any(|line| line.contains(wanted));
    if !has("not locking: no password is set for root: run `passwd` first") {
        return Err(Error::new(format!(
            "{arch}: hyprlock with no password did not say why it did not lock"
        )));
    }
    if has("hyprix: the session is locked") {
        return Err(Error::new(format!(
            "{arch}: hyprlock locked an account with no password"
        )));
    }
    println!("  {arch}: with no password set, hyprlock did not lock, and said why");
    Ok(())
}

/// What the no-password boot must show: the windows, before and after `L`.
const HYPRLOCK_UNSET_EXPECTED: [(&str, &str); 2] = [
    (
        "tiled",
        "src/user/system/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
    (
        "still tiled after L, since nothing could unlock a lock",
        "src/user/system/linux/compositor/render/tests/data/dwindle-two-clients.xrle",
    ),
];

/// Its one key.
const HYPRLOCK_UNSET_BINDS: [(&str, &[&str]); 1] = [("L, which runs hyprlock", &["l"])];

/// What both hyprlock boots carry: hyprlock and its configuration, authd,
/// root's and authd's accounts, the font, and root's password where one is
/// given.
fn hyprlock_files(arch: Arch, password: Option<&str>) -> Result<Vec<crate::ports::File>> {
    let data = paths::workspace_root().join("src/user/system/linux/compositor/hyprlock/tests/data");
    let read = |path: &Path| -> Result<Vec<u8>> {
        std::fs::read(path)
            .map_err(|error| Error::new(format!("reading {}: {error}", path.display())))
    };
    let file = |path: &str, mode: u32, bytes: Vec<u8>| crate::ports::File {
        path: path.to_owned(),
        mode,
        content: crate::ports::Content::Bytes(bytes),
    };
    let fonts = paths::workspace_root().join("assets/fonts/liberation");
    let hyprlock = build(arch, "compositor-hyprlock", "hyprlock")?;
    let mut ports = crate::auth::carried(arch, None)?;
    if ports.is_empty() {
        return Err(Error::new(format!("{arch}: authd is not built for it")));
    }
    ports.extend([
        file("bin/hyprlock", 0o755, read(&hyprlock)?),
        file(
            "etc/hypr/hyprlock.conf",
            0o644,
            read(&data.join("gate.conf"))?,
        ),
        // Root, whom the lock is for, and authd's own account.
        file(
            "etc/passwd",
            0o644,
            format!("root:x:0:0:root:/:/bin/sh\n{}", crate::auth::PASSWD_LINE).into_bytes(),
        ),
        file(
            "etc/group",
            0o644,
            format!("root:x:0:\n{}", crate::auth::GROUP_LINE).into_bytes(),
        ),
        file(
            "usr/share/ferrix/fonts/LiberationSans-Regular.ttf",
            0o644,
            read(&fonts.join("LiberationSans-Regular.ttf"))?,
        ),
    ]);
    if let Some(password) = password {
        ports.push(crate::auth::seed("root", password)?);
    }
    Ok(ports)
}
