package dev.reddb.helpers;

/**
 * Implements {@code tx.*} from the SDK Helper Spec. Supports both
 * imperative ({@link #begin}/{@link #commit}/{@link #rollback}) and
 * callback ({@link #run}) forms. Nested {@code run} calls reject with
 * {@code INVALID_ARGUMENT} — callers needing savepoints issue
 * {@code SAVEPOINT}/{@code RELEASE} via the underlying connection.
 */
public final class TxClient {
    private final Querier q;
    private boolean inCallback;

    TxClient(Querier q) { this.q = q; }

    /** Open a transaction. */
    public void begin() {
        q.query("BEGIN");
    }

    /** Commit the current transaction. */
    public void commit() {
        q.query("COMMIT");
    }

    /** Roll back the current transaction. */
    public void rollback() {
        q.query("ROLLBACK");
    }

    @FunctionalInterface
    public interface TxBody {
        void apply(TxClient tx) throws Exception;
    }

    /**
     * Run {@code body} inside a transaction. The body's return path
     * commits; any thrown exception rolls back and is re-thrown wrapped
     * in {@link RuntimeException} if checked.
     */
    public void run(TxBody body) {
        if (inCallback) {
            throw new HelperException.InvalidArgument(
                "tx.run does not support nesting; use SAVEPOINT via conn.query directly");
        }
        inCallback = true;
        try {
            begin();
            try {
                body.apply(this);
            } catch (RuntimeException re) {
                safeRollback();
                throw re;
            } catch (Exception checked) {
                safeRollback();
                throw new RuntimeException(checked);
            }
            commit();
        } finally {
            inCallback = false;
        }
    }

    private void safeRollback() {
        try { rollback(); } catch (RuntimeException ignored) { /* best effort */ }
    }
}
