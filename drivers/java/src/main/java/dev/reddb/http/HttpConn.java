package dev.reddb.http;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import dev.reddb.Conn;
import dev.reddb.Options;
import dev.reddb.RedDBException;
import dev.reddb.Url;

import javax.net.ssl.SSLContext;
import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.List;

/**
 * HTTP transport. Mirrors the Rust / JS HTTP drivers: a single
 * {@link HttpClient} talking JSON to the RedDB REST endpoints,
 * carrying a bearer token in {@code Authorization}. Login is
 * automatic when {@link Options} carries username + password.
 */
public final class HttpConn implements Conn {
    private static final ObjectMapper MAPPER = new ObjectMapper();

    private final HttpClient client;
    private final String baseUrl;
    private volatile String token;
    private final Duration timeout;
    private volatile boolean closed;

    public HttpConn(HttpClient client, String baseUrl, String token, Duration timeout) {
        this.client = client;
        this.baseUrl = stripTrailingSlash(baseUrl);
        this.token = token;
        this.timeout = timeout == null ? Duration.ofSeconds(30) : timeout;
    }

    /** Open a fresh client, log in if credentials were supplied, and return a ready connection. */
    public static HttpConn connect(Url url, Options opts) {
        String scheme = url.kind() == Url.Kind.HTTPS ? "https" : "http";
        String baseUrl = scheme + "://" + url.host() + ":" + url.port();
        HttpClient.Builder b = HttpClient.newBuilder()
            .version(HttpClient.Version.HTTP_1_1)
            .connectTimeout(opts.timeout());
        if (url.isTls()) {
            try {
                b.sslContext(SSLContext.getDefault());
            } catch (Exception e) {
                throw new RedDBException.ProtocolError("default SSLContext unavailable: " + e.getMessage(), e);
            }
        }
        HttpClient client = b.build();

        String token = opts.token() != null ? opts.token() : url.token();
        HttpConn conn = new HttpConn(client, baseUrl, token, opts.timeout());

        // Auto-login when the URL / options carry credentials but no token.
        if (token == null) {
            String user = opts.username() != null ? opts.username() : url.username();
            String pass = opts.password() != null ? opts.password() : url.password();
            if (user != null && pass != null) {
                conn.login(user, pass);
            }
        }
        return conn;
    }

    /** POST /auth/login → updates this connection's bearer token. */
    public void login(String username, String password) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("username", username);
        body.put("password", password);
        byte[] resp = post("/auth/login", body, false);
        try {
            JsonNode j = MAPPER.readTree(resp);
            JsonNode tok = j.get("token");
            if (tok == null || !tok.isTextual()) {
                // Some envelopes wrap as { ok, result: { token } }
                JsonNode inner = j.get("result");
                if (inner != null && inner.isObject()) tok = inner.get("token");
            }
            if (tok == null || !tok.isTextual()) {
                throw new RedDBException.ProtocolError("auth/login response missing 'token'");
            }
            this.token = tok.asText();
        } catch (IOException e) {
            throw new RedDBException.ProtocolError("auth/login: invalid JSON: " + e.getMessage(), e);
        }
    }

    @Override
    public byte[] query(String sql) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("sql", sql);
        return post("/query", body, true);
    }

    @Override
    public void insert(String collection, Object payload) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("collection", collection);
        body.set("payload", MAPPER.valueToTree(payload));
        post("/insert", body, true);
    }

    @Override
    public void bulkInsert(String collection, List<?> rows) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("collection", collection);
        body.set("payloads", MAPPER.valueToTree(rows));
        post("/bulk_insert", body, true);
    }

    @Override
    public byte[] get(String collection, String id) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("collection", collection);
        body.put("id", id);
        return post("/get", body, true);
    }

    @Override
    public void delete(String collection, String id) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("collection", collection);
        body.put("id", id);
        post("/delete", body, true);
    }

    @Override
    public void ping() {
        // GET /admin/health — anything 2xx counts as healthy.
        HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(baseUrl + "/admin/health"))
            .timeout(timeout)
            .header("accept", "application/json")
            .GET();
        if (token != null) {
            b = b.header("authorization", "Bearer " + token);
        }
        try {
            HttpResponse<byte[]> resp = client.send(b.build(), HttpResponse.BodyHandlers.ofByteArray());
            if (resp.statusCode() / 100 != 2) {
                throw new RedDBException.EngineError(
                    "ping: HTTP " + resp.statusCode() + ": " + new String(resp.body(), StandardCharsets.UTF_8));
            }
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) Thread.currentThread().interrupt();
            throw new RedDBException.ProtocolError("ping I/O: " + e.getMessage(), e);
        }
    }

    @Override
    public void close() {
        // HttpClient is stateless; nothing to release.
        closed = true;
    }

    public String token() { return token; }
    public boolean isClosed() { return closed; }

    /** POST a JSON body and return the raw response bytes. */
    private byte[] post(String path, ObjectNode body, boolean requireAuthHeader) {
        try {
            byte[] payload = MAPPER.writeValueAsBytes(body);
            HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(baseUrl + path))
                .timeout(timeout)
                .header("content-type", "application/json")
                .header("accept", "application/json")
                .POST(HttpRequest.BodyPublishers.ofByteArray(payload));
            if (requireAuthHeader && token != null) {
                b = b.header("authorization", "Bearer " + token);
            }
            HttpResponse<byte[]> resp = client.send(b.build(), HttpResponse.BodyHandlers.ofByteArray());
            int sc = resp.statusCode();
            byte[] respBody = resp.body();
            if (sc / 100 != 2) {
                String msg = respBody == null ? "" : new String(respBody, StandardCharsets.UTF_8);
                if (sc == 401 || sc == 403) {
                    throw new RedDBException.AuthRefused("HTTP " + sc + " " + path + ": " + msg);
                }
                throw new RedDBException.EngineError("HTTP " + sc + " " + path + ": " + msg);
            }
            return respBody;
        } catch (IOException e) {
            throw new RedDBException.ProtocolError(path + " I/O: " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new RedDBException.ProtocolError(path + " interrupted", e);
        }
    }

    private static String stripTrailingSlash(String s) {
        if (s == null) return null;
        return s.endsWith("/") ? s.substring(0, s.length() - 1) : s;
    }
}
