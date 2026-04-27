package dev.reddb.redwire;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import dev.reddb.Conn;
import dev.reddb.Options;
import dev.reddb.RedDBException;
import dev.reddb.Url;

import javax.net.ssl.SNIHostName;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLSocket;
import javax.net.ssl.SSLSocketFactory;
import java.io.Closeable;
import java.io.DataInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.atomic.AtomicLong;

/**
 * RedWire client over a single TCP socket.
 *
 * One {@link RedWireConn} owns one socket, one mutex, and a
 * monotonic correlation id. All public methods are blocking; the
 * caller can serialise them with normal Java locks.
 */
public final class RedWireConn implements Conn {
    private static final ObjectMapper MAPPER = new ObjectMapper();

    private final InputStream in;
    private final OutputStream out;
    private final Closeable owner;
    private final Object lock = new Object();
    private final AtomicLong nextCorrelation = new AtomicLong(1);
    private final String sessionId;
    private volatile boolean closed;

    /** Raw constructor — caller already has streams + something to close. Tests use this. */
    public RedWireConn(InputStream in, OutputStream out, Closeable owner, String sessionId) {
        this.in = in;
        this.out = out;
        this.owner = owner;
        this.sessionId = sessionId;
    }

    /** Open a TCP / TLS connection and run the RedWire handshake. */
    public static RedWireConn connect(Url url, Options opts) {
        if (!url.isRedwire()) {
            throw new IllegalArgumentException("RedWireConn.connect requires red:// or reds://, got " + url.kind());
        }
        Socket socket = null;
        try {
            int port = url.port();
            socket = url.isTls()
                ? openTls(url.host(), port)
                : new Socket(url.host(), port);
            socket.setTcpNoDelay(true);
            int timeoutMs = (int) Math.min(Integer.MAX_VALUE, opts.timeout().toMillis());
            socket.setSoTimeout(timeoutMs);

            InputStream rawIn = socket.getInputStream();
            OutputStream rawOut = socket.getOutputStream();

            String token = opts.token() != null ? opts.token() : url.token();
            String username = opts.username() != null ? opts.username() : url.username();
            String password = opts.password() != null ? opts.password() : url.password();
            String clientName = opts.clientName() != null ? opts.clientName() : "reddb-jvm/0.1";

            HandshakeResult handshake = performHandshake(rawIn, rawOut, username, password, token, clientName);
            return new RedWireConn(rawIn, rawOut, socket, handshake.sessionId);
        } catch (IOException e) {
            closeQuietly(socket);
            throw new RedDBException.ProtocolError("redwire connect failed: " + e.getMessage(), e);
        } catch (RuntimeException e) {
            closeQuietly(socket);
            throw e;
        }
    }

    /**
     * Drive the handshake on raw streams. Public + static so tests
     * can run it over piped streams without a socket.
     *
     * @param username  may be null when no auth or token-only
     * @param password  may be null
     * @param token     may be null when anonymous / SCRAM
     */
    public static HandshakeResult performHandshake(InputStream in, OutputStream out,
                                                   String username, String password, String token,
                                                   String clientName) throws IOException {
        // 1. Magic preamble. The minor-version byte rides along so the server
        // can fail fast on a future client.
        out.write(new byte[]{Frame.MAGIC, Frame.SUPPORTED_VERSION});
        out.flush();

        // 2. Hello — advertise every method this driver actually supports.
        List<String> methods;
        if (token != null) {
            methods = Collections.singletonList("bearer");
        } else if (username != null && password != null) {
            // SCRAM-first when we have credentials; bearer falls in as a
            // backup so a server that hasn't migrated to SCRAM still works
            // (the wrong password fails downstream, not in negotiation).
            methods = List.of("scram-sha-256", "bearer");
        } else {
            methods = List.of("anonymous", "bearer");
        }

        ObjectNode hello = MAPPER.createObjectNode();
        ArrayNode versions = hello.putArray("versions");
        versions.add(Frame.SUPPORTED_VERSION & 0xff);
        ArrayNode authMethods = hello.putArray("auth_methods");
        for (String m : methods) authMethods.add(m);
        hello.put("features", 0);
        if (clientName != null) hello.put("client_name", clientName);
        writeFrame(out, new Frame(Frame.Kind.Hello, 1L, MAPPER.writeValueAsBytes(hello)));

        // 3. HelloAck or AuthFail.
        Frame ack = readFrame(in);
        if (ack.kind == Frame.Kind.AuthFail) {
            throw new RedDBException.AuthRefused(reason(ack.payload, "AuthFail at HelloAck"));
        }
        if (ack.kind != Frame.Kind.HelloAck) {
            throw new RedDBException.ProtocolError(
                "expected HelloAck, got " + Frame.Kind.name(ack.kind));
        }
        JsonNode ackJson = parseJson(ack.payload, "HelloAck");
        String chosen = textField(ackJson, "auth");
        if (chosen == null) {
            throw new RedDBException.ProtocolError("HelloAck missing 'auth' field");
        }

        // 4. Auth dispatch.
        switch (chosen) {
            case "anonymous":
                writeFrame(out, new Frame(Frame.Kind.AuthResponse, 2L, new byte[0]));
                return finishOneRtt(in);
            case "bearer": {
                if (token == null) {
                    throw new RedDBException.AuthRefused(
                        "server demanded bearer but no token was supplied");
                }
                ObjectNode body = MAPPER.createObjectNode();
                body.put("token", token);
                writeFrame(out, new Frame(Frame.Kind.AuthResponse, 2L, MAPPER.writeValueAsBytes(body)));
                return finishOneRtt(in);
            }
            case "scram-sha-256": {
                if (username == null || password == null) {
                    throw new RedDBException.AuthRefused(
                        "server picked scram-sha-256 but no username/password configured");
                }
                return performScram(in, out, username, password);
            }
            case "oauth-jwt": {
                if (token == null) {
                    throw new RedDBException.AuthRefused(
                        "server picked oauth-jwt but no JWT token configured");
                }
                ObjectNode body = MAPPER.createObjectNode();
                body.put("jwt", token);
                writeFrame(out, new Frame(Frame.Kind.AuthResponse, 2L, MAPPER.writeValueAsBytes(body)));
                return finishOneRtt(in);
            }
            default:
                throw new RedDBException.ProtocolError(
                    "server picked unsupported auth method: " + chosen);
        }
    }

    private static HandshakeResult finishOneRtt(InputStream in) throws IOException {
        Frame f = readFrame(in);
        if (f.kind == Frame.Kind.AuthFail) {
            throw new RedDBException.AuthRefused(reason(f.payload, "auth refused"));
        }
        if (f.kind != Frame.Kind.AuthOk) {
            throw new RedDBException.ProtocolError(
                "expected AuthOk, got " + Frame.Kind.name(f.kind));
        }
        JsonNode j = parseJson(f.payload, "AuthOk");
        String sid = textField(j, "session_id");
        return new HandshakeResult(sid == null ? "" : sid);
    }

    private static HandshakeResult performScram(InputStream in, OutputStream out,
                                                String username, String password) throws IOException {
        // RFC 5802 § 3 — three round trips after the version byte.
        String clientNonce = Scram.newClientNonce();
        String clientFirst = Scram.clientFirst(username, clientNonce);
        String clientFirstBare = Scram.clientFirstBare(clientFirst);

        ObjectNode cf = MAPPER.createObjectNode();
        cf.put("client_first", clientFirst);
        writeFrame(out, new Frame(Frame.Kind.AuthResponse, 2L, MAPPER.writeValueAsBytes(cf)));

        Frame chall = readFrame(in);
        if (chall.kind == Frame.Kind.AuthFail) {
            throw new RedDBException.AuthRefused(reason(chall.payload, "scram challenge refused"));
        }
        if (chall.kind != Frame.Kind.AuthRequest) {
            throw new RedDBException.ProtocolError(
                "scram: expected AuthRequest, got " + Frame.Kind.name(chall.kind));
        }
        // Server may carry the server-first message either as the JSON
        // {"server_first": "..."} (current driver wire) or as a raw
        // string payload — handle both.
        String serverFirstStr = scramServerFirst(chall.payload);
        Scram.ServerFirst sf = Scram.parseServerFirst(serverFirstStr, clientNonce);

        String clientFinalNoProof = Scram.clientFinalNoProof(sf.combinedNonce);
        byte[] authMessage = Scram.authMessage(clientFirstBare, sf.raw, clientFinalNoProof);
        byte[] proof = Scram.clientProof(password, sf.salt, sf.iter, authMessage);
        String clientFinal = Scram.clientFinal(sf.combinedNonce, proof);

        ObjectNode cfin = MAPPER.createObjectNode();
        cfin.put("client_final", clientFinal);
        writeFrame(out, new Frame(Frame.Kind.AuthResponse, 3L, MAPPER.writeValueAsBytes(cfin)));

        Frame ok = readFrame(in);
        if (ok.kind == Frame.Kind.AuthFail) {
            throw new RedDBException.AuthRefused(reason(ok.payload, "scram refused"));
        }
        if (ok.kind != Frame.Kind.AuthOk) {
            throw new RedDBException.ProtocolError(
                "scram: expected AuthOk, got " + Frame.Kind.name(ok.kind));
        }
        JsonNode j = parseJson(ok.payload, "AuthOk");
        String sid = textField(j, "session_id");
        // Verify server signature when present. Engine sends it as
        // base64 under "v" (matches build_scram_auth_ok). Drivers
        // can also see it under "server_signature" as hex; accept
        // either to stay forward-compatible.
        byte[] sig = parseServerSignature(j);
        if (sig != null && !Scram.verifyServerSignature(password, sf.salt, sf.iter, authMessage, sig)) {
            throw new RedDBException.AuthRefused(
                "scram: server signature did not verify — possible MITM");
        }
        return new HandshakeResult(sid == null ? "" : sid);
    }

    /** Pull the server-first string out of an AuthRequest payload. */
    private static String scramServerFirst(byte[] payload) {
        // Engine emits it as the raw `r=...,s=...,i=...` body — but
        // the JS / Rust drivers tolerate a JSON envelope too, so we
        // do the same here.
        if (payload.length > 0 && payload[0] == '{') {
            JsonNode j = parseJson(payload, "AuthRequest");
            String s = textField(j, "server_first");
            if (s == null) {
                throw new RedDBException.ProtocolError("AuthRequest JSON missing 'server_first'");
            }
            return s;
        }
        return new String(payload, StandardCharsets.UTF_8);
    }

    private static byte[] parseServerSignature(JsonNode authOk) {
        JsonNode v = authOk.get("v");
        if (v != null && v.isTextual()) {
            try {
                return java.util.Base64.getDecoder().decode(v.asText());
            } catch (IllegalArgumentException ignore) {
                // fall through and try other shapes
            }
        }
        JsonNode hex = authOk.get("server_signature");
        if (hex != null && hex.isTextual()) {
            return decodeHex(hex.asText());
        }
        return null;
    }

    private static byte[] decodeHex(String s) {
        if (s.length() % 2 != 0) return null;
        byte[] out = new byte[s.length() / 2];
        for (int i = 0; i < out.length; i++) {
            int hi = Character.digit(s.charAt(i * 2), 16);
            int lo = Character.digit(s.charAt(i * 2 + 1), 16);
            if (hi < 0 || lo < 0) return null;
            out[i] = (byte) ((hi << 4) | lo);
        }
        return out;
    }

    // ---------------------------------------------------------------
    // Conn methods
    // ---------------------------------------------------------------

    @Override
    public byte[] query(String sql) {
        synchronized (lock) {
            ensureOpen();
            long corr = nextCorrelation.getAndIncrement();
            try {
                writeFrame(out, new Frame(Frame.Kind.Query, corr, sql.getBytes(StandardCharsets.UTF_8)));
                Frame resp = readFrame(in);
                if (resp.kind == Frame.Kind.Result) return resp.payload;
                if (resp.kind == Frame.Kind.Error) {
                    throw new RedDBException.EngineError(new String(resp.payload, StandardCharsets.UTF_8));
                }
                throw new RedDBException.ProtocolError(
                    "expected Result/Error, got " + Frame.Kind.name(resp.kind));
            } catch (IOException e) {
                throw new RedDBException.ProtocolError("query I/O: " + e.getMessage(), e);
            }
        }
    }

    @Override
    public void insert(String collection, Object payload) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("collection", collection);
        body.set("payload", MAPPER.valueToTree(payload));
        sendInsert(body);
    }

    @Override
    public void bulkInsert(String collection, List<?> rows) {
        ObjectNode body = MAPPER.createObjectNode();
        body.put("collection", collection);
        body.set("payloads", MAPPER.valueToTree(rows));
        sendInsert(body);
    }

    private void sendInsert(ObjectNode body) {
        synchronized (lock) {
            ensureOpen();
            long corr = nextCorrelation.getAndIncrement();
            try {
                byte[] bytes = MAPPER.writeValueAsBytes(body);
                writeFrame(out, new Frame(Frame.Kind.BulkInsert, corr, bytes));
                Frame resp = readFrame(in);
                if (resp.kind == Frame.Kind.BulkOk) return;
                if (resp.kind == Frame.Kind.Error) {
                    throw new RedDBException.EngineError(new String(resp.payload, StandardCharsets.UTF_8));
                }
                throw new RedDBException.ProtocolError(
                    "expected BulkOk/Error, got " + Frame.Kind.name(resp.kind));
            } catch (IOException e) {
                throw new RedDBException.ProtocolError("insert I/O: " + e.getMessage(), e);
            }
        }
    }

    @Override
    public byte[] get(String collection, String id) {
        synchronized (lock) {
            ensureOpen();
            long corr = nextCorrelation.getAndIncrement();
            try {
                ObjectNode body = MAPPER.createObjectNode();
                body.put("collection", collection);
                body.put("id", id);
                writeFrame(out, new Frame(Frame.Kind.Get, corr, MAPPER.writeValueAsBytes(body)));
                Frame resp = readFrame(in);
                if (resp.kind == Frame.Kind.Result) return resp.payload;
                if (resp.kind == Frame.Kind.Error) {
                    throw new RedDBException.EngineError(new String(resp.payload, StandardCharsets.UTF_8));
                }
                throw new RedDBException.ProtocolError(
                    "expected Result/Error, got " + Frame.Kind.name(resp.kind));
            } catch (IOException e) {
                throw new RedDBException.ProtocolError("get I/O: " + e.getMessage(), e);
            }
        }
    }

    @Override
    public void delete(String collection, String id) {
        synchronized (lock) {
            ensureOpen();
            long corr = nextCorrelation.getAndIncrement();
            try {
                ObjectNode body = MAPPER.createObjectNode();
                body.put("collection", collection);
                body.put("id", id);
                writeFrame(out, new Frame(Frame.Kind.Delete, corr, MAPPER.writeValueAsBytes(body)));
                Frame resp = readFrame(in);
                if (resp.kind == Frame.Kind.DeleteOk) return;
                if (resp.kind == Frame.Kind.Error) {
                    throw new RedDBException.EngineError(new String(resp.payload, StandardCharsets.UTF_8));
                }
                throw new RedDBException.ProtocolError(
                    "expected DeleteOk/Error, got " + Frame.Kind.name(resp.kind));
            } catch (IOException e) {
                throw new RedDBException.ProtocolError("delete I/O: " + e.getMessage(), e);
            }
        }
    }

    @Override
    public void ping() {
        synchronized (lock) {
            ensureOpen();
            long corr = nextCorrelation.getAndIncrement();
            try {
                writeFrame(out, new Frame(Frame.Kind.Ping, corr, new byte[0]));
                Frame resp = readFrame(in);
                if (resp.kind != Frame.Kind.Pong) {
                    throw new RedDBException.ProtocolError(
                        "expected Pong, got " + Frame.Kind.name(resp.kind));
                }
            } catch (IOException e) {
                throw new RedDBException.ProtocolError("ping I/O: " + e.getMessage(), e);
            }
        }
    }

    @Override
    public void close() {
        synchronized (lock) {
            if (closed) return;
            closed = true;
            try {
                long corr = nextCorrelation.getAndIncrement();
                writeFrame(out, new Frame(Frame.Kind.Bye, corr, new byte[0]));
            } catch (Throwable ignore) {
                // best-effort
            }
            closeQuietly(owner);
        }
    }

    public String sessionId() { return sessionId; }

    // ---------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------

    private void ensureOpen() {
        if (closed) {
            throw new IllegalStateException("RedWireConn is closed");
        }
    }

    /** Write a fully-encoded frame to {@code out} and flush. */
    static void writeFrame(OutputStream out, Frame frame) throws IOException {
        byte[] bytes = Frame.encode(frame);
        out.write(bytes);
        out.flush();
    }

    /** Read exactly one frame from {@code in}, blocking on partial reads. */
    static Frame readFrame(InputStream in) throws IOException {
        DataInputStream din = in instanceof DataInputStream ? (DataInputStream) in : new DataInputStream(in);
        byte[] header = new byte[Frame.HEADER_SIZE];
        din.readFully(header);
        int length = Frame.encodedLength(header);
        if (length < Frame.HEADER_SIZE || length > Frame.MAX_FRAME_SIZE) {
            throw new RedDBException.FrameTooLarge("frame length out of range: " + length);
        }
        byte[] full = new byte[length];
        System.arraycopy(header, 0, full, 0, Frame.HEADER_SIZE);
        if (length > Frame.HEADER_SIZE) {
            din.readFully(full, Frame.HEADER_SIZE, length - Frame.HEADER_SIZE);
        }
        return Frame.decode(full);
    }

    private static String reason(byte[] payload, String fallback) {
        if (payload == null || payload.length == 0) return fallback;
        try {
            JsonNode n = MAPPER.readTree(payload);
            JsonNode r = n.get("reason");
            if (r != null && r.isTextual()) return r.asText();
        } catch (Exception ignore) {
            // payload may not be JSON
        }
        return new String(payload, StandardCharsets.UTF_8);
    }

    private static JsonNode parseJson(byte[] payload, String label) {
        try {
            return MAPPER.readTree(payload);
        } catch (Exception e) {
            throw new RedDBException.ProtocolError(label + ": invalid JSON: " + e.getMessage());
        }
    }

    private static String textField(JsonNode node, String name) {
        if (node == null || !node.isObject()) return null;
        JsonNode v = node.get(name);
        return (v != null && v.isTextual()) ? v.asText() : null;
    }

    private static SSLSocket openTls(String host, int port) throws IOException {
        SSLContext ctx;
        try {
            ctx = SSLContext.getDefault();
        } catch (Exception e) {
            throw new IOException("failed to obtain default SSLContext: " + e.getMessage(), e);
        }
        SSLSocketFactory factory = ctx.getSocketFactory();
        SSLSocket sock = (SSLSocket) factory.createSocket(host, port);
        SSLParameters params = sock.getSSLParameters();
        params.setApplicationProtocols(new String[]{"redwire/1"});
        params.setServerNames(Collections.singletonList(new SNIHostName(host)));
        sock.setSSLParameters(params);
        sock.startHandshake();
        return sock;
    }

    private static void closeQuietly(Closeable c) {
        if (c == null) return;
        try { c.close(); } catch (Throwable ignore) { /* nothing */ }
    }

    /** Outcome of a successful handshake — exposed mostly for tests. */
    public static final class HandshakeResult {
        public final String sessionId;
        public HandshakeResult(String sessionId) { this.sessionId = sessionId; }
    }
}
