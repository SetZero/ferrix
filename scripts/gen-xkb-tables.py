#!/usr/bin/env python3
"""Turn libxkbcommon's own keymaps into the tables the compositor's seat reads.

A `wl_keyboard` hands its client a keymap and then sends raw keycodes; the
client compiles the keymap with libxkbcommon and reads every letter out of it.
So the keymap has to be a real one, and the bits in `wl_keyboard.modifiers`
have to be the ones that keymap declares -- a modifier index is a property of
the keymap, not a constant.

`compositor/xkb/probe/keymap-*.txt` is what libxkbcommon says, one file a
layout, printed by a committed probe (`probe/keymap.c`, `probe/keymap.sh`) on
a host that has it. This turns each into two committed files: the keymap text
the compositor sends, and a Rust table of every key's name and its keysyms.

A layout the compositor does not ship is not a layout it can make up:
libxkbcommon compiles a keymap out of the XKB data files, which are tens of
megabytes of a desktop distribution and are not on a machine running Ferrix.
So the layouts are chosen here, generated on a host that has the data, and
committed.

None of it may be written by hand, and none of it is generated from whatever
libxkbcommon the builder happens to have: the probe's output is committed, so
a change to a keymap is a change to a file somebody reviewed.

Usage:
    python3 scripts/gen-xkb-tables.py           # write the files
    python3 scripts/gen-xkb-tables.py --check   # fail if they are stale
"""

import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBES = ROOT / "compositor" / "xkb" / "probe"
OUT = ROOT / "compositor" / "xkb" / "src"
KEYMAPS = OUT / "keymaps"
TABLES = OUT / "generated"


def layouts():
    """Every layout the probe wrote a file for, by its stem, in name order.

    `us` first, because it is the default and the fallback and the rest are
    read against it.
    """
    found = sorted(path.stem[len("keymap-") :] for path in PROBES.glob("keymap-*.txt"))
    if "us" not in found:
        raise SystemExit(f"{PROBES} has no keymap-us.txt; run probe/keymap.sh")
    return ["us"] + [stem for stem in found if stem != "us"]


def read_probe(probe):
    """The probe's three sections: its banner, the modifiers, the keys, the keymap."""
    text = probe.read_text(encoding="utf-8")
    lines = text.split("\n")
    if not lines or not lines[0].startswith("# "):
        raise SystemExit(f"{probe} has no banner; run probe/keymap.sh")
    banner = lines[0][2:].strip()

    sections = {}
    name = None
    for line in lines[1:]:
        if line.startswith("## "):
            name = line[3:].split()[0]
            sections[name] = []
            continue
        if name is not None:
            sections[name].append(line)

    for wanted in ("modifiers", "keys", "keymap"):
        if wanted not in sections:
            raise SystemExit(f"{probe} has no `## {wanted}` section")

    modifiers = []
    for line in sections["modifiers"]:
        if not line.strip():
            continue
        label, index = line.split()
        modifiers.append((label, None if index == "-" else int(index)))

    keys = []
    for line in sections["keys"]:
        if not line.strip():
            continue
        code, key, plain, shifted, held, locked = line.split()
        keys.append(
            (
                int(code),
                key,
                None if plain == "-" else plain,
                None if shifted == "-" else shifted,
                int(held, 0),
                int(locked, 0),
            )
        )

    # The keymap is the rest of the file verbatim, including its blank lines.
    # The probe prints its length so a truncated file is caught here rather
    # than by a client that cannot compile it.
    declared = None
    for line in lines:
        if line.startswith("## keymap "):
            declared = int(line.split()[2])
    keymap = "\n".join(sections["keymap"])
    if keymap.endswith("\n"):
        keymap = keymap[:-1]
    keymap += "\n"
    if declared is not None and len(keymap) != declared:
        raise SystemExit(
            f"{probe}'s keymap is {len(keymap)} bytes, not the {declared} it declares"
        )
    return banner, modifiers, keys, keymap


def escaped(text):
    """`text` as a Rust string literal's contents."""
    return text.replace("\\", "\\\\").replace('"', '\\"')


MODIFIER_NAMES = {
    "Shift": "SHIFT",
    "Lock": "LOCK",
    "Control": "CONTROL",
    "Mod1": "MOD1",
    "Mod2": "MOD2",
    "Mod3": "MOD3",
    "Mod4": "MOD4",
    "Mod5": "MOD5",
}


def module(stem):
    """The Rust module name for a layout's file stem: `de-nodeadkeys` is not one."""
    return stem.replace("-", "_")


def modifier_bits(modifiers):
    """Each modifier's own name and mask, by the constant's name."""
    return {
        MODIFIER_NAMES.get(label, label.upper()): (
            label,
            0 if index is None else 1 << index,
        )
        for label, index in modifiers
    }


def tables(stem, banner, keys):
    """One layout's table of keys."""
    out = []
    add = out.append
    add(f"""// @generated by scripts/gen-xkb-tables.py from
// compositor/xkb/probe/keymap-{stem}.txt. Do not edit: run the probe and then
//
//     python3 scripts/gen-xkb-tables.py
//
// which `cargo xtask check` verifies.

//! What each key of one keymap is.
//!
//! From {escaped(banner)}.

use crate::Key;
""")

    add("/// The keymap these tables came from, as the probe named it.")
    add(f'pub const SOURCE: &str = "{escaped(banner)}";\n')

    add("""/// Every key the keymap names, in order of its evdev code.
///
/// `held` is the modifiers pressing it makes depressed and `locked` the ones
/// it leaves locked once it has been pressed and released, both asked of
/// libxkbcommon's own state machine rather than worked out from the key's
/// name.
///
/// A `static` rather than a `const`: a const this size would be copied into
/// every place that read it.""")
    add(f"pub static KEYS: [Key; {len(keys)}] = [")
    for code, key, plain, shifted, held, locked in keys:
        plain_text = "None" if plain is None else f'Some("{escaped(plain)}")'
        shifted_text = "None" if shifted is None else f'Some("{escaped(shifted)}")'
        add(
            f'    Key {{ code: {code}, name: "{escaped(key)}", '
            f"plain: {plain_text}, shifted: {shifted_text}, "
            f"held: {held:#x}, locked: {locked:#x} }},"
        )
    add("];")
    return "\n".join(out) + "\n"


def index(stems, banners, bits):
    """The module that names every layout and the bits they share."""
    out = []
    add = out.append
    add("""// @generated by scripts/gen-xkb-tables.py from
// compositor/xkb/probe/keymap-*.txt. Do not edit: run the probe and then
//
//     python3 scripts/gen-xkb-tables.py
//
// which `cargo xtask check` verifies.

//! The keymaps the compositor ships, and the modifier bits they share.
//!
//! One module a layout, each with the keys that layout makes; the text a
//! client is handed is beside them in `src/keymaps/`. Which one a person
//! gets is `input:kb_layout` and its neighbours, through
//! [`crate::layout`].
//!
//! # The modifier bits are shared, and checked
//!
//! A modifier's bit in `wl_keyboard.modifiers` is a property of the keymap
//! rather than of the protocol, so in principle each layout could number
//! them differently. Every layout here is the `evdev` rules with the same
//! model, and every one of them numbers `Shift`, `Lock`, `Control` and
//! `Mod1` to `Mod5` the same way -- so the constants are declared once, and
//! the generator refuses to write this file if any layout disagrees.

use crate::Key;
""")
    for name, (label, mask) in bits.items():
        add(f"/// `{label}`, as every keymap here numbers it.")
        add(f"pub const {name}: u32 = {mask:#x};\n")
    for stem in stems:
        add(f"/// {escaped(banners[stem])}.")
        add(f"pub mod {module(stem)};")
        add("")
    add("""/// One keymap the compositor can hand a client.""")
    add("#[derive(Clone, Copy, Debug)]")
    add("pub struct Layout {")
    add("    /// What `input:kb_layout` calls it, such as `de`.")
    add("    pub name: &\'static str,")
    add("    /// What `input:kb_variant` calls it, or empty for the plain one.")
    add("    pub variant: &\'static str,")
    add("    /// The probe\'s own line, which names the libxkbcommon it came from.")
    add("    pub source: &\'static str,")
    add("    /// The text a client compiles, which `wl_keyboard.keymap` carries.")
    add("    pub keymap: &\'static str,")
    add("    /// Every key it names, in order of evdev code.")
    add("    pub keys: &\'static [Key],")
    add("}")
    add("")
    add("/// Every keymap the compositor ships, `us` first.")
    add(f"pub static LAYOUTS: [Layout; {len(stems)}] = [")
    for stem in stems:
        name, _, variant = stem.partition("-")
        add("    Layout {")
        add(f'        name: "{name}",')
        add(f'        variant: "{variant}",')
        add(f'        source: {module(stem)}::SOURCE,')
        add(
            '        keymap: include_str!("../keymaps/'
            f'{stem}.xkb"),'
        )
        add(f"        keys: &{module(stem)}::KEYS,")
        add("    },")
    add("];")
    return "\n".join(out) + "\n"


def formatted(text, beside=None):
    """`text` through rustfmt, so the file reads like the rest of the tree.

    `beside` is a directory to format in, for a file whose `mod` declarations
    rustfmt would otherwise fail to resolve: it follows them, so the index
    module has to be formatted where its children are.
    """
    # The file is written and closed before rustfmt is handed its name, and
    # closed before it is deleted. Windows refuses to unlink a file that is
    # still open, so doing either inside the `with` fails there with
    # `WinError 32` and the generator cannot be run on that host at all --
    # which is where `cargo xtask check`'s xkb step was failing.
    handle = tempfile.NamedTemporaryFile(
        "w", suffix=".rs", dir=beside, delete=False, encoding="utf-8", newline="\n"
    )
    written = pathlib.Path(handle.name)
    try:
        with handle:
            handle.write(text)
        result = subprocess.run(
            ["rustfmt", "--edition", "2024", "--emit", "files", str(written)],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            raise SystemExit(f"rustfmt refused the generated code:\n{result.stderr}")
        return written.read_text(encoding="utf-8")
    finally:
        # `beside` is the generated directory itself, and a temporary file
        # left there is a module the crate would try to compile.
        written.unlink(missing_ok=True)
        written.with_suffix(".rs.bk").unlink(missing_ok=True)


def main():
    check = "--check" in sys.argv[1:]
    stems = layouts()
    wanted = {}
    banners = {}
    shared = None
    for stem in stems:
        probe = PROBES / f"keymap-{stem}.txt"
        banner, modifiers, keys, keymap = read_probe(probe)
        bits = modifier_bits(modifiers)
        if shared is None:
            shared = bits
        elif bits != shared:
            raise SystemExit(
                f"{probe} numbers its modifiers differently from keymap-us.txt; "
                "the shared constants in generated/mod.rs would be wrong"
            )
        banners[stem] = banner
        wanted[KEYMAPS / f"{stem}.xkb"] = keymap
        wanted[TABLES / f"{module(stem)}.rs"] = formatted(tables(stem, banner, keys))
    # The index names the child modules, and rustfmt follows a `mod`
    # declaration: it has to be formatted where the children are, so they are
    # written first.
    for path, text in wanted.items():
        if path.suffix != ".rs":
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.is_file() or path.read_text(encoding="utf-8") != text:
            path.write_text(text, encoding="utf-8")
    TABLES.mkdir(parents=True, exist_ok=True)
    wanted[TABLES / "mod.rs"] = formatted(index(stems, banners, shared), beside=TABLES)

    for path in (KEYMAPS, TABLES):
        path.mkdir(parents=True, exist_ok=True)
    stale = [
        path
        for path, text in wanted.items()
        if not path.is_file() or path.read_text(encoding="utf-8") != text
    ]
    if check:
        if stale:
            for path in stale:
                print(f"{path.relative_to(ROOT)} is stale", file=sys.stderr)
            print("run `python3 scripts/gen-xkb-tables.py`", file=sys.stderr)
            return 1
        print(f"gen-xkb-tables: {len(stems)} layouts current: {', '.join(stems)}")
        return 0

    OUT.mkdir(parents=True, exist_ok=True)
    for path, text in wanted.items():
        path.write_text(text, encoding="utf-8")
    print(f"gen-xkb-tables: wrote {len(stems)} layouts: {', '.join(stems)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
