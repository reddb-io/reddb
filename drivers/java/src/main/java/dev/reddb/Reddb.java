package dev.reddb;

import dev.reddb.http.HttpConn;
import dev.reddb.embedded.EmbeddedConn;
import dev.reddb.redwire.RedWireConn;

import java.net.URI;

/**
 * Top-level entry point. {@link #connect(URI, Options)} returns a
 * {@link Conn} backed by whichever transport the URL selected.
 *
 * Embedded URLs load the shared native RedDB ABI dynamically.
 */
public final class Reddb {
    private Reddb() {}

    /** Convenience: parse the URI string and call {@link #connect(URI, Options)}. */
    public static Conn connect(String uri) {
        return connect(Url.parse(uri), Options.DEFAULTS);
    }

    /** Convenience with options. */
    public static Conn connect(String uri, Options opts) {
        return connect(Url.parse(uri), opts == null ? Options.DEFAULTS : opts);
    }

    /**
     * Open a connection with the supplied options.
     * @throws IllegalArgumentException for unsupported / malformed URIs
     * @throws RedDBException.ProtocolError when the embedded native library cannot be loaded
     */
    public static Conn connect(URI uri, Options opts) {
        if (uri == null) throw new IllegalArgumentException("uri is null");
        Url parsed = Url.parse(uri.toString());
        return connect(parsed, opts == null ? Options.DEFAULTS : opts);
    }

    /** Open a connection from an already-parsed URL. */
    public static Conn connect(Url url, Options opts) {
        if (url.isEmbedded()) {
            return EmbeddedConn.open(url.path());
        }
        switch (url.kind()) {
            case REDWIRE:
            case REDWIRE_TLS:
                return RedWireConn.connect(url, opts);
            case HTTP:
            case HTTPS:
                return HttpConn.connect(url, opts);
            default:
                throw new IllegalArgumentException("unhandled URL kind: " + url.kind());
        }
    }
}
