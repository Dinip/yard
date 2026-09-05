package com.dinispimpao.yard.drop

import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodChannel
import java.util.concurrent.Executors

private const val METHOD_CHANNEL = "com.dinispimpao.yard.drop/share"
private const val EVENT_CHANNEL = "com.dinispimpao.yard.drop/share_events"

/** Marks an intent this process has already turned into a share. */
private const val EXTRA_CONSUMED = "com.dinispimpao.yard.drop.CONSUMED"

/**
 * The only place platform types cross into Dart, and they cross as metadata:
 * file contents never travel over a platform channel.
 */
class IncomingShareBridge(
    private val context: Context,
    messenger: BinaryMessenger,
) : MethodChannel.MethodCallHandler, EventChannel.StreamHandler {

    private val methodChannel = MethodChannel(messenger, METHOD_CHANNEL)
    private val eventChannel = EventChannel(messenger, EVENT_CHANNEL)
    private var events: EventChannel.EventSink? = null
    private var observer: ((IncomingShare) -> Unit)? = null

    private val stager = ShareStager(context)
    private val saver = ShareSaver(stager, MediaStoreWriter(context))
    // One worker: batches stage in the order they were shared, and a second
    // share cannot race the first through the same directory.
    private val worker = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    init {
        methodChannel.setMethodCallHandler(this)
        eventChannel.setStreamHandler(this)
        background("purge") { stager.purge() }
    }

    /**
     * Every background task is wrapped: an uncaught throw on a pool thread ends
     * the process, and a share that cannot be handled is a message on screen,
     * not a crash.
     */
    private fun background(name: String, task: () -> Unit) {
        worker.execute {
            try {
                task()
            } catch (error: Throwable) {
                ShareLog.warn("$name failed", error)
            }
        }
    }

    /**
     * Turns a share intent into a pending share. Ignores anything else, so a
     * launcher start and a rotation are both no-ops.
     */
    fun receive(intent: Intent) {
        if (intent.getBooleanExtra(EXTRA_CONSUMED, false)) return
        intent.putExtra(EXTRA_CONSUMED, true)

        when (val parsed = ShareIntentParser.parse(intent)) {
            is ShareIntent.Ignored -> {
                ShareLog.info("ignoring intent with action ${intent.action}")
                return
            }

            is ShareIntent.TextOnly -> {
                ShareLog.info("rejecting a share with no attachment")
                IncomingShareStore.put(
                    IncomingShare(
                        id = IncomingShare.newId(),
                        receivedAtMillis = System.currentTimeMillis(),
                        files = emptyList(),
                        state = ShareState.FAILED,
                        error = "YARD - Device Farm needs a file attachment. Shared text has nothing to save.",
                    ),
                )
            }

            is ShareIntent.Files -> {
                val files = parsed.uris.map { uri ->
                    val metadata = ShareIntentParser.describe(context.contentResolver, uri)
                    IncomingFile(
                        id = IncomingShare.newId(),
                        displayName = metadata.displayName,
                        mimeType = metadata.mimeType,
                        reportedSize = metadata.reportedSize,
                        source = uri,
                    )
                }
                val share = IncomingShare(
                    id = IncomingShare.newId(),
                    receivedAtMillis = System.currentTimeMillis(),
                    files = files,
                    state = ShareState.RECEIVING,
                )
                // Ready means staged. The screen shows the receiving state until
                // the bytes are ours and the URI grant no longer matters.
                ShareLog.info("received share ${share.id} with ${files.size} file(s)")
                IncomingShareStore.put(share)
                background("stage") { stager.stage(share) }
            }
        }
    }

    override fun onMethodCall(call: io.flutter.plugin.common.MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "listPendingShares" -> {
                val pending = IncomingShareStore.all()
                ShareLog.info("Dart asked for pending shares: ${pending.size}")
                result.success(pending.map { it.toWire() })
            }

            "discardShare" -> {
                val id = call.argument<String>("shareId")
                if (id == null) {
                    result.error("invalid_argument", "shareId is required", null)
                } else {
                    // Ahead of the queue on purpose: the worker is single-threaded,
                    // so a discard that waited its turn would run only after the
                    // staging it is meant to stop had already finished.
                    stager.cancel(id)
                    IncomingShareStore.remove(id)
                    background("discard") { stager.discard(id) }
                    result.success(null)
                }
            }

            "saveShare" -> {
                val id = call.argument<String>("shareId")
                val destination = call.argument<String>("destination")
                val share = id?.let(IncomingShareStore::get)
                if (share == null || destination == null) {
                    result.error("unknown_share", "That share is no longer pending.", null)
                } else {
                    // Build identity comes from Dart so the batch manifest and
                    // the About screen can never name different builds.
                    val producer = Producer(
                        appVersion = call.argument<String>("appVersion") ?: "unknown",
                        buildNumber = call.argument<String>("buildNumber")?.toIntOrNull() ?: 0,
                        commit = call.argument<String>("commit") ?: "unknown",
                    )
                    background("save") {
                        val outcome = saver.save(share, destination, producer)
                        main.post {
                            when (outcome) {
                                is SaveOutcome.Success -> result.success(outcome.location)
                                is SaveOutcome.Failure ->
                                    result.error(outcome.code, outcome.message, null)
                            }
                        }
                    }
                }
            }

            "purgeExpiredShares" -> {
                background("purge") { stager.purge() }
                result.success(null)
            }

            else -> result.notImplemented()
        }
    }

    override fun onListen(arguments: Any?, sink: EventChannel.EventSink?) {
        events = sink
        // Staging reports from the worker thread; an event sink is main-thread
        // only.
        val observer: (IncomingShare) -> Unit = { share ->
            val wire = share.toWire()
            main.post { events?.success(wire) }
        }
        this.observer = observer
        IncomingShareStore.observe(observer)
    }

    override fun onCancel(arguments: Any?) {
        observer?.let(IncomingShareStore::stopObserving)
        observer = null
        events = null
    }

    fun dispose() {
        observer?.let(IncomingShareStore::stopObserving)
        observer = null
        main.removeCallbacksAndMessages(null)
        worker.shutdown()
        events = null
        methodChannel.setMethodCallHandler(null)
        eventChannel.setStreamHandler(null)
    }
}

private fun IncomingShare.toWire(): Map<String, Any?> = mapOf(
    "id" to id,
    "receivedAt" to receivedAtMillis,
    "state" to state.wireName,
    "error" to error,
    "progress" to progress,
    "files" to files.map { file ->
        mapOf(
            "id" to file.id,
            "displayName" to file.displayName,
            "mimeType" to file.mimeType,
            "reportedSize" to file.reportedSize,
            "state" to file.state.wireName,
            "error" to file.error,
            "savedName" to file.savedName,
        )
    },
)
