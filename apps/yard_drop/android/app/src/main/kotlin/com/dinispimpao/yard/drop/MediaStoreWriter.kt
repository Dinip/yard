package com.dinispimpao.yard.drop

import android.content.ContentResolver
import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Environment
import android.provider.MediaStore
import java.io.File
import java.io.IOException

/**
 * Publishes a staged file into the device's Downloads collection.
 *
 * YARD Drop holds no storage permission. It writes only rows it creates itself,
 * which is why `MANAGE_EXTERNAL_STORAGE` and broad external-storage reads are
 * not requested and must not be added.
 */
class MediaStoreWriter(private val context: Context) {

    /**
     * Copies [staged] into `Download/<folder>` and returns the name it ended up
     * with, which a duplicate makes differ from [displayName].
     *
     * A failure leaves nothing behind: the pending row is deleted, so a partial
     * file is never visible to the Files app or to a farm cleanup pass.
     */
    fun publish(staged: File, displayName: String, mimeType: String?, folder: String): String {
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, sanitize(displayName))
            put(MediaStore.Downloads.MIME_TYPE, mimeType ?: "application/octet-stream")
            put(MediaStore.Downloads.RELATIVE_PATH, "${Environment.DIRECTORY_DOWNLOADS}/$folder")
            put(MediaStore.Downloads.IS_PENDING, 1)
        }

        val resolver = context.contentResolver
        val target = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: throw IOException("Downloads rejected the file.")

        try {
            resolver.openOutputStream(target)?.use { output ->
                staged.inputStream().use { input ->
                    input.copyTo(output, ShareLimits.COPY_BUFFER_BYTES)
                }
            } ?: throw IOException("Downloads did not accept the file contents.")

            values.clear()
            values.put(MediaStore.Downloads.IS_PENDING, 0)
            resolver.update(target, values, null, null)
        } catch (error: Throwable) {
            resolver.delete(target, null, null)
            throw error
        }

        // Downloads uniquifies a name that already exists, so what the user is
        // told is what the row says, not what was asked for.
        return savedName(resolver, target) ?: sanitize(displayName)
    }

    private fun savedName(resolver: ContentResolver, target: Uri): String? {
        return runCatching {
            resolver.query(target, arrayOf(MediaStore.Downloads.DISPLAY_NAME), null, null, null)
                ?.use { cursor ->
                    if (cursor.moveToFirst()) cursor.getString(0) else null
                }
        }.getOrNull()
    }

    /**
     * A display name comes from whatever app did the sharing, so it is treated
     * as hostile: no separators, no traversal, no empty result.
     */
    private fun sanitize(displayName: String): String {
        val cleaned = displayName
            .map { if (it.isISOControl() || it in ILLEGAL) '_' else it }
            .joinToString("")
            .trim()
            .trimStart('.')
            .take(MAX_NAME_LENGTH)

        return cleaned.ifBlank { "shared-file" }
    }

    private companion object {
        val ILLEGAL = charArrayOf('/', '\\', ':', '*', '?', '"', '<', '>', '|')
        const val MAX_NAME_LENGTH = 120
    }
}
