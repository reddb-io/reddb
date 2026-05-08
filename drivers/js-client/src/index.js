/**
 * @reddb-io/client — thin remote-only RedDB driver.
 *
 * Public API:
 *   import { connect } from '@reddb-io/client'
 *   const db = await connect('red://reddb.example.com:5050')
 *   const result = await db.query('SELECT * FROM users LIMIT 10')
 *   await db.close()
 *
 * Accepted URIs:
 *   - 'red://host:port'        — RedWire TCP (default)
 *   - 'reds://host:port'       — RedWire over TLS
 *   - 'grpc://host:port'       — gRPC
 *   - 'grpcs://host:port'      — gRPC over TLS
 *   - 'http://host:port'       — HTTP JSON-RPC
 *   - 'https://host:port'      — HTTPS JSON-RPC
 *
 * Rejected URIs (use @reddb-io/sdk for these):
 *   - 'memory://', 'memory:'   — in-memory embedded engine
 *   - 'file:///abs/path'       — file-backed embedded engine
 *   - 'red://', 'red:///path'  — same shapes via the unified scheme
 *   - 'red://:memory[:]'       — SQLite-style embedded alias
 *
 * The thin `red_client` binary does not bundle the storage engine —
 * the package is roughly 10x smaller than `@reddb-io/sdk`. If you
 * need an embedded engine, install `@reddb-io/sdk` instead.
 */

import { RedDBError } from './protocol.js'
import { HttpRpcClient } from './http.js'
import { connectRedwire } from './redwire.js'
import { parseUri, deriveLoginUrl } from './url.js'
import {
  EmbeddedNotSupported,
  EMBEDDED_REJECTION_MESSAGE,
  isEmbeddedUri,
  rejectEmbeddedUri,
} from './embedded-rejection.js'
import { CacheClient } from './cache.js'
import { KvClient } from './kv.js'

export { RedDBError, EmbeddedNotSupported, EMBEDDED_REJECTION_MESSAGE, isEmbeddedUri }
export { CacheClient } from './cache.js'
export { KvClient } from './kv.js'
export { parseUri, deriveLoginUrl } from './url.js'

/**
 * Connect to a remote RedDB instance.
 *
 * @param {string} uri Connection URI. See module docstring for accepted schemes.
 * @param {object} [options]
 * @param {object} [options.auth] Authentication credentials.
 * @param {string} [options.auth.token] Bearer / API-key token.
 * @param {string} [options.auth.apiKey] Alias for `token`.
 * @param {string} [options.auth.username] Username for password login.
 * @param {string} [options.auth.password] Password for password login.
 * @param {string} [options.auth.loginUrl] Override URL for the password
 *   exchange (defaults to deriving `/auth/login` from `uri`).
 * @param {object} [options.tls] TLS / mTLS options for redwire(s)://.
 * @returns {Promise<RedDB>}
 */
export async function connect(uri, options = {}) {
  // Reject embedded shapes upfront with the same wording the Rust
  // binary uses, before the URL parser would map them to kind=embedded.
  rejectEmbeddedUri(uri)

  const parsed = parseUri(uri)

  // Belt-and-braces: if the parser still produced an embedded kind
  // (e.g. via a URI shape we forgot to enumerate above), reject it.
  if (parsed.kind === 'embedded') {
    throw new EmbeddedNotSupported(uri)
  }

  const merged = mergeAuthFromUri(parsed, options.auth)

  if (parsed.kind === 'http' || parsed.kind === 'https') {
    const baseUrl = `${parsed.kind}://${parsed.host}:${parsed.port}`
    let token = merged.token
    if (!token && merged.username && merged.password) {
      const loginUrl = merged.loginUrl ?? `${baseUrl}/auth/login`
      const session = await login(loginUrl, {
        username: merged.username,
        password: merged.password,
      })
      token = session.token
    }
    const client = new HttpRpcClient({ baseUrl, token })
    await client.call('health', {})
    return new RedDB(client)
  }

  if (
    parsed.kind === 'red'
    || parsed.kind === 'reds'
    || parsed.kind === 'grpc'
    || parsed.kind === 'grpcs'
  ) {
    let token = merged.token
    if (!token && merged.username && merged.password) {
      const loginUrl = merged.loginUrl ?? deriveLoginUrl(parsed)
      const session = await login(loginUrl, {
        username: merged.username,
        password: merged.password,
      })
      token = session.token
    }
    const auth = token ? { kind: 'bearer', token } : { kind: 'anonymous' }
    const tls = buildTlsOpts(parsed, options.tls)
    const client = await connectRedwire({
      host: parsed.host,
      port: parsed.port,
      auth,
      ...(tls ? { tls } : {}),
    })
    return new RedDB(client)
  }

  if (parsed.kind === 'pg') {
    throw new RedDBError(
      'PG_TRANSPORT_NOT_WIRED',
      "PostgreSQL wire (proto=pg) requires a node-pg-style client; "
        + "the JS thin client doesn't bundle one. Use a separate `pg` "
        + 'package against the same host:port.',
    )
  }

  throw new RedDBError(
    'UNSUPPORTED_KIND',
    `internal: parsed kind '${parsed.kind}' has no transport`,
  )
}

/**
 * Resolve TLS options for a redwire(s) connection. Source order:
 *   1. caller-supplied `options.tls` object.
 *   2. `parsed.kind === 'reds' | 'grpcs'`.
 *   3. `?tls=true` URL param.
 *   4. `?ca=`, `?cert=`, `?key=`, `?servername=`, `?rejectUnauthorized=false`
 *      URL params (path or PEM string).
 */
function buildTlsOpts(parsed, callerTls) {
  if (callerTls && typeof callerTls === 'object') {
    return callerTls
  }
  const params = parsed.params
  const wantsTls =
    parsed.kind === 'reds'
    || parsed.kind === 'grpcs'
    || params?.get?.('tls') === 'true'
    || params?.get?.('tls') === '1'
  if (!wantsTls) return null
  return {
    ca: params?.get?.('ca') ?? undefined,
    cert: params?.get?.('cert') ?? undefined,
    key: params?.get?.('key') ?? undefined,
    servername: params?.get?.('servername') ?? undefined,
    rejectUnauthorized:
      params?.get?.('rejectUnauthorized') === 'false' ? false : true,
  }
}

function mergeAuthFromUri(parsed, optionAuth) {
  const out = {
    token: parsed.token ?? parsed.apiKey ?? null,
    username: parsed.username ?? null,
    password: parsed.password ?? null,
    loginUrl: parsed.loginUrl ?? null,
  }
  if (optionAuth == null) return out
  if (typeof optionAuth !== 'object') {
    throw new TypeError('options.auth must be an object')
  }
  if (optionAuth.token != null) {
    if (typeof optionAuth.token !== 'string' || optionAuth.token.length === 0) {
      throw new TypeError('options.auth.token must be a non-empty string')
    }
    out.token = optionAuth.token
  }
  if (optionAuth.apiKey != null) {
    if (typeof optionAuth.apiKey !== 'string' || optionAuth.apiKey.length === 0) {
      throw new TypeError('options.auth.apiKey must be a non-empty string')
    }
    out.token = optionAuth.apiKey
  }
  if (optionAuth.username != null) {
    if (typeof optionAuth.username !== 'string' || optionAuth.username.length === 0) {
      throw new TypeError('options.auth.username must be a non-empty string')
    }
    out.username = optionAuth.username
  }
  if (optionAuth.password != null) {
    if (typeof optionAuth.password !== 'string' || optionAuth.password.length === 0) {
      throw new TypeError('options.auth.password must be a non-empty string')
    }
    out.password = optionAuth.password
  }
  if (optionAuth.loginUrl != null) {
    out.loginUrl = optionAuth.loginUrl
  }
  return out
}

/**
 * Exchange username + password for a bearer token via the server's
 * `POST /auth/login` HTTP endpoint. Same flow used by `connect()` when
 * the caller passes `auth: { username, password }`.
 *
 * @param {string} loginUrl Full URL of the server's auth endpoint.
 * @param {{ username: string, password: string }} credentials
 * @returns {Promise<{ token: string, username: string, role: string, expires_at: number }>}
 */
export async function login(loginUrl, { username, password }) {
  if (typeof loginUrl !== 'string' || !loginUrl.startsWith('http')) {
    throw new TypeError("login() requires an http(s):// URL pointing at /auth/login")
  }
  if (typeof username !== 'string' || username.length === 0) {
    throw new TypeError('login() requires a non-empty username')
  }
  if (typeof password !== 'string' || password.length === 0) {
    throw new TypeError('login() requires a non-empty password')
  }
  const response = await fetch(loginUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
  const body = await response.json().catch(() => ({}))
  if (!response.ok || body.ok === false) {
    const code = body.error_code || `HTTP_${response.status}`
    const message = body.error || `auth/login returned ${response.status}`
    throw new RedDBError(code, message, body)
  }
  if (typeof body.token !== 'string') {
    throw new RedDBError(
      'AUTH_LOGIN_BAD_RESPONSE',
      'auth/login response missing string token',
      body,
    )
  }
  return body
}

/**
 * Connection handle. Methods map 1:1 to JSON-RPC methods on the server.
 * Identical surface to `@reddb-io/sdk`'s `RedDB`, minus the local-spawn
 * lifecycle.
 */
export class RedDB {
  /** @param {HttpRpcClient | import('./redwire.js').RedWireClient} client */
  constructor(client) {
    this.client = client
    this.cache = new CacheClient(client)
    this.kv = new KvClient(client)
  }

  /** Execute a SQL query. Returns `{ statement, affected, columns, rows }`. */
  query(sql) {
    return this.client.call('query', { sql })
  }

  /** Insert one row. Returns `{ affected, id? }`. */
  insert(collection, payload) {
    return this.client.call('insert', { collection, payload })
  }

  /** Insert many rows in one call. Returns `{ affected }`. */
  bulkInsert(collection, payloads) {
    return this.client.call('bulk_insert', { collection, payloads })
  }

  /** Get an entity by id. Returns `{ entity }` (entity is `null` if not found). */
  get(collection, id) {
    return this.client.call('get', { collection, id: String(id) })
  }

  /** Delete an entity by id. Returns `{ affected }`. */
  delete(collection, id) {
    return this.client.call('delete', { collection, id: String(id) })
  }

  /** Probe the server. Returns `{ ok: true, version }`. */
  health() {
    return this.client.call('health', {})
  }

  /** Server version + protocol version. */
  version() {
    return this.client.call('version', {})
  }

  /** Exchange username + password for a bearer token. */
  login(username, password) {
    return this.client.call('auth.login', { username, password })
  }

  /** Identify the current caller. */
  whoami() {
    return this.client.call('auth.whoami', {})
  }

  /** Change the current caller's password. */
  changePassword(currentPassword, newPassword) {
    return this.client.call('auth.change_password', {
      current_password: currentPassword,
      new_password: newPassword,
    })
  }

  /** Mint a long-lived API key. */
  createApiKey({ username, role } = {}) {
    return this.client.call('auth.create_api_key', { username, role })
  }

  /** Revoke an API key by its public id. */
  revokeApiKey(key) {
    return this.client.call('auth.revoke_api_key', { key })
  }

  /** Close the underlying transport. */
  close() {
    return this.client.close()
  }
}
