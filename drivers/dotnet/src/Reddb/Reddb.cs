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
        string protocol = parsed.Scheme switch {
            RedUrl.Kind.Redwire => "red", RedUrl.Kind.RedwireTls => "reds",
            RedUrl.Kind.Http => "http", RedUrl.Kind.Https => "https",
            _ => throw new ArgumentException("unsupported remote transport"),
        };
        string host = parsed.Host!.Contains(':') ? $"[{parsed.Host}]" : parsed.Host;
        string seed = TopologyCodec.Address($"{protocol}://{host}:{parsed.Port}{parsed.Path}", protocol)!;
        var original = new Uri(parsed.Original);
        async ValueTask<IConn> Dial(string address, CancellationToken token) {
            var target = new UriBuilder(address) {
                UserName = parsed.Username ?? "", Password = parsed.Password ?? "",
                Query = original.Query.TrimStart('?'),
            };
            return await ConnectDirectAsync(RedUrl.Parse(target.Uri.AbsoluteUri), opts, token);
        }
        var router = new RoutedConn(seed, await ConnectDirectAsync(parsed, opts, cancellationToken),
            protocol, parsed.Params.TryGetValue("route", out string? route) && route.Equals("primary", StringComparison.OrdinalIgnoreCase), Dial);
        try { await router.InitializeAsync(cancellationToken); return router; }
        catch { await router.DisposeAsync(); throw; }
    }

    private static async ValueTask<IConn> ConnectDirectAsync(RedUrl parsed, ConnectOptions opts, CancellationToken cancellationToken)
    {
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
