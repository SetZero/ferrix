package dev.ferrix.launcher

import android.view.KeyCharacterMap
import android.view.KeyEvent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Linux's input-event-codes.h, which virtio-input uses as they are: the event
 * types, the keys this file sends, and the pointer's buttons and axes.
 */
internal object Linux {
    const val EV_SYN = 0
    const val EV_KEY = 1
    const val EV_REL = 2
    const val EV_ABS = 3
    const val SYN_REPORT = 0

    const val KEY_ESC = 1
    const val KEY_MINUS = 12
    const val KEY_EQUAL = 13
    const val KEY_BACKSPACE = 14
    const val KEY_TAB = 15
    const val KEY_LEFTBRACE = 26
    const val KEY_RIGHTBRACE = 27
    const val KEY_ENTER = 28
    const val KEY_LEFTCTRL = 29
    const val KEY_SEMICOLON = 39
    const val KEY_APOSTROPHE = 40
    const val KEY_GRAVE = 41
    const val KEY_LEFTSHIFT = 42
    const val KEY_BACKSLASH = 43
    const val KEY_COMMA = 51
    const val KEY_DOT = 52
    const val KEY_SLASH = 53
    const val KEY_RIGHTSHIFT = 54
    const val KEY_KPASTERISK = 55
    const val KEY_LEFTALT = 56
    const val KEY_SPACE = 57
    const val KEY_CAPSLOCK = 58
    const val KEY_F1 = 59
    const val KEY_NUMLOCK = 69
    const val KEY_SCROLLLOCK = 70
    const val KEY_KPMINUS = 74
    const val KEY_KPPLUS = 78
    const val KEY_KPDOT = 83
    const val KEY_F11 = 87
    const val KEY_F12 = 88
    const val KEY_KPENTER = 96
    const val KEY_RIGHTCTRL = 97
    const val KEY_KPSLASH = 98
    const val KEY_SYSRQ = 99
    const val KEY_RIGHTALT = 100
    const val KEY_HOME = 102
    const val KEY_UP = 103
    const val KEY_PAGEUP = 104
    const val KEY_LEFT = 105
    const val KEY_RIGHT = 106
    const val KEY_END = 107
    const val KEY_DOWN = 108
    const val KEY_PAGEDOWN = 109
    const val KEY_INSERT = 110
    const val KEY_DELETE = 111
    const val KEY_PAUSE = 119
    const val KEY_LEFTMETA = 125
    const val KEY_RIGHTMETA = 126
    const val KEY_COMPOSE = 127

    const val BTN_LEFT = 0x110
    const val BTN_RIGHT = 0x111
    const val BTN_MIDDLE = 0x112
    const val BTN_TOUCH = 0x14a

    const val REL_X = 0
    const val REL_Y = 1
    const val REL_WHEEL = 8

    const val ABS_X = 0
    const val ABS_Y = 1

    /** The letters' keys, a to z, in the alphabet's order: a US keyboard's rows. */
    val LETTERS = intArrayOf(
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50,
        49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,
    )

    /** The digits' keys, 0 to 9: KEY_0 is after KEY_9. */
    val DIGITS = intArrayOf(11, 2, 3, 4, 5, 6, 7, 8, 9, 10)
}

/** One virtio_input_event: type, code, value. */
internal typealias InputEvent = Triple<Int, Int, Int>

/**
 * The key a US keyboard types `char` with, and whether it takes Shift; null
 * for a character it has no key for. The guest's keymap is US, so this is
 * the whole of the translation from text to keys.
 */
internal fun usKey(char: Char): Pair<Int, Boolean>? = when (char) {
    in 'a'..'z' -> Linux.LETTERS[char - 'a'] to false
    in 'A'..'Z' -> Linux.LETTERS[char - 'A'] to true
    in '0'..'9' -> Linux.DIGITS[char - '0'] to false
    ' ' -> Linux.KEY_SPACE to false
    '\n' -> Linux.KEY_ENTER to false
    '\t' -> Linux.KEY_TAB to false
    else -> {
        val plain = "-=[];'`\\,./"
        val shifted = "_+{}:\"~|<>?"
        val keys = intArrayOf(
            Linux.KEY_MINUS, Linux.KEY_EQUAL, Linux.KEY_LEFTBRACE, Linux.KEY_RIGHTBRACE,
            Linux.KEY_SEMICOLON, Linux.KEY_APOSTROPHE, Linux.KEY_GRAVE, Linux.KEY_BACKSLASH,
            Linux.KEY_COMMA, Linux.KEY_DOT, Linux.KEY_SLASH,
        )
        // Shift and a digit: !@#$%^&*() over 1 to 9 and 0.
        val overDigits = ")!@#$%^&*("
        when {
            char in plain -> keys[plain.indexOf(char)] to false
            char in shifted -> keys[shifted.indexOf(char)] to true
            char in overDigits -> Linux.DIGITS[overDigits.indexOf(char)] to true
            else -> null
        }
    }
}

/**
 * Android's key code for a key as Linux's, for the keys a keyboard has;
 * null for the rest (Back, volume, media), which stay Android's.
 */
internal fun linuxKey(keyCode: Int): Int? = when (keyCode) {
    in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z -> Linux.LETTERS[keyCode - KeyEvent.KEYCODE_A]
    in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9 -> Linux.DIGITS[keyCode - KeyEvent.KEYCODE_0]
    in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F10 -> Linux.KEY_F1 + keyCode - KeyEvent.KEYCODE_F1
    KeyEvent.KEYCODE_F11 -> Linux.KEY_F11
    KeyEvent.KEYCODE_F12 -> Linux.KEY_F12
    // KEY_KP1 to KEY_KP9 are three rows of three, from the bottom.
    KeyEvent.KEYCODE_NUMPAD_0 -> 82
    in KeyEvent.KEYCODE_NUMPAD_1..KeyEvent.KEYCODE_NUMPAD_9 ->
        intArrayOf(79, 80, 81, 75, 76, 77, 71, 72, 73)[keyCode - KeyEvent.KEYCODE_NUMPAD_1]
    KeyEvent.KEYCODE_NUMPAD_DIVIDE -> Linux.KEY_KPSLASH
    KeyEvent.KEYCODE_NUMPAD_MULTIPLY -> Linux.KEY_KPASTERISK
    KeyEvent.KEYCODE_NUMPAD_SUBTRACT -> Linux.KEY_KPMINUS
    KeyEvent.KEYCODE_NUMPAD_ADD -> Linux.KEY_KPPLUS
    KeyEvent.KEYCODE_NUMPAD_DOT -> Linux.KEY_KPDOT
    KeyEvent.KEYCODE_NUMPAD_ENTER -> Linux.KEY_KPENTER
    KeyEvent.KEYCODE_ESCAPE -> Linux.KEY_ESC
    KeyEvent.KEYCODE_MINUS -> Linux.KEY_MINUS
    KeyEvent.KEYCODE_EQUALS -> Linux.KEY_EQUAL
    KeyEvent.KEYCODE_DEL -> Linux.KEY_BACKSPACE
    KeyEvent.KEYCODE_TAB -> Linux.KEY_TAB
    KeyEvent.KEYCODE_LEFT_BRACKET -> Linux.KEY_LEFTBRACE
    KeyEvent.KEYCODE_RIGHT_BRACKET -> Linux.KEY_RIGHTBRACE
    KeyEvent.KEYCODE_ENTER -> Linux.KEY_ENTER
    KeyEvent.KEYCODE_CTRL_LEFT -> Linux.KEY_LEFTCTRL
    KeyEvent.KEYCODE_SEMICOLON -> Linux.KEY_SEMICOLON
    KeyEvent.KEYCODE_APOSTROPHE -> Linux.KEY_APOSTROPHE
    KeyEvent.KEYCODE_GRAVE -> Linux.KEY_GRAVE
    KeyEvent.KEYCODE_SHIFT_LEFT -> Linux.KEY_LEFTSHIFT
    KeyEvent.KEYCODE_BACKSLASH -> Linux.KEY_BACKSLASH
    KeyEvent.KEYCODE_COMMA -> Linux.KEY_COMMA
    KeyEvent.KEYCODE_PERIOD -> Linux.KEY_DOT
    KeyEvent.KEYCODE_SLASH -> Linux.KEY_SLASH
    KeyEvent.KEYCODE_SHIFT_RIGHT -> Linux.KEY_RIGHTSHIFT
    KeyEvent.KEYCODE_ALT_LEFT -> Linux.KEY_LEFTALT
    KeyEvent.KEYCODE_SPACE -> Linux.KEY_SPACE
    KeyEvent.KEYCODE_CAPS_LOCK -> Linux.KEY_CAPSLOCK
    KeyEvent.KEYCODE_NUM_LOCK -> Linux.KEY_NUMLOCK
    KeyEvent.KEYCODE_SCROLL_LOCK -> Linux.KEY_SCROLLLOCK
    KeyEvent.KEYCODE_CTRL_RIGHT -> Linux.KEY_RIGHTCTRL
    KeyEvent.KEYCODE_SYSRQ -> Linux.KEY_SYSRQ
    KeyEvent.KEYCODE_ALT_RIGHT -> Linux.KEY_RIGHTALT
    KeyEvent.KEYCODE_MOVE_HOME -> Linux.KEY_HOME
    KeyEvent.KEYCODE_DPAD_UP -> Linux.KEY_UP
    KeyEvent.KEYCODE_PAGE_UP -> Linux.KEY_PAGEUP
    KeyEvent.KEYCODE_DPAD_LEFT -> Linux.KEY_LEFT
    KeyEvent.KEYCODE_DPAD_RIGHT -> Linux.KEY_RIGHT
    KeyEvent.KEYCODE_MOVE_END -> Linux.KEY_END
    KeyEvent.KEYCODE_DPAD_DOWN -> Linux.KEY_DOWN
    KeyEvent.KEYCODE_PAGE_DOWN -> Linux.KEY_PAGEDOWN
    KeyEvent.KEYCODE_INSERT -> Linux.KEY_INSERT
    KeyEvent.KEYCODE_FORWARD_DEL -> Linux.KEY_DELETE
    KeyEvent.KEYCODE_BREAK -> Linux.KEY_PAUSE
    KeyEvent.KEYCODE_META_LEFT -> Linux.KEY_LEFTMETA
    KeyEvent.KEYCODE_META_RIGHT -> Linux.KEY_RIGHTMETA
    KeyEvent.KEYCODE_MENU -> Linux.KEY_COMPOSE
    else -> null
}

/**
 * The guest's keyboard: text and keys from the soft keyboard, the extra-keys
 * row and a hardware keyboard, sent to the guest's virtio keyboard as key
 * presses and releases through [send].
 *
 * Ctrl, Alt and Super on the extra-keys row are sticky ([Sticky]): armed,
 * they are held around the next key the soft keyboard or the row types, the
 * way Termux's row does. A hardware keyboard has modifiers of its own, and
 * its keys go through as they are pressed and released.
 */
internal class Keyboard(private val send: (List<InputEvent>) -> Unit) {
    enum class Sticky { OFF, ONCE, LOCKED }

    /** A modifier on the row, and when it was last tapped, which tells a double tap. */
    inner class StickyKey(val key: Int) {
        var state by mutableStateOf(Sticky.OFF)
        private var tapped = 0L

        /** Off to once; once again soon after, locked; otherwise off. */
        fun tap() {
            val now = System.currentTimeMillis()
            state = when (state) {
                Sticky.OFF -> Sticky.ONCE
                Sticky.ONCE -> if (now - tapped < DOUBLE_TAP_MS) Sticky.LOCKED else Sticky.OFF
                Sticky.LOCKED -> Sticky.OFF
            }
            tapped = now
        }
    }

    val ctrl = StickyKey(Linux.KEY_LEFTCTRL)
    val alt = StickyKey(Linux.KEY_LEFTALT)
    val meta = StickyKey(Linux.KEY_LEFTMETA)
    private val sticky = listOf(ctrl, alt, meta)

    /**
     * One key pressed and released, with Shift if `shift`, Ctrl if `ctrl`, and
     * the armed modifiers; the ones armed once are spent.
     */
    fun tap(key: Int, shift: Boolean = false, ctrl: Boolean = false) {
        val held = buildList {
            sticky.filter { it.state != Sticky.OFF }.forEach { add(it.key) }
            if (ctrl && Linux.KEY_LEFTCTRL !in this) add(Linux.KEY_LEFTCTRL)
            if (shift) add(Linux.KEY_LEFTSHIFT)
        }
        sticky.filter { it.state == Sticky.ONCE }.forEach { it.state = Sticky.OFF }
        val events = buildList {
            for (modifier in held) add(InputEvent(Linux.EV_KEY, modifier, 1))
            add(InputEvent(Linux.EV_KEY, key, 1))
            add(InputEvent(Linux.EV_SYN, Linux.SYN_REPORT, 0))
            add(InputEvent(Linux.EV_KEY, key, 0))
            for (modifier in held.asReversed()) add(InputEvent(Linux.EV_KEY, modifier, 0))
            add(InputEvent(Linux.EV_SYN, Linux.SYN_REPORT, 0))
        }
        send(events)
    }

    /** Text, a key at a time; a character the US keymap has no key for is dropped. */
    fun text(text: CharSequence) {
        for (char in text) usKey(char)?.let { (key, shift) -> tap(key, shift) }
    }

    /**
     * A key event from the soft keyboard or a hardware one, while the guest's
     * screen is shown. True when the guest took it; false leaves it to Android.
     */
    fun key(event: KeyEvent): Boolean {
        val soft = event.deviceId == KeyCharacterMap.VIRTUAL_KEYBOARD ||
            event.flags and KeyEvent.FLAG_SOFT_KEYBOARD != 0
        if (event.action == KeyEvent.ACTION_MULTIPLE && event.keyCode == KeyEvent.KEYCODE_UNKNOWN) {
            event.characters?.let { text(it) }
            return true
        }
        val key = linuxKey(event.keyCode)
        if (soft) {
            // The soft keyboard's keys come whole: each press is a tap.
            if (key == null) {
                val char = event.unicodeChar
                if (char == 0) return false
                if (event.action == KeyEvent.ACTION_DOWN) text(char.toChar().toString())
                return true
            }
            if (event.action == KeyEvent.ACTION_DOWN) tap(key, event.isShiftPressed, event.isCtrlPressed)
            return true
        }
        if (key == null) return false
        // A held key's repeats are the guest's to make.
        if (event.action == KeyEvent.ACTION_DOWN && event.repeatCount > 0) return true
        val value = when (event.action) {
            KeyEvent.ACTION_DOWN -> 1
            KeyEvent.ACTION_UP -> 0
            else -> return true
        }
        send(listOf(InputEvent(Linux.EV_KEY, key, value), InputEvent(Linux.EV_SYN, Linux.SYN_REPORT, 0)))
        return true
    }

    private companion object {
        const val DOUBLE_TAP_MS = 400
    }
}
