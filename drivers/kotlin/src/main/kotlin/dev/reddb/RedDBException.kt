package dev.reddb

/**
 * Base type for every error the driver surfaces. Mirrors the
 * sealed hierarchy in the JS / Rust / Java drivers so callers
 * can `catch RedDBException.AuthRefused` etc. without sniffing
 * strings.
 */
public sealed class RedDBException(message: String, cause: Throwable? = null) : RuntimeException(message, cause) {

    /** Server refused the auth handshake (anonymous blocked, bad token, bad SCRAM proof, ...). */
    public class AuthRefused(message: String) : RedDBException(message)

    /** Wire-level error: malformed frame, unexpected message kind, JSON decode failure. */
    public open class ProtocolError(message: String, cause: Throwable? = null) : RedDBException(message, cause)

    /** Server returned an `Error` frame / HTTP 4xx-5xx with an engine-side reason. */
    public class EngineError(message: String) : RedDBException(message)

    /** Frame length out of range (negative, < 16, or > 16 MiB). */
    public class FrameTooLarge(message: String) : ProtocolError(message)

    /** Peer set a flag bit we don't recognise — bail out per the spec. */
    public class UnknownFlags(message: String) : ProtocolError(message)

    /** Inbound frame had COMPRESSED set but zstd-jni isn't available. */
    public class CompressedButNoZstd(message: String, cause: Throwable? = null) : ProtocolError(message, cause)
}
