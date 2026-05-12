package dev.reddb

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.node.ObjectNode
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import dev.reddb.redwire.Frame
import dev.reddb.redwire.MessageKind
import dev.reddb.redwire.RedwireConn
import io.ktor.utils.io.ByteChannel
import io.ktor.utils.io.ByteReadChannel
import io.ktor.utils.io.ByteWriteChannel
import io.ktor.utils.io.readFully
import io.ktor.utils.io.writeFully
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.StandardCharsets

/**
 * Drives the handshake state machine over a pair of in-memory ktor
 * [ByteChannel]s, so we don't need a TCP listener. The "server"
 * coroutine reads/writes the opposite ends.
 */
class RedwireConnTest {

    private val mapper: ObjectMapper = jacksonObjectMapper()

    /**
     * Bidirectional channel pair. `clientRead`/`clientWrite` are what
     * the driver sees; `serverRead`/`serverWrite` are the matching
     * ends the fake server uses.
     */
    private class Pipes {
        // Client writes -> server reads.
        val c2s = ByteChannel(autoFlush = true)
        // Server writes -> client reads.
        val s2c = ByteChannel(autoFlush = true)

        val clientRead: ByteReadChannel get() = s2c
        val clientWrite: ByteWriteChannel get() = c2s
        val serverRead: ByteReadChannel get() = c2s
        val serverWrite: ByteWriteChannel get() = s2c
    }

    /** Read one frame from the channel (server side). */
    private suspend fun readClientFrame(read: ByteReadChannel): Frame {
        val header = ByteArray(Frame.HEADER_SIZE)
        read.readFully(header, 0, Frame.HEADER_SIZE)
        val len = ByteBuffer.wrap(header, 0, 4).order(ByteOrder.LITTLE_ENDIAN).int
        val full = ByteArray(len)
        System.arraycopy(header, 0, full, 0, Frame.HEADER_SIZE)
        if (len > Frame.HEADER_SIZE) {
            read.readFully(full, Frame.HEADER_SIZE, len - Frame.HEADER_SIZE)
        }
        return Frame.decode(full)
    }

    private suspend fun writeServerFrame(write: ByteWriteChannel, kind: Int, correlationId: Long, payload: ByteArray) {
        val f = Frame(kind, 0, 0, correlationId, payload)
        write.writeFully(Frame.encode(f))
        write.flush()
    }

    /** Read the magic preamble + minor version byte. */
    private suspend fun readMagic(read: ByteReadChannel) {
        val magic = read.readByte().toInt() and 0xff
        val version = read.readByte().toInt() and 0xff
        assertEquals(0xfe, magic, "client did not send the magic 0xFE byte")
        assertEquals(1, version, "client did not send minor version 1")
    }

    @Test
    fun handshakeAnonymousSucceeds() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            coroutineScope {
                val server = launch(Dispatchers.IO) {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    assertEquals(MessageKind.Hello, hello.kind)
                    val helloJson = mapper.readTree(hello.payload)
                    assertTrue(helloJson.get("auth_methods").toString().contains("anonymous"))

                    val ack: ObjectNode = mapper.createObjectNode().apply {
                        put("auth", "anonymous")
                        put("version", 1)
                        put("features", 0)
                    }
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId, mapper.writeValueAsBytes(ack))

                    val resp = readClientFrame(p.serverRead)
                    assertEquals(MessageKind.AuthResponse, resp.kind)
                    assertEquals(0, resp.payload.size)

                    val ok: ObjectNode = mapper.createObjectNode().apply {
                        put("session_id", "rwsess-test-anon")
                        put("username", "anonymous")
                        put("role", "read")
                    }
                    writeServerFrame(p.serverWrite, MessageKind.AuthOk, resp.correlationId, mapper.writeValueAsBytes(ok))
                }

                val res = RedwireConn.performHandshake(
                    p.clientRead, p.clientWrite, null, null, null, "test-driver"
                )
                server.join()
                assertEquals("rwsess-test-anon", res.sessionId)
            }
        }
    }

    @Test
    fun handshakeBearerSucceeds() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            coroutineScope {
                val server = launch(Dispatchers.IO) {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    val helloJson = mapper.readTree(hello.payload)
                    assertTrue(helloJson.get("auth_methods").toString().contains("bearer"))

                    val ack: ObjectNode = mapper.createObjectNode().apply {
                        put("auth", "bearer")
                    }
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId, mapper.writeValueAsBytes(ack))

                    val resp = readClientFrame(p.serverRead)
                    val r = mapper.readTree(resp.payload)
                    assertEquals("the-token", r.get("token").asText())

                    val ok: ObjectNode = mapper.createObjectNode().apply {
                        put("session_id", "rwsess-test-bearer")
                    }
                    writeServerFrame(p.serverWrite, MessageKind.AuthOk, resp.correlationId, mapper.writeValueAsBytes(ok))
                }

                val res = RedwireConn.performHandshake(
                    p.clientRead, p.clientWrite, null, null, "the-token", "test-driver"
                )
                server.join()
                assertEquals("rwsess-test-bearer", res.sessionId)
            }
        }
    }

    @Test
    fun authFailAtHelloAckThrowsAuthRefused() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            val serverJob = CoroutineScope(Dispatchers.IO).async {
                runCatching {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    val reason: ObjectNode = mapper.createObjectNode().apply {
                        put("reason", "no overlapping auth method")
                    }
                    writeServerFrame(p.serverWrite, MessageKind.AuthFail, hello.correlationId, mapper.writeValueAsBytes(reason))
                }
            }
            val err = assertThrows(RedDBException.AuthRefused::class.java) {
                runBlocking {
                    RedwireConn.performHandshake(p.clientRead, p.clientWrite, null, null, null, "test-driver")
                }
            }
            serverJob.await()
            assertTrue(err.message!!.contains("no overlapping auth method"), err.message)
        }
    }

    @Test
    fun authFailAtAuthOkThrowsAuthRefused() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            val serverJob = CoroutineScope(Dispatchers.IO).async {
                runCatching {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    val ack: ObjectNode = mapper.createObjectNode().apply { put("auth", "bearer") }
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId, mapper.writeValueAsBytes(ack))

                    val resp = readClientFrame(p.serverRead)
                    val reason: ObjectNode = mapper.createObjectNode().apply { put("reason", "bearer token invalid") }
                    writeServerFrame(p.serverWrite, MessageKind.AuthFail, resp.correlationId, mapper.writeValueAsBytes(reason))
                }
            }
            val err = assertThrows(RedDBException.AuthRefused::class.java) {
                runBlocking {
                    RedwireConn.performHandshake(p.clientRead, p.clientWrite, null, null, "bad-token", "test-driver")
                }
            }
            serverJob.await()
            assertTrue(err.message!!.contains("bearer token invalid"))
        }
    }

    @Test
    fun serverPicksUnsupportedAuthMethodThrowsProtocol() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            val serverJob = CoroutineScope(Dispatchers.IO).async {
                runCatching {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    val ack: ObjectNode = mapper.createObjectNode().apply { put("auth", "made-up-method") }
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId, mapper.writeValueAsBytes(ack))
                }
            }
            val err = assertThrows(RedDBException.ProtocolError::class.java) {
                runBlocking {
                    RedwireConn.performHandshake(p.clientRead, p.clientWrite, null, null, null, "test-driver")
                }
            }
            serverJob.await()
            assertTrue(err.message!!.contains("made-up-method"))
        }
    }

    @Test
    fun malformedHelloAckJsonRaisesProtocolError() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            val serverJob = CoroutineScope(Dispatchers.IO).async {
                runCatching {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId,
                        "not json".toByteArray(StandardCharsets.UTF_8))
                }
            }
            assertThrows(RedDBException.ProtocolError::class.java) {
                runBlocking {
                    RedwireConn.performHandshake(p.clientRead, p.clientWrite, null, null, null, "test-driver")
                }
            }
            serverJob.await()
        }
    }

    @Test
    fun queryWithParamsUsesQueryWithParamsFrame() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            coroutineScope {
                val server = launch(Dispatchers.IO) {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    val ack: ObjectNode = mapper.createObjectNode().apply {
                        put("auth", "anonymous")
                        put("features", Frame.FEATURE_PARAMS)
                    }
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId, mapper.writeValueAsBytes(ack))

                    val resp = readClientFrame(p.serverRead)
                    val ok: ObjectNode = mapper.createObjectNode().apply {
                        put("session_id", "rwsess-test-params")
                        put("features", Frame.FEATURE_PARAMS)
                    }
                    writeServerFrame(p.serverWrite, MessageKind.AuthOk, resp.correlationId, mapper.writeValueAsBytes(ok))

                    val q = readClientFrame(p.serverRead)
                    assertEquals(MessageKind.QueryWithParams, q.kind)
                    val payload = ByteBuffer.wrap(q.payload).order(ByteOrder.LITTLE_ENDIAN)
                    val sqlLen = payload.int
                    val sqlBytes = ByteArray(sqlLen)
                    payload.get(sqlBytes)
                    assertEquals("SELECT $1, $2, $3, $4", String(sqlBytes, StandardCharsets.UTF_8))
                    assertEquals(4, payload.int)
                    assertEquals(0x02, payload.get().toInt() and 0xff)
                    assertEquals(42L, payload.long)
                    assertEquals(0x04, payload.get().toInt() and 0xff)
                    val textLen = payload.int
                    val textBytes = ByteArray(textLen)
                    payload.get(textBytes)
                    assertEquals("alice", String(textBytes, StandardCharsets.UTF_8))
                    assertEquals(0x00, payload.get().toInt() and 0xff)
                    assertEquals(0x06, payload.get().toInt() and 0xff)
                    assertEquals(3, payload.int)
                    assertEquals(1.0f, payload.float)
                    assertEquals(2.0f, payload.float)
                    assertEquals(3.0f, payload.float)

                    val result: ObjectNode = mapper.createObjectNode().put("ok", true)
                    writeServerFrame(p.serverWrite, MessageKind.Result, q.correlationId, mapper.writeValueAsBytes(result))

                    readClientFrame(p.serverRead)
                }

                val res = RedwireConn.performHandshake(
                    p.clientRead, p.clientWrite, null, null, null, "test-driver"
                )
                val conn = RedwireConn(p.clientRead, p.clientWrite, java.io.Closeable {}, res.sessionId, res.features)
                assertTrue(conn.supportsParams())
                val resultBytes = conn.query("SELECT $1, $2, $3, $4", 42, "alice", null, floatArrayOf(1f, 2f, 3f))
                assertTrue(mapper.readTree(resultBytes).get("ok").asBoolean())
                conn.close()
                server.join()
            }
        }
    }

    @Test
    fun queryWithParamsRequiresFeatureParams() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            coroutineScope {
                val server = launch(Dispatchers.IO) {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    val ack: ObjectNode = mapper.createObjectNode().apply {
                        put("auth", "anonymous")
                        put("features", 0)
                    }
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId, mapper.writeValueAsBytes(ack))

                    val resp = readClientFrame(p.serverRead)
                    val ok: ObjectNode = mapper.createObjectNode().apply {
                        put("session_id", "rwsess-test-no-params")
                        put("features", 0)
                    }
                    writeServerFrame(p.serverWrite, MessageKind.AuthOk, resp.correlationId, mapper.writeValueAsBytes(ok))
                }

                val res = RedwireConn.performHandshake(
                    p.clientRead, p.clientWrite, null, null, null, "test-driver"
                )
                val conn = RedwireConn(p.clientRead, p.clientWrite, java.io.Closeable {}, res.sessionId, res.features)
                assertFalse(conn.supportsParams())
                assertThrows(RedDBException.ParamsUnsupported::class.java) {
                    runBlocking { conn.query("SELECT $1", 1) }
                }
                server.join()
            }
        }
    }

    @Test
    fun queryWithEmptyParamsUsesLegacyQueryFrame() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            coroutineScope {
                val server = launch(Dispatchers.IO) {
                    readMagic(p.serverRead)
                    val hello = readClientFrame(p.serverRead)
                    val ack: ObjectNode = mapper.createObjectNode().apply {
                        put("auth", "anonymous")
                        put("features", Frame.FEATURE_PARAMS)
                    }
                    writeServerFrame(p.serverWrite, MessageKind.HelloAck, hello.correlationId, mapper.writeValueAsBytes(ack))

                    val resp = readClientFrame(p.serverRead)
                    val ok: ObjectNode = mapper.createObjectNode().apply {
                        put("session_id", "rwsess-test-empty-params")
                        put("features", Frame.FEATURE_PARAMS)
                    }
                    writeServerFrame(p.serverWrite, MessageKind.AuthOk, resp.correlationId, mapper.writeValueAsBytes(ok))

                    val q = readClientFrame(p.serverRead)
                    assertEquals(MessageKind.Query, q.kind)
                    assertEquals("SELECT 1", String(q.payload, StandardCharsets.UTF_8))
                    writeServerFrame(p.serverWrite, MessageKind.Result, q.correlationId, "{}".toByteArray(StandardCharsets.UTF_8))

                    readClientFrame(p.serverRead)
                }

                val res = RedwireConn.performHandshake(
                    p.clientRead, p.clientWrite, null, null, null, "test-driver"
                )
                val conn = RedwireConn(p.clientRead, p.clientWrite, java.io.Closeable {}, res.sessionId, res.features)
                conn.query("SELECT 1", *emptyArray<Any?>())
                conn.close()
                server.join()
            }
        }
    }

    @Test
    fun clientSendsMagicByteFirst() = runBlocking {
        withTimeout(5_000) {
            val p = Pipes()
            val captured = ByteArray(2)
            val serverJob = CoroutineScope(Dispatchers.IO).async {
                runCatching {
                    captured[0] = (p.serverRead.readByte().toInt() and 0xff).toByte()
                    captured[1] = (p.serverRead.readByte().toInt() and 0xff).toByte()
                    val hello = readClientFrame(p.serverRead)
                    val reason: ObjectNode = mapper.createObjectNode().apply { put("reason", "stop here") }
                    writeServerFrame(p.serverWrite, MessageKind.AuthFail, hello.correlationId, mapper.writeValueAsBytes(reason))
                }
            }
            assertThrows(RedDBException.AuthRefused::class.java) {
                runBlocking {
                    RedwireConn.performHandshake(p.clientRead, p.clientWrite, null, null, null, "test-driver")
                }
            }
            serverJob.await()
            assertArrayEquals(byteArrayOf(0xFE.toByte(), 0x01.toByte()), captured)
        }
    }
}
