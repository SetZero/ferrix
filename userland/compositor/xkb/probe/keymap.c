// Print the keymap and the tables the compositor needs, from libxkbcommon.
//
// Four things come out of this, and none of them may be written by hand:
//
//   1. The keymap text a client is handed through `wl_keyboard.keymap`. A
//      client compiles it with libxkbcommon and reads every keysym out of
//      it, so a keymap that is nearly right is a keyboard that types nearly
//      the right letters.
//   2. The modifier indices that keymap gives, which decide the bits in
//      `wl_keyboard.modifiers`. They are a property of the keymap, not a
//      constant: a keymap that declares its modifiers in another order has
//      other bits.
//   3. Each evdev key's name and the keysyms it produces at the first two
//      shift levels, which is what a keybind is matched against.
//   4. Every shift level of every layout group of every key, with the
//      modifier masks that reach each level. A German keyboard types `@` at
//      level 2 of `AD01` and `~` at level 2 of `AD12`, both behind `Mod5`;
//      a compositor that knows only `plain` and `shifted` cannot produce
//      either, and one that knows the levels but guesses which modifiers
//      select them gets them wrong wherever the level is not behind the
//      modifier it assumed -- level 4 of `FK01` is `Control+Mod1`, not a
//      third or fourth shift of anything.
//
// Needs a Linux host with libxkbcommon's development files and gcc. The
// rules are `evdev`; the model, the layout and the variant are the three
// arguments, and default to `pc105`, `us` and none -- Hyprland's own
// defaults (`input:kb_layout` defaults to empty, which libxkbcommon
// resolves to `us`).
//
// One run prints one keymap, which is not the same as one layout: the layout
// argument is passed to libxkbcommon whole, so `de,us` compiles one keymap
// with two groups, exactly as `kb_layout = de,us` must. The variant argument
// is a comma-separated list the same length, or empty. A person who writes
// `kb_layout = de` gets the letters on their keyboard only if the compositor
// was given that keymap, so the compositor ships one file per layout it
// knows and `keymap.sh` runs this once for each.
//
//
// == The output format ==
//
// Line-oriented text, UTF-8, one record a line, committed and reviewed. A
// first line beginning `# ` is the banner; every section after it opens with
// a `## ` header whose first word is its name and whose remaining words are
// that section's own parameters. `-` in a field means "absent"; it never
// means zero. Modifier masks are written as the *names* of their modifiers
// rather than as numbers everywhere but in the older `## keys` section, so
// that a keymap which renumbers its modifiers shows up as a change to
// `## modifiers` alone and not as churn on 300 key lines.
// Sections appear in this order, `## keymap` always last:
//
//   # libxkbcommon <version>, rules <rules>, model <model>, layout <layout>,
//     variant <variant>            (one line; `-` for an empty variant)
//
//   ## modifiers
//   <name> <index>                 the eight real modifiers, in a fixed
//                                  order, with the index each has in this
//                                  keymap, or `-` for one the keymap does
//                                  not declare -- which is no bit and not
//                                  bit 0. The index is the bit number in
//                                  every mask `wl_keyboard.modifiers`
//                                  carries.
//
//   ## all-modifiers
//   <index> <name>                 every modifier the keymap declares, in
//                                  index order: the eight above and then the
//                                  virtual ones (`LevelThree`, `NumLock`,
//                                  `Alt`, ...), which the masks below name.
//                                  `-` for a declared modifier with no name.
//                                  This is the table that turns a mask name
//                                  back into a bit.
//
//   ## groups <count>
//   <group> <name>                 one line per layout group of the keymap,
//                                  `<group>` counting from 0 as
//                                  libxkbcommon and `wl_keyboard` do, and
//                                  `<name>` the layout's own name, or `-` if
//                                  it has none. **The name is the rest of the
//                                  line and has spaces in it** -- group 1 of
//                                  `de,us` is `English (US)`. It is the only
//                                  field in this file that does, and two
//                                  groups can carry the same name.
//
//   ## keys
//   <code> <name> <plain> <shifted> <held> <locked>
//                                  the older, narrower record, kept because
//                                  the generator still reads it: group 0's
//                                  levels 0 and 1 only, its `held`/`locked`
//                                  group 0's as well, and both as hexadecimal
//                                  masks. Every field of it appears again
//                                  below in the wider form. It is a subset,
//                                  not a summary: on a multi-group keymap it
//                                  describes the first layout and no other.
//
//   ## key-levels <mods-for-level|no-mods-for-level>
//   key <code> <name> <groups> <levels> <held> <locked>
//   level <code> <group> <level> <keysyms> <mods>
//                                  the whole of a key: one `key` record
//                                  followed by one `level` record for every
//                                  group of the keymap and every level of
//                                  that group, in that order, so that a key
//                                  and its levels diff together. A key that
//                                  defines fewer groups than the keymap has
//                                  still gets a record for each of them,
//                                  carrying what libxkbcommon answers once it
//                                  has brought the group back into range: the
//                                  rule for that is the key's own (`wrap`,
//                                  `clamp` or a redirect) and libxkbcommon
//                                  exposes none of it, so it is resolved here
//                                  and not guessed at by the reader. The
//                                  section's parameter says
//                                  whether the libxkbcommon it was built
//                                  against has
//                                  `xkb_keymap_key_get_mods_for_level`; with
//                                  `no-mods-for-level` every `mods` field is
//                                  `-` and the reader must not mistake that
//                                  for "no modifier reaches this level".
//
//                                  `key` fields:
//                                    <code>    evdev keycode, decimal (the
//                                              XKB keycode less 8).
//                                    <name>    the XKB key name, `AD01`.
//                                    <groups>  how many groups this key
//                                              defines, decimal. It is often
//                                              *fewer* than `## groups` says:
//                                              on a `de,us` keymap only the
//                                              47 keys the two layouts
//                                              disagree about define two, and
//                                              the function keys, the keypad
//                                              and the arrows define one.
//                                    <levels>  how many levels each group of
//                                              the *keymap* has for this key,
//                                              comma-separated, one number a
//                                              group (`4,2`), or `-` if the
//                                              key defines no group at all.
//                                              There are as many numbers as
//                                              `## groups` counts, whatever
//                                              `<groups>` says, and they are
//                                              exactly the `level` records
//                                              that follow. Groups differ in
//                                              depth.
//                                    <held>    the modifiers the key makes
//                                              depressed while it is held,
//                                              as names joined by `+`, or
//                                              `none` -- and one entry a
//                                              group of the keymap,
//                                              comma-separated like
//                                              `<levels>`, because a key's
//                                              actions are per group too. In
//                                              `us,de` this reads `Mod1,Mod5`
//                                              for `RALT`: an `Alt` in the
//                                              American group and an AltGr in
//                                              the German one.
//                                    <locked>  the modifiers it leaves
//                                              locked once pressed and
//                                              released, the same way.
//
//                                  `level` fields:
//                                    <code>     the same evdev keycode.
//                                    <group>    the group, counting from 0.
//                                    <level>    the level, counting from 0.
//                                               Level 0 is what the key types
//                                               with nothing held and level 1
//                                               is usually, but not always,
//                                               the shifted one.
//                                    <keysyms>  the keysym names this level
//                                               produces, joined by `+` when
//                                               there are several, or `-`
//                                               when there are none. A level
//                                               in range can still be empty.
//                                    <mods>     the modifier masks that
//                                               select this level: one or
//                                               more alternatives separated
//                                               by `,`, each a set of
//                                               modifier names joined by `+`,
//                                               or `none` for the empty mask,
//                                               or `-` when libxkbcommon
//                                               named no mask at all. So `+`
//                                               joins what must hold
//                                               together and `,` separates
//                                               what would each do. A bit no
//                                               modifier of the keymap claims
//                                               is printed as `0x<hex>`
//                                               rather than dropped.
//
//   ## keymap <bytes>
//   <the keymap text>              verbatim to the end of the file, `<bytes>`
//                                  long, so that a truncated file is caught
//                                  here and not by a client that cannot
//                                  compile it.
//
// A key with no keysym at any level, holding and locking nothing, is left
// out of both key sections. The two sections can therefore disagree about
// which keys exist: `## keys` looks at group 0 alone, so on a `de,us` keymap
// a key that only the second group defines is absent there and present in
// `## key-levels`. `## key-levels` is the one that has seen the whole keymap.
//
// An example, from `de,us` (elided):
//
//   # libxkbcommon 1.13.1, rules evdev, model pc105, layout de,us, variant -
//   ## modifiers
//   Shift 0
//   ...
//   Mod5 7
//   ## all-modifiers
//   0 Shift
//   ...
//   7 Mod5
//   8 NumLock
//   ...
//   ## groups 2
//   0 German
//   1 English (US)
//   ## keys
//   18 AD03 e E 0 0
//   ...
//   ## key-levels mods-for-level
//   key 18 AD03 2 4,2 none,none none,none
//   level 18 0 0 e none,Shift+Lock
//   level 18 0 1 E Shift,Lock
//   level 18 0 2 EuroSign Mod5,Lock+Mod5
//   level 18 0 3 EuroSign Shift+Mod5,Shift+Lock+Mod5
//   level 18 1 0 e none
//   level 18 1 1 E Shift,Lock
//   key 100 RALT 1 1,1 Mod5,Mod5 none,none
//   level 100 0 0 ISO_Level3_Shift none
//   level 100 1 0 ISO_Level3_Shift none
//   ...
//
// That last key is the whole of the argument for printing every group: `de`
// makes `RALT` an AltGr and `us` never mentions the key, so on `de,us` it is
// an AltGr in both groups -- and on `us,de` it reads
// `key 100 RALT 2 1,1 Mod1,Mod5 none,none`, an `Alt` in the first group and
// an AltGr in the second. The order of `kb_layout` decides it.
//
//
// == Four things a reader of `mods` must not assume ==
//
// `mods` is what libxkbcommon will say about a level, and that is less than a
// full inverse of the keyboard. Measured against 1.13.1, `evdev`/`pc105`:
//
//   1. **A mask that is listed nowhere types level 0.** XKB masks the active
//      modifiers with the ones the key's type declares and looks the rest up;
//      a combination with no entry falls to level 0. `Shift` held with `Lock`
//      locked types `e` on `AD03` in both groups of `de,us`, and the `de`
//      group lists `Shift+Lock` against level 0 while the `us` group lists
//      nothing at all -- the two groups give that key different types. So the
//      reader's lookup must end in "otherwise level 0", not in "otherwise
//      nothing".
//   2. **Modifiers the key ignores are in the masks.** `FK01` lists level 0
//      as `none,Control,Mod1` and level 1 as `Shift,Shift+Control,
//      Shift+Mod1,Shift+Control+Mod1`, because its type looks at `Control`
//      and `Mod1` for the sake of level 4 (`XF86Switch_VT_1`, `Control+Mod1`)
//      and they change nothing at the levels below. A lookup keyed on the
//      exact mask therefore needs every alternative, not the first.
//   3. **No mask ever names a virtual modifier.** libxkbcommon hands back
//      real-modifier masks, so level 2 of a German key reads `Mod5` and never
//      `LevelThree`, even though the keymap declares `LevelThree` at index 10
//      and binds it to `Mod5`. `## all-modifiers` is there for the day that
//      changes, and for reading `wl_keyboard.modifiers`.
//   4. **The masks are a set, and one of them can be empty.** `none` is a
//      real answer -- the level is typed with nothing held -- and `-` is the
//      other one, which means libxkbcommon named no way in at all. Across the
//      layouts here a level is reached up to four ways.
//
// And one about levels: a level that exists can be empty. `## keys` shows
// keycodes 196 to 199 as `- Alt_L`, `- Meta_L`, `- Super_L`, `- Hyper_L`:
// level 0 of each types nothing and level 1, with `Shift`, types a modifier
// keysym. A reader that treats a `-` keysym as the end of a key's levels
// stops one line early.

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <xkbcommon/xkbcommon.h>

#ifndef XKBCOMMON_VERSION
#define XKBCOMMON_VERSION "unknown"
#endif

// The evdev keycode of an XKB one: XKB numbers keys from 8.
#define EVDEV_OFFSET 8

// How many masks one level may be reached by before this probe gives up.
// `xkb_keymap_key_get_mods_for_level` silently returns only as many as the
// buffer holds, so a buffer that fills is a record that is quietly wrong;
// rather than commit that, the run fails and whoever hit it raises this.
#define MASKS_MAX 64

// Room for one field. A mask names up to a few dozen modifiers and a level
// can be reached several ways, so the mask field is the long one.
#define FIELD_MAX 4096

static const char *const MODIFIERS[] = {
    XKB_MOD_NAME_SHIFT, XKB_MOD_NAME_CAPS, XKB_MOD_NAME_CTRL,
    XKB_MOD_NAME_ALT,   XKB_MOD_NAME_NUM,  "Mod3",
    XKB_MOD_NAME_LOGO,  "Mod5",
};

#define MODIFIER_COUNT (sizeof MODIFIERS / sizeof *MODIFIERS)

// Append `text` to the string built in `out`, or fail rather than truncate.
static bool append(char *out, size_t size, size_t *at, const char *text)
{
    size_t length = strlen(text);
    if (*at + length + 1 > size) {
        return false;
    }
    memcpy(out + *at, text, length);
    *at += length;
    out[*at] = '\0';
    return true;
}

// `mask` as the names of its modifiers joined by `+`, or `none` when it is
// empty. A bit the keymap names no modifier for is printed as hex, because a
// mask that lost a bit here would read as a level reachable more easily than
// it is.
static bool mask_names(struct xkb_keymap *keymap, xkb_mod_mask_t mask,
                       char *out, size_t size)
{
    size_t at = 0;
    out[0] = '\0';
    if (mask == 0) {
        return append(out, size, &at, "none");
    }
    xkb_mod_index_t declared = xkb_keymap_num_mods(keymap);
    for (xkb_mod_index_t index = 0; index < declared && index < 32; index++) {
        xkb_mod_mask_t bit = UINT32_C(1) << index;
        if (!(mask & bit)) {
            continue;
        }
        const char *name = xkb_keymap_mod_get_name(keymap, index);
        if (!name) {
            continue;
        }
        if (at != 0 && !append(out, size, &at, "+")) {
            return false;
        }
        if (!append(out, size, &at, name)) {
            return false;
        }
        mask &= ~bit;
    }
    if (mask != 0) {
        char unnamed[16];
        snprintf(unnamed, sizeof unnamed, "0x%x", mask);
        if (at != 0 && !append(out, size, &at, "+")) {
            return false;
        }
        if (!append(out, size, &at, unnamed)) {
            return false;
        }
    }
    return true;
}

// The keysyms of one group and level, joined by `+`, or `-` for none.
static bool level_keysyms(struct xkb_keymap *keymap, xkb_keycode_t code,
                          xkb_layout_index_t group, xkb_level_index_t level,
                          char *out, size_t size)
{
    const xkb_keysym_t *syms = NULL;
    int count =
        xkb_keymap_key_get_syms_by_level(keymap, code, group, level, &syms);
    size_t at = 0;
    out[0] = '\0';
    if (count <= 0) {
        return append(out, size, &at, "-");
    }
    for (int index = 0; index < count; index++) {
        char name[128];
        if (xkb_keysym_get_name(syms[index], name, sizeof name) < 0) {
            snprintf(name, sizeof name, "0x%x", syms[index]);
        }
        if (at != 0 && !append(out, size, &at, "+")) {
            return false;
        }
        if (!append(out, size, &at, name)) {
            return false;
        }
    }
    return true;
}

// The masks that select one group and level, `,`-separated, or `-` when
// libxkbcommon named none -- which is what an unreachable level looks like.
//
// `HAVE_MODS_FOR_LEVEL` is settled by `keymap.sh` against the installed
// header rather than against a version number, because the header is what
// the compiler believes: the call arrived in libxkbcommon 1.0.0, and a host
// older than that still has to print the levels it can see.
static bool level_mods(struct xkb_keymap *keymap, xkb_keycode_t code,
                       xkb_layout_index_t group, xkb_level_index_t level,
                       char *out, size_t size)
{
    size_t at = 0;
    out[0] = '\0';
#ifdef HAVE_MODS_FOR_LEVEL
    xkb_mod_mask_t masks[MASKS_MAX];
    size_t count = xkb_keymap_key_get_mods_for_level(keymap, code, group, level,
                                                     masks, MASKS_MAX);
    if (count >= MASKS_MAX) {
        fprintf(stderr,
                "key %u group %u level %u is reached %zu ways or more; raise "
                "MASKS_MAX\n",
                (unsigned)(code - EVDEV_OFFSET), group, level, count);
        return false;
    }
    if (count == 0) {
        return append(out, size, &at, "-");
    }
    for (size_t index = 0; index < count; index++) {
        char names[FIELD_MAX];
        if (!mask_names(keymap, masks[index], names, sizeof names)) {
            return false;
        }
        if (at != 0 && !append(out, size, &at, ",")) {
            return false;
        }
        if (!append(out, size, &at, names)) {
            return false;
        }
    }
    return true;
#else
    (void)keymap;
    (void)code;
    (void)group;
    (void)level;
    return append(out, size, &at, "-");
#endif
}

// The modifiers a key makes depressed while it is held and leaves locked
// once it has been pressed and released, in one group.
//
// Both are asked of libxkbcommon's own state machine rather than worked out
// from the key's name, so that which key is a modifier and which is a lock is
// a property of the keymap here as it is there. And both are asked of a state
// put into the group first, because a key's actions are per group: in
// `us,de`, `RALT` is `Alt_R` holding `Mod1` in group 0 and
// `ISO_Level3_Shift` holding `Mod5` in group 1. Reading one group's answer
// for the whole keymap is how a second layout loses its AltGr.
static bool key_mods(struct xkb_keymap *keymap, xkb_keycode_t code,
                     xkb_layout_index_t group, xkb_mod_mask_t *held,
                     xkb_mod_mask_t *locked)
{
    struct xkb_state *state = xkb_state_new(keymap);
    if (!state) {
        fprintf(stderr, "no xkb state\n");
        return false;
    }
    if (group != 0) {
        xkb_state_update_mask(state, 0, 0, 0, group, 0, 0);
    }
    xkb_state_update_key(state, code, XKB_KEY_DOWN);
    *held = xkb_state_serialize_mods(state, XKB_STATE_MODS_DEPRESSED);
    xkb_state_update_key(state, code, XKB_KEY_UP);
    *locked = xkb_state_serialize_mods(state, XKB_STATE_MODS_LOCKED);
    xkb_state_unref(state);
    return true;
}

// Whether the key is worth a record: one that types nothing in any group and
// holds and locks nothing is left out of both key sections.
static bool interesting(struct xkb_keymap *keymap, xkb_keycode_t code,
                        bool holds)
{
    if (holds) {
        return true;
    }
    xkb_layout_index_t groups = xkb_keymap_num_layouts(keymap);
    for (xkb_layout_index_t group = 0; group < groups; group++) {
        xkb_level_index_t levels =
            xkb_keymap_num_levels_for_key(keymap, code, group);
        for (xkb_level_index_t level = 0; level < levels; level++) {
            const xkb_keysym_t *syms = NULL;
            if (xkb_keymap_key_get_syms_by_level(keymap, code, group, level,
                                                 &syms) > 0) {
                return true;
            }
        }
    }
    return false;
}

int main(int argc, char **argv)
{
    const char *model = argc > 1 ? argv[1] : "pc105";
    const char *layout = argc > 2 ? argv[2] : "us";
    const char *variant = (argc > 3 && argv[3][0]) ? argv[3] : NULL;
    struct xkb_context *context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    if (!context) {
        fprintf(stderr, "no xkb context\n");
        return 1;
    }
    struct xkb_rule_names names = {
        .rules = "evdev", .model = model, .layout = layout,
        .variant = variant, .options = NULL,
    };
    struct xkb_keymap *keymap =
        xkb_keymap_new_from_names(context, &names, XKB_KEYMAP_COMPILE_NO_FLAGS);
    if (!keymap) {
        fprintf(stderr, "no keymap for evdev/%s/%s/%s\n", model, layout,
                variant ? variant : "");
        return 1;
    }

    // The version comes from the build, not from a runtime call:
    // libxkbcommon has no `xkb_get_version`.
    printf("# libxkbcommon %s, rules evdev, model %s, layout %s, variant %s\n",
           XKBCOMMON_VERSION, model, layout, variant ? variant : "-");

    // The modifier indices. A modifier the keymap does not declare prints as
    // `-`, which the reader must treat as a mask of zero rather than as bit 0.
    printf("## modifiers\n");
    for (size_t at = 0; at < MODIFIER_COUNT; at++) {
        xkb_mod_index_t index = xkb_keymap_mod_get_index(keymap, MODIFIERS[at]);
        if (index == XKB_MOD_INVALID) {
            printf("%s -\n", MODIFIERS[at]);
        } else {
            printf("%s %u\n", MODIFIERS[at], index);
        }
    }

    // Every modifier the keymap declares, in index order, the eight real ones
    // above and the virtual ones after them. The virtual modifiers are here
    // because the masks that select a level name them: level 3 of a German
    // key is `LevelThree`, a virtual modifier bound to `Mod5`, and the mask
    // that reaches it is printed both ways.
    printf("## all-modifiers\n");
    xkb_mod_index_t declared = xkb_keymap_num_mods(keymap);
    for (xkb_mod_index_t index = 0; index < declared; index++) {
        const char *name = xkb_keymap_mod_get_name(keymap, index);
        printf("%u %s\n", index, name ? name : "-");
    }

    // The keymap's layout groups. `kb_layout = de,us` is two of them, and
    // which one is in force is state the compositor keeps and sends; the
    // names are here so that a reader can tell which is which.
    xkb_layout_index_t groups = xkb_keymap_num_layouts(keymap);
    printf("## groups %u\n", groups);
    for (xkb_layout_index_t group = 0; group < groups; group++) {
        const char *name = xkb_keymap_layout_get_name(keymap, group);
        printf("%u %s\n", group, name ? name : "-");
    }

    // The narrow key table, unchanged: group 0's first two levels and the
    // held and locked masks in hex. It is a subset of `## key-levels` and is
    // kept only until the generator reads the wider section instead.
    printf("## keys\n");
    xkb_mod_index_t shift = xkb_keymap_mod_get_index(keymap, XKB_MOD_NAME_SHIFT);
    for (xkb_keycode_t code = xkb_keymap_min_keycode(keymap);
         code <= xkb_keymap_max_keycode(keymap); code++) {
        const char *name = xkb_keymap_key_get_name(keymap, code);
        if (!name) {
            continue;
        }
        const xkb_keysym_t *syms = NULL;
        int count = xkb_keymap_key_get_syms_by_level(keymap, code, 0, 0, &syms);
        char plain[128] = "-";
        if (count > 0) {
            xkb_keysym_get_name(syms[0], plain, sizeof plain);
        }
        char shifted[128] = "-";
        if (shift != XKB_MOD_INVALID) {
            count = xkb_keymap_key_get_syms_by_level(keymap, code, 0, 1, &syms);
            if (count > 0) {
                xkb_keysym_get_name(syms[0], shifted, sizeof shifted);
            }
        }

        xkb_mod_mask_t held = 0;
        xkb_mod_mask_t locked = 0;
        if (!key_mods(keymap, code, 0, &held, &locked)) {
            return 1;
        }

        if (strcmp(plain, "-") == 0 && strcmp(shifted, "-") == 0 && held == 0 &&
            locked == 0) {
            continue;
        }
        printf("%u %s %s %s %#x %#x\n", (unsigned)(code - EVDEV_OFFSET), name,
               plain, shifted, held, locked);
    }

    // The whole of every key: all its groups, all their levels, the keysyms
    // at each and the modifiers that get there. This is the section a
    // compositor that means to type `@` and `~` reads.
    printf("## key-levels %s\n",
#ifdef HAVE_MODS_FOR_LEVEL
           "mods-for-level"
#else
           "no-mods-for-level"
#endif
    );
    for (xkb_keycode_t code = xkb_keymap_min_keycode(keymap);
         code <= xkb_keymap_max_keycode(keymap); code++) {
        const char *name = xkb_keymap_key_get_name(keymap, code);
        if (!name) {
            continue;
        }
        // How deep the key is and what it holds and locks, one entry a group
        // of the keymap, because all three are per group: the same key can be
        // four levels deep in one layout and two in the next, and `RALT` is
        // an `Alt` in one and an AltGr in the other.
        xkb_layout_index_t key_groups =
            xkb_keymap_num_layouts_for_key(keymap, code);
        char levels[FIELD_MAX] = "";
        char held_names[FIELD_MAX] = "";
        char locked_names[FIELD_MAX] = "";
        size_t levels_at = 0;
        size_t held_at = 0;
        size_t locked_at = 0;
        bool holds = false;
        for (xkb_layout_index_t group = 0; group < groups; group++) {
            xkb_mod_mask_t held = 0;
            xkb_mod_mask_t locked = 0;
            if (!key_mods(keymap, code, group, &held, &locked)) {
                return 1;
            }
            holds = holds || held != 0 || locked != 0;

            char depth[16];
            snprintf(depth, sizeof depth, "%u",
                     xkb_keymap_num_levels_for_key(keymap, code, group));
            char one_held[FIELD_MAX];
            char one_locked[FIELD_MAX];
            if (!mask_names(keymap, held, one_held, sizeof one_held) ||
                !mask_names(keymap, locked, one_locked, sizeof one_locked)) {
                fprintf(stderr, "key %u's modifiers do not fit a record\n",
                        (unsigned)(code - EVDEV_OFFSET));
                return 1;
            }
            if ((levels_at != 0 &&
                 !append(levels, sizeof levels, &levels_at, ",")) ||
                !append(levels, sizeof levels, &levels_at, depth) ||
                (held_at != 0 &&
                 !append(held_names, sizeof held_names, &held_at, ",")) ||
                !append(held_names, sizeof held_names, &held_at, one_held) ||
                (locked_at != 0 &&
                 !append(locked_names, sizeof locked_names, &locked_at, ",")) ||
                !append(locked_names, sizeof locked_names, &locked_at,
                        one_locked)) {
                fprintf(stderr, "key %u has more groups than fit a record\n",
                        (unsigned)(code - EVDEV_OFFSET));
                return 1;
            }
        }
        if (levels_at == 0) {
            append(levels, sizeof levels, &levels_at, "-");
            append(held_names, sizeof held_names, &held_at, "-");
            append(locked_names, sizeof locked_names, &locked_at, "-");
        }

        if (!interesting(keymap, code, holds)) {
            continue;
        }
        printf("key %u %s %u %s %s %s\n", (unsigned)(code - EVDEV_OFFSET), name,
               key_groups, levels, held_names, locked_names);

        // Every group of the *keymap*, not only the ones this key defines.
        // A key with fewer groups than the keymap still types something in
        // the others: XKB brings an out-of-range group back into range, and
        // by which rule is the key's own business -- `wrap`, `clamp` or a
        // redirect to a named group -- which libxkbcommon does not expose.
        // So the probe asks it for every group and prints the answer, and no
        // reader has to reimplement a rule it cannot see.
        for (xkb_layout_index_t group = 0; group < groups; group++) {
            xkb_level_index_t count =
                xkb_keymap_num_levels_for_key(keymap, code, group);
            for (xkb_level_index_t level = 0; level < count; level++) {
                char keysyms[FIELD_MAX];
                char mods[FIELD_MAX];
                if (!level_keysyms(keymap, code, group, level, keysyms,
                                   sizeof keysyms)) {
                    fprintf(stderr,
                            "key %u group %u level %u has more keysyms than "
                            "fit a record\n",
                            (unsigned)(code - EVDEV_OFFSET), group, level);
                    return 1;
                }
                if (!level_mods(keymap, code, group, level, mods,
                                sizeof mods)) {
                    return 1;
                }
                printf("level %u %u %u %s %s\n",
                       (unsigned)(code - EVDEV_OFFSET), group, level, keysyms,
                       mods);
            }
        }
    }

    // The keymap itself, last, because it is thousands of lines and a reader
    // looking for a number should not have to scroll past it.
    char *text = xkb_keymap_get_as_string(keymap, XKB_KEYMAP_FORMAT_TEXT_V1);
    if (!text) {
        fprintf(stderr, "the keymap would not print itself\n");
        return 1;
    }
    printf("## keymap %zu\n", strlen(text));
    fputs(text, stdout);
    free(text);
    xkb_keymap_unref(keymap);
    xkb_context_unref(context);
    return 0;
}
