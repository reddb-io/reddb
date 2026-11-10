package dev.reddb;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.time.Duration;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.function.Function;

final class RoutedConn implements Conn {
    interface Dial { Conn open(String address, boolean control); }
    private final Dial dial;
    private final String seed, protocol;
    private final boolean forcePrimary;
    private final Map<String, Conn> clients = new HashMap<>();
    private record Replica(String address, long lag) {}
    private List<Replica> replicas = List.of();
    private String primary, pinned;
    private boolean session, broken, closed;
    private long refreshAt, next;

    RoutedConn(String seed, Conn initial, String protocol, boolean forcePrimary, Dial dial) {
        this.seed = this.primary = seed; this.protocol = protocol;
        this.forcePrimary = forcePrimary; this.dial = dial; clients.put(seed, initial);
    }
    synchronized void refresh() {
        if (pinned != null || closed) return;
        refreshAt = System.nanoTime() + Duration.ofSeconds(30).toNanos();
        long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
        List<String> candidates = new ArrayList<>(List.of(primary, seed));
        replicas.forEach(replica -> candidates.add(replica.address));
        for (int index = 0; index < candidates.size() && index < 258 && System.nanoTime() < deadline; index++) {
            try (Conn control = dial.open(candidates.get(index), true)) {
                if (!(control instanceof TopologySource source)) continue;
                ObjectNode topology = source.topology(protocol);
                if (topology == null || topology.path("version").asInt() != 1) continue;
                String writer = TopologyCodec.address(topology.path("primary").path("addr").asText(), protocol);
                if (writer == null) continue;
                if (!candidates.contains(writer)) candidates.add(writer);
                endpoint(writer);
                List<Replica> ready = new ArrayList<>();
                int count = 0;
                for (JsonNode replica : topology.path("replicas")) {
                    if (++count > 256 || System.nanoTime() >= deadline) break;
                    String address = TopologyCodec.address(replica.path("addr").asText(), protocol);
                    if (address == null || address.equals(writer) || !replica.path("healthy").asBoolean() || replica.path("rebootstrapping").asBoolean()) continue;
                    try {
                        endpoint(address);
                        ready.add(new Replica(address, replica.path("lag_ms").asLong(Long.MAX_VALUE)));
                    } catch (RuntimeException error) { if (Thread.currentThread().isInterrupted()) throw error; }
                }
                primary = writer; replicas = ready;
                Set<String> keep = new HashSet<>(List.of(seed, writer));
                ready.forEach(replica -> keep.add(replica.address));
                for (String address : new ArrayList<>(clients.keySet()))
                    if (!keep.contains(address)) clients.remove(address).close();
                if (writer.equals(candidates.get(index))) return;
            } catch (RedDBException.AuthRefused error) { throw error; }
            catch (RuntimeException error) { if (Thread.currentThread().isInterrupted()) throw error; }
        }
    }
    private Conn endpoint(String address) {
        Conn existing = clients.get(address);
        if (existing != null) return existing;
        Conn client = dial.open(address, false);
        try { client.ping(); }
        catch (RuntimeException error) { client.close(); throw error; }
        clients.put(address, client); return client;
    }
    private static boolean network(Throwable error) {
        for (int count = 0; error != null && count < 16; count++, error = error.getCause())
            if (error instanceof java.io.IOException || error instanceof InterruptedException) return true;
        return false;
    }
    private synchronized <T> T invoke(Function<Conn, T> operation, String sql, boolean read, ReadOptions options) {
        if (closed || broken) throw new IllegalStateException("connection is closed");
        if (pinned == null && System.nanoTime() >= refreshAt) refresh();
        RoutingSql.Policy policy = sql == null ? new RoutingSql.Policy("primary", false) : RoutingSql.policy(sql);
        read |= options != null && policy.read();
        List<Replica> eligible = new ArrayList<>();
        if (!forcePrimary && (options == null || options.consistency() != ReadOptions.Consistency.STRONG))
            for (Replica replica : replicas)
                if (options == null || options.consistency() != ReadOptions.Consistency.BOUNDED_STALENESS || replica.lag <= options.maxLag().toMillis()) eligible.add(replica);
        String address = pinned != null ? pinned : read && !eligible.isEmpty()
            ? eligible.get((int)Long.remainderUnsigned(next++, eligible.size())).address : primary;
        if (policy.state().equals("begin") || policy.state().equals("session")) { pinned = address; session |= policy.state().equals("session"); }
        try {
            T result = operation.apply(endpoint(address));
            if (policy.state().equals("end") && !session) pinned = null;
            return result;
        } catch (RuntimeException error) {
            if (!network(error)) throw error;
            refreshAt = 0;
            Conn failed = clients.remove(address); if (failed != null) failed.close();
            if (pinned != null) broken = true;
            if (pinned != null || !read || Thread.currentThread().isInterrupted()) throw error;
            refresh(); return operation.apply(endpoint(primary));
        }
    }
    public byte[] query(String sql) { return query(sql, new Object[0]); }
    public byte[] query(String sql, Object... params) { return invoke(client -> client.query(sql, params), sql, false, null); }
    public byte[] queryWithOptions(String sql, ReadOptions options, Object... params) { return invoke(client -> client.query(sql, params), sql, false, java.util.Objects.requireNonNull(options)); }
    public void insert(String collection, Object payload) { invoke(client -> { client.insert(collection, payload); return null; }, null, false, null); }
    public void bulkInsert(String collection, List<?> rows) { invoke(client -> { client.bulkInsert(collection, rows); return null; }, null, false, null); }
    public byte[] get(String collection, String id) { return invoke(client -> client.get(collection, id), null, true, null); }
    public void delete(String collection, String id) { invoke(client -> { client.delete(collection, id); return null; }, null, false, null); }
    public void ping() { invoke(client -> { client.ping(); return null; }, null, false, null); }
    public synchronized void close() { closed = true; clients.values().forEach(Conn::close); clients.clear(); }
}
