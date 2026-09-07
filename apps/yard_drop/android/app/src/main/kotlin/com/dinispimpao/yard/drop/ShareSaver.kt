package com.dinispimpao.yard.drop

import java.io.File
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject

/** Where each destination puts its files, under the device's Downloads. */
private const val SAVED_FOLDER = "YARD Drop/Saved"
private const val INBOX_FOLDER = "YARD Drop/Inbox"

/** Bumped only when a reader that understands the current shape would misread it. */
const val BATCH_SCHEMA_VERSION = 1

const val BATCH_MANIFEST = "batch.json"

/** A batch without this is half-written, and a reader skips it. */
const val BATCH_READY_MARKER = "_YARD_READY"

private val BATCH_STAMP = DateTimeFormatter
    .ofPattern("yyyyMMdd-HHmmss")
    .withZone(ZoneOffset.UTC)

/** The build that produced a batch, so the browser can name it in an error. */
data class Producer(val appVersion: String, val buildNumber: Int, val commit: String)

/**
 * Turns a user's choice into files on the device.
 *
 * Progress is published through [IncomingShareStore] rather than returned, so
 * the screen can follow a long batch instead of waiting on one call. A file
 * that already reached Downloads is never rewritten and never withdrawn because
 * a later one failed.
 */
class ShareSaver(
    private val stager: ShareStager,
    private val writer: MediaStoreWriter,
) {
    fun save(share: IncomingShare, destination: String, producer: Producer): SaveOutcome {
        val batch = when (destination) {
            "downloads" -> null
            "browserInbox" -> "$INBOX_FOLDER/${BATCH_STAMP.format(Instant.now())}-${UUID.randomUUID()}"
            else -> return SaveOutcome.Failure(
                "unsupported_destination",
                "That destination is not available yet.",
            )
        }
        val folder = batch ?: SAVED_FOLDER
        val location = "Download/$folder"

        val pending = share.files.filter { it.state == FileState.PENDING }
        if (pending.isEmpty()) {
            return SaveOutcome.Failure("nothing_to_save", "There is nothing left to save.")
        }

        val total = pending.sumOf { it.stagedSize ?: 0L }.coerceAtLeast(1L)
        var written = 0L
        val results = share.files.associateBy({ it.id }, { it }).toMutableMap()
        val rows = mutableListOf<Published>()

        IncomingShareStore.put(share.copy(state = ShareState.SAVING, progress = 0.0, error = null))

        for (file in pending) {
            val staged = file.stagedPath?.let(::File)

            results[file.id] = if (staged == null || !staged.exists()) {
                file.copy(state = FileState.FAILED, error = "The file is no longer on this device.")
            } else {
                try {
                    val published = writer.publish(staged, file.displayName, file.mimeType, folder)
                    rows += published
                    // A batch is retried whole, so its staged bytes stay until
                    // the manifest and the marker have both been written.
                    if (batch == null) staged.delete()
                    file.copy(state = FileState.SAVED, savedName = published.name, error = null)
                } catch (error: Throwable) {
                    // The message may name a path or a URI, neither of which is
                    // ours to show. The staged copy stays, so a retry is real.
                    ShareLog.warn("saving a file in share ${share.id} failed", error)
                    file.copy(state = FileState.FAILED, error = "It could not be written to Downloads.")
                }
            }

            written += file.stagedSize ?: 0L
            IncomingShareStore.put(
                share.copy(
                    files = share.files.map { results.getValue(it.id) },
                    state = ShareState.SAVING,
                    progress = written.toDouble() / total,
                ),
            )
        }

        val files = share.files.map { results.getValue(it.id) }
        val failed = files.count { it.state == FileState.FAILED }
        val saved = files.count { it.state == FileState.SAVED }

        if (failed == 0 && batch != null) {
            try {
                arm(batch, files, producer)
            } catch (error: Throwable) {
                ShareLog.warn("arming the inbox batch for share ${share.id} failed", error)
                // Half a batch is worse than none: without the manifest and the
                // marker nothing will ever read it, and the bytes would sit in
                // Downloads until the reservation ended.
                writer.delete(rows)
                return failure(share, unpublish(files), "The batch could not be prepared for the browser.")
            }
        }

        if (failed == 0) {
            stager.discard(share.id)
            IncomingShareStore.put(
                share.copy(files = files, state = ShareState.SAVED, progress = 1.0, error = null),
            )
            return SaveOutcome.Success(location)
        }

        // An incomplete batch is one nobody may read, so the rows that did land
        // go away and the whole share is retried from staging into a fresh one.
        if (batch != null) {
            writer.delete(rows)
            return failure(share, unpublish(files), "No files could be written to $location.")
        }

        val message = if (saved == 0) {
            "No files could be written to $location."
        } else {
            "$saved of ${files.size} files are in $location. The rest could not be written."
        }
        return failure(share, files, message)
    }

    /**
     * Writes the manifest, then the marker. Order is the contract: a reader that
     * sees the marker is promised every file and a manifest it can parse.
     */
    private fun arm(folder: String, files: List<IncomingFile>, producer: Producer) {
        val manifest = JSONObject()
            .put("schemaVersion", BATCH_SCHEMA_VERSION)
            .put("batchId", folder.substringAfterLast('/'))
            .put("createdAt", System.currentTimeMillis())
            .put(
                "producer",
                JSONObject()
                    .put("appVersion", producer.appVersion)
                    .put("buildNumber", producer.buildNumber)
                    .put("commit", producer.commit),
            )
            .put(
                "files",
                JSONArray().apply {
                    files.filter { it.state == FileState.SAVED }.forEach { file ->
                        put(
                            JSONObject()
                                // The name MediaStore gave it, which is what a
                                // reader will find in the directory.
                                .put("name", file.savedName)
                                .put("mimeType", file.mimeType)
                                .put("size", file.stagedSize),
                        )
                    }
                },
            )

        writer.publishText(manifest.toString(), BATCH_MANIFEST, "application/json", folder)
        writer.publishText("", BATCH_READY_MARKER, "application/octet-stream", folder)
    }

    /**
     * Puts withdrawn files back where a retry will pick them up. Their staged
     * bytes were kept for exactly this.
     */
    private fun unpublish(files: List<IncomingFile>) = files.map { file ->
        if (file.state == FileState.SAVED) {
            file.copy(state = FileState.PENDING, savedName = null)
        } else {
            file
        }
    }

    private fun failure(
        share: IncomingShare,
        files: List<IncomingFile>,
        message: String,
    ): SaveOutcome.Failure {
        IncomingShareStore.put(share.copy(files = files, state = ShareState.FAILED, error = message))
        return SaveOutcome.Failure("save_failed", message)
    }
}

sealed interface SaveOutcome {
    /** [location] is the folder the screen names, not a path the app may reopen. */
    data class Success(val location: String) : SaveOutcome

    data class Failure(val code: String, val message: String) : SaveOutcome
}
