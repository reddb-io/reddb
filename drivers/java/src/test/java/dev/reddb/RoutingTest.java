package dev.reddb;

import com.fasterxml.jackson.databind.*;
import com.fasterxml.jackson.databind.node.ObjectNode;
import org.junit.jupiter.api.Test;
import java.nio.file.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import static org.junit.jupiter.api.Assertions.*;

class RoutingTest {
    private static final ObjectMapper JSON = new ObjectMapper();
    private static Path fixtures() {
        Path root = Path.of("").toAbsolutePath();
        while (!Files.exists(root.resolve("testdata/conformance/topology/manifest.json"))) root = root.getParent();
        return root.resolve("testdata/conformance/topology");
    }
    @Test void sharedFixtures() throws Exception {
        for (JsonNode fixture : JSON.readTree(fixtures().resolve("manifest.json").toFile()).path("cases")) {
            byte[] bytes = HexFormat.of().parseHex(fixture.path("hex").asText());
            if (fixture.path("error").asBoolean()) assertThrows(RuntimeException.class, () -> TopologyCodec.decode(bytes));
            else assertEquals(fixture.get("expected").isNull() ? null : fixture.get("expected"), TopologyCodec.decode(bytes) == null ? null : JSON.readTree(TopologyCodec.decode(bytes).toString()), fixture.path("name").asText());
        }
        for (JsonNode fixture : JSON.readTree(fixtures().resolve("sql-policy.json").toFile()))
            assertEquals(fixture.path("replica_read").asBoolean(), RoutingSql.policy(fixture.path("sql").asText()).read(), fixture.path("sql").asText());
    }
    private static class Peer implements Conn, TopologySource {
        final String address; final List<String> calls; final java.util.function.Supplier<ObjectNode> membership;
        Peer(String address, List<String> calls, java.util.function.Supplier<ObjectNode> membership) { this.address=address;this.calls=calls;this.membership=membership; }
        public ObjectNode topology(String protocol) { return membership.get(); }
        public byte[] query(String sql) { return query(sql, new Object[0]); }
        public byte[] query(String sql,Object... params) {
            calls.add(address+" "+sql);
            if (sql.contains("lost") || sql.contains("effectful")) throw new RedDBException.ProtocolError("lost reply",new java.io.IOException());
            return address.getBytes(StandardCharsets.UTF_8);
        }
        public void insert(String c,Object p) {} public void bulkInsert(String c,List<?> p) {}
        public byte[] get(String c,String id) { return query("get"); } public void delete(String c,String id) {}
        public void ping() {} public void close() {}
    }
    @Test void routingAndNoAmbiguousReplay() {
        String primary="https://primary:5000",replica="https://replica:5000";
        List<String> calls=new ArrayList<>();
        java.util.function.Supplier<ObjectNode> membership=()->{
            ObjectNode value=JSON.createObjectNode().put("version",1);
            value.putObject("primary").put("addr",primary);
            value.putArray("replicas").addObject().put("addr",replica).put("healthy",true).put("lag_ms",10);
            return value;
        };
        RoutedConn.Dial dial=(address,control)->new Peer(address,calls,membership);
        try (Conn client=new RoutedConn(primary,dial.open(primary,false),"https",false,dial)) {
            assertEquals(primary,new String(client.query("SELECT 1"),StandardCharsets.UTF_8));
            assertEquals(replica,new String(client.queryWithOptions("SELECT 1",ReadOptions.local()),StandardCharsets.UTF_8));
            client.query("BEGIN");
            assertEquals(primary,new String(client.queryWithOptions("SELECT 1",ReadOptions.local()),StandardCharsets.UTF_8));
            client.query("ROLLBACK");
            for(String sql: List.of("UPDATE lost","SELECT \"effectful\"()")) {
                int before=calls.size();
                assertThrows(RedDBException.ProtocolError.class,()->client.queryWithOptions(sql,ReadOptions.local()));
                assertEquals(before+1,calls.size()); assertTrue(calls.get(calls.size()-1).startsWith(primary));
            }
        }
    }
}
