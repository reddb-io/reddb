/**
 * KV client — exposes `kv.{put,get,delete}` through the underlying transport.
 *
 * HTTP uses the REST KV endpoint. RedWire/stdio transports may bridge the
 * same method names directly or fall back to SQL while dedicated wire frames
 * are still landing server-side.
 */

export class KvClient {
  /** @param {{ call: Function }} client */
  constructor(client) {
    this._client = client
  }

  /**
   * Store or replace a key in a collection.
   * @param {string} collection
   * @param {string | number} key
   * @param {unknown} value
   * @returns {Promise<object>}
   */
  async put(collection, key, value) {
    return await this._client.call('kv.put', { collection, key: String(key), value })
  }

  /**
   * Fetch a key from a collection.
   * @param {string} collection
   * @param {string | number} key
   * @returns {Promise<object>}
   */
  async get(collection, key) {
    return await this._client.call('kv.get', { collection, key: String(key) })
  }

  /**
   * Delete a key from a collection.
   * @param {string} collection
   * @param {string | number} key
   * @returns {Promise<object>}
   */
  async delete(collection, key) {
    return await this._client.call('kv.delete', { collection, key: String(key) })
  }
}
