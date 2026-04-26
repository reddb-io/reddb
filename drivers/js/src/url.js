/**
 * `red://` connection-string parser.
 *
 * One URL covers every transport RedDB speaks:
 *
 *   red://                                          embedded in-memory
 *   red:///abs/path/data.rdb                        embedded persistent
 *   red://user:pass@host:5051                       remote, default proto=grpc
 *   red://host:8080?proto=https                     remote HTTPS
 *   red://host:5432?proto=pg                        PostgreSQL wire
 *   red://host:5051?proto=grpc&token=sk-abc         remote gRPC w/ bearer
 *   red://host:8080?proto=https&apiKey=ak-xyz       remote HTTPS w/ api key
 *
 * Backwards-compat: legacy `memory://`, `file://`, `grpc://` URLs
 * still work via `parseLegacyUrl`. New code should prefer `red://`
 * because it carries auth + protocol selection in one place.
 */

import { RedDBError } from './protocol.js'

/**
 * @typedef {object} ParsedUri
 * @property {'embedded' | 'http' | 'https' | 'grpc' | 'grpcs' | 'pg'} kind
 * @property {string} [host]
 * @property {number} [port]
 * @property {string} [path]            // for embedded `file://`-equivalent
 * @property {string} [username]
 * @property {string} [password]
 * @property {string} [token]
 * @property {string} [apiKey]
 * @property {string} [loginUrl]        // explicit override for login flow
 * @property {URLSearchParams} [params] // remaining query params
 * @property {string} originalUri
 */

/**
 * Parse any URI string into a normalised `ParsedUri`.
 * Accepts `red://`, `memory://`, `file://`, `grpc://` (the latter
 * three for backwards compat).
 *
 * @param {string} uri
 * @returns {ParsedUri}
 */
export function parseUri(uri) {
  if (typeof uri !== 'string' || uri.length === 0) {
    throw new TypeError(
      "connect() requires a URI string (e.g. 'red://localhost:5051' or 'red:///data.rdb')",
    )
  }
  if (uri.startsWith('red://') || uri === 'red:' || uri === 'red:/') {
    return parseRedUrl(uri)
  }
  return parseLegacyUrl(uri)
}

/**
 * Parse a `red://` URL.
 *
 * Authority shape: `[user[:pass]@]host[:port]`
 * Path: optional, used as filesystem path when `host` is absent or
 * is the special token `localhost-embedded` (rare).
 * Query: `proto`, `token`, `apiKey`, `loginUrl`.
 */
export function parseRedUrl(uri) {
  // The host might be missing (`red:///path`), the URL constructor
  // requires *something* there. Re-write to a parse-friendly shape:
  // - `red:///x`     → `red://embedded.local/x`   (embedded with path)
  // - `red://memory` → `red://embedded.local`     (embedded in-memory)
  // - `red://`       → `red://embedded.local`     (embedded in-memory)
  let normalised = uri
  if (uri === 'red:' || uri === 'red:/' || uri === 'red://') {
    normalised = 'red://embedded.local'
  } else if (uri.startsWith('red:///')) {
    normalised = `red://embedded.local${uri.slice('red://'.length)}`
  } else if (
    uri === 'red://memory'
    || uri === 'red://memory/'
    || uri === 'red://:memory'
    || uri === 'red://:memory:'  // SQLite-style ":memory:" alias
  ) {
    normalised = 'red://embedded.local'
  }

  let parsed
  try {
    parsed = new URL(normalised)
  } catch (err) {
    throw new RedDBError('UNPARSEABLE_URI', `failed to parse '${uri}': ${err.message}`)
  }

  const params = parsed.searchParams
  const proto = (params.get('proto') || '').toLowerCase()
  const path = parsed.pathname && parsed.pathname !== '/' ? parsed.pathname : ''

  // Embedded: special host, OR `proto=embedded`, OR no proto + has path
  // and the user clearly meant a file path (red:///abs/path).
  if (parsed.hostname === 'embedded.local') {
    if (path) {
      return {
        kind: 'embedded',
        path,
        params,
        originalUri: uri,
      }
    }
    return {
      kind: 'embedded',
      params,
      originalUri: uri,
    }
  }

  // Remote — default proto is grpc.
  const kind = resolveKind(proto)
  const port = parsed.port ? Number(parsed.port) : defaultPortFor(kind)
  const username = parsed.username ? decodeURIComponent(parsed.username) : undefined
  const password = parsed.password ? decodeURIComponent(parsed.password) : undefined

  return {
    kind,
    host: parsed.hostname,
    port,
    path: path || undefined,
    username,
    password,
    token: params.get('token') ?? undefined,
    apiKey: params.get('apiKey') ?? params.get('api_key') ?? undefined,
    loginUrl: params.get('loginUrl') ?? params.get('login_url') ?? undefined,
    params,
    originalUri: uri,
  }
}

/**
 * Backwards-compat parser for the legacy URL shapes the driver
 * accepted before `red://` existed. Returns the same `ParsedUri`
 * shape so downstream code is uniform.
 */
export function parseLegacyUrl(uri) {
  if (uri === 'memory://' || uri === 'memory:') {
    return { kind: 'embedded', originalUri: uri }
  }
  if (uri.startsWith('file://')) {
    const path = uri.slice('file://'.length)
    if (!path) {
      throw new TypeError(`invalid file:// URI: missing path in '${uri}'`)
    }
    return { kind: 'embedded', path, originalUri: uri }
  }
  if (
    uri.startsWith('grpc://')
    || uri.startsWith('grpcs://')
    || uri.startsWith('reds://')
  ) {
    const isTls = uri.startsWith('grpcs://') || uri.startsWith('reds://')
    const scheme = uri.split('://', 1)[0]
    const stripped = uri.slice(`${scheme}://`.length)
    const [hostPort] = stripped.split(/[/?]/, 1)
    const [host, portStr] = hostPort.split(':')
    if (!host) {
      throw new TypeError(`invalid ${scheme}:// URI: missing host in '${uri}'`)
    }
    return {
      kind: isTls ? 'grpcs' : 'grpc',
      host,
      port: portStr ? Number(portStr) : defaultPortFor(isTls ? 'grpcs' : 'grpc'),
      originalUri: uri,
    }
  }
  if (uri.startsWith('http://') || uri.startsWith('https://')) {
    let parsed
    try {
      parsed = new URL(uri)
    } catch (err) {
      throw new RedDBError('UNPARSEABLE_URI', `failed to parse '${uri}': ${err.message}`)
    }
    return {
      kind: parsed.protocol === 'https:' ? 'https' : 'http',
      host: parsed.hostname,
      port: parsed.port ? Number(parsed.port) : defaultPortFor(parsed.protocol === 'https:' ? 'https' : 'http'),
      path: parsed.pathname !== '/' ? parsed.pathname : undefined,
      username: parsed.username ? decodeURIComponent(parsed.username) : undefined,
      password: parsed.password ? decodeURIComponent(parsed.password) : undefined,
      token: parsed.searchParams.get('token') ?? undefined,
      apiKey: parsed.searchParams.get('apiKey') ?? undefined,
      params: parsed.searchParams,
      originalUri: uri,
    }
  }
  throw new RedDBError(
    'UNSUPPORTED_SCHEME',
    `unsupported URI: '${uri}'. Use 'red://...' or one of memory://, file://, grpc://, http(s)://`,
  )
}

function resolveKind(protoQueryParam) {
  switch (protoQueryParam) {
    case '':
    case 'grpc':
    case 'red':
      return 'grpc'
    case 'grpcs':
    case 'reds':
      return 'grpcs'
    case 'http':
      return 'http'
    case 'https':
      return 'https'
    case 'pg':
    case 'postgres':
    case 'postgresql':
      return 'pg'
    default:
      throw new RedDBError(
        'UNSUPPORTED_PROTO',
        `unknown proto='${protoQueryParam}'. Supported: red | reds | grpc | grpcs | http | https | pg`,
      )
  }
}

function defaultPortFor(kind) {
  switch (kind) {
    case 'http':
      return 8080
    case 'https':
      return 8443
    case 'grpc':
      return 5051
    case 'grpcs':
      return 5052
    case 'pg':
    case 'postgres':
    case 'postgresql':
      return 5432
    default:
      return undefined
  }
}

/**
 * Derive the HTTP login URL (`/auth/login`) from a parsed URI.
 * Used by the auto-login flow when the user supplies `username:password@`
 * but not an explicit `loginUrl`.
 *
 * Strategy: if proto is already http/https, just append `/auth/login`.
 * For grpc/grpcs/pg, default to https://host:443 — operators that
 * don't want that should pass `loginUrl=` explicitly.
 */
export function deriveLoginUrl(parsed) {
  if (parsed.loginUrl) return parsed.loginUrl
  if (!parsed.host) {
    throw new RedDBError(
      'AUTH_LOGIN_NEEDS_HOST',
      'cannot derive loginUrl without a host; pass it explicitly via loginUrl=...',
    )
  }
  if (parsed.kind === 'http' || parsed.kind === 'https') {
    const scheme = parsed.kind
    const port = parsed.port ?? defaultPortFor(parsed.kind)
    return `${scheme}://${parsed.host}:${port}/auth/login`
  }
  // Non-HTTP transports — default to HTTPS on 443 unless the user
  // tells us otherwise.
  return `https://${parsed.host}/auth/login`
}
