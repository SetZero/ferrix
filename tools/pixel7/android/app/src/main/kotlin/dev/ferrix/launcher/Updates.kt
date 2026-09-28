package dev.ferrix.launcher

import android.content.Context
import android.net.ConnectivityManager
import android.os.StatFs
import android.util.Log
import java.io.BufferedInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.util.zip.GZIPInputStream
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

private const val TAG = "FerrixUpdate"

/** Where the releases are; the repository is public, so no token is needed. */
private const val RELEASES = "https://api.github.com/repos/SetZero/ferrix/releases?per_page=30"

/**
 * A debug build's stand-in for [RELEASES], kept in the preferences: a list
 * in GitHub's shape served from the PC, to try a release's assets before a
 * tag publishes them (README.md, "Updates"). Set with
 * `am start -n dev.ferrix.launcher/.MainActivity --es releases <url>`, and
 * cleared with an empty one.
 */
internal const val RELEASES_OVERRIDE = "releases"

/** The app's record, beside the files, of the release it put there. */
private const val RECORD = "release.json"
private const val IMAGE = "desktop.Image"
private const val VOLUME = "chromium.img"

/** Room left on /data after a download, so that an update never fills the phone. */
private const val MARGIN = 256L shl 20

/**
 * The desktop the phone keeps, as a distribution has its editions: [FULL]
 * with Chromium, whose volume comes with it, and [MINIMAL], the desktop
 * alone. A release carries each as `ferrix-pixel7-<id>.json` and the assets
 * it names (`tools/pixel7/package-release.py`).
 */
internal enum class Edition(val id: String, val label: String) {
    FULL("full", "Full"),
    MINIMAL("minimal", "Minimal"),
}

/** A file of a release: where it is, gzipped, and what it unpacks to. */
internal data class Asset(val url: String, val sha256: String, val size: Long, val download: Long)

/**
 * The newest release that carries [edition]. [pins] names Chromium's volume
 * by the packages on it, since two volumes of the same packages are never
 * the same bytes.
 */
internal data class Release(
    val tag: String,
    val edition: Edition,
    val image: Asset,
    val volume: Asset?,
    val pins: String?,
)

/** What the app last put in the VM's directory, from [RECORD]. */
internal data class Record(val edition: String, val tag: String, val image: String?, val pins: String?) {
    fun json(): String = JSONObject()
        .put("edition", edition)
        .put("tag", tag)
        .put("image", image ?: JSONObject.NULL)
        .put("pins", pins ?: JSONObject.NULL)
        .toString()
}

/**
 * The VM's directory as it is: the SHA-256 of `desktop.Image`, whether
 * `chromium.img` is there, and the app's record. A `desktop.Image` whose
 * hash is not the record's was put there by other hands, `build-desktop.sh
 * --push` above all, which also removes the record: that build is not
 * replaced unless asked.
 */
internal data class Installed(val image: String?, val volume: Boolean, val record: Record?) {
    val imageIsOurs get() = image == null || image == record?.image
}

/** One file to fetch: the asset and the name it takes in the VM's directory. */
internal data class Step(val asset: Asset, val name: String, val label: String)

/**
 * What `release` needs fetched, Chromium's volume first. The desktop, when
 * its hash is not the one there. Chromium's volume, for [Edition.FULL],
 * when it is missing or the app put one of other pins there; a volume the
 * app did not put there is the PC's and is kept, unless the person asked
 * for the release (`forced`), since it holds Chromium's profile.
 */
internal fun plan(release: Release, installed: Installed, forced: Boolean): List<Step> = buildList {
    val volume = release.volume
    if (volume != null) {
        val pins = installed.record?.pins
        val wanted = !installed.volume || (pins != null && pins != release.pins) || (pins == null && forced)
        if (wanted) add(Step(volume, VOLUME, "Chromium's volume"))
    }
    if (installed.image != release.image.sha256) add(Step(release.image, IMAGE, "the desktop"))
}

/**
 * Copy `input` to `output`, as [Updater] does a download to the phone's
 * file: `check` before each block, which throws to stop, and `progress`
 * with the bytes so far; the byte count and SHA-256 of what went through.
 */
internal fun copyHashing(
    input: InputStream,
    output: OutputStream,
    check: () -> Unit = {},
    progress: (Long) -> Unit = {},
): Pair<Long, String> {
    val digest = MessageDigest.getInstance("SHA-256")
    val buffer = ByteArray(1 shl 16)
    var done = 0L
    while (true) {
        check()
        val read = input.read(buffer)
        if (read < 0) break
        digest.update(buffer, 0, read)
        output.write(buffer, 0, read)
        done += read
        progress(done)
    }
    return done to digest.digest().joinToString("") { "%02x".format(it) }
}

/** Where the updates stand, for the screen. */
internal sealed interface Update {
    data object Unknown : Update
    data object Checking : Update
    data class Current(val tag: String, val edition: Edition) : Update
    data class NoRelease(val edition: Edition, val desktop: Boolean) : Update
    data class PcBuild(val release: Release, val bytes: Long) : Update
    data class Waiting(val release: Release, val bytes: Long, val why: String, val canForce: Boolean) : Update
    data class Downloading(val release: Release, val label: String, val fraction: Float, val bytes: Long) : Update
    data class Failed(val why: String) : Update
}

/**
 * Keeps the VM's desktop on the newest GitHub release that carries the
 * chosen edition.
 *
 * A check asks GitHub's releases, newest first, for the first one whose
 * manifest and assets are all there, which skips a release whose phone job
 * failed or is still uploading. What is in the VM's directory is read with
 * `su`, as the VM is run. On an unmetered network a newer desktop, and for
 * [Edition.FULL] a volume whose pins moved or which is missing, is fetched
 * by itself; on a metered one, or over a build from the PC, it waits for a
 * tap ([install] with `forced`).
 *
 * Each file is unpacked as it downloads, straight into a `su cat` process
 * that writes `.<name>.part` beside the running VM's files, and hashed on
 * the way; only a file whose hash and size are the manifest's is renamed
 * over the old one. A running guest keeps the file it opened, so an update
 * can land under it and takes effect at the next run.
 */
internal class Updater(context: Context) {
    private val context = context.applicationContext
    private val preferences = context.getSharedPreferences("vm", Context.MODE_PRIVATE)
    private val busy = Mutex()
    private val mutable = MutableStateFlow<Update>(Update.Unknown)
    val state: StateFlow<Update> = mutable

    var edition: Edition
        get() = Edition.entries.firstOrNull { it.id == preferences.getString("edition", null) } ?: Edition.FULL
        set(value) {
            preferences.edit().putString("edition", value.id).apply()
        }

    /** When the last check ended, in `System.currentTimeMillis`. */
    @Volatile
    var checked = 0L
        private set

    /**
     * Check, and fetch what is newer when nothing needs asking. A call
     * while another is under way does nothing.
     */
    suspend fun check() {
        if (!busy.tryLock()) return
        try {
            mutable.value = Update.Checking
            mutable.value = run(forced = false)
        } finally {
            checked = System.currentTimeMillis()
            busy.unlock()
        }
    }

    /**
     * Fetch the newest release now, over the metered network or a build
     * from the PC: the person asked, with its size in front of them.
     */
    suspend fun install() {
        if (!busy.tryLock()) return
        try {
            mutable.value = Update.Checking
            mutable.value = run(forced = true)
        } finally {
            checked = System.currentTimeMillis()
            busy.unlock()
        }
    }

    private suspend fun run(forced: Boolean): Update = withContext(Dispatchers.IO) {
        val edition = edition
        try {
            val installed = installed() ?: return@withContext Update.Failed("root was not granted")
            val release = latest(edition)
                ?: return@withContext Update.NoRelease(edition, installed.image != null)
            val steps = plan(release, installed, forced)
            if (steps.isEmpty()) {
                return@withContext Update.Current(release.tag, edition)
            }
            val bytes = steps.sumOf { it.asset.download }
            val room = StatFs(context.filesDir.path).availableBytes
            val needed = steps.sumOf { it.asset.size } + MARGIN
            if (room < needed) {
                return@withContext Update.Waiting(
                    release, bytes, "needs ${megabytes(needed)} free on the phone, which has ${megabytes(room)}",
                    canForce = false,
                )
            }
            if (!forced && !installed.imageIsOurs) return@withContext Update.PcBuild(release, bytes)
            if (!forced && metered()) {
                return@withContext Update.Waiting(release, bytes, "on a metered network", canForce = true)
            }
            // The record changes a file's line only once that file is in
            // place, so a PC's desktop is never taken for the app's by an
            // update that stopped halfway.
            var record = installed.record ?: Record(edition.id, "", null, null)
            for (step in steps) {
                place(release, step)
                record = if (step.name == VOLUME) {
                    record.copy(pins = release.pins)
                } else {
                    record.copy(edition = edition.id, tag = release.tag, image = step.asset.sha256)
                }
                write(record)
                Log.i(TAG, "put ${release.tag}'s ${step.name} in place")
            }
            Update.Current(release.tag, edition)
        } catch (error: IOException) {
            Log.w(TAG, "update failed", error)
            Update.Failed(error.message ?: error.javaClass.simpleName)
        } catch (error: JSONException) {
            Update.Failed("GitHub answered with something unexpected: ${error.message}")
        }
    }

    /** The newest release with this edition's manifest and every asset it names, or none. */
    private fun latest(edition: Edition): Release? {
        val releases = JSONArray(fetch(preferences.getString(RELEASES_OVERRIDE, null) ?: RELEASES, api = true))
        for (index in 0 until releases.length()) {
            val release = releases.getJSONObject(index)
            if (release.optBoolean("draft")) continue
            val assets = release.optJSONArray("assets") ?: continue
            val named = (0 until assets.length()).map { assets.getJSONObject(it) }.associateBy { it.optString("name") }
            val manifest = named["ferrix-pixel7-${edition.id}.json"] ?: continue
            val entries = JSONObject(fetch(manifest.getString("browser_download_url"), api = false))
            if (entries.optInt("format") != 1) continue
            fun asset(entry: JSONObject): Asset? {
                val file = named[entry.optString("asset")] ?: return null
                return Asset(
                    url = file.getString("browser_download_url"),
                    sha256 = entry.getString("sha256"),
                    size = entry.getLong("size"),
                    download = file.getLong("size"),
                )
            }
            val image = asset(entries.getJSONObject("image")) ?: continue
            val volumeEntry = entries.optJSONObject("volume")
            val volume = volumeEntry?.let { asset(it) }
            if (volumeEntry != null && volume == null) continue
            return Release(release.getString("tag_name"), edition, image, volume, volumeEntry?.optString("pins"))
        }
        return null
    }

    /** The VM's directory, read as root, or null when root is refused. */
    private fun installed(): Installed? {
        val process = ProcessBuilder(
            "su", "-c",
            "cd $VM_DIR 2>/dev/null || exit 0; " +
                "[ ! -f $RECORD ] || { printf 'R '; tr -d '\\n' < $RECORD; echo; }; " +
                "[ ! -f $IMAGE ] || { printf 'I '; sha256sum $IMAGE; }; " +
                "[ ! -f $VOLUME ] || echo V",
        ).start()
        val lines = process.inputStream.bufferedReader().readLines()
        if (process.waitFor() != 0) return null
        var record: Record? = null
        var image: String? = null
        var volume = false
        for (line in lines) {
            when {
                line.startsWith("R ") -> record = try {
                    val json = JSONObject(line.removePrefix("R "))
                    Record(
                        json.optString("edition"),
                        json.optString("tag"),
                        json.optString("image").takeUnless { json.isNull("image") },
                        json.optString("pins").takeUnless { json.isNull("pins") },
                    )
                } catch (_: JSONException) {
                    null
                }
                line.startsWith("I ") -> image = line.removePrefix("I ").substringBefore(' ')
                line == "V" -> volume = true
            }
        }
        return Installed(image, volume, record)
    }

    /**
     * Download `step`'s asset, unpack it into `.<name>.part` in the VM's
     * directory, and rename it into place once its size and hash are the
     * manifest's.
     */
    private suspend fun place(release: Release, step: Step) {
        val part = "$VM_DIR/.${step.name}.part"
        val job = currentCoroutineContext()
        // The directory as the helper makes it, so that adb's pushes still land.
        val writer = ProcessBuilder("su", "-c", "mkdir -p $VM_DIR && chmod 777 $VM_DIR && cat > $part").start()
        var said = 0L
        val (done, hash) = try {
            val connection = open(step.asset.url, api = false)
            val copied = try {
                GZIPInputStream(BufferedInputStream(connection.inputStream, 1 shl 16), 1 shl 16).use { input ->
                    writer.outputStream.use { output ->
                        copyHashing(input, output, check = { job.ensureActive() }) { done ->
                            if (done - said >= 1 shl 20) {
                                said = done
                                mutable.value = Update.Downloading(
                                    release, step.label, done.toFloat() / step.asset.size, step.asset.download,
                                )
                            }
                        }
                    }
                }
            } finally {
                connection.disconnect()
            }
            if (writer.waitFor() != 0) throw IOException("could not write ${step.name} on the phone")
            copied
        } catch (error: Throwable) {
            writer.destroy()
            root("rm -f $part")
            throw error
        }
        if (done != step.asset.size || hash != step.asset.sha256) {
            root("rm -f $part")
            throw IOException("${step.name} arrived damaged: $done bytes hashing to ${hash.take(12)}…")
        }
        if (root("chmod 644 $part && mv -f $part $VM_DIR/${step.name}") != 0) {
            throw IOException("could not put ${step.name} in place")
        }
    }

    private fun write(record: Record) {
        val writer = ProcessBuilder(
            "su", "-c", "cat > $VM_DIR/.$RECORD.part && mv -f $VM_DIR/.$RECORD.part $VM_DIR/$RECORD",
        ).start()
        writer.outputStream.use { it.write(record.json().toByteArray()) }
        if (writer.waitFor() != 0) throw IOException("could not write $RECORD")
    }

    private fun root(command: String): Int = ProcessBuilder("su", "-c", command).start().waitFor()

    private fun metered(): Boolean =
        context.getSystemService(ConnectivityManager::class.java)?.isActiveNetworkMetered ?: true

    private fun open(url: String, api: Boolean): HttpURLConnection {
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.connectTimeout = 10_000
        connection.readTimeout = 30_000
        connection.setRequestProperty("User-Agent", "ferrix-launcher")
        if (api) connection.setRequestProperty("Accept", "application/vnd.github+json")
        val code = connection.responseCode
        if (code != 200) {
            connection.disconnect()
            throw IOException(
                if (code == 403 || code == 429) "GitHub's rate limit; try again in an hour" else "GitHub answered $code for $url",
            )
        }
        return connection
    }

    private fun fetch(url: String, api: Boolean): String {
        val connection = open(url, api)
        try {
            return connection.inputStream.bufferedReader().use { it.readText() }
        } finally {
            connection.disconnect()
        }
    }
}

internal fun megabytes(bytes: Long): String = "${(bytes + (1 shl 19)) shr 20} MB"
