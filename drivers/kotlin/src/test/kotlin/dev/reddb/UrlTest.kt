package dev.reddb

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.Arguments
import org.junit.jupiter.params.provider.MethodSource
import org.junit.jupiter.params.provider.ValueSource
import java.util.stream.Stream

class UrlTest {

    @ParameterizedTest
    @MethodSource("remoteCases")
    fun parsesRemoteShapes(uri: String, kind: Url.Kind, host: String, port: Int) {
        val u = Url.parse(uri)
        assertEquals(kind, u.kind)
        assertEquals(host, u.host)
        assertEquals(port, u.port)
        assertEquals(uri, u.original)
    }

    @Test
    fun extractsUserInfoFromAuthority() {
        val u = Url.parse("red://alice:secret@host:5050")
        assertEquals("alice", u.username)
        assertEquals("secret", u.password)
        assertEquals("host", u.host)
        assertEquals(5050, u.port)
    }

    @Test
    fun decodesPercentEscapesInUserInfo() {
        val u = Url.parse("red://al%40ice:p%2Fass@host")
        assertEquals("al@ice", u.username)
        assertEquals("p/ass", u.password)
    }

    @Test
    fun parsesUserOnlyWithoutPassword() {
        val u = Url.parse("red://alice@host")
        assertEquals("alice", u.username)
        assertNull(u.password)
    }

    @Test
    fun parsesQueryParamsTokenAndApiKey() {
        val u = Url.parse("red://host?token=tok-abc&apiKey=ak-xyz")
        assertEquals("tok-abc", u.token)
        assertEquals("ak-xyz", u.apiKey)
        assertEquals("tok-abc", u.params["token"])
    }

    @Test
    fun apiKeyAcceptsSnakeCaseFallback() {
        val u = Url.parse("red://host?api_key=ak-xyz")
        assertEquals("ak-xyz", u.apiKey)
    }

    @Test
    fun decodesPercentEscapesInQuery() {
        val u = Url.parse("red://host?token=a%20b%2Bc")
        assertEquals("a b+c", u.token)
    }

    @Test
    fun recognisesEmbeddedInMemoryAliases() {
        for (s in arrayOf("red:", "red:/", "red://", "red://memory", "red://memory/", "red://:memory", "red://:memory:")) {
            val u = Url.parse(s)
            assertEquals(Url.Kind.EMBEDDED_MEMORY, u.kind, "expected memory for $s")
            assertTrue(u.isEmbedded())
        }
    }

    @Test
    fun recognisesEmbeddedFileTriple() {
        val u = Url.parse("red:///var/lib/reddb/data.rdb")
        assertEquals(Url.Kind.EMBEDDED_FILE, u.kind)
        assertEquals("/var/lib/reddb/data.rdb", u.path)
    }

    @ParameterizedTest
    @ValueSource(strings = ["mongodb://localhost", "ftp://host", "grpc://host", "tcp://host"])
    fun rejectsUnsupportedSchemes(uri: String) {
        assertThrows(IllegalArgumentException::class.java) { Url.parse(uri) }
    }

    @Test
    fun rejectsEmpty() {
        assertThrows(IllegalArgumentException::class.java) { Url.parse("") }
        assertThrows(IllegalArgumentException::class.java) { Url.parse(null) }
    }

    @Test
    fun redSchemeWithExplicitEmptyAuthorityIsEmbeddedMemory() {
        // `red://` alone is the canonical "embedded in-memory" shortcut.
        assertEquals(Url.Kind.EMBEDDED_MEMORY, Url.parse("red://").kind)
    }

    @Test
    fun schemeIsCaseInsensitive() {
        val u = Url.parse("RED://host:1")
        assertEquals(Url.Kind.REDWIRE, u.kind)
    }

    @Test
    fun isRedwireFlagsCorrectly() {
        assertTrue(Url.parse("red://h").isRedwire())
        assertTrue(Url.parse("reds://h").isRedwire())
        assertFalse(Url.parse("http://h").isRedwire())
    }

    @Test
    fun isTlsFlagsCorrectly() {
        assertTrue(Url.parse("reds://h").isTls())
        assertTrue(Url.parse("https://h").isTls())
        assertFalse(Url.parse("red://h").isTls())
        assertFalse(Url.parse("http://h").isTls())
    }

    @Test
    fun portDefaultsTo5050() {
        assertEquals(5050, Url.parse("red://h").port)
        assertEquals(5050, Url.parse("reds://h").port)
        assertEquals(5050, Url.parse("http://h").port)
        assertEquals(5050, Url.parse("https://h").port)
    }

    @Test
    fun preservesOriginalUri() {
        val s = "red://user:pw@host:5050?token=t"
        assertEquals(s, Url.parse(s).original)
    }

    @Test
    fun ipv4HostParses() {
        val u = Url.parse("red://10.0.0.1:1234")
        assertEquals("10.0.0.1", u.host)
        assertEquals(1234, u.port)
    }

    @Test
    fun multipleQueryParamsKeepInsertionOrder() {
        val u = Url.parse("red://h?a=1&b=2&c=3")
        assertEquals(listOf("a", "b", "c"), u.params.keys.toList())
    }

    companion object {
        @JvmStatic
        fun remoteCases(): Stream<Arguments> = Stream.of(
            Arguments.of("red://localhost", Url.Kind.REDWIRE, "localhost", 5050),
            Arguments.of("red://localhost:5050", Url.Kind.REDWIRE, "localhost", 5050),
            Arguments.of("red://example.com:9999", Url.Kind.REDWIRE, "example.com", 9999),
            Arguments.of("red://10.0.0.1:1234", Url.Kind.REDWIRE, "10.0.0.1", 1234),
            Arguments.of("reds://reddb.example.com", Url.Kind.REDWIRE_TLS, "reddb.example.com", 5050),
            Arguments.of("reds://reddb.example.com:8443", Url.Kind.REDWIRE_TLS, "reddb.example.com", 8443),
            Arguments.of("http://localhost", Url.Kind.HTTP, "localhost", 5050),
            Arguments.of("http://localhost:8080", Url.Kind.HTTP, "localhost", 8080),
            Arguments.of("https://reddb.example.com", Url.Kind.HTTPS, "reddb.example.com", 5050),
            Arguments.of("https://reddb.example.com:8443", Url.Kind.HTTPS, "reddb.example.com", 8443),
        )
    }
}
