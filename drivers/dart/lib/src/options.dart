import 'dart:io' show SecurityContext;

/// Top-level options for `connect()`. Most users won't need to fill this
/// out — the URI carries the same information.
class ConnectOptions {
  const ConnectOptions({
    this.clientName = 'reddb-dart/0.1.0',
    this.timeout = const Duration(seconds: 30),
    this.token,
    this.username,
    this.password,
    this.tls,
  });

  final String clientName;
  final Duration timeout;

  /// Static bearer token. Wins over `username`/`password` (no auto-login).
  final String? token;

  /// HTTP basic-style credentials. When set the HTTP transport calls
  /// `/auth/login` on connect to exchange them for a bearer token.
  final String? username;
  final String? password;

  /// Optional TLS configuration. When supplied for a `red://` URI the
  /// driver promotes the connection to TLS; for `reds://` it is merged
  /// with the URI's defaults.
  final TlsOptions? tls;

  ConnectOptions copyWith({
    String? clientName,
    Duration? timeout,
    String? token,
    String? username,
    String? password,
    TlsOptions? tls,
  }) {
    return ConnectOptions(
      clientName: clientName ?? this.clientName,
      timeout: timeout ?? this.timeout,
      token: token ?? this.token,
      username: username ?? this.username,
      password: password ?? this.password,
      tls: tls ?? this.tls,
    );
  }
}

/// TLS configuration for the RedWire transport.
class TlsOptions {
  const TlsOptions({
    this.context,
    this.servername,
    this.allowInsecure = false,
    this.alpnProtocols = const ['redwire/1'],
  });

  /// Override the default trust store. `null` uses the platform default.
  final SecurityContext? context;

  /// SNI override. Defaults to the connect host.
  final String? servername;

  /// When true, accept self-signed / mismatched certs. Dev-only.
  final bool allowInsecure;

  /// ALPN tokens offered to the server. Default: `['redwire/1']`.
  final List<String> alpnProtocols;
}
