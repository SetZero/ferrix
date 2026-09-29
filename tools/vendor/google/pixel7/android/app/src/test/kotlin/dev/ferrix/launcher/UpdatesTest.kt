package dev.ferrix.launcher

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.security.MessageDigest
import java.util.zip.GZIPInputStream
import java.util.zip.GZIPOutputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What an update fetches, and above all what it leaves alone: a desktop and
 * a Chromium volume put on the phone by other hands than the app's.
 */
class UpdatesTest {
    private val image = Asset("https://example/full.Image.gz", "new-image", 35_000_000, 17_000_000)
    private val volume = Asset("https://example/chromium.img.gz", "new-volume", 1_030_000_000, 202_000_000)
    private val full = Release("stage-12", Edition.FULL, image, volume, "pins-b")
    private val minimal = Release("stage-12", Edition.MINIMAL, image, null, null)

    private fun names(release: Release, installed: Installed, forced: Boolean = false) =
        plan(release, installed, forced).map { it.name }

    @Test
    fun anEmptyPhoneGetsTheVolumeFirstThenTheDesktop() {
        assertEquals(listOf("chromium.img", "desktop.Image"), names(full, Installed(null, false, null)))
    }

    @Test
    fun theReleaseAlreadyThereIsLeftAsItIs() {
        val record = Record("full", "stage-12", "new-image", "pins-b")
        assertEquals(emptyList<String>(), names(full, Installed("new-image", true, record)))
    }

    @Test
    fun aNewReleaseOfTheSamePinsFetchesTheDesktopAlone() {
        val record = Record("full", "stage-11", "old-image", "pins-b")
        assertEquals(listOf("desktop.Image"), names(full, Installed("old-image", true, record)))
    }

    @Test
    fun movedPinsFetchTheVolumeAgain() {
        val record = Record("full", "stage-12", "new-image", "pins-a")
        assertEquals(listOf("chromium.img"), names(full, Installed("new-image", true, record)))
    }

    @Test
    fun aVolumeTheAppDidNotPutThereIsKept() {
        // build-desktop.sh --push: its desktop and volume, and no record.
        val installed = Installed("pc-image", true, null)
        assertFalse(installed.imageIsOurs)
        assertEquals(listOf("desktop.Image"), names(full, installed))
    }

    @Test
    fun askingForTheReleaseReplacesThePcsVolumeToo() {
        assertEquals(
            listOf("chromium.img", "desktop.Image"),
            names(full, Installed("pc-image", true, null), forced = true),
        )
    }

    @Test
    fun aMissingVolumeIsFetchedWhateverTheRecordSays() {
        val record = Record("full", "stage-12", "new-image", "pins-b")
        assertEquals(listOf("chromium.img"), names(full, Installed("new-image", false, record)))
    }

    @Test
    fun minimalNeverTouchesTheVolume() {
        assertEquals(listOf("desktop.Image"), names(minimal, Installed(null, false, null), forced = true))
        assertEquals(listOf("desktop.Image"), names(minimal, Installed("pc-image", true, null), forced = true))
    }

    @Test
    fun aDesktopIsTheAppsOnlyWhenItsHashIsTheRecords() {
        assertTrue(Installed(null, false, null).imageIsOurs)
        assertTrue(Installed("a", false, Record("full", "t", "a", null)).imageIsOurs)
        assertFalse(Installed("b", false, Record("full", "t", "a", null)).imageIsOurs)
        // An update that fetched the volume and stopped before the desktop.
        assertFalse(Installed("pc-image", true, Record("full", "", null, "pins-b")).imageIsOurs)
    }

    @Test
    fun copyHashingUnpacksAndHashesWhatPackageReleaseWrites() {
        val data = ByteArray(3 shl 20) { (it * 31 + it / 7).toByte() }
        val packed = ByteArrayOutputStream().also { GZIPOutputStream(it).use { gz -> gz.write(data) } }.toByteArray()
        val out = ByteArrayOutputStream()
        var last = 0L
        val (count, hash) = copyHashing(GZIPInputStream(ByteArrayInputStream(packed)), out, progress = { last = it })
        val expected = MessageDigest.getInstance("SHA-256").digest(data).joinToString("") { "%02x".format(it) }
        assertEquals(data.size.toLong(), count)
        assertEquals(data.size.toLong(), last)
        assertEquals(expected, hash)
        assertTrue(data.contentEquals(out.toByteArray()))
    }

    @Test(expected = IllegalStateException::class)
    fun copyHashingStopsWhenTold() {
        copyHashing(ByteArrayInputStream(ByteArray(1 shl 20)), ByteArrayOutputStream(), check = { error("stopped") })
    }
}
