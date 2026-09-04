package com.dinispimpao.yard.drop

import android.content.ClipData
import android.content.ClipDescription
import android.content.Intent
import android.net.Uri
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ShareIntentParserTest {

    private val resolver =
        InstrumentationRegistry.getInstrumentation().targetContext.contentResolver

    @Test
    fun readsASingleAttachment() {
        val uri = FakeFile(bytes = 10, displayName = "one.bin").uri()
        val intent = Intent(Intent.ACTION_SEND).putExtra(Intent.EXTRA_STREAM, uri)

        assertEquals(listOf(uri), (ShareIntentParser.parse(intent) as ShareIntent.Files).uris)
    }

    @Test
    fun readsAttachmentsCarriedOnlyInClipData() {
        val first = FakeFile(bytes = 10, displayName = "a.bin").uri()
        val second = FakeFile(bytes = 10, displayName = "b.bin").uri()
        val intent = Intent(Intent.ACTION_SEND_MULTIPLE).apply {
            clipData = ClipData(
                ClipDescription("files", arrayOf("*/*")),
                ClipData.Item(first),
            ).apply { addItem(ClipData.Item(second)) }
        }

        assertEquals(
            listOf(first, second),
            (ShareIntentParser.parse(intent) as ShareIntent.Files).uris,
        )
    }

    @Test
    fun aUriInBothPlacesIsOneAttachment() {
        val shared = FakeFile(bytes = 10, displayName = "same.bin").uri()
        val extra = FakeFile(bytes = 10, displayName = "extra.bin").uri()
        val intent = Intent(Intent.ACTION_SEND_MULTIPLE).apply {
            putParcelableArrayListExtra(Intent.EXTRA_STREAM, arrayListOf(shared, extra))
            clipData = ClipData(ClipDescription("files", arrayOf("*/*")), ClipData.Item(shared))
        }

        // The sender's order survives, and the duplicate is one file, not two.
        assertEquals(
            listOf(shared, extra),
            (ShareIntentParser.parse(intent) as ShareIntent.Files).uris,
        )
    }

    @Test
    fun aTextShareHasNothingToSave() {
        val intent = Intent(Intent.ACTION_SEND)
            .setType("text/plain")
            .putExtra(Intent.EXTRA_TEXT, "https://example.test")

        assertEquals(ShareIntent.TextOnly, ShareIntentParser.parse(intent))
    }

    @Test
    fun aLauncherStartIsNotAShare() {
        assertEquals(ShareIntent.Ignored, ShareIntentParser.parse(Intent(Intent.ACTION_MAIN)))
    }

    @Test
    fun describesWhatTheSenderExposed() {
        val source =
            FakeFile(bytes = 4_096, displayName = "report.pdf", mimeType = "application/pdf")

        val described = ShareIntentParser.describe(resolver, source.uri())

        assertEquals("report.pdf", described.displayName)
        assertEquals("application/pdf", described.mimeType)
        assertEquals(4_096L, described.reportedSize!!)
    }

    @Test
    fun survivesASenderThatAnswersNoMetadataAtAll() {
        val source = FakeFile(bytes = 10, mimeType = null, answersMetadata = false)

        val described = ShareIntentParser.describe(resolver, source.uri())

        assertTrue(described.displayName.startsWith("shared-"))
        assertNull(described.mimeType)
        assertNull(described.reportedSize)
    }

    @Test
    fun survivesAUriNoProviderClaims() {
        val described = ShareIntentParser.describe(
            resolver,
            Uri.parse("content://com.dinispimpao.yard.drop.test.absent/file"),
        )

        assertTrue(described.displayName.startsWith("shared-"))
        assertNull(described.reportedSize)
    }
}
