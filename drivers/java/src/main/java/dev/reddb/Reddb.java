package dev.reddb;

import dev.reddb.http.HttpConn;
import dev.reddb.redwire.RedWireConn;

import java.net.URI;

/**
 * Top-level entry point. {@link #connect(URI, Options)} returns a
 * {@link Conn} backed by whichever transport the URL selected.
 *
 * Embedded URLs (`red:`, `red://`, `red://memory`, `red:///path`)
 * throw {@link UnsupportedOperationException} — the Java driver
 * doesn't ship the embedded engine; once a JNI binding lands, this
 * factory will pick it up via the same dispatch.
 */
public final class Reddb {
    private Reddb() {}

    /** Convenience: parse the URI string and call {@link #connect(URI, Options)}. */
    public static Conn connect(String uri) {
        return connect(URI.create(uri), Options.DEFAULTS);
    }

    /** Convenience with options. */
    public static Conn connect(String uri, Options opts) {
        return connect(URI.create(uri), opts);
    }

    /**
     * Open a connection with the supplied options.
     * @throws IllegalArgumentException for unsupported / malformed URIs
     * @throws UnsupportedOperationException for embedded URLs
     */
    public static Conn connect(URI uri, Options opts) {
        if (uri == null) throw new IllegalArgumentException("uri is null");
        Url parsed = Url.parse(uri.toString());
        return connect(parsed, opts == null ? Options.DEFAULTS : opts);
    }

    /** Open a connection from an already-parsed URL. */
    public static Conn connect(Url url, Options opts) {
        if (url.isEmbedded()) {
            throw new UnsupportedOperationException(
                "embedded RedDB (" + url.original() + ") needs the native lib — not yet shipped in reddb-jvm");
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
