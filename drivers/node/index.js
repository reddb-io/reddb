/**
 * RedDB Wire Protocol Client for Node.js
 *
 * Zero dependencies — uses Node.js built-in `net` module.
 * Speaks the RedDB binary TCP wire protocol directly.
 *
 * Usage:
 *   const { connect } = require('@reddb/client')
 *   const conn = await connect('127.0.0.1:50052')
 *   const result = await conn.query('SELECT * FROM users WHERE _entity_id = 1')
 *   await conn.bulkInsert('users', [{fields: {name: 'Alice', age: 30}}])
 *   conn.close()
 */

const net = require('net')

// Wire protocol message types
const MSG_QUERY = 0x01
const MSG_RESULT = 0x02
const MSG_ERROR = 0x03
const MSG_BULK_INSERT = 0x04
const MSG_BULK_OK = 0x05

class RedDBConnection {
  constructor(socket) {
    this._socket = socket
    this._pending = []
    this._buffer = Buffer.alloc(0)

    socket.on('data', (chunk) => this._onData(chunk))
    socket.on('error', (err) => {
      if (this._pending.length > 0) {
        this._pending.shift().reject(err)
      }
    })
  }

  _onData(chunk) {
    this._buffer = Buffer.concat([this._buffer, chunk])
    this._tryResolve()
  }

  _tryResolve() {
    while (this._buffer.length >= 5 && this._pending.length > 0) {
      const totalLen = this._buffer.readUInt32LE(0)
      const frameSize = 4 + totalLen
      if (this._buffer.length < frameSize) break

      const msgType = this._buffer[4]
      const payload = this._buffer.slice(5, frameSize)
      this._buffer = this._buffer.slice(frameSize)

      const { resolve, reject } = this._pending.shift()

      if (msgType === MSG_ERROR) {
        reject(new Error(payload.toString('utf8')))
      } else {
        resolve({ type: msgType, payload })
      }
    }
  }

  _send(msgType, payload) {
    return new Promise((resolve, reject) => {
      this._pending.push({ resolve, reject })
      const totalLen = 1 + payload.length
      const header = Buffer.alloc(5)
      header.writeUInt32LE(totalLen, 0)
      header[4] = msgType
      this._socket.write(header)
      this._socket.write(payload)
    })
  }

  /**
   * Execute a SQL query. Returns the result as a JSON string.
   * @param {string} sql
   * @returns {Promise<string>}
   */
  async query(sql) {
    const resp = await this._send(MSG_QUERY, Buffer.from(sql, 'utf8'))
    return resp.payload.toString('utf8')
  }

  /**
   * Execute a SQL query and parse the JSON result.
   * @param {string} sql
   * @returns {Promise<object>}
   */
  async queryParsed(sql) {
    const json = await this.query(sql)
    return JSON.parse(json)
  }

  /**
   * Execute a SQL query, return raw byte count (for benchmarking).
   * @param {string} sql
   * @returns {Promise<number>}
   */
  async queryRaw(sql) {
    const resp = await this._send(MSG_QUERY, Buffer.from(sql, 'utf8'))
    return resp.payload.length
  }

  /**
   * Bulk insert rows into a collection.
   * @param {string} collection
   * @param {Array<string>} jsonPayloads - Array of JSON strings
   * @returns {Promise<number>} - Number of rows inserted
   */
  async bulkInsert(collection, jsonPayloads) {
    const collBuf = Buffer.from(collection, 'utf8')
    const parts = [
      // [coll_len:u16][coll_bytes][n:u32]
      Buffer.alloc(2 + collBuf.length + 4)
    ]
    parts[0].writeUInt16LE(collBuf.length, 0)
    collBuf.copy(parts[0], 2)
    parts[0].writeUInt32LE(jsonPayloads.length, 2 + collBuf.length)

    for (const payload of jsonPayloads) {
      const jsonBuf = Buffer.from(payload, 'utf8')
      const lenBuf = Buffer.alloc(4)
      lenBuf.writeUInt32LE(jsonBuf.length, 0)
      parts.push(lenBuf, jsonBuf)
    }

    const body = Buffer.concat(parts)
    const resp = await this._send(MSG_BULK_INSERT, body)
    if (resp.payload.length >= 8) {
      return Number(resp.payload.readBigUInt64LE(0))
    }
    return 0
  }

  close() {
    this._socket.destroy()
  }
}

/**
 * Connect to a RedDB server via wire protocol.
 * @param {string} addr - Host:port (e.g. "127.0.0.1:50052")
 * @returns {Promise<RedDBConnection>}
 */
function connect(addr) {
  return new Promise((resolve, reject) => {
    const [host, portStr] = addr.split(':')
    const port = parseInt(portStr, 10)
    const socket = net.createConnection({ host, port }, () => {
      socket.setNoDelay(true)
      resolve(new RedDBConnection(socket))
    })
    socket.on('error', reject)
  })
}

/**
 * Connect to a RedDB server via TLS-encrypted wire protocol.
 * @param {string} addr - Host:port (e.g. "127.0.0.1:50053")
 * @param {object} [opts] - TLS options
 * @param {string|Buffer} [opts.ca] - CA certificate PEM (for self-signed certs)
 * @param {boolean} [opts.rejectUnauthorized=true] - Set false for self-signed in dev
 * @returns {Promise<RedDBConnection>}
 */
function connectTls(addr, opts = {}) {
  const tls = require('tls')
  return new Promise((resolve, reject) => {
    const [host, portStr] = addr.split(':')
    const port = parseInt(portStr, 10)
    const tlsOpts = {
      host,
      port,
      rejectUnauthorized: opts.rejectUnauthorized !== undefined ? opts.rejectUnauthorized : true,
      servername: opts.servername || host,
    }
    if (opts.ca) tlsOpts.ca = opts.ca
    const socket = tls.connect(tlsOpts, () => {
      socket.setNoDelay(true)
      resolve(new RedDBConnection(socket))
    })
    socket.on('error', reject)
  })
}

module.exports = { connect, connectTls, RedDBConnection }
