package dev.reddb

import dev.reddb.http.HttpConn
import dev.reddb.embedded.EmbeddedConn
import dev.reddb.redwire.RedwireConn

/**
 * Top-level entry point. [connect] returns a [Conn] backed by whichever
 * transport the URL selected.
 *
 * Embedded URLs load the shared native RedDB ABI dynamically.
 */
public suspend fun connect(uri: String, opts: Options = Options.DEFAULTS): Conn {
    val parsed = Url.parse(uri)
    return connect(parsed, opts)
}

/** Open a connection from an already-parsed URL. */
public suspend fun connect(url: Url, opts: Options = Options.DEFAULTS): Conn {
    if (url.isEmbedded()) {
        return EmbeddedConn.open(url.path)
    }
    return when (url.kind) {
        Url.Kind.REDWIRE, Url.Kind.REDWIRE_TLS -> RedwireConn.connect(url, opts)
        Url.Kind.HTTP, Url.Kind.HTTPS -> HttpConn.connect(url, opts)
        else -> throw IllegalArgumentException("unhandled URL kind: ${url.kind}")
    }
}
