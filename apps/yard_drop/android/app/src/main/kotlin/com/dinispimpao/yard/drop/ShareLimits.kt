package com.dinispimpao.yard.drop

import java.util.concurrent.TimeUnit

/** Settled in increment 0; see apps/yard_drop/README.md. */
object ShareLimits {
    const val MAX_FILES = 20
    const val MAX_FILE_BYTES = 512L * 1000 * 1000
    const val MAX_BATCH_BYTES = 2L * 1000 * 1000 * 1000
    val STAGING_TTL_MILLIS: Long = TimeUnit.HOURS.toMillis(24)

    /** Large enough to keep the copy streaming, small enough to stay flat. */
    const val COPY_BUFFER_BYTES = 64 * 1024
}
