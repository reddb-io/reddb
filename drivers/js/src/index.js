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
 * Connection URIs accepted today (Phase 2):
 *   - 'memory://'                — ephemeral in-memory database
 *   - 'file:///absolute/path'    — embedded, persisted to disk
 *   - 'file://relative/path'     — relative path (still parsed)
 *
 * Coming in a later phase (binary needs --connect support first):
 *   - 'grpc://host:port'         — remote server
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
 * @returns {Promise<RedDB>}
 */
export async function connect(uri, options = {}) {
  const args = uriToArgs(uri)
  const binary = options.binary ?? resolveBinaryPath()
  const child = await spawnRed(binary, args)
  const client = new RpcClient(child)
  // Sanity check: bounce a `version` call so connection errors surface
  // here instead of on the user's first real query.
  await client.call('version', {})
  return new RedDB(client)
}

/**
 * Translate a connection URI into the argv passed to `red rpc --stdio`.
 * Drivers in every language share this mapping (see PLAN_DRIVERS.md).
 */
export function uriToArgs(uri) {
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
    // open a tonic client and proxy every JSON-RPC call. No extra
    // translation here so server-side routing stays the source of truth.
    return ['rpc', '--stdio', '--connect', uri]
  }
  throw new RedDBError(
    'UNSUPPORTED_SCHEME',
    `unsupported URI scheme: '${uri}'. Expected 'file://', 'memory://' or 'grpc://'.`,
  )
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

  /** Close the connection and wait for the binary to exit. */
  close() {
    return this.client.close()
  }
}
