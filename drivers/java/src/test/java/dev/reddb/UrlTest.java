package dev.reddb;

import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.Arguments;
import org.junit.jupiter.params.provider.MethodSource;
import org.junit.jupiter.params.provider.ValueSource;

import java.util.stream.Stream;

import static org.junit.jupiter.api.Assertions.*;

class UrlTest {

    static Stream<Arguments> remoteCases() {
        // uri | expected kind | expected host | expected port
        return Stream.of(
            Arguments.of("red://localhost", Url.Kind.REDWIRE, "localhost", 5050),
            Arguments.of("red://localhost:5050", Url.Kind.REDWIRE, "localhost", 5050),
            Arguments.of("red://example.com:9999", Url.Kind.REDWIRE, "example.com", 9999),
            Arguments.of("red://10.0.0.1:1234", Url.Kind.REDWIRE, "10.0.0.1", 1234),
            Arguments.of("reds://reddb.example.com", Url.Kind.REDWIRE_TLS, "reddb.example.com", 5050),
            Arguments.of("reds://reddb.example.com:8443", Url.Kind.REDWIRE_TLS, "reddb.example.com", 8443),
            Arguments.of("http://localhost", Url.Kind.HTTP, "localhost", 5050),
            Arguments.of("http://localhost:8080", Url.Kind.HTTP, "localhost", 8080),
            Arguments.of("https://reddb.example.com", Url.Kind.HTTPS, "reddb.example.com", 5050),
            Arguments.of("https://reddb.example.com:8443", Url.Kind.HTTPS, "reddb.example.com", 8443)
        );
    }

    @ParameterizedTest
    @MethodSource("remoteCases")
    void parsesRemoteShapes(String uri, Url.Kind kind, String host, int port) {
        Url u = Url.parse(uri);
        assertEquals(kind, u.kind());
        assertEquals(host, u.host());
        assertEquals(port, u.port());
        assertEquals(uri, u.original());
    }

    @Test
    void extractsUserInfoFromAuthority() {
        Url u = Url.parse("red://alice:secret@host:5050");
        assertEquals("alice", u.username());
        assertEquals("secret", u.password());
        assertEquals("host", u.host());
        assertEquals(5050, u.port());
    }

    @Test
    void decodesPercentEscapesInUserInfo() {
        Url u = Url.parse("red://al%40ice:p%2Fass@host");
        assertEquals("al@ice", u.username());
        assertEquals("p/ass", u.password());
    }

    @Test
    void parsesUserOnlyWithoutPassword() {
        Url u = Url.parse("red://alice@host");
        assertEquals("alice", u.username());
        assertNull(u.password());
    }

    @Test
    void parsesQueryParamsTokenAndApiKey() {
        Url u = Url.parse("red://host?token=tok-abc&apiKey=ak-xyz");
        assertEquals("tok-abc", u.token());
        assertEquals("ak-xyz", u.apiKey());
        assertEquals("tok-abc", u.params().get("token"));
    }

    @Test
    void apiKeyAcceptsSnakeCaseFallback() {
        Url u = Url.parse("red://host?api_key=ak-xyz");
        assertEquals("ak-xyz", u.apiKey());
    }

    @Test
    void decodesPercentEscapesInQuery() {
        Url u = Url.parse("red://host?token=a%20b%2Bc");
        assertEquals("a b+c", u.token());
    }

    @Test
    void recognisesEmbeddedInMemoryAliases() {
        for (String s : new String[]{"red:", "red:/", "red://", "red://memory", "red://memory/", "red://:memory", "red://:memory:"}) {
            Url u = Url.parse(s);
            assertEquals(Url.Kind.EMBEDDED_MEMORY, u.kind(), "expected memory for " + s);
            assertTrue(u.isEmbedded());
        }
    }

    @Test
    void recognisesEmbeddedFileTriple() {
        Url u = Url.parse("red:///var/lib/reddb/data.rdb");
        assertEquals(Url.Kind.EMBEDDED_FILE, u.kind());
        assertEquals("/var/lib/reddb/data.rdb", u.path());
    }

    @ParameterizedTest
    @ValueSource(strings = {"mongodb://localhost", "ftp://host", "grpc://host", "tcp://host"})
    void rejectsUnsupportedSchemes(String uri) {
        assertThrows(IllegalArgumentException.class, () -> Url.parse(uri));
    }

    @Test
    void rejectsEmpty() {
        assertThrows(IllegalArgumentException.class, () -> Url.parse(""));
        assertThrows(IllegalArgumentException.class, () -> Url.parse(null));
    }

    @Test
    void redSchemeWithExplicitEmptyAuthorityIsEmbeddedMemory() {
        // `red://` alone is the canonical "embedded in-memory" shortcut.
        // Parsers that need a host should reject it via Url.kind() instead.
        assertEquals(Url.Kind.EMBEDDED_MEMORY, Url.parse("red://").kind());
    }

    @Test
    void schemeIsCaseInsensitive() {
        Url u = Url.parse("RED://host:1");
        assertEquals(Url.Kind.REDWIRE, u.kind());
    }

    @Test
    void isRedwireFlagsCorrectly() {
        assertTrue(Url.parse("red://h").isRedwire());
        assertTrue(Url.parse("reds://h").isRedwire());
        assertFalse(Url.parse("http://h").isRedwire());
    }

    @Test
    void isTlsFlagsCorrectly() {
        assertTrue(Url.parse("reds://h").isTls());
        assertTrue(Url.parse("https://h").isTls());
        assertFalse(Url.parse("red://h").isTls());
        assertFalse(Url.parse("http://h").isTls());
    }

    @Test
    void portDefaultsTo5050() {
        assertEquals(5050, Url.parse("red://h").port());
        assertEquals(5050, Url.parse("reds://h").port());
        assertEquals(5050, Url.parse("http://h").port());
        assertEquals(5050, Url.parse("https://h").port());
    }

    @Test
    void preservesOriginalUri() {
        String s = "red://user:pw@host:5050?token=t";
        assertEquals(s, Url.parse(s).original());
    }
}
