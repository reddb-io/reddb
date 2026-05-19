package dev.reddb.helpers;

/**
 * Groups the rich namespaces ({@link DocumentClient}, {@link KvClient},
 * {@link QueueClient}, {@link TxClient}) bound to a single transport.
 * Stateless — safe to construct per call. Mirrors
 * {@code drivers/go/helpers.go}.
 */
public final class Helpers {
    /** SDK Helper Spec version this driver targets — see docs/spec/sdk-helpers.md §14. */
    public static final String HELPER_SPEC_VERSION = "1.0";

    private final Querier q;

    public Helpers(Querier q) { this.q = q; }

    /** Wrap any {@link Querier} (typically a {@link dev.reddb.Conn}). */
    public static Helpers of(Querier q) { return new Helpers(q); }

    /** Wrap a {@link dev.reddb.Conn}. */
    public static Helpers of(dev.reddb.Conn conn) {
        return new Helpers((sql, params) -> {
            if (params == null || params.length == 0) return conn.query(sql);
            return conn.query(sql, params);
        });
    }

    public DocumentClient documents() { return new DocumentClient(q); }
    public KvClient kv() { return new KvClient(q); }
    public KvClient kv(String collection) { return new KvClient(q, collection); }
    public QueueClient queue() { return new QueueClient(q); }
    /** Spec namespace alias for {@link #queue()} (`queues.*`). */
    public QueueClient queues() { return new QueueClient(q); }
    public TxClient tx() { return new TxClient(q); }
}
