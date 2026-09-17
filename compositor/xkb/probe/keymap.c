// Print the keymap and the tables the compositor needs, from libxkbcommon.
//
// Three things come out of this, and none of them may be written by hand:
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
//
// Needs a Linux host with libxkbcommon's development files and gcc. The rules
// are `evdev` with the `us` layout and no variant or options: Hyprland's own
// defaults (`input:kb_layout` defaults to empty, which libxkbcommon resolves
// to `us`).

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <xkbcommon/xkbcommon.h>

#ifndef XKBCOMMON_VERSION
#define XKBCOMMON_VERSION "unknown"
#endif

// The evdev keycode of an XKB one: XKB numbers keys from 8.
#define EVDEV_OFFSET 8

static const char *const MODIFIERS[] = {
    XKB_MOD_NAME_SHIFT, XKB_MOD_NAME_CAPS, XKB_MOD_NAME_CTRL,
    XKB_MOD_NAME_ALT,   XKB_MOD_NAME_NUM,  "Mod3",
    XKB_MOD_NAME_LOGO,  "Mod5",
};

int main(void)
{
    struct xkb_context *context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    if (!context) {
        fprintf(stderr, "no xkb context\n");
        return 1;
    }
    struct xkb_rule_names names = {
        .rules = "evdev", .model = "pc105", .layout = "us",
        .variant = NULL, .options = NULL,
    };
    struct xkb_keymap *keymap =
        xkb_keymap_new_from_names(context, &names, XKB_KEYMAP_COMPILE_NO_FLAGS);
    if (!keymap) {
        fprintf(stderr, "no keymap for evdev/pc105/us\n");
        return 1;
    }

    // The version comes from the build, not from a runtime call:
    // libxkbcommon has no `xkb_get_version`.
    printf("# libxkbcommon %s, rules evdev, model pc105, layout us\n",
           XKBCOMMON_VERSION);

    // The modifier indices. A modifier the keymap does not declare prints as
    // `-`, which the reader must treat as a mask of zero rather than as bit 0.
    printf("## modifiers\n");
    for (size_t at = 0; at < sizeof MODIFIERS / sizeof *MODIFIERS; at++) {
        xkb_mod_index_t index = xkb_keymap_mod_get_index(keymap, MODIFIERS[at]);
        if (index == XKB_MOD_INVALID) {
            printf("%s -\n", MODIFIERS[at]);
        } else {
            printf("%s %u\n", MODIFIERS[at], index);
        }
    }

    // Every key the keymap names: the keysyms it makes unshifted and shifted,
    // the modifiers it makes depressed while it is held, and the modifiers it
    // leaves locked once it has been pressed and released.
    //
    // The last two are asked of libxkbcommon's own state machine rather than
    // worked out from the key's name, so that which key is a modifier and
    // which is a lock is a property of the keymap here as it is there.
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

        struct xkb_state *state = xkb_state_new(keymap);
        if (!state) {
            fprintf(stderr, "no xkb state\n");
            return 1;
        }
        xkb_state_update_key(state, code, XKB_KEY_DOWN);
        xkb_mod_mask_t held =
            xkb_state_serialize_mods(state, XKB_STATE_MODS_DEPRESSED);
        xkb_state_update_key(state, code, XKB_KEY_UP);
        xkb_mod_mask_t locked =
            xkb_state_serialize_mods(state, XKB_STATE_MODS_LOCKED);
        xkb_state_unref(state);

        if (strcmp(plain, "-") == 0 && strcmp(shifted, "-") == 0 && held == 0 &&
            locked == 0) {
            continue;
        }
        printf("%u %s %s %s %#x %#x\n", (unsigned)(code - EVDEV_OFFSET), name,
               plain, shifted, held, locked);
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
