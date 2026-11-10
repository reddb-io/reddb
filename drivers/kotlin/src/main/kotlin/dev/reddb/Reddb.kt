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
    val protocol = when (url.kind) {
        Url.Kind.REDWIRE -> "red"; Url.Kind.REDWIRE_TLS -> "reds"
        Url.Kind.HTTP -> "http"; Url.Kind.HTTPS -> "https"
        else -> throw IllegalArgumentException("unsupported remote transport")
    }
    val original = java.net.URI.create(url.original)
    val host = url.host?.let { if (it.contains(':') && !it.startsWith("[")) "[$it]" else it }
    val seed = TopologyCodec.address("$protocol://$host:${url.port}${url.path ?: ""}", protocol)
        ?: throw IllegalArgumentException("invalid remote endpoint")
    val routed = RoutedConn(seed, connectDirect(url, opts), protocol,
        url.params["route"].equals("primary", ignoreCase = true)) { address, control ->
        var target = address
        if (original.rawUserInfo != null) target = target.replace("://", "://${original.rawUserInfo}@")
        if (original.rawQuery != null) target += "?${original.rawQuery}"
        connectDirect(Url.parse(target), if (control) opts.copy(timeout = kotlin.time.Duration.parse("5s")) else opts)
    }
    try { routed.initialize(); return routed }
    catch (error: Throwable) { routed.close(); throw error }
}

private suspend fun connectDirect(url: Url, opts: Options): Conn {
    return when (url.kind) {
        Url.Kind.REDWIRE, Url.Kind.REDWIRE_TLS -> RedwireConn.connect(url, opts)
        Url.Kind.HTTP, Url.Kind.HTTPS -> HttpConn.connect(url, opts)
        else -> throw IllegalArgumentException("unhandled URL kind: ${url.kind}")
    }
}
