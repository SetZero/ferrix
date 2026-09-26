package dev.ferrix.launcher

import android.app.Activity
import android.os.Bundle
import android.view.WindowInsets
import android.view.WindowInsetsController
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.Build
import androidx.compose.material.icons.rounded.CheckCircle
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Info
import androidx.compose.material.icons.rounded.Menu
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material.icons.rounded.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONException
import org.json.JSONObject

/**
 * Two ways to run Ferrix on this phone.
 *
 * **Boot Ferrix** reboots the phone into it. The phone cannot start Ferrix by
 * itself, so the button asks the helper on the PC
 * (`bootloaders/pixel7/launcher/helper.py`), which adb reverse makes reachable
 * at 127.0.0.1 over the USB cable. The helper runs `fastboot boot`: nothing is
 * written to the phone's partitions, and Ferrix's watchdog brings Android back
 * about 75 seconds later.
 *
 * **Run in a VM** needs no PC and no reboot: Ferrix runs as a guest of the
 * phone's own KVM, through Android's crosvm, started as root, and its console
 * is shown here. The image is the one the helper last put in
 * /data/local/tmp/ferrix-vm.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent { LauncherTheme { Launcher() } }
    }
}

private const val HELPER = "http://127.0.0.1:47707"
private const val VM_DIR = "/data/local/tmp/ferrix-vm"
private const val CROSVM = "/apex/com.android.virt/bin/crosvm"

private const val SOCKET = "$VM_DIR/crosvm.sock"

/**
 * The guest: 8 vCPUs and 4 GiB, its 16550 on crosvm's standard output, and a
 * control socket that `suspend`, `resume` and `stop` go to.
 */
private const val GUEST_COMMAND =
    "cd $VM_DIR && [ -f ferrix.Image ] || { echo 'FERRIX-VM no image in $VM_DIR'; exit 3; }; " +
        "rm -f $SOCKET; echo FERRIX-VM-PID $$; exec $CROSVM run --disable-sandbox " +
        "-m 4096 --cpus 8 -s $SOCKET --serial type=stdout,num=1 ferrix.Image 2>/dev/null"

/** How the guest is doing. */
private sealed interface Guest {
    data object Idle : Guest
    data class Running(val pid: Int?, val paused: Boolean = false, val began: Long = System.nanoTime()) : Guest
    data class Ended(val result: String, val seconds: Long, val booted: Boolean) : Guest
}

/** Ask the running guest's crosvm, through its control socket, to `command`. */
private fun control(command: String) {
    ProcessBuilder("su", "-c", "$CROSVM $command $SOCKET").start().waitFor()
}

/** What the helper last said, or why it could not be asked. */
private sealed interface Helper {
    data object Asking : Helper
    data class Unreachable(val why: String) : Helper
    data class Reachable(
        val phase: String,
        val image: String?,
        val imageTime: String?,
        val last: LastRun?,
    ) : Helper
}

private data class LastRun(val `when`: String, val result: String, val seconds: String?) {
    val booted get() = result.startsWith("FERRIX-BOOT-OK")
}

@Composable
private fun LauncherTheme(content: @Composable () -> Unit) {
    val context = LocalContext.current
    val scheme =
        if (isSystemInDarkTheme()) dynamicDarkColorScheme(context)
        else dynamicLightColorScheme(context)
    MaterialTheme(colorScheme = scheme, content = content)
}

@Composable
private fun Launcher() {
    var helper by remember { mutableStateOf<Helper>(Helper.Asking) }
    var confirming by remember { mutableStateOf(false) }
    var refusal by remember { mutableStateOf<String?>(null) }
    var guest by remember { mutableStateOf<Guest>(Guest.Idle) }
    var fullScreen by remember { mutableStateOf(false) }
    val console = remember { mutableStateListOf<String>() }
    val scope = rememberCoroutineScope()
    val run: () -> Unit = {
        console.clear()
        guest = Guest.Running(null)
        fullScreen = true
        scope.launch {
            guest = runGuest(console) { pid -> (guest as? Guest.Running)?.let { guest = it.copy(pid = pid) } }
        }
    }
    val stop: () -> Unit = {
        if (guest is Guest.Running) scope.launch(Dispatchers.IO) { control("stop") }
    }
    val pause: () -> Unit = {
        (guest as? Guest.Running)?.let { running ->
            scope.launch {
                withContext(Dispatchers.IO) { control(if (running.paused) "resume" else "suspend") }
                (guest as? Guest.Running)?.let { guest = it.copy(paused = !running.paused) }
            }
        }
    }
    val lifecycle = LocalLifecycleOwner.current.lifecycle

    LaunchedEffect(lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                helper = poll()
                delay(2000)
            }
        }
    }

    if (fullScreen) {
        VmScreen(
            guest = guest,
            console = console,
            onBack = { fullScreen = false },
            onPause = pause,
            onStop = stop,
            onRunAgain = run,
        )
        return
    }

    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.surface) {
        Column(
            Modifier
                .fillMaxSize()
                .safeDrawingPadding()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp, vertical = 32.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            Header()
            ConnectionCard(helper)
            val current = helper
            val busy = current is Helper.Reachable && current.phase != "idle"
            AnimatedVisibility(busy) {
                if (current is Helper.Reachable) ProgressCard(current.phase)
            }
            BootButton(
                enabled = current is Helper.Reachable && current.phase == "idle" &&
                    current.image != null,
                onClick = { confirming = true },
            )
            refusal?.let { Notice(Icons.Rounded.Warning, it, MaterialTheme.colorScheme.error) }
            if (current is Helper.Reachable) current.last?.let { LastRunCard(it) }
            GuestCard(
                guest = guest,
                console = console,
                onRun = run,
                onStop = stop,
                onFullScreen = { fullScreen = true },
            )
            Spacer(Modifier.height(8.dp))
            Text(
                "Booting changes nothing on the phone: Ferrix runs from RAM, sent by " +
                    "the PC with fastboot boot, and Android comes back by itself. The " +
                    "VM runs beside Android, under its KVM, and asks for root once.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }

    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            icon = { Icon(Icons.Rounded.PlayArrow, contentDescription = null) },
            title = { Text("Reboot into Ferrix?") },
            text = {
                Text(
                    "The phone restarts into Ferrix. Android comes back by itself " +
                        "after about 75 seconds, locked.",
                )
            },
            confirmButton = {
                Button(onClick = {
                    confirming = false
                    refusal = null
                    scope.launch {
                        refusal = withContext(Dispatchers.IO) {
                            try {
                                request("POST", "/boot")
                                null
                            } catch (error: IOException) {
                                "The helper refused: ${error.message}"
                            }
                        }
                    }
                }) { Text("Boot Ferrix") }
            },
            dismissButton = { TextButton(onClick = { confirming = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun Header() {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            "Ferrix",
            style = MaterialTheme.typography.displayMedium,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.primary,
        )
        Text(
            "Boot the Pixel 7 into Ferrix",
            style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ConnectionCard(helper: Helper) {
    val (dot, title, detail) = when (helper) {
        Helper.Asking -> Triple(MaterialTheme.colorScheme.outline, "Looking for the PC…", null)
        is Helper.Unreachable -> Triple(
            MaterialTheme.colorScheme.error,
            "PC helper not reachable",
            "Plug the phone into the PC and run bootloaders/pixel7/launcher/helper.py there.",
        )
        is Helper.Reachable -> Triple(
            Color(0xFF3DDC84),
            "Connected to the PC",
            helper.image?.let { "${name(it)} · built ${helper.imageTime}" }
                ?: "The helper has no boot.img to boot.",
        )
    }
    val animated by animateColorAsState(dot, label = "status")
    Card(
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(Modifier.padding(20.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(12.dp).background(animated, CircleShape))
            Spacer(Modifier.width(16.dp))
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(title, style = MaterialTheme.typography.titleMedium)
                detail?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun ProgressCard(phase: String) {
    Card(
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(
                phase.replaceFirstChar { it.uppercase() },
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSecondaryContainer,
            )
            LinearProgressIndicator(Modifier.fillMaxWidth())
        }
    }
}

@Composable
private fun BootButton(enabled: Boolean, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        enabled = enabled,
        shape = RoundedCornerShape(28.dp),
        contentPadding = ButtonDefaults.ButtonWithIconContentPadding,
        modifier = Modifier.fillMaxWidth().height(72.dp),
    ) {
        Icon(Icons.Rounded.PlayArrow, contentDescription = null, Modifier.size(28.dp))
        Spacer(Modifier.width(12.dp))
        Text("Boot Ferrix", style = MaterialTheme.typography.titleLarge)
    }
}

@Composable
private fun LastRunCard(last: LastRun) {
    val tint = if (last.booted) Color(0xFF3DDC84) else MaterialTheme.colorScheme.error
    Card(
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    if (last.booted) Icons.Rounded.CheckCircle else Icons.Rounded.Warning,
                    contentDescription = null,
                    tint = tint,
                )
                Spacer(Modifier.width(12.dp))
                Text("Last run", style = MaterialTheme.typography.titleMedium)
                Spacer(Modifier.weight(1f))
                Text(
                    pretty(last.`when`),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                last.result,
                style = MaterialTheme.typography.bodyLarge,
                fontFamily = FontFamily.Monospace,
            )
            last.seconds?.let {
                Text(
                    "Back in Android after $it s",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun GuestCard(
    guest: Guest,
    console: List<String>,
    onRun: () -> Unit,
    onStop: () -> Unit,
    onFullScreen: () -> Unit,
) {
    Card(
        shape = RoundedCornerShape(24.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerHigh),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Or run it in a VM", style = MaterialTheme.typography.titleMedium)
            Text(
                "On the phone's own KVM, beside Android: no PC, no reboot.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            when (guest) {
                is Guest.Running -> {
                    LinearProgressIndicator(Modifier.fillMaxWidth())
                    FilledTonalButton(
                        onClick = onStop,
                        enabled = guest.pid != null,
                        shape = RoundedCornerShape(20.dp),
                        modifier = Modifier.fillMaxWidth().height(56.dp),
                    ) { Text("Stop") }
                }
                else -> FilledTonalButton(
                    onClick = onRun,
                    shape = RoundedCornerShape(20.dp),
                    modifier = Modifier.fillMaxWidth().height(56.dp),
                ) {
                    Icon(Icons.Rounded.Build, contentDescription = null)
                    Spacer(Modifier.width(12.dp))
                    Text("Run in a VM", style = MaterialTheme.typography.titleMedium)
                }
            }
            if (guest is Guest.Ended) {
                Notice(
                    if (guest.booted) Icons.Rounded.CheckCircle else Icons.Rounded.Warning,
                    "${guest.result} · ${guest.seconds} s",
                    if (guest.booted) Color(0xFF3DDC84) else MaterialTheme.colorScheme.error,
                )
            }
            if (console.isNotEmpty()) {
                ConsolePane(console, Modifier.fillMaxWidth().height(320.dp))
                TextButton(onClick = onFullScreen, modifier = Modifier.align(Alignment.End)) {
                    Text("Full screen")
                }
            }
        }
    }
}

/**
 * The guest's console across the whole screen, under one bar: its state, and
 * a menu to pause or resume it, stop it, run it again, or go back. The phone's
 * own bars are hidden meanwhile, and a swipe brings them back for a moment.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun VmScreen(
    guest: Guest,
    console: List<String>,
    onBack: () -> Unit,
    onPause: () -> Unit,
    onStop: () -> Unit,
    onRunAgain: () -> Unit,
) {
    BackHandler(onBack = onBack)
    val window = (LocalContext.current as Activity).window
    DisposableEffect(window) {
        val bars = window.insetsController
        bars?.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        bars?.hide(WindowInsets.Type.systemBars())
        onDispose { bars?.show(WindowInsets.Type.systemBars()) }
    }
    var menu by remember { mutableStateOf(false) }
    var now by remember { mutableStateOf(System.nanoTime()) }
    LaunchedEffect(guest) {
        while (guest is Guest.Running) {
            now = System.nanoTime()
            delay(500)
        }
    }
    val state = when (guest) {
        Guest.Idle -> "not started"
        is Guest.Running -> {
            val seconds = (now - guest.began) / 1_000_000_000
            if (guest.pid == null) "starting…" else if (guest.paused) "paused · $seconds s" else "running · $seconds s"
        }
        is Guest.Ended -> "${guest.result} · ${guest.seconds} s"
    }
    Column(Modifier.fillMaxSize().background(Color(0xFF0B0B10))) {
        TopAppBar(
            title = {
                Column {
                    Text("Ferrix VM", style = MaterialTheme.typography.titleMedium)
                    Text(
                        state,
                        style = MaterialTheme.typography.labelMedium,
                        color = when {
                            guest is Guest.Ended && guest.booted -> Color(0xFF3DDC84)
                            guest is Guest.Ended -> MaterialTheme.colorScheme.error
                            else -> MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            },
            actions = {
                Box {
                    IconButton(onClick = { menu = true }) {
                        Icon(Icons.Rounded.Menu, contentDescription = "Menu")
                    }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        if (guest is Guest.Running) {
                            DropdownMenuItem(
                                text = { Text(if (guest.paused) "Resume" else "Pause") },
                                leadingIcon = if (guest.paused) {
                                    { Icon(Icons.Rounded.PlayArrow, null) }
                                } else {
                                    null
                                },
                                enabled = guest.pid != null,
                                onClick = { menu = false; onPause() },
                            )
                            DropdownMenuItem(
                                text = { Text("Stop") },
                                leadingIcon = { Icon(Icons.Rounded.Close, null) },
                                enabled = guest.pid != null,
                                onClick = { menu = false; onStop() },
                            )
                        } else {
                            DropdownMenuItem(
                                text = { Text("Run again") },
                                leadingIcon = { Icon(Icons.Rounded.Refresh, null) },
                                onClick = { menu = false; onRunAgain() },
                            )
                        }
                        DropdownMenuItem(
                            text = { Text("Back to main page") },
                            leadingIcon = { Icon(Icons.AutoMirrored.Rounded.ArrowBack, null) },
                            onClick = { menu = false; onBack() },
                        )
                    }
                }
            },
            colors = TopAppBarDefaults.topAppBarColors(
                containerColor = MaterialTheme.colorScheme.surfaceContainer,
            ),
        )
        if (guest is Guest.Running && guest.pid != null && !guest.paused) {
            LinearProgressIndicator(Modifier.fillMaxWidth().height(2.dp))
        }
        ConsolePane(console, Modifier.fillMaxSize(), fontSize = 12)
    }
}

/** How near the end, in lines, the reader has to be for the console to follow. */
private const val FOLLOW_LINES = 10

/**
 * The guest's console, following its newest line while the reader is within
 * [FOLLOW_LINES] of the end. Scrolled further up, it stays put, so a line can
 * be read while the boot goes on; scrolled back down near the end, it follows
 * again.
 *
 * Each jump to the end is its own job: a finger on the pane cancels the jump
 * under way, and must not cancel the following itself.
 */
@Composable
private fun ConsolePane(lines: List<String>, modifier: Modifier, fontSize: Int = 11) {
    val scroll = rememberScrollState()
    val near = with(LocalDensity.current) { (fontSize + 3).sp.toPx() } * FOLLOW_LINES
    LaunchedEffect(scroll, near) {
        var following = true
        var lastEnd = scroll.maxValue
        snapshotFlow { scroll.value to scroll.maxValue }.collect { (at, end) ->
            if (end != lastEnd) {
                lastEnd = end
                if (following) launch { scroll.scrollTo(end) }
            } else {
                following = end - at <= near
            }
        }
    }
    Box(
        modifier
            .background(Color(0xFF0B0B10), RoundedCornerShape(16.dp))
            .padding(12.dp)
            .verticalScroll(scroll),
    ) {
        Text(
            lines.joinToString("\n"),
            fontFamily = FontFamily.Monospace,
            fontSize = fontSize.sp,
            lineHeight = (fontSize + 3).sp,
            color = Color(0xFFC8C8D2),
        )
    }
}

/** Run the guest to its end, appending its console to `console`. */
private suspend fun runGuest(console: MutableList<String>, started: (Int) -> Unit): Guest =
    withContext(Dispatchers.IO) {
        val began = System.nanoTime()
        var result: String? = null
        val process = try {
            ProcessBuilder("su", "-c", GUEST_COMMAND).start()
        } catch (error: IOException) {
            return@withContext Guest.Ended("could not run su: ${error.message}", 0, false)
        }
        process.inputStream.bufferedReader().useLines { lines ->
            for (raw in lines) {
                val line = raw.trimEnd('\r')
                if (line.startsWith("FERRIX-VM-PID ")) {
                    line.removePrefix("FERRIX-VM-PID ").toIntOrNull()?.let {
                        withContext(Dispatchers.Main) { started(it) }
                    }
                    continue
                }
                if (line.startsWith("FERRIX-BOOT-OK") || line.startsWith("FERRIX-PANIC") ||
                    line.startsWith("FERRIX-VM ")
                ) {
                    result = line.removePrefix("FERRIX-VM ")
                }
                withContext(Dispatchers.Main) {
                    console.add(line)
                    if (console.size > 2000) console.removeAt(0)
                }
            }
        }
        val status = process.waitFor()
        val seconds = (System.nanoTime() - began) / 1_000_000_000
        val ended = result ?: if (status == 1 && console.isEmpty()) {
            "root was not granted"
        } else {
            "the guest stopped with no FERRIX-BOOT-OK (status $status)"
        }
        Guest.Ended(ended, seconds, ended.startsWith("FERRIX-BOOT-OK"))
    }

@Composable
private fun Notice(icon: ImageVector, text: String, tint: Color) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, contentDescription = null, tint = tint)
        Spacer(Modifier.width(12.dp))
        Text(text, style = MaterialTheme.typography.bodyMedium, color = tint)
    }
}

/** The run directory and file name, which is what tells two images apart. */
private fun name(path: String): String = path.split('/').takeLast(2).joinToString("/")

/** `20260926-144950` as `26 Sep 14:49`. */
private fun pretty(stamp: String): String {
    if (stamp.length < 13) return stamp
    val months = listOf("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
    val month = stamp.substring(4, 6).toIntOrNull()?.let { months.getOrNull(it - 1) } ?: return stamp
    return "${stamp.substring(6, 8).trimStart('0')} $month ${stamp.substring(9, 11)}:${stamp.substring(11, 13)}"
}

private suspend fun poll(): Helper = withContext(Dispatchers.IO) {
    try {
        val reply = JSONObject(request("GET", "/status"))
        val last = reply.optJSONObject("last")?.takeIf { it.length() > 0 }?.let {
            LastRun(it.optString("when"), it.optString("result"), it.optString("seconds").ifEmpty { null })
        }
        Helper.Reachable(
            phase = reply.optString("phase"),
            image = if (reply.isNull("image")) null else reply.optString("image"),
            imageTime = if (reply.isNull("image_time")) null else reply.optString("image_time"),
            last = last,
        )
    } catch (error: IOException) {
        Helper.Unreachable(error.message ?: "no answer")
    } catch (error: JSONException) {
        Helper.Unreachable(error.message ?: "an answer that is not JSON")
    }
}

private fun request(method: String, path: String): String {
    val connection = URL(HELPER + path).openConnection() as HttpURLConnection
    connection.connectTimeout = 1500
    connection.readTimeout = 5000
    connection.requestMethod = method
    if (method == "POST") {
        connection.doOutput = true
        connection.outputStream.close()
    }
    try {
        val code = connection.responseCode
        val stream = if (code < 400) connection.inputStream else connection.errorStream
        val body = stream?.bufferedReader()?.use { it.readText() }.orEmpty()
        if (code >= 400) {
            val reason = try {
                JSONObject(body).optString("error", body)
            } catch (_: JSONException) {
                body
            }
            throw IOException(reason)
        }
        return body
    } finally {
        connection.disconnect()
    }
}
