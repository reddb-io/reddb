package dev.reddb.helpers;

import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Conformance tests mirroring drivers/go/helpers_test.go. They run against
 * a {@link FakeQuerier} that records SQL and replays scripted JSON replies —
 * no server needed.
 */
class HelpersTest {

    private static final ObjectMapper JSON = new ObjectMapper();

    static final class FakeQuerier implements Querier {
        final List<String> sqls = new ArrayList<>();
        final List<Object[]> params = new ArrayList<>();
        final Deque<byte[]> replies = new ArrayDeque<>();
        final Deque<RuntimeException> errs = new ArrayDeque<>();

        @Override
        public byte[] query(String sql, Object... ps) {
            sqls.add(sql);
            params.add(ps);
            RuntimeException err = errs.isEmpty() ? null : errs.poll();
            byte[] body = replies.isEmpty() ? new byte[0] : replies.poll();
            if (err != null) throw err;
            return body;
        }

        FakeQuerier reply(Object obj) {
            try { replies.add(JSON.writeValueAsBytes(obj)); } catch (Exception e) { throw new RuntimeException(e); }
            return this;
        }
        FakeQuerier err(RuntimeException e) { errs.add(e); return this; }
    }

    private static Map<String, Object> map(Object... kv) {
        Map<String, Object> m = new LinkedHashMap<>();
        for (int i = 0; i < kv.length; i += 2) m.put((String) kv[i], kv[i + 1]);
        return m;
    }

    // --- KV path -----------------------------------------------------

    @Test void kvPath_quotesNamespacedKeys() {
        assertEquals("kv_default.'corpus:version'", Sql.kvPath("kv_default", "corpus:version"));
    }

    @Test void kvPath_preservesDotsAndSlashes() {
        assertEquals("kv_default.'a/b.c'", Sql.kvPath("kv_default", "a/b.c"));
    }

    @Test void kvPath_rejectsBadCollection() {
        assertThrows(HelperException.InvalidArgument.class,
            () -> Sql.kvPath("bad-name!", "k"));
    }

    @Test void kvValueLiteral_table() {
        assertEquals("NULL", Sql.kvValueLiteral(null));
        assertEquals("true", Sql.kvValueLiteral(true));
        assertEquals("false", Sql.kvValueLiteral(false));
        assertEquals("42", Sql.kvValueLiteral(42L));
        assertEquals("'hi'", Sql.kvValueLiteral("hi"));
        assertEquals("'o''reilly'", Sql.kvValueLiteral("o'reilly"));
        assertEquals("'{\"a\":1}'", Sql.kvValueLiteral(map("a", 1)));
    }

    // --- KV ops ------------------------------------------------------

    @Test void kv_set_emitsExactKeyPath() {
        FakeQuerier fq = new FakeQuerier().reply(map());
        new Helpers(fq).kv().set("characters:hansel", "ok");
        String sql = fq.sqls.get(0);
        assertTrue(sql.contains("kv_default.'characters:hansel'"), sql);
        assertTrue(sql.contains("= 'ok'"), sql);
    }

    @Test void kv_get_returnsValueOrNull() {
        FakeQuerier fq = new FakeQuerier()
            .reply(map("rows", List.of(map("value", "v"))))
            .reply(map("rows", List.of()));
        KvClient kv = new Helpers(fq).kv();
        assertEquals("v", kv.get("k"));
        assertNull(kv.get("k2"));
    }

    @Test void kv_exists_usesGet() {
        FakeQuerier fq = new FakeQuerier()
            .reply(map("rows", List.of(map("value", "v"))))
            .reply(map("rows", List.of()));
        KvClient kv = new Helpers(fq).kv();
        assertTrue(kv.exists("k").exists());
        assertFalse(kv.exists("k2").exists());
    }

    @Test void kv_list_filtersByPrefixWithoutRewriting() {
        FakeQuerier fq = new FakeQuerier().reply(map("rows", List.of(
            map("key", "a:1", "value", 1),
            map("key", "b:1", "value", 2),
            map("key", "a:2", "value", 3))));
        KvClient.KvListOptions opts = new KvClient.KvListOptions().prefix("a:");
        Envelopes.ListResult out = new Helpers(fq).kv().list(opts);
        assertEquals(2, out.items().size());
        assertEquals("a:1", out.items().get(0).get("key"));
        assertEquals("a:2", out.items().get(1).get("key"));
    }

    @Test void kv_list_rejectsNegativeLimit() {
        KvClient.KvListOptions opts = new KvClient.KvListOptions().limit(-1);
        assertThrows(HelperException.InvalidArgument.class,
            () -> new Helpers(new FakeQuerier()).kv().list(opts));
    }

    // --- Queue -------------------------------------------------------

    @Test void queue_push_emitsPriorityAndPayload() {
        FakeQuerier fq = new FakeQuerier().reply(map("affected", 1));
        QueueClient.PushOptions opts = new QueueClient.PushOptions().priority(5);
        new Helpers(fq).queue().push("jobs", map("id", 1), opts);
        String sql = fq.sqls.get(0);
        assertTrue(sql.startsWith("QUEUE PUSH jobs "), sql);
        assertTrue(sql.contains("PRIORITY 5"), sql);
        assertTrue(sql.contains("{\"id\":1}"), sql);
    }

    @Test void queue_len_returnsInt() {
        FakeQuerier fq = new FakeQuerier().reply(map("rows", List.of(map("len", 3))));
        assertEquals(3L, new Helpers(fq).queue().len("jobs"));
    }

    @Test void queue_pop_returnsPayloads() {
        FakeQuerier fq = new FakeQuerier().reply(map("rows", List.of(
            map("payload", "a"), map("payload", "b"))));
        List<Object> out = new Helpers(fq).queue().pop("jobs", 2);
        assertEquals(List.of("a", "b"), out);
    }

    @Test void queue_pop_rejectsNegativeCount() {
        assertThrows(HelperException.InvalidArgument.class,
            () -> new Helpers(new FakeQuerier()).queue().pop("jobs", -1));
    }

    @Test void queue_push_rejectsInvalidIdentifier() {
        assertThrows(HelperException.InvalidArgument.class,
            () -> new Helpers(new FakeQuerier()).queue().push("bad-name!", "x"));
    }

    // --- Documents ---------------------------------------------------

    @Test void documents_insert_returnsRIDEnvelope() {
        FakeQuerier fq = new FakeQuerier()
            .reply(map("rows", List.of(), "affected", 0))
            .reply(map("rows", List.of(map("rid", "doc-1", "body", map("name", "alice"))),
                       "affected", 1));
        Envelopes.InsertResult out = new Helpers(fq).documents()
            .insert("people", map("name", "alice"));
        assertEquals(1L, out.affected());
        assertEquals("doc-1", out.rid());
        assertEquals("doc-1", out.item().get("rid"));
    }

    @Test void documents_get_raisesNotFoundOnMissing() {
        FakeQuerier fq = new FakeQuerier().reply(map("rows", List.of()));
        assertThrows(HelperException.NotFound.class,
            () -> new Helpers(fq).documents().get("people", "doc-1"));
    }

    @Test void documents_patch_rejectsJSONPointerPaths() {
        assertThrows(HelperException.InvalidArgument.class,
            () -> new Helpers(new FakeQuerier()).documents()
                .patch("people", "doc-1", map("a/b", 1)));
    }

    @Test void documents_list_ordersByRIDByDefault() {
        FakeQuerier fq = new FakeQuerier().reply(map("rows", List.of(
            map("rid", "a"), map("rid", "b"))));
        Envelopes.ListResult out = new Helpers(fq).documents()
            .list("people", new DocumentClient.ListOptions());
        assertEquals(2, out.items().size());
        assertTrue(fq.sqls.get(0).contains("ORDER BY rid ASC"), fq.sqls.get(0));
    }

    @Test void documents_insert_passesThroughExistingCollection() {
        FakeQuerier fq = new FakeQuerier()
            .reply(map())
            .reply(map("rows", List.of(map("rid", "x")), "affected", 1))
            .err(new RuntimeException("collection already exists"));
        // The Sql ensureCollection runs first and throws; KV/queue helpers
        // catch the "already exists" string. Then the real insert proceeds.
        new Helpers(fq).documents().insert("people", map("a", 1));
    }

    // --- decode helpers ----------------------------------------------

    @Test void affectedFromBody_handlesNestedResult() throws Exception {
        byte[] body = JSON.writeValueAsBytes(map("result", map("affected", 7)));
        assertEquals(7L, Sql.affectedFromBody(body));
    }

    // --- v1.0 additions ----------------------------------------------

    @Test void helperSpecVersion_isOne() {
        assertEquals("1.0", Helpers.HELPER_SPEC_VERSION);
    }

    @Test void documents_patch_rejectsEmptyPatch() {
        assertThrows(HelperException.InvalidArgument.class,
            () -> new Helpers(new FakeQuerier()).documents()
                .patch("people", "doc-1", Map.of()));
    }

    @Test void deleteResult_deletedTracksAffected() {
        assertTrue(new Envelopes.DeleteResult(1).deleted());
        assertFalse(new Envelopes.DeleteResult(0).deleted());
        assertTrue(new Envelopes.DeleteResult(2L, true).deleted());
    }

    @Test void documents_delete_missingReturnsNotDeleted() {
        FakeQuerier fq = new FakeQuerier().reply(map("affected", 0));
        Envelopes.DeleteResult out = new Helpers(fq).documents().delete("people", "doc-1");
        assertEquals(0L, out.affected());
        assertFalse(out.deleted());
    }

    @Test void kv_delete_missingReturnsNotDeleted() {
        FakeQuerier fq = new FakeQuerier().reply(map("affected", 0));
        Envelopes.DeleteResult out = new Helpers(fq).kv().delete("missing-key");
        assertEquals(0L, out.affected());
        assertFalse(out.deleted());
    }

    @Test void queues_create_emitsIdempotentDDL() {
        FakeQuerier fq = new FakeQuerier().reply(map());
        new Helpers(fq).queues().create("jobs");
        assertEquals("CREATE QUEUE IF NOT EXISTS jobs", fq.sqls.get(0));
    }

    @Test void queues_create_rejectsBadIdentifier() {
        assertThrows(HelperException.InvalidArgument.class,
            () -> new Helpers(new FakeQuerier()).queues().create("bad-name!"));
    }

    @Test void queues_aliasMatchesQueue() {
        Helpers h = new Helpers(new FakeQuerier());
        assertNotNull(h.queues());
        assertNotNull(h.queue());
    }

    // --- Tx ----------------------------------------------------------

    @Test void tx_imperative_emitsBeginCommit() {
        FakeQuerier fq = new FakeQuerier().reply(map()).reply(map());
        TxClient tx = new Helpers(fq).tx();
        tx.begin();
        tx.commit();
        assertEquals("BEGIN", fq.sqls.get(0));
        assertEquals("COMMIT", fq.sqls.get(1));
    }

    @Test void tx_imperative_rollbackEmitsRollback() {
        FakeQuerier fq = new FakeQuerier().reply(map()).reply(map());
        TxClient tx = new Helpers(fq).tx();
        tx.begin();
        tx.rollback();
        assertEquals("BEGIN", fq.sqls.get(0));
        assertEquals("ROLLBACK", fq.sqls.get(1));
    }

    @Test void tx_run_commitsOnSuccess() {
        FakeQuerier fq = new FakeQuerier()
            .reply(map())  // BEGIN
            .reply(map())  // body query
            .reply(map()); // COMMIT
        new Helpers(fq).tx().run(t -> {
            // body does its own query through the same Querier indirectly;
            // we just need to confirm BEGIN/COMMIT bracket the call.
        });
        assertEquals("BEGIN", fq.sqls.get(0));
        assertEquals("COMMIT", fq.sqls.get(1));
    }

    @Test void tx_run_rollsBackOnException() {
        FakeQuerier fq = new FakeQuerier().reply(map()).reply(map());
        TxClient tx = new Helpers(fq).tx();
        RuntimeException boom = new RuntimeException("nope");
        RuntimeException caught = assertThrows(RuntimeException.class,
            () -> tx.run(t -> { throw boom; }));
        assertSame(boom, caught);
        assertEquals("BEGIN", fq.sqls.get(0));
        assertEquals("ROLLBACK", fq.sqls.get(1));
    }

    @Test void tx_run_rejectsNesting() {
        FakeQuerier fq = new FakeQuerier().reply(map()).reply(map());
        TxClient tx = new Helpers(fq).tx();
        assertThrows(HelperException.InvalidArgument.class,
            () -> tx.run(outer -> outer.run(inner -> {})));
    }
}
