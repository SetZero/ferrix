package dev.ferrix.launcher

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.net.Uri
import android.os.Binder
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Parcel
import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.graphics.PixelFormat
import android.text.InputType
import android.util.Log
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceControl
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.ViewConfiguration
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.io.FileInputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.math.abs
import kotlin.math.hypot
import kotlin.math.min
import kotlin.math.roundToInt

private const val TAG = "FerrixVm"

/** crosvm's display service: its AIDL interface and the transactions used here. */
private const val DISPLAY_SERVICE = "android.crosvm.ICrosvmAndroidDisplayService"
private const val SET_SURFACE = 1
private const val SET_CURSOR_STREAM = 2
private const val REMOVE_SURFACE = 3
private const val SAVE_FRAME = 4
private const val DRAW_SAVED_FRAME = 5

/** What the bridge hands over for a run: crosvm's display service and its own input binder. */
internal class Link(val display: IBinder, private val input: IBinder) {
    /**
     * Send `events` to one of the guest's input devices: [TOUCH_EVENTS],
     * [KEYBOARD_EVENTS] or [MOUSE_EVENTS]. One-way calls to one binder arrive
     * in the order they were made, the devices' among each other too.
     */
    fun send(device: Int, events: List<InputEvent>) {
        // virtio_input_event: le16 type, le16 code, le32 value.
        val bytes = ByteBuffer.allocate(8 * events.size).order(ByteOrder.LITTLE_ENDIAN)
        for ((type, code, value) in events) bytes.putShort(type.toShort()).putShort(code.toShort()).putInt(value)
        val data = Parcel.obtain()
        try {
            data.writeByteArray(bytes.array())
            input.transact(device, data, null, IBinder.FLAG_ONEWAY)
        } catch (error: Exception) {
            Log.w(TAG, "input events lost", error)
        } finally {
            data.recycle()
        }
    }
}

/**
 * What a finger on the guest's screen is: a finger on its touchscreen, where
 * it lands, or a finger on a trackpad, which moves the guest's pointer from
 * where it is. Microsoft's Remote Desktop app calls them "Touch" and "Mouse
 * pointer".
 */
internal enum class PointerMode { TOUCH, TRACKPAD }

/**
 * The running guest's [Link], once the bridge has delivered it. [token] names
 * the run, so that a bridge left from an earlier run cannot attach to this one.
 */
internal object GuestScreen {
    @Volatile
    var token: String? = null
    var link by mutableStateOf<Link?>(null)

    /** Where the activity sends key events while the guest's screen is shown. */
    @Volatile
    var keys: ((KeyEvent) -> Boolean)? = null
}

/**
 * Where the root bridge delivers the [Link]: exported, since the bridge is not
 * this app, and taking calls from root only.
 */
class GuestLink : ContentProvider() {
    override fun call(method: String, arg: String?, extras: Bundle?): Bundle? {
        if (Binder.getCallingUid() != 0 || method != LINK_METHOD || extras == null) return null
        val display = extras.getBinder("display")
        val input = extras.getBinder("input")
        val link = if (display != null && input != null && arg != null && arg == GuestScreen.token) Link(display, input) else null
        Log.i(TAG, "bridge delivered display $display input $input, taken ${link != null}")
        if (link != null) Handler(Looper.getMainLooper()).post { GuestScreen.link = link }
        return Bundle().apply { putBoolean("taken", link != null) }
    }

    override fun onCreate() = true
    override fun query(uri: Uri, p: Array<String>?, s: String?, a: Array<String>?, o: String?): Cursor? = null
    override fun getType(uri: Uri): String? = null
    override fun insert(uri: Uri, values: ContentValues?): Uri? = null
    override fun delete(uri: Uri, s: String?, a: Array<String>?) = 0
    override fun update(uri: Uri, values: ContentValues?, s: String?, a: Array<String>?) = 0
}

/**
 * The guest's screen: two SurfaceViews whose surfaces are lent to crosvm's
 * display service while they exist, the screen and, over it, the guest's
 * cursor, and the fingers on them, which [mode] makes the guest's
 * touchscreen or a trackpad for its mouse. Coordinates are scaled to the
 * [guestWidth] by [guestHeight] crosvm was given.
 *
 * Before the screen's surface goes, crosvm saves its last frame, and draws it
 * again into the next one, so that coming back shows the screen as it was
 * rather than black until the guest draws again.
 *
 * The view is also the soft keyboard's editor: what it types goes to the
 * [keyboard], a key at a time, and keeps nothing.
 */
internal class GuestView(
    context: Context,
    private val guestWidth: Int,
    private val guestHeight: Int,
) : FrameLayout(context) {
    var link: Link? = null
        set(value) {
            if (field === value) return
            cancelGesture()
            field = value
            screen.attach()
            cursor.attach()
        }

    var mode = PointerMode.TOUCH
        set(value) {
            if (field == value) return
            cancelGesture()
            field = value
        }

    var keyboard: Keyboard? = null

    private val screen = Lent(SurfaceView(context), forCursor = false)
    private val cursor = Lent(SurfaceView(context), forCursor = true)

    init {
        screen.view.holder.setFixedSize(guestWidth, guestHeight)
        addView(screen.view, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        cursor.view.holder.setFormat(PixelFormat.RGBA_8888)
        cursor.view.setZOrderMediaOverlay(true)
        addView(cursor.view, LayoutParams(CURSOR_SIZE, CURSOR_SIZE))
        isFocusable = true
        isFocusableInTouchMode = true
    }

    /**
     * One of the view's surfaces, lent to the display service while it
     * exists: the screen, or the cursor, into which crosvm draws the guest's
     * cursor plane and which it moves by a stream of positions.
     */
    private inner class Lent(val view: SurfaceView, val forCursor: Boolean) : SurfaceHolder.Callback {
        /** The display service holding this surface, and the one that saved a frame of it. */
        private var attached: IBinder? = null
        private var saved: IBinder? = null
        private var follower: CursorFollower? = null

        init {
            view.holder.addCallback(this)
        }

        fun attach() {
            val display = link?.display
            if (attached != null && attached !== display) detach()
            if (display == null || attached === display || !view.holder.surface.isValid) return
            val lent = call(display, SET_SURFACE) {
                writeTypedObject(view.holder.surface, 0)
                writeBoolean(forCursor)
            }
            if (!lent) return
            attached = display
            if (forCursor) follower = follow(display)
            else if (saved === display) call(display, DRAW_SAVED_FRAME) { writeBoolean(false) }
        }

        fun detach() {
            val display = attached ?: return
            attached = null
            follower?.finish()
            follower = null
            if (!forCursor && call(display, SAVE_FRAME) { writeBoolean(false) }) saved = display
            call(display, REMOVE_SURFACE) { writeBoolean(forCursor) }
        }

        override fun surfaceCreated(holder: SurfaceHolder) = attach()
        override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) = Unit
        override fun surfaceDestroyed(holder: SurfaceHolder) = detach()
    }

    /** One call to the display service, whose arguments `write` puts after its token. */
    private fun call(display: IBinder, code: Int, write: Parcel.() -> Unit): Boolean {
        val data = Parcel.obtain()
        val reply = Parcel.obtain()
        return try {
            data.writeInterfaceToken(DISPLAY_SERVICE)
            data.write()
            display.transact(code, data, reply, 0)
            reply.readException()
            Log.i(TAG, "display service call $code OK")
            true
        } catch (error: Exception) {
            Log.w(TAG, "display service call $code failed", error)
            false
        } finally {
            data.recycle()
            reply.recycle()
        }
    }

    /**
     * Give crosvm one end of a socket pair for the cursor's positions, and
     * follow them from the other, as AOSP's Terminal app does.
     */
    private fun follow(display: IBinder): CursorFollower? {
        val (ours, theirs) = try {
            ParcelFileDescriptor.createSocketPair()
        } catch (error: IOException) {
            Log.w(TAG, "no socket pair for the cursor", error)
            return null
        }
        // The parcel carries a copy of their end, so ours of it can close.
        val given = call(display, SET_CURSOR_STREAM) { writeTypedObject(theirs, 0) }
        theirs.close()
        if (!given) {
            ours.close()
            return null
        }
        return CursorFollower(ours).also { it.start() }
    }

    /**
     * Reads the cursor's positions, `(x: i32, y: i32)` little-endian in the
     * guest's pixels, and moves the cursor's surface to each. The surface is
     * made a child of the screen's first, so that a position is one on the
     * screen, wherever the screen is.
     */
    private inner class CursorFollower(private val stream: ParcelFileDescriptor) : Thread("ferrix-cursor") {
        @Volatile
        private var finished = false

        override fun run() {
            val moved = cursor.view.surfaceControl ?: return stream.close()
            val parent = screen.view.surfaceControl ?: return stream.close()
            val transaction = SurfaceControl.Transaction()
            transaction.reparent(moved, parent).apply()
            val bytes = ByteArray(8)
            val at = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
            try {
                FileInputStream(stream.fileDescriptor).use { input ->
                    while (!finished) {
                        var have = 0
                        while (have < bytes.size) {
                            val read = input.read(bytes, have, bytes.size - have)
                            if (read < 0) return
                            have += read
                        }
                        val scale = width.toFloat() / guestWidth
                        val x = at.getInt(0) * scale
                        val y = at.getInt(4) * scale
                        transaction.setPosition(moved, x, y).apply()
                    }
                }
            } catch (_: IOException) {
            } finally {
                stream.close()
            }
        }

        /** Stop reading: shutting the socket down wakes the read under way. */
        fun finish() {
            finished = true
            try {
                Os.shutdown(stream.fileDescriptor, OsConstants.SHUT_RDWR)
            } catch (_: ErrnoException) {
            }
        }
    }

    // The fingers.

    /** What the fingers on the view are doing. */
    private enum class Gesture {
        /** No finger down. */
        NONE,

        /** One finger down, not yet moved: a tap, a hold or a move to come. */
        PENDING,

        /** One finger moving: pressed on the touchscreen, the pointer on a trackpad. */
        MOVING,

        /** One finger held: on a trackpad, the left button down while it drags. */
        HELD,

        /** Two fingers: a scroll, or a tap for the right button. */
        TWO,

        /** Done, until every finger is up. */
        SPENT,
    }

    private val slop = ViewConfiguration.get(context).scaledTouchSlop
    private val wheelStep = WHEEL_STEP_DP * resources.displayMetrics.density
    private var gesture = Gesture.NONE
    private var primary = 0
    private var downX = 0f
    private var downY = 0f
    private var downTime = 0L
    private var lastX = 0f
    private var lastY = 0f
    private var lastTime = 0L
    private var twoStartY = 0f
    private var twoY = 0f
    private var scrolling = false
    private var scrolled = 0f
    private var restX = 0f
    private var restY = 0f
    private val longPress = Runnable { held() }

    /**
     * On the touchscreen, a finger moves the guest's pointer where it lands,
     * and presses once it moves (a drag) or lifts (a tap); held still, it is
     * the right button. On a trackpad, a finger moves the pointer by how far
     * it moves, a tap is the left button, and held still first, it drags with
     * the left button down. On both, two fingers scroll, and tapped together
     * are the right button.
     */
    override fun onTouchEvent(event: MotionEvent): Boolean {
        link ?: return false
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                gesture = Gesture.PENDING
                primary = event.getPointerId(0)
                downX = event.x
                downY = event.y
                downTime = event.eventTime
                lastX = downX
                lastY = downY
                lastTime = downTime
                restX = 0f
                restY = 0f
                postDelayed(longPress, ViewConfiguration.getLongPressTimeout().toLong())
                // The pointer comes to the finger without a press, so that a
                // hold or two fingers act where it landed.
                if (mode == PointerMode.TOUCH) touch(absolute(downX, downY))
            }
            MotionEvent.ACTION_POINTER_DOWN -> when (gesture) {
                Gesture.PENDING, Gesture.TWO -> {
                    removeCallbacks(longPress)
                    if (gesture == Gesture.PENDING) {
                        twoStartY = centreY(event)
                        scrolling = false
                        scrolled = 0f
                    }
                    gesture = Gesture.TWO
                    twoY = centreY(event)
                }
                else -> Unit
            }
            MotionEvent.ACTION_MOVE -> when (gesture) {
                Gesture.PENDING -> {
                    if (hypot(event.x - downX, event.y - downY) > slop) {
                        removeCallbacks(longPress)
                        gesture = Gesture.MOVING
                        if (mode == PointerMode.TOUCH) {
                            touch(absolute(downX, downY) + InputEvent(Linux.EV_KEY, Linux.BTN_TOUCH, 1))
                        }
                        move(event)
                    }
                }
                Gesture.MOVING, Gesture.HELD -> move(event)
                Gesture.TWO -> scroll(event)
                else -> Unit
            }
            MotionEvent.ACTION_POINTER_UP -> when (gesture) {
                Gesture.TWO -> {
                    if (!scrolling && event.eventTime - downTime < TWO_TAP_MS) click(Linux.BTN_RIGHT)
                    gesture = Gesture.SPENT
                }
                Gesture.MOVING, Gesture.HELD ->
                    if (event.getPointerId(event.actionIndex) == primary) release(Gesture.SPENT)
                else -> Unit
            }
            MotionEvent.ACTION_UP -> {
                removeCallbacks(longPress)
                if (gesture == Gesture.PENDING) {
                    if (mode == PointerMode.TOUCH) {
                        touch(
                            absolute(downX, downY) + InputEvent(Linux.EV_KEY, Linux.BTN_TOUCH, 1) +
                                InputEvent(Linux.EV_SYN, Linux.SYN_REPORT, 0) +
                                InputEvent(Linux.EV_KEY, Linux.BTN_TOUCH, 0),
                        )
                    } else {
                        click(Linux.BTN_LEFT)
                    }
                }
                release(Gesture.NONE)
                performClick()
            }
            MotionEvent.ACTION_CANCEL -> cancelGesture()
        }
        return true
    }

    override fun performClick(): Boolean = super.performClick()

    /** A finger held still: the right button on the touchscreen, the left held on a trackpad. */
    private fun held() {
        if (gesture != Gesture.PENDING) return
        performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
        if (mode == PointerMode.TOUCH) {
            click(Linux.BTN_RIGHT)
            gesture = Gesture.SPENT
        } else {
            mouse(listOf(InputEvent(Linux.EV_KEY, Linux.BTN_LEFT, 1)))
            gesture = Gesture.HELD
        }
    }

    /** The primary finger moved: on the touchscreen, to where it is; on a trackpad, by how far. */
    private fun move(event: MotionEvent) {
        val index = event.findPointerIndex(primary)
        if (index < 0) return
        val (x, y) = event.getX(index) to event.getY(index)
        if (mode == PointerMode.TOUCH) {
            touch(absolute(x, y))
        } else {
            // Faster is further: a slow finger places the pointer to the
            // pixel, a quick one crosses the screen.
            val dx = (x - lastX) * guestWidth / width
            val dy = (y - lastY) * guestHeight / height
            val speed = hypot(dx, dy) / (event.eventTime - lastTime).coerceAtLeast(1)
            val gain = TRACKPAD_GAIN * (1 + min(speed, MAX_SPEED) * TRACKPAD_ACCELERATION)
            restX += dx * gain
            restY += dy * gain
            val (stepX, stepY) = restX.toInt() to restY.toInt()
            restX -= stepX
            restY -= stepY
            if (stepX != 0 || stepY != 0) {
                mouse(listOf(InputEvent(Linux.EV_REL, Linux.REL_X, stepX), InputEvent(Linux.EV_REL, Linux.REL_Y, stepY)))
            }
        }
        lastX = x
        lastY = y
        lastTime = event.eventTime
    }

    /** Two fingers moved: the wheel, a click for each [wheelStep] they went, content following them. */
    private fun scroll(event: MotionEvent) {
        val y = centreY(event)
        if (!scrolling && abs(y - twoStartY) > slop) scrolling = true
        if (scrolling) scrolled += y - twoY
        twoY = y
        val clicks = (scrolled / wheelStep).toInt()
        if (clicks == 0) return
        scrolled -= clicks * wheelStep
        // Fingers going down show what is above: the wheel turned up, positive.
        mouse(listOf(InputEvent(Linux.EV_REL, Linux.REL_WHEEL, clicks)))
    }

    /** Let go of what the gesture holds down, and go to `next`. */
    private fun release(next: Gesture) {
        when (gesture) {
            Gesture.MOVING -> if (mode == PointerMode.TOUCH) touch(listOf(InputEvent(Linux.EV_KEY, Linux.BTN_TOUCH, 0)))
            Gesture.HELD -> mouse(listOf(InputEvent(Linux.EV_KEY, Linux.BTN_LEFT, 0)))
            else -> Unit
        }
        gesture = next
    }

    private fun cancelGesture() {
        removeCallbacks(longPress)
        release(Gesture.NONE)
    }

    private fun centreY(event: MotionEvent): Float {
        var sum = 0f
        for (i in 0 until event.pointerCount) sum += event.getY(i)
        return sum / event.pointerCount
    }

    /** Where on the guest's touchscreen a point on the view is. */
    private fun absolute(x: Float, y: Float): List<InputEvent> = listOf(
        InputEvent(Linux.EV_ABS, Linux.ABS_X, (x * guestWidth / width).roundToInt().coerceIn(0, guestWidth)),
        InputEvent(Linux.EV_ABS, Linux.ABS_Y, (y * guestHeight / height).roundToInt().coerceIn(0, guestHeight)),
    )

    private fun click(button: Int) = mouse(
        listOf(
            InputEvent(Linux.EV_KEY, button, 1),
            InputEvent(Linux.EV_SYN, Linux.SYN_REPORT, 0),
            InputEvent(Linux.EV_KEY, button, 0),
        ),
    )

    /** One report to the touchscreen, or the mouse: `events` and the SYN_REPORT that ends them. */
    private fun touch(events: List<InputEvent>) = report(TOUCH_EVENTS, events)
    private fun mouse(events: List<InputEvent>) = report(MOUSE_EVENTS, events)

    private fun report(device: Int, events: List<InputEvent>) {
        link?.send(device, events + InputEvent(Linux.EV_SYN, Linux.SYN_REPORT, 0))
    }

    // The keys.

    /** The keys, soft and hardware, while this view is on the screen. */
    private val keys: (KeyEvent) -> Boolean = { event -> link != null && keyboard?.key(event) == true }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        GuestScreen.keys = keys
    }

    override fun onDetachedFromWindow() {
        if (GuestScreen.keys === keys) GuestScreen.keys = null
        cancelGesture()
        showKeyboard(false)
        super.onDetachedFromWindow()
    }

    /**
     * Whether the view is the soft keyboard's editor: only while the keyboard
     * was asked for, so that Android does not bring it up by itself for a
     * finger on the screen.
     */
    var typing = false

    /** Show or hide the soft keyboard, whose editor is this view. */
    fun showKeyboard(show: Boolean) {
        val methods = context.getSystemService(InputMethodManager::class.java) ?: return
        typing = show
        if (show) {
            requestFocus()
            methods.restartInput(this)
            methods.showSoftInput(this, 0)
        } else {
            methods.hideSoftInputFromWindow(windowToken, 0)
        }
    }

    override fun onCheckIsTextEditor() = typing

    /**
     * A visible password's editor, which Gboard types into a character at a
     * time, with no suggestions and no composing, and whose Enter is a key.
     */
    override fun onCreateInputConnection(info: EditorInfo): InputConnection {
        info.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        info.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or
            EditorInfo.IME_ACTION_NONE
        return Typing()
    }

    /**
     * What the soft keyboard types, sent as keys as it is committed. Nothing
     * stays in the editor, so a deletion is a key too: Backspace for each
     * character before the cursor, Delete for each after. Its key events
     * (Enter, and Backspace when it knows the editor is empty) come through
     * the activity to [keys].
     */
    private inner class Typing : BaseInputConnection(this, true) {
        override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
            super.commitText(text, newCursorPosition)
            flush()
            return true
        }

        override fun finishComposingText(): Boolean {
            super.finishComposingText()
            flush()
            return true
        }

        override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
            val keyboard = keyboard ?: return true
            repeat(beforeLength) { keyboard.tap(Linux.KEY_BACKSPACE) }
            repeat(afterLength) { keyboard.tap(Linux.KEY_DELETE) }
            return true
        }

        override fun performEditorAction(action: Int): Boolean {
            keyboard?.tap(Linux.KEY_ENTER)
            return true
        }

        private fun flush() {
            val typed = editable ?: return
            if (typed.isEmpty()) return
            keyboard?.text(typed.toString())
            typed.clear()
        }
    }

    private companion object {
        /** The cursor's surface, as big as a virtio-gpu cursor. */
        const val CURSOR_SIZE = 64

        /** How far two fingers go for a click of the wheel. */
        const val WHEEL_STEP_DP = 12f

        /** How soon two fingers have to lift to be a tap, in ms. */
        const val TWO_TAP_MS = 300

        /** A trackpad's pointer speed: guest pixels for each pixel a finger goes, slowly. */
        const val TRACKPAD_GAIN = 1.2f

        /** How much more that is for each guest pixel a millisecond, up to [MAX_SPEED]. */
        const val TRACKPAD_ACCELERATION = 0.5f
        const val MAX_SPEED = 4f
    }
}
