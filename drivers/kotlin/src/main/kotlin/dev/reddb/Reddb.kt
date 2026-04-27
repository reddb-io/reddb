package dev.reddb

import dev.reddb.http.HttpConn
import dev.reddb.redwire.RedwireConn

/**
 * Top-level entry point. [connect] returns a [Conn] backed by whichever
 * transport the URL selected.
 *
 * Embedded URLs (`red:`, `red://`, `red://memory`, `red:///path`) throw
 * [UnsupportedOperationException] — the Kotlin driver doesn't ship the
 * embedded engine yet.
 */
public suspend fun connect(uri: String, opts: Options = Options.DEFAULTS): Conn {
    val parsed = Url.parse(uri)
    return connect(parsed, opts)
}

/** Open a connection from an already-parsed URL. */
public suspend fun connect(url: Url, opts: Options = Options.DEFAULTS): Conn {
    if (url.isEmbedded()) {
        throw UnsupportedOperationException(
            "embedded RedDB (${url.original}) needs the native lib — not yet shipped in reddb-kotlin"
        )
    }
    return when (url.kind) {
        Url.Kind.REDWIRE, Url.Kind.REDWIRE_TLS -> RedwireConn.connect(url, opts)
        Url.Kind.HTTP, Url.Kind.HTTPS -> HttpConn.connect(url, opts)
        else -> throw IllegalArgumentException("unhandled URL kind: ${url.kind}")
    }
}
