package dev.reddb.helpers;

import java.util.List;
import java.util.Map;

/**
 * Spec envelopes returned by helper methods. Records keep call sites
 * boring — destructure with accessors, no mutation expected.
 */
public final class Envelopes {
    private Envelopes() {}

    public record InsertResult(long affected, String rid, Map<String, Object> item) {}

    public record DeleteResult(long affected, boolean deleted) {
        public DeleteResult(long affected) { this(affected, affected > 0L); }
    }

    public record ExistsResult(boolean exists) {}

    public record ListResult(List<Map<String, Object>> items, String nextCursor) {
        public ListResult(List<Map<String, Object>> items) { this(items, null); }
    }

    public record QueuePushResult(long affected, String rid) {}
}
