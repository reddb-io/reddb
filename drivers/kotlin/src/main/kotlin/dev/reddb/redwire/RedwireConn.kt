package dev.reddb.redwire

import com.fasterxml.jackson.databind.JsonNode
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.node.ObjectNode
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import dev.reddb.Conn
import dev.reddb.Options
import dev.reddb.RedDBException
import dev.reddb.Url
import io.ktor.network.selector.SelectorManager
import io.ktor.network.sockets.InetSocketAddress
import io.ktor.network.sockets.Socket
import io.ktor.network.sockets.aSocket
import io.ktor.network.sockets.openReadChannel
import io.ktor.network.sockets.openWriteChannel
import io.ktor.network.tls.tls
import io.ktor.utils.io.ByteReadChannel
import io.ktor.utils.io.ByteWriteChannel
import io.ktor.utils.io.readFully
import io.ktor.utils.io.writeFully
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.Closeable
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.StandardCharsets
import java.util.Base64
import java.util.concurrent.atomic.AtomicLong

/**
 * RedWire client over a single TCP (or TLS) socket.
 *
 * One [RedwireConn] owns one socket, one [Mutex], and a monotonic
 * correlation id. Every public method is `suspend fun`; concurrent
 * callers serialise through the mutex so a single connection reads
 * exactly one response per request.
 */
public class RedwireConn internal constructor(
    private val readCh: ByteReadChannel,
    private val writeCh: ByteWriteChannel,
    private val owner: Closeable,
    public val sessionId: String,
) : Conn {

    private val mutex = Mutex()
    private val nextCorrelation = AtomicLong(1L)
    @Volatile private var closed = false

    public companion object {
        internal val MAPPER: ObjectMapper = jacksonObjectMapper()

        /** Open a TCP / TLS connection and run the RedWire handshake. */
        public suspend fun connect(url: Url, opts: Options): RedwireConn {
            require(url.isRedwire()) {
                "RedwireConn.connect requires red:// or reds://, got ${url.kind}"
            }
            val host = url.host ?: throw IllegalArgumentException("URL is missing a host")
            val selector = SelectorManager(Dispatchers.IO)
            var socket: Socket? = null
            try {
                val plainSocket = aSocket(selector).tcp().connect(InetSocketAddress(host, url.port))
                val activeSocket: Socket = if (url.isTls()) {
                    plainSocket.tls(Dispatchers.IO) {
                        serverName = host
                    }
                } else {
                    plainSocket
                }
                socket = activeSocket

                val readCh = activeSocket.openReadChannel()
                val writeCh = activeSocket.openWriteChannel(autoFlush = false)

                val token = opts.token ?: url.token
                val username = opts.username ?: url.username
                val password = opts.password ?: url.password
                val clientName = opts.clientName ?: "reddb-kotlin/0.1"

                val handshake = performHandshake(readCh, writeCh, username, password, token, clientName)

                // Wrap the socket and selector so close() releases both.
                val owner = Closeable {
                    try { activeSocket.close() } catch (_: Throwable) { /* ignore */ }
                    try { selector.close() } catch (_: Throwable) { /* ignore */ }
                }
                return RedwireConn(readCh, writeCh, owner, handshake.sessionId)
            } catch (e: RedDBException) {
                runCatching { socket?.close() }
                runCatching { selector.close() }
                throw e
            } catch (e: Throwable) {
                runCatching { socket?.close() }
                runCatching { selector.close() }
                throw RedDBException.ProtocolError("redwire connect failed: ${e.message}", e)
            }
        }

        /**
         * Drive the handshake on raw byte channels. Public + suspend so tests
         * can run it over an in-memory channel pair without a socket.
         */
        public suspend fun performHandshake(
            readCh: ByteReadChannel,
            writeCh: ByteWriteChannel,
            username: String?,
            password: String?,
            token: String?,
            clientName: String?,
        ): HandshakeResult {
            // 1. Magic preamble + minor version. Future major upgrades will
            // fail fast on this byte.
            writeCh.writeFully(byteArrayOf(Frame.MAGIC, Frame.SUPPORTED_VERSION))
            writeCh.flush()

            // 2. Hello — advertise every method this driver supports.
            val methods: List<String> = when {
                token != null -> listOf("bearer")
                username != null && password != null -> listOf("scram-sha-256", "bearer")
                else -> listOf("anonymous", "bearer")
            }

            val hello = MAPPER.createObjectNode().apply {
                putArray("versions").add(Frame.SUPPORTED_VERSION.toInt() and 0xff)
                putArray("auth_methods").also { arr -> methods.forEach { arr.add(it) } }
                put("features", 0)
                if (clientName != null) put("client_name", clientName)
            }
            writeFrame(writeCh, Frame(MessageKind.Hello, 1L, MAPPER.writeValueAsBytes(hello)))

            // 3. HelloAck or AuthFail.
            val ack = readFrame(readCh)
            if (ack.kind == MessageKind.AuthFail) {
                throw RedDBException.AuthRefused(reason(ack.payload, "AuthFail at HelloAck"))
            }
            if (ack.kind != MessageKind.HelloAck) {
                throw RedDBException.ProtocolError("expected HelloAck, got ${MessageKind.name(ack.kind)}")
            }
            val ackJson = parseJson(ack.payload, "HelloAck")
            val chosen = textField(ackJson, "auth")
                ?: throw RedDBException.ProtocolError("HelloAck missing 'auth' field")

            // 4. Auth dispatch.
            return when (chosen) {
                "anonymous" -> {
                    writeFrame(writeCh, Frame(MessageKind.AuthResponse, 2L, ByteArray(0)))
                    finishOneRtt(readCh)
                }
                "bearer" -> {
                    if (token == null) {
                        throw RedDBException.AuthRefused(
                            "server demanded bearer but no token was supplied"
                        )
                    }
                    val body = MAPPER.createObjectNode().put("token", token)
                    writeFrame(writeCh, Frame(MessageKind.AuthResponse, 2L, MAPPER.writeValueAsBytes(body)))
                    finishOneRtt(readCh)
                }
                "scram-sha-256" -> {
                    if (username == null || password == null) {
                        throw RedDBException.AuthRefused(
                            "server picked scram-sha-256 but no username/password configured"
                        )
                    }
                    performScram(readCh, writeCh, username, password)
                }
                "oauth-jwt" -> {
                    if (token == null) {
                        throw RedDBException.AuthRefused(
                            "server picked oauth-jwt but no JWT token configured"
                        )
                    }
                    val body = MAPPER.createObjectNode().put("jwt", token)
                    writeFrame(writeCh, Frame(MessageKind.AuthResponse, 2L, MAPPER.writeValueAsBytes(body)))
                    finishOneRtt(readCh)
                }
                else -> throw RedDBException.ProtocolError(
                    "server picked unsupported auth method: $chosen"
                )
            }
        }

        private suspend fun finishOneRtt(readCh: ByteReadChannel): HandshakeResult {
            val f = readFrame(readCh)
            if (f.kind == MessageKind.AuthFail) {
                throw RedDBException.AuthRefused(reason(f.payload, "auth refused"))
            }
            if (f.kind != MessageKind.AuthOk) {
                throw RedDBException.ProtocolError("expected AuthOk, got ${MessageKind.name(f.kind)}")
            }
            val j = parseJson(f.payload, "AuthOk")
            return HandshakeResult(textField(j, "session_id") ?: "")
        }

        private suspend fun performScram(
            readCh: ByteReadChannel,
            writeCh: ByteWriteChannel,
            username: String,
            password: String,
        ): HandshakeResult {
            val clientNonce = Scram.newClientNonce()
            val clientFirst = Scram.clientFirst(username, clientNonce)
            val clientFirstBare = Scram.clientFirstBare(clientFirst)

            val cf = MAPPER.createObjectNode().put("client_first", clientFirst)
            writeFrame(writeCh, Frame(MessageKind.AuthResponse, 2L, MAPPER.writeValueAsBytes(cf)))

            val chall = readFrame(readCh)
            if (chall.kind == MessageKind.AuthFail) {
                throw RedDBException.AuthRefused(reason(chall.payload, "scram challenge refused"))
            }
            if (chall.kind != MessageKind.AuthRequest) {
                throw RedDBException.ProtocolError(
                    "scram: expected AuthRequest, got ${MessageKind.name(chall.kind)}"
                )
            }
            val serverFirstStr = scramServerFirst(chall.payload)
            val sf = Scram.parseServerFirst(serverFirstStr, clientNonce)

            val clientFinalNoProof = Scram.clientFinalNoProof(sf.combinedNonce)
            val authMessage = Scram.authMessage(clientFirstBare, sf.raw, clientFinalNoProof)
            val proof = Scram.clientProof(password, sf.salt, sf.iter, authMessage)
            val clientFinalStr = Scram.clientFinal(sf.combinedNonce, proof)

            val cfin = MAPPER.createObjectNode().put("client_final", clientFinalStr)
            writeFrame(writeCh, Frame(MessageKind.AuthResponse, 3L, MAPPER.writeValueAsBytes(cfin)))

            val ok = readFrame(readCh)
            if (ok.kind == MessageKind.AuthFail) {
                throw RedDBException.AuthRefused(reason(ok.payload, "scram refused"))
            }
            if (ok.kind != MessageKind.AuthOk) {
                throw RedDBException.ProtocolError("scram: expected AuthOk, got ${MessageKind.name(ok.kind)}")
            }
            val j = parseJson(ok.payload, "AuthOk")
            val sid = textField(j, "session_id") ?: ""
            // Verify server signature when present.
            val sig = parseServerSignature(j)
            if (sig != null && !Scram.verifyServerSignature(password, sf.salt, sf.iter, authMessage, sig)) {
                throw RedDBException.AuthRefused(
                    "scram: server signature did not verify — possible MITM"
                )
            }
            return HandshakeResult(sid)
        }

        /** Pull the server-first string out of an AuthRequest payload. */
        private fun scramServerFirst(payload: ByteArray): String {
            // Engine emits the raw `r=...,s=...,i=...` body. JS / Rust drivers
            // tolerate a JSON envelope too — we do the same.
            if (payload.isNotEmpty() && payload[0] == '{'.code.toByte()) {
                val j = parseJson(payload, "AuthRequest")
                val s = textField(j, "server_first")
                    ?: throw RedDBException.ProtocolError("AuthRequest JSON missing 'server_first'")
                return s
            }
            return String(payload, StandardCharsets.UTF_8)
        }

        private fun parseServerSignature(authOk: JsonNode): ByteArray? {
            val v = authOk.get("v")
            if (v != null && v.isTextual) {
                try {
                    return Base64.getDecoder().decode(v.asText())
                } catch (_: IllegalArgumentException) {
                    // fall through and try other shapes
                }
            }
            val hex = authOk.get("server_signature")
            if (hex != null && hex.isTextual) {
                return decodeHex(hex.asText())
            }
            return null
        }

        private fun decodeHex(s: String): ByteArray? {
            if (s.length % 2 != 0) return null
            val out = ByteArray(s.length / 2)
            for (i in out.indices) {
                val hi = Character.digit(s[i * 2], 16)
                val lo = Character.digit(s[i * 2 + 1], 16)
                if (hi < 0 || lo < 0) return null
                out[i] = ((hi shl 4) or lo).toByte()
            }
            return out
        }

        /** Write a fully-encoded frame and flush. */
        internal suspend fun writeFrame(writeCh: ByteWriteChannel, frame: Frame) {
            val bytes = Frame.encode(frame)
            writeCh.writeFully(bytes)
            writeCh.flush()
        }

        /** Read exactly one frame, blocking on partial reads. */
        internal suspend fun readFrame(readCh: ByteReadChannel): Frame {
            val header = ByteArray(Frame.HEADER_SIZE)
            readCh.readFully(header, 0, Frame.HEADER_SIZE)
            val length = ByteBuffer.wrap(header, 0, 4).order(ByteOrder.LITTLE_ENDIAN).int
            if (length < Frame.HEADER_SIZE || length > Frame.MAX_FRAME_SIZE) {
                throw RedDBException.FrameTooLarge("frame length out of range: $length")
            }
            val full = ByteArray(length)
            System.arraycopy(header, 0, full, 0, Frame.HEADER_SIZE)
            if (length > Frame.HEADER_SIZE) {
                readCh.readFully(full, Frame.HEADER_SIZE, length - Frame.HEADER_SIZE)
            }
            return Frame.decode(full)
        }

        private fun reason(payload: ByteArray, fallback: String): String {
            if (payload.isEmpty()) return fallback
            return try {
                val n = MAPPER.readTree(payload)
                val r = n.get("reason")
                if (r != null && r.isTextual) r.asText() else String(payload, StandardCharsets.UTF_8)
            } catch (_: Exception) {
                String(payload, StandardCharsets.UTF_8)
            }
        }

        private fun parseJson(payload: ByteArray, label: String): JsonNode {
            return try {
                MAPPER.readTree(payload)
            } catch (e: Exception) {
                throw RedDBException.ProtocolError("$label: invalid JSON: ${e.message}")
            }
        }

        private fun textField(node: JsonNode?, name: String): String? {
            if (node == null || !node.isObject) return null
            val v = node.get(name)
            return if (v != null && v.isTextual) v.asText() else null
        }
    }

    // ---------------------------------------------------------------
    // Conn methods
    // ---------------------------------------------------------------

    override suspend fun query(sql: String): ByteArray = withContext(Dispatchers.IO) {
        mutex.withLock {
            ensureOpen()
            val corr = nextCorrelation.getAndIncrement()
            try {
                writeFrame(writeCh, Frame(MessageKind.Query, corr, sql.toByteArray(StandardCharsets.UTF_8)))
                val resp = readFrame(readCh)
                when (resp.kind) {
                    MessageKind.Result -> resp.payload
                    MessageKind.Error -> throw RedDBException.EngineError(
                        String(resp.payload, StandardCharsets.UTF_8)
                    )
                    else -> throw RedDBException.ProtocolError(
                        "expected Result/Error, got ${MessageKind.name(resp.kind)}"
                    )
                }
            } catch (e: RedDBException) {
                throw e
            } catch (e: Throwable) {
                throw RedDBException.ProtocolError("query I/O: ${e.message}", e)
            }
        }
    }

    override suspend fun insert(collection: String, payload: Any) {
        val body = MAPPER.createObjectNode().apply {
            put("collection", collection)
            set<JsonNode>("payload", MAPPER.valueToTree(payload))
        }
        sendInsert(body)
    }

    override suspend fun bulkInsert(collection: String, rows: List<Any?>) {
        val body = MAPPER.createObjectNode().apply {
            put("collection", collection)
            set<JsonNode>("payloads", MAPPER.valueToTree(rows))
        }
        sendInsert(body)
    }

    private suspend fun sendInsert(body: ObjectNode) = withContext(Dispatchers.IO) {
        mutex.withLock {
            ensureOpen()
            val corr = nextCorrelation.getAndIncrement()
            try {
                val bytes = MAPPER.writeValueAsBytes(body)
                writeFrame(writeCh, Frame(MessageKind.BulkInsert, corr, bytes))
                val resp = readFrame(readCh)
                when (resp.kind) {
                    MessageKind.BulkOk -> {}
                    MessageKind.Error -> throw RedDBException.EngineError(
                        String(resp.payload, StandardCharsets.UTF_8)
                    )
                    else -> throw RedDBException.ProtocolError(
                        "expected BulkOk/Error, got ${MessageKind.name(resp.kind)}"
                    )
                }
            } catch (e: RedDBException) {
                throw e
            } catch (e: Throwable) {
                throw RedDBException.ProtocolError("insert I/O: ${e.message}", e)
            }
        }
    }

    override suspend fun get(collection: String, id: String): ByteArray = withContext(Dispatchers.IO) {
        mutex.withLock {
            ensureOpen()
            val corr = nextCorrelation.getAndIncrement()
            try {
                val body = MAPPER.createObjectNode().apply {
                    put("collection", collection)
                    put("id", id)
                }
                writeFrame(writeCh, Frame(MessageKind.Get, corr, MAPPER.writeValueAsBytes(body)))
                val resp = readFrame(readCh)
                when (resp.kind) {
                    MessageKind.Result -> resp.payload
                    MessageKind.Error -> throw RedDBException.EngineError(
                        String(resp.payload, StandardCharsets.UTF_8)
                    )
                    else -> throw RedDBException.ProtocolError(
                        "expected Result/Error, got ${MessageKind.name(resp.kind)}"
                    )
                }
            } catch (e: RedDBException) {
                throw e
            } catch (e: Throwable) {
                throw RedDBException.ProtocolError("get I/O: ${e.message}", e)
            }
        }
    }

    override suspend fun delete(collection: String, id: String) = withContext(Dispatchers.IO) {
        mutex.withLock {
            ensureOpen()
            val corr = nextCorrelation.getAndIncrement()
            try {
                val body = MAPPER.createObjectNode().apply {
                    put("collection", collection)
                    put("id", id)
                }
                writeFrame(writeCh, Frame(MessageKind.Delete, corr, MAPPER.writeValueAsBytes(body)))
                val resp = readFrame(readCh)
                when (resp.kind) {
                    MessageKind.DeleteOk -> Unit
                    MessageKind.Error -> throw RedDBException.EngineError(
                        String(resp.payload, StandardCharsets.UTF_8)
                    )
                    else -> throw RedDBException.ProtocolError(
                        "expected DeleteOk/Error, got ${MessageKind.name(resp.kind)}"
                    )
                }
            } catch (e: RedDBException) {
                throw e
            } catch (e: Throwable) {
                throw RedDBException.ProtocolError("delete I/O: ${e.message}", e)
            }
        }
    }

    override suspend fun ping() = withContext(Dispatchers.IO) {
        mutex.withLock {
            ensureOpen()
            val corr = nextCorrelation.getAndIncrement()
            try {
                writeFrame(writeCh, Frame(MessageKind.Ping, corr, ByteArray(0)))
                val resp = readFrame(readCh)
                if (resp.kind != MessageKind.Pong) {
                    throw RedDBException.ProtocolError("expected Pong, got ${MessageKind.name(resp.kind)}")
                }
            } catch (e: RedDBException) {
                throw e
            } catch (e: Throwable) {
                throw RedDBException.ProtocolError("ping I/O: ${e.message}", e)
            }
        }
    }

    override fun close() {
        if (closed) return
        closed = true
        // Best-effort Bye + close. Block briefly on the suspend write via runBlocking.
        try {
            kotlinx.coroutines.runBlocking {
                val corr = nextCorrelation.getAndIncrement()
                try {
                    writeFrame(writeCh, Frame(MessageKind.Bye, corr, ByteArray(0)))
                } catch (_: Throwable) { /* ignore */ }
            }
        } catch (_: Throwable) { /* ignore */ }
        runCatching { owner.close() }
    }

    private fun ensureOpen() {
        check(!closed) { "RedwireConn is closed" }
    }

    /** Outcome of a successful handshake — exposed mostly for tests. */
    public class HandshakeResult(public val sessionId: String)
}
