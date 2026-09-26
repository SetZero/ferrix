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
import java.util.concurrent.atomic.AtomicReferenceArray
import kotlin.system.exitProcess

/** The provider the bridge hands its two binders to, and the call it makes. */
internal const val LINK_AUTHORITY = "dev.ferrix.launcher.vm"
internal const val LINK_METHOD = "attach"

/**
 * The input binder's transactions, one per device, in the order of the
 * sockets on the bridge's command line: each a byte array of
 * virtio_input_events.
 */
internal const val TOUCH_EVENTS = IBinder.FIRST_CALL_TRANSACTION
internal const val KEYBOARD_EVENTS = IBinder.FIRST_CALL_TRANSACTION + 1
internal const val MOUSE_EVENTS = IBinder.FIRST_CALL_TRANSACTION + 2

/**
 * The root half of the guest's screen, run by the VM's su script under
 * `app_process` with this APK and the Terminal app's APK on its class path:
 *
 *     Bridge <crosvm pid> <token> <app uid> <touch socket> <keyboard socket> <mouse socket>
 *
 * crosvm connects to each input device's socket as it starts, and will not
 * start if nothing listens there, so the bridge listens first; the script
 * waits for the sockets before it runs crosvm. Each is made listening under
 * another name and renamed into place, so that a socket the script sees is
 * one crosvm can connect to. Then the bridge waits for crosvm's display
 * service, which virtualizationservice holds for root only, and hands it to
 * the app's [GuestLink] provider together with a binder of its own that takes
 * the app's input events, device by device, and writes them to crosvm's
 * connections.
 *
 * The bridge ends with crosvm: when its process is gone or it closes one of
 * the connections. virtualizationservice keeps the last display service it
 * was given even after that crosvm died, so the one wanted is the one that
 * answers a ping.
 */
object Bridge {
    @JvmStatic
    fun main(args: Array<String>) {
        val crosvm = args[0]
        val sockets = args.drop(3)
        try {
            serve(crosvm, args[1], args[2].toInt(), sockets)
        } catch (error: Throwable) {
            say("failed: $error")
            // With no sockets, the script runs crosvm without a screen.
            if (!listening) end(sockets, 1)
        }
        // Once crosvm may be connected to the sockets, stay until it ends,
        // whatever went wrong: a closed input device could end it too.
        watch(crosvm, sockets)
    }

    @Volatile
    private var listening = false

    private fun serve(crosvm: String, token: String, appUid: Int, paths: List<String>) {
        val service = displayServiceGetter()
        val servers = paths.map { path ->
            val unbound = LocalSocket(LocalSocket.SOCKET_STREAM)
            val new = "$path.new"
            File(new).delete()
            unbound.bind(LocalSocketAddress(new, LocalSocketAddress.Namespace.FILESYSTEM))
            LocalServerSocket(unbound.fileDescriptor).also {
                if (!File(new).renameTo(File(path))) throw IOException("cannot name $path")
            }
        }
        listening = true
        say("listening on ${paths.joinToString()}")
        Thread { watch(crosvm, paths) }.apply { isDaemon = true }.start()

        val input = Input(appUid, paths.size)
        servers.forEachIndexed { device, server ->
            Thread {
                val connection = server.accept()
                input.out.set(device, connection.outputStream)
                say("${paths[device].substringAfterLast('/')} connected")
                // crosvm writes status updates back, the keyboard's LEDs
                // among them; nothing here needs them.
                val stream = connection.inputStream
                val sink = ByteArray(256)
                try {
                    while (stream.read(sink) >= 0) continue
                } catch (_: IOException) {
                }
                end(paths, 0)
            }.apply { isDaemon = true }.start()
        }

        var display: IBinder? = null
        while (display == null) {
            display = service()?.takeIf { it.pingBinder() }
            if (display == null) Thread.sleep(200)
        }
        say("display ${display.interfaceDescriptor}")
        deliver(token, display, input)
    }

    /**
     * A function asking virtualizationservice for crosvm's display service,
     * after forgetting whichever one it held.
     *
     * virtualizationservice keeps the last display service set, and a crosvm
     * sets its own only once its GPU is made, after the input sockets are
     * connected. A crosvm still running from an earlier run -- one whose app
     * was reinstalled under it -- answers pings, so waiting for "a live one"
     * handed the app that VM's screen while its keys went to the new VM, and
     * nothing typed ever showed (2026-09-26). Cleared here, before the
     * sockets exist and so before crosvm starts, the one waited for is this
     * run's.
     */
    private fun displayServiceGetter(): () -> IBinder? {
        val manager = Class.forName("android.os.ServiceManager")
        val binder = manager.getMethod("waitForService", String::class.java)
            .invoke(null, "android.system.virtualizationservice") as IBinder
        val stub = Class.forName(
            "android.system.virtualizationservice_internal.IVirtualizationServiceInternal\$Stub",
        )
        val service = stub.getMethod("asInterface", IBinder::class.java).invoke(null, binder)
        service.javaClass.getMethod("clearDisplayService").invoke(service)
        val wait = service.javaClass.getMethod("waitDisplayService")
        return { wait.invoke(service) as IBinder? }
    }

    /** Hand both binders to the app, through its provider, as the activity manager lets root. */
    private fun deliver(token: String, display: IBinder, input: IBinder) {
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
                putBinder("input", input)
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
    private fun watch(crosvm: String, sockets: List<String>) {
        while (File("/proc/$crosvm").exists()) Thread.sleep(250)
        end(sockets, 0)
    }

    private fun end(sockets: List<String>, status: Int): Nothing {
        for (socket in sockets) {
            File(socket).delete()
            File("$socket.new").delete()
        }
        exitProcess(status)
    }

    /** One line to the VM's output, which the app shows with the console. */
    private fun say(line: String) = println("FERRIX-VM-BRIDGE $line")

    /**
     * Takes batches of 8-byte virtio_input_events from the app, and only from
     * it, and writes them to the connection of the device the transaction
     * names as they come. Events before crosvm has connected are dropped.
     */
    private class Input(private val appUid: Int, devices: Int) : Binder() {
        val out = AtomicReferenceArray<OutputStream?>(devices)

        override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
            val device = code - TOUCH_EVENTS
            if (device !in 0 until out.length()) return super.onTransact(code, data, reply, flags)
            if (getCallingUid() != appUid) return false
            val events = data.createByteArray() ?: return true
            try {
                out.get(device)?.write(events)
            } catch (_: IOException) {
            }
            return true
        }
    }
}
