/**
 * Cache client — exposes cache.{get,put,exists,invalidate,invalidatePrefix,
 * invalidateTags,flushNamespace} via the underlying HTTP transport.
 *
 * NOTE: These methods require server-side HTTP endpoints under /cache/ns/*.
 * flushNamespace routes to the existing POST /admin/blob_cache/flush_namespace.
 * All others target endpoints planned for a future server release.
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
    const result = await this._client.call('cache.get', { namespace, key })
    if (result == null || result.value == null) return null
    return base64ToBytes(result.value)
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
   * Check whether a key is present.
   * @param {string} namespace
   * @param {string} key
   * @returns {Promise<'present' | 'absent' | 'maybe'>}
   */
  async exists(namespace, key) {
    const result = await this._client.call('cache.exists', { namespace, key })
    return result?.status ?? 'maybe'
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
   * Routes to POST /admin/blob_cache/flush_namespace (live endpoint).
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
