package com.dinispimpao.yard.drop

import android.content.Context
import android.content.Intent
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodChannel

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

    init {
        methodChannel.setMethodCallHandler(this)
        eventChannel.setStreamHandler(this)
    }

    /**
     * Turns a share intent into a pending share. Ignores anything else, so a
     * launcher start and a rotation are both no-ops.
     */
    fun receive(intent: Intent) {
        if (intent.getBooleanExtra(EXTRA_CONSUMED, false)) return
        intent.putExtra(EXTRA_CONSUMED, true)

        when (val parsed = ShareIntentParser.parse(intent)) {
            is ShareIntent.Ignored -> return

            is ShareIntent.TextOnly -> IncomingShareStore.put(
                IncomingShare(
                    id = IncomingShare.newId(),
                    receivedAtMillis = System.currentTimeMillis(),
                    files = emptyList(),
                    state = ShareState.FAILED,
                    error = "YARD Drop needs a file attachment. Shared text has nothing to save.",
                ),
            )

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
                IncomingShareStore.put(
                    IncomingShare(
                        id = IncomingShare.newId(),
                        receivedAtMillis = System.currentTimeMillis(),
                        files = files,
                        state = ShareState.READY,
                    ),
                )
            }
        }
    }

    override fun onMethodCall(call: io.flutter.plugin.common.MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "listPendingShares" -> result.success(IncomingShareStore.all().map { it.toWire() })

            "discardShare" -> {
                val id = call.argument<String>("shareId")
                if (id == null) {
                    result.error("invalid_argument", "shareId is required", null)
                } else {
                    IncomingShareStore.remove(id)
                    result.success(null)
                }
            }

            "saveShare" -> result.error(
                "unimplemented",
                "Saving to Downloads lands in a later increment.",
                null,
            )

            "purgeExpiredShares" -> result.success(null)

            else -> result.notImplemented()
        }
    }

    override fun onListen(arguments: Any?, sink: EventChannel.EventSink?) {
        events = sink
        IncomingShareStore.observe { share -> events?.success(share.toWire()) }
    }

    override fun onCancel(arguments: Any?) {
        IncomingShareStore.observe(null)
        events = null
    }

    fun dispose() {
        IncomingShareStore.observe(null)
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
    "files" to files.map { file ->
        mapOf(
            "id" to file.id,
            "displayName" to file.displayName,
            "mimeType" to file.mimeType,
            "reportedSize" to file.reportedSize,
        )
    },
)
