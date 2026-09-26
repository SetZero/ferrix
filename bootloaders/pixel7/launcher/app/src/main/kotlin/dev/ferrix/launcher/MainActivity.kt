package dev.ferrix.launcher

import android.os.Bundle
import androidx.activity.ComponentActivity
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
import androidx.compose.material.icons.rounded.CheckCircle
import androidx.compose.material.icons.rounded.Info
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
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
 * One button that reboots the phone into Ferrix.
 *
 * The phone cannot start Ferrix by itself, so the button asks the helper on the
 * PC (`bootloaders/pixel7/launcher/helper.py`), which adb reverse makes
 * reachable at 127.0.0.1 over the USB cable. The helper runs `fastboot boot`:
 * nothing is written to the phone, and Ferrix's watchdog brings Android back
 * about 75 seconds later.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent { LauncherTheme { Launcher() } }
    }
}

private const val HELPER = "http://127.0.0.1:47707"

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
    val scope = rememberCoroutineScope()
    val lifecycle = LocalLifecycleOwner.current.lifecycle

    LaunchedEffect(lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                helper = poll()
                delay(2000)
            }
        }
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
            Spacer(Modifier.height(8.dp))
            Text(
                "Nothing on the phone is changed: Ferrix runs from RAM, sent by the " +
                    "PC with fastboot boot, and Android comes back by itself.",
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
