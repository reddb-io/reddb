package dev.reddb

import java.net.URI
import java.net.URISyntaxException
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import java.util.Locale

/**
 * Connection-string parser. Mirrors `drivers/js/src/url.js` semantics:
 * one URL covers every transport.
 *
 * Supported shapes:
 * ```
 *   red://[user[:pass]@]host[:port][?...]      plain RedWire (TCP)
 *   reds://[user[:pass]@]host[:port][?...]     RedWire over TLS
 *   http://host[:port]/                        HTTP (REST)
 *   https://host[:port]/                       HTTPS
 *   red:///abs/path/file.rdb                   embedded file (out of scope here)
 *   red://memory  red://:memory  red://:memory: embedded in-memory (out of scope)
 * ```
 *
 * Default port is 5050 for every scheme (matches `DEFAULT_REDWIRE_PORT`).
 */
public class Url private constructor(
    public val original: String,
    public val kind: Kind,
    public val host: String? = null,
    public val port: Int = DEFAULT_PORT,
    public val path: String? = null,
    public val username: String? = null,
    public val password: String? = null,
    public val token: String? = null,
    public val apiKey: String? = null,
    public val params: Map<String, String> = emptyMap(),
) {
    public enum class Kind { REDWIRE, REDWIRE_TLS, HTTP, HTTPS, EMBEDDED_FILE, EMBEDDED_MEMORY }

    /** True for `red://` and `reds://` (the binary protocol). */
    public fun isRedwire(): Boolean = kind == Kind.REDWIRE || kind == Kind.REDWIRE_TLS

    /** True for `reds://` or `https://`. */
    public fun isTls(): Boolean = kind == Kind.REDWIRE_TLS || kind == Kind.HTTPS

    /** True for either embedded variant — the Kotlin driver doesn't ship an embedded engine. */
    public fun isEmbedded(): Boolean = kind == Kind.EMBEDDED_FILE || kind == Kind.EMBEDDED_MEMORY

    public companion object {
        /** Default port used for every transport. Matches `DEFAULT_REDWIRE_PORT`. */
        public const val DEFAULT_PORT: Int = 5050

        private val EMBEDDED_MEMORY_ALIASES: Set<String> = setOf(
            "red:", "red:/", "red://",
            "red://memory", "red://memory/",
            "red://:memory", "red://:memory:",
        )

        /**
         * Parse any supported URI string.
         *
         * @throws IllegalArgumentException for unsupported schemes / malformed inputs
         */
        public fun parse(uri: String?): Url {
            if (uri.isNullOrEmpty()) {
                throw IllegalArgumentException(
                    "connect requires a URI string (e.g. 'red://localhost:5050')"
                )
            }
            // Embedded shortcuts.
            if (uri in EMBEDDED_MEMORY_ALIASES) {
                return Url(uri, Kind.EMBEDDED_MEMORY)
            }
            if (uri.startsWith("red:///")) {
                val path = uri.substring("red://".length) // keeps leading '/'
                return Url(uri, Kind.EMBEDDED_FILE, path = path)
            }

            val scheme = schemeOf(uri)
            val kind = kindFromScheme(scheme)
                ?: throw IllegalArgumentException(
                    "unsupported URI scheme: '$scheme' in '$uri'." +
                        " Supported: red, reds, http, https"
                )

            val parsed = parseAsJavaUri(uri)
            val host = parsed.host
            if (host.isNullOrEmpty()) {
                throw IllegalArgumentException("URI is missing a host: '$uri'")
            }
            val port = if (parsed.port < 0) DEFAULT_PORT else parsed.port

            var username: String? = null
            var password: String? = null
            val rawUserInfo = parsed.rawUserInfo
            if (!rawUserInfo.isNullOrEmpty()) {
                val colon = rawUserInfo.indexOf(':')
                if (colon >= 0) {
                    username = decode(rawUserInfo.substring(0, colon))
                    password = decode(rawUserInfo.substring(colon + 1))
                } else {
                    username = decode(rawUserInfo)
                }
            }

            val params = parseQuery(parsed.rawQuery)
            val token = params["token"]
            val apiKey = params["apiKey"] ?: params["api_key"]

            var path: String? = parsed.rawPath
            if (path != null && (path.isEmpty() || path == "/")) path = null

            return Url(
                original = uri,
                kind = kind,
                host = host,
                port = port,
                path = path,
                username = username,
                password = password,
                token = token,
                apiKey = apiKey,
                params = params,
            )
        }

        private fun schemeOf(uri: String): String {
            val colon = uri.indexOf(':')
            require(colon > 0) { "URI missing scheme: '$uri'" }
            return uri.substring(0, colon).lowercase(Locale.ROOT)
        }

        private fun kindFromScheme(scheme: String): Kind? = when (scheme) {
            "red" -> Kind.REDWIRE
            "reds" -> Kind.REDWIRE_TLS
            "http" -> Kind.HTTP
            "https" -> Kind.HTTPS
            else -> null
        }

        private fun parseAsJavaUri(uri: String): URI = try {
            URI(uri)
        } catch (e: URISyntaxException) {
            throw IllegalArgumentException("failed to parse URI '$uri': ${e.message}", e)
        }

        private fun parseQuery(raw: String?): Map<String, String> {
            if (raw.isNullOrEmpty()) return emptyMap()
            val out = LinkedHashMap<String, String>()
            for (pair in raw.split('&')) {
                if (pair.isEmpty()) continue
                val eq = pair.indexOf('=')
                val k = if (eq < 0) pair else pair.substring(0, eq)
                val v = if (eq < 0) "" else pair.substring(eq + 1)
                out[decode(k)] = decode(v)
            }
            return out
        }

        private fun decode(s: String): String = URLDecoder.decode(s, StandardCharsets.UTF_8)
    }
}
