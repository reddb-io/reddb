/**
 * Transport-agnostic connection handle.
 *
 * `RedDB` and its `Collection` / transaction handles own the request
 * shaping (parameter serialization, insert-id normalization, transaction
 * orchestration) that is identical regardless of which wire the underlying
 * `client` speaks. The class is constructed with a low-level transport
 * `client` (anything exposing `call(method, params)` / `close()`, and
 * optionally `streamSelect` / `streamInput`) plus an injected `streaming`
 * implementation.
 *
 * Streaming is **injected**, never imported: `stream()` / `inputStream()`
 * delegate to `streaming.createSelectStream` / `streaming.createInputStream`
 * supplied by the entrypoint, so this module never statically references
 * `node:stream` (or Web streams). A connection built without a streaming
 * implementation raises `STREAMING_UNSUPPORTED` if a stream is requested.
 *
 * Imports zero `node:` built-ins.
 */

import { RedDBError } from './errors.js'
import { normalizeExactNumbers, normalizeQueryParams, serializeJsonValue } from './serialization.js'
import { requireInsertId, requireInsertIds } from './insert-ids.js'
import { CacheClient } from '../cache.js'
import { KvClient } from '../kv.js'
import { QueueClient } from '../queue.js'
import { DocumentClient } from '../documents.js'
import { ConfigClient } from '../config.js'
import { VaultClient } from '../vault.js'
import { TypedQueryBuilder, collectionExists, listCollections } from '../db-helpers.js'

const NESTED_TX_NOT_SUPPORTED = 'NESTED_TX_NOT_SUPPORTED'

/**
 * Connection handle. Methods map 1:1 to JSON-RPC methods on the server.
 * Identical surface to `@reddb-io/sdk`'s `RedDB`, minus the local-spawn
 * lifecycle.
 */
class TransactionHandle {
  constructor(db) {
    this.db = db
  }

  query(sql, ...params) {
    return this.db.query(sql, ...params)
  }

  execute(sql, ...params) {
    return this.db.execute(sql, ...params)
  }

  insert(collection, payload) {
    return this.db.insert(collection, payload)
  }

  bulkInsert(collection, payloads) {
    return this.db.bulkInsert(collection, payloads)
  }

  async transaction() {
    throw nestedTransactionError()
  }
}

/**
 * A streaming-capable collection/table handle (PRD #759 S11). `query()`
 * stays a one-shot Promise so callers never accidentally pay streaming
 * overhead for small reads; `stream()` / `inputStream()` are the explicit
 * streaming surfaces. The bound `name` is the default ingest target.
 */
export class Collection {
  /** @param {RedDB} db @param {string} name */
  constructor(db, name) {
    if (typeof name !== 'string' || name.length === 0) {
      throw new RedDBError('INVALID_COLLECTION', 'collection(name) requires a non-empty name')
    }
    this.db = db
    this.name = name
  }

  /** One-shot Promise query — no streaming surface leakage. */
  query(sql, ...params) {
    return this.db.query(sql, ...params)
  }

  /** Stream a read-only SELECT as a Readable/AsyncIterable. */
  stream(sql, opts = {}) {
    return this.db.stream(sql, opts)
  }

  /** Open a streaming write into this collection. */
  inputStream(opts = {}) {
    return this.db.inputStream(this.name, opts)
  }
}

export class RedDB {
  /**
   * @param {object} client transport exposing `call`/`close` (and optionally
   *   `streamSelect`/`streamInput`).
   * @param {{ createSelectStream: Function, createInputStream: Function }} [streaming]
   *   injected streaming implementation. The entrypoint supplies the
   *   `node:stream`-based one; omit it for transports that never stream.
   */
  constructor(client, streaming) {
    this.client = client
    this._streaming = streaming ?? null
    this.cache = new CacheClient(client)
    this.queue = new QueueClient(client)
    this.documents = new DocumentClient(this)
    const defaultKv = new KvClient(client)
    this.kv = Object.assign((collection = 'kv_default') => new KvClient(client, collection), {
      put: defaultKv.put.bind(defaultKv),
      invalidateTags: defaultKv.invalidateTags.bind(defaultKv),
      watch: defaultKv.watch.bind(defaultKv),
      watchPrefix: defaultKv.watchPrefix.bind(defaultKv),
    })
    this.config = (collection = 'red.config') => new ConfigClient(client, collection)
    this.vault = (collection = 'red.vault') => new VaultClient(client, collection)
    this.inTransaction = false
  }

  /** Execute a SQL query. Returns `{ statement, affected, columns, rows }`. */
  query(sql, ...params) {
    const wireParams = normalizeQueryParams(params)
    if (wireParams == null) {
      return this.client.call('query', { sql }).then(normalizeExactNumbers)
    }
    return this.client.call('query', { sql, params: wireParams }).then(normalizeExactNumbers)
  }

  /** Execute a SQL statement. Alias for `query`, including parameter binding. */
  execute(sql, ...params) {
    return this.query(sql, ...params)
  }

  /** Insert one row. Returns `{ affected, rid, id }`; `id` is a legacy alias for `rid`. */
  async insert(collection, payload) {
    let result = await this.client.call('insert', { collection, payload: serializeJsonValue(payload) })
    result = normalizeExactNumbers(result)
    if (
      result &&
      typeof result === 'object' &&
      !('affected' in result) &&
      ('rid' in result || 'id' in result)
    ) {
      result = { ...result, affected: 1 }
    }
    return requireInsertId(result, 'insert')
  }

  /** Insert many rows in one call. Returns `{ affected, rids, ids }`; `ids` is a legacy alias. */
  async bulkInsert(collection, payloads) {
    const result = await this.client.call('bulk_insert', {
      collection,
      payloads: serializeJsonValue(payloads),
    }).then(normalizeExactNumbers)
    return requireInsertIds(result, payloads.length)
  }

  async transaction(callback) {
    if (this.inTransaction) {
      throw nestedTransactionError()
    }
    if (typeof callback !== 'function') {
      throw new TypeError('transaction(callback) requires a function')
    }

    this.inTransaction = true
    let began = false
    try {
      await this.query('BEGIN')
      began = true
      const result = await callback(new TransactionHandle(this))
      await this.query('COMMIT')
      return result
    } catch (err) {
      if (began) {
        try {
          await this.query('ROLLBACK')
        } catch (rollbackErr) {
          attachRollbackError(err, rollbackErr)
        }
      }
      throw err
    } finally {
      this.inTransaction = false
    }
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

  /**
   * Return a streaming-capable handle for a collection/table. Exposes the
   * explicit method separation of PRD #759: `.query()` is a one-shot
   * Promise, `.stream()` is a streaming Readable, `.inputStream()` is a
   * streaming Writable. The collection name binds the ingest target.
   */
  collection(name) {
    return new Collection(this, name)
  }

  /**
   * Stream a read-only SELECT. Returns a Node `Readable` in object mode
   * that also conforms to `AsyncIterable<Row>`. Uses RedWire when the
   * connection is RedWire, HTTP NDJSON when it is HTTP — identical surface
   * either way. Call `.cancel(reason?)` to terminate mid-stream.
   *
   * Delegates to the injected streaming implementation.
   */
  stream(sql, opts = {}) {
    return this.#streaming().createSelectStream(this.client, sql, opts)
  }

  /**
   * Open a streaming write into `target`. Returns a Node `Writable` in
   * object mode whose `.completion()` resolves with the server's terminal
   * envelope. Call `.cancel(reason?)` to abandon the ingest.
   *
   * Delegates to the injected streaming implementation.
   */
  inputStream(target, opts = {}) {
    return this.#streaming().createInputStream(this.client, target, opts)
  }

  #streaming() {
    if (!this._streaming) {
      throw new RedDBError(
        'STREAMING_UNSUPPORTED',
        'this connection was built without a streaming implementation',
      )
    }
    return this._streaming
  }

  /** Get an entity by id. Returns `{ entity }` (entity is `null` if not found). */
  get(collection, id) {
    return this.client.call('get', { collection, id: String(id) }).then(normalizeExactNumbers)
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

function nestedTransactionError() {
  return new RedDBError(
    NESTED_TX_NOT_SUPPORTED,
    `${NESTED_TX_NOT_SUPPORTED}: nested transactions are not supported on one connection`,
  )
}

function attachRollbackError(err, rollbackErr) {
  if (err && typeof err === 'object') {
    try {
      err.rollbackError = rollbackErr
    } catch {
      // Preserve the original callback/query error even for frozen errors.
    }
  }
}
