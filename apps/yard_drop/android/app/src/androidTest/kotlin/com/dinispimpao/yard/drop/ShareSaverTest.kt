package com.dinispimpao.yard.drop

import android.content.ContentUris
import android.content.Context
import android.net.Uri
import android.os.Environment
import android.provider.MediaStore
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.json.JSONObject
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

private val SAVED_PATH = "${Environment.DIRECTORY_DOWNLOADS}/YARD Drop/Saved/"
private val INBOX_PATH = "${Environment.DIRECTORY_DOWNLOADS}/YARD Drop/Inbox/"

private val PRODUCER = Producer(appVersion = "0.4.0", buildNumber = 142, commit = "abc1234")

@RunWith(AndroidJUnit4::class)
class ShareSaverTest {

    private lateinit var context: Context
    private lateinit var stager: ShareStager
    private lateinit var saver: ShareSaver

    @Before
    fun setUp() {
        context = InstrumentationRegistry.getInstrumentation().targetContext
        stager = ShareStager(context)
        saver = ShareSaver(stager, MediaStoreWriter(context))
        clearShares()
        clearDownloads()
    }

    @After
    fun tearDown() {
        clearShares()
        clearDownloads()
        incomingRoot(context).deleteRecursively()
    }

    @Test
    fun savesABatchAndLetsGoOfItsStaging() {
        val share = staged("first.txt" to 2_000L, "second.txt" to 3_000L)

        val outcome = saver.save(share, "downloads", PRODUCER)

        assertEquals("Download/YARD Drop/Saved", (outcome as SaveOutcome.Success).location)
        val saved = IncomingShareStore.get(share.id)!!
        assertEquals(ShareState.SAVED, saved.state)
        assertEquals(1.0, saved.progress!!, 0.0001)
        assertTrue(saved.files.all { it.state == FileState.SAVED })
        assertEquals(setOf("first.txt", "second.txt"), downloadNames())
        assertEquals(2_000L, downloadSize("first.txt"))

        // Nothing staged outlives a save that worked.
        assertFalse(File(incomingRoot(context), share.id).exists())
    }

    @Test
    fun aDuplicateNameIsSavedUnderADifferentOne() {
        saver.save(staged("build.apk" to 1_000L), "downloads", PRODUCER)
        val second = staged("build.apk" to 1_000L)

        saver.save(second, "downloads", PRODUCER)

        val savedName = IncomingShareStore.get(second.id)!!.files.single().savedName!!
        assertNotEquals("build.apk", savedName)
        // The user is told the name the row got, so both files are still there.
        assertEquals(setOf("build.apk", savedName), downloadNames())
    }

    @Test
    fun aHostileNameCannotEscapeTheFolder() {
        val share = staged("../../../etc/passwd" to 100L)

        saver.save(share, "downloads", PRODUCER)

        val savedName = IncomingShareStore.get(share.id)!!.files.single().savedName!!
        assertFalse(savedName.contains("/"))
        assertEquals(setOf(savedName), downloadNames())
    }

    @Test
    fun oneFailedFileDoesNotWithdrawTheOnesThatSaved() {
        val share = staged("good.bin" to 500L, "lost.bin" to 500L)
        // A staged file disappearing under us is the failure a user can retry.
        File(share.files[1].stagedPath!!).delete()

        val outcome = saver.save(share, "downloads", PRODUCER)

        assertTrue(outcome is SaveOutcome.Failure)
        val after = IncomingShareStore.get(share.id)!!
        assertEquals(ShareState.FAILED, after.state)
        assertEquals(FileState.SAVED, after.files[0].state)
        assertEquals(FileState.FAILED, after.files[1].state)
        assertEquals(setOf("good.bin"), downloadNames())
        assertTrue(after.error!!.contains("Download/YARD Drop/Saved"))
    }

    @Test
    fun aRetrySavesOnlyWhatIsLeft() {
        val share = staged("kept.bin" to 400L, "retried.bin" to 400L)
        val failed = share.copy(
            files = listOf(
                share.files[0].copy(state = FileState.SAVED, savedName = "kept.bin"),
                share.files[1],
            ),
        )

        val outcome = saver.save(failed, "downloads", PRODUCER)

        assertTrue(outcome is SaveOutcome.Success)
        // The first file was already in Downloads; a retry must not write it twice.
        assertEquals(setOf("retried.bin"), downloadNames())
    }

    @Test
    fun anUnknownDestinationWritesNothing() {
        val outcome = saver.save(staged("a.bin" to 10L), "carrierPigeon", PRODUCER)

        assertTrue(outcome is SaveOutcome.Failure)
        assertEquals("unsupported_destination", (outcome as SaveOutcome.Failure).code)
        assertTrue(downloadNames().isEmpty())
    }

    @Test
    fun aBrowserBatchCarriesItsFilesAManifestAndAMarker() {
        val share = staged("report.zip" to 1_200L, "notes.pdf" to 800L)

        val outcome = saver.save(share, "browserInbox", PRODUCER)

        val location = (outcome as SaveOutcome.Success).location
        assertTrue(location.startsWith("Download/YARD Drop/Inbox/"))
        assertEquals(
            setOf("report.zip", "notes.pdf", BATCH_MANIFEST, BATCH_READY_MARKER),
            inboxNames(),
        )
        // Saved-to-device wording would name a folder; this destination does not.
        assertEquals(ShareState.SAVED, IncomingShareStore.get(share.id)!!.state)
        assertFalse(File(incomingRoot(context), share.id).exists())

        val manifest = JSONObject(readInbox(BATCH_MANIFEST))
        assertEquals(BATCH_SCHEMA_VERSION, manifest.getInt("schemaVersion"))
        assertEquals(location.substringAfterLast('/'), manifest.getString("batchId"))
        val producer = manifest.getJSONObject("producer")
        assertEquals("0.4.0", producer.getString("appVersion"))
        assertEquals(142, producer.getInt("buildNumber"))
        assertEquals("abc1234", producer.getString("commit"))

        val named = (0 until manifest.getJSONArray("files").length())
            .map { manifest.getJSONArray("files").getJSONObject(it) }
            .associate { it.getString("name") to it.getLong("size") }
        assertEquals(mapOf("report.zip" to 1_200L, "notes.pdf" to 800L), named)
    }

    @Test
    fun aFailedBatchLeavesNothingAReaderCouldPickUp() {
        val share = staged("good.bin" to 500L, "lost.bin" to 500L)
        File(share.files[1].stagedPath!!).delete()

        val outcome = saver.save(share, "browserInbox", PRODUCER)

        assertTrue(outcome is SaveOutcome.Failure)
        // Not even the file that copied: half a batch is one nobody may read.
        assertTrue(inboxNames().isEmpty())

        // The staged bytes stay, and the file that did publish is pending again,
        // so a retry writes the whole batch into a fresh directory.
        val after = IncomingShareStore.get(share.id)!!
        assertEquals(FileState.PENDING, after.files[0].state)
        assertTrue(File(after.files[0].stagedPath!!).exists())
    }

    @Test
    fun eachBrowserBatchGetsItsOwnDirectory() {
        val first = saver.save(staged("a.bin" to 10L), "browserInbox", PRODUCER)
        val second = saver.save(staged("a.bin" to 10L), "browserInbox", PRODUCER)

        assertNotEquals(
            (first as SaveOutcome.Success).location,
            (second as SaveOutcome.Success).location,
        )
        // Same name, different batch: neither had to be uniquified.
        assertEquals(2, inboxRows().count { it.first == "a.bin" })
    }

    /** Stages real bytes through the fake provider, as a real share would. */
    private fun staged(vararg files: Pair<String, Long>): IncomingShare {
        val share = IncomingShare(
            id = IncomingShare.newId(),
            receivedAtMillis = System.currentTimeMillis(),
            files = files.map { (name, size) ->
                IncomingFile(
                    id = IncomingShare.newId(),
                    displayName = name,
                    mimeType = "application/octet-stream",
                    reportedSize = size,
                    source = FakeFile(bytes = size, displayName = name).uri(),
                )
            },
            state = ShareState.RECEIVING,
        )
        IncomingShareStore.put(share)
        stager.stage(share)
        return IncomingShareStore.get(share.id)!!
    }

    private fun downloadRows(): Map<String, Long> {
        val rows = mutableMapOf<String, Long>()
        context.contentResolver.query(
            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
            arrayOf(MediaStore.Downloads.DISPLAY_NAME, MediaStore.Downloads.SIZE),
            "${MediaStore.Downloads.RELATIVE_PATH} = ?",
            arrayOf(SAVED_PATH),
            null,
        )?.use { cursor ->
            while (cursor.moveToNext()) rows[cursor.getString(0)] = cursor.getLong(1)
        }
        return rows
    }

    private fun downloadNames() = downloadRows().keys

    private fun downloadSize(name: String) = downloadRows().getValue(name)

    /** Every row under any batch directory, so a test can see the whole inbox. */
    private fun inboxRows(): List<Pair<String, Uri>> {
        val rows = mutableListOf<Pair<String, Uri>>()
        context.contentResolver.query(
            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
            arrayOf(MediaStore.Downloads._ID, MediaStore.Downloads.DISPLAY_NAME),
            "${MediaStore.Downloads.RELATIVE_PATH} LIKE ?",
            arrayOf("$INBOX_PATH%"),
            null,
        )?.use { cursor ->
            while (cursor.moveToNext()) {
                rows += cursor.getString(1) to ContentUris.withAppendedId(
                    MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                    cursor.getLong(0),
                )
            }
        }
        return rows
    }

    private fun inboxNames() = inboxRows().map { it.first }.toSet()

    private fun readInbox(name: String): String {
        val uri = inboxRows().first { it.first == name }.second
        return context.contentResolver.openInputStream(uri)!!.use { it.readBytes().decodeToString() }
    }

    private fun clearDownloads() {
        context.contentResolver.delete(
            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
            "${MediaStore.Downloads.RELATIVE_PATH} = ?",
            arrayOf(SAVED_PATH),
        )
        context.contentResolver.delete(
            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
            "${MediaStore.Downloads.RELATIVE_PATH} LIKE ?",
            arrayOf("$INBOX_PATH%"),
        )
    }
}
