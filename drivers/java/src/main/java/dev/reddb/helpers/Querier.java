package dev.reddb.helpers;

/**
 * Minimal contract helpers need. {@link dev.reddb.Conn} satisfies it via
 * {@code query(String, Object...)}; tests pass fakes that record SQL.
 */
@FunctionalInterface
public interface Querier {
    /** Run a SQL query with positional {@code $N} parameters. Returns raw JSON envelope. */
    byte[] query(String sql, Object... params);
}
