package dev.reddb

import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

/**
 * Immutable bag of optional connection knobs. Pass to
 * [connect]. The defaults mirror the other RedDB drivers:
 * 30s timeout, no auth overrides.
 */
public data class Options(
    val username: String? = null,
    val password: String? = null,
    val token: String? = null,
    val apiKey: String? = null,
    val clientName: String? = null,
    val timeout: Duration = 30.seconds,
) {
    public companion object {
        public val DEFAULTS: Options = Options()
    }
}
