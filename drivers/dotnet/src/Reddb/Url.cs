using System;
using System.Collections.Generic;
using System.Globalization;
using System.Net;

namespace Reddb;

/// <summary>
/// Connection-string parser for the .NET driver. Mirrors
/// <c>drivers/js/src/url.js</c> and the Java / Go drivers. One URI
/// covers every transport: <c>red://</c>, <c>reds://</c>,
/// <c>http://</c>, <c>https://</c>, plus the embedded shortcuts
/// (<c>red:</c>, <c>red://</c>, <c>red:///path</c>, <c>red://memory</c>).
///
/// Default port for every scheme is <see cref="DefaultPort"/> (5050) —
/// matches the engine's <c>DEFAULT_REDWIRE_PORT</c>.
/// </summary>
public sealed class RedUrl
{
    /// <summary>Default port used for every transport.</summary>
    public const int DefaultPort = 5050;

    public enum Kind { Redwire, RedwireTls, Http, Https, EmbeddedFile, EmbeddedMemory }

    public string Original { get; }
    public Kind Scheme { get; }
    public string? Host { get; }
    public int Port { get; }
    public string? Path { get; }
    public string? Username { get; }
    public string? Password { get; }
    public string? Token { get; }
    public string? ApiKey { get; }
    public IReadOnlyDictionary<string, string> Params { get; }

    /// <summary>True for <c>red://</c> and <c>reds://</c> (the binary protocol).</summary>
    public bool IsRedwire => Scheme is Kind.Redwire or Kind.RedwireTls;

    /// <summary>True for <c>reds://</c> or <c>https://</c>.</summary>
    public bool IsTls => Scheme is Kind.RedwireTls or Kind.Https;

    /// <summary>True for either embedded variant — the .NET driver doesn't ship the embedded engine.</summary>
    public bool IsEmbedded => Scheme is Kind.EmbeddedFile or Kind.EmbeddedMemory;

    private RedUrl(
        string original,
        Kind scheme,
        string? host,
        int port,
        string? path,
        string? username,
        string? password,
        string? token,
        string? apiKey,
        IReadOnlyDictionary<string, string> queryParams)
    {
        Original = original;
        Scheme = scheme;
        Host = host;
        Port = port;
        Path = path;
        Username = username;
        Password = password;
        Token = token;
        ApiKey = apiKey;
        Params = queryParams;
    }

    /// <summary>
    /// Parse any supported URI string. Throws <see cref="ArgumentException"/>
    /// for unsupported schemes / malformed inputs.
    /// </summary>
    public static RedUrl Parse(string uri)
    {
        if (string.IsNullOrEmpty(uri))
        {
            throw new ArgumentException(
                "connect requires a URI string (e.g. 'red://localhost:5050')",
                nameof(uri));
        }

        // Embedded shortcuts.
        if (uri == "red:" || uri == "red:/" || uri == "red://"
            || uri == "red://memory" || uri == "red://memory/"
            || uri == "red://:memory" || uri == "red://:memory:")
        {
            return new RedUrl(
                uri,
                Kind.EmbeddedMemory,
                host: null,
                port: DefaultPort,
                path: null,
                username: null,
                password: null,
                token: null,
                apiKey: null,
                queryParams: EmptyParams);
        }
        if (uri.StartsWith("red:///", StringComparison.Ordinal))
        {
            // Path keeps the leading '/' just like the Java driver.
            string p = uri.Substring("red://".Length);
            return new RedUrl(
                uri,
                Kind.EmbeddedFile,
                host: null,
                port: DefaultPort,
                path: p,
                username: null,
                password: null,
                token: null,
                apiKey: null,
                queryParams: EmptyParams);
        }

        string scheme = SchemeOf(uri);
        Kind kind = KindFromScheme(scheme)
            ?? throw new ArgumentException(
                $"unsupported URI scheme: '{scheme}' in '{uri}'. Supported: red, reds, http, https",
                nameof(uri));

        // Use System.Uri for parsing remote shapes. red:// / reds:// are
        // not registered, so map them to a parseable form ("rwire" /
        // "rwires") just for the parse pass; we keep the original string
        // in the Original field.
        string parseable = uri;
        if (scheme == "red") parseable = "rwire" + uri.Substring(3);
        else if (scheme == "reds") parseable = "rwires" + uri.Substring(4);

        Uri parsed;
        try
        {
            parsed = new Uri(parseable, UriKind.Absolute);
        }
        catch (UriFormatException ex)
        {
            throw new ArgumentException(
                $"failed to parse URI '{uri}': {ex.Message}",
                nameof(uri),
                ex);
        }

        string host = parsed.Host;
        if (string.IsNullOrEmpty(host))
        {
            throw new ArgumentException(
                $"URI is missing a host: '{uri}'",
                nameof(uri));
        }
        // System.Uri fills in scheme defaults (80 for http, 443 for https).
        // We want our own DefaultPort whenever the user didn't write one.
        int port = HasExplicitPort(uri, host) ? parsed.Port : DefaultPort;

        string? username = null;
        string? password = null;
        string userInfo = parsed.UserInfo ?? string.Empty;
        if (userInfo.Length > 0)
        {
            int colon = userInfo.IndexOf(':');
            if (colon >= 0)
            {
                username = WebUtility.UrlDecode(userInfo.Substring(0, colon));
                password = WebUtility.UrlDecode(userInfo.Substring(colon + 1));
            }
            else
            {
                username = WebUtility.UrlDecode(userInfo);
            }
        }

        var queryParams = ParseQuery(parsed.Query);
        queryParams.TryGetValue("token", out string? token);
        string? apiKey = null;
        if (queryParams.TryGetValue("apiKey", out string? a)) apiKey = a;
        else if (queryParams.TryGetValue("api_key", out string? b)) apiKey = b;

        string? path = parsed.AbsolutePath;
        if (string.IsNullOrEmpty(path) || path == "/") path = null;

        return new RedUrl(
            uri,
            kind,
            host,
            port,
            path,
            username,
            password,
            token,
            apiKey,
            queryParams);
    }

    private static readonly IReadOnlyDictionary<string, string> EmptyParams =
        new Dictionary<string, string>(0);

    /// <summary>
    /// Detect whether the original URI string carries an explicit
    /// <c>:port</c>. We can't trust <c>System.Uri.IsDefaultPort</c>
    /// because it returns true for an explicit <c>:80</c> on http.
    /// </summary>
    private static bool HasExplicitPort(string uri, string host)
    {
        // Find the authority slice between '://' and the first
        // '/' / '?' / '#'. The host inside may or may not be wrapped
        // in [] for IPv6 literals.
        int idx = uri.IndexOf("://", StringComparison.Ordinal);
        if (idx < 0) return false;
        int start = idx + 3;
        int end = uri.Length;
        for (int i = start; i < uri.Length; i++)
        {
            char c = uri[i];
            if (c == '/' || c == '?' || c == '#') { end = i; break; }
        }
        string authority = uri.Substring(start, end - start);
        // Strip userinfo prefix ("user:pass@").
        int at = authority.LastIndexOf('@');
        if (at >= 0) authority = authority.Substring(at + 1);
        // IPv6 literal: [::1]:5050 — port is after the closing ']'.
        if (authority.StartsWith('['))
        {
            int rb = authority.IndexOf(']');
            if (rb < 0) return false;
            int portColon = authority.IndexOf(':', rb);
            return portColon > 0 && portColon < authority.Length - 1;
        }
        int lastColon = authority.LastIndexOf(':');
        return lastColon > 0 && lastColon < authority.Length - 1;
    }

    private static string SchemeOf(string uri)
    {
        int colon = uri.IndexOf(':');
        if (colon <= 0)
        {
            throw new ArgumentException(
                $"URI missing scheme: '{uri}'",
                nameof(uri));
        }
        return uri.Substring(0, colon).ToLower(CultureInfo.InvariantCulture);
    }

    private static Kind? KindFromScheme(string scheme) => scheme switch
    {
        "red" => Kind.Redwire,
        "reds" => Kind.RedwireTls,
        "http" => Kind.Http,
        "https" => Kind.Https,
        _ => null,
    };

    private static Dictionary<string, string> ParseQuery(string raw)
    {
        var result = new Dictionary<string, string>(StringComparer.Ordinal);
        if (string.IsNullOrEmpty(raw)) return result;
        if (raw[0] == '?') raw = raw.Substring(1);
        if (raw.Length == 0) return result;
        foreach (string pair in raw.Split('&'))
        {
            if (pair.Length == 0) continue;
            int eq = pair.IndexOf('=');
            string k, v;
            if (eq < 0) { k = pair; v = string.Empty; }
            else { k = pair.Substring(0, eq); v = pair.Substring(eq + 1); }
            result[WebUtility.UrlDecode(k)] = WebUtility.UrlDecode(v);
        }
        return result;
    }
}
