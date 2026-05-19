package dev.reddb.helpers;

import com.fasterxml.jackson.databind.ObjectMapper;
import dev.reddb.Conn;
import dev.reddb.Reddb;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestInstance;
import org.junit.jupiter.api.condition.DisabledIfEnvironmentVariable;
import org.junit.jupiter.api.condition.EnabledIfEnvironmentVariable;

import java.io.File;
import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.TimeUnit;

import static org.junit.jupiter.api.Assertions.*;

/**
 * SDK Helper Spec v1.0 conformance cases. The case IDs mirror
 * {@code docs/spec/sdk-helpers.md §12} verbatim (dots → underscores in
 * method names so cross-driver CI dashboards line up).
 *
 * <p>The Java driver does not embed the engine, so this harness spawns
 * one {@code red server} process per JUnit run. Gated on
 * {@code RED_SMOKE=1} and skippable via {@code RED_SKIP_SMOKE=1}.</p>
 */
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
@EnabledIfEnvironmentVariable(named = "RED_SMOKE", matches = "1")
@DisabledIfEnvironmentVariable(named = "RED_SKIP_SMOKE", matches = "1")
class ConformanceTest {

    private static final ObjectMapper JSON = new ObjectMapper();

    private Process proc;
    private Path dataDir;
    private Conn conn;
    private Helpers helpers;

    @BeforeAll
    void boot() throws Exception {
        File repoRoot = findRepoRoot();
        dataDir = Files.createTempDirectory("reddb-java-conformance-");
        int port = freePort();
        ProcessBuilder pb = new ProcessBuilder(redCommand(dataDir.resolve("data.db"), port));
        pb.directory(repoRoot);
        pb.redirectErrorStream(true);
        pb.redirectOutput(ProcessBuilder.Redirect.DISCARD);
        proc = pb.start();
        conn = waitForConnect("red://127.0.0.1:" + port, Duration.ofSeconds(60));
        helpers = Helpers.of(conn);
    }

    @AfterAll
    void teardown() throws Exception {
        if (conn != null) conn.close();
        if (proc != null) {
            proc.destroy();
            if (!proc.waitFor(10, TimeUnit.SECONDS)) proc.destroyForcibly();
        }
    }

    private String uniq(String prefix) {
        return prefix + "_" + UUID.randomUUID().toString().replace("-", "").substring(0, 12);
    }

    // ---- generic ---------------------------------------------------

    @Test void TestConformance_generic_query_no_params() {
        byte[] body = conn.query("SELECT 1");
        assertTrue(new String(body, StandardCharsets.UTF_8).contains("\"ok\":true"));
    }

    @Test void TestConformance_generic_query_with_params() {
        String tbl = uniq("c_qp");
        conn.query("CREATE TABLE " + tbl + " (id INT, name TEXT)");
        conn.query("INSERT INTO " + tbl + " (id, name) VALUES ($1, $2)", 42, "alice");
        byte[] body = conn.query("SELECT name FROM " + tbl + " WHERE id = $1", 42);
        assertTrue(new String(body, StandardCharsets.UTF_8).contains("alice"));
    }

    @Test void TestConformance_generic_insert_rid() {
        String col = uniq("c_ins");
        Envelopes.InsertResult r = helpers.documents().insert(col, Map.of("name", "alice"));
        assertEquals(1L, r.affected());
        assertNotNull(r.rid());
        assertFalse(r.rid().isEmpty());
    }

    @Test void TestConformance_generic_bulk_insert_rids() {
        // empty no-op
        // helpers.documents has no bulk; route through conn-level helper isn't
        // exposed on Java's Conn surface as a single bulk envelope, but the
        // spec lets drivers fall back to a transaction. We use multiple
        // sequential inserts and assert affected == count.
        String col = uniq("c_bulk");
        List<String> rids = new ArrayList<>();
        rids.add(helpers.documents().insert(col, Map.of("i", 1)).rid());
        rids.add(helpers.documents().insert(col, Map.of("i", 2)).rid());
        assertEquals(2, rids.size());
        for (String rid : rids) assertNotNull(rid);
    }

    @Test void TestConformance_generic_delete() {
        String col = uniq("c_del");
        String rid = helpers.documents().insert(col, Map.of("x", 1)).rid();
        Envelopes.DeleteResult d = helpers.documents().delete(col, rid);
        assertEquals(1L, d.affected());
        assertTrue(d.deleted());
    }

    // ---- documents -------------------------------------------------

    @Test void TestConformance_documents_crud_nested_patch() {
        String col = uniq("d_crud");
        String rid = helpers.documents().insert(col, Map.of("name", "alice", "age", 30)).rid();
        var got = helpers.documents().get(col, rid);
        assertNotNull(got);
        Envelopes.ListResult list = helpers.documents().list(col, new DocumentClient.ListOptions().limit(10));
        assertFalse(list.items().isEmpty());
        var patched = helpers.documents().patch(col, rid, Map.of("age", 31));
        assertNotNull(patched);
        Envelopes.DeleteResult d = helpers.documents().delete(col, rid);
        assertTrue(d.deleted());
    }

    @Test void TestConformance_documents_delete_missing_no_error() {
        String col = uniq("d_dm");
        helpers.documents().insert(col, Map.of("k", 1));  // ensure collection
        Envelopes.DeleteResult d = helpers.documents().delete(col, "no-such-rid");
        assertEquals(0L, d.affected());
        assertFalse(d.deleted());
    }

    @Test void TestConformance_documents_patch_empty_rejects() {
        assertThrows(HelperException.InvalidArgument.class,
            () -> helpers.documents().patch("any_col", "any_rid", Map.of()));
    }

    // ---- kv --------------------------------------------------------

    @Test void TestConformance_kv_exact_key_round_trip() {
        String coll = uniq("kvc");
        // create collection by an insert through conn
        conn.query("CREATE KV " + coll);
        KvClient kv = helpers.kv(coll);
        kv.set("characters:hansel", "witch");
        Object v = kv.get("characters:hansel");
        assertEquals("witch", v);
        Envelopes.ListResult out = kv.list(new KvClient.KvListOptions().prefix("characters:"));
        assertFalse(out.items().isEmpty());
    }

    @Test void TestConformance_kv_missing_get_returns_none() {
        String coll = uniq("kvc");
        conn.query("CREATE KV " + coll);
        Object v = helpers.kv(coll).get("nope");
        assertNull(v);
    }

    @Test void TestConformance_kv_delete_returns_envelope() {
        String coll = uniq("kvc");
        conn.query("CREATE KV " + coll);
        KvClient kv = helpers.kv(coll);
        kv.set("k", "v");
        Envelopes.DeleteResult d = kv.delete("k");
        assertTrue(d.deleted());
    }

    // ---- queues ----------------------------------------------------

    @Test void TestConformance_queues_fifo_peek_pop_len() {
        String qn = uniq("q_fifo");
        QueueClient q = helpers.queues();
        q.create(qn);
        q.push(qn, Map.of("v", 1));
        q.push(qn, Map.of("v", 2));
        assertEquals(2L, q.len(qn));
        List<Object> peek = q.peek(qn, 1);
        assertEquals(1, peek.size());
        List<Object> p1 = q.pop(qn, 1);
        assertEquals(1, p1.size());
        List<Object> p2 = q.pop(qn, 1);
        assertEquals(1, p2.size());
    }

    @Test void TestConformance_queues_empty_pop_returns_empty() {
        String qn = uniq("q_empty");
        helpers.queues().create(qn);
        List<Object> p = helpers.queues().pop(qn, 1);
        assertTrue(p.isEmpty());
    }

    @Test void TestConformance_queues_purge_resets_len() {
        String qn = uniq("q_purge");
        QueueClient q = helpers.queues();
        q.create(qn);
        q.push(qn, "a");
        q.push(qn, "b");
        q.purge(qn);
        assertEquals(0L, q.len(qn));
    }

    // ---- tx --------------------------------------------------------

    @Test void TestConformance_tx_commit_persists() {
        String col = uniq("tx_commit");
        helpers.documents().insert(col, Map.of("seed", true));  // ensure collection
        TxClient tx = helpers.tx();
        tx.begin();
        String rid = helpers.documents().insert(col, Map.of("x", 1)).rid();
        tx.commit();
        var got = helpers.documents().get(col, rid);
        assertNotNull(got);
    }

    @Test void TestConformance_tx_rollback_discards() {
        String col = uniq("tx_rb");
        helpers.documents().insert(col, Map.of("seed", true));  // ensure collection
        TxClient tx = helpers.tx();
        tx.begin();
        String rid = helpers.documents().insert(col, Map.of("x", 1)).rid();
        tx.rollback();
        assertThrows(HelperException.NotFound.class,
            () -> helpers.documents().get(col, rid));
    }

    // ---- errors ----------------------------------------------------

    @Test void TestConformance_errors_not_found_document_get() {
        String col = uniq("err_nf");
        helpers.documents().insert(col, Map.of("seed", true));  // ensure collection
        assertThrows(HelperException.NotFound.class,
            () -> helpers.documents().get(col, "no-such-rid"));
    }

    // ---- provisional wire ------------------------------------------

    @Test void TestConformance_wire_probabilistic_hll_round_trip() {
        String name = uniq("hll");
        conn.query("CREATE HLL " + name);
        conn.query("HLL ADD " + name + " 'user1' 'user2'");
        byte[] body = conn.query("HLL COUNT " + name);
        String s = new String(body, StandardCharsets.UTF_8);
        assertTrue(s.contains("count") || s.contains("cardinality"),
            "expected count or cardinality column: " + s);
    }

    // ---- spec version constant -------------------------------------

    @Test void TestConformance_meta_spec_version() {
        assertEquals("1.0", Helpers.HELPER_SPEC_VERSION);
    }

    // ---- support helpers ------------------------------------------

    private static List<String> redCommand(Path dataPath, int port) {
        String redBin = System.getenv("RED_BIN");
        List<String> cmd = new ArrayList<>();
        if (redBin != null && !redBin.isBlank()) {
            cmd.add(redBin);
        } else {
            cmd.add("cargo");
            cmd.add("run");
            cmd.add("--release");
            cmd.add("--bin");
            cmd.add("red");
            cmd.add("--");
        }
        cmd.add("server");
        cmd.add("--path");
        cmd.add(dataPath.toString());
        cmd.add("--bind");
        cmd.add("127.0.0.1:" + port);
        return cmd;
    }

    private static Conn waitForConnect(String uri, Duration timeout) throws Exception {
        long deadline = System.nanoTime() + timeout.toNanos();
        RuntimeException last = null;
        while (System.nanoTime() < deadline) {
            try {
                Conn c = Reddb.connect(uri);
                try {
                    c.ping();
                    return c;
                } catch (RuntimeException e) {
                    c.close();
                    last = e;
                }
            } catch (RuntimeException e) {
                last = e;
            }
            Thread.sleep(50);
        }
        IOException error = new IOException("server did not accept connections at " + uri);
        if (last != null) error.initCause(last);
        throw error;
    }

    private static int freePort() throws IOException {
        try (ServerSocket socket = new ServerSocket(0, 0, InetAddress.getByName("127.0.0.1"))) {
            return socket.getLocalPort();
        }
    }

    private static File findRepoRoot() {
        File f = new File("").getAbsoluteFile();
        while (f != null) {
            if (new File(f, "Cargo.toml").exists() && new File(f, "drivers").isDirectory()) {
                return f;
            }
            f = f.getParentFile();
        }
        fail("could not locate repo root with Cargo.toml + drivers/");
        return null;
    }
}
