package com.dinispimpao.yard.drop

import java.io.File

/** Where a destination puts its files, under the device's Downloads. */
private const val SAVED_FOLDER = "YARD Drop/Saved"

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
    /** The folder shown to the user once a save succeeds. */
    val savedLocation: String = "Download/$SAVED_FOLDER"

    fun save(share: IncomingShare, destination: String): SaveOutcome {
        if (destination != "downloads") {
            return SaveOutcome.Failure("unsupported_destination", "That destination is not available yet.")
        }

        val pending = share.files.filter { it.state == FileState.PENDING }
        if (pending.isEmpty()) {
            return SaveOutcome.Failure("nothing_to_save", "There is nothing left to save.")
        }

        val total = pending.sumOf { it.stagedSize ?: 0L }.coerceAtLeast(1L)
        var written = 0L
        val results = share.files.associateBy({ it.id }, { it }).toMutableMap()

        IncomingShareStore.put(share.copy(state = ShareState.SAVING, progress = 0.0, error = null))

        for (file in pending) {
            val staged = file.stagedPath?.let(::File)

            results[file.id] = if (staged == null || !staged.exists()) {
                file.copy(state = FileState.FAILED, error = "The file is no longer on this device.")
            } else {
                try {
                    val savedName = writer.publish(staged, file.displayName, file.mimeType, SAVED_FOLDER)
                    staged.delete()
                    file.copy(state = FileState.SAVED, savedName = savedName, error = null)
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

        if (failed == 0) {
            stager.discard(share.id)
            IncomingShareStore.put(
                share.copy(files = files, state = ShareState.SAVED, progress = 1.0, error = null),
            )
            return SaveOutcome.Success
        }

        val message = if (saved == 0) {
            "No files could be written to $savedLocation."
        } else {
            "$saved of ${files.size} files are in $savedLocation. The rest could not be written."
        }
        IncomingShareStore.put(share.copy(files = files, state = ShareState.FAILED, error = message))
        return SaveOutcome.Failure("save_failed", message)
    }
}

sealed interface SaveOutcome {
    data object Success : SaveOutcome

    data class Failure(val code: String, val message: String) : SaveOutcome
}
