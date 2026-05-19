package dev.reddb.helpers;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/** Implements {@code documents.*} from the SDK Helper Spec. */
public final class DocumentClient {
    private final Querier q;
    DocumentClient(Querier q) { this.q = q; }

    /** Tweaks list result ordering and bounds. */
    public static final class ListOptions {
        public int limit = 0;
        public String orderBy = null;
        public String filter = null;

        public ListOptions limit(int v) { this.limit = v; return this; }
        public ListOptions orderBy(String v) { this.orderBy = v; return this; }
        public ListOptions filter(String v) { this.filter = v; return this; }
    }

    /** Insert one document. Returns spec {@link Envelopes.InsertResult}. */
    public Envelopes.InsertResult insert(String collection, Map<String, Object> document) {
        if (document == null) {
            throw new HelperException.InvalidArgument("documents.insert document must be an object");
        }
        ensureCollection(collection);
        String sql = "INSERT INTO " + Sql.sqlIdentifierPath(collection)
            + " DOCUMENT (body) VALUES (" + Sql.jsonLiteral(document) + ") RETURNING *";
        byte[] body = q.query(sql);
        Object[] fr = Sql.firstRow(body);
        @SuppressWarnings("unchecked")
        Map<String, Object> row = (Map<String, Object>) fr[0];
        long affected = (long) fr[1];
        if (row == null || row.get("rid") == null) {
            throw new HelperException.InvalidResponse(
                "documents.insert expected one returned item with rid");
        }
        if (affected == 0L) affected = 1L;
        return new Envelopes.InsertResult(affected, Sql.ridString(row.get("rid")), row);
    }

    /** Fetch one document by rid. Throws {@link HelperException.NotFound} when missing. */
    public Map<String, Object> get(String collection, String rid) {
        String sql = "SELECT * FROM " + Sql.sqlIdentifierPath(collection)
            + " WHERE rid = $1 LIMIT 1";
        byte[] body = q.query(sql, rid);
        Object[] fr = Sql.firstRow(body);
        @SuppressWarnings("unchecked")
        Map<String, Object> row = (Map<String, Object>) fr[0];
        if (row == null) {
            throw new HelperException.NotFound("document \"" + rid + "\" was not found");
        }
        return row;
    }

    /** List up to {@code opts.limit} rows ordered by {@code opts.orderBy} (default {@code rid ASC}). */
    public Envelopes.ListResult list(String collection, ListOptions opts) {
        if (opts == null) opts = new ListOptions();
        int limit = Sql.normalizeLimit(opts.limit);
        String order = (opts.orderBy == null || opts.orderBy.isEmpty()) ? "rid ASC" : opts.orderBy;
        String where = (opts.filter == null || opts.filter.isEmpty()) ? "" : " WHERE " + opts.filter;
        String sql = "SELECT * FROM " + Sql.sqlIdentifierPath(collection) + where
            + " ORDER BY " + order + " LIMIT " + limit;
        byte[] body = q.query(sql);
        return new Envelopes.ListResult(Sql.allRows(body));
    }

    /** Top-level patch one document. JSON-pointer paths rejected. */
    public Map<String, Object> patch(String collection, String rid, Map<String, Object> patch) {
        if (patch == null || patch.isEmpty()) {
            throw new HelperException.InvalidArgument(
                "documents.patch patch must be a non-empty object");
        }
        List<String> parts = new ArrayList<>(patch.size());
        for (Map.Entry<String, Object> e : patch.entrySet()) {
            String field = e.getKey();
            if (field.contains("/")) {
                throw new HelperException.InvalidArgument(
                    "documents.patch currently accepts top-level document fields");
            }
            parts.add(Sql.sqlIdentifier(field) + " = " + Sql.valueLiteral(e.getValue()));
        }
        String sql = "UPDATE " + Sql.sqlIdentifierPath(collection)
            + " DOCUMENTS SET " + String.join(", ", parts) + " WHERE rid = $1 RETURNING *";
        byte[] body = q.query(sql, rid);
        Object[] fr = Sql.firstRow(body);
        @SuppressWarnings("unchecked")
        Map<String, Object> row = (Map<String, Object>) fr[0];
        if (row == null) {
            throw new HelperException.NotFound("document \"" + rid + "\" was not found");
        }
        return row;
    }

    /** Remove a document by rid. */
    public Envelopes.DeleteResult delete(String collection, String rid) {
        String sql = "DELETE FROM " + Sql.sqlIdentifierPath(collection) + " WHERE rid = $1";
        byte[] body = q.query(sql, rid);
        return new Envelopes.DeleteResult(Sql.affectedFromBody(body));
    }

    private void ensureCollection(String collection) {
        try {
            q.query("CREATE DOCUMENT " + Sql.sqlIdentifierPath(collection));
        } catch (RuntimeException e) {
            String msg = e.getMessage();
            if (msg != null && msg.contains("already exists")) return;
            throw e;
        }
    }
}
