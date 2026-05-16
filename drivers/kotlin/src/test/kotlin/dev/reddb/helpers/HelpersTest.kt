package dev.reddb.helpers

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Mirrors `drivers/go/helpers_test.go` 1:1.
 */
private class FakeQuerier(
    val replies: ArrayDeque<ByteArray> = ArrayDeque(),
    val errs: ArrayDeque<Throwable> = ArrayDeque(),
) : Querier {
    val calls = mutableListOf<Pair<String, List<Any?>>>()
    override suspend fun query(sql: String, vararg params: Any?): ByteArray {
        calls += sql to params.toList()
        val err = errs.removeFirstOrNull()
        if (err != null) throw err
        return replies.removeFirstOrNull() ?: ByteArray(0)
    }
}

private val mapper: ObjectMapper = jacksonObjectMapper()
private fun reply(v: Any): ByteArray = mapper.writeValueAsBytes(v)

class HelpersTest {

    // KV path ---------------------------------------------------------------

    @Test fun kvPathQuotesNamespacedKeys() {
        assertEquals("kv_default.'corpus:version'", Sql.kvPath("kv_default", "corpus:version"))
    }

    @Test fun kvPathPreservesDotsAndSlashes() {
        assertEquals("kv_default.'a/b.c'", Sql.kvPath("kv_default", "a/b.c"))
    }

    @Test fun kvPathRejectsBadCollection() {
        assertThrows(HelperException.InvalidArgument::class.java) {
            Sql.kvPath("bad-name!", "k")
        }
    }

    @Test fun kvValueLiteralCases() {
        assertEquals("NULL", Sql.kvValueLiteral(null))
        assertEquals("true", Sql.kvValueLiteral(true))
        assertEquals("false", Sql.kvValueLiteral(false))
        assertEquals("42", Sql.kvValueLiteral(42L))
        assertEquals("'hi'", Sql.kvValueLiteral("hi"))
        assertEquals("'o''reilly'", Sql.kvValueLiteral("o'reilly"))
        assertEquals("'{\"a\":1}'", Sql.kvValueLiteral(mapOf("a" to 1)))
    }

    // KV ops ----------------------------------------------------------------

    @Test fun kvSetEmitsExactKeyPath() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(reply(emptyMap<String, Any>()))))
        val h = Helpers(fq).kv()
        h.set("characters:hansel", "ok")
        val sql = fq.calls[0].first
        assertTrue("kv_default.'characters:hansel'" in sql) { sql }
        assertTrue("= 'ok'" in sql) { sql }
    }

    @Test fun kvGetReturnsValueOrNull() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(
            reply(mapOf("rows" to listOf(mapOf("value" to "v")))),
            reply(mapOf("rows" to emptyList<Any>())),
        )))
        val h = Helpers(fq).kv()
        assertEquals("v", h.get("k"))
        assertNull(h.get("k2"))
    }

    @Test fun kvExistsUsesGet() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(
            reply(mapOf("rows" to listOf(mapOf("value" to "v")))),
            reply(mapOf("rows" to emptyList<Any>())),
        )))
        val h = Helpers(fq).kv()
        assertTrue(h.exists("k").exists)
        assertTrue(!h.exists("k2").exists)
    }

    @Test fun kvListFiltersByPrefixWithoutRewriting() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(reply(mapOf("rows" to listOf(
            mapOf("key" to "a:1", "value" to 1),
            mapOf("key" to "b:1", "value" to 2),
            mapOf("key" to "a:2", "value" to 3),
        ))))))
        val out = Helpers(fq).kv().list(KvClient.ListOpts(prefix = "a:"))
        assertEquals(2, out.items.size)
        assertEquals("a:1", out.items[0]["key"])
        assertEquals("a:2", out.items[1]["key"])
    }

    @Test fun kvListRejectsNegativeLimit() {
        val h = Helpers(FakeQuerier()).kv()
        assertThrows(HelperException.InvalidArgument::class.java) {
            runBlocking { h.list(KvClient.ListOpts(limit = -1)) }
        }
    }

    // Queue -----------------------------------------------------------------

    @Test fun queuePushEmitsPriorityAndPayload() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(reply(mapOf("affected" to 1)))))
        Helpers(fq).queue().push("jobs", mapOf("id" to 1), QueueClient.PushOptions(priority = 5))
        val sql = fq.calls[0].first
        assertTrue(sql.startsWith("QUEUE PUSH jobs ")) { sql }
        assertTrue("PRIORITY 5" in sql) { sql }
        assertTrue("{\"id\":1}" in sql) { sql }
    }

    @Test fun queueLenReturnsInt() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(reply(mapOf("rows" to listOf(mapOf("len" to 3)))))))
        assertEquals(3L, Helpers(fq).queue().len("jobs"))
    }

    @Test fun queuePopReturnsPayloads() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(reply(mapOf("rows" to listOf(
            mapOf("payload" to "a"),
            mapOf("payload" to "b"),
        ))))))
        val out = Helpers(fq).queue().pop("jobs", 2)
        assertEquals(listOf<Any?>("a", "b"), out)
    }

    @Test fun queuePopRejectsNegativeCount() {
        val q = Helpers(FakeQuerier()).queue()
        assertThrows(HelperException.InvalidArgument::class.java) {
            runBlocking { q.pop("jobs", -1) }
        }
    }

    @Test fun queuePushRejectsInvalidIdentifier() {
        val q = Helpers(FakeQuerier()).queue()
        assertThrows(HelperException.InvalidArgument::class.java) {
            runBlocking { q.push("bad-name!", "x") }
        }
    }

    // Documents -------------------------------------------------------------

    @Test fun documentsInsertReturnsRidEnvelope() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(
            reply(mapOf("rows" to emptyList<Any>(), "affected" to 0)),
            reply(mapOf(
                "rows" to listOf(mapOf("rid" to "doc-1", "body" to mapOf("name" to "alice"))),
                "affected" to 1,
            )),
        )))
        val out = Helpers(fq).documents().insert("people", mapOf("name" to "alice"))
        assertEquals(1L, out.affected)
        assertEquals("doc-1", out.rid)
        assertNotNull(out.item)
        assertEquals("doc-1", out.item!!["rid"])
    }

    @Test fun documentsGetRaisesNotFoundOnMissing() {
        val fq = FakeQuerier(ArrayDeque(listOf(reply(mapOf("rows" to emptyList<Any>())))))
        val d = Helpers(fq).documents()
        assertThrows(HelperException.NotFound::class.java) {
            runBlocking { d.get("people", "doc-1") }
        }
    }

    @Test fun documentsPatchRejectsJsonPointerPaths() {
        val d = Helpers(FakeQuerier()).documents()
        assertThrows(HelperException.InvalidArgument::class.java) {
            runBlocking { d.patch("people", "doc-1", mapOf("a/b" to 1)) }
        }
    }

    @Test fun documentsListOrdersByRidByDefault() = runBlocking {
        val fq = FakeQuerier(ArrayDeque(listOf(reply(mapOf("rows" to listOf(
            mapOf("rid" to "a"),
            mapOf("rid" to "b"),
        ))))))
        val out = Helpers(fq).documents().list("people")
        assertEquals(2, out.items.size)
        assertTrue("ORDER BY rid ASC" in fq.calls[0].first) { fq.calls[0].first }
    }

    @Test fun documentsInsertPassesThroughExistingCollection() = runBlocking {
        val fq = FakeQuerier(
            ArrayDeque(listOf(
                reply(emptyMap<String, Any>()),
                reply(mapOf("rows" to listOf(mapOf("rid" to "x")), "affected" to 1)),
            )),
            ArrayDeque(listOf(RuntimeException("collection already exists"))),
        )
        Helpers(fq).documents().insert("people", mapOf("a" to 1))
    }

    // Decode helpers --------------------------------------------------------

    @Test fun affectedFromBodyHandlesNestedResult() {
        val body = reply(mapOf("result" to mapOf("affected" to 7)))
        assertEquals(7L, Sql.affectedFromBody(body))
    }
}
