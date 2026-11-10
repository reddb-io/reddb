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
        String protocol = switch (url.kind()) {
            case REDWIRE -> "red"; case REDWIRE_TLS -> "reds"; case HTTP -> "http"; case HTTPS -> "https";
            default -> throw new IllegalArgumentException("unsupported remote transport");
        };
        URI original = URI.create(url.original());
        String host = url.host().contains(":") && !url.host().startsWith("[") ? "[" + url.host() + "]" : url.host();
        String seed = TopologyCodec.address(protocol + "://" + host + ":" + url.port() + (url.path() == null ? "" : url.path()), protocol);
        Options bounded = Options.builder().username(opts.username()).password(opts.password()).token(opts.token())
            .apiKey(opts.apiKey()).clientName(opts.clientName()).timeout(java.time.Duration.ofSeconds(5)).build();
        RoutedConn.Dial dial = (address, control) -> {
            String target = address;
            if (original.getRawUserInfo() != null) target = target.replace("://", "://" + original.getRawUserInfo() + "@");
            if (original.getRawQuery() != null) target += "?" + original.getRawQuery();
            return connectDirect(Url.parse(target), control ? bounded : opts);
        };
        RoutedConn routed = new RoutedConn(seed, connectDirect(url, opts), protocol,
            "primary".equalsIgnoreCase(url.params().get("route")), dial);
        try { routed.refresh(); return routed; }
        catch (RuntimeException error) { routed.close(); throw error; }
    }

    private static Conn connectDirect(Url url, Options opts) {
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
