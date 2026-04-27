using System;
using System.Threading;
using System.Threading.Tasks;

namespace Reddb;

/// <summary>
/// Top-level entry point. <see cref="ConnectAsync(Uri, ConnectOptions, CancellationToken)"/>
/// returns an <see cref="IConn"/> backed by whichever transport the URL selected.
///
/// Embedded URLs (<c>red:</c>, <c>red://</c>, <c>red://memory</c>,
/// <c>red:///path</c>) throw <see cref="NotSupportedException"/> — the
/// .NET driver doesn't ship the embedded engine; once a native binding
/// lands, this factory will pick it up via the same dispatch.
/// </summary>
public static class Reddb
{
    /// <summary>Convenience: parse the URI string and connect with defaults.</summary>
    public static ValueTask<IConn> ConnectAsync(string uri, CancellationToken cancellationToken = default)
        => ConnectAsync(new Uri(uri, UriKind.Absolute), ConnectOptions.Defaults, cancellationToken);

    /// <summary>Convenience overload taking options.</summary>
    public static ValueTask<IConn> ConnectAsync(string uri, ConnectOptions opts, CancellationToken cancellationToken = default)
        => ConnectAsync(new Uri(uri, UriKind.Absolute), opts, cancellationToken);

    /// <summary>
    /// Open a connection. Throws <see cref="ArgumentException"/> for
    /// unsupported URIs and <see cref="NotSupportedException"/> for
    /// the embedded shapes that aren't implemented yet.
    /// </summary>
    public static async ValueTask<IConn> ConnectAsync(Uri uri, ConnectOptions opts, CancellationToken cancellationToken = default)
    {
        if (uri is null) throw new ArgumentNullException(nameof(uri));
        RedUrl parsed = RedUrl.Parse(uri.ToString());
        opts ??= ConnectOptions.Defaults;

        if (parsed.IsEmbedded)
        {
            throw new NotSupportedException(
                $"embedded RedDB ({parsed.Original}) needs the native lib — not yet shipped in the .NET driver");
        }
        return parsed.Scheme switch
        {
            RedUrl.Kind.Redwire or RedUrl.Kind.RedwireTls
                => await Redwire.RedWireConn.ConnectAsync(parsed, opts, cancellationToken).ConfigureAwait(false),
            RedUrl.Kind.Http or RedUrl.Kind.Https
                => await Http.HttpConn.ConnectAsync(parsed, opts, cancellationToken).ConfigureAwait(false),
            _ => throw new ArgumentException($"unhandled URL scheme: {parsed.Scheme}", nameof(uri)),
        };
    }
}
