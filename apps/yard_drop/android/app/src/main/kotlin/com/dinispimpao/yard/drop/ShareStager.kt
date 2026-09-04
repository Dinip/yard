package com.dinispimpao.yard.drop

import android.content.Context
import android.net.Uri
import java.io.File
import org.json.JSONArray
import org.json.JSONObject

/**
 * Copies incoming attachments into private storage before the user is asked
 * anything.
 *
 * An `ACTION_SEND` URI grant lasts as long as the source app keeps it alive, so
 * a pending choice must never still depend on it. Everything here runs off the
 * main thread and reports through [IncomingShareStore].
 */
class ShareStager(private val context: Context) {

    private val root: File
        get() = File(context.cacheDir, "incoming")

    /** On-disk names are share and file ids, never a name the sender chose. */
    private fun batchDir(shareId: String) = File(root, shareId)

    fun stage(share: IncomingShare) {
        ShareLog.info("staging share ${share.id} with ${share.files.size} file(s)")

        if (share.files.size > ShareLimits.MAX_FILES) {
            fail(share, "A share is limited to ${ShareLimits.MAX_FILES} files.")
            return
        }

        val directory = batchDir(share.id)
        if (!directory.mkdirs() && !directory.isDirectory) {
            fail(share, "Could not prepare storage for the share.")
            return
        }

        val staged = mutableListOf<IncomingFile>()
        var batchBytes = 0L

        for (file in share.files) {
            val target = File(directory, file.id)
            val partial = File(directory, "${file.id}.part")
            val source = file.source

            if (source == null) {
                staged += file.failed("The share arrived without a readable attachment.")
                continue
            }

            val bytes = try {
                copy(source, partial, remaining = ShareLimits.MAX_BATCH_BYTES - batchBytes)
            } catch (error: Throwable) {
                // One bad attachment does not condemn the batch: a sender can
                // mix a URI it no longer owns in with files that are fine, and
                // the user should still get those. The message may name the
                // URI, so only our own wording reaches the user.
                partial.delete()
                ShareLog.warn("staging failed for a file in share ${share.id}", error)
                staged += file.failed(
                    (error as? StagingException)?.userMessage
                        ?: "The file could not be read from the app that shared it.",
                )
                continue
            }

            if (!partial.renameTo(target)) {
                partial.delete()
                staged += file.failed("The file could not be stored on this device.")
                continue
            }

            batchBytes += bytes
            staged += file.copy(source = null, stagedPath = target.absolutePath, stagedSize = bytes)
        }

        writeManifest(directory, share, staged)

        // Every attachment failing is a failed share; one surviving file is
        // still worth offering.
        val anyStaged = staged.any { it.state == FileState.PENDING }
        ShareLog.info("staged share ${share.id}: $batchBytes byte(s), usable=$anyStaged")

        IncomingShareStore.put(
            if (anyStaged) {
                share.copy(files = staged, state = ShareState.READY, error = null)
            } else {
                share.copy(
                    files = staged,
                    state = ShareState.FAILED,
                    error = staged.firstNotNullOfOrNull { it.error }
                        ?: "The share had nothing that could be read.",
                )
            },
        )
    }

    /** Bounded memory, and the reported size is never trusted as a length. */
    private fun copy(source: Uri, target: File, remaining: Long): Long {
        val input = context.contentResolver.openInputStream(source)
            ?: throw StagingException("The app that shared the file withdrew access to it.")

        var written = 0L
        input.use { stream ->
            target.outputStream().use { output ->
                val buffer = ByteArray(ShareLimits.COPY_BUFFER_BYTES)
                while (true) {
                    val read = stream.read(buffer)
                    if (read == -1) break

                    written += read
                    if (written > ShareLimits.MAX_FILE_BYTES) {
                        throw StagingException("A shared file may be at most 512 MB.")
                    }
                    if (written > remaining) {
                        throw StagingException("A share may be at most 2 GB in total.")
                    }
                    output.write(buffer, 0, read)
                }
                output.flush()
            }
        }
        return written
    }

    /**
     * The manifest is what a later process can read: the store is in memory, so
     * without it a staged batch would be bytes with no names or types.
     */
    private fun writeManifest(directory: File, share: IncomingShare, files: List<IncomingFile>) {
        val json = JSONObject()
            .put("shareId", share.id)
            .put("receivedAt", share.receivedAtMillis)
            .put(
                "files",
                JSONArray().apply {
                    files.forEach { file ->
                        put(
                            JSONObject()
                                .put("id", file.id)
                                .put("displayName", file.displayName)
                                .put("mimeType", file.mimeType)
                                .put("size", file.stagedSize),
                        )
                    }
                },
            )
        File(directory, MANIFEST).writeText(json.toString())
    }

    fun discard(shareId: String) {
        batchDir(shareId).deleteRecursively()
    }

    /**
     * Runs at startup. An interrupted copy leaves a `.part` file behind, and a
     * batch nobody answered for is not worth keeping past its expiry. Files
     * already written to Downloads live under MediaStore and are never touched
     * from here.
     */
    fun purge(now: Long = System.currentTimeMillis()) {
        val live = IncomingShareStore.all().map { it.id }.toSet()
        val batches = root.listFiles() ?: return

        for (batch in batches) {
            if (!batch.isDirectory || batch.name in live) continue

            val manifest = File(batch, MANIFEST)
            if (!manifest.exists() || now - manifest.lastModified() > ShareLimits.STAGING_TTL_MILLIS) {
                batch.deleteRecursively()
                continue
            }
            batch.listFiles()?.filter { it.name.endsWith(".part") }?.forEach { it.delete() }
        }
    }

    private fun fail(share: IncomingShare, message: String) {
        ShareLog.warn("share ${share.id} failed to stage")
        IncomingShareStore.put(share.copy(state = ShareState.FAILED, error = message))
    }

    private companion object {
        const val MANIFEST = "manifest.json"
    }
}

private class StagingException(val userMessage: String) : RuntimeException(userMessage)

private fun IncomingFile.failed(message: String) =
    copy(source = null, state = FileState.FAILED, error = message)
