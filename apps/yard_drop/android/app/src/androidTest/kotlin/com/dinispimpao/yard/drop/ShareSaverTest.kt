package com.dinispimpao.yard.drop

import android.content.Context
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
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

private val SAVED_PATH = "${Environment.DIRECTORY_DOWNLOADS}/YARD Drop/Saved/"

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

        val outcome = saver.save(share, "downloads")

        assertEquals(SaveOutcome.Success, outcome)
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
        saver.save(staged("build.apk" to 1_000L), "downloads")
        val second = staged("build.apk" to 1_000L)

        saver.save(second, "downloads")

        val savedName = IncomingShareStore.get(second.id)!!.files.single().savedName!!
        assertNotEquals("build.apk", savedName)
        // The user is told the name the row got, so both files are still there.
        assertEquals(setOf("build.apk", savedName), downloadNames())
    }

    @Test
    fun aHostileNameCannotEscapeTheFolder() {
        val share = staged("../../../etc/passwd" to 100L)

        saver.save(share, "downloads")

        val savedName = IncomingShareStore.get(share.id)!!.files.single().savedName!!
        assertFalse(savedName.contains("/"))
        assertEquals(setOf(savedName), downloadNames())
    }

    @Test
    fun oneFailedFileDoesNotWithdrawTheOnesThatSaved() {
        val share = staged("good.bin" to 500L, "lost.bin" to 500L)
        // A staged file disappearing under us is the failure a user can retry.
        File(share.files[1].stagedPath!!).delete()

        val outcome = saver.save(share, "downloads")

        assertTrue(outcome is SaveOutcome.Failure)
        val after = IncomingShareStore.get(share.id)!!
        assertEquals(ShareState.FAILED, after.state)
        assertEquals(FileState.SAVED, after.files[0].state)
        assertEquals(FileState.FAILED, after.files[1].state)
        assertEquals(setOf("good.bin"), downloadNames())
        assertTrue(after.error!!.contains(saver.savedLocation))
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

        val outcome = saver.save(failed, "downloads")

        assertEquals(SaveOutcome.Success, outcome)
        // The first file was already in Downloads; a retry must not write it twice.
        assertEquals(setOf("retried.bin"), downloadNames())
    }

    @Test
    fun theBrowserDestinationIsNotAvailableYet() {
        val outcome = saver.save(staged("a.bin" to 10L), "browserInbox")

        assertTrue(outcome is SaveOutcome.Failure)
        assertEquals("unsupported_destination", (outcome as SaveOutcome.Failure).code)
        assertTrue(downloadNames().isEmpty())
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

    private fun clearDownloads() {
        context.contentResolver.delete(
            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
            "${MediaStore.Downloads.RELATIVE_PATH} = ?",
            arrayOf(SAVED_PATH),
        )
    }
}
