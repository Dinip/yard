package com.dinispimpao.yard.drop

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import java.io.FileOutputStream

private const val AUTHORITY = "com.dinispimpao.yard.drop.test.files"

/**
 * Stands in for the app on the other side of a share, including the ways a real
 * one misbehaves: no display name, no size, a stream that dies partway, a URI
 * the app is not allowed to read.
 *
 * Bytes come out of a pipe rather than a file, so a test can ask for a stream
 * larger than the emulator's storage and describe its length as unknown, which
 * is what a real streaming provider does.
 */
class FakeFilesProvider : ContentProvider() {

    override fun onCreate() = true

    override fun query(
        uri: Uri,
        projection: Array<out String>?,
        selection: String?,
        selectionArgs: Array<out String>?,
        sortOrder: String?,
    ): Cursor? {
        val file = FakeFile.from(uri)
        if (!file.answersMetadata) return null

        val columns = buildList<String> {
            if (file.displayName != null) add(OpenableColumns.DISPLAY_NAME)
            add(OpenableColumns.SIZE)
        }
        return MatrixCursor(columns.toTypedArray()).apply {
            addRow(
                buildList<Any?> {
                    if (file.displayName != null) add(file.displayName)
                    add(if (file.reportsSize) file.bytes else null)
                },
            )
        }
    }

    override fun getType(uri: Uri): String? = FakeFile.from(uri).mimeType

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        val file = FakeFile.from(uri)
        if (file.denied) throw SecurityException("the caller holds no grant for $uri")

        val pipe = ParcelFileDescriptor.createReliablePipe()
        Thread({ produce(file, pipe[1]) }, "fake-files").start()
        return pipe[0]
    }

    /**
     * A reliable pipe is what makes a mid-stream failure reach the reader as an
     * `IOException`. A plain pipe would look like a clean, short file.
     */
    private fun produce(file: FakeFile, write: ParcelFileDescriptor) {
        val chunk = ByteArray(CHUNK) { (it % 251).toByte() }
        val output = FileOutputStream(write.fileDescriptor)
        var written = 0L

        try {
            while (written < file.bytes) {
                val failAt = file.failsAfter
                if (failAt != null && written >= failAt) {
                    output.flush()
                    write.closeWithError("the source stream failed")
                    return
                }
                val take = minOf(chunk.size.toLong(), file.bytes - written).toInt()
                output.write(chunk, 0, take)
                written += take
            }
            output.flush()
            write.close()
        } catch (_: Throwable) {
            runCatching { write.close() }
        }
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri =
        throw UnsupportedOperationException()

    override fun update(
        uri: Uri,
        values: ContentValues?,
        selection: String?,
        selectionArgs: Array<out String>?,
    ): Int = throw UnsupportedOperationException()

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int =
        throw UnsupportedOperationException()

    private companion object {
        const val CHUNK = 64 * 1024
    }
}

/** One attachment the fake provider will serve, encoded in its URI. */
data class FakeFile(
    val bytes: Long,
    val displayName: String? = "shared.bin",
    val mimeType: String? = "application/octet-stream",
    val reportsSize: Boolean = true,
    val answersMetadata: Boolean = true,
    val denied: Boolean = false,
    /** Byte offset at which the stream breaks, or null for a clean stream. */
    val failsAfter: Long? = null,
) {
    fun uri(): Uri = Uri.Builder()
        .scheme("content")
        .authority(AUTHORITY)
        .appendPath("file")
        .appendPath(displayName ?: "unnamed")
        .appendQueryParameter("bytes", bytes.toString())
        .apply {
            displayName?.let { appendQueryParameter("name", it) }
            mimeType?.let { appendQueryParameter("mime", it) }
            failsAfter?.let { appendQueryParameter("failsAfter", it.toString()) }
            if (!reportsSize) appendQueryParameter("size", "0")
            if (!answersMetadata) appendQueryParameter("meta", "0")
            if (denied) appendQueryParameter("denied", "1")
        }
        .build()

    /** The byte a clean stream produces at [offset], for content assertions. */
    fun byteAt(offset: Int): Byte = ((offset % (64 * 1024)) % 251).toByte()

    companion object {
        fun from(uri: Uri) = FakeFile(
            bytes = uri.getQueryParameter("bytes")?.toLong() ?: 0L,
            displayName = uri.getQueryParameter("name"),
            mimeType = uri.getQueryParameter("mime"),
            reportsSize = uri.getQueryParameter("size") != "0",
            answersMetadata = uri.getQueryParameter("meta") != "0",
            denied = uri.getQueryParameter("denied") == "1",
            failsAfter = uri.getQueryParameter("failsAfter")?.toLong(),
        )
    }
}
