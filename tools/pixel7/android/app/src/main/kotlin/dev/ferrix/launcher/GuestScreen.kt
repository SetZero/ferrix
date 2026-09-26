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
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.math.roundToInt

private const val TAG = "FerrixVm"

/** crosvm's display service: its AIDL interface and the transactions used here. */
private const val DISPLAY_SERVICE = "android.crosvm.ICrosvmAndroidDisplayService"
private const val SET_SURFACE = 1
private const val REMOVE_SURFACE = 3
private const val SAVE_FRAME = 4
private const val DRAW_SAVED_FRAME = 5

/** What the bridge hands over for a run: crosvm's display service and its own touch binder. */
internal class Link(val display: IBinder, val touch: IBinder)

/**
 * The running guest's [Link], once the bridge has delivered it. [token] names
 * the run, so that a bridge left from an earlier run cannot attach to this one.
 */
internal object GuestScreen {
    @Volatile
    var token: String? = null
    var link by mutableStateOf<Link?>(null)
}

/**
 * Where the root bridge delivers the [Link]: exported, since the bridge is not
 * this app, and taking calls from root only.
 */
class GuestLink : ContentProvider() {
    override fun call(method: String, arg: String?, extras: Bundle?): Bundle? {
        if (Binder.getCallingUid() != 0 || method != LINK_METHOD || extras == null) return null
        val display = extras.getBinder("display")
        val touch = extras.getBinder("touch")
        val link = if (display != null && touch != null && arg != null && arg == GuestScreen.token) Link(display, touch) else null
        Log.i(TAG, "bridge delivered display $display touch $touch, taken ${link != null}")
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
 * The guest's screen: a SurfaceView whose surface is lent to crosvm's display
 * service while it exists, and whose touches go to the guest's single-touch
 * device, scaled to the [guestWidth] by [guestHeight] crosvm was given.
 *
 * Before the surface goes, crosvm saves its last frame, and draws it again
 * into the next one, so that coming back shows the screen as it was rather
 * than black until the guest draws again.
 */
internal class GuestView(
    context: Context,
    private val guestWidth: Int,
    private val guestHeight: Int,
) : SurfaceView(context), SurfaceHolder.Callback {
    var link: Link? = null
        set(value) {
            if (field === value) return
            field = value
            attach()
        }

    /** The display service holding this view's surface, and the one that saved a frame of it. */
    private var attached: IBinder? = null
    private var saved: IBinder? = null

    init {
        holder.addCallback(this)
        holder.setFixedSize(guestWidth, guestHeight)
    }

    private fun attach() {
        val display = link?.display
        if (attached != null && attached !== display) detach()
        if (display == null || attached === display || !holder.surface.isValid) return
        if (!transact(display, SET_SURFACE, withSurface = true)) return
        attached = display
        if (saved === display) transact(display, DRAW_SAVED_FRAME)
    }

    private fun detach() {
        val display = attached ?: return
        attached = null
        if (transact(display, SAVE_FRAME)) saved = display
        transact(display, REMOVE_SURFACE)
    }

    /** One call to the display service; each takes `forCursor = false` last. */
    private fun transact(display: IBinder, code: Int, withSurface: Boolean = false): Boolean {
        val data = Parcel.obtain()
        val reply = Parcel.obtain()
        return try {
            data.writeInterfaceToken(DISPLAY_SERVICE)
            if (withSurface) data.writeTypedObject(holder.surface, 0)
            data.writeBoolean(false)
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

    override fun surfaceCreated(holder: SurfaceHolder) = attach()
    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) = Unit
    override fun surfaceDestroyed(holder: SurfaceHolder) = detach()

    override fun onTouchEvent(event: MotionEvent): Boolean {
        val touch = link?.touch ?: return false
        val x = (event.x * guestWidth / width).roundToInt().coerceIn(0, guestWidth)
        val y = (event.y * guestHeight / height).roundToInt().coerceIn(0, guestHeight)
        val events = when (event.actionMasked) {
            MotionEvent.ACTION_DOWN ->
                listOf(Triple(EV_ABS, ABS_X, x), Triple(EV_ABS, ABS_Y, y), Triple(EV_KEY, BTN_TOUCH, 1))
            MotionEvent.ACTION_MOVE -> listOf(Triple(EV_ABS, ABS_X, x), Triple(EV_ABS, ABS_Y, y))
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> listOf(Triple(EV_KEY, BTN_TOUCH, 0))
            else -> return true
        } + Triple(EV_SYN, SYN_REPORT, 0)
        // virtio_input_event: le16 type, le16 code, le32 value.
        val bytes = ByteBuffer.allocate(8 * events.size).order(ByteOrder.LITTLE_ENDIAN)
        for ((type, code, value) in events) bytes.putShort(type.toShort()).putShort(code.toShort()).putInt(value)
        val data = Parcel.obtain()
        try {
            data.writeByteArray(bytes.array())
            touch.transact(TOUCH_EVENTS, data, null, IBinder.FLAG_ONEWAY)
        } catch (error: Exception) {
            Log.w(TAG, "touch events lost", error)
        } finally {
            data.recycle()
        }
        if (event.actionMasked == MotionEvent.ACTION_UP) performClick()
        return true
    }

    override fun performClick(): Boolean = super.performClick()

    private companion object {
        /** From Linux's input-event-codes.h, which virtio-input uses as they are. */
        const val EV_SYN = 0
        const val EV_KEY = 1
        const val EV_ABS = 3
        const val SYN_REPORT = 0
        const val ABS_X = 0
        const val ABS_Y = 1
        const val BTN_TOUCH = 0x14a
    }
}
