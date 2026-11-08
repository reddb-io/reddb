using System;
using System.Threading;
using System.Threading.Tasks;

namespace Reddb;

/// <summary>
/// Top-level entry point. <see cref="ConnectAsync(Uri, ConnectOptions, CancellationToken)"/>
/// returns an <see cref="IConn"/> backed by whichever transport the URL selected.
///
/// Embedded URLs load the shared RedDB native ABI in-process; remote URLs
/// retain the existing RedWire and HTTP transports.
/// </summary>
public static class Reddb
{
    /// <summary>Convenience: parse the URI string and connect with defaults.</summary>
    public static ValueTask<IConn> ConnectAsync(string uri, CancellationToken cancellationToken = default)
        => ConnectAsync(RedUrl.Parse(uri), ConnectOptions.Defaults, cancellationToken);

    /// <summary>Convenience overload taking options.</summary>
    public static ValueTask<IConn> ConnectAsync(string uri, ConnectOptions opts, CancellationToken cancellationToken = default)
        => ConnectAsync(RedUrl.Parse(uri), opts, cancellationToken);

    /// <summary>
    /// Open a connection. Throws <see cref="ArgumentException"/> for
    /// unsupported URIs.
    /// </summary>
    public static async ValueTask<IConn> ConnectAsync(Uri uri, ConnectOptions opts, CancellationToken cancellationToken = default)
    {
        if (uri is null) throw new ArgumentNullException(nameof(uri));
        RedUrl parsed = RedUrl.Parse(uri.ToString());
        opts ??= ConnectOptions.Defaults;

        return await ConnectAsync(parsed, opts, cancellationToken).ConfigureAwait(false);
    }

    private static async ValueTask<IConn> ConnectAsync(
        RedUrl parsed,
        ConnectOptions opts,
        CancellationToken cancellationToken)
    {
        if (parsed.IsEmbedded) return Embedded.EmbeddedConn.Open(parsed);
        return parsed.Scheme switch
        {
            RedUrl.Kind.Redwire or RedUrl.Kind.RedwireTls
                => await Redwire.RedWireConn.ConnectAsync(parsed, opts, cancellationToken).ConfigureAwait(false),
            RedUrl.Kind.Http or RedUrl.Kind.Https
                => await Http.HttpConn.ConnectAsync(parsed, opts, cancellationToken).ConfigureAwait(false),
            _ => throw new ArgumentException($"unhandled URL scheme: {parsed.Scheme}", nameof(parsed)),
        };
    }
}
