import 'errors.dart';

/// Default ports per scheme. Mirrors `drivers/js/src/url.js` and the
/// python-asyncio driver.
const Map<String, int> _defaultPorts = {
  'red': 5050,
  'reds': 5050,
  'redwire': 5050,
  'redwire-tls': 5050,
  'http': 8080,
  'https': 8443,
};

/// Default port for a transport kind. Returns `5050` for unknown kinds
/// to mirror the JS / Python behaviour.
int defaultPortFor(String kind) => _defaultPorts[kind] ?? 5050;

/// Normalised view of a connection URI.
class ParsedUri {
  ParsedUri({
    required this.kind,
    required this.originalUri,
    this.host,
    this.port,
    this.path,
    this.username,
    this.password,
    this.token,
    this.auth,
    this.sslmode,
    this.timeoutMs,
    this.ca,
    this.cert,
    this.key,
    Map<String, String>? params,
  }) : params = params ?? const {};

  /// One of `redwire`, `redwire-tls`, `http`, `https`, `embedded`.
  final String kind;
  final String? host;
  final int? port;
  final String? path;
  final String? username;
  final String? password;
  final String? token;
  final String? auth;
  final String? sslmode;
  final int? timeoutMs;
  final String? ca;
  final String? cert;
  final String? key;
  final Map<String, String> params;
  final String originalUri;

  bool get isEmbedded => kind == 'embedded';
  bool get isHttp => kind == 'http' || kind == 'https';
  bool get isTls => kind == 'redwire-tls' || kind == 'https';
  bool get isRedwire => kind == 'redwire' || kind == 'redwire-tls';

  @override
  String toString() => 'ParsedUri(kind=$kind, host=$host, port=$port)';
}

/// Parse a connection URI string.
///
/// Throws [InvalidUri] for empty / malformed inputs and
/// [UnsupportedScheme] for schemes the driver doesn't speak.
ParsedUri parseUri(String uri) {
  if (uri.isEmpty) {
    throw InvalidUri('connection URI must be a non-empty string');
  }

  // Embedded shortcuts that `Uri.parse` does not represent cleanly.
  const embeddedShortcuts = {
    'red:',
    'red:/',
    'red://',
    'red://memory',
    'red://memory/',
    'red://:memory',
    'red://:memory:',
  };
  if (embeddedShortcuts.contains(uri)) {
    return ParsedUri(kind: 'embedded', originalUri: uri);
  }
  if (uri.startsWith('red:///')) {
    return ParsedUri(
      kind: 'embedded',
      path: uri.substring('red://'.length),
      originalUri: uri,
    );
  }

  Uri parsed;
  try {
    parsed = Uri.parse(uri);
  } on FormatException catch (e) {
    throw InvalidUri("failed to parse '$uri': ${e.message}");
  }

  final scheme = parsed.scheme.toLowerCase();
  const allowed = {'red', 'reds', 'http', 'https'};
  if (!allowed.contains(scheme)) {
    throw UnsupportedScheme(
      "unsupported scheme '$scheme'. Use red://, reds://, http://, https://.",
    );
  }

  final flat = <String, String>{};
  parsed.queryParametersAll.forEach((k, v) {
    if (v.isNotEmpty) flat[k] = v.first;
  });

  // Embedded shortcut for `red://` with no host but with a path
  // already covered above (red:///...). Anything else needs a host.
  if ((parsed.host).isEmpty && scheme == 'red') {
    if (parsed.path.isNotEmpty && parsed.path != '/') {
      return ParsedUri(
        kind: 'embedded',
        path: parsed.path,
        originalUri: uri,
      );
    }
    return ParsedUri(kind: 'embedded', originalUri: uri);
  }

  String kind;
  if (scheme == 'http' || scheme == 'https') {
    kind = scheme;
  } else if (scheme == 'reds') {
    kind = 'redwire-tls';
  } else {
    final proto = (flat['proto'] ?? '').toLowerCase();
    if (proto.isNotEmpty) {
      kind = _kindFromProto(proto);
    } else if ((flat['sslmode'] ?? '').toLowerCase() == 'require') {
      kind = 'redwire-tls';
    } else {
      kind = 'redwire';
    }
  }

  final port = parsed.hasPort
      ? parsed.port
      : defaultPortFor(_schemeForDefault(kind));

  String? username;
  String? password;
  if (parsed.userInfo.isNotEmpty) {
    final ui = parsed.userInfo;
    final colon = ui.indexOf(':');
    if (colon < 0) {
      username = Uri.decodeComponent(ui);
    } else {
      username = Uri.decodeComponent(ui.substring(0, colon));
      password = Uri.decodeComponent(ui.substring(colon + 1));
    }
  }

  int? timeoutMs;
  final timeoutRaw = flat['timeout_ms'];
  if (timeoutRaw != null) {
    final parsedInt = int.tryParse(timeoutRaw);
    if (parsedInt == null) {
      throw InvalidUri(
        "timeout_ms must be an integer, got '$timeoutRaw'",
      );
    }
    timeoutMs = parsedInt;
  }

  String? authChoice = flat['auth'];
  if (authChoice != null) {
    authChoice = authChoice.toLowerCase();
    const supported = {'bearer', 'scram', 'oauth', 'anonymous'};
    if (!supported.contains(authChoice)) {
      throw InvalidUri(
        "auth must be one of bearer/scram/oauth/anonymous, got '$authChoice'",
      );
    }
  }

  return ParsedUri(
    kind: kind,
    host: parsed.host,
    port: port,
    path: (parsed.path.isEmpty || parsed.path == '/') ? null : parsed.path,
    username: username,
    password: password,
    token: flat['token'],
    auth: authChoice,
    sslmode: flat['sslmode'],
    timeoutMs: timeoutMs,
    ca: flat['ca'],
    cert: flat['cert'],
    key: flat['key'],
    params: flat,
    originalUri: uri,
  );
}

String _kindFromProto(String proto) {
  switch (proto) {
    case 'red':
    case 'redwire':
    case 'grpc':
      return 'redwire';
    case 'reds':
    case 'redwires':
    case 'grpcs':
      return 'redwire-tls';
    case 'http':
      return 'http';
    case 'https':
      return 'https';
    default:
      throw UnsupportedScheme(
        "unknown proto='$proto'. Supported: red | reds | http | https",
      );
  }
}

String _schemeForDefault(String kind) {
  switch (kind) {
    case 'redwire':
      return 'red';
    case 'redwire-tls':
      return 'reds';
    default:
      return kind;
  }
}

/// Derive the HTTP login URL (`/auth/login`) from a parsed URI. Used
/// by the auto-login flow when the user supplies `username:password@`
/// but not an explicit `loginUrl`.
String deriveLoginUrl(ParsedUri parsed) {
  final loginUrl = parsed.params['loginUrl'] ?? parsed.params['login_url'];
  if (loginUrl != null && loginUrl.isNotEmpty) return loginUrl;
  if (parsed.host == null) {
    throw RedDBError(
      'AUTH_LOGIN_NEEDS_HOST',
      'cannot derive loginUrl without a host; pass it explicitly via loginUrl=...',
    );
  }
  if (parsed.kind == 'http' || parsed.kind == 'https') {
    final scheme = parsed.kind;
    final port = parsed.port ?? defaultPortFor(parsed.kind);
    return '$scheme://${parsed.host}:$port/auth/login';
  }
  return 'https://${parsed.host}/auth/login';
}
