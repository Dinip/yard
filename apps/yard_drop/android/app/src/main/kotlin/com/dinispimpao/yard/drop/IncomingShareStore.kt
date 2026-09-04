package com.dinispimpao.yard.drop

import android.net.Uri
import java.util.UUID

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
    val reportedSize: Long?,
    val source: Uri?,
)

data class IncomingShare(
    val id: String,
    val receivedAtMillis: Long,
    val files: List<IncomingFile>,
    val state: ShareState,
    val error: String? = null,
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
    fun observe(listener: ((IncomingShare) -> Unit)?) {
        this.listener = listener
    }
}
