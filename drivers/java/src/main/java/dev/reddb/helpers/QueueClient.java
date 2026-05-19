package dev.reddb.helpers;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/** Implements {@code queue.*} from the SDK Helper Spec. */
public final class QueueClient {
    private final Querier q;
    QueueClient(Querier q) { this.q = q; }

    /** Controls {@link #push(String, Object, PushOptions)}. */
    public static final class PushOptions {
        public Integer priority = null;
        public PushOptions priority(int v) { this.priority = v; return this; }
    }

    /** Create a queue if it does not already exist. Idempotent. */
    public void create(String queue) {
        Sql.assertIdentifier(queue, "queue name");
        q.query("CREATE QUEUE IF NOT EXISTS " + Sql.sqlIdentifier(queue));
    }

    public Envelopes.QueuePushResult push(String queue, Object value) { return push(queue, value, null); }

    public Envelopes.QueuePushResult push(String queue, Object value, PushOptions opts) {
        Sql.assertIdentifier(queue, "queue name");
        if (opts == null) opts = new PushOptions();
        String lit = Sql.queueValueLiteral(value);
        String priority = opts.priority == null ? "" : " PRIORITY " + opts.priority;
        String sql = "QUEUE PUSH " + Sql.sqlIdentifier(queue) + " " + lit + priority;
        byte[] body = q.query(sql);
        long affected = Sql.affectedFromBody(body);
        if (affected == 0L) affected = 1L;
        Object[] fr = Sql.firstRow(body);
        @SuppressWarnings("unchecked")
        Map<String, Object> row = (Map<String, Object>) fr[0];
        String rid = row == null ? null : Sql.ridString(row.get("rid"));
        return new Envelopes.QueuePushResult(affected, rid);
    }

    public List<Object> pop(String queue) { return fetch("POP", queue, null); }
    public List<Object> pop(String queue, int count) { return fetch("POP", queue, count); }

    public List<Object> peek(String queue) { return fetch("PEEK", queue, null); }
    public List<Object> peek(String queue, int count) { return fetch("PEEK", queue, count); }

    private List<Object> fetch(String verb, String queue, Integer count) {
        Sql.assertIdentifier(queue, "queue name");
        String suffix = "";
        if (count != null) {
            if (count < 0) {
                throw new HelperException.InvalidArgument(
                    "queue count must be a non-negative integer");
            }
            suffix = " COUNT " + count;
        }
        byte[] body = q.query("QUEUE " + verb + " " + Sql.sqlIdentifier(queue) + suffix);
        List<Map<String, Object>> rows = Sql.allRows(body);
        List<Object> out = new ArrayList<>(rows.size());
        for (Map<String, Object> r : rows) out.add(r.get("payload"));
        return out;
    }

    public long len(String queue) {
        Sql.assertIdentifier(queue, "queue name");
        byte[] body = q.query("QUEUE LEN " + Sql.sqlIdentifier(queue));
        Object[] fr = Sql.firstRow(body);
        @SuppressWarnings("unchecked")
        Map<String, Object> row = (Map<String, Object>) fr[0];
        if (row == null) return 0L;
        Object v = row.get("len");
        if (v instanceof Number n) return n.longValue();
        return 0L;
    }

    public Envelopes.DeleteResult purge(String queue) {
        Sql.assertIdentifier(queue, "queue name");
        byte[] body = q.query("QUEUE PURGE " + Sql.sqlIdentifier(queue));
        return new Envelopes.DeleteResult(Sql.affectedFromBody(body));
    }
}
