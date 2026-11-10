package dev.reddb;

import java.time.Duration;
import java.util.Objects;

/** Explicit read consistency; arbitrary query/execute calls stay on the writer. */
public record ReadOptions(Consistency consistency, Duration maxLag) {
    public enum Consistency { STRONG, LOCAL, BOUNDED_STALENESS }
    public ReadOptions {
        Objects.requireNonNull(consistency, "CONSISTENCY_UNSUPPORTED");
        if (maxLag == null || maxLag.isNegative()) throw new IllegalArgumentException("CONSISTENCY_UNSUPPORTED");
    }
    public static ReadOptions local() { return new ReadOptions(Consistency.LOCAL, Duration.ZERO); }
    public static ReadOptions strong() { return new ReadOptions(Consistency.STRONG, Duration.ZERO); }
    public static ReadOptions boundedStaleness(Duration maxLag) { return new ReadOptions(Consistency.BOUNDED_STALENESS, maxLag); }
}
