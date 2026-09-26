package dev.ferrix.launcher

import android.content.AttributionSource
import android.net.LocalServerSocket
import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.os.Binder
import android.os.Bundle
import android.os.IBinder
import android.os.Parcel
import android.os.Process
import java.io.File
import java.io.IOException
import java.io.OutputStream
import kotlin.system.exitProcess

/** The provider the bridge hands its two binders to, and the call it makes. */
internal const val LINK_AUTHORITY = "dev.ferrix.launcher.vm"
internal const val LINK_METHOD = "attach"

/** The touch binder's one transaction: a byte array of virtio_input_events. */
internal const val TOUCH_EVENTS = IBinder.FIRST_CALL_TRANSACTION

/**
 * The root half of the guest's screen, run by the VM's su script under
 * `app_process` with this APK and the Terminal app's APK on its class path:
 *
 *     Bridge <crosvm pid> <token> <app uid> <touch socket>
 *
 * crosvm connects to the touch socket as it starts, and will not start if
 * nothing listens there, so the bridge listens first; the script waits for
 * the socket before it runs crosvm. Then the bridge waits for crosvm's display
 * service, which virtualizationservice holds for root only, and hands it to
 * the app's [GuestLink] provider together with a binder of its own that takes
 * the app's touch events and writes them to crosvm's connection.
 *
 * The bridge ends with crosvm: when its process is gone or it closes the
 * touch connection. virtualizationservice keeps the last display service it
 * was given even after that crosvm died, so the one wanted is the one that
 * answers a ping.
 */
object Bridge {
    @JvmStatic
    fun main(args: Array<String>) {
        val crosvm = args[0]
        val socket = args[3]
        try {
            serve(crosvm, args[1], args[2].toInt(), socket)
        } catch (error: Throwable) {
            say("failed: $error")
            // With no socket, the script runs crosvm without a screen.
            if (!listening) exitProcess(1)
        }
        // Once crosvm may be connected to the socket, stay until it ends,
        // whatever went wrong: a closed input device could end it too.
        watch(crosvm, socket)
    }

    @Volatile
    private var listening = false

    private fun serve(crosvm: String, token: String, appUid: Int, path: String) {
        val service = displayServiceGetter()
        val unbound = LocalSocket(LocalSocket.SOCKET_STREAM)
        unbound.bind(LocalSocketAddress(path, LocalSocketAddress.Namespace.FILESYSTEM))
        val server = LocalServerSocket(unbound.fileDescriptor)
        listening = true
        say("listening on $path")
        Thread { watch(crosvm, path) }.apply { isDaemon = true }.start()

        val touch = Touch(appUid)
        Thread {
            val connection = server.accept()
            touch.out = connection.outputStream
            say("touch connected")
            // crosvm writes status updates back; nothing here needs them.
            val input = connection.inputStream
            val sink = ByteArray(256)
            try {
                while (input.read(sink) >= 0) continue
            } catch (_: IOException) {
            }
            end(path, 0)
        }.apply { isDaemon = true }.start()

        var display: IBinder? = null
        while (display == null) {
            display = service()?.takeIf { it.pingBinder() }
            if (display == null) Thread.sleep(200)
        }
        say("display ${display.interfaceDescriptor}")
        deliver(token, display, touch)
    }

    /** A function asking virtualizationservice for crosvm's display service. */
    private fun displayServiceGetter(): () -> IBinder? {
        val manager = Class.forName("android.os.ServiceManager")
        val binder = manager.getMethod("waitForService", String::class.java)
            .invoke(null, "android.system.virtualizationservice") as IBinder
        val stub = Class.forName(
            "android.system.virtualizationservice_internal.IVirtualizationServiceInternal\$Stub",
        )
        val service = stub.getMethod("asInterface", IBinder::class.java).invoke(null, binder)
        val wait = service.javaClass.getMethod("waitDisplayService")
        return { wait.invoke(service) as IBinder? }
    }

    /** Hand both binders to the app, through its provider, as the activity manager lets root. */
    private fun deliver(token: String, display: IBinder, touch: IBinder) {
        val manager = Class.forName("android.app.ActivityManager").getMethod("getService").invoke(null)!!
        val methods = manager.javaClass.methods
        val holder = methods.first { it.name == "getContentProviderExternal" }
            .invoke(manager, LINK_AUTHORITY, 0, null, "ferrix-vm")
            ?: return say("no provider $LINK_AUTHORITY")
        try {
            val provider = holder.javaClass.getField("provider").get(holder)!!
            val call = provider.javaClass.methods.first { it.name == "call" && it.parameterCount == 5 }
            val extras = Bundle().apply {
                putBinder("display", display)
                putBinder("touch", touch)
            }
            val source = AttributionSource.Builder(Process.myUid()).build()
            val reply = call.invoke(provider, source, LINK_AUTHORITY, LINK_METHOD, token, extras) as Bundle?
            say(if (reply?.getBoolean("taken") == true) "delivered" else "the app did not take it")
        } finally {
            methods.first { it.name == "removeContentProviderExternalAsUser" }
                .invoke(manager, LINK_AUTHORITY, null, 0)
        }
    }

    /** Return when crosvm's process is gone, and end the bridge then. */
    private fun watch(crosvm: String, socket: String) {
        while (File("/proc/$crosvm").exists()) Thread.sleep(250)
        end(socket, 0)
    }

    private fun end(socket: String, status: Int): Nothing {
        File(socket).delete()
        exitProcess(status)
    }

    /** One line to the VM's output, which the app shows with the console. */
    private fun say(line: String) = println("FERRIX-VM-BRIDGE $line")

    /**
     * Takes batches of 8-byte virtio_input_events from the app, and only from
     * it, and writes them to crosvm's touch connection as they come. Events
     * before crosvm has connected are dropped.
     */
    private class Touch(private val appUid: Int) : Binder() {
        @Volatile
        var out: OutputStream? = null

        override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
            if (code != TOUCH_EVENTS) return super.onTransact(code, data, reply, flags)
            if (getCallingUid() != appUid) return false
            val events = data.createByteArray() ?: return true
            try {
                out?.write(events)
            } catch (_: IOException) {
            }
            return true
        }
    }
}
