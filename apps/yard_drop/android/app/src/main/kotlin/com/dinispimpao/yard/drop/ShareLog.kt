package com.dinispimpao.yard.drop

import android.util.Log

/**
 * Structural logging only: an action, a count, a state, a share id.
 *
 * A content URI, a display name and file bytes are potentially customer data
 * and never go to logcat — a farm device's logs are not a private place.
 */
object ShareLog {
    private const val TAG = "YardDrop"

    fun info(message: String) = Log.i(TAG, message)

    fun warn(message: String, error: Throwable? = null) {
        if (error == null) Log.w(TAG, message) else Log.w(TAG, message, error)
    }
}
