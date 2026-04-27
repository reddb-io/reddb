package dev.reddb;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import dev.reddb.redwire.Frame;
import dev.reddb.redwire.RedWireConn;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayOutputStream;
import java.io.DataInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.PipedInputStream;
import java.io.PipedOutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Drives the handshake state machine over a pair of piped streams so
 * we don't need a TCP listener. The "server" runs on a daemon thread
 * and reads/writes opposite ends of two pipes.
 */
class RedwireConnTest {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    /**
     * Bidirectional pipe pair. {@code clientIn} / {@code clientOut}
     * are what the driver sees; {@code serverIn} / {@code serverOut}
     * are the matching ends the fake server uses.
     */
    static final class Pipes {
        final PipedInputStream clientIn = new PipedInputStream(64 * 1024);
        final PipedOutputStream clientOut = new PipedOutputStream();
        final PipedInputStream serverIn = new PipedInputStream(64 * 1024);
        final PipedOutputStream serverOut = new PipedOutputStream();
        Pipes() throws IOException {
            // server writes to serverOut → clientIn reads
            serverOut.connect(clientIn);
            // client writes to clientOut → serverIn reads
            clientOut.connect(serverIn);
        }
    }

    private static long readU32(InputStream in) throws IOException {
        DataInputStream din = new DataInputStream(in);
        byte[] b = new byte[4];
        din.readFully(b);
        return ByteBuffer.wrap(b).order(ByteOrder.LITTLE_ENDIAN).getInt() & 0xffffffffL;
    }

    /** Read one frame from the server side of the pipe. */
    static Frame readClientFrame(InputStream in) throws IOException {
        DataInputStream din = new DataInputStream(in);
        byte[] header = new byte[Frame.HEADER_SIZE];
        din.readFully(header);
        int len = ByteBuffer.wrap(header, 0, 4).order(ByteOrder.LITTLE_ENDIAN).getInt();
        byte[] full = new byte[len];
        System.arraycopy(header, 0, full, 0, Frame.HEADER_SIZE);
        if (len > Frame.HEADER_SIZE) {
            din.readFully(full, Frame.HEADER_SIZE, len - Frame.HEADER_SIZE);
        }
        return Frame.decode(full);
    }

    static void writeServerFrame(OutputStream out, int kind, long correlationId, byte[] payload) throws IOException {
        Frame f = new Frame(kind, 0, 0, correlationId, payload);
        out.write(Frame.encode(f));
        out.flush();
    }

    /** Read the magic preamble + minor version byte the client sends first. */
    static void readMagic(InputStream in) throws IOException {
        DataInputStream din = new DataInputStream(in);
        int magic = din.readUnsignedByte();
        int version = din.readUnsignedByte();
        assertEquals(0xfe, magic, "client did not send the magic 0xFE byte");
        assertEquals(1, version, "client did not send minor version 1");
    }

    @Test
    void handshakeAnonymousSucceeds() throws Exception {
        Pipes p = new Pipes();
        AtomicReference<Throwable> serverErr = new AtomicReference<>();

        Thread server = new Thread(() -> {
            try {
                readMagic(p.serverIn);
                Frame hello = readClientFrame(p.serverIn);
                assertEquals(Frame.Kind.Hello, hello.kind);
                JsonNode helloJson = MAPPER.readTree(hello.payload);
                assertTrue(helloJson.get("auth_methods").toString().contains("anonymous"));

                ObjectNode ack = MAPPER.createObjectNode();
                ack.put("auth", "anonymous");
                ack.put("version", 1);
                ack.put("features", 0);
                writeServerFrame(p.serverOut, Frame.Kind.HelloAck, hello.correlationId, MAPPER.writeValueAsBytes(ack));

                Frame resp = readClientFrame(p.serverIn);
                assertEquals(Frame.Kind.AuthResponse, resp.kind);
                assertEquals(0, resp.payload.length);

                ObjectNode ok = MAPPER.createObjectNode();
                ok.put("session_id", "rwsess-test-anon");
                ok.put("username", "anonymous");
                ok.put("role", "read");
                writeServerFrame(p.serverOut, Frame.Kind.AuthOk, resp.correlationId, MAPPER.writeValueAsBytes(ok));
            } catch (Throwable t) {
                serverErr.set(t);
            }
        }, "fake-redwire-server-anon");
        server.setDaemon(true);
        server.start();

        RedWireConn.HandshakeResult res = RedWireConn.performHandshake(
            p.clientIn, p.clientOut, null, null, null, "test-driver");
        server.join(5_000);
        if (serverErr.get() != null) throw new AssertionError("server thread", serverErr.get());

        assertEquals("rwsess-test-anon", res.sessionId);
    }

    @Test
    void handshakeBearerSucceeds() throws Exception {
        Pipes p = new Pipes();
        AtomicReference<Throwable> serverErr = new AtomicReference<>();

        Thread server = new Thread(() -> {
            try {
                readMagic(p.serverIn);
                Frame hello = readClientFrame(p.serverIn);
                JsonNode helloJson = MAPPER.readTree(hello.payload);
                assertTrue(helloJson.get("auth_methods").toString().contains("bearer"));

                ObjectNode ack = MAPPER.createObjectNode();
                ack.put("auth", "bearer");
                writeServerFrame(p.serverOut, Frame.Kind.HelloAck, hello.correlationId, MAPPER.writeValueAsBytes(ack));

                Frame resp = readClientFrame(p.serverIn);
                JsonNode r = MAPPER.readTree(resp.payload);
                assertEquals("the-token", r.get("token").asText());

                ObjectNode ok = MAPPER.createObjectNode();
                ok.put("session_id", "rwsess-test-bearer");
                writeServerFrame(p.serverOut, Frame.Kind.AuthOk, resp.correlationId, MAPPER.writeValueAsBytes(ok));
            } catch (Throwable t) {
                serverErr.set(t);
            }
        }, "fake-redwire-server-bearer");
        server.setDaemon(true);
        server.start();

        RedWireConn.HandshakeResult res = RedWireConn.performHandshake(
            p.clientIn, p.clientOut, null, null, "the-token", "test-driver");
        server.join(5_000);
        if (serverErr.get() != null) throw new AssertionError("server thread", serverErr.get());

        assertEquals("rwsess-test-bearer", res.sessionId);
    }

    @Test
    void authFailAtHelloAckThrowsAuthRefused() throws Exception {
        Pipes p = new Pipes();
        Thread server = new Thread(() -> {
            try {
                readMagic(p.serverIn);
                Frame hello = readClientFrame(p.serverIn);
                ObjectNode reason = MAPPER.createObjectNode();
                reason.put("reason", "no overlapping auth method");
                writeServerFrame(p.serverOut, Frame.Kind.AuthFail, hello.correlationId, MAPPER.writeValueAsBytes(reason));
            } catch (Throwable ignored) {
                // pipe close after we send the failure is fine
            }
        }, "fake-redwire-server-fail-ack");
        server.setDaemon(true);
        server.start();

        RedDBException.AuthRefused err = assertThrows(RedDBException.AuthRefused.class,
            () -> RedWireConn.performHandshake(p.clientIn, p.clientOut, null, null, null, "test-driver"));
        assertTrue(err.getMessage().contains("no overlapping auth method"), err.getMessage());
    }

    @Test
    void authFailAtAuthOkThrowsAuthRefused() throws Exception {
        Pipes p = new Pipes();
        Thread server = new Thread(() -> {
            try {
                readMagic(p.serverIn);
                Frame hello = readClientFrame(p.serverIn);
                ObjectNode ack = MAPPER.createObjectNode();
                ack.put("auth", "bearer");
                writeServerFrame(p.serverOut, Frame.Kind.HelloAck, hello.correlationId, MAPPER.writeValueAsBytes(ack));

                Frame resp = readClientFrame(p.serverIn);
                ObjectNode reason = MAPPER.createObjectNode();
                reason.put("reason", "bearer token invalid");
                writeServerFrame(p.serverOut, Frame.Kind.AuthFail, resp.correlationId, MAPPER.writeValueAsBytes(reason));
            } catch (Throwable ignored) {
            }
        }, "fake-redwire-server-fail-ok");
        server.setDaemon(true);
        server.start();

        RedDBException.AuthRefused err = assertThrows(RedDBException.AuthRefused.class,
            () -> RedWireConn.performHandshake(p.clientIn, p.clientOut, null, null, "bad-token", "test-driver"));
        assertTrue(err.getMessage().contains("bearer token invalid"));
    }

    @Test
    void serverPicksUnsupportedAuthMethodThrowsProtocol() throws Exception {
        Pipes p = new Pipes();
        Thread server = new Thread(() -> {
            try {
                readMagic(p.serverIn);
                Frame hello = readClientFrame(p.serverIn);
                ObjectNode ack = MAPPER.createObjectNode();
                ack.put("auth", "made-up-method");
                writeServerFrame(p.serverOut, Frame.Kind.HelloAck, hello.correlationId, MAPPER.writeValueAsBytes(ack));
            } catch (Throwable ignored) {
            }
        }, "fake-redwire-server-bogus-method");
        server.setDaemon(true);
        server.start();

        RedDBException.ProtocolError err = assertThrows(RedDBException.ProtocolError.class,
            () -> RedWireConn.performHandshake(p.clientIn, p.clientOut, null, null, null, "test-driver"));
        assertTrue(err.getMessage().contains("made-up-method"));
    }

    @Test
    void malformedHelloAckJsonRaisesProtocolError() throws Exception {
        Pipes p = new Pipes();
        Thread server = new Thread(() -> {
            try {
                readMagic(p.serverIn);
                Frame hello = readClientFrame(p.serverIn);
                writeServerFrame(p.serverOut, Frame.Kind.HelloAck, hello.correlationId,
                    "not json".getBytes(StandardCharsets.UTF_8));
            } catch (Throwable ignored) {
            }
        }, "fake-redwire-server-bad-json");
        server.setDaemon(true);
        server.start();

        assertThrows(RedDBException.ProtocolError.class,
            () -> RedWireConn.performHandshake(p.clientIn, p.clientOut, null, null, null, "test-driver"));
    }

    @Test
    void queryRoundTripAfterHandshake() throws Exception {
        Pipes p = new Pipes();
        AtomicReference<Throwable> serverErr = new AtomicReference<>();

        Thread server = new Thread(() -> {
            try {
                readMagic(p.serverIn);
                Frame hello = readClientFrame(p.serverIn);
                ObjectNode ack = MAPPER.createObjectNode();
                ack.put("auth", "anonymous");
                writeServerFrame(p.serverOut, Frame.Kind.HelloAck, hello.correlationId, MAPPER.writeValueAsBytes(ack));

                Frame resp = readClientFrame(p.serverIn);
                ObjectNode ok = MAPPER.createObjectNode();
                ok.put("session_id", "rwsess-test-q");
                writeServerFrame(p.serverOut, Frame.Kind.AuthOk, resp.correlationId, MAPPER.writeValueAsBytes(ok));

                // Now respond to a Query frame.
                Frame q = readClientFrame(p.serverIn);
                assertEquals(Frame.Kind.Query, q.kind);
                assertEquals("SELECT 1", new String(q.payload, StandardCharsets.UTF_8));
                ObjectNode result = MAPPER.createObjectNode();
                result.put("ok", true);
                result.put("affected", 1);
                writeServerFrame(p.serverOut, Frame.Kind.Result, q.correlationId, MAPPER.writeValueAsBytes(result));

                // Drain a Bye frame so the test cleans up.
                readClientFrame(p.serverIn);
            } catch (Throwable t) {
                serverErr.set(t);
            }
        }, "fake-redwire-server-query");
        server.setDaemon(true);
        server.start();

        RedWireConn.HandshakeResult res = RedWireConn.performHandshake(
            p.clientIn, p.clientOut, null, null, null, "test-driver");
        assertEquals("rwsess-test-q", res.sessionId);

        // Wrap the streams in a connection and run a query.
        RedWireConn conn = new RedWireConn(p.clientIn, p.clientOut, () -> {}, res.sessionId);
        byte[] resultBytes = conn.query("SELECT 1");
        JsonNode result = MAPPER.readTree(resultBytes);
        assertTrue(result.get("ok").asBoolean());
        assertEquals(1, result.get("affected").asInt());
        conn.close();

        server.join(5_000);
        if (serverErr.get() != null) throw new AssertionError("server thread", serverErr.get());
    }

    @Test
    void clientSendsMagicByteFirst() throws Exception {
        // Spin a tiny "server" that captures the first two bytes off the wire.
        Pipes p = new Pipes();
        ByteArrayOutputStream prefix = new ByteArrayOutputStream();
        Thread server = new Thread(() -> {
            try {
                DataInputStream din = new DataInputStream(p.serverIn);
                prefix.write(din.readUnsignedByte());
                prefix.write(din.readUnsignedByte());
                // Then drain Hello to release the pipe; immediately fail with AuthFail.
                Frame hello = readClientFrame(p.serverIn);
                ObjectNode reason = MAPPER.createObjectNode();
                reason.put("reason", "stop here");
                writeServerFrame(p.serverOut, Frame.Kind.AuthFail, hello.correlationId, MAPPER.writeValueAsBytes(reason));
            } catch (Throwable ignored) {
            }
        });
        server.setDaemon(true);
        server.start();

        assertThrows(RedDBException.AuthRefused.class,
            () -> RedWireConn.performHandshake(p.clientIn, p.clientOut, null, null, null, "test-driver"));
        server.join(5_000);
        byte[] header = prefix.toByteArray();
        assertEquals((byte) 0xfe, header[0]);
        assertEquals((byte) 0x01, header[1]);
    }
}
