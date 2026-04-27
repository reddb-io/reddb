package dev.reddb;

/**
 * Base type for every error the driver surfaces. Subclasses carry
 * the same hierarchy the JS / Rust drivers use so user code can
 * `catch RedDBException.AuthRefused` etc. without sniffing strings.
 */
public class RedDBException extends RuntimeException {
    public RedDBException(String message) { super(message); }
    public RedDBException(String message, Throwable cause) { super(message, cause); }

    /** Server refused the auth handshake (anonymous blocked, bad token, bad SCRAM proof, ...). */
    public static class AuthRefused extends RedDBException {
        public AuthRefused(String message) { super(message); }
    }

    /** Wire-level error: malformed frame, unexpected message kind, JSON decode failure. */
    public static class ProtocolError extends RedDBException {
        public ProtocolError(String message) { super(message); }
        public ProtocolError(String message, Throwable cause) { super(message, cause); }
    }

    /** Server returned an `Error` frame / HTTP 4xx-5xx with an engine-side reason. */
    public static class EngineError extends RedDBException {
        public EngineError(String message) { super(message); }
    }

    /** Frame length out of range (negative, < 16, or > 16 MiB). */
    public static class FrameTooLarge extends ProtocolError {
        public FrameTooLarge(String message) { super(message); }
    }

    /** Peer set a flag bit we don't recognise — bail out per the spec. */
    public static class UnknownFlags extends ProtocolError {
        public UnknownFlags(String message) { super(message); }
    }

    /** Inbound frame had COMPRESSED set but zstd-jni isn't on the classpath / failed to init. */
    public static class CompressedButNoZstd extends ProtocolError {
        public CompressedButNoZstd(String message) { super(message); }
        public CompressedButNoZstd(String message, Throwable cause) { super(message, cause); }
    }
}
