package com.dinispimpao.yard.drop

import android.content.Context
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ShareStagerTest {

    private lateinit var context: Context
    private lateinit var stager: ShareStager

    @Before
    fun setUp() {
        context = InstrumentationRegistry.getInstrumentation().targetContext
        stager = ShareStager(context)
        clearShares()
        incomingRoot(context).deleteRecursively()
    }

    @After
    fun tearDown() {
        clearShares()
        incomingRoot(context).deleteRecursively()
    }

    @Test
    fun stagesAFiniteStream() {
        val source = FakeFile(bytes = 300_000, displayName = "notes.txt", mimeType = "text/plain")
        val share = stage(source)

        assertEquals(ShareState.READY, share.state)
        val file = share.files.single()
        assertEquals(FileState.PENDING, file.state)
        assertEquals(300_000L, file.stagedSize!!)
        // The grant is gone once the bytes are ours, so nothing may still hold it.
        assertNull(file.source)

        val staged = File(file.stagedPath!!)
        assertEquals(300_000L, staged.length())
        val bytes = staged.readBytes()
        assertEquals(source.byteAt(0), bytes[0])
        assertEquals(source.byteAt(299_999), bytes[299_999])
    }

    @Test
    fun stagesAStreamOfUnknownLength() {
        val source = FakeFile(bytes = 128_000, reportsSize = false)
        val share = stage(source)

        val file = share.files.single()
        assertEquals(ShareState.READY, share.state)
        // What the sender claimed is absent; what was copied is still known.
        assertNull(file.reportedSize)
        assertEquals(128_000L, file.stagedSize!!)
    }

    @Test
    fun namesAFileTheSenderDidNotName() {
        val source = FakeFile(bytes = 1_000, displayName = null, mimeType = "image/png")
        val described = ShareIntentParser.describe(context.contentResolver, source.uri())

        assertTrue(described.displayName.startsWith("shared-"))
        assertTrue(described.displayName.endsWith(".png"))
        assertEquals(ShareState.READY, stage(source).state)
    }

    @Test
    fun aStreamThatBreaksPartwayFailsOnlyItsOwnFile() {
        val broken = FakeFile(bytes = 5_000_000, displayName = "broken.bin", failsAfter = 64 * 1024)
        val good = FakeFile(bytes = 2_000, displayName = "good.bin")
        val share = stage(broken, good)

        // One survivor is still worth offering, so the batch stays usable.
        assertEquals(ShareState.READY, share.state)
        val failed = share.files.first()
        assertEquals(FileState.FAILED, failed.state)
        assertNotNull(failed.error)
        assertEquals(FileState.PENDING, share.files[1].state)

        // A half-written copy must not survive as something the user could save.
        val leftovers = File(incomingRoot(context), share.id).listFiles().orEmpty()
        assertTrue(leftovers.none { it.name.endsWith(".part") })
        assertTrue(leftovers.none { it.name == failed.id })
    }

    @Test
    fun aUriWithoutAReadGrantFailsTheShare() {
        val share = stage(FakeFile(bytes = 1_000, denied = true))

        assertEquals(ShareState.FAILED, share.state)
        assertEquals(FileState.FAILED, share.files.single().state)
        // The exception names the URI; the user is told nothing of the sort.
        assertFalse(share.files.single().error!!.contains("content://"))
    }

    @Test
    fun aBatchOverTheFileLimitIsRefusedBeforeAnythingIsRead() {
        val files = (0..ShareLimits.MAX_FILES).map { FakeFile(bytes = 10, displayName = "f$it.bin") }
        val share = stage(*files.toTypedArray())

        assertEquals(ShareState.FAILED, share.state)
        assertTrue(share.error!!.contains("${ShareLimits.MAX_FILES}"))
        assertFalse(File(incomingRoot(context), share.id).exists())
    }

    /**
     * The point of the copy loop: a file far larger than the heap goes through
     * a 64 KB buffer, so the heap must not grow with the file.
     */
    @Test
    fun aLargeStreamIsNeverHeldInMemory() {
        val runtime = Runtime.getRuntime()
        System.gc()
        val before = runtime.totalMemory() - runtime.freeMemory()

        val share = stage(FakeFile(bytes = 64L * 1024 * 1024, reportsSize = false))

        System.gc()
        val after = runtime.totalMemory() - runtime.freeMemory()
        assertEquals(ShareState.READY, share.state)
        assertEquals(64L * 1024 * 1024, share.files.single().stagedSize!!)
        assertTrue("heap grew by ${after - before} bytes", after - before < 16L * 1024 * 1024)
    }

    /**
     * The reason cancellation is a flag and not a thread interrupt: staging
     * runs on the bridge's single worker, so a cancel that queued behind it
     * would arrive only once the copy it means to stop had already finished.
     */
    @Test
    fun cancellingDuringACopyLeavesNothingStagedAndNoPrompt() {
        val source = FakeFile(bytes = 256L * 1024 * 1024, displayName = "big.bin")
        val share = pending(source)

        val staging = Thread { stager.stage(share) }
        staging.start()
        waitForBytes(share.id)
        stager.cancel(share.id)
        staging.join(30_000)

        assertFalse(staging.isAlive)
        assertNull(IncomingShareStore.get(share.id))
        assertFalse(File(incomingRoot(context), share.id).exists())
    }

    @Test
    fun cancellingBeforeStagingStartsNeverReadsTheAttachment() {
        val share = pending(FakeFile(bytes = 1_000, displayName = "queued.bin"))

        stager.cancel(share.id)
        stager.stage(share)

        assertNull(IncomingShareStore.get(share.id))
        assertFalse(File(incomingRoot(context), share.id).exists())
    }

    /** A cancelled id must not poison the next share that reuses the stager. */
    @Test
    fun aLaterShareStagesNormallyAfterACancel() {
        val cancelled = pending(FakeFile(bytes = 1_000, displayName = "gone.bin"))
        stager.cancel(cancelled.id)
        stager.stage(cancelled)
        stager.discard(cancelled.id)

        assertEquals(ShareState.READY, stage(FakeFile(bytes = 500, displayName = "next.bin")).state)
    }

    @Test
    fun purgeDropsAnAbandonedBatchAndKeepsALiveOne() {
        val live = stage(FakeFile(bytes = 100, displayName = "live.bin"))
        val abandoned = stage(FakeFile(bytes = 100, displayName = "abandoned.bin"))
        IncomingShareStore.remove(abandoned.id)

        val directory = File(incomingRoot(context), abandoned.id)
        File(directory, "manifest.json")
            .setLastModified(System.currentTimeMillis() - ShareLimits.STAGING_TTL_MILLIS - 1)

        stager.purge()

        assertFalse(directory.exists())
        assertTrue(File(incomingRoot(context), live.id).exists())
    }

    @Test
    fun purgeRemovesAnInterruptedCopyFromABatchNobodyAnsweredFor() {
        val share = stage(FakeFile(bytes = 100, displayName = "kept.bin"))
        IncomingShareStore.remove(share.id)
        val directory = File(incomingRoot(context), share.id)
        val interrupted = File(directory, "half.part").apply { writeBytes(ByteArray(10)) }

        stager.purge()

        assertFalse(interrupted.exists())
        assertTrue(directory.exists())
    }

    /** Blocks until the copy has put something on disk, so a cancel lands mid-file. */
    private fun waitForBytes(shareId: String) {
        val directory = File(incomingRoot(context), shareId)
        val deadline = System.currentTimeMillis() + 30_000
        while (System.currentTimeMillis() < deadline) {
            val part = directory.listFiles()?.firstOrNull { it.name.endsWith(".part") }
            if (part != null && part.length() > 0) return
            Thread.sleep(10)
        }
        throw AssertionError("staging never started writing for $shareId")
    }

    private fun stage(vararg sources: FakeFile): IncomingShare {
        val share = pending(*sources)
        stager.stage(share)
        return IncomingShareStore.get(share.id)!!
    }

    private fun pending(vararg sources: FakeFile): IncomingShare {
        val share = IncomingShare(
            id = IncomingShare.newId(),
            receivedAtMillis = System.currentTimeMillis(),
            files = sources.map { source ->
                val metadata = ShareIntentParser.describe(context.contentResolver, source.uri())
                IncomingFile(
                    id = IncomingShare.newId(),
                    displayName = metadata.displayName,
                    mimeType = metadata.mimeType,
                    reportedSize = metadata.reportedSize,
                    source = source.uri(),
                )
            },
            state = ShareState.RECEIVING,
        )
        IncomingShareStore.put(share)
        return share
    }
}

internal fun incomingRoot(context: Context) = File(context.cacheDir, "incoming")

/** The store outlives an activity, so it also outlives a test. */
internal fun clearShares() {
    IncomingShareStore.all().forEach { IncomingShareStore.remove(it.id) }
}
