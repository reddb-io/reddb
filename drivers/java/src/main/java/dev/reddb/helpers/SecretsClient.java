package dev.reddb.helpers;

import java.util.Map;

/** Vault helper bound to one explicit collection. */
public final class SecretsClient {
    private final Querier q;
    private final String collection;

    SecretsClient(Querier q, String collection) {
        validatePart(collection, "vault collection");
        this.q = q;
        this.collection = collection;
    }

    public String collection() { return collection; }

    public void put(String key, Object value) {
        q.query("VAULT PUT " + path(key) + " = " + Sql.kvValueLiteral(value));
    }

    public Map<String, Object> get(String key) { return read("GET", key); }
    public Map<String, Object> reveal(String key) { return read("REVEAL", key); }

    @SuppressWarnings("unchecked")
    private Map<String, Object> read(String verb, String key) {
        byte[] body = q.query("VAULT " + verb + " " + path(key));
        return (Map<String, Object>) Sql.firstRow(body)[0];
    }

    private String path(String key) {
        validatePart(key, "vault key");
        return collection + "." + key;
    }

    private static void validatePart(String value, String label) {
        if (value == null || !value.matches("^[A-Za-z0-9_.]+$")) {
            throw new HelperException.InvalidArgument("invalid " + label);
        }
    }
}
