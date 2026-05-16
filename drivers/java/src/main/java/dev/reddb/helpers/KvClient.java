package dev.reddb.helpers;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/** Implements {@code kv.*} from the SDK Helper Spec. */
public final class KvClient {
    private final Querier q;
    private final String collection;

    KvClient(Querier q) { this(q, "kv_default"); }
    KvClient(Querier q, String collection) { this.q = q; this.collection = collection; }

    /** Controls {@link #set(String, Object, SetOptions)} / {@link #put(String, Object, SetOptions)}. */
    public static final class SetOptions {
        public String collection = null;
        public List<String> tags = null;
        public long expireMs = 0L;

        public SetOptions collection(String v) { this.collection = v; return this; }
        public SetOptions tags(List<String> v) { this.tags = v; return this; }
        public SetOptions expireMs(long v) { this.expireMs = v; return this; }
    }

    /** Controls {@link #list(KvListOptions)}. */
    public static final class KvListOptions {
        public String collection = null;
        public int limit = 0;
        public String prefix = null;

        public KvListOptions collection(String v) { this.collection = v; return this; }
        public KvListOptions limit(int v) { this.limit = v; return this; }
        public KvListOptions prefix(String v) { this.prefix = v; return this; }
    }

    public void set(String key, Object value) { put(key, value, null); }
    public void set(String key, Object value, SetOptions opts) { put(key, value, opts); }
    public void put(String key, Object value) { put(key, value, null); }

    /** Store an exact key/value pair. */
    public void put(String key, Object value, SetOptions opts) {
        if (opts == null) opts = new SetOptions();
        String coll = (opts.collection == null || opts.collection.isEmpty()) ? collection : opts.collection;
        String lit = Sql.kvValueLiteral(value);
        String expire = opts.expireMs > 0 ? " EXPIRE " + opts.expireMs + " ms" : "";
        String tagClause = "";
        if (opts.tags != null && !opts.tags.isEmpty()) {
            List<String> parts = new ArrayList<>(opts.tags.size());
            for (String t : opts.tags) parts.add(Sql.kvTagLiteral(t));
            tagClause = " TAGS [" + String.join(", ", parts) + "]";
        }
        String path = Sql.kvPath(coll, key);
        q.query("KV PUT " + path + " = " + lit + expire + tagClause);
    }

    public Object get(String key) { return get(key, null); }

    /** Return the stored value or {@code null} when missing. */
    public Object get(String key, String collection) {
        String coll = (collection == null || collection.isEmpty()) ? this.collection : collection;
        String path = Sql.kvPath(coll, key);
        byte[] body = q.query("KV GET " + path);
        Object[] fr = Sql.firstRow(body);
        @SuppressWarnings("unchecked")
        Map<String, Object> row = (Map<String, Object>) fr[0];
        if (row == null) return null;
        return row.get("value");
    }

    public Envelopes.ExistsResult exists(String key) { return exists(key, null); }

    public Envelopes.ExistsResult exists(String key, String collection) {
        return new Envelopes.ExistsResult(get(key, collection) != null);
    }

    public Envelopes.DeleteResult delete(String key) { return delete(key, null); }

    public Envelopes.DeleteResult delete(String key, String collection) {
        String coll = (collection == null || collection.isEmpty()) ? this.collection : collection;
        String path = Sql.kvPath(coll, key);
        byte[] body = q.query("KV DELETE " + path);
        return new Envelopes.DeleteResult(Sql.affectedFromBody(body));
    }

    public Envelopes.ListResult list(KvListOptions opts) {
        if (opts == null) opts = new KvListOptions();
        String coll = (opts.collection == null || opts.collection.isEmpty()) ? collection : opts.collection;
        int limit = Sql.normalizeLimit(opts.limit);
        String sql = "SELECT key, value FROM " + Sql.sqlIdentifier(coll)
            + " ORDER BY key ASC LIMIT " + limit;
        byte[] body = q.query(sql);
        List<Map<String, Object>> rows = Sql.allRows(body);
        if (opts.prefix != null && !opts.prefix.isEmpty()) {
            List<Map<String, Object>> filtered = new ArrayList<>();
            for (Map<String, Object> r : rows) {
                Object k = r.get("key");
                if (k instanceof String s && s.startsWith(opts.prefix)) filtered.add(r);
            }
            rows = filtered;
        }
        return new Envelopes.ListResult(rows);
    }
}
