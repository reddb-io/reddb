/**
 * RedDB JavaScript driver.
 *
 * Public API:
 *   import { connect } from 'reddb'
 *   const db = await connect('file:///data.rdb')
 *   const result = await db.query('SELECT * FROM users LIMIT 10')
 *   const inserted = await db.insert('users', { name: 'Alice' })
 *   await db.bulkInsert('users', [{ name: 'Bob' }, { name: 'Carol' }])
 *   const row = await db.get('users', '42')
 *   await db.delete('users', '42')
 *   await db.close()
 *
 * Connection URIs:
 *   - 'memory://'                — ephemeral in-memory database (embedded)
 *   - 'file:///absolute/path'    — embedded, persisted to disk
 *   - 'grpc://host:port'         — remote server via gRPC
 *
 * Authentication (only meaningful for `grpc://`; embedded modes ignore
 * auth options because the spawned binary inherits the caller's
 * filesystem privileges):
 *
 *   await connect('grpc://host:5051', {
 *     auth: { token: 'sk-...' }                     // raw bearer / api key
 *   })
 *   await connect('grpc://host:5051', {
 *     auth: { apiKey: 'ak-...' }                    // alias for token
 *   })
 *   await connect('grpc://host:5051', {
 *     auth: { username: 'admin', password: 'x' }    // login flow — driver
 *                                                   // calls /auth/login,
 *                                                   // caches the bearer
 *   })
 *
 * Username/password requires the server to expose the `auth.login`
 * JSON-RPC method (proxied through the gRPC bridge).
 */

import { spawnRed } from './spawn.js'
import { resolveBinaryPath } from './binary.js'
import { RpcClient, RedDBError } from './protocol.js'

export { RedDBError }

/**
 * Connect to a RedDB instance.
 *
 * @param {string} uri Connection URI. See module docstring for accepted schemes.
 * @param {object} [options]
 * @param {string} [options.binary] Override the path to the `red` binary.
 * @param {object} [options.auth] Authentication credentials. See module docstring.
 * @param {string} [options.auth.token] Bearer / API-key token.
 * @param {string} [options.auth.apiKey] Alias for `token`.
 * @param {string} [options.auth.username] Username for password login.
 * @param {string} [options.auth.password] Password for password login.
 * @returns {Promise<RedDB>}
 */
export async function connect(uri, options = {}) {
  const auth = normalizeAuth(uri, options.auth)
  const args = uriToArgs(uri, auth)
  const binary = options.binary ?? resolveBinaryPath()
  const child = await spawnRed(binary, args)
  const client = new RpcClient(child)
  // Sanity check: bounce a `version` call so connection errors surface
  // here instead of on the user's first real query.
  await client.call('version', {})

  return new RedDB(client)
}

/**
 * Exchange username + password for a bearer token by hitting the
 * server's `POST /auth/login` HTTP endpoint, then return that token
 * for use with subsequent `connect({ auth: { token } })` calls.
 *
 * Why a separate function: the gRPC surface does not currently
 * expose `auth.login` as an RPC, so the driver can't piggyback on
 * the binary spawn for password auth. The HTTP listener does
 * expose it, and is the canonical login site (the same endpoint
 * the dashboard uses).
 *
 * @param {string} loginUrl Full URL of the server's auth endpoint
 *                          (e.g. `https://reddb.example.com/auth/login`).
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
 * Translate a connection URI + (optional) auth into the argv passed
 * to `red rpc --stdio`.
 *
 * @param {string} uri
 * @param {object|null} [auth] Output of `normalizeAuth`. Only token-shaped
 *                              auth surfaces here; login flow is handled
 *                              one layer up in `connect`.
 */
export function uriToArgs(uri, auth = null) {
  if (typeof uri !== 'string' || uri.length === 0) {
    throw new TypeError("connect() requires a URI string (e.g. 'file:///data.rdb' or 'memory://')")
  }
  if (uri === 'memory://' || uri === 'memory:') {
    return ['rpc', '--stdio']
  }
  if (uri.startsWith('file://')) {
    const path = uri.slice('file://'.length)
    if (!path) {
      throw new TypeError(`invalid file:// URI: missing path in '${uri}'`)
    }
    return ['rpc', '--stdio', '--path', path]
  }
  if (uri.startsWith('grpc://')) {
    // The driver hands the full URI back to the binary, which will
    // open a tonic client and proxy every JSON-RPC call. The binary
    // attaches the bearer token to gRPC metadata for every call.
    const args = ['rpc', '--stdio', '--connect', uri]
    if (auth?.kind === 'token') {
      args.push('--token', auth.token)
    }
    return args
  }
  throw new RedDBError(
    'UNSUPPORTED_SCHEME',
    `unsupported URI scheme: '${uri}'. Expected 'file://', 'memory://' or 'grpc://'.`,
  )
}

/**
 * Normalise `options.auth` into:
 *   - `null` (no auth supplied)
 *   - `{ kind: 'token', token: '...' }`
 *
 * Validates that auth is only specified for grpc:// URIs and that
 * the token is non-empty.
 *
 * Username/password is intentionally NOT a valid `auth` shape on
 * `connect()` — gRPC doesn't expose an `auth.login` RPC today.
 * Use the standalone `login(httpUrl, { username, password })`
 * helper to exchange credentials for a token first, then pass
 * the token here.
 */
function normalizeAuth(uri, auth) {
  if (auth == null) return null
  if (typeof auth !== 'object') {
    throw new TypeError('options.auth must be an object')
  }

  const isRemote = typeof uri === 'string' && uri.startsWith('grpc://')
  if (!isRemote) {
    throw new RedDBError(
      'AUTH_NOT_APPLICABLE',
      'options.auth is only meaningful for grpc:// connections; '
        + 'embedded modes (memory://, file://) inherit the caller\'s privileges.',
    )
  }

  // Username/password requires a separate HTTP round-trip; reject
  // the combined shape so users don't expect transparent login here.
  if (auth.username != null || auth.password != null) {
    throw new RedDBError(
      'AUTH_LOGIN_NEEDS_HTTP',
      'username/password login is not yet bridged through gRPC. '
        + "Use `await login(httpUrl, { username, password })` to mint a token, "
        + 'then pass `{ token }` here.',
    )
  }

  const tokenLike = auth.token ?? auth.apiKey ?? null
  if (tokenLike == null) {
    throw new TypeError('options.auth must contain { token } or { apiKey }')
  }
  if (typeof tokenLike !== 'string' || tokenLike.length === 0) {
    throw new TypeError('options.auth.token / options.auth.apiKey must be a non-empty string')
  }
  return { kind: 'token', token: tokenLike }
}

/**
 * Connection handle. Methods map 1:1 to JSON-RPC methods on the binary.
 */
export class RedDB {
  /** @param {RpcClient} client */
  constructor(client) {
    this.client = client
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

  // ---------------------------------------------------------------
  // Auth surface — these are no-ops in embedded mode because the
  // bridge layer doesn't expose `auth.*` JSON-RPC methods locally.
  // They forward to the server when the connection is grpc://.
  // ---------------------------------------------------------------

  /**
   * Exchange username + password for a bearer token. Returns
   * `{ token, username, role, expires_at }`. Server-side this
   * routes to `POST /auth/login`.
   *
   * Prefer the `auth: { username, password }` form on `connect()`
   * — it does the same exchange + caches the token transparently.
   */
  login(username, password) {
    return this.client.call('auth.login', { username, password })
  }

  /** Identify the current caller. Returns `{ username, role }`. */
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

  /**
   * Mint a long-lived API key for the caller (or a sub-user, when
   * the caller has `Admin` role). Returns `{ key, role, created_at }`.
   * Pass the returned `key` back via `auth: { apiKey: key }` on
   * future `connect()` calls.
   */
  createApiKey({ username, role } = {}) {
    return this.client.call('auth.create_api_key', { username, role })
  }

  /** Revoke an API key by its public id. */
  revokeApiKey(key) {
    return this.client.call('auth.revoke_api_key', { key })
  }

  /** Close the connection and wait for the binary to exit. */
  close() {
    return this.client.close()
  }
}
