package dev.reddb.http

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.node.ObjectNode
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import dev.reddb.Conn
import dev.reddb.Options
import dev.reddb.RedDBException
import dev.reddb.Url
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.client.request.post
import io.ktor.client.request.setBody
import io.ktor.client.statement.HttpResponse
import io.ktor.client.statement.readBytes
import io.ktor.http.ContentType
import io.ktor.http.HttpHeaders
import io.ktor.http.contentType
import java.nio.charset.StandardCharsets
import kotlin.time.Duration

/**
 * HTTP transport. Mirrors the JS / Java HTTP drivers: a single ktor
 * [HttpClient] (CIO engine) talking JSON to the RedDB REST endpoints
 * with a bearer token in `Authorization`. Login is automatic when
 * the URL or [Options] carries username + password.
 */
public class HttpConn internal constructor(
    private val client: HttpClient,
    baseUrl: String,
    @Volatile private var token: String?,
    private val timeout: Duration,
) : Conn {
    private val baseUrl: String = stripTrailingSlash(baseUrl)
    @Volatile private var closed: Boolean = false

    public companion object {
        internal val MAPPER: ObjectMapper = jacksonObjectMapper()

        /** Open a fresh client, auto-log-in when credentials are present. */
        public suspend fun connect(url: Url, opts: Options): HttpConn {
            val scheme = if (url.kind == Url.Kind.HTTPS) "https" else "http"
            val host = url.host ?: throw IllegalArgumentException("URL is missing a host")
            val baseUrl = "$scheme://$host:${url.port}"
            val timeoutMs = opts.timeout.inWholeMilliseconds

            val client = HttpClient(CIO) {
                expectSuccess = false
                engine {
                    requestTimeout = timeoutMs
                    endpoint.connectTimeout = timeoutMs
                    endpoint.socketTimeout = timeoutMs
                }
            }

            val token = opts.token ?: url.token
            val conn = HttpConn(client, baseUrl, token, opts.timeout)

            if (token == null) {
                val user = opts.username ?: url.username
                val pass = opts.password ?: url.password
                if (user != null && pass != null) {
                    conn.login(user, pass)
                }
            }
            return conn
        }
    }

    /** POST /auth/login → updates this connection's bearer token. */
    public suspend fun login(username: String, password: String) {
        val body = MAPPER.createObjectNode().apply {
            put("username", username)
            put("password", password)
        }
        val resp = post("/auth/login", body, requireAuthHeader = false)
        try {
            val j = MAPPER.readTree(resp)
            var tok = j.get("token")
            if (tok == null || !tok.isTextual) {
                val inner = j.get("result")
                if (inner != null && inner.isObject) tok = inner.get("token")
            }
            if (tok == null || !tok.isTextual) {
                throw RedDBException.ProtocolError("auth/login response missing 'token'")
            }
            this.token = tok.asText()
        } catch (e: RedDBException) {
            throw e
        } catch (e: Exception) {
            throw RedDBException.ProtocolError("auth/login: invalid JSON: ${e.message}", e)
        }
    }

    override suspend fun query(sql: String): ByteArray {
        val body = MAPPER.createObjectNode().put("sql", sql)
        return post("/query", body, requireAuthHeader = true)
    }

    override suspend fun insert(collection: String, payload: Any) {
        val body = MAPPER.createObjectNode().apply {
            put("collection", collection)
            set<com.fasterxml.jackson.databind.JsonNode>("payload", MAPPER.valueToTree(payload))
        }
        post("/insert", body, requireAuthHeader = true)
    }

    override suspend fun bulkInsert(collection: String, rows: List<Any?>) {
        val body = MAPPER.createObjectNode().apply {
            put("collection", collection)
            set<com.fasterxml.jackson.databind.JsonNode>("payloads", MAPPER.valueToTree(rows))
        }
        post("/bulk_insert", body, requireAuthHeader = true)
    }

    override suspend fun get(collection: String, id: String): ByteArray {
        val body = MAPPER.createObjectNode().apply {
            put("collection", collection)
            put("id", id)
        }
        return post("/get", body, requireAuthHeader = true)
    }

    override suspend fun delete(collection: String, id: String) {
        val body = MAPPER.createObjectNode().apply {
            put("collection", collection)
            put("id", id)
        }
        post("/delete", body, requireAuthHeader = true)
    }

    override suspend fun ping() {
        try {
            val resp: HttpResponse = client.get("$baseUrl/admin/health") {
                header(HttpHeaders.Accept, "application/json")
                if (token != null) header(HttpHeaders.Authorization, "Bearer $token")
            }
            val sc = resp.status.value
            if (sc / 100 != 2) {
                val body = String(resp.readBytes(), StandardCharsets.UTF_8)
                throw RedDBException.EngineError("ping: HTTP $sc: $body")
            }
        } catch (e: RedDBException) {
            throw e
        } catch (e: Throwable) {
            throw RedDBException.ProtocolError("ping I/O: ${e.message}", e)
        }
    }

    override fun close() {
        if (closed) return
        closed = true
        try { client.close() } catch (_: Throwable) { /* ignore */ }
    }

    public fun token(): String? = token
    public fun isClosed(): Boolean = closed

    /** POST a JSON body and return the raw response bytes. */
    private suspend fun post(path: String, body: ObjectNode, requireAuthHeader: Boolean): ByteArray {
        try {
            val payload = MAPPER.writeValueAsBytes(body)
            val resp: HttpResponse = client.post("$baseUrl$path") {
                contentType(ContentType.Application.Json)
                header(HttpHeaders.Accept, "application/json")
                if (requireAuthHeader && token != null) {
                    header(HttpHeaders.Authorization, "Bearer $token")
                }
                setBody(payload)
            }
            val sc = resp.status.value
            val respBody = resp.readBytes()
            if (sc / 100 != 2) {
                val msg = String(respBody, StandardCharsets.UTF_8)
                if (sc == 401 || sc == 403) {
                    throw RedDBException.AuthRefused("HTTP $sc $path: $msg")
                }
                throw RedDBException.EngineError("HTTP $sc $path: $msg")
            }
            return respBody
        } catch (e: RedDBException) {
            throw e
        } catch (e: Throwable) {
            throw RedDBException.ProtocolError("$path I/O: ${e.message}", e)
        }
    }

    private fun stripTrailingSlash(s: String): String =
        if (s.endsWith("/")) s.substring(0, s.length - 1) else s
}
