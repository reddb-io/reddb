using System;

namespace Reddb;

/// <summary>
/// Base type for every error the driver surfaces. Mirrors the
/// hierarchy used by the JS / Java / Rust drivers so callers can
/// catch <see cref="AuthRefused"/> etc. without sniffing strings.
/// </summary>
public class RedDBException : Exception
{
    public RedDBException(string message) : base(message) { }
    public RedDBException(string message, Exception inner) : base(message, inner) { }

    /// <summary>Server refused the auth handshake (anonymous blocked, bad token, bad SCRAM proof).</summary>
    public sealed class AuthRefused : RedDBException
    {
        public AuthRefused(string message) : base(message) { }
        public AuthRefused(string message, Exception inner) : base(message, inner) { }
    }

    /// <summary>Wire-level error: malformed frame, unexpected message kind, JSON decode failure.</summary>
    public class ProtocolError : RedDBException
    {
        public ProtocolError(string message) : base(message) { }
        public ProtocolError(string message, Exception inner) : base(message, inner) { }
    }

    /// <summary>Server returned an Error frame / HTTP 4xx-5xx with an engine-side reason.</summary>
    public sealed class EngineError : RedDBException
    {
        public EngineError(string message) : base(message) { }
        public EngineError(string message, Exception inner) : base(message, inner) { }
    }

    /// <summary>Frame length out of range (below the header size or above 16 MiB).</summary>
    public sealed class FrameTooLarge : ProtocolError
    {
        public FrameTooLarge(string message) : base(message) { }
    }

    /// <summary>Peer set a flag bit we don't recognise — bail out per the spec.</summary>
    public sealed class UnknownFlags : ProtocolError
    {
        public UnknownFlags(string message) : base(message) { }
    }

    /// <summary>Inbound frame had the COMPRESSED flag but no zstd codec is available.</summary>
    public sealed class CompressedButNoZstd : ProtocolError
    {
        public CompressedButNoZstd(string message) : base(message) { }
        public CompressedButNoZstd(string message, Exception inner) : base(message, inner) { }
    }
}
