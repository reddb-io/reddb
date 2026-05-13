/**
 * RedDB JavaScript driver.
 *
 * Public API:
 *   import { connect } from '@reddb-io/sdk'
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
 *
 * Remote URIs belong to @reddb-io/client. This SDK is embedded-only.
 */

import { spawnRed } from './spawn.js'
import { resolveSdkBinary } from './binary.js'
import { RpcClient, RedDBError } from './protocol.js'
import { parseUri } from './url.js'
import { CacheClient } from './cache.js'
import { KvClient } from './kv.js'
import { QueueClient } from './queue.js'
import { ConfigClient } from './config.js'
import { VaultClient } from './vault.js'
import { TypedQueryBuilder, collectionExists, listCollections } from './db-helpers.js'

export { RedDBError }
export { CacheClient } from './cache.js'
export { KvClient } from './kv.js'
export { QueueClient } from './queue.js'
export { ConfigClient } from './config.js'
export { VaultClient } from './vault.js'
export { TypedQueryBuilder } from './db-helpers.js'
export { parseUri, deriveLoginUrl } from './url.js'

export const EMBEDDED_ONLY_MESSAGE =
  'remote URIs are not supported in @reddb-io/sdk; install @reddb-io/client for grpc/http/red transports'

const MIN_INSERT_ID_ENGINE_VERSION = '1.0.9'

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
  const parsed = parseUri(uri)
  rejectRemoteUri(parsed)

  // Embedded modes: spawn the binary with stdio JSON-RPC. Auth is
  // not applicable (caller already has filesystem privileges).
  if (parsed.kind === 'embedded') {
    const merged = mergeAuthFromUri(parsed, options.auth)
    if (merged.token || merged.username) {
      throw new RedDBError(
        'AUTH_NOT_APPLICABLE',
        'auth is only meaningful for remote connections; embedded modes inherit caller privileges.',
      )
    }
    const args = embeddedArgs(parsed)
    const binary = options.binary ?? resolveSdkBinary()
    const child = await spawnRed(binary, args)
    const client = new RpcClient(child)
    await client.call('version', {})
    return new RedDB(client, { transport: 'embedded' })
  }
}

// Coerce a JS query parameter to a JSON-serializable shape the server
// understands. Values JSON cannot represent losslessly use the
// stdio/HTTP query parameter envelopes.
function serializeParam(value) {
  assertSupportedParam(value)
  if (value instanceof Float32Array || value instanceof Float64Array) {
    return Array.from(value)
  }
  if (value instanceof Date) {
    return { $ts: String(BigInt(value.getTime()) * 1_000_000n) }
  }
  if (value instanceof Uint8Array || (typeof Buffer !== 'undefined' && value instanceof Buffer)) {
    return { $bytes: bytesToBase64(value) }
  }
  if (typeof value === 'number' && !Number.isFinite(value)) {
    if (Number.isNaN(value)) return { $float: 'NaN' }
    return { $float: value > 0 ? 'Infinity' : '-Infinity' }
  }
  if (typeof value === 'string' && isUuidString(value)) {
    return { $uuid: value }
  }
  return value
}

function assertSupportedParam(value) {
  if (value == null) return
  if (
    typeof value === 'boolean'
    || typeof value === 'number'
    || typeof value === 'string'
  ) {
    return
  }
  if (value instanceof Date) {
    if (Number.isNaN(value.getTime())) {
      throw new RedDBError('UNSUPPORTED_PARAM', 'cannot encode invalid Date query parameter')
    }
    return
  }
  if (
    value instanceof Uint8Array
    || value instanceof Float32Array
    || value instanceof Float64Array
    || (typeof Buffer !== 'undefined' && value instanceof Buffer)
  ) {
    return
  }
  if (Array.isArray(value)) {
    if (value.every((item) => typeof item === 'number')) return
    throw new RedDBError(
      'UNSUPPORTED_PARAM',
      'array query parameters must contain only numbers',
    )
  }
  if (typeof value === 'object' && Object.getPrototypeOf(value) === Object.prototype) {
    return
  }
  throw new RedDBError(
    'UNSUPPORTED_PARAM',
    `cannot encode query parameter of type ${typeof value}`,
  )
}

function normalizeQueryParams(args) {
  if (args.length === 0) return null
  if (args.length === 1 && Array.isArray(args[0])) return args[0].map(serializeParam)
  return args.map(serializeParam)
}

function bytesToBase64(value) {
  const bytes = value instanceof Uint8Array
    ? value
    : new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
  if (typeof Buffer !== 'undefined') {
    return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString('base64')
  }
  let text = ''
  for (const byte of bytes) text += String.fromCharCode(byte)
  // eslint-disable-next-line no-undef
  return btoa(text)
}

function base64ToBytes(value) {
  if (typeof Buffer !== 'undefined') {
    const buf = Buffer.from(value, 'base64')
    return new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength)
  }
  // eslint-disable-next-line no-undef
  const text = atob(value)
  const out = new Uint8Array(text.length)
  for (let i = 0; i < text.length; i++) out[i] = text.charCodeAt(i)
  return out
}

function isUuidString(value) {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)
}

function normalizeResult(value) {
  if (Array.isArray(value)) return value.map(normalizeResult)
  if (value && typeof value === 'object') {
    const keys = Object.keys(value)
    if (keys.length === 1) {
      if (typeof value.$bytes === 'string') return base64ToBytes(value.$bytes)
      if (typeof value.$uuid === 'string') return value.$uuid
      if (typeof value.$float === 'string') {
        if (value.$float === 'NaN') return Number.NaN
        if (value.$float === 'Infinity' || value.$float === '+Infinity') return Infinity
        if (value.$float === '-Infinity') return -Infinity
      }
      if (typeof value.$ts === 'string' || typeof value.$ts === 'number') {
        const raw = typeof value.$ts === 'string'
          ? BigInt(value.$ts)
          : BigInt(Math.trunc(value.$ts))
        return new Date(Number(raw / 1_000_000n))
      }
    }
    const out = {}
    for (const [key, item] of Object.entries(value)) out[key] = normalizeResult(item)
    return out
  }
  return value
}

function embeddedArgs(parsed) {
  if (parsed.path) return ['rpc', '--stdio', '--path', parsed.path]
  return ['rpc', '--stdio']
}

/**
 * Merge `options.auth` (legacy `{ token, apiKey, username, password }`
 * shape) with credentials lifted from the URI itself. Explicit
 * `options.auth` always wins to keep behaviour predictable.
 */
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
 * Backwards-compatible shim: translate a URI into argv for
 * `red rpc --stdio`. New code should call `parseUri` directly and
 * route via `connect`. Kept exported for tests that pre-date the
 * `red://` parser.
 */
export function uriToArgs(uri, auth = null) {
  const parsed = parseUri(uri)
  if (parsed.kind === 'embedded') return embeddedArgs(parsed)
  rejectRemoteUri(parsed)
}

function rejectRemoteUri(parsed) {
  if (parsed.kind === 'embedded') return
  throw new RedDBError('EMBEDDED_ONLY', EMBEDDED_ONLY_MESSAGE)
}


/**
 * Connection handle. Methods map 1:1 to JSON-RPC methods on the binary.
 */
export class RedDB {
  /**
   * @param {RpcClient} client
   * @param {object} [opts]
   * @param {string} [opts.transport] Underlying transport label
   *   (normally 'embedded'). Used to gate calls that the embedded
   *   stdio bridge does not serve, like `cache.*`.
   */
  constructor(client, opts = {}) {
    this.client = client
    this.transport = opts.transport ?? null
    this.cache = new CacheClient(client, this.transport)
    this.queue = new QueueClient(client)
    const defaultKv = new KvClient(client)
    this.kv = Object.assign((collection = 'kv_default') => new KvClient(client, collection), {
      put: defaultKv.put.bind(defaultKv),
      invalidateTags: defaultKv.invalidateTags.bind(defaultKv),
      watch: defaultKv.watch.bind(defaultKv),
      watchPrefix: defaultKv.watchPrefix.bind(defaultKv),
    })
    this.config = (collection = 'red.config') => new ConfigClient(client, collection)
    this.vault = (collection = 'red.vault') => new VaultClient(client, collection)
  }

  /**
   * Execute a SQL query.
   *
   * Two signatures:
   *   - `query(sql)` — legacy single-arg form.
   *   - `query(sql, ...params)` — positional `$N` bind values.
   *   - `query(sql, paramsArray)` — legacy array form.
   *
   * Returns `{ statement, affected, columns, rows }`.
   */
  query(sql, ...params) {
    const wireParams = normalizeQueryParams(params)
    if (wireParams == null) {
      return this.client.call('query', { sql }).then(normalizeResult)
    }
    return this.client.call('query', { sql, params: wireParams }).then(normalizeResult)
  }

  /** Execute a SQL statement. Alias for `query`, including parameter binding. */
  execute(sql, ...params) {
    return this.query(sql, ...params)
  }

  /** Insert one row. Returns `{ affected, id }`. */
  async insert(collection, payload) {
    const result = await this.client.call('insert', { collection, payload })
    return requireInsertId(result, 'insert')
  }

  /** Insert many rows in one call. Returns `{ affected, ids }`. */
  async bulkInsert(collection, payloads) {
    const result = await this.client.call('bulk_insert', { collection, payloads })
    return requireInsertIds(result, payloads.length)
  }

  /** Return true when a collection is visible in the catalog. */
  exists(collection) {
    return collectionExists(this, collection)
  }

  /** List visible collections using SHOW COLLECTIONS. */
  list() {
    return listCollections(this)
  }

  /** Return a caller-typed query builder for a collection. */
  from(collection) {
    return new TypedQueryBuilder(this, collection)
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
  // Auth surface — these are not available in embedded mode because the
  // bridge layer doesn't expose `auth.*` JSON-RPC methods locally.
  // Use @reddb-io/client for remote authenticated servers.
  // ---------------------------------------------------------------

  /**
   * Exchange username + password for a bearer token when the underlying
   * client supports auth RPCs. Embedded SDK connections do not.
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

function requireInsertId(result, method) {
  if (!result || typeof result !== 'object' || result.id == null) {
    throw new RedDBError(
      'ENGINE_TOO_OLD',
      `${method}() requires RedDB engine >= ${MIN_INSERT_ID_ENGINE_VERSION} with insert id support`,
    )
  }
  return result
}

function requireInsertIds(result, expected) {
  if (!result || typeof result !== 'object' || !Array.isArray(result.ids)) {
    throw new RedDBError(
      'ENGINE_TOO_OLD',
      `bulkInsert() requires RedDB engine >= ${MIN_INSERT_ID_ENGINE_VERSION} with bulk insert id support`,
    )
  }
  if (result.ids.length !== expected) {
    throw new RedDBError(
      'INVALID_RESPONSE',
      `bulkInsert() expected ${expected} ids, got ${result.ids.length}`,
    )
  }
  return result
}
