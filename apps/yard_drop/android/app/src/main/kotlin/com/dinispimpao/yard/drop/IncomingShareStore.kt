package com.dinispimpao.yard.drop

import android.net.Uri
import java.util.UUID

/** Where one attachment in a batch got to. */
enum class FileState(val wireName: String) {
    /** Staged privately, waiting for a destination. */
    PENDING("pending"),
    SAVED("saved"),
    FAILED("failed"),
}

enum class ShareState(val wireName: String) {
    RECEIVING("receiving"),
    READY("ready"),
    SAVING("saving"),
    SAVED("saved"),
    FAILED("failed"),
}

data class IncomingFile(
    val id: String,
    val displayName: String,
    val mimeType: String?,
    /** What the sending app claimed, which may be absent or wrong. */
    val reportedSize: Long?,
    /** Non-null only until the file has been copied out of its URI grant. */
    val source: Uri?,
    val stagedPath: String? = null,
    /** Bytes actually copied. This is the one that is true. */
    val stagedSize: Long? = null,
    val state: FileState = FileState.PENDING,
    val error: String? = null,
    /** The name Downloads gave it, which a duplicate makes differ. */
    val savedName: String? = null,
)

data class IncomingShare(
    val id: String,
    val receivedAtMillis: Long,
    val files: List<IncomingFile>,
    val state: ShareState,
    val error: String? = null,
    /** 0..1 while saving, when the staged sizes make it knowable. */
    val progress: Double? = null,
) {
    companion object {
        fun newId(): String = UUID.randomUUID().toString()
    }
}

/**
 * The shares waiting for a decision.
 *
 * A queue, not a single slot: a user can share twice before answering the first
 * prompt, and the second must not overwrite the first. It outlives the activity
 * so a rotation or a process-level recreate does not drop a pending share.
 */
object IncomingShareStore {
    private val shares = LinkedHashMap<String, IncomingShare>()
    private var listener: ((IncomingShare) -> Unit)? = null

    @Synchronized
    fun all(): List<IncomingShare> = shares.values.toList()

    @Synchronized
    fun put(share: IncomingShare) {
        shares[share.id] = share
        listener?.invoke(share)
    }

    @Synchronized
    fun get(id: String): IncomingShare? = shares[id]

    @Synchronized
    fun remove(id: String): IncomingShare? = shares.remove(id)

    /**
     * Only the attached Flutter engine listens. Events emitted with no listener
     * are dropped on purpose: Dart re-reads [all] on startup and on resume, so
     * the queue is the source of truth and an event is only a nudge.
     */
    @Synchronized
    fun observe(listener: (IncomingShare) -> Unit) {
        this.listener = listener
    }

    /**
     * Identity-checked, because a rotation can destroy the old activity after
     * the new one has already subscribed, and a blind clear would silence it.
     */
    @Synchronized
    fun stopObserving(listener: (IncomingShare) -> Unit) {
        if (this.listener === listener) this.listener = null
    }
}
