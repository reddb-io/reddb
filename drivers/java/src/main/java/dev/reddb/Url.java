package dev.reddb;

import java.net.URI;
import java.net.URISyntaxException;
import java.nio.charset.StandardCharsets;
import java.net.URLDecoder;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Locale;
import java.util.Map;

/**
 * Connection-string parser for the Java driver. Mirrors
 * `drivers/js/src/url.js` semantics: one URL covers every transport.
 *
 * Supported shapes:
 * <pre>
 *   red://[user[:pass]@]host[:port][?...]      plain RedWire (TCP)
 *   reds://[user[:pass]@]host[:port][?...]     RedWire over TLS
 *   http://host[:port]/                        HTTP (REST)
 *   https://host[:port]/                       HTTPS
 *   red:///abs/path/file.rdb                   embedded (out of scope here)
 *   red://memory  red://:memory  red://:memory:  embedded in-memory (out of scope)
 * </pre>
 *
 * Default port is 5050 for all schemes (matches RedWire listener default).
 */
public final class Url {
    /** Default port used for every transport. Matches `DEFAULT_REDWIRE_PORT`. */
    public static final int DEFAULT_PORT = 5050;

    public enum Kind { REDWIRE, REDWIRE_TLS, HTTP, HTTPS, EMBEDDED_FILE, EMBEDDED_MEMORY }

    private final String original;
    private final Kind kind;
    private final String host;
    private final int port;
    private final String path;
    private final String username;
    private final String password;
    private final String token;
    private final String apiKey;
    private final Map<String, String> params;

    private Url(Builder b) {
        this.original = b.original;
        this.kind = b.kind;
        this.host = b.host;
        this.port = b.port;
        this.path = b.path;
        this.username = b.username;
        this.password = b.password;
        this.token = b.token;
        this.apiKey = b.apiKey;
        this.params = b.params == null ? Collections.emptyMap() : Collections.unmodifiableMap(b.params);
    }

    public String original() { return original; }
    public Kind kind() { return kind; }
    public String host() { return host; }
    public int port() { return port; }
    public String path() { return path; }
    public String username() { return username; }
    public String password() { return password; }
    public String token() { return token; }
    public String apiKey() { return apiKey; }
    public Map<String, String> params() { return params; }

    /** True for `red://` and `reds://` (the binary protocol). */
    public boolean isRedwire() { return kind == Kind.REDWIRE || kind == Kind.REDWIRE_TLS; }

    /** True for `reds://` or `https://`. */
    public boolean isTls() { return kind == Kind.REDWIRE_TLS || kind == Kind.HTTPS; }

    /** True for either embedded variant — the Java driver doesn't ship an embedded engine. */
    public boolean isEmbedded() { return kind == Kind.EMBEDDED_FILE || kind == Kind.EMBEDDED_MEMORY; }

    /**
     * Parse any supported URI string.
     * @throws IllegalArgumentException for unsupported schemes / malformed inputs
     */
    public static Url parse(String uri) {
        if (uri == null || uri.isEmpty()) {
            throw new IllegalArgumentException(
                "connect requires a URI string (e.g. 'red://localhost:5050')");
        }
        // Embedded shortcuts — `red:`, `red:/`, `red://`, `red://memory`, etc.
        if (uri.equals("red:") || uri.equals("red:/") || uri.equals("red://")
            || uri.equals("red://memory") || uri.equals("red://memory/")
            || uri.equals("red://:memory") || uri.equals("red://:memory:")) {
            return new Builder().original(uri).kind(Kind.EMBEDDED_MEMORY).build();
        }
        if (uri.startsWith("red:///")) {
            String p = uri.substring("red://".length()); // keeps leading '/'
            return new Builder().original(uri).kind(Kind.EMBEDDED_FILE).path(p).build();
        }

        String scheme = schemeOf(uri);
        Kind kind = kindFromScheme(scheme);
        if (kind == null) {
            throw new IllegalArgumentException(
                "unsupported URI scheme: '" + scheme + "' in '" + uri + "'."
                    + " Supported: red, reds, http, https");
        }

        URI parsed = parseAsJavaUri(uri);
        String host = parsed.getHost();
        if (host == null || host.isEmpty()) {
            throw new IllegalArgumentException(
                "URI is missing a host: '" + uri + "'");
        }
        int port = parsed.getPort();
        if (port < 0) port = DEFAULT_PORT;

        String userInfo = parsed.getRawUserInfo();
        String username = null;
        String password = null;
        if (userInfo != null && !userInfo.isEmpty()) {
            int colon = userInfo.indexOf(':');
            if (colon >= 0) {
                username = decode(userInfo.substring(0, colon));
                password = decode(userInfo.substring(colon + 1));
            } else {
                username = decode(userInfo);
            }
        }

        Map<String, String> params = parseQuery(parsed.getRawQuery());
        String token = params.get("token");
        String apiKey = params.containsKey("apiKey") ? params.get("apiKey") : params.get("api_key");

        String path = parsed.getRawPath();
        if (path != null && (path.isEmpty() || path.equals("/"))) path = null;

        return new Builder()
            .original(uri).kind(kind)
            .host(host).port(port).path(path)
            .username(username).password(password)
            .token(token).apiKey(apiKey)
            .params(params)
            .build();
    }

    private static String schemeOf(String uri) {
        int colon = uri.indexOf(':');
        if (colon <= 0) {
            throw new IllegalArgumentException("URI missing scheme: '" + uri + "'");
        }
        return uri.substring(0, colon).toLowerCase(Locale.ROOT);
    }

    private static Kind kindFromScheme(String scheme) {
        switch (scheme) {
            case "red": return Kind.REDWIRE;
            case "reds": return Kind.REDWIRE_TLS;
            case "http": return Kind.HTTP;
            case "https": return Kind.HTTPS;
            default: return null;
        }
    }

    private static URI parseAsJavaUri(String uri) {
        // java.net.URI is happy with red:// and reds:// (any scheme is fine).
        try {
            return new URI(uri);
        } catch (URISyntaxException e) {
            throw new IllegalArgumentException("failed to parse URI '" + uri + "': " + e.getMessage(), e);
        }
    }

    private static Map<String, String> parseQuery(String raw) {
        Map<String, String> out = new LinkedHashMap<>();
        if (raw == null || raw.isEmpty()) return out;
        for (String pair : raw.split("&")) {
            if (pair.isEmpty()) continue;
            int eq = pair.indexOf('=');
            String k = eq < 0 ? pair : pair.substring(0, eq);
            String v = eq < 0 ? "" : pair.substring(eq + 1);
            out.put(decode(k), decode(v));
        }
        return out;
    }

    private static String decode(String s) {
        return URLDecoder.decode(s, StandardCharsets.UTF_8);
    }

    /** Internal builder — keeps the public type immutable. */
    static final class Builder {
        String original;
        Kind kind;
        String host;
        int port = DEFAULT_PORT;
        String path;
        String username;
        String password;
        String token;
        String apiKey;
        Map<String, String> params;

        Builder original(String v) { this.original = v; return this; }
        Builder kind(Kind v) { this.kind = v; return this; }
        Builder host(String v) { this.host = v; return this; }
        Builder port(int v) { this.port = v; return this; }
        Builder path(String v) { this.path = v; return this; }
        Builder username(String v) { this.username = v; return this; }
        Builder password(String v) { this.password = v; return this; }
        Builder token(String v) { this.token = v; return this; }
        Builder apiKey(String v) { this.apiKey = v; return this; }
        Builder params(Map<String, String> v) { this.params = v; return this; }
        Url build() { return new Url(this); }
    }
}
