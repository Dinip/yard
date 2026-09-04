package com.dinispimpao.yard.drop

import android.content.ContentResolver
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import java.util.Locale

/** What an incoming intent turned out to be. */
sealed interface ShareIntent {
    data class Files(val uris: List<Uri>) : ShareIntent

    /** A share YARD Drop cannot act on: text, a link, an empty payload. */
    data object TextOnly : ShareIntent

    /** Anything that is not a share, such as a launcher start. */
    data object Ignored : ShareIntent
}

data class SharedFileMetadata(
    val displayName: String,
    val mimeType: String?,
    val reportedSize: Long?,
)

/**
 * Reads attachments out of a share intent.
 *
 * Nothing here is logged. A content URI, a display name and file bytes are all
 * potentially customer data.
 */
object ShareIntentParser {
    fun parse(intent: Intent): ShareIntent {
        if (intent.action != Intent.ACTION_SEND) return ShareIntent.Ignored

        val uris = streamUris(intent)
        if (uris.isNotEmpty()) return ShareIntent.Files(uris)

        return ShareIntent.TextOnly
    }

    /**
     * `EXTRA_STREAM` is the documented carrier, but an app may put the
     * attachment only in [Intent.getClipData], so both are read.
     */
    private fun streamUris(intent: Intent): List<Uri> {
        val extra = intent.extraStream()
        if (extra != null) return listOf(extra)

        val clip = intent.clipData ?: return emptyList()
        return (0 until clip.itemCount).mapNotNull { clip.getItemAt(it).uri }
    }

    private fun Intent.extraStream(): Uri? {
        @Suppress("DEPRECATION")
        return getParcelableExtra(Intent.EXTRA_STREAM)
    }

    /**
     * Metadata is whatever the sharing app chose to expose, so every column may
     * be missing or wrong. The bytes are the truth; this only feeds the screen.
     */
    fun describe(resolver: ContentResolver, uri: Uri): SharedFileMetadata {
        var displayName: String? = null
        var size: Long? = null

        runCatching {
            resolver.query(uri, null, null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val nameColumn = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (nameColumn >= 0 && !cursor.isNull(nameColumn)) {
                        displayName = cursor.getString(nameColumn)
                    }
                    val sizeColumn = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (sizeColumn >= 0 && !cursor.isNull(sizeColumn)) {
                        size = cursor.getLong(sizeColumn)
                    }
                }
            }
        }

        val mimeType = runCatching { resolver.getType(uri) }.getOrNull()

        return SharedFileMetadata(
            displayName = displayName?.takeIf { it.isNotBlank() } ?: fallbackName(mimeType),
            mimeType = mimeType,
            reportedSize = size?.takeIf { it >= 0 },
        )
    }

    /** A share with no name still has to be nameable in the UI and on disk. */
    private fun fallbackName(mimeType: String?): String {
        val extension = mimeType
            ?.substringAfterLast('/', "")
            ?.takeIf { it.isNotBlank() && it.all { c -> c.isLetterOrDigit() } }
            ?.lowercase(Locale.US)
        val stamp = System.currentTimeMillis()
        return if (extension == null) "shared-$stamp" else "shared-$stamp.$extension"
    }
}
