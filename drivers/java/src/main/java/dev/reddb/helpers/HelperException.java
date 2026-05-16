package dev.reddb.helpers;

/**
 * Typed helper errors mirroring the Go/Python/Dart drivers. Subclasses
 * encode the spec error codes ({@code INVALID_ARGUMENT}, {@code NOT_FOUND},
 * {@code INVALID_RESPONSE}) so callers can match without sniffing strings.
 */
public class HelperException extends RuntimeException {
    public HelperException(String message) { super(message); }
    public HelperException(String message, Throwable cause) { super(message, cause); }

    /** {@code INVALID_ARGUMENT} — bad helper input. */
    public static class InvalidArgument extends HelperException {
        public InvalidArgument(String message) { super(message); }
    }

    /** {@code NOT_FOUND} — server replied empty for a lookup that required a row. */
    public static class NotFound extends HelperException {
        public NotFound(String message) { super(message); }
    }

    /** {@code INVALID_RESPONSE} — server envelope didn't match the spec shape. */
    public static class InvalidResponse extends HelperException {
        public InvalidResponse(String message) { super(message); }
    }
}
