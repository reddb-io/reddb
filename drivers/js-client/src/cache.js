/**
 * Cache client — exposes exact-key reads/writes, batching, TTL management,
 * and invalidation through the underlying HTTP transport.
 *
 * The HTTP data-plane endpoints under /cache/ns/* authorize the caller's
 * tenant and cache namespace. Invalidation is local to the addressed server.
 * Use the HTTP transport for remote cache operations.
 *
 * Values are base64-encoded in transit so binary payloads survive JSON.
 */

export class CacheClient {
  /** @param {{ call: Function }} client */
  constructor(client) {
    this._client = client
  }

  /**
   * Fetch a cached value. Returns a Uint8Array on hit, null on miss.
   * @param {string} namespace
   * @param {string} key
   * @returns {Promise<Uint8Array | null>}
   */
  async get(namespace, key) {
    const entry = await this.getEntry(namespace, key)
    return entry?.value ?? null
  }

  /**
   * Fetch a cached value together with its TTL and version metadata.
   * Returns null on a miss.
   * @param {string} namespace
   * @param {string} key
   * @returns {Promise<{key:string,value:Uint8Array,size_bytes:number,ttl_ms:number|null,expires_at_unix_ms:number|null,version:number|null,stale:boolean}|null>}
   */
  async getEntry(namespace, key) {
    const result = await this._client.call('cache.get', { namespace, key })
    if (result == null || result.value == null) return null
    return { ...result, value: base64ToBytes(result.value) }
  }

  /**
   * Fetch several keys in one round trip. Results preserve input order.
   * @param {string} namespace
   * @param {string[]} keys
   * @returns {Promise<Array<{key:string,value:Uint8Array|null,size_bytes:number|null,ttl_ms:number|null,expires_at_unix_ms:number|null,version:number|null,stale:boolean}>>}
   */
  async getMany(namespace, keys) {
    const result = await this._client.call('cache.mget', { namespace, keys })
    return (result?.entries ?? []).map((entry) => ({
      ...entry,
      value: entry.value == null ? null : base64ToBytes(entry.value),
    }))
  }

  /**
   * Store a value in the cache.
   * @param {string} namespace
   * @param {string} key
   * @param {Uint8Array | Buffer | string} value  String is UTF-8 encoded.
   * @param {object} [opts]
   * @param {number} [opts.ttl_ms]
   * @param {string[]} [opts.tags]
   * @param {object} [opts.policy]
   * @returns {Promise<void>}
   */
  async put(namespace, key, value, opts = {}) {
    const encoded = bytesToBase64(value)
    await this._client.call('cache.put', {
      namespace,
      key,
      value: encoded,
      ...opts,
    })
  }

  /**
   * Store several entries in one round trip. A batch accepts at most 256
   * entries and 8 MiB of decoded values.
   * @param {string} namespace
   * @param {Array<{key:string,value:Uint8Array|Buffer|string,ttl_ms?:number,tags?:string[],policy?:object}>} entries
   * @returns {Promise<number>}
   */
  async putMany(namespace, entries) {
    const encoded = entries.map(({ key, value, ...opts }) => ({
      key,
      value: bytesToBase64(value),
      ...opts,
    }))
    const result = await this._client.call('cache.mput', { namespace, entries: encoded })
    return result?.stored ?? 0
  }

  /**
   * Check whether a key is present.
   * @param {string} namespace
   * @param {string} key
   * @returns {Promise<'present' | 'absent' | 'maybe'>}
   */
  async exists(namespace, key) {
    const result = await this._client.call('cache.exists', { namespace, key })
    return result?.status ?? 'maybe'
  }

  /** Set or replace a key's relative TTL without rewriting its value. */
  async expire(namespace, key, ttl_ms) {
    const result = await this._client.call('cache.expiry', { namespace, key, ttl_ms })
    return result?.updated ?? false
  }

  /** Set or replace a key's absolute expiry deadline. */
  async expireAt(namespace, key, expires_at_unix_ms) {
    const result = await this._client.call('cache.expiry', {
      namespace,
      key,
      expires_at_unix_ms,
    })
    return result?.updated ?? false
  }

  /** Remove expiry from a key without rewriting its value. */
  async persist(namespace, key) {
    const result = await this._client.call('cache.expiry', { namespace, key, persist: true })
    return result?.updated ?? false
  }

  /**
   * Remove a single entry.
   * @param {string} namespace
   * @param {string} key
   * @returns {Promise<void>}
   */
  async invalidate(namespace, key) {
    await this._client.call('cache.invalidate', { namespace, key })
  }

  /**
   * Remove all entries whose key starts with `prefix`.
   * @param {string} namespace
   * @param {string} prefix
   * @returns {Promise<number>} Number of entries removed.
   */
  async invalidatePrefix(namespace, prefix) {
    const result = await this._client.call('cache.invalidate_prefix', { namespace, prefix })
    return result?.removed ?? 0
  }

  /**
   * Remove all entries tagged with any of the given tags.
   * @param {string} namespace
   * @param {string[]} tags
   * @returns {Promise<number>} Number of entries removed.
   */
  async invalidateTags(namespace, tags) {
    const result = await this._client.call('cache.invalidate_tags', { namespace, tags })
    return result?.removed ?? 0
  }

  /**
   * Remove all entries in a namespace.
   * Uses the caller-scoped POST /cache/ns/:namespace/flush endpoint.
   * @param {string} namespace
   * @returns {Promise<void>}
   */
  async flushNamespace(namespace) {
    await this._client.call('cache.flush_namespace', { namespace })
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function bytesToBase64(value) {
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value)
    return bufToBase64(bytes)
  }
  if (value instanceof Uint8Array || (typeof Buffer !== 'undefined' && Buffer.isBuffer(value))) {
    return bufToBase64(value)
  }
  throw new TypeError('cache value must be a string, Uint8Array, or Buffer')
}

function bufToBase64(bytes) {
  if (typeof Buffer !== 'undefined') {
    return Buffer.from(bytes).toString('base64')
  }
  let bin = ''
  for (const b of bytes) bin += String.fromCharCode(b)
  return btoa(bin)
}

function base64ToBytes(b64) {
  if (typeof Buffer !== 'undefined') {
    return new Uint8Array(Buffer.from(b64, 'base64'))
  }
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}
