package com.dinispimpao.yard.drop

import java.io.File

/** Where a destination puts its files, under the device's Downloads. */
private const val SAVED_FOLDER = "YARD Drop/Saved"

/**
 * Turns a user's choice into files on the device.
 *
 * Progress is published through [IncomingShareStore] rather than returned, so
 * the screen can follow a long batch instead of waiting on one call.
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

        val total = share.files.sumOf { it.stagedSize ?: 0L }.coerceAtLeast(1L)
        var written = 0L
        var saved = 0

        IncomingShareStore.put(share.copy(state = ShareState.SAVING, progress = 0.0, error = null))

        for (file in share.files) {
            val staged = file.stagedPath?.let(::File)
            if (staged == null || !staged.exists()) {
                return fail(share, saved, "The file is no longer on this device.")
            }

            try {
                writer.publish(staged, file.displayName, file.mimeType, SAVED_FOLDER)
            } catch (error: Throwable) {
                // The message may name a path or a URI, neither of which is
                // ours to show.
                return fail(share, saved, "The file could not be written to Downloads.")
            }

            staged.delete()
            saved++
            written += file.stagedSize ?: 0L
            IncomingShareStore.put(
                share.copy(state = ShareState.SAVING, progress = written.toDouble() / total),
            )
        }

        stager.discard(share.id)
        IncomingShareStore.put(share.copy(state = ShareState.SAVED, progress = 1.0, error = null))
        return SaveOutcome.Success
    }

    private fun fail(share: IncomingShare, saved: Int, reason: String): SaveOutcome {
        val message = if (saved == 0) {
            reason
        } else {
            "$reason ${saved} of ${share.files.size} files were already saved to $savedLocation."
        }
        IncomingShareStore.put(share.copy(state = ShareState.FAILED, error = message))
        return SaveOutcome.Failure("save_failed", message)
    }
}

sealed interface SaveOutcome {
    data object Success : SaveOutcome

    data class Failure(val code: String, val message: String) : SaveOutcome
}
