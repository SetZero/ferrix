package dev.ferrix.launcher

import android.app.Activity
import android.content.Context
import android.content.pm.ActivityInfo
import android.os.Bundle
import android.os.Process
import android.util.Log
import android.view.KeyEvent
import android.view.WindowInsets
import android.view.WindowInsetsController
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets as ComposeInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.ime
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
import androidx.compose.material.icons.rounded.Check
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
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
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
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathParser
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import java.io.IOException
import java.io.OutputStream
import java.net.HttpURLConnection
import java.net.URL
import java.util.UUID
import kotlin.math.ceil
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONException
import org.json.JSONObject

/**
 * Two ways to run Ferrix on this phone.
 *
 * **Boot Ferrix** reboots the phone into it. The phone cannot start Ferrix by
 * itself, so the button asks the helper on the PC
 * (`tools/pixel7/helper.py`), which adb reverse makes reachable
 * at 127.0.0.1 over the USB cable. The helper runs `fastboot boot`: nothing is
 * written to the phone's partitions, and Ferrix's watchdog brings Android back
 * about 75 seconds later.
 *
 * **Run in a VM** needs no PC and no reboot: Ferrix runs as a guest of the
 * phone's own KVM, through Android's crosvm, started as root, and its console
 * and screen are shown here. The image is the one the helper last put in
 * /data/local/tmp/ferrix-vm.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent { LauncherTheme { Launcher() } }
    }

    /** While the guest's screen is shown, the keys a keyboard has are the guest's. */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean =
        GuestScreen.keys?.invoke(event) == true || super.dispatchKeyEvent(event)
}

private const val HELPER = "http://127.0.0.1:47707"
private const val VM_DIR = "/data/local/tmp/ferrix-vm"
private const val CROSVM = "/apex/com.android.virt/bin/crosvm"

private const val SOCKET = "$VM_DIR/crosvm.sock"
private const val TOUCH = "$VM_DIR/touch.sock"
private const val KEYBOARD = "$VM_DIR/keyboard.sock"
private const val MOUSE = "$VM_DIR/mouse.sock"

/**
 * The guest: 8 vCPUs and 4 GiB, its 16550 on crosvm's standard output, a
 * control socket that `suspend`, `resume` and `stop` go to, and a screen of
 * [size] with a single-touch device on it, a keyboard and a mouse. The
 * 16550 takes crosvm's standard input too, which is the su process's, so
 * that the app can say a line to the guest's shell ([GuestShell]).
 *
 * The screen needs the [Bridge] (from this APK, [apk], with the Terminal
 * app's for virtualizationservice's classes) listening on the input devices'
 * sockets before crosvm starts, for crosvm connects to each and will not
 * start without them. The bridge makes the mouse's last. If it has not made
 * them all within ten seconds, or ended, the guest runs as before, with its
 * console only.
 *
 * The guest is `desktop.Image` when there is one, a build whose init is the
 * compositor (`cargo xtask flash --compositor`, wrapped by the loader), and
 * otherwise `ferrix.Image`, the helper's console image.
 */
private fun guestCommand(apk: String, uid: Int, token: String, size: IntSize): String {
    val (w, h) = size.width to size.height
    return "cd $VM_DIR && I=ferrix.Image && { [ ! -f desktop.Image ] || I=desktop.Image; } && " +
        "[ -f \$I ] || { echo 'FERRIX-VM no image in $VM_DIR'; exit 3; }; echo FERRIX-VM-IMAGE \$I; " +
        "$CROSVM stop $SOCKET >/dev/null 2>&1; " +
        "rm -f $SOCKET $TOUCH $KEYBOARD $MOUSE; echo FERRIX-VM-PID $$; " +
        "T=$(pm path com.android.virtualization.terminal | sed -n 's/^package://p' | head -n 1); " +
        "CLASSPATH=$apk:\$T app_process /system/bin ${Bridge::class.java.name} $$ $token $uid " +
        "$TOUCH $KEYBOARD $MOUSE 2>/dev/null & B=$!; i=0; " +
        "while [ ! -S $MOUSE ] && [ \$i -lt 100 ] && kill -0 \$B 2>/dev/null; do sleep 0.1; i=$((i+1)); done; " +
        "if [ -S $TOUCH ] && [ -S $KEYBOARD ] && [ -S $MOUSE ]; then " +
        "set -- --gpu 'backend=2d,displays=[[mode=windowed[$w,$h]]]' --android-display-service ferrix " +
        "--input 'single-touch[path=$TOUCH,width=$w,height=$h]' --input 'keyboard[path=$KEYBOARD]' " +
        "--input 'mouse[path=$MOUSE]'; " +
        "else echo 'FERRIX-VM-BRIDGE did not start: no screen'; set --; fi; " +
        "exec $CROSVM run --disable-sandbox -m 4096 --cpus 8 -s $SOCKET --serial type=stdout,num=1,stdin " +
        "\"\$@\" \$I 2>/dev/null"
}

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
    var screenSize by remember { mutableStateOf(IntSize.Zero) }
    val console = remember { mutableStateListOf<String>() }
    var shell by remember { mutableStateOf(GuestShell()) }
    val scope = rememberCoroutineScope()
    val activity = LocalContext.current as Activity
    val run: () -> Unit = {
        console.clear()
        guest = Guest.Running(null)
        fullScreen = true
        // The guest's screen is the phone's, held upright.
        val bounds = activity.windowManager.maximumWindowMetrics.bounds
        screenSize = IntSize(minOf(bounds.width(), bounds.height()), maxOf(bounds.width(), bounds.height()))
        val token = UUID.randomUUID().toString()
        GuestScreen.token = token
        GuestScreen.link = null
        val command = guestCommand(activity.applicationInfo.sourceDir, Process.myUid(), token, screenSize)
        val current = GuestShell()
        shell = current
        val reserving = scope.launch { current.reserveAsAsked() }
        scope.launch {
            guest = runGuest(console, current, command) { pid ->
                (guest as? Guest.Running)?.let { guest = it.copy(pid = pid) }
            }
            reserving.cancel()
            if (GuestScreen.token == token) GuestScreen.link = null
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
            link = GuestScreen.link,
            shell = shell,
            screenSize = screenSize,
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
            "Plug the phone into the PC and run tools/pixel7/helper.py there.",
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
 * The guest across the whole screen: its console under one bar, with its
 * state and a menu to pause or resume it, stop it, run it again, or go back;
 * or, once the bridge has delivered its [link], its screen, held upright, with
 * the same menu floating in a corner and a switch between the two in it. The
 * phone's own bars are hidden meanwhile, and a swipe brings them back for a
 * moment.
 *
 * Beside the floating menu, a button opens the soft keyboard, with a row of
 * the keys it lacks above it. The screen stays as it is, under both, and the
 * guest is told how much of it they cover ([GuestShell.reserve]), so that
 * its windows make way for them, as they would on a smaller screen. The
 * menu also says what a finger on the screen is, a touch or a trackpad's
 * ([PointerMode]), which the app remembers.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun VmScreen(
    guest: Guest,
    console: List<String>,
    link: Link?,
    shell: GuestShell,
    screenSize: IntSize,
    onBack: () -> Unit,
    onPause: () -> Unit,
    onStop: () -> Unit,
    onRunAgain: () -> Unit,
) {
    BackHandler(onBack = onBack)
    val activity = LocalContext.current as Activity
    val window = activity.window
    DisposableEffect(window) {
        val bars = window.insetsController
        bars?.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        bars?.hide(WindowInsets.Type.systemBars())
        onDispose { bars?.show(WindowInsets.Type.systemBars()) }
    }
    var menu by remember { mutableStateOf(false) }
    var showScreen by remember { mutableStateOf(false) }
    val preferences = remember { activity.getSharedPreferences("vm", Context.MODE_PRIVATE) }
    var pointer by remember {
        val saved = preferences.getString("pointer", null)
        mutableStateOf(PointerMode.entries.firstOrNull { it.name == saved } ?: PointerMode.TOUCH)
    }
    val keyboard = remember(link) { link?.let { link -> Keyboard { link.send(KEYBOARD_EVENTS, it) } } }
    var view by remember { mutableStateOf<GuestView?>(null) }
    val ime = ComposeInsets.ime.getBottom(LocalDensity.current)
    val extraKeys = with(LocalDensity.current) { EXTRA_KEYS_HEIGHT.roundToPx() }
    val covered = if (showScreen && link != null && ime > 0) ime + extraKeys else 0
    LaunchedEffect(covered) { shell.reserve(covered) }
    DisposableEffect(shell) { onDispose { shell.reserve(0) } }
    // The keyboard gone, by its button or by Back, the view is no editor
    // again, and a finger on the screen does not bring it back.
    LaunchedEffect(ime == 0) { if (ime == 0) view?.typing = false }
    LaunchedEffect(link) { showScreen = link != null }
    DisposableEffect(showScreen) {
        if (showScreen) activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        onDispose { activity.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED }
    }
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
    val menuButton: @Composable () -> Unit = {
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
                if (link != null) {
                    DropdownMenuItem(
                        text = { Text(if (showScreen) "Show the console" else "Show the screen") },
                        onClick = { menu = false; showScreen = !showScreen },
                    )
                }
                if (link != null && showScreen) {
                    for ((mode, label) in listOf(PointerMode.TOUCH to "Touch", PointerMode.TRACKPAD to "Trackpad")) {
                        DropdownMenuItem(
                            text = { Text(label) },
                            leadingIcon = if (pointer == mode) {
                                { Icon(Icons.Rounded.Check, null) }
                            } else {
                                null
                            },
                            onClick = {
                                menu = false
                                pointer = mode
                                preferences.edit().putString("pointer", mode.name).apply()
                            },
                        )
                    }
                }
                DropdownMenuItem(
                    text = { Text("Back to main page") },
                    leadingIcon = { Icon(Icons.AutoMirrored.Rounded.ArrowBack, null) },
                    onClick = { menu = false; onBack() },
                )
            }
        }
    }
    if (showScreen && link != null) {
        Box(Modifier.fillMaxSize().background(Color.Black)) {
            AndroidView(
                factory = { GuestView(it, screenSize.width, screenSize.height).also { view = it } },
                update = {
                    it.link = link
                    it.mode = pointer
                    it.keyboard = keyboard
                },
                modifier = Modifier.fillMaxSize(),
            )
            Row(
                Modifier
                    .align(Alignment.TopEnd)
                    .padding(12.dp)
                    .background(MaterialTheme.colorScheme.surfaceContainer.copy(alpha = 0.6f), CircleShape),
            ) {
                CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurface) {
                    IconButton(onClick = { view?.showKeyboard(ime == 0) }) {
                        Icon(KeyboardIcon, contentDescription = "Keyboard")
                    }
                    menuButton()
                }
            }
            if (ime > 0 && keyboard != null) {
                ExtraKeys(
                    keyboard,
                    Modifier
                        .align(Alignment.BottomCenter)
                        .offset { IntOffset(0, -ime) }
                        .height(EXTRA_KEYS_HEIGHT),
                )
            }
        }
        return
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
            actions = { menuButton() },
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

private val EXTRA_KEYS_HEIGHT = 44.dp

/**
 * The keys a phone's soft keyboard lacks, in a row over it, as Termux has:
 * Esc, Tab, the modifiers, the arrows and three characters it hides away.
 * Ctrl, Alt and Super are sticky: tapped, the next key is typed with them;
 * tapped twice, they stay down until tapped again.
 */
@Composable
private fun ExtraKeys(keyboard: Keyboard, modifier: Modifier) {
    // The row takes every touch on it, between its keys too, so that none
    // reaches the screen under it.
    Row(
        modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .clickable(interactionSource = null, indication = null) {},
    ) {
        val key: @Composable (String, Keyboard.StickyKey?, () -> Unit) -> Unit = { label, sticky, onTap ->
            val state = sticky?.state ?: Keyboard.Sticky.OFF
            Box(
                Modifier
                    .weight(if (label.length > 1) 1.5f else 1f)
                    .fillMaxHeight()
                    .padding(horizontal = 1.dp, vertical = 4.dp)
                    .background(
                        when (state) {
                            Keyboard.Sticky.OFF -> Color.Transparent
                            Keyboard.Sticky.ONCE -> MaterialTheme.colorScheme.secondaryContainer
                            Keyboard.Sticky.LOCKED -> MaterialTheme.colorScheme.primary
                        },
                        RoundedCornerShape(8.dp),
                    )
                    .clickable(onClick = onTap),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    label,
                    fontSize = 13.sp,
                    maxLines = 1,
                    softWrap = false,
                    color = if (state == Keyboard.Sticky.LOCKED) {
                        MaterialTheme.colorScheme.onPrimary
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
                )
            }
        }
        key("Esc", null) { keyboard.tap(Linux.KEY_ESC) }
        key("Tab", null) { keyboard.tap(Linux.KEY_TAB) }
        key("Ctrl", keyboard.ctrl) { keyboard.ctrl.tap() }
        key("Alt", keyboard.alt) { keyboard.alt.tap() }
        key("Super", keyboard.meta) { keyboard.meta.tap() }
        key("←", null) { keyboard.tap(Linux.KEY_LEFT) }
        key("↑", null) { keyboard.tap(Linux.KEY_UP) }
        key("↓", null) { keyboard.tap(Linux.KEY_DOWN) }
        key("→", null) { keyboard.tap(Linux.KEY_RIGHT) }
        key("|", null) { keyboard.text("|") }
        key("/", null) { keyboard.text("/") }
        key("-", null) { keyboard.text("-") }
    }
}

/** Material's keyboard icon, which the core icon set does not have. */
private val KeyboardIcon: ImageVector by lazy {
    ImageVector.Builder("Keyboard", 24.dp, 24.dp, 24f, 24f).addPath(
        PathParser().parsePathString(
            "M20 5H4c-1.1 0-1.99.9-1.99 2L2 17c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm-9 " +
                "3h2v2h-2V8zm0 3h2v2h-2v-2zM8 8h2v2H8V8zm0 3h2v2H8v-2zm-1 2H5v-2h2v2zm0-3H5V8h2v2zm9 " +
                "7H8v-2h8v2zm0-4h-2v-2h2v2zm0-3h-2V8h2v2zm3 3h-2v-2h2v2zm0-3h-2V8h2v2z",
        ).toNodes(),
        fill = SolidColor(Color.Black),
    ).build()
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

/**
 * The guest's shell on its console, which the app can say a line to, and
 * what the console said of the compositor's monitor.
 *
 * The one line said is how much of the screen the soft keyboard and the
 * extra-keys row cover: `hyprctl keyword monitor <name>,addreserved,0,<H>,0,0`,
 * which keeps windows out of the bottom `H` of the monitor, in the
 * compositor's logical pixels. The compositor's boot line names the monitor,
 * `hyprix: 1 monitor [card0 Virtual-1 1080x2400 1080x2400]`, but both sizes
 * in it are the card's mode, whatever the scale: taking their ratio for the
 * scale reserved a scale-2 desktop's whole height (2026-09-26). So the shell
 * is asked, `hyprctl monitors`, and its `scale: 2.00` under the monitor's
 * `Monitor <name> (ID n):` is read off the console. hyprctl refuses
 * ("Broken pipe") until the compositor is listening, so nothing is said
 * until `hyprix: card0 <name>` has been printed and two seconds have passed;
 * nothing is reserved until the scale is known, and then the latest value,
 * if it is not what the guest already has. The keyboard's height moves
 * through many values as it opens, so a value is said once it has held for
 * 150 ms.
 */
internal class GuestShell {
    @Volatile
    var input: OutputStream? = null
    private val covered = MutableStateFlow(0)
    private val listening = MutableStateFlow(false)
    @Volatile
    private var monitor = "Virtual-1"
    private val scale = MutableStateFlow<Double?>(null)
    /** The monitor whose `hyprctl monitors` block the console is in, if any. */
    @Volatile
    private var inBlock: String? = null

    /** How much of the screen, in the phone's pixels from the bottom, is covered now. */
    fun reserve(pixels: Int) {
        covered.value = pixels
    }

    /**
     * A console line: the compositor's monitor, the compositor listening, or
     * a line of `hyprctl monitors`' answer.
     */
    fun heard(line: String) {
        MONITOR.find(line)?.let { monitor = it.groupValues[1] }
        if (line.contains("hyprix: card0 $monitor ")) listening.value = true
        BLOCK.find(line)?.let { inBlock = it.groupValues[1] }
        if (inBlock == monitor) {
            SCALE.find(line)?.let { found ->
                found.groupValues[1].toDoubleOrNull()?.takeIf { it.isFinite() && it > 0 }?.let {
                    Log.i("FerrixVm", "hyprctl says $monitor is at scale $it")
                    scale.value = it
                    inBlock = null
                }
            }
        }
    }

    /** Write `line` to the guest's shell; whether it went. */
    private suspend fun say(line: String): Boolean {
        val stream = input ?: return false
        return try {
            withContext(Dispatchers.IO) {
                stream.write(line.toByteArray())
                stream.flush()
            }
            true
        } catch (error: IOException) {
            Log.w("FerrixVm", "could not tell the guest's shell", error)
            false
        }
    }

    /** Say each new [reserve] to the guest, from once it can hear it until the run ends. */
    suspend fun reserveAsAsked() {
        listening.first { it }
        delay(2000)
        // Asked again until it answers: the first ask can still meet a
        // compositor that is not listening.
        var known: Double? = null
        while (known == null) {
            Log.i("FerrixVm", "asking the guest's shell for $monitor's scale")
            say("hyprctl monitors\n")
            known = withTimeoutOrNull(3000) { scale.first { it != null } }
        }
        val factor = known ?: return
        Log.i("FerrixVm", "the guest's $monitor is at scale $factor")
        var said = 0
        covered.collectLatest { pixels ->
            delay(150)
            val logical = ceil(pixels / factor).toInt()
            if (logical == said) return@collectLatest
            if (say("hyprctl keyword monitor $monitor,addreserved,0,$logical,0,0 >/dev/null 2>&1\n")) {
                said = logical
                Log.i("FerrixVm", "reserved $logical logical pixels (of $pixels) at the bottom of $monitor")
            }
        }
    }

    private companion object {
        val MONITOR = Regex("""hyprix: \d+ monitors? \[\S+ (\S+) \d+x\d+""")
        val BLOCK = Regex("""Monitor (\S+) \(ID \d+\):""")
        val SCALE = Regex("""\bscale: ([0-9.]+)""")
    }
}

/**
 * Run the guest with `command` to its end, appending its console to
 * `console`, and the bridge's lines with it; `shell` has the guest's
 * console input meanwhile, and hears each line.
 */
private suspend fun runGuest(
    console: MutableList<String>,
    shell: GuestShell,
    command: String,
    started: (Int) -> Unit,
): Guest =
    withContext(Dispatchers.IO) {
        val began = System.nanoTime()
        var result: String? = null
        val process = try {
            ProcessBuilder("su", "-c", command).start()
        } catch (error: IOException) {
            return@withContext Guest.Ended("could not run su: ${error.message}", 0, false)
        }
        shell.input = process.outputStream
        process.inputStream.bufferedReader().useLines { lines ->
            for (raw in lines) {
                val line = raw.trimEnd('\r')
                if (line.startsWith("FERRIX-VM-PID ")) {
                    line.removePrefix("FERRIX-VM-PID ").toIntOrNull()?.let {
                        withContext(Dispatchers.Main) { started(it) }
                    }
                    continue
                }
                if (line.startsWith("FERRIX-VM-BRIDGE ")) Log.i("FerrixVm", line)
                shell.heard(line)
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
        shell.input = null
        try {
            process.outputStream.close()
        } catch (_: IOException) {
        }
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
