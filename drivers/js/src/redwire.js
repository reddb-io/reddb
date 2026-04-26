/**
 * RedWire v2 client for Node / Bun / Deno.
 *
 * Speaks the binary TCP protocol from
 * `docs/adr/0001-redwire-tcp-protocol.md` directly — no spawn, no
 * HTTP. Mirrors `drivers/rust/src/redwire/` so the wire shape
 * stays in lockstep across drivers.
 *
 * Public surface:
 *   - `connectRedwire(opts)` → returns a `RedWireClient`
 *   - `RedWireClient.query(sql)` → JSON envelope
 *   - `RedWireClient.ping()`
 *   - `RedWireClient.close()`
 *
 * Auth methods this cut supports: `anonymous`, `bearer`. SCRAM /
 * mTLS / OAuth land in subsequent PRs.
 */

import { RedDBError } from './protocol.js'

const MAGIC = 0xfe
const SUPPORTED_VERSION = 0x01
const FRAME_HEADER_SIZE = 16
const MAX_FRAME_SIZE = 16 * 1024 * 1024
const KNOWN_FLAGS = 0b00000011

export const MessageKind = Object.freeze({
  Query: 0x01,
  Result: 0x02,
  Error: 0x03,
  BulkInsert: 0x04,
  BulkOk: 0x05,
  Hello: 0x10,
  HelloAck: 0x11,
  AuthRequest: 0x12,
  AuthResponse: 0x13,
  AuthOk: 0x14,
  AuthFail: 0x15,
  Bye: 0x16,
  Ping: 0x17,
  Pong: 0x18,
  Get: 0x19,
  Delete: 0x1A,
  DeleteOk: 0x1B,
  BulkInsertBinary: 0x06,
  QueryBinary: 0x07,
  BulkInsertPrevalidated: 0x08,
})

/**
 * Typed value tags for the binary fast path. Identical to the
 * engine-side `wire::protocol::VAL_*` table.
 */
export const BinaryTag = Object.freeze({
  Null: 0,
  I64: 1,
  F64: 2,
  Text: 3,
  Bool: 4,
})

const KIND_NAME = Object.fromEntries(
  Object.entries(MessageKind).map(([k, v]) => [v, k]),
)

/**
 * Open a v2 connection.
 *
 * @param {object} opts
 * @param {string} opts.host
 * @param {number} opts.port
 * @param {{ kind: 'anonymous' } | { kind: 'bearer', token: string }} [opts.auth]
 * @param {string} [opts.clientName]
 * @returns {Promise<RedWireClient>}
 */
export async function connectRedwire(opts) {
  const { host, port } = opts
  if (typeof host !== 'string' || host.length === 0) {
    throw new TypeError('connectRedwire: host required')
  }
  if (typeof port !== 'number' || port <= 0 || port > 0xffff) {
    throw new TypeError('connectRedwire: port required (1-65535)')
  }
  const auth = opts.auth ?? { kind: 'anonymous' }

  const socket = await openSocket(host, port)
  const reader = new FrameReader(socket)

  // Discriminator + minor-version byte.
  await writeAll(socket, Uint8Array.from([MAGIC, SUPPORTED_VERSION]))

  // Hello.
  const methods = auth.kind === 'bearer' ? ['bearer'] : ['anonymous', 'bearer']
  const helloPayload = jsonBytes({
    versions: [SUPPORTED_VERSION],
    auth_methods: methods,
    features: 0,
    client_name: opts.clientName ?? `reddb-js/0.2`,
  })
  await writeFrame(socket, MessageKind.Hello, 1n, helloPayload)

  const ack = await reader.next()
  if (ack.kind === MessageKind.AuthFail) {
    socket.end()
    const reason = jsonReason(ack.payload) ?? 'AuthFail at HelloAck'
    throw new RedDBError('AUTH_REFUSED', `redwire: ${reason}`)
  }
  if (ack.kind !== MessageKind.HelloAck) {
    socket.end()
    throw new RedDBError(
      'PROTOCOL',
      `expected HelloAck, got ${KIND_NAME[ack.kind] ?? ack.kind}`,
    )
  }
  const ackParsed = jsonOf(ack.payload)
  const chosenAuth = ackParsed?.auth
  if (typeof chosenAuth !== 'string') {
    socket.end()
    throw new RedDBError('PROTOCOL', 'HelloAck missing `auth` field')
  }

  // AuthResponse.
  let respPayload
  if (chosenAuth === 'anonymous') {
    respPayload = new Uint8Array()
  } else if (chosenAuth === 'bearer') {
    if (auth.kind !== 'bearer') {
      socket.end()
      throw new RedDBError(
        'AUTH_REFUSED',
        'server demanded bearer but no token was supplied',
      )
    }
    respPayload = jsonBytes({ token: auth.token })
  } else {
    socket.end()
    throw new RedDBError(
      'PROTOCOL',
      `server picked unsupported auth method: ${chosenAuth}`,
    )
  }
  await writeFrame(socket, MessageKind.AuthResponse, 2n, respPayload)

  const final = await reader.next()
  if (final.kind === MessageKind.AuthFail) {
    socket.end()
    const reason = jsonReason(final.payload) ?? 'auth refused'
    throw new RedDBError('AUTH_REFUSED', reason)
  }
  if (final.kind !== MessageKind.AuthOk) {
    socket.end()
    throw new RedDBError(
      'PROTOCOL',
      `expected AuthOk, got ${KIND_NAME[final.kind] ?? final.kind}`,
    )
  }
  const session = jsonOf(final.payload) ?? {}

  return new RedWireClient(socket, reader, session)
}

/**
 * Returned by `connectRedwire`. Methods map 1:1 to RedWire frame
 * kinds. Reuses the same `RedDB`-shaped envelope as the other
 * transports so the surface above this is uniform.
 */
export class RedWireClient {
  constructor(socket, reader, session) {
    this.socket = socket
    this.reader = reader
    this.session = session
    this.nextCorr = 1n
    this.closed = false
  }

  async call(method, params = {}) {
    if (method === 'query') return this.#query(params.sql ?? '')
    if (method === 'insert') return this.#insert({ collection: params.collection, payload: params.payload })
    if (method === 'bulk_insert') return this.#insert({ collection: params.collection, payloads: params.payloads })
    if (method === 'bulk_insert_binary') {
      return this.bulkInsertBinary(params.collection, params.columns, params.rows)
    }
    if (method === 'get') return this.#getOrDelete(MessageKind.Get, MessageKind.Result, params)
    if (method === 'delete') return this.#getOrDelete(MessageKind.Delete, MessageKind.DeleteOk, params)
    if (method === 'health' || method === 'version') return this.#ping()
    throw new RedDBError(
      'UNKNOWN_METHOD',
      `RedWire transport doesn't bridge '${method}' yet`,
    )
  }

  /**
   * Bulk-insert via the v1 binary fast path (frame kind 0x06).
   * Same hot-loop perf as the engine's `MSG_BULK_INSERT_BINARY`
   * stress tests. Each row is an array of `[tag, value]` pairs
   * matching the column order; tag values come from `BinaryTag`.
   *
   * Example:
   *   client.bulkInsertBinary('users', ['name', 'age'], [
   *     [[BinaryTag.Text, 'alice'], [BinaryTag.I64, 30n]],
   *     [[BinaryTag.Text, 'bob'],   [BinaryTag.I64, 25n]],
   *   ])
   */
  async bulkInsertBinary(collection, columns, rows) {
    if (!Array.isArray(columns) || !Array.isArray(rows)) {
      throw new TypeError('bulkInsertBinary: columns and rows must be arrays')
    }
    const buf = encodeBinaryBulk(collection, columns, rows)
    const corr = this.#corr()
    await writeFrame(this.socket, MessageKind.BulkInsertBinary, corr, buf)
    const resp = await this.reader.next()
    if (resp.kind === MessageKind.BulkOk) {
      // v1 BulkOk body is an 8-byte little-endian count.
      if (resp.payload.length < 8) {
        throw new RedDBError('PROTOCOL', 'BulkOk truncated: expected 8-byte count')
      }
      const view = new DataView(
        resp.payload.buffer,
        resp.payload.byteOffset,
        resp.payload.byteLength,
      )
      return Number(view.getBigUint64(0, true))
    }
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError('ENGINE', new TextDecoder().decode(resp.payload))
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected BulkOk/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #getOrDelete(reqKind, okKind, params) {
    const corr = this.#corr()
    const payload = jsonBytes({ collection: params.collection, id: String(params.id) })
    await writeFrame(this.socket, reqKind, corr, payload)
    const resp = await this.reader.next()
    if (resp.kind === okKind) return jsonOf(resp.payload) ?? {}
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError('ENGINE', new TextDecoder().decode(resp.payload))
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected ${KIND_NAME[okKind]}/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #insert(body) {
    const corr = this.#corr()
    const payload = jsonBytes(body)
    await writeFrame(this.socket, MessageKind.BulkInsert, corr, payload)
    const resp = await this.reader.next()
    if (resp.kind === MessageKind.BulkOk) {
      return jsonOf(resp.payload) ?? { affected: 0 }
    }
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError('ENGINE', new TextDecoder().decode(resp.payload))
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected BulkOk/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #query(sql) {
    const corr = this.#corr()
    const payload = new TextEncoder().encode(sql)
    await writeFrame(this.socket, MessageKind.Query, corr, payload)
    const resp = await this.reader.next()
    if (resp.kind === MessageKind.Result) {
      return jsonOf(resp.payload) ?? {}
    }
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError(
        'ENGINE',
        new TextDecoder().decode(resp.payload),
      )
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected Result/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #ping() {
    const corr = this.#corr()
    await writeFrame(this.socket, MessageKind.Ping, corr, new Uint8Array())
    const resp = await this.reader.next()
    if (resp.kind !== MessageKind.Pong) {
      throw new RedDBError(
        'PROTOCOL',
        `expected Pong, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
      )
    }
    return { ok: true }
  }

  async close() {
    if (this.closed) return
    this.closed = true
    try {
      const corr = this.#corr()
      await writeFrame(this.socket, MessageKind.Bye, corr, new Uint8Array())
    } catch {
      // best-effort
    }
    this.socket.end()
  }

  #corr() {
    const c = this.nextCorr
    this.nextCorr = this.nextCorr + 1n
    return c
  }
}

// ---------------------------------------------------------------------------
// Framing helpers
// ---------------------------------------------------------------------------

function encodeFrame(kind, correlationId, payload, flags = 0, streamId = 0) {
  if (!(payload instanceof Uint8Array)) {
    payload = new Uint8Array(payload)
  }
  const length = FRAME_HEADER_SIZE + payload.length
  if (length > MAX_FRAME_SIZE) {
    throw new RedDBError('FRAME_TOO_LARGE', `frame ${length} > ${MAX_FRAME_SIZE}`)
  }
  const buf = new Uint8Array(length)
  const view = new DataView(buf.buffer)
  view.setUint32(0, length, true)
  buf[4] = kind
  buf[5] = flags & KNOWN_FLAGS
  view.setUint16(6, streamId, true)
  view.setBigUint64(8, BigInt(correlationId), true)
  buf.set(payload, FRAME_HEADER_SIZE)
  return buf
}

function writeFrame(socket, kind, correlationId, payload) {
  const buf = encodeFrame(kind, correlationId, payload)
  return writeAll(socket, buf)
}

function decodeFrame(buf) {
  if (buf.length < FRAME_HEADER_SIZE) return null
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  const length = view.getUint32(0, true)
  if (length < FRAME_HEADER_SIZE || length > MAX_FRAME_SIZE) {
    throw new RedDBError('FRAME_INVALID_LENGTH', `length=${length}`)
  }
  if (buf.length < length) return null
  const kind = buf[4]
  const flags = buf[5]
  if (flags & ~KNOWN_FLAGS) {
    throw new RedDBError('FRAME_UNKNOWN_FLAGS', `flags=0x${flags.toString(16)}`)
  }
  const streamId = view.getUint16(6, true)
  const correlationId = view.getBigUint64(8, true)
  const payload = buf.slice(FRAME_HEADER_SIZE, length)
  return { kind, flags, streamId, correlationId, payload, consumed: length }
}

/**
 * Buffered frame reader — TCP delivers byte streams, frames may
 * cross or share `data` events. Maintains a rolling accumulator
 * and yields one frame per `next()` call.
 */
class FrameReader {
  constructor(socket) {
    this.chunks = []
    this.totalLen = 0
    this.waiters = []
    this.error = null
    this.eof = false
    socket.on('data', (chunk) => {
      // chunk is Buffer (Node) or Uint8Array (Bun/Deno)
      const u8 = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk)
      this.chunks.push(u8)
      this.totalLen += u8.length
      this.#tryDeliver()
    })
    socket.on('error', (err) => {
      this.error = err
      this.#flushWaiters()
    })
    socket.on('end', () => {
      this.eof = true
      this.#flushWaiters()
    })
    socket.on('close', () => {
      this.eof = true
      this.#flushWaiters()
    })
  }

  next() {
    if (this.error) return Promise.reject(this.error)
    return new Promise((resolve, reject) => {
      this.waiters.push({ resolve, reject })
      this.#tryDeliver()
    })
  }

  #tryDeliver() {
    while (this.waiters.length > 0 && this.totalLen > 0) {
      const flat = this.#flatten()
      let frame
      try {
        frame = decodeFrame(flat)
      } catch (err) {
        const w = this.waiters.shift()
        w.reject(err)
        return
      }
      if (frame == null) {
        // Need more bytes — put the flattened buffer back as a
        // single chunk so we don't keep flattening repeatedly.
        this.chunks = [flat]
        return
      }
      this.chunks = [flat.subarray(frame.consumed)]
      this.totalLen = this.chunks[0].length
      const w = this.waiters.shift()
      w.resolve(frame)
    }
    if (this.eof && this.waiters.length > 0 && this.totalLen === 0) {
      const err = this.error ?? new RedDBError('CONNECTION_CLOSED', 'redwire: connection closed')
      while (this.waiters.length > 0) {
        this.waiters.shift().reject(err)
      }
    }
  }

  #flushWaiters() {
    if (this.waiters.length === 0) return
    if (this.totalLen > 0) {
      this.#tryDeliver()
      return
    }
    const err = this.error ?? new RedDBError('CONNECTION_CLOSED', 'redwire: connection closed')
    while (this.waiters.length > 0) {
      this.waiters.shift().reject(err)
    }
  }

  #flatten() {
    if (this.chunks.length === 1) return this.chunks[0]
    const out = new Uint8Array(this.totalLen)
    let off = 0
    for (const c of this.chunks) {
      out.set(c, off)
      off += c.length
    }
    this.chunks = [out]
    return out
  }
}

// ---------------------------------------------------------------------------
// Socket helpers — node:net works on Node, Bun, and Deno via shim
// ---------------------------------------------------------------------------

async function openSocket(host, port) {
  const { Socket } = await import('node:net')
  return await new Promise((resolve, reject) => {
    const sock = new Socket()
    const onErr = (err) => {
      sock.removeListener('connect', onOk)
      reject(err)
    }
    const onOk = () => {
      sock.removeListener('error', onErr)
      sock.setNoDelay(true)
      resolve(sock)
    }
    sock.once('error', onErr)
    sock.once('connect', onOk)
    sock.connect(port, host)
  })
}

function writeAll(socket, bytes) {
  return new Promise((resolve, reject) => {
    socket.write(bytes, (err) => (err ? reject(err) : resolve()))
  })
}

// ---------------------------------------------------------------------------
// JSON helpers — handshake payloads use JSON for now (CBOR follow-up)
// ---------------------------------------------------------------------------

function jsonBytes(obj) {
  return new TextEncoder().encode(JSON.stringify(obj))
}

function jsonOf(bytes) {
  if (!bytes || bytes.length === 0) return null
  try {
    return JSON.parse(new TextDecoder().decode(bytes))
  } catch {
    return null
  }
}

/**
 * Encode the v1 binary bulk-insert payload (no v2 frame header).
 * Layout: `[coll_len u16][coll_bytes][ncols u16]
 *          [(name_len u16)(name_bytes)]*ncols
 *          [nrows u32]
 *          [(tag u8)(value)]*ncols * nrows`
 */
function encodeBinaryBulk(collection, columns, rows) {
  const enc = new TextEncoder()
  const collBytes = enc.encode(collection)
  // Pre-encode column names + their length prefixes.
  const colChunks = columns.map((c) => enc.encode(c))
  let total = 2 + collBytes.length + 2
  for (const cb of colChunks) total += 2 + cb.length
  total += 4
  // Estimate row size — we'll resize if needed.
  for (const row of rows) {
    if (!Array.isArray(row) || row.length !== columns.length) {
      throw new TypeError(
        `bulkInsertBinary: each row must be an array of length ${columns.length}`,
      )
    }
    for (const cell of row) {
      total += sizeOfBinaryCell(cell)
    }
  }
  const buf = new Uint8Array(total)
  const view = new DataView(buf.buffer)
  let pos = 0
  view.setUint16(pos, collBytes.length, true); pos += 2
  buf.set(collBytes, pos); pos += collBytes.length
  view.setUint16(pos, colChunks.length, true); pos += 2
  for (const cb of colChunks) {
    view.setUint16(pos, cb.length, true); pos += 2
    buf.set(cb, pos); pos += cb.length
  }
  view.setUint32(pos, rows.length, true); pos += 4
  for (const row of rows) {
    for (const cell of row) {
      pos = writeBinaryCell(buf, view, pos, cell, enc)
    }
  }
  return buf
}

function sizeOfBinaryCell(cell) {
  if (!Array.isArray(cell) || cell.length !== 2) {
    throw new TypeError('bulkInsertBinary cell must be [tag, value]')
  }
  const [tag] = cell
  switch (tag) {
    case 0: return 1
    case 1: return 1 + 8
    case 2: return 1 + 8
    case 3: {
      const v = cell[1]
      const bytes = typeof v === 'string' ? new TextEncoder().encode(v).length : 0
      return 1 + 4 + bytes
    }
    case 4: return 1 + 1
    default: throw new RedDBError('UNKNOWN_BINARY_TAG', `tag=${tag}`)
  }
}

function writeBinaryCell(buf, view, pos, cell, enc) {
  const [tag, value] = cell
  buf[pos++] = tag
  switch (tag) {
    case 0: // Null
      return pos
    case 1: { // I64
      const bi = typeof value === 'bigint' ? value : BigInt(value)
      view.setBigInt64(pos, bi, true)
      return pos + 8
    }
    case 2: { // F64
      view.setFloat64(pos, Number(value), true)
      return pos + 8
    }
    case 3: { // Text
      const bytes = enc.encode(String(value))
      view.setUint32(pos, bytes.length, true); pos += 4
      buf.set(bytes, pos)
      return pos + bytes.length
    }
    case 4: { // Bool
      buf[pos] = value ? 1 : 0
      return pos + 1
    }
    default:
      throw new RedDBError('UNKNOWN_BINARY_TAG', `tag=${tag}`)
  }
}

function jsonReason(bytes) {
  const v = jsonOf(bytes)
  if (v && typeof v === 'object' && typeof v.reason === 'string') {
    return v.reason
  }
  return null
}
