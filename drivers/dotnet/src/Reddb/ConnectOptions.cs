using System;

namespace Reddb;

/// <summary>
/// Optional connection knobs. Anything left null falls back to the
/// matching field in the URI (or the documented default).
/// </summary>
public sealed class ConnectOptions
{
    /// <summary>Reusable empty defaults. Treat as immutable.</summary>
    public static readonly ConnectOptions Defaults = new();

    /// <summary>Username for SCRAM / login. Overrides the URI's userinfo.</summary>
    public string? Username { get; init; }

    /// <summary>Password for SCRAM / login. Overrides the URI's userinfo.</summary>
    public string? Password { get; init; }

    /// <summary>Pre-acquired bearer token. Skips auto-login.</summary>
    public string? Token { get; init; }

    /// <summary>API key (sent as a header on HTTP transports that support it).</summary>
    public string? ApiKey { get; init; }

    /// <summary>Sent in the Hello frame so server logs can identify the driver.</summary>
    public string? ClientName { get; init; }

    /// <summary>Per-call timeout. Defaults to 30 seconds.</summary>
    public TimeSpan Timeout { get; init; } = TimeSpan.FromSeconds(30);

    /// <summary>Connect-phase timeout. Defaults to <see cref="Timeout"/>.</summary>
    public TimeSpan? ConnectTimeout { get; init; }
}
